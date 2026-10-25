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
    compile_program_inner(prog, source, false, false, false)
}

/// [`compile_program`] for a script whose top level hands back its completion
/// value, as ScriptEvaluation does: what `$262.evalScript` returns. The
/// script's declarations bind as a script's, not an eval's.
pub fn compile_script_with_completion(prog: &ast::Program, source: &str) -> R<Program> {
    compile_program_inner(prog, source, false, false, true)
}

/// Compile the ROOT program of a VM — one that `Vm::new` will run directly and
/// that can never be installed through `prepare_eval_program`. Only this goal
/// is eligible for lowerings the VM-lifetime plan-cap degradation cannot
/// rewrite (currently the one-step `FinalizeObject` literal): an over-cap
/// eval/module install rewrites `NewPlannedObject` sites to legacy hints and
/// clears the plan pool, which a finalize site cannot survive. Compiling
/// dynamic code with the plain entries instead is always safe — it only
/// forgoes the optimization.
pub fn compile_main_program(prog: &ast::Program, source: &str) -> R<Program> {
    compile_program_inner(prog, source, false, true, false)
}

/// [`compile_main_program`] for a source that is not all the host's (an
/// embedding preamble ahead of it): call sites record the host's positions.
pub(crate) fn compile_main_program_with_origin(
    prog: &ast::Program,
    source: &str,
    origin: SourceOrigin,
) -> R<Program> {
    compile_program_with(prog, source, false, true, false, Some(origin))
}

/// Compile a MODULE as the program entry: the top level is an async context
/// (top-level `await`), and the VM runs func 0 as an async activation.
#[cfg_attr(feature = "wasm-no-fs-loader", allow(dead_code))]
pub fn compile_module(prog: &ast::Program, source: &str) -> R<Program> {
    compile_program_inner(prog, source, true, false, false)
}

/// [`compile_main_program`]'s module sibling — for a module program `Vm::new`
/// runs directly as the root activation (never via the loader's eval install).
pub fn compile_main_module(prog: &ast::Program, source: &str) -> R<Program> {
    compile_program_inner(prog, source, true, true, false)
}

pub(crate) fn compile_program_inner(
    prog: &ast::Program,
    source: &str,
    module_mode: bool,
    main_goal: bool,
    script_completion: bool,
) -> R<Program> {
    compile_program_with(prog, source, module_mode, main_goal, script_completion, None)
}

fn compile_program_with(
    prog: &ast::Program,
    source: &str,
    module_mode: bool,
    main_goal: bool,
    script_completion: bool,
    origin: Option<SourceOrigin>,
) -> R<Program> {
    // `ZIPP_LAZY=verify`: compile with every body where it is met (seeded) and
    // with bodies deferred, and report how the two differ.
    if super::lazy::lazy_mode() == super::lazy::LazyMode::Verify {
        use super::lazy::LazyMode;
        let origin_is_none = origin.is_none();
        let (a, _) = compile_program_mode(prog, source, module_mode, main_goal, script_completion, origin.clone(), LazyMode::Seeded)?;
        let (b, misses) = compile_program_mode(prog, source, module_mode, main_goal, script_completion, origin, LazyMode::End)?;
        eprintln!("[lazy-stat] deferred {} of {} functions", LAZY_DEFERRED.with(|d| d.get()), b.functions.len());
        for d in super::lazy::diff_programs(&a, &b, &misses) {
            eprintln!("[lazy] {d}");
        }
        // A main script also compiles as a run would (stubs, reserved ids),
        // then every stub's body as its first call would.
        if !module_mode && main_goal && !script_completion && origin_is_none {
            super::lazy::take_eager_census();
            let (c, session) = compile_main_program_lazy(prog, source, 0)?;
            for (why, n, bytes) in super::lazy::take_eager_census() {
                eprintln!("[lazy-stat] eager: {why}: {n} bodies, {bytes} source bytes");
            }
            let stubs = session.as_ref().map_or(0, |s| s.pending_ids().len());
            let (c, notes) = match session {
                Some(mut s) => super::lazy::compile_every_stub(c, &mut s)?,
                None => (c, Vec::new()),
            };
            eprintln!("[lazy-stat] run: {stubs} stubs of {} functions", c.functions.len());
            for d in notes.iter().cloned().chain(super::lazy::diff_programs(&a, &c, &[])) {
                match d.strip_prefix("stat: ") {
                    Some(stat) => eprintln!("[lazy-stat] run: {stat}"),
                    None => eprintln!("[lazy] run: {d}"),
                }
            }
        }
        return Ok(b);
    }
    let mode = super::lazy::lazy_mode();
    compile_program_mode(prog, source, module_mode, main_goal, script_completion, origin, mode).map(|(p, _)| p)
}

