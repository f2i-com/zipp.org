// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
//
// NOTE: this module names no AST type. It is register allocation, the lexical
// scope chain, upvalue threading and the constant pools — all of which were
// already expressed in terms of `String`/`Reg`/`Value` — so the port touches
// exactly one doc comment (`add_string_const_wtf8`, below), whose reference to
// oxc's `.lone_surrogates` flag no longer names anything. No signature and no
// emitted instruction changes.
#![allow(unused_imports)]
use super::*;

impl<'a> FnCompiler<'a> {
    pub(crate) fn new(
        cx: &'a mut Compiler,
        params: &[String],
        rest: Option<&str>,
        captured: HashSet<String>,
        enclosing: Vec<EnclosingFn>,
    ) -> FnCompiler<'a> {
        let inherited_with_shadows = std::mem::take(&mut cx.pending_with_shadows);
        let mut fc = FnCompiler {
            cx,
            code: Vec::new(),
            constants: Vec::new(),
            string_constants: Vec::new(),
            static_key_plans: Vec::new(),
            bigint_consts: Vec::new(),
            wtf8_consts: Vec::new(),
            scopes: vec![Vec::new()],
            scope_lex_names: vec![HashSet::new()],
            next_reg: 0,
            max_reg: 0,
            bool_regs: 0,
            recv_regs: 0,
            reg_kinds: Vec::new(),
            reg_overflow: false,
            rest_reg: None,
            arguments_reg: None,
            uses_arguments: false,
            super_class: None,
            super_home_obj: false,
            super_static: false,
            derived_class: false,
            in_derived_ctor: false,
            heritage_class: None,
            box_all_locals: false,
            script_eval_lexicals: false,
            eval_sites: Vec::new(),
            in_param_init: false,
            param_names: {
                let mut v: Vec<String> = params.to_vec();
                if let Some(r) = rest {
                    v.push(r.to_string());
                }
                v
            },
            this_override: None,
            pattern_block_local: false,
            in_generator: false,
            in_async: false,
            pending_label: None,
            is_script: false,
            completion_reg: None,
            block_tdz_cells: HashSet::new(),
            entry_tdz_cells: HashSet::new(),
            catch_param_regs: HashSet::new(),
            chain_bails: Vec::new(),
            loop_ctx: Vec::new(),
            handler_depth: 0,
            using_scope_reg: None,
            self_name: None,
            captured,
            cell_regs: HashSet::new(),
            lexical_regs: HashSet::new(),
            const_regs: HashSet::new(),
            param_tdz: HashSet::new(),
            upvalues: Rc::new(RefCell::new(Vec::new())),
            enclosing,
            with_stack: Vec::new(),
            inherited_with_shadows,
            b33_names: std::collections::HashSet::new(),
            template_site_count: 0,
            protect_names: std::collections::HashSet::new(),
            entry_lexicals: std::collections::HashSet::new(),
            typeof_alias: Vec::new(),
            typeof_alias_depth: 0,
            typeof_alias_defs: Vec::new(),
        };
        // Register 0 is reserved for `this` in every function (undefined for
        // plain calls, the receiver for method calls). Parameters follow at
        // registers 1.., in order. This uniform convention lets the call path
        // treat `this` as just another register slot.
        let _this_reg = fc.alloc_reg(); // reg 0 = this
        for p in params {
            let r = fc.alloc_reg();
            fc.scopes[0].push((p.clone(), r));
            // A captured parameter is boxed into a cell immediately so a nested
            // closure shares the live slot (mutations visible both ways).
            if fc.captured.contains(p) {
                fc.emit(Instr::MakeCell { reg: r });
                fc.cell_regs.insert(r);
            }
        }
        // Rest parameter (`...rest`) takes the slot right after the fixed params;
        // the VM fills it with an array of the overflow args at call setup. As
        // with a fixed param, box it if a nested closure captures it.
        if let Some(name) = rest {
            let r = fc.alloc_reg();
            fc.rest_reg = Some(r);
            fc.scopes[0].push((name.to_string(), r));
            if fc.captured.contains(name) {
                fc.emit(Instr::MakeCell { reg: r });
                fc.cell_regs.insert(r);
            }
        }
        fc
    }

    /// Reserve the `arguments` register (right after `this`/params/rest) for a
    /// non-arrow function and bind the name in scope, so a body reference to
    /// `arguments` resolves to it. Arrows/scripts don't call this (they inherit /
    /// have no `arguments`). A formal parameter (or rest) NAMED `arguments`
    /// suppresses the binding entirely (FunctionDeclarationInstantiation:
    /// argumentsObjectNeeded is false when parameterNames contains "arguments"
    /// — sloppy only; strict code rejects the name at parse time).
    pub(crate) fn reserve_arguments(&mut self) {
        if self.scopes[0].iter().any(|(n, _)| n == "arguments") {
            return;
        }
        let r = self.alloc_reg();
        self.scopes[0].push(("arguments".to_string(), r));
        self.arguments_reg = Some(r);
    }

    /// Snapshot this function's environment for a nested function to capture
    /// from: cell-backed locals in scope, plus a SHARED handle to this
    /// function's upvalue list (so a grandchild can both read and transitively
    /// extend it via ParentUpval re-sourcing).
    ///
    /// The list is ordered WEAKEST BINDING FIRST — self-name, then the scope
    /// stack outermost-to-innermost, and within a scope in declaration order.
    /// Consumers (`capture_source`, the direct-eval site map) scan it BACKWARDS,
    /// so that order is exactly `resolve`'s shadowing order: a closure created
    /// inside `{ let a; }` under an outer `let a` must capture the INNER cell.
    pub(crate) fn snapshot(&self) -> EnclosingFn {
        let mut cell_locals = Vec::new();
        // A named-function-expression self-binding that was boxed (captured by a
        // nested closure) is also visible to that closure as an upvalue source.
        // It lives OUTSIDE the param/var scope, so it goes in first: every local
        // shadows it (`var f = function g(){ let g = 1; return () => g; }`).
        if let Some((name, reg)) = &self.self_name {
            if self.cell_regs.contains(reg) {
                cell_locals.push((name.clone(), *reg));
            }
        }
        for scope in &self.scopes {
            for (name, reg) in scope {
                if self.cell_regs.contains(reg) {
                    cell_locals.push((name.clone(), *reg));
                }
            }
        }
        EnclosingFn {
            cell_locals,
            upvalues: self.upvalues.clone(),
        }
    }

    /// Resolve a free variable to an upvalue index in THIS function, creating
    /// the upvalue on first use. `None` if not found in any enclosing function
    /// (then it's a global). Transitive: if the variable lives in an ancestor
    /// beyond the direct parent, every intermediate function captures it too.
    pub(crate) fn resolve_upvalue(&mut self, name: &str) -> Option<u16> {
        if let Some(i) = self.upvalues.borrow().iter().position(|(n, _)| n == name) {
            return Some(i as u16);
        }
        let src = capture_source(&self.enclosing, name)?;
        let mut ups = self.upvalues.borrow_mut();
        let idx = ups.len() as u16;
        ups.push((name.to_string(), src));
        Some(idx)
    }

    // ── register allocation ──
    /// Allocate the next frame register.
    ///
    /// A frame is addressed by `u16` (`Reg`, `FuncProto::reg_count`), so the
    /// space is finite. On exhaustion this SATURATES and records
    /// `reg_overflow`; it must never wrap, because wrapping hands out registers
    /// that are already live and silently corrupts the frame. The flag is
    /// checked wherever a `FuncProto` is finalised, which turns the condition
    /// into a clean compile error instead of a wrong answer at runtime.
    pub(crate) fn alloc_reg(&mut self) -> Reg {
        let r = self.next_reg;
        match self.next_reg.checked_add(1) {
            Some(n) => {
                self.next_reg = n;
                if n > self.max_reg {
                    self.max_reg = n;
                }
            }
            None => {
                self.reg_overflow = true;
                self.max_reg = Reg::MAX;
            }
        }
        r
    }
    /// A scratch register that the caller will stop using immediately; we still
    /// bump the high-water mark but let it be reused by resetting next_reg.
    pub(crate) fn temp(&mut self) -> Reg {
        self.alloc_reg()
    }

    /// Finalise the frame: renumber the class registers (`alloc_bool_reg`,
    /// `alloc_recv_reg`) to `[max_reg, max_reg + bools + receivers)` and
    /// refuse to emit a `FuncProto` whose frame overflowed the register space.
    /// Called at every finalisation site; without the overflow check
    /// `alloc_reg`'s saturation would hand the same register to several live
    /// values.
    pub(crate) fn check_regs(&mut self) -> R<()> {
        let total = self.max_reg as usize + self.bool_regs as usize + self.recv_regs as usize;
        if self.reg_overflow || total > BOOL_BASE as usize {
            return Err(format!(
                "function needs more than {} registers (too many locals, \
                 temporaries or literal elements in one function body)",
                BOOL_BASE
            ));
        }
        if self.bool_regs > 0 || self.recv_regs > 0 {
            let base = self.max_reg;
            let bools = self.bool_regs;
            // `NO_REG` / `BARE_MATH_BY_NAME` ride in `Reg` fields as sentinels
            // (`MathOp`'s bare form also carries a global index in `this_v`,
            // which is below `BOOL_BASE` and so untouched).
            let m = move |r: Reg| -> Reg {
                if r >= crate::bytecode::BARE_MATH_BY_NAME {
                    r
                } else if r >= RECV_BASE {
                    base + bools + (r - RECV_BASE)
                } else if r >= BOOL_BASE {
                    base + (r - BOOL_BASE)
                } else {
                    r
                }
            };
            for i in &mut self.code {
                super::remap::remap_regs(i, &m);
            }
            self.max_reg = total as Reg;
        }
        Ok(())
    }

    pub(crate) fn emit(&mut self, i: Instr) {
        if !self.typeof_alias.is_empty() {
            self.typeof_alias_note_emit(&i);
        }
        self.note_reg_kind(&i);
        self.code.push(i);
    }

    /// Record the kind of value `i` writes (see `reg_kinds`): the comparison
    /// opcodes the region planner types `Bool`, a `Move` of a boolean, and
    /// everything else as a number/other. Class registers are not tracked --
    /// they are never handed out again.
    #[cfg(not(feature = "jit"))]
    fn note_reg_kind(&mut self, _i: &Instr) {
        // The history only serves the register tiers' one-type-per-register
        // model; a build without the JIT has no consumer (and no def table).
    }

    #[cfg(feature = "jit")]
    fn note_reg_kind(&mut self, i: &Instr) {
        let Some(dst) = crate::codegen::writes_reg(i) else {
            return;
        };
        if dst >= BOOL_BASE {
            return;
        }
        let kind = match *i {
            Instr::Lt { .. }
            | Instr::Le { .. }
            | Instr::Gt { .. }
            | Instr::Ge { .. }
            | Instr::Eq { .. }
            | Instr::Ne { .. } => KIND_BOOL,
            Instr::Move { src, .. } => {
                if src >= BOOL_BASE && src < RECV_BASE {
                    KIND_BOOL
                } else {
                    self.reg_kind(src)
                }
            }
            _ => KIND_NUM,
        };
        if kind == 0 {
            return;
        }
        let d = dst as usize;
        if self.reg_kinds.len() <= d {
            self.reg_kinds.resize(d + 1, 0);
        }
        self.reg_kinds[d] |= kind;
    }

    fn reg_kind(&self, r: Reg) -> u8 {
        self.reg_kinds.get(r as usize).copied().unwrap_or(0)
    }

    /// A single scratch register for a non-boolean expression value: the
    /// first register at or above `next_reg` with no boolean history (see
    /// `reg_kinds`). Skipped registers are simply left unused this time.
    pub(crate) fn alloc_num_reg(&mut self) -> Reg {
        if reg_classes_enabled() {
            let mut r = self.next_reg;
            while r < self.max_reg && self.reg_kind(r) & KIND_BOOL != 0 {
                r = r.saturating_add(1);
            }
            self.next_reg = r;
        }
        self.alloc_reg()
    }

    /// Reserve `kinds.len()` contiguous registers for an argument window,
    /// starting at the first base at or above `next_reg` where no slot would
    /// take a boolean argument (`kinds[i]`) over a numeric history or a
    /// non-boolean one over a boolean history. Registers at or above
    /// `max_reg` have no history, so the search always terminates.
    pub(crate) fn alloc_block(&mut self, kinds: &[bool]) -> Reg {
        let n = kinds.len() as Reg;
        let mut base = self.next_reg;
        if reg_classes_enabled() {
            'search: while base < self.max_reg {
                for (i, &is_bool) in kinds.iter().enumerate() {
                    let r = base.saturating_add(i as Reg);
                    let k = self.reg_kind(r);
                    if (is_bool && k & KIND_NUM != 0) || (!is_bool && k & KIND_BOOL != 0) {
                        base = base.saturating_add(1);
                        continue 'search;
                    }
                }
                break;
            }
        }
        self.next_reg = base;
        for _ in 0..n {
            self.alloc_reg();
        }
        base
    }

    /// Every scratch reclaim goes through here. A class register (a
    /// provisional number at or above `BOOL_BASE`) is never a reclaim
    /// boundary: a reset computed from one (`save.max(dst + 1)` with a
    /// boolean `dst`) leaves the ordinary stack where it is.
    pub(crate) fn set_next_reg(&mut self, r: Reg) {
        if r < BOOL_BASE {
            self.next_reg = r;
        }
    }

    /// `next_reg -= k` through the same funnel.
    pub(crate) fn dec_next_reg(&mut self, k: Reg) {
        let r = self.next_reg.saturating_sub(k);
        self.set_next_reg(r);
    }

    /// The destination of a boolean-valued expression (`bool_valued`): a
    /// provisional register in `BOOL_BASE..` that is never reclaimed, so no
    /// later numeric temporary shares it and the region planner never sees a
    /// register defined as both a number and a boolean. Renumbered to the top
    /// of the frame by `check_regs`. Booleans share GPR homes by live range in
    /// the planner, so distinct registers cost frame slots, not homes.
    pub(crate) fn alloc_bool_reg(&mut self) -> Reg {
        if !reg_classes_enabled() {
            return self.temp();
        }
        if self.bool_regs >= RECV_BASE - BOOL_BASE - 1 {
            self.reg_overflow = true;
        }
        let r = BOOL_BASE.saturating_add(self.bool_regs);
        self.bool_regs = self.bool_regs.saturating_add(1);
        r
    }

    /// The register of a receiver read from a global (`src.charCodeAt(i)`):
    /// a provisional register in `RECV_BASE..`, never reclaimed, so it has
    /// exactly one definition and the INT region tier can pin it (the
    /// "cleanly excludable" rule). Renumbered by `check_regs`.
    pub(crate) fn alloc_recv_reg(&mut self) -> Reg {
        if self.recv_regs >= crate::bytecode::BARE_MATH_BY_NAME - RECV_BASE - 1 {
            self.reg_overflow = true;
        }
        let r = RECV_BASE.saturating_add(self.recv_regs);
        self.recv_regs = self.recv_regs.saturating_add(1);
        r
    }

    /// Evaluate the receiver of a member access or method call; a plain
    /// global identifier loads into a receiver class register.
    pub(crate) fn recv_expr(&mut self, e: &Expr) -> R<Reg> {
        if let Expr::Ident(name) = e {
            if reg_classes_enabled() && !self.box_all_locals && !self.cx.dyn_global_zone {
                if let Binding::Global(_) = self.resolve(name) {
                    let r = self.alloc_recv_reg();
                    return self.expr_into(e, r);
                }
            }
        }
        self.expr(e)
    }
    pub(crate) fn here(&self) -> u32 {
        self.code.len() as u32
    }

    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
        self.scope_lex_names.push(HashSet::new());
    }

    /// Record the lexically-declared names of the block scope just opened, for
    /// the position-sensitive Annex B B.3.3 blocker test (`block_fn_conflicts`).
    /// Call right after `push_scope`, before compiling the block's contents.
    pub(crate) fn note_block_lexicals(&mut self, stmts: &[ast::Stmt]) {
        let mut names = HashSet::new();
        for st in stmts {
            super::helpers::add_block_lexicals(st, &mut names);
        }
        if let Some(top) = self.scope_lex_names.last_mut() {
            top.extend(names);
        }
    }

    pub(crate) fn pop_scope(&mut self) {
        self.scope_lex_names.pop();
        let scope = self.scopes.pop().unwrap();
        // Free the registers the scope's locals used (block-local reuse) —
        // and drop their per-register markings, or a later local reallocated
        // onto the same register would inherit const-ness / cell-ness
        // (`{ const a = 1; } { let b = 2; b = 3; }` falsely threw TypeError).
        // Saturating: after a `reg_overflow` the counter no longer tracks the
        // real allocation depth, and a plain `-=` would underflow-panic (the
        // release profile is `panic = "abort"`, so that would be a hard crash).
        self.set_next_reg(self.next_reg.saturating_sub(scope.len() as Reg));
        for (_, r) in &scope {
            self.const_regs.remove(r);
            self.cell_regs.remove(r);
            self.lexical_regs.remove(r);
            self.block_tdz_cells.remove(r);
            self.catch_param_regs.remove(r);
        }
    }

    /// True iff `name` currently resolves to the PRISTINE global builtin of
    /// that name: not shadowed by a param/local/upvalue/class binding, not
    /// declared by the script itself (top-level var/function/let/const/class
    /// create USER globals), and not inside a `with` whose object could shadow
    /// it. The by-name builtin lowerings (`new TypeError(...)`,
    /// `new Promise(...)`, bare `Error(...)`, …) fire only then; otherwise the
    /// generic value path constructs/calls whatever the binding holds.
    pub(crate) fn builtin_unshadowed(&mut self, name: &str) -> bool {
        match self.resolve(name) {
            Binding::Global(idx) => {
                let i = idx as u32;
                !self.cx.hoisted_set.contains(&i)
                    && !self.cx.decl_globals.contains(&i)
                    && !self.cx.lexical_globals.contains(&i)
                    && !self.cx.const_globals.contains(&i)
                    && self.with_objs_for(name).is_empty()
            }
            _ => false,
        }
    }

    pub(crate) fn declare_local(&mut self, name: &str) -> Reg {
        let r = self.alloc_reg();
        self.scopes.last_mut().unwrap().push((name.to_string(), r));
        // Box the local into a cell if a nested function captures it, so the
        // closure and this scope share one mutable slot — or unconditionally in
        // a function whose body may direct-eval (the eval closes over cells).
        if self.box_all_locals
            || self.captured.contains(name)
            || (self.script_eval_lexicals && !name.starts_with('<'))
        {
            self.emit(Instr::MakeCell { reg: r });
            self.cell_regs.insert(r);
        }
        r
    }

    /// Like `declare_local` but never emits `MakeCell`. For bindings whose
    /// value is deposited into the register by the runtime (a `catch` param),
    /// where boxing must happen AFTER the value is present.
    pub(crate) fn declare_local_no_box(&mut self, name: &str) -> Reg {
        let r = self.alloc_reg();
        self.scopes.last_mut().unwrap().push((name.to_string(), r));
        r
    }

    /// Resolve a name to a local register (plain or cell-backed), an upvalue, or
    /// a global slot. Upvalue resolution lazily threads captures up the chain.
    pub(crate) fn resolve(&mut self, name: &str) -> Binding {
        // While compiling a named class's HERITAGE in this very function, the
        // inner class binding shadows even this function's locals/params.
        if let Some((n, cid)) = &self.heritage_class {
            if n == name {
                return Binding::ClassName(*cid);
            }
        }
        if name == "arguments" {
            self.uses_arguments = true; // request the call-time `arguments` array
        }
        for scope in self.scopes.iter().rev() {
            for (n, r) in scope.iter().rev() {
                if n == name {
                    return if self.cell_regs.contains(r) {
                        Binding::LocalCell(*r)
                    } else {
                        Binding::Local(*r)
                    };
                }
            }
        }
        // A named function expression's own name: outside the param/var scope, so
        // checked only after the scope stack (params/locals shadow it).
        if let Some((n, r)) = &self.self_name {
            if n == name {
                return if self.cell_regs.contains(r) {
                    Binding::LocalCell(*r)
                } else {
                    Binding::Local(*r)
                };
            }
        }
        // The inner class-name binding: inside a class element — and arrows within
        // it, which inherit `super_class` — the class's own name resolves to the
        // class value (class_values[class_id]), shadowing any outer binding. This
        // is checked before upvalues/globals so a named class EXPRESSION's name
        // (which has no outer binding) and a same-named outer var both yield the
        // class. Read-only (store_binding throws on assignment).
        if let Some(cid) = self.super_class {
            if self
                .cx
                .class_names
                .iter()
                .any(|(n, id)| *id == cid && n == name)
            {
                return Binding::ClassName(cid);
            }
        }
        // The inner class-name binding is ALSO visible throughout the class's
        // HERITAGE expression (classScope encloses ClassHeritage; the binding
        // is in TDZ until the class value exists, so LoadClassValue throws a
        // ReferenceError for `class x extends x`) — including functions
        // created inside the heritage expression.
        if let Some((_, cid)) = self
            .cx
            .heritage_classes
            .iter()
            .rev()
            .find(|(n, _)| n == name)
        {
            return Binding::ClassName(*cid);
        }
        // A free variable that resolves in an enclosing function is an upvalue.
        if let Some(idx) = self.resolve_upvalue(name) {
            return Binding::Upvalue(idx);
        }
        // An ENCLOSING class's inner binding: a class nested lexically inside
        // another class's elements sees the outer class's name through the class
        // scope chain (`class foo { m() { class bar { n() { foo } } } }`).
        // `class_names` is a push/pop stack of exactly the lexically enclosing
        // classes (innermost last). Nearer scopes — this function's locals, the
        // current class's own name, and captured function locals (upvalues) —
        // all shadow it, so this is checked after them but before globals.
        if let Some((_, cid)) = self.cx.class_names.iter().rev().find(|(n, _)| n == name) {
            return Binding::ClassName(*cid);
        }
        if let Some(i) = self.cx.existing_global_slot(name) {
            return Binding::Global(i as u32);
        }
        // Unknown name → treat as a global (read yields undefined; matches JS
        // for declared-later globals; genuine ReferenceErrors are out of v1
        // scope). Reserve a slot so writes/reads are consistent.
        let slot = self.cx.global_slot(name);
        Binding::Global(slot as u32)
    }

    /// True when a plain reference to the enclosing class's own name would
    /// resolve to its inner CLASS-NAME binding here — i.e. nothing nearer
    /// shadows it. Mirrors the prefix of `resolve` (heritage class, then this
    /// function's scopes, then the function-expression self-name) without its
    /// upvalue-threading / global-minting side effects.
    ///
    /// A direct eval inherits the caller's lexical environment, so this is also
    /// the answer for the eval'd code — but only the caller can compute it: an
    /// INLINE static field initializer compiles in the enclosing function's
    /// scope stack, where the dead `class C` local of that function sits right
    /// next to the class binding and only `heritage_class` separates them.
    pub(crate) fn class_inner_name_visible(&self) -> bool {
        let Some(cid) = self.super_class else {
            return false;
        };
        // The class's own name: normally the class_names stack entry for THIS
        // class — but compile_class pops that entry when it returns, so an
        // INLINE static field initializer (compiled afterwards) finds it in
        // `heritage_class` instead (set around those initializers together
        // with super_class).
        let name = if let Some((n, hcid)) = &self.heritage_class {
            if *hcid != cid {
                return false;
            }
            n.as_str()
        } else if let Some((n, _)) = self.cx.class_names.iter().rev().find(|(_, id)| *id == cid) {
            n.as_str()
        } else if let Some((n, _)) = self
            .cx
            .heritage_classes
            .iter()
            .rev()
            .find(|(_, id)| *id == cid)
        {
            // A function nested inside a static field initializer (the
            // class_names entry is popped; the initializer pushes the name
            // here exactly so nested functions keep the inner binding).
            n.as_str()
        } else {
            return false;
        };
        if self.heritage_class.as_ref().is_some_and(|(n, _)| n == name) {
            return true;
        }
        !self.scopes.iter().any(|s| s.iter().any(|(n, _)| n == name))
            && !self.self_name.as_ref().is_some_and(|(n, _)| n == name)
    }

    /// True when `name` is bound by a function ENCLOSING this one — a boxed
    /// local of an outer frame, or something that frame itself captured. Read
    /// straight off the enclosing snapshots, so unlike `resolve_upvalue` it
    /// threads nothing and changes no bytecode. Used by `delete <identifier>`,
    /// where a resolvable binding must answer `false`.
    pub(crate) fn bound_in_enclosing(&self, name: &str) -> bool {
        self.enclosing.iter().any(|enc| {
            enc.cell_locals.iter().any(|(n, _)| n == name)
                || enc.upvalues.borrow().iter().any(|(n, _)| n == name)
        })
    }

    /// Like `resolve`, but NON-creating: returns `None` for a name that has no
    /// existing binding (rather than minting a fresh global slot). Used by
    /// `delete <identifier>` to tell a resolvable binding (→ `false`) from an
    /// unresolvable name (→ `true`, a no-op) without evaluating or declaring it.
    /// Does not thread upvalues (no side effects); an enclosing-function local is
    /// reported as unresolved, which only affects the rare `delete <outer local>`.
    pub(crate) fn resolve_existing(&self, name: &str) -> Option<Binding> {
        for scope in self.scopes.iter().rev() {
            for (n, r) in scope.iter().rev() {
                if n == name {
                    return Some(if self.cell_regs.contains(r) {
                        Binding::LocalCell(*r)
                    } else {
                        Binding::Local(*r)
                    });
                }
            }
        }
        if let Some((n, r)) = &self.self_name {
            if n == name {
                return Some(if self.cell_regs.contains(r) {
                    Binding::LocalCell(*r)
                } else {
                    Binding::Local(*r)
                });
            }
        }
        self.cx
            .existing_global_slot(name)
            .map(|i| Binding::Global(i as u32))
    }

    /// Annex B (B.3.3.3): a block-level function declaration is normally given an
    /// extra function/global-scoped `var` binding, but that extension is SKIPPED
    /// when replacing it with `var <name>` would be an early error — i.e. when
    /// `name` is lexically declared (`let`/`const`/`class`) in an ENCLOSING block.
    /// Then the function stays purely block-scoped. We approximate the early-error
    /// check by looking for `name` in an enclosing block scope (skipping the base
    /// function/script scope `[0]` — top-level lexicals are globals — and the
    /// current block being populated `[last]`). Only consulted at script level;
    /// inside a function body block functions are always block-local already.
    pub(crate) fn block_fn_conflicts(&self, name: &str) -> bool {
        let n = self.scopes.len();
        if n < 2 {
            return false;
        }
        // In an EVAL program, top-level lexicals are scope-[0] LOCALS (the
        // discarded eval lexEnv), not globals — a same-named one is exactly
        // the early-error case that skips the Annex B extension.
        if self.is_script
            && self.cx.eval_locals
            && self.entry_lexicals.contains(name)
            && self.scopes[0].iter().any(|(nm, _)| nm == name)
        {
            return true;
        }
        self.scopes[1..n - 1]
            .iter()
            .any(|s| s.iter().any(|(nm, _)| nm == name))
            || self.enclosing_block_lexical(name)
    }

    /// `name` is lexically declared by an ENCLOSING block, wherever in that
    /// block it is written. `scopes` only lists bindings already compiled, so a
    /// `let` below a nested block is invisible to the walks above — see
    /// `scope_lex_names`.
    fn enclosing_block_lexical(&self, name: &str) -> bool {
        let n = self.scope_lex_names.len();
        n >= 2
            && self.scope_lex_names[1..n - 1]
                .iter()
                .any(|s| s.contains(name))
    }

    /// Like `block_fn_conflicts` but for the B.3.3 VAR-SYNC applicability
    /// check: a CATCH PARAMETER of the same name is NOT a conflict (B.3.5 —
    /// `var`+catch-param coexist without an early error, so the promotion is
    /// NOT skipped: the "no-skip-try" family), while an enclosing block's
    /// function/lexical binding still is.
    pub(crate) fn block_fn_sync_conflicts(&self, name: &str) -> bool {
        let n = self.scopes.len();
        if n < 2 {
            return false;
        }
        if self.is_script
            && self.cx.eval_locals
            && self.entry_lexicals.contains(name)
            && self.scopes[0].iter().any(|(nm, _)| nm == name)
        {
            return true;
        }
        self.scopes[1..n - 1].iter().any(|s| {
            s.iter()
                .any(|(nm, r)| nm == name && !self.catch_param_regs.contains(r))
        }) || self.enclosing_block_lexical(name)
    }

    pub(crate) fn add_const(&mut self, v: Value) -> u32 {
        let i = self.constants.len() as u32;
        self.constants.push(v);
        i
    }
    /// Intern a string LITERAL and return its CONSTANT-POOL index (for
    /// `LoadConst`). The VM interns the pending-string Value on first load.
    pub(crate) fn add_string_const(&mut self, s: &str) -> u32 {
        let si = self.string_constants.len() as u32;
        self.string_constants.push(s.to_string());
        // Encode as a "pending string" heap Value the VM interns on first load.
        let v = Value::heap(STRING_CONST_BIT | si);
        self.add_const(v)
    }

    /// `add_string_const` for a literal that holds a LONE SURROGATE (its
    /// `StrVal` is the `Utf16` arm): `s` is the lossless MARKER form
    /// (`\u{FFFD}XXXX` per lone surrogate, as produced by
    /// `heap::encode_lone_surrogate_markers`); recording the index makes
    /// `resolve_const` decode it to real WTF-8 lone surrogates at intern time.
    /// The marker form stays the on-disk encoding because `string_constants` is
    /// `Vec<String>`, which cannot itself hold a lone surrogate.
    pub(crate) fn add_string_const_wtf8(&mut self, s: &str) -> u32 {
        let si = self.string_constants.len() as u32;
        self.wtf8_consts.push(si);
        self.string_constants.push(s.to_string());
        let v = Value::heap(STRING_CONST_BIT | si);
        self.add_const(v)
    }

    /// Intern a property/method NAME and return its `string_constants` INDEX —
    /// which is what `GetProp`/`SetProp`/`CallMethod` use to look the name up at
    /// runtime. This must NOT be `add_string_const`'s value: that returns the
    /// constant-POOL index, which diverges from the string_constants index as
    /// soon as any non-string constant is added (e.g. a numeric literal), making
    /// `string_constants[name]` go out of bounds (e.g. `(3.5).toFixed(2)`).
    pub(crate) fn string_name(&mut self, s: &str) -> u32 {
        let si = self.string_constants.len() as u32;
        self.string_constants.push(s.to_string());
        si
    }
}

