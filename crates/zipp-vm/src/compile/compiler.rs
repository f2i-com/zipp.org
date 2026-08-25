// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

// The AST this compiles. Imported explicitly (rather than relying on the glob
// above) so the module qualifier is stable no matter how `mod.rs` spells its
// own import; an explicit `use` shadows a glob, so this cannot conflict.
use crate::parse::ast;
use crate::parse::token::StrVal;

fn checked_global_slot_index(len: usize) -> Option<u32> {
    let slot = u32::try_from(len).ok()?;
    (slot < u32::MAX).then_some(slot)
}

/// Default-on removal of unnecessary heap cells for arrow-body lexicals.
///
/// A block-bodied arrow used to pre-create a TDZ cell for every body-level
/// `let`/`const`/`class`, even when no nested callable could observe the binding.
/// A binding whose first possible reference is its own simple declaration does
/// not need that early cell: the declaration compiler enforces its initializer
/// TDZ directly, and all later uses can address a plain register. Captures,
/// direct eval, complex declarations and any earlier reference retain the cell.
/// Keeping the old lowering behind a same-binary switch makes the allocation/JIT
/// effect independently measurable.
#[inline]
fn arrow_lexical_unbox_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let on = std::env::var_os("ZIPP_NO_ARROW_LEXICAL_UNBOX").is_none() as u8;
            ON.store(on, Ordering::Relaxed);
            on == 1
        }
    }
}

impl Compiler {
    /// Whether `name` names a binding of the eval's VARIABLE ENVIRONMENT — the
    /// calling function's own activation. Such a name is re-used by a top-level
    /// `var`/`function` declaration (EvalDeclarationInstantiation: the binding
    /// already exists, so the declaration is a no-op and the initializer assigns
    /// through it). A binding visible only through an ENCLOSING function is
    /// readable but is NOT the varEnv, so a declaration there shadows instead.
    pub(crate) fn eval_caller_var(&self, name: &str) -> bool {
        self.eval_caller_scope.iter().any(|n| n == name)
            && !self.eval_outer_scope.iter().any(|n| n == name)
    }