thread_local! {
    /// The bodies the latest compile on this thread deferred (verify mode's count).
    static LAZY_DEFERRED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many bodies the latest compile on this thread deferred.
#[cfg(test)]
pub(super) fn last_deferred() -> usize {
    LAZY_DEFERRED.with(|d| d.get())
}

/// [`compile_program_with`] in deferred-compile mode `mode`, with the names a
/// deferred body resolved only after it was deferred (`Compiler::lazy_misses`).
#[allow(clippy::too_many_arguments)]
pub(super) fn compile_program_mode(
    prog: &ast::Program,
    source: &str,
    module_mode: bool,
    main_goal: bool,
    script_completion: bool,
    origin: Option<SourceOrigin>,
    mode: super::lazy::LazyMode,
) -> R<(Program, Vec<String>)> {
    let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::Compile);
    let mut c = Compiler::new(source);
    c.lazy_mode = mode;
    c.origin = origin;
    c.module_mode = module_mode;
    c.main_goal = main_goal;
    c.script_completion = script_completion;
    // The parser has already folded the directive prologue, the Module goal
    // and any caller-imposed strictness into `prog.strict`; honour it rather
    // than re-deriving strictness from the directives alone, which would
    // ignore a strictness the parser was told to apply
    // (`front::parse_script_with(.., force_strict)`, used when a preamble
    // precedes a guest whose own prologue said `"use strict"`).
    c.force_strict = prog.strict;
    c.compile(prog)?;
    c.drain_lazy(prog)?;
    let misses = std::mem::take(&mut c.lazy_misses);
    LAZY_DEFERRED.with(|d| d.set(c.lazy_deferred));
    Ok((finish_program(&mut c), misses))
}

/// A main program whose deferred bodies compile at their first call: the
/// program, its stubs in their place, and the session that compiles them
/// (`None` when nothing was deferred). A program with fewer than
/// `min_functions` functions compiles whole instead, exactly as
/// [`compile_main_program`] compiles it. `prog` must outlive the session.
pub(crate) fn compile_main_program_lazy<'a>(
    prog: &'a ast::Program,
    source: &str,
    min_functions: usize,
) -> R<(Program, Option<super::lazy::LazySession<'a>>)> {
    let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::Compile);
    let mut functions = 0usize;
    let mut regexes = Vec::new();
    crate::parse::visit::stmts(&prog.body, &mut |n| {
        match n {
            crate::parse::visit::Node::Expr(ast::Expr::Regex { pattern, flags }) => regexes.push((pattern, flags)),
            crate::parse::visit::Node::Function(_) | crate::parse::visit::Node::Arrow(_) => functions += 1,
            _ => {}
        }
        crate::parse::visit::Flow::Go
    });
    if functions < min_functions {
        return compile_main_program(prog, source).map(|p| (p, None));
    }
    // A regular expression literal's early error is the program's however
    // long its function waits to compile, so every one is checked now.
    for (pattern, flags) in regexes {
        super::exprs::check_regex_literal(&pattern.to_lossy_string(), flags)?;
    }
    let mut c = Compiler::new(source);
    c.lazy_mode = super::lazy::LazyMode::Run;
    c.main_goal = true;
    c.force_strict = prog.strict;
    c.compile(prog)?;
    let program = finish_program(&mut c);
    // The session's compiler resolves globals as the main compile did.
    c.globals = program.global_names.clone();
    let main_globals = program.global_count;
    Ok((program, c.into_lazy_session(prog, main_globals)))
}

