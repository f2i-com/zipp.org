//! Native code generation.
//!
//! Two JIT tiers live here:
//!
//! * [`djit`] — tier 1. dynasm-based single-pass JIT (x86-64 and
//!   AArch64). The only production JIT today. Compiles hot functions
//!   after four calls; emits inline property caches, bounds-checked
//!   array access, self-recursive jumps, NaN-boxed arithmetic fast
//!   paths, and inline Map.set/get via a 2048-entry intern cache.
//! * [`tier2`] — **in progress.** An optimising tier that sits on top
//!   of tier 1 for functions hot enough to justify the longer compile
//!   time. Currently only the SSA-form IR scaffolding is in place;
//!   see [`TIER2-JIT-PLAN.md`] at the repo root and the module docs
//!   for the phased plan.
//!
//! The previous Cranelift experiment (behind a `jit` feature flag)
//! was removed — dynasm beat it on every benchmark and maintaining
//! two parallel JIT codebases was not worth the cost. See the
//! CHANGELOG if you need to rebuild it from git history.

pub mod djit;
pub mod tier2;