    pub(crate) fn new(source: String) -> Compiler {
        Compiler {
            functions: Vec::new(),
            static_key_plan_sites: 0,
            static_key_plan_retained_bytes: 0,
            globals: Vec::new(),
            global_index: rustc_hash::FxHashMap::default(),
            classes: Vec::new(),
            class_names: Vec::new(),
            hoisted_globals: Vec::new(),
            hoisted_set: rustc_hash::FxHashSet::default(),
            source,
            eval_mode: false,
            eval_inherit_super: None,
            in_field_init: false,
            module_mode: false,
            eval_locals: false,
            script_binds_globals: true,
            eval_inherit_super_obj: false,
            eval_caller_scope: Vec::new(),
            eval_outer_scope: Vec::new(),
            eval_catch_params: Vec::new(),
            eval_dynamic_names: std::collections::HashSet::new(),
            eval_fn_context: false,
            dyn_global_zone: false,
            force_strict: false,
            force_new_target_ok: false,
            in_strict: false,
            strict_expr_region: 0,
            new_target_ok: false,
            class_enclosing: Vec::new(),
            class_derived: false,
            compiling_ctor: false,
            private_names_stack: Vec::new(),
            heritage_classes: Vec::new(),
            eval_visible_privates: std::collections::HashSet::new(),
            in_derived_ctor: false,
            fn_ctor_no_self_name: false,
            const_globals: HashSet::new(),
            lexical_globals: HashSet::new(),
            decl_globals: HashSet::new(),
            module_exports: Vec::new(),
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
        self.debug_assert_global_indexes();
        let mut v: Vec<u32> = self.decl_globals.iter().copied().collect();
        v.extend(self.lexical_globals.iter().copied());
        v.extend(self.const_globals.iter().copied());
        v.extend(self.hoisted_globals.iter().copied());
        if let Some(i) = self.existing_global_slot("*default*") {
            v.push(i as u32);
        }
        // An INLINE `export const x` / `export function f` is an
        // ExportNamedDeclaration, so the bare-declaration hoisting pre-passes above
        // never registered its slot. Add every exported LOCAL's slot directly so a
        // module's exports always get fresh per-module slots (isolation).
        for (_, local) in &self.module_exports {
            if let Some(i) = self.existing_global_slot(local) {
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

    /// Look up an already-reserved global without changing the deterministic
    /// slot-to-name vector. All name-to-slot reads go through the reverse index;
    /// the point assertion catches a stale/wrong map entry at its first use.
    pub(crate) fn existing_global_slot(&self, name: &str) -> Option<u32> {
        let slot = self.global_index.get(name).copied();
        #[cfg(debug_assertions)]
        if let Some(slot) = slot {
            debug_assert_eq!(
                self.globals.get(slot as usize).map(String::as_str),
                Some(name),
                "global_index slot disagrees with globals"
            );
        }
        slot
    }

    pub(crate) fn global_slot(&mut self, name: &str) -> u32 {
        if let Some(i) = self.existing_global_slot(name) {
            return i;
        }
        let i = checked_global_slot_index(self.globals.len())
            .expect("a program cannot contain more than u32::MAX globals");
        let owned = name.to_string();
        self.globals.push(owned.clone());
        let old = self.global_index.insert(owned, i);
        debug_assert!(old.is_none(), "global_index insertion replaced an entry");
        debug_assert_eq!(self.globals.len(), self.global_index.len());
        i
    }

    /// Append a top-level `var` slot exactly once while keeping its ordered
    /// replay vector and membership index in lockstep.
    fn record_hoisted_global(&mut self, slot: u32) {
        if self.hoisted_set.insert(slot) {
            self.hoisted_globals.push(slot);
        }
        debug_assert_eq!(self.hoisted_globals.len(), self.hoisted_set.len());
    }

    /// Full debug-only consistency check for the ordered compiler tables and
    /// their lookup indexes. This runs at compile/module boundaries, not per
    /// lookup, so debug builds retain linear rather than quadratic compilation.
    fn debug_assert_global_indexes(&self) {
        #[cfg(debug_assertions)]
        {
            debug_assert_eq!(self.globals.len(), self.global_index.len());
            for (slot, name) in self.globals.iter().enumerate() {
                debug_assert_eq!(
                    self.global_index.get(name).copied(),
                    Some(slot as u32),
                    "globals entry disagrees with global_index"
                );
            }
            debug_assert_eq!(self.hoisted_globals.len(), self.hoisted_set.len());
            let ordered_set: rustc_hash::FxHashSet<u32> =
                self.hoisted_globals.iter().copied().collect();
            debug_assert_eq!(
                ordered_set.len(),
                self.hoisted_globals.len(),
                "hoisted_globals contains a duplicate"
            );
            debug_assert_eq!(
                ordered_set, self.hoisted_set,
                "hoisted_globals disagrees with hoisted_set"
            );
        }
    }

    pub(crate) fn compile(&mut self, prog: &ast::Program) -> R<()> {
        // Module code is always strict; a script is sloppy unless its directive
        // prologue says `"use strict"` (folded in by `compile_function_body`). A
        // direct eval from strict code forces strict for the whole eval program.
        // NOTE: `Program::strict` already ORs in the prologue, but this seeds only
        // the INHERITED strictness — the prologue is applied one level down, per
        // body, so `Goal::Module` (the old `source_type.is_module()`) is what is
        // read here and the field stays deliberately unused.
        self.in_strict = prog.goal == ast::Goal::Module || self.force_strict;
        // Reserve function id 0 for the top-level script body; fill it last so
        // nested function ids are stable as we discover them.
        self.functions.push(placeholder("<script>"));

        // Pass 1: hoist top-level function (and class) declaration names to globals.
        // Record their slots as non-configurable bindings (for `delete <name>`).
        // (A strict eval binds NOTHING globally — its declarations are locals.)
        let binds_globals = self.script_binds_globals;
        for s in prog.body.iter().filter(|_| binds_globals) {
            match s {
                ast::Stmt::FnDecl(f) => {
                    if let Some(id) = &f.name {
                        let slot = self.global_slot(id) as u32;
                        self.decl_globals.insert(slot);
                    }
                }
                ast::Stmt::ClassDecl(c) => {
                    if let Some(id) = &c.name {
                        let slot = self.global_slot(id) as u32;
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
                if self.eval_caller_var(&name) || self.eval_dynamic_names.contains(&name) {
                    continue;
                }
                let slot = self.global_slot(&name) as u32;
                self.record_hoisted_global(slot);
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
            false,     // top-level script is not a generator
            top_async, // a module top level is async (top-level await)
            captured,
            Vec::new(),
        )?;
        self.functions[0] = top;
        self.debug_assert_global_indexes();
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
        params_ast: Option<&ast::Params>,
        body: &[ast::Stmt],
        directives: &[ast::Directive],
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
        let saved_field_init = if is_script {
            self.in_field_init
        } else {
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
        // and the dynamic `GeneratorFunction('x = yield', '')` forms; the AST
        // records no [Yield]/[Await] parameter context, so the check lives here).
        // A parameter's DEFAULT is `Pattern::Assign`'s right side and the rest
        // parameter is the trailing `Pattern::Rest`, so one walk over `items`
        // covers what used to be three separate scans (initializer, pattern, rest).
        // NOTE: that relies on `pattern_has_yield_or_await` recursing through BOTH
        // of those variants — they are the two the old parameter shape kept outside
        // the pattern, so they are exactly what its port must not drop.
        check_params_yield_await(params_ast, is_generator, is_async)?;
        // `new.target` is allowed inside an ordinary function, not at script/eval
        // top level; nested arrows inherit this. Restored at the end. A DIRECT eval
        // from inside a function/method/field initializer forces it on for the eval
        // script top level (the eval inherits the caller's new.target validity).
        let parent_nt = self.new_target_ok;
        self.new_target_ok = !is_script || self.force_new_target_ok;
        // CreateDynamicFunction (`new Function` & kin): the wrapper's
        // "anonymous" name is a source-text artifact only — the created
        // function gets SetFunctionName("anonymous") but NO self-name binding
        // (its params/body are parsed and instantiated separately), so
        // `typeof anonymous` inside is "undefined" (constructor-binding.js).
        // Consume the one-shot flag on the FIRST function named "anonymous":
        // compilation is depth-first, so that is the wrapper itself, never a
        // same-named function nested in its parameter defaults or body.
        let self_name = if self_name == Some("anonymous") && self.fn_ctor_no_self_name {
            self.fn_ctor_no_self_name = false;
            None
        } else {
            self_name
        };
        // A body that references `eval` may direct-eval: box EVERY param and
        // function-scoped local so the eval program can close over the caller
        // scope (cells outlive the frame for closures the eval creates). The
        // reference scan must ALSO see a LOCALLY-bound `eval` (`var eval = f`)
        // — a call through it is still a direct eval when its value is %eval%.
        let mut captured = captured;
        let body_refs_eval = !is_script
            && (capture::stmts_refs_all(body).contains("eval")
                || params_ast.is_some_and(|pa| capture::params_reference("eval", pa)));
        let saved_dyn_zone = self.dyn_global_zone;
        if body_refs_eval {
            self.dyn_global_zone = true;
        }
        // A sloppy FUNCTION-context eval root: its `var`/function names live in
        // the caller activation's dynamic EvalScope, never in a realm global.
        // The zone must cover the functions the eval CREATES as well, not just
        // its own top level — `fc.box_all_locals` below is per-FnCompiler and
        // does not reach nested bodies, so
        //     function h(){ return eval("var w = 5; (function(){ return w; })"); }
        //     h()()          // ReferenceError: w is not defined
        // compiled the inner `w` to a plain LoadGlobal that never consults the
        // EvalScope (staging/sm/eval/exhaustive-fun-*, sm/regress/regress-554955-*).
        if is_script && self.eval_fn_context {
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
            if let Some(ctx) = fc.cx.eval_inherit_super.clone() {
                fc.super_class = Some(u32::MAX);
                fc.super_static = ctx.static_ctx;
                // PerformEval keeps the caller's [[ThisBindingStatus]], so a
                // direct eval in a DERIVED constructor may call `super()` —
                // rejected outright while this flag defaulted to false.
                fc.derived_class = ctx.derived_ctor;
                // …and the eval SCRIPT itself runs with the caller's derived-ctor
                // `this`-TDZ state: `this` reads are checked, arrows it defines
                // inherit the context (compile_arrow_body reads this flag), and a
                // NESTED direct eval re-inherits it (`eval("eval('super()')")`).
                fc.in_derived_ctor = ctx.derived_ctor;
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
                let idx = add_str_val_const(&mut fc, &d.value);
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
                                    //
                                    // A body (or parameter default) that may DIRECT-EVAL needs the same
                                    // treatment: `free_vars` cannot look inside the eval string, so
                                    // nothing else marks `arguments` as used, and an unboxed binding is
                                    // absent from the eval site map — `function(){ return eval("arguments") }`
                                    // threw ReferenceError. A closure in a PARAMETER default is created
                                    // before the body runs and captures the same object, which the
                                    // body-only scan never sees.
            if capture::nested_uses_arguments(body)
                || body_refs_eval
                || params_ast.is_some_and(capture::params_nested_use_arguments)
            {
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
        // A LABELLED declaration (`L: function f(){}`) is a TopLevelVarScoped-
        // Declaration of the body too, so the label chain is unwrapped here as
        // well — otherwise the name had no binding and `func_decl` emitted
        // nothing at all.
        for s in body {
            if let Some(f) = labelled_fn_decl(s) {
                if let Some(id) = &f.name {
                    if is_script && fc.cx.script_binds_globals {
                        fc.cx.global_slot(id);
                    } else {
                        fc.declare_local(id);
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
            // B.3.3.1 and `arguments`: a BLOCKER but deliberately NOT protected.
            // The extension's outer guard is `paramNames does not contain F`, and
            // since ES2018 `paramNames` is never mutated to hold "arguments" — the
            // arguments object is appended to a SEPARATE `paramBindings` list — so
            // the guard does not fire and the extension is NOT skipped. Only the
            // inner step is suppressed (`... and F is not "arguments"`), which
            // creates the var binding; the SetMutableBinding performed when the
            // block declaration is evaluated sits outside that guard and always
            // runs. So the arguments OBJECT is overwritten by the block function:
            //   function f(){ { function arguments(){} } return typeof arguments }
            // is "function", not "object".
            //
            // Protecting the name as well (what this used to do) implemented the
            // ES2017 text, where step 22.f really did append "arguments" to
            // parameterNames. test262 still carries a V8-authored 2017 test,
            // annexB/language/function-code/block-decl-func-skip-arguments.js,
            // that encodes the removed wording — V8 fails it, and passing it was
            // mistaken here for being MORE conformant than V8. It is not.
            //
            // `arguments` as a genuine FORMAL PARAMETER is unaffected: the params
            // loop above puts it in both sets, which is correct, because then
            // paramNames really does contain it and the whole extension is skipped.
            if !is_script {
                blockers.insert("arguments".to_string());
            }
            for s in body {
                match s {
                    ast::Stmt::VarDecl(d) if d.kind.is_lexical() => {
                        for decl in &d.decls {
                            capture::collect_pattern_names(&decl.id, &mut blockers);
                            capture::collect_pattern_names(&decl.id, &mut protect);
                        }
                    }
                    ast::Stmt::ClassDecl(c) => {
                        if let Some(id) = &c.name {
                            blockers.insert(id.to_string());
                            protect.insert(id.to_string());
                        }
                    }
                    // (Labelled top-level declarations too — see the hoist above.)
                    _ => {
                        if let Some(id) = labelled_fn_decl(s).and_then(|f| f.name.as_ref()) {
                            blockers.insert(id.to_string());
                        }
                    }
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
                    if let Some(f) = labelled_fn_decl(s) {
                        if let Some(id) = &f.name {
                            script_blockers.remove(&**id);
                            fn_names.insert(id.to_string());
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
                    if fc.cx.eval_dynamic_names.contains(name) || fc.cx.eval_caller_var(name) {
                        continue;
                    }
                    if !fn_names.contains(name) {
                        let slot = fc.cx.global_slot(name) as u32;
                        fc.cx.record_hoisted_global(slot);
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
        // NOTE: the `with { … }` clause is a plain `Vec<ImportAttribute>` on the
        // declaration now, so `with_clause_type` is called with that list —
        // `with_clause_type(&[ast::ImportAttribute])` is the ported signature this
        // assumes (string_accum.rs owns it).
        if is_script && (fc.cx.module_mode || (fc.cx.eval_mode && !fc.cx.eval_locals)) {
            use crate::bytecode::{ImportEntry, ImportName};
            for s in body {
                match s {
                    ast::Stmt::Import(d) => {
                        // `import defer * as ns` binds the module's DEFERRED
                        // namespace; `import source x` binds the target's
                        // ModuleSource object; a bindingless phase form stays
                        // load-only.
                        if !matches!(d.phase, ast::ImportPhase::Evaluation) {
                            let defer_ns_local = if matches!(d.phase, ast::ImportPhase::Defer) {
                                d.specifiers.iter().find_map(|sp| match sp {
                                    ast::ImportSpecifier::Namespace(local) => {
                                        Some(local.to_string())
                                    }
                                    _ => None,
                                })
                            } else {
                                None
                            };
                            // `import source x from '…'` parses as phase Source
                            // with a default-shaped binding specifier.
                            let source_local = if matches!(d.phase, ast::ImportPhase::Source) {
                                d.specifiers.iter().find_map(|sp| match sp {
                                    ast::ImportSpecifier::Default(local) => Some(local.to_string()),
                                    _ => None,
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
                                    specifier: str_val_text(&d.source),
                                    mtype: with_clause_type(&d.attributes),
                                });
                            } else if let Some(local) = defer_ns_local {
                                let slot = fc.cx.global_slot(&local) as u32;
                                fc.cx.decl_globals.insert(slot);
                                fc.cx.const_globals.insert(slot);
                                fc.cx.module_imports.push(ImportEntry {
                                    local_slot: slot,
                                    import: ImportName::DeferNamespace,
                                    specifier: str_val_text(&d.source),
                                    mtype: with_clause_type(&d.attributes),
                                });
                            } else {
                                fc.cx.module_imports.push(ImportEntry {
                                    local_slot: u32::MAX,
                                    import: ImportName::LoadOnly,
                                    specifier: str_val_text(&d.source),
                                    mtype: with_clause_type(&d.attributes),
                                });
                            }
                            continue;
                        }
                        let spec = str_val_text(&d.source);
                        // `import "m"` (no specifier clause at all) and
                        // `import {} from "m"` both arrive as an empty list —
                        // the same SideEffect entry either way, as before.
                        if !d.specifiers.is_empty() {
                            for sp in &d.specifiers {
                                let (local, import) = match sp {
                                    ast::ImportSpecifier::Named { imported, local } => (
                                        local.to_string(),
                                        ImportName::Named(module_export_name(imported)),
                                    ),
                                    ast::ImportSpecifier::Default(local) => {
                                        (local.to_string(), ImportName::Default)
                                    }
                                    ast::ImportSpecifier::Namespace(local) => {
                                        (local.to_string(), ImportName::Namespace)
                                    }
                                };
                                let slot = fc.cx.global_slot(&local) as u32;
                                fc.cx.decl_globals.insert(slot);
                                fc.cx.const_globals.insert(slot);
                                fc.cx.module_imports.push(ImportEntry {
                                    local_slot: slot,
                                    import,
                                    specifier: spec.clone(),
                                    mtype: with_clause_type(&d.attributes),
                                });
                            }
                        } else {
                            fc.cx.module_imports.push(ImportEntry {
                                local_slot: u32::MAX,
                                import: ImportName::SideEffect,
                                specifier: spec.clone(),
                                mtype: with_clause_type(&d.attributes),
                            });
                        }
                    }
                    // The two export forms that name a SOURCE module are one
                    // statement variant now, so they share an arm; the order of
                    // pushes is still source order.
                    ast::Stmt::Export(e) => match &**e {
                        ast::ExportDecl::Named {
                            source: Some(srcspec),
                            attributes,
                            ..
                        } => {
                            fc.cx.module_imports.push(ImportEntry {
                                local_slot: u32::MAX,
                                import: ImportName::SideEffect,
                                specifier: str_val_text(srcspec),
                                mtype: with_clause_type(attributes),
                            });
                        }
                        ast::ExportDecl::All {
                            source, attributes, ..
                        } => {
                            fc.cx.module_imports.push(ImportEntry {
                                local_slot: u32::MAX,
                                import: ImportName::SideEffect,
                                specifier: str_val_text(source),
                                mtype: with_clause_type(attributes),
                            });
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        if is_script && !fc.cx.eval_locals {
            // (An `export let x` / `export class C` wraps the declaration in an
            // ExportNamedDeclaration — same lexical binding, same TDZ.)
            for s in body {
                let (var_decl, class_decl) = match s {
                    ast::Stmt::VarDecl(d) => (Some(d), None),
                    ast::Stmt::ClassDecl(c) => (None, Some(&**c)),
                    ast::Stmt::Export(e) => match &**e {
                        ast::ExportDecl::Decl(d) => match &**d {
                            ast::Stmt::VarDecl(v) => (Some(v), None),
                            ast::Stmt::ClassDecl(c) => (None, Some(&**c)),
                            _ => (None, None),
                        },
                        _ => (None, None),
                    },
                    _ => (None, None),
                };
                if let Some(d) = var_decl {
                    if d.kind.is_lexical() {
                        for decl in &d.decls {
                            if let ast::Pattern::Ident(id) = &decl.id {
                                // GlobalDeclarationInstantiation step 5: a top-level
                                // lexical name colliding with a RESTRICTED global
                                // property (the non-configurable value properties
                                // undefined/NaN/Infinity) is a SyntaxError.
                                if matches!(&**id, "undefined" | "NaN" | "Infinity") {
                                    return Err(format!(
                                        "SyntaxError: lexical declaration of '{id}' collides with a restricted global property"
                                    ));
                                }
                                let slot = fc.cx.global_slot(id) as u32;
                                fc.cx.lexical_globals.insert(slot);
                            }
                        }
                    }
                }
                // A top-level CLASS declaration is a lexical binding with the
                // same TDZ (typeof C before it runs is a ReferenceError).
                if let Some(c) = class_decl {
                    if let Some(id) = &c.name {
                        let slot = fc.cx.global_slot(id) as u32;
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
                    ast::Stmt::VarDecl(d) if d.kind.is_lexical() => {
                        for decl in &d.decls {
                            capture::collect_pattern_names(&decl.id, &mut lex);
                        }
                    }
                    ast::Stmt::ClassDecl(c) => {
                        if let Some(id) = &c.name {
                            lex.insert(id.to_string());
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
            // FunctionDeclarationInstantiation with parameter EXPRESSIONS (a
            // non-simple list, sloppy mode): the body gets a SEPARATE var-env
            // binding for a hoisted `var arguments`, initialized to the
            // parameter environment's arguments object — so `var arguments = 0`
            // does not clobber the `arguments` a parameter-default closure
            // captured, while bare `var arguments;` still STARTS as that same
            // object (arguments-parameter-shadowing.js). A simple parameter
            // list (or strict mode) uses one environment, where the names stay
            // shared — the skip in the loop above is then exactly right.
            if !is_strict && params_ast.is_some_and(|pa| !pa.simple) && hv.contains("arguments") {
                if let Some(areg) = fc.arguments_reg {
                    fc.uses_arguments = true;
                    let body_reg = fc.declare_local("arguments");
                    match (
                        fc.cell_regs.contains(&areg),
                        fc.cell_regs.contains(&body_reg),
                    ) {
                        (false, false) => fc.emit(Instr::Move {
                            dst: body_reg,
                            src: areg,
                        }),
                        (false, true) => fc.emit(Instr::CellSet {
                            cell: body_reg,
                            src: areg,
                        }),
                        (true, false) => fc.emit(Instr::CellGet {
                            dst: body_reg,
                            cell: areg,
                        }),
                        (true, true) => {
                            let t = fc.alloc_reg();
                            fc.emit(Instr::CellGet { dst: t, cell: areg });
                            fc.emit(Instr::CellSet {
                                cell: body_reg,
                                src: t,
                            });
                        }
                    }
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
                    ast::Stmt::VarDecl(d) if d.kind.is_lexical() => {
                        for decl in &d.decls {
                            capture::collect_pattern_names(&decl.id, &mut lex);
                        }
                    }
                    ast::Stmt::ClassDecl(c) => {
                        if let Some(id) = &c.name {
                            lex.insert(id.to_string());
                        }
                    }
                    _ => {}
                }
            }
            // Sorted: this loop calls alloc_reg(), so raw HashSet order would
            // hand out CELL REGISTERS in a different order per compile.
            for name in &crate::compile::helpers::sorted_name_vec(&lex) {
                // `box_all_locals` here means "this body references `eval`". A
                // direct eval may name any of these lexicals, but
                // `capture::captured_locals` cannot see inside the eval STRING, so
                // `fc.captured` does not list them. Without the cell at entry, the
                // function declarations materialised just below — they compile
                // BEFORE the body's textual statements — snapshot an environment
                // with no such binding, and the eval inside one resolved the name
                // as a global:
                //   (function(){ let a=1; function f(){ return eval("a"); }
                //                return f(); })()   // ReferenceError
                // while the same code with a function EXPRESSION worked. That is
                // what killed every sm/expressions/destructuring-array-default-*
                // (their harness evals `class D extends C` from a nested function
                // declaration, with `C` a lexical of the enclosing IIFE).
                if (fc.captured.contains(name) || fc.box_all_locals)
                    && !fc.scopes[0].iter().any(|(n, _)| n == name)
                {
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
                if let ast::Stmt::FnDecl(f) = s {
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
                    if let ast::Stmt::FnDecl(_) = s {
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

        let upvalues: Vec<UpvalSource> = fc.upvalues.borrow().iter().map(|(_, s)| *s).collect();
        fc.cx.in_field_init = saved_field_init;
        fc.cx.in_derived_ctor = saved_idc;
        fc.cx.dyn_global_zone = saved_dyn_zone;
        fc.check_regs()?;
        Ok(FuncProto {
            name: name.unwrap_or("<script>").to_string(),
            code: fc.code,
            reg_count: fc.max_reg,
            param_count: params.len() as u16,
            length: params_ast
                .map(expected_arg_count)
                .unwrap_or(params.len() as u16),
            rest_reg: fc.rest_reg,
            arguments_reg: if fc.uses_arguments {
                fc.arguments_reg
            } else {
                None
            },
            is_generator,
            is_async,
            non_constructable: false, // a plain function/expression IS constructable
            lexical_this: false,
            // A plain function is not a static class element, so this stays
            // false for every ordinary body — but a direct-eval ROOT inherits
            // the caller's static-ness above, and the super ops read it from
            // the PROTO, not from their operands. Hard-coding false here made
            // `eval("super.m()")` inside a static method walk the instance
            // chain (`super.m is not a function`).
            super_static: fc.super_static,
            is_strict,
            // IsSimpleParameterList is decided by the parser and carried on
            // `Params`, so it is read, not recomputed.
            simple_params: params_ast.map(|pa| pa.simple).unwrap_or(false),
            constants: fc.constants,
            string_constants: fc.string_constants,
            static_key_plans: fc.static_key_plans,
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
        params_ast: Option<&ast::Params>,
        fields: &[(String, Option<&ast::Expr>)],
        computed_inits: &[Option<&ast::Expr>],
        // `field_order`: source order of the two field lists — (0 = `fields`,
        // 1 = `computed_inits`) -> index. Empty when there are no instance fields.
        field_order: &[(u8, usize)],
        body: &[ast::Stmt],
        super_class: Option<u32>,
        super_static: bool,
        is_generator: bool,
        is_async: bool,
        dec: Option<&DecFieldPlan<'_>>,
    ) -> R<FuncProto> {
        check_params_yield_await(params_ast, is_generator, is_async)?;
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
                                // object lexically — materialize + box it (see compile_function_body,
                                // whose direct-eval / parameter-default cases apply here identically).
        if capture::nested_uses_arguments(body)
            || cls_body_refs_eval
            || params_ast.is_some_and(capture::params_nested_use_arguments)
        {
            if let Some(r) = fc.arguments_reg {
                fc.uses_arguments = true;
                fc.emit(Instr::MakeCell { reg: r });
                fc.cell_regs.insert(r);
            }
        }
        // 10.2.2 [[Construct]] step 5: for a BASE class, InitializeInstanceElements
        // runs on `thisArgument` BEFORE OrdinaryCallEvaluateBody — so the field
        // initializers precede FunctionDeclarationInstantiation's parameter
        // defaults. zipp inlines the initializers into the constructor, so the
        // spec order is an EMISSION order here; emitting them after `bind_params`
        // made `class A { x = "h" + g1(); constructor(o = g2()){} }` throw 20
        // instead of 10 (staging/sm/fields/init-order.js).
        //
        // Guarded: this inlined form lets an initializer resolve a name to a
        // PARAMETER register (already wrong — per 15.7.10 a field initializer is
        // its own function and never sees the constructor's parameters), and
        // hoisting it above `bind_params` would read that register unbound. When
        // an initializer mentions a parameter name at all, keep the old order.
        let mut param_names: HashSet<String> = params.iter().cloned().collect();
        if let Some(r) = rest {
            param_names.insert(r.to_string());
        }
        if let Some(pa) = params_ast {
            param_names.extend(param_pattern_leaves(pa));
        }
        let fields_first = params_ast.is_some()
            && (!fields.is_empty() || !computed_inits.is_empty())
            && !fields
                .iter()
                .filter_map(|(_, e)| *e)
                .chain(computed_inits.iter().filter_map(|e| *e))
                .any(|e| {
                    capture::expr_refs_all(e)
                        .iter()
                        .any(|n| param_names.contains(n))
                });
        if !fields_first {
            if let Some(pa) = params_ast {
                fc.bind_params(pa)?;
            }
        }
        // A generator method (sync OR async) runs its parameter prologue eagerly at
        // call and is created suspended here (a constructor is never a generator, so
        // no field initializers precede the body for this case).
        if is_generator {
            fc.emit(Instr::GenStart);
        }
        // InitializeInstanceElements: `constructor.[[Initializers]]` — the
        // `addInitializer` callbacks of this class's decorated instance METHODS,
        // getters and setters — run with `this` = the new object at the HEAD of
        // instance element initialization, before any field. (A `@bound` method's
        // initializer must be able to run before a field initializer that reads
        // the bound method.) A decorated FIELD or ACCESSOR's callbacks are not in
        // this list: they run right after their own element, below.
        if let Some(d) = dec.filter(|d| d.run_inits) {
            fc.emit(Instr::DecInits {
                class_id: d.class_id,
                which: 0,
                elem: 0,
                recv: 0,
            });
        }
        // Instance field initializers in SOURCE order (`this.field = expr`, this =
        // reg 0). `arguments` is an early SyntaxError inside an initializer (and
        // in any direct eval / arrow it contains).
        for &(which, i) in field_order {
            let save = fc.next_reg;
            // A named field carries its own key; a computed one took its key at
            // class-definition time and is stored positionally on the class.
            let (fname, finit, dec_elem) = if which == 0 {
                let (n, e) = &fields[i];
                (
                    Some(n),
                    *e,
                    dec.and_then(|d| d.named.get(i).copied().flatten()),
                )
            } else {
                (
                    None,
                    computed_inits[i],
                    dec.and_then(|d| d.computed.get(i).copied().flatten()),
                )
            };
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
            if let Some(fname) = fname {
                if matches!(finit, Some(e) if is_anonymous_fn_def(e)) {
                    let kr = fc.temp();
                    let idx = fc.add_string_const(fname);
                    fc.emit(Instr::LoadConst { dst: kr, idx });
                    fc.emit(Instr::SetFnNameFromKey {
                        func: v,
                        key: kr,
                        prefix: 0,
                    });
                }
            }
            // A decorated field's value passes through whatever initializers its
            // decorators returned before it is defined. `DecField` writes back
            // through its `val` operand, so the value is first copied into a
            // temp — `fc.expr` may have handed back a live LOCAL's register.
            let v = match dec_elem {
                Some(elem) => {
                    let t = fc.temp();
                    fc.emit(Instr::Move { dst: t, src: v });
                    fc.emit(Instr::DecField {
                        class_id: dec.unwrap().class_id,
                        elem,
                        val: t,
                        recv: 0,
                    });
                    t
                }
                None => v,
            };
            match fname {
                // DefineField (CreateDataPropertyOrThrow) for PUBLIC fields —
                // never a [[Set]] (an inherited setter must not run; a Proxy
                // receiver's defineProperty trap must). Private "#fields" keep
                // the plain store (private semantics bypass proxies entirely).
                Some(fname) => {
                    let name_idx = fc.string_name(fname);
                    if fname.starts_with('#') {
                        fc.emit(Instr::SetProp {
                            obj: 0,
                            name: name_idx,
                            val: v,
                            strict: false,
                        });
                    } else {
                        fc.emit(Instr::DefineField {
                            obj: 0,
                            name: name_idx,
                            val: v,
                        });
                    }
                }
                None => fc.emit(Instr::FieldInit {
                    key_index: i as u16,
                    val: v,
                    class_id: fc.super_class.unwrap_or(u32::MAX),
                }),
            }
            // InitializeFieldOrAccessor runs the element's OWN extraInitializers
            // last, so a `@dec x` initializer observes `this.x` already set and
            // the NEXT field still absent.
            if let Some(elem) = dec_elem {
                fc.emit(Instr::DecInits {
                    class_id: dec.unwrap().class_id,
                    which: 3,
                    elem,
                    recv: 0,
                });
            }
            fc.next_reg = save;
        }
        // …and only now the parameter prologue, when the base-class field
        // initializers above had to precede it (see `fields_first`).
        if fields_first {
            if let Some(pa) = params_ast {
                fc.bind_params(pa)?;
            }
        }
        for s in body {
            if let ast::Stmt::FnDecl(f) = s {
                if let Some(id) = &f.name {
                    fc.declare_local(id);
                }
            }
        }
        // A class method / constructor / accessor / `static {}` body is a
        // FunctionBody (resp. ClassStaticBlockBody) like any other, so a
        // TOP-LEVEL `using` in it must dispose on exit — the same finally
        // desugar `compile_function_body` applies. Without this the resource was
        // registered and never disposed, so `class C { static { using x = r; } }`
        // never ran r[Symbol.dispose]
        // (staging/explicit-resource-management/call-dispose-methods.js).
        if FnCompiler::block_has_using(body) {
            fc.compile_using_block(body, false)?;
        } else {
            for s in body {
                fc.stmt(s)?;
            }
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
            length: params_ast
                .map(expected_arg_count)
                .unwrap_or(params.len() as u16),
            rest_reg: fc.rest_reg,
            arguments_reg: if fc.uses_arguments {
                fc.arguments_reg
            } else {
                None
            },
            is_generator,
            is_async,
            // A class method/getter/setter is non-constructable. The class
            // CONSTRUCTOR is also compiled here, but it is reached only via the
            // HeapObj::Class [[Construct]] path (never as a raw Func), so this flag
            // is never consulted for it — safe to set uniformly.
            non_constructable: true,
            lexical_this: false, // a concise method gets its own `this`, not lexical
            super_static,        // true for static methods/getters/setters/blocks
            is_strict: true,
            simple_params: false, // strict (class body) — never mapped anyway
            constants: fc.constants,
            string_constants: fc.string_constants,
            static_key_plans: fc.static_key_plans,
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
        a: &ast::Arrow,
        captured: HashSet<String>,
        enclosing: Vec<EnclosingFn>,
        super_class: Option<u32>,
        super_static: bool,
        super_home_obj: bool,
        enclosing_derived: bool,
        enclosing_in_derived_ctor: bool,
    ) -> R<FuncProto> {
        let parent_strict = self.in_strict;
        // Only a BLOCK body has a directive prologue: `x => "use strict"` is a
        // value, not a directive, and the expression form carries no directives.
        let is_strict = parent_strict
            || match &a.body {
                ast::ArrowBody::Block(b) => has_use_strict(&b.directives),
                ast::ArrowBody::Expr(_) => false,
            };
        // An arrow that references `eval` (incl. in its parameter defaults)
        // boxes its locals and records DirectEval sites like a function.
        let mut captured = captured;
        let arrow_refs_eval = capture::free_vars_arrow(&[], &a.body).contains("eval")
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
        // reflects the enclosing class while its method bodies (and their arrows) compile;
        // the enclosing FnCompiler's own flag ALSO feeds in, so an arrow compiled inside
        // a direct-eval script (whose root inherited derived-ness from the caller, while
        // `cx.class_derived` is false) may still call `super()`.
        fc.derived_class = fc.cx.class_derived || enclosing_derived;
        // Same route for the `this`-TDZ check: the enclosing activation's
        // derived-ctor-ness (a direct eval inside a derived ctor sets it on the
        // eval root; plain nesting sees `cx.in_derived_ctor`).
        fc.in_derived_ctor = fc.cx.in_derived_ctor || enclosing_in_derived_ctor;
        fc.in_async = a.is_async;
        fc.bind_params(&a.params)?;
        match &a.body {
            // `x => expr`: the body is the single expression to return. (The old
            // shape was a one-statement FunctionBody, so this branch also carried a
            // `ReturnUndefined` fallback for a statement that was not an expression
            // statement — unrepresentable now, hence gone.)
            ast::ArrowBody::Expr(e) => {
                let r = fc.expr(e)?;
                fc.emit(Instr::Return { src: r });
            }
            ast::ArrowBody::Block(b) => {
                // hoist nested function declarations (same as a normal body)
                for s in &b.stmts {
                    if let ast::Stmt::FnDecl(f) = s {
                        if let Some(id) = &f.name {
                            fc.declare_local(id);
                        }
                    }
                }
                // Pre-declare every hoisted var at entry (mirrors the
                // function-body pass — register accounting + with-fallback +
                // for-head resolution; see that pass's comment).
                {
                    let mut hv = std::collections::HashSet::new();
                    for s in &b.stmts {
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
                    let mut early_cell = std::collections::HashSet::new();
                    let mut prior_refs = std::collections::HashSet::new();
                    for s in &b.stmts {
                        match s {
                            ast::Stmt::VarDecl(d) if d.kind.is_lexical() => {
                                let mut declared_here = std::collections::HashSet::new();
                                for decl in &d.decls {
                                    capture::collect_pattern_names(&decl.id, &mut declared_here);
                                }
                                lex.extend(declared_here.iter().cloned());

                                // Multiple declarators and destructuring have
                                // intra-statement TDZ/order edges. Keep their
                                // established cell lowering; the hot package
                                // shape is the common single-identifier case.
                                let simple_single = d.decls.len() == 1
                                    && matches!(&d.decls[0].id, ast::Pattern::Ident(_));
                                for name in declared_here {
                                    if !simple_single || prior_refs.contains(&name) {
                                        early_cell.insert(name);
                                    }
                                }
                            }
                            ast::Stmt::ClassDecl(c) => {
                                if let Some(id) = &c.name {
                                    let name = id.to_string();
                                    lex.insert(name.clone());
                                    // Class heritage, computed names and static
                                    // initialization have richer self-TDZ rules.
                                    early_cell.insert(name);
                                }
                            }
                            _ => {}
                        }
                        prior_refs.extend(capture::stmts_refs_all(std::slice::from_ref(s)));
                    }
                    // Sorted: alloc_reg() below, so raw HashSet order would permute cell registers.
                    for name in &crate::compile::helpers::sorted_name_vec(&lex) {
                        // Captures/direct eval need address-stable storage;
                        // forward references need the early TDZ binding. A
                        // simple declaration reached before any reference can
                        // use the ordinary register path. The ablation switch
                        // keeps the historical eager-cell lowering exactly.
                        let needs_cell = !arrow_lexical_unbox_enabled()
                            || fc.captured.contains(name)
                            || fc.box_all_locals
                            || early_cell.contains(name);
                        if needs_cell && !fc.scopes[0].iter().any(|(n, _)| n == name) {
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
                for s in &b.stmts {
                    fc.stmt(s)?;
                }
                fc.emit(Instr::ReturnUndefined);
            }
        }
        fc.cx.in_strict = parent_strict; // restore: nested compiles are done
        fc.cx.dyn_global_zone = saved_dyn_zone_arrow;
        let upvalues: Vec<UpvalSource> = fc.upvalues.borrow().iter().map(|(_, s)| *s).collect();
        fc.check_regs()?;
        Ok(FuncProto {
            name: "<arrow>".to_string(),
            code: fc.code,
            reg_count: fc.max_reg,
            param_count: params.len() as u16,
            length: expected_arg_count(&a.params),
            rest_reg: fc.rest_reg,
            arguments_reg: if fc.uses_arguments {
                fc.arguments_reg
            } else {
                None
            },
            is_generator: false,
            is_async: a.is_async,
            non_constructable: true, // arrow functions have no [[Construct]]
            lexical_this: true,      // arrows capture `this` lexically (see FuncProto)
            super_static,            // inherited from the enclosing method/block
            is_strict,
            // IsSimpleParameterList is syntax, independent of whether the
            // function has an own `arguments` binding (arrows do not). Keeping
            // every arrow false unnecessarily excluded ordinary `(a, b) => …`
            // from positional-parameter JIT paths; arguments_reg/lexical_this
            // already carry the two arrow-specific semantics separately.
            simple_params: a.params.simple,
            constants: fc.constants,
            string_constants: fc.string_constants,
            static_key_plans: fc.static_key_plans,
            bigint_consts: fc.bigint_consts,
            wtf8_consts: fc.wtf8_consts,
            name_global: None,
            upvalues,
            eval_sites: std::mem::take(&mut fc.eval_sites),
            source: String::new(), // set by compile_arrow from the arrow's span
        })
    }
}

/// The literal TEXT a string value carries into the constant pool / the module
/// tables: well-formed text verbatim, and a value holding lone surrogates
/// re-spelled in the MARKER form (`\u{FFFD}XXXX` per code unit, `\u{FFFD}fffd`
/// for a literal U+FFFD).
///
/// The marker spelling is not decoration: `resolve_const` decodes it back to
/// WTF-8 at intern time, and it is exactly the text the parser used to hand over
/// for a literal it flagged `lone_surrogates` — so keeping it keeps the emitted
/// `string_constants` byte-identical.
fn str_val_text(s: &StrVal) -> String {
    match s {
        StrVal::Utf8(t) => t.clone(),
        StrVal::Utf16(units) => {
            let mut wtf8: Vec<u8> = Vec::with_capacity(units.len() * 3);
            for r in char::decode_utf16(units.iter().copied()) {
                let cp = match r {
                    Ok(c) => c as u32,
                    Err(e) => e.unpaired_surrogate() as u32,
                };
                crate::heap::wtf8_push_cp(&mut wtf8, cp);
            }
            crate::heap::encode_lone_surrogate_markers(&wtf8)
        }
    }
}

/// Intern a [`StrVal`] as a string CONSTANT, routing a lone-surrogate value
/// through the WTF-8-decoding slot — the same choice the parser's
/// `lone_surrogates` flag used to drive.
///
/// NOTE: the representation IS the flag. `StrVal::from_utf16` collapses to
/// `Utf8` only when every unit pairs up, and a literal is flagged precisely when
/// one does not, so `Utf16` ⇔ the old `lone_surrogates == true`.
fn add_str_val_const(fc: &mut FnCompiler<'_>, s: &StrVal) -> u32 {
    match s {
        StrVal::Utf8(t) => fc.add_string_const(t),
        StrVal::Utf16(_) => fc.add_string_const_wtf8(&str_val_text(s)),
    }
}

#[cfg(test)]
mod m1_tests {
    use super::{checked_global_slot_index, Compiler};
    use std::fmt::Write;
    use std::time::Instant;

    #[test]
    fn global_index_does_not_wrap_at_the_u16_boundary() {
        let mut compiler = Compiler::new(String::new());
        for slot in 0..=(u16::MAX as u32 + 1) {
            let name = format!("global_{slot}");
            assert_eq!(compiler.global_slot(&name), slot);
        }
        assert_eq!(compiler.existing_global_slot("global_65536"), Some(65_536));
        compiler.debug_assert_global_indexes();
    }

    #[test]
    fn global_count_rejects_the_unrepresentable_next_slot() {
        assert_eq!(checked_global_slot_index(u32::MAX as usize), None);
    }

    #[test]
    #[ignore = "explicit compiler-only scalability sweep"]
    fn generated_function_compile_sweep() {
        let mut ns_per_mb = Vec::new();
        for count in [3_000, 6_000, 12_000, 24_000] {
            let mut source = String::with_capacity(count * 36);
            for i in 0..count {
                writeln!(
                    source,
                    "function generated_{i}() {{ return generated_{}; }}",
                    i.saturating_sub(1)
                )
                .unwrap();
            }
            let ast = crate::front::parse_script(&source).expect("generated script parses");
            let bytes = source.len();
            let started = Instant::now();
            drop(
                crate::compile::compile_program(&ast, &source).expect("generated script compiles"),
            );
            let elapsed = started.elapsed();
            let rate = elapsed.as_nanos() as f64 * 1_000_000.0 / bytes as f64;
            ns_per_mb.push(rate);
            eprintln!(
                "functions={count} bytes={bytes} elapsed={elapsed:?} ns_per_mb={:.0}",
                rate
            );
        }
        assert!(
            ns_per_mb[3] / ns_per_mb[2] < 1.25,
            "compiler throughput degraded by {:.3}x from 12k to 24k functions",
            ns_per_mb[3] / ns_per_mb[2]
        );
    }
}
