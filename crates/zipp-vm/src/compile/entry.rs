// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

// The AST this compiles. Imported explicitly (rather than relying on the glob
// above) so the module qualifier is stable no matter how `mod.rs` spells its
// own import; an explicit `use` shadows a glob, so this cannot conflict. Note
// `Program` below is always the BYTECODE program — the AST one is `ast::Program`.
use crate::parse::ast;

pub fn compile_program(prog: &ast::Program, source: &str) -> R<Program> {
    compile_program_inner(prog, source, false)
}

/// Compile a MODULE as the program entry: the top level is an async context
/// (top-level `await`), and the VM runs func 0 as an async activation.
pub fn compile_module(prog: &ast::Program, source: &str) -> R<Program> {
    compile_program_inner(prog, source, true)
}

pub(crate) fn compile_program_inner(
    prog: &ast::Program,
    source: &str,
    module_mode: bool,
) -> R<Program> {
    let mut c = Compiler::new(source.to_string());
    c.module_mode = module_mode;
    c.compile(prog)?;
    for (i, f) in c.functions.iter_mut().enumerate() {
        rewrite_string_accumulators(f, i == 0);
        rewrite_local_accumulators(f);
        rewrite_append_indexes(f);
    }
    let module_decl_globals = c.collect_module_decl_globals();
    Ok(Program {
        functions: c.functions,
        global_count: c.globals.len() as u32,
        classes: c.classes,
        global_names: c.globals,
        hoisted_globals: c.hoisted_globals,
        // SORTED, not raw HashSet iteration order. std's HashSet reseeds its
        // hasher per process, so these came out in a different order on every
        // run — and the VM creates global-object properties by walking them, so
        // `Object.getOwnPropertyNames(globalThis)` permuted run to run. It also
        // made the compiler nondeterministic, which rules out comparing
        // bytecode between two front ends. Slots are handed out in order of
        // first mention, so sorting by slot is a stable, source-derived order.
        decl_globals: sorted_slots(&c.decl_globals),
        lexical_globals: sorted_slots(&c.lexical_globals),
        const_globals: sorted_slots(&c.const_globals),
        eval_dynamic_names: sorted_names(&c.eval_dynamic_names),
        module_exports: std::mem::take(&mut c.module_exports),
        module_has_imports: c.module_has_imports,
        module_reexports: std::mem::take(&mut c.module_reexports),
        module_star_reexports: std::mem::take(&mut c.module_star_reexports),
        module_ns_reexports: std::mem::take(&mut c.module_ns_reexports),
        module_imports: std::mem::take(&mut c.module_imports),
        module_decl_globals,
    })
}

/// Compile an `eval` code string. Identical to [`compile_program`] except the
/// top-level script returns its *completion value* (the value of the last
/// evaluated expression statement) — what `eval("1 + 1")` must yield. The VM
/// installs the resulting functions into its runtime function table and remaps
/// the program's independently-numbered global slots onto the live globals.
impl Compiler {
    /// Whether `#name` (full key, '#'-prefixed) is declared by an enclosing
    /// class body or visible to this direct eval.
    pub(crate) fn private_name_declared(&self, key: &str) -> bool {
        self.private_names_stack
            .iter()
            .any(|v| v.iter().any(|n| n == key))
            || self.eval_visible_privates.contains(key)
    }
}

/// The top-level var + function declared names of a parsed eval body —
/// EvalDeclarationInstantiation's varNames/functionNames for collision checks.
pub fn eval_var_and_fn_names(prog: &ast::Program) -> Vec<String> {
    let mut vars = std::collections::HashSet::new();
    for s in &prog.body {
        collect_hoisted_vars(s, &mut vars);
    }
    let mut out: Vec<String> = super::helpers::sorted_name_vec(&vars);
    for s in &prog.body {
        if let ast::Stmt::FnDecl(f) = s {
            if let Some(id) = &f.name {
                out.push(id.to_string());
            }
        }
    }
    out
}

