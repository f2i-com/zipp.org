//! Bytecode → IR translator.
//!
//! Two-pass algorithm, intentionally simple:
//!
//! ### Pass 1: locate basic blocks
//!
//! Walk the bytecode, marking:
//!
//! * `0` — the entry point.
//! * Every jump target (falling through a jump-target mid-instruction
//!   is a validator error and propagates to [`BuildError`]).
//! * The instruction immediately after every branch terminator (so a
//!   fall-through into the next block gets its own `BlockId`).
//!
//! These offsets become the block starts. `offset_to_block` maps each
//! bytecode IP to the containing block.
//!
//! ### Pass 2: emit SSA
//!
//! For each block in IP order, simulate the register file as an SSA
//! name-stack: `reg_state[reg] = Option<ValueId>`. When we hit a join
//! point (block with multiple predecessors), each live-in register
//! becomes a block parameter.
//!
//! This is the **local-SSA** approach — simpler than full
//! Cytron-et-al. SSA construction, and sufficient for our bytecode
//! which rarely has complex joins (structured control flow from a
//! source-level compiler keeps things tame).
//!
//! What we **don't** do in this phase:
//!
//! * Constant folding (pass 3).
//! * Type speculation (pass 5).
//! * Inlining (pass 8).
//! * Any op that involves calls, property access, or heap allocation
//!   — translator returns [`BuildError::UnsupportedOp`] for those.
//!
//! The purpose is to cover the loop-benchmark-relevant subset end to
//! end so later phases have something to optimise.

use std::rc::Rc;

use crate::object::Object;
use crate::rcode::ROp;

use super::types::{
    Block, BlockId, BuildError, IrFunction, IrOp, Terminator, ValueId, ValueType,
};

// ─── Helpers for walking bytecode ───────────────────────────────────────────

fn read_u16(inst: &[u8], offset: usize) -> u16 {
    ((inst[offset] as u16) << 8) | (inst[offset + 1] as u16)
}

fn read_u32(inst: &[u8], offset: usize) -> u32 {
    ((inst[offset] as u32) << 24)
        | ((inst[offset + 1] as u32) << 16)
        | ((inst[offset + 2] as u32) << 8)
        | (inst[offset + 3] as u32)
}

/// Return the size of the instruction starting at `ip`, or `None` if
/// the byte doesn't decode. This matches `ROp::size` with the variable-
/// size `MakeClosure` handling.
fn instruction_size(inst: &[u8], ip: usize) -> Result<usize, BuildError> {
    let op_byte = inst[ip];
    let op = ROp::from_byte(op_byte).ok_or(BuildError::BadOpcode(op_byte, ip))?;
    let sz = match op {
        ROp::MakeClosure => {
            // [op:1, dst:2, const_idx:2, count:1, ...slots:2*count]
            if ip + 5 >= inst.len() {
                return Err(BuildError::MissingTerminator);
            }
            let count = inst[ip + 5] as usize;
            6 + count * 2
        }
        _ => op.size(),
    };
    Ok(sz)
}

// ─── Public entry point ─────────────────────────────────────────────────────

