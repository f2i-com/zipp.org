// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Is `ZIPP_JITDECLINE=1` set? Latched once per process, so the whole
/// `[decline-reason]` channel costs one relaxed load rather than an env lookup.
pub(crate) fn jitdecline_on() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_JITDECLINE").is_some();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// WHICH region the next `[decline-reason]` line is about: `(function name,
/// start ip, end ip)`.
///
/// A decline is named from two places that cannot see each other — `decline!`
/// fires inside the planner, `decline_emit` fires inside three separate
/// EMITTERS long after the planner returned — and a bare reason with no region
/// on it cannot be tied to the region that produced it, which is what made
/// `ZIPP_JITDECLINE=1` unusable for attributing a decline in a program with
/// more than one hot function. Stated ONCE here rather than restated at all 34
/// call sites: `plan_region_cold` records it on the way in, and every emitter
/// runs strictly after the plan it is emitting, on the same thread.
///
/// Diagnostic only: nothing is recorded at all unless `jitdecline_on()`.
fn set_decline_region(proto: &FuncProto, start: u32, end: u32) {
    if !jitdecline_on() {
        return;
    }
    DECLINE_REGION.with(|c| {
        let mut c = c.borrow_mut();
        c.0.clear();
        c.0.push_str(if proto.name.is_empty() {
            "<anon>"
        } else {
            proto.name.as_str()
        });
        c.1 = start;
        c.2 = end;
    });
}

thread_local! {
    static DECLINE_REGION: std::cell::RefCell<(String, u32, u32)> =
        std::cell::RefCell::new((String::new(), u32::MAX, u32::MAX));
}

/// `fn=<name> [start,end]` for the region `set_decline_region` last recorded.
fn decline_region() -> String {
    DECLINE_REGION.with(|c| {
        let c = c.borrow();
        if c.1 == u32::MAX {
            "fn=? [?,?]".to_string()
        } else {
            format!("fn={} [{},{}]", c.0, c.1, c.2)
        }
    })
}

/// Decline this region, naming the reason under `ZIPP_JITDECLINE=1`. The planner
/// has ~25 exit points and `ZIPP_JITLOG` only reports `plan_region=None`, which
/// says a region missed the fastest tier but not what to fix. Diagnostic only —
/// the env lookup happens once per declined region-compile, never per iteration.
macro_rules! decline {
    ($reason:expr) => {{
        if crate::codegen::plan_region::jitdecline_on() {
            eprintln!("[decline-reason] {}: {}", decline_region(), $reason);
        }
        return PlanOutcome::Decline;
    }};
}

/// Inner-planner outcome: a plan, a definitive decline, or "the xmm pool
/// overflowed but constant hoisting held permanent homes — worth one retry
/// with hoisting off". Hoisting a loop-invariant constant saves a per-iteration
/// materialise (~2 ops), but it pins a home for the WHOLE region; when that is
/// the difference between compiling on a register tier and declining to the
/// memory tier (B94 priced that tier gap at 3.2x), the constant goes back in
/// the body. The retry is silent — a `[decline-reason]` only prints if the
/// retry ALSO fails, so the documented log-reading rule (regalloc = the compile
/// with no decline line) still holds.
enum PlanOutcome {
    Plan(Box<RegionPlan>),
    RetryNoHoist,
    Decline,
}

/// Name a POST-PLAN decline through the same `[decline-reason]` channel as
/// `decline!`. The register-tier emitters can pass `region_can_compile` AND
/// `plan_region` and still abandon the region mid-emission (an op the planner
/// types but the emitter has no arm for — e.g. `Mod`, `MathOp` — or an
/// assembler failure). Without a name the region silently falls to the memory
/// tier, and the documented log-reading rule ("the regalloc path is the one
/// with NO [decline-reason] line") then mislabels MEM as REGALLOC. Diagnostic
/// only — callers decline exactly as before; the env lookup happens once per
/// declined region-compile, never per iteration.
pub(crate) fn decline_emit(reason: impl std::fmt::Display) {
    if jitdecline_on() {
        eprintln!("[decline-reason] {}: {reason}", decline_region());
    }
}

env_off_switch! {
    /// W28 TYPE-AWARE LIVE-RANGE SPLITTING. Default ON;
    /// `ZIPP_NO_TYPE_SPLIT=1` restores the pre-wave planner, in which a VM
    /// register used with two different `VTy`s anywhere in a region declines
    /// the WHOLE region ("type conflict on a reused register") to the boxed
    /// MEM tier. With the switch off `plan_type_splits` returns an empty map
    /// and every downstream site is a no-op, so a binary with
    /// `ZIPP_NO_TYPE_SPLIT=1` set is behaviourally identical to the pre-wave
    /// one — which is what lets the mechanism be priced with
    /// `tools/bench.py --ab <exe> <exe> --ab-env - ZIPP_NO_TYPE_SPLIT=1`.
    ///
    /// See `RegionPlan::ty_splits` for the mechanism and `plan_type_splits`
    /// for the legality predicate.
    fn type_split_enabled() = "ZIPP_NO_TYPE_SPLIT"
}

/// Plan register homes for `[start, end]`, or `None` to decline (use mem path).
/// `ta_plan` (unboxed-region epic): the pinned-TypedArray plan, threaded so a
/// later increment can admit a pinned-Float64Array element GetIndex/SetIndex as a
/// VTy::Num xmm home. Not yet consulted — the blanket index/call/bitwise decline
/// below still applies (regions with those take the memory path).
/// `ZIPP_NO_DOUBLE_BITWISE=1` restores the pre-B92 behaviour, where a single
/// `&`/`|`/`>>>`/`|0` demoted a whole region from the register-promoting tier to
/// the memory path. Measured cost of that demotion on an otherwise identical
/// 20M-iteration loop: **0.75ns/iter -> 4.15ns/iter**, against node unchanged at
/// 0.75 either way.
#[inline]
/// `ZIPP_NO_WT_SHARE=1` restores the pre-B97/B98 planner: `read_outside`
/// registers pin a permanent home again, and an `Add` operand no longer counts
/// as a numeric-required use of a read-only live-in. One switch for both because
/// neither promotes a region on its own — B97 clears the register-pressure
/// blocker that only appears once B98 clears the live-in one.
pub(crate) fn wt_share_enabled() -> bool {
    static ON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
    match ON.load(std::sync::atomic::Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_WT_SHARE").is_none();
            ON.store(v as u8, std::sync::atomic::Ordering::Relaxed);
            v
        }
    }
}

/// B94 receiver splitting on the INT tier — built, proven sound, and
/// DEFAULT OFF (`ZIPP_INT_SPLIT=1` enables; `ZIPP_NO_INT_SPLIT=1` forces off).
///
/// The mechanism: a recycled pinned receiver (`iv[i] = st` with `st` also the
/// xorshift temp) declined the whole i32 fill region with "pinned receiver reg
/// not cleanly excludable" — the B94 class the double tier already splits. The
/// INT emitter write-throughs the split register's defs BOXED (its memory slot
/// holds Values, not raw i64s; the store runs BEFORE any i53 guard, whose exit
/// resumes at ip+1 expecting the result flushed) and skips it in flush_exit.
///
/// Why it is off: the promotion it unlocks is a measured REGRESSION. On the
/// typedarray-math i32 fill phase (8M iterations, single-run phase attribution
/// 2026-08-03) the split INT region runs ~96ms against ~60ms for the MEM tier
/// it replaces — and disabling the write-through entirely (an unsound
/// measurement hack) still measured ~100ms, so the cost is the INT tier
/// itself, not the boxing: every Bitwise op round-trips xmm↔gpr (`movq` in,
/// op, `movq` out) on the xorshift's SERIAL dependency chain, six times per
/// iteration, where the MEM tier works the chain in gprs against the register
/// file. The wave-3 attribution of the fill's +43ms-vs-node to this decline is
/// therefore refuted: hosting the region on the INT tier as it exists today
/// makes the phase slower. The real blocker is gpr (not xmm) homes for
/// bitwise-chain regions, recorded for follow-up. Cached once per process.
///
/// W14 RE-CONFIRMED on a second, unrelated shape, and the follow-up is now
/// measured. The parse-large-js mix loop (three dense-Array receivers + a
/// pinned-STRING receiver, all four recycled, hand-inlined so the region
/// reaches the tier — 8x200k iterations of `h = imul(h ^ x, 16777619) >>> 0`):
///     MEM incumbent .................................... 35ms   (node 32ms)
///     ZIPP_INT_SPLIT=1, xmm homes ...................... 50ms   REGRESSION
///     INT-GPR homes (needs ZIPP_GPR_SPILL_SLOTS=1) ..... 13ms   2.7x
/// So the split itself was never the cost — the xmm HOMES are, exactly as this
/// note said. The GPR emitter wins the same region by 2.7x, and the only thing
/// keeping it off there is pool size: the region plans 11-12 homes against an
/// 8-gpr pool, and the W10.3 frame-slot spill that covers the gap is dark by
/// default because typedarray-math's fourteen-xmm-home swizzle refuted it.
/// Those two shapes want opposite answers, so the next step is a per-region
/// admission for the spill (split-receiver regions, whose homes are already
/// write-through-backed) rather than flipping `ZIPP_GPR_SPILL_SLOTS` globally.
/// W20 M1 — `ZIPP_NO_BOOL_REUSE=1` restores the pre-wave BOOL home allocator:
/// a 4-register first-fit that declines the whole region at the fifth distinct
/// bool temp. With the switch OFF (default ON) a region whose bool count would
/// OVERFLOW the pool linear-scan-reuses `BOOL_GPRS` over non-overlapping live
/// ranges, exactly as the numeric path already does above the 14-xmm pool.
///
/// SCOPE IS DELIBERATELY "ON OVERFLOW ONLY". Every region that fits four bools
/// today keeps the identical first-fit assignment, which matters twice over:
/// the INT-GPR emitter hands out whichever `BOOL_GPRS` the bools left free as
/// numeric i64 homes (`gpr_home_map`), so changing the bool assignment of a
/// region that already compiles would silently re-allocate its numeric homes;
/// and it keeps this mechanism a pure WIDENING — no region that compiles today
/// plans differently, so the only rows it can move are ones that decline today.
///
/// Sharing is gated on the same three predicates the numeric reuse path uses
/// (`live_in` / `read_outside` / `outside_dead`); see `bool_range` at the
/// allocation site for the argument, and `RegionPlan::live_in_bools` for why a
/// shared bool must NOT be entry-loaded.
pub(crate) fn bool_reuse_enabled() -> bool {
    static ON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
    match ON.load(std::sync::atomic::Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_BOOL_REUSE").is_none();
            ON.store(v as u8, std::sync::atomic::Ordering::Relaxed);
            v
        }
    }
}

/// W20 M2 — `ZIPP_NO_INT_PUSH=1` restores the pre-wave INT-tier admission: an
/// `arr.push(x)` `CallMethod` declines the whole region to the boxed memory
/// tier (`CallMethod (receiver not a pinned string/DataView)`), and the pin
/// planner stops pinning `push` receivers, so every plan in the suite is
/// bit-identical to the wave-19 binary.
///
/// WHAT THE WIDENING IS, AND WHAT RE-ESTABLISHES THE INVARIANT IT RELAXES.
/// `plan_region`'s CallMethod decline exists to keep the INT tier's no-call
/// contract, which is what lets `BOOL_GPRS` (r8..r11) and the pin snapshots
/// live across the whole loop body with nothing reloading them. Admitting
/// `push` puts ONE call in the body, so the contract is re-established
/// explicitly rather than assumed:
///
///   * VOLATILE HOMES. The arm saves every planner-owned register the win64
///     callee may scratch — the bool gprs actually allocated, and any numeric
///     home in xmm2..xmm5 — into a dedicated 64-byte call-save area on the
///     region's own frame, and restores them after. xmm6..xmm15 are callee
///     saved by the ABI (and by this region's own prologue), rbx/rsi/rdi/r12/
///     r13/r14 are non-volatile. `gpr_const` mirrors are re-materialised from
///     their immediates instead of saved.
///   * PIN SNAPSHOTS. `jit_array_push_pinned` runs NO user code, performs NO
///     VM-heap allocation and therefore cannot GC (it appends to the receiver
///     array's own `Vec` and touches the remembered set; there is no
///     `heap.alloc` on any path). So it cannot detach a buffer, resize an
///     ArrayBuffer, reassign a source global or free a pinned string — the
///     ONE thing it can invalidate is the pushed array's own `Vec` base and
///     length, and it rewrites that pin's snapshot slot itself before
///     returning. Every OTHER pin is provably untouched, PROVIDED no two arr
///     pins alias; the prologue proves that once, by comparing the arr pins'
///     snapshot `obj_bits` pairwise and taking `entry_bail` on a match.
///   * GUARD HOISTING. `hoistable_pins` refuses to hoist any pin that is a
///     push target (its base/len change in-region); the string/DataView pins
///     around it still hoist, since a `Vec` append cannot move a JS string.
pub(crate) fn int_push_enabled() -> bool {
    static ON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
    match ON.load(std::sync::atomic::Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_INT_PUSH").is_none();
            ON.store(v as u8, std::sync::atomic::Ordering::Relaxed);
            v
        }
    }
}

pub(crate) fn int_split_enabled() -> bool {
    static ON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
    match ON.load(std::sync::atomic::Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_INT_SPLIT").is_some()
                && std::env::var_os("ZIPP_NO_INT_SPLIT").is_none();
            ON.store(v as u8, std::sync::atomic::Ordering::Relaxed);
            v
        }
    }
}

/// W14: how many non-DataView B94 receiver splits one region may take. Four is
/// the parse-large-js mix loop (`kinds`, `ends`, `starts` element receivers plus
/// the `src` string receiver, all four recycled by the bytecode register
/// allocator). Each split costs one write-through per numeric def of that
/// register plus one memory store at its `LoadGlobal`; the cap keeps a
/// pathological region from paying that on a dozen registers rather than
/// expressing an emitter limit. `ZIPP_NO_MULTI_SPLIT=1` pins it back to 1.
pub(crate) const MULTI_SPLIT_BUDGET: usize = 4;

pub(crate) fn arr_pin_loose() -> bool {
    static ON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
    match ON.load(std::sync::atomic::Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_ARR_PIN_LOOSE").is_some();
            ON.store(v as u8, std::sync::atomic::Ordering::Relaxed);
            v
        }
    }
}

pub(crate) fn double_bitwise_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_DOUBLE_BITWISE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_DOUBLE_MOD=1` restores the pre-Mod emitter on the DOUBLE path: an
/// `Instr::Mod` falls through to the emitter's catch-all and the whole region
/// declines to the memory tier again (the named `regalloc-emit-unhandled: Mod`
/// decline B113 documented). Cached — read once per process, never on the
/// generated hot path.
pub(crate) fn double_mod_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_DOUBLE_MOD").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_DV_DOUBLE=1` restores the pre-DV planner on the DOUBLE path: a
/// whitelisted DataView `get*` CallMethod declines the whole region to the
/// memory tier again (helper call, boxed operands). Cached — read once per
/// process, never on the generated hot path.
pub(crate) fn dv_double_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_DV_DOUBLE").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_GUARD_HOIST=1` disables W7 pinned-guard hoisting: `hoist_pins` /
/// `hoist_len_ips` stay empty, so every pinned access re-emits its per-access
/// identity compare and the length GetProp stays a body op — the pre-wave
/// emission, byte-identical. Cached once per process, never on the hot path.
pub(crate) fn guard_hoist_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_GUARD_HOIST").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// W7: the pin slots whose per-access identity guard can be hoisted to region
/// entry. This is the WHOLE soundness argument, stated as a predicate — the
/// emitters consume the answer and add no reasoning of their own.
///
/// A pinned access is guarded today by (1) `source value == snapshot.obj_bits`
/// and (2) bounds against the snapshot `len`. The snapshot is taken FROM the
/// source in the same prologue, so (1) holds at entry trivially; it can only
/// go false at a LATER access if something between entry and that access
/// changes the source or the object. Hoisting is therefore sound exactly when
/// the region provably contains nothing that can:
///
///   * write the pin's source — a `StoreGlobal*` to its global slot, or any
///     reg-def of its frame register (checked per pin below, with the
///     emitter-grade `writes_reg` cover, NOT the pin builder's hint cover:
///     the builder's misses are safe there only BECAUSE of the per-access
///     re-check this predicate removes);
///   * run user code, allocate, or GC — the only ways a pinned object's
///     identity/base/length can change: detach is an explicit user call
///     (`transfer()`), resizable ArrayBuffers resize only via user calls,
///     dense-Array Vecs grow only via stores/calls, and this engine's GC
///     runs only at safepoints inside helpers (heap indices never move
///     regardless). Enforced by the closed WHITELIST scan: every region op
///     must be one the register tiers emit inline with no path into user
///     code. The list mirrors the union of the three register-tier emitters'
///     arms — the B115 DV window analysis discipline, region-wide. Any op
///     outside it (Call, unpinned CallMethod/GetProp/index ops, HasProp,
///     IterNext, StrConcat, MathOp other than Imul, …) refuses hoisting for
///     the WHOLE region. Cross-calls and the wave-4 `maybe_gc` safepoint live
///     on the MEM tier, whose ops (Call/helper CallMethod/SetProp) are all
///     outside the whitelist — such regions never hoist.
///
/// What remains per access is the semantic part only: bounds (the index
/// varies) and value-tag guards. The bounds LIMIT itself is loop-invariant
/// under this predicate (nothing can detach/resize/grow), which is what makes
/// `hoist_len_ips` sound for immutable strings.
///
/// A hoisted pin whose snapshot DECLINED ({0,0,0}: receiver no longer a live
/// view of the pinned kind) fails the entry check and `entry_bail`s — the
/// same contract as a failed live-in type guard, and strictly tighter than
/// the per-access compare it replaces (which a 0-bits source value could
/// alias; the bounds check `len == 0` covered element ops, but a pinned
/// `.length` read had no bounds to save it).
fn hoistable_pins(
    proto: &FuncProto,
    s: usize,
    e: usize,
    ta_plan: &TaPinPlan,
    cold: &FxHashSet<usize>,
) -> FxHashSet<u8> {
    let mut out: FxHashSet<u8> = FxHashSet::default();
    // Cold ips are emitted as side exits by a mode that is OFF (B9) — but the
    // predicate must not assume that stays true. A cold block's ops never ran
    // through this whitelist, so refuse outright.
    if !guard_hoist_enabled() || !cold.is_empty() || ta_plan.pins.is_empty() {
        return out;
    }
    let code = &proto.code;
    let captured_math = captured_math_sites(proto, s, e);
    let captured_math_get = |ip: usize| captured_math.iter().any(|site| site.get_ip == ip);
    let pin_kind_at = |ip: usize| -> Option<u8> {
        ta_plan
            .access
            .get(&ip)
            .map(|&j| ta_plan.pins[j as usize].kind)
    };
    for (off, instr) in code[s..=e].iter().enumerate() {
        let ip = s + off;
        let ok = match *instr {
            // Pure value/control ops the register tiers emit inline. None can
            // reach user code, allocate or GC.
            Instr::LoadInt { .. }
            | Instr::LoadConst { .. }
            | Instr::Move { .. }
            // B192: a completion-reg store of the canonical UNDEFINED bits —
            // pure, no user code, no allocation, no GC.
            | Instr::LoadUndefined { .. }
            | Instr::LoadGlobal { .. }
            | Instr::StoreGlobal { .. }
            | Instr::StoreGlobalStrict { .. }
            | Instr::StoreGlobalResolved { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Div { .. }
            | Instr::Mod { .. }
            | Instr::AddInt { .. }
            | Instr::Neg { .. }
            | Instr::Bitwise { .. }
            | Instr::Lt { .. }
            | Instr::Le { .. }
            | Instr::Gt { .. }
            | Instr::Ge { .. }
            | Instr::Eq { .. }
            | Instr::Ne { .. }
            | Instr::Jump { .. }
            | Instr::JumpIfFalse { .. }
            | Instr::JumpIfTrue { .. }
            | Instr::JumpIfNotLt { .. }
            | Instr::JumpIfNotLe { .. }
            | Instr::Return { .. }
            | Instr::ReturnUndefined
            | Instr::ToPropKey { .. } => true,
            // `Math.imul` is a native `imul` on every tier that admits it.
            Instr::MathOp {
                op: MathFn::Imul,
                argc: 2,
                ..
            } => true,
            // W20 M4: `!b` on a bool home is one `xor` -- no user code, no
            // allocation, nothing that can move a pin.
            Instr::Not { .. } if int_push_enabled() => true,
            // A PINNED element read is an inline load (TA raw element or a
            // tag-guarded dense-Array Value) — reads grow nothing.
            Instr::GetIndex { .. } => pin_kind_at(ip).is_some(),
            // A PINNED element write is admitted only for a real TypedArray
            // kind: the store lands in a fixed buffer and can never grow or
            // realloc it. A dense-Array store (which CAN grow the Vec) is not
            // a TA kind and refuses here — as it already declines the
            // register tiers.
            Instr::SetIndex { .. } => pin_kind_at(ip).is_some_and(|k| k < 9),
            // A pinned `.length` read (string units / dense-Array len).
            Instr::GetProp { .. } => {
                captured_math_get(ip)
                    || arr_push_pin(proto, ip, ta_plan).is_some()
                    || matches!(
                        pin_kind_at(ip),
                        Some(STR_PIN_KIND) | Some(DV_PIN_KIND) | Some(ARR_INT_PIN_KIND)
                    )
            }
            // A pinned `charCodeAt` (direct byte load) or DV `get*` (inline
            // guarded byte load) — no user code, no allocation, cannot
            // detach/resize. Any OTHER CallMethod can run arbitrary user code
            // (incl. `transfer()` / `resize()`) and refuses the whole region.
            // W20 M2: an admitted `arr.push(int)` does not refuse the region
            // either. It runs no user code and cannot GC, so it cannot detach,
            // resize, reassign a source or move a string's bytes -- the whole
            // list this predicate is defending. The ONE thing it does change is
            // the pushed array's own base and length, so that pin is dropped
            // from the hoist set below while the string/DataView pins around it
            // keep their entry-hoisted guard.
            Instr::CallMethod { .. } => {
                matches!(pin_kind_at(ip), Some(STR_PIN_KIND) | Some(DV_PIN_KIND))
                    || arr_push_pin(proto, ip, ta_plan).is_some()
            }
            Instr::CallWithThis { .. } => {
                arr_push_pin(proto, ip, ta_plan).is_some()
                    || (ta_plan.captured_call(ip).is_some()
                        && matches!(pin_kind_at(ip), Some(STR_PIN_KIND) | Some(DV_PIN_KIND)))
            }
            _ => false,
        };
        if !ok {
            return out;
        }
    }
    // W20 M2: pins the region PUSHES to. Their identity is stable (a push
    // cannot replace the object), but their snapshot base and length are
    // rewritten in-region, so hoisting -- whose whole licence is "the snapshot
    // cannot change" -- must not cover them.
    let pushed: FxHashSet<u8> = (s..=e)
        .filter_map(|ip| arr_push_pin(proto, ip, ta_plan).map(|j| j as u8))
        .collect();
    // Per-pin: the source must have no in-region write. `writes_reg` (the
    // emitter-grade cover: CallMethod/MathOp/GetProp/GetIndex dsts included)
    // is total over the whitelist above, so "no def found" is a proof here,
    // not a hint.
    for (j, pin) in ta_plan.pins.iter().enumerate() {
        if pushed.contains(&(j as u8)) {
            continue;
        }
        let has_access = ta_plan
            .access
            .iter()
            .any(|(&ip, &jj)| jj as usize == j && ip >= s && ip <= e);
        if !has_access {
            continue; // no in-region access — an entry guard would only cost
        }
        let stable = match pin.src {
            TaPinSrc::Global(g) => !code[s..=e].iter().any(|i| {
                matches!(*i,
                    Instr::StoreGlobal { idx, .. }
                    | Instr::StoreGlobalStrict { idx, .. }
                    | Instr::StoreGlobalResolved { idx, .. } if idx == g)
            }),
            TaPinSrc::Reg(r) => !code[s..=e].iter().any(|i| writes_reg(i) == Some(r)),
        };
        if stable {
            out.insert(j as u8);
        }
    }
    out
}

pub(crate) fn plan_region(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    admit_bitwise: bool,
    admit_split: bool,
    admit_wt_share: bool,
    // W20 BOXREF: may this plan admit boxed heap values (`box_regs`) and the
    // regalloc `GetProp` arm (`getprop_ips`)? Only `compile_region_regalloc`
    // passes `true`, and only when the caller can serve the arm — see
    // `BoxRefAdmit`. Every other planner caller reaches
    // `plan_region_cold`, which pins this to `BoxRefAdmit::NONE`, so their
    // plans are byte-identical to the pre-wave ones.
    boxref: BoxRefAdmit,
) -> Option<RegionPlan> {
    plan_region_cold_ex(
        proto,
        start,
        end,
        ta_plan,
        admit_bitwise,
        admit_split,
        admit_wt_share,
        false,
        &FxHashSet::default(),
        false,
        boxref,
    )
}

/// W20 — what a plan may admit onto the register tier beyond the numeric subset.
/// A plain `Copy` pair rather than two bools so a caller cannot silently swap
/// them, and so the "neither" case has one name (`NONE`) that reads as a
/// decline at every use site.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct BoxRefAdmit {
    /// Admit a dense-Array `GetIndex` over an ARRAY OF OBJECTS, typing its dst
    /// as a slot-resident `box_regs` member (BOXREF proper; `ZIPP_NO_BOX_HOME`).
    pub(crate) elems: bool,
    /// Admit a `GetProp` whose receiver is a READ-ONLY LIVE-IN register
    /// (`ZIPP_NO_REGALLOC_GETPROP`). Orthogonal to `elems`: a BOXREF receiver is
    /// admitted by `elems` alone.
    pub(crate) ro_recv: bool,
}

impl BoxRefAdmit {
    pub(crate) const NONE: BoxRefAdmit = BoxRefAdmit {
        elems: false,
        ro_recv: false,
    };
    pub(crate) fn any(&self) -> bool {
        self.elems || self.ro_recv
    }
}

/// `cold`: ips the caller will emit as SIDE EXITS (B9) rather than as native
/// code. They are skipped by every analysis pass here — they never execute
/// natively, so they neither define a home's value nor constrain its type, and
/// letting them into the walks would only make the planner decline a region
/// whose hot path is perfectly typed.
///
/// `share_homes`: force the linear-scan home-reuse allocation even when
/// one-home-per-value would fit the xmm pool (B119). The GPR emitter's pool is
/// far smaller than the 14 xmm homes, so `region_int` re-plans with this set
/// after a GPR pool overflow: `shareable` temps then collapse onto a few homes
/// and an ENCLOSING region of a loop nest (which carries the inner loop's
/// counters and temps too) can fit the GPR pool. Same soundness argument as
/// the >POOL case below — only `shareable` registers ever share.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_region_cold(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    admit_bitwise: bool,
    admit_split: bool,
    admit_wt_share: bool,
    share_homes: bool,
    cold: &FxHashSet<usize>,
    // W9: admit pinned-DV get* on the BITWISE (INT) path too — int-lane kinds
    // only, routed exclusively into the GPR emitter by `region_int`'s DV
    // retry. `false` keeps every existing caller's plan byte-identical (the
    // predicate below is unchanged when `!admit_bitwise`).
    admit_dv: bool,
) -> Option<RegionPlan> {
    // Every `[decline-reason]` line below — and every one an emitter prints
    // after this plan is handed to it — belongs to THIS region. See
    // `set_decline_region`.
    plan_region_cold_ex(
        proto,
        start,
        end,
        ta_plan,
        admit_bitwise,
        admit_split,
        admit_wt_share,
        share_homes,
        cold,
        admit_dv,
        BoxRefAdmit::NONE,
    )
}

/// `plan_region_cold` plus the W20 `boxref` admission. Split out so the six
/// `region_int` call sites keep their signature (and their `BoxRefAdmit::NONE`)
/// while the regalloc tier can ask for the boxed arms.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_region_cold_ex(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    admit_bitwise: bool,
    admit_split: bool,
    admit_wt_share: bool,
    share_homes: bool,
    cold: &FxHashSet<usize>,
    admit_dv: bool,
    boxref: BoxRefAdmit,
) -> Option<RegionPlan> {
    plan_region_cold_ex_with_home_last(
        proto,
        start,
        end,
        ta_plan,
        admit_bitwise,
        admit_split,
        admit_wt_share,
        share_homes,
        cold,
        admit_dv,
        boxref,
        HOME_XMM_LAST,
    )
}

/// Build a GPR-only plan with a bounded set of symbolic numeric-home colours.
/// Ordinary plans are capped at the fourteen physical XMM homes above.  This
/// entry point exists solely so the already flattened, call-free dense-computed
/// splice can reach the GPR mapper/spill allocator when its proven permanent
/// values exceed fourteen.  The returned plan must never reach an XMM emitter.
///
/// The cap is intentionally small: the current GPR pool plus the verified spill
/// budget can consume at most eighteen colours.  A broader request fails closed
/// here rather than manufacturing arbitrary `Home::Xmm` ids.
fn gpr_virtual_home_last(home_count: usize) -> Option<u8> {
    const PHYSICAL_HOME_COUNT: usize = (HOME_XMM_LAST - HOME_XMM_FIRST + 1) as usize;
    const MAX_VIRTUAL_HOME_COUNT: usize = 18;
    if home_count <= PHYSICAL_HOME_COUNT || home_count > MAX_VIRTUAL_HOME_COUNT {
        return None;
    }
    HOME_XMM_FIRST.checked_add(home_count as u8 - 1)
}

#[cfg(test)]
mod gpr_virtual_home_tests {
    use super::*;

