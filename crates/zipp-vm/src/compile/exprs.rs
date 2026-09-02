// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;
// The AST and its string type, named explicitly so this file does not depend on
// how the parent module happens to re-export them. `use super::*` would supply
// them too; an explicit import SHADOWS a glob rather than clashing with it.
use crate::parse::ast;
use crate::parse::token::StrVal;

// Security/source caps: plans duplicate source key text once per literal site,
// so bound both one hostile literal and aggregate compilation memory. Exceeding
// a cap silently retains ordinary `NewObject`; it never rejects valid source.
const STATIC_KEY_PLAN_MAX_FIELDS: usize = 256;
const STATIC_KEY_PLAN_MAX_BYTES: usize = 64 * 1024;

/// Field cap for the one-step `FinalizeObject` lowering. Its staged value block
/// holds this many registers live at once, so the cap bounds literal register
/// pressure; wider literals keep the per-field append sequence.
const OBJECT_FINALIZE_MAX_FIELDS: usize = crate::bytecode::FINALIZE_STAGE_SLOTS;

/// Retain at most one plan's admissible name indices while the literal body is
/// compiled. `static_keys` continues counting the full unique-static run, so a
/// wider literal still fails `plan_names.len() == static_keys` and falls back;
/// it just cannot grow an optimization-only Vec with attacker-sized source.
#[inline]
fn collect_static_key_plan_name(plan_names: &mut Vec<u32>, name: u32) {
    if plan_names.len() < STATIC_KEY_PLAN_MAX_FIELDS {
        plan_names.push(name);
    }
}

// NOTE: parenthesization. `ast` has no ParenthesizedExpression, so every peel is
// gone from this file (`expr_into`, `typeof (f)`, `delete (x)`). That is a
// deliberate behaviour change in one direction only: a parenthesized operand now
// reaches the pattern matches that recognise a *shape* — `new (Array)(1)`,
// `x instanceof (Array)` and `(Math).PI` — where before the
// wrapper node hid it and the generic path ran. Parenthesization is observable in
// exactly two places (assignment-target simplicity, and NamedEvaluation via
// `Target::Ident { covered }`), and neither is one of these, so the fold is
// correct; it just was not reachable before.
//
// NOTE: helpers defined HERE and nowhere else in this file's group —
// `static_key_text`, `lone_surrogate_markers`, `PropVal`, and the
// `FnCompiler` methods `str_const`, `member`, `private_member`, `prop_value`,
// `object_accessor`, `object_data_prop`. If another section lands an identical
// helper, keep one copy.

/// The TEXT of a non-computed property key, or `None` when the key has no static
/// spelling (a computed key, or a private name). Property keys are Rust `String`s
/// engine-wide (`ObjMap.keys`), which cannot hold a lone surrogate — a key that
/// does decodes LOSSILY here (two distinct lone-surrogate keys collide), the same
/// documented stage-2 limit `string_literal_key` recorded.
///
/// NOTE: a BigInt property key (`({1n: 1})`, whose key is its exact decimal
/// digits) used to be accepted here via `PropertyKey::BigIntLiteral`. `PropKey`
/// cannot represent one — an f64 does not round-trip the digits — so such a
/// literal is now rejected by the parser before it reaches the compiler.
pub(crate) fn static_key_text(k: &ast::PropKey) -> Option<String> {
    match k {
        ast::PropKey::Ident(n) => Some(n.to_string()),
        ast::PropKey::Str(s) => Some(s.to_lossy_string()),
        ast::PropKey::Num(n) => Some(fmt_key_num(*n)),
        ast::PropKey::Computed(_) | ast::PropKey::Private(_) => None,
    }
}

/// UTF-16 code units → the lone-surrogate MARKER form (`\u{FFFD}XXXX` per lone
/// unit, `\u{FFFD}fffd` for a literal U+FFFD) that `add_string_const_wtf8`
/// expects and `resolve_const` decodes back to exact WTF-8 at intern time. The
/// inverse of `crate::heap::decode_lone_surrogate_markers`, from code units
/// rather than from WTF-8 bytes.
pub(crate) fn lone_surrogate_markers(units: &[u16]) -> String {
    let mut out = String::with_capacity(units.len() + 8);
    for r in char::decode_utf16(units.iter().copied()) {
        match r {
            // A literal U+FFFD must be escaped too, or it could be read back as
            // the start of a marker.
            Ok('\u{FFFD}') => out.push_str("\u{FFFD}fffd"),
            Ok(c) => out.push(c),
            Err(e) => {
                let u = e.unpaired_surrogate();
                out.push('\u{FFFD}');
                out.push_str(&format!("{u:04x}"));
            }
        }
    }
    out
}

/// `ZIPP_NO_FUSED_CMPJUMP=1` restores the unfused `Lt`/`Le` + `JumpIfFalse`
/// pair at branch heads (`if`/`while`/`for` tests, the for-in index guard),
/// where the default emits the fused `JumpIfNotLt`/`JumpIfNotLe`. Read once
/// per process.
#[inline]
pub(crate) fn fused_cmp_jump_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_FUSED_CMPJUMP").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_TYPEOF_SAME=1` restores the unfused
/// `TypeOf; TypeOf; Eq/Ne/LooseEq/LooseNe` lowering.  This is a compiler-side
/// switch so a single binary can attribute the dynamic-`typeof` comparison
/// fusion without changing any runtime layout.
#[inline]
pub(crate) fn typeof_same_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_TYPEOF_SAME").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Flatten the LEFT-leaning `+` spine: `L1+L2+…+Ln` parses as
/// `Binary{Add, Binary{Add, …}, Ln}`, so only the LEFT child is walked — a
/// parenthesized right operand (`a + (b + c)`) is its own subexpression with
/// a different pairwise coercion order and stays a single leaf here (its own
/// `binary()` call may fuse it independently). (W11 B124 chain fusion.)
fn add_spine<'e>(e: &'e ast::Expr, leaves: &mut Vec<&'e ast::Expr>) {
    if let ast::Expr::Binary {
        op: ast::BinaryOp::Add,
        left,
        right,
    } = e
    {
        add_spine(left, leaves);
        leaves.push(right);
    } else {
        leaves.push(e);
    }
}

