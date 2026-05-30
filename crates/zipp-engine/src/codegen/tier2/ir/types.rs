//! IR type definitions.
//!
//! Everything here is data-only: no logic. The builder, verifier, and
//! printer live in sibling modules and operate on these types.
//!
//! The enum sizes aren't performance-critical — tier-2 compile is a
//! cold path amortised over ≥ 500 invocations — but the `Copy` + small
//! shapes are deliberate so pass authors can pattern-match on values
//! without pulling in lifetime complexity.

use std::fmt;

// ─── Identifiers ────────────────────────────────────────────────────────────

/// An SSA value. Produced by exactly one source: a block parameter or
/// the result of a single [`IrOp`]. Uses of a `ValueId` must be
/// dominated by the defining site (verifier enforces this).
///
/// 32-bit; we won't build functions with 4 G values in them.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct ValueId(pub u32);

impl fmt::Display for ValueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// A basic block. Block 0 is the function entry; other blocks are
/// reachable only via explicit terminators.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

/// Identifies a deopt point in the function. Shared by the
/// speculation-inserting [`IrOp::Check*`] family and the [`DeoptPoint`]
/// side table that tells the runtime how to reconstruct tier-0 state.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct DeoptId(pub u32);

impl fmt::Display for DeoptId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "d{}", self.0)
    }
}

// ─── Type lattice ───────────────────────────────────────────────────────────

/// The type of a `ValueId`.  Forms a lattice:
///
/// ```text
///                    Value  (top — NaN-boxed, anything)
///                  /  |  \  \
///               I32 F64 Bool Heap
///                             │
///                          HeapShape(u32)
///                             │
///                          Bottom (unreachable)
/// ```
///
/// Narrowing is always explicit: an `IrOp::CheckI32(v)` produces a
/// value whose type is `I32`, guarded by a deopt. Implicit narrowing
/// is not allowed — that's what makes speculation sound across passes.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ValueType {
    /// Top of the lattice: "any NaN-boxed value". This is the default
    /// for a value that came straight from a bytecode register with no
    /// speculation applied.
    Value,
    /// 32-bit signed integer, unboxed.
    I32,
    /// 64-bit float, unboxed.
    F64,
    /// Boolean, unboxed (1-bit really, but codegen uses whole register).
    Bool,
    /// Any heap object. Further narrowing (to a specific shape) needs
    /// [`ValueType::HeapShape`].
    Heap,
    /// Heap object known to have a specific shape version. Unlocks
    /// direct slot access without the inline-cache probe.
    HeapShape(u32),
    /// Interned-symbol identifier. Used as property/Map keys.
    Sym,
    /// Unreachable — the bottom element. Appears in dead-code paths
    /// after DCE and as the result of `Terminator::Unreachable`.
    Bottom,
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueType::Value => f.write_str("val"),
            ValueType::I32 => f.write_str("i32"),
            ValueType::F64 => f.write_str("f64"),
            ValueType::Bool => f.write_str("bool"),
            ValueType::Heap => f.write_str("heap"),
            ValueType::HeapShape(s) => write!(f, "heap@{}", s),
            ValueType::Sym => f.write_str("sym"),
            ValueType::Bottom => f.write_str("_|_"),
        }
    }
}

// ─── Runtime helpers ────────────────────────────────────────────────────────

/// A symbolic reference to a runtime helper the JIT can call. Resolved
/// to a concrete `extern "C"` function pointer during emit.
///
/// Kept abstract here so the IR layer doesn't depend on the concrete
/// function addresses (which live in `vm/mod.rs`). Each variant
/// documents its signature as (arg types) → return type — all passed
/// via NaN-boxed `u64` at the ABI level.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RuntimeHelper {
    /// `(vm, Value, Value) -> Value`: the Add slow path for generic
    /// mixed-type or string operands.
    AddGeneric,
    /// `(vm, Value, Value) -> Value`: subtraction fallback.
    SubGeneric,
    /// `(vm, Value, Value) -> Value`: multiplication fallback.
    MulGeneric,
    /// `(vm, Value, Value) -> Value`: division fallback.
    DivGeneric,
    /// `(vm, Value, Value) -> Value`: modulo fallback.
    ModGeneric,
    /// `(vm, Value) -> i32`: truthiness coercion (for JumpIfNot).
    ToBool,
    /// `(vm, *const u8, len) -> u32`: intern an inline-string byte run.
    Intern,
}