/// `reg_kinds` bits (see `FnCompiler::note_reg_kind`).
pub(crate) const KIND_BOOL: u8 = 1;
pub(crate) const KIND_NUM: u8 = 2;

/// Default ON; `ZIPP_NO_REG_CLASSES=1` restores the v0.0.5 allocation in which
/// booleans and global receivers share the ordinary scratch stack (and
/// tokenizer loops decline the INT tier). Read once per process.
pub(crate) fn reg_classes_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_REG_CLASSES").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Is `e` boolean-valued by its syntax? Comparisons, `in`/`instanceof`,
/// logical not, boolean literals, and `&&`/`||`/`?:` whose every value arm is.
/// Syntactic only: `a < b` may run a `valueOf`, but the value it leaves in its
/// register is still a boolean, which is all the register class asserts.
pub(crate) fn bool_valued(e: &Expr) -> bool {
    match e {
        Expr::Bool(_) => true,
        Expr::Unary { op: ast::UnaryOp::Not, .. } => true,
        Expr::Binary { op, .. } => matches!(
            op,
            ast::BinaryOp::Eq
                | ast::BinaryOp::NotEq
                | ast::BinaryOp::StrictEq
                | ast::BinaryOp::StrictNotEq
                | ast::BinaryOp::Lt
                | ast::BinaryOp::LtEq
                | ast::BinaryOp::Gt
                | ast::BinaryOp::GtEq
                | ast::BinaryOp::In
                | ast::BinaryOp::Instanceof
        ),
        Expr::Logical { op: ast::LogicalOp::And | ast::LogicalOp::Or, left, right } => {
            bool_valued(left) && bool_valued(right)
        }
        Expr::Cond { cons, alt, .. } => bool_valued(cons) && bool_valued(alt),
        Expr::Seq(items) => items.last().is_some_and(bool_valued),
        _ => false,
    }
}

