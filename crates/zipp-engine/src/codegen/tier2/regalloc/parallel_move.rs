//! Parallel-move resolution for block-param edges (SSA deconstruction).
//!
//! When control flows from block A to block B via `jump B(a1, a2, …)`
//! and B's params are `(p1, p2, …)` with respective locations, the
//! semantic effect is a **simultaneous** assignment:
//!
//! ```text
//!     loc(p1) ← loc(a1)
//!     loc(p2) ← loc(a2)
//!                …
//! ```
//!
//! This module produces the **serialised sequence of single moves**
//! that implement each edge's parallel move, introducing scratch
//! slots where needed to break cycles. The emit phase consumes the
//! output directly.
//!
//! ## Algorithm
//!
//! Textbook serialisation via topological peeling, cycle breaks via
//! a scratch slot:
//!
//! 1. A move `(src, dst)` is "ready" iff `dst` is **not a source of
//!    any still-pending move** — i.e. writing to `dst` can't clobber
//!    a value some other pending move still needs to read.
//! 2. Emit ready moves one at a time; each emission can make more
//!    moves ready.
//! 3. If no move is ready and some remain, they form a cycle. Pick
//!    any move `M` in the cycle:
//!    * Save `M.src`'s pre-move value to a scratch slot.
//!    * Remove `M` from pending, but *defer* emitting `M` itself.
//!    * Continue peeling the rest of the cycle — the saved slot
//!      decouples `M.src`'s fate from the remaining moves.
//!    * At the very end, emit `LoadFromScratch{slot → M.dst}` to
//!      realise `M` using the saved value.
//!
//! ## Why dst-in-sources, not src-in-dests
//!
//! A move `(a, b)` is safe to execute whenever no *still-pending*
//! move depends on the current value of `b`. That's equivalent to
//! asking "is `b` a source of some pending move?". Checking "is `a`
//! a destination?" would only answer whether the move's *source*
//! will be overwritten — a different question that permits unsafe
//! emission orders on chains. The version I got wrong the first time.

use std::collections::{BTreeMap, BTreeSet};

use super::Location;

/// A single move's source → destination. Both are [`Location`]s so we
/// can represent reg→reg, reg→spill, spill→reg, and spill→spill
/// moves uniformly.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Move {
    pub src: Location,
    pub dst: Location,
}

/// One emitted step of the serialised move sequence.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MoveStep {
    /// `dst ← src` — plain copy.
    Copy { src: Location, dst: Location },
    /// `scratch ← src` — save to scratch, used to break a cycle.
    SaveToScratch { src: Location, scratch: u32 },
    /// `dst ← scratch` — restore from scratch.
    LoadFromScratch { scratch: u32, dst: Location },
}

