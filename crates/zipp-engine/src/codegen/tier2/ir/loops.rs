//! Loop detection from the IR's control-flow graph.
//!
//! Finds **natural loops** — the standard compilers-textbook notion:
//! a natural loop is identified by a back-edge `B → H` where `H`
//! (the header) dominates `B` (the latch). The loop body is the set
//! of blocks from which there's a path to the latch that doesn't
//! leave the loop (equivalently: blocks dominated by the header
//! that can reach the latch).
//!
//! This is the shape phase 7+ passes operate on:
//!
//! * **Loop-invariant code motion** (this phase) uses the header as
//!   the hoist destination.
//! * **Induction-variable analysis** (future) scans the header's
//!   block parameters for the `phi(init_from_preheader, update_from_
//!   latch)` pattern.
//! * **Strength reduction** walks the loop body replacing `IV * k`
//!   forms with an auxiliary IV stepped by `orig_step * k`.
//!
//! ## Algorithm
//!
//! 1. Compute dominators (iterative fixpoint, already implemented
//!    for the verifier — we ship a second copy here since it's cheap
//!    and the verifier version is private).
//! 2. For each edge `B → H` where `H` dominates `B`, emit a natural
//!    loop with header `H` and latch `B`.
//! 3. Compute the loop body by collecting all blocks `X` such that
//!    `X` is dominated by `H` AND `X` can reach `B` without leaving
//!    the set of blocks dominated by `H`. Standard compute via
//!    reverse-BFS from the latch.
//!
//! Phase-7 scope keeps this single-pass and doesn't try to nest
//! loops or compute LoopInfo hierarchies. Each natural loop gets
//! its own `Loop` struct; outer/inner relationships are derivable
//! on demand.

use std::collections::{BTreeSet, HashMap, VecDeque};

use super::types::{BlockId, IrFunction, Terminator};

/// A natural loop in the CFG.
#[derive(Clone, Debug)]
pub struct Loop {
    /// The header — the unique entry point to the loop body.
    pub header: BlockId,
    /// The latch — the block whose back-edge created this loop. A
    /// single header may have multiple latches (via multiple back-
    /// edges); phase-7 emits one `Loop` per back-edge and we leave
    /// the dedup for consumers that care.
    pub latch: BlockId,
    /// All blocks belonging to the loop body, including the header
    /// and the latch.
    pub body: BTreeSet<BlockId>,
}

impl Loop {
    /// True if `b` is part of the loop body. Includes the header.
    pub fn contains(&self, b: BlockId) -> bool {
        self.body.contains(&b)
    }
}

/// Find every natural loop in the function. Returns them in a
/// deterministic order (header id asc, then latch id asc) so tests
/// aren't sensitive to HashMap iteration quirks.
pub fn find_loops(func: &IrFunction) -> Vec<Loop> {
    let dom = compute_dominators(func);
    let mut loops = Vec::new();

    // Scan every block for back-edges.
    for (idx, block) in func.blocks.iter().enumerate() {
        let me = BlockId(idx as u32);
        for succ in term_successors(&block.term) {
            // Back-edge iff `succ` dominates `me`.
            if dom.dominates(succ, me) {
                let body = compute_loop_body(func, succ, me, &dom);
                loops.push(Loop {
                    header: succ,
                    latch: me,
                    body,
                });
            }
        }
    }

    loops.sort_by_key(|l| (l.header.0, l.latch.0));
    loops
}

// ─── Internal helpers ───────────────────────────────────────────────────────

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

/// Walk backward from `latch` toward `header`, collecting every block
/// that's dominated by `header` along the way. That set is the loop
/// body (standard textbook algorithm).
fn compute_loop_body(
    func: &IrFunction,
    header: BlockId,
    latch: BlockId,
    dom: &Dominance,
) -> BTreeSet<BlockId> {
    // Compute predecessors lazily for this loop.
    let preds = compute_preds(func);

    let mut body = BTreeSet::new();
    body.insert(header);
    if latch != header {
        body.insert(latch);
    }

    let mut work: VecDeque<BlockId> = VecDeque::new();
    if latch != header {
        work.push_back(latch);
    }
    while let Some(b) = work.pop_front() {
        for p in preds.get(&b).map(|v| v.as_slice()).unwrap_or(&[]) {
            // Only include predecessors that are inside the dominance
            // subtree rooted at the header; otherwise they're
            // outside the loop.
            if dom.dominates(header, *p) && body.insert(*p) {
                work.push_back(*p);
            }
        }
    }
    body
}

fn compute_preds(func: &IrFunction) -> HashMap<BlockId, Vec<BlockId>> {
    let mut preds: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
    for (i, block) in func.blocks.iter().enumerate() {
        let me = BlockId(i as u32);
        for succ in term_successors(&block.term) {
            preds.entry(succ).or_default().push(me);
        }
    }
    preds
}

// ─── Dominance (iterative fixpoint) ─────────────────────────────────────────
//
// A second copy of the verifier's dominance computation, kept here so
// passes can query it without plumbing it through. For the typical
// < 1 000-block function this runs in microseconds.

struct Dominance {
    /// `doms[b]` = set of blocks that dominate block `b` (inclusive).
    doms: Vec<BTreeSet<BlockId>>,
}

impl Dominance {
    /// True if `a` dominates `b` (reflexive — `a == b` counts).
    fn dominates(&self, a: BlockId, b: BlockId) -> bool {
        self.doms
            .get(b.0 as usize)
            .map(|s| s.contains(&a))
            .unwrap_or(false)
    }
}

fn compute_dominators(func: &IrFunction) -> Dominance {
    let n = func.blocks.len();
    let all: BTreeSet<BlockId> = (0..n as u32).map(BlockId).collect();
    let mut doms: Vec<BTreeSet<BlockId>> = (0..n)
        .map(|i| {
            if i == 0 {
                let mut s = BTreeSet::new();
                s.insert(BlockId(0));
                s
            } else {
                all.clone()
            }
        })
        .collect();

    let preds = compute_preds(func);

    let mut changed = true;
    while changed {
        changed = false;
        for i in 1..n {
            let i_id = BlockId(i as u32);
            let my_preds = match preds.get(&i_id) {
                Some(v) if !v.is_empty() => v,
                _ => continue,
            };
            let mut it = my_preds.iter();
            let first = it.next().unwrap();
            let mut new_dom = doms[first.0 as usize].clone();
            for p in it {
                new_dom = new_dom
                    .intersection(&doms[p.0 as usize])
                    .copied()
                    .collect();
            }
            new_dom.insert(i_id);
            if new_dom != doms[i] {
                doms[i] = new_dom;
                changed = true;
            }
        }
    }

    Dominance { doms }
}
