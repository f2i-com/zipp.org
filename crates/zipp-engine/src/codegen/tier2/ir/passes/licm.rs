//! Loop-invariant code motion.
//!
//! Walk each natural loop. For every pure op whose operands are all
//! defined outside the loop body (constants or values produced by
//! blocks that dominate the loop header), **hoist the op to just
//! before the loop header's predecessor-to-header jump** — the
//! pre-header slot.
//!
//! Hoisting saves ≥ `loop_trip_count − 1` evaluations per op. On the
//! arithmetic-benchmark loop this typically hoists `const` ops the
//! translator emits inside the body — small absolute wins per op but
//! reliable across every loop in the corpus.
//!
//! # Correctness invariants
//!
//! * The hoisted op's operands must be *strictly* dominated by the
//!   pre-header, i.e. live at every loop entry. Block parameters of
//!   the header are LOCAL to the loop (they're the phi values) and
//!   therefore **not** hoistable targets.
//! * The op must be **pure** — no side effects, no potential throw,
//!   no deopt. Memory loads aren't hoisted because the intervening
//!   iterations might have stored a new value at the same slot.
//! * After hoisting, the op's `ValueId` stays the same — we just
//!   move the `(ValueId, IrOp)` pair. SSA is preserved.
//!
//! # Pre-header handling
//!
//! We don't yet synthesise explicit pre-header blocks. If the header
//! has a unique predecessor outside the loop body, that predecessor
//! serves as the pre-header. If it has multiple outside-predecessors
//! (rare in structured-source compilation — our bytecode mostly
//! hits one), we skip hoisting for that loop rather than introducing
//! a new block; that's a phase-7.5 improvement.

use std::collections::BTreeSet;

use super::super::loops;
use super::super::types::{BlockId, IrFunction, IrOp, Terminator, ValueId};

/// Run the pass. Returns `true` if anything moved.
pub fn run(func: &mut IrFunction) -> bool {
    let loops = loops::find_loops(func);
    if loops.is_empty() {
        return false;
    }

    // Dedup loops by header (keep the first — which, given our sort,
    // is the one with the smallest latch). If two back-edges share a
    // header we'd otherwise hoist into the same pre-header twice.
    let mut seen = BTreeSet::new();
    let unique_loops: Vec<loops::Loop> = loops
        .into_iter()
        .filter(|l| seen.insert(l.header))
        .collect();

    let mut moved = false;
    for l in unique_loops {
        moved |= hoist_from_loop(func, &l);
    }
    moved
}

/// Hoist invariant ops out of a single loop into the pre-header.
fn hoist_from_loop(func: &mut IrFunction, l: &loops::Loop) -> bool {
    // Find the pre-header: the unique predecessor of `l.header` not
    // in `l.body`. If there are zero or ≥ 2 such predecessors, skip.
    let preheader = match find_preheader(func, l) {
        Some(b) => b,
        None => return false,
    };

    // Gather block-param value ids for quick "is this a phi?" test.
    let mut in_loop_defs: BTreeSet<ValueId> = BTreeSet::new();
    for &b in &l.body {
        let block = &func.blocks[b.0 as usize];
        for (vid, _) in &block.params {
            in_loop_defs.insert(*vid);
        }
        for (vid, _) in &block.ops {
            in_loop_defs.insert(*vid);
        }
    }

    // Values defined outside the loop are automatically invariant.
    // Inside the loop, only block-param-free pure ops whose operands
    // are all invariant can be hoisted.
    //
    // Pass up to `max_iters` times per loop — each hoisted op may
    // make another op hoistable (chain-invariance).
    const MAX_ITERS: usize = 8;
    let mut any_moved = false;
    let mut hoistable_here: BTreeSet<ValueId> = BTreeSet::new();

    for _iter in 0..MAX_ITERS {
        let mut moved_this_pass = false;
        let mut to_hoist: Vec<(BlockId, usize)> = Vec::new();

        for &b in &l.body {
            let block = &func.blocks[b.0 as usize];
            for (i, (vid, op)) in block.ops.iter().enumerate() {
                if !is_pure(op) {
                    continue;
                }
                // Block params of loop blocks are loop-local; don't
                // consider their results hoistable.
                if hoistable_here.contains(vid) {
                    continue;
                }
                let operands = op_uses(op);
                let invariant = operands.iter().all(|u| is_invariant(*u, &in_loop_defs, &hoistable_here));
                if invariant {
                    to_hoist.push((b, i));
                    hoistable_here.insert(*vid);
                }
            }
        }

        if to_hoist.is_empty() {
            break;
        }

        // Perform the moves. Since we're mutating Vec::ops and can't
        // hold two mutable refs at once, we extract each op to hoist
        // first, then re-insert into the pre-header. Walk each source
        // block from back to front so indices stay valid during
        // removal.
        let mut by_block: std::collections::BTreeMap<BlockId, Vec<usize>> =
            std::collections::BTreeMap::new();
        for (b, i) in to_hoist {
            by_block.entry(b).or_default().push(i);
        }

        let mut extracted: Vec<(ValueId, IrOp)> = Vec::new();
        for (b, mut idxs) in by_block {
            idxs.sort_unstable();
            idxs.reverse();
            let block = &mut func.blocks[b.0 as usize];
            for idx in idxs {
                extracted.push(block.ops.remove(idx));
            }
        }

        // Order extracted ops so that earlier-defining ops come
        // first. Without SSA-safe ordering, an op using a value
        // defined by another hoisted op could land before it.
        extracted.sort_by_key(|(vid, _)| vid.0);

        // Prepend to the pre-header's op list.
        let ph = &mut func.blocks[preheader.0 as usize];
        // Hoisted ops go at the end of the pre-header's op list
        // (which runs before the pre-header's terminator).
        ph.ops.extend(extracted);

        any_moved = true;
        moved_this_pass = true;
        if !moved_this_pass {
            break;
        }
    }

    any_moved
}

