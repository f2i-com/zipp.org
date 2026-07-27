// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// The key EXPRESSION of a computed class element (`[expr]() {}`, `[expr] = v`),
/// or `None` for a statically-named one.
///
/// NOTE: this replaces oxc's `computed: bool` + `PropertyKey::as_expression()`
/// pair. Computed-ness is a VARIANT of `PropKey` here, not a sibling flag, so
/// one accessor answers both "is it computed?" and "what is the key
/// expression?" — and the two can no longer disagree. That is why the old
/// `Err(e) if m.computed => { … as_expression().ok_or(e)? }` / `Err(e) => Err(e)`
/// pair collapses into a single `Err` arm below: same behaviour, one check.
fn computed_key(k: &ast::PropKey) -> Option<&ast::Expr> {
    match k {
        ast::PropKey::Computed(e) => Some(e),
        _ => None,
    }
}

/// The statement list of an arrow body, for the `&[Stmt]`-shaped analyses
/// (`hoisted_var_names`, `capture::captured_locals`, `stash_child_with_shadows`).
///
/// NOTE: an expression-bodied arrow no longer HAS a statement list — `x => e` is
/// `ArrowBody::Expr`, where oxc synthesised a body of exactly one
/// `ExpressionStatement` (and no directives, so `x => "use strict"` was never a
/// prologue). Those analyses are written against statements, so the same
/// one-element list is rebuilt here: identical input, identical bytecode. It
/// costs one clone of the body expression per expression-bodied arrow; an
/// expression-level entry point in `capture` would remove the clone, but adding
/// one is a change to a module this port does not own.
fn arrow_body_stmts(a: &ast::Arrow) -> std::borrow::Cow<'_, [ast::Stmt]> {
    match &a.body {
        ast::ArrowBody::Block(b) => std::borrow::Cow::Borrowed(&b.stmts),
        ast::ArrowBody::Expr(e) => std::borrow::Cow::Owned(vec![ast::Stmt::Expr((**e).clone())]),
    }
}

impl<'a> FnCompiler<'a> {
    pub(crate) fn func_decl(&mut self, f: &ast::Function) -> R<()> {
        self.func_decl_inner(f, true)
    }

    /// Annex B B.3.3.3 var-binding sync for a block function named `name`,
    /// reading the value from the BLOCK binding (`src`): emitted at the
    /// declaration's TEXTUAL position (SetMutableBinding happens when the
    /// declaration is evaluated, not at block entry).
    pub(crate) fn emit_b33_sync(&mut self, name: &str) {
        if !self.b33_names.contains(name) || self.block_fn_sync_conflicts(name) {
            return;
        }
        let Some(b) = self.resolve_existing(name) else { return };
        let save = self.next_reg;
        let src = match b {
            Binding::Local(r) => r,
            Binding::LocalCell(c) => {
                let t = self.temp();
                self.emit(Instr::CellGet { dst: t, cell: c });
                t
            }
            _ => return,
        };
        let s0reg = self
            .scopes
            .first()
            .and_then(|s| s.iter().find(|(nm, _)| nm == name).map(|(_, r)| *r));
        if let Some(s0) = s0reg {
            if s0 != src {
                if self.cell_regs.contains(&s0) {
                    self.emit(Instr::CellSet { cell: s0, src });
                } else {
                    self.emit(Instr::Move { dst: s0, src });
                }
            }
        }
        if self.is_script {
            let caller_b33_upval = if self.cx.eval_fn_context && self.cx.eval_caller_var(name) {
                self.resolve_upvalue(name)
            } else {
                None
            };
            if let Some(ui) = caller_b33_upval {
                self.emit(Instr::UpvalSet { idx: ui, src });
            } else if s0reg.is_none() {
                let slot = self.cx.global_slot(name) as u32;
                if self.box_all_locals || self.cx.dyn_global_zone {
                    self.emit(Instr::StoreGlobalDyn { idx: slot, src });
                } else {
                    self.emit(Instr::StoreGlobal { idx: slot, src });
                }
            }
        }
        self.next_reg = save;
    }

