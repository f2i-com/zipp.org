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

    Not { dst: Reg, a: Reg },

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

    /// Call `callee` with `argc` arguments staged in registers
    /// `[arg_base, arg_base+argc)`. Result lands in `dst`.
    Call { dst: Reg, callee: Reg, arg_base: Reg, argc: u16 },

    /// Return `src` from the current function.
    Return { src: Reg },
    /// Return undefined.
    ReturnUndefined,

    /// `console.log`-style print of `argc` values starting at `arg_base`.
    /// A dedicated opcode keeps the v1 stdlib trivial; later this becomes an
    /// ordinary builtin call.
    Print { arg_base: Reg, argc: u16 },
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
}

/// A whole program: the top-level function plus every nested function, indexed
/// by id. Function id `0` is the top-level script body.
#[derive(Clone, Debug)]
pub struct Program {
    pub functions: Vec<FuncProto>,
    pub global_count: u32,
}