/// W11 (B124): `ZIPP_NO_CONCAT_FUSE=1` disables the n-ary string-concat chain
/// fusion (`FnCompiler::concat_chain` — the `binary()` Add-spine flatten and
/// the ≥3-piece template emission), restoring the pairwise `Add` bytecode
/// bit-for-bit. Compiler-side gate; read once per process (memoized AtomicU8).
#[inline]
pub(crate) fn concat_fuse_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_CONCAT_FUSE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_PAD2_CACHE=1` restores literal `"0" + x` / `"" + x` to the
/// historical `LoadConst` + `Add` bytecode. The specialized opcode retains the
/// exact `+` fallback; this compiler-side switch also removes its dispatch cost.
#[inline]
pub(crate) fn pad2_cache_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_PAD2_CACHE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// How a property's value arrives: an ordinary expression (`k: v`, `[k]: v`), or
/// a method/accessor's own `Function` node (`m(){}`, `get k(){}`). oxc modelled
/// the second as a `FunctionExpression` in `value`; `ast` names it directly, so
/// the two shapes are re-joined here rather than in every branch.
#[derive(Clone, Copy)]
pub(crate) enum PropVal<'a> {
    Expr(&'a ast::Expr),
    Func(&'a ast::Function),
}

impl<'a> FnCompiler<'a> {
    // ── expressions ──
    /// Compile `e`, returning the register holding its value.
    pub(crate) fn expr(&mut self, e: &ast::Expr) -> R<Reg> {
        let dst = self.temp();
        self.expr_into(e, dst)
    }

    /// Compile `e` for its EFFECTS only — the caller will not read the result.
    ///
    /// The one rewrite this enables: a POSTFIX update whose value nobody reads
    /// IS the prefix update. Both perform exactly one `ToNumeric` of the old
    /// value and one store, and differ only in which value they hand back, so
    /// with the result discarded the prefix form is the same program minus one
    /// `AddInt` and one temp register.
    ///
    /// That temp is not free. `plan_region` requires a pinned TypedArray
    /// receiver to have exactly ONE in-region definition; the extra temp shifts
    /// register allocation by one, lands on the receiver, and demotes the whole
    /// loop from the unboxed tiers to the boxed MEM tier. Measured on a
    /// Float64Array read loop: `for (i = 0; i < n; i++)` 27ms against 7ms for
    /// the byte-identical `while (i < n) { …; ++i; }` — 3.9x, for the choice of
    /// `i++` over `++i` in a position where the two cannot be told apart.
    pub(crate) fn expr_discarded(&mut self, e: &ast::Expr) -> R<Reg> {
        if let ast::Expr::Update {
            op,
            prefix: false,
            target,
        } = e
        {
            let dst = self.temp();
            return self.update(*op, true, target, dst);
        }
        self.expr(e)
    }

    /// Intern a string LITERAL's value and return its CONSTANT-POOL index,
    /// picking the slot its encoding needs: well-formed text goes to
    /// `add_string_const`; a value holding a lone surrogate goes to the
    /// WTF-8-decoding slot in the MARKER form, which `resolve_const` turns back
    /// into the exact code units at intern time.
    pub(crate) fn str_const(&mut self, s: &StrVal) -> u32 {
        match s {
            StrVal::Utf8(t) => self.add_string_const(t),
            StrVal::Utf16(u) => self.add_string_const_wtf8(&lone_surrogate_markers(u)),
        }
    }

    /// Compile `e` placing its value into `dst` (or another register it already
    /// occupies, which the caller may use directly). Returns the register that
    /// actually holds the result.
    pub(crate) fn expr_into(&mut self, e: &ast::Expr, dst: Reg) -> R<Reg> {
        use ast::Expr as E;
        match e {
            // NOTE: the strict-mode legacy-octal early error (`01` / `08` is a
            // SyntaxError in strict code) used to live here, because oxc parsed
            // leniently and deferred the check to semantics via the literal's
            // `raw` text. `Expr::Num` carries only the VALUE — the spelling is
            // `token::NumLit.kind`, which the parser inspects while it still has
            // it — so the check belongs to the parser now and is not
            // reconstructible here. No bytecode changes: it only ever produced
            // an error.
            E::Num(n) => {
                self.load_number(dst, *n);
                Ok(dst)
            }
            E::BigInt(b) => {
                // The digits are base-10 (the source base is already
                // normalized). In-range literals load as inline i128 (the fast
                // tier); beyond-i128 literals parse ONCE into the program's
                // arbitrary-precision constant pool (the digits are decimal by
                // construction, so only an out-of-range value reaches the pool —
                // the canonical-form invariant holds).
                match b.parse::<i128>() {
                    Ok(v) => self.emit(Instr::LoadBigInt { dst, value: v }),
                    Err(_) => {
                        let big = b
                            .parse::<num_bigint::BigInt>()
                            .map_err(|_| "invalid BigInt literal".to_string())?;
                        let idx = self.bigint_consts.len() as u32;
                        self.bigint_consts.push(big);
                        self.emit(Instr::LoadBigIntBig { dst, idx });
                    }
                }
                Ok(dst)
            }
            E::Regex { pattern, flags } => {
                // `/pat/flags` → NewRegExp (compiles via the `regress` engine at
                // runtime). `pattern` is the source (escapes stay escaped — the
                // regex engine compiles the source); `flags` is the JS flag
                // string, already in the spec's canonical order.
                //
                // EXACT-source recovery: a lone surrogate written literally in
                // the pattern lives in the `StrVal` itself, so it reaches the
                // constant pool through the WTF-8-decoding slot (`str_const`).
                // The LEXER does the recovery (`Lexer::set_exact_src`), which is
                // why `Expr::Regex` needs no span of its own: the parser hands
                // over the exact code units rather than the lossy U+FFFD view
                // the compiler used to repair by slicing a parallel buffer.
                let text = pattern.to_lossy_string();
                let pat = self.str_const(pattern);
                // EARLY ERROR: a RegularExpressionLiteral must parse under the
                // `Pattern` goal at COMPILE time, not when it is evaluated. The
                // difference is observable — `if (false) { /a{2,1}/ }` must be a
                // SyntaxError for the whole program, and until this check
                // existed that program ran to completion.
                //
                // Safe to reject here because it was measured, not assumed: of
                // 3,391 distinct regex literals extracted from 13,922 real
                // library files, `regress` rejects zero, while catching all ten
                // spec-invalid patterns probed and accepting all eighteen
                // Annex B / modern valid ones (lone `]`, `a{,2}`, `[a-\d]`,
                // `(?=a)*`, `\c1`, `\08`, lookbehind, `\p{L}` under `u`,
                // `[\q{abc}]` under `v`).
                if let Err(err) = regress::Regex::with_flags(&text, &**flags) {
                    return Err(format!(
                        "SyntaxError: invalid regular expression /{text}/{flags}: {err}"
                    ));
                }
                let flg = self.add_string_const(flags);
                let pt = self.temp();
                self.emit(Instr::LoadConst { dst: pt, idx: pat });
                let ft = self.temp();
                self.emit(Instr::LoadConst { dst: ft, idx: flg });
                self.emit(Instr::NewRegExp {
                    dst,
                    pattern: pt,
                    flags: ft,
                    is_construct: true,
                });
                self.next_reg -= 2;
                Ok(dst)
            }
            E::Str(s) => {
                // A value holding a lone surrogate routes through the
                // WTF-8-decoding constant slot (see `str_const`).
                let idx = self.str_const(s);
                self.emit(Instr::LoadConst { dst, idx });
                Ok(dst)
            }
            E::Template(t) => {
                // Desugar `q0${e0}q1${e1}...qN` to string concatenation
                // q0 + ToString(e0) + q1 + ToString(e1) + ... + qN. Each `${e}` is
                // ToString'd (string hint) FIRST — NOT left to `+`, whose default
                // hint tries `valueOf` before `toString` (wrong for e.g. a Temporal
                // value, whose `valueOf` throws). After ToStr both operands are
                // strings, so each `+` is a pure (rope) concat.
                let q0 = &t.quasis[0];
                let idx = match q0.cooked.as_ref() {
                    Some(s) => self.str_const(s),
                    // No cooked value (an illegal escape) — untagged, so this is
                    // a SyntaxError the parser raised; keep the "" the compiler
                    // has always emitted rather than inventing a value.
                    None => self.add_string_const(""),
                };
                // ── W11 (B124) chain fusion, template form ── the desugared
                // concat pieces: q0, then per `${e}` a ToStr'd value plus its
                // trailing non-empty quasi. A ≥3-piece template fuses like the
                // `binary()` Add spine (q0 is always a string leaf, so the
                // literal gate is satisfied by construction): link 1 is a
                // plain `Add` of q0 with the first ToStr (never grow the
                // JIT-shared LoadConst'd constant in place), links 2.. are
                // `StrConcatChain` on a fresh `acc` temp, and a final `Move`
                // writes `dst`. Building in `acc` rather than `dst` also
                // closes the old build-into-var-dst quirk: a `${}` expression
                // reading the destination var mid-build now sees its PRIOR
                // value (matching node), not a partial intermediate. Op order
                // (LoadConst / leaf eval / ToStr) is otherwise identical.
                let pieces = 1
                    + t.exprs.len()
                    + t.quasis
                        .iter()
                        .skip(1)
                        .filter(|q| q.cooked.as_ref().is_some_and(|s| !s.is_empty()))
                        .count();
                if concat_fuse_enabled() && pieces >= 3 {
                    let acc = self.temp();
                    let save = self.next_reg; // == acc + 1
                    let q0r = self.temp();
                    self.emit(Instr::LoadConst { dst: q0r, idx });
                    // `pieces >= 3` implies at least one `${e}`, so link 1
                    // (the plain Add consuming q0r) fires in iteration 0.
                    let mut started = false;
                    for (i, e) in t.exprs.iter().enumerate() {
                        let r = self.expr(e)?;
                        let rs = self.temp();
                        self.emit(Instr::ToStr { dst: rs, a: r });
                        if started {
                            self.emit(Instr::StrConcatChain {
                                dst: acc,
                                a: acc,
                                b: rs,
                            });
                        } else {
                            self.emit(Instr::Add {
                                dst: acc,
                                a: q0r,
                                b: rs,
                            });
                            started = true;
                        }
                        self.next_reg = save;
                        if let Some(qe) = t.quasis.get(i + 1) {
                            if let Some(q) = qe.cooked.as_ref().filter(|s| !s.is_empty()) {
                                let qidx = self.str_const(q);
                                let qr = self.temp();
                                self.emit(Instr::LoadConst { dst: qr, idx: qidx });
                                self.emit(Instr::StrConcatChain {
                                    dst: acc,
                                    a: acc,
                                    b: qr,
                                });
                                self.next_reg = save;
                            }
                        }
                    }
                    self.emit(Instr::Move { dst, src: acc });
                    self.next_reg = acc.max(dst + 1);
                    return Ok(dst);
                }
                self.emit(Instr::LoadConst { dst, idx });
                for (i, e) in t.exprs.iter().enumerate() {
                    let r = self.expr(e)?;
                    let rs = self.temp();
                    self.emit(Instr::ToStr { dst: rs, a: r });
                    self.emit(Instr::Add { dst, a: dst, b: rs });
                    if let Some(qe) = t.quasis.get(i + 1) {
                        if let Some(q) = qe.cooked.as_ref().filter(|s| !s.is_empty()) {
                            let qidx = self.str_const(q);
                            let qr = self.temp();
                            self.emit(Instr::LoadConst { dst: qr, idx: qidx });
                            self.emit(Instr::Add { dst, a: dst, b: qr });
                        }
                    }
                }
                Ok(dst)
            }
            E::TaggedTemplate { tag, quasi } => self.tagged_template(tag, quasi, dst),
            E::Bool(b) => {
                self.emit(Instr::LoadBool { dst, val: *b });
                Ok(dst)
            }
            E::Null => {
                self.emit(Instr::LoadNull { dst });
                Ok(dst)
            }
            E::Ident(id) => {
                let n: &str = id;
                // A strict-mode reserved word may not appear as an identifier
                // reference (property keys / member names are different AST nodes,
                // so `obj.public` / `{public:1}` are unaffected).
                if self.cx.in_strict && is_strict_reserved_word(n) {
                    return Err(format!(
                        "SyntaxError: '{n}' is a reserved word in strict mode"
                    ));
                }
                // A parameter referenced before its own left-to-right
                // initialization — `(x = x)` (self) or `(x = y, y)` (forward) — is
                // in the Temporal Dead Zone: reading it throws a ReferenceError.
                if n == "arguments" && self.cx.in_field_init {
                    return Err(
                        "SyntaxError: 'arguments' is not allowed in a class field initializer"
                            .into(),
                    );
                }
                if self.param_tdz.contains(n) {
                    let e = self.alloc_reg();
                    self.emit(Instr::NewError {
                        dst: e,
                        kind: 4,
                        arg: None,
                        opts: None,
                        errors: None,
                    });
                    self.emit(Instr::Throw { src: e });
                    return Ok(dst);
                }
                // Inside a `with`, a free identifier may resolve to a property of an
                // active with-object (innermost first), else the static binding.
                let with_objs = self.with_obj_regs(n);
                if !with_objs.is_empty() {
                    return Ok(self.load_with(n, &with_objs, dst));
                }
                let binding = self.resolve(n);
                // These three names are immutable VALUE properties only at the
                // end of ResolveBinding.  Locals/params/upvalues/class names and
                // every active `with` object have already won above.  A
                // direct-eval zone must still perform its dynamic lookup before
                // falling back to the global value, so only the proven-static
                // Global case is folded here.
                if matches!(n, "undefined" | "NaN" | "Infinity")
                    && matches!(binding, Binding::Global(_))
                    && !self.box_all_locals
                    && !self.cx.dyn_global_zone
                {
                    match n {
                        "undefined" => self.emit(Instr::LoadUndefined { dst }),
                        "NaN" => {
                            let idx = self.add_const(Value::num(f64::NAN));
                            self.emit(Instr::LoadConst { dst, idx });
                        }
                        _ => {
                            let idx = self.add_const(Value::num(f64::INFINITY));
                            self.emit(Instr::LoadConst { dst, idx });
                        }
                    }
                    return Ok(dst);
                }
                match binding {
                    Binding::Local(r) => Ok(r), // already in a register
                    Binding::LocalCell(cell) => {
                        self.emit(Instr::CellGet { dst, cell });
                        Ok(dst)
                    }
                    Binding::Upvalue(idx) => {
                        // A sloppy contains-direct-eval function: an eval-
                        // introduced function-scoped `var` shadows the
                        // captured name for READS.
                        if !self.cx.in_strict && self.box_all_locals {
                            let name = self.upvalues.borrow()[idx as usize].0.clone();
                            let slot = self.cx.global_slot(&name) as u32;
                            self.emit(Instr::LoadUpvalDyn {
                                dst,
                                idx,
                                name: slot,
                            });
                        } else {
                            self.emit(Instr::UpvalGet { dst, idx });
                        }
                        Ok(dst)
                    }
                    Binding::Global(idx) => {
                        if self.box_all_locals || self.cx.dyn_global_zone {
                            // A dynamic EvalScope binding may shadow the slot.
                            self.emit(Instr::LoadGlobalDyn { dst, idx });
                        } else {
                            self.emit(Instr::LoadGlobal { dst, idx });
                        }
                        Ok(dst)
                    }
                    Binding::ClassName(class_id) => {
                        self.emit(Instr::LoadClassValue { dst, class_id });
                        Ok(dst)
                    }
                }
            }
            E::This => {
                // `this` lives in register 0 of the current function, unless a
                // static field initializer has redirected it to the class value.
                // In a derived ctor it is in TDZ until super() completes.
                self.this_check();
                Ok(self.this_override.unwrap_or(0))
            }
            E::Binary { op, left, right } => self.binary(*op, left, right, dst),
            E::Logical { op, left, right } => self.logical(*op, left, right, dst),
            E::Unary { op, arg } => self.unary(*op, arg, dst),
            E::Update { op, prefix, target } => self.update(*op, *prefix, target, dst),
            E::Assign {
                op, target, value, ..
            } => self.assign(*op, target, value, dst),
            E::Cond { test, cons, alt } => self.conditional(test, cons, alt, dst),
            E::Yield { arg, delegate } => self.yield_expr(arg.as_deref(), *delegate, dst),
            E::Await(a) => self.await_expr(a, dst),
            E::Call(c) => self.call(c, dst),
            E::New { callee, args } => {
                // Only Array keeps a dedicated user-visible constructor opcode.
                // Every other named builtin uses the generic construct path: it
                // already implements exact constructor identity, proxies,
                // newTarget/prototype selection and the complete argument list.
                let has_spread = args.iter().any(|a| matches!(a, ast::Arg::Spread(_)));
                if !has_spread
                    && matches!(&**callee, ast::Expr::Ident(id) if &**id == "Array" && self.builtin_unshadowed(id))
                {
                    let save = self.next_reg;
                    let callee_reg = self.capture_plain_callee(callee)?;
                    let (arg_base, argc) = self.eval_args_contiguous(args)?;
                    self.emit(Instr::ArrayCtor {
                        dst,
                        callee: Some(callee_reg),
                        arg_base,
                        argc,
                        is_construct: true,
                    });
                    self.next_reg = save.max(dst + 1);
                    return Ok(dst);
                }
                // General `new C(args)`: evaluate the constructor value, then the
                // args (contiguous), and let the VM build the instance. When the
                // arguments contain a spread (`new C(...xs)`), build a flat args
                // array and construct via NewSpread instead. The constructor is
                // SNAPSHOTTED into a temp before the args run: an argument's
                // side effect reassigning the callee variable must not change
                // which value is constructed (EvaluateNew takes GetValue first).
                let save = self.next_reg;
                let callee_reg = self.capture_plain_callee(callee)?;
                if has_spread {
                    let args_arr = self.build_spread_args(args)?;
                    self.emit(Instr::NewSpread {
                        dst,
                        callee: callee_reg,
                        args: args_arr,
                    });
                    self.next_reg = save.max(dst + 1); // reclaim callee + arg scratch
                    return Ok(dst);
                }
                let (arg_base, argc) = self.eval_args_contiguous(args)?;
                self.emit(Instr::New {
                    dst,
                    callee: callee_reg,
                    arg_base,
                    argc,
                });
                self.next_reg = save.max(dst + 1); // reclaim callee + args
                Ok(dst)
            }
            E::Function(f) => {
                let (id, has_up) =
                    self.compile_func_expr(f.name.as_ref().map(|n| n.to_string()), f)?;
                self.emit_make_callable(dst, id, has_up);
                Ok(dst)
            }
            E::Arrow(a) => {
                let (id, _has_up) = self.compile_arrow(a, "")?;
                self.emit_make_arrow(dst, id);
                Ok(dst)
            }
            E::Class(c) => self.class_expr(c, dst, None),
            E::Array(elems, _) => self.array_literal(elems, dst),
            E::Object(props, _) => self.object_literal(props, dst),
            // One node for all three member forms; `prop` says which.
            E::Member(m) => self.member(m, dst),
            // `#field in obj` — private brand check (private fields are stored as
            // the reserved "#field" property, so this is a HasProp on that key).
            E::PrivateIn { name, object } => {
                self.check_private_declared(name)?;
                let kr = self.temp();
                let idx = self.add_string_const(&private_key(name));
                self.emit(Instr::LoadConst { dst: kr, idx });
                let obj = self.expr(object)?;
                // Ergonomic brand check: bypass the private-key reflection filter.
                self.emit(Instr::HasProp {
                    dst,
                    key: kr,
                    obj,
                    brand: true,
                });
                Ok(dst)
            }
            E::Chain(inner) => self.chain_expr(inner, dst),
            E::Seq(exprs) => {
                // `(a, b, c)` — evaluate each for side effects; value is the last.
                let n = exprs.len();
                for (i, e) in exprs.iter().enumerate() {
                    if i + 1 == n {
                        return self.expr_into(e, dst);
                    }
                    let _ = self.expr(e)?;
                }
                self.emit(Instr::LoadUndefined { dst }); // empty sequence (unreachable)
                Ok(dst)
            }
            E::NewTarget => {
                // `new.target` is an early SyntaxError outside a function/class body
                // (e.g. at the top level of a script or an indirect eval), including
                // inside an arrow that has no enclosing ordinary function.
                if !self.cx.new_target_ok {
                    return Err("SyntaxError: new.target expression is not allowed here".into());
                }
                // The current activation's new.target (undefined unless entered via
                // new/Reflect.construct/super). Arrows inherit it lexically from their
                // enclosing function via the frame they run in.
                self.emit(Instr::LoadNewTarget { dst });
                Ok(dst)
            }
            E::ImportCall {
                spec,
                options,
                phase,
            } => {
                // Dynamic `import(specifier [, options])` / `import.defer` /
                // `import.source`. Evaluate the specifier (and options, if any);
                // ImportCall does ToString, the options/phase checks, and the load.
                let spec = self.expr(spec)?;
                let opts = match options {
                    Some(o) => Some(self.expr(o)?),
                    None => None,
                };
                let phase = match phase {
                    ast::ImportPhase::Source => 2,
                    ast::ImportPhase::Defer => 1,
                    ast::ImportPhase::Evaluation => 0,
                };
                self.emit(Instr::ImportCall {
                    dst,
                    spec,
                    phase,
                    opts,
                });
                Ok(dst)
            }
            E::ImportMeta => {
                // `import.meta` — module code only (a SyntaxError in scripts);
                // `new.target` is handled by the dedicated lowering above.
                // (Both module pipelines: compile_module entry and the
                // loader's compile_eval(is_module) — see the import
                // pre-pass gate.)
                let in_module = self.cx.module_mode || (self.cx.eval_mode && !self.cx.eval_locals);
                if !in_module {
                    return Err("SyntaxError: import.meta is only valid in modules".into());
                }
                let dst = self.temp();
                self.emit(Instr::ImportMeta { dst });
                Ok(dst)
            }
            // `super` is not a value; every position where it is legal is handled
            // by the construct that owns it (member reads, `super(...)`).
            E::Super => Err("unsupported expression (not in the zipp-vm v1 subset yet)".into()),
        }
    }

    /// `obj.x` / `obj[k]` / `obj.#x` — one node in `ast`, three lowerings.
    pub(crate) fn member(&mut self, m: &ast::Member, dst: Reg) -> R<Reg> {
        match &m.prop {
            ast::MemberProp::Ident(p) => self.static_member(m, p, dst),
            ast::MemberProp::Computed(key) => self.computed_member(m, key, dst),
            ast::MemberProp::Private(p) => self.private_member(m, p, dst),
        }
    }

    pub(crate) fn static_member(&mut self, m: &ast::Member, prop: &str, dst: Reg) -> R<Reg> {
        // Namespace constants (notably Math.PI/E/…) are ordinary property reads.
        // Folding them by identifier spelling ignored lexical shadowing, a
        // rebound global, and live property replacement/accessors.
        // `super.name` — read an inherited property through the lexical home: a class
        // method via its home class, an object method via its runtime [[HomeObject]].
        if matches!(&m.object, ast::Expr::Super) {
            // MakeSuperPropertyReference: GetThisBinding() throws FIRST in a
            // derived ctor pre-super.
            self.this_check();
            let name = self.string_name(prop);
            if let Some(pid) = self.super_class {
                self.emit(Instr::SuperGet {
                    dst,
                    home_class_id: pid,
                    name,
                });
            } else if self.super_home_obj {
                self.emit(Instr::SuperGetObj { dst, name });
            } else {
                return Err("`super.x` is only valid in a method".into());
            }
            return Ok(dst);
        }
        let obj = self.expr(&m.object)?;
        if m.optional {
            self.emit_optional_check(obj);
        }
        let name = self.string_name(prop);
        self.emit(Instr::GetProp { dst, obj, name });
        Ok(dst)
    }

    pub(crate) fn computed_member(
        &mut self,
        m: &ast::Member,
        key_expr: &ast::Expr,
        dst: Reg,
    ) -> R<Reg> {
        // `super[expr]` — computed inherited-property read.
        if matches!(&m.object, ast::Expr::Super) {
            // GetThisBinding() throws BEFORE the key expression is evaluated.
            self.this_check();
            if let Some(pid) = self.super_class {
                let key = self.expr(key_expr)?;
                self.emit(Instr::SuperGetComputed {
                    dst,
                    home_class_id: pid,
                    key,
                });
            } else if self.super_home_obj {
                let key = self.expr(key_expr)?;
                self.emit(Instr::SuperGetObjComputed { dst, key });
            } else {
                return Err("`super[x]` is only valid in a method".into());
            }
            return Ok(dst);
        }
        let obj = self.expr(&m.object)?;
        if m.optional {
            self.emit_optional_check(obj);
        }
        // Fuse `obj[<plain string literal> + e]` → GetIndexConcat (no throwaway
        // concat-key heap allocation; see the opcode doc). The literal has no
        // side effects, so not emitting its LoadConst is unobservable; `e` is
        // still evaluated after `obj`, matching the unfused order.
        if let Some((name, rhs)) = concat_key_literal_prefix(key_expr) {
            let nidx = self.string_name(name);
            let key = self.expr(rhs)?;
            self.emit(Instr::GetIndexConcat {
                dst,
                obj,
                name: nidx,
                key,
            });
            return Ok(dst);
        }
        let key = self.expr(key_expr)?;
        self.emit(Instr::GetIndex { dst, obj, key });
        Ok(dst)
    }

    /// `obj.#field` → read the reserved "#field" property.
    pub(crate) fn private_member(&mut self, m: &ast::Member, prop: &str, dst: Reg) -> R<Reg> {
        self.check_private_declared(prop)?;
        let obj = self.expr(&m.object)?;
        if m.optional {
            self.emit_optional_check(obj);
        }
        let name = self.string_name(&private_key(prop));
        self.emit(Instr::GetProp { dst, obj, name });
        Ok(dst)
    }

    /// `?.` short-circuit: if `obj` is null/undefined (loose `== null`), jump to
    /// the enclosing chain's "undefined" block, recorded for patching at chain
    /// exit. No-op outside a chain (an `optional` flag can only appear in one).
    /// Emit `cond = (v === undefined) || (v === null)` — the SPEC nullish
    /// test. (LooseEq-against-undefined also matches an [[IsHTMLDDA]]
    /// object, which `??` / `??=` / `?.` must NOT treat as nullish.)
    pub(crate) fn emit_is_nullish(&mut self, v: Reg, cond: Reg, scratch: Reg) {
        self.emit(Instr::LoadUndefined { dst: scratch });
        self.emit(Instr::Eq {
            dst: cond,
            a: v,
            b: scratch,
        });
        let j = self.here();
        self.emit(Instr::JumpIfTrue { cond, target: 0 });
        self.emit(Instr::LoadNull { dst: scratch });
        self.emit(Instr::Eq {
            dst: cond,
            a: v,
            b: scratch,
        });
        let end = self.here();
        self.patch_jump(j, end);
    }

    pub(crate) fn emit_optional_check(&mut self, obj: Reg) {
        if self.chain_bails.is_empty() {
            return;
        }
        let save = self.next_reg;
        let nreg = self.alloc_reg();
        let cond = self.alloc_reg();
        self.emit_is_nullish(obj, cond, nreg);
        let jt = self.here();
        self.emit(Instr::JumpIfTrue { cond, target: 0 });
        self.chain_bails.last_mut().unwrap().push(jt);
        self.next_reg = save; // scratch temps dead after the check
    }

    /// Lower a PARENTHESIZED-chain member callee — `(a?.b)(…)` / `(a?.[k])(…)`
    /// — to `(callee, base)` registers so the call still binds `this` = base.
    /// The chain ends at the parens: a nullish base lands the CALLEE at
    /// undefined (its own bail boundary), and the call then throws (or an
    /// outer `?.()` bails). Returns None for non-member chain elements
    /// (call chains etc. — those stay value-calls). `inner` is the chain's
    /// wrapped expression (`Expr::Chain`'s payload).
    pub(crate) fn chain_member_callee(&mut self, inner: &ast::Expr) -> R<Option<(Reg, Reg)>> {
        let member = match inner {
            ast::Expr::Member(m) => m,
            _ => return Ok(None),
        };
        self.chain_bails.push(Vec::new());
        let res = self.capture_member_callee(member);
        let bails = self.chain_bails.pop().unwrap();
        let (callee, obj) = res?;
        if !bails.is_empty() {
            let jmp = self.here();
            self.emit(Instr::Jump { target: 0 });
            let undef_at = self.here();
            self.emit(Instr::LoadUndefined { dst: callee });
            let end = self.here();
            self.patch_jump(jmp, end);
            for b in bails {
                self.patch_jump(b, undef_at);
            }
        }
        Ok(Some((callee, obj)))
    }

    /// Compile an optional chain `a?.b…`: open a short-circuit boundary, compile
    /// the chain element (its `?.` links record bail jumps), then route any
    /// short-circuit to a single `undefined` result. `inner` is `Expr::Chain`'s
    /// wrapped expression.
    pub(crate) fn chain_expr(&mut self, inner: &ast::Expr, dst: Reg) -> R<Reg> {
        self.chain_bails.push(Vec::new());
        let res = match inner {
            // Static / computed / private links all live in one node now; the
            // private path's GetProp handles brand checks, and nested `?.` links
            // inside the object register their own bails.
            ast::Expr::Member(m) => self.member(m, dst),
            ast::Expr::Call(c) => self.call(c, dst),
            _ => Err("this optional-chain form is not in the zipp-vm subset yet".into()),
        };
        let bails = self.chain_bails.pop().unwrap();
        let v = res?;
        if bails.is_empty() {
            return Ok(v);
        }
        if v != dst {
            self.emit(Instr::Move { dst, src: v });
        }
        let jmp = self.here();
        self.emit(Instr::Jump { target: 0 });
        let undef_at = self.here();
        self.emit(Instr::LoadUndefined { dst });
        let end = self.here();
        self.patch_jump(jmp, end);
        for b in bails {
            self.patch_jump(b, undef_at);
        }
        Ok(dst)
    }

    /// `` tag`q0${e0}q1…` `` — call `tag(strings, e0, e1, …)` where `strings` is
    /// the array of cooked literal parts carrying a `.raw` array of the un-escaped
    /// parts. The tag is resolved exactly like a call reference before any
    /// substitution is evaluated, including its receiver when it is a member
    /// or a `with`-resolved identifier.
    pub(crate) fn tagged_template(
        &mut self,
        tag: &ast::Expr,
        quasi: &ast::TemplateLit,
        dst: Reg,
    ) -> R<Reg> {
        self.tagged_template_impl(tag, quasi, dst, false)
    }

    /// `return tag`…`` in a proper-tail-call position: same lowering with the
    /// `TailCall` frame-reuse prefix in front of the final plain `Call`.
    pub(crate) fn tagged_template_tail(
        &mut self,
        tag: &ast::Expr,
        quasi: &ast::TemplateLit,
        dst: Reg,
    ) -> R<Reg> {
        self.tagged_template_impl(tag, quasi, dst, true)
    }

    pub(crate) fn tagged_template_impl(
        &mut self,
        tag_expr: &ast::Expr,
        quasi: &ast::TemplateLit,
        dst: Reg,
        tail: bool,
    ) -> R<Reg> {
        let n = quasi.exprs.len();
        // Evaluate the tag (and its `this` for a member tag) first, into stable
        // registers that survive the argument block.
        enum Tag {
            Plain(Reg),
            Method { callee: Reg, this_v: Reg },
        }
        let tag = match tag_expr {
            // Static, computed, private and super tags all retain the reference
            // receiver, and complete the property Get before template-object
            // construction or substitution evaluation.
            ast::Expr::Member(m) => {
                let (callee, this_v) = self.capture_member_callee(m)?;
                Tag::Method { callee, this_v }
            }
            // `with (o) { tag`x` }` uses WithBaseObject for `this`, exactly as
            // the corresponding call expression does. The fallback path binds
            // undefined when no object environment carries the name.
            ast::Expr::Ident(id) => {
                let with_objs = self.with_obj_regs(id);
                if with_objs.is_empty() {
                    let callee = self.expr(tag_expr)?;
                    let callee_reg = self.alloc_reg();
                    if callee != callee_reg {
                        self.emit(Instr::Move {
                            dst: callee_reg,
                            src: callee,
                        });
                    }
                    Tag::Plain(callee_reg)
                } else {
                    let (callee, this_v) = self.emit_with_callee_chain(id, &with_objs);
                    Tag::Method { callee, this_v }
                }
            }
            _ => {
                let callee = self.expr(tag_expr)?;
                let callee_reg = self.alloc_reg();
                if callee != callee_reg {
                    self.emit(Instr::Move {
                        dst: callee_reg,
                        src: callee,
                    });
                }
                Tag::Plain(callee_reg)
            }
        };
        // The template object is memoized per source site: load the cache; on a hit
        // (a truthy template object) skip the build, else build + freeze + memoize so
        // every evaluation of THIS literal yields the same canonical frozen object.
        let strings_reg = self.alloc_reg();
        let site = self.template_site_count;
        self.template_site_count += 1;
        self.emit(Instr::TemplateGetCached {
            dst: strings_reg,
            site,
        });
        let skip = self.here();
        self.emit(Instr::JumpIfTrue {
            cond: strings_reg,
            target: 0,
        }); // patched: cache hit
        self.build_template_strings(quasi, strings_reg)?;
        self.emit(Instr::TemplateSetCached {
            site,
            src: strings_reg,
        });
        let after = self.here();
        self.patch_jump(skip, after);
        // Contiguous argument block: [strings, e0, e1, …].
        let arg_base = self.next_reg;
        for _ in 0..=n {
            self.alloc_reg();
        }
        let block_top = self.next_reg;
        self.emit(Instr::Move {
            dst: arg_base,
            src: strings_reg,
        });
        for (i, e) in quasi.exprs.iter().enumerate() {
            let slot = arg_base + 1 + i as Reg;
            let v = self.expr_into(e, slot)?;
            if v != slot {
                self.emit(Instr::Move { dst: slot, src: v });
            }
            self.next_reg = block_top;
        }
        let argc = (n + 1) as u16;
        match tag {
            Tag::Plain(callee) => {
                if tail {
                    // Proper tail call (`return f`…``): reuse the frame for a
                    // plain function tag; others fall through to the Call.
                    self.emit(Instr::TailCall {
                        callee,
                        arg_base,
                        argc,
                    });
                }
                self.emit(Instr::Call {
                    dst,
                    callee,
                    arg_base,
                    argc,
                })
            }
            Tag::Method { callee, this_v } => {
                if tail {
                    self.emit(Instr::TailCallWithThis {
                        callee,
                        this_v,
                        arg_base,
                        argc,
                    });
                }
                self.emit(Instr::CallWithThis {
                    dst,
                    callee,
                    this_v,
                    arg_base,
                    argc,
                })
            }
        }
        Ok(dst)
    }

    /// Build the tagged-template strings array `[q0,q1,…]` (cooked) into `dst`,
    /// with its `.raw` property set to the array of raw (un-escaped) parts.
    pub(crate) fn build_template_strings(&mut self, quasi: &ast::TemplateLit, dst: Reg) -> R<()> {
        let nq = quasi.quasis.len() as u16;
        let save = self.next_reg;
        // Cooked array → dst. A quasi with an ILLEGAL escape sequence has no cooked
        // value (`cooked` is None) — in a TAGGED template that element is
        // `undefined` (only the tag sees it; an untagged template would be a syntax
        // error), so load undefined rather than masking it as "".
        let cooked_base = self.next_reg;
        for q in &quasi.quasis {
            let r = self.alloc_reg();
            match q.cooked.as_ref() {
                Some(s) => {
                    // A cooked value holding a lone surrogate goes to the
                    // WTF-8-decoding constant slot. (Raw parts below are source
                    // text — never markers.)
                    let idx = self.str_const(s);
                    self.emit(Instr::LoadConst { dst: r, idx });
                }
                None => self.emit(Instr::LoadUndefined { dst: r }),
            }
        }
        self.emit(Instr::NewArray {
            dst,
            arg_base: cooked_base,
            argc: nq,
        });
        self.next_reg = save;
        // Raw array → a temp, then dst.raw = it.
        let raw_reg = self.alloc_reg();
        let raw_base = self.next_reg;
        for q in &quasi.quasis {
            let r = self.alloc_reg();
            let idx = self.add_string_const(&q.raw);
            self.emit(Instr::LoadConst { dst: r, idx });
        }
        self.emit(Instr::NewArray {
            dst: raw_reg,
            arg_base: raw_base,
            argc: nq,
        });
        self.emit(Instr::SetRaw {
            arr: dst,
            raw: raw_reg,
        });
        self.next_reg = save;
        Ok(())
    }

    pub(crate) fn array_literal(&mut self, elems: &[Option<ast::ArrayElem>], dst: Reg) -> R<Reg> {
        // The fixed-block `NewArray` form needs one CONTIGUOUS register per
        // element and passes the count as a `u16` argc, so it is only usable
        // for literals that actually fit the frame. A big literal (machine-
        // generated lookup tables are the real-world case) must take the
        // incremental path instead: `[70000 elements]` used to truncate its
        // count to `u16` (70000 -> 4464) and wrap `next_reg` back over live
        // registers, silently corrupting unrelated locals in the same frame.
        //
        // `ArrayAppend` needs a single scratch register regardless of length,
        // and pushes each value (including an explicit `LoadHole`) onto the
        // dense store, so it is semantically identical to `NewArray` — the
        // spread case below has always relied on that.
        const NEWARRAY_MAX_ELEMS: usize = 1024;
        let n_elems = elems.len();
        let block_fits = self.next_reg as usize + n_elems <= Reg::MAX as usize;
        let incremental = n_elems > NEWARRAY_MAX_ELEMS
            || !block_fits
            || elems
                .iter()
                .any(|e| matches!(e, Some(ast::ArrayElem::Spread(_))));
        // With a `...spread` element the final length is dynamic, so build the
        // array incrementally via ArrayAppend instead of the fixed-block NewArray.
        if incremental {
            self.emit(Instr::NewArray {
                dst,
                arg_base: self.next_reg,
                argc: 0,
            }); // []
            for el in elems {
                let save = self.next_reg;
                match el {
                    // A hole is `None`, and is NOT a present `undefined`.
                    None => {
                        let v = self.temp();
                        self.emit(Instr::LoadHole { dst: v });
                        self.emit(Instr::ArrayAppend {
                            arr: dst,
                            val: v,
                            spread: false,
                        });
                    }
                    Some(ast::ArrayElem::Spread(s)) => {
                        let v = self.expr(s)?;
                        self.emit(Instr::ArrayAppend {
                            arr: dst,
                            val: v,
                            spread: true,
                        });
                    }
                    Some(ast::ArrayElem::Expr(e)) => {
                        let v = self.expr(e)?;
                        self.emit(Instr::ArrayAppend {
                            arr: dst,
                            val: v,
                            spread: false,
                        });
                    }
                }
                self.next_reg = save;
            }
            return Ok(dst);
        }
        // Elements must occupy a contiguous register run for NewArray. Reserve
        // the block first (same contiguity discipline as call args) so an
        // element expression's scratch temps allocate above the block.
        // `n_elems <= NEWARRAY_MAX_ELEMS` here, so the cast cannot truncate.
        let count = n_elems as u16;
        let base = self.next_reg;
        for _ in elems {
            self.alloc_reg();
        }
        let block_top = self.next_reg;
        for (i, el) in elems.iter().enumerate() {
            let slot = base + i as Reg;
            match el {
                // A hole is `None`, and is NOT a present `undefined`.
                None => {
                    self.emit(Instr::LoadHole { dst: slot });
                }
                Some(ast::ArrayElem::Spread(_)) => unreachable!("handled above"),
                Some(ast::ArrayElem::Expr(e)) => {
                    let v = self.expr_into(e, slot)?;
                    if v != slot {
                        self.emit(Instr::Move { dst: slot, src: v });
                    }
                }
            }
            self.next_reg = block_top;
        }
        self.emit(Instr::NewArray {
            dst,
            arg_base: base,
            argc: count,
        });
        Ok(dst)
    }

    pub(crate) fn object_literal(&mut self, props: &[ast::ObjectMember], dst: Reg) -> R<Reg> {
        // Count the plain static data keys so the property vectors are sized
        // once, and decide which of them can skip `define`'s existence probe.
        // `appendable` goes false permanently at the first property that could
        // make a later key collide or reorder — a spread, a computed key, an
        // accessor, a `__proto__:` colon form, or a repeated static key. After
        // that everything falls back to `InitDataProp`, which is what runs today.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut appendable = true;
        let mut static_keys = 0usize;
        for prop in props {
            match prop {
                // A data property with a static key. `static_key_text` returns
                // None for a computed (or private) key, which ends the run just
                // as the old `!p.computed` guard did.
                ast::ObjectMember::Prop { key, shorthand, .. } => {
                    match static_key_text(key) {
                        Some(k) if k != "__proto__" || *shorthand => {
                            if !seen.insert(k) {
                                appendable = false; // duplicate key: the second one overwrites
                            } else {
                                static_keys += 1;
                            }
                        }
                        _ => appendable = false,
                    }
                }
                // A concise method with a static key. `__proto__` is an ordinary
                // own property in method form, so it does not end the run.
                ast::ObjectMember::Method { key, .. } => match static_key_text(key) {
                    Some(k) => {
                        if !seen.insert(k) {
                            appendable = false;
                        } else {
                            static_keys += 1;
                        }
                    }
                    None => appendable = false,
                },
                _ => appendable = false,
            }
            if !appendable {
                break;
            }
        }
        let all_appendable = appendable;
        let plan_enabled = crate::bytecode::static_key_plans_enabled();
        // `main_goal` is load-bearing, not a heuristic: a dynamically-installed
        // program's over-cap plan degradation rewrites planned allocations to
        // legacy hints and clears the plan pool, which a one-step finalize
        // site cannot survive. Only the VM's root program is exempt from that
        // path, so only it takes this lowering.
        if plan_enabled
            && self.cx.main_goal
            && crate::bytecode::object_finalize_enabled()
            && all_appendable
            && static_keys > 0
            && static_keys <= OBJECT_FINALIZE_MAX_FIELDS
        {
            return self.object_literal_finalized(props, dst, static_keys);
        }
        let alloc_ip = self.code.len();
        self.emit(Instr::NewObject {
            dst,
            hint: static_keys.min(u16::MAX as usize) as u16,
        });
        let mut plan_names = Vec::with_capacity(if plan_enabled {
            static_keys.min(STATIC_KEY_PLAN_MAX_FIELDS)
        } else {
            0
        });
        for prop in props {
            let save = self.next_reg;
            let planned_name = match prop {
                ast::ObjectMember::Get { key, func } => {
                    self.object_accessor(dst, key, func, false)?;
                    None
                }
                ast::ObjectMember::Set { key, func } => {
                    self.object_accessor(dst, key, func, true)?;
                    None
                }
                // NOTE: `init` (CoverInitializedName — the `= 1` in `{a = 1}`) is
                // ignored here on purpose. `({a = 1})` is a SyntaxError and
                // `({a = 1} = {})` is a destructuring TARGET, so the initializer
                // is either consumed by the target reinterpretation or raised as
                // an error by the parser, which is the only place that knows
                // which of the two this literal resolved as.
                ast::ObjectMember::Prop {
                    key,
                    value,
                    shorthand,
                    ..
                } => self.object_data_prop(
                    dst,
                    key,
                    PropVal::Expr(value),
                    false,
                    *shorthand,
                    all_appendable,
                )?,
                ast::ObjectMember::Method { key, func } => self.object_data_prop(
                    dst,
                    key,
                    PropVal::Func(func),
                    true,
                    false,
                    all_appendable,
                )?,
                ast::ObjectMember::Spread(s) => {
                    let src = self.expr(s)?;
                    self.emit(Instr::ObjectSpread { target: dst, src });
                    None
                }
            };
            if plan_enabled {
                if let Some(name) = planned_name {
                    collect_static_key_plan_name(&mut plan_names, name);
                }
            }
            self.next_reg = save; // reclaim this property's scratch temps
        }
        if plan_enabled && all_appendable && plan_names.len() == static_keys {
            self.install_static_key_plan(alloc_ip, dst, &plan_names);
        }
        Ok(dst)
    }

    /// Install a plan only after every property has compiled successfully. All
    /// arithmetic and pool widths are checked before mutating either counter or
    /// bytecode, so every decline leaves the legacy allocation intact.
    fn install_static_key_plan(&mut self, alloc_ip: usize, dst: Reg, names: &[u32]) {
        if let Some(plan) = self.try_install_static_key_plan(names) {
            self.code[alloc_ip] = Instr::NewPlannedObject { dst, plan };
        }
    }

    /// Reserve one static-key plan for `names`, honouring every site/byte cap.
    /// Returns the plan index, or `None` when a cap declines (the caller keeps
    /// legacy allocation). Shared by the retro-patching `NewPlannedObject` path
    /// and the one-step `FinalizeObject` lowering.
    fn try_install_static_key_plan(&mut self, names: &[u32]) -> Option<u16> {
        if names.is_empty()
            || names.len() > STATIC_KEY_PLAN_MAX_FIELDS
            || self.static_key_plans.len() >= u16::MAX as usize
            || self.cx.static_key_plan_sites >= crate::bytecode::STATIC_KEY_PLAN_COMPILER_MAX_SITES
        {
            return None;
        }
        let mut keys = Vec::with_capacity(names.len());
        let mut bytes = 0usize;
        for &name in names {
            let key = self.string_constants.get(name as usize)?;
            let next = bytes.checked_add(key.len())?;
            if next > STATIC_KEY_PLAN_MAX_BYTES {
                return None;
            }
            bytes = next;
            keys.push(key.clone());
        }
        let plan_charge = crate::bytecode::static_key_plan_retained_charge(&keys)?;
        let total_bytes = self
            .cx
            .static_key_plan_retained_bytes
            .checked_add(plan_charge)?;
        if total_bytes > crate::bytecode::STATIC_KEY_PLAN_MAX_RETAINED_BYTES {
            return None;
        }
        let plan = self.static_key_plans.len() as u16;
        self.static_key_plans
            .push(crate::bytecode::StaticKeyPlan::new(keys));
        self.cx.static_key_plan_sites += 1;
        self.cx.static_key_plan_retained_bytes = total_bytes;
        Some(plan)
    }

    /// One-step lowering for an all-appendable literal: stage every field value
    /// into a contiguous register block (the `NewArray` discipline — nested
    /// scratch allocates above the block, values land in their slots), then
    /// allocate-and-populate with a single `FinalizeObject`. Deferring the
    /// object's creation past every value expression is unobservable: the
    /// literal is unreachable until it completes, an abrupt value expression
    /// discards it either way, and staged registers are GC roots. Concise
    /// methods get their `[[HomeObject]]` wired immediately after the object
    /// exists, before any other instruction can run.
    fn object_literal_finalized(
        &mut self,
        props: &[ast::ObjectMember],
        dst: Reg,
        static_keys: usize,
    ) -> R<Reg> {
        let base = self.next_reg;
        for _ in 0..static_keys {
            self.alloc_reg();
        }
        let block_top = self.next_reg;
        let mut names = Vec::with_capacity(static_keys);
        let mut method_slots: Vec<Reg> = Vec::new();
        let mut slot = base;
        for prop in props {
            let name = match prop {
                ast::ObjectMember::Prop { key, value, .. } => {
                    self.object_finalize_prop(slot, key, PropVal::Expr(value), false)?
                }
                ast::ObjectMember::Method { key, func } => {
                    let name = self.object_finalize_prop(slot, key, PropVal::Func(func), true)?;
                    // B209: wire [[HomeObject]] only when the method (or a
                    // nested arrow / direct eval) can reference `super`. The
                    // just-compiled method proto is `functions.len() - 1`
                    // (nested protos finish and push first).
                    if self.func_can_ref_super(self.cx.functions.len() - 1) {
                        method_slots.push(slot);
                    }
                    name
                }
                _ => return Err("non-appendable member reached the finalize path".into()),
            };
            names.push(name);
            slot += 1;
            self.next_reg = block_top; // reclaim this value's scratch temps
        }
        if let Some(plan) = self.try_install_static_key_plan(&names) {
            self.emit(Instr::FinalizeObject {
                dst,
                plan,
                val_base: base,
                count: static_keys as u16,
            });
        } else {
            // Plan capacity declined: the staged block still serves the legacy
            // allocation+append sequence with identical semantics.
            self.emit(Instr::NewObject {
                dst,
                hint: static_keys.min(u16::MAX as usize) as u16,
            });
            for (i, &name) in names.iter().enumerate() {
                self.emit(Instr::AppendDataProp {
                    obj: dst,
                    name,
                    val: base + i as Reg,
                });
            }
        }
        for &method in &method_slots {
            self.emit(Instr::SetHomeObject { method, home: dst });
        }
        // The block deliberately stays allocated (exactly `NewArray`'s
        // discipline): the enclosing statement reclaims it. Immediate reuse
        // would let the next expression's temps redefine the staged slots,
        // which is what the SROA/materialization lanes prove never happens.
        Ok(dst)
    }

    /// Compile one static-key data property's VALUE into `slot`, preserving
    /// NamedEvaluation and concise-method semantics exactly as
    /// `object_data_prop` does, but emitting no append — the caller finalizes
    /// the whole literal in one step. Returns the interned key name.
    fn object_finalize_prop(
        &mut self,
        slot: Reg,
        key: &ast::PropKey,
        val: PropVal<'_>,
        is_method: bool,
    ) -> R<u32> {
        let key = static_key_text(key).ok_or("unsupported object key in the zipp-vm subset")?;
        let name = self.string_name(&key);
        // A concise method gets a [[HomeObject]] (for `super`); the flag scopes
        // exactly the value compilation, as in `object_data_prop`.
        if is_method {
            self.cx.obj_method_super = true;
        }
        let v = match val {
            PropVal::Expr(e) => self.compile_named_init(slot, e, &key)?,
            PropVal::Func(f) => {
                let n = match &f.name {
                    Some(own) => own.to_string(),
                    None => key.clone(),
                };
                let (id, has_up) = self.compile_func_expr(Some(n), f)?;
                self.emit_make_callable(slot, id, has_up);
                slot
            }
        };
        self.cx.obj_method_super = false;
        if is_method {
            let fid = self.cx.functions.len() - 1;
            self.cx.functions[fid].non_constructable = true; // concise method
        }
        if v != slot {
            self.emit(Instr::Move { dst: slot, src: v });
        }
        Ok(name)
    }

    /// A property's value into a fresh temp, exactly as `self.expr` would have
    /// produced it when a method/accessor was still a `FunctionExpression` value.
    pub(crate) fn prop_value(&mut self, val: PropVal<'_>) -> R<Reg> {
        match val {
            PropVal::Expr(e) => self.expr(e),
            PropVal::Func(f) => {
                let dst = self.temp();
                let (id, has_up) =
                    self.compile_func_expr(f.name.as_ref().map(|n| n.to_string()), f)?;
                self.emit_make_callable(dst, id, has_up);
                Ok(dst)
            }
        }
    }

    /// `{ get k(){…} }` / `{ set k(v){…} }` — an accessor property. The key is
    /// loaded into a register (computed expr or the static key string); a get+set
    /// pair on one key merges.
    pub(crate) fn object_accessor(
        &mut self,
        obj: Reg,
        key: &ast::PropKey,
        func: &ast::Function,
        is_setter: bool,
    ) -> R<()> {
        let key = match key {
            ast::PropKey::Computed(ke) => self.expr(ke)?,
            other => {
                let k = static_key_text(other)
                    .ok_or("unsupported accessor key in the zipp-vm subset")?;
                let kr = self.alloc_reg();
                let idx = self.add_string_const(&k);
                self.emit(Instr::LoadConst { dst: kr, idx });
                kr
            }
        };
        // An accessor is a method: it gets a [[HomeObject]], so `super`
        // inside it resolves via the object (set the transient flag the
        // function-body compiler consumes).
        self.cx.obj_method_super = true;
        let func = self.prop_value(PropVal::Func(func))?;
        self.cx.obj_method_super = false;
        // NOTE: `Function.prototype.toString` of an object accessor is the whole
        // `get k(){}` / `set k(v){}`. That used to be patched in here from the
        // ObjectProperty's span, because oxc's value-`Function` span omits the
        // `get`/`set` and the key. `ast::Function.span` IS the [[SourceText]]
        // range by definition, so `compile_func_expr` already records the right
        // text and there is nothing to patch — `ObjectMember` has no span, and
        // inventing one is exactly what rule 5 forbids. (Under the oxc bridge
        // this is a bridge-side fidelity gap, not a compiler one.)
        let fid = self.cx.functions.len() - 1;
        self.cx.functions[fid].non_constructable = true; // accessor = method
        self.emit(Instr::DefineAccessor {
            obj,
            key,
            func,
            is_setter,
        });
        // B209: elide the [[HomeObject]] wire for a super-free accessor.
        if self.func_can_ref_super(fid) {
            self.emit(Instr::SetHomeObject {
                method: func,
                home: obj,
            });
        }
        // SetFunctionName: a getter/setter is named "get k"/"set k"
        // (a Symbol key → "get [desc]"), at runtime so a computed key
        // is handled too.
        self.emit(Instr::SetFnNameFromKey {
            func,
            key,
            prefix: if is_setter { 2 } else { 1 },
        });
        Ok(())
    }

    /// B209: can `functions[fid]`'s body observe its [[HomeObject]]? True when
    /// the body — or, transitively, a nested ARROW body (arrows capture `super`
    /// lexically), or a direct-eval site in any of those (eval code may contain
    /// `super.x` in a method context) — references `super`. When false the
    /// internal slot is unobservable (no reflection API exposes it), so eliding
    /// `SetHomeObject` is exact: it removes a store barrier + `closure_home`
    /// table insert per method closure. Nested PLAIN functions and class
    /// bodies establish their own home (or none — `super` there is a parse
    /// error), so only `lexical_this` protos are traversed.
    fn func_can_ref_super(&self, fid: usize) -> bool {
        if !crate::bytecode::home_elide_enabled() {
            return true; // latch off: always wire, the pre-B209 behaviour
        }
        let mut stack = vec![fid];
        let mut seen = std::collections::HashSet::new();
        while let Some(f) = stack.pop() {
            if !seen.insert(f) {
                continue;
            }
            let proto = &self.cx.functions[f];
            if !proto.eval_sites.is_empty() {
                return true;
            }
            for i in &proto.code {
                match *i {
                    Instr::SuperCtorFetch { .. }
                    | Instr::SuperCtor { .. }
                    | Instr::SuperCtorSpread { .. }
                    | Instr::SuperBase { .. }
                    | Instr::SuperMethod { .. }
                    | Instr::SuperGet { .. }
                    | Instr::SuperGetComputed { .. }
                    | Instr::SuperMethodComputed { .. }
                    | Instr::SuperSet { .. }
                    | Instr::SuperSetComputed { .. }
                    | Instr::SuperGetObj { .. }
                    | Instr::SuperGetObjComputed { .. }
                    | Instr::SuperSetObj { .. }
                    | Instr::SuperSetObjComputed { .. }
                    | Instr::SuperMethodObj { .. }
                    | Instr::SuperMethodObjComputed { .. }
                    | Instr::SuperMethodSpread { .. }
                    | Instr::SuperMethodComputedSpread { .. } => return true,
                    Instr::MakeFunc { func_id, .. }
                    | Instr::MakeClosure { func_id, .. }
                    | Instr::MakeArrow { func_id, .. } => {
                        let n = func_id as usize;
                        if self.cx.functions[n].lexical_this {
                            stack.push(n);
                        }
                    }
                    _ => {}
                }
            }
        }
        false
    }

    /// A data property or concise method: `{k: v}`, `{[k]: v}`, `{m(){}}`,
    /// `{[k](){}}`.
    pub(crate) fn object_data_prop(
        &mut self,
        obj: Reg,
        key: &ast::PropKey,
        val: PropVal<'_>,
        is_method: bool,
        shorthand: bool,
        all_appendable: bool,
    ) -> R<Option<u32>> {
        // IsAnonymousFunctionDefinition of the value: a method's function is
        // anonymous unless it carries its own name (the property key is not one).
        let anonymous = match val {
            PropVal::Expr(e) => is_anonymous_fn_def(e),
            PropVal::Func(f) => f.name.is_none(),
        };
        if let ast::PropKey::Computed(ke) = key {
            // Computed key `{[expr]: v}` → CreateDataProperty with a
            // runtime key: ToPropertyKey runs BEFORE the value
            // evaluates (its coercion side effects order first), and
            // a computed "__proto__" defines an ORDINARY own
            // property (only the textual colon form sets the proto).
            let raw = self.expr(ke)?;
            let key = self.alloc_reg();
            self.emit(Instr::ToPropKey {
                dst: key,
                obj,
                src: raw,
            });
            // A computed concise method gets a [[HomeObject]] (for super).
            if is_method {
                self.cx.obj_method_super = true;
            }
            let v = self.prop_value(val)?;
            self.cx.obj_method_super = false;
            // A computed concise method `{ [expr](){} }` (incl. `*`/`async`) is
            // non-constructable. Its toString is the whole `[expr](){}`, which
            // `Function.span` already covers — see the NOTE in `object_accessor`
            // for why the old span patch is gone.
            if is_method {
                let fid = self.cx.functions.len() - 1;
                self.cx.functions[fid].non_constructable = true;
            }
            // SetFunctionName: an anonymous function/arrow/class value
            // takes the (runtime) computed key as its name — a Symbol key
            // becomes "[description]".
            if anonymous {
                self.emit(Instr::SetFnNameFromKey {
                    func: v,
                    key,
                    prefix: 0,
                });
            }
            self.emit(Instr::InitDataPropDyn { obj, key, val: v });
            // B209: only a method that can reference `super` needs the wire.
            if is_method && self.func_can_ref_super(self.cx.functions.len() - 1) {
                self.emit(Instr::SetHomeObject {
                    method: v,
                    home: obj,
                });
            }
            return Ok(None);
        }
        // Static identifier / string / number literal key.
        let key = static_key_text(key).ok_or("unsupported object key in the zipp-vm subset")?;
        let name = self.string_name(&key);
        // `{ fn: function(){}, m(){}, C: class{} }` — an anonymous
        // value function/class takes the property key as its name,
        // EXCEPT `{ __proto__: fn }` (a proto-setter, not a data
        // property): its function value stays anonymous.
        let vtmp = self.alloc_reg();
        // A concise method gets a [[HomeObject]] (for `super`); a plain
        // `k: function(){}` data property does NOT.
        if is_method {
            self.cx.obj_method_super = true;
        }
        // `{ __proto__: v }` — the colon form ONLY (shorthand `{ __proto__ }` and
        // the method form are ordinary data properties).
        let is_proto = key == "__proto__" && !is_method && !shorthand;
        let v = match val {
            PropVal::Expr(e) if is_proto => self.expr_into(e, vtmp)?,
            PropVal::Expr(e) => self.compile_named_init(vtmp, e, &key)?,
            // NamedEvaluation for a concise method, which `compile_named_init`
            // used to do via the anonymous-FunctionExpression arm.
            PropVal::Func(f) => {
                let n = match &f.name {
                    Some(own) => own.to_string(),
                    None => key.clone(),
                };
                let (id, has_up) = self.compile_func_expr(Some(n), f)?;
                self.emit_make_callable(vtmp, id, has_up);
                vtmp
            }
        };
        self.cx.obj_method_super = false;
        // A shorthand method `{ m(){}, *g(){}, async a(){} }` is non-constructable;
        // its toString is the whole `m(){}`, which `Function.span` already covers
        // (see the NOTE in `object_accessor`). A regular `k: function(){}` keeps
        // the value's own span and is constructable.
        if is_method {
            let fid = self.cx.functions.len() - 1;
            self.cx.functions[fid].non_constructable = true; // concise method
        }
        // `{ __proto__: v }` sets the prototype through the B.3.1 SPECIAL FORM —
        // a direct [[SetPrototypeOf]], NOT a [[Set]] of the key, so it is
        // unaffected by `delete Object.prototype.__proto__` or a replacement
        // `set __proto__` accessor. Every other key is CreateDataProperty, which
        // must ignore an inherited accessor / non-writable prop.
        if is_proto {
            self.emit(Instr::SetLiteralProto { obj, val: v });
        } else if all_appendable {
            self.emit(Instr::AppendDataProp { obj, name, val: v });
        } else {
            self.emit(Instr::InitDataProp { obj, name, val: v });
        }
        // B209: only a method that can reference `super` needs the wire.
        if is_method && self.func_can_ref_super(self.cx.functions.len() - 1) {
            self.emit(Instr::SetHomeObject {
                method: v,
                home: obj,
            });
        }
        Ok((!is_proto && all_appendable).then_some(name))
    }

    pub(crate) fn load_number(&mut self, dst: Reg, n: f64) {
        if n.fract() == 0.0 && n >= i32::MIN as f64 && n <= i32::MAX as f64 {
            self.emit(Instr::LoadInt { dst, val: n as i32 });
        } else {
            let idx = self.add_const(Value::num(n));
            self.emit(Instr::LoadConst { dst, idx });
        }
    }

    /// Compile the OPERAND of a `typeof`, without the `TypeOf` itself.
    ///
    /// `typeof <unbound identifier>` must yield "undefined", NOT throw a
    /// ReferenceError — and this holds when the identifier is wrapped in
    /// parentheses (`typeof (f)`), which is no longer a node, so the operand IS
    /// the identifier. A bare identifier that resolves to a global is read with
    /// the non-throwing variant so the never-declared sentinel degrades to
    /// undefined. (`undefined`/`NaN`/`Infinity` go through the normal identifier
    /// resolver; only a proven-static global fallback becomes a constant.)
    /// Factored so the bare `typeof x` and the fused
    /// `typeof x === "lit"` compile the operand IDENTICALLY.
    fn typeof_operand(&mut self, arg: &ast::Expr, dst: Reg) -> R<Reg> {
        if let ast::Expr::Ident(id) = arg {
            let n: &str = id;
            if !matches!(n, "undefined" | "NaN" | "Infinity") {
                if let Binding::Global(idx) = self.resolve(n) {
                    // A DECLARED top-level lexical still observes its TDZ
                    // through typeof (a ReferenceError) — only a name the
                    // compiler never saw declared degrades to "undefined" via
                    // the non-throwing load.
                    let declared_lexical = self.cx.lexical_globals.contains(&(idx as u32))
                        || self.cx.const_globals.contains(&(idx as u32));
                    if declared_lexical {
                        self.emit(Instr::LoadGlobal { dst, idx });
                    } else if self.box_all_locals || self.cx.dyn_global_zone {
                        self.emit(Instr::LoadGlobalOrUndefinedDyn { dst, idx });
                    } else {
                        self.emit(Instr::LoadGlobalOrUndefined { dst, idx });
                    }
                    return Ok(dst);
                }
            }
        }
        self.expr(arg)
    }

    /// Whether delaying a `typeof` classification until after the other
    /// operand's evaluation is unobservable. A plain local/cell read cannot
    /// run user code, and `CellGet` snapshots the contained Value. Keep every
    /// expression, global/upvalue lookup, and `with`-shadowable name on the
    /// ordinary two-`TypeOf` lowering: its RHS may run user code or mutate an
    /// aliased binding before this combined opcode classifies the LHS.
    fn typeof_same_stable_local(&mut self, arg: &ast::Expr) -> bool {
        let ast::Expr::Ident(id) = arg else {
            return false;
        };
        self.with_objs_for(id).is_empty()
            && matches!(self.resolve(id), Binding::Local(_) | Binding::LocalCell(_))
    }

    /// Compile a branch TEST and emit the jump taken when it is FALSY,
    /// returning the jump's ip for `patch_jump`. A test that is a bare
    /// `a < b` / `a <= b` fuses into `JumpIfNotLt`/`JumpIfNotLe`: the fused
    /// interpreter arm runs the same `cmp_lt`/`cmp_le` as the unfused pair
    /// (operand evaluation, coercion order and NaN behaviour identical by
    /// construction), and the boolean — a freshly allocated temp whose only
    /// consumer would be this jump — is never materialised. Any other test
    /// shape (including `>`/`>=`: the fused ops carry no swap flag, and
    /// swapping operands would reorder the two ToPrimitive coercions) keeps
    /// the generic `expr` + `JumpIfFalse` pair.
    pub(crate) fn emit_test_jump(&mut self, test: &ast::Expr) -> R<u32> {
        if fused_cmp_jump_enabled() {
            if let ast::Expr::Binary { op, left, right } = test {
                if matches!(op, ast::BinaryOp::Lt | ast::BinaryOp::LtEq) {
                    let a = self.expr(left)?;
                    let b = self.expr(right)?;
                    let j = self.here();
                    match op {
                        ast::BinaryOp::Lt => self.emit(Instr::JumpIfNotLt { a, b, target: 0 }),
                        _ => self.emit(Instr::JumpIfNotLe { a, b, target: 0 }),
                    }
                    return Ok(j);
                }
            }
        }
        let cond = self.expr(test)?;
        let j = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 });
        Ok(j)
    }

    /// Compile a TEST whose value is consumed only for truthiness and return
    /// every jump taken when it is falsy.  `a && b && c` is falsy as soon as
    /// any operand is falsy, so the control-flow form can branch after each
    /// operand instead of materialising the two intermediate values that
    /// `logical()` must preserve when `&&` is used as an expression.
    ///
    /// Evaluation remains exactly left-to-right and short-circuiting: each
    /// returned jump is patched to the caller's one false successor.  `||`
    /// and `??` deliberately retain their ordinary lowering because their
    /// left-success path needs a second patch list; this first step is both
    /// narrow and sufficient for the parser/tokenizer-shaped hot loops.
    pub(crate) fn emit_test_jumps(&mut self, test: &ast::Expr) -> R<Vec<u32>> {
        if let ast::Expr::Logical {
            op: ast::LogicalOp::And,
            left,
            right,
        } = test
        {
            let mut jumps = self.emit_test_jumps(left)?;
            jumps.extend(self.emit_test_jumps(right)?);
            return Ok(jumps);
        }
        Ok(vec![self.emit_test_jump(test)?])
    }

    /// W11 (B124) n-ary string-concat chain fusion: emit a flattened
    /// `L1+L2+…+Ln` (n≥3) as `acc = Add(L1,L2)` then, per remaining leaf,
    /// `StrConcatChain{dst:acc, a:acc, b:leaf}`, and a final `Move` into the
    /// caller's `dst`. Leaf EVALUATION stays exactly where the pairwise tree
    /// put it (E1 E2 C1 C2 | E3 C3 | …), so every observable — a call leaf's
    /// side effects, an object operand's ToPrimitive, a Symbol/BigInt-mix
    /// TypeError's position, a throw mid-chain — is bit-identical to the
    /// unfused emission. What changes is only the combine op: links 2.. may
    /// grow the accumulator (a dead fresh temp) in place instead of
    /// re-allocating per level. Link 1 stays a plain `Add` so a `LoadConst`'d
    /// (JIT-shared) string constant is never the in-place accumulator.
    ///
    /// The trailing `Move` (rather than pointing the last link at `dst`) is
    /// LOAD-BEARING twice over: (a) every `StrConcatChain` dst is then the
    /// `acc` temp, which the same region always also defines via the link-1
    /// `Add` — so write-cover scans that don't enumerate the new op (e.g. the
    /// TA-pin plan's `writes()`) still find a recognised writer and stay
    /// conservative; (b) a var-destination chain (`x = …` / template into a
    /// declared var) never exposes a partial intermediate through `dst`.
    fn concat_chain(&mut self, leaves: &[&ast::Expr], dst: Reg) -> R<Reg> {
        let acc = self.temp();
        let save = self.next_reg; // == acc + 1: leaf temps roll back to here
        let a = self.expr(leaves[0])?;
        let b = self.expr(leaves[1])?;
        self.emit(Instr::Add { dst: acc, a, b });
        self.next_reg = save;
        for leaf in &leaves[2..] {
            let r = self.expr(leaf)?;
            self.emit(Instr::StrConcatChain {
                dst: acc,
                a: acc,
                b: r,
            });
            // Per-link rollback keeps the live-temp footprint flat (~2 above
            // `acc`) however long the chain — never below `acc` while the
            // chain is live (the in-place licence depends on `acc`'s slot
            // staying untouched between links).
            self.next_reg = save;
        }
        self.emit(Instr::Move { dst, src: acc });
        // `acc` is dead after the Move; reclaim it (guarding a high `dst`,
        // following the `calls.rs` `save.max(dst + 1)` precedent).
        self.next_reg = acc.max(dst + 1);
        Ok(dst)
    }

    pub(crate) fn binary(
        &mut self,
        op: ast::BinaryOp,
        left: &ast::Expr,
        right: &ast::Expr,
        dst: Reg,
    ) -> R<Reg> {
        use ast::BinaryOp as Op;
        // `typeof a === typeof b` (and every equality polarity) compares two
        // members of the same fixed eight-name domain. Evaluate both operands
        // in source order, then classify them in one total opcode. This is
        // observably identical to materialising the two primitive strings:
        // equality between strings is exactly equality between their names.
        if typeof_same_enabled()
            && matches!(op, Op::StrictEq | Op::StrictNotEq | Op::Eq | Op::NotEq)
        {
            if let (
                ast::Expr::Unary {
                    op: ast::UnaryOp::Typeof,
                    arg: left_arg,
                },
                ast::Expr::Unary {
                    op: ast::UnaryOp::Typeof,
                    arg: right_arg,
                },
            ) = (left, right)
            {
                // The combined opcode classifies both raw Values after their
                // evaluation. Only fuse reads for which the intervening RHS
                // evaluation cannot change the LHS classification. In
                // particular, `typeof x === typeof (x = 1)` must classify the
                // old x before performing the assignment.
                if self.typeof_same_stable_local(left_arg)
                    && self.typeof_same_stable_local(right_arg)
                {
                    let a_dst = self.temp();
                    let a = self.typeof_operand(left_arg, a_dst)?;
                    let b_dst = self.temp();
                    let b = self.typeof_operand(right_arg, b_dst)?;
                    let neg = matches!(op, Op::StrictNotEq | Op::NotEq);
                    self.emit(Instr::TypeOfSame { dst, a, b, neg });
                    return Ok(dst);
                }
            }
        }
        // ── `t === "lit"` where `t` is a live `typeof v` alias → TypeOfIs over
        // `v` (see `typeof_alias.rs`). `t` is a plain local holding a typeof
        // name, so the loose forms agree with the strict ones here too, and a
        // literal outside the eight names compiles to the never-matching 255.
        if matches!(op, Op::StrictEq | Op::StrictNotEq | Op::Eq | Op::NotEq) {
            let aliased = match (left, right) {
                (ast::Expr::Ident(id), ast::Expr::Str(StrVal::Utf8(lit)))
                | (ast::Expr::Str(StrVal::Utf8(lit)), ast::Expr::Ident(id)) => Some((id, lit)),
                _ => None,
            };
            if let Some((id, lit)) = aliased {
                if let Some(a) = self.typeof_alias_lookup(id) {
                    let neg = matches!(op, Op::StrictNotEq | Op::NotEq);
                    let code = crate::bytecode::typeof_code(lit).unwrap_or(255);
                    self.emit(Instr::TypeOfIs { dst, a, code, neg });
                    return Ok(dst);
                }
            }
        }
        // ── `typeof x === "lit"` → TypeOfIs ── (also `!==`, and the loose
        // forms, which agree with strict when both sides are strings — one side
        // is a string literal and `typeof` always produces a string). The
        // unfused pair allocates a heap string per evaluation and then
        // content-compares it; the fused op compares the classifier's
        // `&'static str` and allocates nothing. A literal outside the eight
        // possible results fuses as code 255 (never matches) so the operand's
        // effects — including the non-throwing undeclared-global read — are
        // preserved. Only a plain-Utf8 literal fuses: a lone-surrogate literal
        // (`StrVal::Utf16`) cannot equal any typeof result but needs its
        // special constant slot, so it keeps the generic path.
        if matches!(op, Op::StrictEq | Op::StrictNotEq | Op::Eq | Op::NotEq) {
            let fused = match (left, right) {
                (
                    ast::Expr::Unary {
                        op: ast::UnaryOp::Typeof,
                        arg,
                    },
                    ast::Expr::Str(StrVal::Utf8(lit)),
                ) => Some((arg, lit)),
                (
                    ast::Expr::Str(StrVal::Utf8(lit)),
                    ast::Expr::Unary {
                        op: ast::UnaryOp::Typeof,
                        arg,
                    },
                ) => Some((arg, lit)),
                _ => None,
            };
            if let Some((arg, lit)) = fused {
                let neg = matches!(op, Op::StrictNotEq | Op::NotEq);
                let code = crate::bytecode::typeof_code(lit).unwrap_or(255);
                let a = self.typeof_operand(arg, dst)?;
                self.emit(Instr::TypeOfIs { dst, a, code, neg });
                return Ok(dst);
            }
        }
        // `x instanceof Ctor`: both operands are evaluated left-to-right and the
        // LIVE RHS value governs @@hasInstance / OrdinaryHasInstance.  Never
        // select a constructor merely from its identifier spelling: that skips
        // the RHS read entirely and miscompiles shadowed, rebound, cross-realm,
        // proxy and @@hasInstance-bearing constructors.
        if matches!(op, Op::Instanceof) {
            let val = self.expr(left)?;
            let ctor = self.expr(right)?;
            self.emit(Instr::InstanceOfDyn { dst, val, ctor });
            return Ok(dst);
        }
        // `key in obj`.
        if matches!(op, Op::In) {
            let key = self.expr(left)?;
            let obj = self.expr(right)?;
            self.emit(Instr::HasProp {
                dst,
                key,
                obj,
                brand: false,
            });
            return Ok(dst);
        }
        // `a - <int literal>` and `a + <int literal>` → AddInt fast path, but
        // ONLY when the left operand is provably numeric. `+` is overloaded for
        // string concatenation, so `'n=' + 42` must NOT take the integer path
        // (it would coerce the string to NaN). Subtraction is always numeric,
        // so it's always eligible; addition is eligible only when `left` is a
        // numeric literal or another arithmetic expression we just produced a
        // number from. When unsure, fall through to the generic `Add`, which
        // handles string concatenation correctly.
        if let ast::Expr::Num(n) = right {
            let imm_ok = n.fract() == 0.0 && *n >= i32::MIN as f64 && *n <= i32::MAX as f64;
            // `x - 0` must stay a real Sub: AddInt would compute `x + 0`, and
            // IEEE `-0.0 + 0.0` is `+0.0` while `-0.0 - 0.0` is `-0.0`.
            // (`x + 0` is the same operation either way, so it stays eligible.)
            let eligible = (matches!(op, Op::Sub) && *n != 0.0)
                || (matches!(op, Op::Add) && is_numeric_expr(left));
            if imm_ok && eligible {
                let a = self.expr(left)?;
                let mut imm = *n as i32;
                if matches!(op, Op::Sub) {
                    imm = -imm;
                }
                self.emit(Instr::AddInt {
                    dst,
                    a,
                    imm,
                    upd: false,
                });
                return Ok(dst);
            }
        }
        // `pad2(n) { return n < 10 ? "0" + n : "" + n; }`: the literal
        // evaluation is unobservable, and Pad2Concat retains an exact ordinary
        // `+` fallback for every non-eligible runtime value. Keep the pattern
        // deliberately literal-left and two-leaf: swapping operands would
        // change string output and accepting an expression prefix could change
        // evaluation/ToPrimitive order.
        if matches!(op, Op::Add) && pad2_cache_enabled() {
            let zero = match left {
                ast::Expr::Str(StrVal::Utf8(s)) if s == "0" => Some(true),
                ast::Expr::Str(StrVal::Utf8(s)) if s.is_empty() => Some(false),
                _ => None,
            };
            if let Some(zero) = zero {
                let src = self.expr(right)?;
                self.emit(Instr::Pad2Concat { dst, src, zero });
                return Ok(dst);
            }
        }
        // ── W11 (B124) n-ary concat-chain fusion ── a ≥3-leaf `+` spine with
        // at least one syntactic string producer (Str literal / template) is a
        // string-concat chain: fuse it (see `concat_chain`). Pure-numeric
        // chains keep the pairwise `Add`s so INT/GPR/DV region admission is
        // unchanged. `ZIPP_NO_CONCAT_FUSE=1` restores the pairwise emission
        // bit-for-bit. (This sits AFTER the AddInt fast path, which never
        // fires on an Add spine — `is_numeric_expr` rejects `Binary::Add`.)
        if matches!(op, Op::Add) && concat_fuse_enabled() {
            let mut leaves: Vec<&ast::Expr> = Vec::new();
            add_spine(left, &mut leaves);
            leaves.push(right);
            if leaves.len() >= 3
                && leaves
                    .iter()
                    .any(|l| matches!(l, ast::Expr::Str(_) | ast::Expr::Template(_)))
            {
                return self.concat_chain(&leaves, dst);
            }
        }
        // A completed expression exposes only its returned value register. Any
        // callee/argument/member/literal scratch allocated while producing it is
        // dead before Evaluate(RHS) begins, so reclaim that suffix while keeping
        // both the pre-existing outer register floor and a newly allocated LHS
        // result. `saturating_add` preserves alloc_reg's clean overflow path.
        let left_floor = self.next_reg;
        let a = self.expr(left)?;
        self.next_reg = left_floor.max(a.saturating_add(1));
        let r = self.expr(right)?;
        let instr = match op {
            Op::Add => Instr::Add { dst, a, b: r },
            Op::Sub => Instr::Sub { dst, a, b: r },
            Op::Mul => Instr::Mul { dst, a, b: r },
            Op::Div => Instr::Div { dst, a, b: r },
            Op::Rem => Instr::Mod { dst, a, b: r },
            Op::Lt => Instr::Lt { dst, a, b: r },
            Op::LtEq => Instr::Le { dst, a, b: r },
            Op::Gt => Instr::Gt { dst, a, b: r },
            Op::GtEq => Instr::Ge { dst, a, b: r },
            Op::StrictEq => Instr::Eq { dst, a, b: r },
            Op::StrictNotEq => Instr::Ne { dst, a, b: r },
            Op::Eq => Instr::LooseEq { dst, a, b: r },
            Op::NotEq => Instr::LooseNe { dst, a, b: r },
            Op::BitAnd => Instr::Bitwise {
                dst,
                a,
                b: r,
                op: BitwiseOp::And,
            },
            Op::BitOr => Instr::Bitwise {
                dst,
                a,
                b: r,
                op: BitwiseOp::Or,
            },
            Op::BitXor => Instr::Bitwise {
                dst,
                a,
                b: r,
                op: BitwiseOp::Xor,
            },
            Op::Shl => Instr::Bitwise {
                dst,
                a,
                b: r,
                op: BitwiseOp::Shl,
            },
            Op::Shr => Instr::Bitwise {
                dst,
                a,
                b: r,
                op: BitwiseOp::Shr,
            },
            Op::UShr => Instr::Bitwise {
                dst,
                a,
                b: r,
                op: BitwiseOp::Ushr,
            },
            Op::Exp => Instr::Pow { dst, a, b: r },
            // Both are handled above; kept explicit so a new operator breaks the
            // build instead of falling into a catch-all.
            Op::In | Op::Instanceof => {
                return Err("unsupported binary operator (zipp-vm v1)".into());
            }
        };
        self.emit(instr);
        Ok(dst)
    }

    pub(crate) fn logical(
        &mut self,
        op: ast::LogicalOp,
        left: &ast::Expr,
        right: &ast::Expr,
        dst: Reg,
    ) -> R<Reg> {
        use ast::LogicalOp as Op;
        // `a && b`: eval a into dst; if falsy, short-circuit; else eval b.
        let _a = self.expr_into(left, dst)?;
        if _a != dst {
            self.emit(Instr::Move { dst, src: _a });
        }
        match op {
            Op::And => {
                let j = self.here();
                self.emit(Instr::JumpIfFalse {
                    cond: dst,
                    target: 0,
                });
                let b = self.expr_into(right, dst)?;
                if b != dst {
                    self.emit(Instr::Move { dst, src: b });
                }
                let end = self.here();
                self.patch_jump(j, end);
            }
            Op::Or => {
                let j = self.here();
                self.emit(Instr::JumpIfTrue {
                    cond: dst,
                    target: 0,
                });
                let b = self.expr_into(right, dst)?;
                if b != dst {
                    self.emit(Instr::Move { dst, src: b });
                }
                let end = self.here();
                self.patch_jump(j, end);
            }
            Op::Coalesce => {
                // `a ?? b`: keep `a` unless it is STRICTLY null/undefined.
                let save = self.next_reg;
                let undef = self.alloc_reg();
                let isnull = self.alloc_reg();
                self.emit_is_nullish(dst, isnull, undef);
                let j = self.here();
                self.emit(Instr::JumpIfFalse {
                    cond: isnull,
                    target: 0,
                }); // non-nullish → keep dst
                self.next_reg = save; // the nullish-test temps are dead now
                let b = self.expr_into(right, dst)?;
                if b != dst {
                    self.emit(Instr::Move { dst, src: b });
                }
                let end = self.here();
                self.patch_jump(j, end);
            }
        }
        Ok(dst)
    }

    pub(crate) fn unary(&mut self, op: ast::UnaryOp, arg: &ast::Expr, dst: Reg) -> R<Reg> {
        use ast::UnaryOp as Op;
        match op {
            Op::Minus => {
                let a = self.expr(arg)?;
                self.emit(Instr::Neg { dst, a });
                Ok(dst)
            }
            Op::Plus => {
                let a = self.expr(arg)?;
                self.emit(Instr::ToNum { dst, a });
                Ok(dst)
            }
            Op::Not => {
                let a = self.expr(arg)?;
                self.emit(Instr::Not { dst, a });
                Ok(dst)
            }
            Op::Typeof => {
                let a = self.typeof_operand(arg, dst)?;
                self.emit(Instr::TypeOf { dst, a });
                Ok(dst)
            }
            Op::BitNot => {
                let a = self.expr(arg)?;
                self.emit(Instr::BitNot { dst, a });
                Ok(dst)
            }
            Op::Void => {
                // Evaluate the operand for side effects; the value is `undefined`.
                let _ = self.expr(arg)?;
                self.emit(Instr::LoadUndefined { dst });
                Ok(dst)
            }
            Op::Delete => self.delete_expr(arg, dst),
        }
    }

    /// `delete <ref>` — remove a property (`obj.x` / `obj[k]`) and yield the
    /// boolean result. A non-reference operand (or a bare identifier) evaluates
    /// for side effects and yields `true` (matching sloppy-mode `delete x`).
    pub(crate) fn delete_expr(&mut self, arg: &ast::Expr, dst: Reg) -> R<Reg> {
        match arg {
            ast::Expr::Member(m) => match &m.prop {
                ast::MemberProp::Ident(prop) => {
                    // `delete super.x` is a runtime ReferenceError (a super reference has
                    // no [[Delete]]). Not a SyntaxError, so it's thrown when evaluated.
                    if matches!(&m.object, ast::Expr::Super) {
                        let e = self.alloc_reg();
                        self.emit(Instr::NewError {
                            dst: e,
                            kind: 4,
                            arg: None,
                            opts: None,
                            errors: None,
                        });
                        self.emit(Instr::Throw { src: e });
                        return Ok(dst);
                    }
                    let obj = self.expr(&m.object)?;
                    let name = self.string_name(prop);
                    let strict = self.cx.in_strict;
                    self.emit(Instr::DeleteProp {
                        dst,
                        obj,
                        name,
                        strict,
                    });
                    Ok(dst)
                }
                ast::MemberProp::Computed(ke) => {
                    // `delete super[expr]`: SuperProperty evaluation does
                    // GetThisBinding BEFORE the key expression — in a derived ctor
                    // before super() that ReferenceError fires FIRST and `expr`
                    // never runs. Otherwise evaluate `expr` (side effects +
                    // ToPropertyKey), then throw a ReferenceError — a super
                    // reference has no delete.
                    if matches!(&m.object, ast::Expr::Super) {
                        self.this_check();
                        let _ = self.expr(ke)?;
                        let e = self.alloc_reg();
                        self.emit(Instr::NewError {
                            dst: e,
                            kind: 4,
                            arg: None,
                            opts: None,
                            errors: None,
                        });
                        self.emit(Instr::Throw { src: e });
                        return Ok(dst);
                    }
                    let obj = self.expr(&m.object)?;
                    let strict = self.cx.in_strict;
                    // Fuse `delete obj[<plain string literal> + e]` → DeleteIndexConcat
                    // (no throwaway concat-key allocation; see GetIndexConcat).
                    if let Some((name, rhs)) = concat_key_literal_prefix(ke) {
                        let nidx = self.string_name(name);
                        let key = self.expr(rhs)?;
                        self.emit(Instr::DeleteIndexConcat {
                            dst,
                            obj,
                            name: nidx,
                            key,
                            strict,
                        });
                        return Ok(dst);
                    }
                    let key = self.expr(ke)?;
                    self.emit(Instr::DeleteIndex {
                        dst,
                        obj,
                        key,
                        strict,
                    });
                    Ok(dst)
                }
                // `delete obj.#x` is a SyntaxError the parser rejects; if one ever
                // reaches here it takes the generic-operand path below (evaluate
                // for side effects, yield `true`), exactly as it did when a
                // private field was its own node this match did not name.
                ast::MemberProp::Private(_) => {
                    let _ = self.expr(arg)?;
                    self.emit(Instr::LoadBool { dst, val: true });
                    Ok(dst)
                }
            },
            // `delete a?.b` / `delete a?.[k]`: the chain IS a reference — evaluate
            // the object (nested `?.` links bail to `true`), short-circuit to
            // `true` on a nullish base, otherwise delete the property for real.
            // A chain ending in a call (`delete a?.()`) is not a reference and
            // takes the generic evaluate-and-`true` path.
            ast::Expr::Chain(inner) => match &**inner {
                ast::Expr::Member(m)
                    if !matches!(m.object, ast::Expr::Super)
                        && !matches!(m.prop, ast::MemberProp::Private(_)) =>
                {
                    self.chain_bails.push(Vec::new());
                    let res: R<Reg> = (|| {
                        let o = self.expr(&m.object)?;
                        let obj = self.alloc_reg();
                        if o != obj {
                            self.emit(Instr::Move { dst: obj, src: o });
                        }
                        if m.optional {
                            self.emit_optional_check(obj);
                        }
                        let strict = self.cx.in_strict;
                        match &m.prop {
                            ast::MemberProp::Ident(p) => {
                                let name = self.string_name(p);
                                self.emit(Instr::DeleteProp {
                                    dst,
                                    obj,
                                    name,
                                    strict,
                                });
                            }
                            ast::MemberProp::Computed(k) => {
                                let key = self.expr(k)?;
                                self.emit(Instr::DeleteIndex {
                                    dst,
                                    obj,
                                    key,
                                    strict,
                                });
                            }
                            ast::MemberProp::Private(_) => unreachable!(),
                        }
                        Ok(dst)
                    })();
                    let bails = self.chain_bails.pop().unwrap();
                    res?;
                    if !bails.is_empty() {
                        let jmp = self.here();
                        self.emit(Instr::Jump { target: 0 });
                        let true_at = self.here();
                        self.emit(Instr::LoadBool { dst, val: true });
                        let end = self.here();
                        self.patch_jump(jmp, end);
                        for b in bails {
                            self.patch_jump(b, true_at);
                        }
                    }
                    Ok(dst)
                }
                _ => {
                    let _ = self.expr(arg)?;
                    self.emit(Instr::LoadBool { dst, val: true });
                    Ok(dst)
                }
            },
            // `delete <identifier>`: in strict mode an early SyntaxError; in sloppy
            // mode deleting a resolvable binding (var/let/const/param/function or a
            // declared global) yields `false` (non-configurable), while an
            // unresolvable name is a no-op that yields `true` — and must NOT be
            // evaluated (evaluating an undeclared name would throw ReferenceError).
            ast::Expr::Ident(id) => {
                let n: &str = id;
                if self.cx.in_strict {
                    return Err(
                        "SyntaxError: Delete of an unqualified identifier in strict mode".into(),
                    );
                }
                // Inside a `with`, `delete name` removes the binding from the
                // innermost with-object that has it (yielding its delete result),
                // else falls through to the static-binding delete semantics below.
                let with_objs = self.with_obj_regs(n);
                if !with_objs.is_empty() {
                    return Ok(self.delete_with(n, &with_objs, dst));
                }
                // A binding is non-configurable — `delete` yields `false` — when it is
                // a local (param/`var`/`let`/`const`/function) or a DECLARED global
                // `var`/`let`/`const`. A builtin, an implicitly-created global
                // (`x = 1` with no declaration), an eval-introduced var, or an
                // unresolvable name is configurable / a no-op, so `delete` yields
                // `true` — and a configurable binding is actually REMOVED.
                // `NaN`/`Infinity`/`undefined` are the only non-configurable builtin
                // global properties; they're not tracked as compiler globals, so
                // check them by name (a local of that name still resolves below).
                self.emit_static_delete(n, dst);
                Ok(dst)
            }
            other => {
                let _ = self.expr(other)?;
                self.emit(Instr::LoadBool { dst, val: true });
                Ok(dst)
            }
        }
    }

    pub(crate) fn update(
        &mut self,
        op: ast::UpdateOp,
        prefix: bool,
        target: &ast::Target,
        dst: Reg,
    ) -> R<Reg> {
        let delta = match op {
            ast::UpdateOp::Inc => 1,
            ast::UpdateOp::Dec => -1,
        };
        // `obj.x++` / `arr[i]--` etc — read the member, yield old (postfix) or
        // new (prefix), write the incremented value back to the same slot.
        if let ast::Target::Member(m) = target {
            match (&m.object, &m.prop) {
                // `super.x++` / `--super.x` — read via the super-get sequence,
                // coerce/step, write back via the super-set sequence.
                (ast::Expr::Super, ast::MemberProp::Ident(prop)) => {
                    let pid = self.super_class;
                    if pid.is_none() && !self.super_home_obj {
                        return Err("`super.x++` is only valid in a method".into());
                    }
                    self.this_check();
                    let name = self.string_name(prop);
                    let cur = self.temp();
                    match pid {
                        Some(p) => self.emit(Instr::SuperGet {
                            dst: cur,
                            home_class_id: p,
                            name,
                        }),
                        None => self.emit(Instr::SuperGetObj { dst: cur, name }),
                    }
                    let oldnum = self.temp();
                    self.emit(Instr::AddInt {
                        dst: oldnum,
                        a: cur,
                        imm: 0,
                        upd: true,
                    });
                    let nw = self.temp();
                    self.emit(Instr::AddInt {
                        dst: nw,
                        a: oldnum,
                        imm: delta,
                        upd: true,
                    });
                    match pid {
                        Some(p) => {
                            let b = self.temp();
                            self.emit(Instr::SuperBase {
                                dst: b,
                                home_class_id: p,
                            });
                            self.emit(Instr::SuperSet {
                                base: b,
                                home_class_id: p,
                                name,
                                val: nw,
                            })
                        }
                        None => self.emit(Instr::SuperSetObj { name, val: nw }),
                    }
                    self.emit(Instr::Move {
                        dst,
                        src: if prefix { nw } else { oldnum },
                    });
                    return Ok(dst);
                }
                // `super[k]++` / `--super[k]` — SuperProperty evaluation checks the
                // this-TDZ BEFORE evaluating the key Expression
                // (prop-expr-uninitialized-this-putvalue-increment), and the
                // computed super ops capture GetSuperBase before ToPropertyKey.
                (ast::Expr::Super, ast::MemberProp::Computed(ke)) => {
                    let pid = self.super_class;
                    if pid.is_none() && !self.super_home_obj {
                        return Err("`super[k]++` is only valid in a method".into());
                    }
                    self.this_check();
                    let key = self.expr(ke)?;
                    let key_reg = self.alloc_reg();
                    if key != key_reg {
                        self.emit(Instr::Move {
                            dst: key_reg,
                            src: key,
                        });
                    }
                    let cur = self.temp();
                    match pid {
                        Some(p) => self.emit(Instr::SuperGetComputed {
                            dst: cur,
                            home_class_id: p,
                            key: key_reg,
                        }),
                        None => self.emit(Instr::SuperGetObjComputed {
                            dst: cur,
                            key: key_reg,
                        }),
                    }
                    let oldnum = self.temp();
                    self.emit(Instr::AddInt {
                        dst: oldnum,
                        a: cur,
                        imm: 0,
                        upd: true,
                    });
                    let nw = self.temp();
                    self.emit(Instr::AddInt {
                        dst: nw,
                        a: oldnum,
                        imm: delta,
                        upd: true,
                    });
                    match pid {
                        Some(p) => {
                            let b = self.temp();
                            self.emit(Instr::SuperBase {
                                dst: b,
                                home_class_id: p,
                            });
                            self.emit(Instr::SuperSetComputed {
                                base: b,
                                home_class_id: p,
                                key: key_reg,
                                val: nw,
                            })
                        }
                        None => self.emit(Instr::SuperSetObjComputed {
                            key: key_reg,
                            val: nw,
                        }),
                    }
                    self.emit(Instr::Move {
                        dst,
                        src: if prefix { nw } else { oldnum },
                    });
                    return Ok(dst);
                }
                (_, ast::MemberProp::Ident(prop)) => {
                    let obj = self.expr(&m.object)?;
                    let name = self.string_name(prop);
                    let cur = self.temp();
                    self.emit(Instr::GetProp {
                        dst: cur,
                        obj,
                        name,
                    });
                    // ToNumeric(old) ONCE (AddInt imm:0), derive the new value from it,
                    // and yield the COERCED old (postfix) — `x++` returns a number, not
                    // the raw operand. Single coercion = one valueOf for an object operand.
                    let oldnum = self.temp();
                    self.emit(Instr::AddInt {
                        dst: oldnum,
                        a: cur,
                        imm: 0,
                        upd: true,
                    });
                    let nw = self.temp();
                    self.emit(Instr::AddInt {
                        dst: nw,
                        a: oldnum,
                        imm: delta,
                        upd: true,
                    });
                    self.emit(Instr::SetProp {
                        obj,
                        name,
                        val: nw,
                        strict: self.cx.strict_expr_region > 0,
                    });
                    self.emit(Instr::Move {
                        dst,
                        src: if prefix { nw } else { oldnum },
                    });
                    return Ok(dst);
                }
                (_, ast::MemberProp::Computed(ke)) => {
                    let obj = self.expr(&m.object)?;
                    let key = self.expr(ke)?;
                    // `o[k]++` reads then writes `o[k]` — coerce the key ToPropertyKey
                    // ONCE and reuse it (its toString/valueOf must not run twice).
                    let keyk = self.temp();
                    self.emit(Instr::ToPropKey {
                        dst: keyk,
                        obj,
                        src: key,
                    });
                    let cur = self.temp();
                    self.emit(Instr::GetIndex {
                        dst: cur,
                        obj,
                        key: keyk,
                    });
                    let oldnum = self.temp();
                    self.emit(Instr::AddInt {
                        dst: oldnum,
                        a: cur,
                        imm: 0,
                        upd: true,
                    });
                    let nw = self.temp();
                    self.emit(Instr::AddInt {
                        dst: nw,
                        a: oldnum,
                        imm: delta,
                        upd: true,
                    });
                    self.emit(Instr::SetIndex {
                        obj,
                        key: keyk,
                        val: nw,
                    });
                    self.emit(Instr::Move {
                        dst,
                        src: if prefix { nw } else { oldnum },
                    });
                    return Ok(dst);
                }
                // `obj.#x++` — like a static member, keyed "#x".
                (_, ast::MemberProp::Private(prop)) => {
                    self.check_private_declared(prop)?;
                    let obj = self.expr(&m.object)?;
                    let name = self.string_name(&private_key(prop));
                    let cur = self.temp();
                    self.emit(Instr::GetProp {
                        dst: cur,
                        obj,
                        name,
                    });
                    let oldnum = self.temp();
                    self.emit(Instr::AddInt {
                        dst: oldnum,
                        a: cur,
                        imm: 0,
                        upd: true,
                    });
                    let nw = self.temp();
                    self.emit(Instr::AddInt {
                        dst: nw,
                        a: oldnum,
                        imm: delta,
                        upd: true,
                    });
                    self.emit(Instr::SetProp {
                        obj,
                        name,
                        val: nw,
                        strict: self.cx.strict_expr_region > 0,
                    });
                    self.emit(Instr::Move {
                        dst,
                        src: if prefix { nw } else { oldnum },
                    });
                    return Ok(dst);
                }
            }
        }
        // `x++` / `++x` / `x--` / `--x` on a simple identifier.
        let name = match target {
            ast::Target::Ident { name, .. } => name.to_string(),
            // Annex B: `f()++` is a SIMPLE assignment target in sloppy code, so
            // it parses and throws a ReferenceError when EVALUATED — after the
            // call has run. NOTE: unreachable through the oxc bridge (oxc cannot
            // build a call target at all), so this changes no existing bytecode.
            ast::Target::Call(c) => {
                let t = self.temp();
                let _ = self.call(c, t)?;
                let e = self.alloc_reg();
                self.emit(Instr::NewError {
                    dst: e,
                    kind: 4,
                    arg: None,
                    opts: None,
                    errors: None,
                });
                self.emit(Instr::Throw { src: e });
                return Ok(dst);
            }
            _ => return Err("update on this target not in zipp-vm v1".into()),
        };
        // Strict mode: `eval++` / `--arguments` is an early SyntaxError.
        strict_name_err(self.cx.in_strict, &name)?;
        // `x++` starts with GetValue(lref), so a name still in its Temporal Dead
        // Zone throws before anything else runs — see `emit_tdz_store_throw`.
        if self.emit_tdz_store_throw(&name) {
            return Ok(dst);
        }
        // Inside a `with`, the updated identifier may be a property of an active
        // with-object (innermost first): read → increment → write through it.
        let with_objs = self.with_obj_regs(&name);
        if !with_objs.is_empty() {
            // Resolve the Reference ONCE (one HasBinding — the @@unscopables
            // getter runs once), then read and write through that target.
            let (found, tgt) = self.emit_with_probe(&name, &with_objs);
            self.emit_with_rmw_read(&name, found, tgt, dst);
            if prefix {
                self.emit(Instr::AddInt {
                    dst,
                    a: dst,
                    imm: delta,
                    upd: true,
                });
                self.emit_with_rmw_write(&name, found, tgt, dst);
                return Ok(dst); // dst holds the new value
            }
            // Postfix: ToNumeric(old) in place, derive the new value, store it,
            // return the COERCED old.
            self.emit(Instr::AddInt {
                dst,
                a: dst,
                imm: 0,
                upd: true,
            });
            let tmp = self.temp();
            self.emit(Instr::AddInt {
                dst: tmp,
                a: dst,
                imm: delta,
                upd: true,
            });
            self.emit_with_rmw_write(&name, found, tgt, tmp);
            self.next_reg -= 1; // reclaim tmp
            return Ok(dst); // dst still holds the (coerced) old value
        }
        let binding = self.resolve(&name);
        if let Binding::Local(r) = binding {
            if !self.const_regs.contains(&r) {
                // Plain mutable register local: mutate in place.
                if prefix {
                    self.emit(Instr::AddInt {
                        dst: r,
                        a: r,
                        imm: delta,
                        upd: true,
                    });
                    if r != dst {
                        self.emit(Instr::Move { dst, src: r });
                    }
                } else {
                    // Yield ToNumeric(old) (one coercion), then increment from it.
                    self.emit(Instr::AddInt {
                        dst,
                        a: r,
                        imm: 0,
                        upd: true,
                    });
                    self.emit(Instr::AddInt {
                        dst: r,
                        a: dst,
                        imm: delta,
                        upd: true,
                    });
                }
                return Ok(dst);
            }
        }
        // Cell / upvalue / global / const-local: read into `dst`, compute, store
        // back (store_binding throws for a const after the read + increment).
        let cur = self.load_binding(&binding, dst); // == dst
        if prefix {
            self.emit(Instr::AddInt {
                dst: cur,
                a: cur,
                imm: delta,
                upd: true,
            });
            // read-first: `load_binding` above resolved the reference.
            self.store_binding_read_first(&binding, cur);
            Ok(dst) // dst holds the new value
        } else {
            // Coerce the old value in place (cur == dst), compute the new value in a
            // temp, store it, and return the COERCED old.
            self.emit(Instr::AddInt {
                dst: cur,
                a: cur,
                imm: 0,
                upd: true,
            });
            let tmp = self.temp();
            self.emit(Instr::AddInt {
                dst: tmp,
                a: cur,
                imm: delta,
                upd: true,
            });
            self.store_binding_read_first(&binding, tmp);
            self.next_reg -= 1; // reclaim tmp
            Ok(dst) // dst still holds the (coerced) old value
        }
    }
}

