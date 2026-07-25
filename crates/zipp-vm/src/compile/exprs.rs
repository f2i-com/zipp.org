// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'a> FnCompiler<'a> {
    // ── expressions ──
    /// Compile `e`, returning the register holding its value.
    pub(crate) fn expr(&mut self, e: &ox::Expression) -> R<Reg> {
        let dst = self.temp();
        self.expr_into(e, dst)
    }

    /// Compile `e` placing its value into `dst` (or another register it already
    /// occupies, which the caller may use directly). Returns the register that
    /// actually holds the result.
    pub(crate) fn expr_into(&mut self, e: &ox::Expression, dst: Reg) -> R<Reg> {
        use ox::Expression as E;
        match e {
            E::NumericLiteral(n) => {
                // Strict-mode early error: a legacy octal (`01`) or non-octal
                // leading-zero (`08`) integer literal is a SyntaxError — oxc
                // parses leniently and defers this check to semantics, so
                // enforce it here (covers strict direct eval too).
                if self.cx.in_strict {
                    if let Some(raw) = &n.raw {
                        let b = raw.as_bytes();
                        if b.len() >= 2 && b[0] == b'0' && b[1].is_ascii_digit() {
                            return Err(
                                "SyntaxError: legacy octal literals are not allowed in strict mode"
                                    .into(),
                            );
                        }
                    }
                }
                self.load_number(dst, n.value);
                Ok(dst)
            }
            E::BigIntLiteral(b) => {
                // oxc gives `value` as a base-10 string (the source base is already
                // normalized). In-range literals load as inline i128 (the fast
                // tier); beyond-i128 literals parse ONCE into the program's
                // arbitrary-precision constant pool (the digits are decimal by
                // construction, so only an out-of-range value reaches the pool —
                // the canonical-form invariant holds).
                match b.value.as_str().parse::<i128>() {
                    Ok(v) => self.emit(Instr::LoadBigInt { dst, value: v }),
                    Err(_) => {
                        let big = b
                            .value
                            .as_str()
                            .parse::<num_bigint::BigInt>()
                            .map_err(|_| "invalid BigInt literal".to_string())?;
                        let idx = self.bigint_consts.len() as u32;
                        self.bigint_consts.push(big);
                        self.emit(Instr::LoadBigIntBig { dst, idx });
                    }
                }
                Ok(dst)
            }
            E::RegExpLiteral(r) => {
                // `/pat/flags` → NewRegExp (compiles via the `regress` engine at
                // runtime). pattern.text is the source; flags as the JS flag string.
                //
                // EXACT-source recovery: when this program is an eval of a string
                // holding lone surrogates (`exact_src` is Some), the parser saw the
                // LOSSY view — a lone surrogate in the pattern reads U+FFFD. Both
                // encodings are 3 bytes, so the pattern's byte range in the lossy
                // source indexes the exact WTF-8 bytes identically: slice it and,
                // if it differs from the lossy text, store the pattern constant in
                // the oxc MARKER form (decoded to real WTF-8 at intern time), so
                // the runtime compiles + stores the exact surrogate.
                let text = r.regex.pattern.text.as_str();
                let pat = 'pat: {
                    if text.contains('\u{FFFD}') {
                        if let Some(exact) = &self.cx.exact_src {
                            // Pattern bytes start right after the opening `/`
                            // (r.span covers the whole `/pat/flags` literal).
                            let start = r.span.start as usize + 1;
                            let end = start + text.len();
                            // Guard: the range must reproduce the parser's pattern
                            // text on the lossy source (validates the span math).
                            if self.cx.source.as_bytes().get(start..end) == Some(text.as_bytes()) {
                                if let Some(b) = exact.get(start..end) {
                                    if b != text.as_bytes() {
                                        let marked =
                                            crate::heap::encode_lone_surrogate_markers(b);
                                        break 'pat self.add_string_const_wtf8(&marked);
                                    }
                                }
                            }
                        }
                    }
                    self.add_string_const(text)
                };
                let flags_s = r.regex.flags.to_inline_string();
                let flg = self.add_string_const(&flags_s);
                let pt = self.temp();
                self.emit(Instr::LoadConst { dst: pt, idx: pat });
                let ft = self.temp();
                self.emit(Instr::LoadConst { dst: ft, idx: flg });
                self.emit(Instr::NewRegExp { dst, pattern: pt, flags: ft, is_construct: true });
                self.next_reg -= 2;
                Ok(dst)
            }
            E::StringLiteral(s) => {
                // `.lone_surrogates` literals carry the oxc marker encoding —
                // route through the WTF-8-decoding constant slot.
                let idx = if s.lone_surrogates {
                    self.add_string_const_wtf8(s.value.as_str())
                } else {
                    self.add_string_const(s.value.as_str())
                };
                self.emit(Instr::LoadConst { dst, idx });
                Ok(dst)
            }
            E::TemplateLiteral(t) => {
                // Desugar `q0${e0}q1${e1}...qN` to string concatenation
                // q0 + ToString(e0) + q1 + ToString(e1) + ... + qN. Each `${e}` is
                // ToString'd (string hint) FIRST — NOT left to `+`, whose default
                // hint tries `valueOf` before `toString` (wrong for e.g. a Temporal
                // value, whose `valueOf` throws). After ToStr both operands are
                // strings, so each `+` is a pure (rope) concat.
                let q0e = &t.quasis[0];
                let q0 = q0e.value.cooked.as_ref().map(|s| s.as_str()).unwrap_or("");
                let idx = if q0e.lone_surrogates {
                    self.add_string_const_wtf8(q0)
                } else {
                    self.add_string_const(q0)
                };
                self.emit(Instr::LoadConst { dst, idx });
                for (i, e) in t.expressions.iter().enumerate() {
                    let r = self.expr(e)?;
                    let rs = self.temp();
                    self.emit(Instr::ToStr { dst: rs, a: r });
                    self.emit(Instr::Add { dst, a: dst, b: rs });
                    if let Some(qe) = t.quasis.get(i + 1) {
                        let q = qe.value.cooked.as_ref().map(|s| s.as_str()).unwrap_or("");
                        if !q.is_empty() {
                            let qidx = if qe.lone_surrogates {
                                self.add_string_const_wtf8(q)
                            } else {
                                self.add_string_const(q)
                            };
                            let qr = self.temp();
                            self.emit(Instr::LoadConst { dst: qr, idx: qidx });
                            self.emit(Instr::Add { dst, a: dst, b: qr });
                        }
                    }
                }
                Ok(dst)
            }
            E::TaggedTemplateExpression(tt) => self.tagged_template(tt, dst),
            E::BooleanLiteral(b) => {
                self.emit(Instr::LoadBool { dst, val: b.value });
                Ok(dst)
            }
            E::NullLiteral(_) => {
                self.emit(Instr::LoadNull { dst });
                Ok(dst)
            }
            E::Identifier(id) => {
                // A strict-mode reserved word may not appear as an identifier
                // reference (property keys / member names are different AST nodes,
                // so `obj.public` / `{public:1}` are unaffected).
                if self.cx.in_strict && is_strict_reserved_word(id.name.as_str()) {
                    return Err(format!(
                        "SyntaxError: '{}' is a reserved word in strict mode",
                        id.name
                    ));
                }
                // Special global value identifiers that are not user bindings.
                // Inside a `with`, an own property of a with-object SHADOWS the
                // literal (e.g. `with({NaN:'x'}) NaN` === 'x'); the literal is the
                // fallback when no with-object carries the name.
                if matches!(id.name.as_str(), "undefined" | "NaN" | "Infinity") {
                    let lit = |s: &mut Self| match id.name.as_str() {
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
                    let with_objs = self.with_obj_regs(id.name.as_str());
                    if with_objs.is_empty() {
                        lit(self);
                        return Ok(dst);
                    }
                    let nidx = self.string_name(id.name.as_str());
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
                if id.name == "arguments" && self.cx.in_field_init {
                    return Err(
                        "SyntaxError: 'arguments' is not allowed in a class field initializer"
                            .into(),
                    );
                }
                if self.param_tdz.contains(id.name.as_str()) {
                    let e = self.alloc_reg();
                    self.emit(Instr::NewError { dst: e, kind: 4, arg: None, opts: None, errors: None });
                    self.emit(Instr::Throw { src: e });
                    return Ok(dst);
                }
                // Inside a `with`, a free identifier may resolve to a property of an
                // active with-object (innermost first), else the static binding.
                let with_objs = self.with_obj_regs(id.name.as_str());
                if !with_objs.is_empty() {
                    return Ok(self.load_with(id.name.as_str(), &with_objs, dst));
                }
                match self.resolve(id.name.as_str()) {
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
            E::ThisExpression(_) => {
                // `this` lives in register 0 of the current function, unless a
                // static field initializer has redirected it to the class value.
                // In a derived ctor it is in TDZ until super() completes.
                self.this_check();
                Ok(self.this_override.unwrap_or(0))
            }
            E::ParenthesizedExpression(p) => self.expr_into(&p.expression, dst),
            E::BinaryExpression(b) => self.binary(b, dst),
            E::LogicalExpression(l) => self.logical(l, dst),
            E::UnaryExpression(u) => self.unary(u, dst),
            E::UpdateExpression(u) => self.update(u, dst),
            E::AssignmentExpression(a) => self.assign(a, dst),
            E::ConditionalExpression(c) => self.conditional(c, dst),
            E::YieldExpression(y) => self.yield_expr(y, dst),
            E::AwaitExpression(a) => self.await_expr(a, dst),
            E::CallExpression(c) => self.call(c, dst),
            E::NewExpression(n) => {
                // `new Error(msg)` / `new TypeError(msg)` / `new RangeError(msg)`
                // → a plain object {name, message}. Other constructors aren't in
                // the subset yet. The by-name lowerings below fire only for the
                // PRISTINE global builtin — a user binding of the same name
                // (`function TypeError() {}`) takes the generic value path.
                let id_opt = match &n.callee {
                    ox::Expression::Identifier(id)
                        if self.builtin_unshadowed(id.name.as_str()) =>
                    {
                        Some(id)
                    }
                    _ => None,
                };
                if let Some(id) = id_opt {
                    if let Some(kind) = error_ctor(&id.name) {
                        return self.build_error(kind, &n.arguments, dst);
                    }
                    // `new Array(…)` / `new Object()` builtins (no real global).
                    if id.name == "Array" {
                        let (arg_base, argc) = self.eval_args_contiguous(&n.arguments)?;
                        self.emit(Instr::ArrayCtor { dst, arg_base, argc });
                        return Ok(dst);
                    }
                    if id.name == "Object" {
                        // `new Object()` → a fresh object; `new Object(x)` → ToObject(x).
                        if let Some(arg) = n.arguments.first().and_then(|a| a.as_expression()) {
                            let src = self.expr(arg)?;
                            self.emit(Instr::ToObject { dst, src });
                        } else {
                            self.emit(Instr::NewObject { dst });
                        }
                        return Ok(dst);
                    }
                    // `new Promise(executor)`. A missing executor is a RUNTIME
                    // TypeError (NewPromise validates callability), not a
                    // compile error — `new Promise()` inside a never-taken
                    // branch must still compile.
                    if id.name == "Promise" {
                        let executor = {
                            let t = self.temp();
                            match n.arguments.first().and_then(|a| a.as_expression()) {
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
                    if id.name == "RegExp" {
                        return self.emit_regexp(&n.arguments, dst, true);
                    }
                    // `new Map(iter?)` / `new Set(iter?)` / `new WeakMap(iter?)` /
                    // `new WeakSet(iter?)`.
                    if matches!(id.name.as_str(), "Map" | "Set" | "WeakMap" | "WeakSet") {
                        let src = match n.arguments.first().and_then(|a| a.as_expression()) {
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
                        self.emit(match id.name.as_str() {
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
                    if matches!(id.name.as_str(), "String" | "Number" | "Boolean") {
                        let kind = match id.name.as_str() {
                            "String" => 0u8,
                            "Number" => 1,
                            _ => 2,
                        };
                        let arg = match n.arguments.first().and_then(|a| a.as_expression()) {
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
                    if id.name == "WeakRef" {
                        let target = self.temp();
                        match n.arguments.first().and_then(|a| a.as_expression()) {
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
                    if id.name == "FinalizationRegistry" {
                        let cleanup = self.temp();
                        match n.arguments.first().and_then(|a| a.as_expression()) {
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
                    if id.name == "Date" {
                        let (arg_base, argc) = self.eval_args_contiguous(&n.arguments)?;
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
                let cv = self.expr(&n.callee)?;
                let save = self.next_reg;
                let callee = self.temp();
                if cv != callee {
                    self.emit(Instr::Move { dst: callee, src: cv });
                }
                if n.arguments.iter().any(|a| a.as_expression().is_none()) {
                    let args_arr = self.build_spread_args(&n.arguments)?;
                    self.emit(Instr::NewSpread { dst, callee, args: args_arr });
                    self.next_reg = save; // reclaim the callee temp (+ arg scratch)
                    return Ok(dst);
                }
                let (arg_base, argc) = self.eval_args_contiguous(&n.arguments)?;
                self.emit(Instr::New { dst, callee, arg_base, argc });
                self.next_reg = save; // reclaim the callee temp + args
                Ok(dst)
            }
            E::FunctionExpression(f) => {
                let (id, has_up) =
                    self.compile_func_expr(f.id.as_ref().map(|i| i.name.to_string()), f)?;
                self.emit_make_callable(dst, id, has_up);
                Ok(dst)
            }
            E::ArrowFunctionExpression(a) => {
                let (id, _has_up) = self.compile_arrow(a, "")?;
                self.emit_make_arrow(dst, id);
                Ok(dst)
            }
            E::ClassExpression(c) => self.class_expr(c, dst, None),
            E::ArrayExpression(a) => self.array_literal(a, dst),
            E::ObjectExpression(o) => self.object_literal(o, dst),
            E::StaticMemberExpression(m) => self.static_member(m, dst),
            E::ComputedMemberExpression(m) => self.computed_member(m, dst),
            E::PrivateFieldExpression(p) => {
                // `obj.#field` → read the reserved "#field" property.
                self.check_private_declared(&p.field.name)?;
                let obj = self.expr(&p.object)?;
                if p.optional {
                    self.emit_optional_check(obj);
                }
                let name = self.string_name(&private_key(&p.field.name));
                self.emit(Instr::GetProp { dst, obj, name });
                Ok(dst)
            }
            // `#field in obj` — private brand check (private fields are stored as
            // the reserved "#field" property, so this is a HasProp on that key).
            E::PrivateInExpression(p) => {
                self.check_private_declared(&p.left.name)?;
                let kr = self.temp();
                let idx = self.add_string_const(&private_key(&p.left.name));
                self.emit(Instr::LoadConst { dst: kr, idx });
                let obj = self.expr(&p.right)?;
                // Ergonomic brand check: bypass the private-key reflection filter.
                self.emit(Instr::HasProp { dst, key: kr, obj, brand: true });
                Ok(dst)
            }
            E::ChainExpression(ce) => self.chain_expr(ce, dst),
            E::SequenceExpression(s) => {
                // `(a, b, c)` — evaluate each for side effects; value is the last.
                let n = s.expressions.len();
                for (i, e) in s.expressions.iter().enumerate() {
                    if i + 1 == n {
                        return self.expr_into(e, dst);
                    }
                    let _ = self.expr(e)?;
                }
                self.emit(Instr::LoadUndefined { dst }); // empty sequence (unreachable)
                Ok(dst)
            }
            E::MetaProperty(mp) if mp.meta.name == "new" && mp.property.name == "target" => {
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
            E::ImportExpression(ie) => {
                // Dynamic `import(specifier [, options])` / `import.defer` /
                // `import.source`. Evaluate the specifier (and options, if any);
                // ImportCall does ToString, the options/phase checks, and the load.
                let spec = self.expr(&ie.source)?;
                let opts = match &ie.options {
                    Some(o) => Some(self.expr(o)?),
                    None => None,
                };
                let phase = match ie.phase {
                    Some(ox::ImportPhase::Source) => 2,
                    Some(ox::ImportPhase::Defer) => 1,
                    None => 0,
                };
                self.emit(Instr::ImportCall { dst, spec, phase, opts });
                Ok(dst)
            }
            ox::Expression::MetaProperty(m) => {
                // `import.meta` — module code only (a SyntaxError in scripts);
                // `new.target` is handled by the dedicated lowering elsewhere.
                if m.meta.name == "import" && m.property.name == "meta" {
                    // (Both module pipelines: compile_module entry and the
                    // loader's compile_eval(is_module) — see the import
                    // pre-pass gate.)
                    let in_module =
                        self.cx.module_mode || (self.cx.eval_mode && !self.cx.eval_locals);
                    if !in_module {
                        return Err("SyntaxError: import.meta is only valid in modules".into());
                    }
                    let dst = self.temp();
                    self.emit(Instr::ImportMeta { dst });
                    return Ok(dst);
                }
                Err("unsupported meta property".into())
            }
            _ => Err("unsupported expression (not in the zipp-vm v1 subset yet)".into()),
        }
    }

    pub(crate) fn static_member(&mut self, m: &ox::StaticMemberExpression, dst: Reg) -> R<Reg> {
        // Math constants (Math.PI, Math.E, …) — Math has no real global object.
        if let ox::Expression::Identifier(o) = &m.object {
            if o.name == "Math" {
                let c = match m.property.name.as_str() {
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
        if matches!(&m.object, ox::Expression::Super(_)) {
            // MakeSuperPropertyReference: GetThisBinding() throws FIRST in a
            // derived ctor pre-super.
            self.this_check();
            let name = self.string_name(m.property.name.as_str());
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
        let name = self.string_name(m.property.name.as_str());
        self.emit(Instr::GetProp { dst, obj, name });
        Ok(dst)
    }

    pub(crate) fn computed_member(&mut self, m: &ox::ComputedMemberExpression, dst: Reg) -> R<Reg> {
        // `super[expr]` — computed inherited-property read.
        if matches!(&m.object, ox::Expression::Super(_)) {
            // GetThisBinding() throws BEFORE the key expression is evaluated.
            self.this_check();
            if let Some(pid) = self.super_class {
                let key = self.expr(&m.expression)?;
                self.emit(Instr::SuperGetComputed { dst, home_class_id: pid, key });
            } else if self.super_home_obj {
                let key = self.expr(&m.expression)?;
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
        if let Some((name, rhs)) = concat_key_literal_prefix(&m.expression) {
            let nidx = self.string_name(name);
            let key = self.expr(rhs)?;
            self.emit(Instr::GetIndexConcat { dst, obj, name: nidx, key });
            return Ok(dst);
        }
        let key = self.expr(&m.expression)?;
        self.emit(Instr::GetIndex { dst, obj, key });
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
    /// (call chains etc. — those stay value-calls).
    pub(crate) fn chain_member_callee(&mut self, ce: &ox::ChainExpression) -> R<Option<(Reg, Reg)>> {
        enum Kind<'a, 'x> {
            Static(&'a ox::StaticMemberExpression<'x>),
            Computed(&'a ox::ComputedMemberExpression<'x>),
        }
        let kind = match &ce.expression {
            ox::ChainElement::StaticMemberExpression(m)
                if !matches!(&m.object, ox::Expression::Super(_)) =>
            {
                Kind::Static(m)
            }
            ox::ChainElement::ComputedMemberExpression(m)
                if !matches!(&m.object, ox::Expression::Super(_)) =>
            {
                Kind::Computed(m)
            }
            _ => return Ok(None),
        };
        self.chain_bails.push(Vec::new());
        let res: R<(Reg, Reg)> = (|| {
            let (object, optional) = match &kind {
                Kind::Static(m) => (&m.object, m.optional),
                Kind::Computed(m) => (&m.object, m.optional),
            };
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
                Kind::Static(m) => {
                    let name = self.string_name(m.property.name.as_str());
                    self.emit(Instr::GetProp { dst: callee, obj, name });
                }
                Kind::Computed(m) => {
                    let key = self.expr(&m.expression)?;
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
    /// short-circuit to a single `undefined` result.
    pub(crate) fn chain_expr(&mut self, ce: &ox::ChainExpression, dst: Reg) -> R<Reg> {
        self.chain_bails.push(Vec::new());
        let res = match &ce.expression {
            ox::ChainElement::StaticMemberExpression(m) => self.static_member(m, dst),
            ox::ChainElement::ComputedMemberExpression(m) => self.computed_member(m, dst),
            ox::ChainElement::CallExpression(c) => self.call(c, dst),
            // `o?.#field` (and nested `?.` links inside the object register
            // their own bails): the GetProp private path handles brand checks.
            ox::ChainElement::PrivateFieldExpression(p) => {
                self.check_private_declared(&p.field.name)?;
                let obj = self.expr(&p.object)?;
                if p.optional {
                    self.emit_optional_check(obj);
                }
                let name = self.string_name(&private_key(&p.field.name));
                self.emit(Instr::GetProp { dst, obj, name });
                Ok(dst)
            }
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
    pub(crate) fn tagged_template(&mut self, tt: &ox::TaggedTemplateExpression, dst: Reg) -> R<Reg> {
        self.tagged_template_impl(tt, dst, false)
    }

    /// `return tag`…`` in a proper-tail-call position: same lowering with the
    /// `TailCall` frame-reuse prefix in front of the final plain `Call`.
    pub(crate) fn tagged_template_tail(&mut self, tt: &ox::TaggedTemplateExpression, dst: Reg) -> R<Reg> {
        self.tagged_template_impl(tt, dst, true)
    }

    pub(crate) fn tagged_template_impl(
        &mut self,
        tt: &ox::TaggedTemplateExpression,
        dst: Reg,
        tail: bool,
    ) -> R<Reg> {
        let quasi = &tt.quasi;
        // `String.raw` template — concatenate the RAW parts with the values.
        if let ox::Expression::StaticMemberExpression(m) = &tt.tag {
            if let ox::Expression::Identifier(o) = &m.object {
                if o.name == "String" && m.property.name == "raw" {
                    return self.string_raw(quasi, dst);
                }
            }
        }
        let n = quasi.expressions.len();
        // Evaluate the tag (and its `this` for a member tag) first, into stable
        // registers that survive the argument block.
        enum Tag {
            Plain(Reg),
            Method(Reg, u32),
        }
        let tag = match &tt.tag {
            ox::Expression::StaticMemberExpression(m) => {
                let obj = self.expr(&m.object)?;
                let obj_reg = self.alloc_reg();
                if obj != obj_reg {
                    self.emit(Instr::Move { dst: obj_reg, src: obj });
                }
                let name = self.string_name(m.property.name.as_str());
                Tag::Method(obj_reg, name)
            }
            other => {
                let callee = self.expr(other)?;
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
        for (i, e) in quasi.expressions.iter().enumerate() {
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
    pub(crate) fn build_template_strings(&mut self, quasi: &ox::TemplateLiteral, dst: Reg) -> R<()> {
        let nq = quasi.quasis.len() as u16;
        let save = self.next_reg;
        // Cooked array → dst. A quasi with an ILLEGAL escape sequence has no cooked
        // value (oxc sets `cooked` to None) — in a TAGGED template that element is
        // `undefined` (only the tag sees it; an untagged template would be a syntax
        // error), so load undefined rather than masking it as "".
        let cooked_base = self.next_reg;
        for q in &quasi.quasis {
            let r = self.alloc_reg();
            match q.value.cooked.as_ref() {
                Some(s) => {
                    // A `.lone_surrogates` quasi cooks to the oxc marker form —
                    // decode to real WTF-8 at intern time. (Raw parts below are
                    // source text — never markers.)
                    let idx = if q.lone_surrogates {
                        self.add_string_const_wtf8(s.as_str())
                    } else {
                        self.add_string_const(s.as_str())
                    };
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
            let idx = self.add_string_const(q.value.raw.as_str());
            self.emit(Instr::LoadConst { dst: r, idx });
        }
        self.emit(Instr::NewArray { dst: raw_reg, arg_base: raw_base, argc: nq });
        self.emit(Instr::SetRaw { arr: dst, raw: raw_reg });
        self.next_reg = save;
        Ok(())
    }

    /// `String.raw` template: concatenate the RAW literal parts with the
    /// stringified interpolation values (`String.raw\`a\\n${1}b\`` → `a\\n1b`).
    pub(crate) fn string_raw(&mut self, quasi: &ox::TemplateLiteral, dst: Reg) -> R<Reg> {
        let r0 = quasi.quasis[0].value.raw.as_str();
        let idx = self.add_string_const(r0);
        self.emit(Instr::LoadConst { dst, idx });
        for (i, e) in quasi.expressions.iter().enumerate() {
            let r = self.expr(e)?;
            self.emit(Instr::Add { dst, a: dst, b: r });
            if let Some(qe) = quasi.quasis.get(i + 1) {
                let raw = qe.value.raw.as_str();
                if !raw.is_empty() {
                    let qidx = self.add_string_const(raw);
                    let qr = self.temp();
                    self.emit(Instr::LoadConst { dst: qr, idx: qidx });
                    self.emit(Instr::Add { dst, a: dst, b: qr });
                }
            }
        }
        Ok(dst)
    }

    pub(crate) fn array_literal(&mut self, a: &ox::ArrayExpression, dst: Reg) -> R<Reg> {
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
        let n_elems = a.elements.len();
        let block_fits = self.next_reg as usize + n_elems <= Reg::MAX as usize;
        let incremental = n_elems > NEWARRAY_MAX_ELEMS
            || !block_fits
            || a.elements.iter().any(|e| matches!(e, ox::ArrayExpressionElement::SpreadElement(_)));
        // With a `...spread` element the final length is dynamic, so build the
        // array incrementally via ArrayAppend instead of the fixed-block NewArray.
        if incremental {
            self.emit(Instr::NewArray { dst, arg_base: self.next_reg, argc: 0 }); // []
            for el in &a.elements {
                let save = self.next_reg;
                match el {
                    ox::ArrayExpressionElement::Elision(_) => {
                        // An elided element is a HOLE, not a present `undefined`.
                        let v = self.temp();
                        self.emit(Instr::LoadHole { dst: v });
                        self.emit(Instr::ArrayAppend { arr: dst, val: v, spread: false });
                    }
                    ox::ArrayExpressionElement::SpreadElement(s) => {
                        let v = self.expr(&s.argument)?;
                        self.emit(Instr::ArrayAppend { arr: dst, val: v, spread: true });
                    }
                    other => {
                        let e = other.as_expression().ok_or("unsupported array element")?;
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
        for _ in &a.elements {
            self.alloc_reg();
        }
        let block_top = self.next_reg;
        for (i, el) in a.elements.iter().enumerate() {
            let slot = base + i as Reg;
            match el {
                ox::ArrayExpressionElement::Elision(_) => {
                    // An elided element is a HOLE, not a present `undefined`.
                    self.emit(Instr::LoadHole { dst: slot });
                }
                ox::ArrayExpressionElement::SpreadElement(_) => unreachable!("handled above"),
                other => {
                    let e = other.as_expression().ok_or("unsupported array element")?;
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

    pub(crate) fn object_literal(&mut self, o: &ox::ObjectExpression, dst: Reg) -> R<Reg> {
        self.emit(Instr::NewObject { dst });
        for prop in &o.properties {
            let save = self.next_reg;
            match prop {
                ox::ObjectPropertyKind::ObjectProperty(p) => {
                    if matches!(p.kind, ox::PropertyKind::Get | ox::PropertyKind::Set) {
                        // `{ get k(){…} }` / `{ set k(v){…} }` — an accessor property.
                        // The key is loaded into a register (computed expr or the
                        // static key string); a get+set pair on one key merges.
                        let key = if p.computed {
                            let ke =
                                p.key.as_expression().ok_or("unsupported computed accessor key")?;
                            self.expr(ke)?
                        } else {
                            let k = match &p.key {
                                ox::PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                                ox::PropertyKey::StringLiteral(s) => string_literal_key(s),
                                ox::PropertyKey::NumericLiteral(n) => fmt_key_num(n.value),
                                ox::PropertyKey::BigIntLiteral(b) => b.value.to_string(),
                                _ => return Err("unsupported accessor key in the zipp-vm subset".into()),
                            };
                            let kr = self.alloc_reg();
                            let idx = self.add_string_const(&k);
                            self.emit(Instr::LoadConst { dst: kr, idx });
                            kr
                        };
                        // An accessor is a method: it gets a [[HomeObject]], so `super`
                        // inside it resolves via the object (set the transient flag the
                        // function-body compiler consumes).
                        self.cx.obj_method_super = true;
                        let func = self.expr(&p.value)?;
                        self.cx.obj_method_super = false;
                        // `Function.prototype.toString` of an object accessor is the
                        // whole `get k(){}` / `set k(v){}`; the value-Function span
                        // omits the `get`/`set` and the key, so patch the just-
                        // compiled proto (pushed last) with the ObjectProperty span.
                        let fid = self.cx.functions.len() - 1;
                        self.cx.functions[fid].source =
                            self.cx.src_slice(p.span.start, p.span.end);
                        self.cx.functions[fid].non_constructable = true; // accessor = method
                        let is_setter = matches!(p.kind, ox::PropertyKind::Set);
                        self.emit(Instr::DefineAccessor { obj: dst, key, func, is_setter });
                        self.emit(Instr::SetHomeObject { method: func, home: dst });
                        // SetFunctionName: a getter/setter is named "get k"/"set k"
                        // (a Symbol key → "get [desc]"), at runtime so a computed key
                        // is handled too.
                        self.emit(Instr::SetFnNameFromKey {
                            func,
                            key,
                            prefix: if is_setter { 2 } else { 1 },
                        });
                    } else if p.computed {
                        // Computed key `{[expr]: v}` → CreateDataProperty with a
                        // runtime key: ToPropertyKey runs BEFORE the value
                        // evaluates (its coercion side effects order first), and
                        // a computed "__proto__" defines an ORDINARY own
                        // property (only the textual colon form sets the proto).
                        let ke = p.key.as_expression().ok_or("unsupported computed object key")?;
                        let raw = self.expr(ke)?;
                        let key = self.alloc_reg();
                        self.emit(Instr::ToPropKey { dst: key, obj: dst, src: raw });
                        // A computed concise method gets a [[HomeObject]] (for super).
                        if p.method {
                            self.cx.obj_method_super = true;
                        }
                        let v = self.expr(&p.value)?;
                        self.cx.obj_method_super = false;
                        // A computed concise method `{ [expr](){} }` (incl. `*`/`async`):
                        // its toString is the whole `[expr](){}` — the value-Function span
                        // omits the computed key + modifiers, so patch it with the
                        // ObjectProperty span (mirrors the static-key method branch below).
                        if p.method {
                            let fid = self.cx.functions.len() - 1;
                            self.cx.functions[fid].source =
                                self.cx.src_slice(p.span.start, p.span.end);
                            self.cx.functions[fid].non_constructable = true;
                        }
                        // SetFunctionName: an anonymous function/arrow/class value
                        // takes the (runtime) computed key as its name — a Symbol key
                        // becomes "[description]".
                        if is_anonymous_fn_def(&p.value) {
                            self.emit(Instr::SetFnNameFromKey { func: v, key, prefix: 0 });
                        }
                        self.emit(Instr::InitDataPropDyn { obj: dst, key, val: v });
                        if p.method {
                            self.emit(Instr::SetHomeObject { method: v, home: dst });
                        }
                    } else {
                        // Static identifier / string / number literal key.
                        let key = match &p.key {
                            ox::PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                            ox::PropertyKey::StringLiteral(s) => string_literal_key(s),
                            ox::PropertyKey::NumericLiteral(n) => fmt_key_num(n.value),
                                ox::PropertyKey::BigIntLiteral(b) => b.value.to_string(),
                            _ => return Err("unsupported object key in the zipp-vm subset".into()),
                        };
                        let name = self.string_name(&key);
                        // `{ fn: function(){}, m(){}, C: class{} }` — an anonymous
                        // value function/class takes the property key as its name,
                        // EXCEPT `{ __proto__: fn }` (a proto-setter, not a data
                        // property): its function value stays anonymous.
                        let vtmp = self.alloc_reg();
                        // A concise method gets a [[HomeObject]] (for `super`); a plain
                        // `k: function(){}` data property does NOT.
                        if p.method {
                            self.cx.obj_method_super = true;
                        }
                        let v = if key == "__proto__" && !p.method && !p.shorthand {
                            self.expr_into(&p.value, vtmp)?
                        } else {
                            self.compile_named_init(vtmp, &p.value, &key)?
                        };
                        self.cx.obj_method_super = false;
                        // Shorthand method `{ m(){}, *g(){}, async a(){} }`: its
                        // toString is the whole `m(){}` (the value-Function span omits
                        // the name/modifiers). Patch the proto just compiled (last).
                        // Regular `k: function(){}` keeps the value's own span.
                        if p.method {
                            let fid = self.cx.functions.len() - 1;
                            self.cx.functions[fid].source =
                                self.cx.src_slice(p.span.start, p.span.end);
                            self.cx.functions[fid].non_constructable = true; // concise method
                        }
                        // `{ __proto__: v }` (colon form ONLY — shorthand
                        // `{ __proto__ }` is an ordinary data property) sets the
                        // prototype — a real [[Set]]/proto-setter; every other
                        // key is CreateDataProperty, which must ignore an
                        // inherited accessor / non-writable prop.
                        if key == "__proto__" && !p.method && !p.shorthand {
                            self.emit(Instr::SetProp { obj: dst, name, val: v });
                        } else {
                            self.emit(Instr::InitDataProp { obj: dst, name, val: v });
                        }
                        if p.method {
                            self.emit(Instr::SetHomeObject { method: v, home: dst });
                        }
                    }
                }
                ox::ObjectPropertyKind::SpreadProperty(s) => {
                    let src = self.expr(&s.argument)?;
                    self.emit(Instr::ObjectSpread { target: dst, src });
                }
            }
            self.next_reg = save; // reclaim this property's scratch temps
        }
        Ok(dst)
    }

    pub(crate) fn load_number(&mut self, dst: Reg, n: f64) {
        if n.fract() == 0.0 && n >= i32::MIN as f64 && n <= i32::MAX as f64 {
            self.emit(Instr::LoadInt { dst, val: n as i32 });
        } else {
            let idx = self.add_const(Value::num(n));
            self.emit(Instr::LoadConst { dst, idx });
        }
    }

    pub(crate) fn binary(&mut self, b: &ox::BinaryExpression, dst: Reg) -> R<Reg> {
        use ox::BinaryOperator as Op;
        // `x instanceof Ctor`: only built-in constructors are recognised (the
        // engine has no user prototype chain). Decided structurally in the VM.
        if matches!(b.operator, Op::Instanceof) {
            // A built-in constructor name → structural InstanceOf; anything else
            // (a user class value) → runtime InstanceOfDyn against its class link.
            if let ox::Expression::Identifier(id) = &b.right {
                if let Some(ctor) = InstanceCtor::from_name(&id.name) {
                    let val = self.expr(&b.left)?;
                    self.emit(Instr::InstanceOf { dst, val, ctor });
                    return Ok(dst);
                }
            }
            let val = self.expr(&b.left)?;
            let ctor = self.expr(&b.right)?;
            self.emit(Instr::InstanceOfDyn { dst, val, ctor });
            return Ok(dst);
        }
        // `key in obj`.
        if matches!(b.operator, Op::In) {
            let key = self.expr(&b.left)?;
            let obj = self.expr(&b.right)?;
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
        if let ox::Expression::NumericLiteral(n) = &b.right {
            let imm_ok = n.value.fract() == 0.0
                && n.value >= i32::MIN as f64
                && n.value <= i32::MAX as f64;
            // `x - 0` must stay a real Sub: AddInt would compute `x + 0`, and
            // IEEE `-0.0 + 0.0` is `+0.0` while `-0.0 - 0.0` is `-0.0`.
            // (`x + 0` is the same operation either way, so it stays eligible.)
            let eligible = (matches!(b.operator, Op::Subtraction) && n.value != 0.0)
                || (matches!(b.operator, Op::Addition) && is_numeric_expr(&b.left));
            if imm_ok && eligible {
                let a = self.expr(&b.left)?;
                let mut imm = n.value as i32;
                if matches!(b.operator, Op::Subtraction) {
                    imm = -imm;
                }
                self.emit(Instr::AddInt { dst, a, imm, upd: false });
                return Ok(dst);
            }
        }
        let a = self.expr(&b.left)?;
        let r = self.expr(&b.right)?;
        let instr = match b.operator {
            Op::Addition => Instr::Add { dst, a, b: r },
            Op::Subtraction => Instr::Sub { dst, a, b: r },
            Op::Multiplication => Instr::Mul { dst, a, b: r },
            Op::Division => Instr::Div { dst, a, b: r },
            Op::Remainder => Instr::Mod { dst, a, b: r },
            Op::LessThan => Instr::Lt { dst, a, b: r },
            Op::LessEqualThan => Instr::Le { dst, a, b: r },
            Op::GreaterThan => Instr::Gt { dst, a, b: r },
            Op::GreaterEqualThan => Instr::Ge { dst, a, b: r },
            Op::StrictEquality => Instr::Eq { dst, a, b: r },
            Op::StrictInequality => Instr::Ne { dst, a, b: r },
            Op::Equality => Instr::LooseEq { dst, a, b: r },
            Op::Inequality => Instr::LooseNe { dst, a, b: r },
            Op::BitwiseAnd => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::And },
            Op::BitwiseOR => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Or },
            Op::BitwiseXOR => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Xor },
            Op::ShiftLeft => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Shl },
            Op::ShiftRight => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Shr },
            Op::ShiftRightZeroFill => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Ushr },
            Op::Exponential => Instr::Pow { dst, a, b: r },
            _ => return Err("unsupported binary operator (zipp-vm v1)".into()),
        };
        self.emit(instr);
        Ok(dst)
    }

    pub(crate) fn logical(&mut self, l: &ox::LogicalExpression, dst: Reg) -> R<Reg> {
        use ox::LogicalOperator as Op;
        // `a && b`: eval a into dst; if falsy, short-circuit; else eval b.
        let _a = self.expr_into(&l.left, dst)?;
        if _a != dst {
            self.emit(Instr::Move { dst, src: _a });
        }
        match l.operator {
            Op::And => {
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond: dst, target: 0 });
                let b = self.expr_into(&l.right, dst)?;
                if b != dst {
                    self.emit(Instr::Move { dst, src: b });
                }
                let end = self.here();
                self.patch_jump(j, end);
            }
            Op::Or => {
                let j = self.here();
                self.emit(Instr::JumpIfTrue { cond: dst, target: 0 });
                let b = self.expr_into(&l.right, dst)?;
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
                let b = self.expr_into(&l.right, dst)?;
                if b != dst {
                    self.emit(Instr::Move { dst, src: b });
                }
                let end = self.here();
                self.patch_jump(j, end);
            }
        }
        Ok(dst)
    }

    pub(crate) fn unary(&mut self, u: &ox::UnaryExpression, dst: Reg) -> R<Reg> {
        use ox::UnaryOperator as Op;
        match u.operator {
            Op::UnaryNegation => {
                let a = self.expr(&u.argument)?;
                self.emit(Instr::Neg { dst, a });
                Ok(dst)
            }
            Op::UnaryPlus => {
                let a = self.expr(&u.argument)?;
                self.emit(Instr::ToNum { dst, a });
                Ok(dst)
            }
            Op::LogicalNot => {
                let a = self.expr(&u.argument)?;
                self.emit(Instr::Not { dst, a });
                Ok(dst)
            }
            Op::Typeof => {
                // `typeof <unbound identifier>` must yield "undefined", NOT throw
                // a ReferenceError — and this holds when the identifier is wrapped in
                // parentheses (`typeof (f)`), so peel them first. A bare identifier
                // that resolves to a global is read with the non-throwing variant so
                // the never-declared sentinel degrades to undefined.
                // (`undefined`/`NaN`/`Infinity` are literals, handled by `expr`.)
                let mut arg = &u.argument;
                while let ox::Expression::ParenthesizedExpression(p) = arg {
                    arg = &p.expression;
                }
                if let ox::Expression::Identifier(id) = arg {
                    if !matches!(id.name.as_str(), "undefined" | "NaN" | "Infinity") {
                        if let Binding::Global(idx) = self.resolve(id.name.as_str()) {
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
            Op::BitwiseNot => {
                let a = self.expr(&u.argument)?;
                self.emit(Instr::BitNot { dst, a });
                Ok(dst)
            }
            Op::Void => {
                // Evaluate the operand for side effects; the value is `undefined`.
                let _ = self.expr(&u.argument)?;
                self.emit(Instr::LoadUndefined { dst });
                Ok(dst)
            }
            Op::Delete => self.delete_expr(&u.argument, dst),
        }
    }

    /// `delete <ref>` — remove a property (`obj.x` / `obj[k]`) and yield the
    /// boolean result. A non-reference operand (or a bare identifier) evaluates
    /// for side effects and yields `true` (matching sloppy-mode `delete x`).
    pub(crate) fn delete_expr(&mut self, arg: &ox::Expression, dst: Reg) -> R<Reg> {
        match arg {
            ox::Expression::StaticMemberExpression(m) => {
                // `delete super.x` is a runtime ReferenceError (a super reference has
                // no [[Delete]]). Not a SyntaxError, so it's thrown when evaluated.
                if matches!(&m.object, ox::Expression::Super(_)) {
                    let e = self.alloc_reg();
                    self.emit(Instr::NewError { dst: e, kind: 4, arg: None, opts: None, errors: None });
                    self.emit(Instr::Throw { src: e });
                    return Ok(dst);
                }
                let obj = self.expr(&m.object)?;
                let name = self.string_name(&m.property.name);
                let strict = self.cx.in_strict;
                self.emit(Instr::DeleteProp { dst, obj, name, strict });
                Ok(dst)
            }
            ox::Expression::ComputedMemberExpression(m) => {
                // `delete super[expr]`: SuperProperty evaluation does
                // GetThisBinding BEFORE the key expression — in a derived ctor
                // before super() that ReferenceError fires FIRST and `expr`
                // never runs. Otherwise evaluate `expr` (side effects +
                // ToPropertyKey), then throw a ReferenceError — a super
                // reference has no delete.
                if matches!(&m.object, ox::Expression::Super(_)) {
                    self.this_check();
                    let _ = self.expr(&m.expression)?;
                    let e = self.alloc_reg();
                    self.emit(Instr::NewError { dst: e, kind: 4, arg: None, opts: None, errors: None });
                    self.emit(Instr::Throw { src: e });
                    return Ok(dst);
                }
                let obj = self.expr(&m.object)?;
                let strict = self.cx.in_strict;
                // Fuse `delete obj[<plain string literal> + e]` → DeleteIndexConcat
                // (no throwaway concat-key allocation; see GetIndexConcat).
                if let Some((name, rhs)) = concat_key_literal_prefix(&m.expression) {
                    let nidx = self.string_name(name);
                    let key = self.expr(rhs)?;
                    self.emit(Instr::DeleteIndexConcat { dst, obj, name: nidx, key, strict });
                    return Ok(dst);
                }
                let key = self.expr(&m.expression)?;
                self.emit(Instr::DeleteIndex { dst, obj, key, strict });
                Ok(dst)
            }
            ox::Expression::ParenthesizedExpression(p) => self.delete_expr(&p.expression, dst),
            // `delete <identifier>`: in strict mode an early SyntaxError; in sloppy
            // mode deleting a resolvable binding (var/let/const/param/function or a
            // declared global) yields `false` (non-configurable), while an
            // unresolvable name is a no-op that yields `true` — and must NOT be
            // evaluated (evaluating an undeclared name would throw ReferenceError).
            ox::Expression::Identifier(id) => {
                if self.cx.in_strict {
                    return Err(
                        "SyntaxError: Delete of an unqualified identifier in strict mode".into(),
                    );
                }
                // Inside a `with`, `delete name` removes the binding from the
                // innermost with-object that has it (yielding its delete result),
                // else falls through to the static-binding delete semantics below.
                let with_objs = self.with_obj_regs(id.name.as_str());
                if !with_objs.is_empty() {
                    return Ok(self.delete_with(id.name.as_str(), &with_objs, dst));
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
                if matches!(id.name.as_str(), "NaN" | "Infinity" | "undefined") {
                    self.emit(Instr::LoadBool { dst, val: false });
                    return Ok(dst);
                }
                match self.resolve_existing(&id.name) {
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
                        let slot = self.cx.global_slot(&id.name) as u32;
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

    pub(crate) fn update(&mut self, u: &ox::UpdateExpression, dst: Reg) -> R<Reg> {
        let delta = match u.operator {
            ox::UpdateOperator::Increment => 1,
            ox::UpdateOperator::Decrement => -1,
        };
        // `obj.x++` / `arr[i]--` etc — read the member, yield old (postfix) or
        // new (prefix), write the incremented value back to the same slot.
        match &u.argument {
            // `super.x++` / `--super.x` — read via the super-get sequence,
            // coerce/step, write back via the super-set sequence.
            ox::SimpleAssignmentTarget::StaticMemberExpression(m)
                if matches!(&m.object, ox::Expression::Super(_)) =>
            {
                let pid = self.super_class;
                if pid.is_none() && !self.super_home_obj {
                    return Err("`super.x++` is only valid in a method".into());
                }
                self.this_check();
                let name = self.string_name(m.property.name.as_str());
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
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { oldnum } });
                return Ok(dst);
            }
            // `super[k]++` / `--super[k]` — SuperProperty evaluation checks the
            // this-TDZ BEFORE evaluating the key Expression
            // (prop-expr-uninitialized-this-putvalue-increment), and the
            // computed super ops capture GetSuperBase before ToPropertyKey.
            ox::SimpleAssignmentTarget::ComputedMemberExpression(m)
                if matches!(&m.object, ox::Expression::Super(_)) =>
            {
                let pid = self.super_class;
                if pid.is_none() && !self.super_home_obj {
                    return Err("`super[k]++` is only valid in a method".into());
                }
                self.this_check();
                let key = self.expr(&m.expression)?;
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
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { oldnum } });
                return Ok(dst);
            }
            ox::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                let obj = self.expr(&m.object)?;
                let name = self.string_name(m.property.name.as_str());
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
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { oldnum } });
                return Ok(dst);
            }
            ox::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
                let obj = self.expr(&m.object)?;
                let key = self.expr(&m.expression)?;
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
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { oldnum } });
                return Ok(dst);
            }
            // `obj.#x++` — like a static member, keyed "#x".
            ox::SimpleAssignmentTarget::PrivateFieldExpression(p) => {
                self.check_private_declared(&p.field.name)?;
                let obj = self.expr(&p.object)?;
                let name = self.string_name(&private_key(&p.field.name));
                let cur = self.temp();
                self.emit(Instr::GetProp { dst: cur, obj, name });
                let oldnum = self.temp();
                self.emit(Instr::AddInt { dst: oldnum, a: cur, imm: 0, upd: true });
                let nw = self.temp();
                self.emit(Instr::AddInt { dst: nw, a: oldnum, imm: delta, upd: true });
                self.emit(Instr::SetProp { obj, name, val: nw });
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { oldnum } });
                return Ok(dst);
            }
            _ => {}
        }
        // `x++` / `++x` / `x--` / `--x` on a simple identifier.
        let name = match &u.argument {
            ox::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => id.name.to_string(),
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
            if u.prefix {
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
                if u.prefix {
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
        if u.prefix {
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