#[cfg(test)]
mod reg_classes_tests {
    use super::*;

    const TOKENIZE: &str = r#"
        var src = "let a = 1; // c\n/* b */ if (a) { a += 2; }";
        var kinds = [], starts = [];
        function tokenize() {
            var i = 0, n = src.length;
            while (i < n) {
                var c = src.charCodeAt(i);
                if (c === 32 || c === 10) { i++; continue; }
                var st = i;
                if (c === 47) {
                    var cc = src.charCodeAt(i + 1);
                    if (cc === 47) {
                        while (i < n && src.charCodeAt(i) !== 10) i++;
                        kinds.push(5); starts.push(st); continue;
                    }
                    if (cc === 42) {
                        i += 2;
                        while (i < n && !(src.charCodeAt(i) === 42 && src.charCodeAt(i + 1) === 47)) i++;
                        i += 2;
                        kinds.push(5); starts.push(st); continue;
                    }
                }
                kinds.push(c); starts.push(st); i++;
            }
            return kinds.length;
        }
        tokenize();
    "#;

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

    /// The destination of the opcodes this test reasons about; `None` for
    /// everything else (their registers are irrelevant to the two rules).
    fn def_of(i: &Instr) -> Option<(Reg, &'static str)> {
        Some(match *i {
            Instr::LoadGlobal { dst, .. } => (dst, "global"),
            Instr::CallMethod { dst, .. } => (dst, "call"),
            Instr::LoadInt { dst, .. }
            | Instr::Add { dst, .. }
            | Instr::AddInt { dst, .. }
            | Instr::Sub { dst, .. }
            | Instr::Mul { dst, .. }
            | Instr::Bitwise { dst, .. } => (dst, "num"),
            Instr::Eq { dst, .. }
            | Instr::Ne { dst, .. }
            | Instr::Lt { dst, .. }
            | Instr::Le { dst, .. }
            | Instr::Gt { dst, .. }
            | Instr::Ge { dst, .. }
            | Instr::Not { dst, .. } => (dst, "bool"),
            Instr::Move { dst, .. } => (dst, "move"),
            _ => return None,
        })
    }

