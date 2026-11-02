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

use super::{ClassReadPlan, CrossCallPlan, FuncProto, JitFn, MathIntrinsicGuard, PropShapePlan};

/// What the VM supplies for Tier O's inlining, from the caches.
#[derive(Default)]
pub(crate) struct OptCallees<'a> {
    /// The bodies Tier O may inline, by function id, each with its own
    /// property plan.
    pub(crate) bodies: FxHashMap<u32, (&'a FuncProto, PropShapePlan)>,
    /// The `GetProp` sites whose cache found a class getter
    /// (`Vm::ic_class_getter_read`), by position.
    pub(crate) getters: ClassReadPlan,
    /// The class values `LoadClassValue` has while no class is evaluated
    /// again (`Vm::class_value_fixed`), by class id, and the
    /// `Vm::mi_class_epoch` that says so.
    pub(crate) class_values: FxHashMap<u32, u64>,
    pub(crate) class_epoch: u32,
    /// The `GetProp` sites with no single-shape plan whose cache holds own
    /// data properties of several shapes (`Vm::ic_own_data_ways`).
    pub(crate) poly: FxHashMap<usize, Vec<(u32, u32)>>,
}

/// The call sites with one plain user-function callee: a `CallMethod`
/// site's class method (the class-method lane's plan), else its own data
/// method by shape, a `Call` site's single cross-call arm, and a `GetProp`
/// site's class getter.
pub(crate) fn plan_sites(
    cross_plan: &CrossCallPlan,
    class_read: &ClassReadPlan,
    getters: &ClassReadPlan,
) -> FxHashMap<usize, build::Site> {
    let mut sites = FxHashMap::default();
    for (&ip, lane) in getters {
        sites.insert(
            ip,
            build::Site::ClassGetter {
                recv_shape: lane.recv_shape,
                class: lane.class,
                ver: lane.ver,
                epoch: lane.epoch,
                fid: lane.fid,
            },
        );
    }
    for (&ip, lane) in class_read {
        if sites.contains_key(&ip) {
            continue;
        }
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
pub(crate) fn plan_callee_ids(cross_plan: &CrossCallPlan, class_read: &ClassReadPlan, getters: &ClassReadPlan) -> Vec<u32> {
    let mut ids: Vec<u32> = plan_sites(cross_plan, class_read, getters).values().map(|s| s.fid()).collect();
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

/// What a compile knows of the running VM.
pub(crate) struct Env<'a> {
    /// The function's string constants, rooted (`Vm::jit_build_const_strs`).
    pub(crate) const_strs: &'a FxHashMap<u32, u64>,
    /// The intrinsic `Math` and its methods.
    pub(crate) math: Option<MathIntrinsicGuard>,
    /// The nursery barrier may test generation bytes.
    pub(crate) gen_lane: bool,
    /// The sites earlier bodies deopted at (`OptFeedback::generic`).
    pub(crate) generic: Option<&'a rustc_hash::FxHashSet<u32>>,
    /// The helpers Tier C's emitted cross-call lanes call.
    pub(crate) heap: super::HeapHelpers,
}

/// What a function's Tier O bodies learned by deopting.
#[derive(Default)]
pub(crate) struct OptFeedback {
    /// Deopts of the current body.
    deopts: u32,
    /// Its deopt sites (`FrameState::why`).
    sites: rustc_hash::FxHashSet<u32>,
    /// The sites the next body builds without the speculation that failed:
    /// a property read with its slow path, `+` as the whole operator, `===`
    /// with the string compare, a call not inlined, parameters left boxed.
    pub(crate) generic: rustc_hash::FxHashSet<u32>,
    /// Bodies evicted so far.
    rounds: u32,
}

/// Bodies a function may have evicted (each recompiled with what the last
/// one learned) before it takes Tier C for good.
const MAX_ROUNDS: u32 = 4;

/// Slow property reads at one site before its function asks for a replan
/// (`Jit::opt_slow_read_due`), and the replans a function may have.
const SLOW_READS_REPLAN: u32 = 1024;
const MAX_REPLANS: u8 = 2;

/// Tier O's body for `proto`, or `None` when it admits none.
pub(crate) fn compile(
    proto: &FuncProto,
    func_id: u32,
    prop_shape: &PropShapePlan,
    cross_plan: &CrossCallPlan,
    class_read: &ClassReadPlan,
    callees: &OptCallees,
    env: &Env,
) -> Option<JitFn> {
    if stress_every() != 0 && STRESS_COUNTDOWN.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        STRESS_COUNTDOWN.store(stress_every(), std::sync::atomic::Ordering::Relaxed);
    }
    let sites = plan_sites(cross_plan, class_read, &callees.getters);
    let new_lanes: FxHashMap<usize, &'static super::NewLanePlan> =
        cross_plan.iter().filter_map(|(&ip, s)| s.new_lane.map(|l| (ip, l))).collect();
    let inputs = build::Inputs {
        new_lanes: &new_lanes,
        class_values: &callees.class_values,
        class_epoch: callees.class_epoch,
        poly: &callees.poly,
        prop_shape,
        sites: &sites,
        callees: &callees.bodies,
        const_strs: env.const_strs,
        math: env.math,
        generic: env.generic,
    };
    let mut func = build::build(proto, func_id, &inputs)?;
    func.gen_lane = env.gen_lane;
    func.heap = Some(env.heap);
    func.cross = cross_plan
        .iter()
        .filter(|(_, s)| !s.cross3.is_empty() || s.cross3m.is_some())
        .map(|(&ip, &s)| (ip as u32, s))
        .collect();
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

    /// A Tier O body of `func_id` deopted, blaming site `why`
    /// (`FrameState::why`): after `DEOPT_LIMIT` deopts the body is evicted
    /// (parked, never freed under itself) and the function compiles again,
    /// building every site that deopted without its speculation; after
    /// `MAX_ROUNDS` bodies it takes Tier C. Under the stress switch every
    /// guard deopts in turn, and nothing is evicted.
    pub(crate) fn note_opt_deopt(&mut self, func_id: u32, why: u32) {
        let every = stress_every();
        if every != 0 {
            STRESS_COUNTDOWN.store(every, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        let fb = self.opt_feedback.entry(func_id).or_default();
        fb.deopts += 1;
        fb.sites.insert(why);
        if fb.deopts < DEOPT_LIMIT {
            return;
        }
        fb.deopts = 0;
        fb.rounds += 1;
        let sites = std::mem::take(&mut fb.sites);
        fb.generic.extend(sites.iter().copied());
        // A second body that deopted as often learned too slowly site by
        // site (a comparator's every compare met strings): every site.
        if fb.rounds >= 2 {
            fb.generic.insert(build::ALL_SITES);
        }
        let round = fb.rounds;
        if round >= MAX_ROUNDS {
            self.opt_blacklist.insert(func_id);
        }
        if crate::jitlog_enabled() && self.compiled.contains_key(&func_id) {
            let mut s: Vec<u32> = sites.into_iter().collect();
            s.sort_unstable();
            eprintln!("[jit] Tier O fn{func_id} EVICTED ({DEOPT_LIMIT} deopts, round {round}, sites {s:?})");
        }
        self.evict_opt(func_id);
    }

    /// Whether the `GetProp` at `(func_id, ip)` of a Tier O body has gone
    /// through its slow helper `SLOW_READS_REPLAN` times since the last ask
    /// (`vm::jit_opt_get_prop_slow`), with a replan still allowed.
    pub(crate) fn opt_slow_read_due(&mut self, func_id: u32, ip: u32) -> bool {
        if self.opt_replans.get(&func_id).copied().unwrap_or(0) >= MAX_REPLANS {
            return false;
        }
        let n = self.opt_slow_reads.entry((func_id, ip)).or_insert(0);
        *n += 1;
        if *n < SLOW_READS_REPLAN {
            return false;
        }
        *n = 0;
        true
    }

    /// Compile `func_id` again from its caches as they are now: the
    /// interpreter's cache knows the shapes of the read at `ip`, which the
    /// body was compiled without (`MAX_REPLANS` per function).
    pub(crate) fn opt_replan(&mut self, func_id: u32, ip: u32) {
        *self.opt_replans.entry(func_id).or_insert(0) += 1;
        if crate::jitlog_enabled() && self.compiled.contains_key(&func_id) {
            eprintln!("[jit] Tier O fn{func_id} EVICTED (replan: read at ip {ip} now has shapes)");
        }
        self.evict_opt(func_id);
    }

    /// Park `func_id`'s Tier O body (never freed under itself) and re-arm
    /// its counter so its next call compiles it again.
    fn evict_opt(&mut self, func_id: u32) {
        // A parked body still running may ask after another tier's took over.
        if !self.opt_bodies.contains(&func_id) {
            return;
        }
        let Some(f) = self.compiled.remove(&func_id) else {
            return;
        };
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
