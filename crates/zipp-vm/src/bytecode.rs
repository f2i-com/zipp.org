//! Register bytecode.
//!
//! Each function compiles to a flat `Vec<Instr>` over a fixed register file
//! (`reg_count` slots, indexed `u16`). Registers — not a value stack — are the
//! addressing mode, because that is the form a register-allocating JIT maps
//! directly onto machine registers. Operands are register indices, small
//! immediates, or constant-pool indices.
//!
//! The instruction set is deliberately small and regular; it favours
//! three-address arithmetic so a value can stay in one place across a basic
//! block (and, later, across a call).

use crate::value::Value;

/// A register index within a function's frame.
pub type Reg = u16;

/// One bytecode instruction. Kept as a fieldful enum (not packed bytes) for v1:
/// the dispatch cost of a wide enum is negligible next to correctness clarity,
/// and the JIT will consume this same structured form rather than re-decoding
/// bytes.
#[derive(Clone, Debug)]
pub enum Instr {
    /// `dst = <constant pool[idx]>`
    LoadConst { dst: Reg, idx: u32 },
    /// `dst = <small integer immediate>`
    LoadInt { dst: Reg, val: i32 },
    /// `dst = undefined`
    LoadUndefined { dst: Reg },
    /// `dst = null`
    LoadNull { dst: Reg },
    /// `dst = true|false`
    LoadBool { dst: Reg, val: bool },
    /// `dst = src`
    Move { dst: Reg, src: Reg },

    /// `dst = globals[idx]`
    LoadGlobal { dst: Reg, idx: u32 },
    /// `globals[idx] = src`
    StoreGlobal { idx: u32, src: Reg },

    /// A clock read: `performance.now()` (`epoch = false`, fractional ms since
    /// VM start) or `Date.now()` (`epoch = true`, integer ms since the Unix
    /// epoch). Both yield an f64 `Value`. Recognised at compile time so the
    /// common timing idiom works without a real global object model.
    Now { dst: Reg, epoch: bool },

    // ── arithmetic (generic: operands may be any number) ──
    Add { dst: Reg, a: Reg, b: Reg },
    Sub { dst: Reg, a: Reg, b: Reg },
    Mul { dst: Reg, a: Reg, b: Reg },
    Div { dst: Reg, a: Reg, b: Reg },
    Mod { dst: Reg, a: Reg, b: Reg },
    Neg { dst: Reg, a: Reg },

    /// `dst = a + <int immediate>` — the canonical `i + 1`, `n - 1` shape.
    AddInt { dst: Reg, a: Reg, imm: i32 },

    // ── comparisons → boolean ──
    Lt { dst: Reg, a: Reg, b: Reg },
    Le { dst: Reg, a: Reg, b: Reg },
    Gt { dst: Reg, a: Reg, b: Reg },
    Ge { dst: Reg, a: Reg, b: Reg },
    /// strict `===`
    Eq { dst: Reg, a: Reg, b: Reg },
    /// strict `!==`
    Ne { dst: Reg, a: Reg, b: Reg },
    /// loose `==` (with type coercion)
    LooseEq { dst: Reg, a: Reg, b: Reg },
    /// loose `!=` (with type coercion)
    LooseNe { dst: Reg, a: Reg, b: Reg },

    Not { dst: Reg, a: Reg },
    /// `dst = typeof a` (a JS type-name string).
    TypeOf { dst: Reg, a: Reg },

    // ── control flow (targets are instruction indices) ──
    Jump { target: u32 },
    /// Jump if `cond` is falsy.
    JumpIfFalse { cond: Reg, target: u32 },
    /// Jump if `cond` is truthy.
    JumpIfTrue { cond: Reg, target: u32 },

    /// Fused compare-and-branch: `if !(a < b) goto target`. Keeps the common
    /// loop/recursion guard in one instruction so the boolean never has to be
    /// materialised into a register.
    JumpIfNotLt { a: Reg, b: Reg, target: u32 },
    JumpIfNotLe { a: Reg, b: Reg, target: u32 },

    // ── reference types ──
    /// `dst = <function object for functions[func_id]>`. Capture-free: used for
    /// functions that reference no enclosing variables.
    MakeFunc { dst: Reg, func_id: u32 },
    /// `dst = <closure over functions[func_id]>` capturing upvalue cells named
    /// by `functions[func_id].upvalues`. Each upvalue source is resolved in the
    /// CURRENT (defining) frame: either a local register that holds a cell, or
    /// one of the current frame's own upvalues (for nested-of-nested capture).
    MakeClosure { dst: Reg, func_id: u32 },

