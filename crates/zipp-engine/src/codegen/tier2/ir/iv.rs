//! Induction-variable analysis.
//!
//! Identifies **basic induction variables** (BIVs) in a loop: block
//! parameters of the loop header that follow the pattern
//!
//! ```text
//!     P = phi(init_from_preheader, P + step_in_latch)
//! ```
//!
//! where `step` is loop-invariant (defined outside the loop or
//! constant). These are the SSA-form equivalent of `for (let i = init;
//! …; i += step) …` counters: the value that changes by exactly
//! `step` each iteration, forever.
//!
//! Output feeds two downstream consumers:
//!
//! 1. **Strength reduction** (`passes::iv_reduce`). For every
//!    `v = IV * const_k` inside the loop with `k` loop-invariant,
//!    rewrite `v` as an auxiliary IV with `init * k` and `step * k`
//!    — saving the multiply per iteration.
//! 2. **Overflow elision (future).** Once we've proven `i` is a
//!    bounded i32-typed IV with `init` / `step` also i32 and a known
//!    upper bound from the loop condition, we can switch the per-iter
//!    `CheckedAddI32` to a plain `AddI32`, eliminating the deopt path.
//!    Lands with phase-8 inlining when both arrive together.
//!
//! ## What qualifies as a BIV (phase-1 scope)
//!
//! Intentionally narrow. The pattern must be:
//!
//! * A block parameter of a loop header.
//! * Exactly two incoming edges: one from a pre-header (outside the
//!   loop), one from a latch (inside the loop).
//! * The latch-edge argument is `P + step` where `step` is a value
//!   defined outside the loop (or a `ConstI32`).
//! * The add uses `IrOp::AddI32` *or* `IrOp::AddGeneric(ConstI32)` —
//!   anything else is rejected so we don't accidentally fold through
//!   an `AddF64` whose step isn't an integer. Later phases can
//!   broaden this.
//!
//! Values that look like IVs but don't quite fit (multiple latches,
//! decrementing forms with explicit negate, etc.) are simply absent
//! from the returned map — callers check-then-use.

use std::collections::{BTreeMap, BTreeSet};

use super::loops::Loop;
use super::types::{Block, BlockId, IrFunction, IrOp, Terminator, ValueId};

/// Information about a single BIV.
#[derive(Clone, Debug)]
pub struct BasicIv {
    /// The loop header's block-param `ValueId` — the "current value"
    /// of the IV on each iteration.
    pub phi: ValueId,
    /// The position of this block param in the header's `params`
    /// list. Used by strength reduction when threading a new
    /// auxiliary IV through all incoming edges at the same index.
    pub param_idx: usize,
    /// The initial value passed on the pre-header edge.
    pub init: ValueId,
    /// The step value (loop-invariant) added each iteration. Always
    /// the RHS operand of the AddI32/AddGeneric that updates the IV.
    pub step: ValueId,
    /// The `IrOp::AddI32` / `IrOp::AddGeneric` value that computes
    /// `phi + step` in the latch block.
    pub update: ValueId,
    /// The block where `update` is defined — typically the loop
    /// latch, but we don't enforce — strength reduction threads new
    /// updates through the same block.
    pub update_block: BlockId,
    /// The index of the `update` op within its defining block's
    /// ops list. Allows strength reduction to insert new ops
    /// adjacent to the existing IV update so they share a pass over
    /// the block list.
    pub update_op_idx: usize,
    /// The step's compile-time i32 value if we could determine one.
    /// `None` if `step` is a non-const loop-invariant (still valid,
    /// but strength reduction needs the concrete value to multiply
    /// by).
    pub const_step: Option<i32>,
}

