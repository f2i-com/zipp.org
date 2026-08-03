// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Decline this region, naming the reason under `ZIPP_JITDECLINE=1`. The planner
/// has ~25 exit points and `ZIPP_JITLOG` only reports `plan_region=None`, which
/// says a region missed the fastest tier but not what to fix. Diagnostic only —
/// the env lookup happens once per declined region-compile, never per iteration.
macro_rules! decline {
    ($reason:expr) => {{
        if std::env::var_os("ZIPP_JITDECLINE").is_some() {
            eprintln!("[decline-reason] {}", $reason);
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
    if std::env::var_os("ZIPP_JITDECLINE").is_some() {
        eprintln!("[decline-reason] {reason}");
    }
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

pub(crate) fn plan_region(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    admit_bitwise: bool,
    admit_split: bool,
    admit_wt_share: bool,
) -> Option<RegionPlan> {
    plan_region_cold(
        proto,
        start,
        end,
        ta_plan,
        admit_bitwise,
        admit_split,
        admit_wt_share,
        &FxHashSet::default(),
    )
}

/// `cold`: ips the caller will emit as SIDE EXITS (B9) rather than as native
/// code. They are skipped by every analysis pass here — they never execute
/// natively, so they neither define a home's value nor constrain its type, and
/// letting them into the walks would only make the planner decline a region
/// whose hot path is perfectly typed.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_region_cold(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    admit_bitwise: bool,
    admit_split: bool,
    admit_wt_share: bool,
    cold: &FxHashSet<usize>,
) -> Option<RegionPlan> {
    match plan_region_cold_inner(
        proto, start, end, ta_plan, admit_bitwise, admit_split, admit_wt_share, cold, true,
    ) {
        PlanOutcome::Plan(p) => Some(*p),
        PlanOutcome::RetryNoHoist => {
            match plan_region_cold_inner(
                proto, start, end, ta_plan, admit_bitwise, admit_split, admit_wt_share, cold,
                false,
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
    cold: &FxHashSet<usize>,
    // See `PlanOutcome::RetryNoHoist`: `false` on the retry pass — no constant
    // is hoisted, so none pins a permanent home.
    allow_hoist: bool,
) -> PlanOutcome {
    let code = &proto.code;
    let (s, e) = (start as usize, end as usize);
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
    let pin_kind: u8 = if admit_bitwise { 5 } else { 8 };
    let pinned_elem = |ip: usize| -> bool {
        ta_plan.access.get(&ip).map_or(false, |&j| {
            let k = ta_plan.pins[j as usize].kind;
            // A dense all-Int ARRAY read joins the int path on the same terms as a
            // kind-5 TypedArray: receiver via the pin, index via `key`'s home, the
            // element unboxed into an i64 home under a per-access tag guard. This is
            // what admits `for (i < a.length) s += a[i]` — the most common hot loop
            // in JS, and previously demoted to the boxed memory path in full.
            if k == pin_kind || (admit_bitwise && k == ARR_INT_PIN_KIND) {
                return true;
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
            !admit_bitwise
                && (k == ARR_INT_PIN_KIND || k == ARR_NUM_PIN_KIND || (arr_pin_loose() && is_arr_pin(k)))
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
    // `arr.length` on a dense all-Int Array pin — like `str.length`, the receiver is
    // read from the pin snapshot rather than a register, so its reg must be excluded
    // from typing/homing too.
    let pinned_arr_len = |ip: usize| -> bool {
        admit_bitwise
            && ta_plan
                .access
                .get(&ip)
                .map_or(false, |&j| ta_plan.pins[j as usize].kind == ARR_INT_PIN_KIND)
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
        !admit_bitwise
            && dv_double_enabled()
            && ta_plan
                .access
                .get(&ip)
                .map_or(false, |&j| ta_plan.pins[j as usize].kind == DV_PIN_KIND)
            && matches!(code[ip], Instr::CallMethod { name, argc, .. }
                if (argc == 1 || argc == 2)
                    && proto
                        .string_constants
                        .get(name as usize)
                        .is_some_and(|k| dv_get_kind(k).is_some()))
    };
    let mut ta_recv_regs: FxHashSet<u16> = FxHashSet::default();
    // B94 recycled receivers (see `plan::RegionPlan::split_recvs`). At most one
    // whose pinned accesses are ELEMENT ops; a receiver whose pinned accesses
    // are all DV `get*` CallMethods is exempt from that limit — each split is
    // proven and written through independently.
    let mut split_recvs: FxHashSet<u16> = FxHashSet::default();
    let mut non_dv_split_used = false;
    let mut write_through: FxHashSet<u16> = FxHashSet::default();
    let mut split_recv_lg: FxHashSet<usize> = FxHashSet::default();
    let mut recv_glob: Option<u32> = None;
    let mut split_all_dv = false;
    {
        // Candidate receiver regs: the `obj` of every pinned index op, and the
        // `obj` of every pinned-STRING charCodeAt/length access.
        let mut recv: FxHashSet<u16> = FxHashSet::default();
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) { continue; }
            if pinned_elem(s + off) {
                if let Instr::GetIndex { obj, .. } | Instr::SetIndex { obj, .. } = *instr {
                    recv.insert(obj);
                }
            }
            if pinned_str(s + off) || pinned_arr_len(s + off) || pinned_dv(s + off) {
                if let Instr::CallMethod { obj, .. } | Instr::GetProp { obj, .. } = *instr {
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
                if cold.contains(&(s + off)) { continue; }
                if let Some(d) = writes_reg(instr) {
                    *def_n.entry(d).or_insert(0) += 1;
                    if matches!(instr, Instr::LoadGlobal { .. }) {
                        def_lg.insert(d);
                    }
                }
            }
            let mut used_elsewhere: FxHashSet<u16> = FxHashSet::default();
            for (off, instr) in code[s..=e].iter().enumerate() {
                if cold.contains(&(s + off)) { continue; }
                // The receiver use AT a pinned access is exempt (read via the pin,
                // not the register). For a pinned-STRING access the obj-use is also
                // invisible to instr_uses today (CallMethod→vec![]; GetProp→[obj]),
                // so the CallMethod half is forward-defensive; the GetProp half is
                // the one that actually exempts a dual-use string receiver.
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
                        if pinned_str(s + off) || pinned_arr_len(s + off) || pinned_dv(s + off) =>
                    {
                        Some(obj)
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
                for u in instr_uses(instr) {
                    if Some(u) != idx_obj && recv.contains(&u) {
                        used_elsewhere.insert(u);
                    }
                }
            }
            for &r in &recv {
                // Cleanly excludable when EITHER: (a) defined by exactly one
                // LoadGlobal and used only as a pinned obj (the global-receiver
                // case); OR (b) a live-in PARAM receiver — ZERO in-region defs and
                // used only as a pinned obj (`str` in fnv1a is reg1, the param,
                // read only via the STR pin). `def_n.get(&r).is_none()` (NOT
                // `!= Some(&1)`) is load-bearing: a multiply-defined reg must NOT
                // be admitted (it would need a real home).
                let clean_global =
                    def_n.get(&r) == Some(&1) && def_lg.contains(&r) && !used_elsewhere.contains(&r);
                let clean_param = def_n.get(&r).is_none() && !used_elsewhere.contains(&r);
                if clean_global || clean_param {
                    ta_recv_regs.insert(r);
                } else if admit_split && def_lg.contains(&r) && {
                    // Every pinned access with THIS receiver must read its
                    // identity from a global slot, and all from the SAME one.
                    // A `TaPinSrc::Reg` pin reads `[rbx + dreg(r)]`, which the
                    // numeric half of a recycled register also owns; that case
                    // is not separable and declines below.
                    let mut g0: Option<u32> = None;
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
                            _ => None,
                        };
                        if let Some((obj, is_dv)) = pin_obj {
                            if obj != r {
                                continue;
                            }
                            all_dv &= is_dv;
                            match ta_plan.access.get(&(s + off)).map(|&j| ta_plan.pins[j as usize].src) {
                                Some(TaPinSrc::Global(g)) if g0.is_none() || g0 == Some(g) => g0 = Some(g),
                                _ => ok = false,
                            }
                        }
                    }
                    if let Some(g) = g0.filter(|_| ok) {
                        recv_glob = Some(g);
                    }
                    split_all_dv = all_dv;
                    // The single-split budget applies to ELEMENT-pinned
                    // receivers only (B94's exercised case); DV-pinned
                    // receivers split independently — the DV swizzle loop
                    // recycles two of them, and a one-split rule declined it.
                    ok && g0.is_some() && (all_dv || !non_dv_split_used)
                } {
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
                    let g = recv_glob.unwrap();
                    let recv_lg_ips: FxHashSet<usize> = (s..=e)
                        .filter(|&ip| matches!(code[ip], Instr::LoadGlobal { dst, idx } if dst == r && idx == g))
                        .collect();
                    let recv_use_at = |ip: usize| -> Option<u16> {
                        match code[ip] {
                            Instr::GetIndex { obj, .. } | Instr::SetIndex { obj, .. }
                                if pinned_elem(ip) =>
                            {
                                Some(obj)
                            }
                            Instr::CallMethod { obj, .. } | Instr::GetProp { obj, .. }
                                if pinned_str(ip) || pinned_arr_len(ip) || pinned_dv(ip) =>
                            {
                                Some(obj)
                            }
                            Instr::ToPropKey { obj, .. } => Some(obj),
                            _ => None,
                        }
                    };
                    if !recv_lg_ips.is_empty()
                        && crate::codegen::plan::split_home_provably_safe(
                            code, s, e, r, cold, &recv_lg_ips, &recv_use_at,
                        )
                    {
                        split_recvs.insert(r);
                        split_recv_lg.extend(recv_lg_ips);
                        if !split_all_dv {
                            non_dv_split_used = true;
                        }
                    } else {
                        decline!("split receiver: home not provably live at a use");
                    }
                } else {
                    // A receiver register reused for other (numeric) values can't be
                    // cleanly excluded under the non-SSA register model → memory path.
                    // (B94 handles element-pinned splits one at a time above; a SECOND
                    // element split in the same region has never been exercised, so it
                    // still declines. DV-pinned splits are exempt from that budget.)
                    decline!("pinned receiver reg not cleanly excludable");
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
            if let Instr::CallMethod { arg_base, argc: 2, name, .. } = *instr {
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
                let tainted_use = (cip + 1..=m).any(|ip2| {
                    !cold.contains(&ip2) && instr_uses(&code[ip2]).contains(&f)
                });
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

    let mut ty: FxHashMap<u16, VTy> = FxHashMap::default();
    let mut first_seen: FxHashMap<u16, bool> = FxHashMap::default(); // reg → was first occurrence a def?
    let mut glob_first_read: FxHashMap<u32, bool> = FxHashMap::default(); // slot → first touch was a read?
    let mut reg_order: Vec<u16> = Vec::new();
    let mut glob_order: Vec<u32> = Vec::new();

    // Record a use (operand) of reg `r` with required type `req`.
    // Returns false on a type conflict (caller declines).
    let note_def = |r: u16, t: VTy, ty: &mut FxHashMap<u16, VTy>, first_seen: &mut FxHashMap<u16, bool>, reg_order: &mut Vec<u16>| -> bool {
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
        if cold.contains(&(s + off)) { continue; }
        // A call or a bitwise op can't be register-allocated (boxed Values / int32
        // lanes / arbitrary user code) — decline to the memory path. A dense-array
        // GetIndex/SetIndex likewise declines UNLESS it is a kind-8 (Float64) pinned
        // TypedArray access, which the f64 element fast path emits inline (the
        // element is a VTy::Num xmm home; receiver/index handled specially below).
        match *instr {
            Instr::Call { .. } => decline!("Call"),
            // A pinned-STRING charCodeAt is admitted on the int path (inlines to a
            // byte load, runs no user code, allocates nothing — the no-call
            // invariant that keeps BOOL_GPRS alive holds), and a pinned-DataView
            // `get*` on the double path (same properties: a guarded byte load, no
            // user code, no allocation). Any other method call, or an access whose
            // receiver isn't pinned, declines.
            Instr::CallMethod { .. } if !pinned_str(s + off) && !pinned_dv(s + off) => {
                decline!("CallMethod (receiver not a pinned string/DataView)")
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
            // Pinned-STRING charCodeAt → a small int (0..65535); pinned-STRING
            // length → the snapshot units; both land in a Num i64 home. A
            // pinned-DV `get*` result is a Num too: every whitelisted kind is
            // exact in an f64 home (u32 <= 2^32-1; the float kinds native).
            Instr::CallMethod { dst, .. } if pinned_str(s + off) || pinned_dv(s + off) => {
                (Some(dst), VTy::Num)
            }
            Instr::GetProp { dst, .. } if pinned_str(s + off) => (Some(dst), VTy::Num),
            // `Math.imul` → a signed i32 (Num). BLOCKER FIX: without this the carried
            // fnv1a accumulator (written ONLY by Imul) is never typed → the
            // used-but-undefined scan declines the whole region.
            Instr::MathOp { dst, op: MathFn::Imul, argc: 2, .. } => (Some(dst), VTy::Num),
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
            Instr::Move { dst, .. } => (Some(dst), VTy::Num), // refined below
            _ => (None, VTy::Num),
        };
        // Record operand first-occurrences (uses) BEFORE the def, so a reg used
        // and defined by the same op counts the use first (live-in).
        for u in instr_uses(instr) {
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
        let is_split_recv_load =
            split_recv_lg.contains(&(s + off)) || dv_flag_elide.contains(&(s + off));
        if let Some(d) = def.filter(|d| !ta_recv_regs.contains(d) && !is_split_recv_load) {
            // Move's dst type follows its src; default Num is corrected here.
            let t = if let Instr::Move { src, .. } = *instr {
                *ty.get(&src).unwrap_or(&VTy::Num)
            } else {
                dty
            };
            if !note_def(d, t, &mut ty, &mut first_seen, &mut reg_order) {
                decline!("type conflict on a reused register");
            }
        }
        // Globals: order + first-touch direction. A TA-receiver's LoadGlobal is
        // excluded (no numeric home; the element emitter reads it via the pin).
        match *instr {
            // Neither a pinned receiver's global nor a B94 split receiver's gets an
            // xmm home: the value is an object, and the entry guard would reject it.
            Instr::LoadGlobal { dst, .. }
                if ta_recv_regs.contains(&dst) || split_recv_lg.contains(&(s + off)) => {}
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
    // TA-receiver regs are intentionally untyped (sourced via the pin) — skip them.
    let mut ro_live_in: Vec<u16> = Vec::new();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
        for u in instr_uses(instr) {
            if !ta_recv_regs.contains(&u) && !ty.contains_key(&u) && !ro_live_in.contains(&u) {
                ro_live_in.push(u);
            }
        }
    }
    if !ro_live_in.is_empty() {
        for (off, instr) in code[s..=e].iter().enumerate() {
            if cold.contains(&(s + off)) { continue; }
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
            for u in instr_uses(instr) {
                if ro_live_in.contains(&u) && !numeric.contains(&u) {
                    decline!("read-only live-in used where a number isn't required"); // 
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
        if cold.contains(&(s + off)) { continue; }
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
            if let Instr::CallMethod { arg_base, argc, name, .. } = *instr {
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
                {
                    decline!("pinned DV endian flag is not a Bool");
                }
            }
        }
    }

    // Validate operand type requirements now that types are known.
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
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
                if ty.get(&cond) != Some(&VTy::Bool) {
                    decline!("branch condition is not a bool");
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
        if cold.contains(&(s + off)) { continue; }
        match *instr {
            Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } => {
                *def_count.entry(dst).or_insert(0) += 1;
                const_def_ip.insert(dst, s + off);
            }
            _ => {
                if let Some(d) = writes_reg(instr) {
                    *def_count.entry(d).or_insert(0) += 1;
                }
            }
        }
    }
    // Registers that are actually USED as an operand somewhere in the region.
    // A defined-but-unused reg is DEAD (e.g. an object-ref load neutralised to
    // `LoadInt 0` by the field-promotion rewrite) — it must NOT be hoisted, or it
    // would consume a permanent xmm home for a value that's never read.
    let mut used: FxHashSet<u16> = FxHashSet::default();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
        for u in instr_uses(instr) {
            used.insert(u);
        }
    }
    // A pinned-DV get* is NOT a pure value op: an out-of-range pos throws
    // RangeError. Keep its dst out of `dead` — the emitter skips a dead-dst op
    // entirely, which would skip the throw (and leave the dst with no home for
    // the arm that does emit).
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
        if let Instr::CallMethod { dst, .. } = *instr {
            if pinned_dv(s + off) {
                used.insert(dst);
            }
        }
    }
    // ── pinned-STRING / Math.imul operand liveness ── instr_uses + writes_reg are
    // BLIND to CallMethod and MathOp operands/dsts (verified: both hit their `_`
    // arm). So the charCodeAt index reg, the Imul operand regs, and the charCodeAt/
    // Imul result-def ips are invisible — without this the index/operands would be
    // classed DEAD (their defining Move/Bitwise/LoadInt DCE'd → the inline op reads
    // an unmaterialised home / `xh` panics), and the home-reuse allocator would
    // free them at the wrong ip. Collect them LOCALLY (NOT by widening the shared
    // instr_uses/writes_reg, which SROA / f64-regalloc / leaf-inline depend on) and
    // feed both `used` (here) and the live-range touch loop (below). `(ip,reg,def)`.
    let mut str_imul_touch: Vec<(usize, u16, bool)> = Vec::new();
    for (off, instr) in code[s..=e].iter().enumerate() {
        if cold.contains(&(s + off)) { continue; }
        let ip = s + off;
        match *instr {
            Instr::CallMethod { dst, arg_base, argc: 1, .. } if pinned_str(ip) => {
                str_imul_touch.push((ip, arg_base, false)); // index (use)
                str_imul_touch.push((ip, dst, true)); // charCodeAt result (def)
            }
            Instr::MathOp { dst, arg_base, op: MathFn::Imul, argc: 2, .. } => {
                str_imul_touch.push((ip, arg_base, false));
                str_imul_touch.push((ip, arg_base + 1, false));
                str_imul_touch.push((ip, dst, true)); // imul result (def)
            }
            // A DV-flag-FUSED call reads the elided Eq's operands AT THE CALL
            // ip (the inline ToBoolean(a === b) compare) — extend their live
            // ranges to the call so the home-reuse allocator cannot free
            // either home one ip early.
            Instr::CallMethod { .. } => {
                if let Some(&(a, b)) = dv_flag_fuse.get(&ip) {
                    str_imul_touch.push((ip, a, false));
                    str_imul_touch.push((ip, b, false));
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
    let dead: FxHashSet<u16> = reg_order
        .iter()
        .copied()
        .filter(|r| {
            !used.contains(r) && first_seen.get(r) != Some(&false) && !read_outside.contains(r)
        })
        .collect();
    reg_order.retain(|r| !dead.contains(r));
    let mut hoist_ips: Vec<usize> = Vec::new();
    let mut hoisted: FxHashSet<u16> = FxHashSet::default();
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
        {
            hoist_ips.push(ip);
            hoisted.insert(r);
        }
    }
    hoist_ips.sort_unstable();

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
            code, s, e, &ty, &first_seen, &dead, &hoisted, &jump_targets, &g,
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
    let mut mul_shift: FxHashMap<usize, (u16, u8)> = FxHashMap::default();
    if region_is_int(proto, start, end, ta_plan) {
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
        elide_guard = analyze_int_guards(proto, s, e, entry);
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
        if cold.contains(&(s + off)) { continue; }
        let ip = s + off;
        let mut touch = |r: u16| {
            first_ip.entry(r).or_insert(ip);
            last_ip.insert(r, ip);
        };
        for u in instr_uses(instr) {
            touch(u);
        }
        if let Some(d) = writes_reg(instr) {
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
        if first_seen.get(&r) == Some(&false)
            || hoisted.contains(&r)
            || (!(admit_wt_share && wt_share_enabled()) && read_outside.contains(&r))
        {
            (s, e)
        } else {
            (first_ip[&r], last_ip[&r])
        }
    };
    // Registers eligible to SHARE an xmm: not loop-carried, not a hoisted
    // constant, and not read after the region. They need no entry load either —
    // an early exit flushing a stale value into their slot is unobservable.
    // B97: `read_outside` no longer disqualifies. Such a register may share a home
    // provided it is WRITTEN THROUGH at every def and skipped by `flush_exit` (see
    // `RegionPlan::write_through`) — that is what makes clobbering the home
    // invisible in its frame slot, which is the property the old rule bought by
    // refusing to share at all.
    let shareable = |r: u16| -> bool {
        first_seen.get(&r) != Some(&false)
            && !hoisted.contains(&r)
            && ((admit_wt_share && wt_share_enabled()) || !read_outside.contains(&r))
    };

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
        if admit_wt_share && read_outside.contains(&r) && shareable(r) {
            write_through.insert(r);
        }
    }

    // The xmm home pool size. If one-home-per-numeric-value fits, use the simple
    // allocation (distinct home each — best ILP, what loop.js relies on). Only
    // when it would OVERFLOW do we linear-scan-reuse homes for non-overlapping
    // live ranges (lets bigger loops JIT, and is required for object SROA).
    const POOL: usize = (HOME_XMM_LAST - HOME_XMM_FIRST + 1) as usize;
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
    let reuse = n_numeric > POOL;

    // ── allocate xmm/gpr homes ──
    let mut reg_home: FxHashMap<u16, Home> = FxHashMap::default();
    let mut glob_home: FxHashMap<u32, u8> = FxHashMap::default();
    let first_free_xmm: u8;
    if reuse {
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
        let mut alloc = XmmAlloc::new();
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
                        let perm = reg_order.iter().filter(|r| ty[r] == VTy::Num && !shareable(**r)).count();
                        let ro = reg_order.iter().filter(|r| ty[r] == VTy::Num && read_outside.contains(r)).count();
                        let li = reg_order.iter().filter(|r| ty[r] == VTy::Num && first_seen.get(r) == Some(&false)).count();
                        let ho = reg_order.iter().filter(|r| ty[r] == VTy::Num && hoisted.contains(r)).count();
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
            if ty[&r] == VTy::Num {
                if next_xmm > HOME_XMM_LAST {
                    decline!("xmm pool exhausted (registers)");
                }
                reg_home.insert(r, Home::Xmm(next_xmm));
                next_xmm += 1;
            }
        }
        for &gi in &glob_order {
            if next_xmm > HOME_XMM_LAST {
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
    // Bools (both modes): gpr homes; a live-in bool is unsupported.
    let mut next_bool = 0usize;
    for &r in &reg_order {
        if ty[&r] == VTy::Bool {
            if first_seen.get(&r) == Some(&false) || next_bool >= BOOL_GPRS.len() {
                decline!("bool live-in, or bool gpr pool exhausted");
            }
            reg_home.insert(r, Home::Gpr(BOOL_GPRS[next_bool]));
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
            if cold.contains(&(s + off)) { continue; }
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
            if cold.contains(&(s + off)) { continue; }
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
                live_in_bools.push((r, g));
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
        live_in_globs.push((gi, x));
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
        elide_guard,
        mul_shift,
        addint_imm_home,
        gpr_const,
        jump_targets,
        ta_recv_regs,
        split_recvs,
        write_through,
        split_recv_lg,
        dv_flag_elide,
        dv_flag_fuse,
    }))
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

/// The VM registers an instruction reads (operands). Used for live-in analysis.
pub(crate) fn instr_uses(i: &Instr) -> Vec<u16> {
    match *i {
        Instr::Move { src, .. } => vec![src],
        Instr::StoreGlobal { src, .. }
        | Instr::StoreGlobalStrict { src, .. }
        | Instr::StoreGlobalResolved { src, .. } => vec![src],
        Instr::AddInt { a, .. } | Instr::Neg { a, .. } => vec![a],
        Instr::Add { a, b, .. }
        | Instr::Sub { a, b, .. }
        | Instr::Mul { a, b, .. }
        | Instr::Div { a, b, .. }
        | Instr::Mod { a, b, .. }
        | Instr::StrConcat { a, b, .. }
        | Instr::StrAppendInPlace { a, b, .. }
        | Instr::Bitwise { a, b, .. }
        | Instr::Lt { a, b, .. }
        | Instr::Le { a, b, .. }
        | Instr::Gt { a, b, .. }
        | Instr::Ge { a, b, .. }
        | Instr::Eq { a, b, .. }
        | Instr::Ne { a, b, .. }
        | Instr::JumpIfNotLt { a, b, .. }
        | Instr::JumpIfNotLe { a, b, .. } => vec![a, b],
        Instr::JumpIfFalse { cond, .. } | Instr::JumpIfTrue { cond, .. } => vec![cond],
        Instr::GetProp { obj, .. } => vec![obj],
        Instr::SetProp { obj, val, .. } => vec![obj, val],
        Instr::GetIndex { obj, key, .. } => vec![obj, key],
        Instr::SetIndex { obj, key, val } => vec![obj, key, val],
        Instr::GetIndexConcat { obj, key, .. } => vec![obj, key],
        Instr::SetIndexConcat { obj, key, val, .. } => vec![obj, key, val],
        // ToPropKey reads the receiver (nullish check) and the key. `src` MUST
        // be listed or the dead-code pass drops the load that feeds it; `obj`
        // follows the GetIndex/SetIndex pattern and is exempted at the pinned
        // receiver's use-site scan like theirs.
        Instr::ToPropKey { obj, src, .. } => vec![obj, src],
        Instr::DeleteIndexConcat { obj, key, .. } => vec![obj, key],
        Instr::Return { src } => vec![src],
        // MathOp / CallMethod read a CONTIGUOUS argument window starting at
        // `arg_base` (plus the receiver for a method call). Reporting no uses
        // let the home-unification passes treat those registers as dead and
        // alias over them — see the matching note in `writes_reg`.
        Instr::MathOp { arg_base, argc, .. } => {
            (0..argc).map(|k| arg_base + k).collect()
        }
        Instr::CallMethod { obj, arg_base, argc, .. } => {
            let mut v = vec![obj];
            v.extend((0..argc).map(|k| arg_base + k));
            v
        }
        _ => vec![],
    }
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
        | Instr::StoreGlobalResolved { idx, .. } = *instr {
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
            Some(slot) if !m.attrs[slot].accessor && (!need_writable || m.attrs[slot].writable) => {}
            _ => return None,
        }
    }
    Some(FieldPromotePlan { obj_global: g, fields, obj_idx, obj_version: heap.version_of(obj_idx) })
}

