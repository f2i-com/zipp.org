//! IR rewriting passes.
//!
//! Every pass here:
//!
//! 1. Takes `&mut IrFunction` (all rewrites are in-place; some passes
//!    rebuild Vec contents but the structural identity is preserved).
//! 2. Runs fast — O(n log n) worst case for any individual pass, with
//!    n = total op count. Phase-3 budget: the whole suite should run
//!    in < 1 ms for the vast majority of functions.
//! 3. Is re-runnable. Running `const_fold` twice produces the same
//!    output as running it once. (Needed because other passes may
//!    expose new fold opportunities.)
//! 4. **Preserves verifier invariants.** Every pass ends with the IR
//!    passing `verify::verify(&func)`. Tests run the verifier after
//!    each pass unconditionally.
//!
//! ## Pass ordering
//!
//! The canonical tier-2 pipeline runs:
//!
//! ```text
//!   build → const_fold → copy_prop → dce → cfg_simplify
//!                       ↑                 │
//!                       └─ loop until a pass produces no change ─┘
//! ```
//!
//! CFG simplification can expose new constant-fold opportunities
//! (e.g. `if (true) { … }` becoming an unconditional jump), so we
//! fix-point the cheap passes together. Speculative-narrowing
//! (phase 5) is a separate pipeline that runs *after* this one.
//!
//! ## Why these four first
//!
//! They're the minimum transformation suite that:
//!
//! * Validates the IR is rewritable without subtle aliasing traps.
//! * Exercises the verifier on non-trivial post-rewrite shapes.
//! * Provides real, measurable simplification on the benchmark
//!   corpus (const-math expressions, dead stores to overwritten
//!   registers, the inevitable dead fall-through block after
//!   `Halt`).

pub mod cfg_simplify;
pub mod const_fold;
pub mod copy_prop;
pub mod dce;
pub mod inline;
pub mod iv_reduce;
pub mod licm;
pub mod speculate;

use super::IrFunction;

/// Run the phase-3 canonical pipeline on `func` to fixed point.
/// Returns the number of outer iterations — exposed for tests that
/// want to confirm passes converge.
pub fn run_default_pipeline(func: &mut IrFunction) -> usize {
    let mut iters = 0usize;
    loop {
        let mut changed = false;
        changed |= const_fold::run(func);
        changed |= copy_prop::run(func);
        changed |= iv_reduce::run(func);
        changed |= licm::run(func);
        changed |= dce::run(func);
        changed |= cfg_simplify::run(func);
        iters += 1;
        if !changed {
            break;
        }
        // Safety valve — passes must converge; if they don't, that's
        // a bug in one of them, not a reason to loop forever.
        if iters > 16 {
            break;
        }
    }
    iters
}