    #[test]
    fn virtual_home_cap_fails_closed_for_physical_and_malformed_requests() {
        assert_eq!(gpr_virtual_home_last(14), None);
        assert_eq!(gpr_virtual_home_last(15), Some(HOME_XMM_LAST + 1));
        assert_eq!(gpr_virtual_home_last(18), Some(HOME_XMM_LAST + 4));
        assert_eq!(gpr_virtual_home_last(19), None);
        assert_eq!(gpr_virtual_home_last(usize::MAX), None);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_region_cold_gpr_virtual(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    admit_bitwise: bool,
    admit_split: bool,
    admit_wt_share: bool,
    share_homes: bool,
    cold: &FxHashSet<usize>,
    admit_dv: bool,
    home_count: usize,
) -> Option<RegionPlan> {
    if !share_homes {
        return None;
    }
    let last = gpr_virtual_home_last(home_count)?;
    debug_assert!(last > HOME_XMM_LAST && last <= HOME_XMM_FIRST + 63);
    plan_region_cold_ex_with_home_last(
        proto,
        start,
        end,
        ta_plan,
        admit_bitwise,
        admit_split,
        admit_wt_share,
        share_homes,
        cold,
        admit_dv,
        BoxRefAdmit::NONE,
        last,
    )
}

#[allow(clippy::too_many_arguments)]
fn plan_region_cold_ex_with_home_last(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    admit_bitwise: bool,
    admit_split: bool,
    admit_wt_share: bool,
    share_homes: bool,
    cold: &FxHashSet<usize>,
    admit_dv: bool,
    boxref: BoxRefAdmit,
    home_last: u8,
) -> Option<RegionPlan> {
    debug_assert!(home_last >= HOME_XMM_LAST);
    set_decline_region(proto, start, end);
    match plan_region_cold_inner(
        proto,
        start,
        end,
        ta_plan,
        admit_bitwise,
        admit_split,
        admit_wt_share,
        share_homes,
        cold,
        true,
        admit_dv,
        boxref,
        home_last,
    ) {
        PlanOutcome::Plan(p) => Some(*p),
        PlanOutcome::RetryNoHoist => {
            match plan_region_cold_inner(
                proto,
                start,
                end,
                ta_plan,
                admit_bitwise,
                admit_split,
                admit_wt_share,
                share_homes,
                cold,
                false,
                admit_dv,
                boxref,
                home_last,
            ) {
                PlanOutcome::Plan(p) => Some(*p),
                _ => None,
            }
        }
        PlanOutcome::Decline => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_region_cold_inner(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    admit_bitwise: bool,
    // Does the CALLER implement write-through (store each def to `[rbx + dreg(r)]`
    // and skip that register in its flush)? These two flags gate the two features
    // that depend on it: `admit_split` is B94 live-range splitting (a recycled
    // pinned receiver gets a numeric home while its memory slot stays the
    // receiver's), `admit_wt_share` is B97 home-sharing for `read_outside`
    // registers. The REGALLOC (double) path implements both and passes
    // (true, true). `region_int` shares this planner: it historically implemented
    // NEITHER — admitting B94 there panicked it ("no entry found for key"), and
    // admitting B97 there silently returned the WRONG ANSWER (a shareable register
    // loses its entry load, so its home starts as garbage and the int flush wrote
    // that garbage into the frame slot; caught by `hoisted_const_on_untaken_branch`).
    // The INT emitter now carries its own (BOXED) write-through for splits — the
    // B94 hazard class the typedarray i32 fill declined on — so it passes
    // admit_split behind `ZIPP_NO_INT_SPLIT`, but still admit_wt_share=false:
    // B97's shareable/no-entry-load allocation stays double-only.
    admit_split: bool,
    admit_wt_share: bool,
    // Force home reuse below the xmm-pool overflow point — see
    // `plan_region_cold`'s doc (B119: refitting a region to the GPR pool).
    share_homes: bool,
    cold: &FxHashSet<usize>,
    // See `PlanOutcome::RetryNoHoist`: `false` on the retry pass — no constant
    // is hoisted, so none pins a permanent home. ONE exemption: the glob-range
    // const REMATERIALIZATION below is deliberately NOT conditioned on this
    // flag, because in the segmented allocation a rematerialized const pins no
    // home of its own (every hoisted constant shares one deliberately-unmapped
    // index), so the pressure-release would have nothing to reclaim from it.
    // That is why the segmented pool-overflow arm asks for the retry only when
    // something this flag ACTUALLY gates was hoisted — a classic
    // runs-every-iteration const or a pinned-string length; with remat consts
    // alone the retry would re-plan bit-identically and decline anyway.
    allow_hoist: bool,
    // W9 — see `plan_region_cold`.
    admit_dv: bool,
    // W20 — see `BoxRefAdmit`. `BoxRefAdmit::NONE` for every caller but the
    // regalloc tier, and the passes below are no-ops under it.
    boxref: BoxRefAdmit,
    // Inclusive numeric-home colour ceiling. `HOME_XMM_LAST` for every normal
    // plan; a small virtual value only for the computed-splice GPR retry.
    home_last: u8,
) -> PlanOutcome {
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
    let captured_math = captured_math_sites(proto, s, e);
    let math_get = |ip: usize| captured_math.iter().any(|site| site.get_ip == ip);
    let captured_pin_get = |ip: usize| ta_plan.captured_get(ip);
    let captured_pin_call = |ip: usize| ta_plan.captured_call(ip);
    let discarded_arr_push_dst = |ip: usize| -> Option<u16> {
        if !admit_bitwise || arr_push_pin(proto, ip, ta_plan).is_none() {
            return None;
        }
        let dst = match code[ip] {
            Instr::CallMethod { dst, .. } | Instr::CallWithThis { dst, .. } => dst,
            _ => return None,
        };
        (!code
            .iter()
            .any(|ins| instr_uses(ins).into_iter().any(|r| r == dst)))
        .then_some(dst)
    };
    // Captured member bytecodes carry boxed reference components in their
    // generic use lists.  Those components are guarded/materialised in frame
    // slots, never numeric homes; only the argument window participates in
    // register-tier typing and liveness.  Keep `instr_uses` exhaustive for all
    // other analyses and filter this exact paired shape locally.
    let numeric_uses = |ip: usize, instr: &Instr| -> Vec<u16> {
        let pin_get = captured_pin_get(ip);
        let pin_call = captured_pin_call(ip);
        let math_site = captured_math
            .iter()
            .find(|site| site.get_ip == ip || site.call_ip == ip);
        instr_uses(instr)
            .into_iter()
            .filter(|&r| {
                !pin_get.is_some_and(|site| r == site.obj)
                    && !pin_call.is_some_and(|site| r == site.obj || r == site.callee)
                    && !math_site.is_some_and(|site| {
                        (ip == site.get_ip && r == site.receiver)
                            || (ip == site.call_ip && (r == site.receiver || r == site.callee))
                    })
            })
            .collect()
    };
    let numeric_def = |ip: usize, instr: &Instr| -> Option<u16> {
        if captured_pin_get(ip).is_some() || math_get(ip) || discarded_arr_push_dst(ip).is_some() {
            None
        } else {
            writes_reg(instr)
        }
    };
    // ── unboxed-region epic: pinned TypedArray element access ──
    // A pinned GetIndex/SetIndex whose element kind matches the REGISTER PATH can run
    // unboxed: the double/regalloc path (admit_bitwise=false) hosts a kind-8 Float64
    // element as an f64 xmm home; the integer path (admit_bitwise=true) hosts a kind-5
    // Int32 element as a sign-extended i64 home. The two are mutually exclusive (an f64
    // can't be an i64 home, and vice-versa), so a region mixing kinds declines the
    // non-matching access to the memory path. The receiver `obj` (a heap TA) and index
    // `key` are NOT homed — the emitter reads the receiver via the pin's source and the
    // index via `key`'s home. Identify the admissible ops + their receiver regs so the
    // typing/homing passes below bypass the receivers.
    // -- W20 BOXREF -- the ONE loop shape the register tier could not host:
    //   `o = arr[i]; ... o.p ...`
    // Both halves are already emitted call-free somewhere in this JIT (the
    // dense-Array element load on this very tier, the 8-way `GetProp` probe on
    // the memory tier); what was missing is a way for a HEAP value to exist
    // between them without a numeric home. `box_regs` is that way, and it is
    // deliberately NOT a new `Home` variant: the value stays in the interpreter
    // frame slot `[rbx + dreg(r)]`, written at its def and never flushed, so
    // every exit is correct without knowing which path reached it (the
    // `split_recvs` / `emit_recv_slot_store` invariant) and no callee-saved
    // register is spent on it.
    //
    // A register qualifies only under a CLOSED def/use shape, checked here:
    //   * defs -- exactly one, an `ARR_PIN_KIND` (sampled: NOT all-numeric, i.e.
    //     an array of objects) dense-Array `GetIndex`. `ARR_INT_PIN_KIND` /
    //     `ARR_NUM_PIN_KIND` keep their f64 homes: they are FASTER, and B102's
    //     11x entry-bail is precisely what happens when a numeric home is handed
    //     an object, which is why the three kinds exist at all;
    //   * uses -- every one is the receiver of an admitted `GetProp`.
    // A live-in receiver with ZERO defs qualifies the same way under
    // `boxref.ro_recv` (the standalone `o.p` arm), because a register the region
    // never writes has an authoritative slot by construction.
    //
    // Anything outside that shape leaves both sets empty and the region declines
    // exactly as it did before the wave.
    let mut box_regs: FxHashSet<u16> = FxHashSet::default();
    let mut getprop_ips: FxHashSet<usize> = FxHashSet::default();
    let mut boxref_gets: FxHashSet<usize> = FxHashSet::default();
    if boxref.any() && !admit_bitwise {
        // Candidate receivers: every `GetProp` in the region not already served
        // by a pin (a `.length` read rides `pinned_len`/`pinned_str` and must
        // keep doing so -- its dst is the snapshot length, not a probe result).
        let mut cand_recv: FxHashSet<u16> = FxHashSet::default();
        let mut gp_of: Vec<(usize, u16)> = Vec::new();
        for (off, instr) in code[s..=e].iter().enumerate() {
            let ip = s + off;
            if cold.contains(&ip) {
                continue;
            }
            if let Instr::GetProp { obj, .. } = *instr {
                if ta_plan.access.contains_key(&ip) || math_get(ip) {
                    continue; // pinned `.length` -- not ours
                }
                cand_recv.insert(obj);
                gp_of.push((ip, obj));
            }
        }
        if !cand_recv.is_empty() {
            // One pass for def counts and for every use that is NOT a candidate
            // receiver position. A receiver appearing anywhere else -- an
            // arithmetic operand, a `Move` source, an element key, a `SetProp`
            // receiver -- is disqualified: the emitter would need a home for it,
            // and the whole point is that it has none.
            let mut def_n: FxHashMap<u16, u32> = FxHashMap::default();
            let mut def_boxable: FxHashSet<u16> = FxHashSet::default();
            let mut def_glob: FxHashSet<u16> = FxHashSet::default();
            let mut used_elsewhere: FxHashSet<u16> = FxHashSet::default();
            for (off, instr) in code[s..=e].iter().enumerate() {
                let ip = s + off;
                if cold.contains(&ip) {
                    continue;
                }
                if let Some(d) = writes_reg(instr) {
                    *def_n.entry(d).or_insert(0) += 1;
                    // The two def forms that can fill a box slot.
                    if matches!(*instr, Instr::GetIndex { .. })
                        && ta_plan
                            .access
                            .get(&ip)
                            .is_some_and(|&j| ta_plan.pins[j as usize].kind == ARR_PIN_KIND)
                    {
                        def_boxable.insert(d);
                    }
                    if let Instr::LoadGlobal { idx, .. } = *instr {
                        // The global must have NO in-region store: its slot gets
                        // no xmm home (an object cannot live in one), so the
                        // emitter's `StoreGlobal` arm would have no home to write
                        // — and the receiver store reads `[r12 + 8*idx]`, which
                        // must therefore BE the live value.
                        let stored = code[s..=e].iter().any(|i| {
                            matches!(*i,
                                Instr::StoreGlobal { idx: g, .. }
                                | Instr::StoreGlobalStrict { idx: g, .. }
                                | Instr::StoreGlobalResolved { idx: g, .. } if g == idx)
                        });
                        if !stored {
                            def_glob.insert(d);
                        }
                    }
                }
                let recv_here = match *instr {
                    Instr::GetProp { obj, .. } if !ta_plan.access.contains_key(&ip) => Some(obj),
                    _ => None,
                };
                for u in numeric_uses(s + off, instr) {
                    if Some(u) != recv_here && cand_recv.contains(&u) {
                        used_elsewhere.insert(u);
                    }
                }
            }
            for &r in &cand_recv {
                if used_elsewhere.contains(&r) {
                    continue;
                }
                let n = def_n.get(&r).copied().unwrap_or(0);
                // (a) BOXREF proper: one `ARR_PIN_KIND` element read fills it.
                // (b) the standalone arm: a live-in the region never writes, or a
                //     single `LoadGlobal` of a slot it never stores -- the two
                //     forms `ta_recv_regs` already calls `clean_param` and
                //     `clean_global`, for exactly the same reason.
                let ok = (boxref.elems && n == 1 && def_boxable.contains(&r))
                    || (boxref.ro_recv && (n == 0 || (n == 1 && def_glob.contains(&r))));
                if ok {
                    box_regs.insert(r);
                }
            }
            for (ip, obj) in gp_of {
                if box_regs.contains(&obj) {
                    getprop_ips.insert(ip);
                }
            }
            if !getprop_ips.is_empty() {
                for (off, instr) in code[s..=e].iter().enumerate() {
                    let ip = s + off;
                    if cold.contains(&ip) {
                        continue;
                    }
                    if let Instr::GetIndex { dst, .. } = *instr {
                        if box_regs.contains(&dst) {
                            boxref_gets.insert(ip);
                        }
                    }
                }
            } else {
                box_regs.clear();
            }
        }
    }
    let pinned_elem = |ip: usize| -> bool {
        ta_plan.access.get(&ip).map_or(false, |&j| {
            let k = ta_plan.pins[j as usize].kind;
            // A dense all-Int ARRAY read joins the int path on the same terms as a
            // kind-5 TypedArray: receiver via the pin, index via `key`'s home, the
            // element unboxed into an i64 home under a per-access tag guard. This is
            // what admits `for (i < a.length) s += a[i]` — the most common hot loop
            // in JS, and previously demoted to the boxed memory path in full.
            match code[ip] {
                Instr::GetIndex { .. } => {
                    if (!admit_bitwise && k == 8)
                        || (admit_bitwise
                            && (int_ta_load_kind(k).is_some() || k == ARR_INT_PIN_KIND))
                    {
                        return true;
                    }
                }
                Instr::SetIndex { .. } => {
                    // This widening is deliberately read-only. Direct narrow
                    // stores need per-dtype wrap/clamp emission; only the landed
                    // Int32 and Float64 store arms qualify here.
                    if (!admit_bitwise && k == 8) || (admit_bitwise && k == 5) {
                        return true;
                    }
                }
                _ => {}
            }
            // B95: the DOUBLE path admits a dense ordinary Array READ on the same
            // terms — receiver via the pin, index via `key`'s home — but the
            // element is a NaN-boxed `Value`, not a raw f64, so it needs the
            // per-access tag guard (`emit_box_to_home`) instead of kind-8's bare
            // `movsd`. A double or an Int lands in the f64 home; a HOLE, a bool,
            // null/undefined or a heap value deopts AT this ip. That guard is what
            // makes the plan-time all-numeric sample a hint and not a soundness
            // gate, exactly as it is on the int path.
            //
            // WRITES are deliberately NOT admitted: storing an f64 home back as a
            // `Value` must reproduce `Value::num`'s exact-int narrowing (and -0 /
            // NaN handling) bit-for-bit, which is separate work. A `SetIndex` on an
            // Array pin still declines the region to the memory path.
            //
            // Staleness cannot bite here the way it can on the memory path: the
            // snapshot's `base` goes stale on any Vec growth, and a regalloc region
            // contains no Call, no CallMethod other than a pinned-DV get* (inline
            // machine code — no user code, no allocation, cannot detach/resize a
            // buffer or grow a Vec) and (by the line above) no SetIndex, so
            // nothing in it can grow the array or trigger a GC.
            //
            // B102 FIX: `ARR_INT_PIN_KIND`, not `is_arr_pin`. The latter also
            // matches `ARR_PIN_KIND`, which is ANY dense array — including an
            // array of OBJECTS. Its dst then gets a numeric home, `live_in_regs`
            // entry-loads that home from the previous iteration's element, the
            // guard sees a heap value and `entry_bail`s — on EVERY OSR entry. The
            // region self-evicts, displaces the memory compile that was working,
            // and the loop ends up fully interpreted: measured **204ms -> 2349ms,
            // an 11x regression**, on `for (…) { o = objs[i & 63]; s += 1.5; }`.
            // ARR_INT_PIN_KIND carries the plan-time all-Int SAMPLE, which is the
            // same hint the int path relies on to avoid exactly this thrash.
            // The double tier takes the two SAMPLED-numeric kinds only. Not
            // `is_arr_pin`, which also matches `ARR_PIN_KIND` — any dense array,
            // including one of OBJECTS; see `ARR_NUM_PIN_KIND` for the 11x
            // entry-bail that caused. `ZIPP_ARR_PIN_LOOSE=1` restores the
            // unsampled B95 behaviour for A/B.
            // W20 BOXREF: an ARR_PIN_KIND (array-of-objects) READ joins on the
            // same terms once its dst is a `box_regs` member -- the element is
            // stored to the register's frame slot as raw Value bits rather than
            // unboxed into an f64 home, so B102's entry-bail (a numeric home
            // entry-loaded from an object) cannot arise: the register has no
            // home to entry-load.
            if !admit_bitwise && boxref_gets.contains(&ip) {
                return true;
            }
            !admit_bitwise
                && (k == ARR_INT_PIN_KIND
                    || k == ARR_NUM_PIN_KIND
                    || (arr_pin_loose() && is_arr_pin(k)))
                && matches!(code[ip], Instr::GetIndex { .. })
        })
    };
    // A pinned flat-ASCII STRING access (kind 254): `str.charCodeAt(i)` (CallMethod)
    // and `str.length` (GetProp), both on the int path only (admit_bitwise). The
    // receiver is read via the pin snapshot, never the register, so its reg is
    // excluded from typing/homing exactly like a pinned-element receiver.
    let pinned_str = |ip: usize| -> bool {
        admit_bitwise
            && ta_plan
                .access
                .get(&ip)
                .map_or(false, |&j| ta_plan.pins[j as usize].kind == STR_PIN_KIND)
    };
    // `arr.length` on an ordinary dense Array pin, or pristine `ta.length` on a
    // distinct TypedArray length-only marker. Like `str.length`, the receiver
    // is read from the pin snapshot rather than a register, so its reg must be
    // excluded from typing/homing too. The marker does not admit TA elements.
    let pinned_len = |ip: usize| -> bool {
        admit_bitwise
            && ta_plan.access.get(&ip).map_or(false, |&j| {
                int_length_pin_kind(ta_plan.pins[j as usize].kind)
            })
    };
    // A pinned DataView `get*` CallMethod (kind DV_PIN_KIND), DOUBLE path only.
    // The INT tier keeps declining these (B22/B32): a getUint32 result ranges
    // through 2^32-1 (feeding `>>>`/`&` past i32) and the float kinds cannot
    // inhabit an i64 home at all — while an f64 home holds every whitelisted
    // kind EXACTLY (a u32 <= 2^32-1 is exact in a double; getFloat32/64 are
    // native). The receiver is read via the pin snapshot, never the register,
    // so its reg is excluded from typing/homing exactly like a pinned-element
    // receiver. `ZIPP_NO_DV_DOUBLE=1` restores the decline for A/B.
    let pinned_dv = |ip: usize| -> bool {
        // W9: the INT path (admit_bitwise) admits DV get* too, INT-LANE KINDS
        // ONLY (<= 6 — a float result cannot inhabit an i64 home; a u32 can,
        // the ±2^53 discipline boxes it exactly). Routed exclusively into the
        // GPR emitter by `region_int`'s DV retry (`admit_dv` is false on every
        // other caller, keeping their plans byte-identical).
        ((!admit_bitwise && dv_double_enabled()) || (admit_bitwise && admit_dv))
            && ta_plan
                .access
                .get(&ip)
                .map_or(false, |&j| ta_plan.pins[j as usize].kind == DV_PIN_KIND)
            && {
                let sig = match code[ip] {
                    Instr::CallMethod { name, argc, .. } => Some((name, argc)),
                    Instr::CallWithThis { .. } => {
                        captured_pin_call(ip).map(|site| (site.name, site.argc))
                    }
                    Instr::GetProp { .. } => {
                        captured_pin_get(ip).map(|site| (site.name, site.argc))
                    }
                    _ => None,
                };
                sig.is_some_and(|(name, argc)| {
                    (argc == 1 || argc == 2)
                        && proto.string_constants.get(name as usize).is_some_and(|k| {
                            dv_get_kind(k).is_some_and(|kid| !admit_bitwise || kid <= 6)
                        })
                })
            }
    };
    // W20 M2: an INT-tier-admissible `arr.push(int)` -- a `CallMethod` on a
    // receiver pinned as a dense all-Int Array. Admitted on the INT path only
    // (`admit_bitwise`), and only behind the mechanism's own latch, so every
    // other tier's plan is unchanged.
    let pinned_arr_push =
        |ip: usize| -> bool { admit_bitwise && arr_push_pin(proto, ip, ta_plan).is_some() };
    // Capture-first lowering deliberately recycles call temporaries across the
    // exact push prefix (a prior numeric arg/result can become the next boxed
    // receiver).  Those receiver registers require the already-proven
    // split/write-through representation even when the broad experimental INT
    // split switch is off.  Keep the exception local to receivers of the exact
    // arr_push_pin proof; every other recycled receiver still obeys
    // `admit_split`.
    // The same compiler recycling occurs for the already-admitted captured
    // string/DataView prefixes in this region.  This is still the exact
    // TaPinPlan pairing (not general CallWithThis), and lets the shared
    // split proof decide safety instead of rejecting solely because the broad
    // INT split experiment is disabled.
    let captured_pinned_recv: FxHashSet<u16> = ta_plan
        .captured_calls
        .values()
        .filter(|site| (s..=e).contains(&site.get_ip) && (s..=e).contains(&site.call_ip))
        .map(|site| site.obj)
        .collect();
    let mut ta_recv_regs: FxHashSet<u16> = FxHashSet::default();
    // B94 recycled receivers (see `plan::RegionPlan::split_recvs`). A receiver
    // whose pinned accesses are all DV `get*` CallMethods is exempt from the
    // budget below — each split is proven and written through independently.
    //
    // W14: the non-DV budget was ONE. Nothing in the emitters is per-region
    // about a split: `split_recvs`/`write_through` are register SETS, the
    // write-through hook fires on every def of any of them, and `flush_exit`
    // skips all of them — so the budget was an untested-shape guard, not a
    // capability limit. It is now `MULTI_SPLIT_BUDGET`, sized to the four
    // recycled receivers of the parse-large-js mix loop (`kinds`, `ends`,
    // `starts`, `src`). `ZIPP_NO_MULTI_SPLIT=1` puts it back to one.
    let mut split_recvs: FxHashSet<u16> = FxHashSet::default();
    let non_dv_split_budget = if crate::codegen::multi_split_enabled() {
        MULTI_SPLIT_BUDGET
    } else {
        1
    };
    let mut non_dv_splits = 0usize;
    let mut write_through: FxHashSet<u16> = FxHashSet::default();
    let mut split_recv_lg: FxHashSet<usize> = FxHashSet::default();
    let mut recv_loads: FxHashSet<usize> = FxHashSet::default();
    let mut split_all_dv;
    {
        // Candidate receiver regs: the `obj` of every pinned index op, and the
        // `obj` of every pinned-STRING charCodeAt/length access.
        let mut recv: FxHashSet<u16> = FxHashSet::default();
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) {
                continue;
            }
            if pinned_elem(s + off) {
                if let Instr::GetIndex { obj, .. } | Instr::SetIndex { obj, .. } = *instr {
                    recv.insert(obj);
                }
            }
            if pinned_str(s + off)
                || pinned_len(s + off)
                || pinned_dv(s + off)
                || pinned_arr_push(s + off)
            {
                let obj = match *instr {
                    Instr::CallMethod { obj, .. } | Instr::GetProp { obj, .. } => Some(obj),
                    Instr::CallWithThis { this_v, .. } => Some(this_v),
                    _ => None,
                };
                if let Some(obj) = obj {
                    recv.insert(obj);
                }
            }
        }
        if !recv.is_empty() {
            // Each receiver reg must be defined by EXACTLY ONE LoadGlobal and used
            // ONLY as a pinned-index `obj` (else it would need a real home — decline
            // the whole region to the memory path, which handles it).
            let mut def_n: FxHashMap<u16, u32> = FxHashMap::default();
            let mut def_lg: FxHashSet<u16> = FxHashSet::default();
            for (off, instr) in code[s..=e].iter().enumerate() {
                if cold.contains(&(s + off)) {
                    continue;
                }
                if let Some(d) = writes_reg(instr) {
                    *def_n.entry(d).or_insert(0) += 1;
                    if matches!(instr, Instr::LoadGlobal { .. }) {
                        def_lg.insert(d);
                    }
                }
            }
            let mut used_elsewhere: FxHashSet<u16> = FxHashSet::default();
            for (off, instr) in code[s..=e].iter().enumerate() {
                if cold.contains(&(s + off)) {
                    continue;
                }
                // The receiver use AT a pinned access is exempt (read via the pin,
                // not the register). Both halves are load-bearing: `instr_uses`
                // declares the receiver of a `CallMethod` and of a `GetProp`, so
                // without the exemption a dual-use string receiver looks
                // used-elsewhere and the whole region declines. (This comment
                // used to say the `CallMethod` half was forward-defensive
                // because `instr_uses` reported that op as reading nothing —
                // that stopped being true when `CallMethod` got an arm.)
                // Match on the INSTRUCTION first, then the predicate. The
                // predicates are keyed by ip alone, so on a receiver that carries
                // BOTH an element access and a `.length` read (`for (i < a.length)
                // s += a[i]`) `pinned_elem` is also true at the GetProp ip — an
                // `if pinned_elem { GetIndex|SetIndex => .. , _ => None }` chain
                // then swallowed the GetProp and exempted nothing, so the receiver
                // looked used-elsewhere and the whole region declined.
                let idx_obj = match *instr {
                    Instr::GetIndex { obj, .. } | Instr::SetIndex { obj, .. }
                        if pinned_elem(s + off) =>
                    {
                        Some(obj)
                    }
                    Instr::CallMethod { obj, .. } | Instr::GetProp { obj, .. }
                        if pinned_str(s + off)
                            || pinned_len(s + off)
                            || pinned_dv(s + off)
                            || pinned_arr_push(s + off) =>
                    {
                        Some(obj)
                    }
                    Instr::CallWithThis { this_v, .. }
                        if pinned_str(s + off)
                            || pinned_dv(s + off)
                            || pinned_arr_push(s + off) =>
                    {
                        Some(this_v)
                    }
                    // ToPropKey's receiver use is only the nullish check, which
                    // the pin subsumes: a compiled region proves the receiver
                    // was a live TypedArray at plan time, nothing in a numeric
                    // region can write the slot non-numerically (LoadConst
                    // admits no null/undefined and no calls run), and every
                    // pinned access re-checks identity anyway. Without this
                    // exemption the ToPropKey-site use marked the receiver
                    // used_elsewhere and traded the ro_live_in decline for
                    // "pinned receiver reg not cleanly excludable".
                    Instr::ToPropKey { obj, .. } => Some(obj),
                    _ => None,
                };
                for u in numeric_uses(s + off, instr) {
                    if Some(u) != idx_obj && recv.contains(&u) {
                        used_elsewhere.insert(u);
                    }
                }
            }
            for &r in &recv {
                // A captured member-call receiver is exempt from the split
                // budget — but only while the multi-split package is on:
                // `ZIPP_NO_MULTI_SPLIT=1` must still send a two-receiver
                // kernel to the memory tier, or the latch measures nothing.
                let captured_prefix_split =
                    captured_pinned_recv.contains(&r) && crate::codegen::multi_split_enabled();
                // Cleanly excludable when EITHER: (a) defined by exactly one
                // LoadGlobal and used only as a pinned obj (the global-receiver
                // case); OR (b) a live-in PARAM receiver — ZERO in-region defs and
                // used only as a pinned obj (`str` in fnv1a is reg1, the param,
                // read only via the STR pin). `def_n.get(&r).is_none()` (NOT
                // `!= Some(&1)`) is load-bearing: a multiply-defined reg must NOT
                // be admitted (it would need a real home).
                let clean_global = def_n.get(&r) == Some(&1)
                    && def_lg.contains(&r)
                    && !used_elsewhere.contains(&r);
                let clean_param = def_n.get(&r).is_none() && !used_elsewhere.contains(&r);
                if clean_global || clean_param {
                    ta_recv_regs.insert(r);
                } else if (admit_split || captured_prefix_split)
                    && def_lg.contains(&r)
                    && {
                        // Every pinned access with THIS receiver must read its
                        // identity from a global slot. The compiler may reuse one
                        // temporary for several distinct receiver globals; each
                        // access therefore re-proves its own nearest dominating
                        // LoadGlobal rather than conflating the sources.
                        // A `TaPinSrc::Reg` pin reads `[rbx + dreg(r)]`, which the
                        // numeric half of a recycled register also owns; that case
                        // is not separable and declines below.
                        recv_loads.clear();
                        let mut ok = true;
                        let mut all_dv = true;
                        for (off, i) in code[s..=e].iter().enumerate() {
                            if cold.contains(&(s + off)) {
                                continue;
                            }
                            // A pinned-DV CallMethod's receiver splits on the same
                            // terms as a pinned element access: the emitted code
                            // reads identity from the pin's GLOBAL, never the reg.
                            let pin_obj = match *i {
                                Instr::GetIndex { obj, .. } | Instr::SetIndex { obj, .. }
                                    if pinned_elem(s + off) =>
                                {
                                    Some((obj, false))
                                }
                                Instr::CallMethod { obj, .. } if pinned_dv(s + off) => {
                                    Some((obj, true))
                                }
                                Instr::GetProp { obj, .. }
                                | Instr::CallWithThis { this_v: obj, .. }
                                    if pinned_dv(s + off) =>
                                {
                                    Some((obj, true))
                                }
                                // W14: a pinned flat-ASCII STRING receiver
                                // (`src.charCodeAt(i)` / `src.length`) and a dense
                                // all-Int Array `.length` receiver split on exactly
                                // the same terms — both emitters read identity from
                                // the pin's GLOBAL, never the register (see the
                                // charCodeAt and pinned-length arms). `recv_use_at`
                                // 40 lines below has always listed them; this match
                                // did not, so a recycled string receiver could never
                                // reach the split and the whole region declined.
                                Instr::CallMethod { obj, .. } | Instr::GetProp { obj, .. }
                                    if crate::codegen::multi_split_enabled()
                                        && (pinned_str(s + off)
                                            || pinned_len(s + off)
                                            || pinned_arr_push(s + off)) =>
                                {
                                    Some((obj, false))
                                }
                                Instr::CallWithThis { this_v, .. }
                                    if crate::codegen::multi_split_enabled()
                                        && (pinned_str(s + off) || pinned_arr_push(s + off)) =>
                                {
                                    Some((this_v, false))
                                }
                                _ => None,
                            };
                            if let Some((obj, is_dv)) = pin_obj {
                                if obj != r {
                                    continue;
                                }
                                all_dv &= is_dv;
                                match ta_plan
                                    .access
                                    .get(&(s + off))
                                    .map(|&j| ta_plan.pins[j as usize].src)
                                {
                                    Some(TaPinSrc::Global(g)) => {
                                        let access_ip = s + off;
                                        let load_ip = (s..access_ip)
                                            .rev()
                                            .find(|&ip| writes_reg(&code[ip]) == Some(r));
                                        let Some(load_ip) = load_ip.filter(|&ip| {
                                        matches!(code[ip], Instr::LoadGlobal { dst, idx } if dst == r && idx == g)
                                    }) else {
                                        ok = false;
                                        continue;
                                    };
                                        let entered = code.iter().any(|ins| {
                                            let target = match *ins {
                                                Instr::Jump { target }
                                                | Instr::JumpIfFalse { target, .. }
                                                | Instr::JumpIfTrue { target, .. }
                                                | Instr::JumpIfNotLt { target, .. }
                                                | Instr::JumpIfNotLe { target, .. } => Some(target),
                                                _ => None,
                                            };
                                            target.is_some_and(|target| {
                                                (load_ip + 1..=access_ip)
                                                    .contains(&(target as usize))
                                            })
                                        });
                                        if entered {
                                            ok = false;
                                        } else {
                                            recv_loads.insert(load_ip);
                                        }
                                    }
                                    _ => ok = false,
                                }
                            }
                        }
                        split_all_dv = all_dv;
                        // The budget applies to ELEMENT/STRING-pinned receivers
                        // only (B94's exercised case); DV-pinned receivers split
                        // independently — the DV swizzle loop recycles two of them,
                        // and a one-split rule declined it.
                        ok && !recv_loads.is_empty()
                            && (all_dv
                                || captured_prefix_split
                                || non_dv_splits < non_dv_split_budget)
                    }
                {
                    // ── B94 live-range splitting ── the bytecode compiler RECYCLED
                    // this register: pinned receiver over one range, arithmetic temp
                    // over another (in `p_ta2` r17 is the array at ip37, the running
                    // sum at ip45 and the loop counter at ip49). The ranges are
                    // disjoint, so give it a numeric home and keep its memory slot
                    // authoritative for the receiver half: the receiver `LoadGlobal`
                    // is emitted as a real store, every numeric def writes through,
                    // and `flush_exit` skips it. What must still be PROVED is that no
                    // use reads the home before a numeric def fills it — the home is
                    // deliberately not entry-loaded, so it starts as garbage.
                    let recv_lg_ips = recv_loads.clone();
                    let recv_use_at = |ip: usize| -> Option<u16> {
                        match code[ip] {
                            Instr::GetIndex { obj, .. } | Instr::SetIndex { obj, .. }
                                if pinned_elem(ip) =>
                            {
                                Some(obj)
                            }
                            Instr::CallMethod { obj, .. } | Instr::GetProp { obj, .. }
                                if pinned_str(ip)
                                    || pinned_len(ip)
                                    || pinned_dv(ip)
                                    || pinned_arr_push(ip) =>
                            {
                                Some(obj)
                            }
                            Instr::CallWithThis { this_v, .. }
                                if pinned_str(ip) || pinned_dv(ip) || pinned_arr_push(ip) =>
                            {
                                Some(this_v)
                            }
                            Instr::ToPropKey { obj, .. } => Some(obj),
                            _ => None,
                        }
                    };
                    if !recv_lg_ips.is_empty()
                        && crate::codegen::plan::split_home_provably_safe(
                            code,
                            s,
                            e,
                            r,
                            cold,
                            &recv_lg_ips,
                            &recv_use_at,
                        )
                    {
                        split_recvs.insert(r);
                        split_recv_lg.extend(recv_lg_ips);
                        if !split_all_dv && !captured_prefix_split {
                            non_dv_splits += 1;
                        }
                    } else {
                        decline!("split receiver: home not provably live at a use");
                    }
                } else {
                    // A receiver register reused for other (numeric) values can't be
                    // cleanly excluded under the non-SSA register model → memory path.
                    // (The split above takes up to `MULTI_SPLIT_BUDGET` element/string
                    // receivers; past that, or when a pin's identity comes from a
                    // register rather than a global slot, this is the fallback.
                    // DV-pinned splits are exempt from the budget.)
                    decline!(format_args!(
                        "pinned receiver r{r} not cleanly excludable (defs={:?}, load-global={}, used-elsewhere={})",
                        def_n.get(&r),
                        def_lg.contains(&r),
                        used_elsewhere.contains(&r)
                    ));
                }
            }
        }
    }
    // Captured Math receiver/callee registers carry boxed identity values, not
    // numbers.  Their LoadGlobal/GetProp bytecodes are emitted as guarded frame
    // writes and MathOp consumes only the numeric argument window.  Reuse the
    // established receiver-exclusion set so neither value is assigned a home.
    for site in &captured_math {
        let receiver_reused = code[s..=e]
            .iter()
            .enumerate()
            .any(|(off, ins)| s + off != site.load_ip && writes_reg(ins) == Some(site.receiver));
        if receiver_reused {
            split_recvs.insert(site.receiver);
            split_recv_lg.insert(site.load_ip);
        } else {
            ta_recv_regs.insert(site.receiver);
        }
        let callee_reused = code[s..=e]
            .iter()
            .enumerate()
            .any(|(off, ins)| s + off != site.get_ip && writes_reg(ins) == Some(site.callee));
        if callee_reused {
            let boxed_defs: FxHashSet<usize> = [site.get_ip].into_iter().collect();
            let boxed_use = |ip: usize| (ip == site.call_ip).then_some(site.callee);
            if crate::codegen::plan::split_home_provably_safe(
                code,
                s,
                e,
                site.callee,
                cold,
                &boxed_defs,
                &boxed_use,
            ) {
                // The frame slot carries the captured callable between GetProp
                // and MathOp; numeric defs outside that window are
                // write-through.  This keeps exact-ip deopts from replacing the
                // saved callable with an unrelated numeric home during flush.
                split_recvs.insert(site.callee);
                split_recv_lg.insert(site.get_ip);
            } else {
                decline!("captured Math callee: home not provably live at a use");
            }
        } else {
            ta_recv_regs.insert(site.callee);
        }
    }
    let captured_pin_sites: Vec<_> = ta_plan
        .captured_calls
        .values()
        .copied()
        .filter(|site| (s..=e).contains(&site.call_ip) && (s..=e).contains(&site.get_ip))
        .collect();
    let captured_pin_get_ips: FxHashSet<usize> =
        captured_pin_sites.iter().map(|site| site.get_ip).collect();
    // The saved method Value is boxed control state.  A dedicated GetProp arm
    // writes its exact guarded bits to the frame; only a later numeric reuse of
    // the compiler temp receives a home.
    for site in &captured_pin_sites {
        let callee_reused = code[s..=e]
            .iter()
            .enumerate()
            .any(|(off, ins)| s + off != site.get_ip && writes_reg(ins) == Some(site.callee));
        if callee_reused {
            let boxed_defs: FxHashSet<usize> = [site.get_ip].into_iter().collect();
            let boxed_use = |ip: usize| (ip == site.call_ip).then_some(site.callee);
            if crate::codegen::plan::split_home_provably_safe(
                code,
                s,
                e,
                site.callee,
                cold,
                &boxed_defs,
                &boxed_use,
            ) {
                // Preserve the spec-order captured callable across every
                // intermediate/call-site deopt. Numeric halves are
                // write-through; flush therefore leaves this boxed frame value
                // intact until CallWithThis consumes it.
                split_recvs.insert(site.callee);
                split_recv_lg.insert(site.get_ip);
            } else {
                decline!("captured member callee: home not provably live at a use");
            }
        } else {
            ta_recv_regs.insert(site.callee);
        }
    }
    // Registers that are actually USED as an operand somewhere in the region.
    // A defined-but-unused reg is DEAD — it must NOT be hoisted, or it would
    // consume a permanent xmm home for a value that is never read. Computed HERE,
    // ahead of its first consumer, because the object-ref rule immediately below
    // asks the same question and there is no reason for the body to be walked
    // twice to answer it; the dead-code pass further down is the other consumer,
    // and the two widenings between here and there (a pinned-DV `CallMethod` dst,
    // the per-ip `str_imul_touch` operands) deliberately apply to that one only —
    // neither can name a register this base set lacks.
    let mut used: FxHashSet<u16> = FxHashSet::default();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) {
            continue;
        }
        for u in numeric_uses(s + off, instr) {
            used.insert(u);
        }
    }
    // ── object-ref `LoadGlobal` with a dst the region never reads ── a
    // `LoadGlobal { dst, idx }` whose `dst` this region defines exactly once (here)
    // and then never reads is a pinned receiver by every property `ta_recv_regs`
    // names: the value is whatever the global holds — routinely an OBJECT, which
    // no numeric home can carry — and no body op wants it in a register. So say
    // it with that set rather than a second way: `dst` is left untyped and
    // unhomed, `idx` gets no global home either (the `LoadGlobal` arm in the
    // typing loop below skips its `glob_order` registration), and all three
    // emitters lower the load to `emit_recv_slot_store` — two `mov`s that make
    // the register's FRAME SLOT mirror the interpreted instruction one-for-one,
    // on exactly the paths that execute it.
    //
    // W18: this is what SROA field promotion needs, and what it used to FAKE.
    // `rewrite_for_field_promotion` turned each object-ref load into `LoadInt 0`
    // and leaned on the dead-code pass below to delete the register — whose
    // licence is `!read_outside`, true only while `instr_uses` was blind to 185
    // of 221 opcodes. W17 made that table exhaustive, the licence started being
    // correctly refused for any register the enclosing function reuses, and the
    // fake `LoadInt` pinned an xmm home that was entry-loaded from a slot holding
    // the object: entry bail on every OSR entry, eviction, recompile at MEM
    // (bench/object.js 0.89ms → 3.84ms). Nothing here weakens `read_outside`;
    // the slot store makes the question moot, because it writes what the
    // interpreter would write, where the interpreter would write it.
    //
    // Counting defs with `writes_reg` (which has a `_ => None` arm) is exact
    // here rather than merely conventional: every emitter declines an op it has
    // no arm for, and every op the numeric emitters DO admit is in that table —
    // so a def this misses cannot appear at a non-cold ip of a region that
    // compiles. Cold ips are skipped on both sides: they are side exits, and the
    // interpreter re-runs them from the frame slot this load keeps authoritative.
    {
        let mut def_n: FxHashMap<u16, u32> = FxHashMap::default();
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) {
                continue;
            }
            if let Some(d) = numeric_def(s + off, instr) {
                *def_n.entry(d).or_insert(0) += 1;
            }
        }
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) {
                continue;
            }
            if let Instr::LoadGlobal { dst, .. } = *instr {
                if def_n.get(&dst) == Some(&1) && !used.contains(&dst) {
                    ta_recv_regs.insert(dst);
                }
            }
        }
    }
    let jump_targets = region_jump_targets(code, s, e);
    // Registers read anywhere OUTSIDE `[s, e]` in the enclosing function. Used
    // by the DV flag-fusion veto here, and by the dead-code / home-sharing
    // passes below (same conservative set, computed once).
    let read_outside: FxHashSet<u16> = code
        .iter()
        .enumerate()
        .filter(|(ip, _)| *ip < s || *ip > e)
        .flat_map(|(_, instr)| instr_uses(instr))
        .collect();

    // ── DV endian-flag fusion ── see `RegionPlan::dv_flag_elide`. The bytecode
    // for `dv.getUint32(o, le === 1)` writes the Eq straight into the arg
    // window, and the compiler then RECYCLES that window register as a numeric
    // temp — a Bool def and a Num def on one register, which is a type
    // conflict under the one-type-per-register model and declined the whole
    // swizzle region. Eliding the Eq (the call computes ToBoolean(a === b)
    // inline from the operands' f64 homes) removes the Bool def; the fuse is
    // admitted only when the Eq's Bool is provably dead past the call:
    //   * the Eq is IMMEDIATELY before the call and writes `arg_base + 1`;
    //   * a later non-compare def `m` of the register kills the value, with NO
    //     use of the register in `(call, m]`, NO branch/Return in `(eq, m)`
    //     (so the region cannot be left while the elided Bool is live — a
    //     flush would write the numeric home over the semantic Bool), and NO
    //     in-region jump target in `(eq, m]` (control cannot enter between
    //     the Eq and its kill, natively or in the interpreter);
    //   * every op in `(call, m)` is a PURE-NUMERIC one that cannot throw on
    //     the numeric state the flush reproduces (no CallMethod / index op /
    //     MathOp inside the window). With no branch, no throw and no jump
    //     target, the only way out of the window is a deopt that resumes IN
    //     it — and the interpreter then runs straight through `m`, rewriting
    //     the register before any use anywhere (in or out of the region) can
    //     see the numeric home the flush wrote where the Bool would have
    //     been. That is what makes a register READ OUTSIDE the region safe
    //     to fuse: the taint provably dies inside every execution;
    //   * no def of the register other than elided Eqs is a compare (so the
    //     register types Num from its remaining defs).
    // On any deopt the fused access resumes AT the Eq ip: the interpreter
    // recomputes the flag into the frame slot, then re-runs the call — the
    // re-executed window is exactly the pure Eq, whose operands' homes were
    // flushed with the values the Eq would have read (nothing sits between).
    let mut dv_flag_elide: FxHashSet<usize> = FxHashSet::default();
    let mut dv_flag_fuse: FxHashMap<usize, (u16, u16)> = FxHashMap::default();
    {
        // Candidates: (call ip, eq ip, flag reg, a, b).
        let mut cand: Vec<(usize, usize, u16, u16, u16)> = Vec::new();
        for (off, instr) in code[s..=e].iter().enumerate() {
            let ip = s + off;
            if cold.contains(&ip) || ip == s || !pinned_dv(ip) {
                continue;
            }
            let sig = match *instr {
                Instr::CallMethod {
                    arg_base,
                    argc: 2,
                    name,
                    ..
                } => Some((arg_base, name)),
                Instr::CallWithThis { .. } => captured_pin_call(ip)
                    .filter(|site| site.argc == 2)
                    .map(|site| (site.arg_base, site.name)),
                _ => None,
            };
            if let Some((arg_base, name)) = sig {
                let size = proto
                    .string_constants
                    .get(name as usize)
                    .and_then(|k| dv_get_kind(k))
                    .map_or(0u8, |k| [1u8, 1, 1, 2, 2, 4, 4, 4, 8][k as usize]);
                if size <= 1 {
                    continue; // single-byte kinds never read the flag
                }
                if let Instr::Eq { dst, a, b } = code[ip - 1] {
                    if dst == arg_base + 1 && !cold.contains(&(ip - 1)) {
                        cand.push((ip, ip - 1, dst, a, b));
                    }
                }
            }
        }
        // Validate to a fixpoint: dropping a candidate restores its Eq as a
        // real (Bool) def, which can invalidate another candidate's analysis
        // of the same register.
        loop {
            let elide: FxHashSet<usize> = cand.iter().map(|&(_, eip, ..)| eip).collect();
            let mut drop_at: Option<usize> = None;
            for (k, &(cip, eip, f, _a, _b)) in cand.iter().enumerate() {
                // Non-elided defs of f, and the first one after the call (m).
                let mut m: Option<usize> = None;
                let mut other_def = false;
                let mut def_is_cmp = false;
                for (off2, i2) in code[s..=e].iter().enumerate() {
                    let ip2 = s + off2;
                    if cold.contains(&ip2) || elide.contains(&ip2) {
                        continue;
                    }
                    if writes_reg(i2) == Some(f) {
                        other_def = true;
                        if matches!(
                            i2,
                            Instr::Lt { .. }
                                | Instr::Le { .. }
                                | Instr::Gt { .. }
                                | Instr::Ge { .. }
                                | Instr::Eq { .. }
                                | Instr::Ne { .. }
                        ) {
                            def_is_cmp = true;
                        }
                        if ip2 > cip && m.is_none() {
                            m = Some(ip2);
                        }
                    }
                }
                let m = match (other_def, def_is_cmp, m) {
                    (true, false, Some(m)) => m,
                    _ => {
                        drop_at = Some(k);
                        break;
                    }
                };
                // No use of f between the call and its kill (the kill's own
                // operands included — they read the pre-def value).
                let tainted_use = (cip + 1..=m)
                    .any(|ip2| !cold.contains(&ip2) && instr_uses(&code[ip2]).contains(&f));
                // No way out of `(eq, m)` while the Bool is live, and no way
                // in: no branch, no jump target, and only PURE-NUMERIC ops
                // that cannot throw on the numeric state a deopt flush
                // reproduces (a DV call in the window could raise RangeError
                // mid-taint; an index op could observe the heap).
                let window_op_bad = (cip + 1..m).any(|ip2| {
                    !matches!(
                        code[ip2],
                        Instr::LoadInt { .. }
                            | Instr::LoadConst { .. }
                            | Instr::Move { .. }
                            | Instr::LoadGlobal { .. }
                            | Instr::StoreGlobal { .. }
                            | Instr::StoreGlobalStrict { .. }
                            | Instr::StoreGlobalResolved { .. }
                            | Instr::Add { .. }
                            | Instr::Sub { .. }
                            | Instr::Mul { .. }
                            | Instr::Div { .. }
                            | Instr::Mod { .. }
                            | Instr::AddInt { .. }
                            | Instr::Neg { .. }
                            | Instr::Bitwise { .. }
                            | Instr::Lt { .. }
                            | Instr::Le { .. }
                            | Instr::Gt { .. }
                            | Instr::Ge { .. }
                            | Instr::Eq { .. }
                            | Instr::Ne { .. }
                    )
                });
                let target_in_window = jump_targets.iter().any(|&t| t > eip && t <= m);
                if tainted_use || window_op_bad || target_in_window {
                    drop_at = Some(k);
                    break;
                }
            }
            match drop_at {
                Some(k) => {
                    cand.remove(k);
                }
                None => break,
            }
        }
        for &(cip, eip, _f, a, b) in &cand {
            dv_flag_elide.insert(eip);
            dv_flag_fuse.insert(cip, (a, b));
        }
    }

    // ── W28 type-aware live-range splitting ── see `RegionPlan::ty_splits` for
    // the mechanism and `plan_type_splits` for the legality predicate. GATE:
    // only for plans routed EXCLUSIVELY into the GPR emitter (`admit_dv` and
    // every `share_homes` call site feed `compile_region_int_gpr` alone — the
    // same routing argument the glob-range pass below stands on), on the
    // integer path, with no cold side exits, and only where the caller
    // implements per-def write-through: the exit contract is write-through's.
    // Every other planner caller sees an empty map and plans byte-identically,
    // which is what makes the 13 `bench/real` rows unchanged by construction.
    // B192: statement-completion regs (in-region `LoadUndefined` dst, never
    // read in-region) stay UNTYPED and UNHOMED — `LoadUndefined` already
    // falls to the no-def catch-all below, and the `Move` arm skips them so
    // a per-statement completion `Move` cannot type the reg Num (a home
    // would clash with the UNDEFINED write and go stale at exit-flush).
    // Both INT emitters write every def of these regs through to the frame
    // slot; the admission scan admits `LoadUndefined` only for this set.
    let undef_dead = if crate::codegen::undef_admit_enabled() {
        super::region_int::undef_dead_regs(proto, s, e)
    } else {
        FxHashSet::default()
    };

    let ty_splits: FxHashMap<u16, TySplit> =
        if admit_bitwise && (admit_dv || share_homes) && admit_wt_share && cold.is_empty() {
            let mut excluded: FxHashSet<u16> = ta_recv_regs.clone();
            excluded.extend(split_recvs.iter().copied());
            excluded.extend(box_regs.iter().copied());
            excluded.extend(undef_dead.iter().copied());
            for pin in &ta_plan.pins {
                if let TaPinSrc::Reg(r) = pin.src {
                    excluded.insert(r);
                }
            }
            let dv_flag_reg = |ip: usize| -> Option<u16> {
                match code[ip] {
                    Instr::CallMethod {
                        arg_base, argc: 2, ..
                    } if pinned_dv(ip) => Some(arg_base + 1),
                    Instr::CallWithThis { .. } if pinned_dv(ip) => captured_pin_call(ip)
                        .filter(|site| site.argc == 2)
                        .map(|site| site.arg_base + 1),
                    _ => None,
                }
            };
            plan_type_splits(
                code,
                s,
                e,
                &jump_targets,
                &excluded,
                &split_recv_lg,
                &dv_flag_elide,
                &dv_flag_fuse,
                &dv_flag_reg,
            )
        } else {
            FxHashMap::default()
        };
    if !ty_splits.is_empty() && std::env::var_os("ZIPP_JITLOG").is_some() {
        let mut v: Vec<(u16, TySplit)> = ty_splits.iter().map(|(&r, &sp)| (r, sp)).collect();
        v.sort_unstable_by_key(|x| x.0);
        for (r, sp) in v {
            eprintln!(
                "[jit] region [{s},{e}] type-split r{r}: bool=[{},{}] num=[{},{}]",
                sp.bool_lo, sp.bool_hi, sp.num_lo, sp.num_hi
            );
        }
    }

    // W28: is `r` a type-split register whose BOOL half is live at `ip`? Every
    // `ty`-based Bool test below has to ask this too — a split register is
    // typed `VTy::Num` (its numeric half is the one `reg_home` names) while
    // being a genuine Bool inside its bool range, where `gh` resolves to its
    // own gpr.
    let split_bool_at = |r: u16, ip: usize| -> bool {
        ty_splits
            .get(&r)
            .is_some_and(|sp| ip >= sp.bool_lo && ip <= sp.bool_hi)
    };

    let mut ty: FxHashMap<u16, VTy> = FxHashMap::default();
    let mut first_seen: FxHashMap<u16, bool> = FxHashMap::default(); // reg → was first occurrence a def?
    let mut glob_first_read: FxHashMap<u32, bool> = FxHashMap::default(); // slot → first touch was a read?
    let mut reg_order: Vec<u16> = Vec::new();
    let mut glob_order: Vec<u32> = Vec::new();

    // Record a use (operand) of reg `r` with required type `req`.
    // Returns false on a type conflict (caller declines).
    let note_def = |r: u16,
                    t: VTy,
                    ty: &mut FxHashMap<u16, VTy>,
                    first_seen: &mut FxHashMap<u16, bool>,
                    reg_order: &mut Vec<u16>|
     -> bool {
        if let Some(prev) = ty.get(&r) {
            if *prev != t {
                return false;
            }
        } else {
            ty.insert(r, t);
            reg_order.push(r);
        }
        first_seen.entry(r).or_insert(true); // first occurrence is a def
        true
    };

    // Two passes are awkward with closures; do a single ordered pass collecting
    // type (from defs) and first-occurrence (def vs use). Operand type
    // requirements are validated in a second loop once types are known.
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) {
            continue;
        }
        // A call or a bitwise op can't be register-allocated (boxed Values / int32
        // lanes / arbitrary user code) — decline to the memory path. A dense-array
        // GetIndex/SetIndex likewise declines UNLESS it is a kind-8 (Float64) pinned
        // TypedArray access, which the f64 element fast path emits inline (the
        // element is a VTy::Num xmm home; receiver/index handled specially below).
        match *instr {
            Instr::Call { .. } => decline!("Call"),
            // The computed-call helper can run arbitrary user code after its
            // pure dense-slot guard succeeds. Keep it off every register tier;
            // the boxed MEMORY emitter owns the helper/refetch protocol.
            Instr::CallMethodComputed { .. } => decline!("CallMethodComputed"),
            // A pinned-STRING charCodeAt is admitted on the int path (inlines to a
            // byte load, runs no user code, allocates nothing — the no-call
            // invariant that keeps BOOL_GPRS alive holds), and a pinned-DataView
            // `get*` on the double path (same properties: a guarded byte load, no
            // user code, no allocation). Any other method call, or an access whose
            // receiver isn't pinned, declines.
            // W20 M2: `arr.push(int)` on a dense all-Int Array pin joins the
            // admitted set on the INT path. It is the ONE admitted op that
            // issues a call, so the no-call invariant this decline protects is
            // re-established explicitly at the emission site rather than by the
            // absence of calls -- see `int_push_enabled` for the argument, and
            // note the two things it rests on: the helper runs no user code and
            // performs no VM-heap allocation (so it cannot GC), and the emitter
            // saves every planner-owned volatile register across it.
            Instr::CallMethod { .. }
                if !pinned_str(s + off) && !pinned_dv(s + off) && !pinned_arr_push(s + off) =>
            {
                decline!("CallMethod (receiver not a pinned string/DataView)")
            }
            Instr::CallWithThis { .. }
                if !pinned_str(s + off) && !pinned_dv(s + off) && !pinned_arr_push(s + off) =>
            {
                decline!("CallWithThis (not a captured pinned string/DataView/Array.push)")
            }
            // A Bitwise op declines UNLESS the caller (the INT path) admits it: its
            // i64 homes hold sign-extended integers, so the low 32 bits ARE ToInt32
            // and the op runs inline with no reload/rebox. The regalloc/double path
            // passes admit_bitwise=false (its homes are f64, not int32 lanes).
            // B92: the double path now hosts Bitwise too, via a 64-bit
            // `cvttsd2si` whose low 32 bits ARE ToInt32 (exact for |x| < 2^63,
            // which covers every u32), so this no longer declines. Kept as an
            // env-gated escape hatch: `ZIPP_NO_DOUBLE_BITWISE=1` restores the
            // old behaviour for A/B and bisection.
            Instr::Bitwise { .. } if !admit_bitwise && !double_bitwise_enabled() => {
                decline!("Bitwise on the double path")
            }
            Instr::GetIndex { .. } | Instr::SetIndex { .. } => {
                if !pinned_elem(s + off) {
                    decline!("GetIndex/SetIndex (element not a pinned TypedArray)");
                }
            }
            _ => {}
        }
        let (def, dty): (Option<u16>, VTy) = match *instr {
            // A pinned-f64 GetIndex loads an f64 element into a Num home.
            Instr::GetIndex { dst, .. } => (Some(dst), VTy::Num),
            Instr::LoadInt { dst, .. } => (Some(dst), VTy::Num),
            Instr::LoadConst { dst, .. } => (Some(dst), VTy::Num),
            Instr::LoadGlobal { dst, .. } => (Some(dst), VTy::Num),
            Instr::AddInt { dst, .. } => (Some(dst), VTy::Num),
            Instr::Neg { dst, .. } => (Some(dst), VTy::Num),
            Instr::Add { dst, .. }
            | Instr::Sub { dst, .. }
            | Instr::Mul { dst, .. }
            | Instr::Div { dst, .. }
            | Instr::Mod { dst, .. } => (Some(dst), VTy::Num),
            // A bitwise/shift result is always a number (a signed i32, or a u32
            // for `>>>` — both fit a Num i64 home).
            Instr::Bitwise { dst, .. } => (Some(dst), VTy::Num),
            Instr::Lt { dst, .. }
            | Instr::Le { dst, .. }
            | Instr::Gt { dst, .. }
            | Instr::Ge { dst, .. }
            | Instr::Eq { dst, .. }
            | Instr::Ne { dst, .. } => (Some(dst), VTy::Bool),
            // Statement-form push results are globally unread.  The call is
            // still emitted for its append side effect, but assigning its
            // recycled compiler temporary a numeric home can conflict with a
            // later Bool/control use and needlessly blocks the capture-first
            // tokenizer region.  A read anywhere keeps the ordinary Num dst.
            Instr::CallMethod { .. } | Instr::CallWithThis { .. }
                if discarded_arr_push_dst(s + off).is_some() =>
            {
                (None, VTy::Num)
            }
            // Pinned-STRING charCodeAt → a small int (0..65535); pinned-STRING
            // length → the snapshot units; both land in a Num i64 home. A
            // pinned-DV `get*` result is a Num too: every whitelisted kind is
            // exact in an f64 home (u32 <= 2^32-1; the float kinds native).
            Instr::CallMethod { dst, .. }
                if pinned_str(s + off) || pinned_dv(s + off) || pinned_arr_push(s + off) =>
            {
                (Some(dst), VTy::Num)
            }
            Instr::CallWithThis { dst, .. }
                if pinned_str(s + off) || pinned_dv(s + off) || pinned_arr_push(s + off) =>
            {
                (Some(dst), VTy::Num)
            }
            // Captured method lookup produces a boxed callable in the frame,
            // not a numeric result. Its paired call below owns the Num dst.
            Instr::GetProp { .. } if captured_pin_get(s + off).is_some() => (None, VTy::Num),
            Instr::GetProp { dst, .. } if pinned_str(s + off) => (Some(dst), VTy::Num),
            // W20: an admitted `GetProp` lands its probe result in a Num home
            // under a tag guard (`emit_box_to_home`) that DEOPTs on anything
            // else -- the same contract the dense-Array element read has used
            // on this tier since B95.
            Instr::GetProp { dst, .. } if getprop_ips.contains(&(s + off)) => (Some(dst), VTy::Num),
            // `Math.imul` → a signed i32 (Num). BLOCKER FIX: without this the carried
            // fnv1a accumulator (written ONLY by Imul) is never typed → the
            // used-but-undefined scan declines the whole region.
            Instr::MathOp {
                dst,
                op: MathFn::Imul,
                argc: 2,
                ..
            } => (Some(dst), VTy::Num),
            // ToPropertyKey of a NUMBER is the identity (no observable
            // coercion), so on this tier the op is a register copy and its dst
            // is a Num. The identity claim needs the src to actually BE a
            // number: a live-in src is entry-guarded numeric (it is in
            // `numeric_operand_uses`), an in-region Num def proves itself, and
            // a Bool def must DECLINE — the interpreter coerces `true` to the
            // STRING key "true", where a copy would index element 1.
            Instr::ToPropKey { dst, src, .. } => {
                if ty.get(&src) == Some(&VTy::Bool) {
                    decline!("ToPropKey of a Bool-typed key");
                }
                (Some(dst), VTy::Num)
            }
            // W20 M4: `!b` yields a Bool from a Bool. INT path only (the other
            // tiers have no `Not` arm and still decline at their emitter's
            // catch-all, exactly as today).
            Instr::Not { dst, .. } if admit_bitwise && int_push_enabled() => (Some(dst), VTy::Bool),
            Instr::Move { dst, .. } => (Some(dst), VTy::Num), // refined below
            _ => (None, VTy::Num),
        };
        // Record operand first-occurrences (uses) BEFORE the def, so a reg used
        // and defined by the same op counts the use first (live-in).
        for u in numeric_uses(s + off, instr) {
            first_seen.entry(u).or_insert(false); // first occurrence is a use ⇒ live-in
            if !ty.contains_key(&u) {
                // Type not yet known; tentatively untyped — refined when defined.
            }
        }
        // A TA-receiver reg is NOT typed/homed (sourced via the pin); skip its def.
        // A B94 SPLIT receiver keeps its numeric home, but its `LoadGlobal` def is
        // the RECEIVER half — typing that as Num would home an object.
        // A DV-flag-fused Eq's def is ELIDED (the call computes the flag inline),
        // so its Bool must not type the register — the remaining defs are Num.
        let is_split_recv_load = split_recv_lg.contains(&(s + off))
            || captured_pin_get_ips.contains(&(s + off))
            || dv_flag_elide.contains(&(s + off));
        // B192: a completion reg (`undef_dead`) may ONLY be defined by the two
        // ops the INT emitters write through (`LoadUndefined` reaches the
        // no-def catch-all; `Move` is filtered below). Any other def-op means
        // this shape is not the statement-completion pattern — decline to the
        // MEM tier rather than home a reg the write-through contract skips.
        if let Some(d) = def {
            if undef_dead.contains(&d) && !matches!(*instr, Instr::Move { .. }) {
                decline!("undef-dead completion reg defined by unsupported op");
            }
        }
        // W20: a `box_regs` member is deliberately UNTYPED and UNHOMED -- its
        // value lives in the frame slot the def writes.
        if let Some(d) = def.filter(|d| {
            !ta_recv_regs.contains(d)
                && !is_split_recv_load
                && !box_regs.contains(d)
                && !undef_dead.contains(d)
        }) {
            // Move's dst type follows its src; default Num is corrected here.
            let t = if let Instr::Move { src, .. } = *instr {
                *ty.get(&src).unwrap_or(&VTy::Num)
            } else {
                dty
            };
            if !note_def(d, t, &mut ty, &mut first_seen, &mut reg_order) {
                // ── W28 ── a register the bytecode compiler recycled across a
                // type boundary is not a decline any more IF `plan_type_splits`
                // proved its two ranges cannot interfere. The register keeps a
                // NUMERIC home (so every numeric lookup is unchanged) and takes
                // a separate gpr for its Bool range, assigned below.
                //
                // This is also where the pre-pass's type PREDICTION is checked
                // against the live typing pass, op by op: the two derive the
                // def type from separate matches, and a disagreement — a new
                // opcode, a refined `dty` — must decline, never silently home a
                // value under the wrong type.
                let ok = ty_splits.get(&d).is_some_and(|sp| {
                    let want = if s + off >= sp.bool_lo && s + off <= sp.bool_hi {
                        VTy::Bool
                    } else {
                        VTy::Num
                    };
                    want == t
                        && ty.get(&d) == Some(&if t == VTy::Bool { VTy::Num } else { VTy::Bool })
                });
                if !ok {
                    decline!("type conflict on a reused register");
                }
                // A split register is typed Num for the whole region: its
                // numeric half is the one `reg_home` names.
                ty.insert(d, VTy::Num);
                first_seen.entry(d).or_insert(true);
            }
        }
        // Globals: order + first-touch direction. A TA-receiver's LoadGlobal is
        // excluded (no numeric home; the element emitter reads it via the pin).
        match *instr {
            // Neither a pinned receiver's global nor a B94 split receiver's gets an
            // xmm home: the value is an object, and the entry guard would reject it.
            Instr::LoadGlobal { dst, .. }
                if ta_recv_regs.contains(&dst)
                    || split_recv_lg.contains(&(s + off))
                    || box_regs.contains(&dst) => {}
            Instr::LoadGlobal { idx, .. } => {
                glob_first_read.entry(idx).or_insert(true);
                if !glob_order.contains(&idx) {
                    glob_order.push(idx);
                }
            }
            Instr::StoreGlobal { idx, .. }
            | Instr::StoreGlobalStrict { idx, .. }
            | Instr::StoreGlobalResolved { idx, .. } => {
                glob_first_read.entry(idx).or_insert(false);
                if !glob_order.contains(&idx) {
                    glob_order.push(idx);
                }
            }
            _ => {}
        }
    }

    // A register used but never defined in the region is a read-only live-in —
    // most often a numeric FUNCTION PARAMETER, e.g. the `n` in
    // `function f(n){ for (var k=0;k<n;k++) ... }`.
    //
    // MEASURED 2026-07-25 — do not admit these BLANKETLY by typing them Num and
    // letting the entry guard sort it out. That is correct (emit_int_entry_load
    // bails unless the value is genuinely Int-tagged, so nothing can be misread)
    // but it is SLOWER: the INT path then accepts regions whose live-ins are
    // strings, doubles or objects, entry-bails on every OSR entry, and displaces
    // the MEM compile that was working. Suite geomean regressed 3.31x -> 3.45x,
    // worst on sparse-array (-18%) and async-promise-chain (-16%).
    //
    // What that note asked for, and what this is: admit ONLY registers used
    // exclusively as ARITHMETIC OPERANDS (`numeric_operand_uses`), so the guard
    // is backed by how the value is consumed rather than by hope. A live-in that
    // ever reaches a heap op, a `Move`, a `StoreGlobal`, an `Add` (which is also
    // string concat) or an `Eq`/`Ne` (which accept any type) still declines the
    // whole region, exactly as before. When the guard does fail, `entry_bail`
    // resumes at the loop header — an in-region ip — so it counts as a deopt and
    // the region self-evicts to the memory path after OSR_DEOPT_LIMIT tries.
    //
    // Worth 2.2x on the shape it unblocks: the same 20M-iteration loop ran 115ms
    // with a parameter bound (INT declined -> DOUBLE/MEM) vs 52ms with a literal
    // bound (INT compiled).
    //
    // B192: every completion `Move` must copy a NUM-typed source — the
    // emitters box the source home as a number. A Bool or untyped (slot-const
    // / receiver / live-in-unknown) source declines to the MEM tier. Checked
    // here, after the walk, so in-region defs on the backedge are settled.
    if !undef_dead.is_empty() {
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) {
                continue;
            }
            if let Instr::Move { dst, src } = *instr {
                if undef_dead.contains(&dst) && ty.get(&src) != Some(&VTy::Num) {
                    decline!("completion move of a non-Num source");
                }
            }
        }
    }
    // TA-receiver regs are intentionally untyped (sourced via the pin) — skip them.
    let mut ro_live_in: Vec<u16> = Vec::new();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) {
            continue;
        }
        for u in numeric_uses(s + off, instr) {
            if !ta_recv_regs.contains(&u)
                && !box_regs.contains(&u)
                && !ty.contains_key(&u)
                && !ro_live_in.contains(&u)
            {
                ro_live_in.push(u);
            }
        }
    }
    if !ro_live_in.is_empty() {
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) {
                continue;
            }
            // B98: on the DOUBLE path, an `Add` operand counts as a numeric-required
            // use. `Add` is excluded globally because it is also string concat, so a
            // string live-in is unremarkable and the entry guard would miss on every
            // OSR entry — that is the 3.31x -> 3.45x regression recorded above. But
            // that measurement was of BLANKET admission on the INT path, and its
            // stated causes were live-ins that are "strings, doubles or objects": a
            // DOUBLE live-in bails the int path and is perfectly native here, so the
            // largest of the three does not apply. A string or object still bails,
            // correctly, via `emit_box_to_home` at entry.
            let mut numeric = numeric_operand_uses(instr);
            if !admit_bitwise && wt_share_enabled() {
                if let Instr::Add { a, b, .. } = *instr {
                    numeric.push(a);
                    numeric.push(b);
                }
            }
            // W20, and the reason the mechanism reaches `property-ic-shapes` at
            // all: its four read loops wrap with `if (k === n) k = 0`, so the
            // bound `n` is a read-only live-in whose ONLY use is an `Eq`. `Eq`
            // is excluded from `numeric_operand_uses` globally because it is
            // defined on every type -- B98's reasoning for admitting `Add` on
            // the DOUBLE path applies here verbatim (a double live-in is native
            // here; a string or object still bails at entry through
            // `emit_box_to_home`) -- but the 3.31x -> 3.45x regression that
            // exclusion is guarding against was BLANKET admission on the INT
            // path. This is confined to regions that already carry a BOXREF
            // probe, so `ZIPP_NO_BOX_HOME=1` restores the old plan exactly.
            if !getprop_ips.is_empty() {
                if let Instr::Eq { a, b, .. } | Instr::Ne { a, b, .. } = *instr {
                    numeric.push(a);
                    numeric.push(b);
                }
            }
            for u in numeric_uses(s + off, instr) {
                if ro_live_in.contains(&u) && !numeric.contains(&u) {
                    decline!("read-only live-in used where a number isn't required");
                    //
                }
            }
        }
        // Entry-guarded Int, permanently homed (live-in ⇒ whole-region range).
        for &r in &ro_live_in {
            ty.insert(r, VTy::Num);
            first_seen.insert(r, false);
            reg_order.push(r);
        }
    }

    // Each pinned index op needs its index (and a SetIndex's stored value) in a
    // Num home: the emitter reads the index from `key`'s home and a SetIndex stores
    // `val`'s home. Decline if either isn't a number home.
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) {
            continue;
        }
        if pinned_elem(s + off) {
            let bad = match *instr {
                Instr::GetIndex { key, .. } => ty.get(&key) != Some(&VTy::Num),
                Instr::SetIndex { key, val, .. } => {
                    ty.get(&key) != Some(&VTy::Num) || ty.get(&val) != Some(&VTy::Num)
                }
                _ => false,
            };
            if bad {
                decline!("pinned index operand is not numeric");
            }
        }
        // A pinned-DV get* reads its pos from an f64 home (cvttsd2si) and — for
        // the multi-byte kinds with an explicit flag — its littleEndian either
        // INLINE (a fused adjacent Eq: ToBoolean(a === b) from two Num homes)
        // or from a Bool gpr home (a 0/1, where `test` IS ToBoolean). Any
        // other operand shape declines: a Num-typed flag would need an
        // observable-equivalent ToBoolean the emitter doesn't implement, and
        // the single-byte kinds ignore the argument entirely (as the
        // interpreter does).
        if pinned_dv(s + off) {
            let sig = match *instr {
                Instr::CallMethod {
                    arg_base,
                    argc,
                    name,
                    ..
                } => Some((arg_base, argc, name)),
                Instr::CallWithThis { .. } => {
                    captured_pin_call(s + off).map(|site| (site.arg_base, site.argc, site.name))
                }
                _ => None,
            };
            if let Some((arg_base, argc, name)) = sig {
                if ty.get(&arg_base) != Some(&VTy::Num) {
                    decline!("pinned DV pos operand is not numeric");
                }
                let size = proto
                    .string_constants
                    .get(name as usize)
                    .and_then(|k| dv_get_kind(k))
                    .map_or(0u8, |k| [1u8, 1, 1, 2, 2, 4, 4, 4, 8][k as usize]);
                if argc == 2
                    && size > 1
                    && !dv_flag_fuse.contains_key(&(s + off))
                    && ty.get(&(arg_base + 1)) != Some(&VTy::Bool)
                    && !split_bool_at(arg_base + 1, s + off)
                {
                    decline!("pinned DV endian flag is not a Bool");
                }
            }
        }
    }

    // Validate operand type requirements now that types are known.
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) {
            continue;
        }
        match *instr {
            Instr::Add { a, b, .. }
            | Instr::Sub { a, b, .. }
            | Instr::Mul { a, b, .. }
            | Instr::Div { a, b, .. }
            | Instr::Mod { a, b, .. }
            | Instr::Lt { a, b, .. }
            | Instr::Le { a, b, .. }
            | Instr::Gt { a, b, .. }
            | Instr::Ge { a, b, .. }
            | Instr::Eq { a, b, .. }
            | Instr::Ne { a, b, .. }
            | Instr::JumpIfNotLt { a, b, .. }
            | Instr::JumpIfNotLe { a, b, .. }
            | Instr::Bitwise { a, b, .. } => {
                if ty.get(&a) == Some(&VTy::Bool) || ty.get(&b) == Some(&VTy::Bool) {
                    decline!("numeric op on a bool"); // outside the subset
                }
            }
            Instr::AddInt { a, .. } | Instr::Neg { a, .. } => {
                if ty.get(&a) == Some(&VTy::Bool) {
                    decline!("numeric op on a bool"); // outside the subset
                }
            }
            Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => {
                // Only bool conditions are supported (the loop-guard shape).
                if ty.get(&cond) != Some(&VTy::Bool) && !split_bool_at(cond, s + off) {
                    decline!("branch condition is not a bool");
                }
            }
            // W20 M2: the pushed value is read from an i64 NUMERIC home and
            // boxed by the same Int-if-it-fits-else-double rule `flush_exit`
            // uses. A Bool-typed operand lives in a gpr instead and would be
            // pushed as a number -- decline the region rather than widen the
            // arm.
            Instr::CallMethod { arg_base, .. } if pinned_arr_push(s + off) => {
                if ty.get(&arg_base) != Some(&VTy::Num) {
                    decline!("arr.push operand is not a numeric home");
                }
            }
            Instr::CallWithThis { .. } if pinned_arr_push(s + off) => {
                let site = captured_pin_call(s + off).expect("captured Array.push metadata");
                if ty.get(&site.arg_base) != Some(&VTy::Num) {
                    decline!("arr.push operand is not a numeric home");
                }
            }
            // W20 M4: `xor home, 1` is `!b` only for a REAL boolean. Anything
            // else (`!0`, `!""`, `!obj`) is JS truthiness, which this tier does
            // not model -- decline rather than widen the arm.
            Instr::Not { a, .. } if admit_bitwise && int_push_enabled() => {
                if ty.get(&a) != Some(&VTy::Bool) {
                    decline!("Not of a non-bool");
                }
            }
            _ => {}
        }
    }

    // Loop-invariant constant detection: a reg defined exactly once, by a
    // LoadInt/LoadConst, and not live-in, holds the same value every iteration —
    // materialise it once in the prologue and skip the body op.
    let mut def_count: FxHashMap<u16, u32> = FxHashMap::default();
    let mut const_def_ip: FxHashMap<u16, usize> = FxHashMap::default();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) {
            continue;
        }
        match *instr {
            Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } => {
                *def_count.entry(dst).or_insert(0) += 1;
                const_def_ip.insert(dst, s + off);
            }
            _ => {
                if let Some(d) = numeric_def(s + off, instr) {
                    *def_count.entry(d).or_insert(0) += 1;
                }
            }
        }
    }
    // A pinned-DV get* is NOT a pure value op: an out-of-range pos throws
    // RangeError. Keep its dst out of `dead` — the emitter skips a dead-dst op
    // entirely, which would skip the throw (and leave the dst with no home for
    // the arm that does emit).
    //
    // W20 M2: an admitted `arr.push(int)` is the same case and a sharper one.
    // Its dst (the new length) is USUALLY dead — `kinds.push(1);` as a
    // statement never reads the result — and the dead-code pass would then skip
    // the whole op, silently dropping the append. Keeping the dst `used` is
    // what makes the side effect survive.
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) {
            continue;
        }
        let effectful_dst = match *instr {
            Instr::CallMethod { dst, .. } => Some(dst),
            Instr::CallWithThis { dst, .. } if pinned_dv(s + off) || pinned_arr_push(s + off) => {
                Some(dst)
            }
            _ => None,
        };
        if let Some(dst) = effectful_dst {
            if pinned_dv(s + off)
                || (pinned_arr_push(s + off) && discarded_arr_push_dst(s + off).is_none())
            {
                used.insert(dst);
            }
        }
    }
    // ── pinned-STRING / Math.imul operand liveness ── this predates the arms
    // `instr_uses`/`writes_reg` now carry for `CallMethod`/`MathOp` (when it was
    // written both ops hit a catch-all and reported reading and writing
    // NOTHING, so the charCodeAt index reg, the Imul operand regs and both
    // result defs were invisible — the index/operands were classed DEAD, their
    // defining Move/Bitwise/LoadInt was DCE'd, and the inline op then read an
    // unmaterialised home or panicked in `xh`). The shared tables cover the
    // plain operand/dst reads today; what stays LOCAL here is the per-IP part
    // they cannot express — WHICH ip a pinned access reads its operands at, so
    // the home-reuse allocator does not free a home one ip early. Feeds both
    // `used` (here) and the live-range touch loop (below). `(ip,reg,def)`.
    let mut str_imul_touch: Vec<(usize, u16, bool)> = Vec::new();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) {
            continue;
        }
        let ip = s + off;
        match *instr {
            Instr::CallMethod {
                dst,
                arg_base,
                argc: 1,
                ..
            } if pinned_str(ip) => {
                str_imul_touch.push((ip, arg_base, false)); // index (use)
                str_imul_touch.push((ip, dst, true)); // charCodeAt result (def)
            }
            Instr::CallWithThis { .. } if pinned_str(ip) => {
                let site = captured_pin_call(ip).expect("captured string call metadata");
                str_imul_touch.push((ip, site.arg_base, false));
                str_imul_touch.push((ip, site.dst, true));
            }
            Instr::MathOp {
                dst,
                arg_base,
                op: MathFn::Imul,
                argc: 2,
                ..
            } => {
                str_imul_touch.push((ip, arg_base, false));
                str_imul_touch.push((ip, arg_base + 1, false));
                str_imul_touch.push((ip, dst, true)); // imul result (def)
            }
            // A DV-flag-FUSED call reads the elided Eq's operands AT THE CALL
            // ip (the inline ToBoolean(a === b) compare) — extend their live
            // ranges to the call so the home-reuse allocator cannot free
            // either home one ip early.
            Instr::CallMethod { dst, arg_base, .. } => {
                if let Some(&(a, b)) = dv_flag_fuse.get(&ip) {
                    str_imul_touch.push((ip, a, false));
                    str_imul_touch.push((ip, b, false));
                }
                // W9: on the INT path a pinned-DV get*'s pos operand and result
                // def are otherwise invisible (instr_uses/writes_reg are blind
                // to CallMethod), and the GPR route leans on the share_homes
                // re-plan where an untouched range frees a home one ip early.
                // Gated on admit_bitwise so DOUBLE planning stays byte-identical
                // (its RetryNoHoist-only allocation never exercised these).
                if admit_bitwise && pinned_dv(ip) {
                    str_imul_touch.push((ip, arg_base, false)); // pos (use)
                    str_imul_touch.push((ip, dst, true)); // result (def)
                }
            }
            Instr::CallWithThis { .. } if pinned_dv(ip) => {
                let site = captured_pin_call(ip).expect("captured DataView call metadata");
                if let Some(&(a, b)) = dv_flag_fuse.get(&ip) {
                    str_imul_touch.push((ip, a, false));
                    str_imul_touch.push((ip, b, false));
                }
                if admit_bitwise {
                    str_imul_touch.push((ip, site.arg_base, false));
                    str_imul_touch.push((ip, site.dst, true));
                }
            }
            _ => {}
        }
    }
    for &(_, r, is_def) in &str_imul_touch {
        if !is_def {
            used.insert(r);
        }
    }
    // ── dead-code elimination ── a register written in the region but never read
    // (not in `used`) is dead. Every int-region op is a pure value computation, so
    // its defining op produces a result nothing observes and can be skipped — and
    // the reg dropped from home allocation. Drop dead regs from `reg_order` so they
    // consume no xmm home and don't count toward the pool-overflow check (which can
    // flip the loop to the slower home-reuse path). `dead` excludes loop-carried
    // (live-in) regs — those are read across iterations even if not within one.
    // "Never read IN THE REGION" is not "dead": a dead reg gets no home, its
    // defining op is skipped and nothing is flushed, so the frame slot keeps
    // whatever the INTERPRETER last left there. If the register is read AFTER
    // the region, that is a silent wrong answer —
    //
    //     function f(){ for (var i=0;i<40;i++) { var q = i; } return q; }
    //
    // returned 7 (the value at the final pre-OSR iteration) instead of 39. The
    // declarator form is what exposes it: a plain `q = expr` also emits
    // `Move{dst:temp, src:q}` for the statement's value, which keeps `q` in
    // `used`, but `var q = expr` does not.
    //
    // There is no live-out analysis in codegen, so require the register to be
    // untouched by the REST OF THE FUNCTION as well. Conservative (a reg read
    // anywhere outside `[s, e]` is kept) and cheap — `read_outside`, computed
    // once above (the DV flag-fusion veto shares it).
    // W20, and a SILENT WRONG ANSWER this mechanism created before this line
    // existed. Dead-code elimination here is licensed by one sentence on
    // `RegionPlan::dead` — "every regalloc-region op is side-effect-free (heap
    // ops decline the region)" — and the BOXREF `GetProp` arm is the first thing
    // ever to make that sentence false. `o.p` as a STATEMENT has a dead dst, the
    // emitter skipped the op, and a GETTER therefore never ran:
    //
    //     class A { get v(){ n++; return this._v } }
    //     class B extends A { get v(){ return super.v * 2 } }
    //     var b = new B(1);
    //     for (var i = 0; i < 200000; i++) b.v;   // n === 8, not 200000
    //
    // The value is discarded, so nothing but the side effect is observable —
    // which is exactly why the receiver-count and semantics matrices missed it
    // and `zipp-vm`'s own `super_getter_inline_preserves_values_and_effects`
    // caught it. An admitted `GetProp` dst is therefore never dead.
    let getprop_dsts: FxHashSet<u16> = getprop_ips
        .iter()
        .filter_map(|&ip| match code[ip] {
            Instr::GetProp { dst, .. } => Some(dst),
            _ => None,
        })
        .collect();
    let dead: FxHashSet<u16> = reg_order
        .iter()
        .copied()
        .filter(|r| {
            !used.contains(r)
                && first_seen.get(r) != Some(&false)
                && !read_outside.contains(r)
                && !getprop_dsts.contains(r)
        })
        .collect();
    reg_order.retain(|r| !dead.contains(r));
    let mut hoist_ips: Vec<usize> = Vec::new();
    let mut hoisted: FxHashSet<u16> = FxHashSet::default();
    // Did anything `allow_hoist` GATES get hoisted? That — not `hoisted`,
    // which the ungated glob-range remat also fills — is what the pool-pressure
    // retry can release. See the `allow_hoist` parameter doc.
    let mut gated_hoists = false;
    // A read-outside constant needs a stronger form of the ordinary loop
    // hoist proof: no forward edge before its def may bypass the def by
    // LEAVING the region. Otherwise the exit flush would publish the hoisted
    // value on a path where the interpreter retained the old frame value.
    // Constants with no outside reader do not need this extra condition.
    let runs_before_forward_exit = |d: usize| {
        code.iter().take(d).skip(s).all(|instr| {
            let target = match *instr {
                Instr::Jump { target }
                | Instr::JumpIfFalse { target, .. }
                | Instr::JumpIfTrue { target, .. }
                | Instr::JumpIfNotLt { target, .. }
                | Instr::JumpIfNotLe { target, .. } => target as usize,
                _ => return true,
            };
            target <= d
        })
    };
    for (&r, &ip) in &const_def_ip {
        // `first_seen == true` only says the first OCCURRENCE is a def — it says
        // nothing about whether that def runs. Hoisting a constant whose def sits
        // on an untaken branch is wrong twice over: the prologue materialises it
        // (so the flush writes it over the register's real value) and the body op
        // is elided (so reads inside the region see the constant too). Require
        // the def to run on every pass. See `runs_every_iteration`.
        // `allow_hoist == false` is the pool-pressure retry: every constant
        // stays a body op with a short live range instead of pinning a
        // permanent home (see `PlanOutcome::RetryNoHoist`).
        if allow_hoist
            && def_count.get(&r) == Some(&1)
            && first_seen.get(&r) == Some(&true)
            && used.contains(&r)
            && runs_every_iteration(code, s, e, ip)
            && (!read_outside.contains(&r) || runs_before_forward_exit(ip))
        {
            hoist_ips.push(ip);
            hoisted.insert(r);
            gated_hoists = true;
        }
    }
    // ── glob-range const rematerialization ── same gate as the narrowing /
    // splitting mechanism at the allocation below (GPR-emitter-only plans;
    // `ZIPP_NO_GLOB_RANGE=1` restores the pass above byte-for-byte). A
    // single-def LoadInt/LoadConst register that is def-first, actually used,
    // and never read outside the region hoists WITHOUT the
    // runs-every-iteration proof: the GPR emitter reads it as an immediate
    // everywhere and its flush writes the compile-time-boxed const — which IS
    // the def's only possible value, so a flush on a path that skipped the
    // def writes the same bits the def would have, and `!read_outside`
    // empties the one observer class left (a post-region read of a
    // pre-region value). The pass above must refuse these (a def inside an
    // inner loop provably does not run on every pass of an ENCLOSING region,
    // and without the const-identity argument the flush would be wrong);
    // each then pins a real home — the swizzle OUTER region carries three
    // such consts (24, 255, 2) and misses the GPR pool by exactly one home.
    // Not conditioned on `allow_hoist`: these consume no home at all in the
    // segmented allocation (one shared unmapped index), so the RetryNoHoist
    // pressure-release has nothing to reclaim from them.
    // ── glob-range: registers PROVABLY DEAD OUTSIDE the region despite being
    // in `read_outside` ── a top-level proto recycles registers across phases,
    // so `read_outside` (any textual use outside [s, e]) marks nearly every
    // swizzle temp, and B97 then write-throughs each def BOXED — ~14 boxing
    // sequences per iteration on the swizzle nest, measured at ~5ns/iter, the
    // whole prize. A register is proven dead-outside when
    //   (a) control is CONFINED: no op after `e` jumps to `target <= e` and
    //       no in-region op jumps backward out (`target < s`) — once the
    //       region is left FORWARD, neither it nor anything before it can
    //       run again, so uses before `s` are unreachable from any exit; and
    //   (b) every use after `e` has a nearest preceding def ALSO after `e`,
    //       with no jump target anywhere in the proto landing strictly
    //       between them (the `slot_guard_key` straight-line rule — entering
    //       AT the def is fine) — every post-exit path rewrites the register
    //       before reading it.
    // The use model is over-approximated: anything outside `modeled` below
    // counts as a use of EVERYTHING, and then needs a dominating def like any
    // other use. `modeled` is now a deliberately NARROW subset of what
    // `instr_uses` covers — that table became exhaustive in W17, so it models
    // every op except the closure-capture reads it cannot name; widening
    // `modeled` to match would enlarge `outside_dead` and is a PERF change for
    // the home-allocation lane to make and measure, not a correctness one. The
    // def model under-approximates (`writes_reg`), which only shortens gaps'
    // candidates — a missed def means an earlier one is found and the gap
    // grows, strictly more conservative. Members lose their B97 write-through
    // (a stale flush into their slot is unobservable — in-region every read
    // is dominated by a def per the segment/shareable rules, outside by (b)),
    // may share homes on the nest paths, and qualify for const hoisting.
    let mut outside_dead: FxHashSet<u16> = FxHashSet::default();
    if (admit_dv || share_homes) && cold.is_empty() && glob_range_enabled() {
        let target_of = |i: &Instr| -> Option<usize> {
            match *i {
                Instr::Jump { target }
                | Instr::JumpIfFalse { target, .. }
                | Instr::JumpIfTrue { target, .. }
                | Instr::JumpIfNotLt { target, .. }
                | Instr::JumpIfNotLe { target, .. }
                | Instr::PushFinally { target, .. }
                | Instr::JumpFinally { target, .. } => Some(target as usize),
                Instr::PushHandler { catch_target, .. } => Some(catch_target as usize),
                _ => None,
            }
        };
        let confined = code.iter().enumerate().all(|(ip, i2)| match target_of(i2) {
            Some(t) => !(ip > e && t <= e) && !((s..=e).contains(&ip) && t < s),
            None => true,
        });
        // The subset of ops this proof trusts `instr_uses` for. It is no longer
        // "everything `instr_uses` has an arm for" — that is now every variant
        // — but the list this analysis was developed and measured against.
        // Anything outside it is a universal use. Adding to it is a perf change
        // (a bigger `outside_dead`), not a correctness one; leaving it narrow is
        // always sound.
        let modeled = |i: &Instr| -> bool {
            matches!(
                *i,
                Instr::Move { .. }
                    | Instr::StoreGlobal { .. }
                    | Instr::StoreGlobalStrict { .. }
                    | Instr::StoreGlobalResolved { .. }
                    | Instr::AddInt { .. }
                    | Instr::Neg { .. }
                    | Instr::Add { .. }
                    | Instr::Sub { .. }
                    | Instr::Mul { .. }
                    | Instr::Div { .. }
                    | Instr::Mod { .. }
                    | Instr::StrConcat { .. }
                    | Instr::StrAppendInPlace { .. }
                    | Instr::StrAppendIndex { .. }
                    | Instr::AddRightPair { .. }
                    | Instr::Pad2Concat { .. }
                    | Instr::Pad2Conditional { .. }
                    | Instr::StrConcatChain { .. }
                    | Instr::Bitwise { .. }
                    | Instr::Lt { .. }
                    | Instr::Le { .. }
                    | Instr::Gt { .. }
                    | Instr::Ge { .. }
                    | Instr::Eq { .. }
                    | Instr::Ne { .. }
                    | Instr::JumpIfNotLt { .. }
                    | Instr::JumpIfNotLe { .. }
                    | Instr::JumpIfFalse { .. }
                    | Instr::JumpIfTrue { .. }
                    | Instr::GetProp { .. }
                    | Instr::SetProp { .. }
                    | Instr::GetIndex { .. }
                    | Instr::SetIndex { .. }
                    | Instr::GetIndexConcat { .. }
                    | Instr::SetIndexConcat { .. }
                    | Instr::ToPropKey { .. }
                    | Instr::DeleteIndexConcat { .. }
                    | Instr::Return { .. }
                    | Instr::MathOp { .. }
                    | Instr::CallMethod { .. }
                    | Instr::CallWithThis { .. }
                    | Instr::LoadInt { .. }
                    | Instr::LoadConst { .. }
                    | Instr::LoadBool { .. }
                    | Instr::LoadNull { .. }
                    | Instr::LoadUndefined { .. }
                    | Instr::LoadGlobal { .. }
                    | Instr::Jump { .. }
                    | Instr::Now { .. }
                    | Instr::ReturnUndefined
            )
        };
        let grdbg = std::env::var_os("ZIPP_GLOBRANGE_DEBUG").is_some();
        if !confined && grdbg {
            eprintln!("[globrange] [{s},{e}] outside-dead: NOT CONFINED");
        }
        if confined {
            let mut all_targets: Vec<usize> = code.iter().filter_map(target_of).collect();
            all_targets.sort_unstable();
            let gap_has_target = |d: usize, u: usize| -> bool {
                let lo = all_targets.partition_point(|&t| t <= d);
                lo < all_targets.len() && all_targets[lo] <= u
            };
            'ro_reg: for &r in &reg_order {
                if !read_outside.contains(&r) {
                    continue;
                }
                for (uip, i2) in code.iter().enumerate() {
                    if (s..=e).contains(&uip) || uip < s {
                        continue;
                    }
                    // Call-family windows, modeled explicitly (their reads are
                    // exactly callee/receiver + the contiguous arg window).
                    let win_use = match *i2 {
                        Instr::Call {
                            callee,
                            arg_base,
                            argc,
                            ..
                        }
                        | Instr::New {
                            callee,
                            arg_base,
                            argc,
                            ..
                        }
                        | Instr::TailCall {
                            callee,
                            arg_base,
                            argc,
                        } => r == callee || (r >= arg_base && (r - arg_base) < argc),
                        Instr::StaticFn {
                            callee,
                            this_v,
                            arg_base,
                            argc,
                            ..
                        } => r == callee || r == this_v || (r >= arg_base && (r - arg_base) < argc),
                        Instr::NewArray { arg_base, argc, .. }
                        | Instr::ArrayCtor { arg_base, argc, .. }
                        | Instr::GlobalFn { arg_base, argc, .. }
                        | Instr::Print { arg_base, argc, .. } => {
                            r >= arg_base && (r - arg_base) < argc
                        }
                        _ => false,
                    };
                    let call_family = matches!(
                        *i2,
                        Instr::Call { .. }
                            | Instr::New { .. }
                            | Instr::TailCall { .. }
                            | Instr::NewArray { .. }
                            | Instr::ArrayCtor { .. }
                            | Instr::GlobalFn { .. }
                            | Instr::StaticFn { .. }
                            | Instr::Print { .. }
                    );
                    if call_family {
                        if !win_use {
                            continue;
                        }
                    } else if modeled(i2) && !instr_uses(i2).contains(&r) {
                        continue;
                    }
                    // Def model: `writes_reg` plus the dst-writing ops it does
                    // not cover (each fully overwrites its dst). A def this
                    // still misses only lengthens the gap — conservative.
                    let defines = |j: usize| -> bool {
                        if writes_reg(&code[j]) == Some(r) {
                            return true;
                        }
                        matches!(code[j],
                            Instr::Now { dst, .. }
                            | Instr::Call { dst, .. }
                            | Instr::New { dst, .. }
                            | Instr::NewArray { dst, .. }
                            | Instr::ArrayCtor { dst, .. }
                            | Instr::GlobalFn { dst, .. }
                            | Instr::StaticFn { dst, .. } if dst == r)
                    };
                    let Some(d) = ((e + 1)..uip).rev().find(|&j| defines(j)) else {
                        if grdbg {
                            eprintln!("[globrange] [{s},{e}] r{r} not dead-out: no post-region def before use @{uip} ({:?})", i2);
                        }
                        continue 'ro_reg;
                    };
                    if gap_has_target(d, uip) {
                        if grdbg {
                            eprintln!(
                                "[globrange] [{s},{e}] r{r} not dead-out: target in ({d},{uip}]"
                            );
                        }
                        continue 'ro_reg;
                    }
                }
                outside_dead.insert(r);
            }
        }
    }

    let mut remat_regs: FxHashSet<u16> = FxHashSet::default();
    let mut slot_consts: FxHashMap<u16, i32> = FxHashMap::default();
    if (admit_dv || share_homes) && cold.is_empty() && glob_range_enabled() {
        for (&r, &ip) in &const_def_ip {
            if def_count.get(&r) != Some(&1)
                || first_seen.get(&r) != Some(&true)
                || !used.contains(&r)
                || hoisted.contains(&r)
            {
                continue;
            }
            // BOTH forms replace every in-region read with the immediate, so
            // both need the def to DOMINATE every in-region use: a use
            // reachable without the def (a jump target strictly inside
            // (def, use], or a hidden window use ahead of the def) would read
            // the immediate natively where pure interpretation reads an older
            // slot value — a silent tier divergence, not a deopt. (The
            // classic pass's `runs_every_iteration` subsumes this; dropping
            // it is exactly what these forms do.)
            //
            // W18 audit: this scan IS a real dominance proof (it fails closed
            // on any target that could enter the window), which is why the
            // conditional-def defect never reached it — but it is a THIRD
            // hand-written one beside `runs_every_iteration` and `live_in`.
            // For a single-def register the exact statement is `!live_in(r)`,
            // and this scan is a conservative approximation of it. Folding the
            // two would REMAT MORE (the scan refuses windows the fixpoint
            // proves safe), so it is a perf change to measure, not a
            // correctness one to slip in here — and it needs `region_liveness`
            // moved above this block. Do not hand-roll a FOURTH.
            let mut last_use = ip;
            let mut use_before_def = false;
            let note_use = |uip: usize, last_use: &mut usize, before: &mut bool| {
                *last_use = (*last_use).max(uip);
                *before |= uip < ip;
            };
            for (off, instr) in code[s..=e].iter().enumerate() {
                if numeric_uses(s + off, instr).contains(&r) {
                    note_use(s + off, &mut last_use, &mut use_before_def);
                }
            }
            for &(uip, ur, is_def) in &str_imul_touch {
                if !is_def && ur == r {
                    note_use(uip, &mut last_use, &mut use_before_def);
                }
            }
            if use_before_def || jump_targets.iter().any(|&t| t > ip && t <= last_use) {
                continue;
            }
            if !read_outside.contains(&r) || outside_dead.contains(&r) {
                hoist_ips.push(ip);
                hoisted.insert(r);
                remat_regs.insert(r);
                continue;
            }
            // Read outside without the dead-outside proof: hoisting is out
            // (the flush would write the const over a pre-region value some
            // later code still reads), but the SLOT-MATERIALIZED form is
            // interpreter-exact on every path (see `RegionPlan::slot_consts`):
            // uses read the immediate, the def stores the boxed const to the
            // frame slot exactly when the interpreter's def would, nothing is
            // flushed.
            let v = match code[ip] {
                Instr::LoadInt { val, .. } => val,
                Instr::LoadConst { idx, .. } => match proto.constants.get(idx as usize) {
                    Some(c) if c.is_int() => c.bits() as u32 as i32,
                    _ => continue,
                },
                _ => continue,
            };
            slot_consts.insert(r, v);
        }
    }
    hoist_ips.sort_unstable();

    // ── W7 pinned-guard hoisting ── which pins can drop their per-access
    // identity compare for one entry check (see `hoistable_pins` for the full
    // predicate), and which pinned-STRING `.length` reads can then leave the
    // body entirely. A hoisted length behaves exactly like a hoisted constant
    // (prologue fill, permanent home, no entry load, body op skipped), so it
    // reuses the `hoisted` machinery under the same conditions — including
    // `allow_hoist`, so the pool-pressure retry releases its home too.
    let hoist_pins = hoistable_pins(proto, s, e, ta_plan, cold);
    let mut hoist_len_ips: Vec<usize> = Vec::new();
    if allow_hoist && !hoist_pins.is_empty() {
        for (off, instr) in code[s..=e].iter().enumerate() {
            let ip = s + off;
            let Instr::GetProp { dst, name, .. } = *instr else {
                continue;
            };
            // A captured `str.charCodeAt` also owns a STRING pin and begins
            // with GetProp. Only the exact `.length` data read may be replaced
            // by a numeric prologue fill; skipping a captured method lookup
            // would leave its callee register non-callable on deopt replay.
            if ta_plan.captured_get(ip).is_some()
                || proto
                    .string_constants
                    .get(name as usize)
                    .map(String::as_str)
                    != Some("length")
            {
                continue;
            }
            let Some(&j) = ta_plan.access.get(&ip) else {
                continue;
            };
            // STRINGS only: immutable, so length is a constant once identity
            // holds (entry-guarded — `hoist_pins` membership is the
            // precondition). A dense-Array length is also region-stable under
            // the predicate, but its GetProp dst is not typed as a def today
            // (it rides the ro_live_in path), so it fails `first_seen` below
            // and keeps its per-iteration read.
            if !hoist_pins.contains(&j) || ta_plan.pins[j as usize].kind != STR_PIN_KIND {
                continue;
            }
            if def_count.get(&dst) == Some(&1)
                && first_seen.get(&dst) == Some(&true)
                && used.contains(&dst)
                && !dead.contains(&dst)
                && runs_every_iteration(code, s, e, ip)
            {
                hoist_len_ips.push(ip);
                hoisted.insert(dst);
                gated_hoists = true;
            }
        }
        hoist_len_ips.sort_unstable();
    }

    // Exact values of single-def integer-constant regs (hoisted or not): used by
    // the analysis entry state, the Mul strength reduction and the gpr mirrors.
    let mut const_vals: FxHashMap<u16, i64> = FxHashMap::default();
    for (&r, &ip) in &const_def_ip {
        if def_count.get(&r) != Some(&1) {
            continue;
        }
        match code[ip] {
            Instr::LoadInt { val, .. } => {
                const_vals.insert(r, val as i64);
            }
            Instr::LoadConst { idx, .. } => {
                if let Some(c) = proto.constants.get(idx as usize) {
                    if c.is_int() {
                        const_vals.insert(r, (c.bits() as u32 as i32) as i64);
                    }
                }
            }
            _ => {}
        }
    }

    // ── bool ↔ global decline ── globals are always homed as NUMBERS (an xmm,
    // flushed through `emit_int_box_from_home`), while a bool-typed reg is homed
    // in a gpr. Moving a value between the two would ask `xh()` for the xmm home
    // of a gpr-homed register, which is `unreachable!` — so `var b = i < 100;` at
    // top level inside any hot loop PANICKED the engine rather than declining.
    // There is no boxing path here, so decline the region to the memory tier.
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) {
            continue;
        }
        let bool_glob = match *instr {
            Instr::StoreGlobal { src, .. }
            | Instr::StoreGlobalStrict { src, .. }
            | Instr::StoreGlobalResolved { src, .. } => src,
            Instr::LoadGlobal { dst, .. } => dst,
            _ => continue,
        };
        if ty.get(&bool_glob) == Some(&VTy::Bool) {
            decline!("bool value moved to/from a global (globals are numeric homes)");
        }
    }

    // ── home unification (copy coalescing) ── temps that only shuttle a global's
    // (or a live-in reg's) value share that value's home; the body copies vanish.
    //
    // DISABLED (soundness): an aliased reg shares another value's home, so its
    // home cannot be initialised from its own frame slot at entry — and an exit
    // taken before the alias's def then flushes the OTHER value into its slot.
    // That is a silent wrong answer, not a deopt: `for (i=0;i<8;i++) s = i;`
    // returned `i` (8) instead of 7, because the region is entered on the final
    // back-edge, runs zero body iterations, and flushes anyway. Re-enabling this
    // needs per-exit flush sets driven by a must-def dataflow (see PERF_ROADMAP).
    const UNIFY_HOMES: bool = false;
    let (glob_alias, move_alias) = if UNIFY_HOMES {
        let g =
            unify_homes_with_globals(code, s, e, &ty, &first_seen, &dead, &hoisted, &jump_targets);
        let m = unify_move_homes(
            code,
            s,
            e,
            &ty,
            &first_seen,
            &dead,
            &hoisted,
            &jump_targets,
            &g,
        );
        (g, m)
    } else {
        (FxHashMap::default(), FxHashMap::default())
    };
    // Aliased regs don't consume an xmm home of their own.
    reg_order.retain(|r| !glob_alias.contains_key(r) && !move_alias.contains_key(r));

    // ── overflow-guard elision (INT path) ── interval analysis proves which
    // arithmetic results always stay inside [-2^53, 2^53].
    let mut elide_guard: FxHashSet<usize> = FxHashSet::default();
    let mut strict_entry_globs: FxHashSet<u32> = FxHashSet::default();
    let mut mul_shift: FxHashMap<usize, (u16, u8)> = FxHashMap::default();
    // W10 (B123): a DV-retry plan (admit_bitwise && admit_dv) runs the prover
    // too, with the DV arms enabled — without this, DV regions kept every i53
    // guard AND lost r13/r14 from the GPR pool (`region_is_int` runs the
    // strict admission, which the DV CallMethods fail). `dv_prover` is None
    // for every other caller, so their elide sets stay byte-identical.
    let dv_prover = (admit_bitwise
        && admit_dv
        && !region_is_int(proto, start, end, ta_plan)
        && int_unadmitted_ips(proto, start, end, ta_plan, true).is_some_and(|v| v.is_empty()))
    .then_some(ta_plan);
    if region_is_int(proto, start, end, ta_plan) || dv_prover.is_some() {
        let mut entry = AbsState {
            regs: FxHashMap::default(),
            globs: FxHashMap::default(),
            alias: FxHashMap::default(),
            cmp: None,
        };
        // Live-ins get the interval the ENTRY GUARD ACTUALLY ENFORCES, which is
        // `IV_FULL` = [-2^53, 2^53] — not `IV_I32`.
        //
        // These were seeded IV_I32 back when `emit_int_entry_load` admitted only
        // Int-TAGGED values, whose payload is an i32 by construction. That load
        // now also admits a double holding an exact integer up to ±2^53 (so a
        // loop whose accumulator crosses 2^31 can re-enter the tier at all), and
        // the two contracts silently diverged: the analysis went on believing
        // every live-in was ≤ 2^31 and elided guards on that basis.
        //
        // It is reachable. `for (…) { out = x * 1024; x = x - 0; }` entered with
        // x = 2^53 seeds x as i32, concludes the product is at most 2^41, elides
        // the 2^53 guard AND strength-reduces the multiply to `psllq 10` — and
        // 2^53 << 10 is the i64 sign bit, i.e. -2^63 instead of +2^63.
        //
        // Eliding is harmless while the i64 arithmetic stays exact (above 2^53
        // an i64 is MORE precise than the f64 JS would produce, and exit boxing
        // rounds it to the same answer); the divergence begins where i64 itself
        // overflows. IV_FULL is what the guard promises, so it is what the
        // analysis is told.
        for (&r, &def_first) in &first_seen {
            if !def_first && ty.get(&r) == Some(&VTy::Num) {
                entry.regs.insert(r, IV_FULL);
            }
        }
        for (&g, &read_first) in &glob_first_read {
            if read_first {
                entry.globs.insert(g, IV_FULL);
            }
        }
        for &r in &hoisted {
            if let Some(&v) = const_vals.get(&r) {
                entry.regs.insert(r, (v, v)); // materialised in the prologue
            }
        }
        // W10: strict-entry candidates for the DV prover — loop-carried
        // globals (read-first AND stored in-region). A survivor's entry load
        // becomes strict-i32 (see `RegionPlan::strict_entry_globs`).
        if dv_prover.is_some() {
            for (&gi, &read_first) in &glob_first_read {
                if read_first
                    && code[s..=e].iter().enumerate().any(|(off, i2)| {
                        !cold.contains(&(s + off))
                            && matches!(*i2,
                                Instr::StoreGlobal { idx, .. }
                                | Instr::StoreGlobalStrict { idx, .. }
                                | Instr::StoreGlobalResolved { idx, .. } if idx == gi)
                    })
                {
                    strict_entry_globs.insert(gi);
                }
            }
        }
        elide_guard =
            analyze_int_guards_strict(proto, s, e, entry, dv_prover, &mut strict_entry_globs);
        // Strength-reduce a guard-elided `Mul` by a constant power of two into a
        // left shift (`psllq`), skipping the imul gpr round-trip.
        for ip in s..=e {
            if !elide_guard.contains(&ip) {
                continue;
            }
            if let Instr::Mul { a, b, .. } = code[ip] {
                let (val_reg, k) = match (const_vals.get(&a), const_vals.get(&b)) {
                    (_, Some(&k)) => (a, k),
                    (Some(&k), _) => (b, k),
                    _ => continue,
                };
                if k >= 2 && (k as u64).is_power_of_two() {
                    mul_shift.insert(ip, (val_reg, (k as u64).trailing_zeros() as u8));
                }
            }
        }
    }

    // Per-register live range [first_ip, last_ip] within the region (for linear-
    // scan reuse). A live-in reg (used before defined) is loop-carried, so its
    // value spans the whole region [s, e]; otherwise it lives from its first
    // appearance to its last. Globals are loop-carried (whole region).
    let mut first_ip: FxHashMap<u16, usize> = FxHashMap::default();
    let mut last_ip: FxHashMap<u16, usize> = FxHashMap::default();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) {
            continue;
        }
        let ip = s + off;
        let mut touch = |r: u16| {
            first_ip.entry(r).or_insert(ip);
            last_ip.insert(r, ip);
        };
        for u in numeric_uses(ip, instr) {
            touch(u);
        }
        if let Some(d) = numeric_def(ip, instr) {
            touch(d);
        }
    }
    // Extend the live ranges for the charCodeAt-index / Imul-operand uses and the
    // charCodeAt/Imul result defs that instr_uses/writes_reg miss (S5 MAJOR): the
    // home-reuse allocator (active when n_numeric > POOL) would otherwise free/
    // reuse these homes at the wrong ip and clobber them. (fnv1a has few regs so
    // reuse is off and this is inert there, but it is required for a larger
    // STR-pinned region to be sound.)
    for &(ip, r, _is_def) in &str_imul_touch {
        first_ip.entry(r).or_insert(ip);
        let e = last_ip.entry(r).or_insert(ip);
        if ip > *e {
            *e = ip;
        }
    }
    // ── control-flow closure of the mention windows ── the two loops above record
    // where a register is MENTIONED, which is its live range only in straight-line
    // code. A region is a loop body and routinely contains an INNER loop: a value
    // defined before it and read inside it stays live across the inner back-edge,
    // so the rest of the inner body runs while its home is still live, past the
    // last mention. Without this the reuse allocator handed that home to a value
    // defined later in the inner body and the read got the clobbered home on every
    // iteration but the first — a silent wrong answer (W16 defects 2 and 4, one on
    // the INT tier and one on DOUBLE). `region_liveness` computes the real live
    // range; WIDEN with it rather than replace, so an under-modelled use can never
    // make a range narrower than it already was.
    let liveness = region_liveness(
        code,
        s,
        e,
        cold,
        &str_imul_touch,
        &numeric_uses,
        &numeric_def,
    );
    for (&r, &(la, lb)) in &liveness.spans {
        if let Some(f) = first_ip.get_mut(&r) {
            *f = (*f).min(la);
        }
        if let Some(l) = last_ip.get_mut(&r) {
            *l = (*l).max(lb);
        }
    }
    // ── W28 type-split numeric interval ── a split register's NUMERIC home is
    // live over its numeric range ALONE: the bool range keeps the value in a
    // gpr of its own, and the gap between the ranges holds nothing (the two
    // ranges cover every touch of the register by construction). This
    // narrowing is what the split is FOR — it is the register-pressure relief
    // that lets a region with a recycled temp fit the pool at all — and it
    // rests on exactly the facts the range's admission proved: the range opens
    // with a def, no in-region jump target lands strictly inside it (so that
    // def dominates every touch in it), and the register is `shareable` with
    // `flush_exit` skipping it (write-through keeps its slot current), so
    // lending the home to another value before `num_lo` and after `num_hi` is
    // unobservable. Deliberately AFTER the `region_liveness` widening: the
    // narrowing is a claim the split proved, not one the hull can make.
    for (&r, sp) in &ty_splits {
        first_ip.insert(r, sp.num_lo);
        last_ip.insert(r, sp.num_hi);
    }
    // ── the ONE live-in predicate ── "is the value this register holds when the
    // region is ENTERED still observable?". Every consumer that decides whether a
    // home must be filled from the frame slot, and whether it may be reused or
    // shared, asks exactly this and nothing else.
    //
    // W18 (silent wrong answer): the consumers below used to ask `first_seen`,
    // which records only whether a register's FIRST OCCURRENCE inside the region
    // is a def. That is not dominance. In
    //
    //     var t = 2; for (i = 0; i < n; i++) { if (i === 3) { t = 7; } h = h + t; }
    //
    // `t`'s first occurrence is the `LoadInt` behind the branch, so it looked
    // def-first, became `shareable`, and dropped out of `live_in_regs` — whose
    // invariant is "every flushed home is entry-loaded". On the 39 iterations
    // that skip the branch the `Add` read an unfilled home: k(40) answered 74
    // (xmm INT) / 42 (GPR) instead of 266. The file already stated the
    // distinction one screen up, where constant hoisting guards ITS consumer
    // with `runs_every_iteration` — this states it once, for the rest.
    //
    // UNION with the old flag rather than replacement: `first_seen == false` is a
    // textual test and the fixpoint is a dataflow one, so neither strictly
    // contains the other in the presence of an under-modelled use. Taking both
    // can only ever move a register from "shareable" to "permanent + entry
    // loaded", never the other way — the direction that is never wrong.
    let entry_live = liveness.entry_live;
    let live_in =
        |r: u16| -> bool { first_seen.get(&r) == Some(&false) || entry_live.contains(&r) };
    let range = |r: u16| -> (usize, usize) {
        // Whole-region (permanent home) if loop-carried (live-in, used before
        // defined) OR a HOISTED constant — hoisted values are materialised once
        // in the prologue and read every iteration, so their home must never be
        // freed/reused mid-region (doing so clobbered them — a real bug).
        // A register READ OUTSIDE the region also keeps a permanent home. Sharing
        // one is only sound when clobbering the loser's frame slot is invisible,
        // and that is exactly the condition: `flush_exit` writes the shared home
        // to EVERY sharer's slot, so a sharer whose value still matters after the
        // region would come back holding an unrelated temp. Region-local liveness
        // does not imply the register is dead in the enclosing function — that is
        // what made blanket reuse unsound. Same `read_outside` set the dead-code
        // pass uses.
        // B97: `read_outside` dropped from this list — those registers now share
        // via write-through instead of pinning a permanent home.
        if live_in(r)
            || hoisted.contains(&r)
            || (!(admit_wt_share && wt_share_enabled())
                && read_outside.contains(&r)
                && !outside_dead.contains(&r))
        {
            (s, e)
        } else {
            (first_ip[&r], last_ip[&r])
        }
    };
    // Registers eligible to SHARE an xmm: not live-in, not a hoisted
    // constant, and not read after the region. They need no entry load either —
    // an early exit flushing a stale value into their slot is unobservable, and
    // NOT being live-in is precisely what says no in-region read can reach the
    // home before a def has filled it.
    // B97: `read_outside` no longer disqualifies. Such a register may share a home
    // provided it is WRITTEN THROUGH at every def and skipped by `flush_exit` (see
    // `RegionPlan::write_through`) — that is what makes clobbering the home
    // invisible in its frame slot, which is the property the old rule bought by
    // refusing to share at all.
    let shareable = |r: u16| -> bool {
        !live_in(r)
            && !hoisted.contains(&r)
            && ((admit_wt_share && wt_share_enabled())
                || !read_outside.contains(&r)
                || outside_dead.contains(&r))
    };

    // ── W28 type-split: the conditions that need liveness ──
    // `plan_type_splits` proved the two ranges cannot interfere structurally.
    // The rest of the predicate needs analyses that did not exist when it ran,
    // and every one of them FAILS CLOSED: a candidate that does not clear all
    // of them declines the region with the pre-wave reason, exactly as if the
    // split had never been considered.
    //
    //   * `!live_in(r)` — the W18 dominance predicate. A value observable at
    //     region entry could be read before either range's opening def, and
    //     that read would be under the wrong type. It is also what makes the
    //     register's frame slot safe to leave alone until its first def.
    //   * `shareable(r)` — the same bar the numeric reuse path applies. On the
    //     write-through callers this is `!live_in && !hoisted`, so a
    //     read-after-region split is admitted precisely because its slot is
    //     kept current by write-through (below).
    //   * not hoisted / dead / remat / slot-const / home-unified — each of
    //     those gives the register a different home story (an immediate, no
    //     home at all, a shared global home) that the two-home model does not
    //     express.
    //   * `str_imul_touch` — the extra live-range mentions `instr_uses` and
    //     `writes_reg` are blind to. Every one for a split register must land
    //     inside the matching range; otherwise the pre-pass's touch walk was
    //     incomplete for it and its ranges are not trustworthy.
    let mut ty_splits = ty_splits;
    if !ty_splits.is_empty() {
        let mut keys: Vec<u16> = ty_splits.keys().copied().collect();
        keys.sort_unstable();
        for r in keys {
            let sp = ty_splits[&r];
            let in_range = |ip: usize| {
                (ip >= sp.bool_lo && ip <= sp.bool_hi) || (ip >= sp.num_lo && ip <= sp.num_hi)
            };
            let touch_ok = str_imul_touch.iter().all(|&(ip, rr, is_def)| {
                rr != r || (in_range(ip) && (!is_def || (ip >= sp.num_lo && ip <= sp.num_hi)))
            });
            if !live_in(r)
                && shareable(r)
                && !hoisted.contains(&r)
                && !dead.contains(&r)
                && !remat_regs.contains(&r)
                && !slot_consts.contains_key(&r)
                && !glob_alias.contains_key(&r)
                && !move_alias.contains_key(&r)
                && !split_recvs.contains(&r)
                && ty.get(&r) == Some(&VTy::Num)
                && touch_ok
            {
                continue;
            }
            decline!("type conflict on a reused register");
        }
        // MEMORY IS AUTHORITATIVE for a split register — see
        // `RegionPlan::ty_splits`. Every numeric def boxes into
        // `[rbx + dreg(r)]` through the standard `write_through` hook, every
        // Bool def stores `BOOL_TAG | gpr` there through the emitter's split
        // arm, and `flush_exit` skips the register on both loops. This is the
        // ONLY way an exit can be correct without knowing which of the two
        // ranges is live at it — including an exit in the GAP between them.
        for &r in ty_splits.keys() {
            write_through.insert(r);
        }
    }

    // ── B97 write-through set ── every read-after-region register that `shareable`
    // now admits. This MUST be populated for BOTH allocation branches, not just the
    // reuse one: making a register shareable also drops it from `live_in_regs` (see
    // there), so even with a private home it starts as GARBAGE, and a `flush_exit`
    // that wrote that home would corrupt its slot. That is not hypothetical — it is
    // exactly what `hoisted_const_on_untaken_branch` catches:
    //
    //   let c = 3; for (…) { if (i > 1e9) { c = 7; … } }  return c;
    //
    // `c`'s only in-region def never executes, so nothing ever fills its home; with
    // the flush skipped, its slot correctly keeps the 3 the frame already held.
    for &r in &reg_order {
        // A slot-materialized const is NOT written through in the B97 sense:
        // its defining op already stores the boxed const to the slot, it has
        // no home to write from, and the flush skips it by construction.
        if admit_wt_share
            && read_outside.contains(&r)
            && shareable(r)
            && !slot_consts.contains_key(&r)
            && !outside_dead.contains(&r)
        {
            // A B94 split receiver already has this exact property from its own
            // mechanism, with its own ip-class exception (the receiver
            // `LoadGlobal` stores the OBJECT and must not write the home
            // through). B97 membership adds nothing for it and only creates the
            // ip where the two mechanisms disagree — every consumer tests the
            // two sets with OR, so emptying the overlap changes nothing else.
            if split_recvs.contains(&r) {
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!(
                        "[jit] region [{s},{e}] B97 write-through excludes B94 split receiver r{r}"
                    );
                }
                continue;
            }
            write_through.insert(r);
        }
    }

    // The xmm home pool size. If one-home-per-numeric-value fits, use the simple
    // allocation (distinct home each — best ILP, what loop.js relies on). Only
    // when it would OVERFLOW do we linear-scan-reuse homes for non-overlapping
    // live ranges (lets bigger loops JIT, and is required for object SROA).
    let pool = (home_last - HOME_XMM_FIRST + 1) as usize;
    let n_numeric = reg_order.iter().filter(|r| ty[r] == VTy::Num).count() + glob_order.len();
    // DISABLED (soundness): linear-scan reuse gives two registers with
    // non-overlapping IN-REGION live ranges the same home, but `flush_exit`
    // writes that home to BOTH frame slots, so the sharer whose range already
    // ended is overwritten with an unrelated temp's value. Region-local liveness
    // does not imply the register is dead in the ENCLOSING FUNCTION — a loop
    // that assigns five locals early and then runs a long chain returned the
    // chain's temps in place of the locals. Reuse also defeats the entry-load
    // fix: `live_in_regs` then holds several `(reg, xmm)` pairs sharing one
    // `xmm`, so the prologue loads overwrite each other and only the last one
    // survives. Needs the same must-def / live-out analysis as UNIFY_HOMES; the
    // regions affected (>POOL numeric values) fall back to the memory tier.
    // Re-enabled, but only for `shareable` registers (see `range` above). The
    // blanket version was unsound: it shared homes on REGION-LOCAL liveness, so a
    // local assigned early in a big loop came back holding a later temp. Scoping
    // it to registers that are provably never read after the region removes the
    // failure mode and keeps what reuse is for — letting a loop with more live
    // numeric values than the 14-home pool reach the register tiers at all,
    // instead of declining to the memory path.
    // `share_homes` (B119) forces reuse below the overflow point: the caller is
    // refitting this region to the GPR emitter's much smaller pool, where the
    // ILP cost of sharing is not the trade — staying on xmm homes is.
    let reuse = share_homes || n_numeric > pool;

    // ── stored-global live-range narrowing + mixed-role temp splitting ──
    // B96 permanence forces every `glob_order` member onto a whole-region
    // [s, e] interval, and the bytecode compiler's register recycling welds a
    // temp's disjoint def-ranges into one wide interval — on the DV swizzle
    // nest the two together hold 13-14 homes against the 7-9 GPR pool while
    // the equivalent function-local form plans 8.
    //
    // NARROWING: a stored global whose every in-region load is DOMINATED by an
    // in-region store (nearest preceding store, no jump target in the gap —
    // the `slot_guard_key` straight-line scan, region-internal targets only:
    // native code is entered at `s` alone, so region-internal branches are the
    // complete native control flow) gets a [first touch, last touch] interval.
    // The GPR emitter then writes each store THROUGH to `[r12 + 8*slot]`,
    // skips the slot in flush_exit and drops the entry load (see
    // `RegionPlan::narrow_globs`): memory holds the last-stored value at every
    // exit, so a mid-iteration exit can never expose a stale slot, and reads
    // of the slot outside the region (any function, `globalThis`) see exactly
    // what the interpreter would have written. A load not provably dominated
    // keeps the permanent home — fail closed (this region's loop-carried
    // globals stay permanent: their header load's gap spans the back-edge
    // target).
    //
    // SPLITTING: a shareable temp whose touches fall into disjoint def-ranges
    // becomes one interval PER RANGE — all ranges bound to the ONE home
    // `reg_home` names — provided no jump target lands strictly inside a range
    // (entering AT the def is fine; mid-range entry could carry a value across
    // a hole). Each range starts with its own def, so a hole's home can be
    // lent to another value and whatever flush_exit writes into the temp's
    // slot from a hole is rewritten by a def before any read — the same
    // argument the shareable single-interval reuse already stands on.
    //
    // Both run only for plans the GPR emitter alone consumes: `admit_dv`
    // routes exclusively there (B119 fallback contract) and every
    // `share_homes` call site feeds `compile_region_int_gpr` only — the xmm
    // emitters never see a narrowed plan. `ZIPP_NO_GLOB_RANGE=1` restores the
    // forced intervals byte-for-byte.
    let glob_range = (admit_dv || share_homes) && reuse && cold.is_empty() && glob_range_enabled();
    let mut narrow_globs: FxHashSet<u32> = FxHashSet::default();
    let mut glob_touch: FxHashMap<u32, (usize, usize)> = FxHashMap::default();
    let mut seg_map: FxHashMap<u16, Vec<(usize, usize)>> = FxHashMap::default();
    if glob_range {
        let is_store_of = |ip: usize, gi: u32| -> bool {
            matches!(code[ip],
                Instr::StoreGlobal { idx, .. }
                | Instr::StoreGlobalStrict { idx, .. }
                | Instr::StoreGlobalResolved { idx, .. } if idx == gi)
        };
        'glob: for &gi in &glob_order {
            let mut first: Option<usize> = None;
            let mut last = s;
            // Raw scan — includes receiver-excluded LoadGlobals of the same
            // slot, which face the same dominance bar (they read the slot's
            // MEMORY, which write-through keeps current, but holding them to
            // the proof costs nothing and stays fail-closed).
            for ip in s..=e {
                let load = matches!(code[ip], Instr::LoadGlobal { idx, .. } if idx == gi);
                if !load && !is_store_of(ip, gi) {
                    continue;
                }
                if first.is_none() {
                    first = Some(ip);
                }
                last = ip;
                if load {
                    let Some(d0) = (s..ip).rev().find(|&j| is_store_of(j, gi)) else {
                        continue 'glob;
                    };
                    if jump_targets.iter().any(|&t| t > d0 && t <= ip) {
                        continue 'glob;
                    }
                }
            }
            if let Some(f0) = first {
                narrow_globs.insert(gi);
                glob_touch.insert(gi, (f0, last));
            }
        }
        // A narrowed global's first touch is a store (a read-first one fails
        // the dominance scan), so it can never carry the strict-entry
        // contract, whose license is the entry load this plan drops.
        debug_assert!(narrow_globs.is_disjoint(&strict_entry_globs));

        // Touches per register, from the same sources as `first_ip`/`last_ip`
        // (instr_uses/writes_reg plus the str_imul/DV-fuse extensions — `cold`
        // is empty under `glob_range`, so no cold filter is needed).
        let mut touch: FxHashMap<u16, Vec<(usize, bool)>> = FxHashMap::default();
        for (off, instr) in code[s..=e].iter().enumerate() {
            let ip = s + off;
            // An ELIDED Eq's dst is a phantom on both sides of the fuse: the
            // elided def writes no home, and the fused call's arg-window
            // mention reads none (the arm computes the flag from the Eq's
            // OPERAND homes; its deopt resumes AT the Eq, whose interpreted
            // re-execution rewrites the dst slot before the call reads it).
            // Recording either would reserve a home across the fuse window
            // for a value that never materializes — one whole home on the
            // swizzle regions.
            let elided_dst = dv_flag_elide
                .contains(&ip)
                .then(|| writes_reg(instr))
                .flatten()
                .or_else(|| {
                    (dv_flag_fuse.contains_key(&ip) && ip > s && dv_flag_elide.contains(&(ip - 1)))
                        .then(|| writes_reg(&code[ip - 1]))
                        .flatten()
                });
            for u in numeric_uses(ip, instr) {
                if dv_flag_fuse.contains_key(&ip) && elided_dst == Some(u) {
                    continue;
                }
                touch.entry(u).or_default().push((ip, false));
            }
            // A B94 receiver LoadGlobal writes the boxed receiver to the frame,
            // not the unrelated numeric half held in this register's home. It
            // therefore cannot open or extend a numeric segment. Every real
            // numeric def is write-through, so lending the home across these
            // receiver-only windows preserves the split-receiver exit contract.
            if !split_recv_lg.contains(&ip) {
                if let Some(d) = numeric_def(ip, instr) {
                    if dv_flag_elide.contains(&ip) {
                        continue;
                    }
                    touch.entry(d).or_default().push((ip, true));
                }
            }
        }
        for &(ip, r, is_def) in &str_imul_touch {
            touch.entry(r).or_default().push((ip, is_def));
        }
        for &r in &reg_order {
            // Split receivers are eligible here: their numeric defs are
            // write-through and receiver LoadGlobals were deliberately omitted
            // above, so each numeric segment retains the existing
            // memory-authoritative contract while its home can be lent during a
            // receiver-only hole. Slot-materialized consts carry no home at all;
            // permanent values keep [s, e] via `range` below.
            if ty[&r] != VTy::Num
                || !shareable(r)
                || slot_consts.contains_key(&r)
                // W28: a type-split register's numeric home must span its whole
                // mention window. Its BOOL range sits inside that window and
                // reserves nothing here, so a second segmentation on top would
                // hand the numeric home out across a hole this pass cannot see.
                || ty_splits.contains_key(&r)
            {
                continue;
            }
            let Some(tv) = touch.get_mut(&r) else {
                continue;
            };
            tv.sort_unstable();
            let mut segs: Vec<(usize, usize)> = Vec::new();
            let mut cur: Option<(usize, usize)> = None;
            let mut ok = true;
            let mut j = 0;
            while j < tv.len() {
                let ip = tv[j].0;
                let mut has_use = false;
                let mut has_def = false;
                while j < tv.len() && tv[j].0 == ip {
                    if tv[j].1 {
                        has_def = true;
                    } else {
                        has_use = true;
                    }
                    j += 1;
                }
                // A use extends the open range (a use-and-def op reads the old
                // value and refills the home at one ip — the range continues
                // through it); a bare def closes it and opens the next.
                match cur.as_mut() {
                    Some(c) if has_use => c.1 = ip,
                    None if has_use => {
                        // A hidden-use-first shape (e.g. a str_imul touch ahead
                        // of every def): not the def-first temp this is for.
                        ok = false;
                        break;
                    }
                    _ => {
                        debug_assert!(has_def);
                        if let Some(c) = cur.take() {
                            segs.push(c);
                        }
                        cur = Some((ip, ip));
                    }
                }
            }
            if !ok {
                continue;
            }
            if let Some(c) = cur.take() {
                segs.push(c);
            }
            // A split receiver may have only one numeric def-range: keeping
            // that single precise segment is still material because the raw
            // first/last range also includes its receiver-only LoadGlobal.
            if (segs.len() < 2 && !split_recvs.contains(&r))
                || segs
                    .iter()
                    .any(|&(a, b)| jump_targets.iter().any(|&t| t > a && t <= b))
            {
                continue;
            }
            // Bounds only — the phantom-drop above may trim an endpoint
            // relative to the raw first_ip/last_ip walk.
            debug_assert!(segs
                .first()
                .is_some_and(|x| x.0 >= *first_ip.get(&r).unwrap_or(&s)));
            debug_assert!(segs
                .last()
                .is_some_and(|x| x.1 <= *last_ip.get(&r).unwrap_or(&e)));
            seg_map.insert(r, segs);
        }
    }

    // ── allocate xmm/gpr homes ──
    let mut reg_home: FxHashMap<u16, Home> = FxHashMap::default();
    let mut glob_home: FxHashMap<u32, u8> = FxHashMap::default();
    let first_free_xmm: u8;
    if reuse
        && (!narrow_globs.is_empty()
            || !seg_map.is_empty()
            || !remat_regs.is_empty()
            || !slot_consts.is_empty())
    {
        // Segmented allocation (glob-range): the same values, but a narrowed
        // global brings its real touch window, a split temp one interval per
        // def-range — all of a value's segments bound to ONE home. Hoisted
        // CONSTANTS (classic and remat) take no allocator value at all: the
        // GPR emitter reads them as immediates and never maps their home, so
        // they all share one deliberately-unmapped index assigned after the
        // real values (a hoisted pinned-STRING length is NOT a constant — its
        // home is prologue-filled and mapped, so it keeps a real value).
        let hoist_const_regs: Vec<u16> = hoist_ips
            .iter()
            .filter_map(|&hip| match code[hip] {
                Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } => Some(dst),
                _ => None,
            })
            .collect();
        let mut values: Vec<(Vec<(usize, usize)>, NumVal)> = Vec::new();
        for &r in &reg_order {
            if ty[&r] == VTy::Num {
                if hoist_const_regs.contains(&r) || slot_consts.contains_key(&r) {
                    continue;
                }
                match seg_map.get(&r) {
                    Some(sv) => values.push((sv.clone(), NumVal::Reg(r))),
                    None => {
                        let (a, b) = range(r);
                        values.push((vec![(a, b)], NumVal::Reg(r)));
                    }
                }
            }
        }
        for &gi in &glob_order {
            let iv = match glob_touch.get(&gi) {
                Some(&t) if narrow_globs.contains(&gi) => t,
                _ => (s, e),
            };
            values.push((vec![iv], NumVal::Glob(gi)));
        }
        values.sort_by_key(|v| v.0[0].0);
        if std::env::var_os("ZIPP_GLOBRANGE_DEBUG").is_some() {
            for (segs, v) in &values {
                let name = match *v {
                    NumVal::Reg(r) => format!("r{r}"),
                    NumVal::Glob(g2) => format!("g{g2}"),
                };
                eprintln!("[globrange] [{s},{e}] {name}: {segs:?}");
            }
        }
        let mut assigned = if home_last == HOME_XMM_LAST {
            alloc_value_homes(&values)
        } else {
            alloc_value_homes_with_last(&values, home_last)
        };
        if let Some(h) = &assigned {
            // The shared hoist-const index must stay inside the xmm range —
            // out of it, fail exactly like an exhausted pool.
            let top = h.iter().copied().max().map_or(HOME_XMM_FIRST, |m| m + 1);
            if !hoist_const_regs.is_empty() && top > home_last {
                assigned = None;
            }
        }
        match assigned {
            Some(homes) => {
                let mut top = HOME_XMM_FIRST;
                for ((_, v), &x) in values.iter().zip(&homes) {
                    top = top.max(x + 1);
                    match *v {
                        NumVal::Reg(r) => {
                            reg_home.insert(r, Home::Xmm(x));
                        }
                        NumVal::Glob(gi) => {
                            glob_home.insert(gi, x);
                        }
                    }
                }
                if !hoist_const_regs.is_empty() {
                    for &r in &hoist_const_regs {
                        reg_home.insert(r, Home::Xmm(top));
                    }
                    top += 1;
                }
                first_free_xmm = top;
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    let mut ng: Vec<u32> = narrow_globs.iter().copied().collect();
                    ng.sort_unstable();
                    eprintln!(
                        "[jit] region [{s},{e}] glob-range plan: narrowed={ng:?} split-temps={} remat={} slotc={} dead-out={} homes={}",
                        seg_map.len(),
                        remat_regs.len(),
                        slot_consts.len(),
                        outside_dead.len(),
                        top - HOME_XMM_FIRST
                    );
                }
            }
            None => {
                // Same decline shape as an XmmAlloc exhaustion below. The
                // retry is worth asking for only when a hoist THIS FLAG gates
                // is holding a home: hoisted constants share one unmapped
                // index here, so releasing the glob-range remat set (which
                // ignores `allow_hoist`) changes nothing — the re-plan would
                // be bit-identical and land right back on this arm, with the
                // `[decline-reason]` line then describing the retry's
                // hoist-free plan instead of the real one.
                if allow_hoist && gated_hoists && dv_double_enabled() {
                    return PlanOutcome::RetryNoHoist;
                }
                if std::env::var_os("ZIPP_JITDECLINE").is_some() {
                    let nregs = reg_order.iter().filter(|r| ty[r] == VTy::Num).count();
                    let perm = reg_order
                        .iter()
                        .filter(|r| ty[r] == VTy::Num && !shareable(**r))
                        .count();
                    let ro = reg_order
                        .iter()
                        .filter(|r| ty[r] == VTy::Num && read_outside.contains(r))
                        .count();
                    let li = reg_order
                        .iter()
                        .filter(|r| ty[r] == VTy::Num && first_seen.get(r) == Some(&false))
                        .count();
                    let ho = reg_order
                        .iter()
                        .filter(|r| ty[r] == VTy::Num && hoisted.contains(r))
                        .count();
                    eprintln!("[pool] region [{s},{e}] numeric regs={nregs} globals={} | PERMANENT={perm} (read_outside={ro} live_in={li} hoisted={ho}) shareable={}",
                        glob_order.len(), nregs - perm);
                }
                decline!("xmm pool exhausted even with home reuse")
            }
        }
    } else if reuse {
        // Linear-scan: numeric values (regs + globals) by ascending range start,
        // reusing a home once a value's range ends. Loop-carried values (globals
        // and live-in regs) span [s, e] and so keep a permanent home.
        let mut intervals: Vec<(usize, usize, NumVal)> = Vec::new();
        for &r in &reg_order {
            if ty[&r] == VTy::Num {
                let (a, b) = range(r);
                intervals.push((a, b, NumVal::Reg(r)));
            }
        }
        for &gi in &glob_order {
            intervals.push((s, e, NumVal::Glob(gi)));
        }
        intervals.sort_by_key(|&(a, _, _)| a);
        let mut alloc = if home_last == HOME_XMM_LAST {
            XmmAlloc::new()
        } else {
            XmmAlloc::with_last(home_last)
        };
        for (a, b, v) in intervals {
            let x = match alloc.alloc(a, b) {
                Some(x) => x,
                None => {
                    // Hoisted constants pinned permanent homes; give them back
                    // and try once more before declining the whole region.
                    // Gated on the same switch as the DV admission so
                    // `ZIPP_NO_DV_DOUBLE=1` restores the pre-wave planner
                    // byte-for-byte on EVERY region, not just the DV ones.
                    if allow_hoist && !hoisted.is_empty() && dv_double_enabled() {
                        return PlanOutcome::RetryNoHoist;
                    }
                    if std::env::var_os("ZIPP_JITDECLINE").is_some() {
                        let nregs = reg_order.iter().filter(|r| ty[r] == VTy::Num).count();
                        let perm = reg_order
                            .iter()
                            .filter(|r| ty[r] == VTy::Num && !shareable(**r))
                            .count();
                        let ro = reg_order
                            .iter()
                            .filter(|r| ty[r] == VTy::Num && read_outside.contains(r))
                            .count();
                        let li = reg_order
                            .iter()
                            .filter(|r| ty[r] == VTy::Num && first_seen.get(r) == Some(&false))
                            .count();
                        let ho = reg_order
                            .iter()
                            .filter(|r| ty[r] == VTy::Num && hoisted.contains(r))
                            .count();
                        eprintln!("[pool] region [{s},{e}] numeric regs={nregs} globals={} | PERMANENT={perm} (read_outside={ro} live_in={li} hoisted={ho}) shareable={}",
                            glob_order.len(), nregs - perm);
                    }
                    decline!("xmm pool exhausted even with home reuse")
                }
            };
            match v {
                NumVal::Reg(r) => {
                    reg_home.insert(r, Home::Xmm(x));
                }
                NumVal::Glob(gi) => {
                    glob_home.insert(gi, x);
                }
            }
        }
        first_free_xmm = alloc.next; // never-touched homes are free for constants
    } else {
        // One distinct home per numeric value (best ILP — what loop.js relies on).
        let mut next_xmm = HOME_XMM_FIRST;
        for &r in &reg_order {
            if ty[&r] == VTy::Num && !slot_consts.contains_key(&r) {
                if next_xmm > home_last {
                    decline!("xmm pool exhausted (registers)");
                }
                reg_home.insert(r, Home::Xmm(next_xmm));
                next_xmm += 1;
            }
        }
        for &gi in &glob_order {
            if next_xmm > home_last {
                decline!("xmm pool exhausted (globals)");
            }
            glob_home.insert(gi, next_xmm);
            next_xmm += 1;
        }
        first_free_xmm = next_xmm;
        // (No RetryNoHoist here: this branch runs only when one-home-per-value
        // FITS the pool — `reuse` is false — so it cannot overflow by pressure
        // that un-hoisting would relieve; the two declines above are the
        // n_numeric miscount guard, unchanged.)
    }
    // W20 M1: bool regs whose gpr home is SHARED with another bool. They must
    // be excluded from `live_in_bools` — see the note there.
    let mut bool_shared: FxHashSet<u16> = FxHashSet::default();
    // Bools (both modes): gpr homes; a live-in bool is unsupported.
    //
    // W20 M1: when the bool count OVERFLOWS the four-register pool, hand the
    // registers out by linear scan over non-overlapping live ranges instead of
    // declining the region — the same mechanism, and the same three soundness
    // predicates, the numeric path above uses over the 14-xmm pool. Below the
    // overflow point the assignment is the identical first-fit, byte for byte:
    // this is a widening of what compiles, never a re-allocation of what
    // already does (see `bool_reuse_enabled`).
    let mut next_bool = 0usize;
    // Registers whose bool home MAY BE SHARED, i.e. whose frame slot no reader
    // can observe: `flush_exit` writes a shared gpr into EVERY sharer's slot,
    // so a sharer whose range has ended comes back holding an unrelated temp.
    // Sound exactly when (a) nothing reads the register after the region and
    // (b) no path from region entry reads it before a def — (b) is the W18
    // `live_in` predicate, and it is also what lets the prologue skip the
    // per-register entry load that would otherwise overwrite a co-tenant's.
    // This is deliberately STRICTER than the numeric `shareable`: a B97
    // write-through bool is excluded, because `flush_exit`'s bool loop does not
    // skip `write_through` the way its numeric loop does.
    let bool_shareable = |r: u16| -> bool {
        !live_in(r) && (!read_outside.contains(&r) || outside_dead.contains(&r))
    };
    let n_bools = reg_order.iter().filter(|r| ty[r] == VTy::Bool).count();
    if n_bools > BOOL_GPRS.len() && bool_reuse_enabled() {
        // A live-in bool still declines: it has no entry-load path at all.
        for &r in &reg_order {
            if ty[&r] == VTy::Bool && first_seen.get(&r) == Some(&false) {
                decline!("bool live-in, or bool gpr pool exhausted");
            }
        }
        // Intervals in ascending start order; a non-shareable bool takes the
        // whole region and so keeps a private register, exactly as `range`
        // does for a numeric value that may not share.
        let mut iv: Vec<(usize, usize, u16)> = Vec::new();
        for &r in &reg_order {
            if ty[&r] == VTy::Bool {
                let (a, b) = if bool_shareable(r) {
                    (first_ip[&r], last_ip[&r])
                } else {
                    (s, e)
                };
                iv.push((a, b, r));
            }
        }
        iv.sort_by_key(|&(a, _, _)| a);
        // XmmAlloc's structure over BOOL_GPRS: expire strictly-before-start
        // intervals (an arm may read an operand home after writing its dst's,
        // so equal ips always conflict), reuse a freed register, else take the
        // next never-used one.
        let mut active: Vec<(usize, u8)> = Vec::new();
        let mut freed: Vec<u8> = Vec::new();
        for (a, b, r) in iv {
            let mut i = 0;
            while i < active.len() {
                if active[i].0 < a {
                    freed.push(active[i].1);
                    active.swap_remove(i);
                } else {
                    i += 1;
                }
            }
            let g = if let Some(g) = freed.pop() {
                g
            } else if next_bool < BOOL_GPRS.len() {
                let g = BOOL_GPRS[next_bool];
                next_bool += 1;
                g
            } else {
                decline!("bool live-in, or bool gpr pool exhausted");
            };
            active.push((b, g));
            reg_home.insert(r, Home::Gpr(g));
            if bool_shareable(r) {
                bool_shared.insert(r);
            }
        }
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!(
                "[jit] region [{s},{e}] bool reuse: {n_bools} bools -> {next_bool} gpr(s), shared={}",
                bool_shared.len()
            );
        }
    } else {
        for &r in &reg_order {
            if ty[&r] == VTy::Bool {
                if first_seen.get(&r) == Some(&false) || next_bool >= BOOL_GPRS.len() {
                    decline!("bool live-in, or bool gpr pool exhausted");
                }
                reg_home.insert(r, Home::Gpr(BOOL_GPRS[next_bool]));
                next_bool += 1;
            }
        }
    }

    // ── W28 type-split bool homes ── one DEDICATED gpr per split register,
    // taken after every real bool so a region that carries no split allocates
    // byte-identically. Deliberately NO linear-scan reuse: a split's bool half
    // is never entry-loaded and never flushed, so the whole of what keeps it
    // correct is that nothing else occupies that register while its range is
    // live — a private register states that instead of proving it. The gprs
    // taken here are excluded from the GPR emitter's numeric pool exactly like
    // `bool_regs` ones (`gpr_home_map`'s `bool_used`).
    if !ty_splits.is_empty() {
        let mut keys: Vec<u16> = ty_splits.keys().copied().collect();
        keys.sort_unstable();
        for r in keys {
            if next_bool >= BOOL_GPRS.len() {
                decline!("type split: bool gpr pool exhausted");
            }
            if let Some(sp) = ty_splits.get_mut(&r) {
                sp.gpr = BOOL_GPRS[next_bool];
            }
            next_bool += 1;
        }
    }

    // ── spare-home constants ──
    // Distinct `AddInt` immediates get a permanent xmm const home when the pool
    // has room (saves a per-iteration materialise+convert in the loop body).
    let mut addint_imm_home: FxHashMap<i32, u8> = FxHashMap::default();
    {
        let mut imms: Vec<i32> = Vec::new();
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) {
                continue;
            }
            if let Instr::AddInt { imm, .. } = *instr {
                if !imms.contains(&imm) {
                    imms.push(imm);
                }
            }
        }
        let mut next = first_free_xmm;
        for imm in imms {
            if next > HOME_XMM_LAST {
                break;
            }
            addint_imm_home.insert(imm, next);
            next += 1;
        }
    }
    // Hoisted integer constants used as compare operands get a spare bool-gpr
    // mirror so int-path compares avoid a second `movq` from the xmm home.
    let mut gpr_const: FxHashMap<u16, (u8, i64)> = FxHashMap::default();
    {
        let mut cand: Vec<u16> = Vec::new();
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) {
                continue;
            }
            let (a, b) = match *instr {
                Instr::Lt { a, b, .. }
                | Instr::Le { a, b, .. }
                | Instr::Gt { a, b, .. }
                | Instr::Ge { a, b, .. }
                | Instr::Eq { a, b, .. }
                | Instr::Ne { a, b, .. }
                | Instr::JumpIfNotLt { a, b, .. }
                | Instr::JumpIfNotLe { a, b, .. } => (a, b),
                _ => continue,
            };
            for r in [a, b] {
                if hoisted.contains(&r) && const_vals.contains_key(&r) && !cand.contains(&r) {
                    cand.push(r);
                }
            }
        }
        let mut nb = next_bool;
        for r in cand {
            if nb >= BOOL_GPRS.len() {
                break;
            }
            gpr_const.insert(r, (BOOL_GPRS[nb], const_vals[&r]));
            nb += 1;
        }
    }

    // ── apply home unification ── aliased regs share their value's home; their
    // own slots are still flushed (from the shared home) on every exit.
    for (&r, &g) in &glob_alias {
        reg_home.insert(r, Home::Xmm(glob_home[&g]));
    }
    for (&r, &src) in &move_alias {
        let h = reg_home[&src];
        reg_home.insert(r, h);
    }

    // ── derived lists from the final homes (unified for both modes) ──
    // With reuse, several regs may share an xmm; flush_exit writes the shared
    // value to each reg's slot, which is sound (non-overlapping live ranges mean
    // the dead members are never read before being redefined).
    let mut num_regs = Vec::new();
    let mut bool_regs = Vec::new();
    let mut live_in_regs = Vec::new();
    let mut live_in_bools = Vec::new();
    for &r in &reg_order {
        // A slot-materialized const has no home: nothing to flush (its def
        // stores the boxed const straight to the slot), nothing to entry-load.
        if slot_consts.contains_key(&r) {
            continue;
        }
        match reg_home[&r] {
            Home::Xmm(x) => {
                num_regs.push((r, x));
                // Every flushed home is entry-loaded, not just the read-first
                // ones — see `RegionPlan::live_in_regs`. Two exceptions:
                // `hoisted` regs (the prologue materialises their constant right
                // after this, so a load would be dead), and `shareable` regs,
                // whose slot is never read after the region — flushing a stale
                // value into it is unobservable, and they may SHARE an xmm with
                // another register, which makes a per-register entry load
                // meaningless (the loads would overwrite each other).
                // A B94 split receiver is a THIRD exception, and a load-bearing
                // one: its memory slot holds the receiver OBJECT, so an entry load
                // would hand `emit_box_to_home` a non-number and `entry_bail` on
                // every single OSR entry. Its home is filled by its own numeric
                // def, and `split_home_invalid` covers every exit before that.
                if !hoisted.contains(&r) && !shareable(r) && !split_recvs.contains(&r) {
                    live_in_regs.push((r, x));
                }
            }
            Home::Gpr(g) => {
                bool_regs.push((r, g));
                // W20 M1: a bool that SHARES its gpr is not entry-loaded. Two
                // sharers' entry loads would overwrite each other (only the
                // last would survive), and the load is meaningless anyway:
                // `bool_shareable` proved no path from entry reads the register
                // before a def. The emitters zero every bool gpr that gets no
                // entry load, so a home is 0/1 from the prologue on and
                // `flush_exit`'s `BOOL_TAG | gpr` can never manufacture a
                // non-Bool Value out of an inherited register.
                if !bool_shared.contains(&r) {
                    live_in_bools.push((r, g));
                }
            }
        }
    }
    // Home-unified regs aren't in reg_order; they're flushed from the SHARED home
    // (never live-in: unification requires the first occurrence to be a def).
    for (&r, &g) in &glob_alias {
        num_regs.push((r, glob_home[&g]));
    }
    for (&r, _) in &move_alias {
        if let Home::Xmm(x) = reg_home[&r] {
            num_regs.push((r, x));
        }
    }
    let mut globs = Vec::new();
    let mut live_in_globs = Vec::new();
    for &gi in &glob_order {
        let x = glob_home[&gi];
        globs.push((gi, x));
        // Every flushed global is entry-loaded too. A def-first global used to
        // flush an uninitialised xmm, which surfaced as a raw f64 bit pattern
        // (`g = i * 2` in an 8-trip loop printed 4626604192193053000).
        // A NARROWED global is the exception on both counts: it is neither
        // flushed (the emitter writes each store through to memory and skips
        // it at exits) nor entry-loaded (its narrowed home may be lent out
        // before its window opens; the first touch is a store, so the load
        // would be dead anyway).
        if !narrow_globs.contains(&gi) {
            live_in_globs.push((gi, x));
        }
    }

    PlanOutcome::Plan(Box::new(RegionPlan {
        reg_home,
        glob_home,
        live_in_regs,
        live_in_bools,
        live_in_globs,
        num_regs,
        bool_regs,
        globs,
        hoist_ips,
        hoisted,
        dead,
        outside_dead,
        elide_guard,
        mul_shift,
        addint_imm_home,
        gpr_const,
        jump_targets,
        ta_recv_regs,
        split_recvs,
        write_through,
        split_recv_lg,
        narrow_globs,
        slot_consts,
        dv_flag_elide,
        dv_flag_fuse,
        hoist_pins,
        hoist_len_ips,
        strict_entry_globs,
        box_regs,
        undef_dead,
        getprop_ips,
        ty_splits,
    }))
}

