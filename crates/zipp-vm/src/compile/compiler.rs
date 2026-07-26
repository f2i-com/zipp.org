// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl Compiler {
    pub(crate) fn new(source: String) -> Compiler {
        Compiler {
            functions: Vec::new(),
            globals: Vec::new(),
            global_index: rustc_hash::FxHashMap::default(),
            classes: Vec::new(),
            class_names: Vec::new(),
            hoisted_globals: Vec::new(),
            hoisted_set: rustc_hash::FxHashSet::default(),
            source,
            exact_src: None,
            eval_mode: false,
            eval_inherit_super: None,
            in_field_init: false,
            module_mode: false,
            eval_locals: false,
            script_binds_globals: true,
            eval_inherit_super_obj: false,
            eval_caller_scope: Vec::new(),
            eval_dynamic_names: std::collections::HashSet::new(),
            eval_fn_context: false,
            dyn_global_zone: false,
            force_strict: false,
            force_new_target_ok: false,
            in_strict: false,
            new_target_ok: false,
            class_enclosing: Vec::new(),
            class_derived: false,
            compiling_ctor: false,
            private_names_stack: Vec::new(),
            heritage_classes: Vec::new(),
            eval_visible_privates: std::collections::HashSet::new(),
            in_derived_ctor: false,
            const_globals: HashSet::new(),
            lexical_globals: HashSet::new(),
            decl_globals: HashSet::new(),
            module_exports: Vec::new(),
            module_has_imports: false,
            module_reexports: Vec::new(),
            module_star_reexports: Vec::new(),
            module_ns_reexports: Vec::new(),
            module_imports: Vec::new(),
            with_name_counter: 0,
            pending_with_shadows: std::collections::HashMap::new(),
            obj_method_super: false,
        }
    }

    /// Global slots a module's top level DECLARES (var/let/const/function/class +
    /// the synthetic `*default*`) — remapped to per-module fresh slots by the
    /// loader. Meaningful only for module compiles; harmless (unused) for scripts.
    pub(crate) fn collect_module_decl_globals(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.decl_globals.iter().copied().collect();
        v.extend(self.lexical_globals.iter().copied());
        v.extend(self.const_globals.iter().copied());
        v.extend(self.hoisted_globals.iter().copied());
        if let Some(i) = self.globals.iter().position(|n| n == "*default*") {
            v.push(i as u32);
        }
        // An INLINE `export const x` / `export function f` is an
        // ExportNamedDeclaration, so the bare-declaration hoisting pre-passes above
        // never registered its slot. Add every exported LOCAL's slot directly so a
        // module's exports always get fresh per-module slots (isolation).
        for (_, local) in &self.module_exports {
            if let Some(i) = self.globals.iter().position(|n| n == local) {
                v.push(i as u32);
            }
        }
        // Sorted for the same reason the emitted Program fields are: these come
        // from HashSets, and `RandomState` reseeds per INSTANCE, so the order
        // differs between two sets in one process, not merely between runs.
        v.sort_unstable();
        v
    }

    /// Slice the program source by a function node's byte span, for
    /// `Function.prototype.toString`. Empty if the range is degenerate or not on
    /// a UTF-8 boundary (then `toString` uses the native-function fallback).
    pub(crate) fn src_slice(&self, start: u32, end: u32) -> String {
        self.source
            .get(start as usize..end as usize)
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    pub(crate) fn global_slot(&mut self, name: &str) -> u16 {
        if let Some(&i) = self.global_index.get(name) {
            return i;
        }
        let i = self.globals.len() as u16;
        self.globals.push(name.to_string());
        self.global_index.insert(name.to_string(), i);
        i
    }

    pub(crate) fn compile(&mut self, prog: &ox::Program) -> R<()> {
        // Module code is always strict; a script is sloppy unless its directive
        // prologue says `"use strict"` (folded in by `compile_function_body`). A
        // direct eval from strict code forces strict for the whole eval program.
        self.in_strict = prog.source_type.is_module() || self.force_strict;
        // Reserve function id 0 for the top-level script body; fill it last so
        // nested function ids are stable as we discover them.
        self.functions.push(placeholder("<script>"));

        // Pass 1: hoist top-level function (and class) declaration names to globals.
        // Record their slots as non-configurable bindings (for `delete <name>`).
        // (A strict eval binds NOTHING globally — its declarations are locals.)
        let binds_globals = self.script_binds_globals;
        for s in prog.body.iter().filter(|_| binds_globals) {
            match s {
                ox::Statement::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        let slot = self.global_slot(id.name.as_str()) as u32;
                        self.decl_globals.insert(slot);
                    }
                }
                ox::Statement::ClassDeclaration(c) => {
                    if let Some(id) = &c.id {
                        let slot = self.global_slot(id.name.as_str()) as u32;
                        self.decl_globals.insert(slot);
                    }
                }
                _ => {}
            }
        }
        // Also hoist every top-level `var` name (recursing through blocks/loops/
        // try/switch but not into nested functions). These slots are pre-set to
        // `undefined` at startup so `x; var x;` reads `undefined` rather than
        // throwing the never-declared ReferenceError.
        if self.script_binds_globals {
            let mut vars = std::collections::HashSet::new();
            for s in &prog.body {
                collect_hoisted_vars(s, &mut vars);
            }
            for name in sorted_name_vec(&vars) {
                // A name the CALLER binds is not a global var of the eval —
                // the declaration is a no-op and assignments write the cell.
                // A dynamic (EvalScope) name is never a global either.
                if self.eval_caller_scope.iter().any(|n| *n == name)
                    || self.eval_dynamic_names.contains(&name)
                {
                    continue;
                }
                let slot = self.global_slot(&name) as u32;
                if self.hoisted_set.insert(slot) {
                    self.hoisted_globals.push(slot);
                }
            }
        }

        // Compile the top-level body as function 0. The script binds its
        // `let`/`var`/function declarations to GLOBALS, but for-of/for-in loop
        // variables and catch params are true locals — so it still needs a
        // captured set, or a closure over such a local can't box it.
        let captured = capture::captured_locals(&[], &prog.body);
        // A MODULE entry's top level is an async context (top-level `await`).
        let top_async = self.module_mode;
        let top = self.compile_function_body(
            None,
            None, // a script has no self-name binding
            &[],
            None,
            None,
            &prog.body,
            &prog.directives,
            true,
            false, // top-level script is not a generator
            top_async, // a module top level is async (top-level await)
            captured,
            Vec::new(),
        )?;
        self.functions[0] = top;
        Ok(())
    }

    /// Compile a function (or the script top-level when `is_script`).
    /// `params` are parameter names; `body` are its statements.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compile_function_body(
        &mut self,
        name: Option<&str>,
        self_name: Option<&str>,
        params: &[String],
        rest: Option<&str>,
        params_ast: Option<&ox::FormalParameters>,
        body: &[ox::Statement],
        directives: &[ox::Directive],
        is_script: bool,
        is_generator: bool,
        is_async: bool,
        captured: HashSet<String>,
        enclosing: Vec<EnclosingFn>,
    ) -> R<FuncProto> {
        let eval_completion = is_script && self.eval_mode;
        // A real function body leaves any enclosing field-initializer context
        // (the eval ROOT script keeps it: PerformEval's ContainsArguments check
        // spans the eval program's top level and its arrows).
        let saved_field_init = if is_script { self.in_field_init } else {
            std::mem::replace(&mut self.in_field_init, false)
        };
        // A (non-arrow) function body has its own `this` — leave any enclosing
        // derived-constructor TDZ context (arrows keep it).
        let saved_idc = std::mem::replace(&mut self.in_derived_ctor, false);
        // Strict if the enclosing scope is strict OR this body opens with a
        // `"use strict"` directive. Propagate it to `cx` for the duration of the
        // body so nested functions/arrows inherit it; restore the parent's after.
        let parent_strict = self.in_strict;
        let is_strict = parent_strict || has_use_strict(directives);
        // Strict-mode early errors on the FormalParameterList: a parameter may not
        // be named `eval`/`arguments`, and names may not repeat (a duplicate is a
        // SyntaxError in strict mode even for otherwise-simple parameter lists).
        if is_strict {
            if let Some(pa) = params_ast {
                let mut ordered = Vec::new();
                collect_param_names_ordered(pa, &mut ordered);
                let mut seen = HashSet::new();
                for nm in &ordered {
                    strict_name_err(true, nm)?;
                    if !seen.insert(nm.as_str()) {
                        return Err(format!(
                            "SyntaxError: duplicate parameter name '{nm}' not allowed in strict mode"
                        ));
                    }
                }
            }
        }
        // Early errors on a generator/async FormalParameterList: a generator's
        // parameters may not contain a YieldExpression and an async function's
        // may not contain an AwaitExpression (covers `function* g(x = yield)`
        // and the dynamic `GeneratorFunction('x = yield', '')` forms; oxc
        // parses these but defers the error to semantic analysis).
        if is_generator || is_async {
            if let Some(pa) = params_ast {
                let bad = pa.items.iter().any(|item| {
                    item.initializer
                        .as_ref()
                        .map_or(false, |init| expr_has_yield_or_await(init, is_generator, is_async))
                        || pattern_has_yield_or_await(&item.pattern, is_generator, is_async)
                }) || pa.rest.as_ref().map_or(false, |r| {
                    pattern_has_yield_or_await(&r.rest.argument, is_generator, is_async)
                });
                if bad {
                    return Err(
                        "SyntaxError: yield/await expression not permitted in formal parameters"
                            .into(),
                    );
                }
            }
        }
        // `new.target` is allowed inside an ordinary function, not at script/eval
        // top level; nested arrows inherit this. Restored at the end. A DIRECT eval
        // from inside a function/method/field initializer forces it on for the eval
        // script top level (the eval inherits the caller's new.target validity).
        let parent_nt = self.new_target_ok;
        self.new_target_ok = !is_script || self.force_new_target_ok;
        // A body that references `eval` may direct-eval: box EVERY param and
        // function-scoped local so the eval program can close over the caller
        // scope (cells outlive the frame for closures the eval creates).
        let mut captured = captured;
        let body_refs_eval = !is_script
            && (capture::free_vars(&[], body).contains("eval")
                || params_ast.is_some_and(|pa| capture::params_reference("eval", pa)));
        let saved_dyn_zone = self.dyn_global_zone;
        if body_refs_eval {
            self.dyn_global_zone = true;
        }
        if body_refs_eval {
            let mut all: Vec<String> = params.to_vec();
            if let Some(r) = rest {
                all.push(r.to_string());
            }
            all.extend(hoisted_var_names(body));
            // The named function expression's own name too: the eval program
            // must close over its (immutable) cell — `eval("fn = 1")` resolves
            // the funcEnv binding, not a fresh global.
            if let Some(sn) = self_name {
                all.push(sn.to_string());
            }
            captured.extend(all);
        }
        let mut fc = FnCompiler::new(self, params, rest, captured, enclosing);
        if body_refs_eval {
            fc.box_all_locals = true;
        }
        // An eval root with DYNAMIC (EvalScope) names compiles its global
        // accesses through the Dyn ops (same gate as eval-containing callers).
        if is_script && !fc.cx.eval_dynamic_names.is_empty() {
            fc.box_all_locals = true;
        }
        // A SCRIPT whose program references `eval`: box block-level lexicals
        // and record eval-site maps so a direct eval sees them (the script's
        // globals keep their fast paths — only true block locals are boxed).
        if is_script && capture::free_vars(&[], body).contains("eval") {
            fc.script_eval_lexicals = true;
        }
        // A FUNCTION-context eval root compiles its global accesses through
        // the Dyn ops — the caller activation's EvalScope (from this or an
        // earlier eval) may bind any name.
        if is_script && fc.cx.eval_fn_context {
            fc.box_all_locals = true;
        }
        // The eval ROOT closes over the caller bindings: seed them as upvalues
        // (the runtime hands the cells to the manually-built eval closure).
        if is_script && !fc.cx.eval_caller_scope.is_empty() {
            let seed = fc.cx.eval_caller_scope.clone();
            let mut ups = fc.upvalues.borrow_mut();
            for n in &seed {
                ups.push((n.clone(), UpvalSource::ParentLocal(0)));
            }
        }
        fc.cx.in_strict = is_strict;
        fc.is_script = is_script;
        fc.in_generator = is_generator;
        fc.in_async = is_async;
        // A direct eval from a class member: the top-level eval script carries
        // the caller's home class as the u32::MAX sentinel (arrows inherit it;
        // plain nested functions reset super_class as usual).
        if is_script {
            if let Some(stat) = fc.cx.eval_inherit_super {
                fc.super_class = Some(u32::MAX);
                fc.super_static = stat;
            }
        }
        // An object-literal concise method / accessor compiles with object-method
        // super (set transiently by the object-literal compiler just before this).
        fc.super_home_obj = std::mem::take(&mut fc.cx.obj_method_super);
        // Direct eval inside an object-literal method: `super.x` in the eval
        // resolves via the caller's runtime [[HomeObject]] (closure_home is
        // stamped on the eval script value). After the take above, which is
        // always false for an eval script.
        if is_script && fc.cx.eval_inherit_super_obj {
            fc.super_home_obj = true;
        }
        // eval completion-value accumulator: a low, never-reclaimed register
        // (allocated right after `this`/params, below every statement's
        // save/restore high-water) seeded to `undefined`.
        if eval_completion {
            let cr = fc.alloc_reg();
            fc.emit(Instr::LoadUndefined { dst: cr });
            fc.completion_reg = Some(cr);
            // A directive prologue is a string-literal expression statement whose
            // value is a completion value (`eval("'use strict'")` === "use
            // strict"). Directives run before the body, so seed the completion with
            // each (the last wins) — body expression statements then overwrite it.
            for d in directives {
                // A directive's literal may carry lone surrogates
                // (`eval("'\uD800'")` completes with the 1-unit string).
                let idx = if d.expression.lone_surrogates {
                    fc.add_string_const_wtf8(d.expression.value.as_str())
                } else {
                    fc.add_string_const(d.expression.value.as_str())
                };
                fc.emit(Instr::LoadConst { dst: cr, idx });
            }
        }
        if !is_script {
            fc.reserve_arguments(); // non-arrow functions bind `arguments`
            // A nested arrow that reads `arguments` captures THIS function's
            // arguments object lexically: materialize it (uses_arguments) and box
            // its register into a cell so the arrow grabs the live cell as an upvalue.
            // (No-op when a formal named `arguments` suppressed the binding — the
            // arrow then captures the PARAMETER like any other name.)
            if capture::nested_uses_arguments(body) {
                if let Some(r) = fc.arguments_reg {
                    fc.uses_arguments = true;
                    fc.emit(Instr::MakeCell { reg: r });
                    fc.cell_regs.insert(r);
                }
            }
        }
        // A named function expression binds its own name to itself inside the body
        // (`(function f(){ … f … })`). Reserve a register AFTER the params/rest/
        // arguments slots (so the call ABI's fixed param layout is untouched), load
        // the running function value into it (LoadCallee), and — if a nested closure
        // captures the name — box it into a cell like a captured parameter. Only set
        // up when the name is actually referenced and not shadowed by a param/local.
        if let Some(sn) = self_name {
            // A body containing a direct eval may reference the name only
            // inside the eval STRING (invisible to free_vars): bind it anyway
            // so the eval program can close over it.
            if capture::free_vars(params, body).contains(sn) || body_refs_eval {
                let r = fc.alloc_reg();
                fc.emit(Instr::LoadCallee { dst: r });
                if fc.captured.contains(sn) {
                    // The self-name binding is IMMUTABLE: the cell is tagged
                    // so a nested closure's / eval's write is a sloppy no-op
                    // or a strict TypeError (the body's own writes are
                    // intercepted at compile time in store_binding).
                    fc.emit(Instr::MakeCellFnName { reg: r });
                    fc.cell_regs.insert(r);
                }
                fc.self_name = Some((sn.to_string(), r));
            }
        }

        // Apply default parameter values (`function f(x = expr)`) before the body:
        // for each defaulted param, `if (x === undefined) x = expr`.
        if let Some(pa) = params_ast {
            fc.bind_params(pa)?;
        }
        // A generator (sync OR async) runs its parameter prologue eagerly at call
        // time and is then created suspended here; mark the body entry.
        if is_generator {
            fc.emit(Instr::GenStart);
        }

        // Hoist function declarations in this body so calls resolve before the
        // textual definition. Top-level names become globals (the VM
        // materialises the function object at startup via `name_global`).
        // Nested names become locals, populated by a `MakeFunc` at the point
        // `func_decl` reaches them.
        for s in body {
            if let ox::Statement::FunctionDeclaration(f) = s {
                if let Some(id) = &f.id {
                    if is_script && fc.cx.script_binds_globals {
                        fc.cx.global_slot(id.name.as_str());
                    } else {
                        fc.declare_local(id.name.as_str());
                    }
                }
            }
        }

        // Annex B B.3.3: in a SLOPPY (non-script) function body, a `function`
        // declared inside a block also gets a function-scoped `var` binding,
        // initialized to undefined here and assigned the function value when the
        // block declaration executes (see func_decl). Names that would be an early
        // error (a formal parameter, or a lexical `let`/`const`/`class` in scope —
        // top-level OR an enclosing block/for-head/catch param) are skipped, as is
        // a top-level function name (already var-scoped).
        if !fc.cx.in_strict && (!is_script || fc.cx.script_binds_globals) {
            let mut blockers = std::collections::HashSet::new();
            // `protect`: params + lexical/class names a same-named block function
            // must NOT touch (B.3.3 skip). Existing FUNCTION names are blockers (no
            // NEW b33 var) but are NOT protected — a block function updates them.
            let mut protect = std::collections::HashSet::new();
            for p in params {
                blockers.insert(p.clone());
                protect.insert(p.clone());
            }
            // B.3.3.1: a block function named `arguments` is never promoted —
            // the implicit arguments binding behaves like a formal parameter
            // (block-decl-func-skip-arguments). Script top level has no
            // `arguments` binding, so the skip applies to function bodies only.
            if !is_script {
                blockers.insert("arguments".to_string());
                protect.insert("arguments".to_string());
            }
            for s in body {
                match s {
                    ox::Statement::VariableDeclaration(d) if d.kind.is_lexical() => {
                        for decl in &d.declarations {
                            capture::collect_pattern_names(&decl.id, &mut blockers);
                            capture::collect_pattern_names(&decl.id, &mut protect);
                        }
                    }
                    ox::Statement::ClassDeclaration(c) => {
                        if let Some(id) = &c.id {
                            blockers.insert(id.name.to_string());
                            protect.insert(id.name.to_string());
                        }
                    }
                    ox::Statement::FunctionDeclaration(f) => {
                        if let Some(id) = &f.id {
                            blockers.insert(id.name.to_string());
                        }
                    }
                    _ => {}
                }
            }
            fc.protect_names = protect;
            let mut b33 = std::collections::HashSet::new();
            if is_script {
                // SCRIPT level: an existing top-level FUNCTION name is NOT a
                // blocker — the block function UPDATES its global binding at
                // declaration evaluation (B.3.3.3 SetMutableBinding; the
                // binding already exists via name_global materialization).
                let mut script_blockers = blockers.clone();
                let mut fn_names = std::collections::HashSet::new();
                for s in body {
                    if let ox::Statement::FunctionDeclaration(f) = s {
                        if let Some(id) = &f.id {
                            script_blockers.remove(id.name.as_str());
                            fn_names.insert(id.name.to_string());
                        }
                    }
                }
                for s in body {
                    collect_b33_block_fns(s, false, &script_blockers, &mut b33);
                }
                for name in &b33 {
                    // CreateGlobalVarBinding(undefined) at instantiation —
                    // rides the hoisted-globals machinery (startup seed for
                    // main scripts; the own-prop step for global-varEnv
                    // evals). The function itself stays BLOCK-local;
                    // func_decl copies the value out when the declaration
                    // evaluates. An existing-function name keeps its binding.
                    // A sloppy fn-context eval binds these in the CALLER's
                    // EvalScope (or updates a caller binding) instead — no
                    // realm global is created.
                    if fc.cx.eval_dynamic_names.contains(name)
                        || fc.cx.eval_caller_scope.iter().any(|n| n == name)
                    {
                        continue;
                    }
                    if !fn_names.contains(name) {
                        let slot = fc.cx.global_slot(name) as u32;
                        if fc.cx.hoisted_set.insert(slot) {
                            fc.cx.hoisted_globals.push(slot);
                        }
                    }
                }
            } else {
                for s in body {
                    collect_b33_block_fns(s, false, &blockers, &mut b33);
                }
                for name in &b33 {
                    // CreateMutableBinding + InitializeBinding(undefined) at entry.
                    let reg = fc.declare_local(name);
                    if fc.cell_regs.contains(&reg) {
                        let t = fc.temp();
                        fc.emit(Instr::LoadUndefined { dst: t });
                        fc.emit(Instr::CellSet { cell: reg, src: t });
                        fc.next_reg -= 1;
                    } else {
                        fc.emit(Instr::LoadUndefined { dst: reg });
                    }
                }
            }
            fc.b33_names = b33;
        }

        // Hoist top-level lexical (`let`/`const`) names into `lexical_globals` so an
        // assignment to one BEFORE its declaration runs (its TDZ) is a ReferenceError
        // even in sloppy mode — `for ([x] of [[]]) {} let x;`. Only DIRECT top-level
        // declarations bind to globals (block-scoped lexicals don't leak), so a
        // VariableDeclaration nested in a block is not registered.
        // MODULE import pre-pass: bindings created by `import` declarations
        // HOIST (exist before any statement runs), are immutable, and module-
        // scoped. `export … from` specifiers are recorded as SideEffect loads
        // so dependencies evaluate in SOURCE order.
        // (Modules reach here through TWO pipelines: compile_module for the
        // entry — module_mode — and compile_eval(is_module) from the engine's
        // loader — eval_mode with module-style globals.)
        if is_script && (fc.cx.module_mode || (fc.cx.eval_mode && !fc.cx.eval_locals)) {
            use crate::bytecode::{ImportEntry, ImportName};
            for s in body {
                match s {
                    ox::Statement::ImportDeclaration(d) => {
                        // `import defer * as ns` binds the module's DEFERRED
                        // namespace; `import source x` binds the target's
                        // ModuleSource object; a bindingless phase form stays
                        // load-only.
                        if !matches!(d.phase, None) {
                            let defer_ns_local = if matches!(d.phase, Some(ox::ImportPhase::Defer))
                            {
                                d.specifiers.as_ref().and_then(|specs| {
                                    specs.iter().find_map(|sp| match sp {
                                        ox::ImportDeclarationSpecifier::ImportNamespaceSpecifier(
                                            i,
                                        ) => Some(i.local.name.to_string()),
                                        _ => None,
                                    })
                                })
                            } else {
                                None
                            };
                            // `import source x from '…'` parses as phase Source
                            // with a default-shaped binding specifier.
                            let source_local = if matches!(d.phase, Some(ox::ImportPhase::Source))
                            {
                                d.specifiers.as_ref().and_then(|specs| {
                                    specs.iter().find_map(|sp| match sp {
                                        ox::ImportDeclarationSpecifier::ImportDefaultSpecifier(
                                            i,
                                        ) => Some(i.local.name.to_string()),
                                        _ => None,
                                    })
                                })
                            } else {
                                None
                            };
                            if let Some(local) = source_local {
                                let slot = fc.cx.global_slot(&local) as u32;
                                fc.cx.decl_globals.insert(slot);
                                fc.cx.const_globals.insert(slot);
                                fc.cx.module_imports.push(ImportEntry {
                                    local_slot: slot,
                                    import: ImportName::Source,
                                    specifier: d.source.value.to_string(),
                                    mtype: with_clause_type(&d.with_clause),
                                });
                            } else if let Some(local) = defer_ns_local {
                                let slot = fc.cx.global_slot(&local) as u32;
                                fc.cx.decl_globals.insert(slot);
                                fc.cx.const_globals.insert(slot);
                                fc.cx.module_imports.push(ImportEntry {
                                    local_slot: slot,
                                    import: ImportName::DeferNamespace,
                                    specifier: d.source.value.to_string(),
                                    mtype: with_clause_type(&d.with_clause),
                                });
                            } else {
                                fc.cx.module_imports.push(ImportEntry {
                                    local_slot: u32::MAX,
                                    import: ImportName::LoadOnly,
                                    specifier: d.source.value.to_string(),
                                    mtype: with_clause_type(&d.with_clause),
                                });
                            }
                            continue;
                        }
                        let spec = d.source.value.to_string();
                        match &d.specifiers {
                            Some(specs) if !specs.is_empty() => {
                                for sp in specs {
                                    use ox::ImportDeclarationSpecifier as IS;
                                    let (local, import) = match sp {
                                        IS::ImportSpecifier(i) => (
                                            i.local.name.to_string(),
                                            ImportName::Named(module_export_name(&i.imported)),
                                        ),
                                        IS::ImportDefaultSpecifier(i) => {
                                            (i.local.name.to_string(), ImportName::Default)
                                        }
                                        IS::ImportNamespaceSpecifier(i) => {
                                            (i.local.name.to_string(), ImportName::Namespace)
                                        }
                                    };
                                    let slot = fc.cx.global_slot(&local) as u32;
                                    fc.cx.decl_globals.insert(slot);
                                    fc.cx.const_globals.insert(slot);
                                    fc.cx.module_imports.push(ImportEntry {
                                        local_slot: slot,
                                        import,
                                        specifier: spec.clone(),
                                        mtype: with_clause_type(&d.with_clause),
                                    });
                                }
                            }
                            _ => {
                                fc.cx.module_imports.push(ImportEntry {
                                    local_slot: u32::MAX,
                                    import: ImportName::SideEffect,
                                    specifier: spec.clone(),
                                    mtype: with_clause_type(&d.with_clause),
                                });
                            }
                        }
                    }
                    ox::Statement::ExportNamedDeclaration(e) => {
                        if let Some(srcspec) = &e.source {
                            fc.cx.module_imports.push(ImportEntry {
                                local_slot: u32::MAX,
                                import: ImportName::SideEffect,
                                specifier: srcspec.value.to_string(),
                                mtype: with_clause_type(&e.with_clause),
                            });
                        }
                    }
                    ox::Statement::ExportAllDeclaration(e) => {
                        fc.cx.module_imports.push(ImportEntry {
                            local_slot: u32::MAX,
                            import: ImportName::SideEffect,
                            specifier: e.source.value.to_string(),
                            mtype: with_clause_type(&e.with_clause),
                        });
                    }
                    _ => {}
                }
            }
        }
        if is_script && !fc.cx.eval_locals {
            // (An `export let x` / `export class C` wraps the declaration in an
            // ExportNamedDeclaration — same lexical binding, same TDZ.)
            for s in body {
                let (var_decl, class_decl) = match s {
                    ox::Statement::VariableDeclaration(d) => (Some(d), None),
                    ox::Statement::ClassDeclaration(c) => (None, Some(c)),
                    ox::Statement::ExportNamedDeclaration(e) => match &e.declaration {
                        Some(ox::Declaration::VariableDeclaration(d)) => (Some(d), None),
                        Some(ox::Declaration::ClassDeclaration(c)) => (None, Some(c)),
                        _ => (None, None),
                    },
                    _ => (None, None),
                };
                if let Some(d) = var_decl {
                    if d.kind.is_lexical() {
                        for decl in &d.declarations {
                            if let ox::BindingPattern::BindingIdentifier(id) = &decl.id {
                                // GlobalDeclarationInstantiation step 5: a top-level
                                // lexical name colliding with a RESTRICTED global
                                // property (the non-configurable value properties
                                // undefined/NaN/Infinity) is a SyntaxError.
                                if matches!(id.name.as_str(), "undefined" | "NaN" | "Infinity") {
                                    return Err(format!(
                                        "SyntaxError: lexical declaration of '{}' collides with a restricted global property",
                                        id.name
                                    ));
                                }
                                let slot = fc.cx.global_slot(id.name.as_str()) as u32;
                                fc.cx.lexical_globals.insert(slot);
                            }
                        }
                    }
                }
                // A top-level CLASS declaration is a lexical binding with the
                // same TDZ (typeof C before it runs is a ReferenceError).
                if let Some(c) = class_decl {
                    if let Some(id) = &c.id {
                        let slot = fc.cx.global_slot(id.name.as_str()) as u32;
                        fc.cx.lexical_globals.insert(slot);
                    }
                }
            }
        }
        // EVAL top level: lexical declarations live in the eval's own
        // (discarded-on-return) lexical environment, never the realm's globals
        // (spec PerformEval NewDeclarativeEnvironment). Pre-create a TDZ cell
        // for EVERY top-level lexical name — EvalDeclarationInstantiation
        // creates them uninitialized before the body runs, so
        // `typeof x; let x;` throws ReferenceError. The textual declaration
        // reuses the cell (entry_lexicals), ending the TDZ.
        if is_script && fc.cx.eval_locals {
            let mut lex = std::collections::HashSet::new();
            for s in body {
                match s {
                    ox::Statement::VariableDeclaration(d) if d.kind.is_lexical() => {
                        for decl in &d.declarations {
                            capture::collect_pattern_names(&decl.id, &mut lex);
                        }
                    }
                    ox::Statement::ClassDeclaration(c) => {
                        if let Some(id) = &c.id {
                            lex.insert(id.name.to_string());
                        }
                    }
                    _ => {}
                }
            }
            // Sorted: alloc_reg() below, so raw HashSet order would permute cell registers.
            for name in &crate::compile::helpers::sorted_name_vec(&lex) {
                if !fc.scopes[0].iter().any(|(n, _)| n == name) {
                    let r = fc.alloc_reg();
                    fc.scopes[0].push((name.clone(), r));
                    fc.emit(Instr::MakeCellTdz { reg: r });
                    fc.cell_regs.insert(r);
                    fc.entry_lexicals.insert(name.clone());
                }
            }
        }

        // Pre-declare every captured function-scope `var` so a function or closure
        // that captures a sibling `var` declared LATER finds its (undefined) cell at
        // creation time rather than failing to resolve it (`var g=function(){return
        // v}; var v=5; g()` ⇒ 5). `var_decl` reuses the binding. The register window
        // starts undefined, so the boxed cell holds undefined until the assignment.
        // (Lexical `let`/`const` are NOT pre-created — they have a TDZ and are bound
        // at their textual declaration.)
        if !is_script || !fc.cx.script_binds_globals {
            let mut hv = std::collections::HashSet::new();
            for s in body {
                collect_hoisted_vars(s, &mut hv);
            }
            // Pre-declare EVERY hoisted var at entry (not just captured ones):
            // (1) a `var` declared inside a BLOCK must occupy a register BELOW
            // the block scope's, or the block's pop reclaims register space
            // while the function-scoped binding still references it (a later
            // local then aliases it: `{ var x = 2 } var z = 3` clobbered x);
            // (2) the with-chain's static fallback must resolve a `var`
            // textually after the `with` (unscopables-with); (3) `for (var k
            // in …)` heads resolve the hoisted binding instead of minting one.
            for name in &sorted_name_vec(&hv) {
                if !fc.scopes[0].iter().any(|(n, _)| n == name) {
                    fc.declare_local(name); // boxes a cell if captured (undefined)
                }
            }
        }

        // Pre-create cells for captured function-body-level lexical (`let`/`const`/
        // `class`) bindings so a function materialised at entry (a forward
        // reference) can capture them. Only DIRECT top-level lexicals are
        // function-body-scoped (block-nested ones are block-local), so this scans
        // the body's own statements, not recursively. The textual declaration
        // REUSES the cell (var_decl / class_decl). Pre-creating an (undefined) cell
        // does not weaken TDZ — zipp does not runtime-enforce a function-body
        // lexical's TDZ today, and only CAPTURED names are touched.
        if !is_script {
            let mut lex = std::collections::HashSet::new();
            for s in body {
                match s {
                    ox::Statement::VariableDeclaration(d) if d.kind.is_lexical() => {
                        for decl in &d.declarations {
                            capture::collect_pattern_names(&decl.id, &mut lex);
                        }
                    }
                    ox::Statement::ClassDeclaration(c) => {
                        if let Some(id) = &c.id {
                            lex.insert(id.name.to_string());
                        }
                    }
                    _ => {}
                }
            }
            // Sorted: this loop calls alloc_reg(), so raw HashSet order would
            // hand out CELL REGISTERS in a different order per compile.
            for name in &crate::compile::helpers::sorted_name_vec(&lex) {
                if fc.captured.contains(name) && !fc.scopes[0].iter().any(|(n, _)| n == name) {
                    // Box a TDZ cell: a read before the textual declaration runs
                    // (e.g. via a forward-materialised function) throws a
                    // ReferenceError rather than reading undefined.
                    let r = fc.alloc_reg();
                    fc.scopes[0].push((name.clone(), r));
                    fc.emit(Instr::MakeCellTdz { reg: r });
                    fc.cell_regs.insert(r);
                    fc.entry_lexicals.insert(name.clone());
                    // `const`-ness is recorded by the textual declaration (which
                    // reuses this reg), so an assignment after it still TypeErrors.
                }
            }
        }

        // Materialise top-level function declarations at entry so a forward call or
        // reference (`f(); function f(){}`, `var g = f; function f(){}`) resolves to
        // the live function object rather than an undefined hoist slot. Sibling
        // functions, captured vars, and captured lexicals are all bound above, so
        // each function's captures resolve. They are skipped in the statement loop
        // below (a function declaration has no textual side effects). Script-level
        // functions are materialised at VM startup, so this applies to nested
        // function bodies only.
        // An EVAL program ALSO initializes its top-level function declarations at
        // entry (EvalDeclarationInstantiation step 15 runs functionsToInitialize
        // before the body): `eval('initial = f; function f(){}')` reads the
        // function, not the caller's prior var value.
        let entry_fns = !is_script || !fc.cx.script_binds_globals || fc.cx.eval_mode;
        if entry_fns {
            for s in body {
                if let ox::Statement::FunctionDeclaration(f) = s {
                    fc.func_decl(f)?;
                }
            }
        }

        if FnCompiler::block_has_using(body) {
            // A function/generator/async body with a top-level `using` disposes its
            // resources on return/throw — same finally desugar as a block.
            fc.compile_using_block(body, entry_fns)?;
        } else {
            for s in body {
                // Top-level function declarations were materialised at entry above.
                if entry_fns {
                    if let ox::Statement::FunctionDeclaration(_) = s {
                        continue;
                    }
                }
                fc.stmt(s)?;
            }
        }
        fc.cx.in_strict = parent_strict; // restore: nested compiles are done
        fc.cx.new_target_ok = parent_nt;
        // An eval script returns its accumulated completion value; everything else
        // returns undefined.
        if let Some(cr) = fc.completion_reg {
            fc.emit(Instr::Return { src: cr });
        } else {
            fc.emit(Instr::ReturnUndefined);
        }

        let upvalues: Vec<UpvalSource> =
            fc.upvalues.borrow().iter().map(|(_, s)| *s).collect();
        fc.cx.in_field_init = saved_field_init;
        fc.cx.in_derived_ctor = saved_idc;
        fc.cx.dyn_global_zone = saved_dyn_zone;
        fc.check_regs()?;
        Ok(FuncProto {
            name: name.unwrap_or("<script>").to_string(),
            code: fc.code,
            reg_count: fc.max_reg,
            param_count: params.len() as u16,
            length: params_ast.map(expected_arg_count).unwrap_or(params.len() as u16),
            rest_reg: fc.rest_reg,
            arguments_reg: if fc.uses_arguments { fc.arguments_reg } else { None },
            is_generator,
            is_async,
            non_constructable: false, // a plain function/expression IS constructable
            lexical_this: false,
            super_static: false, // a plain function is not a static class element
            is_strict,
            simple_params: params_ast.map(params_are_simple).unwrap_or(false),
            constants: fc.constants,
            string_constants: fc.string_constants,
            bigint_consts: fc.bigint_consts,
            wtf8_consts: fc.wtf8_consts,
            name_global: None, // set by the caller for top-level declarations
            upvalues,
            eval_sites: std::mem::take(&mut fc.eval_sites),
            source: String::new(), // set by the caller from the function node's span
        })
    }

    /// Compile a class method or constructor. Like `compile_function_body` but
    /// (a) it closes over `class_enclosing` (the function containing the class), so
    /// a free var resolves to an upvalue, not a global — empty at script level, and
    /// (b) it first emits instance-field initializers `this.field = expr` (only
    /// for the constructor; `fields` is empty for plain methods). `this` is reg 0.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compile_class_fn(
        &mut self,
        name: &str,
        params: &[String],
        rest: Option<&str>,
        params_ast: Option<&ox::FormalParameters>,
        fields: &[(String, Option<&ox::Expression>)],
        computed_inits: &[Option<&ox::Expression>],
        body: &[ox::Statement],
        super_class: Option<u32>,
        super_static: bool,
        is_generator: bool,
        is_async: bool,
    ) -> R<FuncProto> {
        // A nested closure in the method body may capture the method's own
        // params/locals — they must be boxed. (Class methods don't close over an
        // enclosing function in this subset, so the enclosing chain stays empty.)
        let mut names: Vec<String> = params.to_vec();
        if let Some(r) = rest {
            names.push(r.to_string());
        }
        if let Some(pa) = params_ast {
            names.extend(param_pattern_leaves(pa));
        }
        names.extend(hoisted_var_names(body)); // function-scoped `var`s (capture)
        let mut captured = capture::captured_locals(&names, body);
        let cls_body_refs_eval = capture::free_vars(&[], body).contains("eval")
            || params_ast.is_some_and(|pa| capture::params_reference("eval", pa));
        let saved_dyn_zone_cls = self.dyn_global_zone;
        if cls_body_refs_eval {
            self.dyn_global_zone = true;
        }
        if cls_body_refs_eval {
            captured.extend(names.iter().cloned());
        }
        // Class bodies are always strict, regardless of the enclosing scope.
        let parent_strict = self.in_strict;
        // A class method/ctor/field-init closes over the function that contains
        // the class (its enclosing chain, stashed by compile_class), so a free var
        // resolves to an upvalue (not a global). `MakeClass` builds the per-method
        // closures at runtime. Empty at script level → free vars stay globals.
        let enclosing = self.class_enclosing.clone();
        // `new.target` is allowed in a class method/ctor/field-init body.
        let parent_nt = self.new_target_ok;
        self.new_target_ok = true;
        let saved_field_init = std::mem::replace(&mut self.in_field_init, false);
        // Consumed flag: set by the ctor call site just before this call. Only a
        // DERIVED class's constructor body (and arrows inside it) checks the
        // this-TDZ on `this` reads / super-property references.
        let is_ctor = std::mem::replace(&mut self.compiling_ctor, false);
        let saved_idc = self.in_derived_ctor;
        self.in_derived_ctor = is_ctor && self.class_derived;
        let mut fc = FnCompiler::new(self, params, rest, captured, enclosing);
        if cls_body_refs_eval {
            fc.box_all_locals = true;
        }
        fc.in_derived_ctor = fc.cx.in_derived_ctor;
        fc.cx.in_strict = true;
        fc.super_class = super_class;
        fc.super_static = super_static;
        fc.derived_class = fc.cx.class_derived;
        fc.in_generator = is_generator;
        fc.in_async = is_async;
        fc.reserve_arguments(); // class methods/ctors bind `arguments`
        // A nested arrow reading `arguments` captures this method's arguments
        // object lexically — materialize + box it (see compile_function_body).
        if capture::nested_uses_arguments(body) {
            if let Some(r) = fc.arguments_reg {
                fc.uses_arguments = true;
                fc.emit(Instr::MakeCell { reg: r });
                fc.cell_regs.insert(r);
            }
        }
        if let Some(pa) = params_ast {
            fc.bind_params(pa)?;
        }
        // A generator method (sync OR async) runs its parameter prologue eagerly at
        // call and is created suspended here (a constructor is never a generator, so
        // no field initializers precede the body for this case).
        if is_generator {
            fc.emit(Instr::GenStart);
        }
        // Instance field initializers: `this.field = expr` (this = reg 0).
        // `arguments` is an early SyntaxError inside an initializer (and in any
        // direct eval / arrow it contains).
        for (fname, finit) in fields {
            let save = fc.next_reg;
            let v = match finit {
                Some(e) => {
                    fc.cx.in_field_init = true;
                    let r = fc.expr(e);
                    fc.cx.in_field_init = false;
                    r?
                }
                None => {
                    let t = fc.temp();
                    fc.emit(Instr::LoadUndefined { dst: t });
                    t
                }
            };
            // NamedEvaluation for anonymous initializers (incl. "#field" names).
            if matches!(finit, Some(e) if is_anonymous_fn_def(e)) {
                let kr = fc.temp();
                let idx = fc.add_string_const(fname);
                fc.emit(Instr::LoadConst { dst: kr, idx });
                fc.emit(Instr::SetFnNameFromKey { func: v, key: kr, prefix: 0 });
            }
            let name_idx = fc.string_name(fname);
            // DefineField (CreateDataPropertyOrThrow) for PUBLIC fields — never
            // a [[Set]] (an inherited setter must not run; a Proxy receiver's
            // defineProperty trap must). Private "#fields" keep the plain store
            // (private semantics bypass proxies entirely).
            if fname.starts_with('#') {
                fc.emit(Instr::SetProp { obj: 0, name: name_idx, val: v });
            } else {
                fc.emit(Instr::DefineField { obj: 0, name: name_idx, val: v });
            }
            fc.next_reg = save;
        }
        // Computed instance fields (`[k] = v`): the key was evaluated at class
        // definition and stored on the class; init the i-th here as `this[key]=v`.
        for (i, finit) in computed_inits.iter().enumerate() {
            let save = fc.next_reg;
            let v = match finit {
                Some(e) => fc.expr(e)?,
                None => {
                    let t = fc.temp();
                    fc.emit(Instr::LoadUndefined { dst: t });
                    t
                }
            };
            fc.emit(Instr::FieldInit {
                key_index: i as u16,
                val: v,
                class_id: fc.super_class.unwrap_or(u32::MAX),
            });
            fc.next_reg = save;
        }
        for s in body {
            if let ox::Statement::FunctionDeclaration(f) = s {
                if let Some(id) = &f.id {
                    fc.declare_local(id.name.as_str());
                }
            }
        }
        for s in body {
            fc.stmt(s)?;
        }
        fc.cx.in_strict = parent_strict; // restore after the (strict) class body
        fc.cx.new_target_ok = parent_nt;
        fc.emit(Instr::ReturnUndefined);
        let upvalues: Vec<UpvalSource> = fc.upvalues.borrow().iter().map(|(_, s)| *s).collect();
        fc.cx.in_field_init = saved_field_init;
        fc.cx.in_derived_ctor = saved_idc;
        fc.cx.dyn_global_zone = saved_dyn_zone_cls;
        fc.check_regs()?;
        Ok(FuncProto {
            name: name.to_string(),
            code: fc.code,
            reg_count: fc.max_reg,
            param_count: params.len() as u16,
            length: params_ast.map(expected_arg_count).unwrap_or(params.len() as u16),
            rest_reg: fc.rest_reg,
            arguments_reg: if fc.uses_arguments { fc.arguments_reg } else { None },
            is_generator,
            is_async,
            // A class method/getter/setter is non-constructable. The class
            // CONSTRUCTOR is also compiled here, but it is reached only via the
            // HeapObj::Class [[Construct]] path (never as a raw Func), so this flag
            // is never consulted for it — safe to set uniformly.
            non_constructable: true,
            lexical_this: false, // a concise method gets its own `this`, not lexical
            super_static, // true for static methods/getters/setters/blocks
            is_strict: true,
            simple_params: false, // strict (class body) — never mapped anyway
            constants: fc.constants,
            string_constants: fc.string_constants,
            bigint_consts: fc.bigint_consts,
            wtf8_consts: fc.wtf8_consts,
            name_global: None,
            upvalues,
            eval_sites: std::mem::take(&mut fc.eval_sites),
            source: String::new(), // class methods: caller may override from span
        })
    }

    /// Compile an arrow function body (expression- or block-bodied).
    pub(crate) fn compile_arrow_body(
        &mut self,
        params: &[String],
        rest: Option<&str>,
        a: &ox::ArrowFunctionExpression,
        captured: HashSet<String>,
        enclosing: Vec<EnclosingFn>,
        super_class: Option<u32>,
        super_static: bool,
        super_home_obj: bool,
    ) -> R<FuncProto> {
        let parent_strict = self.in_strict;
        let is_strict = parent_strict || has_use_strict(&a.body.directives);
        // An arrow that references `eval` (incl. in its parameter defaults)
        // boxes its locals and records DirectEval sites like a function.
        let mut captured = captured;
        let arrow_refs_eval = capture::free_vars(&[], &a.body.statements).contains("eval")
            || capture::params_reference("eval", &a.params);
        if arrow_refs_eval {
            let mut all: Vec<String> = params.to_vec();
            if let Some(r) = rest {
                all.push(r.to_string());
            }
            captured.extend(all);
        }
        let saved_dyn_zone_arrow = self.dyn_global_zone;
        if arrow_refs_eval {
            self.dyn_global_zone = true;
        }
        let mut fc = FnCompiler::new(self, params, rest, captured, enclosing);
        if arrow_refs_eval {
            fc.box_all_locals = true;
        }
        fc.cx.in_strict = is_strict;
        // An arrow has no `super` binding of its own: `super.x` / `super.m()` inside
        // it resolves LEXICALLY to the enclosing non-arrow method's home class. The
        // runtime resolves super via the home-class id + the lexical `this` (which
        // arrows already capture), so propagating the enclosing method's compile-time
        // home-class id is sufficient.
        fc.super_class = super_class;
        // An arrow inside an OBJECT method inherits its object-method super context, so
        // `super.x` in the arrow resolves via the runtime [[HomeObject]] (which the
        // arrow's MakeArrow copies from the enclosing method's closure).
        fc.super_home_obj = super_home_obj;
        // An arrow inherits the enclosing method's static-ness (so `super.x` in an
        // arrow inside a static method resolves against the parent CLASS).
        fc.super_static = super_static;
        // An arrow inherits the enclosing method's derived-ness (so `super(...)` in an
        // arrow inside a derived constructor is allowed). `cx.class_derived` still
        // reflects the enclosing class while its method bodies (and their arrows) compile.
        fc.derived_class = fc.cx.class_derived;
        fc.in_derived_ctor = fc.cx.in_derived_ctor;
        fc.in_async = a.r#async;
        fc.bind_params(&a.params)?;
        if a.expression {
            // `x => expr`: the body is a single ExpressionStatement to return.
            let mut returned = false;
            for s in &a.body.statements {
                if let ox::Statement::ExpressionStatement(es) = s {
                    let r = fc.expr(&es.expression)?;
                    fc.emit(Instr::Return { src: r });
                    returned = true;
                }
            }
            if !returned {
                fc.emit(Instr::ReturnUndefined);
            }
        } else {
            // hoist nested function declarations (same as a normal body)
            for s in &a.body.statements {
                if let ox::Statement::FunctionDeclaration(f) = s {
                    if let Some(id) = &f.id {
                        fc.declare_local(id.name.as_str());
                    }
                }
            }
            // Pre-declare every hoisted var at entry (mirrors the
            // function-body pass — register accounting + with-fallback +
            // for-head resolution; see that pass's comment).
            {
                let mut hv = std::collections::HashSet::new();
                for s in &a.body.statements {
                    collect_hoisted_vars(s, &mut hv);
                }
                for name in &sorted_name_vec(&hv) {
                    if !fc.scopes[0].iter().any(|(n, _)| n == name) {
                        fc.declare_local(name);
                    }
                }
            }
            // Pre-create cells for body-level lexical (`let`/`const`/`class`)
            // bindings, exactly as a function body does — an arrow body is a
            // scope like any other, and a nested function declaration hoisted
            // above the declaration must be able to capture its cell.
            //
            // Without this the forward reference finds no binding and compiles
            // to a GLOBAL load, so it fails at runtime with "x is not defined"
            // (not the TDZ error, which is what a real forward-read reports).
            // Webpack wraps whole bundles in `(() => { "use strict"; … })()`, so
            // every `function f(){ …G… } … const G = …` pair in a bundle hit
            // this — it is why react-router could not resolve its own helpers.
            {
                let mut lex = std::collections::HashSet::new();
                for s in &a.body.statements {
                    match s {
                        ox::Statement::VariableDeclaration(d) if d.kind.is_lexical() => {
                            for decl in &d.declarations {
                                capture::collect_pattern_names(&decl.id, &mut lex);
                            }
                        }
                        ox::Statement::ClassDeclaration(c) => {
                            if let Some(id) = &c.id {
                                lex.insert(id.name.to_string());
                            }
                        }
                        _ => {}
                    }
                }
                // Sorted: alloc_reg() below, so raw HashSet order would permute cell registers.
                for name in &crate::compile::helpers::sorted_name_vec(&lex) {
                    if !fc.scopes[0].iter().any(|(n, _)| n == name) {
                        let r = fc.alloc_reg();
                        fc.scopes[0].push((name.clone(), r));
                        fc.emit(Instr::MakeCellTdz { reg: r });
                        fc.cell_regs.insert(r);
                        fc.entry_lexicals.insert(name.clone());
                        // The cell now EXISTS, so an assignment before the
                        // textual declaration resolves to it and would emit a
                        // plain `CellSet`, writing straight through the TDZ.
                        // A read always threw, which is why this looked fine.
                        fc.entry_tdz_cells.insert(r);
                    }
                }
            }
            for s in &a.body.statements {
                fc.stmt(s)?;
            }
            fc.emit(Instr::ReturnUndefined);
        }
        fc.cx.in_strict = parent_strict; // restore: nested compiles are done
        fc.cx.dyn_global_zone = saved_dyn_zone_arrow;
        let upvalues: Vec<UpvalSource> =
            fc.upvalues.borrow().iter().map(|(_, s)| *s).collect();
        fc.check_regs()?;
        Ok(FuncProto {
            name: "<arrow>".to_string(),
            code: fc.code,
            reg_count: fc.max_reg,
            param_count: params.len() as u16,
            length: expected_arg_count(&a.params),
            rest_reg: fc.rest_reg,
            arguments_reg: if fc.uses_arguments { fc.arguments_reg } else { None },
            is_generator: false,
            is_async: a.r#async,
            non_constructable: true, // arrow functions have no [[Construct]]
            lexical_this: true, // arrows capture `this` lexically (see FuncProto)
            super_static, // inherited from the enclosing method/block
            is_strict,
            simple_params: false, // an arrow has no own `arguments`
            constants: fc.constants,
            string_constants: fc.string_constants,
            bigint_consts: fc.bigint_consts,
            wtf8_consts: fc.wtf8_consts,
            name_global: None,
            upvalues,
            eval_sites: std::mem::take(&mut fc.eval_sites),
            source: String::new(), // set by compile_arrow from the arrow's span
        })
    }
}
