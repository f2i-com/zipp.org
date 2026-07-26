// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;
// The AST this module consumes. Imported explicitly rather than relying on how
// the parent spells its own import. NOTE: `ast::Program` and
// `crate::bytecode::Program` are both in scope through globs, so `Program` is
// deliberately never named in this file.
use crate::parse::ast::*;

/// The expression of a non-spread argument — the replacement for oxc's
/// `Argument::as_expression()`.
fn arg_expr(a: &Arg) -> Option<&Expr> {
    match a {
        Arg::Expr(e) => Some(e),
        Arg::Spread(_) => None,
    }
}

/// A parameter item split into its pattern and its `= default`. oxc kept those
/// in two fields (`FormalParameter::{pattern, initializer}`); this AST folds the
/// default into `Pattern::Assign`, so it is peeled back off here — everything
/// downstream (`declare_pattern`, `extract_pattern`, the TDZ name list) wants
/// the bare pattern, exactly as it did before.
fn param_parts(p: &Pattern) -> (&Pattern, Option<&Expr>) {
    match p {
        Pattern::Assign { left, right } => (left, Some(right)),
        other => (other, None),
    }
}

/// Split `Params.items` into the positional parameters and the rest pattern.
/// oxc parked the rest in its own field; this AST appends it as a trailing
/// `Pattern::Rest` (the grammar puts it last and nothing may follow it). The
/// loop in `bind_params_inner` indexes argument registers BY POSITION, so the
/// rest has to come back off the end or every index would shift.
fn split_rest(items: &[Pattern]) -> (&[Pattern], Option<&Pattern>) {
    match items.last() {
        Some(Pattern::Rest(inner)) => (&items[..items.len() - 1], Some(&**inner)),
        _ => (items, None),
    }
}

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
    pub(crate) fn tail_callable(&mut self, c: &CallExpr) -> bool {
        if c.optional || c.args.iter().any(|a| matches!(a, Arg::Spread(_))) {
            return false;
        }
        fn callee_ok(e: &Expr) -> bool {
            match e {
                // An identifier — including `eval` (direct eval gets the
                // DirectEval{tail} form; a shadowed/with-resolved `eval` is an
                // ordinary call) and with-shadowable names (lowered through
                // the with chain with a TailCallWithThis prefix).
                Expr::Ident(_) => true,
                Expr::Call(inner) => !inner.optional,
                // (The parenthesized arm is gone with the node: `(f)()` and
                // `(f())()` now arrive as the identifier / call themselves,
                // which is what peeling produced anyway.)
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
    pub(crate) fn expr_has_tail_call(&mut self, e: &Expr) -> bool {
        match e {
            Expr::Cond { cons, alt, .. } => {
                self.expr_has_tail_call(cons) || self.expr_has_tail_call(alt)
            }
            Expr::Logical { right, .. } => self.expr_has_tail_call(right),
            Expr::Seq(exprs) => match exprs.last() {
                Some(last) => self.expr_has_tail_call(last),
                None => false,
            },
            Expr::Call(c) => self.tail_callable(c),
            Expr::TaggedTemplate { tag, .. } => self.tagged_tail_callable(tag),
            _ => false,
        }
    }

    /// A tagged template is tail-callable when its tag is a plain callee
    /// (identifier / call — no member tag, whose call binds `this` to the
    /// object; `String.raw` keeps its fast path).
    // NOTE: signature. `Expr::TaggedTemplate { tag, quasi }` inlines the node, so
    // this takes the TAG expression.
    //
    // NOTE: behaviour, forced by the AST. The old oxc arm returned `true` for a
    // PARENTHESIZED identifier tag WITHOUT the with-shadow test that a bare
    // identifier tag gets, so `` with (o) { (f)`x` } `` used to be treated as
    // tail-callable and `` f`x` `` was not. Parens are no longer a node, so both
    // now take the identifier arm — i.e. the with-shadowed case falls back to the
    // ordinary (non-tail) lowering. There is no bit left to reproduce the old
    // split; this is the only reachable difference in this function.
    pub(crate) fn tagged_tail_callable(&mut self, tag: &Expr) -> bool {
        match tag {
            Expr::Ident(id) => {
                self.with_objs_for(id).is_empty()
                    && !self.inherited_with_shadows.contains_key(&**id)
            }
            Expr::Call(inner) => !inner.optional,
            _ => false,
        }
    }

    /// Lower `return e;` where `e` HAS a tail call in tail position (see
    /// `expr_has_tail_call`): every control path ends in a `Return`, with the
    /// `TailCall` frame-reuse prefix emitted in front of each tail-position
    /// call. Only entered from a `tail_call_position()` context (strict, no
    /// handlers / iterator closes / using scopes / generator / async).
    pub(crate) fn emit_tail_return(&mut self, e: &Expr) -> R<()> {
        match e {
            Expr::Cond { test, cons, alt } => {
                let save = self.next_reg;
                let cond = self.expr(test)?;
                let jf = self.here();
                self.emit(Instr::JumpIfFalse { cond, target: 0 });
                self.next_reg = save;
                self.emit_tail_return(cons)?; // every path returns
                let alt_at = self.here();
                self.patch_jump(jf, alt_at);
                self.emit_tail_return(alt)
            }
            Expr::Logical { op, left, right } => {
                let save = self.next_reg;
                let v = self.alloc_reg();
                let lv = self.expr_into(left, v)?;
                if lv != v {
                    self.emit(Instr::Move { dst: v, src: lv });
                }
                // Short-circuit → return the LEFT value; else the right
                // operand is in tail position.
                let jshort = match op {
                    LogicalOp::And => {
                        let j = self.here();
                        self.emit(Instr::JumpIfFalse { cond: v, target: 0 });
                        j
                    }
                    LogicalOp::Or => {
                        let j = self.here();
                        self.emit(Instr::JumpIfTrue { cond: v, target: 0 });
                        j
                    }
                    LogicalOp::Coalesce => {
                        let tsave = self.next_reg;
                        let undef = self.alloc_reg();
                        let isnull = self.alloc_reg();
                        self.emit_is_nullish(v, isnull, undef);
                        let j = self.here();
                        // non-nullish → return v
                        self.emit(Instr::JumpIfFalse { cond: isnull, target: 0 });
                        self.next_reg = tsave;
                        // nullish: the right operand is the tail position
                        self.emit_tail_return(right)?;
                        let keep = self.here();
                        self.patch_jump(j, keep);
                        self.emit(Instr::Return { src: v });
                        self.next_reg = save;
                        return Ok(());
                    }
                };
                self.emit_tail_return(right)?;
                let short = self.here();
                self.patch_jump(jshort, short);
                self.emit(Instr::Return { src: v });
                self.next_reg = save;
                Ok(())
            }
            Expr::Seq(exprs) if !exprs.is_empty() => {
                let n = exprs.len();
                for ex in &exprs[..n - 1] {
                    let save = self.next_reg;
                    self.expr(ex)?;
                    self.next_reg = save;
                }
                self.emit_tail_return(&exprs[n - 1])
            }
            Expr::Call(c) if self.tail_callable(c) => {
                // A with-shadowable identifier callee: the with-chain resolves
                // the callee + `this` (= the with-object), then the frame is
                // reused via TailCallWithThis (tco-non-eval-with).
                if let Expr::Ident(id) = &c.callee {
                    let with_objs = self.with_obj_regs(id);
                    if !with_objs.is_empty() {
                        let save = self.next_reg;
                        let (callee_reg, this_reg) = self.emit_with_callee_chain(id, &with_objs);
                        let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
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
                if let Expr::Ident(id) = &c.callee {
                    if &**id == "eval"
                        && matches!(self.resolve("eval"), Binding::Global(_))
                        && self.with_objs_for("eval").is_empty()
                    {
                        let save = self.next_reg;
                        let dst = self.alloc_reg();
                        let (arg_base, argc) = self.eval_args_contiguous(&c.args)?;
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
                let exprs: Vec<&Expr> = c.args.iter().filter_map(arg_expr).collect();
                let arg_base = self.eval_contiguous(&exprs)?;
                let argc = exprs.len() as u16;
                self.emit(Instr::TailCall { callee: ct, arg_base, argc });
                let dst = self.alloc_reg();
                self.emit(Instr::Call { dst, callee: ct, arg_base, argc });
                self.emit(Instr::Return { src: dst });
                self.next_reg = save;
                Ok(())
            }
            // NOTE: signature. `tagged_template_tail` took an
            // `ox::TaggedTemplateExpression`; with the node inlined into
            // `Expr::TaggedTemplate { tag, quasi }` it takes the two payloads —
            // `fn tagged_template_tail(&mut self, tag: &Expr, quasi: &TemplateLit,
            // dst: Reg) -> R<Reg>` (owned by `compile/exprs.rs`).
            Expr::TaggedTemplate { tag, quasi } if self.tagged_tail_callable(tag) => {
                let save = self.next_reg;
                let dst = self.alloc_reg();
                self.tagged_template_tail(tag, quasi, dst)?;
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
    pub(crate) fn bind_params(&mut self, params: &Params) -> R<()> {
        // Parameter defaults compile inside this call — a direct eval there is
        // in the PARAM scope (see FnCompiler::in_param_init).
        self.in_param_init = true;
        let r = self.bind_params_inner(params);
        self.in_param_init = false;
        r
    }

    pub(crate) fn bind_params_inner(&mut self, params: &Params) -> R<()> {
        // `Params.simple` is precomputed by the front end and is not recomputed
        // here; this function only needs the positional/rest split.
        let (items, rest) = split_rest(&params.items);
        // Ordered identifier-parameter names, for Temporal-Dead-Zone tracking of a
        // default initializer that references the parameter itself or a later one.
        // A `= default` is a `Pattern::Assign` wrapper, so it is peeled before the
        // identifier test — `function f(x = 1, y = x)` still lists `x`.
        let param_names: Vec<Option<String>> = items
            .iter()
            .map(|item| match param_parts(item).0 {
                Pattern::Ident(id) => Some(id.to_string()),
                _ => None,
            })
            .collect();
        for (i, item) in items.iter().enumerate() {
            // While compiling param i's default, param i and every LATER identifier
            // parameter are in the TDZ (a self/forward reference throws); earlier
            // parameters are already bound, so backward references resolve normally.
            self.param_tdz.clear();
            for n in param_names.iter().skip(i).flatten() {
                self.param_tdz.insert(n.clone());
            }
            let (pat, default) = param_parts(item);
            match pat {
                // `x = default`: if (x === undefined) x = default.
                Pattern::Ident(id) => {
                    if let Some(default) = default {
                        let name = id.to_string();
                        self.emit_ident_param_default(&name, default)?;
                    }
                }
                // A destructuring pattern: apply its parameter-level default to the
                // incoming argument register (when undefined) BEFORE extracting.
                Pattern::Object { .. } | Pattern::Array(_) => {
                    if let Some(default) = default {
                        self.apply_default_in_place((i + 1) as Reg, default)?;
                    }
                    self.declare_pattern(pat)?;
                    let save = self.next_reg;
                    self.extract_pattern(pat, (i + 1) as Reg)?;
                    self.next_reg = save;
                }
                _ => {}
            }
        }
        // A destructuring rest parameter (`function f(...[a,b])`): the overflow args
        // were gathered into the rest array (rest_reg, the synthetic `<rest>` slot);
        // destructure that array into the pattern's leaves, like a normal pattern param.
        if let Some(rest) = rest {
            if !matches!(rest, Pattern::Ident(_)) {
                if let Some(rr) = self.rest_reg {
                    self.declare_pattern(rest)?;
                    let save = self.next_reg;
                    self.extract_pattern(rest, rr)?;
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
    pub(crate) fn emit_ident_param_default(&mut self, name: &str, default: &Expr) -> R<()> {
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
