// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'a> FnCompiler<'a> {
    /// Emit a read of `binding` into `dst`; returns the register holding the
    /// value (the binding's own register for a plain Local, else `dst`).
    pub(crate) fn load_binding(&mut self, binding: &Binding, dst: Reg) -> Reg {
        match binding {
            Binding::Local(r) => *r,
            Binding::LocalCell(cell) => {
                self.emit(Instr::CellGet { dst, cell: *cell });
                dst
            }
            Binding::Upvalue(idx) => {
                // A sloppy contains-direct-eval function: an eval-introduced
                // function-scoped `var` shadows the captured name for READS.
                if !self.cx.in_strict && self.box_all_locals {
                    let name = self.upvalues.borrow()[*idx as usize].0.clone();
                    let slot = self.cx.global_slot(&name) as u32;
                    self.emit(Instr::LoadUpvalDyn { dst, idx: *idx, name: slot });
                } else {
                    self.emit(Instr::UpvalGet { dst, idx: *idx });
                }
                dst
            }
            Binding::Global(idx) => {
                if self.box_all_locals || self.cx.dyn_global_zone {
                    self.emit(Instr::LoadGlobalDyn { dst, idx: *idx });
                } else {
                    self.emit(Instr::LoadGlobal { dst, idx: *idx });
                }
                dst
            }
            Binding::ClassName(class_id) => {
                self.emit(Instr::LoadClassValue { dst, class_id: *class_id });
                dst
            }
        }
    }

    /// Emit a write of `src` to `binding`.
    /// True if `r` is the register holding a named function/generator expression's
    /// own name — an IMMUTABLE binding inside its body.
    pub(crate) fn is_self_name_reg(&self, r: Reg) -> bool {
        self.self_name.as_ref().is_some_and(|(_, sr)| *sr == r)
    }

    pub(crate) fn store_binding(&mut self, b: &Binding, src: Reg) {
        // A named function expression's own name is an immutable binding: assigning
        // to it inside the body throws a TypeError in strict mode and is a silent
        // no-op in sloppy mode (the RHS in `src` was already evaluated for its side
        // effects). Unlike `const`, the sloppy case does NOT throw.
        if let Binding::Local(r) | Binding::LocalCell(r) = b {
            if self.is_self_name_reg(*r) {
                if self.cx.in_strict {
                    let e = self.alloc_reg();
                    self.emit(Instr::NewError { dst: e, kind: 1, arg: None, opts: None, errors: None });
                    self.emit(Instr::Throw { src: e });
                    self.next_reg -= 1;
                }
                return;
            }
        }
        // Assignment to a `const` binding is a runtime TypeError (PutValue on an
        // immutable binding). The RHS has already been evaluated into `src` (its
        // side effects must happen first), so emit the throw now. Initialization
        // uses Move/CellSet/StoreGlobal directly, never this path.
        let is_const = match b {
            Binding::Local(r) | Binding::LocalCell(r) => self.const_regs.contains(r),
            Binding::Global(idx) => self.cx.const_globals.contains(idx),
            Binding::Upvalue(_) => false, // a const captured by a closure: not tracked
            Binding::ClassName(_) => true, // the inner class-name binding is immutable
        };
        if is_const {
            let e = self.alloc_reg();
            self.emit(Instr::NewError { dst: e, kind: 1, arg: None, opts: None, errors: None });
            self.emit(Instr::Throw { src: e });
            self.next_reg -= 1;
            return;
        }
        match b {
            Binding::Local(r) => {
                if *r != src {
                    self.emit(Instr::Move { dst: *r, src });
                }
            }
            Binding::LocalCell(cell) => {
                // A block-entry pre-created lexical cell whose declaration has
                // not yet been compiled: the assignment may run during its TDZ,
                // so the checked store rejects an UNINITIALIZED cell.
                if self.block_tdz_cells.contains(cell) || self.entry_tdz_cells.contains(cell) {
                    self.emit(Instr::CellSetChecked { cell: *cell, src });
                } else {
                    self.emit(Instr::CellSet { cell: *cell, src });
                }
            }
            Binding::Upvalue(idx) => {
                // A sloppy contains-direct-eval function: SetMutableBinding
                // resolves at store time — an eval-introduced shadow wins.
                if !self.cx.in_strict && self.box_all_locals {
                    let name = self.upvalues.borrow()[*idx as usize].0.clone();
                    let slot = self.cx.global_slot(&name) as u32;
                    self.emit(Instr::StoreUpvalDyn { idx: *idx, src, name: slot });
                } else {
                    self.emit(Instr::UpvalSet { idx: *idx, src });
                }
            }
            Binding::Global(idx) => {
                // In strict mode, assigning to an unresolvable (never-declared) global
                // is a ReferenceError, not a silent global creation. A top-level
                // lexical (`let`) binding is likewise checked even in sloppy mode: a
                // store while it is still in its TDZ (UNINITIALIZED) is a ReferenceError.
                if self.cx.in_strict || self.cx.lexical_globals.contains(idx) {
                    self.emit(Instr::StoreGlobalStrict { idx: *idx, src });
                } else if self.box_all_locals || self.cx.dyn_global_zone {
                    self.emit(Instr::StoreGlobalDyn { idx: *idx, src });
                } else {
                    self.emit(Instr::StoreGlobal { idx: *idx, src });
                }
            }
            // Unreachable: the inner class binding is const (is_const above threw).
            Binding::ClassName(_) => {}
        }
    }

    /// Whether the current compile position allows a PROPER TAIL CALL: a
    /// strict function body with nothing to unwind through on return — no
    /// try handlers, no enclosing loop holding an iterator to close, no
    /// `using` scope — and not a generator/async/script/eval body (their
    /// returns thread extra machinery).
    pub(crate) fn tail_call_position(&self) -> bool {
        self.cx.in_strict
            && !self.is_script
            && self.handler_depth == 0
            && self.completion_reg.is_none()
            && self.using_scope_reg.is_none()
            && !self.in_generator
            && !self.in_async
            && !self.in_param_init
            && self.loop_ctx.iter().all(|c| c.iter_close.is_none())
    }

    /// Whether the call expression itself is tail-callable: a plain
    /// (non-optional, spread-free) call whose callee carries no receiver —
    /// an identifier or another plain call. An identifier `eval` qualifies
    /// in all forms EXCEPT a with-shadowable site (the with-chain call binds
    /// `this` to the with-object — not frame-reusable): a compile-time
    /// direct eval gets the `DirectEval { tail }` form (frame reuse fires
    /// only when `eval` is REBOUND at runtime), and a user-shadowed `eval`
    /// is an ordinary call.
    pub(crate) fn tail_callable(&mut self, c: &ox::CallExpression) -> bool {
        if c.optional || c.arguments.iter().any(|a| a.as_expression().is_none()) {
            return false;
        }
        fn callee_ok(e: &ox::Expression) -> bool {
            match e {
                // An identifier — including `eval` (direct eval gets the
                // DirectEval{tail} form; a shadowed/with-resolved `eval` is an
                // ordinary call) and with-shadowable names (lowered through
                // the with chain with a TailCallWithThis prefix).
                ox::Expression::Identifier(_) => true,
                ox::Expression::CallExpression(inner) => !inner.optional,
                ox::Expression::ParenthesizedExpression(p) => callee_ok(&p.expression),
                _ => false,
            }
        }
        callee_ok(&c.callee)
    }

    /// Whether `e` contains a tail-callable call in a spec TAIL POSITION:
    /// the expression itself, a conditional's arms, a logical operator's
    /// right operand, a sequence's final element, a parenthesized inner, or
    /// a (plain-tag) tagged template. Pure predicate — mirrors exactly what
    /// `emit_tail_return` lowers, so the return statement either emits the
    /// whole tail-aware form or falls back to the ordinary path untouched.
    pub(crate) fn expr_has_tail_call(&mut self, e: &ox::Expression) -> bool {
        use ox::Expression as E;
        match e {
            E::ParenthesizedExpression(p) => self.expr_has_tail_call(&p.expression),
            E::ConditionalExpression(c) => {
                self.expr_has_tail_call(&c.consequent) || self.expr_has_tail_call(&c.alternate)
            }
            E::LogicalExpression(l) => self.expr_has_tail_call(&l.right),
            E::SequenceExpression(s) => match s.expressions.last() {
                Some(last) => self.expr_has_tail_call(last),
                None => false,
            },
            E::CallExpression(c) => self.tail_callable(c),
            E::TaggedTemplateExpression(tt) => self.tagged_tail_callable(tt),
            _ => false,
        }
    }

    /// A tagged template is tail-callable when its tag is a plain callee
    /// (identifier / call / parenthesized of those — no member tag, whose
    /// call binds `this` to the object; `String.raw` keeps its fast path).
    pub(crate) fn tagged_tail_callable(&mut self, tt: &ox::TaggedTemplateExpression) -> bool {
        match &tt.tag {
            ox::Expression::Identifier(id) => {
                self.with_objs_for(id.name.as_str()).is_empty()
                    && !self.inherited_with_shadows.contains_key(id.name.as_str())
            }
            ox::Expression::CallExpression(inner) => !inner.optional,
            ox::Expression::ParenthesizedExpression(p) => matches!(
                &p.expression,
                ox::Expression::Identifier(_) | ox::Expression::CallExpression(_)
            ),
            _ => false,
        }
    }

    /// Lower `return e;` where `e` HAS a tail call in tail position (see
    /// `expr_has_tail_call`): every control path ends in a `Return`, with the
    /// `TailCall` frame-reuse prefix emitted in front of each tail-position
    /// call. Only entered from a `tail_call_position()` context (strict, no
    /// handlers / iterator closes / using scopes / generator / async).
    pub(crate) fn emit_tail_return(&mut self, e: &ox::Expression) -> R<()> {
        use ox::Expression as E;
        match e {
            E::ParenthesizedExpression(p) => self.emit_tail_return(&p.expression),
            E::ConditionalExpression(c) => {
                let save = self.next_reg;
                let cond = self.expr(&c.test)?;
                let jf = self.here();
                self.emit(Instr::JumpIfFalse { cond, target: 0 });
                self.next_reg = save;
                self.emit_tail_return(&c.consequent)?; // every path returns
                let alt = self.here();
                self.patch_jump(jf, alt);
                self.emit_tail_return(&c.alternate)
            }
            E::LogicalExpression(l) => {
                use ox::LogicalOperator as Op;
                let save = self.next_reg;
                let v = self.alloc_reg();
                let lv = self.expr_into(&l.left, v)?;
                if lv != v {
                    self.emit(Instr::Move { dst: v, src: lv });
                }
                // Short-circuit → return the LEFT value; else the right
                // operand is in tail position.
                let jshort = match l.operator {
                    Op::And => {
                        let j = self.here();
                        self.emit(Instr::JumpIfFalse { cond: v, target: 0 });
                        j
                    }
                    Op::Or => {
                        let j = self.here();
                        self.emit(Instr::JumpIfTrue { cond: v, target: 0 });
                        j
                    }
                    Op::Coalesce => {
                        let tsave = self.next_reg;
                        let undef = self.alloc_reg();
                        let isnull = self.alloc_reg();
                        self.emit_is_nullish(v, isnull, undef);
                        let j = self.here();
                        // non-nullish → return v
                        self.emit(Instr::JumpIfFalse { cond: isnull, target: 0 });
                        self.next_reg = tsave;
                        // nullish: the right operand is the tail position
                        self.emit_tail_return(&l.right)?;
                        let keep = self.here();
                        self.patch_jump(j, keep);
                        self.emit(Instr::Return { src: v });
                        self.next_reg = save;
                        return Ok(());
                    }
                };
                self.emit_tail_return(&l.right)?;
                let short = self.here();
                self.patch_jump(jshort, short);
                self.emit(Instr::Return { src: v });
                self.next_reg = save;
                Ok(())
            }
            E::SequenceExpression(s) if !s.expressions.is_empty() => {
                let n = s.expressions.len();
                for ex in &s.expressions[..n - 1] {
                    let save = self.next_reg;
                    self.expr(ex)?;
                    self.next_reg = save;
                }
                self.emit_tail_return(&s.expressions[n - 1])
            }
            E::CallExpression(c) if self.tail_callable(c) => {
                // A with-shadowable identifier callee: the with-chain resolves
                // the callee + `this` (= the with-object), then the frame is
                // reused via TailCallWithThis (tco-non-eval-with).
                if let E::Identifier(id) = &c.callee {
                    let with_objs = self.with_obj_regs(id.name.as_str());
                    if !with_objs.is_empty() {
                        let save = self.next_reg;
                        let (callee_reg, this_reg) =
                            self.emit_with_callee_chain(id.name.as_str(), &with_objs);
                        let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                        self.emit(Instr::TailCallWithThis {
                            callee: callee_reg,
                            this_v: this_reg,
                            arg_base,
                            argc,
                        });
                        let dst = self.alloc_reg();
                        self.emit(Instr::CallWithThis {
                            dst,
                            callee: callee_reg,
                            this_v: this_reg,
                            arg_base,
                            argc,
                        });
                        self.emit(Instr::Return { src: dst });
                        self.next_reg = save;
                        return Ok(());
                    }
                }
                // A compile-time DIRECT eval in tail position: the DirectEval
                // op itself frame-reuses only when `eval` is REBOUND at
                // runtime (an ordinary call); the genuine-eval path is not a
                // tail call per spec.
                if let E::Identifier(id) = &c.callee {
                    if id.name == "eval"
                        && matches!(self.resolve("eval"), Binding::Global(_))
                        && self.with_objs_for("eval").is_empty()
                    {
                        let save = self.next_reg;
                        let dst = self.alloc_reg();
                        let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                        let arg = if argc == 0 {
                            let r = self.temp();
                            self.emit(Instr::LoadUndefined { dst: r });
                            r
                        } else {
                            arg_base
                        };
                        self.emit_direct_eval(arg, dst, true);
                        self.emit(Instr::Return { src: dst });
                        self.next_reg = save;
                        return Ok(());
                    }
                }
                let save = self.next_reg;
                let ct = self.alloc_reg();
                let cv = self.expr_into(&c.callee, ct)?;
                if cv != ct {
                    self.emit(Instr::Move { dst: ct, src: cv });
                }
                let exprs: Vec<&ox::Expression> =
                    c.arguments.iter().filter_map(|a| a.as_expression()).collect();
                let arg_base = self.eval_contiguous(&exprs)?;
                let argc = exprs.len() as u16;
                self.emit(Instr::TailCall { callee: ct, arg_base, argc });
                let dst = self.alloc_reg();
                self.emit(Instr::Call { dst, callee: ct, arg_base, argc });
                self.emit(Instr::Return { src: dst });
                self.next_reg = save;
                Ok(())
            }
            E::TaggedTemplateExpression(tt) if self.tagged_tail_callable(tt) => {
                let save = self.next_reg;
                let dst = self.alloc_reg();
                self.tagged_template_tail(tt, dst)?;
                self.emit(Instr::Return { src: dst });
                self.next_reg = save;
                Ok(())
            }
            // A tail position that bottomed out in a non-tail-callable
            // expression: ordinary evaluate + return.
            other => {
                let save = self.next_reg;
                let v = self.expr(other)?;
                self.emit(Instr::Return { src: v });
                self.next_reg = save;
                Ok(())
            }
        }
    }

    /// Assignment-reference SNAPSHOT for a sloppy direct-eval zone: PutValue
    /// writes the reference resolved BEFORE the RHS ran, so a `var` binding a
    /// direct eval in the RHS introduces is visible to later reads but NOT to
    /// the in-flight assignment. Emits an `EvalScopeHas` probe (None when the
    /// target isn't a dyn-zone sloppy global — then `store_binding` is exact).
    pub(crate) fn eval_snap_probe(&mut self, binding: &Binding) -> Option<Reg> {
        match binding {
            Binding::Global(idx)
                if !self.cx.in_strict
                    && !self.cx.lexical_globals.contains(idx)
                    && (self.box_all_locals || self.cx.dyn_global_zone) =>
            {
                let p = self.alloc_reg();
                self.emit(Instr::EvalScopeHas { dst: p, idx: *idx });
                Some(p)
            }
            Binding::Upvalue(idx) if !self.cx.in_strict && self.box_all_locals => {
                let name = self.upvalues.borrow()[*idx as usize].0.clone();
                let slot = self.cx.global_slot(&name) as u32;
                let p = self.alloc_reg();
                self.emit(Instr::EvalScopeHas { dst: p, idx: slot });
                Some(p)
            }
            _ => None,
        }
    }

    /// Store through a reference snapshotted by `eval_snap_probe`: the probed
    /// state (not the store-time state) picks EvalScope vs the static target.
    pub(crate) fn store_binding_snapped(&mut self, b: &Binding, src: Reg, snap: Option<Reg>) {
        let (p, name_slot, static_store): (Reg, u32, Instr) = match (snap, b) {
            (Some(p), Binding::Global(idx)) => {
                (p, *idx, Instr::StoreGlobal { idx: *idx, src })
            }
            (Some(p), Binding::Upvalue(uidx)) => {
                let name = self.upvalues.borrow()[*uidx as usize].0.clone();
                let slot = self.cx.global_slot(&name) as u32;
                (p, slot, Instr::UpvalSet { idx: *uidx, src })
            }
            _ => {
                self.store_binding(b, src);
                return;
            }
        };
        let j_scope = self.here();
        self.emit(Instr::JumpIfTrue { cond: p, target: 0 });
        self.emit(static_store);
        let j_end = self.here();
        self.emit(Instr::Jump { target: 0 });
        let at_scope = self.here();
        self.patch_jump(j_scope, at_scope);
        self.emit(Instr::EvalScopeSet { idx: name_slot, src });
        let end = self.here();
        self.patch_jump(j_end, end);
    }

    /// Bind all parameters at function entry, strictly LEFT-TO-RIGHT, applying each
    /// one's `= default` and (for a destructuring pattern) extracting it before
    /// moving to the next. The single interleaved pass is required by the spec:
    /// a later parameter's default may reference an earlier (already-bound)
    /// parameter — `function f([x, y] = [1, 2], z = x + y)` must see x, y bound
    /// when it evaluates `z`. (A two-pass "all defaults, then all destructuring"
    /// order would read those names before the pattern extracted them.)
    pub(crate) fn bind_params(&mut self, params: &ox::FormalParameters) -> R<()> {
        // Parameter defaults compile inside this call — a direct eval there is
        // in the PARAM scope (see FnCompiler::in_param_init).
        self.in_param_init = true;
        let r = self.bind_params_inner(params);
        self.in_param_init = false;
        r
    }

    pub(crate) fn bind_params_inner(&mut self, params: &ox::FormalParameters) -> R<()> {
        // Ordered identifier-parameter names, for Temporal-Dead-Zone tracking of a
        // default initializer that references the parameter itself or a later one.
        let param_names: Vec<Option<String>> = params
            .items
            .iter()
            .map(|item| match &item.pattern {
                ox::BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
                _ => None,
            })
            .collect();
        for (i, item) in params.items.iter().enumerate() {
            // While compiling param i's default, param i and every LATER identifier
            // parameter are in the TDZ (a self/forward reference throws); earlier
            // parameters are already bound, so backward references resolve normally.
            self.param_tdz.clear();
            for n in param_names.iter().skip(i).flatten() {
                self.param_tdz.insert(n.clone());
            }
            match &item.pattern {
                // `x = default`: if (x === undefined) x = default.
                ox::BindingPattern::BindingIdentifier(id) => {
                    if let Some(default) = &item.initializer {
                        let name = id.name.to_string();
                        self.emit_ident_param_default(&name, default)?;
                    }
                }
                // A destructuring pattern: apply its parameter-level default to the
                // incoming argument register (when undefined) BEFORE extracting.
                ox::BindingPattern::ObjectPattern(_) | ox::BindingPattern::ArrayPattern(_) => {
                    if let Some(default) = &item.initializer {
                        self.apply_default_in_place((i + 1) as Reg, default)?;
                    }
                    self.declare_pattern(&item.pattern)?;
                    let save = self.next_reg;
                    self.extract_pattern(&item.pattern, (i + 1) as Reg)?;
                    self.next_reg = save;
                }
                _ => {}
            }
        }
        // A destructuring rest parameter (`function f(...[a,b])`): the overflow args
        // were gathered into the rest array (rest_reg, the synthetic `<rest>` slot);
        // destructure that array into the pattern's leaves, like a normal pattern param.
        if let Some(r) = &params.rest {
            if !matches!(&r.rest.argument, ox::BindingPattern::BindingIdentifier(_)) {
                if let Some(rr) = self.rest_reg {
                    self.declare_pattern(&r.rest.argument)?;
                    let save = self.next_reg;
                    self.extract_pattern(&r.rest.argument, rr)?;
                    self.next_reg = save;
                }
            }
        }
        self.param_tdz.clear(); // the body resolves parameters normally
        Ok(())
    }

    /// Emit `if (x === undefined) x = default` for one identifier parameter. Param
    /// regs are already bound (captured ones boxed), so reads/writes go through
    /// resolve + load_binding/store_binding (plain locals and cells uniformly).
    pub(crate) fn emit_ident_param_default(&mut self, name: &str, default: &ox::Expression) -> R<()> {
        let b = self.resolve(name);
        let save = self.next_reg;
        let prtmp = self.alloc_reg();
        let pr = self.load_binding(&b, prtmp);
        let undef = self.alloc_reg();
        self.emit(Instr::LoadUndefined { dst: undef });
        let cond = self.alloc_reg();
        self.emit(Instr::Eq { dst: cond, a: pr, b: undef });
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 }); // skip when x !== undefined
        let dtmp = self.alloc_reg();
        // `function f(x = function(){})` ⇒ the default takes the name "x".
        let dv = self.compile_named_init(dtmp, default, name)?;
        self.store_binding(&b, dv);
        let end = self.here();
        self.patch_jump(jf, end);
        // The init temps are dead before the body; reclaim them (max_reg has
        // already captured the high-water) so body locals reuse the registers.
        self.next_reg = save;
        Ok(())
    }

}
