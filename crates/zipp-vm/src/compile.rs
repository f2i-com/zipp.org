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
        Instr::StoreGlobal { src, .. } => src == r,
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
            Instr::StoreGlobal { idx, .. } if idx == g => {
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
                matches!(f.code[m], Instr::StoreGlobal { idx: _, src } if src == dst)
            });
            let (g, store_ip) = match store_ip {
                Some(m) => match f.code[m] {
                    Instr::StoreGlobal { idx, .. } => (idx, m),
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

pub fn compile_program(prog: &ox::Program, source: &str) -> R<Program> {
    let mut c = Compiler::new(source.to_string());
    c.compile(prog)?;
    for (i, f) in c.functions.iter_mut().enumerate() {
        rewrite_string_accumulators(f, i == 0);
    }
    Ok(Program {
        functions: c.functions,
        global_count: c.globals.len() as u32,
        classes: c.classes,
        global_names: c.globals,
        hoisted_globals: c.hoisted_globals,
    })
}

/// Compile an `eval` code string. Identical to [`compile_program`] except the
/// top-level script returns its *completion value* (the value of the last
/// evaluated expression statement) — what `eval("1 + 1")` must yield. The VM
/// installs the resulting functions into its runtime function table and remaps
/// the program's independently-numbered global slots onto the live globals.
pub fn compile_eval(
    prog: &ox::Program,
    source: &str,
    force_strict: bool,
    force_new_target_ok: bool,
) -> R<Program> {
    let mut c = Compiler::new(source.to_string());
    c.eval_mode = true;
    c.force_strict = force_strict;
    c.force_new_target_ok = force_new_target_ok;
    c.compile(prog)?;
    for (i, f) in c.functions.iter_mut().enumerate() {
        rewrite_string_accumulators(f, i == 0);
    }
    Ok(Program {
        functions: c.functions,
        global_count: c.globals.len() as u32,
        classes: c.classes,
        global_names: c.globals,
        hoisted_globals: c.hoisted_globals,
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
    /// True when compiling an `eval` code string: the top-level script returns
    /// its *completion value* (the value of the last evaluated expression
    /// statement) instead of `undefined`.
    eval_mode: bool,
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
            eval_mode: false,
            force_strict: false,
            force_new_target_ok: false,
            in_strict: false,
            new_target_ok: false,
            class_enclosing: Vec::new(),
            class_derived: false,
            const_globals: HashSet::new(),
            lexical_globals: HashSet::new(),
            decl_globals: HashSet::new(),
        }
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
        for s in &prog.body {
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
        {
            let mut vars = std::collections::HashSet::new();
            for s in &prog.body {
                collect_hoisted_vars(s, &mut vars);
            }
            for name in vars {
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
            false, // top-level script is not async
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
        let mut fc = FnCompiler::new(self, params, rest, captured, enclosing);
        fc.cx.in_strict = is_strict;
        fc.is_script = is_script;
        fc.in_generator = is_generator;
        fc.in_async = is_async;
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
                let idx = fc.add_string_const(d.expression.value.as_str());
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
            if capture::free_vars(params, body).contains(sn) {
                let r = fc.alloc_reg();
                fc.emit(Instr::LoadCallee { dst: r });
                if fc.captured.contains(sn) {
                    fc.emit(Instr::MakeCell { reg: r });
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
                    if is_script {
                        fc.cx.global_slot(id.name.as_str());
                    } else {
                        fc.declare_local(id.name.as_str());
                    }
                }
            }
        }

        // Hoist top-level lexical (`let`/`const`) names into `lexical_globals` so an
        // assignment to one BEFORE its declaration runs (its TDZ) is a ReferenceError
        // even in sloppy mode — `for ([x] of [[]]) {} let x;`. Only DIRECT top-level
        // declarations bind to globals (block-scoped lexicals don't leak), so a
        // VariableDeclaration nested in a block is not registered.
        if is_script {
            for s in body {
                if let ox::Statement::VariableDeclaration(d) = s {
                    if d.kind.is_lexical() {
                        for decl in &d.declarations {
                            if let ox::BindingPattern::BindingIdentifier(id) = &decl.id {
                                let slot = fc.cx.global_slot(id.name.as_str()) as u32;
                                fc.cx.lexical_globals.insert(slot);
                            }
                        }
                    }
                }
            }
        }

        for s in body {
            fc.stmt(s)?;
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
            is_strict,
            constants: fc.constants,
            string_constants: fc.string_constants,
            name_global: None, // set by the caller for top-level declarations
            upvalues,
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
        let captured = capture::captured_locals(&names, body);
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
        let mut fc = FnCompiler::new(self, params, rest, captured, enclosing);
        fc.cx.in_strict = true;
        fc.super_class = super_class;
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
        for (fname, finit) in fields {
            let save = fc.next_reg;
            let v = match finit {
                Some(e) => fc.expr(e)?,
                None => {
                    let t = fc.temp();
                    fc.emit(Instr::LoadUndefined { dst: t });
                    t
                }
            };
            let name_idx = fc.string_name(fname);
            fc.emit(Instr::SetProp { obj: 0, name: name_idx, val: v });
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
            fc.emit(Instr::FieldInit { key_index: i as u16, val: v });
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
            is_strict: true,
            constants: fc.constants,
            string_constants: fc.string_constants,
            name_global: None,
            upvalues,
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
    ) -> R<FuncProto> {
        let parent_strict = self.in_strict;
        let is_strict = parent_strict || has_use_strict(&a.body.directives);
        let mut fc = FnCompiler::new(self, params, rest, captured, enclosing);
        fc.cx.in_strict = is_strict;
        // An arrow has no `super` binding of its own: `super.x` / `super.m()` inside
        // it resolves LEXICALLY to the enclosing non-arrow method's home class. The
        // runtime resolves super via the home-class id + the lexical `this` (which
        // arrows already capture), so propagating the enclosing method's compile-time
        // home-class id is sufficient.
        fc.super_class = super_class;
        // An arrow inherits the enclosing method's derived-ness (so `super(...)` in an
        // arrow inside a derived constructor is allowed). `cx.class_derived` still
        // reflects the enclosing class while its method bodies (and their arrows) compile.
        fc.derived_class = fc.cx.class_derived;
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
            for s in &a.body.statements {
                fc.stmt(s)?;
            }
            fc.emit(Instr::ReturnUndefined);
        }
        fc.cx.in_strict = parent_strict; // restore: nested compiles are done
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
            is_strict,
            constants: fc.constants,
            string_constants: fc.string_constants,
            name_global: None,
            upvalues,
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
            return rest.trim_start().to_string();
        }
    }
    text
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
        is_strict: false,
        constants: Vec::new(),
        string_constants: Vec::new(),
        name_global: None,
        upvalues: Vec::new(),
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
    /// True only inside a DERIVED class's methods — gates `super(...)` (calling it in
    /// a base class constructor is an early SyntaxError). `super.x` is not gated.
    derived_class: bool,
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
}

impl LoopCtx {
    fn loop_frame(label: Option<String>, handler_depth: usize) -> LoopCtx {
        LoopCtx {
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
            is_loop: true,
            label,
            handler_depth,
        }
    }
    fn switch_frame(label: Option<String>, handler_depth: usize) -> LoopCtx {
        LoopCtx {
            break_jumps: Vec::new(),
            continue_jumps: Vec::new(),
            is_loop: false,
            label,
            handler_depth,
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
        let mut fc = FnCompiler {
            cx,
            code: Vec::new(),
            constants: Vec::new(),
            string_constants: Vec::new(),
            scopes: vec![Vec::new()],
            next_reg: 0,
            max_reg: 0,
            rest_reg: None,
            arguments_reg: None,
            uses_arguments: false,
            super_class: None,
            derived_class: false,
            this_override: None,
            pattern_block_local: false,
            in_generator: false,
            in_async: false,
            pending_label: None,
            is_script: false,
            completion_reg: None,
            chain_bails: Vec::new(),
            loop_ctx: Vec::new(),
            handler_depth: 0,
            self_name: None,
            captured,
            cell_regs: HashSet::new(),
            const_regs: HashSet::new(),
            param_tdz: HashSet::new(),
            upvalues: Rc::new(RefCell::new(Vec::new())),
            enclosing,
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
        // Free the registers the scope's locals used (block-local reuse).
        self.next_reg -= scope.len() as Reg;
    }

    fn declare_local(&mut self, name: &str) -> Reg {
        let r = self.alloc_reg();
        self.scopes.last_mut().unwrap().push((name.to_string(), r));
        // Box the local into a cell if a nested function captures it, so the
        // closure and this scope share one mutable slot.
        if self.captured.contains(name) {
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
        self.scopes[1..n - 1]
            .iter()
            .any(|s| s.iter().any(|(nm, _)| nm == name))
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
                // Hoist block-level function declarations: declare each as a local
                // in this block scope first, so `func_decl` binds it (and forward
                // references / calls within the block resolve to the local rather
                // than an undeclared global). Only inside a real function body —
                // at script top level, `func_decl` binds block functions to globals
                // (Annex B hoisting), so a local here would shadow that with an
                // uninitialized slot.
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
                            if !self.is_script || self.cx.in_strict || self.block_fn_conflicts(nm) {
                                self.declare_local(nm);
                            }
                        }
                    }
                }
                for st in &b.body {
                    self.stmt(st)?;
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
                if let Some(arg) = &r.argument {
                    let v = self.expr(arg)?;
                    self.emit(Instr::Return { src: v });
                } else {
                    self.emit(Instr::ReturnUndefined);
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
        // store_binding, so it is unaffected).
        let is_const = d.kind == ox::VariableDeclarationKind::Const;
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
                let src = self.alloc_reg();
                let sv = self.expr_into(init, src)?;
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
            let block_scoped_lexical = d.kind.is_lexical() && self.scopes.len() > 1;
            if self.is_script && !block_scoped_lexical {
                let slot = self.cx.global_slot(name) as u32;
                if is_const {
                    self.cx.const_globals.insert(slot);
                }
                let tmp = self.temp();
                let v = if let Some(init) = &decl.init {
                    self.compile_named_init(tmp, init, name)?
                } else {
                    self.emit(Instr::LoadUndefined { dst: tmp });
                    tmp
                };
                self.emit(Instr::StoreGlobal { idx: slot, src: v });
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
                    if self.cell_regs.contains(&reg) {
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
            let reg = self.declare_local(name);
            if is_const {
                self.const_regs.insert(reg);
            }
            let is_cell = self.cell_regs.contains(&reg);
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
            } else if !is_cell {
                self.emit(Instr::LoadUndefined { dst: reg });
            }
            // A captured local with no initializer keeps the cell's default
            // (undefined), set when MakeCell boxed the freshly-undefined reg.
        }
        Ok(())
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
            ox::PropertyKey::StringLiteral(s) => s.value.to_string(),
            ox::PropertyKey::NumericLiteral(n) => fmt_key_num(n.value),
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
        if self.is_script && !is_block_local && !has_upvalues {
            // Top-level (or no-conflict block function) with no captures: bind the
            // name to a global; the VM materialises the function object at startup.
            if let Some(n) = &name {
                let slot = self.cx.global_slot(n);
                proto.name_global = Some(slot);
            }
            self.cx.functions.push(proto);
        } else if self.is_script && !is_block_local {
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
            match binding {
                Some(Binding::Local(reg)) => self.emit_make_callable(reg, id, has_upvalues),
                Some(Binding::LocalCell(cell)) => {
                    let tmp = self.temp();
                    self.emit_make_callable(tmp, id, has_upvalues);
                    self.emit(Instr::CellSet { cell, src: tmp });
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
            _ if self.is_script => {
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
        let (class_id, static_fields, computed, computed_fields, static_block_fns) =
            self.compile_class(class, name)?;
        // Evaluate the superclass value (`extends P`) into a temp the VM links in.
        let parent_reg = if let Some(sc) = &class.super_class {
            let t = self.temp();
            let v = self.expr_into(sc, t)?;
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
        // Static fields are own properties of the class value, initialized here
        // (in the enclosing scope) right after the class is created.
        for (fname, finit) in &static_fields {
            let save = self.next_reg;
            // A static field initializer evaluates with `this` = the class.
            self.this_override = Some(cls);
            let v = match finit {
                Some(e) => self.expr(e)?,
                None => {
                    let t = self.temp();
                    self.emit(Instr::LoadUndefined { dst: t });
                    t
                }
            };
            self.this_override = None;
            let name_idx = self.string_name(fname);
            self.emit(Instr::SetProp { obj: cls, name: name_idx, val: v });
            self.next_reg = save;
        }
        // Computed-key methods: evaluate each key now and install it on the class.
        for (key, func, kind) in &computed {
            let save = self.next_reg;
            let kr = self.expr(key)?;
            self.emit(Instr::ClassAddMember { class: cls, key: kr, func: *func, kind: *kind });
            self.next_reg = save;
        }
        // Computed-key fields: evaluate each KEY now (once, in source order). A
        // static field also assigns its value on the class here; an instance
        // field's key is parked on the class for the ctor's per-instance FieldInit.
        for (key, init, is_static) in &computed_fields {
            let save = self.next_reg;
            let kr = self.expr(key)?; // key evaluates with the enclosing `this`
            if *is_static {
                // …but the value initializer evaluates with `this` = the class.
                self.this_override = Some(cls);
                let vr = match init {
                    Some(e) => self.expr(e)?,
                    None => {
                        let t = self.temp();
                        self.emit(Instr::LoadUndefined { dst: t });
                        t
                    }
                };
                self.this_override = None;
                self.emit(Instr::SetIndex { obj: cls, key: kr, val: vr });
            } else {
                self.emit(Instr::PushFieldKey { class: cls, key: kr });
            }
            self.next_reg = save;
        }
        // `static { … }` blocks: run each thunk with `this` = the class, in source
        // order, after the static fields. Invoked as `thunk.call(cls)` so the
        // existing call machinery binds `this` — no new opcode needed.
        for &fid in &static_block_fns {
            let save = self.next_reg;
            let f = self.temp();
            self.emit(Instr::MakeFunc { dst: f, func_id: fid });
            let argb = self.temp();
            self.emit(Instr::Move { dst: argb, src: cls });
            let trash = self.temp();
            let call_idx = self.string_name("call");
            self.emit(Instr::CallMethod { dst: trash, obj: f, name: call_idx, arg_base: argb, argc: 1 });
            self.next_reg = save;
        }
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
            methods: Vec::new(),
            getters: Vec::new(),
            setters: Vec::new(),
            statics: Vec::new(),
            static_getters: Vec::new(),
            static_setters: Vec::new(),
            source: String::new(), // filled in below once the body is compiled
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
                    match class_key_name(&m.key) {
                        Ok(name) => match (m.r#static, m.kind) {
                            (true, ox::MethodDefinitionKind::Method) => statics.push((name, &m.value)),
                            (true, ox::MethodDefinitionKind::Get) => static_getters.push((name, &m.value)),
                            (true, ox::MethodDefinitionKind::Set) => static_setters.push((name, &m.value)),
                            (true, ox::MethodDefinitionKind::Constructor) => unreachable!(),
                            (false, ox::MethodDefinitionKind::Method) => methods.push((name, &m.value)),
                            (false, ox::MethodDefinitionKind::Get) => getters.push((name, &m.value)),
                            (false, ox::MethodDefinitionKind::Set) => setters.push((name, &m.value)),
                            (false, ox::MethodDefinitionKind::Constructor) => unreachable!(),
                        },
                        Err(e) if m.computed => {
                            let key = m.key.as_expression().ok_or(e)?;
                            computed.push((key, &m.value, kind));
                        }
                        Err(e) => return Err(e),
                    }
                }
                ox::ClassElement::PropertyDefinition(p) => {
                    match class_key_name(&p.key) {
                        // Static string key.
                        Ok(name) if p.r#static => static_fields.push((name, p.value.as_ref())),
                        // Instance string key.
                        Ok(name) => fields.push((name, p.value.as_ref())),
                        // Computed key `[expr] = v` — evaluated once at class def.
                        Err(e) => {
                            let key = p.key.as_expression().ok_or(e)?;
                            computed_fields_ordered.push((key, p.value.as_ref(), p.r#static));
                            if !p.r#static {
                                instance_computed_inits.push(p.value.as_ref());
                            }
                        }
                    }
                }
                ox::ClassElement::StaticBlock(b) => {
                    static_blocks.push(&b.body);
                }
                _ => return Err("unsupported class member in the zipp-vm subset".into()),
            }
        }
        // Method protos.
        let mut method_defs: Vec<(String, u32)> = Vec::new();
        for (mname, func) in &methods {
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
                None, // statics: `super` would refer to the parent class, not handled
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
                None, // statics: no `super`
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
                None, // statics: no `super`
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
        let ctor = if has_explicit_ctor || !fields.is_empty() || !instance_computed_inits.is_empty() {
            let (params, rest, body) = match ctor_fn {
                Some(f) => function_parts(f)?,
                None => (Vec::new(), None, &[][..]),
            };
            let params_ast = ctor_fn.map(|f| &*f.params);
            let mut proto = self.cx.compile_class_fn(
                &format!("{cname}.constructor"),
                &params,
                rest.as_deref(),
                params_ast,
                &fields,
                &instance_computed_inits,
                body,
                super_class_id,
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
                if matches!(*kind, 3 | 4 | 5) { None } else { super_class_id }, // statics get no super
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
                None, // static context: `super` is the parent class (as for static methods, not modelled)
                false,
                false,
            )?;
            let fid = self.cx.functions.len() as u32;
            self.cx.functions.push(proto);
            static_block_fns.push(fid);
        }
        self.cx.classes[class_id as usize] = ClassDef {
            name: cname,
            ctor,
            has_explicit_ctor,
            methods: method_defs,
            getters: getter_defs,
            setters: setter_defs,
            statics: static_defs,
            static_getters: static_getter_defs,
            static_setters: static_setter_defs,
            source: self.cx.src_slice(class.span.start, class.span.end),
        };
        self.cx.class_enclosing = saved_enclosing;
        self.cx.class_derived = saved_derived;
        Ok((class_id, static_fields, computed_defs, computed_fields_ordered, static_block_fns))
    }

    /// The enclosing-function chain to hand a function nested in THIS one: our
    /// own enclosing chain plus a snapshot of our current bindings.
    fn child_enclosing(&self) -> Vec<EnclosingFn> {
        let mut e = self.enclosing.clone();
        e.push(self.snapshot());
        e
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
        let enclosing = self.child_enclosing();
        let mut proto =
            self.cx.compile_arrow_body(&params, rest.as_deref(), a, captured, enclosing, self.super_class)?;
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

    /// Emit creation of an ARROW value. Always `MakeArrow` (even with no
    /// upvalues) so the resulting closure carries the lexically-captured `this`
    /// of the defining frame — `MakeFunc` has no slot for it. The captured `this`
    /// is read from the effective-`this` register at the definition site
    /// (`this_override` when inside a static field initializer, else reg 0).
    fn emit_make_arrow(&mut self, dst: Reg, id: u32) {
        let this_reg = self.this_override.unwrap_or(0);
        self.emit(Instr::MakeArrow { dst, func_id: id, this_reg });
    }

    fn if_stmt(&mut self, i: &ox::IfStatement) -> R<()> {
        // The statement's completion V starts as undefined (a not-taken / empty
        // branch yields undefined, not the prior statement's value). No-op outside
        // eval mode.
        self.reset_loop_completion();
        let cond = self.expr(&i.test)?;
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 }); // patched
        self.stmt(&i.consequent)?;
        if let Some(alt) = &i.alternate {
            let jmp = self.here();
            self.emit(Instr::Jump { target: 0 }); // patched
            let else_start = self.here();
            self.patch_jump(jf, else_start);
            self.stmt(alt)?;
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
        // init
        if let Some(init) = &f.init {
            match init {
                ox::ForStatementInit::VariableDeclaration(d) => self.var_decl(d)?,
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
        for s in &t.block.body {
            self.stmt(s)?;
        }
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
        for s in &t.block.body {
            self.stmt(s)?;
        }
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
                    (self.declare_local_no_box(id.name.as_str()), Some(id.name.to_string()), None)
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
            self.declare_pattern(pat)?;
            self.extract_pattern(pat, e_reg)?;
        }
        if let Some(n) = &e_name {
            if self.captured.contains(n) {
                self.emit(Instr::MakeCell { reg: e_reg });
                self.cell_regs.insert(e_reg);
            }
        }
        for s in &handler.body.body {
            self.stmt(s)?;
        }
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
                            if self.cx.in_strict || f.generator || f.r#async {
                                self.declare_local(id.name.as_str());
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
        let v = self.expr_into(&f.right, iter_reg)?;
        if v != iter_reg {
            self.emit(Instr::Move { dst: iter_reg, src: v });
        }
        // Resolve the iterator. `for await` uses the ASYNC iterator (@@asyncIterator
        // → @@iterator fallback); plain `for of` uses @@iterator. Built-ins/async
        // generators pass through and are driven by IterNext / ForAwaitNext.
        if f.r#await {
            self.emit(Instr::GetAsyncIterator { dst: iter_reg, src: iter_reg });
        } else {
            self.emit(Instr::GetIterator { dst: iter_reg, src: iter_reg });
        }
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
        let (var_reg, var_is_cell) = match (pattern, assign_tgt) {
            (Some(p), _) => {
                self.declare_pattern(p)?;
                (0, false)
            }
            (None, Some(_)) => (0, false), // assignment target: nothing to declare
            (None, None) => {
                let var_name = for_left_name(&f.left)?;
                let r = self.declare_local(&var_name);
                (r, self.cell_regs.contains(&r))
            }
        };

        let top = self.here();
        let save = self.next_reg;
        let done = self.alloc_reg();
        // Write the element straight into a plain-local loop var; use a temp for a
        // destructuring pattern, an assignment target, or a cell-boxed var.
        let elem = if pattern.is_some() || assign_tgt.is_some() || var_is_cell {
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
            self.emit(Instr::IterNext { value_dst: elem, done_dst: done, iter: iter_reg, idx: idx_reg });
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
        let close_push = if let Some(er) = exc_reg {
            let at = self.here();
            self.emit(Instr::PushHandler { catch_target: 0, catch_reg: er });
            self.handler_depth += 1;
            Some(at)
        } else {
            None
        };
        if let Some(p) = pattern {
            self.extract_pattern(p, elem)?;
        } else if let Some(tgt) = assign_tgt {
            self.assign_target(tgt, elem)?;
        } else if var_is_cell {
            // Per-iteration binding: a FRESH cell each iteration so a closure in
            // the body captures THIS element, not the last one (for-of let).
            self.emit(Instr::Move { dst: var_reg, src: elem });
            self.emit(Instr::MakeCell { reg: var_reg });
        }
        self.next_reg = save; // reclaim done + elem temps

        self.stmt(&f.body)?;
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

        let obj_reg = self.declare_local("<forin.obj>");
        let v = self.expr_into(&f.right, obj_reg)?;
        if v != obj_reg {
            self.emit(Instr::Move { dst: obj_reg, src: v });
        }
        let keys_reg = self.declare_local("<forin.keys>");
        self.emit(Instr::ForInKeys { dst: keys_reg, obj: obj_reg });
        let len_reg = self.declare_local("<forin.len>");
        self.emit(Instr::LenOf { dst: len_reg, obj: keys_reg });
        let idx_reg = self.declare_local("<forin.idx>");
        self.emit(Instr::LoadInt { dst: idx_reg, val: 0 });

        let (var_reg, var_is_cell) = match (pattern, assign_tgt) {
            (Some(p), _) => {
                self.declare_pattern(p)?;
                (0, false)
            }
            (None, Some(_)) => (0, false),
            (None, None) => {
                let var_name = for_left_name(&f.left)?;
                let r = self.declare_local(&var_name);
                (r, self.cell_regs.contains(&r))
            }
        };

        let top = self.here();
        let cond = self.temp();
        self.emit(Instr::Lt { dst: cond, a: idx_reg, b: len_reg });
        let jf = self.here();
        self.emit(Instr::JumpIfFalse { cond, target: 0 });
        self.next_reg -= 1;

        let save = self.next_reg;
        // Read the current key into a temp (pattern / assignment target) or
        // straight into the loop var (a per-iteration cell var is boxed after).
        let key_dst = if pattern.is_some() || assign_tgt.is_some() {
            self.alloc_reg()
        } else {
            var_reg
        };
        self.emit(Instr::GetIndex { dst: key_dst, obj: keys_reg, key: idx_reg });
        if let Some(p) = pattern {
            self.extract_pattern(p, key_dst)?;
        } else if let Some(tgt) = assign_tgt {
            self.assign_target(tgt, key_dst)?;
        } else if var_is_cell {
            // Per-iteration binding: a FRESH cell each iteration (for-in let).
            self.emit(Instr::MakeCell { reg: var_reg });
        }
        self.next_reg = save;

        self.loop_ctx.push(LoopCtx::loop_frame(self.pending_label.take(), self.handler_depth));
        self.stmt(&f.body)?;
        let ctx = self.loop_ctx.pop().unwrap();
        let cont = self.here();
        for c in ctx.continue_jumps {
            self.patch_jump(c, cont); // continue → increment + re-test
        }
        self.emit(Instr::AddInt { dst: idx_reg, a: idx_reg, imm: 1 });
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
                // normalized). Parse to i128 (our BigInt repr); too-large literals
                // are out of range for now.
                let v = b.value.as_str().parse::<i128>().map_err(|_| {
                    "BigInt literals beyond i128 are not in the zipp-vm subset yet".to_string()
                })?;
                self.emit(Instr::LoadBigInt { dst, value: v });
                Ok(dst)
            }
            E::RegExpLiteral(r) => {
                // `/pat/flags` → NewRegExp (compiles via the `regress` engine at
                // runtime). pattern.text is the source; flags as the JS flag string.
                let pat = self.add_string_const(r.regex.pattern.text.as_str());
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
                let idx = self.add_string_const(s.value.as_str());
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
                let q0 = t.quasis[0].value.cooked.as_ref().map(|s| s.as_str()).unwrap_or("");
                let idx = self.add_string_const(q0);
                self.emit(Instr::LoadConst { dst, idx });
                for (i, e) in t.expressions.iter().enumerate() {
                    let r = self.expr(e)?;
                    let rs = self.temp();
                    self.emit(Instr::ToStr { dst: rs, a: r });
                    self.emit(Instr::Add { dst, a: dst, b: rs });
                    if let Some(qe) = t.quasis.get(i + 1) {
                        let q = qe.value.cooked.as_ref().map(|s| s.as_str()).unwrap_or("");
                        if !q.is_empty() {
                            let qidx = self.add_string_const(q);
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
                match id.name.as_str() {
                    "undefined" => {
                        self.emit(Instr::LoadUndefined { dst });
                        return Ok(dst);
                    }
                    "NaN" => {
                        let idx = self.add_const(Value::num(f64::NAN));
                        self.emit(Instr::LoadConst { dst, idx });
                        return Ok(dst);
                    }
                    "Infinity" => {
                        let idx = self.add_const(Value::num(f64::INFINITY));
                        self.emit(Instr::LoadConst { dst, idx });
                        return Ok(dst);
                    }
                    _ => {}
                }
                // A parameter referenced before its own left-to-right
                // initialization — `(x = x)` (self) or `(x = y, y)` (forward) — is
                // in the Temporal Dead Zone: reading it throws a ReferenceError.
                if self.param_tdz.contains(id.name.as_str()) {
                    let e = self.alloc_reg();
                    self.emit(Instr::NewError { dst: e, kind: 4, arg: None, opts: None });
                    self.emit(Instr::Throw { src: e });
                    return Ok(dst);
                }
                match self.resolve(id.name.as_str()) {
                    Binding::Local(r) => Ok(r), // already in a register
                    Binding::LocalCell(cell) => {
                        self.emit(Instr::CellGet { dst, cell });
                        Ok(dst)
                    }
                    Binding::Upvalue(idx) => {
                        self.emit(Instr::UpvalGet { dst, idx });
                        Ok(dst)
                    }
                    Binding::Global(idx) => {
                        self.emit(Instr::LoadGlobal { dst, idx });
                        Ok(dst)
                    }
                }
            }
            E::ThisExpression(_) => {
                // `this` lives in register 0 of the current function, unless a
                // static field initializer has redirected it to the class value.
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
                // array and construct via NewSpread instead.
                let callee = self.expr(&n.callee)?;
                if n.arguments.iter().any(|a| a.as_expression().is_none()) {
                    let args_arr = self.build_spread_args(&n.arguments)?;
                    self.emit(Instr::NewSpread { dst, callee, args: args_arr });
                    return Ok(dst);
                }
                let (arg_base, argc) = self.eval_args_contiguous(&n.arguments)?;
                self.emit(Instr::New { dst, callee, arg_base, argc });
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
        // `super.name` — read an inherited property through the lexical superclass.
        if matches!(&m.object, ox::Expression::Super(_)) {
            let pid = self.super_class.ok_or("`super.x` is only valid in a derived class")?;
            let name = self.string_name(m.property.name.as_str());
            self.emit(Instr::SuperGet { dst, home_class_id: pid, name });
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
            let pid = self.super_class.ok_or("`super[x]` is only valid in a derived class")?;
            let key = self.expr(&m.expression)?;
            self.emit(Instr::SuperGetComputed { dst, home_class_id: pid, key });
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
    fn emit_optional_check(&mut self, obj: Reg) {
        if self.chain_bails.is_empty() {
            return;
        }
        let save = self.next_reg;
        let nreg = self.alloc_reg();
        self.emit(Instr::LoadNull { dst: nreg });
        let cond = self.alloc_reg();
        self.emit(Instr::LooseEq { dst: cond, a: obj, b: nreg }); // true iff null OR undefined
        let jt = self.here();
        self.emit(Instr::JumpIfTrue { cond, target: 0 });
        self.chain_bails.last_mut().unwrap().push(jt);
        self.next_reg = save; // scratch temps dead after the check
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
        // Build the (cooked + .raw) strings array into a stable register.
        let strings_reg = self.alloc_reg();
        self.build_template_strings(quasi, strings_reg)?;
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
            Tag::Plain(callee) => self.emit(Instr::Call { dst, callee, arg_base, argc }),
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
        // Cooked array → dst.
        let cooked_base = self.next_reg;
        for q in &quasi.quasis {
            let r = self.alloc_reg();
            let cooked = q.value.cooked.as_ref().map(|s| s.as_str()).unwrap_or("");
            let idx = self.add_string_const(cooked);
            self.emit(Instr::LoadConst { dst: r, idx });
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
                                ox::PropertyKey::StringLiteral(s) => s.value.to_string(),
                                ox::PropertyKey::NumericLiteral(n) => fmt_key_num(n.value),
                                _ => return Err("unsupported accessor key in the zipp-vm subset".into()),
                            };
                            let kr = self.alloc_reg();
                            let idx = self.add_string_const(&k);
                            self.emit(Instr::LoadConst { dst: kr, idx });
                            kr
                        };
                        let func = self.expr(&p.value)?;
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
                        // SetFunctionName: a getter/setter is named "get k"/"set k"
                        // (a Symbol key → "get [desc]"), at runtime so a computed key
                        // is handled too.
                        self.emit(Instr::SetFnNameFromKey {
                            func,
                            key,
                            prefix: if is_setter { 2 } else { 1 },
                        });
                    } else if p.computed {
                        // Computed key `{[expr]: v}` → SetIndex.
                        let ke = p.key.as_expression().ok_or("unsupported computed object key")?;
                        let key = self.expr(ke)?;
                        let v = self.expr(&p.value)?;
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
                        self.emit(Instr::SetIndex { obj: dst, key, val: v });
                    } else {
                        // Static identifier / string / number literal key.
                        let key = match &p.key {
                            ox::PropertyKey::StaticIdentifier(id) => id.name.to_string(),
                            ox::PropertyKey::StringLiteral(s) => s.value.to_string(),
                            ox::PropertyKey::NumericLiteral(n) => fmt_key_num(n.value),
                            _ => return Err("unsupported object key in the zipp-vm subset".into()),
                        };
                        let name = self.string_name(&key);
                        // `{ fn: function(){}, m(){}, C: class{} }` — an anonymous
                        // value function/class takes the property key as its name,
                        // EXCEPT `{ __proto__: fn }` (a proto-setter, not a data
                        // property): its function value stays anonymous.
                        let vtmp = self.alloc_reg();
                        let v = if key == "__proto__" && !p.method {
                            self.expr_into(&p.value, vtmp)?
                        } else {
                            self.compile_named_init(vtmp, &p.value, &key)?
                        };
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
                        self.emit(Instr::SetProp { obj: dst, name, val: v });
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
                self.emit(Instr::AddInt { dst, a, imm });
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
                // `a ?? b`: keep `a` unless it is null/undefined. `== undefined`
                // (loose) is true for both null and undefined.
                let save = self.next_reg;
                let undef = self.alloc_reg();
                self.emit(Instr::LoadUndefined { dst: undef });
                let isnull = self.alloc_reg();
                self.emit(Instr::LooseEq { dst: isnull, a: dst, b: undef });
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
                            self.emit(Instr::LoadGlobalOrUndefined { dst, idx });
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
                let obj = self.expr(&m.object)?;
                let name = self.string_name(&m.property.name);
                let strict = self.cx.in_strict;
                self.emit(Instr::DeleteProp { dst, obj, name, strict });
                Ok(dst)
            }
            ox::Expression::ComputedMemberExpression(m) => {
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
                // A binding is non-configurable — `delete` yields `false` — when it is
                // a local (param/`var`/`let`/`const`/function) or a DECLARED global
                // `var`/`let`/`const`. A builtin, an implicitly-created global
                // (`x = 1` with no declaration), or an unresolvable name is
                // configurable / a no-op, so `delete` yields `true`.
                // `NaN`/`Infinity`/`undefined` are the only non-configurable builtin
                // global properties; they're not tracked as compiler globals, so
                // check them by name (a local of that name still resolves below).
                let non_configurable = matches!(id.name.as_str(), "NaN" | "Infinity" | "undefined")
                    || match self.resolve_existing(&id.name) {
                        Some(Binding::Local(_))
                        | Some(Binding::LocalCell(_))
                        | Some(Binding::Upvalue(_)) => true,
                        Some(Binding::Global(slot)) => {
                            self.cx.hoisted_globals.contains(&slot)
                                || self.cx.lexical_globals.contains(&slot)
                                || self.cx.decl_globals.contains(&slot)
                        }
                        None => false,
                    };
                self.emit(Instr::LoadBool { dst, val: !non_configurable });
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
            ox::SimpleAssignmentTarget::StaticMemberExpression(m) => {
                let obj = self.expr(&m.object)?;
                let name = self.string_name(m.property.name.as_str());
                let cur = self.temp();
                self.emit(Instr::GetProp { dst: cur, obj, name });
                let nw = self.temp();
                self.emit(Instr::AddInt { dst: nw, a: cur, imm: delta });
                self.emit(Instr::SetProp { obj, name, val: nw });
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { cur } });
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
                let nw = self.temp();
                self.emit(Instr::AddInt { dst: nw, a: cur, imm: delta });
                self.emit(Instr::SetIndex { obj, key: keyk, val: nw });
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { cur } });
                return Ok(dst);
            }
            // `obj.#x++` — like a static member, keyed "#x".
            ox::SimpleAssignmentTarget::PrivateFieldExpression(p) => {
                let obj = self.expr(&p.object)?;
                let name = self.string_name(&private_key(&p.field.name));
                let cur = self.temp();
                self.emit(Instr::GetProp { dst: cur, obj, name });
                let nw = self.temp();
                self.emit(Instr::AddInt { dst: nw, a: cur, imm: delta });
                self.emit(Instr::SetProp { obj, name, val: nw });
                self.emit(Instr::Move { dst, src: if u.prefix { nw } else { cur } });
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
        let binding = self.resolve(&name);
        if let Binding::Local(r) = binding {
            if !self.const_regs.contains(&r) {
                // Plain mutable register local: mutate in place.
                if u.prefix {
                    self.emit(Instr::AddInt { dst: r, a: r, imm: delta });
                    if r != dst {
                        self.emit(Instr::Move { dst, src: r });
                    }
                } else {
                    self.emit(Instr::Move { dst, src: r }); // yield old value
                    self.emit(Instr::AddInt { dst: r, a: r, imm: delta });
                }
                return Ok(dst);
            }
        }
        // Cell / upvalue / global / const-local: read into `dst`, compute, store
        // back (store_binding throws for a const after the read + increment).
        let cur = self.load_binding(&binding, dst); // == dst
        if u.prefix {
            self.emit(Instr::AddInt { dst: cur, a: cur, imm: delta });
            self.store_binding(&binding, cur);
            Ok(dst) // dst holds the new value
        } else {
            // Keep the old value in `dst`; compute the new value in a temp.
            let tmp = self.temp();
            self.emit(Instr::AddInt { dst: tmp, a: cur, imm: delta });
            self.store_binding(&binding, tmp);
            self.next_reg -= 1; // reclaim tmp
            Ok(dst) // dst still holds the old value
        }
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
                self.emit(Instr::UpvalGet { dst, idx: *idx });
                dst
            }
            Binding::Global(idx) => {
                self.emit(Instr::LoadGlobal { dst, idx: *idx });
                dst
            }
        }
    }

    /// Emit a write of `src` to `binding`.
    fn store_binding(&mut self, b: &Binding, src: Reg) {
        // Assignment to a `const` binding is a runtime TypeError (PutValue on an
        // immutable binding). The RHS has already been evaluated into `src` (its
        // side effects must happen first), so emit the throw now. Initialization
        // uses Move/CellSet/StoreGlobal directly, never this path.
        let is_const = match b {
            Binding::Local(r) | Binding::LocalCell(r) => self.const_regs.contains(r),
            Binding::Global(idx) => self.cx.const_globals.contains(idx),
            Binding::Upvalue(_) => false, // a const captured by a closure: not tracked
        };
        if is_const {
            let e = self.alloc_reg();
            self.emit(Instr::NewError { dst: e, kind: 1, arg: None, opts: None });
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
            Binding::LocalCell(cell) => self.emit(Instr::CellSet { cell: *cell, src }),
            Binding::Upvalue(idx) => self.emit(Instr::UpvalSet { idx: *idx, src }),
            Binding::Global(idx) => {
                // In strict mode, assigning to an unresolvable (never-declared) global
                // is a ReferenceError, not a silent global creation. A top-level
                // lexical (`let`) binding is likewise checked even in sloppy mode: a
                // store while it is still in its TDZ (UNINITIALIZED) is a ReferenceError.
                if self.cx.in_strict || self.cx.lexical_globals.contains(idx) {
                    self.emit(Instr::StoreGlobalStrict { idx: *idx, src });
                } else {
                    self.emit(Instr::StoreGlobal { idx: *idx, src });
                }
            }
        }
    }

    /// Bind all parameters at function entry, strictly LEFT-TO-RIGHT, applying each
    /// one's `= default` and (for a destructuring pattern) extracting it before
    /// moving to the next. The single interleaved pass is required by the spec:
    /// a later parameter's default may reference an earlier (already-bound)
    /// parameter — `function f([x, y] = [1, 2], z = x + y)` must see x, y bound
    /// when it evaluates `z`. (A two-pass "all defaults, then all destructuring"
    /// order would read those names before the pattern extracted them.)
    fn bind_params(&mut self, params: &ox::FormalParameters) -> R<()> {
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
            T::ArrayAssignmentTarget(arr) => self.assign_array_target(arr, src),
            T::ObjectAssignmentTarget(o) => self.assign_object_target(o, src),
            _ => Err("unsupported destructuring-assignment target in the zipp-vm subset".into()),
        }
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
            M::ArrayAssignmentTarget(arr) => self.assign_array_target(arr, val),
            M::ObjectAssignmentTarget(o) => self.assign_object_target(o, val),
            _ => Err("unsupported destructuring-assignment element in the zipp-vm subset".into()),
        }
    }

    fn assign_array_target(&mut self, arr: &ox::ArrayAssignmentTarget, src_in: Reg) -> R<()> {
        // Array assignment destructuring uses the iterator protocol (like the
        // binding form): normalize the source into an array, pulling only the
        // needed elements (unbounded with `...rest`). IterToArray drives a custom
        // iterable's next()/return() — so a non-array source yields the right
        // values AND the iterator is closed when not fully consumed. A plain array
        // takes IterToArray's no-op fast path, so indexed reads are unchanged.
        let save_top = self.next_reg;
        let count = if arr.rest.is_some() { u32::MAX } else { arr.elements.len() as u32 };
        let src = self.alloc_reg();
        self.emit(Instr::IterToArray { dst: src, src: src_in, count });
        for (i, el) in arr.elements.iter().enumerate() {
            if let Some(maybe) = el {
                let save = self.next_reg;
                let val = self.alloc_reg();
                let idx = self.alloc_reg();
                self.emit(Instr::LoadInt { dst: idx, val: i as i32 });
                self.emit(Instr::GetIndex { dst: val, obj: src, key: idx });
                self.assign_maybe_default(maybe, val)?;
                self.next_reg = save;
            }
        }
        if let Some(rest) = &arr.rest {
            let save = self.next_reg;
            let val = self.alloc_reg();
            self.emit(Instr::ArrayRest { dst: val, src, start: arr.elements.len() as u32 });
            self.assign_target(&rest.target, val)?;
            self.next_reg = save;
        }
        self.next_reg = save_top;
        Ok(())
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
                    // `({key: target} = o)`.
                    let val = self.alloc_reg();
                    self.extract_member(src, &p.name, p.computed, val)?;
                    self.assign_maybe_default(&p.binding, val)?;
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
                // ??= : skip when `val` is NOT null/undefined.
                let save = self.next_reg;
                let undef = self.alloc_reg();
                self.emit(Instr::LoadUndefined { dst: undef });
                let isnull = self.alloc_reg();
                self.emit(Instr::LooseEq { dst: isnull, a: val, b: undef });
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
                let pid = self.super_class.ok_or("`super.x = …` is only valid in a derived class")?;
                let name = self.string_name(m.property.name.as_str());
                if is_logical {
                    self.emit(Instr::SuperGet { dst, home_class_id: pid, name });
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    self.emit(Instr::SuperSet { home_class_id: pid, name, val: dst });
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    self.emit(Instr::SuperSet { home_class_id: pid, name, val: dst });
                } else {
                    let cur = self.temp();
                    self.emit(Instr::SuperGet { dst: cur, home_class_id: pid, name });
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    self.emit(Instr::SuperSet { home_class_id: pid, name, val: dst });
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
                    self.emit(Instr::SetProp { obj, name, val: dst });
                } else {
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
            // `super[k] = v` / compound / logical.
            ox::AssignmentTarget::ComputedMemberExpression(m)
                if matches!(&m.object, ox::Expression::Super(_)) =>
            {
                let pid = self.super_class.ok_or("`super[k] = …` is only valid in a derived class")?;
                let key = self.expr(&m.expression)?;
                let key_reg = self.alloc_reg();
                if key != key_reg {
                    self.emit(Instr::Move { dst: key_reg, src: key });
                }
                if is_logical {
                    self.emit(Instr::SuperGetComputed { dst, home_class_id: pid, key: key_reg });
                    let j = self.emit_logical_skip(a.operator, dst);
                    let v = self.expr_into(&a.right, dst)?;
                    if v != dst {
                        self.emit(Instr::Move { dst, src: v });
                    }
                    self.emit(Instr::SuperSetComputed { home_class_id: pid, key: key_reg, val: dst });
                    let end = self.here();
                    self.patch_jump(j, end);
                } else if matches!(a.operator, Op::Assign) {
                    let val = self.expr_into(&a.right, dst)?;
                    if val != dst {
                        self.emit(Instr::Move { dst, src: val });
                    }
                    self.emit(Instr::SuperSetComputed { home_class_id: pid, key: key_reg, val: dst });
                } else {
                    let cur = self.temp();
                    self.emit(Instr::SuperGetComputed { dst: cur, home_class_id: pid, key: key_reg });
                    let rhs = self.expr(&a.right)?;
                    let instr = compound_assign_instr(a.operator, dst, cur, rhs)
                        .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                    self.emit(instr);
                    self.emit(Instr::SuperSetComputed { home_class_id: pid, key: key_reg, val: dst });
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
        let name = match &a.left {
            ox::AssignmentTarget::AssignmentTargetIdentifier(id) => id.name.to_string(),
            _ => return Err("assignment to non-identifier not in zipp-vm v1".into()),
        };
        // Strict mode: assignment to `eval`/`arguments` is an early SyntaxError.
        strict_name_err(self.cx.in_strict, &name)?;
        let binding = self.resolve(&name);
        match a.operator {
            Op::Assign => {
                // `x = function(){}` / `x = class {}` names the anonymous value
                // after the target (NamedEvaluation), like a declaration.
                // A const local takes the store_binding path so the RHS is evaluated
                // (side effects) and the assignment then throws a TypeError.
                if let Binding::Local(r) = binding {
                    if !self.const_regs.contains(&r) {
                        // Plain mutable local: evaluate the RHS directly into its reg.
                        let v = self.compile_named_init(r, &a.right, &name)?;
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
                let v = self.compile_named_init(dst, &a.right, &name)?;
                if v != dst {
                    self.emit(Instr::Move { dst, src: v });
                }
                self.store_binding(&binding, dst);
                Ok(dst)
            }
            // Logical assignment: `x ||= y` / `x &&= y` / `x ??= y` only assign
            // `y` when the short-circuit condition holds (truthy-skip for ||=,
            // falsy-skip for &&=, non-nullish-skip for ??=).
            Op::LogicalOr | Op::LogicalAnd | Op::LogicalNullish => {
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
                self.store_binding(&binding, dst);
                let end = self.here();
                self.patch_jump(j, end);
                Ok(dst)
            }
            // Arithmetic / bitwise compound assignment (`+= -= *= /= %= **= <<=
            // >>= >>>= |= ^= &=`).
            other => {
                if let Binding::Local(r) = binding {
                    if !self.const_regs.contains(&r) {
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
                // const, after the RHS + arithmetic side effects).
                let cur = self.load_binding(&binding, dst); // == dst
                let rhs = self.expr(&a.right)?;
                let instr = compound_assign_instr(other, dst, cur, rhs)
                    .ok_or("unsupported assignment operator (zipp-vm v1)")?;
                self.emit(instr);
                self.store_binding(&binding, dst);
                Ok(dst)
            }
        }
    }

    fn yield_expr(&mut self, y: &ox::YieldExpression, dst: Reg) -> R<Reg> {
        if !self.in_generator {
            return Err("`yield` is only valid inside a generator (function*)".into());
        }
        if y.delegate {
            // `yield* expr`: lazily delegate to the iterable, yielding each of its
            // elements (drives any iterable via IterNext — generator, array,
            // string, Map, Set). Sent values aren't forwarded and the delegate's
            // own return value is approximated as undefined (both rare).
            let arg = y.argument.as_ref().ok_or("yield* requires an operand")?;
            let save = self.next_reg;
            let iter = self.alloc_reg();
            let v = self.expr_into(arg, iter)?;
            if v != iter {
                self.emit(Instr::Move { dst: iter, src: v });
            }
            let idx = self.alloc_reg();
            self.emit(Instr::LoadInt { dst: idx, val: 0 });
            let elem = self.alloc_reg();
            let done = self.alloc_reg();
            let sink = self.alloc_reg(); // discards the value sent to next()
            let top = self.here();
            self.emit(Instr::IterNext { value_dst: elem, done_dst: done, iter, idx });
            let jdone = self.here();
            self.emit(Instr::JumpIfTrue { cond: done, target: 0 });
            self.emit(Instr::Yield { dst: sink, val: elem });
            self.emit(Instr::Jump { target: top });
            let end = self.here();
            self.patch_jump(jdone, end);
            self.next_reg = save;
            self.emit(Instr::LoadUndefined { dst });
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
        self.emit(Instr::Yield { dst, val });
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
        self.emit(Instr::NewError { dst, kind: kidx, arg, opts });
        // Reclaim the message + options temps (allocated in order).
        self.next_reg -= arg.is_some() as Reg + opts.is_some() as Reg;
        Ok(dst)
    }

    fn call(&mut self, c: &ox::CallExpression, dst: Reg) -> R<Reg> {
        // Optional call `f?.(args)`: evaluate the callee as a VALUE (its own `?.`
        // links short-circuit inside the chain), bail to undefined if it's
        // nullish, else call it. (Uses the general value-call, so `o?.m?.()` calls
        // with `this = undefined` — a rare edge.)
        if c.optional {
            let callee = self.expr(&c.callee)?;
            self.emit_optional_check(callee);
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
                self.emit(Instr::LoadUndefined { dst });
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
            // `super.m(...args)` — a StaticMemberExpression whose object is `super`
            // (which is not a value, so it must be handled before the generic
            // StaticMember case evaluates the object).
            if let ox::Expression::StaticMemberExpression(m) = &c.callee {
                if matches!(&m.object, ox::Expression::Super(_)) {
                    let pid = self
                        .super_class
                        .ok_or("`super.method(...)` is only valid in a derived class")?;
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
            self.emit(Instr::LoadUndefined { dst }); // `super()` yields undefined here
            return Ok(dst);
        }
        // `super.method(args)` — call an inherited method with the current `this`.
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            if matches!(&m.object, ox::Expression::Super(_)) {
                let pid = self
                    .super_class
                    .ok_or("`super.method(...)` is only valid in a derived class")?;
                let name = self.string_name(m.property.name.as_str());
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                self.emit(Instr::SuperMethod { dst, home_class_id: pid, name, arg_base, argc });
                return Ok(dst);
            }
        }

        // Bare `Error("msg")` call (no `new`) → same Error object.
        if let ox::Expression::Identifier(id) = &c.callee {
            if let Some(kind) = error_ctor(&id.name) {
                return self.build_error(kind, &c.arguments, dst);
            }
        }
        // Direct `eval(code)` from STRICT-mode code: the evaluated string inherits
        // strict mode (a direct eval shares the caller's strictness). Only fires for
        // the unshadowed global `eval` — an enclosing user binding named `eval`
        // (legal only in sloppy code, hence reachable here as an upvalue/local) is
        // an ordinary call. Sloppy direct eval and indirect eval are untouched: they
        // still route through the generic `Call` → `GLOBAL_EVAL` native (sloppy).
        if let ox::Expression::Identifier(id) = &c.callee {
            if id.name == "eval"
                && self.cx.in_strict
                && matches!(self.resolve("eval"), Binding::Global(_))
            {
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                let arg = if argc == 0 {
                    let r = self.temp();
                    self.emit(Instr::LoadUndefined { dst: r });
                    r
                } else {
                    arg_base
                };
                self.emit(Instr::DirectEval { dst, arg, new_target_ok: self.cx.new_target_ok });
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

        // Method call `obj.name(args…)` → CallMethod, binding `this` to obj.
        // (Computed-member calls `obj[k](…)` fall through to the generic path.)
        if let ox::Expression::StaticMemberExpression(m) = &c.callee {
            let obj = self.expr(&m.object)?;
            if m.optional {
                self.emit_optional_check(obj); // `obj?.method()` — short-circuit if obj nullish
            }
            let name = self.string_name(m.property.name.as_str());
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::CallMethod { dst, obj, name, arg_base, argc });
            return Ok(dst);
        }
        // Private method call `obj.#m(args…)` → CallMethod on the "#m" key.
        if let ox::Expression::PrivateFieldExpression(p) = &c.callee {
            let obj = self.expr(&p.object)?;
            let name = self.string_name(&private_key(&p.field.name));
            let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
            self.emit(Instr::CallMethod { dst, obj, name, arg_base, argc });
            return Ok(dst);
        }

        // Computed method call `obj[key](args…)` → bind `this` to obj. Evaluate
        // `super[expr](args…)` — computed inherited-method call.
        if let ox::Expression::ComputedMemberExpression(m) = &c.callee {
            if matches!(&m.object, ox::Expression::Super(_)) {
                let pid = self.super_class.ok_or("`super[x](...)` is only valid in a derived class")?;
                let key = self.expr(&m.expression)?;
                let key_reg = self.alloc_reg();
                if key != key_reg {
                    self.emit(Instr::Move { dst: key_reg, src: key });
                }
                let (arg_base, argc) = self.eval_args_contiguous(&c.arguments)?;
                self.emit(Instr::SuperMethodComputed { dst, home_class_id: pid, key: key_reg, arg_base, argc });
                return Ok(dst);
            }
        }
        // obj and the key into stable registers (below the contiguous arg block).
        if let ox::Expression::ComputedMemberExpression(m) = &c.callee {
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

/// A class member's (non-computed) name. Computed `[expr]` and `#private` names
/// are out of the subset.
fn class_key_name(key: &ox::PropertyKey) -> R<String> {
    match key {
        ox::PropertyKey::StaticIdentifier(id) => Ok(id.name.to_string()),
        ox::PropertyKey::StringLiteral(s) => Ok(s.value.to_string()),
        ox::PropertyKey::NumericLiteral(n) => Ok(fmt_key_num(n.value)),
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

fn collect_hoisted_vars(s: &ox::Statement, out: &mut std::collections::HashSet<String>) {
    use ox::Statement as S;
    match s {
        S::VariableDeclaration(d) if d.kind == ox::VariableDeclarationKind::Var => {
            for decl in &d.declarations {
                capture::collect_pattern_names(&decl.id, out);
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
    if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
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