    /// Box the value currently in `reg` into a fresh heap Cell and write the
    /// cell reference back into `reg`. Emitted for a captured local/param so
    /// later reads/writes go through the shared cell.
    MakeCell { reg: Reg },
    /// `dst = *<cell in reg>` — read a captured local's cell.
    CellGet { dst: Reg, cell: Reg },
    /// `*<cell in reg> = src` — write a captured local's cell.
    CellSet { cell: Reg, src: Reg },
    /// `dst = *<upvalue[idx]>` — read one of this closure's captured cells.
    UpvalGet { dst: Reg, idx: u16 },
    /// `*<upvalue[idx]> = src` — write one of this closure's captured cells.
    UpvalSet { idx: u16, src: Reg },
    /// `dst = [reg[arg_base], …, reg[arg_base+argc-1]]` — array literal.
    NewArray { dst: Reg, arg_base: Reg, argc: u16 },
    /// `dst = {}` — empty object (populated by following SetProp/SetIndex).
    NewObject { dst: Reg },
    /// `dst = <array of obj's own enumerable string keys>` — drives `for-in`.
    /// For an array, the keys are the index strings "0".."len-1".
    ObjectKeys { dst: Reg, obj: Reg },
    /// `dst = <length of array/string in obj>` (0 for anything else). Used by
    /// the `for-of` desugaring's bound check.
    LenOf { dst: Reg, obj: Reg },
    /// `dst = obj[key]` — computed member read (array element or object prop).
    GetIndex { dst: Reg, obj: Reg, key: Reg },
    /// `obj[key] = val` — computed member write.
    SetIndex { obj: Reg, key: Reg, val: Reg },
    /// `dst = obj.<string_constants[name]>` — static property read
    /// (also resolves `.length` for arrays/strings).
    GetProp { dst: Reg, obj: Reg, name: u32 },
    /// `obj.<string_constants[name]> = val` — static property write.
    SetProp { obj: Reg, name: u32, val: Reg },

    /// Call `callee` with `argc` arguments staged in registers
    /// `[arg_base, arg_base+argc)`. Result lands in `dst`.
    Call { dst: Reg, callee: Reg, arg_base: Reg, argc: u16 },

    /// `dst = obj.<string_constants[name]>(args…)` — method call with `this`
    /// bound to `obj`. Arguments occupy `[arg_base, arg_base+argc)`.
    CallMethod { dst: Reg, obj: Reg, name: u32, arg_base: Reg, argc: u16 },

    /// Throw the value in `src`. Unwinds to the nearest enclosing catch handler
    /// (in this or a caller frame), or aborts the program if none.
    Throw { src: Reg },
    /// Push a try-handler: on a throw before the matching `PopHandler`, control
    /// jumps to `catch_target` with the thrown value placed in `catch_reg`.
    PushHandler { catch_target: u32, catch_reg: Reg },
    /// Pop the most recent try-handler (reached when the try block completes
    /// without throwing).
    PopHandler,

    /// Return `src` from the current function.
    Return { src: Reg },
    /// Return undefined.
    ReturnUndefined,

    /// `console.log`-style print of `argc` values starting at `arg_base`.
    /// A dedicated opcode keeps the v1 stdlib trivial; later this becomes an
    /// ordinary builtin call. `to_stderr` is set for `console.error`/`warn`
    /// (which write to stderr in node), clear for `log`/`info`/`debug`.
    Print { arg_base: Reg, argc: u16, to_stderr: bool },
}

/// A compiled function: its code, register-file size, parameter count, and the
/// constant pool it references.
#[derive(Clone, Debug)]
pub struct FuncProto {
    pub name: String,
    pub code: Vec<Instr>,
    pub reg_count: u16,
    pub param_count: u16,
    pub constants: Vec<Value>,
    /// Heap-string constants referenced by `LoadConst` need their text; this
    /// parallels `constants` for the string case (resolved at load time).
    pub string_constants: Vec<String>,
    /// If this function's name is hoisted to a global binding, the slot index;
    /// the VM materialises a function object into that global at startup.
    pub name_global: Option<u16>,
    /// Upvalues this function captures, in order. Index `i` of a `UpvalGet`/
    /// `UpvalSet` refers to `upvalues[i]`. Each entry says where the DEFINING
    /// frame finds the cell to capture: a local register holding a cell, or one
    /// of the defining frame's own upvalues (nested-of-nested capture).
    pub upvalues: Vec<UpvalSource>,
}

/// Where a closure's upvalue is sourced from, evaluated in the defining frame.
#[derive(Clone, Copy, Debug)]
pub enum UpvalSource {
    /// Capture the cell currently in the defining frame's register `reg`.
    ParentLocal(Reg),
    /// Capture the defining frame's own upvalue `idx` (re-capture up the chain).
    ParentUpval(u16),
}

/// A whole program: the top-level function plus every nested function, indexed
/// by id. Function id `0` is the top-level script body.
#[derive(Clone, Debug)]
pub struct Program {
    pub functions: Vec<FuncProto>,
    pub global_count: u32,
}