/// Translate a function's bytecode into the tier-2 IR.
///
/// `instructions` and `constants` come straight from a
/// [`CompiledFunctionObject`](crate::object::CompiledFunctionObject).
/// `num_bytecode_regs` is `register_count`; `num_parameters` is the
/// parameter count.
///
/// The entry block has one block parameter per bytecode register;
/// subsequent blocks are built with explicit parameters for joined
/// values.
pub fn translate(
    instructions: &[u8],
    constants: Rc<Vec<Object>>,
    num_bytecode_regs: u16,
    num_parameters: u16,
) -> Result<IrFunction, BuildError> {
    if instructions.is_empty() {
        return Err(BuildError::MissingTerminator);
    }

    // ── Pass 1: locate block starts ────────────────────────────────
    let block_starts = find_block_starts(instructions)?;
    let offset_to_block = build_offset_to_block(&block_starts);

    // ── Pass 2: translate each block ───────────────────────────────
    let mut builder = Builder::new(
        instructions,
        constants.clone(),
        num_bytecode_regs,
        num_parameters,
    );

    // Allocate the block-id shells up front so cross-block references
    // already have a BlockId when we emit jumps.
    for (idx, _start) in block_starts.iter().enumerate() {
        builder.blocks.push(Block {
            id: BlockId(idx as u32),
            params: Vec::new(),
            ops: Vec::new(),
            term: Terminator::Unreachable,
        });
    }

    // Entry block: parameters = one-per-bytecode-register.
    // This gives us a stable starting point; later blocks synthesise
    // their own params as needed when they have multiple predecessors.
    let entry_params = builder.alloc_entry_params(num_bytecode_regs);
    builder.blocks[0].params = entry_params.clone();

    // Per-block register snapshots. For block 0 the snapshot is just
    // the entry params; for later blocks we fill in as we translate.
    let mut block_reg_in: Vec<Option<Vec<Option<ValueId>>>> =
        vec![None; block_starts.len()];
    let mut initial = vec![None; num_bytecode_regs as usize];
    for (i, (vid, _)) in entry_params.iter().enumerate() {
        initial[i] = Some(*vid);
    }
    block_reg_in[0] = Some(initial);

    // Translate blocks in IP order. Since our bytecode comes from a
    // structured source compiler, the straightforward IP-ordered walk
    // is a reasonable approximation to a reverse-postorder traversal.
    //
    // Every non-entry block needs its parameters allocated *before*
    // we translate it: jumps to it (emitted in predecessor blocks)
    // must pass the right number of arguments. The simplest reliable
    // rule — used here — is "always allocate one block param per
    // bytecode register," matching `collect_live_args` on the
    // predecessor side. A later pass (dead-block-param removal) can
    // shrink the parameter list once it has global liveness info;
    // phase 1 doesn't need that refinement.
    for block_idx in 1..block_starts.len() {
        let params = builder.alloc_entry_params(num_bytecode_regs);
        builder.blocks[block_idx].params = params.clone();
        // The register snapshot for this block is its params — but
        // only if a predecessor hasn't already filled it in. The first
        // arm of `block_reg_in` wins; predecessors jumping here pass
        // whatever they have.
        if block_reg_in[block_idx].is_none() {
            let mut regs = vec![None; num_bytecode_regs as usize];
            for (i, (vid, _)) in params.iter().enumerate() {
                regs[i] = Some(*vid);
            }
            block_reg_in[block_idx] = Some(regs);
        } else {
            // Replace whatever a predecessor recorded with the block's
            // own fresh param names — that's the SSA convention, and
            // keeps each block's interior independent of its callers.
            let mut regs = vec![None; num_bytecode_regs as usize];
            for (i, (vid, _)) in params.iter().enumerate() {
                regs[i] = Some(*vid);
            }
            block_reg_in[block_idx] = Some(regs);
        }
    }

    for (block_idx, &start) in block_starts.iter().enumerate() {
        let end = block_starts.get(block_idx + 1).copied().unwrap_or(instructions.len());

        let regs_in = block_reg_in[block_idx]
            .clone()
            .expect("block_reg_in pre-populated above");

        translate_block(
            &mut builder,
            BlockId(block_idx as u32),
            start,
            end,
            regs_in,
            &offset_to_block,
            &mut block_reg_in,
        )?;
    }

    Ok(IrFunction {
        bytecode_len: instructions.len(),
        num_bytecode_regs,
        num_parameters,
        blocks: builder.blocks,
        deopt_points: Vec::new(),
        constants,
    })
}

// ─── Pass 1 ─────────────────────────────────────────────────────────────────

/// Compute the set of bytecode offsets that start a basic block.
/// Always sorted + unique; always includes 0.
fn find_block_starts(inst: &[u8]) -> Result<Vec<usize>, BuildError> {
    let mut starts = std::collections::BTreeSet::new();
    starts.insert(0usize);

    let mut ip = 0;
    while ip < inst.len() {
        let op_byte = inst[ip];
        let op = ROp::from_byte(op_byte).ok_or(BuildError::BadOpcode(op_byte, ip))?;
        let size = instruction_size(inst, ip)?;

        match op {
            ROp::Jump => {
                let target = read_u32(inst, ip + 1) as usize;
                starts.insert(target);
                // Instruction after a Jump starts a new block (even
                // though it may only be reached by a jump from
                // elsewhere; the validator guarantees all offsets are
                // instruction-aligned).
                if ip + size < inst.len() {
                    starts.insert(ip + size);
                }
            }
            ROp::JumpIfNot | ROp::JumpIfTruthy => {
                // Layout: [cond:2, target:4]
                let target = read_u32(inst, ip + 3) as usize;
                starts.insert(target);
                if ip + size < inst.len() {
                    starts.insert(ip + size);
                }
            }
            ROp::IncrementRegAndJump
            | ROp::TestLtConstJump
            | ROp::TestLeConstJump
            | ROp::TestLtRegJump
            | ROp::TestLeRegJump => {
                // Layout: [r:2, x:2, target:4]
                let target = read_u32(inst, ip + 5) as usize;
                starts.insert(target);
                if ip + size < inst.len() {
                    starts.insert(ip + size);
                }
            }
            ROp::ModRegConstStrictEqConstJump | ROp::TestModRegStrictEqConstJump => {
                // Layout: [r:2, mod:2, cmp:2, target:4]
                let target = read_u32(inst, ip + 7) as usize;
                starts.insert(target);
                if ip + size < inst.len() {
                    starts.insert(ip + size);
                }
            }
            ROp::Return | ROp::ReturnUndef | ROp::Halt | ROp::HaltValue => {
                // Everything after a terminator is a new block
                // (if any code follows — usually nothing does).
                if ip + size < inst.len() {
                    starts.insert(ip + size);
                }
            }
            _ => {}
        }

        ip += size;
    }

    // Validate that every collected start actually aligns with an
    // instruction. This catches both off-by-one bugs in the decoder
    // above and malicious bytecode.
    let mut all_boundaries = std::collections::BTreeSet::new();
    let mut ip = 0;
    while ip < inst.len() {
        all_boundaries.insert(ip);
        ip += instruction_size(inst, ip)?;
    }
    all_boundaries.insert(inst.len());

    for &s in &starts {
        if s > inst.len() || !all_boundaries.contains(&s) {
            return Err(BuildError::BadJumpTarget {
                from_ip: 0,
                target: s,
            });
        }
    }

    Ok(starts.into_iter().collect())
}