// ─── Ops ────────────────────────────────────────────────────────────────────

/// An IR operation. Each op produces at most one value; ops that don't
/// produce a usable result (e.g. stores) sit in the op list anyway and
/// their `ValueId` is considered "void" — verifier rejects uses of it.
#[derive(Clone, Debug)]
pub enum IrOp {
    // ── Constants ──────────────────────────────────────────────────
    /// Materialise a compile-time i32.
    ConstI32(i32),
    /// Materialise a compile-time f64 (by bits, to dodge NaN equality).
    ConstF64(u64),
    /// Materialise `true` / `false`.
    ConstBool(bool),
    /// Materialise `null`.
    ConstNull,
    /// Materialise `undefined`.
    ConstUndef,
    /// Materialise a raw NaN-boxed value (escape hatch for constants
    /// that don't fit the narrower variants — e.g. a pre-interned
    /// string Value).
    ConstValue(u64),

    // ── Register-file bridging ─────────────────────────────────────
    /// Read bytecode register `reg` at function entry. Only valid in
    /// the entry block; elsewhere, inter-block value flow uses block
    /// parameters.
    ///
    /// Produces a [`ValueType::Value`] — callers must `Check*` to
    /// narrow.
    LoadReg(u16),

    // ── Arithmetic (typed) ─────────────────────────────────────────
    /// Wrapping i32 addition. Both operands must be i32.
    AddI32(ValueId, ValueId),
    /// Wrapping i32 subtraction.
    SubI32(ValueId, ValueId),
    /// Wrapping i32 multiplication.
    MulI32(ValueId, ValueId),
    /// i32 addition with overflow deopt: produces i32 on no-overflow,
    /// triggers [`DeoptId`] on overflow (caller can retry as f64).
    CheckedAddI32(ValueId, ValueId, DeoptId),
    /// i32 subtraction with overflow deopt.
    CheckedSubI32(ValueId, ValueId, DeoptId),
    /// i32 multiplication with overflow deopt.
    CheckedMulI32(ValueId, ValueId, DeoptId),

    /// f64 addition. Both operands must be f64.
    AddF64(ValueId, ValueId),
    SubF64(ValueId, ValueId),
    MulF64(ValueId, ValueId),
    DivF64(ValueId, ValueId),

    // ── Arithmetic (generic fallback) ──────────────────────────────
    /// Generic add — accepts any two values, dispatches at runtime.
    /// Produced by the translator when feedback is absent or mixed.
    /// Speculation passes may rewrite this to `AddI32` / `AddF64`.
    AddGeneric(ValueId, ValueId),
    SubGeneric(ValueId, ValueId),
    MulGeneric(ValueId, ValueId),
    /// Generic division — accepts any two values. Always goes through
    /// the runtime helper because JS `/` semantics (i32 → f64 widen
    /// on non-exact, +∞ / −∞ for div-by-zero, NaN for non-numeric)
    /// don't have a compact inline form worth open-coding.
    DivGeneric(ValueId, ValueId),
    /// Generic modulo — same rationale as [`Self::DivGeneric`].
    ModGeneric(ValueId, ValueId),

    // ── Unary ──────────────────────────────────────────────────────
    NegI32(ValueId),
    NegF64(ValueId),
    NotBool(ValueId),

