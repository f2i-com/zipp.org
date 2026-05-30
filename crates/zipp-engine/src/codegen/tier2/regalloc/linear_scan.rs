//! Linear-scan register allocation.
//!
//! Classical Poletto & Sarkar 1999 algorithm, adapted for SSA block-
//! parameter IR:
//!
//! 1. Linearise the CFG (reverse postorder from [`liveness::compute_rpo`]).
//! 2. For every ValueId, compute a half-open live interval
//!    `[first_def, last_use]` over positions in the linearisation.
//!    Positions are numbered so each op gets its own slot and the
//!    terminator sits one past the last op.
//! 3. Walk intervals in `first_def` order, maintaining an "active"
//!    set of intervals currently holding a physical register.
//!    * Expire intervals that end before the current one starts.
//!    * If a free register exists, assign.
//!    * Else spill the active interval with the latest `last_use`.
//!
//! The algorithm is well-documented; the corner cases in our context:
//!
//! * Block parameters are "defined at position 0 of their block" —
//!   just like the first op.
//! * A jump / branch terminator's arg reads *extend* the live range
//!   of the argument values to the terminator's position.
//! * Void ops (StoreGlobal, StoreSlot) have no result to allocate —
//!   we skip them in the interval map.
//! * Constants (Const* ops) still get intervals; emission may choose
//!   to materialise them inline rather than reserve a register, but
//!   that's a phase-4c decision.
//!
//! The output is an [`Allocation`] — consumed by the (future)
//! emit phase to know where each ValueId lives at each program point.
//!
//! ## Spill strategy
//!
//! When all registers are live and we need a new one, we spill the
//! active interval whose `last_use` is furthest in the future. That's
//! the "farthest-use" heuristic — standard for linear-scan and
//! optimal for single-use values. A more sophisticated allocator
//! (Wimmer/Franz) makes local decisions per use; for phase 4a we
//! take the simple route.
//!
//! Spill slots are assigned sequentially as needed. Reuse after a
//! spilled interval dies is a phase-4b optimisation.

use std::collections::BTreeMap;

use super::super::ir::{BlockId, IrFunction, IrOp, Terminator, ValueId};
use super::liveness::Liveness;
use super::{Allocation, Location};

/// One half-open interval `[start, end]` (both inclusive) over the
/// linearised position space.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Interval {
    value: ValueId,
    start: u32,
    end: u32,
}

