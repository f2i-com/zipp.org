//! Tier O: the optimizing tier.
//!
//! Every other tier translates bytecode straight to machine code: Tier C
//! keeps each value boxed in the VM register window, and only loop regions
//! keep unboxed values in machine registers. Tier O builds an IR first: the
//! function's control-flow graph in SSA form over the VM registers
//! (`build`), with each value in one representation (an unboxed `f64`, an
//! `i32`, a boolean or a NaN-boxed value), then allocates machine registers
//! by linear scan (`alloc`) and emits x86-64 (`emit`).
//!
//! A body has Tier C's ABI (`extern "win64" fn(regs, *mut u32 bail, vm)`):
//! it installs as the function's compiled body and cross entry, and the
//! interpreter enters it wherever it enters Tier C's. Every speculation is a
//! guard that carries a frame state: the bytecode position and, for each VM
//! register live there, the SSA value it holds. A failed guard boxes those
//! values into the register window and leaves through the bail word at that
//! position, where the interpreter goes on exactly as after a Tier C bail.
//! A body that deopts `DEOPT_LIMIT` times is evicted and its function never
//! takes Tier O again (`Jit::note_opt_deopt`).
//!
//! First slice: Number arithmetic, comparisons, strict equality, bitwise
//! ops, branches and loops, own-data property reads behind the receiver's
//! shape (the interpreter cache's `PropShapePlan` pair), returns. No calls:
//! nothing in a body can allocate or collect, so a heap value held in a
//! machine register needs no rooting. `ZIPP_OPT=1` turns it on;
//! `ZIPP_OPT_DEOPT_EVERY=N` fails every Nth guard check for testing.

mod alloc;
mod build;
mod emit;

use super::{FuncProto, JitFn, PropShapePlan};

env_on_switch! {
    /// `ZIPP_OPT=1`: functions Tier O admits compile there instead of Tier C.
    #[inline]
    fn opt_enabled() = "ZIPP_OPT"
}

/// Deopts after which a Tier O body is evicted for good.
pub(crate) const DEOPT_LIMIT: u32 = 64;

/// `ZIPP_OPT_DEOPT_EVERY=N`: every Nth guard check deopts (0: off).
pub(crate) fn stress_every() -> u32 {
    use std::sync::OnceLock;
    static N: OnceLock<u32> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("ZIPP_OPT_DEOPT_EVERY")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    })
}

/// The stress countdown the emitted guards decrement (`stress_every`).
pub(crate) static STRESS_COUNTDOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Tier O's body for `proto`, or `None` when it admits none.
pub(crate) fn compile(proto: &FuncProto, func_id: u32, prop_shape: &PropShapePlan) -> Option<JitFn> {
    let func = build::build(proto, func_id, prop_shape)?;
    let alloc = alloc::allocate(&func);
    let f = emit::emit(&func, &alloc)?;
    if crate::jitlog_enabled() {
        eprintln!(
            "[jit] Tier O fn{func_id} compiled ({} ops, {} blocks, {} values, {} guards, {} spills)",
            proto.code.len(),
            func.blocks.len(),
            func.insts.len(),
            func.states.len(),
            alloc.slots
        );
    }
    Some(f)
}

impl super::Jit {
    /// Whether `func_id` may take Tier O.
    pub(crate) fn opt_may_compile(&self, func_id: u32) -> bool {
        opt_enabled() && self.meter.is_none() && !self.opt_blacklist.contains(&func_id)
    }

    /// A Tier O body of `func_id` deopted at `ip`: after `DEOPT_LIMIT`
    /// deopts the body is evicted (parked, never freed under itself) and the
    /// function's next compile takes Tier C. Under the stress switch every
    /// guard deopts in turn, and nothing is evicted.
    pub(crate) fn note_opt_deopt(&mut self, func_id: u32, ip: u32) {
        let every = stress_every();
        if every != 0 {
            STRESS_COUNTDOWN.store(every, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        let n = self.opt_deopts.entry(func_id).or_insert(0);
        *n += 1;
        if *n < DEOPT_LIMIT {
            return;
        }
        self.opt_blacklist.insert(func_id);
        let Some(f) = self.compiled.remove(&func_id) else {
            return;
        };
        if crate::jitlog_enabled() {
            eprintln!("[jit] Tier O fn{func_id} EVICTED ({DEOPT_LIMIT} deopts, last at ip {ip})");
        }
        self.retired_fns.push(f);
        let rearm = self.fn_threshold().saturating_sub(1);
        self.counts.insert(func_id, rearm);
        self.set_fn_state(func_id, super::FN_COLD);
        self.clear_cross_entry(func_id);
        if self.self_cache.is_some_and(|(id, _)| id == func_id) {
            self.self_cache = None;
        }
    }
}
