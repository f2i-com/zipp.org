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
            bigint_consts: Vec::new(),
            wtf8_consts: Vec::new(),
            scopes: vec![Vec::new()],
            scope_lex_names: vec![HashSet::new()],
            next_reg: 0,
            max_reg: 0,
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
        EnclosingFn { cell_locals, upvalues: self.upvalues.clone() }
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

    /// Refuse to emit a `FuncProto` whose frame overflowed the `u16` register
    /// space. Called at every finalisation site; without it `alloc_reg`'s
    /// saturation would hand the same register to several live values.
    pub(crate) fn check_regs(&self) -> R<()> {
        if self.reg_overflow {
            return Err(format!(
                "function needs more than {} registers (too many locals, \
                 temporaries or literal elements in one function body)",
                Reg::MAX
            ));
        }
        Ok(())
    }

    pub(crate) fn emit(&mut self, i: Instr) {
        self.code.push(i);
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
        self.next_reg = self.next_reg.saturating_sub(scope.len() as Reg);
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
                !self.cx.hoisted_globals.contains(&i)
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
            if self.cx.class_names.iter().any(|(n, id)| *id == cid && n == name) {
                return Binding::ClassName(cid);
            }
        }
        // The inner class-name binding is ALSO visible throughout the class's
        // HERITAGE expression (classScope encloses ClassHeritage; the binding
        // is in TDZ until the class value exists, so LoadClassValue throws a
        // ReferenceError for `class x extends x`) — including functions
        // created inside the heritage expression.
        if let Some((_, cid)) = self.cx.heritage_classes.iter().rev().find(|(n, _)| n == name) {
            return Binding::ClassName(*cid);
        }
        // A free variable that resolves in an enclosing function is an upvalue.
        if let Some(idx) = self.resolve_upvalue(name) {
            return Binding::Upvalue(idx);
        }
        if let Some(i) = self.cx.globals.iter().position(|g| g == name) {
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
        let Some(cid) = self.super_class else { return false };
        let Some(name) = self
            .cx
            .class_names
            .iter()
            .rev()
            .find(|(_, id)| *id == cid)
            .map(|(n, _)| n.as_str())
        else {
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
            .globals
            .iter()
            .position(|g| g == name)
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
        n >= 2 && self.scope_lex_names[1..n - 1].iter().any(|s| s.contains(name))
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
            s.iter().any(|(nm, r)| nm == name && !self.catch_param_regs.contains(r))
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
