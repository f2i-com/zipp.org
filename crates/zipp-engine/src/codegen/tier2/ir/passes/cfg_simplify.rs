//! Control-flow graph simplification.
//!
//! Phase-3 scope: remove blocks that aren't reachable from the entry.
//! This catches the dead fall-through blocks the translator emits
//! after `Halt` / `HaltValue` (harmless but noisy in IR dumps), plus
//! any block made unreachable by a later const-fold that collapses a
//! conditional branch into an unconditional jump.
//!
//! Deliberately **not** included in this first cut:
//!
//! * Single-predecessor block merging (threading jumps through a
//!   block that only has one predecessor). It's straightforward but
//!   moves block IDs around and makes debugging before-and-after
//!   diffs harder; adding it as a separate pass once the codegen
//!   exists to prove it helps.
//! * Empty-block elimination. Same reasoning.
//! * Folding `Branch(ConstBool(true), …)` into an unconditional
//!   `Jump`. This is a peephole that const-fold should cover, but
//!   cross-op/terminator folding isn't plumbed in phase 3 — landed
//!   separately once we have type feedback.
//!
//! ## Algorithm
//!
//! 1. Walk the CFG from `blocks[0]` collecting reachable ids.
//! 2. If every block is reachable, do nothing.
//! 3. Otherwise, rebuild `blocks` keeping only the reachable ones and
//!    **remap every BlockId in terminators** to the new indices.
//!
//! BlockId remapping is what makes this an O(n) pass rather than
//! O(n²) — we compute a `old_id → new_id` table once and apply it to
//! all terminators in a single walk.

use std::collections::{HashMap, HashSet, VecDeque};

use super::super::types::{Block, BlockId, IrFunction, Terminator};

pub fn run(func: &mut IrFunction) -> bool {
    let reachable = reachable_blocks(func);
    if reachable.len() == func.blocks.len() {
        return false;
    }

    // Build old→new id map in order: iterate `func.blocks` preserving
    // input order (so bb0 stays bb0, later blocks get compacted).
    let mut old_to_new: HashMap<BlockId, BlockId> = HashMap::new();
    let mut kept: Vec<Block> = Vec::with_capacity(reachable.len());
    for block in &func.blocks {
        if reachable.contains(&block.id) {
            let new_id = BlockId(kept.len() as u32);
            old_to_new.insert(block.id, new_id);
            let mut nb = block.clone();
            nb.id = new_id;
            kept.push(nb);
        }
    }
    for block in &mut kept {
        remap_term(&mut block.term, &old_to_new);
    }
    func.blocks = kept;

    true
}

fn reachable_blocks(func: &IrFunction) -> HashSet<BlockId> {
    let mut seen = HashSet::new();
    let mut work: VecDeque<BlockId> = VecDeque::new();
    if func.blocks.is_empty() {
        return seen;
    }
    work.push_back(BlockId(0));
    seen.insert(BlockId(0));

    while let Some(b) = work.pop_front() {
        let block = match func.blocks.get(b.0 as usize) {
            Some(x) => x,
            None => continue,
        };
        for succ in term_successors(&block.term) {
            if seen.insert(succ) {
                work.push_back(succ);
            }
        }
    }
    seen
}

fn term_successors(term: &Terminator) -> Vec<BlockId> {
    match term {
        Terminator::Return(_) | Terminator::Deopt(_) | Terminator::Unreachable => vec![],
        Terminator::Jump(b, _) => vec![*b],
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => vec![*then_block, *else_block],
    }
}

fn remap_term(term: &mut Terminator, map: &HashMap<BlockId, BlockId>) {
    match term {
        Terminator::Return(_) | Terminator::Deopt(_) | Terminator::Unreachable => {}
        Terminator::Jump(b, _) => {
            if let Some(&nb) = map.get(b) {
                *b = nb;
            }
        }
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => {
            if let Some(&nb) = map.get(then_block) {
                *then_block = nb;
            }
            if let Some(&nb) = map.get(else_block) {
                *else_block = nb;
            }
        }
    }
}