/// Map every bytecode offset that starts a block to its BlockId.
fn build_offset_to_block(starts: &[usize]) -> std::collections::BTreeMap<usize, BlockId> {
    starts
        .iter()
        .enumerate()
        .map(|(idx, &off)| (off, BlockId(idx as u32)))
        .collect()
}

// ─── Builder state ──────────────────────────────────────────────────────────

struct Builder<'a> {
    instructions: &'a [u8],
    #[allow(dead_code)]
    constants: Rc<Vec<Object>>,
    #[allow(dead_code)]
    num_bytecode_regs: u16,
    #[allow(dead_code)]
    num_parameters: u16,

    /// Monotonic counter for SSA value IDs.
    next_value_id: u32,
    /// Blocks in index order.
    blocks: Vec<Block>,
}

impl<'a> Builder<'a> {
    fn new(
        instructions: &'a [u8],
        constants: Rc<Vec<Object>>,
        num_bytecode_regs: u16,
        num_parameters: u16,
    ) -> Self {
        Self {
            instructions,
            constants,
            num_bytecode_regs,
            num_parameters,
            next_value_id: 0,
            blocks: Vec::new(),
        }
    }

    fn alloc_value(&mut self) -> ValueId {
        let id = ValueId(self.next_value_id);
        self.next_value_id += 1;
        id
    }

    /// Allocate one block parameter per bytecode register, all typed
    /// as `Value` (un-narrowed). Returns the parameter list ready to
    /// attach to a block.
    fn alloc_entry_params(&mut self, num_regs: u16) -> Vec<(ValueId, ValueType)> {
        (0..num_regs)
            .map(|_| (self.alloc_value(), ValueType::Value))
            .collect()
    }

    /// Append an op to a block, allocating a fresh SSA id for its result.
    fn push_op(&mut self, block: BlockId, op: IrOp) -> ValueId {
        let vid = self.alloc_value();
        self.blocks[block.0 as usize].ops.push((vid, op));
        vid
    }
}

// ─── Pass 2: per-block translation ──────────────────────────────────────────