    // ── Comparisons ────────────────────────────────────────────────
    EqI32(ValueId, ValueId),
    NeI32(ValueId, ValueId),
    LtI32(ValueId, ValueId),
    LeI32(ValueId, ValueId),
    GtI32(ValueId, ValueId),
    GeI32(ValueId, ValueId),
    /// Generic equality (===). Accepts any two values.
    EqValue(ValueId, ValueId),
    NeValue(ValueId, ValueId),
    /// Loose equality (==). Accepts any two values.
    LooseEqValue(ValueId, ValueId),
    LtValue(ValueId, ValueId),
    LeValue(ValueId, ValueId),

    // ── Type speculation ───────────────────────────────────────────
    /// Assert `v` is an i32. Emits a cheap tag check; deopts otherwise.
    /// Produces a value typed as `I32`.
    CheckI32(ValueId, DeoptId),
    /// Assert `v` is an f64 (unboxed double, not NaN-boxed tag).
    CheckF64(ValueId, DeoptId),
    /// Assert `v` is a heap object (any shape).
    CheckHeap(ValueId, DeoptId),
    /// Assert `v` is a heap object of a specific shape. Unlocks
    /// [`IrOp::LoadSlot`] / [`IrOp::StoreSlot`] without the IC probe.
    CheckHeapShape(ValueId, u32, DeoptId),
    /// Assert `v` is exactly the function with bit-pattern `func_bits`.
    /// Unlocks direct inlining.
    CheckFunctionIs(ValueId, u64, DeoptId),

    // ── Boxing / unboxing ──────────────────────────────────────────
    /// Wrap an `i32` in a NaN-boxed `Value`. Typed input → `Value`.
    BoxI32(ValueId),
    BoxF64(ValueId),
    BoxBool(ValueId),
    /// Unbox a `Value` known to be an `i32`. Undefined if not —
    /// always guard with [`IrOp::CheckI32`] first.
    UnboxI32(ValueId),
    UnboxF64(ValueId),
    UnboxBool(ValueId),

    // ── Memory / heap ──────────────────────────────────────────────
    /// Read a slot at a fixed byte offset from a heap object that's
    /// been narrowed to a specific shape. Only emitted after a
    /// matching `CheckHeapShape`.
    LoadSlot(ValueId, u32, ValueType),
    /// Write a slot at a fixed byte offset.
    StoreSlot(ValueId, u32, ValueId),
    /// Read a global by its slot index.
    LoadGlobal(u16),
    /// Write a global by its slot index.
    StoreGlobal(u16, ValueId),

    // ── Calls ──────────────────────────────────────────────────────
    /// Generic runtime-helper call. The runtime helper handles its own
    /// ABI; the regalloc treats this as a generic call clobbering the
    /// caller-saved regs.
    CallRuntime(RuntimeHelper, Vec<ValueId>),
    /// User-level JS function call. `callee` is a Value that (if it's a
    /// heap-resident function) identifies the target at runtime; the
    /// VM-side `djit_call_helper` handles the full dispatch including
    /// tier-1 / tier-2 / interpreter fall-through. Exists so tier-2
    /// can compile caller functions even when the callee isn't known
    /// at compile time — the arithmetic surrounding the call runs as
    /// native tier-2 code, only the call itself round-trips through
    /// the VM's dispatch path.
    CallValue(ValueId, Vec<ValueId>),
    /// `function(...) { ... }` expression with no captured upvalues.
    /// The constant table at `u32` holds the compiled function; at
    /// runtime we clone it onto the heap and return a reference.
    /// Narrower than the general `MakeClosure` bytecode opcode —
    /// the translator only emits this for count=0 closures.
    MakeClosureNoCapture(u32),

    // ── Convenience ────────────────────────────────────────────────
    /// Pure copy. Introduced by passes (copy-prop normally eliminates
    /// it); also used by the builder for clarity in rare cases.
    Copy(ValueId),
}