#[cfg(test)]
mod binary_lhs_reclaim_tests {
    use super::*;

    fn compile(source: &str) -> crate::bytecode::Program {
        let ast = crate::front::parse_script(source).expect("source parses");
        crate::compile::compile_program(&ast, source).expect("source compiles")
    }

    fn compile_module(source: &str) -> crate::bytecode::Program {
        let ast = crate::front::parse_module(source).expect("module parses");
        crate::compile::compile_module(&ast, source).expect("module compiles")
    }

    fn named<'a>(program: &'a crate::bytecode::Program, name: &str) -> &'a FuncProto {
        program
            .functions
            .iter()
            .find(|func| func.name == name)
            .unwrap_or_else(|| panic!("missing function {name:?}"))
    }

    #[test]
    fn fib_reclaims_only_left_scratch_without_changing_opcode_shape() {
        let program = compile("function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2); } fib(8);");
        let fib = named(&program, "fib");
        assert_eq!(fib.reg_count, 9, "unexpected fib register frame:\n{fib:#?}");
        assert_eq!(fib.code.len(), 12, "scratch reuse changed opcode count");

        let calls = fib
            .code
            .iter()
            .filter_map(|instr| match *instr {
                Instr::Call {
                    dst,
                    callee,
                    arg_base,
                    argc: 1,
                } => Some((dst, callee, arg_base)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2, "recursive call shape changed: {fib:#?}");
        let add_inputs = fib.code.iter().find_map(|instr| match *instr {
            Instr::Add { a, b, .. } => Some((a, b)),
            _ => None,
        });
        assert_eq!(add_inputs, Some((calls[0].0, calls[1].0)));
        assert_eq!(
            calls[1],
            (calls[0].1, calls[0].2, calls[0].2.saturating_add(1)),
            "RHS did not reuse only dead LHS call scratch"
        );
    }

    #[test]
    fn a_non_dst_left_result_is_kept_above_the_reclaimed_floor() {
        // ImportMeta deliberately allocates and returns a register OTHER than
        // the dst requested by expr(). Reclaiming to the entry floor alone
        // would let the RHS overwrite the first module object before Eq reads it.
        let program =
            compile_module("export function same(){ return import.meta === import.meta; }");
        let same = named(&program, "same");
        let metas = same
            .code
            .iter()
            .filter_map(|instr| match *instr {
                Instr::ImportMeta { dst } => Some(dst),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(metas.len(), 2, "import.meta lowering changed: {same:#?}");
        assert_ne!(metas[0], metas[1], "RHS overwrote the live LHS result");
        let eq_inputs = same.code.iter().find_map(|instr| match *instr {
            Instr::Eq { a, b, .. } => Some((a, b)),
            _ => None,
        });
        assert_eq!(eq_inputs, Some((metas[0], metas[1])));
    }
}

#[cfg(test)]
mod static_key_plan_tests {
    use super::*;

    fn compile(source: &str) -> crate::bytecode::Program {
        let ast = crate::front::parse_script(source).expect("source parses");
        crate::compile::compile_program(&ast, source).expect("source compiles")
    }

    fn named<'a>(program: &'a crate::bytecode::Program, name: &str) -> &'a FuncProto {
        program
            .functions
            .iter()
            .find(|func| func.name == name)
            .unwrap_or_else(|| panic!("missing function {name:?}"))
    }

    #[test]
    fn compiler_plans_only_the_exact_unique_static_append_sequence() {
        let program = compile("function build(x){return {a:x,b:2,m(){return this.a}}} build(1);");
        let func = named(&program, "build");
        let plan_id = func
            .code
            .iter()
            .find_map(|instr| match *instr {
                Instr::NewPlannedObject { plan, .. } => Some(plan),
                _ => None,
            })
            .expect("eligible literal received a plan");
        assert_eq!(
            func.static_key_plans[plan_id as usize].keys(),
            &["a".to_string(), "b".to_string(), "m".to_string()]
        );
        assert_eq!(
            func.code
                .iter()
                .filter(|instr| matches!(instr, Instr::AppendDataProp { .. }))
                .count(),
            3
        );
    }

    #[test]
    fn dynamic_duplicate_accessor_spread_and_proto_literals_stay_owned() {
        for body in [
            "let k='b';return {a:1,[k]:2}",
            "return {a:1,a:2}",
            "return {a:1,get b(){return 2}}",
            "return {a:1,...globalThis}",
            "return {a:1,__proto__:null}",
        ] {
            let source = format!("function build(){{{body}}} build();");
            let program = compile(&source);
            let func = named(&program, "build");
            assert!(
                !func
                    .code
                    .iter()
                    .any(|instr| matches!(instr, Instr::NewPlannedObject { .. })),
                "ineligible body unexpectedly planned: {body}"
            );
            assert!(func.static_key_plans.is_empty());
        }
    }

    #[test]
    fn per_literal_field_and_byte_caps_fall_back_without_rejecting_source() {
        let fields = (0..=STATIC_KEY_PLAN_MAX_FIELDS)
            .map(|i| format!("k{i}:{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let source = format!("function wide(){{return {{{fields}}}}} wide();");
        let program = compile(&source);
        let wide = named(&program, "wide");
        assert!(wide.static_key_plans.is_empty());
        assert!(wide
            .code
            .iter()
            .any(|instr| matches!(instr, Instr::NewObject { .. })));

        let key = "x".repeat(STATIC_KEY_PLAN_MAX_BYTES + 1);
        let source = format!("function bytes(){{return {{'{key}':1}}}} bytes();");
        let program = compile(&source);
        let bytes = named(&program, "bytes");
        assert!(bytes.static_key_plans.is_empty());
        assert!(bytes
            .code
            .iter()
            .any(|instr| matches!(instr, Instr::NewObject { .. })));
    }

    #[test]
    fn retained_charge_prices_site_string_records_and_rounded_allocations() {
        let keys = vec![String::new(), "x".into(), "y".repeat(17)];
        // 96 site/Arc/Vec + (48 String/allocator + rounded payload) per key.
        assert_eq!(
            crate::bytecode::static_key_plan_retained_charge(&keys),
            Some(96 + (48 + 16) + (48 + 16) + (48 + 32))
        );
    }

    #[test]
    fn plan_name_collection_stops_at_the_per_literal_field_cap() {
        let mut names = Vec::with_capacity(STATIC_KEY_PLAN_MAX_FIELDS);
        for name in 0..10_000u32 {
            collect_static_key_plan_name(&mut names, name);
        }
        assert_eq!(names.len(), STATIC_KEY_PLAN_MAX_FIELDS);
        assert_eq!(names.first(), Some(&0));
        assert_eq!(names.last(), Some(&255));
    }

    #[test]
    fn compiler_site_cap_falls_back_at_257_without_rejecting_source() {
        let source = (0..=crate::bytecode::STATIC_KEY_PLAN_COMPILER_MAX_SITES)
            .map(|i| format!("var x{i}={{k:{i}}};"))
            .collect::<String>();
        let program = compile(&source);
        let main = &program.functions[0];
        assert_eq!(
            main.static_key_plans.len(),
            crate::bytecode::STATIC_KEY_PLAN_COMPILER_MAX_SITES
        );
        assert_eq!(
            main.code
                .iter()
                .filter(|instr| matches!(instr, Instr::NewPlannedObject { .. }))
                .count(),
            crate::bytecode::STATIC_KEY_PLAN_COMPILER_MAX_SITES
        );
        assert!(
            main.code
                .iter()
                .any(|instr| matches!(instr, Instr::NewObject { hint: 1, .. })),
            "site 257 must retain exact legacy capacity bytecode"
        );
    }

    #[test]
    fn compiler_off_child() {
        if std::env::var_os("ZIPP_STATIC_KEY_COMPILER_OFF_CHILD").is_none() {
            return;
        }
        let program = compile("function build(v){return {a:v,b:2}} build(1);");
        assert!(program
            .functions
            .iter()
            .all(|func| func.static_key_plans.is_empty()));
        assert!(program.functions.iter().all(|func| {
            !func
                .code
                .iter()
                .any(|instr| matches!(instr, Instr::NewPlannedObject { .. }))
        }));
        assert!(program.functions.iter().any(|func| {
            func.code
                .iter()
                .any(|instr| matches!(instr, Instr::NewObject { hint: 2, .. }))
        }));
    }

    #[test]
    fn compiler_off_switch_declines_before_allocating_metadata() {
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "compile::exprs::static_key_plan_tests::compiler_off_child",
                "--exact",
                "--nocapture",
            ])
            .env("ZIPP_STATIC_KEY_COMPILER_OFF_CHILD", "1")
            .env("ZIPP_NO_STATIC_KEY_PLANS", "1")
            .output()
            .expect("spawn compiler-off child");
        assert!(
            out.status.success(),
            "compiler-off child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