/// Translate a single block starting at `start` (inclusive) and
/// ending at `end` (exclusive).
///
/// `regs` is the SSA name-stack for the function's bytecode registers
/// at the start of this block. The function updates it in-place as it
/// emits ops; the final state describes the live-outs.
#[allow(clippy::too_many_arguments)]
fn translate_block(
    b: &mut Builder<'_>,
    block_id: BlockId,
    start: usize,
    end: usize,
    mut regs: Vec<Option<ValueId>>,
    offset_to_block: &std::collections::BTreeMap<usize, BlockId>,
    block_reg_in: &mut Vec<Option<Vec<Option<ValueId>>>>,
) -> Result<(), BuildError> {
    let inst = b.instructions;
    let mut ip = start;

    // Helper: read a register's current SSA name, allocating a fresh
    // one if the register is uninitialised (which would be a bytecode
    // bug but we emit something plausible anyway).
    macro_rules! reg_read {
        ($r:expr) => {{
            let r = $r as usize;
            if r >= regs.len() {
                return Err(BuildError::BadRegister {
                    at_ip: ip,
                    reg: $r as u16,
                });
            }
            match regs[r] {
                Some(v) => v,
                None => {
                    let v = b.push_op(block_id, IrOp::ConstUndef);
                    regs[r] = Some(v);
                    v
                }
            }
        }};
    }

    // Helper: write a register with a fresh SSA name.
    macro_rules! reg_write {
        ($r:expr, $v:expr) => {{
            let r = $r as usize;
            if r >= regs.len() {
                return Err(BuildError::BadRegister {
                    at_ip: ip,
                    reg: $r as u16,
                });
            }
            regs[r] = Some($v);
        }};
    }

    // Helper: propagate `regs` into a successor block.
    // If the successor has no recorded state, this is its initial
    // state. If it already has state, we verify shape compatibility
    // but otherwise trust the existing (phase 1 doesn't merge —
    // that's a phase-2 improvement).
    let record_successor = |succ: BlockId,
                            regs: &[Option<ValueId>],
                            block_reg_in: &mut Vec<Option<Vec<Option<ValueId>>>>|
     -> () {
        let i = succ.0 as usize;
        if block_reg_in[i].is_none() {
            block_reg_in[i] = Some(regs.to_vec());
        }
    };

    while ip < end {
        let op_byte = inst[ip];
        let op = ROp::from_byte(op_byte).ok_or(BuildError::BadOpcode(op_byte, ip))?;
        let size = instruction_size(inst, ip)?;

        match op {
            // ── Loads ──
            ROp::LoadConst => {
                let dst = read_u16(inst, ip + 1);
                let idx = read_u16(inst, ip + 3) as usize;
                if idx >= b.constants.len() {
                    return Err(BuildError::BadConstIdx { at_ip: ip, idx });
                }
                let ir_op = match &b.constants[idx] {
                    Object::Integer(v) if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 => {
                        IrOp::ConstI32(*v as i32)
                    }
                    Object::Integer(v) => IrOp::ConstF64((*v as f64).to_bits()),
                    Object::Float(v) => IrOp::ConstF64(v.to_bits()),
                    Object::Boolean(v) => IrOp::ConstBool(*v),
                    Object::Null => IrOp::ConstNull,
                    Object::Undefined => IrOp::ConstUndef,
                    // A `CompiledFunction` constant is the body of a
                    // function *expression* — each LoadConst
                    // evaluation should yield a fresh heap-allocated
                    // Value. Reuse the zero-capture closure helper to
                    // materialise it. (Capture-bearing closures still
                    // land here as distinct constants with associated
                    // MakeClosure bytecode; we only handle the
                    // no-capture case from LoadConst.)
                    Object::CompiledFunction(_) => {
                        IrOp::MakeClosureNoCapture(idx as u32)
                    }
                    // Phase 1 bails out on anything string-shaped,
                    // etc. — those come back in phase 6+ once we
                    // handle property access.
                    _ => return Err(BuildError::UnsupportedOp("LoadConst-non-primitive", ip)),
                };
                let v = b.push_op(block_id, ir_op);
                reg_write!(dst, v);
            }
            ROp::LoadTrue => {
                let dst = read_u16(inst, ip + 1);
                let v = b.push_op(block_id, IrOp::ConstBool(true));
                reg_write!(dst, v);
            }
            ROp::LoadFalse => {
                let dst = read_u16(inst, ip + 1);
                let v = b.push_op(block_id, IrOp::ConstBool(false));
                reg_write!(dst, v);
            }
            ROp::LoadNull => {
                let dst = read_u16(inst, ip + 1);
                let v = b.push_op(block_id, IrOp::ConstNull);
                reg_write!(dst, v);
            }
            ROp::LoadUndef => {
                let dst = read_u16(inst, ip + 1);
                let v = b.push_op(block_id, IrOp::ConstUndef);
                reg_write!(dst, v);
            }

            // ── Register ops ──
            ROp::Move => {
                let dst = read_u16(inst, ip + 1);
                let src = read_u16(inst, ip + 3);
                let sv = reg_read!(src);
                reg_write!(dst, sv);
            }

            // ── Global slot access ──
            //
            // Top-level scripts keep their `let`/`const`/`var`
            // bindings in the engine's global slot table. For tier 2
            // we model each as a memory load / store — the deopt
            // machinery (phase 6) will need to thread the global
            // table through live-state reconstruction.
            ROp::GetGlobal => {
                let dst = read_u16(inst, ip + 1);
                let g = read_u16(inst, ip + 3);
                let v = b.push_op(block_id, IrOp::LoadGlobal(g));
                reg_write!(dst, v);
            }
            ROp::SetGlobal => {
                let g = read_u16(inst, ip + 1);
                let src = reg_read!(read_u16(inst, ip + 3));
                // StoreGlobal is a void op; we still allocate a
                // ValueId so the op list is uniform, but nothing
                // reads it.
                let _ = b.push_op(block_id, IrOp::StoreGlobal(g, src));
            }

            // ── Arithmetic (generic fallback — passes 5+ will narrow
            //    these to AddI32/AddF64 where feedback says so) ──
            ROp::Add => {
                let dst = read_u16(inst, ip + 1);
                let l = reg_read!(read_u16(inst, ip + 3));
                let r = reg_read!(read_u16(inst, ip + 5));
                let v = b.push_op(block_id, IrOp::AddGeneric(l, r));
                reg_write!(dst, v);
            }
            ROp::Sub => {
                let dst = read_u16(inst, ip + 1);
                let l = reg_read!(read_u16(inst, ip + 3));
                let r = reg_read!(read_u16(inst, ip + 5));
                let v = b.push_op(block_id, IrOp::SubGeneric(l, r));
                reg_write!(dst, v);
            }
            ROp::Mul => {
                let dst = read_u16(inst, ip + 1);
                let l = reg_read!(read_u16(inst, ip + 3));
                let r = reg_read!(read_u16(inst, ip + 5));
                let v = b.push_op(block_id, IrOp::MulGeneric(l, r));
                reg_write!(dst, v);
            }
            ROp::Div => {
                let dst = read_u16(inst, ip + 1);
                let l = reg_read!(read_u16(inst, ip + 3));
                let r = reg_read!(read_u16(inst, ip + 5));
                let v = b.push_op(block_id, IrOp::DivGeneric(l, r));
                reg_write!(dst, v);
            }
            ROp::Mod => {
                let dst = read_u16(inst, ip + 1);
                let l = reg_read!(read_u16(inst, ip + 3));
                let r = reg_read!(read_u16(inst, ip + 5));
                let v = b.push_op(block_id, IrOp::ModGeneric(l, r));
                reg_write!(dst, v);
            }
            ROp::Neg => {
                // Generic unary negation — we pessimistically box via
                // a runtime helper today; pass 5 can narrow to NegI32
                // when speculation permits.
                let dst = read_u16(inst, ip + 1);
                let src = reg_read!(read_u16(inst, ip + 3));
                // Express as 0 - src at the IR level; constant-fold +
                // speculation can recover.
                let zero = b.push_op(block_id, IrOp::ConstI32(0));
                let v = b.push_op(block_id, IrOp::SubGeneric(zero, src));
                reg_write!(dst, v);
            }

            // ── Closure creation (zero-capture case only) ──
            //
            // Bytecode shape is `[dst:2, const_idx:2, count:1, slots:2*count]`.
            // The translator only handles `count == 0` — i.e.
            // `function(...) { ... }` expressions with no free
            // variables. Capture-having closures still bail out as
            // unsupported; a future phase can add the runtime plumbing
            // to snapshot captured slots at emit time.
            ROp::MakeClosure => {
                let dst = read_u16(inst, ip + 1);
                let const_idx = read_u16(inst, ip + 3);
                let count = inst[ip + 5];
                if count != 0 {
                    return Err(BuildError::UnsupportedOp(
                        "MakeClosure with captures",
                        ip,
                    ));
                }
                let v = b.push_op(block_id, IrOp::MakeClosureNoCapture(const_idx as u32));
                reg_write!(dst, v);
            }

            // ── Function calls ──
            //
            // [dst:2, base:2, nargs:1] — callee is at `base`, args are
            // contiguous registers `base+1 .. base+nargs`. Translates
            // to `CallValue(callee, [args])`; the emitter lowers this
            // to `djit_call_helper` which performs the full dispatch
            // and returns the callee's result. A tier-1-rejecting
            // non-self call now tier-2-compiles; the arithmetic
            // around the call runs as native tier-2 code.
            ROp::Call => {
                let dst = read_u16(inst, ip + 1);
                let base = read_u16(inst, ip + 3);
                let nargs = inst[ip + 5] as usize;
                let callee = reg_read!(base);
                let mut args: Vec<ValueId> = Vec::with_capacity(nargs);
                for i in 0..nargs {
                    args.push(reg_read!(base + 1 + i as u16));
                }
                let v = b.push_op(block_id, IrOp::CallValue(callee, args));
                reg_write!(dst, v);
            }

            // ── Comparisons (generic) ──
            ROp::Equal | ROp::StrictEqual => {
                let dst = read_u16(inst, ip + 1);
                let l = reg_read!(read_u16(inst, ip + 3));
                let r = reg_read!(read_u16(inst, ip + 5));
                let irop = if matches!(op, ROp::StrictEqual) {
                    IrOp::EqValue(l, r)
                } else {
                    IrOp::LooseEqValue(l, r)
                };
                let v = b.push_op(block_id, irop);
                reg_write!(dst, v);
            }
            ROp::NotEqual | ROp::StrictNotEqual => {
                let dst = read_u16(inst, ip + 1);
                let l = reg_read!(read_u16(inst, ip + 3));
                let r = reg_read!(read_u16(inst, ip + 5));
                let v = b.push_op(block_id, IrOp::NeValue(l, r));
                reg_write!(dst, v);
            }
            ROp::LessThan => {
                let dst = read_u16(inst, ip + 1);
                let l = reg_read!(read_u16(inst, ip + 3));
                let r = reg_read!(read_u16(inst, ip + 5));
                let v = b.push_op(block_id, IrOp::LtValue(l, r));
                reg_write!(dst, v);
            }
            ROp::LessOrEqual => {
                let dst = read_u16(inst, ip + 1);
                let l = reg_read!(read_u16(inst, ip + 3));
                let r = reg_read!(read_u16(inst, ip + 5));
                let v = b.push_op(block_id, IrOp::LeValue(l, r));
                reg_write!(dst, v);
            }
            ROp::GreaterThan => {
                let dst = read_u16(inst, ip + 1);
                let l = reg_read!(read_u16(inst, ip + 3));
                let r = reg_read!(read_u16(inst, ip + 5));
                // Express as NOT Le; pass 3 can collapse.
                let le = b.push_op(block_id, IrOp::LeValue(l, r));
                let v = b.push_op(block_id, IrOp::NotBool(le));
                reg_write!(dst, v);
            }
            ROp::GreaterOrEqual => {
                let dst = read_u16(inst, ip + 1);
                let l = reg_read!(read_u16(inst, ip + 3));
                let r = reg_read!(read_u16(inst, ip + 5));
                let lt = b.push_op(block_id, IrOp::LtValue(l, r));
                let v = b.push_op(block_id, IrOp::NotBool(lt));
                reg_write!(dst, v);
            }

            // ── Fused register+const arith ──
            ROp::AddRegConst | ROp::SubRegConst | ROp::MulRegConst => {
                let dst = read_u16(inst, ip + 1);
                let src = reg_read!(read_u16(inst, ip + 3));
                let cidx = read_u16(inst, ip + 5) as usize;
                if cidx >= b.constants.len() {
                    return Err(BuildError::BadConstIdx { at_ip: ip, idx: cidx });
                }
                let c = match &b.constants[cidx] {
                    Object::Integer(v) if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 => {
                        b.push_op(block_id, IrOp::ConstI32(*v as i32))
                    }
                    Object::Integer(v) => b.push_op(block_id, IrOp::ConstF64((*v as f64).to_bits())),
                    Object::Float(v) => b.push_op(block_id, IrOp::ConstF64(v.to_bits())),
                    _ => {
                        return Err(BuildError::UnsupportedOp(
                            "AddRegConst-non-numeric",
                            ip,
                        ))
                    }
                };
                let ir = match op {
                    ROp::AddRegConst => IrOp::AddGeneric(src, c),
                    ROp::SubRegConst => IrOp::SubGeneric(src, c),
                    ROp::MulRegConst => IrOp::MulGeneric(src, c),
                    _ => unreachable!(),
                };
                let v = b.push_op(block_id, ir);
                reg_write!(dst, v);
            }

            // ── Fused regs-only test-and-jump ──
            // Pattern: TestLtRegJump a, b, target
            // Semantics: if !(regs[a] < regs[b]) jump target.
            ROp::TestLtRegJump | ROp::TestLeRegJump => {
                let a_reg = read_u16(inst, ip + 1);
                let b_reg = read_u16(inst, ip + 3);
                let target = read_u32(inst, ip + 5) as usize;

                let a_val = reg_read!(a_reg);
                let b_val = reg_read!(b_reg);
                let cmp = match op {
                    ROp::TestLtRegJump => IrOp::LtValue(a_val, b_val),
                    ROp::TestLeRegJump => IrOp::LeValue(a_val, b_val),
                    _ => unreachable!(),
                };
                let cmp_v = b.push_op(block_id, cmp);
                let fallthrough = ip + size;
                let then_block = *offset_to_block
                    .get(&fallthrough)
                    .ok_or(BuildError::BadJumpTarget { from_ip: ip, target: fallthrough })?;
                let else_block = *offset_to_block
                    .get(&target)
                    .ok_or(BuildError::BadJumpTarget { from_ip: ip, target })?;
                let regs_snapshot = regs.clone();
                let args = collect_live_args(&regs_snapshot);
                record_successor(then_block, &regs_snapshot, block_reg_in);
                record_successor(else_block, &regs_snapshot, block_reg_in);
                b.blocks[block_id.0 as usize].term = Terminator::Branch {
                    cond: cmp_v,
                    then_block,
                    then_args: args.clone(),
                    else_block,
                    else_args: args,
                };
                return Ok(());
            }

            // ── Fused increment-and-jump ──
            // Pattern: IncrementRegAndJump r, const_idx, target
            // Semantics: `regs[r] += constants[idx]; jump target`.
            //
            // Emitted by the compiler for `i = i + 1` at the bottom of a
            // `for` loop. We decompose into an add + unconditional jump
            // so the induction-variable pass in phase 7 can re-fuse it.
            ROp::IncrementRegAndJump => {
                let r_reg = read_u16(inst, ip + 1);
                let cidx = read_u16(inst, ip + 3) as usize;
                let target = read_u32(inst, ip + 5) as usize;

                if cidx >= b.constants.len() {
                    return Err(BuildError::BadConstIdx { at_ip: ip, idx: cidx });
                }
                let const_v = match &b.constants[cidx] {
                    Object::Integer(v) if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 => {
                        b.push_op(block_id, IrOp::ConstI32(*v as i32))
                    }
                    _ => {
                        return Err(BuildError::UnsupportedOp(
                            "IncrementRegAndJump-non-i32",
                            ip,
                        ))
                    }
                };
                let r_val = reg_read!(r_reg);
                let sum = b.push_op(block_id, IrOp::AddGeneric(r_val, const_v));
                reg_write!(r_reg, sum);

                let succ = *offset_to_block
                    .get(&target)
                    .ok_or(BuildError::BadJumpTarget { from_ip: ip, target })?;
                let args = collect_live_args(&regs);
                record_successor(succ, &regs, block_reg_in);
                b.blocks[block_id.0 as usize].term = Terminator::Jump(succ, args);
                return Ok(());
            }

            // ── Fused const test-and-jump ──
            // Pattern: TestLtConstJump r, const_idx, target
            // Semantics: if !(regs[r] < constants[idx]) jump target
            // else fall through.
            //
            // We decompose into (compare, branch) and let later passes
            // (in particular induction-variable analysis in phase 7)
            // recognise the canonical form to re-fuse at codegen.
            ROp::TestLtConstJump | ROp::TestLeConstJump => {
                let r_reg = read_u16(inst, ip + 1);
                let cidx = read_u16(inst, ip + 3) as usize;
                let target = read_u32(inst, ip + 5) as usize;

                if cidx >= b.constants.len() {
                    return Err(BuildError::BadConstIdx { at_ip: ip, idx: cidx });
                }
                let const_v = match &b.constants[cidx] {
                    Object::Integer(v) if *v >= i32::MIN as i64 && *v <= i32::MAX as i64 => {
                        b.push_op(block_id, IrOp::ConstI32(*v as i32))
                    }
                    _ => {
                        return Err(BuildError::UnsupportedOp("TestLtConstJump-non-i32", ip));
                    }
                };
                let r_val = reg_read!(r_reg);
                let cmp = match op {
                    ROp::TestLtConstJump => IrOp::LtValue(r_val, const_v),
                    ROp::TestLeConstJump => IrOp::LeValue(r_val, const_v),
                    _ => unreachable!(),
                };
                let cmp_v = b.push_op(block_id, cmp);
                // Semantics are "jump if NOT (r op const)" → i.e.
                // branch into target when cond is false, fall through
                // when cond is true.
                let fallthrough = ip + size;
                let then_block = *offset_to_block
                    .get(&fallthrough)
                    .ok_or(BuildError::BadJumpTarget { from_ip: ip, target: fallthrough })?;
                let else_block = *offset_to_block
                    .get(&target)
                    .ok_or(BuildError::BadJumpTarget { from_ip: ip, target })?;

                let regs_snapshot = regs.clone();
                let args = collect_live_args(&regs_snapshot);
                record_successor(then_block, &regs_snapshot, block_reg_in);
                record_successor(else_block, &regs_snapshot, block_reg_in);

                b.blocks[block_id.0 as usize].term = Terminator::Branch {
                    cond: cmp_v,
                    then_block,
                    then_args: args.clone(),
                    else_block,
                    else_args: args,
                };
                return Ok(());
            }

            // ── Control flow ──
            ROp::Jump => {
                let target = read_u32(inst, ip + 1) as usize;
                let succ = *offset_to_block
                    .get(&target)
                    .ok_or(BuildError::BadJumpTarget { from_ip: ip, target })?;
                let args = collect_live_args(&regs);
                record_successor(succ, &regs, block_reg_in);
                b.blocks[block_id.0 as usize].term = Terminator::Jump(succ, args);
                return Ok(());
            }
            ROp::JumpIfNot => {
                let cond = reg_read!(read_u16(inst, ip + 1));
                let target = read_u32(inst, ip + 3) as usize;
                let fallthrough = ip + size;
                let then_block = *offset_to_block
                    .get(&fallthrough)
                    .ok_or(BuildError::BadJumpTarget { from_ip: ip, target: fallthrough })?;
                let else_block = *offset_to_block
                    .get(&target)
                    .ok_or(BuildError::BadJumpTarget { from_ip: ip, target })?;
                let args = collect_live_args(&regs);
                record_successor(then_block, &regs, block_reg_in);
                record_successor(else_block, &regs, block_reg_in);
                b.blocks[block_id.0 as usize].term = Terminator::Branch {
                    cond,
                    then_block,
                    then_args: args.clone(),
                    else_block,
                    else_args: args,
                };
                return Ok(());
            }
            ROp::JumpIfTruthy => {
                // Opposite polarity of JumpIfNot.
                let cond = reg_read!(read_u16(inst, ip + 1));
                let target = read_u32(inst, ip + 3) as usize;
                let fallthrough = ip + size;
                let then_block = *offset_to_block
                    .get(&target)
                    .ok_or(BuildError::BadJumpTarget { from_ip: ip, target })?;
                let else_block = *offset_to_block
                    .get(&fallthrough)
                    .ok_or(BuildError::BadJumpTarget { from_ip: ip, target: fallthrough })?;
                let args = collect_live_args(&regs);
                record_successor(then_block, &regs, block_reg_in);
                record_successor(else_block, &regs, block_reg_in);
                b.blocks[block_id.0 as usize].term = Terminator::Branch {
                    cond,
                    then_block,
                    then_args: args.clone(),
                    else_block,
                    else_args: args,
                };
                return Ok(());
            }

            // ── Returns / halts ──
            ROp::Return => {
                let src = reg_read!(read_u16(inst, ip + 1));
                b.blocks[block_id.0 as usize].term = Terminator::Return(Some(src));
                return Ok(());
            }
            ROp::ReturnUndef => {
                b.blocks[block_id.0 as usize].term = Terminator::Return(None);
                return Ok(());
            }
            ROp::HaltValue => {
                let src = reg_read!(read_u16(inst, ip + 1));
                b.blocks[block_id.0 as usize].term = Terminator::Return(Some(src));
                return Ok(());
            }
            ROp::Halt => {
                b.blocks[block_id.0 as usize].term = Terminator::Return(None);
                return Ok(());
            }

            // ── Anything else is deferred to later phases. ──
            other => {
                return Err(BuildError::UnsupportedOp(rop_name(other), ip));
            }
        }

        ip += size;
    }

    // Fell off the end of this block without hitting a terminator.
    // That's fine — it means the next block starts where this one
    // stops (because `find_block_starts` inserted a split at a jump
    // target). Emit an implicit `Jump` so the IR stays well-formed.
    if let Some(&succ) = offset_to_block.get(&end) {
        let args = collect_live_args(&regs);
        record_successor(succ, &regs, block_reg_in);
        b.blocks[block_id.0 as usize].term = Terminator::Jump(succ, args);
        Ok(())
    } else {
        Err(BuildError::MissingTerminator)
    }
}