/// The in-region successors of `ip` — the ips native control can reach from it
/// WITHOUT leaving `[s, e]`.
///
/// A transfer whose target lies outside the region, a `Return`, and a cold ip
/// (which flushes every home and hands the exact ip back to the interpreter) all
/// contribute nothing: the native code is entered at `s` alone, so once control
/// leaves it never re-enters except through a fresh OSR entry, which re-loads
/// every live-in home from the frame. Target-bearing ops that a region never
/// admits (`PushHandler`/`PushFinally`/`JumpFinally`) are listed anyway so the
/// walk stays sound if the admission set ever widens.
fn region_succs(code: &[Instr], s: usize, e: usize, ip: usize, out: &mut Vec<usize>) {
    out.clear();
    let instr = &code[ip];
    let (target, falls_through) = match *instr {
        Instr::Jump { target } => (Some(target as usize), false),
        Instr::Return { .. } | Instr::ReturnUndefined => (None, false),
        Instr::JumpIfFalse { target, .. }
        | Instr::JumpIfTrue { target, .. }
        | Instr::JumpIfNotLt { target, .. }
        | Instr::JumpIfNotLe { target, .. }
        | Instr::PushFinally { target, .. }
        | Instr::JumpFinally { target, .. } => (Some(target as usize), true),
        Instr::PushHandler { catch_target, .. } => (Some(catch_target as usize), true),
        _ => (None, true),
    };
    if let Some(t) = target {
        if (s..=e).contains(&t) {
            out.push(t);
        }
    }
    if falls_through && ip < e {
        out.push(ip + 1);
    }
}

