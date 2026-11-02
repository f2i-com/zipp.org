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

use rustc_hash::FxHashMap;

use super::{ClassReadPlan, CrossCallPlan, FuncProto, JitFn, PropShapePlan};

/// The bodies Tier O may inline, by function id, each with its own property
/// plan (built by the VM, which has the callees' caches).
pub(crate) type OptCallees<'a> = FxHashMap<u32, (&'a FuncProto, PropShapePlan)>;

/// The call sites with one plain user-function callee: a `CallMethod`
/// site's class method (the class-method lane's plan), else its own data
/// method by shape, and a `Call` site's single cross-call arm.
pub(crate) fn plan_sites(cross_plan: &CrossCallPlan, class_read: &ClassReadPlan) -> FxHashMap<usize, build::Site> {
    let mut sites = FxHashMap::default();
    for (&ip, lane) in class_read {
        sites.insert(
            ip,
            build::Site::ClassMethod {
                recv_shape: lane.recv_shape,
                class: lane.class,
                ver: lane.ver,
                epoch: lane.epoch,
                fid: lane.fid,
            },
        );
    }
    for (&ip, site) in cross_plan {
        if sites.contains_key(&ip) {
            continue;
        }
        if let Some(m) = site.cross3m {
            sites.insert(
                ip,
                build::Site::Method {
                    shape: m.shape,
                    slot: m.slot,
                    fid: m.fid,
                },
            );
        } else if site.cross3.len() == 1 && !site.class_getter && !site.class_method {
            if let Some(arm) = site.cross3.iter().next() {
                sites.insert(ip, build::Site::Call { fid: arm.fid });
            }
        }
    }
    sites
}

/// The callees `plan_sites` names, for the VM to supply (`OptCallees`).
pub(crate) fn plan_callee_ids(cross_plan: &CrossCallPlan, class_read: &ClassReadPlan) -> Vec<u32> {
    let mut ids: Vec<u32> = plan_sites(cross_plan, class_read).values().map(|s| s.fid()).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

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
pub(crate) fn compile(
    proto: &FuncProto,
    func_id: u32,
    prop_shape: &PropShapePlan,
    cross_plan: &CrossCallPlan,
    class_read: &ClassReadPlan,
    callees: &OptCallees,
) -> Option<JitFn> {
    if stress_every() != 0 && STRESS_COUNTDOWN.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        STRESS_COUNTDOWN.store(stress_every(), std::sync::atomic::Ordering::Relaxed);
    }
    let sites = plan_sites(cross_plan, class_read);
    let inputs = build::Inputs {
        prop_shape,
        sites: &sites,
        callees,
    };
    let func = build::build(proto, func_id, &inputs)?;
    let alloc = alloc::allocate(&func);
    let f = emit::emit(&func, &alloc)?;
    if crate::jitlog_enabled() {
        eprintln!(
            "[jit] Tier O fn{func_id} compiled ({} ops, {} blocks, {} values, {} guards, {} spills, {} inlined)",
            proto.code.len(),
            func.blocks.len(),
            func.insts.len(),
            func.states.len(),
            alloc.slots,
            func.inlined
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