pub fn allocate(
    func: &IrFunction,
    block_order: &[BlockId],
    live: &Liveness,
    num_regs: u8,
) -> Allocation {
    // ── 1. Position numbering ──
    //
    // Each block gets a contiguous range of positions:
    //   * Position 0 of the block's range = block-entry (params defined here).
    //   * Positions 1..=N are the ops (op `i` sits at position `i + 1`).
    //   * Position N+1 is the terminator.
    //
    // Keeps arithmetic simple; cost is a small gap at each block
    // boundary.
    let mut block_start: BTreeMap<BlockId, u32> = BTreeMap::new();
    let mut next_pos: u32 = 0;
    for &b in block_order {
        block_start.insert(b, next_pos);
        let block = &func.blocks[b.0 as usize];
        // Reserve: 1 for entry + ops.len() + 1 for terminator.
        next_pos = next_pos.saturating_add(block.ops.len() as u32 + 2);
    }

    // ── 2. Build intervals ──
    //
    // Seed with [first_def, first_def] for every defined value;
    // extend `end` as we encounter uses.
    let mut intervals: BTreeMap<ValueId, Interval> = BTreeMap::new();

    let mut extend = |intervals: &mut BTreeMap<ValueId, Interval>, v: ValueId, pos: u32| {
        intervals
            .entry(v)
            .and_modify(|iv| {
                if pos > iv.end {
                    iv.end = pos;
                }
                if pos < iv.start {
                    iv.start = pos;
                }
            })
            .or_insert(Interval {
                value: v,
                start: pos,
                end: pos,
            });
    };

    for &b in block_order {
        let b_base = block_start[&b];
        let block = &func.blocks[b.0 as usize];

        // Block params defined at entry (position b_base).
        for (v, _) in &block.params {
            intervals.insert(
                *v,
                Interval {
                    value: *v,
                    start: b_base,
                    end: b_base,
                },
            );
        }

        // live_in values are alive from entry onward — extend to at
        // least the entry position (so if they're not used locally,
        // they still participate in interference).
        for v in &live[b.0 as usize].live_in {
            extend(&mut intervals, *v, b_base);
        }

        // Ops: op at index `i` is at position `b_base + 1 + i`.
        for (i, (vid, op)) in block.ops.iter().enumerate() {
            let pos = b_base + 1 + i as u32;
            // Uses first — they reference values still live at `pos`.
            for u in op_reads(op) {
                extend(&mut intervals, u, pos);
            }
            // Skip defs for void ops (StoreGlobal, StoreSlot): their
            // result is never read, so an interval would just waste
            // a register slot.
            if !op.is_void() {
                intervals
                    .entry(*vid)
                    .and_modify(|iv| {
                        iv.start = pos;
                        if pos > iv.end {
                            iv.end = pos;
                        }
                    })
                    .or_insert(Interval {
                        value: *vid,
                        start: pos,
                        end: pos,
                    });
            }
        }

        // Terminator at position b_base + 1 + ops.len().
        let term_pos = b_base + 1 + block.ops.len() as u32;
        for u in term_reads(&block.term) {
            extend(&mut intervals, u, term_pos);
        }

        // live_out values stay live through the terminator.
        for v in &live[b.0 as usize].live_out {
            extend(&mut intervals, *v, term_pos);
        }
    }

    // ── 3. Linear-scan ──
    let mut intervals_sorted: Vec<Interval> = intervals.into_values().collect();
    intervals_sorted.sort_by_key(|iv| (iv.start, iv.end));

    // `active` holds currently-assigned register intervals, sorted by
    // increasing `end`. Replacing this BTreeMap with a Vec + sort-at-
    // insert would match the textbook; using BTreeMap keyed by `end`
    // gives us ordered-iteration-by-end for free.
    let mut active: Vec<Interval> = Vec::new();
    let mut free_regs: Vec<u8> = (0..num_regs).rev().collect();

    let mut locations: BTreeMap<ValueId, Location> = BTreeMap::new();
    let mut spill_active: BTreeMap<ValueId, u32> = BTreeMap::new();
    let mut next_spill_slot: u32 = 0;

    for iv in &intervals_sorted {
        // Expire: any active interval that ends before `iv.start`
        // frees its register.
        active.retain(|a| {
            if a.end < iv.start {
                if let Some(Location::Reg(r)) = locations.get(&a.value).copied() {
                    free_regs.push(r);
                }
                false
            } else {
                true
            }
        });

        if let Some(r) = free_regs.pop() {
            locations.insert(iv.value, Location::Reg(r));
            active.push(*iv);
        } else {
            // Spill the furthest-use-end interval — either `iv` itself
            // or the one with the latest end in `active`.
            let (spill_idx, spill_end) = active
                .iter()
                .enumerate()
                .max_by_key(|(_, a)| a.end)
                .map(|(i, a)| (i, a.end))
                .unwrap_or((usize::MAX, 0));

            if spill_idx != usize::MAX && spill_end > iv.end {
                // Evict the active interval, take its register for
                // `iv`, spill the evictee.
                let evicted = active.swap_remove(spill_idx);
                let Some(Location::Reg(r)) = locations.get(&evicted.value).copied() else {
                    unreachable!("active interval must have a register location")
                };
                let slot = next_spill_slot;
                next_spill_slot += 1;
                spill_active.insert(evicted.value, slot);
                locations.insert(evicted.value, Location::Spill(slot));
                locations.insert(iv.value, Location::Reg(r));
                active.push(*iv);
            } else {
                // Spill `iv` itself.
                let slot = next_spill_slot;
                next_spill_slot += 1;
                spill_active.insert(iv.value, slot);
                locations.insert(iv.value, Location::Spill(slot));
                // Not added to active; spilled values don't hold
                // registers.
            }
        }
    }

    Allocation {
        locations,
        num_spill_slots: next_spill_slot,
        block_order: block_order.to_vec(),
    }
}

// ─── Local helpers ──────────────────────────────────────────────────────────

fn op_reads(op: &IrOp) -> Vec<ValueId> {
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

fn term_reads(t: &Terminator) -> Vec<ValueId> {
    match t {
        Terminator::Return(Some(v)) => vec![*v],
        Terminator::Return(None) | Terminator::Deopt(_) | Terminator::Unreachable => vec![],
        Terminator::Jump(_, args) => args.clone(),
        Terminator::Branch {
            cond,
            then_args,
            else_args,
            ..
        } => {
            let mut v = Vec::with_capacity(1 + then_args.len() + else_args.len());
            v.push(*cond);
            v.extend_from_slice(then_args);
            v.extend_from_slice(else_args);
            v
        }
    }
}