/// Detect every BIV in `loop_`, indexed by its phi `ValueId`.
pub fn find_basic_ivs(
    func: &IrFunction,
    loop_: &Loop,
    preheader: BlockId,
) -> BTreeMap<ValueId, BasicIv> {
    let mut out = BTreeMap::new();

    let header = &func.blocks[loop_.header.0 as usize];

    // Collect predecessors that target the header — we need to
    // separate "from preheader" (init) from "from inside loop" (latch).
    let latch_edges: Vec<(BlockId, Vec<ValueId>)> = collect_header_incoming(func, loop_);
    // `latch_edges` includes both preheader and latch. Split.
    let mut preheader_args: Option<Vec<ValueId>> = None;
    let mut in_loop_args: Option<(BlockId, Vec<ValueId>)> = None;
    for (from, args) in &latch_edges {
        if *from == preheader {
            preheader_args = Some(args.clone());
        } else if loop_.body.contains(from) {
            // If we already saw one in-loop predecessor, bail — multiple
            // latches would require disjunctive analysis the phase-1
            // pass doesn't handle.
            if in_loop_args.is_some() {
                return out;
            }
            in_loop_args = Some((*from, args.clone()));
        }
    }
    let Some(preheader_args) = preheader_args else {
        return out;
    };
    let Some((latch_block_id, latch_args)) = in_loop_args else {
        return out;
    };

    if preheader_args.len() != header.params.len() || latch_args.len() != header.params.len() {
        return out;
    }

    let latch_block = &func.blocks[latch_block_id.0 as usize];

    // For each param, check if the latch argument is a recognisable
    // IV update.
    for (idx, (phi_id, _ty)) in header.params.iter().enumerate() {
        let init = preheader_args[idx];
        let update = latch_args[idx];
        // The update value must be the result of an AddI32/AddGeneric
        // where:
        //   - one operand is the phi itself (possibly forwarded
        //     through a chain of single-predecessor block params —
        //     the translator over-allocates these, so the textual
        //     "same value" often hides behind a trivial rename)
        //   - the other operand is loop-invariant
        let Some((update_op_idx, update_op)) =
            find_defining_op(latch_block, update)
        else {
            continue;
        };

        let (other_operand, const_step) = match update_op {
            IrOp::AddI32(a, b) => {
                if same_value(func, *a, *phi_id) {
                    (*b, None)
                } else if same_value(func, *b, *phi_id) {
                    (*a, None)
                } else {
                    continue;
                }
            }
            IrOp::AddGeneric(a, b) => {
                if same_value(func, *a, *phi_id) {
                    (*b, None)
                } else if same_value(func, *b, *phi_id) {
                    (*a, None)
                } else {
                    continue;
                }
            }
            _ => continue,
        };

        // Check `other_operand` is loop-invariant: defined outside
        // the loop body or a ConstI32 (which may be defined anywhere
        // since it's a pure op that const-fold may promote).
        if !is_loop_invariant(func, other_operand, loop_) {
            continue;
        }

        // Try to grab a compile-time i32 step if the step operand is
        // a ConstI32 — used by strength reduction to precompute
        // `step * k`.
        let const_step = const_step.or_else(|| const_i32_of(func, other_operand));

        out.insert(
            *phi_id,
            BasicIv {
                phi: *phi_id,
                param_idx: idx,
                init,
                step: other_operand,
                update,
                update_block: latch_block_id,
                update_op_idx,
                const_step,
            },
        );
    }

    out
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn collect_header_incoming(
    func: &IrFunction,
    loop_: &Loop,
) -> Vec<(BlockId, Vec<ValueId>)> {
    let mut edges = Vec::new();
    for (idx, block) in func.blocks.iter().enumerate() {
        let from = BlockId(idx as u32);
        match &block.term {
            Terminator::Jump(t, args) if *t == loop_.header => {
                edges.push((from, args.clone()));
            }
            Terminator::Branch {
                then_block,
                then_args,
                else_block,
                else_args,
                ..
            } => {
                if *then_block == loop_.header {
                    edges.push((from, then_args.clone()));
                }
                if *else_block == loop_.header {
                    edges.push((from, else_args.clone()));
                }
            }
            _ => {}
        }
    }
    edges
}

fn find_defining_op(block: &Block, v: ValueId) -> Option<(usize, &IrOp)> {
    for (i, (vid, op)) in block.ops.iter().enumerate() {
        if *vid == v {
            return Some((i, op));
        }
    }
    None
}

/// Are `a` and `b` effectively the same SSA value?
///
/// Direct equality is the usual case.  The tier-2 translator (for
/// simplicity) over-allocates one block parameter per bytecode
/// register on every block edge, so the "same" runtime value often
/// has different `ValueId`s in different blocks.  We chain-follow
/// through block parameters whose incoming edges all supply the same
/// value to peel those renames away.
///
/// Capped at 32 hops as a defensive measure against pathological IR.
fn same_value(func: &IrFunction, a: ValueId, b: ValueId) -> bool {
    normalise(func, a) == normalise(func, b)
}

fn normalise(func: &IrFunction, mut v: ValueId) -> ValueId {
    for _ in 0..32 {
        match find_param_definer(func, v) {
            Some((block, param_idx)) => {
                // Collect every incoming arg at this param index.
                let mut incomings: Vec<ValueId> = Vec::new();
                for (idx, blk) in func.blocks.iter().enumerate() {
                    let from = BlockId(idx as u32);
                    match &blk.term {
                        Terminator::Jump(t, args) if *t == block => {
                            if let Some(&a) = args.get(param_idx) {
                                // Don't chase self-loops (those are real phis).
                                if a != v {
                                    incomings.push(a);
                                }
                            }
                        }
                        Terminator::Branch {
                            then_block,
                            then_args,
                            else_block,
                            else_args,
                            ..
                        } => {
                            if *then_block == block {
                                if let Some(&a) = then_args.get(param_idx) {
                                    if a != v {
                                        incomings.push(a);
                                    }
                                }
                            }
                            if *else_block == block {
                                if let Some(&a) = else_args.get(param_idx) {
                                    if a != v {
                                        incomings.push(a);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    let _ = from;
                }
                if incomings.is_empty() {
                    return v;
                }
                let first = incomings[0];
                if incomings.iter().all(|x| *x == first) {
                    v = first;
                    continue;
                }
                return v;
            }
            None => return v,
        }
    }
    v
}

/// If `v` is a block parameter, return `(block, param_idx)`. Else
/// return `None`.
fn find_param_definer(func: &IrFunction, v: ValueId) -> Option<(BlockId, usize)> {
    for (idx, block) in func.blocks.iter().enumerate() {
        for (i, (vid, _)) in block.params.iter().enumerate() {
            if *vid == v {
                return Some((BlockId(idx as u32), i));
            }
        }
    }
    None
}

/// True if `v` is defined outside the loop's body, or is a
/// compile-time constant defined anywhere (constants are always
/// loop-invariant regardless of definition site).
pub fn is_loop_invariant(func: &IrFunction, v: ValueId, loop_: &Loop) -> bool {
    // Walk every block looking for v's definition. Not indexed —
    // assumes functions are small enough that linear is fine; for
    // phase-7 analysis the cost is amortised over one scan per loop.
    for (idx, block) in func.blocks.iter().enumerate() {
        let b = BlockId(idx as u32);
        for (vid, op) in &block.ops {
            if *vid == v {
                // Constants are invariant regardless of where they sit.
                if is_constant_op(op) {
                    return true;
                }
                return !loop_.body.contains(&b);
            }
        }
        for (vid, _) in &block.params {
            if *vid == v {
                return !loop_.body.contains(&b);
            }
        }
    }
    // Value not found — we've been handed a dangling reference. Treat
    // as non-invariant to be safe.
    false
}

/// Return the i32 value of `v` if it's directly defined by a
/// `ConstI32` anywhere in the function. Ignores more complex forms
/// — caller just wants a compile-time integer or `None`.
pub fn const_i32_of(func: &IrFunction, v: ValueId) -> Option<i32> {
    for block in &func.blocks {
        for (vid, op) in &block.ops {
            if *vid == v {
                if let IrOp::ConstI32(n) = op {
                    return Some(*n);
                }
                return None;
            }
        }
    }
    None
}

fn is_constant_op(op: &IrOp) -> bool {
    matches!(
        op,
        IrOp::ConstI32(_)
            | IrOp::ConstF64(_)
            | IrOp::ConstBool(_)
            | IrOp::ConstNull
            | IrOp::ConstUndef
            | IrOp::ConstValue(_)
    )
}

/// Find the pre-header for a loop: the unique non-body predecessor
/// of the header. Same rules as `passes::licm::find_preheader` —
/// copy here so phase-7 analysis doesn't depend on the LICM pass
/// module (loops.rs can't import passes without a cycle).
pub fn find_preheader(func: &IrFunction, loop_: &Loop) -> Option<BlockId> {
    let mut outside: Vec<BlockId> = Vec::new();
    for (idx, block) in func.blocks.iter().enumerate() {
        let me = BlockId(idx as u32);
        if loop_.body.contains(&me) {
            continue;
        }
        let successors = match &block.term {
            Terminator::Return(_) | Terminator::Deopt(_) | Terminator::Unreachable => vec![],
            Terminator::Jump(b, _) => vec![*b],
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => vec![*then_block, *else_block],
        };
        for s in successors {
            if s == loop_.header {
                outside.push(me);
            }
        }
    }
    if outside.len() == 1 {
        Some(outside[0])
    } else {
        None
    }
}

/// Find every `MulI32` / `MulGeneric` op inside a loop whose operands
/// are one of the loop's BIVs and one loop-invariant value. These are
/// the strength-reduction candidates.
///
/// Chain-follows through trivial block-param forwarding the translator
/// emits — a block-local `ValueId` that ultimately forwards from a
/// header BIV param still counts as that BIV for reduction purposes.
pub fn find_reducible_muls(
    func: &IrFunction,
    loop_: &Loop,
    ivs: &BTreeMap<ValueId, BasicIv>,
) -> Vec<ReducibleMul> {
    let mut out = Vec::new();
    let mut seen_result: BTreeSet<ValueId> = BTreeSet::new();

    // Helper: does `v` resolve (through param-forwarding) to one of
    // our BIV phis?
    let resolve_biv = |v: ValueId| -> Option<ValueId> {
        let n = normalise(func, v);
        if ivs.contains_key(&n) {
            Some(n)
        } else {
            None
        }
    };

    for &b in &loop_.body {
        let block = &func.blocks[b.0 as usize];
        for (i, (vid, op)) in block.ops.iter().enumerate() {
            let (a, c) = match op {
                IrOp::MulI32(a, c) | IrOp::MulGeneric(a, c) => (*a, *c),
                _ => continue,
            };
            // Classify operands: is either a BIV (via chain), and
            // the other loop-invariant?
            let (biv, factor) = if let Some(biv_id) = resolve_biv(a) {
                if is_loop_invariant(func, c, loop_) {
                    (biv_id, c)
                } else {
                    continue;
                }
            } else if let Some(biv_id) = resolve_biv(c) {
                if is_loop_invariant(func, a, loop_) {
                    (biv_id, a)
                } else {
                    continue;
                }
            } else {
                continue;
            };
            // Only reduce when the factor has a known compile-time
            // i32 value — otherwise we'd have to emit the runtime
            // multiplication anyway.
            let Some(k) = const_i32_of(func, factor) else {
                continue;
            };
            if seen_result.insert(*vid) {
                out.push(ReducibleMul {
                    result: *vid,
                    block: b,
                    op_idx: i,
                    biv,
                    factor_const: k,
                });
            }
        }
    }
    out
}

/// A `IrOp::Mul(biv, const_k)` or `Mul(const_k, biv)` inside a loop
/// body — a candidate for strength reduction.
#[derive(Clone, Debug)]
pub struct ReducibleMul {
    pub result: ValueId,
    pub block: BlockId,
    pub op_idx: usize,
    pub biv: ValueId,
    pub factor_const: i32,
}
