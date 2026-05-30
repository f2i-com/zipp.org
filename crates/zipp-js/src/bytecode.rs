//! A compact bytecode for the hot subset of zipp-js.
//!
//! Function bodies that fit the supported subset (numbers/strings/bools, locals,
//! arithmetic & comparison, `if`/`while`/`for`/ternary, plain calls, `return`)
//! are compiled once (see [`crate::compile`]) to a [`Chunk`] and executed by the
//! register/stack VM (see [`crate::vm`]). Everything else falls back to the
//! tree-walking interpreter. Locals are *slot-indexed* (resolved to integers at
//! compile time), so the VM never hashes a variable name or allocates a scope
//! map — the main reason it beats the tree-walker on compute-bound code.

use crate::ast::BinOp;
use crate::value::JsValue;

/// A single VM instruction. The VM is stack-based: most ops pop their inputs from
/// an operand stack and push their result. `Store*` ops *peek* (leave the value
/// on the stack) so an assignment expression yields the assigned value; the
/// statement wrapper emits a `Pop`.
#[derive(Clone)]
pub enum Op {
    /// Push `consts[idx]`.
    PushConst(u32),
    PushUndefined,
    PushNull,
    PushTrue,
    PushFalse,
    /// Discard the top of stack.
    Pop,
    /// Duplicate the top of stack.
    Dup,
    /// Push the value in local slot `n`.
    LoadLocal(u16),
    /// Copy the top of stack into local slot `n` (does NOT pop).
    StoreLocal(u16),
    /// Push the value of free variable `names[idx]` (resolved via the closure
    /// scope chain); ReferenceError if undeclared.
    LoadName(u32),
    /// Assign the top of stack to free variable `names[idx]` (does NOT pop);
    /// creates an implicit global if undeclared (sloppy mode), matching the
    /// tree-walker.
    StoreName(u32),
    /// Pop `r`, pop `l`, push `binop(op, l, r)`.
    Bin(BinOp),
    /// Unary minus / plus / logical-not on the top of stack.
    Neg,
    Pos,
    Not,
    /// Unconditional jump to instruction index.
    Jump(u32),
    /// Pop; jump if falsy.
    JumpIfFalse(u32),
    /// Pop; jump if truthy.
    JumpIfTrue(u32),
    /// Call: the callee and `argc` arguments are on the stack (callee deepest);
    /// pop them, push the result. `this` is undefined (method calls aren't
    /// compiled).
    Call(u8),
    /// Pop and return the top of stack (or undefined if empty).
    Return,
}

/// A compiled function body. `nlocals` slots back the VM's local array; the first
/// `nparams` are filled from the call arguments.
pub struct Chunk {
    pub code: Vec<Op>,
    pub consts: Vec<JsValue>,
    pub names: Vec<String>,
    pub nlocals: usize,
    pub nparams: usize,
}
