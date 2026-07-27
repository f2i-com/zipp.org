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

// NOTE: parenthesization. `ast` has no ParenthesizedExpression, so every peel is
// gone from this file (`expr_into`, `typeof (f)`, `delete (x)`). That is a
// deliberate behaviour change in one direction only: a parenthesized operand now
// reaches the pattern matches that recognise a *shape* — `new (Array)(1)`,
// `x instanceof (Array)`, `(Math).PI`, `` (String.raw)`…` `` — where before the
// wrapper node hid it and the generic path ran. Parenthesization is observable in
// exactly two places (assignment-target simplicity, and NamedEvaluation via
// `Target::Ident { covered }`), and neither is one of these, so the fold is
// correct; it just was not reachable before.
//
// NOTE: helpers defined HERE and nowhere else in this file's group —
// `arg_expr`, `static_key_text`, `lone_surrogate_markers`, `PropVal`, and the
// `FnCompiler` methods `str_const`, `member`, `private_member`, `prop_value`,
// `object_accessor`, `object_data_prop`. If another section lands an identical
// helper, keep one copy.

/// The expression of a plain (non-spread) argument. Mirrors oxc's
/// `Argument::as_expression`, which returned `None` for a `...spread`, so every
/// `args.first().and_then(|a| a.as_expression())` reads the same as before.
pub(crate) fn arg_expr(a: &ast::Arg) -> Option<&ast::Expr> {
    match a {
        ast::Arg::Expr(e) => Some(e),
        ast::Arg::Spread(_) => None,
    }
}

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
        if let ast::Expr::Update { op, prefix: false, target } = e {
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
                self.emit(Instr::NewRegExp { dst, pattern: pt, flags: ft, is_construct: true });
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
                    return Err(format!("SyntaxError: '{n}' is a reserved word in strict mode"));
                }
                // Special global value identifiers that are not user bindings.
                // Inside a `with`, an own property of a with-object SHADOWS the
                // literal (e.g. `with({NaN:'x'}) NaN` === 'x'); the literal is the
                // fallback when no with-object carries the name.
                if matches!(n, "undefined" | "NaN" | "Infinity") {
                    let lit = |s: &mut Self| match n {
                        "undefined" => s.emit(Instr::LoadUndefined { dst }),
                        "NaN" => {
                            let idx = s.add_const(Value::num(f64::NAN));
                            s.emit(Instr::LoadConst { dst, idx });
                        }
                        _ => {
                            let idx = s.add_const(Value::num(f64::INFINITY));
                            s.emit(Instr::LoadConst { dst, idx });
                        }
                    };
                    let with_objs = self.with_obj_regs(n);
                    if with_objs.is_empty() {
                        lit(self);
                        return Ok(dst);
                    }
                    let nidx = self.string_name(n);
                    let end_jumps = self.emit_with_get_chain(nidx, &with_objs, dst);
                    lit(self);
                    let end = self.here();
                    for je in end_jumps {
                        self.patch_jump(je, end);
                    }
                    return Ok(dst);
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
                    self.emit(Instr::NewError { dst: e, kind: 4, arg: None, opts: None, errors: None });
                    self.emit(Instr::Throw { src: e });
                    return Ok(dst);
                }
                // Inside a `with`, a free identifier may resolve to a property of an
                // active with-object (innermost first), else the static binding.
                let with_objs = self.with_obj_regs(n);
                if !with_objs.is_empty() {
                    return Ok(self.load_with(n, &with_objs, dst));
                }
                match self.resolve(n) {
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
                            self.emit(Instr::LoadUpvalDyn { dst, idx, name: slot });
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
            E::Assign { op, target, value } => self.assign(*op, target, value, dst),
            E::Cond { test, cons, alt } => self.conditional(test, cons, alt, dst),
            E::Yield { arg, delegate } => self.yield_expr(arg.as_deref(), *delegate, dst),
            E::Await(a) => self.await_expr(a, dst),
            E::Call(c) => self.call(c, dst),
            E::New { callee, args } => {
                // `new Error(msg)` / `new TypeError(msg)` / `new RangeError(msg)`
                // → a plain object {name, message}. Other constructors aren't in
                // the subset yet. The by-name lowerings below fire only for the
                // PRISTINE global builtin — a user binding of the same name
                // (`function TypeError() {}`) takes the generic value path.
                // A spread anywhere in the arguments disqualifies EVERY by-name
                // lowering below: they all reach for `args.first()` or
                // `eval_args_contiguous`, which either hard-errors ("spread
                // arguments are not in the zipp-vm subset yet") or silently drops
                // the spread — `new Map(...[[[1,2]]])` built an empty Map. The
                // generic `NewSpread` path at the bottom constructs the same
                // builtin correctly, so route spread calls there.
                let has_spread = args.iter().any(|a| matches!(a, ast::Arg::Spread(_)));
                let id_opt = match &**callee {
                    ast::Expr::Ident(id) if !has_spread && self.builtin_unshadowed(id) => Some(id),
                    _ => None,
                };
                if let Some(id) = id_opt {
                    let id: &str = id;
                    if let Some(kind) = error_ctor(id) {
                        return self.build_error(kind, args, dst);
                    }
                    // `new Array(…)` / `new Object()` builtins (no real global).
                    if id == "Array" {
                        let (arg_base, argc) = self.eval_args_contiguous(args)?;
                        self.emit(Instr::ArrayCtor { dst, arg_base, argc });
                        return Ok(dst);
                    }
                    if id == "Object" {
                        // `new Object()` → a fresh object; `new Object(x)` → ToObject(x).
                        if let Some(arg) = args.first().and_then(arg_expr) {
                            let src = self.expr(arg)?;
                            self.emit(Instr::ToObject { dst, src });
                        } else {
                            self.emit(Instr::NewObject { dst, hint: 0 });
                        }
                        return Ok(dst);
                    }
                    // `new Promise(executor)`. A missing executor is a RUNTIME
                    // TypeError (NewPromise validates callability), not a
                    // compile error — `new Promise()` inside a never-taken
                    // branch must still compile.
                    if id == "Promise" {
                        let executor = {
                            let t = self.temp();
                            match args.first().and_then(arg_expr) {
                                Some(e) => {
                                    let v = self.expr_into(e, t)?;
                                    if v != t {
                                        self.emit(Instr::Move { dst: t, src: v });
                                    }
                                }
                                None => self.emit(Instr::LoadUndefined { dst: t }),
                            }
                            t
                        };
                        self.emit(Instr::NewPromise { dst, executor });
                        self.next_reg -= 1; // reclaim executor temp
                        return Ok(dst);
                    }
                    // `new RegExp(pattern?, flags?)` — pattern may be a string or a RegExp.
                    if id == "RegExp" {
                        return self.emit_regexp(args, dst, true);
                    }
                    // `new Map(iter?)` / `new Set(iter?)` / `new WeakMap(iter?)` /
                    // `new WeakSet(iter?)`.
                    if matches!(id, "Map" | "Set" | "WeakMap" | "WeakSet") {
                        let src = match args.first().and_then(arg_expr) {
                            Some(e) => {
                                let t = self.temp();
                                let v = self.expr_into(e, t)?;
                                if v != t {
                                    self.emit(Instr::Move { dst: t, src: v });
                                }
                                Some(t)
                            }
                            None => None,
                        };
                        self.emit(match id {
                            "Set" => Instr::NewSet { dst, src },
                            "Map" => Instr::NewMap { dst, src },
                            "WeakSet" => Instr::NewWeakSet { dst, src },
                            _ => Instr::NewWeakMap { dst, src },
                        });
                        if src.is_some() {
                            self.next_reg -= 1; // reclaim the src temp
                        }
                        return Ok(dst);
                    }
                    // `new String/Number/Boolean(arg?)` — a boxed primitive wrapper.
                    if matches!(id, "String" | "Number" | "Boolean") {
                        let kind = match id {
                            "String" => 0u8,
                            "Number" => 1,
                            _ => 2,
                        };
                        let arg = match args.first().and_then(arg_expr) {
                            Some(e) => {
                                let t = self.temp();
                                let v = self.expr_into(e, t)?;
                                if v != t {
                                    self.emit(Instr::Move { dst: t, src: v });
                                }
                                Some(t)
                            }
                            None => None,
                        };
                        self.emit(Instr::NewBox { dst, kind, arg });
                        if arg.is_some() {
                            self.next_reg -= 1;
                        }
                        return Ok(dst);
                    }
                    // `new WeakRef(target)` — target required (the op validates it's
                    // an object).
                    if id == "WeakRef" {
                        let target = self.temp();
                        match args.first().and_then(arg_expr) {
                            Some(e) => {
                                let v = self.expr_into(e, target)?;
                                if v != target {
                                    self.emit(Instr::Move { dst: target, src: v });
                                }
                            }
                            None => self.emit(Instr::LoadUndefined { dst: target }),
                        }
                        self.emit(Instr::NewWeakRef { dst, target });
                        self.next_reg -= 1; // reclaim target temp
                        return Ok(dst);
                    }
                    // `new FinalizationRegistry(cleanupCallback)`.
                    if id == "FinalizationRegistry" {
                        let cleanup = self.temp();
                        match args.first().and_then(arg_expr) {
                            Some(e) => {
                                let v = self.expr_into(e, cleanup)?;
                                if v != cleanup {
                                    self.emit(Instr::Move { dst: cleanup, src: v });
                                }
                            }
                            None => self.emit(Instr::LoadUndefined { dst: cleanup }),
                        }
                        self.emit(Instr::NewFinalizationRegistry { dst, cleanup });
                        self.next_reg -= 1; // reclaim cleanup temp
                        return Ok(dst);
                    }
                    // `new Date(...)`.
                    if id == "Date" {
                        let (arg_base, argc) = self.eval_args_contiguous(args)?;
                        self.emit(Instr::DateNew { dst, arg_base, argc });
                        return Ok(dst);
                    }
                }
                // General `new C(args)`: evaluate the constructor value, then the
                // args (contiguous), and let the VM build the instance. When the
                // arguments contain a spread (`new C(...xs)`), build a flat args
                // array and construct via NewSpread instead. The constructor is
                // SNAPSHOTTED into a temp before the args run: an argument's
                // side effect reassigning the callee variable must not change
                // which value is constructed (EvaluateNew takes GetValue first).
                let cv = self.expr(callee)?;
                let save = self.next_reg;
                let callee_reg = self.temp();
                if cv != callee_reg {
                    self.emit(Instr::Move { dst: callee_reg, src: cv });
                }
                if has_spread {
                    let args_arr = self.build_spread_args(args)?;
                    self.emit(Instr::NewSpread { dst, callee: callee_reg, args: args_arr });
                    self.next_reg = save; // reclaim the callee temp (+ arg scratch)
                    return Ok(dst);
                }
                let (arg_base, argc) = self.eval_args_contiguous(args)?;
                self.emit(Instr::New { dst, callee: callee_reg, arg_base, argc });
                self.next_reg = save; // reclaim the callee temp + args
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
                self.emit(Instr::HasProp { dst, key: kr, obj, brand: true });
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
            E::ImportCall { spec, options, phase } => {
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
                self.emit(Instr::ImportCall { dst, spec, phase, opts });
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
        // Math constants (Math.PI, Math.E, …) — Math has no real global object.
        if let ast::Expr::Ident(o) = &m.object {
            if &**o == "Math" {
                let c = match prop {
                    "PI" => Some(std::f64::consts::PI),
                    "E" => Some(std::f64::consts::E),
                    "LN2" => Some(std::f64::consts::LN_2),
                    "LN10" => Some(std::f64::consts::LN_10),
                    "LOG2E" => Some(std::f64::consts::LOG2_E),
                    "LOG10E" => Some(std::f64::consts::LOG10_E),
                    "SQRT2" => Some(std::f64::consts::SQRT_2),
                    "SQRT1_2" => Some(std::f64::consts::FRAC_1_SQRT_2),
                    _ => None,
                };
                if let Some(v) = c {
                    self.load_number(dst, v);
                    return Ok(dst);
                }
            }
            // `Symbol.iterator` etc. are now real Symbol VALUES — they resolve as
            // ordinary property reads of the `Symbol` global (whose key_of maps to
            // the engine's `@@iterator` convention, so iteration is unchanged).
        }
        // `super.name` — read an inherited property through the lexical home: a class
        // method via its home class, an object method via its runtime [[HomeObject]].
        if matches!(&m.object, ast::Expr::Super) {
            // MakeSuperPropertyReference: GetThisBinding() throws FIRST in a
            // derived ctor pre-super.
            self.this_check();
            let name = self.string_name(prop);
            if let Some(pid) = self.super_class {
                self.emit(Instr::SuperGet { dst, home_class_id: pid, name });
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

    pub(crate) fn computed_member(&mut self, m: &ast::Member, key_expr: &ast::Expr, dst: Reg) -> R<Reg> {
        // `super[expr]` — computed inherited-property read.
        if matches!(&m.object, ast::Expr::Super) {
            // GetThisBinding() throws BEFORE the key expression is evaluated.
            self.this_check();
            if let Some(pid) = self.super_class {
                let key = self.expr(key_expr)?;
                self.emit(Instr::SuperGetComputed { dst, home_class_id: pid, key });
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
            self.emit(Instr::GetIndexConcat { dst, obj, name: nidx, key });
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
        self.emit(Instr::Eq { dst: cond, a: v, b: scratch });
        let j = self.here();
        self.emit(Instr::JumpIfTrue { cond, target: 0 });
        self.emit(Instr::LoadNull { dst: scratch });
        self.emit(Instr::Eq { dst: cond, a: v, b: scratch });
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
        enum Kind<'k> {
            Static(&'k str),
            Computed(&'k ast::Expr),
        }
        let (object, optional, kind) = match inner {
            ast::Expr::Member(m) if !matches!(m.object, ast::Expr::Super) => match &m.prop {
                ast::MemberProp::Ident(p) => (&m.object, m.optional, Kind::Static(p)),
                ast::MemberProp::Computed(k) => (&m.object, m.optional, Kind::Computed(k)),
                // A private member is neither of the two forms this handles.
                ast::MemberProp::Private(_) => return Ok(None),
            },
            _ => return Ok(None),
        };
        self.chain_bails.push(Vec::new());
        let res: R<(Reg, Reg)> = (|| {
            let o = self.expr(object)?;
            let obj = self.alloc_reg();
            if o != obj {
                self.emit(Instr::Move { dst: obj, src: o });
            }
            if optional {
                self.emit_optional_check(obj);
            }
            let callee = self.alloc_reg();
            match &kind {
                Kind::Static(p) => {
                    let name = self.string_name(p);
                    self.emit(Instr::GetProp { dst: callee, obj, name });
                }
                Kind::Computed(k) => {
                    let key = self.expr(k)?;
                    self.emit(Instr::GetIndex { dst: callee, obj, key });
                }
            }
            Ok((callee, obj))
        })();
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
    /// parts. `String.raw` is handled inline (no real global exists).
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
        // `String.raw` template — concatenate the RAW parts with the values.
        if let ast::Expr::Member(m) = tag_expr {
            if let (ast::Expr::Ident(o), ast::MemberProp::Ident(p)) = (&m.object, &m.prop) {
                if &**o == "String" && &**p == "raw" {
                    return self.string_raw(quasi, dst);
                }
            }
        }
        let n = quasi.exprs.len();
        // Evaluate the tag (and its `this` for a member tag) first, into stable
        // registers that survive the argument block.
        enum Tag {
            Plain(Reg),
            Method(Reg, u32),
        }
        // Only a STATIC member tag binds a receiver here; a computed or private
        // member tag goes down the plain-value path, as it always has.
        let static_tag = match tag_expr {
            ast::Expr::Member(m) => match &m.prop {
                ast::MemberProp::Ident(p) => Some((&m.object, p)),
                _ => None,
            },
            _ => None,
        };
        let tag = match static_tag {
            Some((object, prop)) => {
                let obj = self.expr(object)?;
                let obj_reg = self.alloc_reg();
                if obj != obj_reg {
                    self.emit(Instr::Move { dst: obj_reg, src: obj });
                }
                let name = self.string_name(prop);
                Tag::Method(obj_reg, name)
            }
            None => {
                let callee = self.expr(tag_expr)?;
                let callee_reg = self.alloc_reg();
                if callee != callee_reg {
                    self.emit(Instr::Move { dst: callee_reg, src: callee });
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
        self.emit(Instr::TemplateGetCached { dst: strings_reg, site });
        let skip = self.here();
        self.emit(Instr::JumpIfTrue { cond: strings_reg, target: 0 }); // patched: cache hit
        self.build_template_strings(quasi, strings_reg)?;
        self.emit(Instr::TemplateSetCached { site, src: strings_reg });
        let after = self.here();
        self.patch_jump(skip, after);
        // Contiguous argument block: [strings, e0, e1, …].
        let arg_base = self.next_reg;
        for _ in 0..=n {
            self.alloc_reg();
        }
        let block_top = self.next_reg;
        self.emit(Instr::Move { dst: arg_base, src: strings_reg });
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
                    self.emit(Instr::TailCall { callee, arg_base, argc });
                }
                self.emit(Instr::Call { dst, callee, arg_base, argc })
            }
            Tag::Method(obj, name) => {
                self.emit(Instr::CallMethod { dst, obj, name, arg_base, argc })
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
        self.emit(Instr::NewArray { dst, arg_base: cooked_base, argc: nq });
        self.next_reg = save;
        // Raw array → a temp, then dst.raw = it.
        let raw_reg = self.alloc_reg();
        let raw_base = self.next_reg;
        for q in &quasi.quasis {
            let r = self.alloc_reg();
            let idx = self.add_string_const(&q.raw);
            self.emit(Instr::LoadConst { dst: r, idx });
        }
        self.emit(Instr::NewArray { dst: raw_reg, arg_base: raw_base, argc: nq });
        self.emit(Instr::SetRaw { arr: dst, raw: raw_reg });
        self.next_reg = save;
        Ok(())
    }

    /// `String.raw` template: concatenate the RAW literal parts with the
    /// stringified interpolation values (`String.raw\`a\\n${1}b\`` → `a\\n1b`).
    pub(crate) fn string_raw(&mut self, quasi: &ast::TemplateLit, dst: Reg) -> R<Reg> {
        let idx = self.add_string_const(&quasi.quasis[0].raw);
        self.emit(Instr::LoadConst { dst, idx });
        for (i, e) in quasi.exprs.iter().enumerate() {
            let r = self.expr(e)?;
            self.emit(Instr::Add { dst, a: dst, b: r });
            if let Some(qe) = quasi.quasis.get(i + 1) {
                if !qe.raw.is_empty() {
                    let qidx = self.add_string_const(&qe.raw);
                    let qr = self.temp();
                    self.emit(Instr::LoadConst { dst: qr, idx: qidx });
                    self.emit(Instr::Add { dst, a: dst, b: qr });
                }
            }
        }
        Ok(dst)
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
            || elems.iter().any(|e| matches!(e, Some(ast::ArrayElem::Spread(_))));
        // With a `...spread` element the final length is dynamic, so build the
        // array incrementally via ArrayAppend instead of the fixed-block NewArray.
        if incremental {
            self.emit(Instr::NewArray { dst, arg_base: self.next_reg, argc: 0 }); // []
            for el in elems {
                let save = self.next_reg;
                match el {
                    // A hole is `None`, and is NOT a present `undefined`.
                    None => {
                        let v = self.temp();
                        self.emit(Instr::LoadHole { dst: v });
                        self.emit(Instr::ArrayAppend { arr: dst, val: v, spread: false });
                    }
                    Some(ast::ArrayElem::Spread(s)) => {
                        let v = self.expr(s)?;
                        self.emit(Instr::ArrayAppend { arr: dst, val: v, spread: true });
                    }
                    Some(ast::ArrayElem::Expr(e)) => {
                        let v = self.expr(e)?;
                        self.emit(Instr::ArrayAppend { arr: dst, val: v, spread: false });
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
        self.emit(Instr::NewArray { dst, arg_base: base, argc: count });
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
        self.emit(Instr::NewObject {
            dst,
            hint: static_keys.min(u16::MAX as usize) as u16,
        });
        for prop in props {
            let save = self.next_reg;
            match prop {
                ast::ObjectMember::Get { key, func } => {
                    self.object_accessor(dst, key, func, false)?
                }
                ast::ObjectMember::Set { key, func } => {
                    self.object_accessor(dst, key, func, true)?
                }
                // NOTE: `init` (CoverInitializedName — the `= 1` in `{a = 1}`) is
                // ignored here on purpose. `({a = 1})` is a SyntaxError and
                // `({a = 1} = {})` is a destructuring TARGET, so the initializer
                // is either consumed by the target reinterpretation or raised as
                // an error by the parser, which is the only place that knows
                // which of the two this literal resolved as.
                ast::ObjectMember::Prop { key, value, shorthand, .. } => self.object_data_prop(
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
                }
            }
            self.next_reg = save; // reclaim this property's scratch temps
        }
        Ok(dst)
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
        self.emit(Instr::DefineAccessor { obj, key, func, is_setter });
        self.emit(Instr::SetHomeObject { method: func, home: obj });
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
    ) -> R<()> {
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
            self.emit(Instr::ToPropKey { dst: key, obj, src: raw });
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
                self.emit(Instr::SetFnNameFromKey { func: v, key, prefix: 0 });
            }
            self.emit(Instr::InitDataPropDyn { obj, key, val: v });
            if is_method {
                self.emit(Instr::SetHomeObject { method: v, home: obj });
            }
            return Ok(());
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
        // `{ __proto__: v }` sets the prototype — a real [[Set]]/proto-setter;
        // every other key is CreateDataProperty, which must ignore an
        // inherited accessor / non-writable prop.
        if is_proto {
            self.emit(Instr::SetProp { obj, name, val: v });
        } else if all_appendable {
            self.emit(Instr::AppendDataProp { obj, name, val: v });
        } else {
            self.emit(Instr::InitDataProp { obj, name, val: v });
        }
        if is_method {
            self.emit(Instr::SetHomeObject { method: v, home: obj });
        }
        Ok(())
    }

    pub(crate) fn load_number(&mut self, dst: Reg, n: f64) {
        if n.fract() == 0.0 && n >= i32::MIN as f64 && n <= i32::MAX as f64 {
            self.emit(Instr::LoadInt { dst, val: n as i32 });
        } else {
            let idx = self.add_const(Value::num(n));
            self.emit(Instr::LoadConst { dst, idx });
        }
    }

    pub(crate) fn binary(
        &mut self,
        op: ast::BinaryOp,
        left: &ast::Expr,
        right: &ast::Expr,
        dst: Reg,
    ) -> R<Reg> {
        use ast::BinaryOp as Op;
        // `x instanceof Ctor`: only built-in constructors are recognised (the
        // engine has no user prototype chain). Decided structurally in the VM.
        if matches!(op, Op::Instanceof) {
            // A built-in constructor name → structural InstanceOf; anything else
            // (a user class value) → runtime InstanceOfDyn against its class link.
            if let ast::Expr::Ident(id) = right {
                if let Some(ctor) = InstanceCtor::from_name(id) {
                    let val = self.expr(left)?;
                    self.emit(Instr::InstanceOf { dst, val, ctor });
                    return Ok(dst);
                }
            }
            let val = self.expr(left)?;
            let ctor = self.expr(right)?;
            self.emit(Instr::InstanceOfDyn { dst, val, ctor });
            return Ok(dst);
        }
        // `key in obj`.
        if matches!(op, Op::In) {
            let key = self.expr(left)?;
            let obj = self.expr(right)?;
            self.emit(Instr::HasProp { dst, key, obj, brand: false });
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
                self.emit(Instr::AddInt { dst, a, imm, upd: false });
                return Ok(dst);
            }
        }
        let a = self.expr(left)?;
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
            Op::BitAnd => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::And },
            Op::BitOr => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Or },
            Op::BitXor => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Xor },
            Op::Shl => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Shl },
            Op::Shr => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Shr },
            Op::UShr => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Ushr },
            Op::Exp => Instr::Pow { dst, a, b: r },
            // Both are handled above; kept explicit so a new operator breaks the
            // build instead of falling into a catch-all.
            Op::In | Op::Instanceof => {
                return Err("unsupported binary operator (zipp-vm v1)".into())
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
                self.emit(Instr::JumpIfFalse { cond: dst, target: 0 });
                let b = self.expr_into(right, dst)?;
                if b != dst {
                    self.emit(Instr::Move { dst, src: b });
                }
                let end = self.here();
                self.patch_jump(j, end);
            }
            Op::Or => {
                let j = self.here();
                self.emit(Instr::JumpIfTrue { cond: dst, target: 0 });
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
                self.emit(Instr::JumpIfFalse { cond: isnull, target: 0 }); // non-nullish → keep dst
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
                // `typeof <unbound identifier>` must yield "undefined", NOT throw
                // a ReferenceError — and this holds when the identifier is wrapped
                // in parentheses (`typeof (f)`), which is no longer a node, so the
                // operand IS the identifier. A bare identifier that resolves to a
                // global is read with the non-throwing variant so the
                // never-declared sentinel degrades to undefined.
                // (`undefined`/`NaN`/`Infinity` are literals, handled by `expr`.)
                if let ast::Expr::Ident(id) = arg {
                    let n: &str = id;
                    if !matches!(n, "undefined" | "NaN" | "Infinity") {
                        if let Binding::Global(idx) = self.resolve(n) {
                            // A DECLARED top-level lexical still observes its
                            // TDZ through typeof (a ReferenceError) — only a
                            // name the compiler never saw declared degrades to
                            // "undefined" via the non-throwing load.
                            let declared_lexical = self.cx.lexical_globals.contains(&(idx as u32))
                                || self.cx.const_globals.contains(&(idx as u32));
                            if declared_lexical {
                                self.emit(Instr::LoadGlobal { dst, idx });
                            } else if self.box_all_locals || self.cx.dyn_global_zone {
                                self.emit(Instr::LoadGlobalOrUndefinedDyn { dst, idx });
                            } else {
                                self.emit(Instr::LoadGlobalOrUndefined { dst, idx });
                            }
                            self.emit(Instr::TypeOf { dst, a: dst });
                            return Ok(dst);
                        }
                    }
                }
                let a = self.expr(arg)?;
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
                        self.emit(Instr::NewError { dst: e, kind: 4, arg: None, opts: None, errors: None });
                        self.emit(Instr::Throw { src: e });
                        return Ok(dst);
                    }
                    let obj = self.expr(&m.object)?;
                    let name = self.string_name(prop);
                    let strict = self.cx.in_strict;
                    self.emit(Instr::DeleteProp { dst, obj, name, strict });
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
                        self.emit(Instr::NewError { dst: e, kind: 4, arg: None, opts: None, errors: None });
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
                        self.emit(Instr::DeleteIndexConcat { dst, obj, name: nidx, key, strict });
                        return Ok(dst);
                    }
                    let key = self.expr(ke)?;
                    self.emit(Instr::DeleteIndex { dst, obj, key, strict });
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
                if matches!(n, "NaN" | "Infinity" | "undefined") {
                    self.emit(Instr::LoadBool { dst, val: false });
                    return Ok(dst);
                }
                match self.resolve_existing(n) {
                    Some(Binding::Local(_))
                    | Some(Binding::LocalCell(_))
                    | Some(Binding::Upvalue(_))
                    | Some(Binding::ClassName(_)) => {
                        self.emit(Instr::LoadBool { dst, val: false });
                    }
                    Some(Binding::Global(slot)) => {
                        // A resolved GLOBAL defers to the runtime: DeleteGlobal
                        // checks the PROGRAM's decl lists (an eval-compiled
                        // `delete x` must see the program's `var x` as
                        // non-configurable, and an eval-introduced var as
                        // deletable — this compilation's own cx lists can't
                        // tell) and removes a configurable binding.
                        self.emit(Instr::DeleteGlobal { dst, slot });
                    }
                    None => {
                        // Unreferenced-so-far name: allocate its global slot
                        // (like an identifier reference would) so the runtime
                        // check sees the LIVE binding — crucial for an eval'd
                        // `delete x` whose `x` only exists in the outer
                        // program. A never-declared name's fresh slot is
                        // UNINITIALIZED, so DeleteGlobal is a true no-op.
                        let slot = self.cx.global_slot(n) as u32;
                        self.emit(Instr::DeleteGlobal { dst, slot });
                    }
                }
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
                        Some(p) => self.emit(Instr::SuperGet { dst: cur, home_class_id: p, name }),
                        None => self.emit(Instr::SuperGetObj { dst: cur, name }),
                    }
                    let oldnum = self.temp();
                    self.emit(Instr::AddInt { dst: oldnum, a: cur, imm: 0, upd: true });
                    let nw = self.temp();
                    self.emit(Instr::AddInt { dst: nw, a: oldnum, imm: delta, upd: true });
                    match pid {
                        Some(p) => self.emit(Instr::SuperSet { home_class_id: p, name, val: nw }),
                        None => self.emit(Instr::SuperSetObj { name, val: nw }),
                    }
                    self.emit(Instr::Move { dst, src: if prefix { nw } else { oldnum } });
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
                        self.emit(Instr::Move { dst: key_reg, src: key });
                    }
                    let cur = self.temp();
                    match pid {
                        Some(p) => self.emit(Instr::SuperGetComputed { dst: cur, home_class_id: p, key: key_reg }),
                        None => self.emit(Instr::SuperGetObjComputed { dst: cur, key: key_reg }),
                    }
                    let oldnum = self.temp();
                    self.emit(Instr::AddInt { dst: oldnum, a: cur, imm: 0, upd: true });
                    let nw = self.temp();
                    self.emit(Instr::AddInt { dst: nw, a: oldnum, imm: delta, upd: true });
                    match pid {
                        Some(p) => self.emit(Instr::SuperSetComputed { home_class_id: p, key: key_reg, val: nw }),
                        None => self.emit(Instr::SuperSetObjComputed { key: key_reg, val: nw }),
                    }
                    self.emit(Instr::Move { dst, src: if prefix { nw } else { oldnum } });
                    return Ok(dst);
                }
                (_, ast::MemberProp::Ident(prop)) => {
                    let obj = self.expr(&m.object)?;
                    let name = self.string_name(prop);
                    let cur = self.temp();
                    self.emit(Instr::GetProp { dst: cur, obj, name });
                    // ToNumeric(old) ONCE (AddInt imm:0), derive the new value from it,
                    // and yield the COERCED old (postfix) — `x++` returns a number, not
                    // the raw operand. Single coercion = one valueOf for an object operand.
                    let oldnum = self.temp();
                    self.emit(Instr::AddInt { dst: oldnum, a: cur, imm: 0, upd: true });
                    let nw = self.temp();
                    self.emit(Instr::AddInt { dst: nw, a: oldnum, imm: delta, upd: true });
                    self.emit(Instr::SetProp { obj, name, val: nw });
                    self.emit(Instr::Move { dst, src: if prefix { nw } else { oldnum } });
                    return Ok(dst);
                }
                (_, ast::MemberProp::Computed(ke)) => {
                    let obj = self.expr(&m.object)?;
                    let key = self.expr(ke)?;
                    // `o[k]++` reads then writes `o[k]` — coerce the key ToPropertyKey
                    // ONCE and reuse it (its toString/valueOf must not run twice).
                    let keyk = self.temp();
                    self.emit(Instr::ToPropKey { dst: keyk, obj, src: key });
                    let cur = self.temp();
                    self.emit(Instr::GetIndex { dst: cur, obj, key: keyk });
                    let oldnum = self.temp();
                    self.emit(Instr::AddInt { dst: oldnum, a: cur, imm: 0, upd: true });
                    let nw = self.temp();
                    self.emit(Instr::AddInt { dst: nw, a: oldnum, imm: delta, upd: true });
                    self.emit(Instr::SetIndex { obj, key: keyk, val: nw });
                    self.emit(Instr::Move { dst, src: if prefix { nw } else { oldnum } });
                    return Ok(dst);
                }
                // `obj.#x++` — like a static member, keyed "#x".
                (_, ast::MemberProp::Private(prop)) => {
                    self.check_private_declared(prop)?;
                    let obj = self.expr(&m.object)?;
                    let name = self.string_name(&private_key(prop));
                    let cur = self.temp();
                    self.emit(Instr::GetProp { dst: cur, obj, name });
                    let oldnum = self.temp();
                    self.emit(Instr::AddInt { dst: oldnum, a: cur, imm: 0, upd: true });
                    let nw = self.temp();
                    self.emit(Instr::AddInt { dst: nw, a: oldnum, imm: delta, upd: true });
                    self.emit(Instr::SetProp { obj, name, val: nw });
                    self.emit(Instr::Move { dst, src: if prefix { nw } else { oldnum } });
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
                self.emit(Instr::NewError { dst: e, kind: 4, arg: None, opts: None, errors: None });
                self.emit(Instr::Throw { src: e });
                return Ok(dst);
            }
            _ => return Err("update on this target not in zipp-vm v1".into()),
        };
        // Strict mode: `eval++` / `--arguments` is an early SyntaxError.
        strict_name_err(self.cx.in_strict, &name)?;
        // Inside a `with`, the updated identifier may be a property of an active
        // with-object (innermost first): read → increment → write through it.
        let with_objs = self.with_obj_regs(&name);
        if !with_objs.is_empty() {
            // Resolve the Reference ONCE (one HasBinding — the @@unscopables
            // getter runs once), then read and write through that target.
            let (found, tgt) = self.emit_with_probe(&name, &with_objs);
            self.emit_with_rmw_read(&name, found, tgt, dst);
            if prefix {
                self.emit(Instr::AddInt { dst, a: dst, imm: delta, upd: true });
                self.emit_with_rmw_write(&name, found, tgt, dst);
                return Ok(dst); // dst holds the new value
            }
            // Postfix: ToNumeric(old) in place, derive the new value, store it,
            // return the COERCED old.
            self.emit(Instr::AddInt { dst, a: dst, imm: 0, upd: true });
            let tmp = self.temp();
            self.emit(Instr::AddInt { dst: tmp, a: dst, imm: delta, upd: true });
            self.emit_with_rmw_write(&name, found, tgt, tmp);
            self.next_reg -= 1; // reclaim tmp
            return Ok(dst); // dst still holds the (coerced) old value
        }
        let binding = self.resolve(&name);
        if let Binding::Local(r) = binding {
            if !self.const_regs.contains(&r) {
                // Plain mutable register local: mutate in place.
                if prefix {
                    self.emit(Instr::AddInt { dst: r, a: r, imm: delta, upd: true });
                    if r != dst {
                        self.emit(Instr::Move { dst, src: r });
                    }
                } else {
                    // Yield ToNumeric(old) (one coercion), then increment from it.
                    self.emit(Instr::AddInt { dst, a: r, imm: 0, upd: true });
                    self.emit(Instr::AddInt { dst: r, a: dst, imm: delta, upd: true });
                }
                return Ok(dst);
            }
        }
        // Cell / upvalue / global / const-local: read into `dst`, compute, store
        // back (store_binding throws for a const after the read + increment).
        let cur = self.load_binding(&binding, dst); // == dst
        if prefix {
            self.emit(Instr::AddInt { dst: cur, a: cur, imm: delta, upd: true });
            self.store_binding(&binding, cur);
            Ok(dst) // dst holds the new value
        } else {
            // Coerce the old value in place (cur == dst), compute the new value in a
            // temp, store it, and return the COERCED old.
            self.emit(Instr::AddInt { dst: cur, a: cur, imm: 0, upd: true });
            let tmp = self.temp();
            self.emit(Instr::AddInt { dst: tmp, a: cur, imm: delta, upd: true });
            self.store_binding(&binding, tmp);
            self.next_reg -= 1; // reclaim tmp
            Ok(dst) // dst still holds the (coerced) old value
        }
    }

}