    #[test]
    fn global_receivers_have_exactly_one_definition() {
        let program = compile(TOKENIZE);
        let f = named(&program, "tokenize");
        let receivers: Vec<Reg> = f
            .code
            .iter()
            .filter_map(|i| match *i {
                Instr::CallMethod { obj, .. } => Some(obj),
                _ => None,
            })
            .collect();
        assert!(receivers.len() >= 10, "tokenize lost its method calls:\n{f:#?}");
        for r in receivers {
            let defs: Vec<&Instr> = f
                .code
                .iter()
                .filter(|i| def_of(i).is_some_and(|(d, _)| d == r))
                .collect();
            assert_eq!(defs.len(), 1, "receiver r{r} has {} definitions: {defs:?}", defs.len());
            assert!(matches!(defs[0], Instr::LoadGlobal { .. }), "receiver r{r}: {:?}", defs[0]);
        }
    }

    #[test]
    fn no_register_in_the_loop_is_both_a_number_and_a_boolean() {
        let program = compile(TOKENIZE);
        let f = named(&program, "tokenize");
        let mut kinds: std::collections::BTreeMap<Reg, std::collections::BTreeSet<&str>> =
            Default::default();
        for i in &f.code {
            if let Some((d, k)) = def_of(i) {
                if k == "num" || k == "bool" {
                    kinds.entry(d).or_default().insert(k);
                }
            }
        }
        let mixed: Vec<Reg> = kinds
            .iter()
            .filter(|(_, ks)| ks.len() > 1)
            .map(|(r, _)| *r)
            .collect();
        assert!(mixed.is_empty(), "registers defined as both number and boolean: {mixed:?}\n{f:#?}");
    }