fn find_preheader(func: &IrFunction, l: &loops::Loop) -> Option<BlockId> {
    let mut outside_preds: Vec<BlockId> = Vec::new();
    for (idx, block) in func.blocks.iter().enumerate() {
        let me = BlockId(idx as u32);
        if l.body.contains(&me) {
            continue;
        }
        for s in term_successors(&block.term) {
            if s == l.header {
                outside_preds.push(me);
            }
        }
    }
    if outside_preds.len() == 1 {
        Some(outside_preds[0])
    } else {
        None
    }
}

fn term_successors(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Return(_) | Terminator::Deopt(_) | Terminator::Unreachable => vec![],
        Terminator::Jump(b, _) => vec![*b],
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
    }
}

fn is_invariant(
    v: ValueId,
    in_loop: &BTreeSet<ValueId>,
    hoistable: &BTreeSet<ValueId>,
) -> bool {
    // Invariant if defined outside the loop, or already queued for
    // hoisting this round.
    !in_loop.contains(&v) || hoistable.contains(&v)
}

fn is_pure(op: &IrOp) -> bool {
    // Mirrors the DCE pass's side-effect definition. Everything
    // non-side-effecting is pure for LICM purposes.
    !matches!(
        op,
        IrOp::StoreSlot(..)
            | IrOp::StoreGlobal(..)
            | IrOp::CallRuntime(..)
            | IrOp::CallValue(..)
            | IrOp::MakeClosureNoCapture(..)
            | IrOp::CheckI32(..)
            | IrOp::CheckF64(..)
            | IrOp::CheckHeap(..)
            | IrOp::CheckHeapShape(..)
            | IrOp::CheckFunctionIs(..)
            | IrOp::CheckedAddI32(..)
            | IrOp::CheckedSubI32(..)
            | IrOp::CheckedMulI32(..)
            | IrOp::LoadSlot(..)
            | IrOp::LoadGlobal(..)
    )
}

fn op_uses(op: &IrOp) -> Vec<ValueId> {
    // Mirror of the other passes' helper — kept local so the module
    // dependency graph stays readable.
    match op {
        IrOp::ConstI32(_)
        | IrOp::ConstF64(_)
        | IrOp::ConstBool(_)
        | IrOp::ConstNull
        | IrOp::ConstUndef
        | IrOp::ConstValue(_)
        | IrOp::LoadReg(_)
        | IrOp::LoadGlobal(_) => vec![],

        IrOp::Copy(v)
        | IrOp::NegI32(v)
        | IrOp::NegF64(v)
        | IrOp::NotBool(v)
        | IrOp::BoxI32(v)
        | IrOp::BoxF64(v)
        | IrOp::BoxBool(v)
        | IrOp::UnboxI32(v)
        | IrOp::UnboxF64(v)
        | IrOp::UnboxBool(v)
        | IrOp::CheckI32(v, _)
        | IrOp::CheckF64(v, _)
        | IrOp::CheckHeap(v, _)
        | IrOp::CheckHeapShape(v, _, _)
        | IrOp::CheckFunctionIs(v, _, _)
        | IrOp::LoadSlot(v, _, _)
        | IrOp::StoreGlobal(_, v) => vec![*v],

        IrOp::AddI32(a, b)
        | IrOp::SubI32(a, b)
        | IrOp::MulI32(a, b)
        | IrOp::CheckedAddI32(a, b, _)
        | IrOp::CheckedSubI32(a, b, _)
        | IrOp::CheckedMulI32(a, b, _)
        | IrOp::AddF64(a, b)
        | IrOp::SubF64(a, b)
        | IrOp::MulF64(a, b)
        | IrOp::DivF64(a, b)
        | IrOp::AddGeneric(a, b)
        | IrOp::SubGeneric(a, b)
        | IrOp::MulGeneric(a, b)
        | IrOp::DivGeneric(a, b)
        | IrOp::ModGeneric(a, b)
        | IrOp::EqI32(a, b)
        | IrOp::NeI32(a, b)
        | IrOp::LtI32(a, b)
        | IrOp::LeI32(a, b)
        | IrOp::GtI32(a, b)
        | IrOp::GeI32(a, b)
        | IrOp::EqValue(a, b)
        | IrOp::NeValue(a, b)
        | IrOp::LooseEqValue(a, b)
        | IrOp::LtValue(a, b)
        | IrOp::LeValue(a, b) => vec![*a, *b],

        IrOp::StoreSlot(obj, _, val) => vec![*obj, *val],
        IrOp::CallRuntime(_, args) => args.clone(),
        IrOp::CallValue(callee, args) => {
            let mut uses = Vec::with_capacity(args.len() + 1);
            uses.push(*callee);
            uses.extend_from_slice(args);
            uses
        }
        IrOp::MakeClosureNoCapture(_) => vec![],
    }
}
