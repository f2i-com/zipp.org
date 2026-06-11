//! Compile the oxc AST directly to register bytecode.
//!
//! Two passes over the source:
//!
//! 1. **Hoist**: every top-level `function f(...)` and `var/let f = function`
//!    name is assigned a global slot, so calls resolve regardless of textual
//!    order (matching JS function hoisting) and recursion works.
//! 2. **Emit**: each function body compiles to a `FuncProto`. Locals and
//!    parameters live in registers, tracked by a `Scope` (name → register).
//!    Expression results flow into caller-chosen destination registers, which
//!    is what lets a value stay in one place across a basic block.
//!
//! The supported subset (v1): numbers/strings/bools/null, `let`/`const`/`var`,
//! function declarations + calls + recursion, `if/else`, `while`, C-style
//! `for`, `return`, the arithmetic/comparison/logical operators, and
//! `console.log`. Anything else is a clear compile error — coverage grows over
//! time, the same way the old engine did.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use oxc_ast::ast as ox;

use crate::bytecode::{
    BitwiseOp, ClassDef, FuncProto, InstanceCtor, Instr, Program, Reg, UpvalSource,
};
use crate::capture;
use crate::value::Value;
use crate::vm::STRING_CONST_BIT;

type R<T> = Result<T, String>;

/// Shared, mutable upvalue list of a function: (name, where-the-cell-comes-from).
/// Shared via `Rc<RefCell>` so a deeply-nested function can append upvalues to
/// an ancestor (transitive capture): an intermediate function that only *passes
/// through* a variable still needs to capture it so its child can re-source it.
type UpvalList = Rc<RefCell<Vec<(String, UpvalSource)>>>;

/// A snapshot of an enclosing function's binding environment, used by a nested
/// function to resolve free variables to upvalues. The chain is ordered
/// outermost → innermost-parent; the last entry is the direct parent. The
/// `upvalues` handle is SHARED with the live ancestor `FnCompiler`, so resolving
/// a variable through this snapshot also records the upvalue on the ancestor.
#[derive(Clone)]
struct EnclosingFn {
    /// name → register holding that binding's CELL in the parent frame. Only
    /// captured bindings appear here (non-captured locals can't be upvalues).
    cell_locals: Vec<(String, Reg)>,
    /// The ancestor's own upvalue list (shared handle).
    upvalues: UpvalList,
}

/// Compile a parsed program into bytecode.
/// Is constant Value `v` a (pending) string literal? String constants are encoded
/// as `Value::heap(STRING_CONST_BIT | si)` (see `add_string_const`).
fn is_string_const(v: Value) -> bool {
    v.is_heap() && (v.heap_index() & STRING_CONST_BIT) != 0
}

/// Does global slot `g`'s last write BEFORE `before` initialise it to a string
/// literal? Recognises the `s = "…"` shape the compiler emits as an adjacent
/// `LoadConst{dst:r}; StoreGlobal{idx:g, src:r}`.
fn global_inits_string(code: &[Instr], constants: &[Value], g: u32, before: usize) -> bool {
    let mut store_ip = None;
    for (ip, instr) in code.iter().enumerate().take(before) {
        if let Instr::StoreGlobal { idx, .. } = *instr {
            if idx == g {
                store_ip = Some(ip);
            }
        }
    }
    let sp = match store_ip {
        Some(s) if s >= 1 => s,
        _ => return false,
    };
    let src = match code[sp] {
        Instr::StoreGlobal { src, .. } => src,
        _ => return false,
    };
    match code[sp - 1] {
        Instr::LoadConst { dst, idx } if dst == src => {
            constants.get(idx as usize).copied().map(is_string_const).unwrap_or(false)
        }
        _ => false,
    }
}

/// Does instruction `i` reference register `r` in ANY of its register fields?
/// CONSERVATIVE: handles the simple op set a string-accumulator loop body is
/// restricted to (see `loop_inplace_safe`); any other variant returns `true`
/// (assume it touches `r`), so an unrecognised op can never let the buffer escape
/// unnoticed.
fn instr_touches(i: &Instr, r: Reg) -> bool {
    match *i {
        Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } | Instr::LoadGlobal { dst, .. } => {
            dst == r
        }
        Instr::Move { dst, src } => dst == r || src == r,
        Instr::StoreGlobal { src, .. } | Instr::StoreGlobalStrict { src, .. } => src == r,
        Instr::AddInt { dst, a, .. } | Instr::Neg { dst, a } => dst == r || a == r,
        Instr::Add { dst, a, b }
        | Instr::Sub { dst, a, b }
        | Instr::Mul { dst, a, b }
        | Instr::Div { dst, a, b }
        | Instr::Mod { dst, a, b }
        | Instr::StrConcat { dst, a, b }
        | Instr::StrAppendInPlace { dst, a, b }
        | Instr::Lt { dst, a, b }
        | Instr::Le { dst, a, b }
        | Instr::Gt { dst, a, b }
        | Instr::Ge { dst, a, b }
        | Instr::Eq { dst, a, b }
        | Instr::Ne { dst, a, b } => dst == r || a == r || b == r,
        Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => cond == r,
        Instr::JumpIfNotLt { a, b, .. } | Instr::JumpIfNotLe { a, b, .. } => a == r || b == r,
        Instr::Jump { .. } => false,
        _ => true, // unrecognised op → assume it could reference `r`
    }
}

/// Is op `i` one of the simple, no-user-code, no-implicit-global-access ops a
/// linear string-accumulator loop body may contain? (Calls, heap property/index
/// ops, closures, etc. could read/alias the accumulator global, so they bar the
/// in-place rewrite.)
fn is_simple_loop_op(i: &Instr) -> bool {
    matches!(
        i,
        Instr::LoadInt { .. }
            | Instr::LoadConst { .. }
            | Instr::Move { .. }
            | Instr::LoadGlobal { .. }
            | Instr::StoreGlobal { .. }
            | Instr::StoreGlobalStrict { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Div { .. }
            | Instr::Mod { .. }
            | Instr::AddInt { .. }
            | Instr::Neg { .. }
            | Instr::Lt { .. }
            | Instr::Le { .. }
            | Instr::Gt { .. }
            | Instr::Ge { .. }
            | Instr::Eq { .. }
            | Instr::Ne { .. }
            | Instr::Jump { .. }
            | Instr::JumpIfFalse { .. }
            | Instr::JumpIfTrue { .. }
            | Instr::JumpIfNotLt { .. }
            | Instr::JumpIfNotLe { .. }
    )
}

/// Can the string accumulator `g` (loaded at `load_ip`, appended at `k`, stored at
/// `store_ip`) in loop `[start, end]` be mutated IN PLACE soundly? PROVES the
/// accumulator's buffer is never aliased, so an in-place append is unobservable:
///  - top-level script function (runs once — a global accumulator must not persist
///    into a second call that would mutate a returned/escaped buffer);
///  - loop not nested in another loop (so post-loop code runs once, no re-entry);
///  - `g` is written ONLY by its init (before the loop) and this loop's store, read
///    ONLY by this loop's load (post-loop reads are fine — building is done);
///  - the body contains only simple ops (no calls/heap ops that could read `g`);
///  - the loaded value `a` and the result `dst` (the buffer's registers) are
///    touched ONLY by the three accumulator ops — never copied/stored elsewhere,
///    so the buffer can't leak into a second live reference.
#[allow(clippy::too_many_arguments)]
fn loop_inplace_safe(
    code: &[Instr],
    g: u32,
    start: usize,
    end: usize,
    is_top_level: bool,
    load_ip: usize,
    k: usize,
    store_ip: usize,
    a: Reg,
    dst: Reg,
) -> bool {
    if !is_top_level {
        return false;
    }
    // Not nested in another back-edge loop.
    for (jp, instr) in code.iter().enumerate() {
        if let Instr::Jump { target } = *instr {
            let t = target as usize;
            if t < jp && (t, jp) != (start, end) && t <= start && jp >= end {
                return false;
            }
        }
    }
    // `g` access discipline across the WHOLE function.
    let (mut st_before, mut st_in, mut st_after) = (0u32, 0u32, 0u32);
    let (mut ld_before, mut ld_in) = (0u32, 0u32);
    for (ip, instr) in code.iter().enumerate() {
        match *instr {
            Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } if idx == g => {
                if ip < start {
                    st_before += 1
                } else if ip <= end {
                    st_in += 1
                } else {
                    st_after += 1
                }
            }
            Instr::LoadGlobal { idx, .. } if idx == g => {
                if ip < start {
                    ld_before += 1
                } else if ip <= end {
                    ld_in += 1
                }
            }
            _ => {}
        }
    }
    if st_before != 1 || st_in != 1 || st_after != 0 || ld_before != 0 || ld_in != 1 {
        return false;
    }
    // Only simple ops in the body, and the buffer's registers (`a`, `dst`) are
    // touched ONLY by the load/append/store — never leaking the buffer elsewhere.
    for ip in start..=end {
        if !is_simple_loop_op(&code[ip]) {
            return false;
        }
        if ip != load_ip && ip != k && ip != store_ip && (instr_touches(&code[ip], a) || instr_touches(&code[ip], dst)) {
            return false;
        }
    }
    true
}

/// Rewrite a string-accumulator loop's `g = g + b` so it JITs. Always emits
/// `StrConcat` (a JIT routing hint, semantically identical to `Add`) when `g` is
/// initialised to a string literal; UPGRADES to `StrAppendInPlace` (mutates the
/// buffer, no per-element allocation) when `loop_inplace_safe` PROVES the
/// accumulator is never aliased. Numeric accumulators (`sum += x`) keep `Add` and
/// stay on the faster integer region. `is_top_level` is true for the script body.
fn rewrite_string_accumulators(f: &mut FuncProto, is_top_level: bool) {
    let n = f.code.len();
    // (ip, in_place)
    let mut rewrites: Vec<(usize, bool)> = Vec::new();
    for j in 0..n {
        let start = match f.code[j] {
            Instr::Jump { target } if (target as usize) < j => target as usize,
            _ => continue,
        };
        for k in start..=j {
            let (dst, a) = match f.code[k] {
                Instr::Add { dst, a, .. } => (dst, a),
                _ => continue,
            };
            // result `dst` stored back to some global `g` in the body
            let store_ip = (start..=j).find(|&m| {
                matches!(f.code[m], Instr::StoreGlobal { src, .. } | Instr::StoreGlobalStrict { src, .. } if src == dst)
            });
            let (g, store_ip) = match store_ip {
                Some(m) => match f.code[m] {
                    Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } => (idx, m),
                    _ => continue,
                },
                None => continue,
            };
            // operand `a` loaded from that same global `g` in the body
            let load_ip = (start..=j).find(|&m| {
                matches!(f.code[m], Instr::LoadGlobal { dst: ld, idx } if ld == a && idx == g)
            });
            let load_ip = match load_ip {
                Some(m) => m,
                None => continue,
            };
            if !global_inits_string(&f.code, &f.constants, g, start) {
                continue;
            }
            let in_place =
                loop_inplace_safe(&f.code, g, start, j, is_top_level, load_ip, k, store_ip, a, dst);
            rewrites.push((k, in_place));
        }
    }
    for (k, in_place) in rewrites {
        if let Instr::Add { dst, a, b } = f.code[k] {
            f.code[k] = if in_place {
                Instr::StrAppendInPlace { dst, a, b }
            } else {
                Instr::StrConcat { dst, a, b }
            };
        }
    }
}


/// The `type` import attribute of an import/export-from declaration's
/// `with { ... }` clause, if present.
fn with_clause_type(wc: &Option<oxc_allocator::Box<ox::WithClause>>) -> Option<String> {
    let wc = wc.as_ref()?;
    for e in &wc.with_entries {
        let key = match &e.key {
            ox::ImportAttributeKey::Identifier(id) => id.name.as_str(),
            ox::ImportAttributeKey::StringLiteral(s) => s.value.as_str(),
        };
        if key == "type" {
            return Some(e.value.value.to_string());
        }
    }
    None
}

pub fn compile_program(prog: &ox::Program, source: &str) -> R<Program> {
    compile_program_inner(prog, source, false)
}

/// Compile a MODULE as the program entry: the top level is an async context
/// (top-level `await`), and the VM runs func 0 as an async activation.
pub fn compile_module(prog: &ox::Program, source: &str) -> R<Program> {
    compile_program_inner(prog, source, true)
}

fn compile_program_inner(prog: &ox::Program, source: &str, module_mode: bool) -> R<Program> {
    let mut c = Compiler::new(source.to_string());
    c.module_mode = module_mode;
    c.compile(prog)?;
    for (i, f) in c.functions.iter_mut().enumerate() {
        rewrite_string_accumulators(f, i == 0);
    }
    let module_decl_globals = c.collect_module_decl_globals();
    Ok(Program {
        functions: c.functions,
        global_count: c.globals.len() as u32,
        classes: c.classes,
        global_names: c.globals,
        hoisted_globals: c.hoisted_globals,
        decl_globals: c.decl_globals.iter().copied().collect(),
        lexical_globals: c.lexical_globals.iter().copied().collect(),
        const_globals: c.const_globals.iter().copied().collect(),
        eval_dynamic_names: c.eval_dynamic_names.iter().cloned().collect(),
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
    fn private_name_declared(&self, key: &str) -> bool {
        self.private_names_stack.iter().any(|v| v.iter().any(|n| n == key))
            || self.eval_visible_privates.contains(key)
    }
}

/// The top-level var + function declared names of a parsed eval body —
/// EvalDeclarationInstantiation's varNames/functionNames for collision checks.
pub fn eval_var_and_fn_names(prog: &ox::Program) -> Vec<String> {
    let mut vars = std::collections::HashSet::new();
    for s in &prog.body {
        collect_hoisted_vars(s, &mut vars);
    }
    let mut out: Vec<String> = vars.into_iter().collect();
    for s in &prog.body {
        if let ox::Statement::FunctionDeclaration(f) = s {
            if let Some(id) = &f.id {
                out.push(id.name.to_string());
            }
        }
    }
    out
}

pub fn compile_eval(
    prog: &ox::Program,
    source: &str,
    force_strict: bool,
    force_new_target_ok: bool,
    inherit_super: Option<bool>,
    ban_arguments: bool,
    visible_privates: std::collections::HashSet<String>,
    is_module: bool,
    inherit_super_obj: bool,
    caller_scope: Vec<String>,
    fn_var_env: bool,
    exact_src: Option<&[u8]>,
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
        let mut add_lexical = |n: String,
                               lexical: &mut std::collections::HashSet<String>|
         -> Result<(), String> {
            if !lexical.insert(n.clone()) || vars.contains(&n) {
                return Err(format!("duplicate declaration of '{n}' in module code"));
            }
            Ok(())
        };
        let mut check_decl = |d: &ox::Declaration,
                              lexical: &mut std::collections::HashSet<String>|
         -> Result<(), String> {
            match d {
                ox::Declaration::VariableDeclaration(vd) if vd.kind.is_lexical() => {
                    let mut names = std::collections::HashSet::new();
                    for decl in &vd.declarations {
                        capture::collect_pattern_names(&decl.id, &mut names);
                    }
                    for n in names {
                        add_lexical(n, lexical)?;
                    }
                }
                ox::Declaration::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        add_lexical(id.name.to_string(), lexical)?;
                    }
                }
                ox::Declaration::ClassDeclaration(cd) => {
                    if let Some(id) = &cd.id {
                        add_lexical(id.name.to_string(), lexical)?;
                    }
                }
                _ => {}
            }
            Ok(())
        };
        for s in &prog.body {
            match s {
                ox::Statement::VariableDeclaration(d) if d.kind.is_lexical() => {
                    let mut names = std::collections::HashSet::new();
                    for decl in &d.declarations {
                        capture::collect_pattern_names(&decl.id, &mut names);
                    }
                    for n in names {
                        add_lexical(n, &mut lexical)?;
                    }
                }
                ox::Statement::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        add_lexical(id.name.to_string(), &mut lexical)?;
                    }
                }
                ox::Statement::ClassDeclaration(cd) => {
                    if let Some(id) = &cd.id {
                        add_lexical(id.name.to_string(), &mut lexical)?;
                    }
                }
                ox::Statement::ExportNamedDeclaration(e) => {
                    if let Some(d) = &e.declaration {
                        check_decl(d, &mut lexical)?;
                    }
                }
                ox::Statement::ExportDefaultDeclaration(e) => {
                    use ox::ExportDefaultDeclarationKind as K;
                    match &e.declaration {
                        K::FunctionDeclaration(f) => {
                            if let Some(id) = &f.id {
                                add_lexical(id.name.to_string(), &mut lexical)?;
                            }
                        }
                        K::ClassDeclaration(cd) => {
                            if let Some(id) = &cd.id {
                                add_lexical(id.name.to_string(), &mut lexical)?;
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    let mut c = Compiler::new(source.to_string());
    // EXACT WTF-8 source bytes (eval of a string holding lone surrogates):
    // lets regex literals recover their exact pattern text. None (free) for
    // well-formed sources.
    c.exact_src = exact_src.map(<[u8]>::to_vec);
    c.eval_mode = true;
    c.eval_locals = !is_module;
    // A module body is an ASYNC context: top-level `await` compiles and the
    // activation returns its body promise (read by the loader). No-await
    // bodies still complete synchronously.
    c.module_mode = is_module;
    // A sloppy FUNCTION-context eval declares its var/function names into the
    // caller's dynamic EvalScope (never globals): record them so the var
    // hoist skips them and their accesses compile to the Dyn global ops.
    if fn_var_env && !(force_strict || has_use_strict(&prog.directives)) {
        c.eval_fn_context = true;
        for n in eval_var_and_fn_names(prog) {
            if !caller_scope.iter().any(|c| *c == n) {
                c.eval_dynamic_names.insert(n);
            }
        }
        // Annex B.3.3: a BLOCK-level function declaration in this sloppy eval
        // also creates a function-scoped binding — in the CALLER's EvalScope,
        // exactly like the eval's top-level vars (never a realm global).
        let mut blockers = std::collections::HashSet::new();
        for s in &prog.body {
            match s {
                ox::Statement::VariableDeclaration(d) if d.kind.is_lexical() => {
                    for decl in &d.declarations {
                        capture::collect_pattern_names(&decl.id, &mut blockers);
                    }
                }
                ox::Statement::ClassDeclaration(cd) => {
                    if let Some(id) = &cd.id {
                        blockers.insert(id.name.to_string());
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
            if !caller_scope.iter().any(|c| *c == n) {
                c.eval_dynamic_names.insert(n);
            }
        }
    }
    c.eval_caller_scope = caller_scope;
    // A STRICT eval (strict caller or "use strict" source) gets its own
    // discarded variable environment: top-level var/function decls are frame
    // locals, never realm globals.
    c.script_binds_globals =
        !(c.eval_locals && (force_strict || has_use_strict(&prog.directives)));
    c.eval_inherit_super_obj = inherit_super_obj;
    c.force_strict = force_strict;
    c.force_new_target_ok = force_new_target_ok;
    // A direct eval from a class-member context inherits the caller's home
    // class: `super.x` in the eval'd top level (and its arrows) compiles
    // against the u32::MAX SENTINEL, which prepare_eval_program remaps to the
    // caller's runtime class id. Plain nested functions still reset it.
    c.eval_inherit_super = inherit_super;
    c.in_field_init = ban_arguments;
    c.eval_visible_privates = visible_privates;
    c.compile(prog)?;
    for (i, f) in c.functions.iter_mut().enumerate() {
        rewrite_string_accumulators(f, i == 0);
    }
    let module_decl_globals = c.collect_module_decl_globals();
    Ok(Program {
        functions: c.functions,
        global_count: c.globals.len() as u32,
        classes: c.classes,
        global_names: c.globals,
        hoisted_globals: c.hoisted_globals,
        decl_globals: c.decl_globals.iter().copied().collect(),
        lexical_globals: c.lexical_globals.iter().copied().collect(),
        const_globals: c.const_globals.iter().copied().collect(),
        eval_dynamic_names: c.eval_dynamic_names.iter().cloned().collect(),
        module_exports: std::mem::take(&mut c.module_exports),
        module_has_imports: c.module_has_imports,
        module_reexports: std::mem::take(&mut c.module_reexports),
        module_star_reexports: std::mem::take(&mut c.module_star_reexports),
        module_ns_reexports: std::mem::take(&mut c.module_ns_reexports),
        module_imports: std::mem::take(&mut c.module_imports),
        module_decl_globals,
    })
}

struct Compiler {
    functions: Vec<FuncProto>,
    /// Global name → slot.
    globals: Vec<String>,
    /// Compiled class descriptors, indexed by the `MakeClass` class_id.
    classes: Vec<ClassDef>,
    /// Class name → class_id, for resolving `extends <Name>` and `super`.
    class_names: Vec<(String, u32)>,
    /// Slots for top-level `var` names — pre-initialized to `undefined` at VM
    /// startup (var hoisting), so a read before the textual decl isn't a
    /// never-declared ReferenceError.
    hoisted_globals: Vec<u32>,
    /// The full program source, kept so each function can record its exact
    /// source slice (by oxc span) for `Function.prototype.toString`.
    source: String,
    /// EXACT WTF-8 bytes of the source, present only when an eval'd code
    /// string held lone surrogates (`source` is then the LOSSY view — U+FFFD
    /// per surrogate; both encodings are 3 bytes, so byte offsets/oxc spans
    /// in `source` index `exact_src` identically). Lets a regex literal
    /// recover its exact pattern bytes. None for well-formed sources (the
    /// overwhelmingly common case) — zero cost there.
    exact_src: Option<Vec<u8>>,
    /// True when compiling an `eval` code string: the top-level script returns
    /// its *completion value* (the value of the last evaluated expression
    /// statement) instead of `undefined`.
    eval_mode: bool,
    /// Set for a DIRECT eval from a class-member context: the eval's top-level
    /// script (and its arrows) inherits the caller's home class for `super`
    /// (compiled against the u32::MAX sentinel) — Some(super_static).
    eval_inherit_super: Option<bool>,
    /// True while compiling a class FIELD INITIALIZER expression (or an eval
    /// program invoked from one): `arguments` is an early SyntaxError there.
    /// Arrows inherit (Compiler-level state); function/method bodies reset it.
    in_field_init: bool,
    /// True when compiling a MODULE as the program entry (not a dynamic import):
    /// the top-level body is an ASYNC context (top-level `await` is allowed), so
    /// func 0 is compiled with `in_async` and the VM runs it as an async activation.
    module_mode: bool,
    /// True only for a REAL eval program (do_eval): top-level lexicals are
    /// frame-locals (the spec's discarded eval lexEnv). False for modules,
    /// which also compile through compile_eval but whose top-level lexicals
    /// are live global-slot export bindings.
    eval_locals: bool,
    /// False for a STRICT eval program: its top-level var/function declarations
    /// live in the eval's own (discarded) variable environment — frame locals —
    /// instead of the realm's globals. True everywhere else.
    script_binds_globals: bool,
    /// Direct eval from an OBJECT-literal method/accessor: the eval top level
    /// (and arrows) resolve `super.x` via the caller's runtime [[HomeObject]].
    eval_inherit_super_obj: bool,
    /// For a DIRECT eval program: the caller bindings (ordered) the runtime
    /// supplies as the eval closure's upvalue cells. Free names in the eval
    /// resolve to these as UpvalGet/UpvalSet before falling back to globals.
    eval_caller_scope: Vec<String>,
    /// FUNCTION-context sloppy eval: the eval's own var/function names that
    /// live in the caller's dynamic EvalScope (the Dyn global ops find them).
    eval_dynamic_names: std::collections::HashSet<String>,
    /// True for a sloppy FUNCTION-context eval program: its global accesses
    /// must be dynamic-first (the caller activation may carry an EvalScope).
    eval_fn_context: bool,
    /// True while compiling code lexically inside a contains-direct-eval
    /// function: global accesses at ANY nesting depth compile to the Dyn ops.
    dyn_global_zone: bool,
    /// Force strict mode for the whole compilation, regardless of a `"use strict"`
    /// directive — set for a DIRECT eval invoked from strict-mode code (the
    /// evaluated string inherits the caller's strictness).
    force_strict: bool,
    /// Allow `new.target` at the eval script top level — set for a DIRECT eval
    /// invoked from inside a function/method/class-field initializer (the eval
    /// inherits the caller's new.target validity).
    force_new_target_ok: bool,
    /// Strictness of the lexical scope currently being compiled. Set on entry to
    /// a function/arrow body (inherited from the enclosing scope, OR'd with the
    /// body's own `"use strict"` directive), forced `true` inside class bodies,
    /// and seeded from module-ness for the top-level script. A function records
    /// this into `FuncProto.is_strict`; the VM uses it to decide `this`
    /// substitution at the call site.
    in_strict: bool,
    /// The enclosing-function chain that the methods of the class CURRENTLY being
    /// compiled close over (set by `compile_class`, read by `compile_class_fn`).
    /// Empty at script level, so script-level class methods keep free vars as
    /// globals. Saved/restored around each class to handle nesting.
    class_enclosing: Vec<EnclosingFn>,
    /// True while compiling the methods of a DERIVED class (`class X extends …`):
    /// gates `super(...)` (a base class's constructor calling `super()` is an early
    /// SyntaxError). `super.x` property access is allowed in ANY class method, so it
    /// uses the per-method home-class id instead. Saved/restored around each class.
    class_derived: bool,
    /// Set by the class compiler around the CONSTRUCTOR's `compile_class_fn`
    /// call only (consumed at its entry), so the ctor body — and nothing else —
    /// gets derived-ctor this-TDZ checks.
    compiling_ctor: bool,
    /// Private names declared by each enclosing class body (innermost last),
    /// pushed by compile_class / popped by build_class_into. A private access
    /// whose name no enclosing class declares is an early SyntaxError.
    private_names_stack: Vec<Vec<String>>,
    /// Class names whose HERITAGE expression is currently compiling (innermost
    /// last): the spec classScope makes a named class's own binding visible
    /// (in TDZ, immutable) throughout `extends ...`, including functions
    /// created inside it.
    heritage_classes: Vec<(String, u32)>,
    /// For DIRECT eval programs: private names lexically visible at the eval
    /// call site (the caller's brand-chain names). Empty otherwise.
    eval_visible_privates: std::collections::HashSet<String>,
    /// True while compiling a derived class's constructor BODY (and read by
    /// arrows lexically inside it). Gates `ThisCheck` emission on `this` reads
    /// and super-property references.
    in_derived_ctor: bool,
    /// Global slots bound by a top-level `const` (immutable): assignment to one
    /// is a runtime TypeError.
    const_globals: HashSet<u32>,
    /// Global slots bound by a top-level `let`/`const` (lexical) declaration. Such a
    /// binding is in its TDZ — the slot is UNINITIALIZED — until its declaration runs,
    /// so an assignment to it before then is a ReferenceError even in sloppy mode
    /// (where an *undeclared* name would instead create a global). Registered by a
    /// hoisting pre-pass so a forward reference (`for([x] of …){} let x;`) is known.
    lexical_globals: HashSet<u32>,
    /// Global slots bound by a top-level FUNCTION or CLASS declaration. Like a
    /// global `var`/`let`/`const`, these are non-configurable bindings, so
    /// `delete <name>` on one yields `false`.
    decl_globals: HashSet<u32>,
    /// True while compiling code where `new.target` is syntactically allowed —
    /// inside an ordinary function/method/constructor/class-field body, and
    /// inherited by nested arrows. False at the top level of a script or eval (a
    /// `new.target` there is an early SyntaxError). Saved/restored per function.
    new_target_ok: bool,
    /// For a MODULE compile: the (exported name, local name) pairs collected from
    /// `export` declarations, in source order. The loader reads each local's
    /// top-level (eval-global) binding after the module runs to build its
    /// namespace. Empty for scripts/eval (which have no `export`).
    module_exports: Vec<(String, String)>,
    /// True if a module has a real `import` declaration or `export * as ns from` —
    /// dependencies the loader cannot link yet, so such a module rejects the dynamic
    /// `import()`. RE-EXPORTS are recorded in the two fields below instead.
    module_has_imports: bool,
    /// `export {imported as exported} from 'spec'` re-exports: (exported, imported,
    /// specifier). Resolved by the loader against the dependency module.
    module_reexports: Vec<(String, String, String)>,
    /// `export * from 'spec'` star re-exports: the specifier string.
    module_star_reexports: Vec<String>,
    module_ns_reexports: Vec<(String, String)>,
    module_imports: Vec<crate::bytecode::ImportEntry>,
    /// Monotonic counter giving every `with`-object hidden local a UNIQUE name
    /// (" with-object-N"), so a nested closure capturing two different with
    /// scopes resolves each to the right cell across the enclosing chain.
    with_name_counter: u32,
    /// Transient (consumed by `FnCompiler::new`): for each free name of the
    /// function about to be compiled, the ordered (innermost-first) chain of
    /// enclosing with-object binding names that may shadow it at runtime.
    /// Stashed by `stash_child_with_shadows` right before the nested compile.
    pending_with_shadows: std::collections::HashMap<String, Vec<String>>,
    /// Transient: set by the object-literal compiler right before compiling a concise
    /// method / get-set accessor value, so that function's body compiles with
    /// `super_home_obj = true` (object-method super). Consumed (taken) when the
    /// function body starts compiling.
    obj_method_super: bool,
}

impl Compiler {
    fn new(source: String) -> Compiler {
        Compiler {
            functions: Vec::new(),
            globals: Vec::new(),
            classes: Vec::new(),
            class_names: Vec::new(),
            hoisted_globals: Vec::new(),
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
    fn collect_module_decl_globals(&self) -> Vec<u32> {
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
        v
    }

    /// Slice the program source by a function node's byte span, for
    /// `Function.prototype.toString`. Empty if the range is degenerate or not on
    /// a UTF-8 boundary (then `toString` uses the native-function fallback).
    fn src_slice(&self, start: u32, end: u32) -> String {
        self.source
            .get(start as usize..end as usize)
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    fn global_slot(&mut self, name: &str) -> u16 {
        if let Some(i) = self.globals.iter().position(|g| g == name) {
            return i as u16;
        }
        let i = self.globals.len() as u16;
        self.globals.push(name.to_string());
        i
    }

    fn compile(&mut self, prog: &ox::Program) -> R<()> {
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
            for name in vars {
                // A name the CALLER binds is not a global var of the eval —
                // the declaration is a no-op and assignments write the cell.
                // A dynamic (EvalScope) name is never a global either.
                if self.eval_caller_scope.iter().any(|n| *n == name)
                    || self.eval_dynamic_names.contains(&name)
                {
                    continue;
                }
                let slot = self.global_slot(&name) as u32;
                if !self.hoisted_globals.contains(&slot) {
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
    fn compile_function_body(
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
            if capture::nested_uses_arguments(body) {
                fc.uses_arguments = true;
                let r = fc.arguments_reg.unwrap();
                fc.emit(Instr::MakeCell { reg: r });
                fc.cell_regs.insert(r);
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
                        if !fc.cx.hoisted_globals.contains(&slot) {
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
                        // namespace; `import source` (and other phase forms)
                        // stay load-only.
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
                            if let Some(local) = defer_ns_local {
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
            for name in &lex {
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
            for name in &hv {
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
            for name in &lex {
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
        if !is_script || !fc.cx.script_binds_globals {
            for s in body {
                if let ox::Statement::FunctionDeclaration(f) = s {
                    fc.func_decl(f)?;
                }
            }
        }

        if FnCompiler::block_has_using(body) {
            // A function/generator/async body with a top-level `using` disposes its
            // resources on return/throw — same finally desugar as a block.
            fc.compile_using_block(body, !is_script)?;
        } else {
            for s in body {
                // Top-level function declarations were materialised at entry above.
                if !is_script || !fc.cx.script_binds_globals {
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
    fn compile_class_fn(
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
            fc.uses_arguments = true;
            let r = fc.arguments_reg.unwrap();
            fc.emit(Instr::MakeCell { reg: r });
            fc.cell_regs.insert(r);
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
    fn compile_arrow_body(
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
                for name in &hv {
                    if !fc.scopes[0].iter().any(|(n, _)| n == name) {
                        fc.declare_local(name);
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

/// Resolve `name` to an `UpvalSource` for the INNERMOST function whose enclosing
/// chain is `chain` (outermost → direct parent). The direct parent is
/// `chain.last()`. If the variable is a cell-local of the direct parent, the
/// source is `ParentLocal`. Otherwise the parent must itself capture it as one
/// of its upvalues — found, or recursively created by capturing from the
/// grandparent — and the source is `ParentUpval(parent's upvalue index)`. This
/// is the standard Lua "find/create upvalue" walk, threading a capture through
/// every intermediate level. `None` if no enclosing function binds `name`.
fn capture_source(chain: &[EnclosingFn], name: &str) -> Option<UpvalSource> {
    let (parent, outer) = chain.split_last()?;
    // Direct parent holds the cell as a local.
    if let Some((_, reg)) = parent.cell_locals.iter().find(|(n, _)| n == name) {
        return Some(UpvalSource::ParentLocal(*reg));
    }
    // Parent already captured it as an upvalue.
    if let Some(i) = parent.upvalues.borrow().iter().position(|(n, _)| n == name) {
        return Some(UpvalSource::ParentUpval(i as u16));
    }
    // Otherwise make the parent capture it from ITS enclosing chain, then point
    // at the parent's freshly-added upvalue.
    let src_for_parent = capture_source(outer, name)?;
    let mut pups = parent.upvalues.borrow_mut();
    let idx = pups.len() as u16;
    pups.push((name.to_string(), src_for_parent));
    Some(UpvalSource::ParentUpval(idx))
}

/// A method's `Function.prototype.toString` source. For a `static` member the
/// `static` keyword is part of the ClassElement, not the MethodDefinition, so
/// it is excluded from the method's [[SourceText]] (e.g. `static s(){}` →
/// `s(){}`, `static get x(){}` → `get x(){}`). When `is_static` the slice begins
/// with the `static` keyword (the parser flagged it), so strip it + trailing ws.
fn method_source(text: String, is_static: bool) -> String {
    if is_static {
        if let Some(rest) = text.strip_prefix("static") {
            // The MethodDefinition's [[SourceText]] starts at its first real token
            // (name / get / set / async / * / [). Whitespace AND comments between
            // `static` and that token are trivia, not part of the method source.
            return skip_leading_trivia(rest).to_string();
        }
    }
    text
}

/// Drop leading whitespace and comments (`// …` line + `/* … */` block) — the
/// trivia between a `static` keyword and the start of the MethodDefinition.
fn skip_leading_trivia(s: &str) -> &str {
    let mut t = s.trim_start();
    loop {
        if let Some(rest) = t.strip_prefix("//") {
            t = match rest.find(|c| c == '\n' || c == '\r') {
                Some(i) => rest[i..].trim_start(),
                None => "",
            };
        } else if let Some(rest) = t.strip_prefix("/*") {
            t = match rest.find("*/") {
                Some(i) => rest[i + 2..].trim_start(),
                None => "",
            };
        } else {
            return t;
        }
    }
}

fn placeholder(name: &str) -> FuncProto {
    FuncProto {
        name: name.to_string(),
        code: Vec::new(),
        reg_count: 0,
        param_count: 0,
        length: 0,
        rest_reg: None,
        arguments_reg: None,
        is_generator: false,
        is_async: false,
        non_constructable: false,
        lexical_this: false,
        super_static: false,
        is_strict: false,
        simple_params: false,
        constants: Vec::new(),
        string_constants: Vec::new(),
        bigint_consts: Vec::new(),
        wtf8_consts: Vec::new(),
        name_global: None,
        upvalues: Vec::new(),
        eval_sites: Vec::new(),
        source: String::new(),
    }
}

/// True if a directive prologue opens with `"use strict"`. Per spec the match is
/// against the directive's RAW source (so an escaped `"use strict"` does NOT
/// count); oxc's `Directive.directive` holds exactly that unescaped-but-raw text.
fn has_use_strict(directives: &[ox::Directive]) -> bool {
    directives.iter().any(|d| d.directive.as_str() == "use strict")
}

/// A function's directive prologue (empty when the function has no body), used to
/// detect its own `"use strict"`.
fn fn_directives<'a>(f: &'a ox::Function<'a>) -> &'a [ox::Directive<'a>] {
    f.body.as_ref().map(|b| b.directives.as_slice()).unwrap_or(&[])
}

/// Per-function compilation state.
struct FnCompiler<'a> {
    cx: &'a mut Compiler,
    code: Vec<Instr>,
    constants: Vec<Value>,
    string_constants: Vec<String>,
    /// BigInt literal constants beyond i128 (see `FuncProto::bigint_consts`).
    bigint_consts: Vec<num_bigint::BigInt>,
    /// `string_constants` indices holding the oxc lone-surrogate MARKER form
    /// (see `Function::wtf8_consts`).
    wtf8_consts: Vec<u32>,
    /// Lexical scope chain: each entry is (name, register).
    scopes: Vec<Vec<(String, Reg)>>,
    /// Next free register / high-water mark.
    next_reg: Reg,
    max_reg: Reg,
    /// Register holding the rest-parameter array, if this function has one.
    rest_reg: Option<Reg>,
    /// Register reserved for the `arguments` object (non-arrow functions), and
    /// whether the body actually referenced `arguments` (gates building it).
    arguments_reg: Option<Reg>,
    uses_arguments: bool,
    /// When compiling a class method/constructor, THIS class's class_id — the home
    /// for `super.x`/`super.m()`, which resolve at runtime via the home prototype's
    /// [[Prototype]] (so they work in base classes too, reaching %Object.prototype%).
    super_class: Option<u32>,
    /// True when compiling an OBJECT-LITERAL method/accessor (or an arrow lexically
    /// inside one). `super.x` then resolves via the runtime [[HomeObject]] (the object
    /// the method was defined in) — emitted as the Super*Obj ops — rather than the
    /// compile-time class home. Mutually distinct from `super_class` (a fresh function
    /// has neither unless set for its kind).
    super_home_obj: bool,
    /// True when compiling a STATIC class element (static method/getter/setter or
    /// `static { … }` block, and arrows lexically inside one). Selects the static
    /// super base (the class's [[Prototype]] = parent class) at runtime. Baked into
    /// the emitted function's `FuncProto::super_static`.
    super_static: bool,
    /// True only inside a DERIVED class's methods — gates `super(...)` (calling it in
    /// a base class constructor is an early SyntaxError). `super.x` is not gated.
    derived_class: bool,
    /// True only inside a derived class's CONSTRUCTOR body (and arrows
    /// lexically inside it): `this` reads and super-property references emit a
    /// `ThisCheck` (ReferenceError until `super()` has completed).
    in_derived_ctor: bool,
    /// Set while THIS FnCompiler compiles a named class's heritage expression:
    /// the inner class binding shadows even this function's locals/params for
    /// the duration (classScope nests inside the function scope). Nested
    /// functions inside the heritage use the cx-level `heritage_classes`
    /// instead (after their own scopes, so their params still shadow).
    heritage_class: Option<(String, u32)>,
    /// True when this function's body references `eval` (a possible direct
    /// eval): EVERY local is boxed into a cell so the eval program can close
    /// over the caller scope; DirectEval sites record the visible bindings.
    box_all_locals: bool,
    /// SCRIPT top level whose program references `eval`: BLOCK-level lexicals
    /// (which are true locals even at script level) are boxed into cells and
    /// each direct-eval site records its visible-bindings map, so the eval
    /// program can close over them (`{ let x; eval('x') }` at script level).
    /// Never set for non-script bodies (those use `box_all_locals`).
    script_eval_lexicals: bool,
    /// Scope maps for this function's DirectEval call sites (see
    /// FuncProto::eval_sites).
    eval_sites: Vec<(Vec<(String, u8, u16)>, Option<Vec<String>>, Vec<String>)>,
    /// True while this function's parameter defaults compile: a direct eval
    /// there sits in the PARAM scope (its sloppy var/function declarations
    /// collide with parameter names / the implicit `arguments`).
    in_param_init: bool,
    /// This function's parameter names (incl. rest) for that collision check.
    param_names: Vec<String>,
    /// When set, `this` resolves to this register instead of reg 0. Used while
    /// evaluating static field initializers inline at class-definition time,
    /// where `this` must be the class value (not the enclosing `this`) — without
    /// moving the init into a non-capturing thunk (which would lose closure over
    /// the enclosing scope).
    this_override: Option<Reg>,
    /// Set transiently around a block-nested lexical destructuring declaration at
    /// SCRIPT level so `declare_pattern` binds the pattern's leaves as block-locals
    /// (not globals), mirroring the simple-identifier `let`/`const` path — block
    /// `let {a} = …` must not leak to the global scope. Off everywhere else.
    pattern_block_local: bool,
    /// True while compiling a `function*` body, so `yield` is allowed.
    in_generator: bool,
    /// True while compiling an `async` body, so `await` is allowed.
    in_async: bool,
    /// A label seen on a `LabeledStatement`, consumed by the immediately-following
    /// loop/switch so `break label` / `continue label` can target it.
    pending_label: Option<String>,
    /// True for the top-level script body: declarations (functions AND
    /// let/const/var) bind to globals rather than registers, so only genuinely
    /// nested functions ever capture.
    is_script: bool,
    /// For an `eval` top-level script: a persistent register that accumulates the
    /// completion value (last evaluated expression statement). `Return`ed at the
    /// end instead of `undefined`. `None` outside eval mode / in nested functions.
    completion_reg: Option<Reg>,
    /// A named function expression's own name bound to itself (the running
    /// function value): `(name, reg)`. Sits OUTSIDE the parameter/var scope, so
    /// `resolve` consults it only after the scope stack (params/locals shadow it).
    /// `None` for declarations, arrows, methods, and anonymous expressions.
    self_name: Option<(String, Reg)>,
    /// This function's own bindings that some nested function captures; these
    /// are boxed into heap cells at declaration so the closure shares the slot.
    captured: HashSet<String>,
    /// Registers currently holding a CELL (a boxed captured binding), so reads/
    /// writes of them go through CellGet/CellSet.
    cell_regs: HashSet<Reg>,
    /// Registers bound by a `const` (immutable local): assignment to one is a
    /// runtime TypeError. Each const local has a unique register, so reassignment
    /// is detected by register identity (no name-shadowing ambiguity).
    const_regs: HashSet<Reg>,
    /// Registers bound by LEXICAL local declarations (let/const/class) —
    /// visible-lexical collection for direct-eval site maps.
    lexical_regs: HashSet<Reg>,
    /// Parameter names currently in the Temporal Dead Zone while a parameter
    /// default initializer is being compiled: a default that references the
    /// parameter itself (`(x = x)`) or a later parameter (`(x = y, y)`) reads a
    /// name in this set and throws a ReferenceError. Empty except inside
    /// `bind_params`.
    param_tdz: HashSet<String>,
    /// Upvalues this function captures, built lazily as free vars are resolved:
    /// (name, source-in-parent). Index in this Vec is the runtime upvalue index.
    /// Shared (`Rc<RefCell>`) so nested functions can append transitively.
    upvalues: UpvalList,
    /// Enclosing functions' binding snapshots (outermost → direct parent).
    enclosing: Vec<EnclosingFn>,
    /// Active `with` scopes (innermost last). Each records the register holding
    /// the (ToObject'd) with-object and the lexical-scope depth at which the
    /// `with` was entered: an identifier whose static binding lives in a scope
    /// SHALLOWER than this `floor` (or is a global/upvalue) can be shadowed by
    /// the with-object and so resolves dynamically; a binding declared INSIDE
    /// the with body (depth ≥ floor) is not shadowed. Empty except inside a
    /// `with` body, so non-`with` code compiles identically (zero regression).
    with_stack: Vec<WithScope>,
    /// `with` scopes of ENCLOSING functions that may shadow this function's
    /// free names: free name → ordered (innermost-first) chain of with-object
    /// binding names (each resolvable as an upvalue). Computed by the parent
    /// (`stash_child_with_shadows`) with its floor-vs-depth logic, so a name
    /// bound INSIDE a with body never appears. Empty for code not nested in a
    /// `with` (zero cost on the common path).
    inherited_with_shadows: std::collections::HashMap<String, Vec<String>>,
    /// Annex B B.3.3: names of functions declared inside BLOCKS of this (sloppy,
    /// non-script) function body that also get a function-scoped `var` binding,
    /// synced to the function value when the block declaration executes. Excludes
    /// names with a lexical conflict (which would be an early error → B.3.3 skipped).
    b33_names: std::collections::HashSet<String>,
    /// Per-function counter assigning a stable site index to each tagged-template
    /// literal, so the VM can memoize one canonical template object per (func, site).
    template_site_count: u32,
    /// Annex B B.3.3: function-body-level names that a same-named block function
    /// must NOT overwrite — formal parameters, lexical (`let`/`const`) bindings,
    /// and class declarations. A block function with one of these names stays
    /// purely block-local (the "skip" cases), unlike a name matching an existing
    /// `var`/function (which gets the function-scoped update via `b33_names`).
    protect_names: std::collections::HashSet<String>,
    /// Function-body-level lexical (`let`/`const`/`class`) names that were
    /// pre-created as cells at entry because a nested function captures them, so a
    /// function materialised at entry (forward reference) can bind their cell. The
    /// textual declaration REUSES this cell instead of allocating a fresh binding.
    /// Empty unless a body has a captured forward-referenced lexical.
    entry_lexicals: std::collections::HashSet<String>,
    /// Registers of BLOCK-level lexical (`let`/`const`/`class`) bindings
    /// pre-created as TDZ cells at block entry (because a nested function
    /// captures them), whose textual declaration has not yet been compiled.
    /// The declaration REUSES the register (ending its TDZ); an ASSIGNMENT
    /// through `store_binding` while still in this set emits the checked
    /// `CellSetChecked` (a write during the TDZ is a ReferenceError).
    /// Cleaned per-register in `pop_scope`.
    block_tdz_cells: HashSet<Reg>,
    /// Registers holding CATCH PARAMETERS (simple-identifier `catch (e)`
    /// bindings). Annex B B.3.5 allows a same-named `var` / promoted block
    /// function alongside one (no early error), so the B.3.3-applicability
    /// check skips these scope entries. Cleaned per-register in `pop_scope`.
    catch_param_regs: HashSet<Reg>,
    /// Active optional-chain short-circuit targets: a stack (chains can nest),
    /// each entry collecting the ip of every `?.` nullish-bail jump in that chain.
    /// On exit the chain patches them to a "load undefined" block.
    chain_bails: Vec<Vec<u32>>,
    /// Enclosing loop contexts (innermost last) for `break`/`continue`: each
    /// collects the jump ips to patch to the loop's end (break) and continue
    /// point (continue).
    loop_ctx: Vec<LoopCtx>,
    /// Count of `try` handlers (catch and finally) statically active at the
    /// current compilation point — mirrors the runtime handler-stack depth here.
    /// A `break`/`continue` whose target loop has a SMALLER handler depth must
    /// unwind the difference (running any intervening `finally`), so it compiles
    /// to `JumpFinally` instead of `Jump`. Maintained across try/catch/finally
    /// regions (see `try_with_finally` / `try_catch_only`).
    handler_depth: usize,
    /// Register holding the runtime resource-scope id of the innermost enclosing
    /// `using` block currently being compiled (set by `compile_using_block`), so a
    /// `using` declaration's `RegisterDisposable` knows which scope to push onto.
    /// `None` outside any `using` block; saved/restored across block nesting.
    using_scope_reg: Option<Reg>,
}

/// Pending `break`/`continue` jumps for one enclosing breakable construct. A
/// `switch` is a break target but NOT a continue target, so `continue` skips
/// switch frames to the innermost loop (`is_loop`).
struct LoopCtx {
    break_jumps: Vec<u32>,
    continue_jumps: Vec<u32>,
    is_loop: bool,
    /// The label attached to this loop/switch (`outer: for …`), for labeled
    /// `break outer` / `continue outer`.
    label: Option<String>,
    /// The `handler_depth` in effect where this loop/switch was entered — the
    /// `floor` a `break`/`continue` targeting it must unwind the handler stack to.
    handler_depth: usize,
    /// For a `for-of` / `for-await-of` frame: the register holding the live
    /// iterator, so an abrupt `return` out of the body runs IteratorClose on it
    /// (a normal `break` already closes via its own block). `None` for other loops.
    iter_close: Option<Reg>,
}

/// A pre-evaluated destructuring member-target key (see `pre_member_ref`).
enum PreKey {
    Static(u32),
    Computed(Reg),
    Private(u32),
}

/// One active `with` scope (see `FnCompiler::with_stack`).
struct WithScope {
    /// Register holding the ToObject'd with-object, kept live across the body
    /// (allocated as a hidden scope-local so per-statement temp resets preserve it).
    obj_reg: Reg,
    /// Lexical-scope depth (`scopes.len()`) at the point the `with` was entered.
    floor: usize,
}

impl LoopCtx {
    fn loop_frame(label: Option<String>, handler_depth: usize) -> LoopCtx {
        LoopCtx {
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
            is_loop: true,
            label,
            handler_depth,
            iter_close: None,
        }
    }
    fn switch_frame(label: Option<String>, handler_depth: usize) -> LoopCtx {
        LoopCtx {
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
            is_loop: false,
            label,
            handler_depth,
            iter_close: None,
        }
    }
}

impl<'a> FnCompiler<'a> {
    fn new(
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
            next_reg: 0,
            max_reg: 0,
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
    /// have no `arguments`).
    fn reserve_arguments(&mut self) {
        let r = self.alloc_reg();
        self.scopes[0].push(("arguments".to_string(), r));
        self.arguments_reg = Some(r);
    }

    /// Snapshot this function's environment for a nested function to capture
    /// from: cell-backed locals in scope, plus a SHARED handle to this
    /// function's upvalue list (so a grandchild can both read and transitively
    /// extend it via ParentUpval re-sourcing).
    fn snapshot(&self) -> EnclosingFn {
        let mut cell_locals = Vec::new();
        for scope in &self.scopes {
            for (name, reg) in scope {
                if self.cell_regs.contains(reg) {
                    cell_locals.push((name.clone(), *reg));
                }
            }
        }
        // A named-function-expression self-binding that was boxed (captured by a
        // nested closure) is also visible to that closure as an upvalue source.
        if let Some((name, reg)) = &self.self_name {
            if self.cell_regs.contains(reg) {
                cell_locals.push((name.clone(), *reg));
            }
        }
        EnclosingFn { cell_locals, upvalues: self.upvalues.clone() }
    }

    /// Resolve a free variable to an upvalue index in THIS function, creating
    /// the upvalue on first use. `None` if not found in any enclosing function
    /// (then it's a global). Transitive: if the variable lives in an ancestor
    /// beyond the direct parent, every intermediate function captures it too.
    fn resolve_upvalue(&mut self, name: &str) -> Option<u16> {
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
    fn alloc_reg(&mut self) -> Reg {
        let r = self.next_reg;
        self.next_reg += 1;
        if self.next_reg > self.max_reg {
            self.max_reg = self.next_reg;
        }
        r
    }
    /// A scratch register that the caller will stop using immediately; we still
    /// bump the high-water mark but let it be reused by resetting next_reg.
    fn temp(&mut self) -> Reg {
        self.alloc_reg()
    }

    fn emit(&mut self, i: Instr) {
        self.code.push(i);
    }
    fn here(&self) -> u32 {
        self.code.len() as u32
    }

    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }
    fn pop_scope(&mut self) {
        let scope = self.scopes.pop().unwrap();
        // Free the registers the scope's locals used (block-local reuse) —
        // and drop their per-register markings, or a later local reallocated
        // onto the same register would inherit const-ness / cell-ness
        // (`{ const a = 1; } { let b = 2; b = 3; }` falsely threw TypeError).
        self.next_reg -= scope.len() as Reg;
        for (_, r) in &scope {
            self.const_regs.remove(r);
            self.cell_regs.remove(r);
            self.lexical_regs.remove(r);
            self.block_tdz_cells.remove(r);
            self.catch_param_regs.remove(r);
        }
    }

    fn declare_local(&mut self, name: &str) -> Reg {
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
    fn declare_local_no_box(&mut self, name: &str) -> Reg {
        let r = self.alloc_reg();
        self.scopes.last_mut().unwrap().push((name.to_string(), r));
        r
    }

    /// Resolve a name to a local register (plain or cell-backed), an upvalue, or
    /// a global slot. Upvalue resolution lazily threads captures up the chain.
    fn resolve(&mut self, name: &str) -> Binding {
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

    /// Like `resolve`, but NON-creating: returns `None` for a name that has no
    /// existing binding (rather than minting a fresh global slot). Used by
    /// `delete <identifier>` to tell a resolvable binding (→ `false`) from an
    /// unresolvable name (→ `true`, a no-op) without evaluating or declaring it.
    /// Does not thread upvalues (no side effects); an enclosing-function local is
    /// reported as unresolved, which only affects the rare `delete <outer local>`.
    fn resolve_existing(&self, name: &str) -> Option<Binding> {
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
    fn block_fn_conflicts(&self, name: &str) -> bool {
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
    }

    /// Like `block_fn_conflicts` but for the B.3.3 VAR-SYNC applicability
    /// check: a CATCH PARAMETER of the same name is NOT a conflict (B.3.5 —
    /// `var`+catch-param coexist without an early error, so the promotion is
    /// NOT skipped: the "no-skip-try" family), while an enclosing block's
    /// function/lexical binding still is.
    fn block_fn_sync_conflicts(&self, name: &str) -> bool {
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
        })
    }

    fn add_const(&mut self, v: Value) -> u32 {
        let i = self.constants.len() as u32;
        self.constants.push(v);
        i
    }
    /// Intern a string LITERAL and return its CONSTANT-POOL index (for
    /// `LoadConst`). The VM interns the pending-string Value on first load.
    fn add_string_const(&mut self, s: &str) -> u32 {
        let si = self.string_constants.len() as u32;
        self.string_constants.push(s.to_string());
        // Encode as a "pending string" heap Value the VM interns on first load.
        let v = Value::heap(STRING_CONST_BIT | si);
        self.add_const(v)
    }

    /// `add_string_const` for a literal oxc flagged `.lone_surrogates`: the
    /// text is the lossless MARKER form (`\u{FFFD}XXXX` per lone surrogate);
    /// recording the index makes `resolve_const` decode it to real WTF-8 lone
    /// surrogates at intern time.
    fn add_string_const_wtf8(&mut self, s: &str) -> u32 {
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
    fn string_name(&mut self, s: &str) -> u32 {
        let si = self.string_constants.len() as u32;
        self.string_constants.push(s.to_string());
        si
    }

    // ── statements ──
    fn stmt(&mut self, s: &ox::Statement) -> R<()> {
        use ox::Statement as S;
        match s {
            S::ExpressionStatement(e) => {
                let r = self.expr(&e.expression)?;
                // eval completion: remember this expression's value (the last one
                // executed wins, matching the spec's expression-completion value).
                if let Some(cr) = self.completion_reg {
                    self.emit(Instr::Move { dst: cr, src: r });
                }
                let _ = r; // value otherwise discarded
            }
            S::VariableDeclaration(d) => self.var_decl(d)?,
            S::BlockStatement(b) => {
                self.push_scope();
                // Pre-create TDZ cells for the block's CAPTURED simple-identifier
                // lexical (`let`/`const`/`class`) declarations, so a closure
                // materialized BEFORE the textual declaration (`{ function f() {
                // x = 1; } f(); let x; }`) captures the block's binding (in its
                // TDZ → ReferenceError) instead of resolving to a global. The
                // textual declaration reuses the register, ending the TDZ.
                // Non-captured names keep plain registers (no runtime cost).
                for st in &b.body {
                    let mut pre = |fc: &mut Self, name: &str| {
                        if fc.captured.contains(name)
                            && !fc.scopes.last().unwrap().iter().any(|(n, _)| n == name)
                        {
                            let r = fc.alloc_reg();
                            fc.scopes.last_mut().unwrap().push((name.to_string(), r));
                            fc.emit(Instr::MakeCellTdz { reg: r });
                            fc.cell_regs.insert(r);
                            fc.block_tdz_cells.insert(r);
                        }
                    };
                    match st {
                        S::VariableDeclaration(d) if d.kind.is_lexical() => {
                            for decl in &d.declarations {
                                if let ox::BindingPattern::BindingIdentifier(id) = &decl.id {
                                    pre(self, id.name.as_str());
                                }
                            }
                        }
                        S::ClassDeclaration(c) => {
                            if let Some(id) = &c.id {
                                pre(self, id.name.as_str());
                            }
                        }
                        _ => {}
                    }
                }
                // Hoist block-level function declarations: declare each as a local
                // in this block scope first, so `func_decl` binds it (and forward
                // references / calls within the block resolve to the local rather
                // than an undeclared global). Only inside a real function body —
                // at script top level, `func_decl` binds block functions to globals
                // (Annex B hoisting), so a local here would shadow that with an
                // uninitialized slot.
                let mut entry_fns: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for st in &b.body {
                    if let S::FunctionDeclaration(f) = st {
                        if let Some(id) = &f.id {
                            // Inside a function body, block functions are always
                            // block-local. At script level they normally hoist to
                            // a global (Annex B) and so are NOT pre-declared here —
                            // UNLESS the name conflicts with an enclosing-block
                            // lexical binding (conflict-skip), OR the code is STRICT
                            // (Annex B is not honored in strict mode, so the function
                            // stays block-local and does not leak past the block).
                            let nm = id.name.as_str();
                            // Block-local for strict / enclosing-block lexical
                            // conflict / a protected param-lexical-class name / a
                            // B.3.3 var name. A name matching an existing function is
                            // NOT shadowed — it is directly updated (B.3.3).
                            if self.cx.in_strict
                                || self.block_fn_conflicts(nm)
                                || self.protect_names.contains(nm)
                                || self.b33_names.contains(nm)
                            {
                                self.declare_local(nm);
                                entry_fns.insert(nm.to_string());
                            }
                        }
                    }
                }
                if Self::block_has_using(&b.body) {
                    // A block with a top-level `using` declaration disposes its
                    // resources on every exit — desugar onto a synthetic finally.
                    self.compile_using_block(&b.body, false)?;
                } else {
                    // BlockDeclarationInstantiation: the block's function
                    // declarations are materialized at BLOCK ENTRY (a call
                    // before the textual declaration works), while the Annex B
                    // var-binding sync stays at the declaration's textual
                    // position (B.3.3.3 fires at evaluation, not entry).
                    for st in &b.body {
                        if let S::FunctionDeclaration(f) = st {
                            if let Some(id) = &f.id {
                                if entry_fns.contains(id.name.as_str()) {
                                    self.func_decl_inner(f, false)?;
                                }
                            }
                        }
                    }
                    for st in &b.body {
                        if let S::FunctionDeclaration(f) = st {
                            if let Some(id) = &f.id {
                                if entry_fns.contains(id.name.as_str()) {
                                    self.emit_b33_sync(id.name.as_str());
                                    continue;
                                }
                            }
                        }
                        self.stmt(st)?;
                    }
                }
                self.pop_scope();
            }
            S::IfStatement(i) => self.if_stmt(i)?,
            S::WhileStatement(w) => {
                self.reset_loop_completion();
                self.while_stmt(w)?
            }
            S::DoWhileStatement(d) => {
                self.reset_loop_completion();
                self.do_while_statement(d)?
            }
            S::ForStatement(f) => {
                self.reset_loop_completion();
                self.for_stmt(f)?
            }
            S::ForOfStatement(f) => {
                self.reset_loop_completion();
                self.for_of_statement(f)?
            }
            S::ForInStatement(f) => {
                self.reset_loop_completion();
                self.for_in_statement(f)?
            }
            S::BreakStatement(b) => {
                // `break label` targets the labeled loop/switch; bare `break` the
                // innermost.
                let idx = match &b.label {
                    Some(lbl) => self
                        .loop_ctx
                        .iter()
                        .rposition(|c| c.label.as_deref() == Some(lbl.name.as_str())),
                    None => self.loop_ctx.len().checked_sub(1),
                };
                let idx = match idx {
                    Some(i) => i,
                    None => return Err("`break` target not found (outside a loop / unknown label)".into()),
                };
                // Iterators of for-of frames BETWEEN here and the target close
                // (innermost first); the target loop's own exit path closes its.
                let iters: Vec<Reg> = self.loop_ctx[idx + 1..]
                    .iter()
                    .rev()
                    .filter_map(|c| c.iter_close)
                    .collect();
                for it in iters {
                    self.emit(Instr::IterClose { iter: it });
                }
                self.emit_loop_jump(idx, true);
            }
            S::ContinueStatement(c) => {
                // `continue [label]` targets the (labeled) enclosing LOOP, skipping
                // switch frames.
                let idx = match &c.label {
                    Some(lbl) => self
                        .loop_ctx
                        .iter()
                        .rposition(|ctx| ctx.is_loop && ctx.label.as_deref() == Some(lbl.name.as_str())),
                    None => self.loop_ctx.iter().rposition(|ctx| ctx.is_loop),
                };
                let idx = match idx {
                    Some(i) => i,
                    None => return Err("`continue` target not found (outside a loop / unknown label)".into()),
                };
                let iters: Vec<Reg> = self.loop_ctx[idx + 1..]
                    .iter()
                    .rev()
                    .filter_map(|c| c.iter_close)
                    .collect();
                for it in iters {
                    self.emit(Instr::IterClose { iter: it });
                }
                self.emit_loop_jump(idx, false);
            }
            S::LabeledStatement(l) => {
                if let S::BlockStatement(b) = &l.body {
                    // `label: { … break label … }` — a break-only target around a
                    // block (continue to a block label is invalid, and naturally
                    // won't match: the frame is not a loop).
                    self.loop_ctx
                        .push(LoopCtx::switch_frame(Some(l.label.name.to_string()), self.handler_depth));
                    self.push_scope();
                    for s in &b.body {
                        self.stmt(s)?;
                    }
                    self.pop_scope();
                    let ctx = self.loop_ctx.pop().unwrap();
                    let end = self.here();
                    for j in ctx.break_jumps {
                        self.patch_jump(j, end);
                    }
                } else {
                    // A loop/switch consumes the label for break/continue;
                    // cleared afterwards if the body was something else.
                    self.pending_label = Some(l.label.name.to_string());
                    self.stmt(&l.body)?;
                    self.pending_label = None;
                }
            }
            S::SwitchStatement(s) => self.switch_stmt(s)?,
            S::ReturnStatement(r) => {
                // Proper tail call: strict `return <expr with a call in tail
                // position>` in an UNPROTECTED context (no try handlers, no
                // enclosing loop with an iterator to close, no using scope,
                // not generator/async/script/eval) reuses the current frame —
                // constant stack for tail recursion. Tail positions cover the
                // call itself plus conditional arms, logical right operands,
                // sequence finals, parenthesization, and plain-tag tagged
                // templates. The TailCall prefix falls through to the
                // ordinary Call+Return for non-plain callees.
                if let Some(arg) = &r.argument {
                    if self.tail_call_position() && self.expr_has_tail_call(arg) {
                        self.emit_tail_return(arg)?;
                        return Ok(());
                    }
                }
                // Evaluate the return value FIRST (its side effects precede the
                // iterator closes), then run IteratorClose on every enclosing for-of /
                // for-await-of (innermost first) — a `return` is an abrupt completion
                // that closes the iterator (a throwing `return()` then propagates,
                // discarding the value). `break`/`throw` already close via their paths.
                let v = match &r.argument {
                    Some(arg) => {
                        let v = self.expr(arg)?;
                        // In an ASYNC GENERATOR, `return expr;` performs
                        // Await(exprValue) (spec ReturnStatement evaluation): a
                        // thenable operand is adopted (observable `then` read)
                        // and an explicit `return undefined` settles one tick
                        // later than an implicit return. Awaits into a temp so
                        // a returned local's register isn't clobbered.
                        if self.in_generator && self.in_async {
                            let t = self.temp();
                            self.emit(Instr::Await { dst: t, val: v });
                            Some(t)
                        } else {
                            Some(v)
                        }
                    }
                    None => None,
                };
                let iters: Vec<Reg> =
                    self.loop_ctx.iter().rev().filter_map(|c| c.iter_close).collect();
                for it in iters {
                    self.emit(Instr::IterClose { iter: it });
                }
                match v {
                    Some(v) => self.emit(Instr::Return { src: v }),
                    None => self.emit(Instr::ReturnUndefined),
                }
            }
            S::FunctionDeclaration(f) => self.func_decl(f)?,
            S::ThrowStatement(t) => {
                let v = self.expr(&t.argument)?;
                self.emit(Instr::Throw { src: v });
            }
            S::TryStatement(t) => self.try_statement(t)?,
            S::ClassDeclaration(c) => self.class_decl(c)?,
            S::EmptyStatement(_) => {}
            S::DebuggerStatement(_) => {} // `debugger;` is a no-op (no attached debugger)
            S::WithStatement(w) => {
                // `with` is a SyntaxError in strict mode (early error) — preserve
                // that so strict negative tests keep passing.
                if self.cx.in_strict {
                    return Err("SyntaxError: 'with' statements are not allowed in strict mode".into());
                }
                // Completion: UpdateEmpty(C, undefined) — an empty/abrupt body
                // yields undefined for eval, not the previous statement's value.
                self.reset_loop_completion();
                // ToObject(GetValue(object)) becomes the with-environment's binding
                // object. Held in a hidden scope-local so it survives the whole body
                // (per-statement temp resets allocate above it).
                let raw = self.expr(&w.object)?;
                // ToObject(null)/ToObject(undefined) throw a TypeError (the with
                // object must be coercible).
                self.emit(Instr::CheckCoercible { src: raw });
                self.push_scope();
                // UNIQUE hidden name (leading space — uncollidable) so a nested
                // closure can capture THIS with-object across the enclosing
                // chain; boxed into a cell after the value exists (like a catch
                // param) so it is upvalue-capturable. Probe sites unwrap via
                // `with_obj_regs`.
                let wname = format!(" with-object-{}", self.cx.with_name_counter);
                self.cx.with_name_counter += 1;
                let obj_reg = self.declare_local_no_box(&wname);
                self.emit(Instr::ToObject { dst: obj_reg, src: raw });
                self.emit(Instr::MakeCell { reg: obj_reg });
                self.cell_regs.insert(obj_reg);
                let floor = self.scopes.len();
                self.with_stack.push(WithScope { obj_reg, floor });
                let r = self.stmt(&w.body);
                self.with_stack.pop();
                self.pop_scope();
                r?;
            }
            // ── ES module declarations (only reached for SourceType::module, i.e.
            // a fixture loaded by a dynamic `import()`; a script never parses these).
            S::ImportDeclaration(_) => {
                // Handled by the MODULE PRE-PASS (import bindings hoist: a
                // reference or assignment may precede the declaration).
            }
            S::ExportNamedDeclaration(e) => {
                // `export {imported as exported} from './m'` (re-export): record the
                // (exported, imported, specifier) triples so the loader can resolve
                // them against the dependency module. No local binding is created.
                if let Some(src) = &e.source {
                    let spec = src.value.to_string();
                    for spec_item in &e.specifiers {
                        let exported = module_export_name(&spec_item.exported);
                        let imported = module_export_name(&spec_item.local);
                        self.cx.module_reexports.push((exported, imported, spec.clone()));
                    }
                    return Ok(());
                }
                // `export var/let/const/function/class …`: compile the inner
                // declaration normally (its top-level binding becomes a global), then
                // record each bound name as an export (exported name == local name).
                if let Some(decl) = &e.declaration {
                    match decl {
                        ox::Declaration::VariableDeclaration(d) => {
                            self.var_decl(d)?;
                            let mut names = std::collections::HashSet::new();
                            for dd in &d.declarations {
                                capture::collect_pattern_names(&dd.id, &mut names);
                            }
                            for n in names {
                                self.cx.module_exports.push((n.clone(), n));
                            }
                        }
                        ox::Declaration::FunctionDeclaration(f) => {
                            self.func_decl(f)?;
                            if let Some(id) = &f.id {
                                let n = id.name.to_string();
                                self.cx.module_exports.push((n.clone(), n));
                            }
                        }
                        ox::Declaration::ClassDeclaration(c) => {
                            self.class_decl(c)?;
                            if let Some(id) = &c.id {
                                let n = id.name.to_string();
                                self.cx.module_exports.push((n.clone(), n));
                            }
                        }
                        _ => return Err("unsupported export declaration".into()),
                    }
                }
                // `export { local as exported, … }`.
                for spec in &e.specifiers {
                    let local = module_export_name(&spec.local);
                    let exported = module_export_name(&spec.exported);
                    self.cx.module_exports.push((exported, local));
                }
            }
            S::ExportDefaultDeclaration(e) => {
                use ox::ExportDefaultDeclarationKind as K;
                // Bind the default value to a synthetic global "*default*" (not a
                // valid identifier, so no user collision) and export it as "default".
                let slot = self.cx.global_slot("*default*") as u32;
                let tmp = self.temp();
                // `export default function f(){}` / `class C{}` also binds the NAME
                // (f / C) as a module-local declaration, so code in the module can
                // reference it (the slot is module-declared for per-module isolation).
                let mut bind_name: Option<String> = None;
                match &e.declaration {
                    K::FunctionDeclaration(f) => {
                        // A NAMED default hoistable declaration is an ordinary
                        // MUTABLE module binding (`fn = 2` inside the body works,
                        // unlike a function EXPRESSION self-name) and the export
                        // entry LocalName is the NAME — ns.default tracks the
                        // LIVE binding, not a *default* snapshot.
                        if let Some(id) = &f.id {
                            let n = id.name.to_string();
                            self.func_decl(f)?;
                            let nslot = self.cx.global_slot(&n) as u32;
                            self.cx.decl_globals.insert(nslot);
                            self.cx.module_exports.push(("default".to_string(), n));
                            self.next_reg -= 1;
                            return Ok(());
                        }
                        // An ANONYMOUS default-exported function/generator is named
                        // "default" (NamedEvaluation) and HOISTS like any other
                        // function declaration — `f()` before this statement works
                        // through an `import f from './self'` alias.
                        let (id, has_up) =
                            self.compile_func_expr(Some("default".to_string()), f)?;
                        if self.is_script && self.cx.script_binds_globals && !has_up {
                            self.cx.functions[id as usize].name_global = Some(slot as u16);
                            self.cx.decl_globals.insert(slot);
                            self.next_reg -= 1;
                            self.cx
                                .module_exports
                                .push(("default".to_string(), "*default*".to_string()));
                            return Ok(());
                        }
                        self.emit_make_callable(tmp, id, has_up);
                        bind_name = None;
                    }
                    K::ClassDeclaration(c) => {
                        // An ANONYMOUS default-exported class is named "default".
                        let r = self.class_expr(c, tmp, if c.id.is_none() { Some("default") } else { None })?;
                        if r != tmp {
                            self.emit(Instr::Move { dst: tmp, src: r });
                        }
                        bind_name = c.id.as_ref().map(|i| i.name.to_string());
                    }
                    other => {
                        // `export default <AssignmentExpression>`: an anonymous
                        // function/arrow/class expression is named "default"
                        // (NamedEvaluation), like `const default = …` would.
                        let expr = other.as_expression().ok_or("unsupported default export")?;
                        let v = self.compile_named_init(tmp, expr, "default")?;
                        if v != tmp {
                            self.emit(Instr::Move { dst: tmp, src: v });
                        }
                    }
                }
                self.emit(Instr::StoreGlobal { idx: slot, src: tmp });
                if let Some(name) = bind_name {
                    let nslot = self.cx.global_slot(&name) as u32;
                    self.cx.decl_globals.insert(nslot); // module-declared → per-module slot
                    self.emit(Instr::StoreGlobal { idx: nslot, src: tmp });
                }
                self.next_reg -= 1;
                self.cx
                    .module_exports
                    .push(("default".to_string(), "*default*".to_string()));
            }
            S::ExportAllDeclaration(e) => {
                if let Some(exported) = &e.exported {
                    // `export * as ns from './m'` exports the dependency's
                    // NAMESPACE object under `ns` (linked by the loader).
                    self.cx
                        .module_ns_reexports
                        .push((module_export_name(exported), e.source.value.to_string()));
                } else {
                    // `export * from './m'` — copy all of the dependency's exports
                    // (except default) into this module's namespace at link time.
                    self.cx.module_star_reexports.push(e.source.value.to_string());
                }
            }
            _ => return Err("unsupported statement (not in the zipp-vm v1 subset yet)".into()),
        }
        Ok(())
    }

    /// Evaluate an initializer into `dst`, inferring a name for an anonymous
    /// function/arrow assigned to a binding (`const f = () => {}` ⇒ `f.name`
    /// === "f"). A named function expression keeps its own name.
    fn compile_named_init(&mut self, dst: Reg, init: &ox::Expression, name: &str) -> R<Reg> {
        match init {
            ox::Expression::ArrowFunctionExpression(a) => {
                let (id, _has_up) = self.compile_arrow(a, name)?;
                self.emit_make_arrow(dst, id);
                Ok(dst)
            }
            ox::Expression::FunctionExpression(f) if f.id.is_none() => {
                let (id, has_up) = self.compile_func_expr(Some(name.to_string()), f)?;
                self.emit_make_callable(dst, id, has_up);
                Ok(dst)
            }
            // `const C = class {}` / `x = class {}` — an anonymous class takes the
            // binding name (a named `class C {}` keeps its own).
            ox::Expression::ClassExpression(c) if c.id.is_none() => {
                self.class_expr(c, dst, Some(name))
            }
            // NamedEvaluation sees through parentheses: `var f = (function(){})`.
            ox::Expression::ParenthesizedExpression(p) => {
                self.compile_named_init(dst, &p.expression, name)
            }
            _ => self.expr_into(init, dst),
        }
    }

    fn var_decl(&mut self, d: &ox::VariableDeclaration) -> R<()> {
        // A `const` binding is immutable: record its slot/register so a later
        // assignment throws a TypeError (initialization below never goes through
        // store_binding, so it is unaffected). `using`/`await using` bindings
        // are equally immutable (CreateImmutableBinding in the spec).
        let is_const = matches!(
            d.kind,
            ox::VariableDeclarationKind::Const
                | ox::VariableDeclarationKind::Using
                | ox::VariableDeclarationKind::AwaitUsing
        );
        for decl in &d.declarations {
            // Destructuring declaration (`let {a,b} = o`, `let [x,...r] = arr`):
            // declare every leaf binding, evaluate the initializer once into a
            // scratch register, then extract each target from it.
            if !matches!(decl.id, ox::BindingPattern::BindingIdentifier(_)) {
                let init = decl
                    .init
                    .as_ref()
                    .ok_or("a destructuring declaration requires an initializer")?;
                // A block-nested lexical (`let`/`const`) destructuring at script
                // level binds its leaves block-local, not global — same rule as the
                // simple-identifier path below, so `{ let {a} = o; }` doesn't leak.
                let block_local = d.kind.is_lexical() && self.scopes.len() > 1;
                self.pattern_block_local = block_local;
                self.declare_pattern(&decl.id)?;
                let save = self.next_reg;
                // TOP-LEVEL global-bound leaves: INITIALIZE each slot before
                // extraction. The extraction stores are assignment-flavored —
                // in STRICT code (a module body) StoreGlobalStrict throws on
                // an UNINITIALIZED slot, so the leaves' TDZ must end at this
                // statement, not at the store. Lexical leaves also register
                // like their simple-identifier siblings (const immutability,
                // module per-module slots, script-GDI bookkeeping).
                if self.is_script && self.cx.script_binds_globals && !block_local {
                    let mut leaves = std::collections::HashSet::new();
                    capture::collect_pattern_names(&decl.id, &mut leaves);
                    let undef = self.alloc_reg();
                    self.emit(Instr::LoadUndefined { dst: undef });
                    for n in leaves {
                        let slot = self.cx.global_slot(&n) as u32;
                        if d.kind.is_lexical() {
                            self.cx.lexical_globals.insert(slot);
                        }
                        // (NOT const_globals: the extraction itself stores
                        // through store_binding, which throws for a known
                        // const — initialization must stay writable.)
                        self.emit(Instr::StoreGlobal { idx: slot, src: undef });
                    }
                }
                let src = self.alloc_reg();
                // The pattern's leaves are in their TDZ while the INITIALIZER
                // evaluates (`let {a} = a` throws); they leave it before the
                // extraction (a sibling default may read an earlier leaf).
                let tdz_leaves: Vec<String> = if d.kind.is_lexical() {
                    let mut names = std::collections::HashSet::new();
                    capture::collect_pattern_names(&decl.id, &mut names);
                    names.into_iter().filter(|n| self.param_tdz.insert(n.clone())).collect()
                } else {
                    Vec::new()
                };
                let sv = self.expr_into(init, src);
                for n in &tdz_leaves {
                    self.param_tdz.remove(n);
                }
                let sv = sv?;
                if sv != src {
                    self.emit(Instr::Move { dst: src, src: sv });
                }
                self.extract_pattern(&decl.id, src)?;
                self.pattern_block_local = false;
                self.next_reg = save; // reclaim the source + extraction temps
                continue;
            }
            let name = match &decl.id {
                ox::BindingPattern::BindingIdentifier(id) => id.name.as_str(),
                _ => unreachable!("handled above"),
            };
            // Strict mode: `var eval` / `let arguments` etc. are early SyntaxErrors.
            strict_name_err(self.cx.in_strict, name)?;

            // `var` (function-scoped) and a TRUE top-level `let`/`const` bind to
            // GLOBAL slots, so a nested function resolves them via LoadGlobal (a
            // top-level binding is never an upvalue). But a `let`/`const` nested
            // inside a BLOCK is block-scoped and must NOT leak to the global
            // scope (`{ let x = 1; }` leaves `x` undeclared after the block) — it
            // falls through to a block-local binding even at script level.
            let block_scoped_lexical =
                d.kind.is_lexical() && (self.scopes.len() > 1 || self.cx.eval_locals);
            // EVAL root: `var x` where x is a CALLER binding — the declaration
            // is a no-op (the binding exists); an initializer assigns THROUGH
            // the captured cell (sloppy direct eval's var env is the caller's).
            if self.is_script
                && !d.kind.is_lexical()
                && self.scopes.len() == 1
                && self.cx.eval_caller_scope.iter().any(|n| n == name)
            {
                if let Some(init) = &decl.init {
                    let tmp = self.temp();
                    let v = self.compile_named_init(tmp, init, name)?;
                    let idx = self
                        .resolve_upvalue(name)
                        .ok_or("eval caller binding upvalue")?;
                    self.emit(Instr::UpvalSet { idx, src: v });
                    self.next_reg -= 1;
                }
                continue;
            }
            // Annex B 3.5: `var foo = init` where `foo` is SHADOWED by a deeper
            // block binding (a catch parameter — the only legal shadow of a
            // var name): the hoisted function/global var binding is untouched
            // here; the INITIALIZER assigns the shadowing binding via ordinary
            // resolution (`catch (foo) { var foo = x; }` writes the parameter).
            if !d.kind.is_lexical()
                && decl.init.is_some()
                && self.scopes.len() > 1
                && self.scopes[1..].iter().flatten().any(|(n, _)| n == name)
            {
                let init = decl.init.as_ref().unwrap();
                let save = self.next_reg;
                let tmp = self.temp();
                let v = self.compile_named_init(tmp, init, name)?;
                let with_objs = self.with_obj_regs(name);
                if with_objs.is_empty() {
                    let b = self.resolve(name);
                    self.store_binding(&b, v);
                } else {
                    self.store_with(name, &with_objs, v);
                }
                self.next_reg = save;
                continue;
            }
            if self.is_script && self.cx.script_binds_globals && !block_scoped_lexical {
                let slot = self.cx.global_slot(name) as u32;
                if is_const {
                    self.cx.const_globals.insert(slot);
                }
                // A bare `var x;` performs NO assignment — the hoisted slot was
                // seeded undefined at startup, and re-storing here would RESET
                // an already-assigned binding (`var x = 5; var x;` keeps 5). A
                // bare lexical (`let x;`) DOES store: that write ends its TDZ.
                if decl.init.is_none() && !d.kind.is_lexical() {
                    continue;
                }
                let tmp = self.temp();
                let v = if let Some(init) = &decl.init {
                    self.compile_named_init(tmp, init, name)?
                } else {
                    self.emit(Instr::LoadUndefined { dst: tmp });
                    tmp
                };
                // `var x = init` inside a `with` whose object has `x`: the
                // declaration's binding is hoisted (the global slot is already
                // undefined), but the INITIALIZER is an assignment evaluated in the
                // with-scope, so it targets the with-object. A bare `var x;` (no
                // init) performs no assignment, so it never routes here.
                let with_objs = if decl.init.is_some() {
                    self.with_obj_regs(name)
                } else {
                    Vec::new()
                };
                if with_objs.is_empty() {
                    if self.box_all_locals || self.cx.dyn_global_zone {
                        self.emit(Instr::StoreGlobalDyn { idx: slot, src: v });
                    } else {
                        self.emit(Instr::StoreGlobal { idx: slot, src: v });
                    }
                } else {
                    self.store_with(name, &with_objs, v);
                }
                self.next_reg -= 1; // reclaim tmp
                continue;
            }

            // A `var` inside a function is FUNCTION-scoped, not block-scoped: bind it
            // in the function's BASE scope (scopes[0]) so it survives past its block
            // (`{ var x = 1 } x`, `for (var i…){} i`), reusing an existing binding
            // rather than duplicating. A nested closure over it boxes the slot (the
            // var name is in the capture set). A bare `var x;` never resets `x`.
            if !d.kind.is_lexical() {
                let existing =
                    self.scopes[0].iter().rev().find(|(n, _)| n == name).map(|(_, r)| *r);
                let reg = match existing {
                    Some(r) => r,
                    None => {
                        let r = self.alloc_reg();
                        self.scopes[0].push((name.to_string(), r));
                        if self.captured.contains(name) {
                            self.emit(Instr::MakeCell { reg: r });
                            self.cell_regs.insert(r);
                        }
                        r
                    }
                };
                if let Some(init) = &decl.init {
                    // `var x = init` inside a `with` whose object has `x`: the
                    // declaration is hoisted (the function-scope slot is already
                    // undefined), but the initializer assignment targets the
                    // with-object (falling back to this slot if absent).
                    let with_objs = self.with_obj_regs(name);
                    if !with_objs.is_empty() {
                        let tmp = self.temp();
                        let v = self.compile_named_init(tmp, init, name)?;
                        self.store_with(name, &with_objs, v);
                        self.next_reg -= 1;
                    } else if self.cell_regs.contains(&reg) {
                        let tmp = self.temp();
                        let v = self.compile_named_init(tmp, init, name)?;
                        self.emit(Instr::CellSet { cell: reg, src: v });
                        self.next_reg -= 1;
                    } else {
                        let v = self.compile_named_init(reg, init, name)?;
                        if v != reg {
                            self.emit(Instr::Move { dst: reg, src: v });
                        }
                    }
                }
                continue;
            }

            // Allocate the local FIRST so `let x = x`-style self-reference and
            // ordinary declarations land in a stable register. declare_local
            // boxes the register into a cell if a nested function captures it.
            // A captured function-body-level lexical pre-created as a cell at entry
            // (so a forward-referenced function could capture it) is REUSED here
            // rather than shadowed, so the closure and this declaration share one
            // cell; otherwise a fresh binding is allocated.
            let reg = if self.scopes.len() == 1 && self.entry_lexicals.contains(name) {
                self.scopes[0]
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, r)| *r)
                    .unwrap_or_else(|| self.declare_local(name))
            } else if let Some(r) = self
                .scopes
                .last()
                .unwrap()
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, r)| *r)
                .filter(|r| self.block_tdz_cells.contains(r))
            {
                // A block-entry pre-created TDZ cell (captured forward-referenced
                // lexical): the textual declaration reuses it, ending the TDZ.
                self.block_tdz_cells.remove(&r);
                r
            } else {
                self.declare_local(name)
            };
            if is_const {
                self.const_regs.insert(reg);
            }
            self.lexical_regs.insert(reg);
            let is_cell = self.cell_regs.contains(&reg);
            // A lexical declaration's own initializer reading its binding
            // (`let x = x + 1`, `const c = c`, `using u = u`) is a TDZ
            // ReferenceError: the binding initializes only when the
            // declaration completes.
            let tdz_added = d.kind.is_lexical() && self.param_tdz.insert(name.to_string());
            if let Some(init) = &decl.init {
                if is_cell {
                    // The init value must be written THROUGH the cell.
                    let tmp = self.temp();
                    let v = self.compile_named_init(tmp, init, name)?;
                    self.emit(Instr::CellSet { cell: reg, src: v });
                    self.next_reg -= 1; // reclaim tmp
                } else {
                    let v = self.compile_named_init(reg, init, name)?;
                    if v != reg {
                        self.emit(Instr::Move { dst: reg, src: v });
                    }
                }
            }
            if tdz_added {
                self.param_tdz.remove(name);
            }
            if decl.init.is_none() {
                if is_cell {
                    // A bare `let x;` initializes the binding to undefined, exiting its
                    // TDZ. A reused entry-precreated cell starts UNINITIALIZED, so this
                    // is where it becomes legal to read; an ordinary captured cell is
                    // already undefined, so this is a harmless re-set.
                    let t = self.temp();
                    self.emit(Instr::LoadUndefined { dst: t });
                    self.emit(Instr::CellSet { cell: reg, src: t });
                    self.next_reg -= 1;
                } else {
                    self.emit(Instr::LoadUndefined { dst: reg });
                }
            }
            // A `using`/`await using x = init` registers its resource for disposal
            // at block exit (after the binding is stored). `using_scope_reg` is set
            // by the enclosing `compile_using_block`; it is always present for such a
            // declaration (the block/body/try that contains one is wrapped).
            let using_async = match d.kind {
                ox::VariableDeclarationKind::Using => Some(false),
                ox::VariableDeclarationKind::AwaitUsing => Some(true),
                _ => None,
            };
            // `await using` is only legal where `await` is (async function /
            // module top level). Erroring here (instead of mis-compiling a
            // sync disposal) also routes a loader-compiled module entry onto
            // the direct async path via the TLA containment.
            if using_async == Some(true) && !self.in_async {
                return Err("`await` is only valid inside an async function".into());
            }
            if let (Some(is_async_using), Some(scope_reg)) = (using_async, self.using_scope_reg) {
                let src = if is_cell {
                    let t = self.temp();
                    self.emit(Instr::CellGet { dst: t, cell: reg });
                    t
                } else {
                    reg
                };
                if is_async_using {
                    self.emit(Instr::RegisterAsyncDisposable { scope: scope_reg, val: src });
                } else {
                    self.emit(Instr::RegisterDisposable { scope: scope_reg, val: src });
                }
                if is_cell {
                    self.next_reg -= 1;
                }
            }
        }
        Ok(())
    }

    /// Resolve the binding of a `for (var x in/of …)` HEAD: the loop creates
    /// NO binding of its own — it assigns the existing one (a shadowing catch
    /// parameter, the hoisted function-scope var, or the script global). A
    /// first-mention name creates the FUNCTION-scoped var binding here (the
    /// `var` hoist), exactly like `var_decl`'s function path.
    fn head_var_binding(&mut self, name: &str) -> Binding {
        // Innermost existing binding (catch param / pre-declared var / param).
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
        if self.is_script && self.cx.script_binds_globals {
            return Binding::Global(self.cx.global_slot(name) as u32);
        }
        // EVAL root with the name in the caller scope: assign through the
        // seeded caller upvalue (mirrors var_decl's eval-caller path).
        if self.is_script && self.cx.eval_caller_scope.iter().any(|n| n == name) {
            if let Some(idx) = self.resolve_upvalue(name) {
                return Binding::Upvalue(idx);
            }
        }
        // First mention: create the function-scoped var binding (scopes[0]).
        let r = self.alloc_reg();
        self.scopes[0].push((name.to_string(), r));
        if self.box_all_locals || self.captured.contains(name) {
            self.emit(Instr::MakeCell { reg: r });
            self.cell_regs.insert(r);
            Binding::LocalCell(r)
        } else {
            Binding::Local(r)
        }
    }

    /// Phase 1 of a destructuring declaration: declare every leaf binding the
    /// pattern introduces, so they occupy stable (low) registers / global slots
    /// and any captured ones are boxed before extraction writes to them.
    fn declare_pattern(&mut self, pat: &ox::BindingPattern) -> R<()> {
        use ox::BindingPattern as P;
        match pat {
            P::BindingIdentifier(id) => {
                if self.is_script && !self.pattern_block_local {
                    self.cx.global_slot(&id.name);
                } else if self.scopes.len() == 1 && self.entry_lexicals.contains(id.name.as_str())
                {
                    // Pre-created as a cell at entry (a captured forward-referenced
                    // lexical); reuse it so extraction and the capturing closure
                    // share one cell rather than shadowing with a fresh binding.
                } else {
                    self.declare_local(&id.name);
                }
                Ok(())
            }
            P::AssignmentPattern(ap) => self.declare_pattern(&ap.left),
            P::ObjectPattern(op) => {
                for prop in &op.properties {
                    self.declare_pattern(&prop.value)?;
                }
                if let Some(rest) = &op.rest {
                    self.declare_pattern(&rest.argument)?;
                }
                Ok(())
            }
            P::ArrayPattern(arr) => {
                for el in arr.elements.iter().flatten() {
                    self.declare_pattern(el)?;
                }
                if let Some(rest) = &arr.rest {
                    self.declare_pattern(&rest.argument)?;
                }
                Ok(())
            }
        }
    }

    /// Phase 2: extract values from `src` (the initializer's value) into the
    /// already-declared bindings. Every temp this allocates sits above the
    /// declared locals, so callers reclaim them with a single `next_reg` reset.
    fn extract_pattern(&mut self, pat: &ox::BindingPattern, src: Reg) -> R<()> {
        use ox::BindingPattern as P;
        match pat {
            P::BindingIdentifier(id) => {
                let b = self.resolve(&id.name);
                self.store_binding(&b, src);
                Ok(())
            }
            // `target = default`: `src` is our scratch temp, so patch the default
            // into it in place when it came out undefined, then bind the target.
            P::AssignmentPattern(ap) => {
                // `[x = function(){}]` ⇒ the default function takes the name "x".
                let name = match &ap.left {
                    ox::BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
                    _ => None,
                };
                self.apply_default_in_place_named(src, &ap.right, name.as_deref())?;
                self.extract_pattern(&ap.left, src)
            }
            P::ObjectPattern(op) => {
                // RequireObjectCoercible(src): an object pattern with NO named
                // properties (`{}` or `{...rest}`) never performs a member access,
                // so without this an empty pattern would silently accept null /
                // undefined. (A pattern WITH named properties throws via the
                // GetProp/GetIndex below.)
                if op.properties.is_empty() {
                    self.emit(Instr::CheckCoercible { src });
                }
                // With a `...rest` AND a computed sibling key, the exclusion set
                // isn't known until runtime: evaluate each sibling key once into a
                // contiguous block (reused for extraction + ObjectRestDyn).
                if op.rest.is_some() && op.properties.iter().any(|p| p.computed) {
                    let block_save = self.next_reg;
                    let keys_base = self.next_reg;
                    let n = op.properties.len() as u16;
                    for _ in 0..op.properties.len() {
                        self.alloc_reg();
                    }
                    for (i, prop) in op.properties.iter().enumerate() {
                        let kreg = keys_base + i as Reg;
                        if prop.computed {
                            let e = prop
                                .key
                                .as_expression()
                                .ok_or("unsupported computed destructuring key")?;
                            let v = self.expr_into(e, kreg)?;
                            if v != kreg {
                                self.emit(Instr::Move { dst: kreg, src: v });
                            }
                        } else {
                            let name = class_key_name(&prop.key)?;
                            let idx = self.add_string_const(&name);
                            self.emit(Instr::LoadConst { dst: kreg, idx });
                        }
                    }
                    for (i, prop) in op.properties.iter().enumerate() {
                        let save = self.next_reg;
                        let kreg = keys_base + i as Reg;
                        let val = self.alloc_reg();
                        self.emit(Instr::GetIndex { dst: val, obj: src, key: kreg });
                        self.extract_pattern(&prop.value, val)?;
                        self.next_reg = save;
                    }
                    let rest = op.rest.as_ref().unwrap();
                    let save = self.next_reg;
                    let val = self.alloc_reg();
                    self.emit(Instr::ObjectRestDyn { dst: val, src, keys_base, n });
                    self.extract_pattern(&rest.argument, val)?;
                    self.next_reg = save;
                    self.next_reg = block_save;
                    return Ok(());
                }
                for prop in &op.properties {
                    let save = self.next_reg;
                    let val = self.alloc_reg();
                    self.extract_member(src, &prop.key, prop.computed, val)?;
                    self.extract_pattern(&prop.value, val)?;
                    self.next_reg = save;
                }
                // `...rest` — a new object of `src`'s own keys minus the siblings.
                if let Some(rest) = &op.rest {
                    // Lay the excluded (sibling) names out contiguously so the op
                    // can reference them by index range.
                    let exclude_start = self.string_constants.len() as u32;
                    let mut exclude_count = 0u16;
                    for prop in &op.properties {
                        let key = class_key_name(&prop.key)
                            .map_err(|_| "object-rest with a computed sibling key is not in the subset")?;
                        self.string_name(&key);
                        exclude_count += 1;
                    }
                    let save = self.next_reg;
                    let val = self.alloc_reg();
                    self.emit(Instr::ObjectRest { dst: val, src, exclude_start, exclude_count });
                    self.extract_pattern(&rest.argument, val)?;
                    self.next_reg = save;
                }
                Ok(())
            }
            P::ArrayPattern(arr) => {
                // JS array destructuring uses the iterator protocol; positional
                // GetIndex matches it for arrays/strings/Map/Set, so we only need
                // to drain a generator / custom iterable into an array first.
                let src = {
                    let norm = self.alloc_reg();
                    // Pull only as many as the fixed elements need (unbounded with
                    // a `...rest`), so destructuring an infinite iterator is fine.
                    let count =
                        if arr.rest.is_some() { u32::MAX } else { arr.elements.len() as u32 };
                    self.emit(Instr::IterToArray { dst: norm, src, count });
                    norm
                };
                for (i, el) in arr.elements.iter().enumerate() {
                    if let Some(p) = el {
                        let save = self.next_reg;
                        let val = self.alloc_reg();
                        let idx = self.alloc_reg();
                        self.emit(Instr::LoadInt { dst: idx, val: i as i32 });
                        self.emit(Instr::GetIndex { dst: val, obj: src, key: idx });
                        self.extract_pattern(p, val)?;
                        self.next_reg = save;
                    }
                    // a hole (`[, x]`) binds nothing
                }
                if let Some(rest) = &arr.rest {
                    let save = self.next_reg;
                    let val = self.alloc_reg();
                    self.emit(Instr::ArrayRest { dst: val, src, start: arr.elements.len() as u32 });
                    self.extract_pattern(&rest.argument, val)?;
                    self.next_reg = save;
                }
                Ok(())
            }
        }
    }

    /// Read `obj[key]` into `dst` for a destructuring property. A static key
    /// (identifier / string / number) uses GetProp; a computed `[expr]` key is
    /// evaluated and read with GetIndex.
    fn extract_member(
        &mut self,
        obj: Reg,
        key: &ox::PropertyKey,
        computed: bool,
        dst: Reg,
    ) -> R<()> {
        if computed {
            let e = key
                .as_expression()
                .ok_or("unsupported computed destructuring key")?;
            let save = self.next_reg; // `dst` was allocated below this
            let k = self.expr(e)?;
            self.emit(Instr::GetIndex { dst, obj, key: k });
            self.next_reg = save; // reclaim the key-expression temps
            return Ok(());
        }
        let name = match key {
            ox::PropertyKey::StaticIdentifier(id) => id.name.to_string(),
            ox::PropertyKey::StringLiteral(s) => string_literal_key(s),
            ox::PropertyKey::NumericLiteral(n) => fmt_key_num(n.value),
                                ox::PropertyKey::BigIntLiteral(b) => b.value.to_string(),
            _ => return Err("unsupported destructuring property key".into()),
        };
        let nidx = self.string_name(&name);
        self.emit(Instr::GetProp { dst, obj, name: nidx });
        Ok(())
    }

    /// `if (reg === undefined) reg = default` — apply a destructuring/parameter
    /// default to a scratch register in place.
    fn apply_default_in_place(&mut self, reg: Reg, default: &ox::Expression) -> R<()> {
        self.apply_default_in_place_named(reg, default, None)
    }

    /// As `apply_default_in_place`, but when the default fills a single named
    /// binding (`[x = function(){}]` ⇒ `x.name === "x"`), infer that name for an
    /// anonymous function/class default (NamedEvaluation).
    fn apply_default_in_place_named(
        &mut self,
        reg: Reg,
        default: &ox::Expression,
        name: Option<&str>,
    ) -> R<()> {
        let save = self.next_reg;
        let undef = self.alloc_reg();
        self.emit(Instr::LoadUndefined { dst: undef });
        let cond = self.alloc_reg();
        self.emit(Instr::Eq { dst: cond, a: reg, b: undef });
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 }); // skip default when defined
        let dv = match name {
            Some(n) => self.compile_named_init(reg, default, n)?,
            None => self.expr_into(default, reg)?,
        };
        if dv != reg {
            self.emit(Instr::Move { dst: reg, src: dv });
        }
        let end = self.here();
        self.patch_jump(jf, end);
        self.next_reg = save;
        Ok(())
    }

    fn func_decl(&mut self, f: &ox::Function) -> R<()> {
        self.func_decl_inner(f, true)
    }

    /// Annex B B.3.3.3 var-binding sync for a block function named `name`,
    /// reading the value from the BLOCK binding (`src`): emitted at the
    /// declaration's TEXTUAL position (SetMutableBinding happens when the
    /// declaration is evaluated, not at block entry).
    fn emit_b33_sync(&mut self, name: &str) {
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
            let caller_b33_upval = if self.cx.eval_fn_context
                && self.cx.eval_caller_scope.iter().any(|c| c == name)
            {
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

    fn func_decl_inner(&mut self, f: &ox::Function, do_sync: bool) -> R<()> {
        let name = f.id.as_ref().map(|i| i.name.to_string());
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
            f.generator,
            f.r#async,
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
                        && self.cx.eval_caller_scope.iter().any(|c| c == n) =>
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
    fn class_decl(&mut self, class: &ox::Class) -> R<()> {
        let name = class.id.as_ref().map(|i| i.name.to_string());
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
    fn build_class_into(&mut self, class: &ox::Class, cls: Reg, name: Option<&str>) -> R<()> {
        let (class_id, static_fields, computed, computed_fields, static_block_fns, static_order) =
            self.compile_class(class, name)?;
        // Evaluate the superclass value (`extends P`) into a temp the VM links in.
        let parent_reg = if let Some(sc) = &class.super_class {
            let t = self.temp();
            // ClassHeritage evaluates in STRICT mode (the whole ClassTail is
            // strict code), regardless of the enclosing scope.
            let prev_strict = self.cx.in_strict;
            self.cx.in_strict = true;
            // A NAMED class's own binding is in scope for the heritage.
            let heritage_named = class.id.as_ref().map(|id| id.name.to_string());
            let saved_hc = self.heritage_class.take();
            if let Some(n) = &heritage_named {
                self.cx.heritage_classes.push((n.clone(), class_id));
                self.heritage_class = Some((n.clone(), class_id));
            }
            let r = self.expr_into(sc, t);
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
        // super base.
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
                    let v = match finit {
                        Some(e) => self.expr(e)?,
                        None => {
                            let t = self.temp();
                            self.emit(Instr::LoadUndefined { dst: t });
                            t
                        }
                    };
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
                    let vr = match init {
                        Some(e) => self.expr(e)?,
                        None => {
                            let t = self.temp();
                            self.emit(Instr::LoadUndefined { dst: t });
                            t
                        }
                    };
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
    fn class_expr(&mut self, class: &ox::Class, dst: Reg, name: Option<&str>) -> R<Reg> {
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
    fn compile_class<'b>(
        &mut self,
        class: &'b ox::Class,
        name: Option<&str>,
    ) -> R<(
        u32,
        Vec<(String, Option<&'b ox::Expression<'b>>)>,
        Vec<(&'b ox::Expression<'b>, u32, u8)>,
        Vec<(&'b ox::Expression<'b>, Option<&'b ox::Expression<'b>>, bool)>,
        Vec<u32>,
        Vec<(u8, usize)>,
    )> {
        // A named class expression keeps its own name; an anonymous one inherits
        // the binding it's assigned to (NamedEvaluation), else the "<class>" stub.
        let cname = class
            .id
            .as_ref()
            .map(|i| i.name.to_string())
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
        self.cx.class_derived = class.super_class.is_some();
        let mut ctor_fn: Option<&ox::Function> = None;
        // Each method's value-Function start byte → its MethodDefinition span, so
        // `Function.prototype.toString` can recover the exact `m() {}` / `get x()
        // {}` source (the value-Function span omits the method name/key).
        let mut method_spans: std::collections::HashMap<u32, (u32, u32)> =
            std::collections::HashMap::new();
        let mut methods: Vec<(String, &ox::Function)> = Vec::new();
        let mut getters: Vec<(String, &ox::Function)> = Vec::new();
        let mut setters: Vec<(String, &ox::Function)> = Vec::new();
        let mut statics: Vec<(String, &ox::Function)> = Vec::new();
        let mut static_getters: Vec<(String, &ox::Function)> = Vec::new();
        let mut static_setters: Vec<(String, &ox::Function)> = Vec::new();
        let mut fields: Vec<(String, Option<&ox::Expression>)> = Vec::new();
        let mut static_fields: Vec<(String, Option<&'b ox::Expression<'b>>)> = Vec::new();
        // `static { … }` initializer blocks, in source order. Each is compiled to a
        // thunk and run once at class definition time with `this` = the class.
        let mut static_blocks: Vec<&'b [ox::Statement<'b>]> = Vec::new();
        // Computed-key fields (`[expr] = v` / `static [expr] = v`). Their KEYS are
        // evaluated once at class definition (in source order, see class_decl);
        // `computed_fields_ordered` drives that. Instance ones also need their init
        // run per-instance in the ctor — `instance_computed_inits` (index i ↔ the
        // i-th instance computed key) feeds the ctor's `FieldInit` ops.
        #[allow(clippy::type_complexity)]
        let mut computed_fields_ordered: Vec<(
            &'b ox::Expression<'b>,
            Option<&'b ox::Expression<'b>>,
            bool,
        )> = Vec::new();
        // Source order of STATIC elements: (0=named field, 1=computed field,
        // 2=static block) -> index into its vec; drives phase-2 evaluation.
        let mut static_order: Vec<(u8, usize)> = Vec::new();
        let mut instance_computed_inits: Vec<Option<&'b ox::Expression<'b>>> = Vec::new();
        // Members with a runtime-computed key (`[expr]() {}`) — the key is
        // evaluated and the member installed at class-creation time (see
        // class_decl). kind: 0=method 1=getter 2=setter 3=static method.
        let mut computed: Vec<(&'b ox::Expression<'b>, &'b ox::Function<'b>, u8)> = Vec::new();
        for el in &class.body.body {
            match el {
                ox::ClassElement::MethodDefinition(m) => {
                    // Record the full MethodDefinition span (keyed by the value
                    // function's start) for toString source recovery.
                    method_spans.insert(m.value.span.start, (m.span.start, m.span.end));
                    // A constructor is never computed; otherwise a key that
                    // class_key_name can't name statically (and is `computed`) is a
                    // runtime-keyed member.
                    // kind: 0=method 1=getter 2=setter 3=static method
                    //       4=static getter 5=static setter
                    let kind = match m.kind {
                        ox::MethodDefinitionKind::Constructor => {
                            ctor_fn = Some(&m.value);
                            continue;
                        }
                        ox::MethodDefinitionKind::Get if m.r#static => 4u8,
                        ox::MethodDefinitionKind::Set if m.r#static => 5u8,
                        ox::MethodDefinitionKind::Get => 1u8,
                        ox::MethodDefinitionKind::Set => 2u8,
                        ox::MethodDefinitionKind::Method if m.r#static => 3u8,
                        ox::MethodDefinitionKind::Method => 0u8,
                    };
                    // A DUPLICATE member name in the same list (`get b(){}` +
                    // `get ['b'](){}` — both statically nameable) REPLACES the
                    // earlier definition (last wins) while keeping its original
                    // position in property order.
                    fn put_member<'x>(
                        list: &mut Vec<(String, &'x ox::Function<'x>)>,
                        name: String,
                        f: &'x ox::Function<'x>,
                    ) {
                        if let Some(slot) = list.iter_mut().find(|(n, _)| *n == name) {
                            slot.1 = f;
                        } else {
                            list.push((name, f));
                        }
                    }
                    match class_key_name(&m.key) {
                        Ok(name) => match (m.r#static, m.kind) {
                            (true, ox::MethodDefinitionKind::Method) => put_member(&mut statics, name, &m.value),
                            (true, ox::MethodDefinitionKind::Get) => put_member(&mut static_getters, name, &m.value),
                            (true, ox::MethodDefinitionKind::Set) => put_member(&mut static_setters, name, &m.value),
                            (true, ox::MethodDefinitionKind::Constructor) => unreachable!(),
                            (false, ox::MethodDefinitionKind::Method) => put_member(&mut methods, name, &m.value),
                            (false, ox::MethodDefinitionKind::Get) => put_member(&mut getters, name, &m.value),
                            (false, ox::MethodDefinitionKind::Set) => put_member(&mut setters, name, &m.value),
                            (false, ox::MethodDefinitionKind::Constructor) => unreachable!(),
                        },
                        Err(e) if m.computed => {
                            let key = m.key.as_expression().ok_or(e)?;
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
                                list.push((ph, &m.value));
                            }
                            computed.push((key, &m.value, kind));
                        }
                        Err(e) => return Err(e),
                    }
                }
                ox::ClassElement::PropertyDefinition(p) => {
                    match class_key_name(&p.key) {
                        // A COMPUTED key whose literal folds to a "#..." STRING
                        // is a PUBLIC property that merely looks private — route it
                        // through the computed path (define_field → an ordinary,
                        // visible own prop) so it never collides with the class's
                        // real same-named private element.
                        Ok(name) if p.computed && name.starts_with('#') => {
                            let key = p
                                .key
                                .as_expression()
                                .ok_or("unsupported computed class field key")?;
                            computed_fields_ordered.push((key, p.value.as_ref(), p.r#static));
                            if p.r#static {
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
                        Ok(name) if p.r#static && p.computed && name == "prototype" => {
                            let key = p
                                .key
                                .as_expression()
                                .ok_or("unsupported computed class field key")?;
                            computed_fields_ordered.push((key, p.value.as_ref(), true));
                            static_order.push((1, computed_fields_ordered.len() - 1));
                        }
                        // Static string key.
                        Ok(name) if p.r#static => {
                            static_fields.push((name, p.value.as_ref()));
                            static_order.push((0, static_fields.len() - 1));
                        }
                        // Instance string key.
                        Ok(name) => fields.push((name, p.value.as_ref())),
                        // Computed key `[expr] = v` — evaluated once at class def.
                        Err(e) => {
                            let key = p.key.as_expression().ok_or(e)?;
                            computed_fields_ordered.push((key, p.value.as_ref(), p.r#static));
                            if p.r#static {
                                static_order.push((1, computed_fields_ordered.len() - 1));
                            } else {
                                instance_computed_inits.push(p.value.as_ref());
                            }
                        }
                    }
                }
                ox::ClassElement::StaticBlock(b) => {
                    static_blocks.push(&b.body);
                    static_order.push((2, static_blocks.len() - 1));
                }
                _ => return Err("unsupported class member in the zipp-vm subset".into()),
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
                Some(&*func.params),
                &[],
                &[],
                body,
                super_class_id,
                false, // instance method: super resolves via the prototype chain
                func.generator,
                func.r#async,
            )?;
            if let Some(&(s, e)) = method_spans.get(&func.span.start) {
                proto.source = self.cx.src_slice(s, e);
            }
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
                Some(&*func.params),
                &[],
                &[],
                body,
                super_class_id,
                false, // instance getter: super resolves via the prototype chain
                false, // getters are never generators
                false, // getters are never async
            )?;
            if let Some(&(s, e)) = method_spans.get(&func.span.start) {
                proto.source = self.cx.src_slice(s, e);
            }
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
                Some(&*func.params),
                &[],
                &[],
                body,
                super_class_id,
                false, // instance setter: super resolves via the prototype chain
                false, // setters are never generators
                false, // setters are never async
            )?;
            if let Some(&(s, e)) = method_spans.get(&func.span.start) {
                proto.source = self.cx.src_slice(s, e);
            }
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
                Some(&*func.params),
                &[],
                &[],
                body,
                super_class_id,
                true, // static method: `super.x` resolves via the class's [[Prototype]] (parent class)
                func.generator,
                func.r#async,
            )?;
            if let Some(&(s, e)) = method_spans.get(&func.span.start) {
                proto.source = method_source(self.cx.src_slice(s, e), true);
            }
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
                Some(&*func.params),
                &[],
                &[],
                body,
                super_class_id,
                true, // static getter: `super.x` resolves via the class's [[Prototype]]
                false,
                false,
            )?;
            if let Some(&(s, e)) = method_spans.get(&func.span.start) {
                proto.source = method_source(self.cx.src_slice(s, e), true);
            }
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
                Some(&*func.params),
                &[],
                &[],
                body,
                super_class_id,
                true, // static setter: `super.x` resolves via the class's [[Prototype]]
                false,
                false,
            )?;
            if let Some(&(s, e)) = method_spans.get(&func.span.start) {
                proto.source = method_source(self.cx.src_slice(s, e), true);
            }
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
            let params_ast = ctor_fn.map(|f| &*f.params);
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
                if let Some(&(s, e)) = method_spans.get(&cf.span.start) {
                    proto.source = self.cx.src_slice(s, e);
                }
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
        let mut computed_defs: Vec<(&'b ox::Expression<'b>, u32, u8)> = Vec::new();
        for (key, func, kind) in &computed {
            let (params, rest, body) = function_parts(func)?;
            let mut proto = self.cx.compile_class_fn(
                &format!("{cname}.[computed]"),
                &params,
                rest.as_deref(),
                Some(&*func.params),
                &[],
                &[],
                body,
                super_class_id,
                matches!(*kind, 3 | 4 | 5), // static computed members get the parent-class super base
                func.generator,
                func.r#async,
            )?;
            if let Some(&(s, e)) = method_spans.get(&func.span.start) {
                proto.source = method_source(self.cx.src_slice(s, e), matches!(*kind, 3 | 4 | 5));
            }
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
    fn child_enclosing(&self) -> Vec<EnclosingFn> {
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
    fn stash_child_with_shadows(&mut self, bound: &[String], body: &[ox::Statement]) {
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
    fn compile_func_expr(&mut self, name: Option<String>, f: &ox::Function) -> R<(u32, bool)> {
        // Strict mode: a named function expression may not be named `eval`/`arguments`.
        // Use the SYNTACTIC name (`f.id`), not the inferred NamedEvaluation name.
        if let Some(id) = &f.id {
            strict_name_err(
                self.cx.in_strict || has_use_strict(fn_directives(f)),
                id.name.as_str(),
            )?;
        }
        // A named function expression's own name is self-bound (resolves to the
        // function) inside the body — and a nested closure may capture it, so add it
        // to the capture-analysis name set. Use the SYNTACTIC name (`f.id`), not the
        // inferred NamedEvaluation name (an anonymous expr has no self-binding).
        let self_name = f.id.as_ref().map(|i| i.name.to_string());
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
            f.generator,
            f.r#async,
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
    fn compile_arrow(&mut self, a: &ox::ArrowFunctionExpression, name: &str) -> R<(u32, bool)> {
        let params = param_slot_names(&a.params)?;
        let rest = rest_name(&a.params)?;
        let mut names = with_rest(&params, &rest);
        names.extend(param_pattern_leaves(&a.params));
        names.extend(hoisted_var_names(&a.body.statements)); // function-scoped `var`s (capture)
        let captured = capture::captured_locals(&names, &a.body.statements);
        self.stash_child_with_shadows(&names, &a.body.statements);
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
    fn emit_make_callable(&mut self, dst: Reg, id: u32, has_upvalues: bool) {
        if has_upvalues {
            self.emit(Instr::MakeClosure { dst, func_id: id });
        } else {
            self.emit(Instr::MakeFunc { dst, func_id: id });
        }
    }

    /// Early SyntaxError (spec: AllPrivateIdentifiersValid) for a private
    /// access whose name no enclosing class declares (and, in a direct eval,
    /// is not visible from the call site).
    fn check_private_declared(&self, raw: &str) -> R<()> {
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
    fn this_check(&mut self) {
        if self.in_derived_ctor && self.this_override.is_none() {
            self.emit(Instr::ThisCheck { src: 0 });
        }
    }

    /// Emit creation of an ARROW value. Always `MakeArrow` (even with no
    /// upvalues) so the resulting closure carries the lexically-captured `this`
    /// of the defining frame — `MakeFunc` has no slot for it. The captured `this`
    /// is read from the effective-`this` register at the definition site
    /// (`this_override` when inside a static field initializer, else reg 0).
    fn emit_make_arrow(&mut self, dst: Reg, id: u32) {
        let this_reg = self.this_override.unwrap_or(0);
        self.emit(Instr::MakeArrow { dst, func_id: id, this_reg });
    }

    /// Compile an `if`/`else` BRANCH. Annex B B.3.3: a bare FunctionDeclaration
    /// used directly as a branch (not inside a `{ }` block) is block-scoped to
    /// that branch. Declare it block-local first — using the SAME guard as the
    /// BlockStatement hoisting pre-pass — so `func_decl` binds the local instead
    /// of overwriting an enclosing parameter / lexical of the same name. The
    /// Annex B function-scoped `var` assignment (b33_names / s0reg in func_decl)
    /// still runs for the non-conflicting case.
    fn branch_stmt(&mut self, s: &ox::Statement) -> R<()> {
        if let ox::Statement::FunctionDeclaration(f) = s {
            if let Some(id) = &f.id {
                let nm = id.name.as_str();
                self.push_scope();
                // Block-local when the name must stay block-scoped: strict mode, an
                // enclosing-block lexical conflict, a protected param/lexical/class
                // (skip), or a B.3.3 var name (block-local + func-scope sync). NOT
                // for a same-named existing function (it is directly updated).
                if self.cx.in_strict
                    || self.block_fn_conflicts(nm)
                    || self.protect_names.contains(nm)
                    || self.b33_names.contains(nm)
                {
                    self.declare_local(nm);
                }
                let r = self.stmt(s);
                self.pop_scope();
                return r;
            }
        }
        self.stmt(s)
    }

    fn if_stmt(&mut self, i: &ox::IfStatement) -> R<()> {
        // The statement's completion V starts as undefined (a not-taken / empty
        // branch yields undefined, not the prior statement's value). No-op outside
        // eval mode.
        self.reset_loop_completion();
        let cond = self.expr(&i.test)?;
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 }); // patched
        self.branch_stmt(&i.consequent)?;
        if let Some(alt) = &i.alternate {
            let jmp = self.here();
            self.emit(Instr::Jump { target: 0 }); // patched
            let else_start = self.here();
            self.patch_jump(jf, else_start);
            self.branch_stmt(alt)?;
            let end = self.here();
            self.patch_jump(jmp, end);
        } else {
            let end = self.here();
            self.patch_jump(jf, end);
        }
        Ok(())
    }

    /// In eval mode, a loop's completion value starts as `undefined` (spec: the
    /// loop's V is initialized to undefined, then updated by each non-empty body
    /// completion). Emitting this once before the loop makes
    /// `eval('1; do { break; } while(false)')` undefined rather than 1, while
    /// `eval('do { 3 } while(false)')` stays 3. No-op outside eval mode.
    fn reset_loop_completion(&mut self) {
        if let Some(cr) = self.completion_reg {
            self.emit(Instr::LoadUndefined { dst: cr });
        }
    }

    fn while_stmt(&mut self, w: &ox::WhileStatement) -> R<()> {
        let top = self.here();
        let cond = self.expr(&w.test)?;
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 });
        self.loop_ctx.push(LoopCtx::loop_frame(self.pending_label.take(), self.handler_depth));
        self.stmt(&w.body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        for c in ctx.continue_jumps {
            self.patch_jump(c, top); // continue → re-test
        }
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        self.patch_jump(jf, end);
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
        Ok(())
    }

    /// Look up an in-scope local's register (innermost scope first), if any.
    fn local_reg(&self, name: &str) -> Option<Reg> {
        for scope in self.scopes.iter().rev() {
            for (n, r) in scope.iter().rev() {
                if n == name {
                    return Some(*r);
                }
            }
        }
        None
    }

    /// The registers of a declaration's simple-identifier bindings that are
    /// cell-boxed (captured by a nested closure) — the ones needing per-iteration
    /// freshening in a loop head.
    fn captured_decl_regs(&self, d: &ox::VariableDeclaration) -> Vec<Reg> {
        let mut regs = Vec::new();
        for decl in &d.declarations {
            if let ox::BindingPattern::BindingIdentifier(id) = &decl.id {
                if let Some(reg) = self.local_reg(id.name.as_str()) {
                    if self.cell_regs.contains(&reg) {
                        regs.push(reg);
                    }
                }
            }
        }
        regs
    }

    /// Rebind each register to a FRESH cell holding its current value (read the
    /// old cell, re-wrap). Used for per-iteration `let` loop bindings.
    fn emit_freshen_cells(&mut self, regs: &[Reg]) {
        for &reg in regs {
            let tmp = self.alloc_reg();
            self.emit(Instr::CellGet { dst: tmp, cell: reg });
            self.emit(Instr::Move { dst: reg, src: tmp });
            self.emit(Instr::MakeCell { reg });
            self.next_reg -= 1; // free tmp
        }
    }

    fn for_stmt(&mut self, f: &ox::ForStatement) -> R<()> {
        self.push_scope();
        // `for (using x = r; …) body` / `for (await using …)`: the resource is
        // LOOP-scoped (disposed ONCE when the for-statement completes, normal or
        // abrupt), unlike for-of which disposes per-iteration. Open a scope + a
        // finally wrapping the whole loop; the `using` init registers x into it.
        let using_async: Option<bool> = match &f.init {
            Some(ox::ForStatementInit::VariableDeclaration(d)) => match d.kind {
                ox::VariableDeclarationKind::Using => Some(false),
                ox::VariableDeclarationKind::AwaitUsing => Some(true),
                _ => None,
            },
            _ => None,
        };
        let using_ctx = if let Some(is_async) = using_async {
            let sreg = self.declare_local("<for.uscope>");
            let kreg = self.declare_local("<for.ukind>");
            let vreg = self.declare_local("<for.uval>");
            self.emit(Instr::OpenUsingScope { dst: sreg });
            let push_at = self.here();
            self.emit(Instr::PushFinally { target: 0, kind_reg: kreg, val_reg: vreg });
            self.handler_depth += 1;
            Some((is_async, sreg, kreg, vreg, push_at))
        } else {
            None
        };
        // init
        if let Some(init) = &f.init {
            match init {
                ox::ForStatementInit::VariableDeclaration(d) => {
                    // While compiling the `using` init, route its RegisterDisposable
                    // to the loop scope (restored after, so a using-block in the body
                    // uses its own scope).
                    let prev = using_ctx.map(|(_, s, _, _, _)| self.using_scope_reg.replace(s));
                    self.var_decl(d)?;
                    if let Some(p) = prev {
                        self.using_scope_reg = p;
                    }
                }
                other => {
                    let e = other
                        .as_expression()
                        .ok_or("unsupported for-init")?;
                    self.expr(e)?;
                }
            }
        }
        // Per-iteration bindings: a `for (let i …)` loop variable captured by a
        // closure in the body gets a FRESH binding each iteration (JS semantics:
        // `for(let i…) fns.push(()=>i)` yields 0,1,2 not 3,3,3). Only when the var
        // is actually boxed (captured) — otherwise the plain-register fast path
        // (and the hot-loop JIT) is preserved. `var` is function-scoped → no
        // freshening.
        let fresh_regs: Vec<Reg> = match &f.init {
            Some(ox::ForStatementInit::VariableDeclaration(d))
                if d.kind != ox::VariableDeclarationKind::Var =>
            {
                self.captured_decl_regs(d)
            }
            _ => Vec::new(),
        };
        let top = self.here();
        let jf = match &f.test {
            Some(t) => {
                let cond = self.expr(t)?;
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond, target: 0 });
                Some(j)
            }
            None => None,
        };
        self.loop_ctx.push(LoopCtx::loop_frame(self.pending_label.take(), self.handler_depth));
        self.stmt(&f.body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        let cont = self.here();
        for c in ctx.continue_jumps {
            self.patch_jump(c, cont); // continue → freshen, run the update, re-test
        }
        // End of iteration: copy each captured loop var into a fresh cell, so the
        // body's closures (which captured the OLD cell) keep this iteration's
        // value and the update mutates the new binding.
        self.emit_freshen_cells(&fresh_regs);
        if let Some(update) = &f.update {
            self.expr(update)?;
        }
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        if let Some(j) = jf {
            self.patch_jump(j, end);
        }
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
        // Loop-scoped `using` disposal: the normal exit (test false) and `break`
        // land at `end` and run DisposeScope once; a throw/return out of the body
        // routes through the finally handler. (break/continue are plain jumps —
        // loop_ctx's floor is the handler depth INCLUDING this finally.)
        if let Some((is_async, sreg, kreg, vreg, push_at)) = using_ctx {
            self.emit_leave_finally_normal(kreg);
            self.handler_depth -= 1;
            let jto = self.here();
            self.emit(Instr::Jump { target: 0 });
            let fin = self.here();
            if let Instr::PushFinally { target, .. } = &mut self.code[push_at as usize] {
                *target = fin;
            }
            self.patch_jump(jto, fin);
            if is_async {
                self.emit_async_dispose_loop(sreg, kreg, vreg);
            } else {
                self.emit(Instr::DisposeScope { scope: sreg, kind_reg: kreg, val_reg: vreg });
            }
            self.emit(Instr::EndFinally { kind_reg: kreg, val_reg: vreg });
        }
        self.pop_scope();
        Ok(())
    }

    /// `try { … } catch (e) { … } finally { … }`.
    ///
    /// Without a `finally`: `PushHandler(catch, e_reg)` ; try-body ; `PopHandler` ;
    /// jump past catch. The catch lands the thrown value in `e_reg` and runs.
    ///
    /// With a `finally`: a `PushFinally` wraps the whole construct. Every exit from
    /// the try/catch — normal completion, `return`, a throw (caught here or
    /// propagating), or a `break`/`continue` that leaves the construct — routes
    /// through the single finally block, which `EndFinally` closes by resuming the
    /// recorded completion (fall through / re-return / re-throw / resume-jump),
    /// chaining through any outer finally. `break`/`continue` exit via `JumpFinally`
    /// (emitted by `emit_loop_jump` when the target's handler depth is below the
    /// current one).
    fn try_statement(&mut self, t: &ox::TryStatement) -> R<()> {
        match &t.finalizer {
            Some(finalizer) => self.try_with_finally(t, finalizer),
            None => self.try_catch_only(t),
        }
    }

    /// `try { … } catch (e) { … }` (no finalizer).
    fn try_catch_only(&mut self, t: &ox::TryStatement) -> R<()> {
        // The statement's completion V starts at undefined (an empty try/catch
        // yields undefined, not the prior statement's value).
        self.reset_loop_completion();
        let push_at = self.here();
        // catch_reg/target patched once known.
        self.emit(Instr::PushHandler { catch_target: 0, catch_reg: 0 });

        // The catch handler is active throughout the try body (a `break`/`continue`
        // exiting it must pop the stale handler — see `emit_loop_jump`).
        self.handler_depth += 1;
        self.push_scope();
        self.compile_stmt_list(&t.block.body, false)?;
        self.pop_scope();
        // After the try body the catch handler is no longer active (the catch body
        // runs with it already popped by the unwind).
        self.handler_depth -= 1;

        let handler = t.handler.as_ref().ok_or("try requires catch or finally")?;
        // Normal completion of the try: pop the handler, skip the catch.
        self.emit(Instr::PopHandler);
        let skip = self.here();
        self.emit(Instr::Jump { target: 0 });

        let catch_start = self.here();
        self.compile_catch_body(handler, push_at)?;

        let after = self.here();
        self.patch_jump(skip, after);
        let _ = catch_start;
        Ok(())
    }

    /// True iff `body` declares a top-level `using`/`await using` resource. Only
    /// such blocks/bodies get the disposal scaffolding — every other block keeps
    /// its byte-for-byte-identical fast path (the zero-regression gate).
    fn block_has_using(body: &[ox::Statement]) -> bool {
        body.iter().any(|st| {
            matches!(st,
                ox::Statement::VariableDeclaration(d)
                    if matches!(d.kind,
                        ox::VariableDeclarationKind::Using
                        | ox::VariableDeclarationKind::AwaitUsing))
        })
    }

    /// True iff `body` declares a top-level `await using` — such a scope disposes
    /// ASYNCHRONOUSLY (each dispose result is awaited), so its finally epilogue is
    /// the awaited disposal loop rather than the single sync `DisposeScope` op.
    fn block_has_await_using(body: &[ox::Statement]) -> bool {
        body.iter().any(|st| {
            matches!(st,
                ox::Statement::VariableDeclaration(d)
                    if d.kind == ox::VariableDeclarationKind::AwaitUsing)
        })
    }

    /// Emit the async-disposal epilogue (the finally body for an `await using`
    /// scope): a loop that pops each disposer LIFO, calls it, and AWAITs the result,
    /// catching a sync throw or an awaited rejection and merging it into the
    /// completion (`kind_reg`/`val_reg`) as a SuppressedError chain. An inert
    /// (null-initializer) record still performs one `Await(undefined)`, so an
    /// evaluated `await using x = null` yields a microtask tick; a scope that was
    /// opened but registered nothing (a `break` before the declaration) runs the
    /// loop zero times and awaits nothing.
    fn emit_async_dispose_loop(&mut self, scope_reg: Reg, kind_reg: Reg, val_reg: Reg) {
        let save = self.next_reg;
        let res = self.alloc_reg();
        let done = self.alloc_reg();
        let exc = self.alloc_reg();

        let loop_top = self.here();
        let push_at = self.here();
        self.emit(Instr::PushHandler { catch_target: 0, catch_reg: exc });
        self.emit(Instr::AsyncDisposeNext { scope: scope_reg, res, done });
        let jdone = self.here();
        self.emit(Instr::JumpIfTrue { cond: done, target: 0 });
        self.emit(Instr::Await { dst: res, val: res });
        self.emit(Instr::PopHandler);
        self.emit(Instr::Jump { target: loop_top });

        let done_path = self.here();
        self.emit(Instr::PopHandler);
        let jafter = self.here();
        self.emit(Instr::Jump { target: 0 });

        let cat = self.here();
        self.emit(Instr::MergeDispose { kind_reg, val_reg, err: exc });
        self.emit(Instr::Jump { target: loop_top });

        let after = self.here();
        if let Instr::PushHandler { catch_target, .. } = &mut self.code[push_at as usize] {
            *catch_target = cat;
        }
        self.patch_jump(jdone, done_path);
        self.patch_jump(jafter, after);
        self.next_reg = save; // reclaim res/done/exc
    }

    /// Compile a statement list that contains `using` declarations: desugar it onto
    /// the existing `PushFinally`/`EndFinally` machinery so the registered resources
    /// are disposed (LIFO, SuppressedError-chained) on EVERY exit — normal, throw,
    /// break, continue, return. A fresh runtime resource-scope id lives in
    /// `scope_reg` and becomes `using_scope_reg` for the body, so each `using`'s
    /// `RegisterDisposable` pushes onto it. (Sync `using` only this iteration;
    /// `await using` still binds like `let` but is not yet disposed.)
    fn compile_using_block(&mut self, body: &[ox::Statement], skip_fn_decls: bool) -> R<()> {
        // NB: unlike `try_with_finally`, do NOT reset the completion register — a
        // `using` block's own completion is empty (UpdateEmpty), so it must
        // PRESERVE the prior eval completion (`4; {using x=null;}` ⇒ 4). The
        // DisposeScope/EndFinally epilogue never writes the completion register.
        let scope_reg = self.alloc_reg();
        let kind_reg = self.alloc_reg();
        let val_reg = self.alloc_reg();
        self.emit(Instr::OpenUsingScope { dst: scope_reg });

        let fin_push = self.here();
        self.emit(Instr::PushFinally { target: 0, kind_reg, val_reg });
        self.handler_depth += 1;

        let prev_scope = self.using_scope_reg.replace(scope_reg);
        for st in body {
            // Function-body callers materialise top-level function declarations at
            // entry and skip them here (mirrors the non-using body loop).
            if skip_fn_decls {
                if let ox::Statement::FunctionDeclaration(_) = st {
                    continue;
                }
            }
            self.stmt(st)?;
        }
        self.using_scope_reg = prev_scope;

        self.emit_leave_finally_normal(kind_reg);
        let normal_jump = self.here();
        self.emit(Instr::Jump { target: 0 });

        self.handler_depth -= 1;
        let fin_start = self.here();
        if let Instr::PushFinally { target, .. } = &mut self.code[fin_push as usize] {
            *target = fin_start;
        }
        self.patch_jump(normal_jump, fin_start);

        // An `await using` scope (in an async context) disposes asynchronously —
        // emit the awaited disposal loop; otherwise the single sync DisposeScope op.
        if Self::block_has_await_using(body) && self.in_async {
            self.emit_async_dispose_loop(scope_reg, kind_reg, val_reg);
        } else {
            self.emit(Instr::DisposeScope { scope: scope_reg, kind_reg, val_reg });
        }
        self.emit(Instr::EndFinally { kind_reg, val_reg });
        self.next_reg -= 3; // reclaim scope_reg / kind_reg / val_reg
        Ok(())
    }

    /// Compile a statement list, transparently adding `using`-disposal scaffolding
    /// when the list declares a top-level `using` (else the byte-for-byte-identical
    /// plain loop). Used by the statement-list contexts that are NOT the
    /// `BlockStatement` arm — try/catch/finally bodies — so `using` inside them is
    /// disposed at the end of that block, like any other block.
    fn compile_stmt_list(&mut self, body: &[ox::Statement], skip_fn_decls: bool) -> R<()> {
        if Self::block_has_using(body) {
            self.compile_using_block(body, skip_fn_decls)
        } else {
            for s in body {
                if skip_fn_decls {
                    if let ox::Statement::FunctionDeclaration(_) = s {
                        continue;
                    }
                }
                self.stmt(s)?;
            }
            Ok(())
        }
    }

    /// `try … finally { F }` (with or without a catch). The finally runs on every
    /// exit path via a `PushFinally` handler + `EndFinally` epilogue.
    fn try_with_finally(
        &mut self,
        t: &ox::TryStatement,
        finalizer: &ox::BlockStatement,
    ) -> R<()> {
        // The statement's completion V starts at undefined (an empty try/finally
        // yields undefined). The try/catch body value is then preserved through a
        // normally-completing finally (the finally body's own value is not reset
        // here, matching "if F is normal, set F to B").
        self.reset_loop_completion();
        // Two persistent registers carry the completion record (kind + value)
        // from each exit path into the shared finally block. Allocated for the
        // whole construct; reclaimed after `EndFinally`.
        let kind_reg = self.alloc_reg();
        let val_reg = self.alloc_reg();

        let fin_push = self.here();
        self.emit(Instr::PushFinally { target: 0, kind_reg, val_reg });
        // The finally handler is active for the whole try/catch (a `break`/
        // `continue` exiting it must route through the finally — see
        // `emit_loop_jump`).
        self.handler_depth += 1;

        let has_catch = t.handler.is_some();
        let catch_push = if has_catch {
            let at = self.here();
            self.emit(Instr::PushHandler { catch_target: 0, catch_reg: 0 });
            self.handler_depth += 1; // catch handler active during the try body
            Some(at)
        } else {
            None
        };

        // Try body.
        self.push_scope();
        self.compile_stmt_list(&t.block.body, false)?;
        self.pop_scope();

        // Normal-completion jumps (from the try body and, if present, the catch
        // body) land at the finally entry below.
        let mut normal_jumps: Vec<u32> = Vec::new();
        if has_catch {
            self.emit(Instr::PopHandler);
            self.handler_depth -= 1; // catch popped; finally still active for catch body
        }
        self.emit_leave_finally_normal(kind_reg);
        normal_jumps.push(self.here());
        self.emit(Instr::Jump { target: 0 });

        // Catch body (entered by the unwind, which already popped the Catch
        // handler; the Finally handler is still active for throws inside it).
        if let (Some(catch_push), Some(handler)) = (catch_push, &t.handler) {
            self.compile_catch_body(handler, catch_push)?;
            self.emit_leave_finally_normal(kind_reg);
            normal_jumps.push(self.here());
            self.emit(Instr::Jump { target: 0 });
        }

        // The finally handler is popped before its own body runs (a `break`/
        // `return` inside the finally routes to OUTER handlers).
        self.handler_depth -= 1;

        // Finally entry.
        let fin_start = self.here();
        if let Instr::PushFinally { target, .. } = &mut self.code[fin_push as usize] {
            *target = fin_start;
        }
        for j in normal_jumps {
            self.patch_jump(j, fin_start);
        }

        // Finally body, then resume whatever completion brought us here. In eval
        // mode a NORMALLY-completing `finally` discards its own completion value —
        // the try/catch block's value is the result (`try{39}finally{1}` ⇒ 39) — so
        // save the accumulated completion across the finally body and restore it.
        let saved_cmpl = self.completion_reg.map(|cr| {
            let r = self.alloc_reg();
            self.emit(Instr::Move { dst: r, src: cr });
            r
        });
        self.push_scope();
        for s in &finalizer.body {
            self.stmt(s)?;
        }
        self.pop_scope();
        if let (Some(cr), Some(r)) = (self.completion_reg, saved_cmpl) {
            self.emit(Instr::Move { dst: cr, src: r });
            self.next_reg -= 1; // reclaim the saved-completion temp
        }
        self.emit(Instr::EndFinally { kind_reg, val_reg });

        self.next_reg -= 2; // reclaim kind_reg / val_reg
        Ok(())
    }

    /// Emit the normal-completion prologue to a finally: record kind 0 (normal)
    /// and pop the still-active `Finally` handler (abnormal paths pop it via the
    /// unwind / Return op instead).
    fn emit_leave_finally_normal(&mut self, kind_reg: Reg) {
        self.emit(Instr::LoadInt { dst: kind_reg, val: 0 });
        self.emit(Instr::PopFinally);
    }

    /// Compile a `catch (e) { … }` body and patch its `PushHandler` (at `push_at`)
    /// to land here with the thrown value in the catch register.
    fn compile_catch_body(&mut self, handler: &ox::CatchClause, push_at: u32) -> R<()> {
        // The VM deposits the thrown value into the catch register directly, so
        // reserve it WITHOUT auto-boxing; box it after if a nested closure
        // captures the binding (the value is present by then).
        let catch_start = self.here();
        self.push_scope();
        // The VM deposits the thrown value into `e_reg`. For `catch (id)` that IS
        // the binding; for `catch ([a,b])` / `catch ({e})` it's a scratch slot we
        // destructure into the pattern's bindings.
        let (e_reg, e_name, pattern) = match &handler.param {
            Some(p) => match &p.pattern {
                ox::BindingPattern::BindingIdentifier(id) => {
                    // Strict mode: `catch (eval)` / `catch (arguments)` is an early SyntaxError.
                    strict_name_err(self.cx.in_strict, id.name.as_str())?;
                    let r = self.declare_local_no_box(id.name.as_str());
                    // Mark the catch-PARAMETER binding: Annex B B.3.5 lets a
                    // same-named `var` / block function coexist with it (NOT an
                    // early error), so the B.3.3 promotion check must ignore it.
                    self.catch_param_regs.insert(r);
                    (r, Some(id.name.to_string()), None)
                }
                pat => (self.declare_local_no_box("<catch.val>"), None, Some(pat)),
            },
            None => (self.declare_local_no_box("<catch.ignored>"), None, None),
        };
        if let Instr::PushHandler { catch_target, catch_reg } = &mut self.code[push_at as usize] {
            *catch_target = catch_start;
            *catch_reg = e_reg;
        }
        if let Some(pat) = pattern {
            // Catch-parameter leaves are CATCH-SCOPE locals (never script
            // globals): a destructured name must not leak past the catch nor
            // hoist a same-named block function to a global (B.3.5 skip).
            self.pattern_block_local = true;
            let r = self.declare_pattern(pat).and_then(|_| self.extract_pattern(pat, e_reg));
            self.pattern_block_local = false;
            r?;
        }
        if let Some(n) = &e_name {
            if self.captured.contains(n) {
                self.emit(Instr::MakeCell { reg: e_reg });
                self.cell_regs.insert(e_reg);
            }
        }
        self.compile_stmt_list(&handler.body.body, false)?;
        self.pop_scope();
        Ok(())
    }

    fn do_while_statement(&mut self, d: &ox::DoWhileStatement) -> R<()> {
        let top = self.here();
        self.loop_ctx.push(LoopCtx::loop_frame(self.pending_label.take(), self.handler_depth));
        self.stmt(&d.body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        let cont = self.here();
        for c in ctx.continue_jumps {
            self.patch_jump(c, cont); // continue → re-evaluate the condition
        }
        let cond = self.expr(&d.test)?;
        // Loop back to top while the condition is truthy.
        self.emit(Instr::JumpIfTrue { cond, target: top });
        let end = self.here();
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
        Ok(())
    }

    /// `switch (disc) { case A: …; default: …; … }`. Two passes: emit the
    /// `disc === testᵢ` comparison jumps in order (and remember `default`), then
    /// emit the case bodies consecutively so fall-through is natural; `break`
    /// (collected in a non-loop frame) jumps to the end.
    fn switch_stmt(&mut self, s: &ox::SwitchStatement) -> R<()> {
        self.push_scope();
        // A switch CaseBlock is one block scope. Pre-declare its lexically-scoped
        // declarations as block-local so they don't leak past the switch:
        //  - class / generator / async(-generator) function declarations are ALWAYS
        //    lexical (no Annex B), so block-local in both strict and sloppy;
        //  - an ordinary function declaration is block-local only in strict (sloppy
        //    keeps the Annex B hoist to the function/global scope).
        for c in &s.cases {
            for st in &c.consequent {
                match st {
                    ox::Statement::FunctionDeclaration(f) => {
                        if let Some(id) = &f.id {
                            // Block-local in strict / generator / async (always
                            // lexical), on a lexical conflict, or for a protected
                            // param/lexical/class or a B.3.3 var name — so Annex B
                            // param/lexical skip-leak applies in case bodies too. A
                            // same-named existing function is directly updated, not
                            // shadowed.
                            let nm = id.name.as_str();
                            if self.cx.in_strict
                                || f.generator
                                || f.r#async
                                || self.block_fn_conflicts(nm)
                                || self.protect_names.contains(nm)
                                || self.b33_names.contains(nm)
                            {
                                self.declare_local(nm);
                            }
                        }
                    }
                    ox::Statement::ClassDeclaration(cd) => {
                        if let Some(id) = &cd.id {
                            self.declare_local(id.name.as_str());
                        }
                    }
                    _ => {}
                }
            }
        }
        // CaseBlockEvaluation starts the completion V at undefined (a no-match / all-
        // empty switch yields undefined, not the prior statement's value).
        self.reset_loop_completion();
        let disc = self.expr(&s.discriminant)?;

        // Pass 1: comparison jumps (strict `===`, like JS). `default` is recorded
        // and dispatched after the others fail.
        let mut case_jumps: Vec<(usize, u32)> = Vec::new();
        let mut default_index: Option<usize> = None;
        for (i, c) in s.cases.iter().enumerate() {
            match &c.test {
                Some(t) => {
                    let save = self.next_reg;
                    let tv = self.expr(t)?;
                    let cond = self.temp();
                    self.emit(Instr::Eq { dst: cond, a: disc, b: tv });
                    let j = self.here();
                    self.emit(Instr::JumpIfTrue { cond, target: 0 });
                    self.next_reg = save; // reclaim test/cond temps (disc survives)
                    case_jumps.push((i, j));
                }
                None => default_index = Some(i),
            }
        }
        // No case matched → default body (if present) else the end.
        let dispatch_default = self.here();
        self.emit(Instr::Jump { target: 0 });

        // Pass 2: case bodies, in source order (fall-through is natural).
        self.loop_ctx.push(LoopCtx::switch_frame(self.pending_label.take(), self.handler_depth));
        let mut body_start: Vec<u32> = Vec::with_capacity(s.cases.len());
        for c in &s.cases {
            body_start.push(self.here());
            for st in &c.consequent {
                self.stmt(st)?;
            }
        }
        let ctx = self.loop_ctx.pop().unwrap();
        let end = self.here();

        for (i, j) in case_jumps {
            self.patch_jump(j, body_start[i]);
        }
        let default_target = default_index.map(|i| body_start[i]).unwrap_or(end);
        self.patch_jump(dispatch_default, default_target);
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
        self.pop_scope();
        Ok(())
    }

    /// `for (const x of iter) body` — desugars to an index loop over an
    /// array/string: `let i=0; while (i < len(iter)) { x = iter[i]; body; i++ }`.
    /// (Generic iterables/iterators are not in the subset; arrays and strings
    /// cover the corpus and common code.)
    fn for_of_statement(&mut self, f: &ox::ForOfStatement) -> R<()> {
        if f.r#await && !self.in_async {
            return Err("`for await` is only valid in an async function".into());
        }
        self.push_scope();

        // A `for (let [a,b] of …)` / `for (let {x} of …)` head destructures each
        // element; a plain `for (let x of …)` binds it to one variable. A head
        // that's NOT a declaration (`for (x of …)`, `for ([a,b] of …)`,
        // `for (obj.k of …)`) ASSIGNS each element to an existing target.
        let decl_pat = match &f.left {
            ox::ForStatementLeft::VariableDeclaration(d) => Some(&d.declarations[0].id),
            _ => None,
        };
        let pattern =
            decl_pat.filter(|p| !matches!(p, ox::BindingPattern::BindingIdentifier(_)));
        let assign_tgt = f.left.as_assignment_target();

        // Evaluate the iterable into a stable scratch local; `idx` is the cursor
        // IterNext advances for array/string/Map/Set (ignored for a generator,
        // which it drives via `.next()`). One loop shape handles every iterable.
        let iter_reg = self.declare_local("<forof.iter>");
        // The iterable expression is evaluated with the loop's `let`/`const` binding(s)
        // already in scope but in their TDZ — so `for (let x of [x]) {}` throws a
        // ReferenceError (the inner `x` shadows, uninitialized), per
        // ForIn/OfHeadEvaluation. Mark the names as TDZ for the duration of `f.right`.
        let tdz_added: Vec<String> = match &f.left {
            ox::ForStatementLeft::VariableDeclaration(d) if d.kind.is_lexical() => {
                let mut names = std::collections::HashSet::new();
                capture::collect_pattern_names(&d.declarations[0].id, &mut names);
                names
                    .into_iter()
                    .filter(|n| self.param_tdz.insert(n.clone()))
                    .collect()
            }
            _ => Vec::new(),
        };
        // A RUNTIME TDZ scope over the head expression too: a CLOSURE created
        // in `f.right` captures an uninitialized cell for each head name and
        // throws ReferenceError when it later reads it (the head env binding
        // is never initialized), instead of capturing the outer binding.
        let head_tdz_scope = !tdz_added.is_empty();
        if head_tdz_scope {
            self.push_scope();
            for n in &tdz_added {
                let r = self.alloc_reg();
                self.scopes.last_mut().unwrap().push((n.clone(), r));
                self.emit(Instr::MakeCellTdz { reg: r });
                self.cell_regs.insert(r);
            }
        }
        let v = self.expr_into(&f.right, iter_reg)?;
        if head_tdz_scope {
            self.pop_scope();
        }
        for n in &tdz_added {
            self.param_tdz.remove(n);
        }
        if v != iter_reg {
            self.emit(Instr::Move { dst: iter_reg, src: v });
        }
        // Resolve the iterator. `for await` uses the ASYNC iterator (@@asyncIterator
        // → @@iterator fallback); plain `for of` uses @@iterator. Built-ins/async
        // generators pass through and are driven by IterNext / ForAwaitNext.
        let sync_reg = if f.r#await {
            // The sync flag must survive the whole loop (read each iteration
            // for the AsyncFromSyncIteratorContinuation value-await below).
            let s = self.declare_local("<forof.sync>");
            self.emit(Instr::GetAsyncIterator { dst: iter_reg, src: iter_reg, sync_dst: s });
            Some(s)
        } else {
            self.emit(Instr::GetIterator { dst: iter_reg, src: iter_reg });
            None
        };
        // GetIterator's iterator RECORD caches the `next` method ONCE in the
        // prologue (a mid-loop redefinition of `iterator.next` is not observed
        // — iterator-next-reference). Sync loops only; user-object iterators
        // only (built-ins keep the registerless cursor fast path, with no
        // observable prologue get).
        let next_reg = if f.r#await {
            None
        } else {
            let n = self.declare_local("<forof.next>");
            self.emit(Instr::IterPrime { dst: n, iter: iter_reg });
            Some(n)
        };
        let idx_reg = self.declare_local("<forof.idx>");
        self.emit(Instr::LoadInt { dst: idx_reg, val: 0 });
        // Close-on-throw (sync for-of only): if the element binding or the body
        // throws, the iterator must be closed before the error propagates. The
        // thrown value lands in `exc_reg`; a catch block after the loop closes the
        // iterator and re-throws. (for-await iterates asynchronously — left as-is.)
        let exc_reg = if f.r#await {
            None
        } else {
            Some(self.declare_local("<forof.exc>"))
        };

        // The loop binding: a destructuring pattern's leaves, an assignment to an
        // existing target, or a single (possibly cell-boxed) declared variable.
        let head_lexical = matches!(&f.left,
            ox::ForStatementLeft::VariableDeclaration(d) if d.kind.is_lexical());
        // A `var` head whose binding is NOT a plain current-function register
        // (catch-param cell / global slot / upvalue): per-iteration writes go
        // through `store_binding` — the loop creates NO binding of its own
        // (Annex B catch-redeclared-for-of-var).
        let mut var_binding: Option<Binding> = None;
        let (var_reg, var_is_cell) = match (pattern, assign_tgt) {
            (Some(p), _) => {
                // A `let`/`const` head pattern binds HEAD-SCOPE locals (a
                // script-level `var` pattern still binds globals).
                self.pattern_block_local = head_lexical;
                let r = self.declare_pattern(p);
                self.pattern_block_local = false;
                r?;
                (0, false)
            }
            (None, Some(_)) => (0, false), // assignment target: nothing to declare
            (None, None) => {
                let var_name = for_left_name(&f.left)?;
                if matches!(&f.left,
                    ox::ForStatementLeft::VariableDeclaration(d)
                        if d.kind == ox::VariableDeclarationKind::Var)
                {
                    // `for (var x of …)`: resolve the EXISTING binding instead
                    // of declaring a loop-local; a first-mention var creates
                    // its FUNCTION-scoped binding.
                    match self.head_var_binding(&var_name) {
                        Binding::Local(r) => (r, false),
                        other => {
                            var_binding = Some(other);
                            (0, false)
                        }
                    }
                } else {
                    let r = self.declare_local(&var_name);
                    // A `const`/`using`/`await using` loop variable is immutable
                    // WITHIN an iteration: a body assignment throws TypeError.
                    if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
                        if matches!(
                            d.kind,
                            ox::VariableDeclarationKind::Const
                                | ox::VariableDeclarationKind::Using
                                | ox::VariableDeclarationKind::AwaitUsing
                        ) {
                            self.const_regs.insert(r);
                        }
                    }
                    (r, self.cell_regs.contains(&r))
                }
            }
        };

        // `for (using x of it)` / `for (await using x of it)`: each iteration
        // disposes the loop variable's resource at the end of the iteration (a
        // per-iteration disposal scope). Allocate the scope/completion registers
        // once (stable across iterations); OpenUsingScope writes a fresh scope id
        // each turn. Only a simple-identifier head is a using declaration.
        let using_async: Option<bool> = match &f.left {
            ox::ForStatementLeft::VariableDeclaration(d) => match d.kind {
                ox::VariableDeclarationKind::Using => Some(false),
                ox::VariableDeclarationKind::AwaitUsing => Some(true),
                _ => None,
            },
            _ => None,
        };
        let using_regs = using_async.map(|_| {
            (
                self.declare_local("<forof.uscope>"),
                self.declare_local("<forof.ukind>"),
                self.declare_local("<forof.uval>"),
            )
        });

        let top = self.here();
        let save = self.next_reg;
        let done = self.alloc_reg();
        // Write the element straight into a plain-local loop var; use a temp for a
        // destructuring pattern, an assignment target, a cell-boxed var, or a
        // store-through `var` binding (catch-param / global / upvalue).
        let elem = if pattern.is_some()
            || assign_tgt.is_some()
            || var_is_cell
            || var_binding.is_some()
        {
            self.alloc_reg()
        } else {
            var_reg
        };
        let jdone = if f.r#await {
            // `r = await <next step>; done = r.done; value = r.value`. ForAwaitNext
            // yields a Promise (async iterator) or a {value,done} (sync) — awaiting
            // suspends on the former and passes the latter straight through.
            let step = self.alloc_reg();
            self.emit(Instr::ForAwaitNext { dst: step, iter: iter_reg, idx: idx_reg });
            // AsyncFromSyncIteratorContinuation: a SYNC source's raw
            // {value,done} becomes the spec's capability promise resolving
            // to { value: await value, done } — built synchronously inside
            // this turn (observable constructor read + the one-job unwrap
            // hop), so the single Await below covers both iterator kinds.
            if let Some(s) = sync_reg {
                let jskip = self.here();
                self.emit(Instr::JumpIfFalse { cond: s, target: 0 });
                self.emit(Instr::AsyncFromSyncStep { dst: step, step, iter: iter_reg });
                let after = self.here();
                self.patch_jump(jskip, after);
            }
            let r = self.alloc_reg();
            self.emit(Instr::Await { dst: r, val: step });
            let done_name = self.string_name("done");
            self.emit(Instr::GetProp { dst: done, obj: r, name: done_name });
            let j = self.here();
            self.emit(Instr::JumpIfTrue { cond: done, target: 0 }); // done → exit
            let value_name = self.string_name("value");
            self.emit(Instr::GetProp { dst: elem, obj: r, name: value_name });
            j
        } else {
            self.emit(Instr::IterNext {
                value_dst: elem,
                done_dst: done,
                iter: iter_reg,
                idx: idx_reg,
                next: next_reg.unwrap_or(Reg::MAX),
            });
            let j = self.here();
            self.emit(Instr::JumpIfTrue { cond: done, target: 0 }); // done → exit
            j
        };
        // Enter the loop body's break/continue scope, then install the per-iteration
        // close-on-throw handler. The scope's `handler_depth` floor is recorded
        // BEFORE the push so a `break`/`continue` (handler_depth > floor) unwinds
        // through it (popping the handler) on its way to the break/continue target.
        // The handler is OUTSIDE the IterNext/done-check above, so a throwing
        // `next()` (or normal exhaustion) does NOT close the iterator (per spec).
        self.loop_ctx.push(LoopCtx::loop_frame(self.pending_label.take(), self.handler_depth));
        // Record the iterator so a `return` out of the body closes it (the
        // close-on-throw handler and the break-close block cover the other exits).
        self.loop_ctx.last_mut().unwrap().iter_close = Some(iter_reg);
        let close_push = if let Some(er) = exc_reg {
            let at = self.here();
            self.emit(Instr::PushHandler { catch_target: 0, catch_reg: er });
            self.handler_depth += 1;
            Some(at)
        } else {
            None
        };
        if let Some(p) = pattern {
            // Per-iteration bindings: captured LEXICAL leaves get a FRESH cell
            // each iteration (LoadUndefined resets the reg so MakeCell boxes a
            // new cell rather than nesting the old one), then the extraction
            // CellSets this iteration's values into it.
            if head_lexical {
                let mut names = std::collections::HashSet::new();
                capture::collect_pattern_names(p, &mut names);
                for n in &names {
                    let found = self
                        .scopes
                        .iter()
                        .flatten()
                        .find(|(nm, _)| nm == n)
                        .map(|(_, r)| *r);
                    if let Some(r) = found {
                        if self.cell_regs.contains(&r) {
                            self.emit(Instr::LoadUndefined { dst: r });
                            self.emit(Instr::MakeCell { reg: r });
                        }
                    }
                }
            }
            self.extract_pattern(p, elem)?;
        } else if let Some(tgt) = assign_tgt {
            self.assign_target(tgt, elem)?;
        } else if let Some(b) = &var_binding {
            // `for (var x of …)` writing a non-register binding (catch-param
            // cell / global slot / upvalue): plain assignment, NO fresh cell —
            // `var` is function-scoped (one binding for the whole loop).
            self.store_binding(b, elem);
        } else if var_is_cell {
            // Per-iteration binding: a FRESH cell each iteration so a closure in
            // the body captures THIS element, not the last one (for-of let).
            self.emit(Instr::Move { dst: var_reg, src: elem });
            self.emit(Instr::MakeCell { reg: var_reg });
        }
        self.next_reg = save; // reclaim done + elem temps

        // Per-iteration `using` disposal: open a fresh scope, register the loop
        // variable's value, and wrap the body in a finally that disposes it on every
        // iteration exit (normal/throw/break/continue). Nested INSIDE the for-of's
        // close-on-throw handler, so a body throw disposes the resource first, then
        // closes the iterator.
        let using_fin = if let (Some(is_async), Some((sreg, kreg, vreg))) =
            (using_async, using_regs)
        {
            self.emit(Instr::OpenUsingScope { dst: sreg });
            let src = if var_is_cell {
                let t = self.alloc_reg();
                self.emit(Instr::CellGet { dst: t, cell: var_reg });
                t
            } else {
                var_reg
            };
            if is_async {
                self.emit(Instr::RegisterAsyncDisposable { scope: sreg, val: src });
            } else {
                self.emit(Instr::RegisterDisposable { scope: sreg, val: src });
            }
            if var_is_cell {
                self.next_reg -= 1;
            }
            let push_at = self.here();
            self.emit(Instr::PushFinally { target: 0, kind_reg: kreg, val_reg: vreg });
            self.handler_depth += 1;
            Some((is_async, sreg, kreg, vreg, push_at))
        } else {
            None
        };

        self.stmt(&f.body)?;

        // Close the per-iteration `using` finally (its DisposeScope/await-loop runs
        // on the normal path here, and on abrupt exits via the finally handler).
        if let Some((is_async, sreg, kreg, vreg, push_at)) = using_fin {
            self.emit_leave_finally_normal(kreg);
            self.handler_depth -= 1;
            let jto_fin = self.here();
            self.emit(Instr::Jump { target: 0 });
            let fin_start = self.here();
            if let Instr::PushFinally { target, .. } = &mut self.code[push_at as usize] {
                *target = fin_start;
            }
            self.patch_jump(jto_fin, fin_start);
            if is_async {
                self.emit_async_dispose_loop(sreg, kreg, vreg);
            } else {
                self.emit(Instr::DisposeScope { scope: sreg, kind_reg: kreg, val_reg: vreg });
            }
            self.emit(Instr::EndFinally { kind_reg: kreg, val_reg: vreg });
        }

        // Normal iteration completion: pop the close-on-throw handler before looping.
        if close_push.is_some() {
            self.emit(Instr::PopHandler);
            self.handler_depth -= 1;
        }
        let ctx = self.loop_ctx.pop().unwrap();
        for c in ctx.continue_jumps {
            self.patch_jump(c, top); // continue → re-run IterNext (advance + test)
        }
        self.emit(Instr::Jump { target: top });
        // Close-on-throw catch block: reached only when a throw unwinds out of the
        // element binding or body (the handler is already popped by the unwind).
        // Close the iterator quietly (error context — the original error wins) and
        // re-throw it. `return` out of the body discards the catch handler (it is
        // not a finally), so it does not close here — a remaining gap.
        if let (Some(at), Some(er)) = (close_push, exc_reg) {
            let catch_target = self.here();
            self.emit(Instr::IterCloseQuiet { iter: iter_reg });
            self.emit(Instr::Throw { src: er });
            if let Instr::PushHandler { catch_target: ct, .. } = &mut self.code[at as usize] {
                *ct = catch_target;
            }
        }
        // A `break` out of the loop closes the (not-yet-exhausted) iterator via a
        // block reached only from break jumps; the normal `done` exit skips it
        // (the iterator already signalled completion).
        let end = if ctx.break_jumps.is_empty() {
            let e = self.here();
            self.patch_jump(jdone, e);
            e
        } else {
            let brk_target = self.here();
            self.emit(Instr::IterClose { iter: iter_reg });
            let e = self.here();
            self.patch_jump(jdone, e);
            for b in ctx.break_jumps {
                self.patch_jump(b, brk_target);
            }
            e
        };
        let _ = end;
        self.pop_scope();
        Ok(())
    }

    /// `for (const k in obj) body` — iterate the object's own enumerable string
    /// keys (or an array's index strings), via the ObjectKeys op + an index loop.
    fn for_in_statement(&mut self, f: &ox::ForInStatement) -> R<()> {
        self.push_scope();
        // Mirror for-of: `for (let k in …)` declares, `for (let [a,b] in …)`
        // destructures, `for (k in …)` / `for (obj.k in …)` assigns to a target.
        let decl_pat = match &f.left {
            ox::ForStatementLeft::VariableDeclaration(d) => Some(&d.declarations[0].id),
            _ => None,
        };
        let pattern =
            decl_pat.filter(|p| !matches!(p, ox::BindingPattern::BindingIdentifier(_)));
        let assign_tgt = f.left.as_assignment_target();

        // Annex B B.3.6: `for (var a = init in obj)` evaluates the initializer
        // ONCE and assigns it to the (function-scoped / shadowing) binding
        // BEFORE the object expression runs.
        if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
            if d.kind == ox::VariableDeclarationKind::Var {
                if let (Some(init), ox::BindingPattern::BindingIdentifier(id)) =
                    (&d.declarations[0].init, &d.declarations[0].id)
                {
                    // ResolveBinding FIRST (head_var_binding may permanently
                    // allocate the function-scoped register — it must stay
                    // below the temp watermark we reclaim to), then evaluate
                    // the initializer, then PutValue.
                    let b = self.head_var_binding(id.name.as_str());
                    let save = self.next_reg;
                    let tmp = self.temp();
                    let v = self.compile_named_init(tmp, init, id.name.as_str())?;
                    let with_objs = self.with_obj_regs(id.name.as_str());
                    if with_objs.is_empty() {
                        self.store_binding(&b, v);
                    } else {
                        self.store_with(id.name.as_str(), &with_objs, v);
                    }
                    self.next_reg = save;
                }
            }
        }

        let obj_reg = self.declare_local("<forin.obj>");
        // The right-hand expression sees the loop's `let`/`const` binding(s) in their
        // TDZ (`for (let x in x) {}` throws a ReferenceError), per ForIn/OfHeadEvaluation.
        let tdz_added: Vec<String> = match &f.left {
            ox::ForStatementLeft::VariableDeclaration(d) if d.kind.is_lexical() => {
                let mut names = std::collections::HashSet::new();
                capture::collect_pattern_names(&d.declarations[0].id, &mut names);
                names
                    .into_iter()
                    .filter(|n| self.param_tdz.insert(n.clone()))
                    .collect()
            }
            _ => Vec::new(),
        };
        // A RUNTIME TDZ scope over the head expression too: a CLOSURE created
        // in `f.right` captures an uninitialized cell for each head name and
        // throws ReferenceError when it later reads it (the head env binding
        // is never initialized), instead of capturing the outer binding.
        let head_tdz_scope = !tdz_added.is_empty();
        if head_tdz_scope {
            self.push_scope();
            for n in &tdz_added {
                let r = self.alloc_reg();
                self.scopes.last_mut().unwrap().push((n.clone(), r));
                self.emit(Instr::MakeCellTdz { reg: r });
                self.cell_regs.insert(r);
            }
        }
        let v = self.expr_into(&f.right, obj_reg)?;
        if head_tdz_scope {
            self.pop_scope();
        }
        for n in &tdz_added {
            self.param_tdz.remove(n);
        }
        if v != obj_reg {
            self.emit(Instr::Move { dst: obj_reg, src: v });
        }
        let keys_reg = self.declare_local("<forin.keys>");
        self.emit(Instr::ForInKeys { dst: keys_reg, obj: obj_reg });
        let len_reg = self.declare_local("<forin.len>");
        self.emit(Instr::LenOf { dst: len_reg, obj: keys_reg });
        let idx_reg = self.declare_local("<forin.idx>");
        self.emit(Instr::LoadInt { dst: idx_reg, val: 0 });

        let head_lexical = matches!(&f.left,
            ox::ForStatementLeft::VariableDeclaration(d) if d.kind.is_lexical());
        // A `var` head whose binding is NOT a plain current-function register —
        // a shadowing catch parameter cell, a global slot, an upvalue: the
        // per-iteration write goes through `store_binding` (the loop creates NO
        // binding of its own — Annex B catch-redeclared-for-in-var).
        let mut var_binding: Option<Binding> = None;
        let (var_reg, var_is_cell) = match (pattern, assign_tgt) {
            (Some(p), _) => {
                self.pattern_block_local = head_lexical;
                let r = self.declare_pattern(p);
                self.pattern_block_local = false;
                r?;
                (0, false)
            }
            (None, Some(_)) => (0, false),
            (None, None) => {
                let var_name = for_left_name(&f.left)?;
                if matches!(&f.left,
                    ox::ForStatementLeft::VariableDeclaration(d)
                        if d.kind == ox::VariableDeclarationKind::Var)
                {
                    // `for (var x in …)`: resolve the EXISTING binding (the
                    // hoisted function-scope var, a shadowing catch param, or
                    // the global slot) instead of declaring a loop-local; a
                    // first-mention var creates its FUNCTION-scoped binding.
                    match self.head_var_binding(&var_name) {
                        Binding::Local(r) => (r, false),
                        other => {
                            var_binding = Some(other);
                            (0, false)
                        }
                    }
                } else {
                    let r = self.declare_local(&var_name);
                    // A `const` loop variable is immutable WITHIN an iteration:
                    // a body assignment throws TypeError (mirrors for-of).
                    if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
                        if matches!(
                            d.kind,
                            ox::VariableDeclarationKind::Const
                                | ox::VariableDeclarationKind::Using
                                | ox::VariableDeclarationKind::AwaitUsing
                        ) {
                            self.const_regs.insert(r);
                        }
                    }
                    (r, self.cell_regs.contains(&r))
                }
            }
        };

        let top = self.here();
        let cond = self.temp();
        self.emit(Instr::Lt { dst: cond, a: idx_reg, b: len_reg });
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 });
        self.next_reg -= 1;

        let save = self.next_reg;
        // Read the current key into a temp (pattern / assignment target /
        // store-through binding) or straight into the loop var (a
        // per-iteration cell var is boxed after).
        let key_dst = if pattern.is_some() || assign_tgt.is_some() || var_binding.is_some() {
            self.alloc_reg()
        } else {
            var_reg
        };
        self.emit(Instr::GetIndex { dst: key_dst, obj: keys_reg, key: idx_reg });
        // EnumerateObjectProperties: a not-yet-visited key DELETED during the
        // loop is skipped — re-check liveness against the receiver each
        // iteration (the snapshot in keys_reg is taken once) and jump to the
        // increment when the key is gone.
        let live = self.temp();
        self.emit(Instr::ForInLive { dst: live, obj: obj_reg, key: key_dst });
        let live_jf = self.here();
        self.emit(Instr::JumpIfFalse { cond: live, target: 0 });
        self.next_reg -= 1;
        if let Some(p) = pattern {
            if head_lexical {
                let mut names = std::collections::HashSet::new();
                capture::collect_pattern_names(p, &mut names);
                for n in &names {
                    let found = self
                        .scopes
                        .iter()
                        .flatten()
                        .find(|(nm, _)| nm == n)
                        .map(|(_, r)| *r);
                    if let Some(r) = found {
                        if self.cell_regs.contains(&r) {
                            self.emit(Instr::LoadUndefined { dst: r });
                            self.emit(Instr::MakeCell { reg: r });
                        }
                    }
                }
            }
            self.extract_pattern(p, key_dst)?;
        } else if let Some(tgt) = assign_tgt {
            self.assign_target(tgt, key_dst)?;
        } else if let Some(b) = &var_binding {
            // `for (var x in …)` writing a non-register binding (catch-param
            // cell / global slot / upvalue): plain assignment, NO fresh cell —
            // `var` is function-scoped (one binding for the whole loop).
            self.store_binding(b, key_dst);
        } else if var_is_cell {
            // Per-iteration binding: a FRESH cell each iteration (for-in let).
            self.emit(Instr::MakeCell { reg: var_reg });
        }
        self.next_reg = save;

        self.loop_ctx.push(LoopCtx::loop_frame(self.pending_label.take(), self.handler_depth));
        self.stmt(&f.body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        let cont = self.here();
        self.patch_jump(live_jf, cont); // dead (deleted) key → skip to increment
        for c in ctx.continue_jumps {
            self.patch_jump(c, cont); // continue → increment + re-test
        }
        self.emit(Instr::AddInt { dst: idx_reg, a: idx_reg, imm: 1, upd: false });
        self.emit(Instr::Jump { target: top });
        let end = self.here();
        self.patch_jump(jf, end);
        for b in ctx.break_jumps {
            self.patch_jump(b, end);
        }
        self.pop_scope();
        Ok(())
    }

    fn patch_jump(&mut self, at: u32, target: u32) {
        match &mut self.code[at as usize] {
            Instr::Jump { target: t }
            | Instr::JumpIfFalse { target: t, .. }
            | Instr::JumpIfTrue { target: t, .. }
            | Instr::JumpFinally { target: t, .. } => *t = target,
            _ => panic!("patch_jump on non-jump"),
        }
    }

    /// Emit a `break`/`continue` jump targeting `loop_ctx[idx]`. When the target's
    /// handler depth is below the current one, the jump exits one or more `try`
    /// blocks: emit `JumpFinally` (routes through each intervening `finally`,
    /// popping any intervening `catch`); otherwise a plain `Jump` (so loops without
    /// an enclosing `try` stay JIT-eligible). Records the jump ip so the loop
    /// epilogue patches its target.
    fn emit_loop_jump(&mut self, idx: usize, is_break: bool) {
        let floor = self.loop_ctx[idx].handler_depth;
        let j = self.here();
        if self.handler_depth > floor {
            self.emit(Instr::JumpFinally { target: 0, floor: floor as u16 });
        } else {
            self.emit(Instr::Jump { target: 0 });
        }
        if is_break {
            self.loop_ctx[idx].break_jumps.push(j);
        } else {
            self.loop_ctx[idx].continue_jumps.push(j);
        }
    }

    // ── expressions ──
    /// Compile `e`, returning the register holding its value.
    fn expr(&mut self, e: &ox::Expression) -> R<Reg> {
        let dst = self.temp();
        self.expr_into(e, dst)
    }

    /// Compile `e` placing its value into `dst` (or another register it already
    /// occupies, which the caller may use directly). Returns the register that
    /// actually holds the result.
    fn expr_into(&mut self, e: &ox::Expression, dst: Reg) -> R<Reg> {
        use ox::Expression as E;
        match e {
            E::NumericLiteral(n) => {
                self.load_number(dst, n.value);
                Ok(dst)
            }
            E::BigIntLiteral(b) => {
                // oxc gives `value` as a base-10 string (the source base is already
                // normalized). In-range literals load as inline i128 (the fast
                // tier); beyond-i128 literals parse ONCE into the program's
                // arbitrary-precision constant pool (the digits are decimal by
                // construction, so only an out-of-range value reaches the pool —
                // the canonical-form invariant holds).
                match b.value.as_str().parse::<i128>() {
                    Ok(v) => self.emit(Instr::LoadBigInt { dst, value: v }),
                    Err(_) => {
                        let big = b
                            .value
                            .as_str()
                            .parse::<num_bigint::BigInt>()
                            .map_err(|_| "invalid BigInt literal".to_string())?;
                        let idx = self.bigint_consts.len() as u32;
                        self.bigint_consts.push(big);
                        self.emit(Instr::LoadBigIntBig { dst, idx });
                    }
                }
                Ok(dst)
            }
            E::RegExpLiteral(r) => {
                // `/pat/flags` → NewRegExp (compiles via the `regress` engine at
                // runtime). pattern.text is the source; flags as the JS flag string.
                //
                // EXACT-source recovery: when this program is an eval of a string
                // holding lone surrogates (`exact_src` is Some), the parser saw the
                // LOSSY view — a lone surrogate in the pattern reads U+FFFD. Both
                // encodings are 3 bytes, so the pattern's byte range in the lossy
                // source indexes the exact WTF-8 bytes identically: slice it and,
                // if it differs from the lossy text, store the pattern constant in
                // the oxc MARKER form (decoded to real WTF-8 at intern time), so
                // the runtime compiles + stores the exact surrogate.
                let text = r.regex.pattern.text.as_str();
                let pat = 'pat: {
                    if text.contains('\u{FFFD}') {
                        if let Some(exact) = &self.cx.exact_src {
                            // Pattern bytes start right after the opening `/`
                            // (r.span covers the whole `/pat/flags` literal).
                            let start = r.span.start as usize + 1;
                            let end = start + text.len();
                            // Guard: the range must reproduce the parser's pattern
                            // text on the lossy source (validates the span math).
                            if self.cx.source.as_bytes().get(start..end) == Some(text.as_bytes()) {
                                if let Some(b) = exact.get(start..end) {
                                    if b != text.as_bytes() {
                                        let marked =
                                            crate::heap::encode_lone_surrogate_markers(b);
                                        break 'pat self.add_string_const_wtf8(&marked);
                                    }
                                }
                            }
                        }
                    }
                    self.add_string_const(text)
                };
                let flags_s = r.regex.flags.to_inline_string();
                let flg = self.add_string_const(&flags_s);
                let pt = self.temp();
                self.emit(Instr::LoadConst { dst: pt, idx: pat });
                let ft = self.temp();
                self.emit(Instr::LoadConst { dst: ft, idx: flg });
                self.emit(Instr::NewRegExp { dst, pattern: pt, flags: ft, is_construct: true });
                self.next_reg -= 2;
                Ok(dst)
            }
            E::StringLiteral(s) => {
                // `.lone_surrogates` literals carry the oxc marker encoding —
                // route through the WTF-8-decoding constant slot.
                let idx = if s.lone_surrogates {
                    self.add_string_const_wtf8(s.value.as_str())
                } else {
                    self.add_string_const(s.value.as_str())
                };
                self.emit(Instr::LoadConst { dst, idx });
                Ok(dst)
            }
            E::TemplateLiteral(t) => {
                // Desugar `q0${e0}q1${e1}...qN` to string concatenation
                // q0 + ToString(e0) + q1 + ToString(e1) + ... + qN. Each `${e}` is
                // ToString'd (string hint) FIRST — NOT left to `+`, whose default
                // hint tries `valueOf` before `toString` (wrong for e.g. a Temporal
                // value, whose `valueOf` throws). After ToStr both operands are
                // strings, so each `+` is a pure (rope) concat.
                let q0e = &t.quasis[0];
                let q0 = q0e.value.cooked.as_ref().map(|s| s.as_str()).unwrap_or("");
                let idx = if q0e.lone_surrogates {
                    self.add_string_const_wtf8(q0)
                } else {
                    self.add_string_const(q0)
                };
                self.emit(Instr::LoadConst { dst, idx });
                for (i, e) in t.expressions.iter().enumerate() {
                    let r = self.expr(e)?;
                    let rs = self.temp();
                    self.emit(Instr::ToStr { dst: rs, a: r });
                    self.emit(Instr::Add { dst, a: dst, b: rs });
                    if let Some(qe) = t.quasis.get(i + 1) {
                        let q = qe.value.cooked.as_ref().map(|s| s.as_str()).unwrap_or("");
                        if !q.is_empty() {
                            let qidx = if qe.lone_surrogates {
                                self.add_string_const_wtf8(q)
                            } else {
                                self.add_string_const(q)
                            };
                            let qr = self.temp();
                            self.emit(Instr::LoadConst { dst: qr, idx: qidx });
                            self.emit(Instr::Add { dst, a: dst, b: qr });
                        }
                    }
                }
                Ok(dst)
            }
            E::TaggedTemplateExpression(tt) => self.tagged_template(tt, dst),
            E::BooleanLiteral(b) => {
                self.emit(Instr::LoadBool { dst, val: b.value });
                Ok(dst)
            }
            E::NullLiteral(_) => {
                self.emit(Instr::LoadNull { dst });
                Ok(dst)
            }
            E::Identifier(id) => {
                // A strict-mode reserved word may not appear as an identifier
                // reference (property keys / member names are different AST nodes,
                // so `obj.public` / `{public:1}` are unaffected).
                if self.cx.in_strict && is_strict_reserved_word(id.name.as_str()) {
                    return Err(format!(
                        "SyntaxError: '{}' is a reserved word in strict mode",
                        id.name
                    ));
                }
                // Special global value identifiers that are not user bindings.
                // Inside a `with`, an own property of a with-object SHADOWS the
                // literal (e.g. `with({NaN:'x'}) NaN` === 'x'); the literal is the
                // fallback when no with-object carries the name.
                if matches!(id.name.as_str(), "undefined" | "NaN" | "Infinity") {
                    let lit = |s: &mut Self| match id.name.as_str() {
                        "undefined" => s.emit(Instr::LoadUndefined { dst }),
                        "NaN" => {
                            let idx = s.add_const(Value::num(f64::NAN));
                            s.emit(Instr::LoadConst { dst, idx });
                        }
                        _ => {
                            let idx = s.add_const(Value::num(f64::INFINITY));
                            s.emit(Instr::LoadConst { dst, idx });
                        }
                    };
                    let with_objs = self.with_obj_regs(id.name.as_str());
                    if with_objs.is_empty() {
                        lit(self);
                        return Ok(dst);
                    }
                    let nidx = self.string_name(id.name.as_str());
                    let end_jumps = self.emit_with_get_chain(nidx, &with_objs, dst);
                    lit(self);
                    let end = self.here();
                    for je in end_jumps {
                        self.patch_jump(je, end);
                    }
                    return Ok(dst);
                }
                // A parameter referenced before its own left-to-right
                // initialization — `(x = x)` (self) or `(x = y, y)` (forward) — is
                // in the Temporal Dead Zone: reading it throws a ReferenceError.
                if id.name == "arguments" && self.cx.in_field_init {
                    return Err(
                        "SyntaxError: 'arguments' is not allowed in a class field initializer"
                            .into(),
                    );
                }
                if self.param_tdz.contains(id.name.as_str()) {
                    let e = self.alloc_reg();
                    self.emit(Instr::NewError { dst: e, kind: 4, arg: None, opts: None, errors: None });
                    self.emit(Instr::Throw { src: e });
                    return Ok(dst);
                }
                // Inside a `with`, a free identifier may resolve to a property of an
                // active with-object (innermost first), else the static binding.
                let with_objs = self.with_obj_regs(id.name.as_str());
                if !with_objs.is_empty() {
                    return Ok(self.load_with(id.name.as_str(), &with_objs, dst));
                }
                match self.resolve(id.name.as_str()) {
                    Binding::Local(r) => Ok(r), // already in a register
                    Binding::LocalCell(cell) => {
                        self.emit(Instr::CellGet { dst, cell });
                        Ok(dst)
                    }
                    Binding::Upvalue(idx) => {
                        // A sloppy contains-direct-eval function: an eval-
                        // introduced function-scoped `var` shadows the
                        // captured name for READS.
                        if !self.cx.in_strict && self.box_all_locals {
                            let name = self.upvalues.borrow()[idx as usize].0.clone();
                            let slot = self.cx.global_slot(&name) as u32;
                            self.emit(Instr::LoadUpvalDyn { dst, idx, name: slot });
                        } else {
                            self.emit(Instr::UpvalGet { dst, idx });
                        }
                        Ok(dst)
                    }
                    Binding::Global(idx) => {
                        if self.box_all_locals || self.cx.dyn_global_zone {
                            // A dynamic EvalScope binding may shadow the slot.
                            self.emit(Instr::LoadGlobalDyn { dst, idx });
                        } else {
                            self.emit(Instr::LoadGlobal { dst, idx });
                        }
                        Ok(dst)
                    }
                    Binding::ClassName(class_id) => {
                        self.emit(Instr::LoadClassValue { dst, class_id });
                        Ok(dst)
                    }
                }
            }
            E::ThisExpression(_) => {
                // `this` lives in register 0 of the current function, unless a
                // static field initializer has redirected it to the class value.
                // In a derived ctor it is in TDZ until super() completes.
                self.this_check();
                Ok(self.this_override.unwrap_or(0))
            }
            E::ParenthesizedExpression(p) => self.expr_into(&p.expression, dst),
            E::BinaryExpression(b) => self.binary(b, dst),
            E::LogicalExpression(l) => self.logical(l, dst),
            E::UnaryExpression(u) => self.unary(u, dst),
            E::UpdateExpression(u) => self.update(u, dst),
            E::AssignmentExpression(a) => self.assign(a, dst),
            E::ConditionalExpression(c) => self.conditional(c, dst),
            E::YieldExpression(y) => self.yield_expr(y, dst),
            E::AwaitExpression(a) => self.await_expr(a, dst),
            E::CallExpression(c) => self.call(c, dst),
            E::NewExpression(n) => {
                // `new Error(msg)` / `new TypeError(msg)` / `new RangeError(msg)`
                // → a plain object {name, message}. Other constructors aren't in
                // the subset yet.
                if let ox::Expression::Identifier(id) = &n.callee {
                    if let Some(kind) = error_ctor(&id.name) {
                        return self.build_error(kind, &n.arguments, dst);
                    }
                    // `new Array(…)` / `new Object()` builtins (no real global).
                    if id.name == "Array" {
                        let (arg_base, argc) = self.eval_args_contiguous(&n.arguments)?;
                        self.emit(Instr::ArrayCtor { dst, arg_base, argc });
                        return Ok(dst);
                    }
                    if id.name == "Object" {
                        // `new Object()` → a fresh object; `new Object(x)` → ToObject(x).
                        if let Some(arg) = n.arguments.first().and_then(|a| a.as_expression()) {
                            let src = self.expr(arg)?;
                            self.emit(Instr::ToObject { dst, src });
                        } else {
                            self.emit(Instr::NewObject { dst });
                        }
                        return Ok(dst);
                    }
                    // `new Promise(executor)`.
                    if id.name == "Promise" {
                        let executor = match n.arguments.first().and_then(|a| a.as_expression()) {
                            Some(e) => {
                                let t = self.temp();
                                let v = self.expr_into(e, t)?;
                                if v != t {
                                    self.emit(Instr::Move { dst: t, src: v });
                                }
                                t
                            }
                            None => return Err("new Promise requires an executor function".into()),
                        };
                        self.emit(Instr::NewPromise { dst, executor });
                        self.next_reg -= 1; // reclaim executor temp
                        return Ok(dst);
                    }
                    // `new RegExp(pattern?, flags?)` — pattern may be a string or a RegExp.
                    if id.name == "RegExp" {
                        return self.emit_regexp(&n.arguments, dst, true);
                    }
                    // `new Map(iter?)` / `new Set(iter?)` / `new WeakMap(iter?)` /
                    // `new WeakSet(iter?)`.
                    if matches!(id.name.as_str(), "Map" | "Set" | "WeakMap" | "WeakSet") {
                        let src = match n.arguments.first().and_then(|a| a.as_expression()) {
                            Some(e) => {
                                let t = self.temp();
                                let v = self.expr_into(e, t)?;
                                if v != t {
                                    self.emit(Instr::Move { dst: t, src: v });
                                }
                                Some(t)
                            }
                            None => None,
                        };
                        self.emit(match id.name.as_str() {
                            "Set" => Instr::NewSet { dst, src },
                            "Map" => Instr::NewMap { dst, src },
                            "WeakSet" => Instr::NewWeakSet { dst, src },
                            _ => Instr::NewWeakMap { dst, src },
                        });
                        if src.is_some() {
                            self.next_reg -= 1; // reclaim the src temp
                        }
                        return Ok(dst);
                    }
                    // `new String/Number/Boolean(arg?)` — a boxed primitive wrapper.
                    if matches!(id.name.as_str(), "String" | "Number" | "Boolean") {
                        let kind = match id.name.as_str() {
                            "String" => 0u8,
                            "Number" => 1,
                            _ => 2,
                        };
                        let arg = match n.arguments.first().and_then(|a| a.as_expression()) {
                            Some(e) => {
                                let t = self.temp();
                                let v = self.expr_into(e, t)?;
                                if v != t {
                                    self.emit(Instr::Move { dst: t, src: v });
                                }
                                Some(t)
                            }
                            None => None,
                        };
                        self.emit(Instr::NewBox { dst, kind, arg });
                        if arg.is_some() {
                            self.next_reg -= 1;
                        }
                        return Ok(dst);
                    }
                    // `new WeakRef(target)` — target required (the op validates it's
                    // an object).
                    if id.name == "WeakRef" {
                        let target = self.temp();
                        match n.arguments.first().and_then(|a| a.as_expression()) {
                            Some(e) => {
                                let v = self.expr_into(e, target)?;
                                if v != target {
                                    self.emit(Instr::Move { dst: target, src: v });
                                }
                            }
                            None => self.emit(Instr::LoadUndefined { dst: target }),
                        }
                        self.emit(Instr::NewWeakRef { dst, target });
                        self.next_reg -= 1; // reclaim target temp
                        return Ok(dst);
                    }
                    // `new FinalizationRegistry(cleanupCallback)`.
                    if id.name == "FinalizationRegistry" {
                        let cleanup = self.temp();
                        match n.arguments.first().and_then(|a| a.as_expression()) {
                            Some(e) => {
                                let v = self.expr_into(e, cleanup)?;
                                if v != cleanup {
                                    self.emit(Instr::Move { dst: cleanup, src: v });
                                }
                            }
                            None => self.emit(Instr::LoadUndefined { dst: cleanup }),
                        }
                        self.emit(Instr::NewFinalizationRegistry { dst, cleanup });
                        self.next_reg -= 1; // reclaim cleanup temp
                        return Ok(dst);
                    }
                    // `new Date(...)`.
                    if id.name == "Date" {
                        let (arg_base, argc) = self.eval_args_contiguous(&n.arguments)?;
                        self.emit(Instr::DateNew { dst, arg_base, argc });
                        return Ok(dst);
                    }
                }
                // General `new C(args)`: evaluate the constructor value, then the
                // args (contiguous), and let the VM build the instance. When the
                // arguments contain a spread (`new C(...xs)`), build a flat args
                // array and construct via NewSpread instead. The constructor is
                // SNAPSHOTTED into a temp before the args run: an argument's
                // side effect reassigning the callee variable must not change
                // which value is constructed (EvaluateNew takes GetValue first).
                let cv = self.expr(&n.callee)?;
                let save = self.next_reg;
                let callee = self.temp();
                if cv != callee {
                    self.emit(Instr::Move { dst: callee, src: cv });
                }
                if n.arguments.iter().any(|a| a.as_expression().is_none()) {
                    let args_arr = self.build_spread_args(&n.arguments)?;
                    self.emit(Instr::NewSpread { dst, callee, args: args_arr });
                    self.next_reg = save; // reclaim the callee temp (+ arg scratch)
                    return Ok(dst);
                }
                let (arg_base, argc) = self.eval_args_contiguous(&n.arguments)?;
                self.emit(Instr::New { dst, callee, arg_base, argc });
                self.next_reg = save; // reclaim the callee temp + args
                Ok(dst)
            }
            E::FunctionExpression(f) => {
                let (id, has_up) =
                    self.compile_func_expr(f.id.as_ref().map(|i| i.name.to_string()), f)?;
                self.emit_make_callable(dst, id, has_up);
                Ok(dst)
            }
            E::ArrowFunctionExpression(a) => {
                let (id, _has_up) = self.compile_arrow(a, "")?;
                self.emit_make_arrow(dst, id);
                Ok(dst)
            }
            E::ClassExpression(c) => self.class_expr(c, dst, None),
            E::ArrayExpression(a) => self.array_literal(a, dst),
            E::ObjectExpression(o) => self.object_literal(o, dst),
            E::StaticMemberExpression(m) => self.static_member(m, dst),
            E::ComputedMemberExpression(m) => self.computed_member(m, dst),
            E::PrivateFieldExpression(p) => {
                // `obj.#field` → read the reserved "#field" property.
                self.check_private_declared(&p.field.name)?;
                let obj = self.expr(&p.object)?;
                if p.optional {
                    self.emit_optional_check(obj);
                }
                let name = self.string_name(&private_key(&p.field.name));
                self.emit(Instr::GetProp { dst, obj, name });
                Ok(dst)
            }
            // `#field in obj` — private brand check (private fields are stored as
            // the reserved "#field" property, so this is a HasProp on that key).
            E::PrivateInExpression(p) => {
                self.check_private_declared(&p.left.name)?;
                let kr = self.temp();
                let idx = self.add_string_const(&private_key(&p.left.name));
                self.emit(Instr::LoadConst { dst: kr, idx });
                let obj = self.expr(&p.right)?;
                // Ergonomic brand check: bypass the private-key reflection filter.
                self.emit(Instr::HasProp { dst, key: kr, obj, brand: true });
                Ok(dst)
            }
            E::ChainExpression(ce) => self.chain_expr(ce, dst),
            E::SequenceExpression(s) => {
                // `(a, b, c)` — evaluate each for side effects; value is the last.
                let n = s.expressions.len();
                for (i, e) in s.expressions.iter().enumerate() {
                    if i + 1 == n {
                        return self.expr_into(e, dst);
                    }
                    let _ = self.expr(e)?;
                }
                self.emit(Instr::LoadUndefined { dst }); // empty sequence (unreachable)
                Ok(dst)
            }
            E::MetaProperty(mp) if mp.meta.name == "new" && mp.property.name == "target" => {
                // `new.target` is an early SyntaxError outside a function/class body
                // (e.g. at the top level of a script or an indirect eval), including
                // inside an arrow that has no enclosing ordinary function.
                if !self.cx.new_target_ok {
                    return Err("SyntaxError: new.target expression is not allowed here".into());
                }
                // The current activation's new.target (undefined unless entered via
                // new/Reflect.construct/super). Arrows inherit it lexically from their
                // enclosing function via the frame they run in.
                self.emit(Instr::LoadNewTarget { dst });
                Ok(dst)
            }
            E::ImportExpression(ie) => {
                // Dynamic `import(specifier [, options])` / `import.defer` /
                // `import.source`. Evaluate the specifier (and options, if any);
                // ImportCall does ToString, the options/phase checks, and the load.
                let spec = self.expr(&ie.source)?;
                let opts = match &ie.options {
                    Some(o) => Some(self.expr(o)?),
                    None => None,
                };
                let phase = match ie.phase {
                    Some(ox::ImportPhase::Source) => 2,
                    Some(ox::ImportPhase::Defer) => 1,
                    None => 0,
                };
                self.emit(Instr::ImportCall { dst, spec, phase, opts });
                Ok(dst)
            }
            ox::Expression::MetaProperty(m) => {
                // `import.meta` — module code only (a SyntaxError in scripts);
                // `new.target` is handled by the dedicated lowering elsewhere.
                if m.meta.name == "import" && m.property.name == "meta" {
                    // (Both module pipelines: compile_module entry and the
                    // loader's compile_eval(is_module) — see the import
                    // pre-pass gate.)
                    let in_module =
                        self.cx.module_mode || (self.cx.eval_mode && !self.cx.eval_locals);
                    if !in_module {
                        return Err("SyntaxError: import.meta is only valid in modules".into());
                    }
                    let dst = self.temp();
                    self.emit(Instr::ImportMeta { dst });
                    return Ok(dst);
                }
                Err("unsupported meta property".into())
            }
            _ => Err("unsupported expression (not in the zipp-vm v1 subset yet)".into()),
        }
    }

    fn static_member(&mut self, m: &ox::StaticMemberExpression, dst: Reg) -> R<Reg> {
        // Math constants (Math.PI, Math.E, …) — Math has no real global object.
        if let ox::Expression::Identifier(o) = &m.object {
            if o.name == "Math" {
                let c = match m.property.name.as_str() {
                    "PI" => Some(std::f64::consts::PI),
                    "E" => Some(std::f64::consts::E),
                    "LN2" => Some(std::f64::consts::LN_2),
                    "LN10" => Some(std::f64::consts::LN_10),
                    "LOG2E" => Some(std::f64::consts::LOG2_E),
                    "LOG10E" => Some(std::f64::consts::LOG10_E),
                    "SQRT2" => Some(std::f64::consts::SQRT_2),
                    "SQRT1_2" => Some(std::f64::consts::FRAC_1_SQRT_2),
                    _ => None,
                };
                if let Some(v) = c {
                    self.load_number(dst, v);
                    return Ok(dst);
                }
            }
            // `Symbol.iterator` etc. are now real Symbol VALUES — they resolve as
            // ordinary property reads of the `Symbol` global (whose key_of maps to
            // the engine's `@@iterator` convention, so iteration is unchanged).
        }
        // `super.name` — read an inherited property through the lexical home: a class
        // method via its home class, an object method via its runtime [[HomeObject]].
        if matches!(&m.object, ox::Expression::Super(_)) {
            // MakeSuperPropertyReference: GetThisBinding() throws FIRST in a
            // derived ctor pre-super.
            self.this_check();
            let name = self.string_name(m.property.name.as_str());
            if let Some(pid) = self.super_class {
                self.emit(Instr::SuperGet { dst, home_class_id: pid, name });
            } else if self.super_home_obj {
                self.emit(Instr::SuperGetObj { dst, name });
            } else {
                return Err("`super.x` is only valid in a method".into());
            }
            return Ok(dst);
        }
        let obj = self.expr(&m.object)?;
        if m.optional {
            self.emit_optional_check(obj);
        }
        let name = self.string_name(m.property.name.as_str());
        self.emit(Instr::GetProp { dst, obj, name });
        Ok(dst)
    }

    fn computed_member(&mut self, m: &ox::ComputedMemberExpression, dst: Reg) -> R<Reg> {
        // `super[expr]` — computed inherited-property read.
        if matches!(&m.object, ox::Expression::Super(_)) {
            // GetThisBinding() throws BEFORE the key expression is evaluated.
            self.this_check();
            if let Some(pid) = self.super_class {
                let key = self.expr(&m.expression)?;
                self.emit(Instr::SuperGetComputed { dst, home_class_id: pid, key });
            } else if self.super_home_obj {
                let key = self.expr(&m.expression)?;
                self.emit(Instr::SuperGetObjComputed { dst, key });
            } else {
                return Err("`super[x]` is only valid in a method".into());
            }
            return Ok(dst);
        }
        let obj = self.expr(&m.object)?;
        if m.optional {
            self.emit_optional_check(obj);
        }
        let key = self.expr(&m.expression)?;
        self.emit(Instr::GetIndex { dst, obj, key });
        Ok(dst)
    }

    /// `?.` short-circuit: if `obj` is null/undefined (loose `== null`), jump to
    /// the enclosing chain's "undefined" block, recorded for patching at chain
    /// exit. No-op outside a chain (an `optional` flag can only appear in one).
    /// Emit `cond = (v === undefined) || (v === null)` — the SPEC nullish
    /// test. (LooseEq-against-undefined also matches an [[IsHTMLDDA]]
    /// object, which `??` / `??=` / `?.` must NOT treat as nullish.)
    fn emit_is_nullish(&mut self, v: Reg, cond: Reg, scratch: Reg) {
        self.emit(Instr::LoadUndefined { dst: scratch });
        self.emit(Instr::Eq { dst: cond, a: v, b: scratch });
        let j = self.here();
        self.emit(Instr::JumpIfTrue { cond, target: 0 });
        self.emit(Instr::LoadNull { dst: scratch });
        self.emit(Instr::Eq { dst: cond, a: v, b: scratch });
        let end = self.here();
        self.patch_jump(j, end);
    }

    fn emit_optional_check(&mut self, obj: Reg) {
        if self.chain_bails.is_empty() {
            return;
        }
        let save = self.next_reg;
        let nreg = self.alloc_reg();
        let cond = self.alloc_reg();
        self.emit_is_nullish(obj, cond, nreg);
        let jt = self.here();
        self.emit(Instr::JumpIfTrue { cond, target: 0 });
        self.chain_bails.last_mut().unwrap().push(jt);
        self.next_reg = save; // scratch temps dead after the check
    }

    /// Lower a PARENTHESIZED-chain member callee — `(a?.b)(…)` / `(a?.[k])(…)`
    /// — to `(callee, base)` registers so the call still binds `this` = base.
    /// The chain ends at the parens: a nullish base lands the CALLEE at
    /// undefined (its own bail boundary), and the call then throws (or an
    /// outer `?.()` bails). Returns None for non-member chain elements
    /// (call chains etc. — those stay value-calls).
    fn chain_member_callee(&mut self, ce: &ox::ChainExpression) -> R<Option<(Reg, Reg)>> {
        enum Kind<'a, 'x> {
            Static(&'a ox::StaticMemberExpression<'x>),
            Computed(&'a ox::ComputedMemberExpression<'x>),
        }
        let kind = match &ce.expression {
            ox::ChainElement::StaticMemberExpression(m)
                if !matches!(&m.object, ox::Expression::Super(_)) =>
            {
                Kind::Static(m)
            }
            ox::ChainElement::ComputedMemberExpression(m)
                if !matches!(&m.object, ox::Expression::Super(_)) =>
            {
                Kind::Computed(m)
            }
            _ => return Ok(None),
        };
        self.chain_bails.push(Vec::new());
        let res: R<(Reg, Reg)> = (|| {
            let (object, optional) = match &kind {
                Kind::Static(m) => (&m.object, m.optional),
                Kind::Computed(m) => (&m.object, m.optional),
            };
            let o = self.expr(object)?;
            let obj = self.alloc_reg();
            if o != obj {
                self.emit(Instr::Move { dst: obj, src: o });
            }
            if optional {
                self.emit_optional_check(obj);
            }
            let callee = self.alloc_reg();
            match &kind {
                Kind::Static(m) => {
                    let name = self.string_name(m.property.name.as_str());
                    self.emit(Instr::GetProp { dst: callee, obj, name });
                }
                Kind::Computed(m) => {
                    let key = self.expr(&m.expression)?;
                    self.emit(Instr::GetIndex { dst: callee, obj, key });
                }
            }
            Ok((callee, obj))
        })();
        let bails = self.chain_bails.pop().unwrap();
        let (callee, obj) = res?;
        if !bails.is_empty() {
            let jmp = self.here();
            self.emit(Instr::Jump { target: 0 });
            let undef_at = self.here();
            self.emit(Instr::LoadUndefined { dst: callee });
            let end = self.here();
            self.patch_jump(jmp, end);
            for b in bails {
                self.patch_jump(b, undef_at);
            }
        }
        Ok(Some((callee, obj)))
    }

    /// Compile an optional chain `a?.b…`: open a short-circuit boundary, compile
    /// the chain element (its `?.` links record bail jumps), then route any
    /// short-circuit to a single `undefined` result.
    fn chain_expr(&mut self, ce: &ox::ChainExpression, dst: Reg) -> R<Reg> {
        self.chain_bails.push(Vec::new());
        let res = match &ce.expression {
            ox::ChainElement::StaticMemberExpression(m) => self.static_member(m, dst),
            ox::ChainElement::ComputedMemberExpression(m) => self.computed_member(m, dst),
            ox::ChainElement::CallExpression(c) => self.call(c, dst),
            // `o?.#field` (and nested `?.` links inside the object register
            // their own bails): the GetProp private path handles brand checks.
            ox::ChainElement::PrivateFieldExpression(p) => {
                self.check_private_declared(&p.field.name)?;
                let obj = self.expr(&p.object)?;
                if p.optional {
                    self.emit_optional_check(obj);
                }
                let name = self.string_name(&private_key(&p.field.name));
                self.emit(Instr::GetProp { dst, obj, name });
                Ok(dst)
            }
            _ => Err("this optional-chain form is not in the zipp-vm subset yet".into()),
        };
        let bails = self.chain_bails.pop().unwrap();
        let v = res?;
        if bails.is_empty() {
            return Ok(v);
        }
        if v != dst {
            self.emit(Instr::Move { dst, src: v });
        }
        let jmp = self.here();
        self.emit(Instr::Jump { target: 0 });
        let undef_at = self.here();
        self.emit(Instr::LoadUndefined { dst });
        let end = self.here();
        self.patch_jump(jmp, end);
        for b in bails {
            self.patch_jump(b, undef_at);
        }
        Ok(dst)
    }

    /// `` tag`q0${e0}q1…` `` — call `tag(strings, e0, e1, …)` where `strings` is
    /// the array of cooked literal parts carrying a `.raw` array of the un-escaped
    /// parts. `String.raw` is handled inline (no real global exists).
    fn tagged_template(&mut self, tt: &ox::TaggedTemplateExpression, dst: Reg) -> R<Reg> {
        self.tagged_template_impl(tt, dst, false)
    }

    /// `return tag`…`` in a proper-tail-call position: same lowering with the
    /// `TailCall` frame-reuse prefix in front of the final plain `Call`.
    fn tagged_template_tail(&mut self, tt: &ox::TaggedTemplateExpression, dst: Reg) -> R<Reg> {
        self.tagged_template_impl(tt, dst, true)
    }

    fn tagged_template_impl(
        &mut self,
        tt: &ox::TaggedTemplateExpression,
        dst: Reg,
        tail: bool,
    ) -> R<Reg> {
        let quasi = &tt.quasi;
        // `String.raw` template — concatenate the RAW parts with the values.
        if let ox::Expression::StaticMemberExpression(m) = &tt.tag {
            if let ox::Expression::Identifier(o) = &m.object {
                if o.name == "String" && m.property.name == "raw" {
                    return self.string_raw(quasi, dst);
                }
            }
        }
        let n = quasi.expressions.len();
        // Evaluate the tag (and its `this` for a member tag) first, into stable
        // registers that survive the argument block.
        enum Tag {
            Plain(Reg),
            Method(Reg, u32),
        }
        let tag = match &tt.tag {
            ox::Expression::StaticMemberExpression(m) => {
                let obj = self.expr(&m.object)?;
                let obj_reg = self.alloc_reg();
                if obj != obj_reg {
                    self.emit(Instr::Move { dst: obj_reg, src: obj });
                }
                let name = self.string_name(m.property.name.as_str());
                Tag::Method(obj_reg, name)
            }
            other => {
                let callee = self.expr(other)?;
                let callee_reg = self.alloc_reg();
                if callee != callee_reg {
                    self.emit(Instr::Move { dst: callee_reg, src: callee });
                }
                Tag::Plain(callee_reg)
            }
        };
        // The template object is memoized per source site: load the cache; on a hit
        // (a truthy template object) skip the build, else build + freeze + memoize so
        // every evaluation of THIS literal yields the same canonical frozen object.
        let strings_reg = self.alloc_reg();
        let site = self.template_site_count;
        self.template_site_count += 1;
        self.emit(Instr::TemplateGetCached { dst: strings_reg, site });
        let skip = self.here();
        self.emit(Instr::JumpIfTrue { cond: strings_reg, target: 0 }); // patched: cache hit
        self.build_template_strings(quasi, strings_reg)?;
        self.emit(Instr::TemplateSetCached { site, src: strings_reg });
        let after = self.here();
        self.patch_jump(skip, after);
        // Contiguous argument block: [strings, e0, e1, …].
        let arg_base = self.next_reg;
        for _ in 0..=n {
            self.alloc_reg();
        }
        let block_top = self.next_reg;
        self.emit(Instr::Move { dst: arg_base, src: strings_reg });
        for (i, e) in quasi.expressions.iter().enumerate() {
            let slot = arg_base + 1 + i as Reg;
            let v = self.expr_into(e, slot)?;
            if v != slot {
                self.emit(Instr::Move { dst: slot, src: v });
            }
            self.next_reg = block_top;
        }
        let argc = (n + 1) as u16;
        match tag {
            Tag::Plain(callee) => {
                if tail {
                    // Proper tail call (`return f`…``): reuse the frame for a
                    // plain function tag; others fall through to the Call.
                    self.emit(Instr::TailCall { callee, arg_base, argc });
                }
                self.emit(Instr::Call { dst, callee, arg_base, argc })
            }
            Tag::Method(obj, name) => {
                self.emit(Instr::CallMethod { dst, obj, name, arg_base, argc })
            }
        }
        Ok(dst)
    }

    /// Build the tagged-template strings array `[q0,q1,…]` (cooked) into `dst`,
    /// with its `.raw` property set to the array of raw (un-escaped) parts.
    fn build_template_strings(&mut self, quasi: &ox::TemplateLiteral, dst: Reg) -> R<()> {
        let nq = quasi.quasis.len() as u16;
        let save = self.next_reg;
        // Cooked array → dst. A quasi with an ILLEGAL escape sequence has no cooked
        // value (oxc sets `cooked` to None) — in a TAGGED template that element is
        // `undefined` (only the tag sees it; an untagged template would be a syntax
        // error), so load undefined rather than masking it as "".
        let cooked_base = self.next_reg;
        for q in &quasi.quasis {
            let r = self.alloc_reg();
            match q.value.cooked.as_ref() {
                Some(s) => {
                    // A `.lone_surrogates` quasi cooks to the oxc marker form —
                    // decode to real WTF-8 at intern time. (Raw parts below are
                    // source text — never markers.)
                    let idx = if q.lone_surrogates {
                        self.add_string_const_wtf8(s.as_str())
                    } else {
                        self.add_string_const(s.as_str())
                    };
                    self.emit(Instr::LoadConst { dst: r, idx });
                }
                None => self.emit(Instr::LoadUndefined { dst: r }),
            }
        }
        self.emit(Instr::NewArray { dst, arg_base: cooked_base, argc: nq });
        self.next_reg = save;
        // Raw array → a temp, then dst.raw = it.
        let raw_reg = self.alloc_reg();
        let raw_base = self.next_reg;
        for q in &quasi.quasis {
            let r = self.alloc_reg();
            let idx = self.add_string_const(q.value.raw.as_str());
            self.emit(Instr::LoadConst { dst: r, idx });
        }
        self.emit(Instr::NewArray { dst: raw_reg, arg_base: raw_base, argc: nq });
        self.emit(Instr::SetRaw { arr: dst, raw: raw_reg });
        self.next_reg = save;
        Ok(())
    }

    /// `String.raw` template: concatenate the RAW literal parts with the
    /// stringified interpolation values (`String.raw\`a\\n${1}b\`` → `a\\n1b`).
    fn string_raw(&mut self, quasi: &ox::TemplateLiteral, dst: Reg) -> R<Reg> {
        let r0 = quasi.quasis[0].value.raw.as_str();
        let idx = self.add_string_const(r0);
        self.emit(Instr::LoadConst { dst, idx });
        for (i, e) in quasi.expressions.iter().enumerate() {
            let r = self.expr(e)?;
            self.emit(Instr::Add { dst, a: dst, b: r });
            if let Some(qe) = quasi.quasis.get(i + 1) {
                let raw = qe.value.raw.as_str();
                if !raw.is_empty() {
                    let qidx = self.add_string_const(raw);
                    let qr = self.temp();
                    self.emit(Instr::LoadConst { dst: qr, idx: qidx });
                    self.emit(Instr::Add { dst, a: dst, b: qr });
                }
            }
        }
        Ok(dst)
    }

    fn array_literal(&mut self, a: &ox::ArrayExpression, dst: Reg) -> R<Reg> {
        // With a `...spread` element the final length is dynamic, so build the
        // array incrementally via ArrayAppend instead of the fixed-block NewArray.
        if a.elements.iter().any(|e| matches!(e, ox::ArrayExpressionElement::SpreadElement(_))) {
            self.emit(Instr::NewArray { dst, arg_base: self.next_reg, argc: 0 }); // []
            for el in &a.elements {
                let save = self.next_reg;
                match el {
                    ox::ArrayExpressionElement::Elision(_) => {
                        // An elided element is a HOLE, not a present `undefined`.
                        let v = self.temp();
                        self.emit(Instr::LoadHole { dst: v });
                        self.emit(Instr::ArrayAppend { arr: dst, val: v, spread: false });
                    }
                    ox::ArrayExpressionElement::SpreadElement(s) => {
                        let v = self.expr(&s.argument)?;
                        self.emit(Instr::ArrayAppend { arr: dst, val: v, spread: true });
                    }
                    other => {
                        let e = other.as_expression().ok_or("unsupported array element")?;
                        let v = self.expr(e)?;
                        self.emit(Instr::ArrayAppend { arr: dst, val: v, spread: false });
                    }
                }
                self.next_reg = save;
            }
            return Ok(dst);
        }
        // Elements must occupy a contiguous register run for NewArray. Reserve
        // the block first (same contiguity discipline as call args) so an
        // element expression's scratch temps allocate above the block.
        let count = a.elements.len() as u16;
        let base = self.next_reg;
        for _ in &a.elements {
            self.alloc_reg();
        }
        let block_top = self.next_reg;
        for (i, el) in a.elements.iter().enumerate() {
            let slot = base + i as Reg;
            match el {
                ox::ArrayExpressionElement::Elision(_) => {
                    // An elided element is a HOLE, not a present `undefined`.
                    self.emit(Instr::LoadHole { dst: slot });
                }
                ox::ArrayExpressionElement::SpreadElement(_) => unreachable!("handled above"),
                other => {
                    let e = other.as_expression().ok_or("unsupported array element")?;
                    let v = self.expr_into(e, slot)?;
                    if v != slot {
                        self.emit(Instr::Move { dst: slot, src: v });
                    }
                }
            }
            self.next_reg = block_top;
        }
        self.emit(Instr::NewArray { dst, arg_base: base, argc: count });
        Ok(dst)
    }

    fn object_literal(&mut self, o: &ox::ObjectExpression, dst: Reg) -> R<Reg> {
        self.emit(Instr::NewObject { dst });
        for prop in &o.properties {
            let save = self.next_reg;
            match prop {
                ox::ObjectPropertyKind::ObjectProperty(p) => {
                    if matches!(p.kind, ox::PropertyKind::Get | ox::PropertyKind::Set) {
                        // `{ get k(){…} }` / `{ set k(v){…} }` — an accessor property.
                        // The key is loaded into a register (computed expr or the
                        // static key string); a get+set pair on one key merges.
                        let key = if p.computed {
                            let ke =
                                p.key.as_expression().ok_or("unsupported computed accessor key")?;
                            self.expr(ke)?
                        } else {
                            let k = match &p.key {
                                ox::PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                                ox::PropertyKey::StringLiteral(s) => string_literal_key(s),
                                ox::PropertyKey::NumericLiteral(n) => fmt_key_num(n.value),
                                ox::PropertyKey::BigIntLiteral(b) => b.value.to_string(),
                                _ => return Err("unsupported accessor key in the zipp-vm subset".into()),
                            };
                            let kr = self.alloc_reg();
                            let idx = self.add_string_const(&k);
                            self.emit(Instr::LoadConst { dst: kr, idx });
                            kr
                        };
                        // An accessor is a method: it gets a [[HomeObject]], so `super`
                        // inside it resolves via the object (set the transient flag the
                        // function-body compiler consumes).
                        self.cx.obj_method_super = true;
                        let func = self.expr(&p.value)?;
                        self.cx.obj_method_super = false;
                        // `Function.prototype.toString` of an object accessor is the
                        // whole `get k(){}` / `set k(v){}`; the value-Function span
                        // omits the `get`/`set` and the key, so patch the just-
                        // compiled proto (pushed last) with the ObjectProperty span.
                        let fid = self.cx.functions.len() - 1;
                        self.cx.functions[fid].source =
                            self.cx.src_slice(p.span.start, p.span.end);
                        self.cx.functions[fid].non_constructable = true; // accessor = method
                        let is_setter = matches!(p.kind, ox::PropertyKind::Set);
                        self.emit(Instr::DefineAccessor { obj: dst, key, func, is_setter });
                        self.emit(Instr::SetHomeObject { method: func, home: dst });
                        // SetFunctionName: a getter/setter is named "get k"/"set k"
                        // (a Symbol key → "get [desc]"), at runtime so a computed key
                        // is handled too.
                        self.emit(Instr::SetFnNameFromKey {
                            func,
                            key,
                            prefix: if is_setter { 2 } else { 1 },
                        });
                    } else if p.computed {
                        // Computed key `{[expr]: v}` → CreateDataProperty with a
                        // runtime key: ToPropertyKey runs BEFORE the value
                        // evaluates (its coercion side effects order first), and
                        // a computed "__proto__" defines an ORDINARY own
                        // property (only the textual colon form sets the proto).
                        let ke = p.key.as_expression().ok_or("unsupported computed object key")?;
                        let raw = self.expr(ke)?;
                        let key = self.alloc_reg();
                        self.emit(Instr::ToPropKey { dst: key, obj: dst, src: raw });
                        // A computed concise method gets a [[HomeObject]] (for super).
                        if p.method {
                            self.cx.obj_method_super = true;
                        }
                        let v = self.expr(&p.value)?;
                        self.cx.obj_method_super = false;
                        // A computed concise method `{ [expr](){} }` (incl. `*`/`async`):
                        // its toString is the whole `[expr](){}` — the value-Function span
                        // omits the computed key + modifiers, so patch it with the
                        // ObjectProperty span (mirrors the static-key method branch below).
                        if p.method {
                            let fid = self.cx.functions.len() - 1;
                            self.cx.functions[fid].source =
                                self.cx.src_slice(p.span.start, p.span.end);
                            self.cx.functions[fid].non_constructable = true;
                        }
                        // SetFunctionName: an anonymous function/arrow/class value
                        // takes the (runtime) computed key as its name — a Symbol key
                        // becomes "[description]".
                        if is_anonymous_fn_def(&p.value) {
                            self.emit(Instr::SetFnNameFromKey { func: v, key, prefix: 0 });
                        }
                        self.emit(Instr::InitDataPropDyn { obj: dst, key, val: v });
                        if p.method {
                            self.emit(Instr::SetHomeObject { method: v, home: dst });
                        }
                    } else {
                        // Static identifier / string / number literal key.
                        let key = match &p.key {
                            ox::PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                            ox::PropertyKey::StringLiteral(s) => string_literal_key(s),
                            ox::PropertyKey::NumericLiteral(n) => fmt_key_num(n.value),
                                ox::PropertyKey::BigIntLiteral(b) => b.value.to_string(),
                            _ => return Err("unsupported object key in the zipp-vm subset".into()),
                        };
                        let name = self.string_name(&key);
                        // `{ fn: function(){}, m(){}, C: class{} }` — an anonymous
                        // value function/class takes the property key as its name,
                        // EXCEPT `{ __proto__: fn }` (a proto-setter, not a data
                        // property): its function value stays anonymous.
                        let vtmp = self.alloc_reg();
                        // A concise method gets a [[HomeObject]] (for `super`); a plain
                        // `k: function(){}` data property does NOT.
                        if p.method {
                            self.cx.obj_method_super = true;
                        }
                        let v = if key == "__proto__" && !p.method && !p.shorthand {
                            self.expr_into(&p.value, vtmp)?
                        } else {
                            self.compile_named_init(vtmp, &p.value, &key)?
                        };
                        self.cx.obj_method_super = false;
                        // Shorthand method `{ m(){}, *g(){}, async a(){} }`: its
                        // toString is the whole `m(){}` (the value-Function span omits
                        // the name/modifiers). Patch the proto just compiled (last).
                        // Regular `k: function(){}` keeps the value's own span.
                        if p.method {
                            let fid = self.cx.functions.len() - 1;
                            self.cx.functions[fid].source =
                                self.cx.src_slice(p.span.start, p.span.end);
                            self.cx.functions[fid].non_constructable = true; // concise method
                        }
                        // `{ __proto__: v }` (colon form ONLY — shorthand
                        // `{ __proto__ }` is an ordinary data property) sets the
                        // prototype — a real [[Set]]/proto-setter; every other
                        // key is CreateDataProperty, which must ignore an
                        // inherited accessor / non-writable prop.
                        if key == "__proto__" && !p.method && !p.shorthand {
                            self.emit(Instr::SetProp { obj: dst, name, val: v });
                        } else {
                            self.emit(Instr::InitDataProp { obj: dst, name, val: v });
                        }
                        if p.method {
                            self.emit(Instr::SetHomeObject { method: v, home: dst });
                        }
                    }
                }
                ox::ObjectPropertyKind::SpreadProperty(s) => {
                    let src = self.expr(&s.argument)?;
                    self.emit(Instr::ObjectSpread { target: dst, src });
                }
            }
            self.next_reg = save; // reclaim this property's scratch temps
        }
        Ok(dst)
    }

    fn load_number(&mut self, dst: Reg, n: f64) {
        if n.fract() == 0.0 && n >= i32::MIN as f64 && n <= i32::MAX as f64 {
            self.emit(Instr::LoadInt { dst, val: n as i32 });
        } else {
            let idx = self.add_const(Value::num(n));
            self.emit(Instr::LoadConst { dst, idx });
        }
    }

    fn binary(&mut self, b: &ox::BinaryExpression, dst: Reg) -> R<Reg> {
        use ox::BinaryOperator as Op;
        // `x instanceof Ctor`: only built-in constructors are recognised (the
        // engine has no user prototype chain). Decided structurally in the VM.
        if matches!(b.operator, Op::Instanceof) {
            // A built-in constructor name → structural InstanceOf; anything else
            // (a user class value) → runtime InstanceOfDyn against its class link.
            if let ox::Expression::Identifier(id) = &b.right {
                if let Some(ctor) = InstanceCtor::from_name(&id.name) {
                    let val = self.expr(&b.left)?;
                    self.emit(Instr::InstanceOf { dst, val, ctor });
                    return Ok(dst);
                }
            }
            let val = self.expr(&b.left)?;
            let ctor = self.expr(&b.right)?;
            self.emit(Instr::InstanceOfDyn { dst, val, ctor });
            return Ok(dst);
        }
        // `key in obj`.
        if matches!(b.operator, Op::In) {
            let key = self.expr(&b.left)?;
            let obj = self.expr(&b.right)?;
            self.emit(Instr::HasProp { dst, key, obj, brand: false });
            return Ok(dst);
        }
        // `a - <int literal>` and `a + <int literal>` → AddInt fast path, but
        // ONLY when the left operand is provably numeric. `+` is overloaded for
        // string concatenation, so `'n=' + 42` must NOT take the integer path
        // (it would coerce the string to NaN). Subtraction is always numeric,
        // so it's always eligible; addition is eligible only when `left` is a
        // numeric literal or another arithmetic expression we just produced a
        // number from. When unsure, fall through to the generic `Add`, which
        // handles string concatenation correctly.
        if let ox::Expression::NumericLiteral(n) = &b.right {
            let imm_ok = n.value.fract() == 0.0
                && n.value >= i32::MIN as f64
                && n.value <= i32::MAX as f64;
            let eligible = matches!(b.operator, Op::Subtraction)
                || (matches!(b.operator, Op::Addition) && is_numeric_expr(&b.left));
            if imm_ok && eligible {
                let a = self.expr(&b.left)?;
                let mut imm = n.value as i32;
                if matches!(b.operator, Op::Subtraction) {
                    imm = -imm;
                }
                self.emit(Instr::AddInt { dst, a, imm, upd: false });
                return Ok(dst);
            }
        }
        let a = self.expr(&b.left)?;
        let r = self.expr(&b.right)?;
        let instr = match b.operator {
            Op::Addition => Instr::Add { dst, a, b: r },
            Op::Subtraction => Instr::Sub { dst, a, b: r },
            Op::Multiplication => Instr::Mul { dst, a, b: r },
            Op::Division => Instr::Div { dst, a, b: r },
            Op::Remainder => Instr::Mod { dst, a, b: r },
            Op::LessThan => Instr::Lt { dst, a, b: r },
            Op::LessEqualThan => Instr::Le { dst, a, b: r },
            Op::GreaterThan => Instr::Gt { dst, a, b: r },
            Op::GreaterEqualThan => Instr::Ge { dst, a, b: r },
            Op::StrictEquality => Instr::Eq { dst, a, b: r },
            Op::StrictInequality => Instr::Ne { dst, a, b: r },
            Op::Equality => Instr::LooseEq { dst, a, b: r },
            Op::Inequality => Instr::LooseNe { dst, a, b: r },
            Op::BitwiseAnd => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::And },
            Op::BitwiseOR => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Or },
            Op::BitwiseXOR => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Xor },
            Op::ShiftLeft => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Shl },
            Op::ShiftRight => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Shr },
            Op::ShiftRightZeroFill => Instr::Bitwise { dst, a, b: r, op: BitwiseOp::Ushr },
            Op::Exponential => Instr::Pow { dst, a, b: r },
            _ => return Err("unsupported binary operator (zipp-vm v1)".into()),
        };
        self.emit(instr);
        Ok(dst)
    }

    fn logical(&mut self, l: &ox::LogicalExpression, dst: Reg) -> R<Reg> {
        use ox::LogicalOperator as Op;
        // `a && b`: eval a into dst; if falsy, short-circuit; else eval b.
        let _a = self.expr_into(&l.left, dst)?;
        if _a != dst {
            self.emit(Instr::Move { dst, src: _a });
        }
        match l.operator {
            Op::And => {
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond: dst, target: 0 });
                let b = self.expr_into(&l.right, dst)?;
                if b != dst {
                    self.emit(Instr::Move { dst, src: b });
                }
                let end = self.here();
                self.patch_jump(j, end);
            }
            Op::Or => {
                let j = self.here();
                self.emit(Instr::JumpIfTrue { cond: dst, target: 0 });
                let b = self.expr_into(&l.right, dst)?;
                if b != dst {
                    self.emit(Instr::Move { dst, src: b });
                }
                let end = self.here();
                self.patch_jump(j, end);
            }
            Op::Coalesce => {
                // `a ?? b`: keep `a` unless it is STRICTLY null/undefined.
                let save = self.next_reg;
                let undef = self.alloc_reg();
                let isnull = self.alloc_reg();
                self.emit_is_nullish(dst, isnull, undef);
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond: isnull, target: 0 }); // non-nullish → keep dst
                self.next_reg = save; // the nullish-test temps are dead now
                let b = self.expr_into(&l.right, dst)?;
                if b != dst {
                    self.emit(Instr::Move { dst, src: b });
                }
                let end = self.here();
                self.patch_jump(j, end);
            }
        }
        Ok(dst)
    }

    fn unary(&mut self, u: &ox::UnaryExpression, dst: Reg) -> R<Reg> {
        use ox::UnaryOperator as Op;
        match u.operator {
            Op::UnaryNegation => {
                let a = self.expr(&u.argument)?;
                self.emit(Instr::Neg { dst, a });
                Ok(dst)
            }
            Op::UnaryPlus => {
                let a = self.expr(&u.argument)?;
                self.emit(Instr::ToNum { dst, a });
                Ok(dst)
            }
            Op::LogicalNot => {
                let a = self.expr(&u.argument)?;
                self.emit(Instr::Not { dst, a });
                Ok(dst)
            }
            Op::Typeof => {
                // `typeof <unbound identifier>` must yield "undefined", NOT throw
                // a ReferenceError — and this holds when the identifier is wrapped in
                // parentheses (`typeof (f)`), so peel them first. A bare identifier
                // that resolves to a global is read with the non-throwing variant so
                // the never-declared sentinel degrades to undefined.
                // (`undefined`/`NaN`/`Infinity` are literals, handled by `expr`.)
                let mut arg = &u.argument;
                while let ox::Expression::ParenthesizedExpression(p) = arg {
                    arg = &p.expression;
                }
                if let ox::Expression::Identifier(id) = arg {
                    if !matches!(id.name.as_str(), "undefined" | "NaN" | "Infinity") {
                        if let Binding::Global(idx) = self.resolve(id.name.as_str()) {
                            // A DECLARED top-level lexical still observes its
                            // TDZ through typeof (a ReferenceError) — only a
                            // name the compiler never saw declared degrades to
                            // "undefined" via the non-throwing load.
                            let declared_lexical = self.cx.lexical_globals.contains(&(idx as u32))
                                || self.cx.const_globals.contains(&(idx as u32));
                            if declared_lexical {
                                self.emit(Instr::LoadGlobal { dst, idx });
                            } else if self.box_all_locals || self.cx.dyn_global_zone {
                                self.emit(Instr::LoadGlobalOrUndefinedDyn { dst, idx });
                            } else {
                                self.emit(Instr::LoadGlobalOrUndefined { dst, idx });
                            }
                            self.emit(Instr::TypeOf { dst, a: dst });
                            return Ok(dst);
                        }
                    }
                }
                let a = self.expr(arg)?;
                self.emit(Instr::TypeOf { dst, a });
                Ok(dst)
            }
            Op::BitwiseNot => {
                let a = self.expr(&u.argument)?;
                self.emit(Instr::BitNot { dst, a });
                Ok(dst)
            }
            Op::Void => {
                // Evaluate the operand for side effects; the value is `undefined`.
                let _ = self.expr(&u.argument)?;
                self.emit(Instr::LoadUndefined { dst });
                Ok(dst)
            }
            Op::Delete => self.delete_expr(&u.argument, dst),
        }
    }

    /// `delete <ref>` — remove a property (`obj.x` / `obj[k]`) and yield the
    /// boolean result. A non-reference operand (or a bare identifier) evaluates
    /// for side effects and yields `true` (matching sloppy-mode `delete x`).
    fn delete_expr(&mut self, arg: &ox::Expression, dst: Reg) -> R<Reg> {
        match arg {
            ox::Expression::StaticMemberExpression(m) => {
                // `delete super.x` is a runtime ReferenceError (a super reference has
                // no [[Delete]]). Not a SyntaxError, so it's thrown when evaluated.
                if matches!(&m.object, ox::Expression::Super(_)) {
                    let e = self.alloc_reg();
                    self.emit(Instr::NewError { dst: e, kind: 4, arg: None, opts: None, errors: None });
                    self.emit(Instr::Throw { src: e });
                    return Ok(dst);
                }
                let obj = self.expr(&m.object)?;
                let name = self.string_name(&m.property.name);
                let strict = self.cx.in_strict;
                self.emit(Instr::DeleteProp { dst, obj, name, strict });
                Ok(dst)
            }
            ox::Expression::ComputedMemberExpression(m) => {
                // `delete super[expr]`: SuperProperty evaluation does
                // GetThisBinding BEFORE the key expression — in a derived ctor
                // before super() that ReferenceError fires FIRST and `expr`
                // never runs. Otherwise evaluate `expr` (side effects +
                // ToPropertyKey), then throw a ReferenceError — a super
                // reference has no delete.
                if matches!(&m.object, ox::Expression::Super(_)) {
                    self.this_check();
                    let _ = self.expr(&m.expression)?;
                    let e = self.alloc_reg();
                    self.emit(Instr::NewError { dst: e, kind: 4, arg: None, opts: None, errors: None });
                    self.emit(Instr::Throw { src: e });
                    return Ok(dst);
                }
                let obj = self.expr(&m.object)?;
                let key = self.expr(&m.expression)?;
                let strict = self.cx.in_strict;
                self.emit(Instr::DeleteIndex { dst, obj, key, strict });
                Ok(dst)
            }
            ox::Expression::ParenthesizedExpression(p) => self.delete_expr(&p.expression, dst),
            // `delete <identifier>`: in strict mode an early SyntaxError; in sloppy
            // mode deleting a resolvable binding (var/let/const/param/function or a
            // declared global) yields `false` (non-configurable), while an
            // unresolvable name is a no-op that yields `true` — and must NOT be
            // evaluated (evaluating an undeclared name would throw ReferenceError).
            ox::Expression::Identifier(id) => {
                if self.cx.in_strict {
                    return Err(
                        "SyntaxError: Delete of an unqualified identifier in strict mode".into(),
                    );
                }
                // Inside a `with`, `delete name` removes the binding from the
                // innermost with-object that has it (yielding its delete result),
                // else falls through to the static-binding delete semantics below.
                let with_objs = self.with_obj_regs(id.name.as_str());
                if !with_objs.is_empty() {
                    return Ok(self.delete_with(id.name.as_str(), &with_objs, dst));
                }
                // A binding is non-configurable — `delete` yields `false` — when it is
                // a local (param/`var`/`let`/`const`/function) or a DECLARED global
                // `var`/`let`/`const`. A builtin, an implicitly-created global
                // (`x = 1` with no declaration), an eval-introduced var, or an
                // unresolvable name is configurable / a no-op, so `delete` yields
                // `true` — and a configurable binding is actually REMOVED.
                // `NaN`/`Infinity`/`undefined` are the only non-configurable builtin
                // global properties; they're not tracked as compiler globals, so
                // check them by name (a local of that name still resolves below).
                if matches!(id.name.as_str(), "NaN" | "Infinity" | "undefined") {
                    self.emit(Instr::LoadBool { dst, val: false });
                    return Ok(dst);
                }
                match self.resolve_existing(&id.name) {
                    Some(Binding::Local(_))
                    | Some(Binding::LocalCell(_))
                    | Some(Binding::Upvalue(_))
                    | Some(Binding::ClassName(_)) => {
                        self.emit(Instr::LoadBool { dst, val: false });
                    }
                    Some(Binding::Global(slot)) => {
                        // A resolved GLOBAL defers to the runtime: DeleteGlobal
                        // checks the PROGRAM's decl lists (an eval-compiled
                        // `delete x` must see the program's `var x` as
                        // non-configurable, and an eval-introduced var as
                        // deletable — this compilation's own cx lists can't
                        // tell) and removes a configurable binding.
                        self.emit(Instr::DeleteGlobal { dst, slot });
                    }
                    None => {
                        // Unreferenced-so-far name: allocate its global slot
                        // (like an identifier reference would) so the runtime
                        // check sees the LIVE binding — crucial for an eval'd
                        // `delete x` whose `x` only exists in the outer
                        // program. A never-declared name's fresh slot is
                        // UNINITIALIZED, so DeleteGlobal is a true no-op.
                        let slot = self.cx.global_slot(&id.name) as u32;
                        self.emit(Instr::DeleteGlobal { dst, slot });
                    }
                }
                Ok(dst)
            }
            other => {
                let _ = self.expr(other)?;
                self.emit(Instr::LoadBool { dst, val: true });
                Ok(dst)
            }
        }
    }

    fn update(&mut self, u: &ox::UpdateExpression, dst: Reg) -> R<Reg> {
        let delta = match u.operator {
            ox::UpdateOperator::Increment => 1,
            ox::UpdateOperator::Decrement => -1,
        };
        // `obj.x++` / `arr[i]--` etc — read the member, yield old (postfix) or
        // new (prefix), write the incremented value back to the same slot.
        match &u.argument {
            // `super.x++` / `--super.x` — read via the super-get sequence,
            // coerce/step, write back via the super-set sequence.
            ox::SimpleAssignmentTarget::StaticMemberExpression(m)
                if matches!(&m.object, ox::Expression::Super(_)) =>
            {
                let pid = self.super_class;
                if pid.is_none() && !self.super_home_obj {
                    return Err("`super.x++` is only valid in a method".into());
                }
                self.this_check();
                let name = self.string_name(m.property.name.as_str());
                let cur = self.temp();
                match pid {
                    Some(p) => self.emit(Instr::SuperGet { dst: cur, home_class_id: p, name }),
                    None => self.emit(Instr::SuperGetObj { dst: cur, name }),
                }
                let oldnum = self.temp();
                self.emit(Instr::AddInt { dst: oldnum, a: cur, imm: 0, upd: true });
                let nw = self.temp();
                self.emit(Instr::AddInt { dst: nw, a: oldnum, imm: delta, upd: true });
                match pid {
                    Some(p) => self.emit(Instr::SuperSet { home_class_id: p, name, val: nw }),
                    None => self.emit(Instr::SuperSetObj { name, val: nw }),
                }
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { oldnum } });
                return Ok(dst);
            }
            // `super[k]++` / `--super[k]` — SuperProperty evaluation checks the
            // this-TDZ BEFORE evaluating the key Expression
            // (prop-expr-uninitialized-this-putvalue-increment), and the
            // computed super ops capture GetSuperBase before ToPropertyKey.
            ox::SimpleAssignmentTarget::ComputedMemberExpression(m)
                if matches!(&m.object, ox::Expression::Super(_)) =>
            {
                let pid = self.super_class;
                if pid.is_none() && !self.super_home_obj {
                    return Err("`super[k]++` is only valid in a method".into());
                }
                self.this_check();
                let key = self.expr(&m.expression)?;
                let key_reg = self.alloc_reg();
                if key != key_reg {
                    self.emit(Instr::Move { dst: key_reg, src: key });
                }
                let cur = self.temp();
                match pid {
                    Some(p) => self.emit(Instr::SuperGetComputed { dst: cur, home_class_id: p, key: key_reg }),
                    None => self.emit(Instr::SuperGetObjComputed { dst: cur, key: key_reg }),
                }
                let oldnum = self.temp();
                self.emit(Instr::AddInt { dst: oldnum, a: cur, imm: 0, upd: true });
                let nw = self.temp();
                self.emit(Instr::AddInt { dst: nw, a: oldnum, imm: delta, upd: true });
                match pid {
                    Some(p) => self.emit(Instr::SuperSetComputed { home_class_id: p, key: key_reg, val: nw }),
                    None => self.emit(Instr::SuperSetObjComputed { key: key_reg, val: nw }),
                }
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { oldnum } });
                return Ok(dst);
            }
            ox::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                let obj = self.expr(&m.object)?;
                let name = self.string_name(m.property.name.as_str());
                let cur = self.temp();
                self.emit(Instr::GetProp { dst: cur, obj, name });
                // ToNumeric(old) ONCE (AddInt imm:0), derive the new value from it,
                // and yield the COERCED old (postfix) — `x++` returns a number, not
                // the raw operand. Single coercion = one valueOf for an object operand.
                let oldnum = self.temp();
                self.emit(Instr::AddInt { dst: oldnum, a: cur, imm: 0, upd: true });
                let nw = self.temp();
                self.emit(Instr::AddInt { dst: nw, a: oldnum, imm: delta, upd: true });
                self.emit(Instr::SetProp { obj, name, val: nw });
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { oldnum } });
                return Ok(dst);
            }
            ox::SimpleAssignmentTarget::ComputedMemberExpression(m) => {
                let obj = self.expr(&m.object)?;
                let key = self.expr(&m.expression)?;
                // `o[k]++` reads then writes `o[k]` — coerce the key ToPropertyKey
                // ONCE and reuse it (its toString/valueOf must not run twice).
                let keyk = self.temp();
                self.emit(Instr::ToPropKey { dst: keyk, obj, src: key });
                let cur = self.temp();
                self.emit(Instr::GetIndex { dst: cur, obj, key: keyk });
                let oldnum = self.temp();
                self.emit(Instr::AddInt { dst: oldnum, a: cur, imm: 0, upd: true });
                let nw = self.temp();
                self.emit(Instr::AddInt { dst: nw, a: oldnum, imm: delta, upd: true });
                self.emit(Instr::SetIndex { obj, key: keyk, val: nw });
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { oldnum } });
                return Ok(dst);
            }
            // `obj.#x++` — like a static member, keyed "#x".
            ox::SimpleAssignmentTarget::PrivateFieldExpression(p) => {
                self.check_private_declared(&p.field.name)?;
                let obj = self.expr(&p.object)?;
                let name = self.string_name(&private_key(&p.field.name));
                let cur = self.temp();
                self.emit(Instr::GetProp { dst: cur, obj, name });
                let oldnum = self.temp();
                self.emit(Instr::AddInt { dst: oldnum, a: cur, imm: 0, upd: true });
                let nw = self.temp();
                self.emit(Instr::AddInt { dst: nw, a: oldnum, imm: delta, upd: true });
                self.emit(Instr::SetProp { obj, name, val: nw });
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { oldnum } });
                return Ok(dst);
            }
            _ => {}
        }
        // `x++` / `++x` / `x--` / `--x` on a simple identifier.
        let name = match &u.argument {
            ox::SimpleAssignmentTarget::AssignmentTargetIdentifier(id) => id.name.to_string(),
            _ => return Err("update on this target not in zipp-vm v1".into()),
        };
        // Strict mode: `eval++` / `--arguments` is an early SyntaxError.
        strict_name_err(self.cx.in_strict, &name)?;
        // Inside a `with`, the updated identifier may be a property of an active
        // with-object (innermost first): read → increment → write through it.
        let with_objs = self.with_obj_regs(&name);
        if !with_objs.is_empty() {
            // Resolve the Reference ONCE (one HasBinding — the @@unscopables
            // getter runs once), then read and write through that target.
            let (found, tgt) = self.emit_with_probe(&name, &with_objs);
            self.emit_with_rmw_read(&name, found, tgt, dst);
            if u.prefix {
                self.emit(Instr::AddInt { dst, a: dst, imm: delta, upd: true });
                self.emit_with_rmw_write(&name, found, tgt, dst);
                return Ok(dst); // dst holds the new value
            }
            // Postfix: ToNumeric(old) in place, derive the new value, store it,
            // return the COERCED old.
            self.emit(Instr::AddInt { dst, a: dst, imm: 0, upd: true });
            let tmp = self.temp();
            self.emit(Instr::AddInt { dst: tmp, a: dst, imm: delta, upd: true });
            self.emit_with_rmw_write(&name, found, tgt, tmp);
            self.next_reg -= 1; // reclaim tmp
            return Ok(dst); // dst still holds the (coerced) old value
        }
        let binding = self.resolve(&name);
        if let Binding::Local(r) = binding {
            if !self.const_regs.contains(&r) {
                // Plain mutable register local: mutate in place.
                if u.prefix {
                    self.emit(Instr::AddInt { dst: r, a: r, imm: delta, upd: true });
                    if r != dst {
                        self.emit(Instr::Move { dst, src: r });
                    }
                } else {
                    // Yield ToNumeric(old) (one coercion), then increment from it.
                    self.emit(Instr::AddInt { dst, a: r, imm: 0, upd: true });
                    self.emit(Instr::AddInt { dst: r, a: dst, imm: delta, upd: true });
                }
                return Ok(dst);
            }
        }
        // Cell / upvalue / global / const-local: read into `dst`, compute, store
        // back (store_binding throws for a const after the read + increment).
        let cur = self.load_binding(&binding, dst); // == dst
        if u.prefix {
            self.emit(Instr::AddInt { dst: cur, a: cur, imm: delta, upd: true });
            self.store_binding(&binding, cur);
            Ok(dst) // dst holds the new value
        } else {
            // Coerce the old value in place (cur == dst), compute the new value in a
            // temp, store it, and return the COERCED old.
            self.emit(Instr::AddInt { dst: cur, a: cur, imm: 0, upd: true });
            let tmp = self.temp();
            self.emit(Instr::AddInt { dst: tmp, a: cur, imm: delta, upd: true });
            self.store_binding(&binding, tmp);
            self.next_reg -= 1; // reclaim tmp
            Ok(dst) // dst still holds the (coerced) old value
        }
    }

    /// The active `with`-object registers that can shadow `name`, innermost
    /// first. Empty (the common case) when no `with` is active or the name is
    /// bound by a declaration INSIDE the innermost applicable `with` body —
    /// in which case the binding resolves statically with no dynamic probe.
    fn with_objs_for(&self, name: &str) -> Vec<Reg> {
        if self.with_stack.is_empty() {
            return Vec::new();
        }
        // Depth of the innermost lexical scope that declares `name` (-1 if the
        // name is free here → a global/upvalue/unresolved, shadowable by all).
        let mut depth: isize = -1;
        for (i, scope) in self.scopes.iter().enumerate() {
            if scope.iter().any(|(n, _)| n == name) {
                depth = i as isize;
            }
        }
        // A with entered ABOVE the binding's scope (floor > depth) can shadow it.
        self.with_stack
            .iter()
            .rev()
            .filter(|w| w.floor as isize > depth)
            .map(|w| w.obj_reg)
            .collect()
    }

    /// Like `with_objs_for` but yields each shadowing with-object's hidden
    /// BINDING NAME (for a nested function to capture as an upvalue).
    fn with_names_for(&self, name: &str) -> Vec<String> {
        if self.with_stack.is_empty() {
            return Vec::new();
        }
        let mut depth: isize = -1;
        for (i, scope) in self.scopes.iter().enumerate() {
            if scope.iter().any(|(n, _)| n == name) {
                depth = i as isize;
            }
        }
        self.with_stack
            .iter()
            .rev()
            .filter(|w| w.floor as isize > depth)
            .filter_map(|w| {
                self.scopes
                    .iter()
                    .flatten()
                    .find(|(_, r)| *r == w.obj_reg)
                    .map(|(n, _)| n.clone())
            })
            .collect()
    }

    /// EVERY with-object that can shadow `name` here — this function's own
    /// `with` scopes (innermost first) followed by ENCLOSING functions' with
    /// scopes (via `inherited_with_shadows`) — each materialized into a plain
    /// register for the probe chain (own cells unwrapped with CellGet,
    /// inherited ones loaded as upvalues). The with-aware identifier paths all
    /// route through this; it returns empty on the common no-`with` path.
    fn with_obj_regs(&mut self, name: &str) -> Vec<Reg> {
        let mut out: Vec<Reg> = Vec::new();
        for reg in self.with_objs_for(name) {
            if self.cell_regs.contains(&reg) {
                let t = self.temp();
                self.emit(Instr::CellGet { dst: t, cell: reg });
                out.push(t);
            } else {
                out.push(reg);
            }
        }
        // Enclosing-function withs apply only to names this function does not
        // bind itself (a local/param/self-name is never shadowed from outside).
        let bound_here = self.scopes.iter().flatten().any(|(n, _)| n == name)
            || self.self_name.as_ref().is_some_and(|(n, _)| n == name);
        if !bound_here {
            if let Some(chain) = self.inherited_with_shadows.get(name).cloned() {
                for wname in chain {
                    if let Some(idx) = self.resolve_upvalue(&wname) {
                        let t = self.temp();
                        self.emit(Instr::UpvalGet { dst: t, idx });
                        out.push(t);
                    }
                }
            }
        }
        out
    }

    /// Emit the per-with-object probe+read chain for a READ of `nidx`: for each
    /// object innermost-first, `WithHas` → on hit `GetProp` into `dst` then jump
    /// past the fallback. Returns the "jump to end" ips for the caller to patch
    /// AFTER it emits its own fallback (the static binding, or a literal).
    fn emit_with_get_chain(&mut self, nidx: u32, objs: &[Reg], dst: Reg) -> Vec<u32> {
        let mut end_jumps = Vec::new();
        for &obj in objs {
            let flag = self.temp();
            self.emit(Instr::WithHas { dst: flag, obj, name: nidx });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse { cond: flag, target: 0 });
            self.next_reg -= 1; // reclaim the flag temp (dead after the branch)
            // GetBindingValue re-checks HasProperty (the WithHas @@unscopables
            // getter may delete the binding); strictness is the REFERENCE
            // site's, not the with statement's.
            self.emit(Instr::WithGet { dst, obj, name: nidx, strict: self.cx.in_strict });
            let je = self.here();
            self.emit(Instr::Jump { target: 0 });
            end_jumps.push(je);
            let nxt = self.here();
            self.patch_jump(jf, nxt);
        }
        end_jumps
    }

    /// Emit the callee-resolution chain for a bare-identifier CALL inside a
    /// `with`: probe each with-object (innermost first); the first hit reads
    /// the callee via the GetBindingValue protocol (`WithGet`: HasProperty +
    /// Get) and records the with-object as `this` (WithBaseObject); the
    /// fall-through resolves the static binding with `this` = undefined.
    /// Returns `(callee_reg, this_reg)` — two freshly-allocated registers the
    /// caller must keep live across argument evaluation.
    fn emit_with_callee_chain(&mut self, name: &str, objs: &[Reg]) -> (Reg, Reg) {
        let nidx = self.string_name(name);
        let callee_reg = self.alloc_reg();
        let this_reg = self.alloc_reg();
        let mut chain_done = Vec::new();
        for &obj in objs {
            let flag = self.temp();
            self.emit(Instr::WithHas { dst: flag, obj, name: nidx });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse { cond: flag, target: 0 });
            self.next_reg -= 1; // reclaim the flag temp
            self.emit(Instr::WithGet {
                dst: callee_reg,
                obj,
                name: nidx,
                strict: self.cx.in_strict,
            });
            self.emit(Instr::Move { dst: this_reg, src: obj });
            let jd = self.here();
            self.emit(Instr::Jump { target: 0 });
            chain_done.push(jd);
            let nxt = self.here();
            self.patch_jump(jf, nxt);
        }
        // Fallback: the static binding, this = undefined.
        let b = self.resolve(name);
        let r = self.load_binding(&b, callee_reg);
        if r != callee_reg {
            self.emit(Instr::Move { dst: callee_reg, src: r });
        }
        self.emit(Instr::LoadUndefined { dst: this_reg });
        let end = self.here();
        for j in chain_done {
            self.patch_jump(j, end);
        }
        (callee_reg, this_reg)
    }

    /// Emit a `with`-aware read of `name` into `dst`: probe each with-object
    /// (innermost first) and read from the first that has the binding; otherwise
    /// fall back to the static (lexical/global) binding. Returns `dst`.
    fn load_with(&mut self, name: &str, objs: &[Reg], dst: Reg) -> Reg {
        let nidx = self.string_name(name);
        let end_jumps = self.emit_with_get_chain(nidx, objs, dst);
        // Fallback: the static binding.
        let b = self.resolve(name);
        let r = self.load_binding(&b, dst);
        if r != dst {
            self.emit(Instr::Move { dst, src: r });
        }
        let end = self.here();
        for je in end_jumps {
            self.patch_jump(je, end);
        }
        dst
    }

    /// Emit a `with`-aware write of `src` to `name`: store into the first
    /// with-object (innermost first) that has the binding; otherwise fall back
    /// to the static (lexical/global) binding.
    fn store_with(&mut self, name: &str, objs: &[Reg], src: Reg) {
        let nidx = self.string_name(name);
        let mut end_jumps = Vec::new();
        for &obj in objs {
            let flag = self.temp();
            self.emit(Instr::WithHas { dst: flag, obj, name: nidx });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse { cond: flag, target: 0 });
            self.next_reg -= 1;
            self.emit(Instr::WithSet { obj, name: nidx, val: src, strict: self.cx.in_strict });
            let je = self.here();
            self.emit(Instr::Jump { target: 0 });
            end_jumps.push(je);
            let nxt = self.here();
            self.patch_jump(jf, nxt);
        }
        let b = self.resolve(name);
        self.store_binding(&b, src);
        let end = self.here();
        for je in end_jumps {
            self.patch_jump(je, end);
        }
    }

    /// Emit a `with`-aware `delete name` into `dst`: delete from the first
    /// with-object (innermost first) that has the binding, else fall back to the
    /// static-binding delete semantics (mirrors `delete_expr`'s identifier arm).
    fn delete_with(&mut self, name: &str, objs: &[Reg], dst: Reg) -> Reg {
        let nidx = self.string_name(name);
        let mut end_jumps = Vec::new();
        for &obj in objs {
            let flag = self.temp();
            self.emit(Instr::WithHas { dst: flag, obj, name: nidx });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse { cond: flag, target: 0 });
            self.next_reg -= 1;
            self.emit(Instr::DeleteProp { dst, obj, name: nidx, strict: false });
            let je = self.here();
            self.emit(Instr::Jump { target: 0 });
            end_jumps.push(je);
            let nxt = self.here();
            self.patch_jump(jf, nxt);
        }
        // Fallback: the static binding's delete result (no with-object had it).
        let non_configurable = matches!(name, "NaN" | "Infinity" | "undefined")
            || match self.resolve_existing(name) {
                Some(Binding::Local(_))
                | Some(Binding::LocalCell(_))
                | Some(Binding::Upvalue(_))
                | Some(Binding::ClassName(_)) => true,
                Some(Binding::Global(slot)) => {
                    self.cx.hoisted_globals.contains(&slot)
                        || self.cx.lexical_globals.contains(&slot)
                        || self.cx.decl_globals.contains(&slot)
                }
                None => false,
            };
        self.emit(Instr::LoadBool { dst, val: !non_configurable });
        let end = self.here();
        for je in end_jumps {
            self.patch_jump(je, end);
        }
        dst
    }

    /// Emit a read of `binding` into `dst`; returns the register holding the
    /// value (the binding's own register for a plain Local, else `dst`).
    fn load_binding(&mut self, binding: &Binding, dst: Reg) -> Reg {
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
    fn is_self_name_reg(&self, r: Reg) -> bool {
        self.self_name.as_ref().is_some_and(|(_, sr)| *sr == r)
    }

    fn store_binding(&mut self, b: &Binding, src: Reg) {
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
                if self.block_tdz_cells.contains(cell) {
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
    fn tail_call_position(&self) -> bool {
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
    fn tail_callable(&mut self, c: &ox::CallExpression) -> bool {
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
    fn expr_has_tail_call(&mut self, e: &ox::Expression) -> bool {
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
    fn tagged_tail_callable(&mut self, tt: &ox::TaggedTemplateExpression) -> bool {
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
    fn emit_tail_return(&mut self, e: &ox::Expression) -> R<()> {
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
    fn eval_snap_probe(&mut self, binding: &Binding) -> Option<Reg> {
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
    fn store_binding_snapped(&mut self, b: &Binding, src: Reg, snap: Option<Reg>) {
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
    fn bind_params(&mut self, params: &ox::FormalParameters) -> R<()> {
        // Parameter defaults compile inside this call — a direct eval there is
        // in the PARAM scope (see FnCompiler::in_param_init).
        self.in_param_init = true;
        let r = self.bind_params_inner(params);
        self.in_param_init = false;
        r
    }

    fn bind_params_inner(&mut self, params: &ox::FormalParameters) -> R<()> {
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
    fn emit_ident_param_default(&mut self, name: &str, default: &ox::Expression) -> R<()> {
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

    /// Assign `src` to a destructuring-assignment target (existing binding or
    /// member, or a nested array/object pattern). Counterpart to `extract_pattern`
    /// for `=` targets that aren't declarations.
    fn assign_target(&mut self, target: &ox::AssignmentTarget, src: Reg) -> R<()> {
        use ox::AssignmentTarget as T;
        match target {
            T::AssignmentTargetIdentifier(id) => {
                let b = self.resolve(&id.name);
                self.store_binding(&b, src);
                Ok(())
            }
            T::StaticMemberExpression(m) => {
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let name = self.string_name(m.property.name.as_str());
                self.emit(Instr::SetProp { obj, name, val: src });
                self.next_reg = save;
                Ok(())
            }
            T::ComputedMemberExpression(m) => {
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let key = self.expr(&m.expression)?;
                self.emit(Instr::SetIndex { obj, key, val: src });
                self.next_reg = save;
                Ok(())
            }
            T::PrivateFieldExpression(m) => {
                // `[this.#x] = arr` / `({a: this.#x} = o)`: a private field as a
                // destructuring target — brand-checked PrivateSet (the target
                // reference is taken before the value per the destructuring driver).
                self.check_private_declared(&m.field.name)?;
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let name = self.string_name(&private_key(&m.field.name));
                self.emit(Instr::SetPrivate { obj, name, val: src });
                self.next_reg = save;
                Ok(())
            }
            T::ArrayAssignmentTarget(arr) => self.assign_array_target(arr, src),
            T::ObjectAssignmentTarget(o) => self.assign_object_target(o, src),
            _ => Err("unsupported destructuring-assignment target in the zipp-vm subset".into()),
        }
    }

    /// A destructuring MEMBER target's reference, evaluated BEFORE the source
    /// property read (KeyedDestructuringAssignmentEvaluation: the
    /// DestructuringAssignmentTarget reference comes first, then GetV).
    fn pre_member_ref(
        &mut self,
        m: &ox::AssignmentTargetMaybeDefault,
    ) -> R<Option<(Reg, PreKey)>> {
        use ox::AssignmentTargetMaybeDefault as M;
        // Unwrap a `target = default` to its inner target.
        let inner: &ox::AssignmentTarget = match m {
            M::AssignmentTargetWithDefault(d) => &d.binding,
            // The flattened member variants reuse the same node types — handle
            // them via a reconstructed reference below.
            M::StaticMemberExpression(sm) => {
                let r = self.pin_expr(&sm.object)?;
                let name = self.string_name(sm.property.name.as_str());
                return Ok(Some((r, PreKey::Static(name))));
            }
            M::ComputedMemberExpression(cm) => {
                let r = self.pin_expr(&cm.object)?;
                let k = self.pin_expr(&cm.expression)?;
                return Ok(Some((r, PreKey::Computed(k))));
            }
            M::PrivateFieldExpression(pm) => {
                self.check_private_declared(&pm.field.name)?;
                let r = self.pin_expr(&pm.object)?;
                let name = self.string_name(&private_key(&pm.field.name));
                return Ok(Some((r, PreKey::Private(name))));
            }
            _ => return Ok(None),
        };
        use ox::AssignmentTarget as T;
        match inner {
            T::StaticMemberExpression(sm) => {
                let r = self.pin_expr(&sm.object)?;
                let name = self.string_name(sm.property.name.as_str());
                Ok(Some((r, PreKey::Static(name))))
            }
            T::ComputedMemberExpression(cm) => {
                let r = self.pin_expr(&cm.object)?;
                let k = self.pin_expr(&cm.expression)?;
                Ok(Some((r, PreKey::Computed(k))))
            }
            T::PrivateFieldExpression(pm) => {
                self.check_private_declared(&pm.field.name)?;
                let r = self.pin_expr(&pm.object)?;
                let name = self.string_name(&private_key(&pm.field.name));
                Ok(Some((r, PreKey::Private(name))))
            }
            _ => Ok(None),
        }
    }

    /// Evaluate `e` into a PINNED register (survives later evaluation; the
    /// caller's next_reg reset reclaims it).
    fn pin_expr(&mut self, e: &ox::Expression) -> R<Reg> {
        let r = self.alloc_reg();
        let v = self.expr_into(e, r)?;
        if v != r {
            self.emit(Instr::Move { dst: r, src: v });
        }
        Ok(r)
    }

    /// Store through a reference produced by `pre_member_ref`, applying any
    /// `= default` of `m` first (no NamedEvaluation — the target is a member).
    fn store_pre_ref(
        &mut self,
        m: &ox::AssignmentTargetMaybeDefault,
        obj: Reg,
        key: &PreKey,
        val: Reg,
    ) -> R<()> {
        if let ox::AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d) = m {
            self.apply_default_in_place_named(val, &d.init, None)?;
        }
        match *key {
            PreKey::Static(name) => self.emit(Instr::SetProp { obj, name, val }),
            PreKey::Computed(k) => self.emit(Instr::SetIndex { obj, key: k, val }),
            PreKey::Private(name) => self.emit(Instr::SetPrivate { obj, name, val }),
        }
        Ok(())
    }

    /// One element of a destructuring assignment, applying its `= default` first.
    fn assign_maybe_default(
        &mut self,
        m: &ox::AssignmentTargetMaybeDefault,
        val: Reg,
    ) -> R<()> {
        use ox::AssignmentTargetMaybeDefault as M;
        match m {
            M::AssignmentTargetWithDefault(d) => {
                // `[a = function(){}] = arr` ⇒ the default function takes name "a".
                let name = match &d.binding {
                    ox::AssignmentTarget::AssignmentTargetIdentifier(id) => Some(id.name.to_string()),
                    _ => None,
                };
                self.apply_default_in_place_named(val, &d.init, name.as_deref())?;
                self.assign_target(&d.binding, val)
            }
            M::AssignmentTargetIdentifier(id) => {
                let b = self.resolve(&id.name);
                self.store_binding(&b, val);
                Ok(())
            }
            M::StaticMemberExpression(m) => {
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let name = self.string_name(m.property.name.as_str());
                self.emit(Instr::SetProp { obj, name, val });
                self.next_reg = save;
                Ok(())
            }
            M::ComputedMemberExpression(m) => {
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let key = self.expr(&m.expression)?;
                self.emit(Instr::SetIndex { obj, key, val });
                self.next_reg = save;
                Ok(())
            }
            M::PrivateFieldExpression(m) => {
                self.check_private_declared(&m.field.name)?;
                let save = self.next_reg;
                let obj = self.expr(&m.object)?;
                let name = self.string_name(&private_key(&m.field.name));
                self.emit(Instr::SetPrivate { obj, name, val });
                self.next_reg = save;
                Ok(())
            }
            M::ArrayAssignmentTarget(arr) => self.assign_array_target(arr, val),
            M::ObjectAssignmentTarget(o) => self.assign_object_target(o, val),
            _ => Err("unsupported destructuring-assignment element in the zipp-vm subset".into()),
        }
    }

    fn assign_array_target(&mut self, arr: &ox::ArrayAssignmentTarget, src_in: Reg) -> R<()> {
        // Array assignment destructuring: the SPEC's stepwise iterator driver.
        // Per element: evaluate a member target's REFERENCE first, then
        // IteratorStep (no step once exhausted), then the default, then store
        // through the saved reference. An abrupt completion anywhere closes a
        // non-exhausted iterator QUIETLY (the original throw wins); a normal
        // completion closes STRICTLY (a throwing/non-object return() result
        // propagates).
        let save_top = self.next_reg;
        let iter_reg = self.alloc_reg();
        self.emit(Instr::CheckIterable { src: src_in });
        self.emit(Instr::GetIterator { dst: iter_reg, src: src_in });
        let idx_reg = self.alloc_reg();
        self.emit(Instr::LoadInt { dst: idx_reg, val: 0 });
        let done = self.alloc_reg();
        self.emit(Instr::LoadBool { dst: done, val: false });
        // FINALLY-kind protection (not catch): a `yield` inside the pattern
        // can suspend the generator with this iterator OPEN — a later
        // `.return()` injects a RETURN completion that unwinds through
        // finally handlers only, and IteratorClose must run then too.
        let kind_reg = self.alloc_reg();
        let val_reg = self.alloc_reg();
        let push_at = self.here();
        self.emit(Instr::PushFinally { target: 0, kind_reg, val_reg });
        self.handler_depth += 1;
        for el in &arr.elements {
            let save = self.next_reg;
            let pre = match el {
                Some(maybe) => self.pre_member_ref(maybe)?,
                None => None,
            };
            let val = self.alloc_reg();
            let dflag = self.alloc_reg();
            // Step (skipped once exhausted): done elements read undefined.
            // `done` is PRE-SET before the step — an abrupt completion FROM
            // the iterator itself (next()/done-getter/value-getter throw)
            // leaves it true, so the catch path skips IteratorClose (the
            // spec marks [[Done]] before propagating); a successful value
            // clears it.
            let jdone = self.here();
            self.emit(Instr::JumpIfTrue { cond: done, target: 0 });
            self.emit(Instr::LoadBool { dst: done, val: true });
            self.emit(Instr::IterNext { value_dst: val, done_dst: dflag, iter: iter_reg, idx: idx_reg, next: Reg::MAX });
            let jexh = self.here();
            self.emit(Instr::JumpIfTrue { cond: dflag, target: 0 });
            self.emit(Instr::LoadBool { dst: done, val: false });
            let jgot = self.here();
            self.emit(Instr::Jump { target: 0 });
            let at_undef = self.here();
            self.patch_jump(jdone, at_undef);
            self.patch_jump(jexh, at_undef);
            self.emit(Instr::LoadUndefined { dst: val });
            let got = self.here();
            self.patch_jump(jgot, got);
            if let Some(maybe) = el {
                match pre {
                    Some((obj, key)) => self.store_pre_ref(maybe, obj, &key, val)?,
                    None => self.assign_maybe_default(maybe, val)?,
                }
            }
            self.next_reg = save;
        }
        if let Some(rest) = &arr.rest {
            let save = self.next_reg;
            let pre = self.pre_rest_ref(&rest.target)?;
            let out = self.alloc_reg();
            self.emit(Instr::ArrayCtor { dst: out, arg_base: 0, argc: 0 });
            let v = self.alloc_reg();
            let dflag = self.alloc_reg();
            let loop_top = self.here();
            let jrest_done = self.here();
            self.emit(Instr::JumpIfTrue { cond: done, target: 0 });
            self.emit(Instr::LoadBool { dst: done, val: true });
            self.emit(Instr::IterNext { value_dst: v, done_dst: dflag, iter: iter_reg, idx: idx_reg, next: Reg::MAX });
            let jout = self.here();
            self.emit(Instr::JumpIfTrue { cond: dflag, target: 0 });
            self.emit(Instr::LoadBool { dst: done, val: false });
            self.emit(Instr::ArrayAppend { arr: out, val: v, spread: false });
            self.emit(Instr::Jump { target: loop_top });
            let rest_done = self.here();
            self.patch_jump(jrest_done, rest_done);
            self.patch_jump(jout, rest_done);
            match pre {
                Some((obj, key)) => {
                    match key {
                        PreKey::Static(name) => self.emit(Instr::SetProp { obj, name, val: out }),
                        PreKey::Computed(k) => self.emit(Instr::SetIndex { obj, key: k, val: out }),
                        PreKey::Private(name) => self.emit(Instr::SetPrivate { obj, name, val: out }),
                    }
                }
                None => self.assign_target(&rest.target, out)?,
            }
            self.next_reg = save;
        }
        self.emit(Instr::PopFinally);
        self.handler_depth -= 1;
        // Normal completion: close iff not exhausted (strict result checks).
        let jskip = self.here();
        self.emit(Instr::JumpIfTrue { cond: done, target: 0 });
        self.emit(Instr::IterClose { iter: iter_reg });
        let jend = self.here();
        self.emit(Instr::Jump { target: 0 });
        // ABRUPT exits land here (a throw from the pattern, or a RETURN
        // completion unwinding through a `yield` inside it). Exhausted /
        // iterator-own abrupts skip the close; a THROW closes QUIETLY (the
        // original reason wins); a return closes STRICTLY (IteratorClose's
        // own throw / non-object TypeError replaces the completion).
        // EndFinally then resumes whatever completion remains pending.
        let fin_start = self.here();
        if let Instr::PushFinally { target, .. } = &mut self.code[push_at as usize] {
            *target = fin_start;
        }
        let jresume = self.here();
        self.emit(Instr::JumpIfTrue { cond: done, target: 0 });
        let two = self.alloc_reg();
        self.emit(Instr::LoadInt { dst: two, val: 2 });
        let isthrow = self.alloc_reg();
        self.emit(Instr::Eq { dst: isthrow, a: kind_reg, b: two });
        let jnotthrow = self.here();
        self.emit(Instr::JumpIfFalse { cond: isthrow, target: 0 });
        self.emit(Instr::IterCloseQuiet { iter: iter_reg });
        let jresume2 = self.here();
        self.emit(Instr::Jump { target: 0 });
        let at_ret = self.here();
        self.patch_jump(jnotthrow, at_ret);
        self.emit(Instr::IterClose { iter: iter_reg });
        let resume = self.here();
        self.patch_jump(jresume, resume);
        self.patch_jump(jresume2, resume);
        self.emit(Instr::EndFinally { kind_reg, val_reg });
        let end = self.here();
        self.patch_jump(jskip, end);
        self.patch_jump(jend, end);
        self.next_reg = save_top;
        Ok(())
    }

    /// `pre_member_ref` for a REST target (a plain AssignmentTarget).
    fn pre_rest_ref(&mut self, t: &ox::AssignmentTarget) -> R<Option<(Reg, PreKey)>> {
        use ox::AssignmentTarget as T;
        match t {
            T::StaticMemberExpression(sm) => {
                let r = self.pin_expr(&sm.object)?;
                let name = self.string_name(sm.property.name.as_str());
                Ok(Some((r, PreKey::Static(name))))
            }
            T::ComputedMemberExpression(cm) => {
                let r = self.pin_expr(&cm.object)?;
                let k = self.pin_expr(&cm.expression)?;
                Ok(Some((r, PreKey::Computed(k))))
            }
            T::PrivateFieldExpression(pm) => {
                self.check_private_declared(&pm.field.name)?;
                let r = self.pin_expr(&pm.object)?;
                let name = self.string_name(&private_key(&pm.field.name));
                Ok(Some((r, PreKey::Private(name))))
            }
            _ => Ok(None),
        }
    }

    fn assign_object_target(&mut self, o: &ox::ObjectAssignmentTarget, src: Reg) -> R<()> {
        // RequireObjectCoercible(src) for an empty pattern (`({} = x)` /
        // `({...rest} = x)`) — no member access would otherwise guard null/undefined.
        if o.properties.is_empty() {
            self.emit(Instr::CheckCoercible { src });
        }
        // Computed sibling key + `...rest`: evaluate each sibling key once into a
        // contiguous block (reused for extraction and the ObjectRestDyn exclusion).
        let has_computed = o.rest.is_some()
            && o.properties.iter().any(|p| {
                matches!(p, ox::AssignmentTargetProperty::AssignmentTargetPropertyProperty(pp) if pp.computed)
            });
        if has_computed {
            use ox::AssignmentTargetProperty as ATP;
            let block_save = self.next_reg;
            let keys_base = self.next_reg;
            let n = o.properties.len() as u16;
            for _ in 0..o.properties.len() {
                self.alloc_reg();
            }
            for (i, prop) in o.properties.iter().enumerate() {
                let kreg = keys_base + i as Reg;
                match prop {
                    ATP::AssignmentTargetPropertyProperty(p) if p.computed => {
                        let e = p.name.as_expression().ok_or("unsupported computed destructuring key")?;
                        let v = self.expr_into(e, kreg)?;
                        if v != kreg {
                            self.emit(Instr::Move { dst: kreg, src: v });
                        }
                    }
                    ATP::AssignmentTargetPropertyProperty(p) => {
                        let name = class_key_name(&p.name)?;
                        let idx = self.add_string_const(&name);
                        self.emit(Instr::LoadConst { dst: kreg, idx });
                    }
                    ATP::AssignmentTargetPropertyIdentifier(p) => {
                        let idx = self.add_string_const(&p.binding.name);
                        self.emit(Instr::LoadConst { dst: kreg, idx });
                    }
                }
            }
            for (i, prop) in o.properties.iter().enumerate() {
                let save = self.next_reg;
                let kreg = keys_base + i as Reg;
                let val = self.alloc_reg();
                self.emit(Instr::GetIndex { dst: val, obj: src, key: kreg });
                match prop {
                    ATP::AssignmentTargetPropertyIdentifier(p) => {
                        if let Some(init) = &p.init {
                            self.apply_default_in_place_named(val, init, Some(&p.binding.name))?;
                        }
                        let b = self.resolve(&p.binding.name);
                        self.store_binding(&b, val);
                    }
                    ATP::AssignmentTargetPropertyProperty(p) => {
                        self.assign_maybe_default(&p.binding, val)?;
                    }
                }
                self.next_reg = save;
            }
            let rest = o.rest.as_ref().unwrap();
            let save = self.next_reg;
            let val = self.alloc_reg();
            self.emit(Instr::ObjectRestDyn { dst: val, src, keys_base, n });
            self.assign_target(&rest.target, val)?;
            self.next_reg = save;
            self.next_reg = block_save;
            return Ok(());
        }
        for prop in &o.properties {
            let save = self.next_reg;
            match prop {
                ox::AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(p) => {
                    // `({x} = o)` / `({x = d} = o)` — target is the identifier itself.
                    let val = self.alloc_reg();
                    let name = self.string_name(&p.binding.name);
                    self.emit(Instr::GetProp { dst: val, obj: src, name });
                    if let Some(init) = &p.init {
                        // `({x = function(){}} = o)` ⇒ default takes the name "x".
                        self.apply_default_in_place_named(val, init, Some(&p.binding.name))?;
                    }
                    let b = self.resolve(&p.binding.name);
                    self.store_binding(&b, val);
                }
                ox::AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => {
                    // `({key: target} = o)`. For a MEMBER target the spec
                    // order is: PropertyName (incl. ToPropertyKey) -> target
                    // REFERENCE (object + uncoerced key exprs) -> GetV ->
                    // default -> PutValue (target-key coercion at store).
                    let is_member_target = matches!(
                        &p.binding,
                        ox::AssignmentTargetMaybeDefault::StaticMemberExpression(_)
                            | ox::AssignmentTargetMaybeDefault::ComputedMemberExpression(_)
                            | ox::AssignmentTargetMaybeDefault::PrivateFieldExpression(_)
                    ) || matches!(
                        &p.binding,
                        ox::AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(d)
                            if matches!(
                                &d.binding,
                                ox::AssignmentTarget::StaticMemberExpression(_)
                                    | ox::AssignmentTarget::ComputedMemberExpression(_)
                                    | ox::AssignmentTarget::PrivateFieldExpression(_)
                            )
                    );
                    if is_member_target {
                        let skey: Option<Reg> = if p.computed {
                            let e = p
                                .name
                                .as_expression()
                                .ok_or("unsupported computed destructuring key")?;
                            let raw = self.pin_expr(e)?;
                            let k = self.alloc_reg();
                            self.emit(Instr::ToPropKey { dst: k, obj: src, src: raw });
                            Some(k)
                        } else {
                            None
                        };
                        let pre = self.pre_member_ref(&p.binding)?;
                        let val = self.alloc_reg();
                        match skey {
                            Some(k) => self.emit(Instr::GetIndex { dst: val, obj: src, key: k }),
                            None => {
                                let name = class_key_name(&p.name)?;
                                let nidx = self.string_name(&name);
                                self.emit(Instr::GetProp { dst: val, obj: src, name: nidx });
                            }
                        }
                        let (obj, key) = pre.expect("member target shape checked above");
                        self.store_pre_ref(&p.binding, obj, &key, val)?;
                    } else {
                        let val = self.alloc_reg();
                        self.extract_member(src, &p.name, p.computed, val)?;
                        self.assign_maybe_default(&p.binding, val)?;
                    }
                }
            }
            self.next_reg = save;
        }
        // `({a, ...rest} = o)` — a new object of `src`'s own keys minus the
        // siblings, assigned to the rest target (mirrors the declaration form).
        if let Some(rest) = &o.rest {
            let exclude_start = self.string_constants.len() as u32;
            let mut exclude_count = 0u16;
            for prop in &o.properties {
                let key = match prop {
                    ox::AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(p) => {
                        p.binding.name.to_string()
                    }
                    ox::AssignmentTargetProperty::AssignmentTargetPropertyProperty(p) => {
                        class_key_name(&p.name).map_err(|_| {
                            "object-rest with a computed sibling key is not in the subset"
                        })?
                    }
                };
                self.string_name(&key);
                exclude_count += 1;
            }
            let save = self.next_reg;
            let val = self.alloc_reg();
            self.emit(Instr::ObjectRest { dst: val, src, exclude_start, exclude_count });
            self.assign_target(&rest.target, val)?;
            self.next_reg = save;
        }
        Ok(())
    }

    /// For a logical assignment (`||= &&= ??=`), emit the short-circuit test on
    /// `val` (which already holds the target's current value) and return the ip
    /// of the jump that, when taken, SKIPS the assignment (keeping `val`).
    fn emit_logical_skip(&mut self, op: ox::AssignmentOperator, val: Reg) -> u32 {
        use ox::AssignmentOperator as Op;
        match op {
            Op::LogicalOr => {
                let j = self.here();
                self.emit(Instr::JumpIfTrue { cond: val, target: 0 }); // truthy → skip
                j
            }
            Op::LogicalAnd => {
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond: val, target: 0 }); // falsy → skip
                j
            }
            _ => {
                // ??= : skip when `val` is NOT strictly null/undefined.
                let save = self.next_reg;
                let undef = self.alloc_reg();
                let isnull = self.alloc_reg();
                self.emit_is_nullish(val, isnull, undef);
                let j = self.here();
                self.emit(Instr::JumpIfFalse { cond: isnull, target: 0 });
                self.next_reg = save;
                j
            }
        }
    }

    fn assign(&mut self, a: &ox::AssignmentExpression, dst: Reg) -> R<Reg> {
        use ox::AssignmentOperator as Op;
        let is_logical =
            matches!(a.operator, Op::LogicalOr | Op::LogicalAnd | Op::LogicalNullish);
        // Member-target assignment: `obj.x = v` / `arr[i] = v`. Only plain
        // `=` is supported for members in this subset.
        match &a.left {
            // `super.x = v` / `super.x op= v` / `super.x ??= v`.
            ox::AssignmentTarget::StaticMemberExpression(m)
                if matches!(&m.object, ox::Expression::Super(_)) =>
            {
                let pid = self.super_class;
                if pid.is_none() && !self.super_home_obj {
                    return Err("`super.x = …` is only valid in a method".into());
                }
                self.this_check();
                let name = self.string_name(m.property.name.as_str());
                // A super GET/SET routes to the class op (home_class_id) or the
                // object-method op ([[HomeObject]]), depending on the lexical context.
                let emit_get = |s: &mut Self, d: Reg| match pid {
                    Some(p) => s.emit(Instr::SuperGet { dst: d, home_class_id: p, name }),
                    None => s.emit(Instr::SuperGetObj { dst: d, name }),
                };
                let emit_set = |s: &mut Self, v: Reg| match pid {
                    Some(p) => s.emit(Instr::SuperSet { home_class_id: p, name, val: v }),
                    None => s.emit(Instr::SuperSetObj { name, val: v }),
                };
                if is_logical {
                    emit_get(self, dst);
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    emit_set(self, dst);
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    emit_set(self, dst);
                } else {
                    let cur = self.temp();
                    emit_get(self, cur);
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    emit_set(self, dst);
                }
                return Ok(dst);
            }
            ox::AssignmentTarget::StaticMemberExpression(m) => {
                let obj = self.expr(&m.object)?; // evaluate the receiver once
                let name = self.string_name(m.property.name.as_str());
                if is_logical {
                    // `obj.x ??= v` etc: read current; skip the store on short-circuit.
                    self.emit(Instr::GetProp { dst, obj, name });
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    self.emit(Instr::SetProp { obj, name, val: dst });
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    self.emit(Instr::SetProp { obj, name, val: dst });
                } else {
                    // Compound `obj.x op= v`: read obj.x, combine, write back.
                    let cur = self.temp();
                    self.emit(Instr::GetProp { dst: cur, obj, name });
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    self.emit(Instr::SetProp { obj, name, val: dst });
                }
                return Ok(dst);
            }
            // `obj.#x = v` / `obj.#x op= v` — same as a static member, keyed "#x".
            ox::AssignmentTarget::PrivateFieldExpression(p) => {
                self.check_private_declared(&p.field.name)?;
                let obj = self.expr(&p.object)?;
                let name = self.string_name(&private_key(&p.field.name));
                if is_logical {
                    self.emit(Instr::GetProp { dst, obj, name });
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    self.emit(Instr::SetProp { obj, name, val: dst });
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    self.emit(Instr::SetPrivate { obj, name, val: dst });
                } else {
                    let cur = self.temp();
                    self.emit(Instr::GetProp { dst: cur, obj, name });
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    self.emit(Instr::SetPrivate { obj, name, val: dst });
                }
                return Ok(dst);
            }
            // `super[k] = v` / compound / logical.
            ox::AssignmentTarget::ComputedMemberExpression(m)
                if matches!(&m.object, ox::Expression::Super(_)) =>
            {
                let pid = self.super_class;
                if pid.is_none() && !self.super_home_obj {
                    return Err("`super[k] = …` is only valid in a method".into());
                }
                self.this_check();
                let key = self.expr(&m.expression)?;
                let key_reg = self.alloc_reg();
                if key != key_reg {
                    self.emit(Instr::Move { dst: key_reg, src: key });
                }
                let emit_get = |s: &mut Self, d: Reg| match pid {
                    Some(p) => s.emit(Instr::SuperGetComputed { dst: d, home_class_id: p, key: key_reg }),
                    None => s.emit(Instr::SuperGetObjComputed { dst: d, key: key_reg }),
                };
                let emit_set = |s: &mut Self, v: Reg| match pid {
                    Some(p) => s.emit(Instr::SuperSetComputed { home_class_id: p, key: key_reg, val: v }),
                    None => s.emit(Instr::SuperSetObjComputed { key: key_reg, val: v }),
                };
                if is_logical {
                    emit_get(self, dst);
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    emit_set(self, dst);
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    emit_set(self, dst);
                } else {
                    let cur = self.temp();
                    emit_get(self, cur);
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    emit_set(self, dst);
                }
                return Ok(dst);
            }
            ox::AssignmentTarget::ComputedMemberExpression(m) => {
                let obj = self.expr(&m.object)?; // evaluate receiver + key once
                let key = self.expr(&m.expression)?;
                if is_logical {
                    // A read-modify-write reuses the SAME property key for the load
                    // and the store: coerce ToPropertyKey ONCE (its toString/valueOf
                    // must not run twice).
                    let keyk = self.temp();
                    self.emit(Instr::ToPropKey { dst: keyk, obj, src: key });
                    self.emit(Instr::GetIndex { dst, obj, key: keyk });
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    self.emit(Instr::SetIndex { obj, key: keyk, val: dst });
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    // A plain store coerces the key once (the single SetIndex).
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    self.emit(Instr::SetIndex { obj, key, val: dst });
                } else {
                    let keyk = self.temp();
                    self.emit(Instr::ToPropKey { dst: keyk, obj, src: key });
                    let cur = self.temp();
                    self.emit(Instr::GetIndex { dst: cur, obj, key: keyk });
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    self.emit(Instr::SetIndex { obj, key: keyk, val: dst });
                }
                return Ok(dst);
            }
            // Destructuring assignment to existing targets: `[a,b]=arr`, `({x}=o)`.
            ox::AssignmentTarget::ArrayAssignmentTarget(_)
            | ox::AssignmentTarget::ObjectAssignmentTarget(_) => {
                let src = self.expr_into(&a.right, dst)?;
                if src != dst {
                    self.emit(Instr::Move { dst, src });
                }
                self.assign_target(&a.left, dst)?;
                return Ok(dst);
            }
            _ => {}
        }
        let (name, lhs_covered) = match &a.left {
            ox::AssignmentTarget::AssignmentTargetIdentifier(id) => (
                id.name.to_string(),
                // oxc strips cover parens but keeps spans: a target starting
                // AFTER the assignment expression was parenthesized — not an
                // IdentifierRef, so NamedEvaluation does not apply.
                id.span.start > a.span.start,
            ),
            _ => return Err("assignment to non-identifier not in zipp-vm v1".into()),
        };
        // Strict mode: assignment to `eval`/`arguments` is an early SyntaxError.
        strict_name_err(self.cx.in_strict, &name)?;
        // Inside a `with`, an assignment target may be a property of an active
        // with-object (innermost first), else the static binding.
        let with_objs = self.with_obj_regs(&name);
        if !with_objs.is_empty() {
            return self.assign_with(a, &name, &with_objs, dst);
        }
        let binding = self.resolve(&name);
        match a.operator {
            Op::Assign => {
                // `x = function(){}` / `x = class {}` names the anonymous value
                // after the target (NamedEvaluation), like a declaration.
                // A const local takes the store_binding path so the RHS is evaluated
                // (side effects) and the assignment then throws a TypeError.
                if let Binding::Local(r) = binding {
                    if !self.const_regs.contains(&r) && !self.is_self_name_reg(r) {
                        // Plain mutable local: evaluate the RHS directly into its reg.
                        let v = if lhs_covered {
                            self.expr_into(&a.right, r)?
                        } else {
                            self.compile_named_init(r, &a.right, &name)?
                        };
                        if v != r {
                            self.emit(Instr::Move { dst: r, src: v });
                        }
                        if r != dst {
                            self.emit(Instr::Move { dst, src: r });
                        }
                        return Ok(dst);
                    }
                }
                // Cell / upvalue / global / const-local: evaluate into dst, store.
                // The target reference resolves BEFORE the RHS (a direct eval
                // there may introduce a shadowing `var` — snapshot first).
                let save_p = self.next_reg;
                let snap = self.eval_snap_probe(&binding);
                let v = if lhs_covered {
                    self.expr_into(&a.right, dst)?
                } else {
                    self.compile_named_init(dst, &a.right, &name)?
                };
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.store_binding_snapped(&binding, dst, snap);
                self.next_reg = save_p;
                Ok(dst)
            }
            // Logical assignment: `x ||= y` / `x &&= y` / `x ??= y` only assign
            // `y` when the short-circuit condition holds (truthy-skip for ||=,
            // falsy-skip for &&=, non-nullish-skip for ??=).
            Op::LogicalOr | Op::LogicalAnd | Op::LogicalNullish => {
                let save_p = self.next_reg;
                let snap = self.eval_snap_probe(&binding);
                let cur = self.load_binding(&binding, dst);
                if cur != dst {
                    self.emit(Instr::Move { dst, src: cur });
                }
                let j = self.emit_logical_skip(a.operator, dst);
                // NamedEvaluation: `x ||= function(){}` / `&&=` / `??=` names the
                // anonymous fn/arrow/class after the identifier LHS (IsIdentifierRef),
                // matching plain `=`. `compile_named_init` no-ops to `expr_into` for a
                // non-anonymous RHS, so a named/expression RHS is unaffected.
                let v = self.compile_named_init(dst, &a.right, &name)?;
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.store_binding_snapped(&binding, dst, snap);
                let end = self.here();
                self.patch_jump(j, end);
                self.next_reg = save_p;
                Ok(dst)
            }
            // Arithmetic / bitwise compound assignment (`+= -= *= /= %= **= <<=
            // >>= >>>= |= ^= &=`).
            other => {
                if let Binding::Local(r) = binding {
                    if !self.const_regs.contains(&r) && !self.is_self_name_reg(r) {
                        // Plain mutable local: compute in place.
                        let rhs = self.expr(&a.right)?;
                        let instr = compound_assign_instr(other, r, r, rhs)
                            .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                        self.emit(instr);
                        if r != dst {
                            self.emit(Instr::Move { dst, src: r });
                        }
                        return Ok(dst);
                    }
                }
                // Cell / upvalue / global / const-local: load current → dst, compute
                // → dst, store back through the binding (store_binding throws for a
                // const, after the RHS + arithmetic side effects). The reference
                // is resolved (snapshotted) before the RHS, like plain `=`.
                let save_p = self.next_reg;
                let snap = self.eval_snap_probe(&binding);
                let cur = self.load_binding(&binding, dst); // == dst
                let rhs = self.expr(&a.right)?;
                let instr = compound_assign_instr(other, dst, cur, rhs)
                    .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                self.emit(instr);
                self.store_binding_snapped(&binding, dst, snap);
                self.next_reg = save_p;
                Ok(dst)
            }
        }
    }

    /// Assignment to a plain identifier inside a `with` body, where `objs`
    /// (innermost first) may shadow the static binding. Mirrors the identifier
    /// branch of `assign`, routing the read/write through `load_with`/`store_with`.
    fn assign_with(
        &mut self,
        a: &ox::AssignmentExpression,
        name: &str,
        objs: &[Reg],
        dst: Reg,
    ) -> R<Reg> {
        use ox::AssignmentOperator as Op;
        match a.operator {
            Op::Assign => {
                // The REFERENCE resolves before the RHS runs (which with-object,
                // if any, holds the binding); PutValue writes through that
                // snapshot even if the RHS deletes the with-object property.
                let (found, tgt) = self.emit_with_probe(name, objs);
                let v = self.compile_named_init(dst, &a.right, name)?;
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.emit_with_rmw_write(name, found, tgt, dst);
                Ok(dst)
            }
            Op::LogicalOr | Op::LogicalAnd | Op::LogicalNullish => {
                // Resolve the reference ONCE (which with-object, if any, holds the
                // binding), then read and write through that same target — even if
                // a getter run by the read mutates the object meanwhile.
                let (found, tgt) = self.emit_with_probe(name, objs);
                self.emit_with_rmw_read(name, found, tgt, dst);
                let j = self.emit_logical_skip(a.operator, dst);
                let v = self.compile_named_init(dst, &a.right, name)?;
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.emit_with_rmw_write(name, found, tgt, dst);
                let end = self.here();
                self.patch_jump(j, end);
                Ok(dst)
            }
            other => {
                // Compound `x op= y` in a `with`: PutValue reuses the Reference from
                // GetValue, so the write targets the same object the read used even
                // when the getter deletes/replaces the property in between.
                let (found, tgt) = self.emit_with_probe(name, objs);
                self.emit_with_rmw_read(name, found, tgt, dst); // current value → dst
                let rhs = self.expr(&a.right)?;
                let instr = compound_assign_instr(other, dst, dst, rhs)
                    .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                self.emit(instr);
                self.emit_with_rmw_write(name, found, tgt, dst);
                Ok(dst)
            }
        }
    }

    /// Emit a runtime `with`-target probe for a read-modify-write of `name`: find
    /// the innermost with-object that HAS the binding, recording whether one
    /// matched (`found`, a bool reg) and which it is (`tgt`). The reference is
    /// resolved ONCE so a later read and write target the SAME object even if a
    /// getter mutates that object's properties in between (spec: PutValue reuses
    /// the Reference produced by reference resolution). The two returned registers
    /// stay live across the caller's read, RHS evaluation, and write.
    fn emit_with_probe(&mut self, name: &str, objs: &[Reg]) -> (Reg, Reg) {
        let nidx = self.string_name(name);
        let found = self.alloc_reg();
        let tgt = self.alloc_reg();
        self.emit(Instr::LoadBool { dst: found, val: false });
        self.emit(Instr::LoadUndefined { dst: tgt });
        let mut done = Vec::new();
        for &obj in objs {
            let flag = self.temp();
            self.emit(Instr::WithHas { dst: flag, obj, name: nidx });
            let jf = self.here();
            self.emit(Instr::JumpIfFalse { cond: flag, target: 0 });
            self.next_reg -= 1; // reclaim the flag temp
            self.emit(Instr::LoadBool { dst: found, val: true });
            self.emit(Instr::Move { dst: tgt, src: obj });
            let jd = self.here();
            self.emit(Instr::Jump { target: 0 });
            done.push(jd);
            let nxt = self.here();
            self.patch_jump(jf, nxt);
        }
        let end = self.here();
        for jd in done {
            self.patch_jump(jd, end);
        }
        (found, tgt)
    }

    /// Read `name` for a with read-modify-write: `dst = tgt.name` when a with-object
    /// matched (`found`), else read the static (lexical/global) binding into `dst`.
    fn emit_with_rmw_read(&mut self, name: &str, found: Reg, tgt: Reg, dst: Reg) {
        let nidx = self.string_name(name);
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond: found, target: 0 }); // → static read
        // GetBindingValue: HasProperty AGAIN, then Get (both observable
        // through Proxy traps) — not a bare [[Get]].
        self.emit(Instr::WithGet { dst, obj: tgt, name: nidx, strict: self.cx.in_strict });
        let je = self.here();
        self.emit(Instr::Jump { target: 0 });
        let stat = self.here();
        self.patch_jump(jf, stat);
        let b = self.resolve(name);
        let r = self.load_binding(&b, dst);
        if r != dst {
            self.emit(Instr::Move { dst, src: r });
        }
        let end = self.here();
        self.patch_jump(je, end);
    }

    /// Write `src` to `name` for a with read-modify-write: `tgt.name = src` when a
    /// with-object matched (`found`), else store to the static binding.
    fn emit_with_rmw_write(&mut self, name: &str, found: Reg, tgt: Reg, src: Reg) {
        let nidx = self.string_name(name);
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond: found, target: 0 }); // → static write
        // SetMutableBinding re-checks HasProperty (the binding may have been
        // DELETED since the reference resolved): strict throws, sloppy does
        // not silently recreate the property on the with-object.
        self.emit(Instr::WithSet { obj: tgt, name: nidx, val: src, strict: self.cx.in_strict });
        let je = self.here();
        self.emit(Instr::Jump { target: 0 });
        let stat = self.here();
        self.patch_jump(jf, stat);
        let b = self.resolve(name);
        self.store_binding(&b, src);
        let end = self.here();
        self.patch_jump(je, end);
    }

    fn yield_expr(&mut self, y: &ox::YieldExpression, dst: Reg) -> R<Reg> {
        if !self.in_generator {
            return Err("`yield` is only valid inside a generator (function*)".into());
        }
        if y.delegate {
            let arg = y.argument.as_ref().ok_or("yield* requires an operand")?;
            if self.in_async {
                // ASYNC `yield*` (delegation inside an `async function*`): drive the
                // operand's ASYNC iterator, awaiting each step exactly like the working
                // `for await` codegen, and async-yield each value; the `yield*`
                // expression evaluates to the inner iterator's final `{value}` once it
                // is done. Uses only existing, proven ops (GetAsyncIterator /
                // ForAwaitNext / Await / Yield) — no VM change, and no ip-0 suspension
                // so the iter-170 resume-delivery hazard does not apply.
                //
                // SCOPE (minimal slice): next-OUT delegation + completion value + async
                // iterator acquisition + error propagation (an abrupt operand / inner
                // next() unwinds the async-gen activation, rejecting its front promise).
                // NOT yet: forwarding the value sent to the OUTER .next(v) into the
                // inner iterator (ForAwaitNext calls next() with no arg, so the sent
                // value is discarded into `sink`), nor delegating the outer
                // .throw()/.return() into the inner iterator (those force-complete the
                // async gen) — both are a separate, larger follow-up.
                let save = self.next_reg;
                // All registers are allocated once and kept stable: the whole window is
                // saved/restored across each suspension, and the non-linear control flow
                // (the throw-delegation catch jumps back into the loop) makes
                // per-iteration reclaim unsafe.
                let iter = self.alloc_reg();
                let v = self.expr_into(arg, iter)?;
                if v != iter {
                    self.emit(Instr::Move { dst: iter, src: v });
                }
                // Whether the INNER iterator is a sync one (the @@iterator
                // fallback / a raw array): its values get the AsyncFromSync
                // await-unwrap before each yield.
                let is_sync = self.alloc_reg();
                self.emit(Instr::GetAsyncIterator { dst: iter, src: iter, sync_dst: is_sync });
                let idx = self.alloc_reg();
                self.emit(Instr::LoadInt { dst: idx, val: 0 });
                // Cache the inner iterator's `next` ONCE (IteratorRecord.[[NextMethod]]),
                // matching the spec's get-next-once ordering for a user iterator.
                let next_fn = self.alloc_reg();
                let next_name = self.string_name("next");
                self.emit(Instr::GetProp { dst: next_fn, obj: iter, name: next_name });
                let excr = self.alloc_reg(); // catch reg for an injected outer .throw()
                let cerr = self.alloc_reg(); // abrupt unwrap completion (close-on-rejection)
                let step = self.alloc_reg();
                let r = self.alloc_reg();
                let done = self.alloc_reg();
                let value = self.alloc_reg();
                // The value sent to the OUTER `.next(v)` — forwarded into the inner
                // iterator's next() each step (initially undefined). The
                // AsyncYieldDelegate resume writes the new sent value here.
                let sent = self.alloc_reg();
                self.emit(Instr::LoadUndefined { dst: sent });
                let tstep = self.alloc_reg();
                let taw = self.alloc_reg();
                let mode = self.alloc_reg(); // resume mode from AsyncYieldDelegate: 0 next / 2 return
                let hasret = self.alloc_reg(); // does the inner iterator have a `return`?
                let done_name = self.string_name("done");
                let value_name = self.string_name("value");
                // --- one next(sent) step: r = await iter.next(sent); require Object ---
                let top = self.here();
                self.emit(Instr::AsyncIterNextStep { dst: step, iter, idx, sent, next_fn });
                self.emit(Instr::Await { dst: r, val: step });
                self.emit(Instr::RequireObject { val: r });
                self.emit(Instr::GetProp { dst: done, obj: r, name: done_name });
                let jdone = self.here();
                self.emit(Instr::JumpIfTrue { cond: done, target: 0 }); // done → yield* value (r.value)
                self.emit(Instr::GetProp { dst: value, obj: r, name: value_name });
                // AsyncFromSyncIterator unwrap (AsyncFromSyncIteratorContinuation,
                // closeOnRejection = true): a SYNC inner iterator's stepped value
                // is AWAITED before the async-yield; an abrupt unwrap — the Await's
                // observable PromiseResolve (a poisoned `constructor`) or a
                // rejected value-promise — first does IteratorClose(inner) QUIETLY,
                // then re-throws the ORIGINAL reason (closing the generator and
                // rejecting the front promise). An ASYNC inner iterator's value is
                // yielded as-is (yield-star-promise-not-unwrapped). The
                // throw-delegation's not-done path re-enters here at `unwrap_pt`.
                let unwrap_pt = self.here();
                let jskip_aw = self.here();
                self.emit(Instr::JumpIfFalse { cond: is_sync, target: 0 });
                let ph_aw = self.here();
                self.emit(Instr::PushHandler { catch_target: 0, catch_reg: cerr });
                self.handler_depth += 1;
                self.emit(Instr::Await { dst: value, val: value });
                self.emit(Instr::PopHandler);
                self.handler_depth -= 1;
                let jaw_ok = self.here();
                self.emit(Instr::Jump { target: 0 });
                let close_catch = self.here();
                if let Instr::PushHandler { catch_target, .. } = &mut self.code[ph_aw as usize] {
                    *catch_target = close_catch;
                }
                self.emit(Instr::IterCloseQuiet { iter });
                self.emit(Instr::Throw { src: cerr });
                let after_aw = self.here();
                self.patch_jump(jskip_aw, after_aw);
                self.patch_jump(jaw_ok, after_aw);
                // --- (async-)yield the value, with a handler that delegates an outer
                //     .throw() into the inner iterator's `throw` ---
                let yield_pt = self.here();
                let ph = self.here();
                self.emit(Instr::PushHandler { catch_target: 0, catch_reg: excr });
                self.handler_depth += 1;
                // Suspend; resume delivers (mode, value) into (mode, sent).
                self.emit(Instr::AsyncYieldDelegate { mode_dst: mode, val_dst: sent, val: value });
                self.emit(Instr::PopHandler);
                self.handler_depth -= 1;
                // mode 2 (return) → return-delegation; mode 0 (next, falsy) → loop.
                let jret = self.here();
                self.emit(Instr::JumpIfTrue { cond: mode, target: 0 });
                self.emit(Instr::Jump { target: top });
                // --- return delegation: outer .return(sent). Delegate to inner.return;
                //     no method → outer returns `sent`; else await, then finish-return
                //     (done) or yield the value and continue. ---
                let ret_label = self.here();
                self.patch_jump(jret, ret_label);
                self.emit(Instr::AsyncIterReturnStep { dst: tstep, has_dst: hasret, iter, ret: sent });
                let jhas = self.here();
                self.emit(Instr::JumpIfTrue { cond: hasret, target: 0 });
                // No inner `return` method: the received return value is AWAITED
                // (spec: if return is undefined and generatorKind is async, set
                // received.[[Value]] to ? Await(received.[[Value]])) before the
                // generator returns it — a thenable is adopted (observable `then`
                // read + one tick), and a rejection unwinds instead.
                self.emit(Instr::Await { dst: sent, val: sent });
                self.emit(Instr::Return { src: sent });
                let has_ret = self.here();
                self.patch_jump(jhas, has_ret);
                self.emit(Instr::Await { dst: taw, val: tstep });
                self.emit(Instr::RequireObject { val: taw });
                self.emit(Instr::GetProp { dst: done, obj: taw, name: done_name });
                let jretdone = self.here();
                self.emit(Instr::JumpIfTrue { cond: done, target: 0 }); // inner.return done → generator returns value
                self.emit(Instr::GetProp { dst: value, obj: taw, name: value_name });
                self.emit(Instr::Jump { target: yield_pt }); // not done → yield value, continue
                let ret_done = self.here();
                self.patch_jump(jretdone, ret_done);
                self.emit(Instr::GetProp { dst: value, obj: taw, name: value_name });
                // A SYNC inner's `return()` result value is unwrapped by the
                // AsyncFromSync continuation (closeOnRejection = false here):
                // await it before completing (iterator-result-unwrap-promise).
                let jrv = self.here();
                self.emit(Instr::JumpIfFalse { cond: is_sync, target: 0 });
                self.emit(Instr::Await { dst: value, val: value });
                let after_rv = self.here();
                self.patch_jump(jrv, after_rv);
                self.emit(Instr::Return { src: value }); // generator returns inner.return's value
                // --- catch: an outer .throw(excr) was injected here. Delegate to the
                //     inner iterator's throw (or TypeError if it has none), await the
                //     result, then either finish (done) or yield the delegated value. ---
                let catch_label = self.here();
                if let Instr::PushHandler { catch_target, .. } = &mut self.code[ph as usize] {
                    *catch_target = catch_label;
                }
                self.emit(Instr::AsyncIterThrowStep { dst: tstep, iter, exc: excr });
                self.emit(Instr::Await { dst: taw, val: tstep });
                self.emit(Instr::RequireObject { val: taw });
                self.emit(Instr::GetProp { dst: done, obj: taw, name: done_name });
                let jdone2 = self.here();
                self.emit(Instr::JumpIfTrue { cond: done, target: 0 }); // inner.throw done → value
                self.emit(Instr::GetProp { dst: value, obj: taw, name: value_name });
                // Not done: a SYNC inner's delegated value takes the same
                // close-on-rejection unwrap as a next() step (the AsyncFromSync
                // throw() continuation also has closeOnRejection = true), then
                // the yield at `yield_pt`.
                self.emit(Instr::Jump { target: unwrap_pt });
                // done via inner.throw: yield* value = taw.value (awaited for a
                // SYNC inner — the continuation unwraps it; done == true means
                // no close-on-rejection, a plain await).
                let done_label2 = self.here();
                self.patch_jump(jdone2, done_label2);
                self.emit(Instr::GetProp { dst, obj: taw, name: value_name });
                let jtv = self.here();
                self.emit(Instr::JumpIfFalse { cond: is_sync, target: 0 });
                self.emit(Instr::Await { dst, val: dst });
                let after_tv = self.here();
                self.patch_jump(jtv, after_tv);
                let jend = self.here();
                self.emit(Instr::Jump { target: 0 });
                // done via next(): yield* value = r.value.
                let done_label = self.here();
                self.patch_jump(jdone, done_label);
                self.emit(Instr::GetProp { dst, obj: r, name: value_name });
                let end = self.here();
                self.patch_jump(jend, end);
                self.next_reg = save;
                return Ok(dst);
            }
            // SYNC `yield*` — the full delegation protocol (spec 14.4.14 step 5):
            // GetIterator, then a loop that drives the inner iterator per the OUTER
            // generator's resume mode (next/throw/return), forwarding sent values,
            // `gen.throw()`, and `gen.return()`. `IterDelegate` performs one step
            // (incl. the missing-method rules) and classifies the outcome;
            // `YieldDelegate` yields and on resume delivers (mode, value).
            let save = self.next_reg;
            let iter = self.alloc_reg();
            let v = self.expr_into(arg, iter)?;
            if v != iter {
                self.emit(Instr::Move { dst: iter, src: v });
            }
            self.emit(Instr::GetIteratorObj { dst: iter, src: iter });
            let mode = self.alloc_reg();
            self.emit(Instr::LoadInt { dst: mode, val: 0 }); // start as a `next`
            let sent = self.alloc_reg();
            self.emit(Instr::LoadUndefined { dst: sent });
            let value = self.alloc_reg();
            let done = self.alloc_reg();
            let ret = self.alloc_reg();
            let top = self.here();
            self.emit(Instr::IterDelegate {
                value_dst: value,
                done_dst: done,
                ret_dst: ret,
                iter,
                mode,
                sent,
            });
            let jret = self.here();
            self.emit(Instr::JumpIfTrue { cond: ret, target: 0 }); // → generator return
            let jdone = self.here();
            self.emit(Instr::JumpIfTrue { cond: done, target: 0 }); // → yield* completes
            // Neither: yield the value; on resume (mode,sent) are delivered, loop.
            self.emit(Instr::YieldDelegate { mode_dst: mode, val_dst: sent, val: value });
            self.emit(Instr::Jump { target: top });
            // ret: the inner iterator's return (or a missing one) ends the generator.
            let ret_label = self.here();
            self.patch_jump(jret, ret_label);
            self.emit(Instr::Return { src: value });
            // done: the `yield*` expression evaluates to the inner's final value.
            let done_label = self.here();
            self.patch_jump(jdone, done_label);
            if dst != value {
                self.emit(Instr::Move { dst, src: value });
            }
            self.next_reg = save;
            return Ok(dst);
        }
        // Evaluate the yielded value (undefined for a bare `yield`); on resume
        // the value passed to `.next(v)` lands in `dst`.
        let val = match &y.argument {
            Some(e) => self.expr(e)?,
            None => {
                let t = self.temp();
                self.emit(Instr::LoadUndefined { dst: t });
                t
            }
        };
        // In an ASYNC generator, `yield v` is AsyncGeneratorYield: it first AWAITs the
        // value (so `yield Promise.reject(e)` rejects the consumer's `.next()`, and
        // `yield Promise.resolve(x)` yields the unwrapped `x`), then suspends. The
        // Await and the Yield are distinct suspension points, each resumed
        // independently, so the resume `.next(v)` value still lands in `dst`.
        if self.in_async {
            let awaited = self.temp();
            self.emit(Instr::Await { dst: awaited, val });
            self.emit(Instr::Yield { dst, val: awaited });
        } else {
            self.emit(Instr::Yield { dst, val });
        }
        Ok(dst)
    }

    fn await_expr(&mut self, a: &ox::AwaitExpression, dst: Reg) -> R<Reg> {
        if !self.in_async {
            return Err("`await` is only valid inside an async function".into());
        }
        // Evaluate the awaited value; on resume the settled result (or a thrown
        // rejection) lands in `dst`. The VM coerces non-promises via Promise.resolve.
        let val = self.expr(&a.argument)?;
        self.emit(Instr::Await { dst, val });
        Ok(dst)
    }

    fn conditional(&mut self, c: &ox::ConditionalExpression, dst: Reg) -> R<Reg> {
        let cond = self.expr(&c.test)?;
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 });
        let t = self.expr_into(&c.consequent, dst)?;
        if t != dst {
            self.emit(Instr::Move { dst, src: t });
        }
        let jmp = self.here();
        self.emit(Instr::Jump { target: 0 });
        let else_start = self.here();
        self.patch_jump(jf, else_start);
        let e = self.expr_into(&c.alternate, dst)?;
        if e != dst {
            self.emit(Instr::Move { dst, src: e });
        }
        let end = self.here();
        self.patch_jump(jmp, end);
        Ok(dst)
    }

    /// Build an Error-like object `{ name, message }` for `Error("msg")` (called
    /// either with `new` or bare). `arg` is the optional message argument.
    /// Emit `NewRegExp` for `RegExp(pattern?, flags?)` / `new RegExp(...)` — the
    /// VM coerces a string/RegExp pattern and the flags (undefined → defaults).
    fn emit_regexp(&mut self, args: &[ox::Argument], dst: Reg, is_construct: bool) -> R<Reg> {
        let pt = self.temp();
        match args.first().and_then(|a| a.as_expression()) {
            Some(e) => {
                let v = self.expr_into(e, pt)?;
                if v != pt {
                    self.emit(Instr::Move { dst: pt, src: v });
                }
            }
            None => self.emit(Instr::LoadUndefined { dst: pt }),
        }
        let ft = self.temp();
        match args.get(1).and_then(|a| a.as_expression()) {
            Some(e) => {
                let v = self.expr_into(e, ft)?;
                if v != ft {
                    self.emit(Instr::Move { dst: ft, src: v });
                }
            }
            None => self.emit(Instr::LoadUndefined { dst: ft }),
        }
        self.emit(Instr::NewRegExp { dst, pattern: pt, flags: ft, is_construct });
        self.next_reg -= 2;
        Ok(dst)
    }

    fn build_error(&mut self, kind: &str, args: &[ox::Argument], dst: Reg) -> R<Reg> {
        // `new TypeError(msg)` etc. → a proto-linked error instance (NewError op
        // sets own `name`/`message` and links the prototype so `.constructor`,
        // `.toString`, and `instanceof` resolve). AggregateError takes the message
        // as its SECOND argument (`new AggregateError(errors, message)`).
        let kidx = error_kind_index(kind);
        let msg_pos = if kidx == 7 { 1 } else { 0 };
        // AggregateError's first argument is the `errors` iterable. Evaluate it FIRST
        // (matching left-to-right argument evaluation); the NewError op IterableToList's
        // it into a non-enumerable own `errors` array, AFTER coercing the message.
        let errors = if kidx == 7 {
            match args.first().and_then(|a| a.as_expression()) {
                Some(e) => {
                    let t = self.temp();
                    let v = self.expr_into(e, t)?;
                    if v != t {
                        self.emit(Instr::Move { dst: t, src: v });
                    }
                    Some(t)
                }
                None => None,
            }
        } else {
            None
        };
        let arg = match args.get(msg_pos).and_then(|a| a.as_expression()) {
            Some(e) => {
                let t = self.temp();
                let v = self.expr_into(e, t)?;
                if v != t {
                    self.emit(Instr::Move { dst: t, src: v });
                }
                Some(t)
            }
            None => None,
        };
        // The options object follows the message (`new Error(msg, options)`,
        // `new AggregateError(errors, msg, options)`) — its `cause` becomes the
        // error's `cause` (NewError installs it).
        let opts = match args.get(msg_pos + 1).and_then(|a| a.as_expression()) {
            Some(e) => {
                let t = self.temp();
                let v = self.expr_into(e, t)?;
                if v != t {
                    self.emit(Instr::Move { dst: t, src: v });
                }
                Some(t)
            }
            None => None,
        };
        self.emit(Instr::NewError { dst, kind: kidx, arg, opts, errors });
        // Reclaim the errors + message + options temps (allocated in that order).
        self.next_reg -=
            errors.is_some() as Reg + arg.is_some() as Reg + opts.is_some() as Reg;
        Ok(dst)
    }

    fn call(&mut self, c: &ox::CallExpression, dst: Reg) -> R<Reg> {
        // Optional call `f?.(args)` — EvaluateCall: a MEMBER callee (even
        // through parens) preserves its base as `this`; `super.m?.()` binds the
        // running `this`. The base's own `?.` links short-circuit inside the
        // chain, and a nullish callee bails to undefined WITHOUT evaluating
        // the arguments.
        if c.optional {
            let mut inner: &ox::Expression = &c.callee;
            while let ox::Expression::ParenthesizedExpression(p) = inner {
                inner = &p.expression;
            }
            let has_spread =
                c.arguments.iter().any(|a| matches!(a, ox::Argument::SpreadElement(_)));
            match inner {
                ox::Expression::StaticMemberExpression(m)
                    if !matches!(&m.object, ox::Expression::Super(_)) && !has_spread =>
                {
                    let o = self.expr(&m.object)?;
                    let obj = self.alloc_reg();
                    if o != obj {
                        self.emit(Instr::Move { dst: obj, src: o });
                    }
                    if m.optional {
                        self.emit_optional_check(obj);
                    }
                    let name = self.string_name(m.property.name.as_str());
                    let callee = self.alloc_reg();
                    self.emit(Instr::GetProp { dst: callee, obj, name });
                    self.emit_optional_check(callee);
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::CallWithThis { dst, callee, this_v: obj, arg_base, argc });
                    return Ok(dst);
                }
                ox::Expression::ComputedMemberExpression(m)
                    if !matches!(&m.object, ox::Expression::Super(_)) && !has_spread =>
                {
                    let o = self.expr(&m.object)?;
                    let obj = self.alloc_reg();
                    if o != obj {
                        self.emit(Instr::Move { dst: obj, src: o });
                    }
                    if m.optional {
                        self.emit_optional_check(obj);
                    }
                    let key = self.expr(&m.expression)?;
                    let callee = self.alloc_reg();
                    self.emit(Instr::GetIndex { dst: callee, obj, key });
                    self.emit_optional_check(callee);
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::CallWithThis { dst, callee, this_v: obj, arg_base, argc });
                    return Ok(dst);
                }
                ox::Expression::PrivateFieldExpression(p) if !has_spread => {
                    self.check_private_declared(&p.field.name)?;
                    let o = self.expr(&p.object)?;
                    let obj = self.alloc_reg();
                    if o != obj {
                        self.emit(Instr::Move { dst: obj, src: o });
                    }
                    if p.optional {
                        self.emit_optional_check(obj);
                    }
                    let name = self.string_name(&private_key(&p.field.name));
                    let callee = self.alloc_reg();
                    self.emit(Instr::GetProp { dst: callee, obj, name });
                    self.emit_optional_check(callee);
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::CallWithThis { dst, callee, this_v: obj, arg_base, argc });
                    return Ok(dst);
                }
                // `super.m?.()` / `super[k]?.()`: the member lowering performs
                // the read (incl. the this-TDZ check); `this` is frame reg 0.
                ox::Expression::StaticMemberExpression(_)
                | ox::Expression::ComputedMemberExpression(_)
                    if !has_spread =>
                {
                    let callee = self.expr(inner)?;
                    self.emit_optional_check(callee);
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::CallWithThis { dst, callee, this_v: 0, arg_base, argc });
                    return Ok(dst);
                }
                // `(a?.b)?.()`: a parenthesized-chain member callee still
                // binds `this` = base; the inner chain's bail lands the
                // callee at undefined, then the outer `?.()` bails on it.
                ox::Expression::ChainExpression(ce) if !has_spread => {
                    if let Some((callee, obj)) = self.chain_member_callee(ce)? {
                        self.emit_optional_check(callee);
                        let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                        self.emit(Instr::CallWithThis { dst, callee, this_v: obj, arg_base, argc });
                        return Ok(dst);
                    }
                }
                _ => {}
            }
            let callee = self.expr(&c.callee)?;
            self.emit_optional_check(callee);
            if has_spread {
                // `fn?.(...xs)`: spread args after the nullish bail.
                let args_arr = self.build_spread_args(&c.arguments)?;
                self.emit(Instr::CallSpread { dst, callee, args: args_arr });
                return Ok(dst);
            }
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::Call { dst, callee, arg_base, argc });
            return Ok(dst);
        }
        // Spread call: `f(...args)`, `obj.m(...args)`, `arr.push(...xs)`, etc.
        // Build the argument list as an array (spreading each `...x` element),
        // then dispatch via CallMethodSpread (method receiver) or CallSpread
        // (plain function value). Spread on a builtin like Math.max(...arr) that
        // isn't a method call is out of scope.
        if c.arguments.iter().any(|a| matches!(a, ox::Argument::SpreadElement(_))) {
            // `super(...args)` — spread into the superclass constructor. Handled
            // here (before the generic branches) because `super` is not a value
            // and would fail `expr(callee)`.
            if matches!(&c.callee, ox::Expression::Super(_)) {
                if !self.derived_class {
                    return Err("`super(...)` is only valid in a derived class constructor".into());
                }
                let pid = self
                    .super_class
                    .ok_or("`super(...)` is only valid in a derived class constructor")?;
                let args_arr = self.build_spread_args(&c.arguments)?;
                self.emit(Instr::SuperCtorSpread { home_class_id: pid, args: args_arr });
                // `super(...)` evaluates to the new bound `this` (BindThisValue's
                // result) — SuperCtorSpread rebinds reg 0 to it (call-expr-value).
                self.emit(Instr::Move { dst, src: 0 });
                return Ok(dst);
            }
            // `Math.max(...arr)` / `Math.min(...arr)` / `Math.hypot(...arr)` —
            // a variadic Math reduction over the spread array.
            if let ox::Expression::StaticMemberExpression(m) = &c.callee {
                if let ox::Expression::Identifier(obj) = &m.object {
                    if obj.name == "Math" {
                        if let Some(op) = crate::bytecode::MathFn::from_name(m.property.name.as_str()) {
                            let args_arr = self.build_spread_args(&c.arguments)?;
                            self.emit(Instr::MathSpread { dst, op, args: args_arr });
                            return Ok(dst);
                        }
                    }
                }
            }
            // Direct `eval(...args)`: a spread call of the unshadowed global
            // `eval` is STILL a direct eval — the spread list's first element
            // is the code argument (extras are ignored, like eval(a, b)).
            if let ox::Expression::Identifier(id) = &c.callee {
                if id.name == "eval"
                    && matches!(self.resolve("eval"), Binding::Global(_))
                    && self.with_objs_for("eval").is_empty()
                {
                    let args_arr = self.build_spread_args(&c.arguments)?;
                    let arg = self.alloc_reg();
                    let zero = self.alloc_reg();
                    self.emit(Instr::LoadInt { dst: zero, val: 0 });
                    self.emit(Instr::GetIndex { dst: arg, obj: args_arr, key: zero });
                    self.emit_direct_eval(arg, dst, false);
                    return Ok(dst);
                }
            }
            // `super.m(...args)` — a StaticMemberExpression whose object is `super`
            // (which is not a value, so it must be handled before the generic
            // StaticMember case evaluates the object).
            if let ox::Expression::StaticMemberExpression(m) = &c.callee {
                if matches!(&m.object, ox::Expression::Super(_)) {
                    let pid = self
                        .super_class
                        .ok_or("`super.method(...)` is only valid in a derived class")?;
                    self.this_check();
                    let name = self.string_name(m.property.name.as_str());
                    let args_arr = self.build_spread_args(&c.arguments)?;
                    self.emit(Instr::SuperMethodSpread { dst, home_class_id: pid, name, args: args_arr });
                    return Ok(dst);
                }
            }
            // Method call `obj.m(...)` — evaluate the receiver first so `this`
            // binds correctly, then build args, then CallMethodSpread.
            if let ox::Expression::StaticMemberExpression(m) = &c.callee {
                let obj = self.expr(&m.object)?;
                if m.optional {
                    self.emit_optional_check(obj);
                }
                let name = self.string_name(m.property.name.as_str());
                let args_arr = self.build_spread_args(&c.arguments)?;
                self.emit(Instr::CallMethodSpread { dst, obj, name, args: args_arr });
                return Ok(dst);
            }
            // `super[key](...args)` — computed super member with spread args
            // (must precede the generic computed arm: `super` is not a value).
            if let ox::Expression::ComputedMemberExpression(m) = &c.callee {
                if matches!(&m.object, ox::Expression::Super(_)) {
                    let pid = self
                        .super_class
                        .ok_or("`super[...](...)` is only valid in a derived class")?;
                    self.this_check();
                    let key = self.expr(&m.expression)?;
                    let args_arr = self.build_spread_args(&c.arguments)?;
                    self.emit(Instr::SuperMethodComputedSpread {
                        dst,
                        home_class_id: pid,
                        key,
                        args: args_arr,
                    });
                    return Ok(dst);
                }
            }
            // Computed method call `obj[key](...)` — bind `this` = obj (a plain
            // CallSpread on the GET result would lose the receiver).
            if let ox::Expression::ComputedMemberExpression(m) = &c.callee {
                let obj = self.expr(&m.object)?;
                if m.optional {
                    self.emit_optional_check(obj);
                }
                let key = self.expr(&m.expression)?;
                let args_arr = self.build_spread_args(&c.arguments)?;
                self.emit(Instr::CallMethodComputedSpread { dst, obj, key, args: args_arr });
                return Ok(dst);
            }
            let callee = self.expr(&c.callee)?;
            let args_arr = self.build_spread_args(&c.arguments)?;
            self.emit(Instr::CallSpread { dst, callee, args: args_arr });
            return Ok(dst);
        }

        // `super(args)` — run the superclass constructor on the current `this`.
        if matches!(&c.callee, ox::Expression::Super(_)) {
            if !self.derived_class {
                return Err("`super(...)` is only valid in a derived class constructor".into());
            }
            let pid = self
                .super_class
                .ok_or("`super(...)` is only valid in a derived class constructor")?;
            // (Spread `super(...args)` is handled in the spread block above.)
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::SuperCtor { home_class_id: pid, arg_base, argc });
            // `super()` evaluates to the new bound `this` (BindThisValue's
            // result) — SuperCtor rebinds reg 0 to it (call-expr-value).
            self.emit(Instr::Move { dst, src: 0 });
            return Ok(dst);
        }
        // `super.method(args)` — call an inherited method with the current `this`.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if matches!(&m.object, ox::Expression::Super(_)) {
                self.this_check();
                let name = self.string_name(m.property.name.as_str());
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                if let Some(pid) = self.super_class {
                    self.emit(Instr::SuperMethod { dst, home_class_id: pid, name, arg_base, argc });
                } else if self.super_home_obj {
                    self.emit(Instr::SuperMethodObj { dst, name, arg_base, argc });
                } else {
                    return Err("`super.method(...)` is only valid in a method".into());
                }
                return Ok(dst);
            }
        }

        // Inside a `with`, a bare-identifier call resolves through the with
        // chain FIRST — before any builtin special-case lowering, so
        // `with (proxy) { Object() }` fires the `has` trap exactly like a
        // user-function callee. The probe (HasBinding) and the read
        // (GetBindingValue: HasProperty + Get) are both observable; a
        // with-resolved callee is invoked with `this` = the with-object
        // (WithBaseObject). Names no with-object carries fall back to the
        // static binding with `this` = undefined (the dedicated builtin
        // lowerings stay bypassed — the loaded global builtin VALUE is
        // called instead, same semantics).
        if let ox::Expression::Identifier(id) = &c.callee {
            let with_objs = self.with_obj_regs(id.name.as_str());
            if !with_objs.is_empty() {
                let save = self.next_reg;
                let (callee_reg, this_reg) =
                    self.emit_with_callee_chain(id.name.as_str(), &with_objs);
                // Arguments evaluate AFTER the callee reference resolved.
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                self.emit(Instr::CallWithThis {
                    dst,
                    callee: callee_reg,
                    this_v: this_reg,
                    arg_base,
                    argc,
                });
                self.next_reg = save.max(dst + 1);
                return Ok(dst);
            }
        }

        // Bare `Error("msg")` call (no `new`) → same Error object.
        if let ox::Expression::Identifier(id) = &c.callee {
            if let Some(kind) = error_ctor(&id.name) {
                return self.build_error(kind, &c.arguments, dst);
            }
        }
        // Direct `eval(code)`: the evaluated string runs with the caller's
        // strictness, `this`, new.target permission, home class and private
        // scope. Fires only for the unshadowed global `eval` — an enclosing
        // user binding named `eval` is an ordinary call, and inside a `with`
        // whose object could shadow `eval` the generic dynamic path is kept.
        // The DISPATCH arm re-checks at runtime that the live global `eval`
        // still IS %eval% (a rebound `eval` gets an ordinary call). Indirect
        // eval stays on the generic `Call` → `GLOBAL_EVAL` native.
        if let ox::Expression::Identifier(id) = &c.callee {
            if id.name == "eval"
                && matches!(self.resolve("eval"), Binding::Global(_))
                && self.with_objs_for("eval").is_empty()
            {
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                let arg = if argc == 0 {
                    let r = self.temp();
                    self.emit(Instr::LoadUndefined { dst: r });
                    r
                } else {
                    arg_base
                };
                self.emit_direct_eval(arg, dst, false);
                return Ok(dst);
            }
        }
        // `Symbol(desc?)` → a fresh Symbol primitive (MakeSymbol op). `Symbol` is
        // not constructable, so only the call form is lowered here.
        if let ox::Expression::Identifier(id) = &c.callee {
            if id.name == "Symbol" {
                let desc = match c.arguments.first().and_then(|a| a.as_expression()) {
                    Some(e) => {
                        let t = self.temp();
                        let v = self.expr_into(e, t)?;
                        if v != t {
                            self.emit(Instr::Move { dst: t, src: v });
                        }
                        Some(t)
                    }
                    None => None,
                };
                self.emit(Instr::MakeSymbol { dst, desc });
                if desc.is_some() {
                    self.next_reg -= 1;
                }
                return Ok(dst);
            }
        }
        // `RegExp(pattern?, flags?)` (no `new`) → like `new RegExp(...)`, except a
        // RegExp pattern with no flags + a RegExp `constructor` returns it unchanged
        // (is_construct: false signals the runtime short-circuit).
        if let ox::Expression::Identifier(id) = &c.callee {
            if id.name == "RegExp" {
                return self.emit_regexp(&c.arguments, dst, false);
            }
        }
        // `BigInt(x)` → conversion (BigIntFrom op). No arg → undefined (→ TypeError
        // at runtime, matching the spec).
        if let ox::Expression::Identifier(id) = &c.callee {
            if id.name == "BigInt" {
                let t = self.temp();
                match c.arguments.first().and_then(|a| a.as_expression()) {
                    Some(e) => {
                        let v = self.expr_into(e, t)?;
                        if v != t {
                            self.emit(Instr::Move { dst: t, src: v });
                        }
                    }
                    None => self.emit(Instr::LoadUndefined { dst: t }),
                }
                self.emit(Instr::BigIntFrom { dst, arg: t });
                self.next_reg -= 1;
                return Ok(dst);
            }
        }
        // Number(x) / parseInt(s,radix) / parseFloat(s) → GlobalFn op.
        if let ox::Expression::Identifier(id) = &c.callee {
            if let Some(op) = crate::bytecode::GlobalFn::from_name(&id.name) {
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                self.emit(Instr::GlobalFn { dst, op, arg_base, argc });
                return Ok(dst);
            }
            // Bare `Array(…)` / `Object()` behave like their `new` forms.
            if id.name == "Array" {
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                self.emit(Instr::ArrayCtor { dst, arg_base, argc });
                return Ok(dst);
            }
            if id.name == "Object" {
                // `Object()` → a fresh object; `Object(x)` → ToObject(x).
                if let Some(arg) = c.arguments.first().and_then(|a| a.as_expression()) {
                    let src = self.expr(arg)?;
                    self.emit(Instr::ToObject { dst, src });
                } else {
                    self.emit(Instr::NewObject { dst });
                }
                return Ok(dst);
            }
        }

        // console.log(...) → Print opcode.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "console"
                    && matches!(m.property.name.as_str(), "log" | "info" | "warn" | "error" | "debug")
                {
                    // node routes console.error / console.warn to stderr.
                    let to_stderr = matches!(m.property.name.as_str(), "error" | "warn");
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::Print { arg_base, argc, to_stderr });
                    self.emit(Instr::LoadUndefined { dst });
                    return Ok(dst);
                }
            }
        }

        // Host `print(...)` → Print to stdout. JS shells (and the test262
        // `doneprintHandle.js` harness, which calls `print('Test262:Async…')` to
        // signal async-test completion) expect it as a global. Yield to a lexical
        // `print` binding (local/param/upvalue) if the program defined one.
        if let ox::Expression::Identifier(id) = &c.callee {
            if id.name == "print"
                && !matches!(
                    self.resolve("print"),
                    Binding::Local(_) | Binding::LocalCell(_) | Binding::Upvalue(_)
                )
            {
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                self.emit(Instr::Print { arg_base, argc, to_stderr: false });
                self.emit(Instr::LoadUndefined { dst });
                return Ok(dst);
            }
        }

        // Clock idioms: `performance.now()` and `Date.now()` → Now opcode. They
        // have no real global object in the subset, so recognise the call shape.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                let epoch = match (obj.name.as_str(), m.property.name.as_str()) {
                    ("performance", "now") => Some(false),
                    ("Date", "now") => Some(true),
                    _ => None,
                };
                if let Some(epoch) = epoch {
                    self.emit(Instr::Now { dst, epoch });
                    return Ok(dst);
                }
                // `Date.UTC(y, m0, …)` → ms; `Date.parse(str)` → ms.
                if obj.name == "Date" && m.property.name == "UTC" {
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::DateUTC { dst, arg_base, argc });
                    return Ok(dst);
                }
                if obj.name == "Date" && m.property.name == "parse" && c.arguments.len() == 1 {
                    if let Some(ae) = c.arguments[0].as_expression() {
                        let src = self.expr(ae)?;
                        self.emit(Instr::DateParse { dst, src });
                        return Ok(dst);
                    }
                }
            }
        }

        // `JSON.parse(text)` / `JSON.stringify(value)` → fast ops. The forms with
        // a reviver / replacer (2+ args) fall through to the generic call so the
        // `JSON_PARSE` / `JSON_STRINGIFY` natives can honour them.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "JSON" && m.property.name == "parse" && c.arguments.len() == 1 {
                    if let Some(ae) = c.arguments[0].as_expression() {
                        let a = self.expr(ae)?;
                        self.emit(Instr::JsonParse { dst, a });
                        return Ok(dst);
                    }
                }
                if obj.name == "JSON" && m.property.name == "stringify" && c.arguments.len() == 1 {
                    if let Some(ve) = c.arguments[0].as_expression() {
                        let val = self.expr(ve)?;
                        let space = self.temp();
                        self.emit(Instr::LoadUndefined { dst: space });
                        self.emit(Instr::JsonStringify { dst, val, space });
                        return Ok(dst);
                    }
                }
            }
        }

        // `Array.isArray(x)` → IsArray op.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "Array" && m.property.name == "isArray" && c.arguments.len() == 1 {
                    if let Some(arg_expr) = c.arguments[0].as_expression() {
                        let a = self.expr(arg_expr)?;
                        self.emit(Instr::IsArray { dst, a });
                        return Ok(dst);
                    }
                }
            }
        }

        // `Object.keys/values/entries(o)` → dedicated ops (Object has no real
        // global object in the subset).
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "Object" && c.arguments.len() == 1 {
                    let mk = match m.property.name.as_str() {
                        "keys" => Some(0u8),
                        "values" => Some(1u8),
                        "entries" => Some(2u8),
                        _ => None,
                    };
                    if let Some(kind) = mk {
                        if let Some(arg_expr) = c.arguments[0].as_expression() {
                            let o = self.expr(arg_expr)?;
                            self.emit(match kind {
                                0 => Instr::ObjectKeys { dst, obj: o },
                                1 => Instr::ObjectValues { dst, obj: o },
                                _ => Instr::ObjectEntries { dst, obj: o },
                            });
                            return Ok(dst);
                        }
                    }
                }
            }
        }

        // `Math.<fn>(args…)` → MathOp. Math has no real global object in the
        // subset, so recognise the call shape (like console.log / Date.now).
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if obj.name == "Math" {
                    if m.property.name == "random" {
                        self.emit(Instr::Random { dst });
                        return Ok(dst);
                    }
                    if let Some(op) = crate::bytecode::MathFn::from_name(m.property.name.as_str()) {
                        let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                        self.emit(Instr::MathOp { dst, op, arg_base, argc });
                        return Ok(dst);
                    }
                }
            }
        }

        // Constructor-namespace static methods with a flat arg list:
        // Object.assign, Array.of, String.fromCharCode, Number.isInteger/… .
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if let ox::Expression::Identifier(obj) = &m.object {
                if let Some(op) =
                    crate::bytecode::StaticFn::from_name(&obj.name, m.property.name.as_str())
                {
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::StaticFn { dst, op, arg_base, argc });
                    return Ok(dst);
                }
                // `Array.from(src[, mapFn])` — needs iteration + optional callback.
                // Only the 1-/2-arg form is lowered; a 3rd `thisArg` falls through
                // to the general call (the native honours it).
                if obj.name == "Array"
                    && m.property.name == "from"
                    && (1..=2).contains(&c.arguments.len())
                {
                    if let Some(se) = c.arguments[0].as_expression() {
                        let save = self.next_reg;
                        let src = self.expr(se)?;
                        let mapfn = self.temp();
                        match c.arguments.get(1).and_then(|a| a.as_expression()) {
                            Some(fe) => {
                                let f = self.expr_into(fe, mapfn)?;
                                if f != mapfn {
                                    self.emit(Instr::Move { dst: mapfn, src: f });
                                }
                            }
                            None => self.emit(Instr::LoadUndefined { dst: mapfn }),
                        }
                        self.emit(Instr::ArrayFrom { dst, src, mapfn });
                        self.next_reg = save.max(dst + 1);
                        return Ok(dst);
                    }
                }
            }
        }

        // A PARENTHESIZED member callee preserves its reference — `(a.b)()` and
        // `(a?.b)()` call with `this` = a (parens are transparent to
        // EvaluateCall; only a comma/assignment breaks the reference).
        let mut peeled: &ox::Expression = &c.callee;
        while let ox::Expression::ParenthesizedExpression(p) = peeled {
            peeled = &p.expression;
        }
        // `(a?.b)(args)`: a parenthesized-CHAIN member callee — the chain ends
        // at the parens (nullish base → undefined callee → the call throws),
        // but the member base still binds `this`.
        if let ox::Expression::ChainExpression(ce) = peeled {
            if !c.arguments.iter().any(|a| matches!(a, ox::Argument::SpreadElement(_))) {
                if let Some((callee, obj)) = self.chain_member_callee(ce)? {
                    let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                    self.emit(Instr::CallWithThis { dst, callee, this_v: obj, arg_base, argc });
                    return Ok(dst);
                }
            }
        }
        // Method call `obj.name(args…)` → CallMethod, binding `this` to obj.
        // (Computed-member calls `obj[k](…)` fall through to the generic path.)
        if let ox::Expression::StaticMemberExpression(m) = peeled {
            let obj = self.expr(&m.object)?;
            if m.optional {
                self.emit_optional_check(obj); // `obj?.method()` — short-circuit if obj nullish
            } else if matches!(
                &m.object,
                ox::Expression::StaticMemberExpression(_)
                    | ox::Expression::ComputedMemberExpression(_)
                    | ox::Expression::PrivateFieldExpression(_)
            ) {
                // `o.bar.gar(args)`: the callee GET (`.gar` of a possibly-
                // nullish `o.bar`) throws BEFORE the arguments evaluate (spec
                // EvaluateCall: func = GetValue(ref) precedes ArgumentList-
                // Evaluation). The fused CallMethod defers the get past the
                // args, so reject a nullish receiver here. Kept off simple
                // identifier/this receivers (hot path; their get cannot throw
                // earlier than the fused op observes).
                self.emit(Instr::CheckCoercible { src: obj });
            }
            let name = self.string_name(m.property.name.as_str());
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::CallMethod { dst, obj, name, arg_base, argc });
            return Ok(dst);
        }
        // Private method call `obj.#m(args…)` → CallMethod on the "#m" key.
        if let ox::Expression::PrivateFieldExpression(p) = peeled {
            self.check_private_declared(&p.field.name)?;
            let obj = self.expr(&p.object)?;
            let name = self.string_name(&private_key(&p.field.name));
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::CallMethod { dst, obj, name, arg_base, argc });
            return Ok(dst);
        }

        // Computed method call `obj[key](args…)` → bind `this` to obj. Evaluate
        // `super[expr](args…)` — computed inherited-method call.
        if let ox::Expression::ComputedMemberExpression(m) = peeled {
            if matches!(&m.object, ox::Expression::Super(_)) {
                let is_class = self.super_class;
                if is_class.is_none() && !self.super_home_obj {
                    return Err("`super[x](...)` is only valid in a method".into());
                }
                self.this_check();
                let key = self.expr(&m.expression)?;
                let key_reg = self.alloc_reg();
                if key != key_reg {
                    self.emit(Instr::Move { dst: key_reg, src: key });
                }
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                if let Some(pid) = is_class {
                    self.emit(Instr::SuperMethodComputed { dst, home_class_id: pid, key: key_reg, arg_base, argc });
                } else {
                    self.emit(Instr::SuperMethodObjComputed { dst, key: key_reg, arg_base, argc });
                }
                return Ok(dst);
            }
        }
        // obj and the key into stable registers (below the contiguous arg block).
        if let ox::Expression::ComputedMemberExpression(m) = peeled {
            let obj = self.expr(&m.object)?;
            let obj_reg = self.alloc_reg();
            if obj != obj_reg {
                self.emit(Instr::Move { dst: obj_reg, src: obj });
            }
            if m.optional {
                self.emit_optional_check(obj_reg);
            }
            let key = self.expr(&m.expression)?;
            let key_reg = self.alloc_reg();
            if key != key_reg {
                self.emit(Instr::Move { dst: key_reg, src: key });
            }
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::CallMethodComputed { dst, obj: obj_reg, key: key_reg, arg_base, argc });
            return Ok(dst);
        }

        // (Bare-identifier calls inside a `with` were already routed through the
        // with chain near the top of this function — before the builtin
        // special-cases — so nothing with-related remains here.)

        // General call: evaluate callee, then contiguous args.
        let callee = self.expr(&c.callee)?;
        let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
        self.emit(Instr::Call { dst, callee, arg_base, argc });
        Ok(dst)
    }

    /// Evaluate call arguments into a contiguous run of registers and return
    /// (first register, count). The run MUST be contiguous because the
    /// `Call`/`Print`/`NewArray` opcodes address args as
    /// `[arg_base, arg_base+argc)`.
    ///
    /// Correctness subtlety: evaluating one argument may itself allocate scratch
    /// temps (e.g. `a[i]` evaluates `a` and `i` into temps). Those temps must
    /// NOT land inside the still-unfilled argument slots. So we reserve the
    /// whole block first (bumping `next_reg` past it), which forces per-arg
    /// temps to allocate ABOVE the block; we then reclaim them after each arg.
    /// Emit the `DirectEval` op for a direct `eval(<arg>)` call site:
    /// builds the visible-caller-bindings site map and the instruction.
    /// `tail`: the site is a proper-tail-call return position, so a REBOUND
    /// `eval` (ordinary call at runtime) reuses the frame.
    fn emit_direct_eval(&mut self, arg: Reg, dst: Reg, tail: bool) {
                // The visible caller bindings (boxed cells, innermost shadowing
        // first) — the eval program closes over them. An EVAL ROOT also
        // maps its own cell locals AND its seeded caller upvalues (kind
        // 1), so nested evals keep reaching the original caller scope.
        let eval_root = self.is_script && self.cx.eval_locals;
        let site = if self.box_all_locals || eval_root || self.script_eval_lexicals {
            let mut map: Vec<(String, u8, u16)> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for scope in self.scopes.iter().rev() {
                for (n, r) in scope.iter().rev() {
                    if self.cell_regs.contains(r) && seen.insert(n.clone()) {
                        map.push((n.clone(), 0u8, *r));
                    }
                }
            }
            if let Some((n, r)) = self.self_name.clone() {
                if self.cell_regs.contains(&r) && seen.insert(n.clone()) {
                    map.push((n, 0u8, r));
                }
            }
            if eval_root {
                let ups: Vec<String> =
                    self.upvalues.borrow().iter().map(|(n, _)| n.clone()).collect();
                for (i, n) in ups.iter().enumerate() {
                    if seen.insert(n.clone()) {
                        map.push((n.clone(), 1u8, i as u16));
                    }
                }
            }
            // In a parameter default, the eval's sloppy var/function
            // names may not collide with the PARAM scope (the params
            // and, for non-arrows, the implicit `arguments`).
            let param_collisions = if self.in_param_init {
                let mut names = self.param_names.clone();
                if self.arguments_reg.is_some()
                    && !names.iter().any(|n| n == "arguments")
                {
                    names.push("arguments".to_string());
                }
                Some(names)
            } else {
                None
            };
            // LEXICAL caller bindings visible here (the live scope stack
            // at this call site gives the correct block nesting).
            let mut lex: Vec<String> = Vec::new();
            for scope in self.scopes.iter().rev() {
                for (n, r) in scope.iter().rev() {
                    if self.lexical_regs.contains(r) && !lex.iter().any(|e| e == n) {
                        lex.push(n.clone());
                    }
                }
            }
            let s = self.eval_sites.len() as u16;
            self.eval_sites.push((map, param_collisions, lex));
            s
        } else {
            u16::MAX
        };
        // The effective `this` to inherit: a static field initializer holds
        // it in `this_override`, otherwise it is reg 0.
        let this_reg = self.this_override.unwrap_or(0);
        self.emit(Instr::DirectEval {
            dst,
            arg,
            new_target_ok: self.cx.new_target_ok,
            this_reg,
            home_class: self.super_class.unwrap_or(u32::MAX),
            super_static: self.super_static,
            ban_arguments: self.cx.in_field_init,
            strict_caller: self.cx.in_strict,
            super_home_obj: self.super_home_obj,
            // The eval's variable environment: GLOBAL only when the
            // call site is the script top level (a function/arrow/param
            // context keeps the old slot behavior until the dynamic
            // caller-env lands).
            var_env_is_global: self.is_script,
            site,
            tail,
        });
    }

    fn eval_args_contiguous(
        &mut self,
        args: &oxc_allocator::Vec<ox::Argument>,
    ) -> R<(Reg, u16)> {
        let exprs: Vec<&ox::Expression> = args
            .iter()
            .map(|a| {
                a.as_expression()
                    .ok_or_else(|| "spread arguments are not in the zipp-vm subset yet".to_string())
            })
            .collect::<R<Vec<_>>>()?;
        let base = self.eval_contiguous(&exprs)?;
        Ok((base, exprs.len() as u16))
    }

    /// Build a call-argument list containing `...spread` into a fresh array and
    /// return its (live) register. Each plain arg is pushed as one element; each
    /// `...x` arg appends every element of `x` (an array, or a string's chars).
    /// Consumed by `CallSpread` / `CallMethodSpread`.
    fn build_spread_args(&mut self, args: &oxc_allocator::Vec<ox::Argument>) -> R<Reg> {
        let args_arr = self.temp();
        self.emit(Instr::NewArray { dst: args_arr, arg_base: self.next_reg, argc: 0 });
        for a in args {
            let save = self.next_reg;
            match a {
                ox::Argument::SpreadElement(s) => {
                    let v = self.expr(&s.argument)?;
                    self.emit(Instr::ArrayAppend { arr: args_arr, val: v, spread: true });
                }
                other => {
                    let e = other.as_expression().ok_or("unsupported spread-call argument")?;
                    let v = self.expr(e)?;
                    self.emit(Instr::ArrayAppend { arr: args_arr, val: v, spread: false });
                }
            }
            self.next_reg = save;
        }
        Ok(args_arr)
    }

    /// Evaluate `exprs` into the contiguous register block `[base, base+len)`,
    /// reclaiming each expression's scratch temps. Returns `base`.
    fn eval_contiguous(&mut self, exprs: &[&ox::Expression]) -> R<Reg> {
        let base = self.next_reg;
        // Reserve the block up front so arg-evaluation temps allocate above it.
        for _ in exprs {
            self.alloc_reg();
        }
        let block_top = self.next_reg;
        for (i, e) in exprs.iter().enumerate() {
            let slot = base + i as Reg;
            let v = self.expr_into(e, slot)?;
            if v != slot {
                self.emit(Instr::Move { dst: slot, src: v });
            }
            // Reclaim temps this argument used (everything above the block).
            self.next_reg = block_top;
        }
        Ok(base)
    }
}

enum Binding {
    /// Plain register-resident local (the fast path; no capture).
    Local(Reg),
    /// Local that has been boxed into a heap cell because a nested function
    /// captures it; the register holds the cell reference.
    LocalCell(Reg),
    /// A variable captured from an enclosing function: index into this
    /// function's upvalue list.
    Upvalue(u16),
    Global(u32),
    /// The inner immutable class-name binding inside a class body (resolved at
    /// runtime from `class_values[class_id]`); read-only — assignment is a
    /// TypeError. Visible to methods/ctor/static-blocks and arrows within them.
    ClassName(u32),
}

/// The instruction for an arithmetic/bitwise compound assignment (`dst = a <op>
/// b`). `None` for `=` and the logical-assignment operators (handled separately).
fn compound_assign_instr(op: ox::AssignmentOperator, dst: Reg, a: Reg, b: Reg) -> Option<Instr> {
    use ox::AssignmentOperator as Op;
    Some(match op {
        Op::Addition => Instr::Add { dst, a, b },
        Op::Subtraction => Instr::Sub { dst, a, b },
        Op::Multiplication => Instr::Mul { dst, a, b },
        Op::Division => Instr::Div { dst, a, b },
        Op::Remainder => Instr::Mod { dst, a, b },
        Op::Exponential => Instr::Pow { dst, a, b },
        Op::ShiftLeft => Instr::Bitwise { dst, a, b, op: BitwiseOp::Shl },
        Op::ShiftRight => Instr::Bitwise { dst, a, b, op: BitwiseOp::Shr },
        Op::ShiftRightZeroFill => Instr::Bitwise { dst, a, b, op: BitwiseOp::Ushr },
        Op::BitwiseOR => Instr::Bitwise { dst, a, b, op: BitwiseOp::Or },
        Op::BitwiseXOR => Instr::Bitwise { dst, a, b, op: BitwiseOp::Xor },
        Op::BitwiseAnd => Instr::Bitwise { dst, a, b, op: BitwiseOp::And },
        _ => return None,
    })
}

/// The string of an `export`/`import` ModuleExportName (`foo`, `foo as bar`,
/// `"a-b"`), for recording a module's (exported, local) export pairs.
fn module_export_name(n: &ox::ModuleExportName) -> String {
    match n {
        ox::ModuleExportName::IdentifierName(id) => id.name.to_string(),
        ox::ModuleExportName::IdentifierReference(id) => id.name.to_string(),
        ox::ModuleExportName::StringLiteral(s) => string_literal_key(s),
    }
}

/// A string-literal PROPERTY KEY's text. Property keys are Rust `String`s
/// engine-wide (`ObjMap.keys`), which cannot hold a lone surrogate — a
/// `.lone_surrogates` key decodes LOSSILY (each lone surrogate → U+FFFD, so
/// two distinct lone-surrogate keys collide). Documented stage-2 limit.
fn string_literal_key(s: &ox::StringLiteral) -> String {
    if s.lone_surrogates {
        crate::heap::wtf8_into_lossy_string(crate::heap::decode_lone_surrogate_markers(
            s.value.as_str(),
        ))
    } else {
        s.value.to_string()
    }
}

/// A class member's (non-computed) name. Computed `[expr]` and `#private` names
/// are out of the subset.
fn class_key_name(key: &ox::PropertyKey) -> R<String> {
    match key {
        ox::PropertyKey::StaticIdentifier(id) => Ok(id.name.to_string()),
        ox::PropertyKey::StringLiteral(s) => Ok(string_literal_key(s)),
        ox::PropertyKey::NumericLiteral(n) => Ok(fmt_key_num(n.value)),
        // A BigInt key's property name is its base-10 value string.
        ox::PropertyKey::BigIntLiteral(b) => Ok(b.value.to_string()),
        // A private member `#x`: keyed by "#x" (a reserved property name; the
        // engine does not enforce true hard privacy, but `this.#x` works).
        ox::PropertyKey::PrivateIdentifier(id) => Ok(private_key(&id.name)),
        // A computed well-known-symbol key, e.g. `[Symbol.iterator]() {}`, maps to
        // the reserved string key (so a class can define the iteration method).
        ox::PropertyKey::StaticMemberExpression(m) => {
            if let ox::Expression::Identifier(o) = &m.object {
                if o.name == "Symbol" {
                    // Well-known symbols use the `@@<name>` key convention (matching
                    // the VM's `key_of`), so `[Symbol.toPrimitive]() {}` etc. work.
                    if let Some(k) = well_known_symbol_key(m.property.name.as_str()) {
                        return Ok(k.into());
                    }
                }
            }
            Err("computed or private class member names are not in the zipp-vm subset yet".into())
        }
        _ => Err("computed or private class member names are not in the zipp-vm subset yet".into()),
    }
}

/// The property key for a private member `#name` — keyed by "#name" (the leading
/// `#` makes it un-spellable as a normal property, our soft-privacy stand-in).
fn private_key(name: &str) -> String {
    if name.starts_with('#') {
        name.to_string()
    } else {
        format!("#{name}")
    }
}

/// Recognise the built-in Error constructor names the subset supports. Returns
/// the canonical `name` to store on the error object.
fn error_ctor(name: &str) -> Option<&'static str> {
    Some(match name {
        "Error" => "Error",
        "TypeError" => "TypeError",
        "RangeError" => "RangeError",
        "SyntaxError" => "SyntaxError",
        "ReferenceError" => "ReferenceError",
        "EvalError" => "EvalError",
        "URIError" => "URIError",
        "AggregateError" => "AggregateError",
        _ => return None,
    })
}

/// Collect the names introduced by top-level `var` declarations, recursing
/// through nested statements (blocks, loops, `if`, `try`, `switch`, labels,
/// `with`) but NOT into nested function/class bodies — `var` hoists to the
/// enclosing function/script scope, stopping at a function boundary. `let`/
/// `const`/`class` are excluded (they keep TDZ — a forward read throws). These
/// slots are pre-initialized to `undefined` so var hoisting matches JS.
/// All `var` binding names declared anywhere in `body` (recursing through blocks/
/// loops/if/try/switch but not nested functions). These bind in FUNCTION scope, so a
/// nested closure over one must be in the capture set to box the right register.
fn hoisted_var_names(body: &[ox::Statement]) -> Vec<String> {
    let mut set = std::collections::HashSet::new();
    for s in body {
        collect_hoisted_vars(s, &mut set);
    }
    set.into_iter().collect()
}

/// Add a block's DIRECT lexical declaration names (top-level `let`/`const`/
/// `class` of the block) to `out` — the names that block Annex B B.3.3 for a
/// same-block function declaration.
fn add_block_lexicals(s: &ox::Statement, out: &mut std::collections::HashSet<String>) {
    use ox::Statement as S;
    match s {
        S::VariableDeclaration(d) if d.kind.is_lexical() => {
            for decl in &d.declarations {
                capture::collect_pattern_names(&decl.id, out);
            }
        }
        S::ClassDeclaration(c) => {
            if let Some(id) = &c.id {
                out.insert(id.name.to_string());
            }
        }
        _ => {}
    }
}

/// Annex B B.3.3: collect names of `function` declarations inside BLOCKS (not at
/// the top level of the function body) that are eligible for a function-scoped
/// `var` binding. `blockers` is the set of lexical names in scope (params,
/// top-level lexicals, plus the lexical declarations of every enclosing block /
/// for-head / catch param): a function whose name is blocked would be an early
/// error under B.3.3 and so is SKIPPED (left block-local).
fn collect_b33_block_fns(
    s: &ox::Statement,
    nested: bool,
    blockers: &std::collections::HashSet<String>,
    out: &mut std::collections::HashSet<String>,
) {
    use ox::Statement as S;
    let for_left_lex = |d: &ox::VariableDeclaration, bk: &mut std::collections::HashSet<String>| {
        if d.kind.is_lexical() {
            for decl in &d.declarations {
                capture::collect_pattern_names(&decl.id, bk);
            }
        }
    };
    match s {
        S::FunctionDeclaration(f) => {
            // Annex B B.3.3 applies to PLAIN functions only — generator and
            // async (generator) declarations stay purely block-scoped.
            if nested && !f.generator && !f.r#async {
                if let Some(id) = &f.id {
                    let n = id.name.as_str();
                    if !blockers.contains(n) {
                        out.insert(n.to_string());
                    }
                }
            }
        }
        S::BlockStatement(b) => {
            let mut bk = blockers.clone();
            for st in &b.body {
                add_block_lexicals(st, &mut bk);
            }
            for st in &b.body {
                collect_b33_block_fns(st, true, &bk, out);
            }
        }
        S::ForStatement(f) => {
            let mut bk = blockers.clone();
            if let Some(ox::ForStatementInit::VariableDeclaration(d)) = &f.init {
                for_left_lex(d, &mut bk);
            }
            collect_b33_block_fns(&f.body, true, &bk, out);
        }
        S::ForOfStatement(f) => {
            let mut bk = blockers.clone();
            if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
                for_left_lex(d, &mut bk);
            }
            collect_b33_block_fns(&f.body, true, &bk, out);
        }
        S::ForInStatement(f) => {
            let mut bk = blockers.clone();
            if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
                for_left_lex(d, &mut bk);
            }
            collect_b33_block_fns(&f.body, true, &bk, out);
        }
        S::WhileStatement(w) => collect_b33_block_fns(&w.body, true, blockers, out),
        S::DoWhileStatement(d) => collect_b33_block_fns(&d.body, true, blockers, out),
        S::IfStatement(i) => {
            collect_b33_block_fns(&i.consequent, true, blockers, out);
            if let Some(a) = &i.alternate {
                collect_b33_block_fns(a, true, blockers, out);
            }
        }
        S::SwitchStatement(sw) => {
            // All cases share one block scope: their lexicals block every case.
            let mut bk = blockers.clone();
            for c in &sw.cases {
                for st in &c.consequent {
                    add_block_lexicals(st, &mut bk);
                }
            }
            for c in &sw.cases {
                for st in &c.consequent {
                    collect_b33_block_fns(st, true, &bk, out);
                }
            }
        }
        S::TryStatement(t) => {
            {
                let mut bk = blockers.clone();
                for st in &t.block.body {
                    add_block_lexicals(st, &mut bk);
                }
                for st in &t.block.body {
                    collect_b33_block_fns(st, true, &bk, out);
                }
            }
            if let Some(h) = &t.handler {
                let mut bk = blockers.clone();
                if let Some(p) = &h.param {
                    // B.3.5: a SIMPLE (BindingIdentifier) catch parameter does NOT
                    // block the B.3.3 var-binding extension — a `var`/block-function
                    // of the same name may redeclare it. A DESTRUCTURING catch param
                    // does block (a matching `var` there is an early error).
                    if !matches!(&p.pattern, ox::BindingPattern::BindingIdentifier(_)) {
                        capture::collect_pattern_names(&p.pattern, &mut bk);
                    }
                }
                for st in &h.body.body {
                    add_block_lexicals(st, &mut bk);
                }
                for st in &h.body.body {
                    collect_b33_block_fns(st, true, &bk, out);
                }
            }
            if let Some(fin) = &t.finalizer {
                let mut bk = blockers.clone();
                for st in &fin.body {
                    add_block_lexicals(st, &mut bk);
                }
                for st in &fin.body {
                    collect_b33_block_fns(st, true, &bk, out);
                }
            }
        }
        S::LabeledStatement(l) => collect_b33_block_fns(&l.body, nested, blockers, out),
        _ => {}
    }
}

/// Whether a statement (transitively, NOT descending into nested function
/// bodies) contains a `with` statement — gates the pre-declare-all-vars pass.
fn stmt_contains_with(s: &ox::Statement) -> bool {
    use ox::Statement as S;
    match s {
        S::WithStatement(_) => true,
        S::BlockStatement(b) => b.body.iter().any(stmt_contains_with),
        S::IfStatement(i) => {
            stmt_contains_with(&i.consequent)
                || i.alternate.as_ref().is_some_and(stmt_contains_with)
        }
        S::WhileStatement(w) => stmt_contains_with(&w.body),
        S::DoWhileStatement(d) => stmt_contains_with(&d.body),
        S::ForStatement(f) => stmt_contains_with(&f.body),
        S::ForOfStatement(f) => stmt_contains_with(&f.body),
        S::ForInStatement(f) => stmt_contains_with(&f.body),
        S::TryStatement(t) => {
            t.block.body.iter().any(stmt_contains_with)
                || t.handler.as_ref().is_some_and(|h| h.body.body.iter().any(stmt_contains_with))
                || t.finalizer.as_ref().is_some_and(|f| f.body.iter().any(stmt_contains_with))
        }
        S::SwitchStatement(sw) => {
            sw.cases.iter().any(|c| c.consequent.iter().any(stmt_contains_with))
        }
        S::LabeledStatement(l) => stmt_contains_with(&l.body),
        _ => false,
    }
}

fn collect_hoisted_vars(s: &ox::Statement, out: &mut std::collections::HashSet<String>) {
    use ox::Statement as S;
    match s {
        S::VariableDeclaration(d) if d.kind == ox::VariableDeclarationKind::Var => {
            for decl in &d.declarations {
                capture::collect_pattern_names(&decl.id, out);
            }
        }
        // `export var x` / `export var {x} = ...`: the declared names hoist
        // exactly like an unexported top-level var.
        S::ExportNamedDeclaration(e) => {
            if let Some(ox::Declaration::VariableDeclaration(d)) = &e.declaration {
                if d.kind == ox::VariableDeclarationKind::Var {
                    for decl in &d.declarations {
                        capture::collect_pattern_names(&decl.id, out);
                    }
                }
            }
        }
        S::BlockStatement(b) => {
            for s in &b.body {
                collect_hoisted_vars(s, out);
            }
        }
        S::IfStatement(i) => {
            collect_hoisted_vars(&i.consequent, out);
            if let Some(a) = &i.alternate {
                collect_hoisted_vars(a, out);
            }
        }
        S::WhileStatement(w) => collect_hoisted_vars(&w.body, out),
        S::DoWhileStatement(d) => collect_hoisted_vars(&d.body, out),
        S::ForStatement(f) => {
            if let Some(ox::ForStatementInit::VariableDeclaration(d)) = &f.init {
                if d.kind == ox::VariableDeclarationKind::Var {
                    for decl in &d.declarations {
                        capture::collect_pattern_names(&decl.id, out);
                    }
                }
            }
            collect_hoisted_vars(&f.body, out);
        }
        S::ForOfStatement(f) => {
            if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
                if d.kind == ox::VariableDeclarationKind::Var {
                    for decl in &d.declarations {
                        capture::collect_pattern_names(&decl.id, out);
                    }
                }
            }
            collect_hoisted_vars(&f.body, out);
        }
        S::ForInStatement(f) => {
            if let ox::ForStatementLeft::VariableDeclaration(d) = &f.left {
                if d.kind == ox::VariableDeclarationKind::Var {
                    for decl in &d.declarations {
                        capture::collect_pattern_names(&decl.id, out);
                    }
                }
            }
            collect_hoisted_vars(&f.body, out);
        }
        S::TryStatement(t) => {
            for s in &t.block.body {
                collect_hoisted_vars(s, out);
            }
            if let Some(h) = &t.handler {
                for s in &h.body.body {
                    collect_hoisted_vars(s, out);
                }
            }
            if let Some(f) = &t.finalizer {
                for s in &f.body {
                    collect_hoisted_vars(s, out);
                }
            }
        }
        S::SwitchStatement(sw) => {
            for case in &sw.cases {
                for s in &case.consequent {
                    collect_hoisted_vars(s, out);
                }
            }
        }
        S::LabeledStatement(l) => collect_hoisted_vars(&l.body, out),
        S::WithStatement(w) => collect_hoisted_vars(&w.body, out),
        _ => {}
    }
}

/// The internal property key for a well-known symbol (`Symbol.<name>`), matching
/// the VM's `WELL_KNOWN_SYMBOLS` / `key_of` convention. `None` for non-well-known
/// names (a computed `[Symbol.foo]` that isn't a known symbol stays unsupported).
fn well_known_symbol_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "iterator" => "@@iterator",
        "asyncIterator" => "@@asyncIterator",
        "toPrimitive" => "@@toPrimitive",
        "toStringTag" => "@@toStringTag",
        "hasInstance" => "@@hasInstance",
        "isConcatSpreadable" => "@@isConcatSpreadable",
        "species" => "@@species",
        "match" => "@@match",
        "matchAll" => "@@matchAll",
        "replace" => "@@replace",
        "search" => "@@search",
        "split" => "@@split",
        "unscopables" => "@@unscopables",
        "dispose" => "@@dispose",
        "asyncDispose" => "@@asyncDispose",
        _ => return None,
    })
}

/// The canonical index of an error constructor name (parallel to the VM's
/// `ERROR_NAMES` / `error_protos`). Unknown → 0 (`Error`).
fn error_kind_index(name: &str) -> u8 {
    match name {
        "TypeError" => 1,
        "RangeError" => 2,
        "SyntaxError" => 3,
        "ReferenceError" => 4,
        "EvalError" => 5,
        "URIError" => 6,
        "AggregateError" => 7,
        _ => 0, // "Error" and anything unexpected
    }
}

/// Extract the loop-variable name from a `for-of`/`for-in` left-hand side.
/// Supports `for (let/const/var x of …)` and `for (x of …)`.
fn for_left_name(left: &ox::ForStatementLeft) -> R<String> {
    match left {
        ox::ForStatementLeft::VariableDeclaration(d) => match &d.declarations[0].id {
            ox::BindingPattern::BindingIdentifier(id) => Ok(id.name.to_string()),
            _ => Err("for-of/for-in destructuring not in the zipp-vm subset yet".into()),
        },
        ox::ForStatementLeft::AssignmentTargetIdentifier(id) => Ok(id.name.to_string()),
        _ => Err("for-of/for-in needs a simple variable target".into()),
    }
}

/// Render a numeric object key the way JS does (`{0: 'a'}` has key `"0"`).
fn fmt_key_num(n: f64) -> String {
    // The property key for a numeric literal is ToString(value) — the canonical
    // ECMAScript Number→String (e.g. 0.0000001 → "1e-7", 1e21 → "1e+21"), the SAME
    // form the runtime uses for `obj[n]`, so a numeric-keyed member is stored and
    // read under one key. (Rust's `{}` differs for small/large magnitudes.)
    crate::vm::helpers_num2::fmt_f64(n)
}

/// Conservative static check: is this expression definitely a number? Used to
/// gate the `+ <int>` fast path (where `+` could otherwise mean string concat).
/// Only returns true for cases that cannot be strings.
fn is_numeric_expr(e: &ox::Expression) -> bool {
    use ox::Expression as E;
    match e {
        E::NumericLiteral(_) => true,
        E::ParenthesizedExpression(p) => is_numeric_expr(&p.expression),
        E::UnaryExpression(u) => matches!(
            u.operator,
            ox::UnaryOperator::UnaryNegation | ox::UnaryOperator::UnaryPlus
        ),
        E::BinaryExpression(b) => matches!(
            b.operator,
            ox::BinaryOperator::Subtraction
                | ox::BinaryOperator::Multiplication
                | ox::BinaryOperator::Division
                | ox::BinaryOperator::Remainder
        ),
        _ => false,
    }
}

/// Extract (fixed param names, optional rest-param name, body statements) from
/// an oxc function.
fn function_parts<'a>(
    f: &'a ox::Function,
) -> R<(Vec<String>, Option<String>, &'a [ox::Statement<'a>])> {
    let params = param_slot_names(&f.params)?;
    let rest = rest_name(&f.params)?;
    let body = match &f.body {
        Some(b) => b.statements.as_slice(),
        None => &[],
    };
    Ok((params, rest, body))
}

/// One name per parameter SLOT (reg 1..). A plain identifier (or `x = default`)
/// uses its name; a destructuring pattern (`{a}` / `[a,b]`) gets a synthetic
/// slot name and is destructured into its leaves at function entry by
/// `bind_pattern_params`.
/// ExpectedArgumentCount → the function's `.length`: the count of leading formal
/// parameters before the first one with a default value (an AssignmentPattern).
/// A destructuring parameter without a default counts; the rest parameter lives
/// in `params.rest`, not `items`, so it is excluded automatically.
/// IsAnonymousFunctionDefinition: an anonymous function/arrow/class expression
/// (function/generator/async-function expressions count when they have no `id`).
/// Such a value takes the property/binding name via NamedEvaluation.
fn is_anonymous_fn_def(e: &ox::Expression) -> bool {
    match e {
        ox::Expression::FunctionExpression(f) => f.id.is_none(),
        ox::Expression::ArrowFunctionExpression(_) => true,
        ox::Expression::ClassExpression(c) => c.id.is_none(),
        ox::Expression::ParenthesizedExpression(p) => is_anonymous_fn_def(&p.expression),
        _ => false,
    }
}

fn expected_arg_count(params: &ox::FormalParameters) -> u16 {
    let mut n = 0u16;
    for item in &params.items {
        // A default value lives in `item.initializer` (simple params) or as an
        // AssignmentPattern (destructuring defaults); the first such param stops
        // the count.
        if item.initializer.is_some()
            || matches!(&item.pattern, ox::BindingPattern::AssignmentPattern(_))
        {
            break;
        }
        n += 1;
    }
    n
}

/// IsSimpleParameterList: every parameter a plain identifier with no default,
/// and no rest. (A mapped arguments object requires this in sloppy mode.)
fn params_are_simple(params: &ox::FormalParameters) -> bool {
    params.rest.is_none()
        && params.items.iter().all(|item| {
            item.initializer.is_none()
                && matches!(&item.pattern, ox::BindingPattern::BindingIdentifier(_))
        })
}

fn param_slot_names(params: &ox::FormalParameters) -> R<Vec<String>> {
    let mut out = Vec::new();
    for (i, item) in params.items.iter().enumerate() {
        match &item.pattern {
            ox::BindingPattern::BindingIdentifier(id) => out.push(id.name.to_string()),
            ox::BindingPattern::AssignmentPattern(ap) => match &ap.left {
                ox::BindingPattern::BindingIdentifier(id) => out.push(id.name.to_string()),
                _ => return Err("a default on a destructuring parameter is not in the subset yet".into()),
            },
            ox::BindingPattern::ObjectPattern(_) | ox::BindingPattern::ArrayPattern(_) => {
                out.push(format!("<arg{i}>"))
            }
        }
    }
    Ok(out)
}

/// All parameter binding identifiers in source order, duplicates preserved
/// (for strict-mode early-error checks: `eval`/`arguments` and duplicate names).
fn collect_param_names_ordered(params: &ox::FormalParameters, out: &mut Vec<String>) {
    fn walk(p: &ox::BindingPattern, out: &mut Vec<String>) {
        use ox::BindingPattern as P;
        match p {
            P::BindingIdentifier(id) => out.push(id.name.to_string()),
            P::AssignmentPattern(ap) => walk(&ap.left, out),
            P::ObjectPattern(op) => {
                for prop in &op.properties {
                    walk(&prop.value, out);
                }
                if let Some(rest) = &op.rest {
                    walk(&rest.argument, out);
                }
            }
            P::ArrayPattern(arr) => {
                for el in arr.elements.iter().flatten() {
                    walk(el, out);
                }
                if let Some(rest) = &arr.rest {
                    walk(&rest.argument, out);
                }
            }
        }
    }
    for item in &params.items {
        walk(&item.pattern, out);
    }
    if let Some(r) = &params.rest {
        walk(&r.rest.argument, out);
    }
}

/// Strict-mode early error: `eval` and `arguments` may not be used as a binding
/// name or assignment target in strict-mode code. Returns a `SyntaxError`-prefixed
/// error (mapped to a thrown SyntaxError by the eval/compile entry points).
fn strict_name_err(strict: bool, name: &str) -> R<()> {
    if !strict {
        return Ok(());
    }
    if name == "eval" || name == "arguments" {
        return Err(format!(
            "SyntaxError: '{name}' may not be used as a binding name or assignment target in strict mode"
        ));
    }
    if is_strict_reserved_word(name) {
        return Err(format!(
            "SyntaxError: '{name}' is a reserved word in strict mode"
        ));
    }
    Ok(())
}

/// The ECMAScript identifiers reserved ONLY in strict mode — they may not be used
/// as a binding name, assignment target, or identifier reference there. The six
/// FutureReservedWords plus the contextual keywords `let`/`static`/`yield` (these
/// reach a binding/reference position as plain identifiers only where they are NOT
/// the declaration keyword / class modifier / YieldExpression). All are valid
/// identifiers in sloppy mode.
fn is_strict_reserved_word(name: &str) -> bool {
    matches!(
        name,
        "implements"
            | "interface"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "let"
            | "static"
            | "yield"
    )
}

/// Leaf binding names introduced by destructuring parameters (for capture
/// analysis — a closure may capture a destructured parameter's leaf).
fn param_pattern_leaves(params: &ox::FormalParameters) -> Vec<String> {
    let mut set = HashSet::new();
    for item in &params.items {
        if matches!(
            &item.pattern,
            ox::BindingPattern::ObjectPattern(_) | ox::BindingPattern::ArrayPattern(_)
        ) {
            capture::collect_pattern_names(&item.pattern, &mut set);
        }
    }
    // A destructuring rest parameter (`...[a,b]`) introduces its leaves too.
    if let Some(r) = &params.rest {
        if matches!(
            &r.rest.argument,
            ox::BindingPattern::ObjectPattern(_) | ox::BindingPattern::ArrayPattern(_)
        ) {
            capture::collect_pattern_names(&r.rest.argument, &mut set);
        }
    }
    set.into_iter().collect()
}

/// All bindable parameter names (fixed params plus the rest name, if any) — the
/// set capture analysis must consider locals of this function.
fn with_rest(params: &[String], rest: &Option<String>) -> Vec<String> {
    let mut v = params.to_vec();
    if let Some(r) = rest {
        v.push(r.clone());
    }
    v
}

/// The rest-parameter SLOT name (`function f(...args)` → `Some("args")`), or
/// `None`. A destructuring rest target (`...[a,b]` / `...{x}`) uses a synthetic
/// slot `"<rest>"` that holds the gathered array; `bind_params` then destructures
/// it into the pattern's leaves.
fn rest_name(params: &ox::FormalParameters) -> R<Option<String>> {
    match &params.rest {
        None => Ok(None),
        Some(r) => match &r.rest.argument {
            ox::BindingPattern::BindingIdentifier(id) => Ok(Some(id.name.to_string())),
            ox::BindingPattern::ObjectPattern(_) | ox::BindingPattern::ArrayPattern(_) => {
                Ok(Some("<rest>".to_string()))
            }
            _ => Err("rest-parameter destructuring is not in the zipp-vm subset yet".into()),
        },
    }
}