// ─── Tiny helpers ───────────────────────────────────────────────────────────

/// Collect the current SSA names of live registers, in register order.
/// Used to build the argument list passed to a successor block.
fn collect_live_args(regs: &[Option<ValueId>]) -> Vec<ValueId> {
    regs.iter()
        .filter_map(|slot| *slot)
        .collect::<Vec<_>>()
}

/// Static display name for an opcode — `Debug` would do but we want a
/// stable string-literal form for error messages.
fn rop_name(op: ROp) -> &'static str {
    // Hand-roll rather than `format!("{:?}", op)` so the returned string
    // is 'static and BuildError stays `Copy`/`Clone`-cheap.
    match op {
        ROp::LoadConst => "LoadConst",
        ROp::LoadTrue => "LoadTrue",
        ROp::LoadFalse => "LoadFalse",
        ROp::LoadNull => "LoadNull",
        ROp::LoadUndef => "LoadUndef",
        ROp::Move => "Move",
        ROp::GetGlobal => "GetGlobal",
        ROp::SetGlobal => "SetGlobal",
        ROp::Add => "Add",
        ROp::Sub => "Sub",
        ROp::Mul => "Mul",
        ROp::Div => "Div",
        ROp::Mod => "Mod",
        ROp::Pow => "Pow",
        ROp::Equal => "Equal",
        ROp::NotEqual => "NotEqual",
        ROp::StrictEqual => "StrictEqual",
        ROp::StrictNotEqual => "StrictNotEqual",
        ROp::GreaterThan => "GreaterThan",
        ROp::GreaterOrEqual => "GreaterOrEqual",
        ROp::LessThan => "LessThan",
        ROp::LessOrEqual => "LessOrEqual",
        ROp::Instanceof => "Instanceof",
        ROp::In => "In",
        ROp::BitwiseAnd => "BitwiseAnd",
        ROp::BitwiseOr => "BitwiseOr",
        ROp::BitwiseXor => "BitwiseXor",
        ROp::LeftShift => "LeftShift",
        ROp::RightShift => "RightShift",
        ROp::UnsignedRightShift => "UnsignedRightShift",
        ROp::Neg => "Neg",
        ROp::Not => "Not",
        ROp::UnaryPlus => "UnaryPlus",
        ROp::Typeof => "Typeof",
        ROp::IsNullish => "IsNullish",
        ROp::Jump => "Jump",
        ROp::JumpIfNot => "JumpIfNot",
        ROp::JumpIfTruthy => "JumpIfTruthy",
        ROp::Call => "Call",
        ROp::CallMethod => "CallMethod",
        ROp::CallSpread => "CallSpread",
        ROp::Return => "Return",
        ROp::ReturnUndef => "ReturnUndef",
        ROp::New => "New",
        ROp::NewSpread => "NewSpread",
        ROp::Super => "Super",
        ROp::Array => "Array",
        ROp::Hash => "Hash",
        ROp::AppendElement => "AppendElement",
        ROp::AppendSpread => "AppendSpread",
        ROp::GetProp => "GetProp",
        ROp::SetProp => "SetProp",
        ROp::GetGlobalProp => "GetGlobalProp",
        ROp::SetGlobalProp => "SetGlobalProp",
        ROp::Index => "Index",
        ROp::SetIndex => "SetIndex",
        ROp::DeleteProp => "DeleteProp",
        ROp::IteratorRest => "IteratorRest",
        ROp::GetKeysIter => "GetKeysIter",
        ROp::ObjectRest => "ObjectRest",
        ROp::Await => "Await",
        ROp::Throw => "Throw",
        ROp::CallGlobal => "CallGlobal",
        ROp::AddRegConst => "AddRegConst",
        ROp::SubRegConst => "SubRegConst",
        ROp::MulRegConst => "MulRegConst",
        ROp::TestLtConstJump => "TestLtConstJump",
        ROp::TestLeConstJump => "TestLeConstJump",
        ROp::IncrementRegAndJump => "IncrementRegAndJump",
        ROp::ModRegConstStrictEqConstJump => "ModRegConstStrictEqConstJump",
        ROp::AddConstToRegProp => "AddConstToRegProp",
        ROp::AddRegPropsToRegProp => "AddRegPropsToRegProp",
        ROp::TestLtRegJump => "TestLtRegJump",
        ROp::TestLeRegJump => "TestLeRegJump",
        ROp::TestModRegStrictEqConstJump => "TestModRegStrictEqConstJump",
        ROp::DefineAccessor => "DefineAccessor",
        ROp::InitClass => "InitClass",
        ROp::NewTarget => "NewTarget",
        ROp::ImportMeta => "ImportMeta",
        ROp::Yield => "Yield",
        ROp::MakeClosure => "MakeClosure",
        ROp::MakeArguments => "MakeArguments",
        ROp::EnterTry => "EnterTry",
        ROp::LeaveTry => "LeaveTry",
        ROp::Halt => "Halt",
        ROp::HaltValue => "HaltValue",
    }
}