/// What one backward liveness walk over region `[s, e]` tells its consumers.
///
/// Both answers come from the SAME fixpoint because they are the same fact seen
/// from two sides: `spans` is where a register's value is live, `entry_live` is
/// whether the value it holds ON ENTRY is one of them. Computing them apart is
/// how W18's defect happened — the span side already modelled control flow (W16)
/// while the entry side was still reading a first-mention flag.
pub(crate) struct RegionLiveness {
    /// Control-flow-correct live span per register: `(min, max)` ip at which it
    /// is live, not merely mentioned.
    pub(crate) spans: FxHashMap<u16, (usize, usize)>,
    /// Registers LIVE AT `s` — some path from the region's single native entry
    /// reaches a USE of the register without passing a def of it first. This is
    /// the region's true live-in set, and the one predicate that answers
    /// "must this register's home be filled from its frame slot at entry?".
    pub(crate) entry_live: FxHashSet<u16>,
}

/// Control-flow-correct LIVE span of every register the region `[s, e]` touches:
/// the (min, max) ip at which the register is live, not merely mentioned — plus
/// the region's true live-in set (see [`RegionLiveness`]).
///
/// A `[first mention, last mention]` window is a live range only for
/// straight-line code, and a region is a loop body that routinely contains an
/// INNER loop. A value defined before the inner loop and read inside it is still
/// live on the inner back-edge, so the whole rest of the inner body executes
/// while its home holds that value — but its mention window closes at the read,
/// and the home-reuse allocator then hands the home to a value defined later in
/// the inner body, which clobbers it for the second and every later inner
/// iteration. That is a silent wrong answer in shipping code:
///
/// ```text
/// for (i = 0; i < n; i++) { d = 255 * -3;                    // mentions: 10
///   for (j = 0; j < 2; j++) h = (h + ((d * 1024)|0))|0; }    // …and 17
/// ```
///
/// `d`'s window `[10, 17]` looked free from ip 18 on, so the `|0` literal's
/// register took its home and every SECOND inner iteration multiplied by 0.
///
/// Standard backward liveness over `region_succs`. The use/def model is exactly
/// the one that builds the mention windows (`instr_uses`/`writes_reg` plus the
/// `extra_touch` pairs that cover the CallMethod/MathOp operands both are blind
/// to), and callers WIDEN their windows with this rather than replacing them, so
/// the result is never narrower than the pre-existing allocation — an
/// under-modelled use can only leave a range where it already was, never shrink
/// one. A missing def in the model likewise only keeps a value live longer.
///
/// `entry_live` is read off the SAME fixpoint as `live_in[s]`. It fails closed
/// three ways: an unconverged walk, and a `cold` entry ip (whose row is skipped,
/// so its `live_in` would be a vacuous empty set) both report every touched
/// register as live-in, and `extra_touch` uses are modelled here exactly as they
/// are for the spans.
///
/// `extra_touch` entries are `(ip, reg, is_def)`, matching `str_imul_touch`.
pub(crate) fn region_liveness<F>(
    code: &[Instr],
    s: usize,
    e: usize,
    cold: &FxHashSet<usize>,
    extra_touch: &[(usize, u16, bool)],
    uses_at: &F,
    defs_at: &impl Fn(usize, &Instr) -> Option<u16>,
) -> RegionLiveness
where
    F: Fn(usize, &Instr) -> Vec<u16>,
{
    let n = e - s + 1;
    let mut uses: Vec<Vec<u16>> = vec![Vec::new(); n];
    let mut defs: Vec<Vec<u16>> = vec![Vec::new(); n];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut scratch: Vec<usize> = Vec::new();
    for ip in s..=e {
        // A cold ip never runs natively — it flushes and returns this exact ip
        // to the interpreter. No touches, no in-region successors.
        if cold.contains(&ip) {
            continue;
        }
        let k = ip - s;
        uses[k].extend(uses_at(ip, &code[ip]));
        if let Some(d) = defs_at(ip, &code[ip]) {
            defs[k].push(d);
        }
        region_succs(code, s, e, ip, &mut scratch);
        succs[k].extend(scratch.iter().copied());
    }
    for &(ip, r, is_def) in extra_touch {
        if !(s..=e).contains(&ip) || cold.contains(&ip) {
            continue;
        }
        if is_def {
            &mut defs[ip - s]
        } else {
            &mut uses[ip - s]
        }
        .push(r);
    }
    let mut live_in: Vec<FxHashSet<u16>> = vec![FxHashSet::default(); n];
    let mut live_out: Vec<FxHashSet<u16>> = vec![FxHashSet::default(); n];
    // Monotone fixed point (sets only grow), walked backwards so a straight-line
    // stretch converges in one pass and each loop nesting level costs one more.
    // `n + 2` passes is a compile-time bound, not a correctness one: stopping
    // early would UNDER-approximate liveness (the sets are still growing), so an
    // unconverged walk hands every register the whole region instead — the same
    // permanent-home answer a loop-carried value gets.
    let mut settled = false;
    for _ in 0..n + 2 {
        let mut changed = false;
        for k in (0..n).rev() {
            let mut out: FxHashSet<u16> = FxHashSet::default();
            for &t in &succs[k] {
                out.extend(live_in[t - s].iter().copied());
            }
            let mut inn = out.clone();
            for d in &defs[k] {
                inn.remove(d);
            }
            inn.extend(uses[k].iter().copied());
            if out != live_out[k] {
                live_out[k] = out;
                changed = true;
            }
            if inn != live_in[k] {
                live_in[k] = inn;
                changed = true;
            }
        }
        if !changed {
            settled = true;
            break;
        }
    }
    let mut span: FxHashMap<u16, (usize, usize)> = FxHashMap::default();
    // Fail closed: an unconverged walk under-approximates liveness, and a cold
    // entry ip has no `live_in` row at all (the loop above skips it). Either way
    // every touched register is reported live at entry — the answer that keeps a
    // permanent home and an entry load, which is never wrong, only slower.
    if !settled || cold.contains(&s) {
        let mut entry_live: FxHashSet<u16> = FxHashSet::default();
        for k in 0..n {
            for &r in uses[k].iter().chain(defs[k].iter()) {
                span.insert(r, (s, e));
                entry_live.insert(r);
            }
        }
        return RegionLiveness {
            spans: span,
            entry_live,
        };
    }
    for k in 0..n {
        let ip = s + k;
        // `defs` is included on its own account: a def writes the home at `ip`
        // even when nothing downstream reads it.
        let touched = live_in[k]
            .iter()
            .chain(live_out[k].iter())
            .chain(uses[k].iter())
            .chain(defs[k].iter());
        for &r in touched {
            let en = span.entry(r).or_insert((ip, ip));
            en.0 = en.0.min(ip);
            en.1 = en.1.max(ip);
        }
    }
    RegionLiveness {
        spans: span,
        entry_live: live_in[0].clone(),
    }
}