    pub(crate) fn func_decl_inner(&mut self, f: &ast::Function, do_sync: bool) -> R<()> {
        let name = f.name.as_ref().map(|n| n.to_string());
        // Strict mode: a function may not be named `eval`/`arguments` (the binding
        // is strict if the enclosing scope is strict OR the body opens `"use strict"`).
        if let Some(n) = &name {
            strict_name_err(self.cx.in_strict || has_use_strict(fn_directives(f)), n)?;
        }
        let (params, rest, body) = function_parts(f)?;
        let mut names = with_rest(&params, &rest);
        names.extend(param_pattern_leaves(&f.params));
        names.extend(hoisted_var_names(body)); // function-scoped `var`s (capture)
        let captured = capture::captured_locals(&names, body);
        self.stash_child_with_shadows(&names, body);
        let enclosing = self.child_enclosing();
        let mut proto = self.cx.compile_function_body(
            name.as_deref(),
            None, // a declaration's name lives in the enclosing scope, not self-bound
            &params,
            rest.as_deref(),
            Some(&f.params),
            body,
            fn_directives(f), // body prologue: drives `"use strict"` strictness
            false,
            f.is_generator,
            f.is_async,
            captured,
            enclosing,
        )?;
        proto.source = self.cx.src_slice(f.span.start, f.span.end);
        let id = self.cx.functions.len() as u32;
        let has_upvalues = !proto.upvalues.is_empty();
        // Resolve the name once. A script-level function hoists to a GLOBAL var
        // binding (Annex B), UNLESS its name was pre-declared as a block-local
        // because it conflicts with an enclosing-block lexical binding
        // (conflict-skip) — then it binds locally like a nested-function block fn.
        let binding = name.as_deref().map(|n| self.resolve(n));
        let is_block_local =
            matches!(binding, Some(Binding::Local(_)) | Some(Binding::LocalCell(_)));
        // EvalDeclarationInstantiation step 15.d (sloppy fn-context direct eval):
        // a top-level function declaration whose name the CALLER's var scope
        // already binds performs SetMutableBinding on the caller's EXISTING
        // binding (the upvalue cell the eval root seeded) — never a realm global
        // and never a fresh EvalScope binding. This runs at EVAL ENTRY
        // (functionsToInitialize precede the body statements — see `entry_fns`),
        // so `eval('initial = f; function f() {…}')` reads the new function.
        if self.is_script
            && self.cx.script_binds_globals
            && self.cx.eval_fn_context
            && !is_block_local
        {
            if let Some(ui) = name
                .as_deref()
                .filter(|n| self.cx.eval_caller_var(n))
                .and_then(|n| self.resolve_upvalue(n))
            {
                self.cx.functions.push(proto);
                let tmp = self.temp();
                self.emit_make_callable(tmp, id, has_upvalues);
                self.emit(Instr::UpvalSet { idx: ui, src: tmp });
                self.next_reg -= 1;
                return Ok(());
            }
        }
        if self.is_script && self.cx.script_binds_globals && !is_block_local && !has_upvalues {
            // Top-level (or no-conflict block function) with no captures: bind the
            // name to a global; the VM materialises the function object at startup.
            if let Some(n) = &name {
                let slot = self.cx.global_slot(n);
                proto.name_global = Some(slot);
            }
            self.cx.functions.push(proto);
        } else if self.is_script && self.cx.script_binds_globals && !is_block_local {
            // A script-level BLOCK function that captures enclosing block-locals
            // can't be a startup-materialised global Func — its captured cells
            // don't exist at startup, and its UpvalGet ops would run with no
            // closure. Build the CLOSURE at the declaration point (capturing the
            // live cells) and store it into the function's global var slot.
            self.cx.functions.push(proto);
            if let Some(n) = &name {
                let slot = self.cx.global_slot(n) as u32;
                let tmp = self.temp();
                self.emit(Instr::MakeClosure { dst: tmp, func_id: id });
                self.emit(Instr::StoreGlobal { idx: slot, src: tmp });
                self.next_reg -= 1;
            }
        } else {
            // Nested function, or a script-level conflict-skip block function:
            // create the function object now into the local the hoisting pre-pass
            // reserved for this name. If it captures, build a closure; otherwise a
            // plain function object. The name's binding may be a plain register or
            // a cell (when a sibling/inner function captures this function name).
            self.cx.functions.push(proto);
            // Annex B B.3.3: a block function with a function-scoped `var` binding
            // (in `b33_names`) assigns the var the function value when this
            // declaration executes. `s0reg` is that binding's register in the
            // function's base scope; the block-local shadows it inside the block.
            // A declaration whose `var`-replacement would conflict with a
            // SAME-NAMED binding in an enclosing block (e.g. a nested block's
            // `function f(){}` under an outer block that also declares `f`) is
            // NOT B.3.3-applicable — it stays purely block-local (B.3.3.1's
            // would-not-produce-Early-Errors condition, checked per declaration
            // POSITION, not just per name).
            let b33_applicable = match name.as_deref() {
                Some(n) => {
                    do_sync && self.b33_names.contains(n) && !self.block_fn_sync_conflicts(n)
                }
                None => false,
            };
            let s0reg = match name.as_deref() {
                Some(n) if b33_applicable => self
                    .scopes
                    .first()
                    .and_then(|s| s.iter().find(|(nm, _)| nm == n).map(|(_, r)| *r)),
                _ => None,
            };
            // SCRIPT-level B.3.3: the block function ALSO updates its
            // function-scoped binding when the declaration evaluates
            // (SetMutableBinding): the global var slot normally, the CALLER
            // BINDING (an upvalue the eval root seeded) when this is a sloppy
            // fn-context eval whose caller declares the same name.
            let caller_b33_upval = match name.as_deref() {
                Some(n)
                    if self.is_script
                        && b33_applicable
                        && self.cx.eval_fn_context
                        && self.cx.eval_caller_var(n) =>
                {
                    self.resolve_upvalue(n)
                }
                _ => None,
            };
            let script_b33_slot = match name.as_deref() {
                Some(n)
                    if self.is_script && b33_applicable && caller_b33_upval.is_none() =>
                {
                    Some(self.cx.global_slot(n) as u32)
                }
                _ => None,
            };
            match binding {
                Some(Binding::Local(reg)) => {
                    self.emit_make_callable(reg, id, has_upvalues);
                    if let Some(s0) = s0reg {
                        if s0 != reg {
                            if self.cell_regs.contains(&s0) {
                                self.emit(Instr::CellSet { cell: s0, src: reg });
                            } else {
                                self.emit(Instr::Move { dst: s0, src: reg });
                            }
                        }
                    }
                    if let Some(slot) = script_b33_slot {
                        if self.box_all_locals || self.cx.dyn_global_zone {
                            self.emit(Instr::StoreGlobalDyn { idx: slot, src: reg });
                        } else {
                            self.emit(Instr::StoreGlobal { idx: slot, src: reg });
                        }
                    }
                    if let Some(ui) = caller_b33_upval {
                        self.emit(Instr::UpvalSet { idx: ui, src: reg });
                    }
                }
                Some(Binding::LocalCell(cell)) => {
                    let tmp = self.temp();
                    self.emit_make_callable(tmp, id, has_upvalues);
                    self.emit(Instr::CellSet { cell, src: tmp });
                    if let Some(s0) = s0reg {
                        if self.cell_regs.contains(&s0) {
                            self.emit(Instr::CellSet { cell: s0, src: tmp });
                        } else {
                            self.emit(Instr::Move { dst: s0, src: tmp });
                        }
                    }
                    if let Some(slot) = script_b33_slot {
                        if self.box_all_locals || self.cx.dyn_global_zone {
                            self.emit(Instr::StoreGlobalDyn { idx: slot, src: tmp });
                        } else {
                            self.emit(Instr::StoreGlobal { idx: slot, src: tmp });
                        }
                    }
                    if let Some(ui) = caller_b33_upval {
                        self.emit(Instr::UpvalSet { idx: ui, src: tmp });
                    }
                    self.next_reg -= 1;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Compile a `class C { … }` declaration: build the method + constructor
    /// protos, register a ClassDef, and bind `C` to the materialized class value.
    pub(crate) fn class_decl(&mut self, class: &ast::Class) -> R<()> {
        let name = class.name.as_ref().map(|n| n.to_string());
        let Some(n) = name else {
            // Anonymous class declaration (e.g. `export default class {}`):
            // compile its body for completeness; it binds no name.
            self.compile_class(class, None)?;
            return Ok(());
        };

        // Resolve where the class value will live. A plain register local builds
        // in place; a cell/global builds in a temp then stores through.
        enum Dest {
            Reg(Reg),
            Cell(Reg, Reg),   // (cell, temp)
            Global(u32, Reg), // (slot, temp)
        }
        // A class declaration pre-declared block-local (e.g. inside a switch/block,
        // where it is lexically scoped) binds to that existing innermost local
        // rather than leaking to a global. `resolve_existing` scans innermost-first,
        // so it returns the current block's binding (shadowing any outer/global one).
        let dest = match self.resolve_existing(&n) {
            Some(Binding::LocalCell(r)) => Dest::Cell(r, self.temp()),
            Some(Binding::Local(r)) => Dest::Reg(r),
            _ if self.is_script && !self.cx.eval_locals => {
                let slot = self.cx.global_slot(&n) as u32;
                Dest::Global(slot, self.temp())
            }
            _ => {
                let reg = self.declare_local(&n);
                if self.cell_regs.contains(&reg) {
                    Dest::Cell(reg, self.temp())
                } else {
                    Dest::Reg(reg)
                }
            }
        };
        let cls = match &dest {
            Dest::Reg(r) => *r,
            Dest::Cell(_, t) | Dest::Global(_, t) => *t,
        };
        self.build_class_into(class, cls, None)?;
        match &dest {
            Dest::Reg(_) => {}
            Dest::Cell(cell, t) => {
                self.emit(Instr::CellSet { cell: *cell, src: *t });
                self.next_reg -= 1; // reclaim the temp
            }
            Dest::Global(slot, t) => {
                self.emit(Instr::StoreGlobal { idx: *slot, src: *t });
                self.next_reg -= 1;
            }
        }
        Ok(())
    }

    /// Compile a class body and emit its runtime materialization into register
    /// `cls`: evaluate `extends`, `MakeClass`, then install static fields and
    /// computed-key members. Shared by class declarations and class expressions.
    pub(crate) fn build_class_into(&mut self, class: &ast::Class, cls: Reg, name: Option<&str>) -> R<()> {
        let (class_id, static_fields, computed, computed_fields, static_block_fns, static_order) =
            self.compile_class(class, name)?;
        // Evaluate the superclass value (`extends P`) into a temp the VM links in.
        let parent_reg = if let Some(sc) = &class.superclass {
            let t = self.temp();
            // ClassHeritage evaluates in STRICT mode (the whole ClassTail is
            // strict code), regardless of the enclosing scope.
            let prev_strict = self.cx.in_strict;
            self.cx.in_strict = true;
            // A NAMED class's own binding is in scope for the heritage.
            let heritage_named = class.name.as_ref().map(|n| n.to_string());
            let saved_hc = self.heritage_class.take();
            if let Some(n) = &heritage_named {
                self.cx.heritage_classes.push((n.clone(), class_id));
                self.heritage_class = Some((n.clone(), class_id));
            }
            // ClassHeritage is evaluated OUTSIDE this class's PrivateEnvironment
            // (§15.7.14 creates it only after the heritage), so `class C extends
            // (this.#x) {}` is a SyntaxError even when `#x` is declared right
            // below. `compile_class` above has already pushed this class's names,
            // so lift them for the duration — a nested class inside the heritage
            // still pushes and sees its OWN names, which is what keeps
            // `class C extends (class { #y; m(){ return this.#y } }) {}` legal.
            let own_privates = self.cx.private_names_stack.pop();
            let r = self.expr_into(sc, t);
            if let Some(p) = own_privates {
                self.cx.private_names_stack.push(p);
            }
            if heritage_named.is_some() {
                self.cx.heritage_classes.pop();
            }
            self.heritage_class = saved_hc;
            self.cx.in_strict = prev_strict;
            let v = r?;
            if v != t {
                self.emit(Instr::Move { dst: t, src: v });
            }
            Some(t)
        } else {
            None
        };
        self.emit(Instr::MakeClass { dst: cls, class_id, parent: parent_reg });
        if parent_reg.is_some() {
            self.next_reg -= 1; // reclaim the parent temp
        }
        // PHASE 1 — evaluate every ClassElementName in source position: computed
        // member keys install the members; computed FIELD keys evaluate ONCE
        // (an instance key parks on the class for the ctor's FieldInit; a
        // static key parks in a register that phase 2 consumes).
        for (key, func, kind) in &computed {
            let save = self.next_reg;
            let kr = self.expr(key)?;
            self.emit(Instr::ClassAddMember { class: cls, key: kr, func: *func, kind: *kind });
            self.next_reg = save;
        }
        let mut parked: Vec<Option<Reg>> = Vec::with_capacity(computed_fields.len());
        for (key, _init, is_static) in &computed_fields {
            if *is_static {
                // Survives until phase 2 (not reclaimed).
                let kr = self.temp();
                let v = self.expr_into(key, kr)?;
                if v != kr {
                    self.emit(Instr::Move { dst: kr, src: v });
                }
                parked.push(Some(kr));
            } else {
                let save = self.next_reg;
                let kr = self.expr(key)?;
                self.emit(Instr::PushFieldKey { class: cls, key: kr });
                self.next_reg = save;
                parked.push(None);
            }
        }
        // PHASE 2 — run the STATIC field initializers and `static {}` blocks in
        // SOURCE order (spec ClassDefinitionEvaluation: one interleaved list; an
        // abrupt completion aborts the remaining elements). Initializers run
        // with `this` = the class, in strict mode, with the class's static
        // super base. The classScope's own-name binding (already initialized —
        // MakeClass ran) shadows the still-TDZ outer declaration binding while
        // these INLINE initializers compile, so `class C { static x = C }`
        // inside a function resolves to the class value, not the dead local.
        let static_self_name = class.name.as_ref().map(|n| n.to_string());
        for &(elem_kind, idx) in &static_order {
            match elem_kind {
                0 => {
                    let (fname, finit) = &static_fields[idx];
                    let save = self.next_reg;
                    self.this_override = Some(cls);
                    let prev_strict = self.cx.in_strict;
                    self.cx.in_strict = true;
                    let (prev_sc, prev_ss) = (self.super_class, self.super_static);
                    self.super_class = Some(class_id);
                    self.super_static = true;
                    let saved_hc = self.heritage_class.take();
                    if let Some(n) = &static_self_name {
                        self.heritage_class = Some((n.clone(), class_id));
                    }
                    let v = match finit {
                        Some(e) => self.expr(e)?,
                        None => {
                            let t = self.temp();
                            self.emit(Instr::LoadUndefined { dst: t });
                            t
                        }
                    };
                    self.heritage_class = saved_hc;
                    self.super_class = prev_sc;
                    self.super_static = prev_ss;
                    self.cx.in_strict = prev_strict;
                    self.this_override = None;
                    // NamedEvaluation: an anonymous fn/arrow/class initializer takes
                    // the field name (incl. the literal "#field" for privates).
                    if matches!(finit, Some(e) if is_anonymous_fn_def(e)) {
                        let kr = self.temp();
                        let cidx = self.add_string_const(fname);
                        self.emit(Instr::LoadConst { dst: kr, idx: cidx });
                        self.emit(Instr::SetFnNameFromKey { func: v, key: kr, prefix: 0 });
                    }
                    let name_idx = self.string_name(fname);
                    self.emit(Instr::SetProp { obj: cls, name: name_idx, val: v });
                    self.next_reg = save;
                }
                1 => {
                    let Some(kr) = parked[idx] else { continue };
                    let (_key, init, _is_static) = &computed_fields[idx];
                    let save = self.next_reg;
                    self.this_override = Some(cls);
                    let prev_strict = self.cx.in_strict;
                    self.cx.in_strict = true;
                    let (prev_sc, prev_ss) = (self.super_class, self.super_static);
                    self.super_class = Some(class_id);
                    self.super_static = true;
                    let saved_hc = self.heritage_class.take();
                    if let Some(n) = &static_self_name {
                        self.heritage_class = Some((n.clone(), class_id));
                    }
                    let vr = match init {
                        Some(e) => self.expr(e)?,
                        None => {
                            let t = self.temp();
                            self.emit(Instr::LoadUndefined { dst: t });
                            t
                        }
                    };
                    self.heritage_class = saved_hc;
                    self.super_class = prev_sc;
                    self.super_static = prev_ss;
                    self.cx.in_strict = prev_strict;
                    self.this_override = None;
                    // A static field may not be named `prototype` (TypeError); the
                    // op ToPropertyKeys the (already-evaluated) key and checks it.
                    self.emit(Instr::ClassStaticField { class: cls, key: kr, val: vr });
                    self.next_reg = save;
                }
                _ => {
                    let fid = static_block_fns[idx];
                    let save = self.next_reg;
                    let f = self.temp();
                    self.emit(Instr::MakeFunc { dst: f, func_id: fid });
                    let argb = self.temp();
                    self.emit(Instr::Move { dst: argb, src: cls });
                    let trash = self.temp();
                    let call_idx = self.string_name("call");
                    self.emit(Instr::CallMethod {
                        dst: trash,
                        obj: f,
                        name: call_idx,
                        arg_base: argb,
                        argc: 1,
                    });
                    self.next_reg = save;
                }
            }
        }
        self.cx.private_names_stack.pop();
        Ok(())
    }

    /// A class expression (`let C = class { … }`, `x = class extends B {}`):
    /// materialize the class value into `dst` and return it.
    pub(crate) fn class_expr(&mut self, class: &ast::Class, dst: Reg, name: Option<&str>) -> R<Reg> {
        self.build_class_into(class, dst, name)?;
        Ok(dst)
    }

    /// Compile a class body into protos (methods get `this` at reg 0; the
    /// constructor proto runs instance-field initializers then the user ctor
    /// body) and register a ClassDef. Returns its class_id. Methods are compiled
    /// as non-capturing functions (free vars resolve to globals), so a class at
    /// module scope works fully; `extends`/`super`, static members, and
    /// get/set accessors are out of this subset.
    #[allow(clippy::type_complexity)]
    pub(crate) fn compile_class<'b>(
        &mut self,
        class: &'b ast::Class,
        name: Option<&str>,
    ) -> R<(
        u32,
        Vec<(String, Option<&'b ast::Expr>)>,
        Vec<(&'b ast::Expr, u32, u8)>,
        Vec<(&'b ast::Expr, Option<&'b ast::Expr>, bool)>,
        Vec<u32>,
        Vec<(u8, usize)>,
    )> {
        // A named class expression keeps its own name; an anonymous one inherits
        // the binding it's assigned to (NamedEvaluation), else the "<class>" stub.
        let cname = class
            .name
            .as_ref()
            .map(|n| n.to_string())
            .or_else(|| name.map(|s| s.to_string()))
            .unwrap_or_else(|| "<class>".into());
        // Reserve this class's id and register its name BEFORE compiling members,
        // so a method body containing a subclass / `new ThisClass` resolves, and
        // nested classes compiled within get distinct ids.
        let class_id = self.cx.classes.len() as u32;
        self.cx.classes.push(ClassDef {
            name: cname.clone(),
            ctor: None,
            has_explicit_ctor: false,
            field_thunk: None,
            methods: Vec::new(),
            getters: Vec::new(),
            setters: Vec::new(),
            statics: Vec::new(),
            static_getters: Vec::new(),
            static_setters: Vec::new(),
            source: String::new(), // filled in below once the body is compiled
            instance_field_names: Vec::new(),
            static_field_names: Vec::new(),
        });
        self.cx.class_names.push((cname.clone(), class_id));
        // The methods/ctor/field-inits of this class close over the function that
        // contains it. Stash that enclosing chain for `compile_class_fn` to read,
        // saving the outer class's (for a class nested in a method) and restoring
        // it after the body is compiled.
        let saved_enclosing = std::mem::take(&mut self.cx.class_enclosing);
        let chain = self.child_enclosing();
        self.cx.class_enclosing = chain;
        // `super.x` resolves its target at RUNTIME via this class's prototype's
        // [[Prototype]] (the parent's prototype for a derived class, %Object.prototype%
        // for a base class). EVERY class method carries THIS class's id as its super
        // (home-class) context — so `super.x`/`super.m()` work in base classes too;
        // `super(...)` is separately gated on `class_derived` below. Carrying the
        // class id (not the parent) also lets `extends <any expression>` (mixins,
        // conditionals, built-ins, class expressions) work.
        let super_class_id = Some(class_id);
        // Gate `super(...)`: only a derived class's constructor may call it.
        let saved_derived = self.cx.class_derived;
        self.cx.class_derived = class.superclass.is_some();
        let mut ctor_fn: Option<&ast::Function> = None;
        // NOTE: the `method_spans` side table is gone. It existed to recover each
        // method's MethodDefinition span (`m.span`) from its value-Function span,
        // because oxc's value-`Function` span starts at the `(` and so cannot
        // reproduce `Function.prototype.toString`'s [[SourceText]]. Here a class
        // member's `Function.span` ALREADY IS the MethodDefinition span (the
        // parser records the [[SourceText]] range on the function itself), so
        // `func.span` is read directly below. The lookup it replaces could never
        // miss — every MethodDefinition inserted its own entry — so dropping the
        // `if let Some(..)` guard emits the same `proto.source` as before. The
        // `static` keyword is still part of that range and is still stripped by
        // `method_source` at the static call sites.
        let mut methods: Vec<(String, &ast::Function)> = Vec::new();
        let mut getters: Vec<(String, &ast::Function)> = Vec::new();
        let mut setters: Vec<(String, &ast::Function)> = Vec::new();
        let mut statics: Vec<(String, &ast::Function)> = Vec::new();
        let mut static_getters: Vec<(String, &ast::Function)> = Vec::new();
        let mut static_setters: Vec<(String, &ast::Function)> = Vec::new();
        let mut fields: Vec<(String, Option<&ast::Expr>)> = Vec::new();
        let mut static_fields: Vec<(String, Option<&'b ast::Expr>)> = Vec::new();
        // `static { … }` initializer blocks, in source order. Each is compiled to a
        // thunk and run once at class definition time with `this` = the class.
        let mut static_blocks: Vec<&'b [ast::Stmt]> = Vec::new();
        // Computed-key fields (`[expr] = v` / `static [expr] = v`). Their KEYS are
        // evaluated once at class definition (in source order, see class_decl);
        // `computed_fields_ordered` drives that. Instance ones also need their init
        // run per-instance in the ctor — `instance_computed_inits` (index i ↔ the
        // i-th instance computed key) feeds the ctor's `FieldInit` ops.
        #[allow(clippy::type_complexity)]
        let mut computed_fields_ordered: Vec<(
            &'b ast::Expr,
            Option<&'b ast::Expr>,
            bool,
        )> = Vec::new();
        // Source order of STATIC elements: (0=named field, 1=computed field,
        // 2=static block) -> index into its vec; drives phase-2 evaluation.
        let mut static_order: Vec<(u8, usize)> = Vec::new();
        let mut instance_computed_inits: Vec<Option<&'b ast::Expr>> = Vec::new();
        // Members with a runtime-computed key (`[expr]() {}`) — the key is
        // evaluated and the member installed at class-creation time (see
        // class_decl). kind: 0=method 1=getter 2=setter 3=static method.
        let mut computed: Vec<(&'b ast::Expr, &'b ast::Function, u8)> = Vec::new();
        // NOTE: `ClassMember` has exactly three variants (Method / Field /
        // StaticBlock), so the old trailing `_ => Err("unsupported class member
        // in the zipp-vm subset")` arm is gone — it would now be an unreachable
        // pattern. Two constructs that used to reach the compiler no longer can:
        // `accessor x = init` (which this file lowered to an ordinary field with
        // the same key/initializer timing) and decorated members. Neither has a
        // `ClassMember` variant, so they are rejected before compilation instead
        // of here. If the hand-written parser adds an accessor variant, the
        // SUBSET lowering it needs is the one that used to live in this match:
        // an ordinary field, with the synthesized private backing slot and the
        // prototype get/set pair NOT modeled.
        for el in &class.body {
            match el {
                ast::ClassMember::Method(m) => {
                    // A constructor is never computed; otherwise a key that
                    // class_key_name can't name statically (and is `computed`) is a
                    // runtime-keyed member.
                    // kind: 0=method 1=getter 2=setter 3=static method
                    //       4=static getter 5=static setter
                    let kind = match m.kind {
                        ast::MethodKind::Constructor => {
                            ctor_fn = Some(&*m.func);
                            continue;
                        }
                        ast::MethodKind::Get if m.is_static => 4u8,
                        ast::MethodKind::Set if m.is_static => 5u8,
                        ast::MethodKind::Get => 1u8,
                        ast::MethodKind::Set => 2u8,
                        ast::MethodKind::Method if m.is_static => 3u8,
                        ast::MethodKind::Method => 0u8,
                    };
                    // A DUPLICATE member name in the same list (`get b(){}` +
                    // `get ['b'](){}` — both statically nameable) REPLACES the
                    // earlier definition (last wins) while keeping its original
                    // position in property order.
                    fn put_member<'x>(
                        list: &mut Vec<(String, &'x ast::Function)>,
                        name: String,
                        f: &'x ast::Function,
                    ) {
                        if let Some(slot) = list.iter_mut().find(|(n, _)| *n == name) {
                            slot.1 = f;
                        } else {
                            list.push((name, f));
                        }
                    }
                    match class_key_name(&m.key) {
                        Ok(name) => match (m.is_static, m.kind) {
                            (true, ast::MethodKind::Method) => put_member(&mut statics, name, &*m.func),
                            (true, ast::MethodKind::Get) => put_member(&mut static_getters, name, &*m.func),
                            (true, ast::MethodKind::Set) => put_member(&mut static_setters, name, &*m.func),
                            (true, ast::MethodKind::Constructor) => unreachable!(),
                            (false, ast::MethodKind::Method) => put_member(&mut methods, name, &*m.func),
                            (false, ast::MethodKind::Get) => put_member(&mut getters, name, &*m.func),
                            (false, ast::MethodKind::Set) => put_member(&mut setters, name, &*m.func),
                            (false, ast::MethodKind::Constructor) => unreachable!(),
                        },
                        // Not statically nameable. A COMPUTED key is a
                        // runtime-keyed member; anything else is the error.
                        Err(e) => {
                            let Some(key) = computed_key(&m.key) else { return Err(e) };
                            // An INSTANCE member with a runtime-computed key keeps
                            // its SOURCE position in the prototype's property order:
                            // park a placeholder entry here; `ClassAddMember` renames
                            // it in place once the key value is known (the ordinal is
                            // rewritten to the member's func id below, which the
                            // dispatch arm can recompute).
                            if matches!(kind, 0 | 1 | 2) {
                                let ph = format!("\u{1}cm{}", computed.len());
                                let list = match kind {
                                    1 => &mut getters,
                                    2 => &mut setters,
                                    _ => &mut methods,
                                };
                                list.push((ph, &*m.func));
                            }
                            computed.push((key, &*m.func, kind));
                        }
                    }
                }
                ast::ClassMember::Field(p) => {
                    match class_key_name(&p.key) {
                        // A COMPUTED key whose literal folds to a "#..." STRING
                        // is a PUBLIC property that merely looks private — route it
                        // through the computed path (define_field → an ordinary,
                        // visible own prop) so it never collides with the class's
                        // real same-named private element.
                        Ok(name) if computed_key(&p.key).is_some() && name.starts_with('#') => {
                            let key = computed_key(&p.key)
                                .ok_or("unsupported computed class field key")?;
                            computed_fields_ordered.push((key, p.value.as_ref(), p.is_static));
                            if p.is_static {
                                static_order.push((1, computed_fields_ordered.len() - 1));
                            } else {
                                instance_computed_inits.push(p.value.as_ref());
                            }
                        }
                        // A COMPUTED static key whose literal folds to "prototype" is
                        // a runtime TypeError (not the named-`static prototype`
                        // early SyntaxError) — route it through the computed path so
                        // ClassStaticField performs the check. (An instance
                        // `['prototype']` field is allowed; this is static-only.)
                        Ok(name)
                            if p.is_static
                                && computed_key(&p.key).is_some()
                                && name == "prototype" =>
                        {
                            let key = computed_key(&p.key)
                                .ok_or("unsupported computed class field key")?;
                            computed_fields_ordered.push((key, p.value.as_ref(), true));
                            static_order.push((1, computed_fields_ordered.len() - 1));
                        }
                        // Static string key.
                        Ok(name) if p.is_static => {
                            static_fields.push((name, p.value.as_ref()));
                            static_order.push((0, static_fields.len() - 1));
                        }
                        // Instance string key.
                        Ok(name) => fields.push((name, p.value.as_ref())),
                        // Computed key `[expr] = v` — evaluated once at class def.
                        Err(e) => {
                            let Some(key) = computed_key(&p.key) else { return Err(e) };
                            computed_fields_ordered.push((key, p.value.as_ref(), p.is_static));
                            if p.is_static {
                                static_order.push((1, computed_fields_ordered.len() - 1));
                            } else {
                                instance_computed_inits.push(p.value.as_ref());
                            }
                        }
                    }
                }
                ast::ClassMember::StaticBlock(b) => {
                    static_blocks.push(&b[..]);
                    static_order.push((2, static_blocks.len() - 1));
                }
            }
        }
        // The class's private names are lexically visible to everything
        // compiled within its body (heritage, methods, field inits, static
        // blocks, nested evals). Pushed here; build_class_into pops when the
        // class-creation emission (incl. static-element phases) completes.
        let mut declared_privates: Vec<String> = Vec::new();
        for n in fields.iter().map(|(n, _)| n).chain(static_fields.iter().map(|(n, _)| n)) {
            if n.starts_with('#') {
                declared_privates.push(n.clone());
            }
        }
        for n in methods
            .iter()
            .map(|(n, _)| n)
            .chain(getters.iter().map(|(n, _)| n))
            .chain(setters.iter().map(|(n, _)| n))
            .chain(statics.iter().map(|(n, _)| n))
            .chain(static_getters.iter().map(|(n, _)| n))
            .chain(static_setters.iter().map(|(n, _)| n))
        {
            if n.starts_with('#') {
                declared_privates.push(n.clone());
            }
        }
        self.cx.private_names_stack.push(declared_privates);
        // Method protos.
        let mut method_defs: Vec<(String, u32)> = Vec::new();
        for (mname, func) in &methods {
            // A computed-key placeholder: position-only (no function of its
            // own — the computed loop below compiles it and rewrites the name).
            if mname.starts_with('\u{1}') {
                method_defs.push((mname.clone(), u32::MAX));
                continue;
            }
            let (params, rest, body) = function_parts(func)?;
            // A method's `.name` is the bare property key (`"m"` / `"#m"`), NOT
            // class-qualified — `toString` uses `proto.source`, set below.
            let mut proto = self.cx.compile_class_fn(
                mname,
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                body,
                super_class_id,
                false, // instance method: super resolves via the prototype chain
                func.is_generator,
                func.is_async,
            )?;
            proto.source = self.cx.src_slice(func.span.start, func.span.end);
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            method_defs.push((mname.clone(), fid));
        }
        // Getter protos (compiled identically to a no-arg method).
        let mut getter_defs: Vec<(String, u32)> = Vec::new();
        for (gname, func) in &getters {
            if gname.starts_with('\u{1}') {
                getter_defs.push((gname.clone(), u32::MAX));
                continue;
            }
            let (params, rest, body) = function_parts(func)?;
            let mut proto = self.cx.compile_class_fn(
                &format!("get {gname}"),
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                body,
                super_class_id,
                false, // instance getter: super resolves via the prototype chain
                false, // getters are never generators
                false, // getters are never async
            )?;
            proto.source = self.cx.src_slice(func.span.start, func.span.end);
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            getter_defs.push((gname.clone(), fid));
        }
        // Setter protos (a one-parameter method invoked on property write).
        let mut setter_defs: Vec<(String, u32)> = Vec::new();
        for (sname, func) in &setters {
            if sname.starts_with('\u{1}') {
                setter_defs.push((sname.clone(), u32::MAX));
                continue;
            }
            let (params, rest, body) = function_parts(func)?;
            let mut proto = self.cx.compile_class_fn(
                &format!("set {sname}"),
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                body,
                super_class_id,
                false, // instance setter: super resolves via the prototype chain
                false, // setters are never generators
                false, // setters are never async
            )?;
            proto.source = self.cx.src_slice(func.span.start, func.span.end);
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            setter_defs.push((sname.clone(), fid));
        }
        // Static method protos (this = the class value when called as `C.m()`).
        let mut static_defs: Vec<(String, u32)> = Vec::new();
        for (sname, func) in &statics {
            let (params, rest, body) = function_parts(func)?;
            let mut proto = self.cx.compile_class_fn(
                sname,
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                body,
                super_class_id,
                true, // static method: `super.x` resolves via the class's [[Prototype]] (parent class)
                func.is_generator,
                func.is_async,
            )?;
            proto.source =
                method_source(self.cx.src_slice(func.span.start, func.span.end), true);
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            static_defs.push((sname.clone(), fid));
        }
        // Static accessor protos (this = the class value on `C.name` read/write).
        let mut static_getter_defs: Vec<(String, u32)> = Vec::new();
        for (gname, func) in &static_getters {
            let (params, rest, body) = function_parts(func)?;
            let mut proto = self.cx.compile_class_fn(
                &format!("get {gname}"),
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                body,
                super_class_id,
                true, // static getter: `super.x` resolves via the class's [[Prototype]]
                false,
                false,
            )?;
            proto.source =
                method_source(self.cx.src_slice(func.span.start, func.span.end), true);
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            static_getter_defs.push((gname.clone(), fid));
        }
        let mut static_setter_defs: Vec<(String, u32)> = Vec::new();
        for (sname, func) in &static_setters {
            let (params, rest, body) = function_parts(func)?;
            let mut proto = self.cx.compile_class_fn(
                &format!("set {sname}"),
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                body,
                super_class_id,
                true, // static setter: `super.x` resolves via the class's [[Prototype]]
                false,
                false,
            )?;
            proto.source =
                method_source(self.cx.src_slice(func.span.start, func.span.end), true);
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            static_setter_defs.push((sname.clone(), fid));
        }
        // Constructor proto. With an explicit ctor: field inits prepended + the
        // user body (which calls `super` itself). Without one but with fields: a
        // fields-only proto (the `new` path runs the parent ctor first). Neither:
        // None.
        let has_explicit_ctor = ctor_fn.is_some();
        // A DERIVED class's EXPLICIT ctor defers its instance-field
        // initializers to a separate thunk run by the SuperCtor ops right
        // after super() completes (spec BindThisValue →
        // InitializeInstanceElements); its body carries no entry inits.
        // Base classes and implicit (fields-only) ctors keep the entry layout.
        let defer_fields = has_explicit_ctor && self.cx.class_derived;
        let empty_fields = Vec::new();
        let empty_cinits = Vec::new();
        let (ctor_fields, ctor_cinits) = if defer_fields {
            (&empty_fields, &empty_cinits)
        } else {
            (&fields, &instance_computed_inits)
        };
        let ctor = if has_explicit_ctor || !fields.is_empty() || !instance_computed_inits.is_empty() {
            let (params, rest, body) = match ctor_fn {
                Some(f) => function_parts(f)?,
                None => (Vec::new(), None, &[][..]),
            };
            let params_ast = ctor_fn.map(|f| &f.params);
            self.cx.compiling_ctor = true;
            let mut proto = self.cx.compile_class_fn(
                &format!("{cname}.constructor"),
                &params,
                rest.as_deref(),
                params_ast,
                ctor_fields,
                ctor_cinits,
                body,
                super_class_id,
                false, // a constructor's super is the instance prototype chain
                false, // a constructor is never a generator
                false, // a constructor is never async
            )?;
            if let Some(cf) = ctor_fn {
                proto.source = self.cx.src_slice(cf.span.start, cf.span.end);
            }
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            Some(fid)
        } else {
            None
        };
        let field_thunk = if defer_fields
            && (!fields.is_empty() || !instance_computed_inits.is_empty())
        {
            let proto = self.cx.compile_class_fn(
                &format!("{cname}.<instance_fields>"),
                &[],
                None,
                None,
                &fields,
                &instance_computed_inits,
                &[],
                super_class_id,
                false, // instance fields: super via the instance prototype chain
                false,
                false,
            )?;
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            Some(fid)
        } else {
            None
        };
        // Computed-key method protos. They carry no static name, so they're
        // installed at runtime by class_decl (which evaluates each key) via
        // ClassAddMember; here we just compile each proto and pair it with its key.
        let mut computed_defs: Vec<(&'b ast::Expr, u32, u8)> = Vec::new();
        for (key, func, kind) in &computed {
            let (params, rest, body) = function_parts(func)?;
            let mut proto = self.cx.compile_class_fn(
                &format!("{cname}.[computed]"),
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                body,
                super_class_id,
                matches!(*kind, 3 | 4 | 5), // static computed members get the parent-class super base
                func.is_generator,
                func.is_async,
            )?;
            proto.source = method_source(
                self.cx.src_slice(func.span.start, func.span.end),
                matches!(*kind, 3 | 4 | 5),
            );
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            computed_defs.push((key, fid, *kind));
        }
        // Rewrite each instance placeholder's ordinal to its member's FUNC ID
        // ("\u{1}cm{ordinal}" → "\u{1}cm{fid}"), so the `ClassAddMember`
        // dispatch arm — which knows only the func id — can find and rename
        // the parked entry in place (preserving the member's source position).
        for (i, (_, fid, kind)) in computed_defs.iter().enumerate() {
            if !matches!(*kind, 0 | 1 | 2) {
                continue;
            }
            let old = format!("\u{1}cm{i}");
            let list = match *kind {
                1 => &mut getter_defs,
                2 => &mut setter_defs,
                _ => &mut method_defs,
            };
            if let Some(slot) = list.iter_mut().find(|(n, _)| *n == old) {
                slot.0 = format!("\u{1}cm{fid}");
            }
        }
        // `static { … }` blocks: each body compiles to a zero-arg thunk (like a
        // method, so `this`/`super` and arguments work) that class_decl runs once
        // with `this` = the class.
        let mut static_block_fns: Vec<u32> = Vec::new();
        for body in &static_blocks {
            let proto = self.cx.compile_class_fn(
                &format!("{cname}.<static_block>"),
                &[],
                None,
                None,
                &[],
                &[],
                body,
                super_class_id,
                true, // a static block's `super.x` resolves via the class's [[Prototype]]
                false,
                false,
            )?;
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            static_block_fns.push(fid);
        }
        // Field names declared in this class body (instance + static), with the
        // "#" prefix preserved for private fields — fed to `MakeClass` so the
        // brand knows which private names it declares.
        let mut instance_field_names: Vec<String> = Vec::new();
        for (n, _) in &fields {
            instance_field_names.push(n.clone());
        }
        let mut static_field_names: Vec<String> = Vec::new();
        for (n, _) in &static_fields {
            static_field_names.push(n.clone());
        }
        self.cx.classes[class_id as usize] = ClassDef {
            name: cname,
            ctor,
            has_explicit_ctor,
            field_thunk,
            methods: method_defs,
            getters: getter_defs,
            setters: setter_defs,
            statics: static_defs,
            static_getters: static_getter_defs,
            static_setters: static_setter_defs,
            source: self.cx.src_slice(class.span.start, class.span.end),
            instance_field_names,
            static_field_names,
        };
        self.cx.class_enclosing = saved_enclosing;
        self.cx.class_derived = saved_derived;
        Ok((class_id, static_fields, computed_defs, computed_fields_ordered, static_block_fns, static_order))
    }

    /// The enclosing-function chain to hand a function nested in THIS one: our
    /// own enclosing chain plus a snapshot of our current bindings.
    pub(crate) fn child_enclosing(&self) -> Vec<EnclosingFn> {
        let mut e = self.enclosing.clone();
        e.push(self.snapshot());
        e
    }

    /// Stash (into `cx.pending_with_shadows`, consumed by the next
    /// `FnCompiler::new`) the with-shadow map for a function nested in THIS
    /// one: for each of the child's free names, the ordered (innermost-first)
    /// chain of enclosing with-object binding names that may shadow it —
    /// this function's active `with` scopes first, then the ones it inherited
    /// itself. `bound` is the child's own binding set (params + self-name +
    /// hoisted vars); no-op outside any `with` (the common path).
    pub(crate) fn stash_child_with_shadows(&mut self, bound: &[String], body: &[ast::Stmt]) {
        if self.with_stack.is_empty() && self.inherited_with_shadows.is_empty() {
            return;
        }
        let mut map = std::collections::HashMap::new();
        for name in capture::free_vars(bound, body) {
            let mut chain = self.with_names_for(&name);
            if !self.scopes.iter().flatten().any(|(n, _)| *n == name)
                && self.self_name.as_ref().map_or(true, |(n, _)| *n != name)
            {
                if let Some(inh) = self.inherited_with_shadows.get(&name) {
                    chain.extend(inh.iter().cloned());
                }
            }
            if !chain.is_empty() {
                map.insert(name, chain);
            }
        }
        self.cx.pending_with_shadows = map;
    }

    /// Compile a function expression, returning `(func_id, has_upvalues)`. The
    /// name (if any) is not hoisted — the value is produced explicitly by a
    /// `MakeFunc`/`MakeClosure` at the use site.
    pub(crate) fn compile_func_expr(&mut self, name: Option<String>, f: &ast::Function) -> R<(u32, bool)> {
        // Strict mode: a named function expression may not be named `eval`/`arguments`.
        // Use the SYNTACTIC name (`f.name`), not the inferred NamedEvaluation name.
        if let Some(id) = &f.name {
            strict_name_err(self.cx.in_strict || has_use_strict(fn_directives(f)), id)?;
        }
        // A named function expression's own name is self-bound (resolves to the
        // function) inside the body — and a nested closure may capture it, so add it
        // to the capture-analysis name set. Use the SYNTACTIC name (`f.name`), not the
        // inferred NamedEvaluation name (an anonymous expr has no self-binding).
        let self_name = f.name.as_ref().map(|n| n.to_string());
        let (params, rest, body) = function_parts(f)?;
        let mut names = with_rest(&params, &rest);
        names.extend(param_pattern_leaves(&f.params));
        if let Some(sn) = &self_name {
            names.push(sn.clone());
        }
        names.extend(hoisted_var_names(body)); // function-scoped `var`s (capture)
        let captured = capture::captured_locals(&names, body);
        self.stash_child_with_shadows(&names, body);
        let enclosing = self.child_enclosing();
        let mut proto = self.cx.compile_function_body(
            name.as_deref(),
            self_name.as_deref(),
            &params,
            rest.as_deref(),
            Some(&f.params),
            body,
            fn_directives(f), // body prologue: drives `"use strict"` strictness
            false,
            f.is_generator,
            f.is_async,
            captured,
            enclosing,
        )?;
        proto.source = self.cx.src_slice(f.span.start, f.span.end);
        let has_upvalues = !proto.upvalues.is_empty();
        let id = self.cx.functions.len() as u32;
        self.cx.functions.push(proto);
        Ok((id, has_upvalues))
    }

    /// Compile an arrow function, returning `(func_id, has_upvalues)`. An
    /// expression-bodied arrow (`x => x + 1`) is a function whose single
    /// statement returns the expression.
    pub(crate) fn compile_arrow(&mut self, a: &ast::Arrow, name: &str) -> R<(u32, bool)> {
        let params = param_slot_names(&a.params)?;
        let rest = rest_name(&a.params)?;
        let mut names = with_rest(&params, &rest);
        names.extend(param_pattern_leaves(&a.params));
        // See `arrow_body_stmts`: an expression body has no statement list of its
        // own, so the one-statement list oxc used to synthesise is rebuilt.
        let body = arrow_body_stmts(a);
        names.extend(hoisted_var_names(&body)); // function-scoped `var`s (capture)
        let captured = capture::captured_locals(&names, &body);
        self.stash_child_with_shadows(&names, &body);
        let enclosing = self.child_enclosing();
        let mut proto =
            self.cx.compile_arrow_body(&params, rest.as_deref(), a, captured, enclosing, self.super_class, self.super_static, self.super_home_obj)?;
        proto.name = name.to_string();
        proto.source = self.cx.src_slice(a.span.start, a.span.end);
        let has_upvalues = !proto.upvalues.is_empty();
        let id = self.cx.functions.len() as u32;
        self.cx.functions.push(proto);
        Ok((id, has_upvalues))
    }

    /// Emit `MakeClosure` if the just-compiled function captures upvalues, else
    /// `MakeFunc`.
    pub(crate) fn emit_make_callable(&mut self, dst: Reg, id: u32, has_upvalues: bool) {
        if has_upvalues {
            self.emit(Instr::MakeClosure { dst, func_id: id });
        } else {
            self.emit(Instr::MakeFunc { dst, func_id: id });
        }
    }

    /// Early SyntaxError (spec: AllPrivateIdentifiersValid) for a private
    /// access whose name no enclosing class declares (and, in a direct eval,
    /// is not visible from the call site).
    pub(crate) fn check_private_declared(&self, raw: &str) -> R<()> {
        let key = private_key(raw);
        if self.cx.private_name_declared(&key) {
            Ok(())
        } else {
            Err(format!(
                "SyntaxError: Private field '{key}' must be declared in an enclosing class"
            ))
        }
    }

    /// In a derived class constructor (or an arrow lexically inside one),
    /// `this` (reg 0) is in TDZ until `super()` completes: emit the runtime
    /// check before any `this` read or super-property reference. `this_override`
    /// (static-initializer context) has an initialized `this` — no check.
    pub(crate) fn this_check(&mut self) {
        if self.in_derived_ctor && self.this_override.is_none() {
            self.emit(Instr::ThisCheck { src: 0 });
        }
    }

    /// Emit creation of an ARROW value. Always `MakeArrow` (even with no
    /// upvalues) so the resulting closure carries the lexically-captured `this`
    /// of the defining frame — `MakeFunc` has no slot for it. The captured `this`
    /// is read from the effective-`this` register at the definition site
    /// (`this_override` when inside a static field initializer, else reg 0).
    pub(crate) fn emit_make_arrow(&mut self, dst: Reg, id: u32) {
        let this_reg = self.this_override.unwrap_or(0);
        self.emit(Instr::MakeArrow { dst, func_id: id, this_reg });
    }

}