pub fn compile_eval(
    prog: &ast::Program,
    source: &str,
    force_strict: bool,
    force_new_target_ok: bool,
    inherit_super: Option<EvalClassCtx>,
    ban_arguments: bool,
    visible_privates: std::collections::HashSet<String>,
    is_module: bool,
    inherit_super_obj: bool,
    caller_scope: Vec<String>,
    // The subset of `caller_scope` that belongs to an ENCLOSING function of the
    // caller rather than to the caller's own activation — readable, but not the
    // eval's variable environment (see `Compiler::eval_caller_var`).
    caller_outer_scope: Vec<String>,
    // The subset of `caller_scope` that is a CATCH PARAMETER of the caller's
    // activation: an eval'd `var` of that name still declares into the caller's
    // varEnv (Annex B.3.5 — so the name sits in `caller_outer_scope` too, making
    // `eval_caller_var` false), but the eval body's reads/writes resolve to the
    // param CELL (it is lexically nearer than the new varEnv binding), never to
    // the caller's dynamic EvalScope.
    caller_catch_params: Vec<String>,
    fn_var_env: bool,
    // CreateDynamicFunction compile: suppress the "anonymous" wrapper's
    // self-name binding (see `Compiler::fn_ctor_no_self_name`).
    fn_ctor: bool,
) -> R<Program> {
    // MODULE early errors (ModuleDeclarationInstantiation): duplicate
    // LexicallyDeclaredNames (let/const/class AND top-level function — module
    // top-level functions are LEXICAL), or a lexical name colliding with any
    // VarDeclaredName (deep), is a SyntaxError before any evaluation.
    if is_module {
        let mut vars = std::collections::HashSet::new();
        for s in &prog.body {
            collect_hoisted_vars(s, &mut vars);
        }
        let mut lexical: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut add_lexical =
            |n: String, lexical: &mut std::collections::HashSet<String>| -> Result<(), String> {
                if !lexical.insert(n.clone()) || vars.contains(&n) {
                    return Err(format!("duplicate declaration of '{n}' in module code"));
                }
                Ok(())
            };
        // `export <decl>` keeps the declaration it wraps, so the exported form
        // goes through the same three cases as a bare one.
        let mut check_decl = |d: &ast::Stmt,
                              lexical: &mut std::collections::HashSet<String>|
         -> Result<(), String> {
            match d {
                ast::Stmt::VarDecl(vd) if vd.kind.is_lexical() => {
                    let mut names = std::collections::HashSet::new();
                    for decl in &vd.decls {
                        capture::collect_pattern_names(&decl.id, &mut names);
                    }
                    for n in names {
                        add_lexical(n, lexical)?;
                    }
                }
                ast::Stmt::FnDecl(f) => {
                    if let Some(id) = &f.name {
                        add_lexical(id.to_string(), lexical)?;
                    }
                }
                ast::Stmt::ClassDecl(cd) => {
                    if let Some(id) = &cd.name {
                        add_lexical(id.to_string(), lexical)?;
                    }
                }
                _ => {}
            }
            Ok(())
        };
        for s in &prog.body {
            match s {
                ast::Stmt::VarDecl(d) if d.kind.is_lexical() => {
                    let mut names = std::collections::HashSet::new();
                    for decl in &d.decls {
                        capture::collect_pattern_names(&decl.id, &mut names);
                    }
                    for n in names {
                        add_lexical(n, &mut lexical)?;
                    }
                }
                ast::Stmt::FnDecl(f) => {
                    if let Some(id) = &f.name {
                        add_lexical(id.to_string(), &mut lexical)?;
                    }
                }
                ast::Stmt::ClassDecl(cd) => {
                    if let Some(id) = &cd.name {
                        add_lexical(id.to_string(), &mut lexical)?;
                    }
                }
                // The named-with-declaration and default forms are one statement
                // variant now; `export {…}` / `export * from …` declare nothing
                // locally, so they still fall through.
                ast::Stmt::Export(e) => match &**e {
                    ast::ExportDecl::Decl(d) => check_decl(&**d, &mut lexical)?,
                    ast::ExportDecl::Default(k) => match k {
                        ast::ExportDefault::Function(f) => {
                            if let Some(id) = &f.name {
                                add_lexical(id.to_string(), &mut lexical)?;
                            }
                        }
                        ast::ExportDefault::Class(cd) => {
                            if let Some(id) = &cd.name {
                                add_lexical(id.to_string(), &mut lexical)?;
                            }
                        }
                        ast::ExportDefault::Expr(_) => {}
                    },
                    _ => {}
                },
                _ => {}
            }
        }
    }
    let mut c = Compiler::new(source.to_string());
    c.eval_mode = true;
    c.eval_locals = !is_module;
    c.fn_ctor_no_self_name = fn_ctor;
    // A module body is an ASYNC context: top-level `await` compiles and the
    // activation returns its body promise (read by the loader). No-await
    // bodies still complete synchronously.
    c.module_mode = is_module;
    // A sloppy FUNCTION-context eval declares its var/function names into the
    // caller's dynamic EvalScope (never globals): record them so the var
    // hoist skips them and their accesses compile to the Dyn global ops.
    if fn_var_env && !(force_strict || has_use_strict(&prog.directives)) {
        c.eval_fn_context = true;
        // A name bound only by an ENCLOSING function of the caller is NOT the
        // eval's varEnv, so the declaration still goes into the EvalScope.
        let caller_var = |n: &str| {
            caller_scope.iter().any(|c| c == n) && !caller_outer_scope.iter().any(|c| c == n)
        };
        for n in eval_var_and_fn_names(prog) {
            if !caller_var(&n) {
                c.eval_dynamic_names.insert(n);
            }
        }
        // Annex B.3.3: a BLOCK-level function declaration in this sloppy eval
        // also creates a function-scoped binding — in the CALLER's EvalScope,
        // exactly like the eval's top-level vars (never a realm global).
        let mut blockers = std::collections::HashSet::new();
        for s in &prog.body {
            match s {
                ast::Stmt::VarDecl(d) if d.kind.is_lexical() => {
                    for decl in &d.decls {
                        capture::collect_pattern_names(&decl.id, &mut blockers);
                    }
                }
                ast::Stmt::ClassDecl(cd) => {
                    if let Some(id) = &cd.name {
                        blockers.insert(id.to_string());
                    }
                }
                _ => {}
            }
        }
        let mut b33 = std::collections::HashSet::new();
        for s in &prog.body {
            collect_b33_block_fns(s, false, &blockers, &mut b33);
        }
        for n in b33 {
            if !caller_var(&n) {
                c.eval_dynamic_names.insert(n);
            }
        }
    }
    c.eval_caller_scope = caller_scope;
    c.eval_outer_scope = caller_outer_scope;
    c.eval_catch_params = caller_catch_params;
    // DIRECT EVAL INSIDE `with`: the caller's eval-site map threads each active
    // with-object's hidden cell binding (" with-object-N") along with the
    // ordinary caller bindings, listed INNERMOST-FIRST. ResolveBinding for the
    // eval program's identifiers must probe those objects (HasProperty —
    // observable through a Proxy `has` trap) before the caller bindings /
    // globals, so seed the eval ROOT's inherited_with_shadows exactly like a
    // nested closure's: per free-or-var-declared name, the chain of with-objects
    // that shadow it. Map ORDER encodes nesting — a with-object shadows
    // precisely the caller bindings listed AFTER it; a name with no caller
    // binding (a global, or a sloppy eval var that lives in the EvalScope) is
    // shadowed by the entire chain. Names the eval binds LEXICALLY become root
    // locals, and with_obj_regs' bound-here check skips their chains.
    if !is_module {
        let with_chain: Vec<(usize, String)> = c
            .eval_caller_scope
            .iter()
            .enumerate()
            .filter(|(_, n)| n.starts_with(" with-object-"))
            .map(|(i, n)| (i, n.clone()))
            .collect();
        if !with_chain.is_empty() {
            let mut names = capture::free_vars(&[], &prog.body);
            for n in eval_var_and_fn_names(prog) {
                names.insert(n);
            }
            let mut map = std::collections::HashMap::new();
            for name in names {
                if name.starts_with(" with-object-") {
                    continue;
                }
                let pos = c.eval_caller_scope.iter().position(|n| *n == name);
                let chain: Vec<String> = with_chain
                    .iter()
                    .filter(|(i, _)| pos.map_or(true, |p| *i < p))
                    .map(|(_, n)| n.clone())
                    .collect();
                if !chain.is_empty() {
                    map.insert(name, chain);
                }
            }
            c.pending_with_shadows = map;
        }
    }
    // A STRICT eval (strict caller or "use strict" source) gets its own
    // discarded variable environment: top-level var/function decls are frame
    // locals, never realm globals.
    c.script_binds_globals = !(c.eval_locals && (force_strict || has_use_strict(&prog.directives)));
    c.eval_inherit_super_obj = inherit_super_obj;
    c.force_strict = force_strict;
    c.force_new_target_ok = force_new_target_ok;
    // A direct eval from a class-member context inherits the caller's home
    // class: `super.x` in the eval'd top level (and its arrows) compiles
    // against the u32::MAX SENTINEL, which prepare_eval_program remaps to the
    // caller's runtime class id. Plain nested functions still reset it.
    // The caller class's inner NAME rides the same sentinel, so `eval("C")`
    // inside a class element yields the class value rather than resolving to
    // (and, in a static field initializer, tripping the TDZ of) the outer
    // binding.
    // (`name` is already None when something at the call site shadows it — see
    // `class_inner_name_visible`.)
    if let Some(n) = inherit_super.as_ref().and_then(|cx| cx.name.clone()) {
        c.class_names.push((n, u32::MAX));
    }
    c.eval_inherit_super = inherit_super;
    c.in_field_init = ban_arguments;
    c.eval_visible_privates = visible_privates;
    c.compile(prog)?;
    for (i, f) in c.functions.iter_mut().enumerate() {
        rewrite_string_accumulators(f, i == 0);
        rewrite_local_accumulators(f);
        rewrite_append_indexes(f);
    }
    let module_decl_globals = c.collect_module_decl_globals();
    Ok(Program {
        functions: c.functions,
        global_count: c.globals.len() as u32,
        classes: c.classes,
        global_names: c.globals,
        hoisted_globals: c.hoisted_globals,
        // SORTED, not raw HashSet iteration order. std's HashSet reseeds its
        // hasher per process, so these came out in a different order on every
        // run — and the VM creates global-object properties by walking them, so
        // `Object.getOwnPropertyNames(globalThis)` permuted run to run. It also
        // made the compiler nondeterministic, which rules out comparing
        // bytecode between two front ends. Slots are handed out in order of
        // first mention, so sorting by slot is a stable, source-derived order.
        decl_globals: sorted_slots(&c.decl_globals),
        lexical_globals: sorted_slots(&c.lexical_globals),
        const_globals: sorted_slots(&c.const_globals),
        eval_dynamic_names: sorted_names(&c.eval_dynamic_names),
        module_exports: std::mem::take(&mut c.module_exports),
        module_has_imports: c.module_has_imports,
        module_reexports: std::mem::take(&mut c.module_reexports),
        module_star_reexports: std::mem::take(&mut c.module_star_reexports),
        module_ns_reexports: std::mem::take(&mut c.module_ns_reexports),
        module_imports: std::mem::take(&mut c.module_imports),
        module_decl_globals,
    })
}

/// Global slots as a sorted `Vec`, so a `HashSet`'s per-process iteration order
/// never reaches the emitted `Program`. Slots are assigned in order of first
/// mention, so slot order is a deterministic, source-derived order.
fn sorted_slots(set: &std::collections::HashSet<u32>) -> Vec<u32> {
    let mut v: Vec<u32> = set.iter().copied().collect();
    v.sort_unstable();
    v
}

/// As `sorted_slots`, for the name-keyed set. These are looked up, never
/// replayed in order, so alphabetical is only about determinism.
fn sorted_names(set: &std::collections::HashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = set.iter().cloned().collect();
    v.sort_unstable();
    v
}