/// The program compiler `c` has compiled, its tables moved out of it.
fn finish_program(c: &mut Compiler) -> Program {
    for (i, f) in c.functions.iter_mut().enumerate() {
        rewrite_string_accumulators(f, i == 0);
        rewrite_local_accumulators(f);
        rewrite_append_indexes(f);
    }
    let module_decl_globals = c.collect_module_decl_globals();
    Program {
        functions: std::mem::take(&mut c.functions).into(),
        global_count: c.globals.len() as u32,
        classes: std::mem::take(&mut c.classes),
        global_names: std::mem::take(&mut c.globals),
        hoisted_globals: std::mem::take(&mut c.hoisted_globals),
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
        module_reexports: std::mem::take(&mut c.module_reexports),
        module_star_reexports: std::mem::take(&mut c.module_star_reexports),
        module_ns_reexports: std::mem::take(&mut c.module_ns_reexports),
        module_imports: std::mem::take(&mut c.module_imports),
        module_decl_globals,
        python_natives: false,
        python_lazy: None,
    }
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
    let mut vars = rustc_hash::FxHashSet::default();
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
    visible_privates: rustc_hash::FxHashSet<String>,
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
    // An embedder's expression wrapped in its own code
    // (`ScriptState::eval_wrapped_in_context`): see `SourceOrigin`.
    origin: Option<SourceOrigin>,
) -> R<Program> {
    let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::Compile);
    // MODULE early errors (ModuleDeclarationInstantiation): duplicate
    // LexicallyDeclaredNames (let/const/class AND top-level function — module
    // top-level functions are LEXICAL), or a lexical name colliding with any
    // VarDeclaredName (deep), is a SyntaxError before any evaluation.
    if is_module {
        let mut vars = rustc_hash::FxHashSet::default();
        for s in &prog.body {
            collect_hoisted_vars(s, &mut vars);
        }
        let mut lexical: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
        let add_lexical =
            |n: String, lexical: &mut rustc_hash::FxHashSet<String>| -> Result<(), String> {
                if !lexical.insert(n.clone()) || vars.contains(&n) {
                    return Err(format!("duplicate declaration of '{n}' in module code"));
                }
                Ok(())
            };
        // `export <decl>` keeps the declaration it wraps, so the exported form
        // goes through the same three cases as a bare one.
        let check_decl = |d: &ast::Stmt,
                          lexical: &mut rustc_hash::FxHashSet<String>|
         -> Result<(), String> {
            match d {
                ast::Stmt::VarDecl(vd) if vd.kind.is_lexical() => {
                    let mut names = rustc_hash::FxHashSet::default();
                    for decl in &vd.decls {
                        capture::collect_pattern_names(&decl.id, &mut names);
                    }
                    // Sorted, so a repeated name is reported the same way every run.
                    for n in super::helpers::sorted_name_vec(&names) {
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
                    let mut names = rustc_hash::FxHashSet::default();
                    for decl in &d.decls {
                        capture::collect_pattern_names(&decl.id, &mut names);
                    }
                    // Sorted, as in `check_decl` above.
                    for n in super::helpers::sorted_name_vec(&names) {
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
    let mut c = Compiler::new(source);
    c.origin = origin;
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
        let mut blockers = rustc_hash::FxHashSet::default();
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
        let mut b33 = rustc_hash::FxHashSet::default();
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
            let mut map = rustc_hash::FxHashMap::default();
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
        functions: c.functions.into(),
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
        module_reexports: std::mem::take(&mut c.module_reexports),
        module_star_reexports: std::mem::take(&mut c.module_star_reexports),
        module_ns_reexports: std::mem::take(&mut c.module_ns_reexports),
        module_imports: std::mem::take(&mut c.module_imports),
        module_decl_globals,
        python_natives: false,
        python_lazy: None,
    })
}

/// Global slots as a sorted `Vec`, so a `HashSet`'s per-process iteration order
/// never reaches the emitted `Program`. Slots are assigned in order of first
/// mention, so slot order is a deterministic, source-derived order.
fn sorted_slots(set: &rustc_hash::FxHashSet<u32>) -> Vec<u32> {
    let mut v: Vec<u32> = set.iter().copied().collect();
    v.sort_unstable();
    v
}

/// As `sorted_slots`, for the name-keyed set. These are looked up, never
/// replayed in order, so alphabetical is only about determinism.
fn sorted_names(set: &rustc_hash::FxHashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = set.iter().cloned().collect();
    v.sort_unstable();
    v
}