/// Does the instruction at `d` run on EVERY pass through region `[s, e]`?
///
/// True when no branch in `[s, d)` can jump PAST `d` while staying inside the
/// region. Branches that LEAVE the region are deliberately allowed, including
/// the loop header's own exit test: OSR entry only happens after the interpreter
/// has already executed the loop `OSR_THRESHOLD` times, so a def that runs every
/// iteration has already written its value into the frame slot — the region
/// materialising the same constant into a home and flushing it back is then a
/// no-op. A def that can be SKIPPED has no such guarantee.
///
/// This is the cheap sound approximation of "the def dominates every exit". It
/// is what makes constant hoisting (and `hoistable_length`) safe without a full
/// dominator tree.
///
/// AUDITED W17 — "which ops name a control-flow target" is stated in FIVE
/// places and they do not agree. The five branch ops below are also all that
/// `region_jump_targets` and `split_home_provably_safe::succ` (both in
/// `plan.rs`) recognise; `succ_of` (below) and `outside_dead`'s `target_of`
/// additionally recognise `PushFinally`, `JumpFinally` and `PushHandler`. The
/// narrow three are sound only because of a fact stated nowhere near them: of
/// those extra ops `region_can_compile` admits ONLY `PushFinally`, and a
/// `PushFinally` whose target lands inside `[s, e]` drags its handler body —
/// `IterCloseFinally` / `EndFinally`, neither admitted — into the region, so
/// the region is rejected before any of these ever runs. Widening the three to
/// the superset would be strictly conservative (more targets ⇒ fewer hoists,
/// more unify vetoes) and is the right fix if that admission ever changes.
pub(crate) fn runs_every_iteration(code: &[Instr], s: usize, e: usize, d: usize) -> bool {
    for (ip, instr) in code.iter().enumerate().take(d).skip(s) {
        let target = match *instr {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => target as usize,
            _ => continue,
        };
        let _ = ip;
        if target > d && target <= e {
            return false; // jumps over `d` and stays in the region
        }
    }
    true
}