impl IrOp {
    /// True if this op's result is *only* produced for side-effects
    /// (never read). `StoreSlot` etc. Verifier enforces that no other
    /// op uses the ValueId attached to a void op.
    pub fn is_void(&self) -> bool {
        matches!(
            self,
            IrOp::StoreSlot(..) | IrOp::StoreGlobal(..)
        )
    }
}

// ─── Terminators ────────────────────────────────────────────────────────────

/// Block exit. Exactly one per block; verifier enforces.
#[derive(Clone, Debug)]
pub enum Terminator {
    /// Function return. `Some(v)` returns `v`; `None` returns undefined.
    Return(Option<ValueId>),
    /// Unconditional jump. `args` are passed as the target block's
    /// parameters; the target block's parameter list must match in
    /// length and types.
    Jump(BlockId, Vec<ValueId>),
    /// Conditional branch on a `Bool`-typed value. Each side carries
    /// its own argument list (they typically share the same live state
    /// but the IR doesn't force that).
    Branch {
        cond: ValueId,
        then_block: BlockId,
        then_args: Vec<ValueId>,
        else_block: BlockId,
        else_args: Vec<ValueId>,
    },
    /// Deoptimise at this point. `live_values` is the set of SSA
    /// values that must be reconstructed into bytecode registers
    /// before resuming tier 0. See [`DeoptPoint::live_regs`].
    Deopt(DeoptId),
    /// Explicitly unreachable (e.g. after a `throw`). No codegen; used
    /// for DCE.
    Unreachable,
}

// ─── Blocks ─────────────────────────────────────────────────────────────────

/// A basic block: straight-line ops, terminator at the end.
#[derive(Clone, Debug)]
pub struct Block {
    pub id: BlockId,
    /// Block parameters — the incoming edges supply arguments at each
    /// jump site. The entry block starts with parameters representing
    /// the function's bytecode arguments.
    pub params: Vec<(ValueId, ValueType)>,
    /// Straight-line operations. Each entry is `(result_id, op)`. For
    /// void ops (side-effect-only) the result_id is still allocated
    /// but never referenced.
    pub ops: Vec<(ValueId, IrOp)>,
    /// Block exit.
    pub term: Terminator,
}

// ─── Deopt metadata ─────────────────────────────────────────────────────────

/// How a single SSA value should appear at the tier-0 hand-off time.
///
/// Populated during phase 4 (register allocation). Phase 1 only
/// tracks `ValueId` — the concrete storage is decided later.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Location {
    /// Currently in a physical register (by register index as assigned
    /// by the allocator).
    Reg(u8),
    /// Spilled to a stack slot at the given byte offset from the
    /// frame base.
    Spill(u32),
    /// Known constant — doesn't need a live register.
    Constant(u64),
    /// Phase-1 placeholder: regalloc hasn't run yet, so we only know
    /// the SSA value.
    Unallocated(ValueId),
}

/// Information needed to resume tier 0 at a specific deopt point.
///
/// One entry per `DeoptId` in the function. Filled in during phase 1
/// with `Unallocated` locations; regalloc in phase 4 rewrites them to
/// `Reg`/`Spill`/`Constant`.
#[derive(Clone, Debug)]
pub struct DeoptPoint {
    pub id: DeoptId,
    /// Where to resume execution inside the tier-0 bytecode. Points
    /// at the instruction the tier-2 code was trying to speculatively
    /// skip past — tier 0 re-executes from here without the
    /// speculation.
    pub bytecode_ip: usize,
    /// Bytecode registers live at this point and their current SSA
    /// origin. The runtime writes these back to the VM stack window
    /// before transferring control.
    pub live_regs: Vec<(u16, Location)>,
}

// ─── Functions ──────────────────────────────────────────────────────────────

