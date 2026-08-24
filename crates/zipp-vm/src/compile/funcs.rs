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

/// Install a statically-named class member. A DUPLICATE name in the same list
/// (`get b(){}` + `get ['b'](){}` — both statically nameable, or a user getter
/// and an `accessor` of the same name) REPLACES the earlier definition (last
/// wins) while keeping its original position in property order.
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

/// Drop `name` from a member list.
///
/// Each ClassElement is a DefinePropertyOrThrow onto ONE property map
/// (15.7.14 ClassDefinitionEvaluation → MethodDefinitionEvaluation), so a data
/// method and an accessor of the same key cannot coexist: whichever comes LAST
/// replaces the other's kind entirely. zipp keeps methods and accessors in
/// separate lists, so without this both survived and the earlier list won —
/// `class C { method(){r+=1} get method(){r+=2} }` ran the METHOD
/// (staging/sm/class/methodOverwrites.js). A get/set pair is the one case that
/// merges rather than replaces, so a getter/setter only clears `methods`.
fn drop_member(list: &mut Vec<(String, &ast::Function)>, name: &str) {
    list.retain(|(n, _)| n != name);
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
                // Dyn store in an eval program (the same gate `emit_b33_sync`
                // uses): the eval's own var/function names live in the caller
                // activation's EvalScope, and READS of them compile to
                // LoadGlobalDyn, so a plain StoreGlobal wrote the closure to a
                // realm slot the reader never consults —
                // `eval("function f(){ return outerLet; } typeof f")` answered
                // "undefined" whenever f captured ANYTHING.
                if self.box_all_locals || self.cx.dyn_global_zone {
                    self.emit(Instr::StoreGlobalDyn { idx: slot, src: tmp });
                } else {
                    self.emit(Instr::StoreGlobal { idx: slot, src: tmp });
                }
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
        let CompiledClass {
            class_id,
            static_fields,
            computed,
            computed_fields,
            static_block_fns,
            static_order,
            steps,
            dec_static_named,
            dec_computed,
            has_dec,
        } = self.compile_class(class, name)?;
        // The CLASS's own DecoratorList is evaluated FIRST — before the heritage.
        // `ClassDeclaration : DecoratorList class BindingIdentifier ClassTail`
        // evaluates the list, then ClassTail (which is where `extends` lives), so
        // `@log class C extends (side(), P) {}` calls `log`'s expression before
        // `side()`. The values must survive the whole class body, so their
        // registers are allocated once here and never reclaimed until the end.
        let class_decs = self.eval_decorator_list(&class.decorators)?;
        // Evaluate the superclass value (`extends P`) into a temp the VM links in.
        let parent_reg = if let Some(sc) = &class.superclass {
            let t = self.temp();
            // ClassHeritage evaluates in STRICT mode (the whole ClassTail is
            // strict code), regardless of the enclosing scope.
            let prev_strict = self.cx.in_strict;
            self.cx.in_strict = true;
            self.cx.strict_expr_region += 1;
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
            self.cx.strict_expr_region -= 1;
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
        // PHASE 1 — walk the class elements in DOCUMENT order, evaluating each
        // one's DecoratorList and then its ClassElementName. Computed member keys
        // install the members; computed FIELD keys evaluate ONCE (an instance key
        // parks on the class for the ctor's FieldInit; a static key parks in a
        // register that phase 2 consumes).
        //
        // Document order matters twice over: a decorator expression must run
        // immediately before its own element's key, and the two key kinds used to
        // be driven by two separate loops — so `class C { [a] = 1; [b](){} }`
        // evaluated `b` before `a`.
        let mut parked: Vec<Option<Reg>> = vec![None; computed_fields.len()];
        // (decoration group, element index, decorator register block) for phase 1b.
        let mut elem_decs: Vec<(u8, u16, Reg, u16)> = Vec::new();
        // Computed ClassElementNames (and element decorator expressions) are
        // ClassTail code: strict, even when the enclosing function is sloppy.
        let prev_strict_keys = self.cx.in_strict;
        self.cx.in_strict = true;
        self.cx.strict_expr_region += 1;
        for step in &steps {
            // The element's decorators FIRST (spec ClassElementEvaluation:
            // DecoratorListEvaluation precedes the key).
            let (dbase, dn) = self.eval_decorator_list(step.decorators)?;
            if let Some(elem) = step.dec_elem {
                elem_decs.push((step.dec_group, elem, dbase, dn));
            }
            match step.key {
                StepKey::None => {}
                StepKey::Member(i) => {
                    let (key, func, kind, pair) = &computed[i];
                    let save = self.next_reg;
                    // A DECORATED element needs its resolved key at decoration
                    // time (`context.name`), which only a temp can carry — and
                    // `DecKey` ToPropertyKeys in place, so the member op below
                    // consumes an already-coerced key and the element's
                    // ToPropertyKey still runs exactly once observably.
                    let needs_temp = pair.is_some() || step.dec_elem.is_some();
                    if !needs_temp {
                        let kr = self.expr(key)?;
                        self.emit(Instr::ClassAddMember {
                            class: cls,
                            key: kr,
                            func: *func,
                            kind: *kind,
                        });
                        self.next_reg = save;
                        continue;
                    }
                    // An auto-accessor installs TWO members from ONE
                    // ClassElementName, so `accessor [k] = v` must evaluate `k`
                    // once AND ToPropertyKey it once — a key whose `toString`
                    // counts calls sees exactly one. The key goes into a fresh
                    // temp (`expr` may hand back a live local's register) so the
                    // KEY_WRITEBACK bit can leave the coerced key there for the
                    // setter's instruction to reuse.
                    let kt = self.temp();
                    let v = self.expr_into(key, kt)?;
                    if v != kt {
                        self.emit(Instr::Move { dst: kt, src: v });
                    }
                    if let Some(elem) = step.dec_elem {
                        self.emit(Instr::DecKey { class: cls, elem, key: kt, class_id });
                    }
                    self.emit(Instr::ClassAddMember {
                        class: cls,
                        key: kt,
                        func: *func,
                        kind: *kind | if pair.is_some() { KEY_WRITEBACK } else { 0 },
                    });
                    if let Some(sfid) = pair {
                        self.emit(Instr::ClassAddMember {
                            class: cls,
                            key: kt,
                            func: *sfid,
                            kind: if *kind == 4 { 5 } else { 2 },
                        });
                    }
                    self.next_reg = save;
                }
                StepKey::Field(i) => {
                    let (key, _init, is_static) = &computed_fields[i];
                    if *is_static {
                        // Survives until phase 2 (not reclaimed).
                        let kr = self.temp();
                        let v = self.expr_into(key, kr)?;
                        if v != kr {
                            self.emit(Instr::Move { dst: kr, src: v });
                        }
                        if let Some(elem) = step.dec_elem {
                            self.emit(Instr::DecKey { class: cls, elem, key: kr, class_id });
                        }
                        parked[i] = Some(kr);
                    } else {
                        let save = self.next_reg;
                        let kr = match step.dec_elem {
                            // `DecKey` writes the coerced key back, so it needs a
                            // temp it may clobber.
                            Some(elem) => {
                                let kt = self.temp();
                                let v = self.expr_into(key, kt)?;
                                if v != kt {
                                    self.emit(Instr::Move { dst: kt, src: v });
                                }
                                self.emit(Instr::DecKey { class: cls, elem, key: kt, class_id });
                                kt
                            }
                            None => self.expr(key)?,
                        };
                        self.emit(Instr::PushFieldKey { class: cls, key: kr });
                        self.next_reg = save;
                    }
                }
            }
        }
        self.cx.in_strict = prev_strict_keys;
        self.cx.strict_expr_region -= 1;
        // PHASE 1b — APPLY the element decorators. A second pass, because
        // ClassDefinitionEvaluation evaluates every element (decorators + key +
        // method) before decorating any of them.
        //
        // The pass runs in FOUR GROUPS, not document order: static non-fields,
        // instance non-fields, static fields, instance fields — the four separate
        // loops the spec spells out. A stable sort keeps document order inside
        // each group. Flat document order is observably different the moment a
        // class mixes a decorated field with a decorated method, and it is what
        // Babel's 2023-11 transform and TypeScript's __esDecorate both produce.
        elem_decs.sort_by_key(|&(group, ..)| group);
        for (_, elem, dbase, dn) in elem_decs {
            self.emit(Instr::DecElem { class: cls, elem, arg_base: dbase, argc: dn, class_id });
        }
        // PHASE 1c — the CLASS decorators, then the static `addInitializer`
        // callbacks. Both precede the static field initializers: a class
        // decorator may REPLACE the class, and `@dec class C { static x = 1 }`
        // must put `x` on the replacement (which is what `cls` now holds).
        if has_dec {
            self.emit(Instr::DecClass {
                class: cls,
                arg_base: class_decs.0,
                argc: class_decs.1,
            });
            self.emit(Instr::DecInits { class_id, which: 1, elem: 0, recv: cls });
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
                    self.cx.strict_expr_region += 1;
                    let (prev_sc, prev_ss) = (self.super_class, self.super_static);
                    self.super_class = Some(class_id);
                    self.super_static = true;
                    let saved_hc = self.heritage_class.take();
                    if let Some(n) = &static_self_name {
                        self.heritage_class = Some((n.clone(), class_id));
                        // …and on the cx-level stack, so a nested function/arrow
                        // inside the initializer also sees the inner binding
                        // (class_names is popped when compile_class returns, so
                        // without this `static field = () => C` lost the binding).
                        self.cx.heritage_classes.push((n.clone(), class_id));
                    }
                    let v = match finit {
                        Some(e) => self.expr(e)?,
                        None => {
                            let t = self.temp();
                            self.emit(Instr::LoadUndefined { dst: t });
                            t
                        }
                    };
                    if static_self_name.is_some() {
                        self.cx.heritage_classes.pop();
                    }
                    self.heritage_class = saved_hc;
                    self.super_class = prev_sc;
                    self.super_static = prev_ss;
                    self.cx.in_strict = prev_strict;
                    self.cx.strict_expr_region -= 1;
                    self.this_override = None;
                    // NamedEvaluation: an anonymous fn/arrow/class initializer takes
                    // the field name (incl. the literal "#field" for privates).
                    if matches!(finit, Some(e) if is_anonymous_fn_def(e)) {
                        let kr = self.temp();
                        let cidx = self.add_string_const(fname);
                        self.emit(Instr::LoadConst { dst: kr, idx: cidx });
                        self.emit(Instr::SetFnNameFromKey { func: v, key: kr, prefix: 0 });
                    }
                    // A decorated static field's value goes through the
                    // initializer chain its decorators returned, with `this` =
                    // the class (which by now is the DECORATED class).
                    let v = match dec_static_named.get(idx).copied().flatten() {
                        Some(elem) => {
                            let t = self.temp();
                            self.emit(Instr::Move { dst: t, src: v });
                            self.emit(Instr::DecField { class_id, elem, val: t, recv: cls });
                            t
                        }
                        None => v,
                    };
                    let name_idx = self.string_name(fname);
                    self.emit(Instr::SetProp { obj: cls, name: name_idx, val: v, strict: false });
                    // InitializeFieldOrAccessor: this element's OWN
                    // `addInitializer` callbacks run once it is defined and
                    // before the next static element, so `this[name]` is already
                    // set and the following static field is not.
                    if let Some(elem) = dec_static_named.get(idx).copied().flatten() {
                        self.emit(Instr::DecInits { class_id, which: 3, elem, recv: cls });
                    }
                    self.next_reg = save;
                }
                1 => {
                    let Some(kr) = parked[idx] else { continue };
                    let (_key, init, _is_static) = &computed_fields[idx];
                    let save = self.next_reg;
                    self.this_override = Some(cls);
                    let prev_strict = self.cx.in_strict;
                    self.cx.in_strict = true;
                    self.cx.strict_expr_region += 1;
                    let (prev_sc, prev_ss) = (self.super_class, self.super_static);
                    self.super_class = Some(class_id);
                    self.super_static = true;
                    let saved_hc = self.heritage_class.take();
                    if let Some(n) = &static_self_name {
                        self.heritage_class = Some((n.clone(), class_id));
                        self.cx.heritage_classes.push((n.clone(), class_id));
                    }
                    let vr = match init {
                        Some(e) => self.expr(e)?,
                        None => {
                            let t = self.temp();
                            self.emit(Instr::LoadUndefined { dst: t });
                            t
                        }
                    };
                    if static_self_name.is_some() {
                        self.cx.heritage_classes.pop();
                    }
                    self.heritage_class = saved_hc;
                    self.super_class = prev_sc;
                    self.super_static = prev_ss;
                    self.cx.in_strict = prev_strict;
                    self.cx.strict_expr_region -= 1;
                    self.this_override = None;
                    let vr = match dec_computed.get(idx).copied().flatten() {
                        Some(elem) => {
                            let t = self.temp();
                            self.emit(Instr::Move { dst: t, src: vr });
                            self.emit(Instr::DecField { class_id, elem, val: t, recv: cls });
                            t
                        }
                        None => vr,
                    };
                    // A static field may not be named `prototype` (TypeError); the
                    // op ToPropertyKeys the (already-evaluated) key and checks it.
                    self.emit(Instr::ClassStaticField { class: cls, key: kr, val: vr });
                    if let Some(elem) = dec_computed.get(idx).copied().flatten() {
                        self.emit(Instr::DecInits { class_id, which: 3, elem, recv: cls });
                    }
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
        // The class `addInitializer` callbacks are the LAST step of
        // ClassDefinitionEvaluation — the point at which the class is fully
        // defined (decorated, with its static elements initialized), which is
        // exactly the guarantee the proposal gives them.
        if has_dec {
            self.emit(Instr::DecInits { class_id, which: 2, elem: 0, recv: cls });
        }
        self.cx.private_names_stack.pop();
        Ok(())
    }

    /// Evaluate a DecoratorList left to right into a CONTIGUOUS register block of
    /// PAIRS — `[fn0, recv0, fn1, recv1, …]` — returning `(base, count)` where
    /// count is the number of decorators. The block is allocated up front so a
    /// decorator's own scratch registers cannot land between two of them, and it
    /// is deliberately NOT reclaimed: a class decorator's value has to survive
    /// the entire class body, and an element's has to survive until the
    /// decoration pass.
    ///
    /// `recv` is the decorator's `[[Receiver]]`. DecoratorEvaluation keeps the
    /// REFERENCE it evaluated, and ApplyDecorators calls through it, so `@a.b`
    /// invokes `b` as a method of `a` — `this` inside it is `a`, not undefined.
    /// Only a Reference has a base, so a bare `@a` and a `@a.b()` call (whose
    /// Evaluation produces a value) get `undefined`.
    fn eval_decorator_list(&mut self, list: &[ast::Expr]) -> R<(Reg, u16)> {
        if list.is_empty() {
            return Ok((0, 0));
        }
        let base = self.next_reg;
        for _ in 0..list.len() * 2 {
            self.temp();
        }
        let floor = self.next_reg;
        for (i, d) in list.iter().enumerate() {
            self.next_reg = floor;
            let dst = base + (i * 2) as Reg;
            let recv = dst + 1;
            // `@(a.b)` parses to the same Member node as `@a.b` — the
            // parenthesized production covers a MemberExpression, so it keeps the
            // reference too. `super.x` and an optional chain are excluded: neither
            // can appear in the Decorator grammar, and `super`'s receiver is not
            // its object expression.
            let bound = match d {
                ast::Expr::Member(m)
                    if !m.optional && !matches!(m.object, ast::Expr::Super) =>
                {
                    let o = self.expr_into(&m.object, recv)?;
                    if o != recv {
                        self.emit(Instr::Move { dst: recv, src: o });
                    }
                    match &m.prop {
                        ast::MemberProp::Ident(p) => {
                            let name = self.string_name(p);
                            self.emit(Instr::GetProp { dst, obj: recv, name });
                        }
                        ast::MemberProp::Private(p) => {
                            self.check_private_declared(p)?;
                            let name = self.string_name(&private_key(p));
                            self.emit(Instr::GetProp { dst, obj: recv, name });
                        }
                        ast::MemberProp::Computed(k) => {
                            let save = self.next_reg;
                            let kr = self.expr(k)?;
                            self.emit(Instr::GetIndex { dst, obj: recv, key: kr });
                            self.next_reg = save;
                        }
                    }
                    true
                }
                _ => false,
            };
            if !bound {
                let v = self.expr(d)?;
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.emit(Instr::LoadUndefined { dst: recv });
            }
        }
        self.next_reg = floor;
        Ok((base, list.len() as u16))
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
    ) -> R<CompiledClass<'b>> {
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
            proto_order: Vec::new(),
            statics: Vec::new(),
            static_getters: Vec::new(),
            static_setters: Vec::new(),
            source: String::new(), // filled in below once the body is compiled
            instance_field_names: Vec::new(),
            static_field_names: Vec::new(),
            dec_plan: None,
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
        // Public INSTANCE prototype keys in SOURCE order — see `ClassDef::proto_order`.
        // The kind-grouped lists above cannot express `get g(){} m(){}`'s interleaving.
        let mut proto_order: Vec<String> = Vec::new();
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
        // The 4th slot is an AUTO-ACCESSOR's setter, riding along on its
        // getter's entry: `accessor [k] = v` must evaluate `k` exactly ONCE and
        // install two members from it, which two independent entries could not
        // do.
        let mut computed: Vec<(&'b ast::Expr, &'b ast::Function, u8, Option<&'b ast::Function>)> =
            Vec::new();
        // ── decorators ──
        // `plan` collects one DecElemDef per decorated element, in document
        // order; `steps` records for each element what class-definition-time work
        // it needs (its decorator expressions and/or its computed key), also in
        // document order. `dec_*` map a decorated FIELD back to its element index
        // so the constructor's initializer sequence can find its chain.
        let mut plan = crate::bytecode::DecPlan {
            class_decorators: class.decorators.len() as u32,
            elements: Vec::new(),
        };
        let mut steps: Vec<ClassStep<'b>> = Vec::new();
        let mut dec_named: Vec<Option<u16>> = Vec::new();
        let mut dec_static_named: Vec<Option<u16>> = Vec::new();
        let mut dec_computed: Vec<Option<u16>> = Vec::new();
        let mut dec_instance_computed: Vec<Option<u16>> = Vec::new();
        // Source order of the INSTANCE field lists — the twin of `static_order`,
        // so a decorated or undecorated `[a] = 1; b = 2` initializes a then b.
        let mut instance_order: Vec<(u8, usize)> = Vec::new();
        // Push a `DecElemDef` for the element about to be compiled and return its
        // index, or `None` when it carries no decorators. `computed` is only a
        // GUESS here — `class_key_name` folds `["a"]`, `[1]` and
        // `[Symbol.iterator]` to a compile-time name, and such an element takes
        // the static-name path (no `DecKey` op). `dec_fix_key!` below corrects it
        // once the branch that consumed the key has run.
        macro_rules! dec_elem {
            ($kind:expr, $is_static:expr, $key:expr, $decs:expr, $storage:expr) => {{
                if $decs.is_empty() {
                    None
                } else {
                    let (nm, computed) = match class_key_name($key) {
                        Ok(n) if computed_key($key).is_none() => (n, false),
                        _ => (String::new(), true),
                    };
                    plan.elements.push(crate::bytecode::DecElemDef {
                        kind: $kind,
                        is_static: $is_static,
                        is_private: nm.starts_with('#')
                            || matches!($key, ast::PropKey::Private(_)),
                        name: nm,
                        computed,
                        sym_key: false,
                        storage: $storage,
                    });
                    Some((plan.elements.len() - 1) as u16)
                }
            }};
        }
        // Settle a decorated element's key AFTER its branch has run: `went`
        // records whether the element actually took the computed path. A folded
        // literal key (`@dec ["m"]() {}`) did not, and marking it computed left
        // the runtime reading an unset `DecState::keys` slot — `context.name`
        // came out `undefined` and a method decorator's replacement was installed
        // under the key "undefined", i.e. silently dropped.
        macro_rules! dec_fix_key {
            ($de:expr, $key:expr, $went:expr) => {
                if let Some(ix) = $de {
                    if !$went {
                        let nm = class_key_name($key).unwrap_or_default();
                        let e = &mut plan.elements[ix as usize];
                        e.is_private = nm.starts_with('#');
                        // Only `[Symbol.x]` — a computed MEMBER expression —
                        // folds to a Symbol. A `"@@x"` string key, computed or
                        // not, is a string key that merely spells the engine's
                        // internal convention, and `context.name` must say so.
                        e.sym_key = nm.starts_with("@@")
                            && matches!($key, ast::PropKey::Computed(ast::Expr::Member(_)));
                        e.name = nm;
                        e.computed = false;
                    }
                }
            };
        }
        // NOTE: `ClassMember` has exactly three variants (Method / Field /
        // StaticBlock), so the old trailing `_ => Err("unsupported class member
        // in the zipp-vm subset")` arm is gone — it would now be an unreachable
        // pattern. Decorated members still cannot reach here: they have no AST
        // representation and are rejected by the parser.
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
                    let dkind = match m.kind {
                        ast::MethodKind::Get => crate::vm::decorators::DK_GETTER,
                        ast::MethodKind::Set => crate::vm::decorators::DK_SETTER,
                        _ => crate::vm::decorators::DK_METHOD,
                    };
                    let de =
                        dec_elem!(dkind, m.is_static, &m.key, m.decorators, String::new());
                    let computed_before = computed.len();
                    match class_key_name(&m.key) {
                        Ok(name) => {
                            // A public INSTANCE member takes (or keeps) its
                            // source position on the prototype; a later
                            // same-key element redefines it in place.
                            if !m.is_static
                                && !name.starts_with('#')
                                && !proto_order.iter().any(|n| *n == name)
                            {
                                proto_order.push(name.clone());
                            }
                            match (m.is_static, m.kind) {
                            (true, ast::MethodKind::Method) => {
                                drop_member(&mut static_getters, &name);
                                drop_member(&mut static_setters, &name);
                                put_member(&mut statics, name, &*m.func)
                            }
                            (true, ast::MethodKind::Get) => {
                                drop_member(&mut statics, &name);
                                put_member(&mut static_getters, name, &*m.func)
                            }
                            (true, ast::MethodKind::Set) => {
                                drop_member(&mut statics, &name);
                                put_member(&mut static_setters, name, &*m.func)
                            }
                            (true, ast::MethodKind::Constructor) => unreachable!(),
                            (false, ast::MethodKind::Method) => {
                                drop_member(&mut getters, &name);
                                drop_member(&mut setters, &name);
                                put_member(&mut methods, name, &*m.func)
                            }
                            (false, ast::MethodKind::Get) => {
                                drop_member(&mut methods, &name);
                                put_member(&mut getters, name, &*m.func)
                            }
                            (false, ast::MethodKind::Set) => {
                                drop_member(&mut methods, &name);
                                put_member(&mut setters, name, &*m.func)
                            }
                            (false, ast::MethodKind::Constructor) => unreachable!(),
                            }
                        }
                        // Not statically nameable. A COMPUTED key is a
                        // runtime-keyed member; anything else is the error.
                        Err(e) => {
                            let Some(key) = computed_key(&m.key) else { return Err(e) };
                            // A member with a runtime-computed key keeps its
                            // SOURCE position in the owner's property order:
                            // park a placeholder entry here; `ClassAddMember`
                            // renames it in place once the key value is known
                            // (the ordinal is rewritten to the member's func id
                            // below, which the dispatch arm can recompute). The
                            // placeholder is also what tells the VM which of a
                            // duplicate pair came first. Kind 3 (static method)
                            // is excluded: it lands in the class's ObjMap, which
                            // already keeps insertion order.
                            if matches!(kind, 0 | 1 | 2 | 4 | 5) {
                                let ph = format!("\u{1}cm{}", computed.len());
                                if matches!(kind, 0 | 1 | 2) {
                                    proto_order.push(ph.clone());
                                }
                                let list = match kind {
                                    1 => &mut getters,
                                    2 => &mut setters,
                                    4 => &mut static_getters,
                                    5 => &mut static_setters,
                                    _ => &mut methods,
                                };
                                list.push((ph, &*m.func));
                            }
                            computed.push((key, &*m.func, kind, None));
                        }
                    }
                    let went = computed.len() != computed_before;
                    dec_fix_key!(de, &m.key, went);
                    // Only elements with class-definition-time work get a step:
                    // a decorator list, a computed key, or both.
                    if de.is_some() || went {
                        steps.push(ClassStep {
                            decorators: &m.decorators,
                            dec_elem: de,
                            dec_group: if m.is_static { 0 } else { 1 },
                            key: if went {
                                StepKey::Member(computed_before)
                            } else {
                                StepKey::None
                            },
                        });
                    }
                }
                // `accessor x = v` — the auto-accessor of proposal-decorators.
                // One element, three pieces: a private backing FIELD (same
                // initializer, same timing as an ordinary field) plus the
                // get/set pair that reads and writes it, installed under the
                // declared key. Nothing here is new machinery — the pair is
                // ordinary class accessors and the slot an ordinary private
                // field, which is what gives `Derived.staticAccessor` its
                // TypeError and keeps a base/derived same-named pair distinct.
                ast::ClassMember::Field(p) if p.accessor.is_some() => {
                    let acc = p.accessor.as_ref().unwrap();
                    let slot = private_key(&acc.storage);
                    // An auto-accessor decorator's returned `init` feeds the
                    // BACKING SLOT, not the public key — so the element index is
                    // recorded against the private field, which is always a
                    // statically-named one even when the accessor's key is
                    // computed.
                    let de = dec_elem!(
                        crate::vm::decorators::DK_ACCESSOR,
                        p.is_static,
                        &p.key,
                        p.decorators,
                        slot.clone()
                    );
                    let computed_before = computed.len();
                    if p.is_static {
                        static_fields.push((slot, p.value.as_ref()));
                        dec_static_named.push(de);
                        static_order.push((0, static_fields.len() - 1));
                    } else {
                        fields.push((slot, p.value.as_ref()));
                        dec_named.push(de);
                        instance_order.push((0, fields.len() - 1));
                    }
                    match class_key_name(&p.key) {
                        Ok(name) => {
                            if !p.is_static
                                && !name.starts_with('#')
                                && !proto_order.iter().any(|n| *n == name)
                            {
                                proto_order.push(name.clone());
                            }
                            let (ml, gl, sl) = if p.is_static {
                                (&mut statics, &mut static_getters, &mut static_setters)
                            } else {
                                (&mut methods, &mut getters, &mut setters)
                            };
                            // An auto-accessor is a get/set PAIR: like any
                            // accessor it replaces a same-key data method.
                            drop_member(ml, &name);
                            put_member(gl, name.clone(), &*acc.getter);
                            put_member(sl, name, &*acc.setter);
                        }
                        Err(e) => {
                            let Some(key) = computed_key(&p.key) else { return Err(e) };
                            // Both sides keep their source position via a parked
                            // placeholder, exactly as a computed accessor does.
                            let i = computed.len();
                            let (gl, sl) = if p.is_static {
                                (&mut static_getters, &mut static_setters)
                            } else {
                                (&mut getters, &mut setters)
                            };
                            if !p.is_static {
                                proto_order.push(format!("\u{1}cm{i}"));
                            }
                            gl.push((format!("\u{1}cm{i}"), &*acc.getter));
                            sl.push((format!("\u{1}cs{i}"), &*acc.setter));
                            let kind = if p.is_static { 4u8 } else { 1u8 };
                            computed.push((key, &*acc.getter, kind, Some(&*acc.setter)));
                        }
                    }
                    let went = computed.len() != computed_before;
                    dec_fix_key!(de, &p.key, went);
                    if de.is_some() || went {
                        steps.push(ClassStep {
                            decorators: &p.decorators,
                            dec_elem: de,
                            // An auto-accessor is an ~accessor~ element, which
                            // ClassDefinitionEvaluation decorates in the NON-field
                            // loop even though its storage is a field.
                            dec_group: if p.is_static { 0 } else { 1 },
                            key: if went {
                                StepKey::Member(computed_before)
                            } else {
                                StepKey::None
                            },
                        });
                    }
                }
                ast::ClassMember::Field(p) => {
                    let de = dec_elem!(
                        crate::vm::decorators::DK_FIELD,
                        p.is_static,
                        &p.key,
                        p.decorators,
                        String::new()
                    );
                    let cf_before = computed_fields_ordered.len();
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
                            dec_computed.push(de);
                            if p.is_static {
                                static_order.push((1, computed_fields_ordered.len() - 1));
                            } else {
                                instance_computed_inits.push(p.value.as_ref());
                                dec_instance_computed.push(de);
                                instance_order
                                    .push((1, instance_computed_inits.len() - 1));
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
                            dec_computed.push(de);
                            static_order.push((1, computed_fields_ordered.len() - 1));
                        }
                        // Static string key.
                        Ok(name) if p.is_static => {
                            static_fields.push((name, p.value.as_ref()));
                            dec_static_named.push(de);
                            static_order.push((0, static_fields.len() - 1));
                        }
                        // Instance string key.
                        Ok(name) => {
                            fields.push((name, p.value.as_ref()));
                            dec_named.push(de);
                            instance_order.push((0, fields.len() - 1));
                        }
                        // Computed key `[expr] = v` — evaluated once at class def.
                        Err(e) => {
                            let Some(key) = computed_key(&p.key) else { return Err(e) };
                            computed_fields_ordered.push((key, p.value.as_ref(), p.is_static));
                            dec_computed.push(de);
                            if p.is_static {
                                static_order.push((1, computed_fields_ordered.len() - 1));
                            } else {
                                instance_computed_inits.push(p.value.as_ref());
                                dec_instance_computed.push(de);
                                instance_order
                                    .push((1, instance_computed_inits.len() - 1));
                            }
                        }
                    }
                    let went = computed_fields_ordered.len() != cf_before;
                    dec_fix_key!(de, &p.key, went);
                    if de.is_some() || went {
                        steps.push(ClassStep {
                            decorators: &p.decorators,
                            dec_elem: de,
                            dec_group: if p.is_static { 2 } else { 3 },
                            key: if went {
                                StepKey::Field(cf_before)
                            } else {
                                StepKey::None
                            },
                        });
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
                &fn_name_for_key(mname),
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                &[],
                body,
                super_class_id,
                false, // instance method: super resolves via the prototype chain
                func.is_generator,
                func.is_async,
                None,
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
                &format!("get {}", fn_name_for_key(gname)),
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                &[],
                body,
                super_class_id,
                false, // instance getter: super resolves via the prototype chain
                false, // getters are never generators
                false, // getters are never async
                None,
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
                &format!("set {}", fn_name_for_key(sname)),
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                &[],
                body,
                super_class_id,
                false, // instance setter: super resolves via the prototype chain
                false, // setters are never generators
                false, // setters are never async
                None,
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
                &fn_name_for_key(sname),
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                &[],
                body,
                super_class_id,
                true, // static method: `super.x` resolves via the class's [[Prototype]] (parent class)
                func.is_generator,
                func.is_async,
                None,
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
            if gname.starts_with('\u{1}') {
                static_getter_defs.push((gname.clone(), u32::MAX));
                continue;
            }
            let (params, rest, body) = function_parts(func)?;
            let mut proto = self.cx.compile_class_fn(
                &format!("get {}", fn_name_for_key(gname)),
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                &[],
                body,
                super_class_id,
                true, // static getter: `super.x` resolves via the class's [[Prototype]]
                false,
                false,
                None,
            )?;
            proto.source =
                method_source(self.cx.src_slice(func.span.start, func.span.end), true);
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            static_getter_defs.push((gname.clone(), fid));
        }
        let mut static_setter_defs: Vec<(String, u32)> = Vec::new();
        for (sname, func) in &static_setters {
            if sname.starts_with('\u{1}') {
                static_setter_defs.push((sname.clone(), u32::MAX));
                continue;
            }
            let (params, rest, body) = function_parts(func)?;
            let mut proto = self.cx.compile_class_fn(
                &format!("set {}", fn_name_for_key(sname)),
                &params,
                rest.as_deref(),
                Some(&func.params),
                &[],
                &[],
                &[],
                body,
                super_class_id,
                true, // static setter: `super.x` resolves via the class's [[Prototype]]
                false,
                false,
                None,
            )?;
            proto.source =
                method_source(self.cx.src_slice(func.span.start, func.span.end), true);
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            static_setter_defs.push((sname.clone(), fid));
        }
        // Constructor proto. With an explicit ctor: the user body alone (field
        // inits move to the thunk below). Without one but with fields: a
        // fields-only proto (the `new` path runs the parent ctor first). Neither:
        // None.
        let has_explicit_ctor = ctor_fn.is_some();
        // ANY class with an EXPLICIT ctor defers its instance-field initializers
        // to a separate thunk: a derived class's is run by the SuperCtor ops
        // right after super() completes, a base class's by [[Construct]] before
        // it enters the ctor (spec InitializeInstanceElements precedes
        // OrdinaryCallEvaluateBody). Prepending them to the ctor body — which is
        // what a base class used to do — put them AFTER
        // FunctionDeclarationInstantiation, so `class A { #x = f(); constructor(o
        // = g()) {} }` ran g before f (staging/sm/fields/init-order.js) and
        // `constructor(o = this.#x)` could not even see the field
        // (staging/sm/PrivateName/constructor-args.js). The thunk is also a fresh
        // scope, so a field initializer's free variable no longer binds to a
        // same-named CONSTRUCTOR PARAMETER — an initializer is evaluated in the
        // class scope, never the ctor's.
        // Implicit (fields-only) ctors keep the entry layout: there are no
        // parameters and no body for the fields to race with.
        let defer_fields = has_explicit_ctor;
        let empty_fields = Vec::new();
        let empty_cinits = Vec::new();
        let empty_order: Vec<(u8, usize)> = Vec::new();
        let (ctor_fields, ctor_cinits, ctor_order) = if defer_fields {
            (&empty_fields, &empty_cinits, &empty_order)
        } else {
            (&fields, &instance_computed_inits, &instance_order)
        };
        // The decorator side of instance element initialization. `run_inits` also
        // FORCES a ctor/thunk to exist: `@dec m(){}` with no fields at all still
        // needs somewhere to run the instance `addInitializer` callbacks from.
        // Only METHOD-ish elements feed `instanceMethodExtraInitializers`; a
        // decorated field or auto-accessor already has a field entry (so a thunk
        // exists) and runs its own callbacks from its `DecInits{which:3}`.
        let run_inits = plan.elements.iter().any(|e| {
            !e.is_static
                && !matches!(
                    e.kind,
                    crate::vm::decorators::DK_FIELD | crate::vm::decorators::DK_ACCESSOR
                )
        });
        let dec_plan_fields = (!plan.elements.is_empty()).then(|| DecFieldPlan {
            class_id,
            named: &dec_named,
            computed: &dec_instance_computed,
            run_inits,
        });
        // With an explicit ctor the fields moved to the thunk, and so did the
        // extra-initializer run — the thunk IS InitializeInstanceElements there.
        let ctor_dec = if defer_fields { None } else { dec_plan_fields.as_ref() };
        let ctor = if has_explicit_ctor
            || !fields.is_empty()
            || !instance_computed_inits.is_empty()
            || (run_inits && !defer_fields)
        {
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
                ctor_order,
                body,
                super_class_id,
                false, // a constructor's super is the instance prototype chain
                false, // a constructor is never a generator
                false, // a constructor is never async
                ctor_dec,
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
            && (!fields.is_empty() || !instance_computed_inits.is_empty() || run_inits)
        {
            let proto = self.cx.compile_class_fn(
                &format!("{cname}.<instance_fields>"),
                &[],
                None,
                None,
                &fields,
                &instance_computed_inits,
                &instance_order,
                &[],
                super_class_id,
                false, // instance fields: super via the instance prototype chain
                false,
                false,
                dec_plan_fields.as_ref(),
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
        let mut computed_defs: Vec<(&'b ast::Expr, u32, u8, Option<u32>)> = Vec::new();
        for (key, func, kind, pair) in &computed {
            let is_static = matches!(*kind, 3 | 4 | 5);
            let mut compile_one = |f: &ast::Function| -> R<u32> {
                let (params, rest, body) = function_parts(f)?;
                let mut proto = self.cx.compile_class_fn(
                    &format!("{cname}.[computed]"),
                    &params,
                    rest.as_deref(),
                    Some(&f.params),
                    &[],
                    &[],
                    &[],
                    body,
                    super_class_id,
                    is_static, // static computed members get the parent-class super base
                    f.is_generator,
                    f.is_async,
                    None,
                )?;
                proto.source =
                    method_source(self.cx.src_slice(f.span.start, f.span.end), is_static);
                let fid = self.cx.functions.len() as u32;
                self.cx.functions.push(proto);
                Ok(fid)
            };
            let fid = compile_one(func)?;
            let pair_fid = match pair {
                Some(s) => Some(compile_one(s)?),
                None => None,
            };
            computed_defs.push((key, fid, *kind, pair_fid));
        }
        // Rewrite each instance placeholder's ordinal to its member's FUNC ID
        // ("\u{1}cm{ordinal}" → "\u{1}cm{fid}"), so the `ClassAddMember`
        // dispatch arm — which knows only the func id — can find and rename
        // the parked entry in place (preserving the member's source position).
        // An auto-accessor pair parks two entries, distinguished at compile time
        // by the "cs" marker; both end up as the "cm{fid}" the VM looks for.
        for (i, (_, fid, kind, pair_fid)) in computed_defs.iter().enumerate() {
            if !matches!(*kind, 0 | 1 | 2 | 4 | 5) {
                continue;
            }
            let old = format!("\u{1}cm{i}");
            let list = match *kind {
                1 => &mut getter_defs,
                2 => &mut setter_defs,
                4 => &mut static_getter_defs,
                5 => &mut static_setter_defs,
                _ => &mut method_defs,
            };
            if let Some(slot) = list.iter_mut().find(|(n, _)| *n == old) {
                slot.0 = format!("\u{1}cm{fid}");
            }
            // …and the same rename in the source-order list the prototype's
            // property map is built from.
            if let Some(slot) = proto_order.iter_mut().find(|n| **n == old) {
                *slot = format!("\u{1}cm{fid}");
            }
            if let Some(sf) = pair_fid {
                let old = format!("\u{1}cs{i}");
                let list =
                    if *kind == 4 { &mut static_setter_defs } else { &mut setter_defs };
                if let Some(slot) = list.iter_mut().find(|(n, _)| *n == old) {
                    slot.0 = format!("\u{1}cm{sf}");
                }
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
                &[],
                body,
                super_class_id,
                true, // a static block's `super.x` resolves via the class's [[Prototype]]
                false,
                false,
                None,
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
            proto_order,
            statics: static_defs,
            static_getters: static_getter_defs,
            static_setters: static_setter_defs,
            source: self.cx.src_slice(class.span.start, class.span.end),
            instance_field_names,
            static_field_names,
            dec_plan: (plan.class_decorators > 0 || !plan.elements.is_empty())
                .then(|| Box::new(plan.clone())),
        };
        self.cx.class_enclosing = saved_enclosing;
        self.cx.class_derived = saved_derived;
        // Pop this class's inner-binding entry: the stack must hold exactly the
        // LEXICALLY ENCLOSING classes of the compile point (sibling classes
        // compiled earlier must not leak their names into later code).
        self.cx.class_names.pop();
        Ok(CompiledClass {
            class_id,
            static_fields,
            computed: computed_defs,
            computed_fields: computed_fields_ordered,
            static_block_fns,
            static_order,
            steps,
            dec_static_named,
            dec_computed,
            has_dec: plan.class_decorators > 0 || !plan.elements.is_empty(),
        })
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
        self.stash_child_free_names(capture::free_vars(bound, body));
    }

    /// Arrow-body counterpart of `stash_child_with_shadows`. The capture walk
    /// accepts both block and bare-expression bodies directly, so an expression
    /// arrow never needs a cloned synthetic `ExpressionStatement`.
    fn stash_arrow_child_with_shadows(&mut self, bound: &[String], body: &ast::ArrowBody) {
        if self.with_stack.is_empty() && self.inherited_with_shadows.is_empty() {
            return;
        }
        self.stash_child_free_names(capture::free_vars_arrow(bound, body));
    }

    fn stash_child_free_names(&mut self, free_names: HashSet<String>) {
        let mut map = std::collections::HashMap::new();
        for name in free_names {
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
        // A bare expression cannot contain this arrow's own `var` declaration;
        // block bodies retain the ordinary hoisting analysis.
        if let ast::ArrowBody::Block(b) = &a.body {
            names.extend(hoisted_var_names(&b.stmts));
        }
        let captured = capture::captured_locals_arrow(&names, &a.body);
        self.stash_arrow_child_with_shadows(&names, &a.body);
        let enclosing = self.child_enclosing();
        let mut proto =
            self.cx.compile_arrow_body(&params, rest.as_deref(), a, captured, enclosing, self.super_class, self.super_static, self.super_home_obj, self.derived_class, self.in_derived_ctor)?;
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