/// W28 — candidate TYPE SPLITS for `[s, e]`: VM registers the bytecode
/// compiler recycled ACROSS A `VTy` BOUNDARY, whose two ranges are each
/// provably self-contained. Returns `reg -> TySplit` with `gpr` left at 0 —
/// the bool gpr is handed out at home-allocation time, where the pool is known.
///
/// THE LEGALITY PREDICATE, in full. Everything below must hold; anything
/// unprovable keeps today's whole-region decline. A decline is a slow correct
/// answer, a bad split is a silent wrong one, and this file has shipped
/// several of the latter.
///
///  1. (caller's gate, not here) The plan is one only the GPR emitter can
///     consume — `admit_dv || share_homes`, `admit_bitwise`, `cold` empty —
///     and the caller implements per-def write-through (`admit_wt_share`),
///     because the exit contract below IS write-through's.
///  2. `r` is an ordinary homed value: not a pinned-TA receiver, not a B94
///     split receiver, not a `box_regs` member, not the source register of any
///     pin (`excluded`), never mentioned by a `Not` or `ToPropKey`, and never
///     used as a `Move` source. A `Move` may define ONLY `r`'s numeric half
///     when its numeric source has a dominating in-region def and no branch
///     can enter between that def and the copy. Those are exactly the ops
///     whose handling reads a TYPE or a HOME KIND rather than the opcode: the
///     GPR `Move` arm dispatches on `home(plan, dst)`, so an unproved /
///     Bool-half copy into a split register (numeric home plus a separate bool
///     gpr) would take the wrong arm. The source remains unsplittable, so the
///     numeric arm cannot read its Bool view.
///  3. Every DEF of `r` is one of the ops in the whitelist below, whose type
///     is the same one `plan_region`'s `(def, dty)` match assigns. A def the
///     two views could disagree about refuses the candidate.
///  4. `r`'s touches — the same def/use walk `seg_map` uses, with the DV
///     flag-fuse phantoms dropped — partition into EXACTLY TWO inclusive
///     ranges `R1 = [a1, b1]`, `R2 = [a2, b2]` with `b1 < a2` STRICTLY, every
///     def in `R1` of one type and every def in `R2` of the other. `VTy` has
///     two variants, so one range is the Bool one and the other the Num one.
///  5. Each range STARTS with a def of `r` that does not itself READ `r`. A
///     use-first range could read a value written under the other type, and a
///     def that reads `r` (`Eq { dst: r, a: r, .. }`) is such a read.
///  6. No in-region jump target `t` satisfies `a < t <= b` for either range.
///     Native code is entered at `s` alone (and `region_jump_targets` counts
///     `s` as a target), so region-internal branch targets are the COMPLETE
///     native control flow: with none strictly inside a range, the opening def
///     of (5) DOMINATES every touch in that range, and no value can enter the
///     range from the other one. This is also what makes a loop-carried
///     register unsplittable — a value crossing a back-edge inside a range
///     puts that back-edge's target inside the range.
///  7. Every DEF in the BOOL range is a compare, and every USE in it is one of
///     the exactly two shapes an emitter reads a bool from: a `JumpIfFalse` /
///     `JumpIfTrue` condition, or the `littleEndian` flag of a pinned
///     two-argument DataView `get*`. A whitelist, not a blacklist. No ip in
///     the bool range may both define and use `r` (a compare's operands are
///     numeric, so such an ip would read the register as a number).
///
/// Three further conditions need analyses that do not exist yet at this point
/// in the planner — liveness, dead-code, hoisting, home unification. They are
/// re-checked at the allocation site and DECLINE the region there exactly as
/// the pre-wave planner did: `!live_in(r)`, `shareable(r)`, and `r` being
/// neither hoisted, dead, a slot-const, a remat const, nor home-unified.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_type_splits(
    code: &[Instr],
    s: usize,
    e: usize,
    jump_targets: &FxHashSet<usize>,
    excluded: &FxHashSet<u16>,
    split_recv_lg: &FxHashSet<usize>,
    dv_flag_elide: &FxHashSet<usize>,
    dv_flag_fuse: &FxHashMap<usize, (u16, u16)>,
    dv_flag_reg: &dyn Fn(usize) -> Option<u16>,
) -> FxHashMap<u16, TySplit> {
    let mut out: FxHashMap<u16, TySplit> = FxHashMap::default();
    if !type_split_enabled() || e < s {
        return out;
    }
    let is_cmp = |i: &Instr| {
        matches!(
            i,
            Instr::Lt { .. }
                | Instr::Le { .. }
                | Instr::Gt { .. }
                | Instr::Ge { .. }
                | Instr::Eq { .. }
                | Instr::Ne { .. }
        )
    };
    // (3) The def ops whose `VTy` this pass may predict. Each entry maps to the
    // SAME type the `(def, dty)` match in `plan_region_cold_inner` assigns; the
    // typing loop re-checks the prediction op by op and declines on a mismatch,
    // so drift here is a decline, never a wrong home.
    let def_ty_of = |i: &Instr| -> Option<VTy> {
        match *i {
            Instr::Lt { .. }
            | Instr::Le { .. }
            | Instr::Gt { .. }
            | Instr::Ge { .. }
            | Instr::Eq { .. }
            | Instr::Ne { .. } => Some(VTy::Bool),
            Instr::LoadInt { .. }
            | Instr::LoadConst { .. }
            | Instr::LoadGlobal { .. }
            | Instr::AddInt { .. }
            | Instr::Neg { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Div { .. }
            | Instr::Mod { .. }
            | Instr::Bitwise { .. }
            | Instr::GetIndex { .. }
            | Instr::CallMethod { .. }
            | Instr::CallWithThis { .. }
            | Instr::MathOp {
                op: MathFn::Imul,
                argc: 2,
                ..
            } => Some(VTy::Num),
            _ => None,
        }
    };
    // The sole safe Move exception to (2). The most recent textual source def
    // is a genuine reaching def only when no control-flow edge can enter after
    // it and before (or at) the Move. Native entry is at `s`, while
    // `jump_targets` is the complete set of in-region branch destinations, so
    // this closed interval check proves that every path reaching the copy ran
    // the numeric def first. Elided defs, receiver-object defs, live-ins,
    // Bool/unpredictable defs, and sources with their own split story all fail
    // closed. The source is tainted below even on success.
    let proven_numeric_move = |ip: usize| -> Option<(u16, u16)> {
        let Instr::Move { dst, src } = code[ip] else {
            return None;
        };
        if src == dst || excluded.contains(&src) {
            return None;
        }
        let def_ip = (s..ip)
            .rev()
            .find(|&prev| writes_reg(&code[prev]) == Some(src))?;
        if dv_flag_elide.contains(&def_ip)
            || split_recv_lg.contains(&def_ip)
            || def_ty_of(&code[def_ip]) != Some(VTy::Num)
            || jump_targets
                .iter()
                .any(|&target| target > def_ip && target <= ip)
        {
            return None;
        }
        Some((dst, src))
    };
    let def_ty_at = |ip: usize| -> Option<VTy> {
        if proven_numeric_move(ip).is_some() {
            Some(VTy::Num)
        } else {
            def_ty_of(&code[ip])
        }
    };
    // Cheap candidate prefilter. Most regions contain no recycled register
    // with both predictable Bool and Num defs, so do not allocate a touch Vec
    // for every operand in those regions. This scan deliberately uses the
    // exact same def exclusions and `def_ty_of` whitelist as the full walk
    // below. A register reaches that walk only if all of its visible defs are
    // predictable and their type mask contains both variants; that is a
    // necessary (not sufficient) condition for the existing predicate.
    const DEF_BOOL: u8 = 1;
    const DEF_NUM: u8 = 2;
    const DEF_UNPREDICTABLE: u8 = 4;
    let mut def_kinds: FxHashMap<u16, u8> = FxHashMap::default();

    // (2) Refuse outright any register a Not/ToPropKey mentions. Move sources
    // remain refused; only a destination with the proof above is exempt.
    let mut tainted: FxHashSet<u16> = FxHashSet::default();
    for ip in s..=e {
        match code[ip] {
            Instr::Move { dst, src } => {
                tainted.insert(src);
                if proven_numeric_move(ip).is_none() {
                    tainted.insert(dst);
                }
            }
            Instr::Not { dst, .. } => {
                tainted.insert(dst);
                for u in instr_uses(&code[ip]) {
                    tainted.insert(u);
                }
            }
            Instr::ToPropKey { dst, src, .. } => {
                tainted.insert(dst);
                tainted.insert(src);
            }
            _ => {}
        }
        if let Some(d) = writes_reg(&code[ip]) {
            if !dv_flag_elide.contains(&ip) && !split_recv_lg.contains(&ip) {
                let kind = match def_ty_at(ip) {
                    Some(VTy::Bool) => DEF_BOOL,
                    Some(VTy::Num) => DEF_NUM,
                    None => DEF_UNPREDICTABLE,
                };
                *def_kinds.entry(d).or_insert(0) |= kind;
            }
        }
    }
    let candidates: FxHashSet<u16> = def_kinds
        .into_iter()
        .filter_map(|(r, kinds)| {
            (kinds == (DEF_BOOL | DEF_NUM) && !excluded.contains(&r) && !tainted.contains(&r))
                .then_some(r)
        })
        .collect();
    if candidates.is_empty() {
        return out;
    }
    // (4) Touches, exactly as the `seg_map` walk collects them: `instr_uses` /
    // `writes_reg`, minus the DV flag-fuse phantoms (an elided `Eq` writes no
    // home, and the fused call reads the Eq's OPERANDS, not its dst) and minus
    // a B94 receiver `LoadGlobal` (that def stores the object to memory and
    // never fills a home).
    let mut touch: FxHashMap<u16, Vec<(usize, bool)>> = FxHashMap::default();
    for ip in s..=e {
        let instr = &code[ip];
        let elided_dst = dv_flag_elide
            .contains(&ip)
            .then(|| writes_reg(instr))
            .flatten()
            .or_else(|| {
                (dv_flag_fuse.contains_key(&ip) && ip > s && dv_flag_elide.contains(&(ip - 1)))
                    .then(|| writes_reg(&code[ip - 1]))
                    .flatten()
            });
        for u in instr_uses(instr) {
            if !candidates.contains(&u) || (dv_flag_fuse.contains_key(&ip) && elided_dst == Some(u))
            {
                continue;
            }
            touch.entry(u).or_default().push((ip, false));
        }
        if let Some(d) = writes_reg(instr) {
            if candidates.contains(&d)
                && !dv_flag_elide.contains(&ip)
                && !split_recv_lg.contains(&ip)
            {
                touch.entry(d).or_default().push((ip, true));
            }
        }
    }
    let mut regs: Vec<u16> = touch.keys().copied().collect();
    regs.sort_unstable(); // deterministic plans
    for r in regs {
        if excluded.contains(&r) || tainted.contains(&r) {
            continue;
        }
        // Collapse to one entry per ip: (ip, has_def, has_use).
        let mut ips: Vec<(usize, bool, bool)> = Vec::new();
        {
            let mut v = touch[&r].clone();
            v.sort_unstable();
            for (ip, is_def) in v {
                match ips.last_mut() {
                    Some(last) if last.0 == ip => {
                        if is_def {
                            last.1 = true;
                        } else {
                            last.2 = true;
                        }
                    }
                    _ => ips.push((ip, is_def, !is_def)),
                }
            }
        }
        // (5) the first touch is a def that does not read `r`.
        if ips.is_empty() || !ips[0].1 || ips[0].2 {
            continue;
        }
        // (3) every def op must be predictable.
        let mut tys: Vec<Option<VTy>> = Vec::with_capacity(ips.len());
        let mut unpredictable = false;
        for &(ip, has_def, _) in &ips {
            if has_def {
                match def_ty_at(ip) {
                    Some(t) => tys.push(Some(t)),
                    None => {
                        unpredictable = true;
                        break;
                    }
                }
            } else {
                tys.push(None);
            }
        }
        if unpredictable {
            continue;
        }
        let first_ty = match tys[0] {
            Some(t) => t,
            None => continue,
        };
        // (4) the single type boundary: the first def of the OTHER type.
        let k = match (0..ips.len()).find(|&i| matches!(tys[i], Some(t) if t != first_ty)) {
            Some(k) => k,
            None => continue, // no conflict on this register — nothing to split
        };
        // Every def before `k` is `first_ty` by construction; every def from
        // `k` on must be the other type (a third alternation is refused).
        let other_ty = match tys[k] {
            Some(t) => t,
            None => continue,
        };
        if (k..ips.len()).any(|i| matches!(tys[i], Some(t) if t != other_ty)) || k == 0 {
            continue;
        }
        let (a1, b1) = (ips[0].0, ips[k - 1].0);
        let (a2, b2) = (ips[k].0, ips[ips.len() - 1].0);
        // (4) strict separation, and (5) the later range opens with a def that
        // does not read `r`.
        if b1 >= a2 || !ips[k].1 || ips[k].2 {
            continue;
        }
        // (6) no in-region jump target strictly inside either range.
        if jump_targets
            .iter()
            .any(|&t| (t > a1 && t <= b1) || (t > a2 && t <= b2))
        {
            continue;
        }
        let (bool_lo, bool_hi, num_lo, num_hi) = if first_ty == VTy::Bool {
            (a1, b1, a2, b2)
        } else {
            (a2, b2, a1, b1)
        };
        // (7) the bool range's closed shape.
        let mut bad = false;
        for &(ip, has_def, has_use) in &ips {
            if ip < bool_lo || ip > bool_hi {
                continue;
            }
            if (has_def && has_use) || (has_def && !is_cmp(&code[ip])) {
                bad = true;
                break;
            }
            if has_use {
                let ok = match code[ip] {
                    Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => cond == r,
                    _ => dv_flag_reg(ip) == Some(r),
                };
                if !ok {
                    bad = true;
                    break;
                }
            }
        }
        if bad {
            continue;
        }
        out.insert(
            r,
            TySplit {
                bool_lo,
                bool_hi,
                num_lo,
                num_hi,
                gpr: 0,
            },
        );
    }
    out
}

#[cfg(test)]
mod type_split_tests {
    use super::*;

    fn splits(code: &[Instr]) -> FxHashMap<u16, TySplit> {
        splits_with_targets(code, &[0])
    }

    fn splits_with_targets(code: &[Instr], targets: &[usize]) -> FxHashMap<u16, TySplit> {
        let jump_targets: FxHashSet<usize> = targets.iter().copied().collect();
        let excluded = FxHashSet::default();
        let split_recv_lg = FxHashSet::default();
        let dv_flag_elide = FxHashSet::default();
        let dv_flag_fuse = FxHashMap::default();
        plan_type_splits(
            code,
            0,
            code.len() - 1,
            &jump_targets,
            &excluded,
            &split_recv_lg,
            &dv_flag_elide,
            &dv_flag_fuse,
            &|_| None,
        )
    }

    #[test]
    fn type_split_prefilter_keeps_the_existing_mixed_def_predicate() {
        if !type_split_enabled() {
            return;
        }

        let eligible = [
            Instr::Lt { dst: 3, a: 0, b: 1 },
            Instr::JumpIfFalse { cond: 3, target: 3 },
            Instr::LoadInt { dst: 3, val: 7 },
        ];
        let got = splits(&eligible);
        let sp = got.get(&3).expect("mixed Bool/Num register should split");
        assert_eq!((sp.bool_lo, sp.bool_hi), (0, 1));
        assert_eq!((sp.num_lo, sp.num_hi), (2, 2));
        assert_eq!(sp.gpr, 0);

        let no_conflict = [
            Instr::LoadInt { dst: 3, val: 1 },
            Instr::LoadInt { dst: 3, val: 2 },
        ];
        assert!(splits(&no_conflict).is_empty());

        // A third, unrecognised def must not be hidden by the cheap mask. The
        // full predicate has always failed closed when any visible def cannot
        // be assigned a type by `def_ty_of`.
        let unpredictable = [
            Instr::Lt { dst: 3, a: 0, b: 1 },
            Instr::JumpIfFalse { cond: 3, target: 4 },
            Instr::LoadInt { dst: 3, val: 7 },
            Instr::StrConcat { dst: 3, a: 0, b: 1 },
        ];
        assert!(splits(&unpredictable).is_empty());
    }

    #[test]
    fn type_split_move_requires_a_dominating_numeric_source_def() {
        if !type_split_enabled() {
            return;
        }

        let eligible = [
            Instr::Lt { dst: 3, a: 0, b: 1 },
            Instr::JumpIfFalse { cond: 3, target: 4 },
            Instr::Add { dst: 4, a: 0, b: 1 },
            Instr::Move { dst: 3, src: 4 },
        ];
        let got = splits_with_targets(&eligible, &[0, 4]);
        let sp = got
            .get(&3)
            .expect("a dominated numeric Move may open the numeric half");
        assert_eq!((sp.bool_lo, sp.bool_hi), (0, 1));
        assert_eq!((sp.num_lo, sp.num_hi), (3, 3));

        // A branch may not enter after the source definition: then the Move
        // can observe a value not established by the proposed reaching def.
        assert!(splits_with_targets(&eligible, &[0, 3, 4]).is_empty());

        let bool_source = [
            Instr::Lt { dst: 3, a: 0, b: 1 },
            Instr::JumpIfFalse { cond: 3, target: 4 },
            Instr::Eq { dst: 4, a: 0, b: 1 },
            Instr::Move { dst: 3, src: 4 },
        ];
        assert!(splits_with_targets(&bool_source, &[0, 4]).is_empty());

        let live_in_source = [
            Instr::Lt { dst: 3, a: 0, b: 1 },
            Instr::JumpIfFalse { cond: 3, target: 3 },
            Instr::Move { dst: 3, src: 4 },
        ];
        assert!(splits_with_targets(&live_in_source, &[0, 3]).is_empty());
    }
}

/// The operand positions of `i` that REQUIRE a number, as opposed to positions
/// that accept any value. A read-only live-in is admitted as a numeric home only
/// when every one of its uses appears here, so the entry Int-guard is backed by
/// how the value is consumed.
///
/// Deliberately EXCLUDES: `Add` (also string concatenation), `Eq`/`Ne` (defined
/// on every type), `Move` and `StoreGlobal` (pure transfers that say nothing
/// about the value), `StrConcat`, and every heap-op receiver/key. Admitting
/// those is what made the blanket version of this a 3.31x -> 3.45x regression.
/// Relational ops are included: they are string-comparable in principle, but in
/// a region that already type-checked as numeric a `<` operand is a loop bound.
pub(crate) fn numeric_operand_uses(i: &Instr) -> Vec<u16> {
    match *i {
        Instr::Sub { a, b, .. }
        | Instr::Mul { a, b, .. }
        | Instr::Div { a, b, .. }
        | Instr::Mod { a, b, .. }
        | Instr::Bitwise { a, b, .. }
        | Instr::Lt { a, b, .. }
        | Instr::Le { a, b, .. }
        | Instr::Gt { a, b, .. }
        | Instr::Ge { a, b, .. }
        | Instr::JumpIfNotLt { a, b, .. }
        | Instr::JumpIfNotLe { a, b, .. } => vec![a, b],
        Instr::AddInt { a, .. } | Instr::Neg { a, .. } => vec![a],
        Instr::MathOp { arg_base, argc, .. } => (0..argc).map(|k| arg_base + k).collect(),
        // The KEY of a read-modify-write (`x[i] *= v`). Declaring it numeric is
        // what makes a live-in key sound on this tier: the entry guard bails to
        // the interpreter for anything that is not a genuine number, and a
        // NUMBER key is exactly the case where ToPropertyKey is the identity.
        // (A string key thereby DECLINES/bails rather than compiling — the same
        // treatment the general heap-op key gets by exclusion from this list.)
        Instr::ToPropKey { src, .. } => vec![src],
        _ => vec![],
    }
}

/// The VM registers an instruction reads (operands). Feeds live-in analysis,
/// in-region dead-code elimination, home sharing/unification, and — through
/// `read_outside`, which runs this over the WHOLE enclosing function — the
/// decision whether a register's frame slot is still observed after the region.
///
/// EXHAUSTIVE BY CONSTRUCTION: there is no `_` arm, so a new `Instr` variant is
/// a BUILD ERROR here until its operands are declared. That is the point. This
/// table used to end in `_ => vec![]` — "an opcode I have never heard of reads
/// nothing" — and every consumer then reasoned from a false fact. The live case
/// was `TypeOf`: `read_outside` missed `typeof x`, so a register whose only
/// post-region use was a `typeof` looked dead-after-region, became `shareable`,
/// dropped out of `live_in_regs` (whose invariant is "every flushed home is
/// entry-loaded") while staying in `num_regs`, and `flush_exit` wrote a home
/// nothing had ever filled into its slot. `typeof x` on an `undefined` local
/// answered "number" after a hot loop.
///
/// Conventions, so a new arm has one obvious right answer:
///   * `dst` / `*_dst` fields are WRITES and never appear here (`writes_reg`
///     covers those). A field that is read-modify-write DOES appear — it is
///     read: `ArrayAppend::arr`, an iterator cursor `idx`, `MakeCell::reg`,
///     `DecKey::key`, `DisposeScope::kind_reg`.
///   * A contiguous ARGUMENT WINDOW (`arg_base` + `argc`) is expanded. The
///     decorator ops are the exception the naming hides: `DecElem`/`DecClass`
///     read `argc` (decorator, receiver) PAIRS, so their window is `2 * argc`.
///   * An op that consumes the activation's `this` reads REGISTER 0 — the
///     `super`-* family, `FieldInit`. That read appears in no operand field, and
///     omitting it would let a region home reg 0 and flush over the receiver.
///   * A `u16::MAX` register field is the "absent" sentinel (`IterNext::next` at
///     a destructuring site), not a register; it is filtered out.
///
/// The ONE read this signature cannot name: `MakeClosure`/`MakeArrow` capture
/// the cells listed as `UpvalSource::ParentLocal(reg)` in the CALLEE's proto,
/// which is not reachable from an `&Instr`. Every such register is boxed by a
/// `MakeCell`/`MakeCellTdz`/`MakeCellFnName` in the SAME function, so those ops
/// declare their `reg` as a use — `MakeCellTdz` only writes it, but declaring it
/// is what puts a capture source into the set-valued consumers (`read_outside`,
/// `used`). No cell or closure op is admitted into a compiled region
/// (`region_can_compile` rejects them all), so attributing the capture read to
/// the cell op rather than the closure op cannot mis-order in-region liveness.
pub(crate) fn instr_uses(i: &Instr) -> Vec<u16> {
    /// A contiguous argument window `[base, base + n)`, preceded by any
    /// explicitly named operands (a callee, a receiver, a key).
    fn win(head: &[u16], base: u16, n: u16) -> Vec<u16> {
        let mut v = Vec::with_capacity(head.len() + n as usize);
        v.extend_from_slice(head);
        v.extend((0..n).map(|k| base + k));
        v
    }
    /// The activation's `this`.
    const THIS: u16 = 0;
    match *i {
        // ── pure transfers ──
        Instr::Move { src, .. } => vec![src],
        Instr::StoreGlobal { src, .. }
        | Instr::StoreGlobalStrict { src, .. }
        | Instr::StoreGlobalResolved { src, .. }
        | Instr::StoreGlobalDyn { src, .. }
        | Instr::EvalScopeSet { src, .. }
        | Instr::UpvalSet { src, .. }
        | Instr::StoreUpvalDyn { src, .. }
        | Instr::TemplateSetCached { src, .. } => vec![src],

        // ── arithmetic / logic ──
        Instr::AddInt { a, .. } | Instr::Neg { a, .. } => vec![a],
        Instr::Add { a, b, .. }
        | Instr::Sub { a, b, .. }
        | Instr::Mul { a, b, .. }
        | Instr::Div { a, b, .. }
        | Instr::Mod { a, b, .. }
        | Instr::Pow { a, b, .. }
        | Instr::StrConcat { a, b, .. }
        | Instr::StrAppendInPlace { a, b, .. }
        // W11 (B124) fused chain link. In `numeric_operand_uses` above it is
        // deliberately ABSENT, like `StrConcat` (the 3.31x→3.45x lesson).
        | Instr::StrConcatChain { a, b, .. }
        | Instr::Bitwise { a, b, .. }
        | Instr::Lt { a, b, .. }
        | Instr::Le { a, b, .. }
        | Instr::Gt { a, b, .. }
        | Instr::Ge { a, b, .. }
        | Instr::Eq { a, b, .. }
        | Instr::Ne { a, b, .. }
        | Instr::LooseEq { a, b, .. }
        | Instr::LooseNe { a, b, .. }
        | Instr::JumpIfNotLt { a, b, .. }
        | Instr::JumpIfNotLe { a, b, .. } => vec![a, b],
        Instr::StrAppendIndex {
            a, obj, key, ..
        } => vec![a, obj, key],
        Instr::AddRightPair { a, b, c, .. } => vec![a, b, c],
        Instr::Pad2Concat { src, .. } => vec![src],
        Instr::Pad2Conditional { src, .. } => vec![src],
        // Unary value ops. `TypeOf`/`TypeOfIs` are the pair this table was
        // missing; see the note above.
        Instr::ToNum { a, .. }
        | Instr::BitNot { a, .. }
        | Instr::Not { a, .. }
        | Instr::ToStr { a, .. }
        | Instr::TypeOf { a, .. }
        | Instr::TypeOfIs { a, .. } => vec![a],
        Instr::JsonParse {
            a, callee, this_v, ..
        } => vec![a, callee, this_v],
        Instr::IsArray {
            a, callee, this_v, ..
        } => vec![a, callee, this_v],
        Instr::JsonStringify {
            val,
            space,
            callee,
            this_v,
            ..
        } => vec![val, space, callee, this_v],

        // ── control flow ──
        Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => vec![cond],
        Instr::Return { src } => vec![src],
        Instr::Throw { src } => vec![src],
        // The handler/finally BRACKET ops record their registers for the
        // unwinder, which WRITES them; `EndFinally` reads the completion back.
        Instr::EndFinally { kind_reg, val_reg } => vec![kind_reg, val_reg],

        // ── heap property / element ops ──
        Instr::GetProp { obj, .. }
        | Instr::DeleteProp { obj, .. }
        | Instr::WithHas { obj, .. }
        | Instr::WithGet { obj, .. }
        | Instr::ForInKeys { obj, .. }
        | Instr::LenOf { obj, .. } => vec![obj],
        Instr::ObjectKeys {
            obj, callee, this_v, ..
        }
        | Instr::ObjectValues {
            obj, callee, this_v, ..
        }
        | Instr::ObjectEntries {
            obj, callee, this_v, ..
        } => vec![obj, callee, this_v],
        Instr::SetProp { obj, val, .. }
        | Instr::SetPrivate { obj, val, .. }
        | Instr::InitDataProp { obj, val, .. }
        | Instr::AppendDataProp { obj, val, .. }
        | Instr::DefineField { obj, val, .. }
        | Instr::SetLiteralProto { obj, val }
        | Instr::ObjectSpread { target: obj, src: val }
        | Instr::WithSet { obj, val, .. } => vec![obj, val],
        // The staged value block of a one-step literal — exactly `NewArray`'s
        // contiguous-window pattern, with the count carried in the instruction.
        Instr::FinalizeObject {
            val_base, count, ..
        } => win(&[], val_base, count),
        Instr::GetIndex { obj, key, .. } => vec![obj, key],
        Instr::SetIndex { obj, key, val } => vec![obj, key, val],
        Instr::GetIndexConcat { obj, key, .. } => vec![obj, key],
        Instr::SetIndexConcat { obj, key, val, .. } => vec![obj, key, val],
        Instr::InitDataPropDyn { obj, key, val } => vec![obj, key, val],
        Instr::DeleteIndex { obj, key, .. } => vec![obj, key],
        Instr::DeleteIndexConcat { obj, key, .. } => vec![obj, key],
        Instr::ForInLive { obj, key, .. } => vec![obj, key],
        Instr::HasProp { obj, key, .. } => vec![obj, key],
        Instr::DefineAccessor { obj, key, func, .. } => vec![obj, key, func],
        Instr::SetFnNameFromKey { func, key, .. } => vec![func, key],
        // ToPropKey reads the receiver (nullish check) and the key. `src` MUST
        // be listed or the dead-code pass drops the load that feeds it; `obj`
        // follows the GetIndex/SetIndex pattern and is exempted at the pinned
        // receiver's use-site scan like theirs.
        Instr::ToPropKey { obj, src, .. } => vec![obj, src],
        Instr::ToConcatKey { src, .. }
        | Instr::ToObject { src, .. }
        | Instr::CheckCoercible { src }
        | Instr::ThisCheck { src }
        | Instr::ArrayRest { src, .. }
        | Instr::ObjectRest { src, .. }
        | Instr::GetIterator { src, .. }
        | Instr::GetIteratorObj { src, .. }
        | Instr::IterToArray { src, .. }
        | Instr::GetAsyncIterator { src, .. }
        | Instr::DateParse { src, .. } => vec![src],
        Instr::ObjectRestDyn { src, keys_base, n, .. } => win(&[src], keys_base, n),

        // ── calls ──
        // A call reads a CONTIGUOUS argument window starting at `arg_base`, plus
        // the callee or receiver. Reporting no uses let the home-unification
        // passes treat those registers as dead and alias over them — see the
        // matching note in `writes_reg`.
        Instr::StaticFn {
            callee,
            this_v,
            arg_base,
            argc,
            ..
        } => win(&[callee, this_v], arg_base, argc),
        Instr::MathOp {
            callee,
            this_v,
            arg_base,
            argc,
            ..
        } => win(&[callee, this_v], arg_base, argc),
        Instr::GlobalFn { callee, arg_base, argc, .. } => win(&[callee], arg_base, argc),
        Instr::ArrayCtor { callee, arg_base, argc, .. } => {
            match callee {
                Some(callee) => win(&[callee], arg_base, argc),
                None => win(&[], arg_base, argc),
            }
        }
        Instr::NewArray { arg_base, argc, .. }
        | Instr::DateNew { arg_base, argc, .. }
        | Instr::DateUTC { arg_base, argc, .. }
        | Instr::Print { arg_base, argc, .. } => win(&[], arg_base, argc),
        Instr::Call { callee, arg_base, argc, .. }
        | Instr::TailCall { callee, arg_base, argc }
        | Instr::New { callee, arg_base, argc, .. } => win(&[callee], arg_base, argc),
        Instr::CallWithThis { callee, this_v, arg_base, argc, .. }
        | Instr::RegExpMethod { callee, this_v, arg_base, argc, .. }
        | Instr::TailCallWithThis { callee, this_v, arg_base, argc } => {
            win(&[callee, this_v], arg_base, argc)
        }
        Instr::CallMethod { obj, arg_base, argc, .. } => win(&[obj], arg_base, argc),
        Instr::CallMethodComputed { obj, key, arg_base, argc, .. } => {
            win(&[obj, key], arg_base, argc)
        }
        Instr::CallSpread { callee, args, .. } | Instr::NewSpread { callee, args, .. } => {
            vec![callee, args]
        }
        Instr::CallWithThisSpread {
            callee,
            this_v,
            args,
            ..
        } => vec![callee, this_v, args],
        Instr::CallMethodSpread { obj, args, .. } => vec![obj, args],
        Instr::CallMethodComputedSpread { obj, key, args, .. } => vec![obj, key, args],
        Instr::MathSpread {
            callee,
            this_v,
            args,
            ..
        } => vec![callee, this_v, args],
        Instr::ArrayFrom {
            src,
            mapfn,
            callee,
            this_v,
            ..
        } => vec![src, mapfn, callee, this_v],
        Instr::InstanceOfDyn { val, ctor, .. } => vec![val, ctor],
        // The syntactic `eval` reference (callee + WithBaseObject), caller
        // lexical `this`, and its COMPLETE argument source. A spread site uses
        // one materialized Array register; an ordinary site uses a contiguous
        // argument window.
        Instr::DirectEval {
            callee,
            this_v,
            arg_base,
            argc,
            args_array,
            this_reg,
            ..
        } => {
            if args_array {
                vec![callee, this_v, arg_base, this_reg]
            } else {
                win(&[callee, this_v, this_reg], arg_base, argc)
            }
        }

        // ── `super` ── every form also consumes the activation's `this`
        // (register 0), which no operand field names.
        Instr::SuperCtor { ctor, arg_base, argc, .. } => win(&[ctor, THIS], arg_base, argc),
        Instr::SuperCtorSpread { ctor, args, .. } => vec![ctor, THIS, args],
        Instr::SuperMethod { base, arg_base, argc, .. } => win(&[base, THIS], arg_base, argc),
        Instr::SuperMethodComputed { base, key, arg_base, argc, .. } => {
            win(&[base, key, THIS], arg_base, argc)
        }
        Instr::SuperGet { .. } | Instr::SuperGetObj { .. } => vec![THIS],
        Instr::SuperGetComputed { key, .. } | Instr::SuperGetObjComputed { key, .. } => {
            vec![key, THIS]
        }
        Instr::SuperGetRef { receiver, .. } => vec![receiver],
        Instr::SuperGetRefComputed { key, receiver, .. } => vec![key, receiver],
        Instr::SuperSet { base, val, .. } => vec![base, val, THIS],
        Instr::SuperSetComputed { base, key, val, .. } => vec![base, key, val, THIS],
        Instr::SuperSetObj { val, .. } => vec![val, THIS],
        Instr::SuperSetObjComputed { key, val } => vec![key, val, THIS],
        Instr::SuperMethodObj { arg_base, argc, .. } => win(&[THIS], arg_base, argc),
        Instr::SuperMethodObjComputed { key, arg_base, argc, .. } => {
            win(&[key, THIS], arg_base, argc)
        }
        Instr::SuperMethodSpread { args, .. } => vec![args, THIS],
        Instr::SuperMethodComputedSpread { key, args, .. } => vec![key, args, THIS],
        Instr::SetHomeObject { method, home } => vec![method, home],

        // ── classes / decorators ──
        // `DecElem`/`DecClass` read `argc` (decorator, receiver) PAIRS — the
        // window is 2 * argc wide, not argc.
        Instr::DecElem { class, arg_base, argc, .. } => {
            win(&[class], arg_base, argc.saturating_mul(2))
        }
        Instr::DecClass { class, arg_base, argc } => {
            win(&[class], arg_base, argc.saturating_mul(2))
        }
        // DecKey / PushFieldKey / ClassAddMember ToPropertyKey the key IN PLACE:
        // read-modify-write, so `key` is a use as well as a def.
        Instr::DecKey { class, key, .. }
        | Instr::PushFieldKey { class, key }
        | Instr::ClassAddMember { class, key, .. } => vec![class, key],
        Instr::ClassStaticField { class, key, val } => vec![class, key, val],
        Instr::DecInits { recv, .. } => vec![recv],
        Instr::DecField { val, recv, .. } => vec![val, recv],
        // The field's value plus the activation's `this` (the instance).
        Instr::FieldInit { val, .. } => vec![val, THIS],
        Instr::MakeClass { parent, .. } => parent.into_iter().collect(),

        // ── generators / async / iteration ──
        Instr::Yield { val, .. }
        | Instr::Await { val, .. }
        | Instr::YieldDelegate { val, .. }
        | Instr::AsyncYieldDelegate { val, .. }
        | Instr::RequireObject { val } => vec![val],
        Instr::AsyncIterThrowStep { iter, exc, .. } => vec![iter, exc],
        Instr::AsyncIterNextStep { iter, idx, sent, next_fn, .. } => {
            vec![iter, idx, sent, next_fn]
        }
        Instr::AsyncIterReturnStep { iter, ret, .. } => vec![iter, ret],
        Instr::AsyncFromSyncStep { step, iter, .. } => vec![step, iter],
        Instr::IterDelegate { iter, mode, sent, .. } => vec![iter, mode, sent],
        // The cursor `idx` is read-modify-write. `next` is the PRIMED next
        // method, or the `u16::MAX` absent sentinel at a destructuring site.
        Instr::IterNext { iter, idx, next, .. } => {
            let mut v = vec![iter, idx];
            if next != u16::MAX {
                v.push(next);
            }
            v
        }
        Instr::ForAwaitNext { iter, idx, .. } => vec![iter, idx],
        Instr::IterPrime { iter, .. }
        | Instr::IterClose { iter }
        | Instr::IterCloseQuiet { iter } => vec![iter],
        Instr::IterCloseFinally { iter, kind_reg } => vec![iter, kind_reg],

        // ── `using` / disposal ──
        Instr::RegisterDisposable { scope, val }
        | Instr::RegisterAsyncDisposable { scope, val } => vec![scope, val],
        Instr::DisposeScope { scope, kind_reg, val_reg } => vec![scope, kind_reg, val_reg],
        Instr::AsyncDisposeNext { scope, .. } => vec![scope],
        Instr::MergeDispose { kind_reg, val_reg, err } => vec![kind_reg, val_reg, err],

        // ── closure cells ──
        // `MakeCellTdz` only WRITES its reg; it is declared a use because it is
        // the one textual naming of a register a later `MakeClosure`/`MakeArrow`
        // captures through the callee proto's upvalue list (see the note above).
        Instr::MakeCell { reg }
        | Instr::MakeCellTdz { reg }
        | Instr::MakeCellFnName { reg }
        | Instr::MarkCellConst { reg } => vec![reg],
        Instr::CellGet { cell, .. } => vec![cell],
        Instr::CellSet { cell, src } | Instr::CellSetChecked { cell, src } => vec![cell, src],
        Instr::MakeArrow { this_reg, .. } => vec![this_reg],

        // ── constructors / literals with register operands ──
        Instr::NewMap { src, .. }
        | Instr::NewSet { src, .. }
        | Instr::NewWeakMap { src, .. }
        | Instr::NewWeakSet { src, .. } => src.into_iter().collect(),
        Instr::NewBox { arg, .. } | Instr::MakeSymbol { desc: arg, .. } => {
            arg.into_iter().collect()
        }
        Instr::NewError { arg, opts, errors, .. } => {
            arg.into_iter().chain(opts).chain(errors).collect()
        }
        Instr::NewWeakRef { target, .. } => vec![target],
        Instr::NewFinalizationRegistry { cleanup, .. } => vec![cleanup],
        Instr::NewPromise { executor, .. } => vec![executor],
        Instr::NewRegExp { pattern, flags, .. } => vec![pattern, flags],
        Instr::BigIntFrom { arg, .. } => vec![arg],
        Instr::ArrayAppend { arr, val, .. } => vec![arr, val],
        Instr::SetRaw { arr, raw } => vec![arr, raw],
        Instr::ImportCall { spec, opts, .. } => std::iter::once(spec).chain(opts).collect(),

        // ── reads NO register ──
        // Constants and materializations (their only register is `dst`); the
        // global/upvalue LOADS (a slot index is the operand, not a register);
        // the jump and handler-bracket ops, which record a target or a register
        // for the UNWINDER to write; and the generator entry marker.
        Instr::LoadConst { .. }
        | Instr::LoadInt { .. }
        | Instr::LoadUndefined { .. }
        | Instr::LoadNewTarget { .. }
        | Instr::LoadCallee { .. }
        | Instr::LoadClassValue { .. }
        | Instr::LoadHole { .. }
        | Instr::LoadNull { .. }
        | Instr::LoadBool { .. }
        | Instr::LoadBigInt { .. }
        | Instr::LoadBigIntBig { .. }
        | Instr::LoadGlobal { .. }
        | Instr::LoadGlobalOrUndefined { .. }
        | Instr::LoadGlobalDyn { .. }
        | Instr::LoadGlobalOrUndefinedDyn { .. }
        | Instr::EvalScopeHas { .. }
        | Instr::CheckGlobalResolvable { .. }
        | Instr::DeleteGlobal { .. }
        | Instr::UpvalGet { .. }
        | Instr::LoadUpvalDyn { .. }
        | Instr::NewObject { .. }
        | Instr::NewPlannedObject { .. }
        | Instr::MakeFunc { .. }
        // The capture sources live in the CALLEE proto; they are declared at the
        // `MakeCell*` that boxes them (see the note above).
        | Instr::MakeClosure { .. }
        | Instr::SuperCtorFetch { .. }
        | Instr::SuperBase { .. }
        | Instr::OpenUsingScope { .. }
        | Instr::TemplateGetCached { .. }
        | Instr::ImportMeta { .. }
        | Instr::Now { .. }
        | Instr::Jump { .. }
        | Instr::JumpFinally { .. }
        | Instr::PushHandler { .. }
        | Instr::PopHandler
        | Instr::PushFinally { .. }
        | Instr::PopFinally
        | Instr::GenStart
        | Instr::ReturnUndefined => vec![],
    }
}

/// One own data field of a fresh object scalar-replaced inside a loop.
/// `scratch` is a register that is otherwise unmentioned by the whole function,
/// so it remains a normal GC root and cannot alias a nested activation.
#[derive(Clone, Debug)]
pub(crate) struct LocalSroaField {
    pub(crate) name: u32,
    pub(crate) scratch: u16,
    pub(crate) append_ip: u32,
}

/// Runtime information needed to materialize a fresh object after a native
/// guard bail. A clean loop exit needs no materialization because the planner
/// proves the allocation reference dead outside its construction/use window.
#[derive(Clone, Debug)]
pub(crate) struct LocalSroaObject {
    pub(crate) alloc_ip: u32,
    pub(crate) dst: u16,
    pub(crate) hint: u16,
    pub(crate) fields: Vec<LocalSroaField>,
}

/// Runtime information needed to materialize a dense array after a native
/// guard bail. Eligibility proves the argument registers are not overwritten
/// between `NewArray` and the end of the region.
#[derive(Clone, Debug)]
pub(crate) struct LocalSroaArray {
    pub(crate) alloc_ip: u32,
    pub(crate) dst: u16,
    pub(crate) arg_base: u16,
    pub(crate) argc: u16,
}

/// Runtime information needed to materialize a one-step `FinalizeObject`
/// literal after a native guard bail. Like the array case, eligibility proves
/// the staged value block is not overwritten between the (elided) finalize and
/// the region end, so `ObjMap::finalized_from_plan` over the live block regs
/// rebuilds the exact object the interpreter would have produced.
#[derive(Clone, Debug)]
pub(crate) struct LocalSroaFinalized {
    pub(crate) alloc_ip: u32,
    pub(crate) dst: u16,
    pub(crate) plan_idx: u16,
    pub(crate) val_base: u16,
    pub(crate) count: u16,
}

/// A string concat whose result is virtual inside a local-SROA region because
/// its only observation is `.length`. The native clone keeps the proved i32 in
/// `scratch`; an internal bail recreates the exact primitive string before the
/// original bytecode resumes.
#[derive(Clone, Debug)]
pub(crate) struct LocalSroaConcatLen {
    pub(crate) prefix_const: u32,
    pub(crate) prefix_load_ip: u32,
    pub(crate) prefix_reg: u16,
    pub(crate) add_ip: u32,
    pub(crate) add_dst: u16,
    pub(crate) add_live_until: u32,
    pub(crate) get_ip: u32,
    pub(crate) get_dst: u16,
    pub(crate) get_live_until: u32,
    pub(crate) scratch: u16,
}

/// Deopt/materialization half of a local aggregate scalar-replacement plan.
#[derive(Clone, Debug, Default)]
pub(crate) struct LocalSroaPlan {
    pub(crate) objects: Vec<LocalSroaObject>,
    pub(crate) arrays: Vec<LocalSroaArray>,
    pub(crate) finalized: Vec<LocalSroaFinalized>,
    pub(crate) concat_lens: Vec<LocalSroaConcatLen>,
    pub(crate) scratch: Vec<u16>,
}

/// Compile-time half of [`LocalSroaPlan`]. The rewritten proto has identical
/// instruction indices, so every native bail ip remains an ip in the original
/// bytecode and the interpreter can resume after materialization.
pub(crate) struct LocalSroaCompilePlan {
    pub(crate) proto: FuncProto,
    pub(crate) runtime: LocalSroaPlan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalScalarTy {
    Unknown,
    Num,
    Str,
    Object(usize),
    Array(usize),
    Finalized(usize),
}

/// Destination cover for the deliberately narrow local-SROA input language.
/// Returning `None` is ambiguous (an instruction may genuinely have no dst),
/// so `local_sroa_supported_instr` is the fail-closed companion gate.
fn local_sroa_dst(i: &Instr) -> Option<u16> {
    match *i {
        Instr::LoadInt { dst, .. }
        | Instr::LoadConst { dst, .. }
        | Instr::LoadUndefined { dst }
        | Instr::Move { dst, .. }
        | Instr::AddInt { dst, .. }
        | Instr::Add { dst, .. }
        | Instr::Mul { dst, .. }
        | Instr::Bitwise { dst, .. }
        | Instr::GetProp { dst, .. }
        | Instr::GetIndex { dst, .. }
        | Instr::NewObject { dst, .. }
        | Instr::NewPlannedObject { dst, .. }
        | Instr::FinalizeObject { dst, .. }
        | Instr::NewArray { dst, .. } => Some(dst),
        _ => None,
    }
}

/// The first local-SROA lane is intentionally a closed, audit-friendly loop
/// language. In particular it excludes calls, handlers, dynamic property keys,
/// accessors, spreads, stores and closure ops. Widening this list requires a
/// corresponding escape/type/materialization proof below.
fn local_sroa_supported_instr(i: &Instr) -> bool {
    matches!(
        i,
        Instr::LoadInt { .. }
            | Instr::LoadConst { .. }
            | Instr::LoadUndefined { .. }
            | Instr::Move { .. }
            | Instr::AddInt { .. }
            | Instr::Add { .. }
            | Instr::Mul { .. }
            | Instr::Bitwise { .. }
            | Instr::JumpIfNotLt { .. }
            | Instr::Jump { .. }
            | Instr::NewObject { .. }
            | Instr::NewPlannedObject { .. }
            | Instr::FinalizeObject { .. }
            | Instr::AppendDataProp { .. }
            | Instr::GetProp { .. }
            | Instr::NewArray { .. }
            | Instr::GetIndex { .. }
            | Instr::Print { .. }
            | Instr::ReturnUndefined
    )
}

fn local_const_ty(proto: &FuncProto, idx: u32) -> LocalScalarTy {
    let Some(&v) = proto.constants.get(idx as usize) else {
        return LocalScalarTy::Unknown;
    };
    if v.is_number() {
        return LocalScalarTy::Num;
    }
    if v.is_heap() && (v.heap_index() & crate::vm::STRING_CONST_BIT) != 0 {
        let si = (v.heap_index() & !crate::vm::STRING_CONST_BIT) as usize;
        if si < proto.string_constants.len() {
            return LocalScalarTy::Str;
        }
    }
    LocalScalarTy::Unknown
}

/// Find the last `LoadInt` reaching `reg` at `ip` in a straight-line region.
fn reaching_int(code: &[Instr], start: usize, ip: usize, reg: u16) -> Option<i32> {
    for j in (start..ip).rev() {
        if local_sroa_dst(&code[j]) == Some(reg) {
            return match code[j] {
                Instr::LoadInt { val, .. } => Some(val),
                _ => None,
            };
        }
    }
    None
}

/// Plan scalar replacement for fresh, non-escaping object/array literals in a
/// single straight-line loop body.
///
/// This is deliberately narrower than general escape analysis:
///
/// * one fused numeric header test and one canonical back-edge, with no inner
///   branches or handlers;
/// * a fresh object is used only by unique-name `AppendDataProp` followed by
///   matching own `GetProp` reads;
/// * a fresh dense array is used only by constant, in-range `GetIndex` reads,
///   and its element registers remain unchanged through every possible bail;
/// * every remaining potentially observable heap read is exactly `.length` on
///   a statically-proved primitive string, and every `Add` operand is a proved
///   number/string primitive, so native helpers cannot invoke user code;
/// * scalar homes are registers unmentioned anywhere in the function. This
///   keeps them rooted for GC and activation-local across nested execution.
///
/// The rewritten proto preserves bytecode indices. On an internal native bail,
/// `Vm::try_run_osr` materializes allocations whose original allocation ip has
/// already executed, then resumes the untouched interpreter bytecode.
pub(crate) fn plan_local_sroa(
    proto: &FuncProto,
    start: u32,
    end: u32,
) -> Option<LocalSroaCompilePlan> {
    if proto.param_count != 0
        || proto.arguments_reg.is_some()
        || proto.is_generator
        || proto.is_async
        || !proto.eval_sites.is_empty()
    {
        return None;
    }
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
    if e <= s || e >= code.len() {
        return None;
    }
    match code[s] {
        Instr::JumpIfNotLt { target, .. } if target as usize == e + 1 => {}
        _ => return None,
    }
    match code[e] {
        Instr::Jump { target } if target == start => {}
        _ => return None,
    }
    if code[s + 1..e].iter().any(|i| {
        matches!(
            i,
            Instr::Jump { .. }
                | Instr::JumpIfFalse { .. }
                | Instr::JumpIfTrue { .. }
                | Instr::JumpIfNotLt { .. }
                | Instr::JumpIfNotLe { .. }
                | Instr::PushHandler { .. }
                | Instr::PopHandler
                | Instr::PushFinally { .. }
                | Instr::PopFinally
        )
    }) || code.iter().any(|i| !local_sroa_supported_instr(i))
    {
        return None;
    }

    // A completely-unmentioned register is an activation-local, GC-scanned
    // scalar home. Restricting the WHOLE function (not merely the region)
    // prevents a clean exit or catch continuation from observing our scratch.
    let mut mentioned = FxHashSet::default();
    mentioned.insert(0u16); // `this` is implicit even when no opcode names it.
    mentioned.insert(1u16); // return/temporary convention; keep it untouched.
    for instr in code {
        mentioned.extend(instr_uses(instr));
        if let Some(dst) = local_sroa_dst(instr) {
            mentioned.insert(dst);
        }
    }
    let spare: Vec<u16> = (2..proto.reg_count)
        .filter(|r| !mentioned.contains(r))
        .collect();

    // Discover fresh object lifetimes and prove every reference use belongs to
    // the literal builder or an own-field read. A prior iteration's reference
    // must be killed before the allocation register's first prefix use.
    let mut objects = Vec::new();
    for alloc_ip in s + 1..e {
        let (dst, hint) = match code[alloc_ip] {
            Instr::NewObject { dst, hint } => (dst, hint),
            Instr::NewPlannedObject { dst, plan } => {
                // SROA can erase the allocation/helper entirely, so it must
                // enforce the same metadata trust boundary itself instead of
                // relying on the interpreter/JIT allocation helper to reject
                // a duplicate or oversize hand-built plan.
                let plan = proto.static_key_plans.get(plan as usize)?;
                if !plan.runtime_valid() || plan.len() > 256 {
                    return None;
                }
                let hint = u16::try_from(plan.len()).ok()?;
                (dst, hint)
            }
            _ => continue,
        };
        let mut prefix_killed = false;
        for instr in &code[s..alloc_ip] {
            let used = instr_uses(instr).contains(&dst);
            let wrote = local_sroa_dst(instr) == Some(dst);
            if used && !prefix_killed {
                return None;
            }
            prefix_killed |= wrote;
        }
        let mut fields: Vec<(u32, u32)> = Vec::new(); // (name, append ip)
        for (ip, instr) in code.iter().enumerate().take(e + 1).skip(alloc_ip + 1) {
            if local_sroa_dst(instr) == Some(dst) {
                return None;
            }
            if !instr_uses(instr).contains(&dst) {
                continue;
            }
            match *instr {
                Instr::AppendDataProp { obj, name, val } if obj == dst && val != dst => {
                    let key = proto.string_constants.get(name as usize)?;
                    if fields
                        .iter()
                        .any(|&(n, _)| proto.string_constants.get(n as usize) == Some(key))
                    {
                        return None;
                    }
                    fields.push((name, ip as u32));
                }
                Instr::GetProp { obj, name, .. } if obj == dst => {
                    let key = proto.string_constants.get(name as usize)?;
                    if !fields
                        .iter()
                        .any(|&(n, _)| proto.string_constants.get(n as usize) == Some(key))
                    {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        // A clean loop exit does not materialize the object. Its original
        // register therefore must be dead until a post-region definition (or
        // function end), not merely unused by the loop body itself.
        for instr in &code[e + 1..] {
            if instr_uses(instr).contains(&dst) {
                return None;
            }
            if local_sroa_dst(instr) == Some(dst) {
                break;
            }
        }
        if fields.is_empty() || fields.len() != hint as usize {
            return None;
        }
        objects.push((alloc_ip as u32, dst, hint, fields));
    }

    // Dense arrays may forward an element read directly to its construction
    // argument only while that argument register is unchanged. A reaching
    // `LoadHole` (or any value we cannot prove user-visible) is rejected by the
    // scalar type walk below.
    let mut arrays: Vec<(u32, u16, u16, u16, Vec<(u32, u16)>)> = Vec::new();
    for alloc_ip in s + 1..e {
        let (dst, arg_base, argc) = match code[alloc_ip] {
            Instr::NewArray {
                dst,
                arg_base,
                argc,
            } if argc != 0 => (dst, arg_base, argc),
            _ => continue,
        };
        let mut prefix_killed = false;
        for instr in &code[s..alloc_ip] {
            let used = instr_uses(instr).contains(&dst);
            let wrote = local_sroa_dst(instr) == Some(dst);
            if used && !prefix_killed {
                return None;
            }
            prefix_killed |= wrote;
        }
        let mut reads = Vec::new();
        for (ip, instr) in code.iter().enumerate().take(e + 1).skip(alloc_ip + 1) {
            if local_sroa_dst(instr) == Some(dst) {
                return None;
            }
            if !instr_uses(instr).contains(&dst) {
                continue;
            }
            let (key, get_dst) = match *instr {
                Instr::GetIndex {
                    dst: get_dst,
                    obj,
                    key,
                } if obj == dst => (key, get_dst),
                _ => return None,
            };
            let idx = reaching_int(code, s, ip, key)?;
            if idx < 0 || idx >= argc as i32 {
                return None;
            }
            let src = arg_base.checked_add(idx as u16)?;
            if src == get_dst
                || code[alloc_ip + 1..=e]
                    .iter()
                    .any(|i| local_sroa_dst(i) == Some(src))
            {
                return None;
            }
            reads.push((ip as u32, src));
        }
        // As for objects, a clean exit leaves no real array in `dst`.
        for instr in &code[e + 1..] {
            if instr_uses(instr).contains(&dst) {
                return None;
            }
            if local_sroa_dst(instr) == Some(dst) {
                break;
            }
        }
        if reads.is_empty() {
            return None;
        }
        arrays.push((alloc_ip as u32, dst, arg_base, argc, reads));
    }

    // One-step `FinalizeObject` literals follow the ARRAY model, not the
    // append model: every field value already lives in a staged register that
    // eligibility proves unchanged through every possible bail, and each own
    // field read forwards directly to its staged slot. No scratch homes.
    type FinalizedSite = (u32, u16, u16, u16, u16, Vec<(u32, u16)>);
    let mut finalized: Vec<FinalizedSite> = Vec::new();
    for alloc_ip in s + 1..e {
        let (dst, plan_idx, val_base, count) = match code[alloc_ip] {
            Instr::FinalizeObject {
                dst,
                plan,
                val_base,
                count,
            } if count != 0 => (dst, plan, val_base, count),
            _ => continue,
        };
        // SROA erases the allocation, so it enforces the same metadata trust
        // boundary the interpreter arm would have.
        let plan = proto.static_key_plans.get(plan_idx as usize)?;
        if !plan.runtime_valid() || plan.len() > 256 || plan.len() != count as usize {
            return None;
        }
        let mut prefix_killed = false;
        for instr in &code[s..alloc_ip] {
            let used = instr_uses(instr).contains(&dst);
            let wrote = local_sroa_dst(instr) == Some(dst);
            if used && !prefix_killed {
                return None;
            }
            prefix_killed |= wrote;
        }
        let mut reads = Vec::new();
        for (ip, instr) in code.iter().enumerate().take(e + 1).skip(alloc_ip + 1) {
            if local_sroa_dst(instr) == Some(dst) {
                return None;
            }
            if !instr_uses(instr).contains(&dst) {
                continue;
            }
            let (name, get_dst) = match *instr {
                Instr::GetProp {
                    dst: get_dst,
                    obj,
                    name,
                } if obj == dst => (name, get_dst),
                _ => return None,
            };
            let key = proto.string_constants.get(name as usize)?;
            let field = plan.keys().iter().position(|k| k == key)?;
            let src = val_base.checked_add(u16::try_from(field).ok()?)?;
            if src == get_dst
                || code[alloc_ip + 1..=e]
                    .iter()
                    .any(|i| local_sroa_dst(i) == Some(src))
            {
                return None;
            }
            reads.push((ip as u32, src));
        }
        // As for arrays/objects, a clean loop exit leaves no real object.
        for instr in &code[e + 1..] {
            if instr_uses(instr).contains(&dst) {
                return None;
            }
            if local_sroa_dst(instr) == Some(dst) {
                break;
            }
        }
        if reads.is_empty() {
            return None;
        }
        finalized.push((alloc_ip as u32, dst, plan_idx, val_base, count, reads));
    }

    if objects.is_empty() && arrays.is_empty() && finalized.is_empty() {
        return None;
    }
    let nfields: usize = objects.iter().map(|o| o.3.len()).sum();
    if spare.len() < nfields {
        return None;
    }

    // Assign the otherwise-unused frame homes, then build ip lookup maps for
    // both the scalar type proof and the index-preserving rewrite.
    let mut runtime = LocalSroaPlan::default();
    let mut field_slot: FxHashMap<(usize, u32), u16> = FxHashMap::default();
    let mut obj_by_reg: FxHashMap<u16, usize> = FxHashMap::default();
    let mut scratch_i = 0usize;
    for (oid, (alloc_ip, dst, hint, fields)) in objects.iter().enumerate() {
        obj_by_reg.insert(*dst, oid);
        let mut out = Vec::with_capacity(fields.len());
        for &(name, append_ip) in fields {
            let scratch = spare[scratch_i];
            scratch_i += 1;
            field_slot.insert((oid, name), scratch);
            out.push(LocalSroaField {
                name,
                scratch,
                append_ip,
            });
            runtime.scratch.push(scratch);
        }
        runtime.objects.push(LocalSroaObject {
            alloc_ip: *alloc_ip,
            dst: *dst,
            hint: *hint,
            fields: out,
        });
    }
    let mut arr_by_reg: FxHashMap<u16, usize> = FxHashMap::default();
    let mut arr_reads: FxHashMap<u32, u16> = FxHashMap::default();
    for (aid, &(alloc_ip, dst, arg_base, argc, ref reads)) in arrays.iter().enumerate() {
        arr_by_reg.insert(dst, aid);
        for &(ip, src) in reads {
            arr_reads.insert(ip, src);
        }
        runtime.arrays.push(LocalSroaArray {
            alloc_ip,
            dst,
            arg_base,
            argc,
        });
    }
    let mut fin_by_reg: FxHashMap<u16, usize> = FxHashMap::default();
    let mut fin_reads: FxHashMap<u32, u16> = FxHashMap::default();
    for (fid_local, &(alloc_ip, dst, plan_idx, val_base, count, ref reads)) in
        finalized.iter().enumerate()
    {
        fin_by_reg.insert(dst, fid_local);
        for &(ip, src) in reads {
            fin_reads.insert(ip, src);
        }
        runtime.finalized.push(LocalSroaFinalized {
            alloc_ip,
            dst,
            plan_idx,
            val_base,
            count,
        });
    }

    // Static primitive proof. This is what makes the shared MEM emitter's
    // remaining Add/GetProp helpers non-reentrant: every Add is number/string
    // primitive-only, and every non-virtual property read is string `.length`.
    let mut tys = vec![LocalScalarTy::Unknown; proto.reg_count as usize];
    for instr in &code[..s] {
        match *instr {
            Instr::LoadInt { dst, .. } => tys[dst as usize] = LocalScalarTy::Num,
            Instr::LoadConst { dst, idx } => tys[dst as usize] = local_const_ty(proto, idx),
            Instr::Move { dst, src } => tys[dst as usize] = tys[src as usize],
            _ => return None,
        }
    }
    let mut field_ty: FxHashMap<(usize, u32), LocalScalarTy> = FxHashMap::default();
    let mut array_ty: Vec<Vec<LocalScalarTy>> = vec![Vec::new(); arrays.len()];
    let mut fin_ty: Vec<Vec<LocalScalarTy>> = vec![Vec::new(); finalized.len()];
    for (ip, instr) in code.iter().enumerate().take(e + 1).skip(s) {
        match *instr {
            Instr::JumpIfNotLt { a, b, .. } => {
                if tys[a as usize] != LocalScalarTy::Num || tys[b as usize] != LocalScalarTy::Num {
                    return None;
                }
            }
            Instr::LoadInt { dst, .. } => tys[dst as usize] = LocalScalarTy::Num,
            Instr::LoadConst { dst, idx } => tys[dst as usize] = local_const_ty(proto, idx),
            Instr::LoadUndefined { dst } => tys[dst as usize] = LocalScalarTy::Unknown,
            Instr::Move { dst, src } => tys[dst as usize] = tys[src as usize],
            Instr::AddInt { dst, a, .. } => {
                if tys[a as usize] != LocalScalarTy::Num {
                    return None;
                }
                tys[dst as usize] = LocalScalarTy::Num;
            }
            Instr::Mul { dst, a, b } | Instr::Bitwise { dst, a, b, .. } => {
                if tys[a as usize] != LocalScalarTy::Num || tys[b as usize] != LocalScalarTy::Num {
                    return None;
                }
                tys[dst as usize] = LocalScalarTy::Num;
            }
            Instr::Add { dst, a, b } => {
                tys[dst as usize] = match (tys[a as usize], tys[b as usize]) {
                    (LocalScalarTy::Num, LocalScalarTy::Num) => LocalScalarTy::Num,
                    (LocalScalarTy::Str, LocalScalarTy::Num)
                    | (LocalScalarTy::Num, LocalScalarTy::Str)
                    | (LocalScalarTy::Str, LocalScalarTy::Str) => LocalScalarTy::Str,
                    _ => return None,
                };
            }
            Instr::NewObject { dst, .. } | Instr::NewPlannedObject { dst, .. } => {
                let oid = *obj_by_reg.get(&dst)?;
                tys[dst as usize] = LocalScalarTy::Object(oid);
            }
            Instr::AppendDataProp { obj, name, val } => {
                let LocalScalarTy::Object(oid) = tys[obj as usize] else {
                    return None;
                };
                let ty = tys[val as usize];
                if !matches!(ty, LocalScalarTy::Num | LocalScalarTy::Str) {
                    return None;
                }
                field_ty.insert((oid, name), ty);
            }
            Instr::GetProp { dst, obj, name } => match tys[obj as usize] {
                LocalScalarTy::Object(oid) => {
                    let key = proto.string_constants.get(name as usize)?;
                    let (&(_, stored_name), &ty) =
                        field_ty.iter().find(|((field_oid, field_name), _)| {
                            *field_oid == oid
                                && proto.string_constants.get(*field_name as usize) == Some(key)
                        })?;
                    let _ = stored_name;
                    tys[dst as usize] = ty;
                }
                LocalScalarTy::Finalized(fid_local) => {
                    let src = *fin_reads.get(&(ip as u32))?;
                    let (_, _, _, val_base, _, _) = &finalized[fid_local];
                    let idx = src.checked_sub(*val_base)? as usize;
                    tys[dst as usize] = *fin_ty.get(fid_local)?.get(idx)?;
                }
                LocalScalarTy::Str
                    if proto
                        .string_constants
                        .get(name as usize)
                        .map(String::as_str)
                        == Some("length") =>
                {
                    tys[dst as usize] = LocalScalarTy::Num;
                }
                _ => return None,
            },
            Instr::NewArray {
                dst,
                arg_base,
                argc,
            } => {
                let aid = *arr_by_reg.get(&dst)?;
                let elems: Vec<_> = (0..argc).map(|i| tys[(arg_base + i) as usize]).collect();
                if elems
                    .iter()
                    .any(|t| !matches!(t, LocalScalarTy::Num | LocalScalarTy::Str))
                {
                    return None;
                }
                array_ty[aid] = elems;
                tys[dst as usize] = LocalScalarTy::Array(aid);
            }
            Instr::FinalizeObject {
                dst,
                val_base,
                count,
                ..
            } => {
                let fid_local = *fin_by_reg.get(&dst)?;
                let elems: Vec<_> = (0..count).map(|j| tys[(val_base + j) as usize]).collect();
                if elems
                    .iter()
                    .any(|t| !matches!(t, LocalScalarTy::Num | LocalScalarTy::Str))
                {
                    return None;
                }
                fin_ty[fid_local] = elems;
                tys[dst as usize] = LocalScalarTy::Finalized(fid_local);
            }
            Instr::GetIndex { dst, obj, .. } => {
                let LocalScalarTy::Array(aid) = tys[obj as usize] else {
                    return None;
                };
                let src = *arr_reads.get(&(ip as u32))?;
                let (_, _, arg_base, _, _) = &arrays[aid];
                let idx = src.checked_sub(*arg_base)? as usize;
                tys[dst as usize] = *array_ty.get(aid)?.get(idx)?;
            }
            Instr::Jump { .. } => {}
            _ => return None,
        }
    }

    let mut rewritten = proto.clone();
    for object in &runtime.objects {
        rewritten.code[object.alloc_ip as usize] = Instr::LoadInt {
            dst: object.dst,
            val: 0,
        };
        for field in &object.fields {
            let val = match code[field.append_ip as usize] {
                Instr::AppendDataProp { val, .. } => val,
                _ => return None,
            };
            rewritten.code[field.append_ip as usize] = Instr::Move {
                dst: field.scratch,
                src: val,
            };
        }
        for ip in s..=e {
            let Instr::GetProp { dst, obj, name } = code[ip] else {
                continue;
            };
            if obj != object.dst {
                continue;
            }
            let key = proto.string_constants.get(name as usize)?;
            let field = object
                .fields
                .iter()
                .find(|f| proto.string_constants.get(f.name as usize) == Some(key))?;
            rewritten.code[ip] = Instr::Move {
                dst,
                src: field.scratch,
            };
        }
    }
    for array in &runtime.arrays {
        rewritten.code[array.alloc_ip as usize] = Instr::LoadInt {
            dst: array.dst,
            val: 0,
        };
    }
    for (&ip, &src) in &arr_reads {
        let dst = match code[ip as usize] {
            Instr::GetIndex { dst, .. } => dst,
            _ => return None,
        };
        rewritten.code[ip as usize] = Instr::Move { dst, src };
    }
    for fin in &runtime.finalized {
        // A benign filler: nothing writes `dst` in the loop once the finalize
        // is elided, and indices must be preserved. The concat-length sub-lane
        // may later repurpose this exact slot for its shift-count load.
        rewritten.code[fin.alloc_ip as usize] = Instr::LoadInt {
            dst: fin.dst,
            val: 0,
        };
    }
    for (&ip, &src) in &fin_reads {
        let dst = match code[ip as usize] {
            Instr::GetProp { dst, .. } => dst,
            _ => return None,
        };
        rewritten.code[ip as usize] = Instr::Move { dst, src };
    }
    Some(LocalSroaCompilePlan {
        proto: rewritten,
        runtime,
    })
}

#[derive(Clone, Debug)]
struct LocalConcatLenCandidate {
    runtime: LocalSroaConcatLen,
    append_ip: u32,
    number_reg: u16,
    /// Register the rewritten projection (`AddInt` at `get_ip`) reads `n`
    /// from. The append form keeps `n` live in `number_reg` itself; the
    /// finalized form reads the staged slot, whose integrity to the region end
    /// is already part of object eligibility.
    projection_src: u16,
    shift_reg: u16,
    length_ip: u32,
    length_dst: u16,
    length_bias: i32,
}

fn next_local_def(code: &[Instr], after: usize, reg: u16) -> u32 {
    code.iter()
        .enumerate()
        .skip(after + 1)
        .find_map(|(ip, instr)| (local_sroa_dst(instr) == Some(reg)).then_some(ip as u32))
        .unwrap_or(u32::MAX)
}

/// Every use after `after` and before the next definition of `reg` must be one
/// of `allowed`. The defining instruction is checked for a read-before-write as
/// well (for example `r = r + 1`), which is the conservative deopt-safe rule.
fn local_uses_until_def_are(code: &[Instr], after: usize, reg: u16, allowed: &[usize]) -> bool {
    for (ip, instr) in code.iter().enumerate().skip(after + 1) {
        if instr_uses(instr).contains(&reg) && !allowed.contains(&ip) {
            return false;
        }
        if local_sroa_dst(instr) == Some(reg) {
            break;
        }
    }
    true
}

/// Return the UTF-16 length of a compile-time string only for the deliberately
/// tiny ASCII subset used by the virtual concat-length lane. WTF-8 marker
/// constants and non-ASCII text decline: their lengths are computable, but
/// widening this proof is unnecessary for the measured allocation kernel.
fn local_ascii_const_units(proto: &FuncProto, const_idx: u32) -> Option<usize> {
    let &v = proto.constants.get(const_idx as usize)?;
    if !v.is_heap() || (v.heap_index() & crate::vm::STRING_CONST_BIT) == 0 {
        return None;
    }
    let si = v.heap_index() & !crate::vm::STRING_CONST_BIT;
    if proto.wtf8_consts.binary_search(&si).is_ok() {
        return None;
    }
    let text = proto.string_constants.get(si as usize)?;
    text.is_ascii().then_some(text.len())
}

/// Fold one exact, locally scalar-replaced `"ascii" + (x & MASK)` field when
/// the produced string is observed solely through `.length`.
///
/// For `0 <= n <= 99`, decimal length is one below 10 and two otherwise. With
/// `L = prefix.len()` this is exactly:
///
/// `((n + ((L + 2) * 256 - 10)) >> 8)`.
///
/// Five already-elided/virtual instruction slots are reused, preserving every
/// bytecode ip for native guards. The field's otherwise-unused SROA scratch
/// holds `n` while native code runs. On an internal bail, `try_run_osr`
/// recreates the real concat and restores any still-live original destinations
/// before resuming the untouched interpreter bytecode.
///
/// This intentionally accepts exactly one candidate. Multiple virtual strings
/// could share ordinary compiler temporaries and need interference analysis;
/// declining them keeps the first lane small and fail-closed.
pub(crate) fn plan_local_concat_len(
    proto: &FuncProto,
    start: u32,
    end: u32,
    plan: &mut LocalSroaCompilePlan,
) -> bool {
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
    if e >= code.len() || (plan.runtime.objects.is_empty() && plan.runtime.finalized.is_empty()) {
        return false;
    }

    let mut candidates = Vec::new();
    for object in &plan.runtime.objects {
        for field in &object.fields {
            let append_ip = field.append_ip as usize;
            if append_ip < s + 4 || append_ip > e {
                continue;
            }
            let add_ip = append_ip - 1;
            let bit_ip = add_ip - 1;
            let mask_ip = add_ip - 2;
            let prefix_ip = add_ip - 3;

            let Instr::AppendDataProp { obj, name, val } = code[append_ip] else {
                continue;
            };
            if obj != object.dst || name != field.name {
                continue;
            }
            let Instr::Add {
                dst: add_dst,
                a: prefix_reg,
                b: number_reg,
            } = code[add_ip]
            else {
                continue;
            };
            if add_dst != val {
                continue;
            }
            let Instr::Bitwise {
                dst: bit_dst,
                b: mask_reg,
                op: crate::bytecode::BitwiseOp::And,
                ..
            } = code[bit_ip]
            else {
                continue;
            };
            let Instr::LoadInt {
                dst: mask_dst,
                val: mask,
            } = code[mask_ip]
            else {
                continue;
            };
            let Instr::LoadConst {
                dst: prefix_dst,
                idx: prefix_const,
            } = code[prefix_ip]
            else {
                continue;
            };
            if bit_dst != number_reg
                || mask_reg != mask_dst
                || prefix_reg != prefix_dst
                || !(0..=99).contains(&mask)
            {
                continue;
            }
            let Some(prefix_units) = local_ascii_const_units(proto, prefix_const) else {
                continue;
            };
            let bias = (prefix_units as i64 + 2)
                .checked_mul(256)
                .and_then(|v| v.checked_sub(10));
            let Some(bias) = bias else {
                continue;
            };
            if bias > i32::MAX as i64 || bias + mask as i64 > i32::MAX as i64 {
                continue;
            }

            // The scalarized field must have exactly one read, immediately
            // projected to primitive-string `.length`.
            let Some(field_key) = proto.string_constants.get(field.name as usize) else {
                continue;
            };
            let reads: Vec<(usize, u16)> = code[s..=e]
                .iter()
                .enumerate()
                .filter_map(|(off, instr)| match *instr {
                    Instr::GetProp { dst, obj, name }
                        if obj == object.dst
                            && proto.string_constants.get(name as usize) == Some(field_key) =>
                    {
                        Some((s + off, dst))
                    }
                    _ => None,
                })
                .collect();
            let [(get_ip, get_dst)] = reads.as_slice() else {
                continue;
            };
            let (get_ip, get_dst) = (*get_ip, *get_dst);
            let length_ip = get_ip + 1;
            if length_ip > e {
                continue;
            }
            let Instr::GetProp {
                dst: length_dst,
                obj: length_obj,
                name: length_name,
            } = code[length_ip]
            else {
                continue;
            };
            if length_obj != get_dst
                || length_dst == get_dst
                || proto
                    .string_constants
                    .get(length_name as usize)
                    .map(String::as_str)
                    != Some("length")
            {
                continue;
            }

            // Reused temporaries must not interfere. `length_dst == number_reg`
            // is the useful exact shape: the final projection consumes then
            // overwrites `n`. All other working registers stay distinct.
            let distinct = [
                prefix_reg,
                number_reg,
                mask_reg,
                add_dst,
                get_dst,
                field.scratch,
            ];
            if distinct
                .iter()
                .enumerate()
                .any(|(i, r)| distinct[..i].contains(r))
                || length_dst != number_reg
            {
                continue;
            }

            // The prefix and concat result have no observation beyond the Add
            // and field append respectively. The projected string is observed
            // only by `.length`. The mask register is dead after the And, so
            // its slot can safely carry shift-count 8 until the projection.
            if !local_uses_until_def_are(code, prefix_ip, prefix_reg, &[add_ip])
                || !local_uses_until_def_are(code, add_ip, add_dst, &[append_ip])
                || !local_uses_until_def_are(code, get_ip, get_dst, &[length_ip])
                || !local_uses_until_def_are(code, bit_ip, mask_reg, &[])
                || code[bit_ip + 1..length_ip]
                    .iter()
                    .any(|instr| local_sroa_dst(instr) == Some(number_reg))
                || code[bit_ip + 1..length_ip]
                    .iter()
                    .any(|instr| local_sroa_dst(instr) == Some(mask_reg))
            {
                continue;
            }

            candidates.push(LocalConcatLenCandidate {
                runtime: LocalSroaConcatLen {
                    prefix_const,
                    prefix_load_ip: prefix_ip as u32,
                    prefix_reg,
                    add_ip: add_ip as u32,
                    add_dst,
                    add_live_until: next_local_def(code, add_ip, add_dst),
                    get_ip: get_ip as u32,
                    get_dst,
                    get_live_until: next_local_def(code, get_ip, get_dst),
                    scratch: field.scratch,
                },
                append_ip: append_ip as u32,
                number_reg,
                projection_src: number_reg,
                shift_reg: mask_reg,
                length_ip: length_ip as u32,
                length_dst,
                length_bias: bias as i32,
            });
        }
    }

    // The same exact `"ascii" + (x & MASK)` fold for a field of a one-step
    // `FinalizeObject` literal. The staged slot doubles as the SROA scratch (n
    // lives there while native code runs, and bail materialization restores
    // the real string into it BEFORE any object materializes or the
    // interpreter re-reads the block). The shift-count load reuses the elided
    // finalize ip — its `LoadInt {dst, 0}` filler is not load-bearing, because
    // nothing reads the object register once every own-field read forwards.
    for fin in &plan.runtime.finalized {
        let Some(key_plan) = proto.static_key_plans.get(fin.plan_idx as usize) else {
            continue;
        };
        for (j, field_key) in key_plan.keys().iter().enumerate() {
            let Some(slot) = fin.val_base.checked_add(j as u16) else {
                continue;
            };
            let adds: Vec<usize> = code[s..=e]
                .iter()
                .enumerate()
                .filter_map(|(off, instr)| {
                    matches!(instr, Instr::Add { dst, .. } if *dst == slot).then_some(s + off)
                })
                .collect();
            let [add_ip] = adds.as_slice() else {
                continue;
            };
            let add_ip = *add_ip;
            if add_ip < s + 4 || add_ip >= fin.alloc_ip as usize {
                continue;
            }
            let bit_ip = add_ip - 1;
            let mask_ip = add_ip - 2;
            let prefix_ip = add_ip - 3;
            let Instr::Add {
                dst: add_dst,
                a: prefix_reg,
                b: number_reg,
            } = code[add_ip]
            else {
                continue;
            };
            debug_assert_eq!(add_dst, slot);
            let Instr::Bitwise {
                dst: bit_dst,
                b: mask_reg,
                op: crate::bytecode::BitwiseOp::And,
                ..
            } = code[bit_ip]
            else {
                continue;
            };
            let Instr::LoadInt {
                dst: mask_dst,
                val: mask,
            } = code[mask_ip]
            else {
                continue;
            };
            let Instr::LoadConst {
                dst: prefix_dst,
                idx: prefix_const,
            } = code[prefix_ip]
            else {
                continue;
            };
            if bit_dst != number_reg
                || mask_reg != mask_dst
                || prefix_reg != prefix_dst
                || !(0..=99).contains(&mask)
            {
                continue;
            }
            let Some(prefix_units) = local_ascii_const_units(proto, prefix_const) else {
                continue;
            };
            let bias = (prefix_units as i64 + 2)
                .checked_mul(256)
                .and_then(|v| v.checked_sub(10));
            let Some(bias) = bias else {
                continue;
            };
            if bias > i32::MAX as i64 || bias + mask as i64 > i32::MAX as i64 {
                continue;
            }

            let reads: Vec<(usize, u16)> = code[s..=e]
                .iter()
                .enumerate()
                .filter_map(|(off, instr)| match *instr {
                    Instr::GetProp { dst, obj, name }
                        if obj == fin.dst
                            && proto.string_constants.get(name as usize) == Some(field_key) =>
                    {
                        Some((s + off, dst))
                    }
                    _ => None,
                })
                .collect();
            let [(get_ip, get_dst)] = reads.as_slice() else {
                continue;
            };
            let (get_ip, get_dst) = (*get_ip, *get_dst);
            let length_ip = get_ip + 1;
            if length_ip > e {
                continue;
            }
            let Instr::GetProp {
                dst: length_dst,
                obj: length_obj,
                name: length_name,
            } = code[length_ip]
            else {
                continue;
            };
            if length_obj != get_dst
                || length_dst == get_dst
                || proto
                    .string_constants
                    .get(length_name as usize)
                    .map(String::as_str)
                    != Some("length")
            {
                continue;
            }

            // Interference differs from the append form in two ways. The
            // staged slot IS the scratch, so it appears once. And the shift
            // count's home is the DEAD object register `fin.dst` (loaded at
            // the elided finalize ip) rather than the mask register — the
            // compiler's tighter staged layout may reuse the mask register
            // for a later destination, and eligibility already proves
            // `fin.dst` has no other def or use once every field read
            // forwards. The mask register therefore needs no liveness beyond
            // its own And, and `length_dst` may be any register except the
            // projection's own source.
            let distinct = [prefix_reg, number_reg, slot, get_dst];
            if distinct
                .iter()
                .enumerate()
                .any(|(i, r)| distinct[..i].contains(r))
                || length_dst == get_dst
                || fin.dst == number_reg
                || fin.dst == get_dst
                || fin.dst == slot
                || fin.dst == prefix_reg
            {
                continue;
            }

            // `n` needs to survive only from the And to the rewritten Move at
            // `add_ip`; the projection reads the staged SLOT, so the slot must
            // have no other definition through the projection (the object
            // eligibility pass already declined any def past the finalize).
            if !local_uses_until_def_are(code, prefix_ip, prefix_reg, &[add_ip])
                || !local_uses_until_def_are(code, add_ip, slot, &[fin.alloc_ip as usize])
                || !local_uses_until_def_are(code, get_ip, get_dst, &[length_ip])
                || code[bit_ip + 1..=add_ip]
                    .iter()
                    .any(|instr| local_sroa_dst(instr) == Some(number_reg))
                || code[add_ip + 1..length_ip]
                    .iter()
                    .any(|instr| local_sroa_dst(instr) == Some(slot))
            {
                continue;
            }

            candidates.push(LocalConcatLenCandidate {
                runtime: LocalSroaConcatLen {
                    prefix_const,
                    prefix_load_ip: prefix_ip as u32,
                    prefix_reg,
                    add_ip: add_ip as u32,
                    add_dst: slot,
                    add_live_until: next_local_def(code, add_ip, slot),
                    get_ip: get_ip as u32,
                    get_dst,
                    get_live_until: next_local_def(code, get_ip, get_dst),
                    scratch: slot,
                },
                append_ip: fin.alloc_ip,
                number_reg,
                projection_src: slot,
                shift_reg: fin.dst,
                length_ip: length_ip as u32,
                length_dst,
                length_bias: bias as i32,
            });
        }
    }
    let [candidate] = candidates.as_slice() else {
        return false;
    };
    let candidate = candidate.clone();

    plan.proto.code[candidate.runtime.prefix_load_ip as usize] = Instr::LoadInt {
        dst: candidate.runtime.prefix_reg,
        val: 0,
    };
    plan.proto.code[candidate.runtime.add_ip as usize] = Instr::Move {
        dst: candidate.runtime.scratch,
        src: candidate.number_reg,
    };
    plan.proto.code[candidate.append_ip as usize] = Instr::LoadInt {
        dst: candidate.shift_reg,
        val: 8,
    };
    plan.proto.code[candidate.runtime.get_ip as usize] = Instr::AddInt {
        dst: candidate.runtime.get_dst,
        a: candidate.projection_src,
        imm: candidate.length_bias,
        upd: false,
    };
    plan.proto.code[candidate.length_ip as usize] = Instr::Bitwise {
        dst: candidate.length_dst,
        a: candidate.runtime.get_dst,
        b: candidate.shift_reg,
        op: crate::bytecode::BitwiseOp::Shr,
    };
    plan.runtime.concat_lens.push(candidate.runtime);
    true
}

/// Plan for promoting a single stable object's fields to registers (SROA-lite,
/// the effect of V8's escape-analysis + scalar replacement): when EVERY
/// GetProp/SetProp in a region targets the SAME object — a global `obj_global`
/// loaded by `LoadGlobal` and never re-stored in the region, and whose ref reg
/// is used ONLY as the GetProp/SetProp receiver — its accessed fields can live in
/// registers for the loop body, synced to the heap object only at region
/// entry/exit, so the loop becomes register-only like V8.
#[allow(dead_code)] // wired into codegen in a following step
pub(crate) struct FieldPromotePlan {
    /// The global slot holding the promoted object.
    pub(crate) obj_global: u32,
    /// Distinct accessed field name-constant indices, in first-seen order. Each
    /// maps to a synthetic "field global" the heap ops are rewritten to use.
    pub(crate) fields: Vec<u32>,
    /// Heap index of the live promoted object at compile time (an identity guard
    /// at region entry: the global could be reassigned to a different object).
    pub(crate) obj_idx: u32,
    /// The object's heap version at compile time. A key add/remove/redefine,
    /// freeze, or `setPrototypeOf` bumps it, so a mismatch at region entry means
    /// the validated all-own-data-slot shape may no longer hold (a field could
    /// have become an accessor / non-writable / inherited) — bail to the
    /// interpreter. See memory: sroa-accessor-miscompile.
    pub(crate) obj_version: u32,
}

/// Detect whether `[start, end]` is field-promotable; see `FieldPromotePlan`.
/// `globals`/`heap` give the live runtime shape so we can reject a receiver
/// whose promoted fields aren't all OWN non-accessor data slots (an accessor /
/// inherited / non-writable field would diverge — see sroa-accessor-miscompile).
#[allow(dead_code)] // wired into codegen in a following step
pub(crate) fn plan_field_promotion(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals: &[Value],
    heap: &crate::heap::Heap,
) -> Option<FieldPromotePlan> {
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
    if !code[s..=e]
        .iter()
        .any(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }))
    {
        return None;
    }

    // Single-def map (for tracing an obj-ref reg to its LoadGlobal).
    let mut reg_def: FxHashMap<u16, usize> = FxHashMap::default();
    let mut reg_def_count: FxHashMap<u16, u32> = FxHashMap::default();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if let Some(d) = writes_reg(instr) {
            reg_def.insert(d, s + off);
            *reg_def_count.entry(d).or_insert(0) += 1;
        }
    }

    // Every heap-op receiver must be the SAME global object, loaded once.
    let mut obj_global: Option<u32> = None;
    let mut obj_ref_regs: FxHashSet<u16> = FxHashSet::default();
    let mut fields: Vec<u32> = Vec::new();
    for instr in &code[s..=e] {
        let (obj_reg, name) = match *instr {
            Instr::GetProp { obj, name, .. } => (obj, name),
            Instr::SetProp { obj, name, .. } => (obj, name),
            _ => continue,
        };
        let def_ip = *reg_def.get(&obj_reg)?; // must be defined in the region
        if reg_def_count.get(&obj_reg) != Some(&1) {
            return None; // multiple defs → can't trace
        }
        let g = match code[def_ip] {
            Instr::LoadGlobal { idx, .. } => idx,
            _ => return None, // receiver isn't a plain global load
        };
        match obj_global {
            None => obj_global = Some(g),
            Some(prev) if prev == g => {}
            Some(_) => return None, // two different objects at the site set
        }
        obj_ref_regs.insert(obj_reg);
        // Dedup by the field STRING, not the name-constant INDEX: the compiler
        // emits a distinct string-constant per occurrence, so `o.a` read and
        // `o.a` write have DIFFERENT name indices for the SAME field. Keying by
        // index would give them separate pool slots (the read wouldn't see the
        // write). Keep one representative index per distinct field string.
        let fname = &proto.string_constants[name as usize];
        // `length` is a SPECIAL property (an array's element count / a string's
        // length), not a plain stored slot. Scalar-replacing it diverges from the
        // interpreter — e.g. `arr.length = n` truncates the array, but a promoted
        // scalar would just track a dead pool slot. Decline; the inline-cache /
        // helper path handles `.length` correctly (read) and deopts the write.
        if fname == "length" {
            return None;
        }
        if !fields
            .iter()
            .any(|&n| proto.string_constants[n as usize] == *fname)
        {
            fields.push(name);
        }
    }
    let g = obj_global?;

    // The object ref must be stable (G not re-stored) and its ref reg must not
    // escape (used only as the GetProp/SetProp receiver, nowhere else).
    for instr in &code[s..=e] {
        if let Instr::StoreGlobal { idx, .. }
        | Instr::StoreGlobalStrict { idx, .. }
        | Instr::StoreGlobalResolved { idx, .. } = *instr
        {
            if idx == g {
                return None;
            }
        }
        // EVERY load of the promoted object must feed a heap op only — if `g` is
        // also loaded into a register that is NOT a heap-op receiver, that ref
        // could escape (be stored, or used numerically), so the object isn't
        // provably confined to the rewritten accesses. Decline (→ inline cache).
        if let Instr::LoadGlobal { dst, idx } = *instr {
            if idx == g && !obj_ref_regs.contains(&dst) {
                return None;
            }
        }
        if matches!(instr, Instr::GetProp { .. } | Instr::SetProp { .. }) {
            continue;
        }
        for u in instr_uses(instr) {
            if obj_ref_regs.contains(&u) {
                return None; // ref reg used outside a heap op → object escapes
            }
        }
    }

    // ── runtime shape check ── SROA scalar-replaces each field with a pool slot,
    // bypassing any getter/setter AND the property's writability. It is sound
    // ONLY when the live global is a plain object whose every promoted field is
    // an OWN, non-accessor DATA slot (writable if the region stores to it). An
    // inherited field, an accessor (a class get/set or a defineProperty accessor),
    // or a non-writable store target would diverge from the interpreter — which
    // runs the accessor / honours non-writability each iteration while the scalar
    // pool would not. (Found by the accessor-inline audit 2026-06-14; class
    // get/set live on the PROTOTYPE so `m.pos` returns None and we decline.)
    let gv = *globals.get(g as usize)?;
    if !gv.is_heap() {
        return None;
    }
    let obj_idx = gv.heap_index();
    let m = match heap.get(obj_idx) {
        crate::heap::HeapObj::Object(m) => m,
        _ => return None, // arrays / typed-arrays / fns aren't plain-field SROA targets
    };
    for &name_idx in &fields {
        let fname = &proto.string_constants[name_idx as usize];
        // Writability is required only if the region STORES to this field.
        let need_writable = code[s..=e].iter().any(|instr| match *instr {
            Instr::SetProp { name, .. } => proto.string_constants[name as usize] == *fname,
            _ => false,
        });
        match m.pos(fname) {
            Some(slot)
                if !m.attr_at(slot).accessor && (!need_writable || m.attr_at(slot).writable) => {}
            _ => return None,
        }
    }
    Some(FieldPromotePlan {
        obj_global: g,
        fields,
        obj_idx,
        obj_version: heap.version_of(obj_idx),
    })
}

#[cfg(test)]
mod local_sroa_tests {
    use super::*;

    const BODY: &str = r#"
        function main() {
          const rounds = 100;
          let checksum = 0;
          for (let i = 0; i < rounds; i++) {
            const point = { x: i & 15, y: (i * 3) & 31, tag: "p" + (i & 7) };
            const pair = [point.x + point.y, point.tag.length];
            checksum = (checksum + pair[0] + pair[1]) | 0;
          }
          console.log(checksum);
        }
        main();
    "#;

    fn main_proto(src: &str) -> FuncProto {
        let ast = crate::front::parse_script(src).expect("parse");
        let program = crate::compile::compile_program(&ast, src).expect("compile");
        program
            .functions
            .into_iter()
            .find(|f| f.name == "main")
            .expect("main proto")
    }

    fn loop_bounds(proto: &FuncProto) -> (u32, u32) {
        proto
            .code
            .iter()
            .enumerate()
            .find_map(|(ip, instr)| match *instr {
                Instr::Jump { target } if (target as usize) < ip => Some((target, ip as u32)),
                _ => None,
            })
            .expect("loop backedge")
    }

    #[test]
    fn local_sroa_rewrites_the_exact_fresh_object_and_dense_array_lane() {
        let proto = main_proto(BODY);
        let (start, end) = loop_bounds(&proto);
        let plan = plan_local_sroa(&proto, start, end).expect("eligible local aggregate lane");
        assert_eq!(plan.runtime.objects.len(), 1);
        assert_eq!(plan.runtime.arrays.len(), 1);
        assert_eq!(plan.runtime.objects[0].fields.len(), 3);
        assert_eq!(plan.runtime.scratch.len(), 3);
        assert!(!plan.proto.code[start as usize..=end as usize]
            .iter()
            .any(|i| matches!(
                i,
                Instr::NewObject { .. }
                    | Instr::NewPlannedObject { .. }
                    | Instr::NewArray { .. }
                    | Instr::AppendDataProp { .. }
            )));
        // `point.tag.length` remains a real primitive-string length read; the
        // three point reads and two pair reads have become Move instructions.
        assert_eq!(
            plan.proto.code[start as usize..=end as usize]
                .iter()
                .filter(|i| matches!(i, Instr::GetProp { .. }))
                .count(),
            1
        );
        assert!(!plan.proto.code[start as usize..=end as usize]
            .iter()
            .any(|i| matches!(i, Instr::GetIndex { .. })));
    }

    #[test]
    fn local_concat_length_virtualizes_only_the_exact_single_observation() {
        let proto = main_proto(BODY);
        let (start, end) = loop_bounds(&proto);
        let mut plan = plan_local_sroa(&proto, start, end).expect("eligible local aggregate lane");
        assert!(plan_local_concat_len(&proto, start, end, &mut plan));
        assert_eq!(plan.runtime.concat_lens.len(), 1);
        let concat = &plan.runtime.concat_lens[0];
        assert!(matches!(
            plan.proto.code[concat.add_ip as usize],
            Instr::Move { dst, .. } if dst == concat.scratch
        ));
        assert!(matches!(
            plan.proto.code[concat.get_ip as usize],
            Instr::AddInt { imm: 758, .. }
        ));
        assert!(matches!(
            plan.proto.code[concat.get_ip as usize + 1],
            Instr::Bitwise {
                op: crate::bytecode::BitwiseOp::Shr,
                ..
            }
        ));
        assert!(!plan.proto.code[start as usize..=end as usize]
            .iter()
            .any(|i| matches!(i, Instr::GetProp { .. })));

        // Two reads mean two separately-live primitive strings. The aggregate
        // lane remains valid, but the intentionally single-candidate concat
        // fold declines without partially rewriting it.
        let repeated = BODY.replace(
            "point.tag.length];",
            "point.tag.length + point.tag.length];",
        );
        let proto = main_proto(&repeated);
        let (start, end) = loop_bounds(&proto);
        let mut plan = plan_local_sroa(&proto, start, end).expect("base SROA remains eligible");
        assert!(!plan_local_concat_len(&proto, start, end, &mut plan));
        assert!(plan.runtime.concat_lens.is_empty());
        assert!(plan.proto.code[start as usize..=end as usize]
            .iter()
            .any(|i| matches!(i, Instr::GetProp { .. })));
    }

    #[test]
    fn local_concat_length_declines_wide_masks_and_non_ascii_prefixes() {
        for src in [
            BODY.replace("i & 7)", "i & 127)"),
            BODY.replace("tag: \"p\"", "tag: \"\u{00e9}\""),
        ] {
            let proto = main_proto(&src);
            let (start, end) = loop_bounds(&proto);
            let mut plan =
                plan_local_sroa(&proto, start, end).expect("base local SROA remains eligible");
            assert!(!plan_local_concat_len(&proto, start, end, &mut plan));
            assert!(plan.runtime.concat_lens.is_empty());
        }
    }

    #[test]
    fn local_sroa_declines_alias_escape_holes_and_reentrant_initializers() {
        let escaped = BODY.replace(
            "const pair = [point.x + point.y, point.tag.length];",
            "const alias = point; const pair = [alias.x + point.y, point.tag.length];",
        );
        let proto = main_proto(&escaped);
        let (start, end) = loop_bounds(&proto);
        assert!(plan_local_sroa(&proto, start, end).is_none());

        let hole = BODY.replace(
            "const pair = [point.x + point.y, point.tag.length];",
            "const pair = [, point.tag.length];",
        );
        let proto = main_proto(&hole);
        let (start, end) = loop_bounds(&proto);
        assert!(plan_local_sroa(&proto, start, end).is_none());

        let call = BODY.replace("tag: \"p\" + (i & 7)", "tag: String(i)");
        let proto = main_proto(&call);
        let (start, end) = loop_bounds(&proto);
        assert!(plan_local_sroa(&proto, start, end).is_none());
    }

    #[test]
    fn local_sroa_declines_an_array_element_register_overwrite() {
        let mut proto = main_proto(BODY);
        let (start, end) = loop_bounds(&proto);
        let (new_ip, arg_base) = proto.code[start as usize..=end as usize]
            .iter()
            .enumerate()
            .find_map(|(off, instr)| match *instr {
                Instr::NewArray { arg_base, .. } => Some((start as usize + off, arg_base)),
                _ => None,
            })
            .expect("new array");
        let clobber = proto.code[new_ip + 1..=end as usize]
            .iter()
            .position(|i| matches!(i, Instr::LoadInt { .. }))
            .map(|off| new_ip + 1 + off)
            .expect("post-allocation constant");
        proto.code[clobber] = Instr::LoadInt {
            dst: arg_base,
            val: 0,
        };
        assert!(plan_local_sroa(&proto, start, end).is_none());
    }

    #[test]
    fn local_sroa_declines_a_clean_exit_that_observes_the_elided_reference() {
        let mut proto = main_proto(BODY);
        let (start, end) = loop_bounds(&proto);
        let object_dst = proto.code[start as usize..=end as usize]
            .iter()
            .find_map(|instr| match *instr {
                Instr::NewObject { dst, .. } | Instr::NewPlannedObject { dst, .. } => Some(dst),
                _ => None,
            })
            .expect("new object");
        let post_move = proto.code[end as usize + 1..]
            .iter()
            .position(|i| matches!(i, Instr::Move { .. }))
            .map(|off| end as usize + 1 + off)
            .expect("post-loop move");
        let dst = match proto.code[post_move] {
            Instr::Move { dst, .. } => dst,
            _ => unreachable!(),
        };
        proto.code[post_move] = Instr::Move {
            dst,
            src: object_dst,
        };
        assert!(plan_local_sroa(&proto, start, end).is_none());
    }
}