/// A whole function in tier-2 IR.
#[derive(Clone, Debug)]
pub struct IrFunction {
    /// Source bytecode size — stored so passes can budget against it
    /// (e.g. refuse to inline if IR already grew past some multiple).
    pub bytecode_len: usize,
    /// Number of bytecode registers (== `CompiledFunctionObject::register_count`).
    /// Determines how many arguments the entry block takes.
    pub num_bytecode_regs: u16,
    /// Number of parameters the function was called with.
    pub num_parameters: u16,
    /// Blocks in index order; `blocks[0]` is the entry block.
    pub blocks: Vec<Block>,
    /// Deopt metadata; `deopt_points[i]` corresponds to `DeoptId(i as u32)`.
    pub deopt_points: Vec<DeoptPoint>,
    /// Source constants table — passes need it to evaluate
    /// constant-folded expressions.
    pub constants: std::rc::Rc<Vec<crate::object::Object>>,
}

impl IrFunction {
    /// Total IR value count — used for allocating side-arrays indexed
    /// by `ValueId`.
    pub fn num_values(&self) -> usize {
        // Every ValueId is allocated sequentially by the builder, so
        // max + 1 across all block params and ops is the count.
        let mut max_id = 0u32;
        for block in &self.blocks {
            for (vid, _) in &block.params {
                max_id = max_id.max(vid.0);
            }
            for (vid, _) in &block.ops {
                max_id = max_id.max(vid.0);
            }
        }
        (max_id + 1) as usize
    }

    /// Lookup a block by id. Panics on an out-of-range id, which would
    /// indicate a mal-constructed IR — the verifier catches this.
    pub fn block(&self, id: BlockId) -> &Block {
        &self.blocks[id.0 as usize]
    }

    /// Mutable block lookup (passes only).
    pub fn block_mut(&mut self, id: BlockId) -> &mut Block {
        &mut self.blocks[id.0 as usize]
    }
}

// ─── Builder-side error types ───────────────────────────────────────────────

/// What can go wrong while translating bytecode → IR.
///
/// All variants are "internal compiler problems" from the caller's
/// perspective: a well-formed [`Bytecode`](crate::backend::bytecode::Bytecode)
/// should always translate.  If a `BuildError` escapes, the tier-2
/// pipeline falls back to running tier 1 / tier 0 for the function.
#[derive(Clone, Debug)]
pub enum BuildError {
    /// Encountered an opcode this phase doesn't support yet.
    ///
    /// Phase 1 deliberately covers a narrow subset (arithmetic +
    /// control flow); later phases extend this.
    UnsupportedOp(&'static str, usize /*ip*/),
    /// Instruction decodes to a byte that isn't a valid ROp.
    BadOpcode(u8, usize /*ip*/),
    /// A jump target doesn't land on an instruction boundary.
    BadJumpTarget { from_ip: usize, target: usize },
    /// A `LoadConst` references an out-of-range constant-table entry.
    BadConstIdx { at_ip: usize, idx: usize },
    /// An operand references a register outside the function's
    /// declared window.
    BadRegister { at_ip: usize, reg: u16 },
    /// Bytecode ended without a proper terminator (Halt / Return).
    /// The validator should normally catch this before we get here,
    /// but translation re-checks to stay robust.
    MissingTerminator,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::UnsupportedOp(name, ip) => {
                write!(f, "tier-2 phase 1 doesn't support opcode {name:?} at ip {ip}")
            }
            BuildError::BadOpcode(b, ip) => write!(f, "invalid opcode byte {b:#04x} at ip {ip}"),
            BuildError::BadJumpTarget { from_ip, target } => {
                write!(f, "jump at ip {from_ip} targets non-instruction offset {target}")
            }
            BuildError::BadConstIdx { at_ip, idx } => {
                write!(f, "const-index {idx} out of range at ip {at_ip}")
            }
            BuildError::BadRegister { at_ip, reg } => {
                write!(f, "register r{reg} out of range at ip {at_ip}")
            }
            BuildError::MissingTerminator => f.write_str("bytecode is missing a terminator"),
        }
    }
}

impl std::error::Error for BuildError {}