/// Resolve a parallel-move set into a serialised sequence. Returns
/// the move steps and the number of scratch slots consumed; the
/// caller adds the latter to the regalloc's `num_spill_slots` so the
/// tier-2 frame is sized correctly.
///
/// `scratch_slot_start` is the index of the first spill slot the
/// caller has reserved for this parallel move (typically
/// `num_spill_slots` after the linear-scan pass).
pub fn resolve(moves: &[Move], scratch_slot_start: u32) -> (Vec<MoveStep>, u32) {
    // Drop trivial self-moves up front.
    let pending: Vec<Move> = moves
        .iter()
        .copied()
        .filter(|m| m.src != m.dst)
        .collect();
    if pending.is_empty() {
        return (Vec::new(), 0);
    }

    // Index by destination. Two moves sharing a destination would be
    // a pathologically-shaped input — the caller shouldn't produce
    // one. If it happens, later arrivals win silently.
    let mut remaining: BTreeMap<Location, Move> = BTreeMap::new();
    for m in &pending {
        remaining.insert(m.dst, *m);
    }

    let mut out: Vec<MoveStep> = Vec::with_capacity(pending.len() + 4);
    let mut deferred_loads: Vec<(u32, Location)> = Vec::new();
    let mut scratch_used: u32 = 0;

    loop {
        // Source set — values currently "wanted" by some pending
        // move. A move (src, dst) is ready iff dst ∉ this set: no
        // remaining move is waiting to read from dst.
        let sources: BTreeSet<Location> = remaining.values().map(|m| m.src).collect();
        let ready = remaining
            .iter()
            .find(|(dst, _)| !sources.contains(*dst))
            .map(|(dst, _)| *dst);

        if let Some(dst) = ready {
            let m = remaining.remove(&dst).unwrap();
            out.push(MoveStep::Copy { src: m.src, dst: m.dst });
            continue;
        }

        if remaining.is_empty() {
            break;
        }

        // No ready moves and some remain ⇒ cycle. Pick the move
        // with the smallest destination (for determinism), save its
        // source to scratch, remove it from pending, and defer its
        // final realisation as a `LoadFromScratch` at the end.
        let pivot_dst = *remaining.keys().next().unwrap();
        let pivot = remaining.remove(&pivot_dst).unwrap();
        let slot = scratch_slot_start + scratch_used;
        scratch_used += 1;
        out.push(MoveStep::SaveToScratch {
            src: pivot.src,
            scratch: slot,
        });
        deferred_loads.push((slot, pivot.dst));
    }

    // Emit deferred loads. Each cycle has its own scratch slot
    // and its own destination; no load aliases another's dst, so
    // ordering among loads is free.
    for (slot, dst) in deferred_loads {
        out.push(MoveStep::LoadFromScratch { scratch: slot, dst });
    }

    (out, scratch_used)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(n: u8) -> Location {
        Location::Reg(n)
    }
    fn spill(n: u32) -> Location {
        Location::Spill(n)
    }

    #[test]
    fn empty_input() {
        let (out, scratch) = resolve(&[], 0);
        assert!(out.is_empty());
        assert_eq!(scratch, 0);
    }

    #[test]
    fn drops_self_moves() {
        let moves = [Move { src: reg(0), dst: reg(0) }];
        let (out, scratch) = resolve(&moves, 0);
        assert!(out.is_empty());
        assert_eq!(scratch, 0);
    }

    #[test]
    fn simple_chain() {
        // r0 → r1, r1 → r2, r2 → r3
        // Safe order: r2→r3 first (r3 is nobody's source), then
        // r1→r2 (r2 freed), then r0→r1.
        let moves = vec![
            Move { src: reg(0), dst: reg(1) },
            Move { src: reg(1), dst: reg(2) },
            Move { src: reg(2), dst: reg(3) },
        ];
        let (out, scratch) = resolve(&moves, 0);
        assert_eq!(scratch, 0);
        assert_eq!(out.len(), 3);
        let pattern: Vec<(Location, Location)> = out
            .iter()
            .map(|s| match s {
                MoveStep::Copy { src, dst } => (*src, *dst),
                _ => unreachable!("no scratch needed"),
            })
            .collect();
        assert_eq!(
            pattern,
            vec![
                (reg(2), reg(3)),
                (reg(1), reg(2)),
                (reg(0), reg(1)),
            ]
        );
    }

    #[test]
    fn swap_via_scratch() {
        // r0 ↔ r1. 2-cycle; needs one scratch slot.
        let moves = vec![
            Move { src: reg(0), dst: reg(1) },
            Move { src: reg(1), dst: reg(0) },
        ];
        let (out, scratch) = resolve(&moves, 7);
        assert_eq!(scratch, 1);
        // Expected shape: Save r? to scratch 7, Copy r? → r?, Load 7 → r?.
        assert_eq!(out.len(), 3);
        assert!(out
            .iter()
            .any(|s| matches!(s, MoveStep::SaveToScratch { scratch: 7, .. })));
        assert!(out
            .iter()
            .any(|s| matches!(s, MoveStep::LoadFromScratch { scratch: 7, .. })));
    }

    #[test]
    fn three_way_cycle() {
        // r0 → r1, r1 → r2, r2 → r0.
        let moves = vec![
            Move { src: reg(0), dst: reg(1) },
            Move { src: reg(1), dst: reg(2) },
            Move { src: reg(2), dst: reg(0) },
        ];
        let (out, scratch) = resolve(&moves, 5);
        assert_eq!(scratch, 1);
        // Should emit 1 save + 2 copies + 1 load = 4 steps.
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn mixed_chain_and_cycle() {
        // Independent chain r4 → r5 + cycle r0 ↔ r1.
        let moves = vec![
            Move { src: reg(4), dst: reg(5) },
            Move { src: reg(0), dst: reg(1) },
            Move { src: reg(1), dst: reg(0) },
        ];
        let (out, scratch) = resolve(&moves, 0);
        assert_eq!(scratch, 1);
        // Chain is ready immediately (r4 is nobody's dst, r5 is
        // nobody's source); cycle breaks afterward.
        assert!(matches!(
            out[0],
            MoveStep::Copy {
                src: Location::Reg(4),
                dst: Location::Reg(5)
            }
        ));
    }

    #[test]
    fn spill_to_reg() {
        // spill(0) → r2
        let moves = vec![Move { src: spill(0), dst: reg(2) }];
        let (out, _) = resolve(&moves, 10);
        assert_eq!(out.len(), 1);
        assert!(matches!(
            out[0],
            MoveStep::Copy {
                src: Location::Spill(0),
                dst: Location::Reg(2)
            }
        ));
    }

    /// Brute-force soundness over all valid move sets using 4 regs.
    /// Simulates emission on a register file and compares against
    /// the expected parallel-move post-state.
    #[test]
    fn soundness_small_permutations() {
        const N: usize = 4;
        for mask in 0u32..(1 << (N * N)) {
            let mut moves = Vec::new();
            let mut dst_seen = [false; 8];
            let mut bad = false;
            for i in 0..N {
                for j in 0..N {
                    let bit = (mask >> (i * N + j)) & 1;
                    if bit == 1 {
                        if i == j {
                            bad = true;
                            break;
                        }
                        if dst_seen[j] {
                            bad = true;
                            break;
                        }
                        dst_seen[j] = true;
                        moves.push(Move {
                            src: reg(i as u8),
                            dst: reg(j as u8),
                        });
                    }
                }
                if bad {
                    break;
                }
            }
            if bad {
                continue;
            }
            simulate_and_verify(&moves);
        }
    }

    fn simulate_and_verify(moves: &[Move]) {
        // Start: reg i holds value i.
        // Parallel-move semantics: after, dst holds src's pre-move value.
        let mut expected = [0u8; 8];
        for i in 0..8 {
            expected[i] = i as u8;
        }
        for m in moves {
            if let (Location::Reg(src), Location::Reg(dst)) = (m.src, m.dst) {
                expected[dst as usize] = src;
            }
        }

        let (steps, _) = resolve(moves, 0);
        let mut regs = [0u8; 8];
        for i in 0..8 {
            regs[i] = i as u8;
        }
        let mut scratch: BTreeMap<u32, u8> = BTreeMap::new();
        for s in steps {
            match s {
                MoveStep::Copy { src, dst } => {
                    let v = read_loc(&regs, &scratch, src);
                    write_loc(&mut regs, &mut scratch, dst, v);
                }
                MoveStep::SaveToScratch { src, scratch: slot } => {
                    let v = read_loc(&regs, &scratch, src);
                    scratch.insert(slot, v);
                }
                MoveStep::LoadFromScratch { scratch: slot, dst } => {
                    let v = *scratch.get(&slot).unwrap();
                    write_loc(&mut regs, &mut scratch, dst, v);
                }
            }
        }

        // Only check registers that were actually affected (either
        // written to or present as a source). Unaffected registers
        // should still hold their initial value — which matches
        // `expected` by construction.
        for i in 0..8 {
            assert_eq!(
                regs[i], expected[i],
                "mismatch at reg {i} for moves {moves:?}: got {regs:?}, expected {expected:?}"
            );
        }
    }

    fn read_loc(regs: &[u8; 8], scratch: &BTreeMap<u32, u8>, loc: Location) -> u8 {
        match loc {
            Location::Reg(n) => regs[n as usize],
            Location::Spill(n) => *scratch.get(&n).unwrap_or(&0),
        }
    }
    fn write_loc(regs: &mut [u8; 8], scratch: &mut BTreeMap<u32, u8>, loc: Location, v: u8) {
        match loc {
            Location::Reg(n) => regs[n as usize] = v,
            Location::Spill(n) => {
                scratch.insert(n, v);
            }
        }
    }
}