    #[test]
    fn loop_free_functions_still_reclaim_scratch() {
        // The v0.0.5 fib frame: nine registers, two recursive calls whose
        // argument scratch is reused (see `binary_lhs_reclaim_tests`).
        let program = compile("function fib(n){ return n < 2 ? n : fib(n-1) + fib(n-2); } fib(8);");
        assert_eq!(named(&program, "fib").reg_count, 9);
    }

    #[test]
    fn class_registers_are_renumbered_into_the_frame() {
        let program = compile(TOKENIZE);
        let f = named(&program, "tokenize");
        for i in &f.code {
            if let Some((d, _)) = def_of(i) {
                assert!(d < f.reg_count, "r{d} outside the {}-register frame: {i:?}", f.reg_count);
            }
        }
        // Booleans and receivers sit above every ordinary register.
        let recv: Vec<Reg> = f
            .code
            .iter()
            .filter_map(|i| match *i {
                Instr::CallMethod { obj, .. } => Some(obj),
                _ => None,
            })
            .collect();
        let ordinary_top = f
            .code
            .iter()
            .filter_map(|i| match def_of(i) {
                Some((d, "num")) => Some(d),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        assert!(recv.iter().all(|&r| r > ordinary_top), "receivers {recv:?} below ordinary top {ordinary_top}");
    }
}
