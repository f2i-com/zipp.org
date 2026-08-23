//! GPR-home sub-mode of the INTEGER region tier (the B118 blocker).
//!
//! The base INT emitter keeps every numeric value as a raw i64 in the low
//! quadword of an XMM home. That is ideal for `paddq` chains, but every
//! `Bitwise`/`Math.imul` op must round-trip the operands through the integer
//! unit: `movq rax, xmm ; movq rcx, xmm ; op ; movsxd ; movq xmm, rax` — three
//! xmm↔gpr transfers (~2-3 cycles latency each) per op, ON the loop's serial
//! dependency chain. On the fnv1a hash loop (`h = imul(h ^ cc, P)`) that is the
//! whole gap to node: V8 keeps `h`, `i` and the length in GPRs and pays zero
//! transfers.
//!
//! This emitter is the GPR sibling: the SAME `RegionPlan` (same homes, same
//! liveness, same entry-load / flush-on-exit discipline — the B96/B97 template),
//! but each xmm home index is mapped to a GPR and every arm is the integer-unit
//! form. The pool is small, so two pressure relief valves make real loops fit:
//! hoisted constants (always i32 — `emit_int_const`'s own invariant) become
//! IMMEDIATES instead of homes, and the `[rsi]` resume-ip pointer moves to a
//! frame slot so rsi itself is a home (rdi joins too when the VM is unmetered).
//!
//! Scope is bounded aggressively (see [`gpr_home_map`]): no cold blocks, no
//! B94 splits / B97 write-through / DV fusion, at least one Bitwise/imul op
//! (else the xmm form is already optimal), and the live set must fit the pool.
//! Anything outside falls back to the xmm emitter unchanged — except a pool
//! OVERFLOW, which first earns one re-plan with forced home sharing (B119:
//! the enclosing region of a loop nest carries the inner loop's counters and
//! temps, most of which never overlap; sharing fits it so the OUTER region
//! goes GPR instead of shadowing an engaged GPR inner on xmm homes).
//!
//! Correctness model is IDENTICAL to the xmm tier: sign-extended i64 homes,
//! entry guards on every live-in, i53 guards on add/sub/mul (unless proven
//! elidable), every exit flushes every home boxed through the same
//! Int-if-it-fits-else-double rule, deopts resume at the exact ip the xmm tier
//! would. `ZIPP_NO_GPR_HOMES=1` turns the whole mode off.
//!
//! W8 (B120's correction of B119) shortens the accumulator DEPENDENCY CHAIN
//! itself: the wave-7 arms canonicalized every Bitwise/imul result on the spot
//! (`op eax ; movsxd Rq(d), eax`), and on a serial chain like fnv1a's
//! `h = imul(h ^ cc, P)` those two `movsxd` are 2 of the 6 chain cycles while
//! the mov32s rename away — the whole measured gap to node. The fix is
//! DEFERRED SIGN-EXTENSION ([`lazy_sx_sets`]): a home whose every def provably
//! writes an i32-range value may keep a Bitwise/imul result ZERO-extended, and
//! the canonicalizing `movsxd` moves off the chain — to the few consumers that
//! genuinely need the i64 form, and to one fix-up block at `flush_exit` (so
//! every exit still boxes a canonical i32-in-i64 Int Value, exactly as wave 7
//! did). B94 split receivers and B97 write-through registers are EXCLUDED from
//! deferral: their per-def write-through boxes the FULL home, so they stay
//! canonical at every def, and `emit_int_wt_gpr` needs no lazy awareness. The
//! same wave rewrites the arms to operate IN PLACE on the dst home (32-bit
//! forms, `imul r32, r32, imm`, `movzx home, byte` for charCodeAt), dropping
//! the rax staging round-trips. `ZIPP_NO_GPR_LAZYSX=1` turns only the deferred
//! sign-extension off (defs canonicalize immediately again).
//!
//! W10.3 — GPR SPILL SLOTS: a pool overflow is no longer final. On the attempt
//! AFTER the B119 shared-home re-plan still overflows (B96's ordering,
//! `allow_spill` threaded from `region_int`'s DV and split-fallback retries),
//! [`gpr_home_map`] ranks homes by a weighted use census (8^loop-depth), keeps
//! the hottest resident (r13/r14 join the pool with inline guards; split
//! receivers force-resident), and parks the coldest — at most
//! [`GPR_SPILL_CAP`] — in frame slots after the resume-ip slot. A slot ALWAYS
//! holds the canonical sign-extended i64 (spilled homes are never lazy), so
//! operands read as qword mem forms, dsts stage through rax/eax and park the
//! full qword, and every exit boxes spilled homes from their slots. This is
//! what lets typedarray-math's 13/14-home DV swizzle regions ([215,262] /
//! [210,266]) host on this tier instead of falling to DOUBLE.
//! `ZIPP_NO_GPR_SPILL_SLOTS=1` restores the pre-wave decline byte-for-byte.

#![allow(unused_imports)]
use super::*;

/// Kill switch: `ZIPP_NO_GPR_HOMES=1` disables the GPR-home sub-mode (the
/// region then compiles on the xmm-home INT emitter exactly as before).
pub(crate) fn gpr_homes_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_GPR_HOMES").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Kill switch: `ZIPP_NO_DV_GPR=1` disables the W9 DataView-on-GPR-homes
/// hosting (the `region_int` DV retry and this emitter's DV arms). The region
/// then falls to the DOUBLE tier's B115 DV arm exactly as before the wave.
pub(crate) fn dv_gpr_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_DV_GPR").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Kill switch: `ZIPP_NO_GPR_NEST=1` disables ONLY the B119 shared-home
/// re-plan after a [`GprAttempt::PoolOverflow`] (see `compile_region_int`) —
/// a region that fits the pool one-home-per-value still engages, restoring
/// wave-6 behavior byte-for-byte. `ZIPP_NO_GPR_HOMES=1` kills the whole
/// sub-mode, retry included.
pub(crate) fn gpr_nest_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_GPR_NEST").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Kill switch: `ZIPP_NO_GPR_SPLIT=1` refuses B94 split-receiver plans in this
/// emitter again (W8). With it set, a split plan reaches native code only via
/// the xmm INT emitter under `ZIPP_INT_SPLIT=1` — exactly the pre-W8 world.
///
/// Why splits are IN scope here: `int_split_enabled`'s refutation priced the
/// split on the XMM emitter, where every Bitwise op round-trips xmm↔gpr on the
/// serial chain (the i32 fill measured ~96ms vs ~60ms MEM), and its own
/// follow-up note named "gpr (not xmm) homes for bitwise-chain regions" as the
/// real fix. This emitter IS those homes; the write-through contract is
/// mirrored instruction-for-instruction from the xmm emitter's proven B94 arm
/// (def → boxed store to the reg-file slot BEFORE any i53 guard; flush skips
/// the register; the receiver LoadGlobal stores the object to the same slot).
pub(crate) fn gpr_split_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_GPR_SPLIT").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Kill switch: `ZIPP_NO_GPR_LAZYSX=1` disables ONLY the W8 deferred
/// sign-extension ([`lazy_sx_sets`] returns empty sets): every Bitwise/imul
/// def canonicalizes immediately and entry admission stays wave-7 loose. The
/// in-place instruction selection is unconditional — it is representation-
/// neutral and locally verifiable arm by arm.
pub(crate) fn gpr_lazysx_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_GPR_LAZYSX").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Kill switch: `ZIPP_NO_GPR_SPILL_SLOTS=1` disables the W10.3 frame spill
/// slots ONLY: a pool overflow declines exactly as before the wave
/// (`PoolOverflow` stays final after the shared-home re-plan) and emission is
/// byte-identical to the pre-spill emitter. NOTE the `_SLOTS` suffix —
/// `ZIPP_NO_GPR_SPLIT` (above) is a DIFFERENT switch one edit-distance away.
pub(crate) fn gpr_spill_slots_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            // W10 verdict: OPT-IN (dark). The mechanism is built, correct and
            // multi-mode-proven, but on its target shape it LOSES to the
            // DOUBLE-tier incumbent — typedarray-math's swizzle regions
            // measured 448ms spilled vs 361ms declining-to-DOUBLE (best-of-5,
            // replicated at 451 vs 344 on an independent build), even after
            // the DV guard prover freed r13/r14 and elided the chain guards:
            // four spilled homes' slot traffic plus per-def movsxd on a
            // 0-lazy region loses to fourteen xmm homes with no spills. The
            // B99 lesson generalizes ("register homes are only a win when the
            // representation makes the op cheaper"). Kept dark for shapes a
            // future planner cost model may admit; `ZIPP_GPR_SPILL_SLOTS=1`
            // engages it, `ZIPP_NO_GPR_SPILL_SLOTS=1` forces it off even then.
            let on = std::env::var_os("ZIPP_GPR_SPILL_SLOTS").is_some()
                && std::env::var_os("ZIPP_NO_GPR_SPILL_SLOTS").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// `compile_region_int_gpr`'s result. `PoolOverflow` is the one decline worth
/// acting on: every OTHER gate passed and only the live set didn't fit, so the
/// caller can re-plan with forced home sharing (B119 — the nested-loop
/// residual: an enclosing region carries the inner loop's counters and temps
/// too) and try once more. Every other decline is final for this region shape.
pub(crate) enum GprAttempt {
    Emitted(JitFn),
    PoolOverflow,
    OutOfScope,
}

/// An operand as this emitter reads it: a GPR home, a hoisted-constant
/// immediate (always i32 — `LoadInt`'s payload and `LoadConst`-Int's payload
/// both are, which is what makes every imm form below encodable), or — W10.3 —
/// a frame spill slot at `[rsp + disp]`. A slot ALWAYS holds the canonical
/// sign-extended i64 (spilled homes are never W8-lazy), so a qword mem-operand
/// form reads it directly and the canon variants need no extra work.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Src {
    R(u8),
    I(i32),
    S(i32),
}

/// W10.3: where a home LIVES — a hardware GPR (as before the wave) or a frame
/// spill slot (`[rsp + disp]`, canonical sign-extended i64 at all times).
/// Local to this emitter; the plan still identifies homes by xmm index.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Loc {
    R(u8),
    S(i32),
}

/// The register an arm computes a possibly-spilled dst into: the home itself
/// when resident, rax (scratch 0) when spilled — [`store_dst`] then parks the
/// staged qword in the slot. Sound wherever rax is otherwise dead at the def.
fn dst_reg(l: Loc) -> u8 {
    match l {
        Loc::R(h) => h,
        Loc::S(_) => 0,
    }
}

/// Park a staged (rax) dst into its spill slot; emits NOTHING for a resident
/// home — which is what keeps every resident-only arm byte-identical. A plain
/// mov, so compare flags survive.
fn store_dst(ops: &mut dynasmrt::x64::Assembler, l: Loc) {
    if let Loc::S(d) = l {
        dynasm!(ops ; mov [rsp + d], rax);
    }
}

/// A mapped home as an operand.
fn loc_src(l: Loc) -> Src {
    match l {
        Loc::R(h) => Src::R(h),
        Loc::S(d) => Src::S(d),
    }
}

/// W10.3: at most this many homes may live in frame slots — a region colder
/// than this declines to the other tiers exactly as before the wave (the
/// failure mode stays "decline as today", never "engage mostly-spilled").
const GPR_SPILL_CAP: usize = 8;

/// The home map and the hoisted-constant table, or the decline reason when
/// the region is outside this mode's bounded scope (`Err(true)` = the live set
/// alone overflowed the pool — the retryable case; `Err(false)` = any other
/// gate failed).
///
/// Pool (hand-out order): r15 and rbp — pushed by this emitter's own prologue —
/// then whichever of the BOOL_GPRS (r8..r11) the plan left free (this emitter
/// ignores `gpr_const`, whose mirrors the compare arms here never need), then
/// rsi (its resume-ip pointer lives in a frame slot here), then rdi when the VM
/// is unmetered (a metered body charges through `[rdi + off]`). rax/rcx/rdx
/// stay scratch; rbx/r12/r13/r14 keep their fixed roles from the xmm tier.
///
/// A home whose only regs are hoisted constants is NOT mapped: its regs read as
/// immediates and flush as compile-time-boxed Values, so it costs no GPR.
///
/// NOTE r8..r11: every arm in THIS file scratches only rax/rcx/rdx, which is
/// what makes handing them out as numeric homes sound. That is now the rule for
/// EVERY region emitter — see the register contract on `BOOL_GPRS`. (It used to
/// be this file's local discipline while the xmm tiers scratched r10 in their
/// entry loads, their dense-Array tag check and the DOUBLE `Bitwise` sentinel;
/// each of those was a silent wrong answer, fixed in W14 and W16.)
fn gpr_home_map(
    proto: &FuncProto,
    plan: &RegionPlan,
    s: usize,
    e: usize,
    metered: bool,
    allow_spill: bool,
    spill_base: i32,
) -> Result<(FxHashMap<u8, Loc>, FxHashMap<u16, i32>, bool, usize), bool> {
    // Out of scope: any plan feature whose write-through/flush interplay was
    // only ever proven against the other emitters. B94 split receivers are in
    // scope since W8 (per-def write-through mirrored from the xmm INT emitter;
    // `ZIPP_NO_GPR_SPLIT=1` restores the refusal). W9 brings DV flag fusion
    // (the DV arm emits the fused compare on integer homes) and B97
    // write-through sharing (every def site now write-throughs — the generic
    // hook plus explicit calls in the writes_reg-blind CallMethod/MathOp/DV
    // arms; flush and lazy_sx already treated `write_through` like splits) IN
    // scope, both only under `dv_gpr_enabled()`: the `region_int` DV retry is
    // the only INT caller that plans with either, so `ZIPP_NO_DV_GPR=1`
    // restores the exact pre-W9 surface.
    if ((!plan.write_through.is_empty()
        || !plan.dv_flag_elide.is_empty()
        || !plan.dv_flag_fuse.is_empty())
        && !dv_gpr_enabled())
        || (!gpr_split_enabled()
            && (!plan.split_recvs.is_empty() || !plan.split_recv_lg.is_empty()))
    {
        return Err(false);
    }
    // Engage only where the mode pays: at least one op that would round-trip
    // xmm↔gpr on the xmm tier.
    let pays = proto.code[s..=e].iter().any(|i| {
        matches!(i, Instr::Bitwise { .. } | Instr::MathOp { op: MathFn::Imul, argc: 2, .. })
    });
    if !pays {
        return Err(false);
    }
    // Hoisted constants: reg → i32 value (from the hoist ips' own opcodes).
    let mut hoist_c: FxHashMap<u16, i32> = FxHashMap::default();
    for &hip in &plan.hoist_ips {
        match proto.code[hip] {
            Instr::LoadInt { dst, val } => {
                hoist_c.insert(dst, val);
            }
            Instr::LoadConst { dst, idx } => {
                // region admission guaranteed the constant is Int-tagged.
                hoist_c.insert(dst, proto.constants[idx as usize].bits() as u32 as i32);
            }
            _ => return Err(false), // not a shape this emitter hoists
        }
    }
    // Slot-materialized consts read as immediates too — but their defining op
    // is EMITTED (a boxed store to the frame slot), not skipped, and they own
    // no home (see `RegionPlan::slot_consts`).
    for (&r, &v) in plan.slot_consts.iter() {
        hoist_c.insert(r, v);
    }
    // Homes that need a GPR: every home some NON-hoisted reg or a global uses.
    let mut used: Vec<u8> = Vec::new();
    let mut push = |x: u8, used: &mut Vec<u8>| {
        if !used.contains(&x) {
            used.push(x);
        }
    };
    for &(r, x) in &plan.num_regs {
        if !hoist_c.contains_key(&r) {
            push(x, &mut used);
        }
    }
    for &(_, x) in &plan.globs {
        push(x, &mut used);
    }
    used.sort_unstable();
    let bool_used: FxHashSet<u8> = plan.bool_regs.iter().map(|&(_, g)| g).collect();
    let mut pool: Vec<u8> = vec![15, 5]; // r15, rbp (pushed by this prologue)
    pool.extend(BOOL_GPRS.iter().copied().filter(|g| !bool_used.contains(g)));
    pool.push(6); // rsi — the resume-ip pointer lives in the frame here
    if !metered {
        pool.push(7); // rdi — only the meter reads it in the body
    }
    // r13/r14 hold the 2^53/2^54 guard constants — but only i53 guards read
    // them, so a region that provably emits none (every add/sub/addint/neg is
    // guard-elided, every mul is a proven shift) can use both as homes. The
    // prologue still loads the constants; an entry load or def simply
    // overwrites them, which is fine exactly because no guard ever reads them.
    let needs_guard = proto.code[s..=e].iter().enumerate().any(|(off, i)| {
        let ip = s + off;
        match *i {
            Instr::Add { .. } | Instr::Sub { .. } | Instr::Neg { .. } => {
                !plan.elide_guard.contains(&ip)
            }
            Instr::AddInt { imm, a, .. } => {
                imm != 0 && !hoist_c.contains_key(&a) && !plan.elide_guard.contains(&ip)
            }
            Instr::Mul { .. } => {
                !plan.mul_shift.contains_key(&ip) && !plan.elide_guard.contains(&ip)
            }
            _ => false,
        }
    });
    // When that alone doesn't fit but two more would, take r13/r14 anyway and
    // pay for each remaining guard with inline (movabs) constants — a few
    // bytes per guard against keeping the whole loop in registers.
    let mut inline_guards = false;
    if !needs_guard {
        pool.push(13);
        pool.push(14);
    } else if used.len() > pool.len() && used.len() <= pool.len() + 2 {
        inline_guards = true;
        pool.push(13);
        pool.push(14);
    }
    let mut n_spill = 0usize;
    let mut spilled_path = false;
    // Review fix (B123): the pre-spill pool size, so a spill-path decline
    // logs the SAME "N homes > M gprs" line the pre-wave decline logged —
    // the ablation discipline greps and diffs these lines across modes.
    let base_pool_len = pool.len();
    let map: FxHashMap<u8, Loc> = if used.len() <= pool.len() {
        used.into_iter().zip(pool).map(|(x, h)| (x, Loc::R(h))).collect()
    } else if allow_spill && gpr_spill_slots_enabled() {
        // ── W10.3 spill slots ── the overflow is no longer final: rank homes
        // by weighted use count, keep the hottest resident, park the coldest
        // in frame slots (canonical i64, [rsp + spill_base + 8k]). Engaged
        // only on the attempt AFTER the shared-home re-plan still overflowed
        // (allow_spill — B96's ordering, threaded from `region_int`'s retry
        // sites) and only under `gpr_spill_slots_enabled()`.
        //
        // r13/r14 always join the pool here (inline_guards=true): once slot
        // traffic is being paid, two more resident homes are strictly cheaper
        // than two guard-constant registers plus movabs pairs.
        spilled_path = true;
        if needs_guard && !inline_guards {
            inline_guards = true;
            pool.push(13);
            pool.push(14);
        }
        // B94 split receivers are FORCE-RESIDENT (v1): their contract — home
        // garbage until the first numeric def, memory slot authoritative,
        // per-def boxed write-through before any i53 guard, flush skips the
        // reg — was proven against register homes and stays textually intact.
        let mut forced: FxHashSet<u8> = FxHashSet::default();
        for r in plan.split_recvs.iter() {
            match plan.reg_home.get(r) {
                Some(&Home::Xmm(x)) if used.contains(&x) => {
                    forced.insert(x);
                }
                _ => return Err(false),
            }
        }
        n_spill = used.len() - pool.len();
        if forced.len() > pool.len() || n_spill > GPR_SPILL_CAP {
            if std::env::var_os("ZIPP_JITLOG").is_some() {
                // base_pool_len, not pool.len(): the spill path forcibly
                // added r13/r14 above, and the ablation discipline diffs
                // these lines against the pre-wave form (review fix, B123).
                eprintln!(
                    "[jit] INT-GPR decline [{s},{e}]: {} homes > {} gprs",
                    used.len(),
                    base_pool_len
                );
            }
            return Err(true); // decline to DOUBLE/MEM exactly as today
        }
        let universe: FxHashSet<u8> = used.iter().copied().collect();
        let weights = spill_census(proto, plan, s, e, &hoist_c, &universe);
        // Forced-resident first, then hottest first; home index breaks ties
        // deterministically. The hottest pool.len() homes take the pool in
        // hand-out order, the rest take slots 0..n_spill.
        let mut order = used;
        order.sort_by_key(|&x| {
            (
                !forced.contains(&x),
                std::cmp::Reverse(weights.get(&x).copied().unwrap_or(0)),
                x,
            )
        });
        let mut m: FxHashMap<u8, Loc> = FxHashMap::default();
        for (i, &x) in order.iter().enumerate() {
            let l = if i < pool.len() {
                Loc::R(pool[i])
            } else {
                Loc::S(spill_base + 8 * (i - pool.len()) as i32)
            };
            m.insert(x, l);
        }
        m
    } else {
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!(
                "[jit] INT-GPR decline [{s},{e}]: {} homes > {} gprs",
                used.len(),
                pool.len()
            );
        }
        return Err(true); // the retryable decline — see `GprAttempt::PoolOverflow`
    };
    // A B94 split register must own a RESIDENT home — its defs write through
    // it to memory and the split contract is only proven against register
    // homes (force-resident above guarantees this; the check keeps the
    // no-spill path's refusal). A write-through register needs a MAPPED home
    // (register or slot — its raw slot store is additive to the boxed
    // write-through its defs already pay). A split/wt register whose home
    // didn't map (hoisted-const-only, which its defs should make impossible)
    // would leave its slot stale on exit; refuse rather than assume.
    for r in plan.split_recvs.iter() {
        match plan.reg_home.get(r) {
            Some(&Home::Xmm(x)) if matches!(map.get(&x), Some(Loc::R(_))) => {}
            _ => return Err(false),
        }
    }
    for r in plan.write_through.iter() {
        match plan.reg_home.get(r) {
            Some(&Home::Xmm(x)) if map.contains_key(&x) => {}
            // ── W10.3 (spill attempts only, so the kill switch restores the
            // strict refusal byte-for-byte) ── the swizzle regions' recycled
            // endian-flag registers land in `write_through` (shareable +
            // read_outside is a REG-level rule, blind to type) without a
            // numeric home, and the strict check above declined the whole
            // region. Two shapes are provably fine:
            //
            // A real BOOL home (`bool_regs` member): bool homes are never
            // shared (one gpr per bool, no linear-scan reuse), every def
            // writes the home, and flush_exit's bool loop boxes the CURRENT
            // value into the register's own slot at every exit — the stale-
            // sharer hazard write-through exists for cannot occur. (The
            // DOUBLE tier engages this exact plan shape today with the same
            // treatment.) Nothing to write through; the wt hook is Xmm-only
            // and never fires for it.
            //
            // NO home at all: every in-region def is a W9-ELIDED Eq
            // (`dv_flag_elide`), so nothing is ever written natively — the
            // fused call reads the Eq OPERANDS, not the flag — and a fused
            // deopt resumes AT the elided Eq, which recomputes the flag into
            // the frame slot the interpreter owns. Verified op by op.
            //
            // Anything else stays a refusal.
            // (Review fix, B123: the admitted Bool-home wt reg additionally
            // gets an ENTRY LOAD in the prologue — without one the home is
            // caller garbage until the flag's first def, and flush_exit's
            // bool loop boxes EVERY bool home at every exit, so an exit
            // before that def would write BOOL_TAG|garbage into a
            // read-outside slot. The former "no home at all, every def
            // elided" escape was removed: the elision fixpoint requires a
            // non-elided kill def, which types the reg — the arm could only
            // fire on a plan shape the planner cannot produce, and admitting
            // it on faith was a latent stale-slot hazard.)
            // A glob-range plan (`narrow_globs` non-empty — only ever built
            // for this emitter) reaches this check on the NON-spill attempts
            // too: the narrowed intervals fit the pool where the pre-wave
            // plan overflowed first. The Bool-home admission argument above
            // is path-independent (one gpr per bool, every def writes it,
            // the B123 entry load runs on every attempt), so admit it here
            // on the same terms; the spilled_path gate stays for un-narrowed
            // plans so `ZIPP_NO_GPR_SPILL_SLOTS=1` keeps its byte-identical
            // ablation, and `ZIPP_NO_GLOB_RANGE=1` empties `narrow_globs`
            // and restores the strict refusal byte-for-byte.
            Some(&Home::Gpr(_))
                if (spilled_path || !plan.narrow_globs.is_empty())
                    && plan.bool_regs.iter().any(|&(br, _)| br == *r) => {}
            _ => return Err(false),
        }
    }
    Ok((map, hoist_c, inline_guards, n_spill))
}

/// W10.3 weighted use census, per HOME (the spill ranking's cost model: a
/// home's slot traffic is the SUM over its linear-scan sharers). Walks the
/// body exactly as the emitter will — skipping hoisted-const fills, hoisted
/// length reads, W9-elided Eqs and dead-dst ops — and counts one touch per
/// operand read / def write, weighted `8^loop-depth` (back-edge spans).
/// `instr_uses`/`writes_reg` cover CallMethod/MathOp operand windows and dsts;
/// the two remaining blind spots are the global side of LoadGlobal/StoreGlobal
/// (a home with no reg operand) and a fused DV call's elided-Eq operand reads
/// at the call ip — both counted explicitly below.
fn spill_census(
    proto: &FuncProto,
    plan: &RegionPlan,
    s: usize,
    e: usize,
    hoist_c: &FxHashMap<u16, i32>,
    universe: &FxHashSet<u8>,
) -> FxHashMap<u8, u64> {
    let mut depth = vec![0u32; e - s + 1];
    for ip in s..=e {
        let t = match proto.code[ip] {
            Instr::Jump { target }
            | Instr::JumpIfFalse { target, .. }
            | Instr::JumpIfTrue { target, .. }
            | Instr::JumpIfNotLt { target, .. }
            | Instr::JumpIfNotLe { target, .. } => target as usize,
            _ => continue,
        };
        if t >= s && t <= ip {
            for d in depth[(t - s)..=(ip - s)].iter_mut() {
                *d += 1;
            }
        }
    }
    let mut w: FxHashMap<u8, u64> = FxHashMap::default();
    for ip in s..=e {
        if plan.hoist_len_ips.contains(&ip) || plan.dv_flag_elide.contains(&ip) {
            continue;
        }
        if let Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } = proto.code[ip] {
            if plan.hoisted.contains(&dst) {
                continue;
            }
        }
        if let Some(d) = writes_reg(&proto.code[ip]) {
            if plan.dead.contains(&d) {
                continue;
            }
        }
        let wt = 8u64.saturating_pow(depth[ip - s].min(20));
        let mut touch_reg = |r: u16, w: &mut FxHashMap<u8, u64>| {
            if hoist_c.contains_key(&r) {
                return; // reads as an immediate — no home traffic
            }
            if let Some(&Home::Xmm(x)) = plan.reg_home.get(&r) {
                if universe.contains(&x) {
                    { let e2 = w.entry(x).or_insert(0); *e2 = e2.saturating_add(wt); }
                }
            }
        };
        for r in instr_uses(&proto.code[ip]) {
            touch_reg(r, &mut w);
        }
        if let Some(d) = writes_reg(&proto.code[ip]) {
            touch_reg(d, &mut w);
        }
        if let Some(&(fa, fb)) = plan.dv_flag_fuse.get(&ip) {
            touch_reg(fa, &mut w);
            touch_reg(fb, &mut w);
        }
        match proto.code[ip] {
            Instr::LoadGlobal { dst, idx } => {
                if !plan.ta_recv_regs.contains(&dst) && !plan.split_recv_lg.contains(&ip) {
                    if let Some(&x) = plan.glob_home.get(&idx) {
                        if universe.contains(&x) {
                            { let e2 = w.entry(x).or_insert(0); *e2 = e2.saturating_add(wt); }
                        }
                    }
                }
            }
            Instr::StoreGlobal { idx, .. }
            | Instr::StoreGlobalStrict { idx, .. }
            | Instr::StoreGlobalResolved { idx, .. } => {
                if let Some(&x) = plan.glob_home.get(&idx) {
                    if universe.contains(&x) {
                        { let e2 = w.entry(x).or_insert(0); *e2 = e2.saturating_add(wt); }
                    }
                }
            }
            _ => {}
        }
    }
    w
}

/// W8 deferred sign-extension: `(lazy, strict)` home sets.
///
/// A home is I32-INVARIANT when every value it can hold while the region runs
/// provably fits i32. For such a home `movsxd Rq(h), Rd(h)` RECOVERS the
/// canonical i64 from either representation — it is a no-op on the
/// sign-extended form and exact on the zero-extended low-32 form — so a
/// Bitwise/imul def may skip its canonicalizing `movsxd` (leaving the 32-bit
/// zero-extended result the ALU wrote) and the movsxd moves OFF the serial
/// chain: each consumer that needs the i64 form re-canonicalizes (compare /
/// add / index / copy-out — one movsxd each, off the accumulator recurrence),
/// and `flush_exit` opens with one fix-up movsxd per lazy home so EVERY exit
/// still boxes the same canonical i32-in-i64 Int Value wave 7 boxed.
///
/// `lazy` (⊆ i32-invariant): homes with at least one Bitwise (non-`>>>`) or
/// `Math.imul` def — the only defs that pay a def-site movsxd, so the only
/// homes where deferral buys anything. Membership requires EVERY def of the
/// home to write an i32-range value:
///   - Bitwise (non-`>>>`) and `Math.imul` results are ToInt32 by definition;
///   - `LoadInt` / Int `LoadConst` / hoisted-const fills are i32 payloads;
///   - pinned `GetIndex` (Int payload / i32 element, both movsxd'd) and pinned
///     `charCodeAt` (movzx byte, 0..255) land in i32 range;
///   - `Move` / `LoadGlobal` / `StoreGlobal` / `AddInt +0` copies require the
///     SOURCE home i32-invariant (or an i32 immediate);
///   - everything else — Add/Sub/Mul/Mod/Neg, `AddInt` with a real immediate,
///     `>>>` (a u32 result may exceed i32), pinned lengths, any unlisted op —
///     is WIDE and disqualifies the home.
///
/// W8 split reconciliation: a B94 split receiver's (or B97 write-through
/// register's) home is seeded WIDE, so it can never go lazy. Its per-def
/// write-through (`emit_int_wt_gpr`) boxes the FULL 64-bit home through
/// `emit_int_box_from_gpr`, which demands the canonical representation at
/// every single def — deferral would have to re-canonicalize right there,
/// on the store, buying nothing (and the split register is the recycled
/// RECEIVER temp, never the serial accumulator this pass targets). Exclusion
/// keeps the landed write-through contract — "split home is a canonical i64
/// at every def" — textually intact. The receiver-half `LoadGlobal`
/// (`split_recv_lg`) stores the OBJECT to memory and never touches the home,
/// so those ips record no def at all.
///
/// `strict` (⊇ `lazy`): the copy-closure of `lazy`'s sources. These homes'
/// entry loads take the STRICT form (i32 range required, else `entry_bail`),
/// which is what makes the invariant hold from the region's first instruction:
/// an Int-tagged live-in is i32 by construction, and an integral double
/// outside i32 now bails to the interpreter instead of entering (nothing is
/// computed at that point, so bailing is always sound — the cost is that such
/// a region keeps interpreting, which no shape that reaches a Bitwise/imul
/// accumulator does with a >i32 value).
///
/// `>>>` sits out on both sides by design: its result is already written
/// zero-extended = canonical, and it neither needs deferral nor tolerates a
/// movsxd (its u32 value may not survive one).
/// W10.3 spilled homes: a spill slot is read as a full qword by 64-bit
/// consumers, so a spilled home can NEVER be lazy — it simply does not enter
/// the h-universe below (which holds hardware-GPR ids), and any copy FROM a
/// spilled slot into a resident home marks the dst WIDE (fail safe, exactly
/// like an unmapped source). That seeds every spilled home WIDE by
/// construction, and the strict-entry closure stays consistent: no lazy home
/// ever (transitively) copies from a spilled one.
fn lazy_sx_sets(
    proto: &FuncProto,
    plan: &RegionPlan,
    s: usize,
    e: usize,
    map: &FxHashMap<u8, Loc>,
    hoist_c: &FxHashMap<u16, i32>,
) -> (FxHashSet<u8>, FxHashSet<u8>) {
    if !gpr_lazysx_enabled() {
        return (FxHashSet::default(), FxHashSet::default());
    }
    // Census of defs per home: copy edges (dst home ← src home), WIDE defs
    // (result may exceed i32), and Bitwise/imul defs (the ones deferral pays
    // for). An i32-producing def (LoadInt, pinned GetIndex/charCodeAt, imm
    // copy) constrains nothing, so it records nothing.
    let mut copies: Vec<(u8, u8)> = Vec::new();
    let mut wide: FxHashSet<u8> = FxHashSet::default();
    let mut bit: FxHashSet<u8> = FxHashSet::default();
    // Split/write-through homes: WIDE by decree (see the header — their per-def
    // write-through boxes the full canonical home).
    for r in plan.split_recvs.iter().chain(plan.write_through.iter()) {
        if let Some(&Home::Xmm(x)) = plan.reg_home.get(r) {
            if let Some(&Loc::R(h)) = map.get(&x) {
                wide.insert(h);
            }
        }
    }
    // (A NARROWED global's home needs no seeding here: its write-through
    // loads through `emit_src64_canon`, which repairs a lazy source itself.)
    // Homes prologue-filled from a pinned-string length snapshot are WIDE
    // (the u64 units count is not proven i32).
    for &hip in &plan.hoist_len_ips {
        if let Instr::GetProp { dst, .. } = proto.code[hip] {
            if let Some(&Loc::R(h)) = map.get(&xh(plan, dst)) {
                wide.insert(h);
            }
        }
    }
    for ip in s..=e {
        if let Some(d) = writes_reg(&proto.code[ip]) {
            if plan.dead.contains(&d) {
                continue; // not emitted — no def
            }
        }
        if plan.hoist_len_ips.contains(&ip) {
            continue; // prologue-filled above; body op skipped
        }
        if plan.dv_flag_elide.contains(&ip) {
            continue; // W9: elided Eq — the DV arm emits the compare inline
        }
        match proto.code[ip] {
            // i32 payloads by region admission (body op or prologue fill).
            Instr::LoadInt { .. } | Instr::LoadConst { .. } => {}
            Instr::Move { dst, src: sr } => {
                if let Home::Xmm(dx) = home(plan, dst) {
                    if let Some(&Loc::R(dh)) = map.get(&dx) {
                        if !hoist_c.contains_key(&sr) {
                            match map.get(&xh(plan, sr)) {
                                Some(&Loc::R(sh)) => copies.push((dh, sh)),
                                _ => {
                                    wide.insert(dh); // unmapped/spilled source: fail safe
                                }
                            }
                        }
                    }
                }
            }
            Instr::LoadGlobal { dst, .. } if plan.ta_recv_regs.contains(&dst) => {}
            // The split receiver's OBJECT load: memory store only, no home def.
            Instr::LoadGlobal { .. } if plan.split_recv_lg.contains(&ip) => {}
            Instr::LoadGlobal { dst, idx } => {
                if let Some(&Loc::R(dh)) = map.get(&xh(plan, dst)) {
                    match map.get(&plan.glob_home[&idx]) {
                        Some(&Loc::R(sh)) => copies.push((dh, sh)),
                        _ => {
                            wide.insert(dh); // spilled global source: fail safe
                        }
                    }
                }
            }
            Instr::StoreGlobal { idx, src: sr }
            | Instr::StoreGlobalStrict { idx, src: sr }
            | Instr::StoreGlobalResolved { idx, src: sr } => {
                if let Some(&Loc::R(dh)) = map.get(&plan.glob_home[&idx]) {
                    if !hoist_c.contains_key(&sr) {
                        match map.get(&xh(plan, sr)) {
                            Some(&Loc::R(sh)) => copies.push((dh, sh)),
                            _ => {
                                wide.insert(dh);
                            }
                        }
                    }
                }
            }
            Instr::AddInt { dst, a, imm, .. } => {
                if let Some(&Loc::R(dh)) = map.get(&xh(plan, dst)) {
                    if imm == 0 && !hoist_c.contains_key(&a) {
                        match map.get(&xh(plan, a)) {
                            Some(&Loc::R(sh)) => copies.push((dh, sh)),
                            _ => {
                                wide.insert(dh);
                            }
                        }
                    } else if imm != 0 {
                        wide.insert(dh); // real add: i53 result
                    }
                    // imm == 0 with a hoisted-const a: an i32 immediate copy.
                }
            }
            Instr::Bitwise { dst, op, .. } => {
                if let Some(&Loc::R(dh)) = map.get(&xh(plan, dst)) {
                    if matches!(op, crate::bytecode::BitwiseOp::Ushr) {
                        wide.insert(dh); // u32 result may exceed i32
                    } else {
                        bit.insert(dh);
                    }
                }
            }
            Instr::MathOp { dst, op: MathFn::Imul, argc: 2, .. } => {
                if let Some(&Loc::R(dh)) = map.get(&xh(plan, dst)) {
                    bit.insert(dh);
                }
            }
            // Pinned element reads sign-extend an i32 (Int payload / i32
            // element); pinned charCodeAt zero-extends a byte. Both i32.
            Instr::GetIndex { .. } => {}
            // W9: DV get* kinds 0-5 land in i32 range (census-neutral, like
            // charCodeAt); a getUint32 (kind 6) ranges to 2^32-1 and is NOT
            // i32-provable — its home is WIDE, or a consumer's lazy movsxd
            // would sign-mangle values > i32::MAX. charCodeAt maps to no DV
            // kind and stays neutral.
            Instr::CallMethod { dst, name, .. } => {
                if proto
                    .string_constants
                    .get(name as usize)
                    .and_then(|k| dv_get_kind(k))
                    == Some(6)
                {
                    if let Some(&Home::Xmm(dx)) = plan.reg_home.get(&dst) {
                        if let Some(&Loc::R(dh)) = map.get(&dx) {
                            wide.insert(dh);
                        }
                    }
                }
            }
            // Anything else that writes a numeric home is WIDE: Add/Sub/Mul/
            // Mod/Neg (i53 results), pinned lengths, and any op this emitter
            // does not know (it would decline the region at emission anyway).
            _ => {
                if let Some(d) = writes_reg(&proto.code[ip]) {
                    if let Some(Home::Xmm(dx)) = plan.reg_home.get(&d) {
                        if let Some(&Loc::R(dh)) = map.get(dx) {
                            wide.insert(dh);
                        }
                    }
                }
            }
        }
    }
    // Fixpoint: i32-invariant = not WIDE and copying only from i32-invariant.
    // (Resident homes only — a W10.3 spilled home never enters the universe.)
    let mut i32ok: FxHashSet<u8> = map
        .values()
        .filter_map(|l| match l {
            Loc::R(h) => Some(*h),
            Loc::S(_) => None,
        })
        .collect();
    for h in &wide {
        i32ok.remove(h);
    }
    loop {
        let mut changed = false;
        for &(dh, sh) in &copies {
            if i32ok.contains(&dh) && !i32ok.contains(&sh) {
                i32ok.remove(&dh);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let lazy: FxHashSet<u8> = bit.into_iter().filter(|h| i32ok.contains(h)).collect();
    if lazy.is_empty() {
        return (FxHashSet::default(), FxHashSet::default());
    }
    // Strict-entry closure: every home a lazy home (transitively) copies from
    // must ALSO hold an i32-range value from entry on.
    let mut strict = lazy.clone();
    loop {
        let mut changed = false;
        for &(dh, sh) in &copies {
            if strict.contains(&dh) && strict.insert(sh) {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    (lazy, strict)
}

/// Entry load into a GPR home: the Value bits are in `rax`. Same admission as
/// `emit_int_entry_load` (Int tag sign-extends; an exactly-integral double in
/// [-2^53, 2^53], except -0.0, converts; everything else takes `entry_bail`).
/// Scratch is rcx/rdx/xmm0/xmm1 — NOT r10, which may itself be a numeric home
/// here.
///
/// W8 `strict_i32`: the home is in [`lazy_sx_sets`]'s strict set, so the
/// integral-double path ADDITIONALLY requires the value to fit i32 (the Int
/// path is i32 by construction). A wider value takes `entry_bail` — nothing
/// has been computed, so resuming the header interpreted is always sound.
fn emit_int_entry_load_gpr(
    ops: &mut dynasmrt::x64::Assembler,
    home: Loc,
    entry_bail: dynasmrt::DynamicLabel,
    strict_i32: bool,
) {
    let as_double = ops.new_dynamic_label();
    let store = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rdx, rax
        ; shr rdx, 48
        ; cmp edx, INT_TAG_HI as i32
        ; jne => as_double         // not Int-tagged — try the integral-double form
    );
    match home {
        Loc::R(h) => dynasm!(ops ; movsxd Rq(h), eax), // sign-extend the i32 payload to i64
        // W10.3 spilled home: stage the canonical i64 and store to the slot.
        Loc::S(d) => dynasm!(ops ; movsxd rax, eax ; mov [rsp + d], rax),
    }
    dynasm!(ops
        ; jmp => done
        ; => as_double
        ; movq xmm0, rax
        ; cvttsd2si rcx, xmm0      // truncate toward zero (i64::MIN on NaN/Inf/overflow)
        ; cvtsi2sd xmm1, rcx
        ; ucomisd xmm0, xmm1
        ; jp => entry_bail         // unordered ⇒ NaN ⇒ a NaN-boxed non-double
        ; jne => entry_bail        // not exactly integral
        ; mov rdx, QWORD 1i64 << 53
        ; cmp rcx, rdx
        ; jg => entry_bail
        ; neg rdx
        ; cmp rcx, rdx
        ; jl => entry_bail         // outside [-2^53, 2^53]
    );
    if strict_i32 {
        dynasm!(ops
            ; movsxd rdx, ecx
            ; cmp rdx, rcx
            ; jne => entry_bail    // integral but outside i32 — see strict_i32
        );
    }
    dynasm!(ops
        ; test rcx, rcx            // reject -0.0 (same invariant as the xmm tier)
        ; jnz => store
        ; test rax, rax
        ; js => entry_bail
        ; => store
    );
    match home {
        Loc::R(h) => dynasm!(ops ; mov Rq(h), rcx),
        Loc::S(d) => dynasm!(ops ; mov [rsp + d], rcx),
    }
    dynasm!(ops ; => done);
}

/// Box the canonical i64 in `rax` into a Value in `rax` (Int if it fits i32,
/// else a double — exact, |x| ≤ 2^53). The core [`emit_int_box_from_loc`]
/// wraps with the home load.
fn emit_int_box_from_rax(ops: &mut dynasmrt::x64::Assembler) {
    let big = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; cmp rax, 0x7FFFFFFF            // > i32::MAX ?
        ; jg => big
        ; cmp rax, -0x80000000           // < i32::MIN ?
        ; jl => big
        ; mov ecx, eax                   // low 32 (zero-extended into rcx)
        ; mov rdx, QWORD INT_TAG as i64
        ; or rdx, rcx
        ; mov rax, rdx                   // Int-tagged Value
        ; jmp => done
        ; => big
        ; cvtsi2sd xmm0, rax             // exact: |rax| ≤ 2^53
        ; movq rax, xmm0                 // double Value bits
        ; => done
    );
}

/// Box the i64 home `l` into a Value in `rax` — a resident home loads from its
/// GPR (the pre-W10.3 `emit_int_box_from_gpr`, byte-identical), a spilled home
/// from its slot (canonical by the slot invariant). The GPR twin of
/// `emit_int_box_from_home`.
fn emit_int_box_from_loc(ops: &mut dynasmrt::x64::Assembler, l: Loc) {
    match l {
        Loc::R(h) => dynasm!(ops ; mov rax, Rq(h)),
        Loc::S(d) => dynasm!(ops ; mov rax, [rsp + d]),
    }
    emit_int_box_from_rax(ops);
}

/// [`emit_int_box_from_loc`], BRANCHLESS — for the narrowed-global
/// write-through, which boxes a value of random magnitude (a `getUint32`
/// swizzle alternates int-range and above ~every iteration, and the
/// two-compare box then eats a mispredict per iteration on the hot path).
/// Same canonical Value bits as `emit_int_box_from_rax`: the Int tag on the
/// zero-extended low 32 when the value fits i32 exactly, else the cvtsi2sd
/// double (exact — every home is |x| ≤ 2^53 by the tier's guard invariant).
/// Both candidates are computed, `cmove` selects. A lazy source
/// canonicalizes on the load. Scratch: rax/rcx/rdx/xmm0; clobbers FLAGS.
fn emit_int_box_branchless_from_loc(
    ops: &mut dynasmrt::x64::Assembler,
    l: Loc,
    lazy: &FxHashSet<u8>,
) {
    emit_src64_canon(ops, loc_src(l), 0, lazy); // rax = canonical i64
    dynasm!(ops
        ; cvtsi2sd xmm0, rax            // double candidate (exact ≤ 2^53)
        ; mov ecx, eax                  // zero-extended low 32
        ; mov rdx, QWORD INT_TAG as i64
        ; or rdx, rcx                   // Int-tagged candidate
        ; movsxd rcx, eax
        ; cmp rcx, rax                  // ZF ⇔ the value IS its low 32 (i32)
        ; movq rax, xmm0                // (movq leaves FLAGS alone)
        ; cmove rax, rdx
    );
}

/// B94 write-through on GPR homes (W8) — the twin of `emit_int_wt`: a numeric
/// def of a split receiver (or write-through register) must reach MEMORY boxed
/// as well as its home, because this emitter's flush_exit deliberately skips
/// the register — memory is what the interpreter reads on ANY exit, including
/// one taken while the slot holds the receiver OBJECT (which a home flush
/// would clobber). Emitted right after the def's home store and BEFORE any i53
/// guard (whose exit resumes at ip+1 with the result expected flushed).
/// `known_i32` (a non-`>>>` Bitwise result or `Math.imul`) takes the
/// branchless int-tag; the generic path boxes exactly as flush_exit would.
/// Scratch: rax/rcx/rdx/xmm0 — never a home (the map hands those out from
/// r15/rbp/r8..r11/rsi/rdi/r13/r14 only). Returns whether anything was
/// emitted (the caller must then drop any live compare-flag fusion — the
/// boxing clobbers FLAGS).
fn emit_int_wt_gpr(
    ops: &mut dynasmrt::x64::Assembler,
    plan: &RegionPlan,
    map: &FxHashMap<u8, Loc>,
    dst: u16,
    known_i32: bool,
) -> bool {
    if !plan.split_recvs.contains(&dst) && !plan.write_through.contains(&dst) {
        return false;
    }
    if let Some(&Home::Xmm(x)) = plan.reg_home.get(&dst) {
        if let Some(&l) = map.get(&x) {
            if known_i32 {
                // Zero-extend the i32 payload (a 32-bit load/mov zero-extends;
                // a W10.3 slot holds the canonical i64, whose low 32 IS the
                // payload — the def's raw slot store already ran).
                match l {
                    Loc::R(h) => dynasm!(ops ; mov eax, Rd(h)),
                    Loc::S(d) => dynasm!(ops ; mov eax, [rsp + d]),
                }
                dynasm!(ops
                    ; mov rcx, QWORD INT_TAG as i64
                    ; or rax, rcx
                    ; mov [rbx + dreg(dst)], rax
                );
            } else {
                emit_int_box_from_loc(ops, l);
                dynasm!(ops ; mov [rbx + dreg(dst)], rax);
            }
            return true;
        }
    }
    false
}

/// `*resume_ip = v` — through the frame slot that replaced the rsi pointer
/// (rsi is a numeric home in this mode). rax is dead at every call site.
fn emit_store_ip(ops: &mut dynasmrt::x64::Assembler, ip_slot: i32, v: i32) {
    dynasm!(ops ; mov rax, [rsp + ip_slot] ; mov DWORD [rax], v);
}

/// The i53 range guard on a GPR home: same trick and same resume-at-ip+1
/// contract as `emit_i53_guard` (r13 = 2^53, r14 = 2^54, prologue-loaded).
/// With `inline` the two constants come as movabs immediates instead — used
/// when r13/r14 were handed out as homes to make the region fit.
fn emit_i53_guard_gpr(
    ops: &mut dynasmrt::x64::Assembler,
    h: Loc,
    resume_ip: i32,
    ip_slot: i32,
    inline: bool,
    flush_exit: dynasmrt::DynamicLabel,
) {
    let done = ops.new_dynamic_label();
    match h {
        Loc::R(h) => dynasm!(ops ; mov rax, Rq(h)),
        // W10.3 spilled dst: the def's raw slot store already ran.
        Loc::S(d) => dynasm!(ops ; mov rax, [rsp + d]),
    }
    if inline {
        dynasm!(ops
            ; mov rcx, QWORD TWO_POW_53
            ; add rax, rcx           // + 2^53 (no i64 overflow: |x| ≤ 2^54 here)
            ; mov rcx, QWORD TWO_POW_54
            ; cmp rax, rcx
            ; jbe => done
        );
    } else {
        dynasm!(ops
            ; add rax, r13           // + 2^53 (no i64 overflow: |x| ≤ 2^54 here)
            ; cmp rax, r14           // 2^54
            ; jbe => done
        );
    }
    emit_store_ip(ops, ip_slot, resume_ip); // resume AFTER this op (result flushed)
    dynasm!(ops
        ; jmp => flush_exit
        ; => done
    );
}

/// Load operand `s` into 64-bit scratch `Rq(scratch)` (imm sign-extends, the
/// same i64 the home would hold). For arms with no better imm form.
fn emit_src64(ops: &mut dynasmrt::x64::Assembler, s: Src, scratch: u8) {
    match s {
        Src::R(h) => {
            if h != scratch {
                dynasm!(ops ; mov Rq(scratch), Rq(h));
            }
        }
        Src::I(v) => dynasm!(ops ; mov Rq(scratch), v),
        // W10.3 spill slot: always the canonical sign-extended i64.
        Src::S(d) => dynasm!(ops ; mov Rq(scratch), [rsp + d]),
    }
}

/// W8: canonicalize a possibly-lazy home IN PLACE before an i64-consuming op.
/// For a lazy home the value fits i32 (the [`lazy_sx_sets`] invariant), so
/// `movsxd` is exact under either representation — and a plain no-op when the
/// home is already sign-extended. Emits nothing for a non-lazy home (those
/// are canonical at all times). Never touches flags.
fn emit_canon_home(ops: &mut dynasmrt::x64::Assembler, h: u8, lazy: &FxHashSet<u8>) {
    if lazy.contains(&h) {
        dynasm!(ops ; movsxd Rq(h), Rd(h));
    }
}

/// [`emit_src64`] with W8 lazy-home awareness: a lazy source loads through
/// `movsxd` (same cost as the plain mov) so the scratch holds the canonical
/// i64 the consumer expects. The home itself is left untouched.
fn emit_src64_canon(ops: &mut dynasmrt::x64::Assembler, s: Src, scratch: u8, lazy: &FxHashSet<u8>) {
    match s {
        Src::R(h) if lazy.contains(&h) => dynasm!(ops ; movsxd Rq(scratch), Rd(h)),
        _ => emit_src64(ops, s, scratch),
    }
}

/// W8 home-to-home copy (`Move` / `LoadGlobal` / `StoreGlobal` / `AddInt +0`):
/// copying INTO a lazy home may carry either representation (its consumers
/// re-canonicalize), but copying a lazy source OUT to a canonical home must
/// sign-extend — that includes copies into a split/write-through home, whose
/// per-def boxing reads the full canonical i64. The caller handles the
/// `sg == d` nothing-to-emit case (it also preserves compare flags there).
fn emit_home_copy(ops: &mut dynasmrt::x64::Assembler, d: u8, s: Src, lazy: &FxHashSet<u8>) {
    match s {
        Src::R(sg) if lazy.contains(&sg) && !lazy.contains(&d) => {
            dynasm!(ops ; movsxd Rq(d), Rd(sg));
        }
        s_ => emit_src64(ops, s_, d),
    }
}

/// W10.3 [`emit_home_copy`] generalized to a possibly-spilled DST: a resident
/// dst takes the existing home-copy forms (byte-identical; a spill-slot source
/// reads as a canonical qword), a spilled dst stages through rax — a lazy
/// source canonicalizes on the way (slots always hold the canonical i64), an
/// immediate stores directly (`mov r/m64, imm32` sign-extends). All plain
/// movs: compare flags survive. The caller handles the same-location
/// nothing-to-emit case.
fn emit_loc_copy(ops: &mut dynasmrt::x64::Assembler, d: Loc, s: Src, lazy: &FxHashSet<u8>) {
    match d {
        Loc::R(dg) => emit_home_copy(ops, dg, s, lazy),
        Loc::S(dd) => match s {
            Src::R(sg) if lazy.contains(&sg) => {
                dynasm!(ops ; movsxd rax, Rd(sg) ; mov [rsp + dd], rax);
            }
            Src::R(sg) => dynasm!(ops ; mov rax, Rq(sg) ; mov [rsp + dd], rax),
            Src::I(v) => dynasm!(ops ; mov QWORD [rsp + dd], v),
            Src::S(sd) => dynasm!(ops ; mov rax, [rsp + sd] ; mov [rsp + dd], rax),
        },
    }
}

/// GPR-home INT region codegen. Same plan, same guards, same exits as
/// `compile_region_int` — only the home register file differs. A non-`Emitted`
/// result sends the caller back to the xmm emitter, except `PoolOverflow`,
/// which invites one shared-home re-plan first (B119).
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_region_int_gpr(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    ta_plan: &TaPinPlan,
    ta_snapshot: usize,
    plan: &RegionPlan,
    // See `compile_region_int_maybe_cold`: the deopt resume map + hoisted entry
    // guards for a SPLICE-FLATTENED body (default = an ordinary region).
    entry: &IntEntry<'_>,
    meter: Option<crate::codegen::meter::Meter>,
    allow_spill: bool,
) -> GprAttempt {
    let (s, e) = (start as usize, end as usize);
    // W20 M2: a region with an admitted `arr.push(int)` is out of scope for the
    // GPR sub-mode. The xmm INT emitter states its call-save set in three lines
    // (the bool gprs in use, plus any numeric home in xmm2..xmm5; xmm6..15 and
    // rbx/rsi/rdi/r12/r13/r14 are non-volatile), while this emitter's pool mixes
    // r15/rbp/rsi/rdi/r13/r14 with whichever `BOOL_GPRS` the bools left free --
    // the same set would have to be re-derived per plan. Refusing costs nothing:
    // `gpr_home_map` already requires a Bitwise/imul to engage, which a
    // push-bearing scan loop does not have.
    if region_has_arr_push(proto, s, e, ta_plan) {
        return GprAttempt::OutOfScope;
    }
    let rip = |ip: usize| -> i32 {
        // See `compile_region_int`'s twin: empty => `ip` is already the resume ip.
        entry.resume.get(ip.wrapping_sub(s)).map_or(ip as i32, |&r| r as i32)
    };
    // W10.3 frame spill slots sit AFTER the resume-ip slot (see the frame
    // layout at the prologue below): slot k at [rsp + spill_base + 8k].
    let spill_base = 40 + 32 * ta_plan.pins.len() as i32;
    let (map, hoist_c, inline_guards, n_spill) =
        match gpr_home_map(proto, plan, s, e, meter.is_some(), allow_spill, spill_base) {
            Ok(m) => m,
            Err(true) => return GprAttempt::PoolOverflow,
            Err(false) => return GprAttempt::OutOfScope,
        };
    // W8: homes that defer sign-extension, and the entry loads that must be
    // i32-strict to license it (empty sets under ZIPP_NO_GPR_LAZYSX=1; split,
    // write-through and W10.3 spilled homes are never members — see
    // `lazy_sx_sets`).
    let (lazy, strict) = lazy_sx_sets(proto, plan, s, e, &map, &hoist_c);
    // Home location of raw xmm-index `x` / dst register `r` (dsts are never
    // hoisted-const, so their home is always mapped).
    let gx = |x: u8| map[&x];
    let g = |r: u16| gx(xh(plan, r));
    // Operand register `r` as this emitter reads it.
    let src = |r: u16| -> Src {
        match hoist_c.get(&r) {
            Some(&v) => Src::I(v),
            None => loc_src(g(r)),
        }
    };
    if std::env::var_os("ZIPP_JITLOG").is_some() {
        if n_spill > 0 {
            eprintln!(
                "[jit] INT region [{start},{end}] GPR homes engaged ({} homes, {} lazy-sx, {} spilled)",
                map.len(),
                lazy.len(),
                n_spill
            );
        } else {
            eprintln!(
                "[jit] INT region [{start},{end}] GPR homes engaged ({} homes, {} lazy-sx)",
                map.len(),
                lazy.len()
            );
        }
        // W8 mechanism proof: name every split receiver this GPR body carries
        // (the xmm/double tiers print the same line for theirs).
        let mut srs: Vec<u16> = plan.split_recvs.iter().copied().collect();
        srs.sort_unstable();
        for sr in srs {
            eprintln!("[jit] INT-GPR region [{start},{end}] B94 split receiver r{sr}");
        }
    }

    let mut ops = match dynasmrt::x64::Assembler::new() {
        Ok(a) => a,
        Err(_) => {
            decline_emit("int-gpr-emit: assembler alloc failed");
            return GprAttempt::OutOfScope;
        }
    };
    let in_region: Vec<_> = (s..=e).map(|_| ops.new_dynamic_label()).collect();
    let mut exit_stubs: FxHashMap<u32, dynasmrt::DynamicLabel> = FxHashMap::default();
    let flush_exit = ops.new_dynamic_label();
    let entry_bail = ops.new_dynamic_label();
    let blocks = crate::codegen::meter::block_map(meter, &proto.code, s, e);
    let lbl = |ip: u32, in_region: &[dynasmrt::DynamicLabel]| in_region[(ip - start) as usize];

    // ── prologue ── the xmm tier's, plus r15/rbp pushed (they join the home
    // pool) and minus the xmm6..15 save area (no xmm homes; only the volatile
    // xmm0/xmm1 are scratched). 8 pushes keep rsp ≡ 8 (mod 16) before the subs,
    // so both call sites below stay 16-aligned. Frame: [shadow 32]
    // [TA snapshot slots 32·n_ta][resume-ip pointer 8][W10.3 spill slots
    // 8·n_spill, rounded up to 16]; the ip slot frees rsi. The spill area must
    // be a 16-byte multiple so frame stays ≡ 8 (mod 16) — the pin-snapshot
    // `call rax` sites below run after `sub rsp, frame` and need rsp ≡ 0.
    let n_ta = ta_plan.pins.len() as i32;
    let frame = 40 + 32 * n_ta + ((8 * n_spill as i32 + 15) & !15);
    let ta_base = 32i32;
    let ip_slot = 32 + 32 * n_ta;
    debug_assert_eq!(spill_base, 40 + 32 * n_ta);
    dynasm!(ops
        ; push rbx
        ; push rsi
        ; push rdi
        ; push r12
        ; push r13
        ; push r14
        ; push r15
        ; push rbp
        ; mov rbx, rcx
        ; mov rsi, rdx
        ; mov rdi, r8
        ; sub rsp, 40
        ; mov rcx, rdi
        ; mov rax, QWORD globals_base_helper as i64
        ; call rax
        ; mov r12, rax
        ; add rsp, 40
        ; mov r13, QWORD TWO_POW_53           // guard: + 2^53
        ; mov r14, QWORD TWO_POW_54           // guard: unsigned upper bound 2^54
        ; sub rsp, frame
        ; mov [rsp + ip_slot], rsi            // rsi becomes a numeric home
    );
    // Same frame contract as the xmm tier (shadow at the bottom, rsp 16-aligned)
    // — always, here, since `ta_base` is 32 whether or not the region pins.
    emit_int_splice_entry_guards(&mut ops, entry, true, entry_bail);
    // Pinned-view snapshots BEFORE any home is loaded (the helper clobbers
    // volatile registers; every home here is either callee-saved or filled
    // later). Same slot layout and guards as the xmm tier.
    for (j, pin) in ta_plan.pins.iter().enumerate() {
        match pin.src {
            TaPinSrc::Global(gi) => dynasm!(ops ; mov rdx, [r12 + (gi as i32) * 8]),
            TaPinSrc::Reg(r) => dynasm!(ops ; mov rdx, [rbx + dreg(r)]),
        }
        dynasm!(ops
            ; mov rcx, rdi                      // vm
            ; mov r8d, pin.kind as i32          // expected element kind
            ; lea r9, [rsp + ta_base + 32 * j as i32] // out: {obj_bits,base,len}
            ; mov rax, QWORD ta_snapshot as i64
            ; call rax
        );
    }
    // ── W7 hoisted pin identity guards ── same contract as the xmm tier: one
    // snapshot-validity check per hoisted pin replaces the per-access source
    // load + compare (the snapshot was just taken FROM the source, and the
    // region provably cannot change either — see `hoistable_pins`). A miss
    // takes `entry_bail`. Runs BEFORE any home is loaded, so no state to keep.
    for j in 0..ta_plan.pins.len() {
        if plan.hoist_pins.contains(&(j as u8)) {
            dynasm!(ops ; cmp QWORD [rsp + ta_base + 32 * j as i32], 0 ; je => entry_bail);
        }
    }
    // Live-in loads (globals, regs, then bools — same order and same guards as
    // the xmm tier; the bool loader's rdx scratch never aliases a home). A
    // W10.3 spilled home is never strict (nothing lazy ever copies from it).
    let is_strict = |l: Loc| match l {
        Loc::R(h) => strict.contains(&h),
        Loc::S(_) => false,
    };
    for &(gi, x) in &plan.live_in_globs {
        dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]);
        // W10: a strict-entry global (the DV prover's verified accumulators)
        // must bail on a value wider than i32 — that bail is what licenses
        // the i32 entry interval its guard elisions were proven under.
        let strict = is_strict(gx(x)) || plan.strict_entry_globs.contains(&gi);
        emit_int_entry_load_gpr(&mut ops, gx(x), entry_bail, strict);
    }
    for &(r, x) in &plan.live_in_regs {
        dynasm!(ops ; mov rax, [rbx + dreg(r)]);
        emit_int_entry_load_gpr(&mut ops, gx(x), entry_bail, is_strict(gx(x)));
    }
    emit_bool_home_zero(&mut ops, plan);
    for &(r, gb) in &plan.live_in_bools {
        dynasm!(ops ; mov rax, [rbx + dreg(r)]);
        emit_bool_entry_load(&mut ops, gb, entry_bail);
    }
    // W10.3 review fix (B123): a write-through Bool home is shareable (no
    // live-in entry) but flush_exit boxes EVERY bool home at every exit — an
    // exit before the flag's first in-region def would box caller garbage
    // into a read-outside slot. Load the interpreter's value at entry so the
    // home is faithful on every path; a non-Bool slot (the reg's first-ever
    // def is inside the region) takes entry_bail, and the next OSR attempt —
    // after the interpreter has run one iteration — finds a real Bool.
    for r in plan.write_through.iter() {
        if let Some(&Home::Gpr(gb)) = plan.reg_home.get(r) {
            if !plan.live_in_bools.iter().any(|&(br, _)| br == *r) {
                dynasm!(ops ; mov rax, [rbx + dreg(*r)]);
                emit_bool_entry_load(&mut ops, gb, entry_bail);
            }
        }
    }
    // Hoisted constants: only a MAPPED home needs the fill (a home is mapped
    // when a non-hoisted reg — e.g. a Move alias — shares it; an unmapped
    // hoisted reg reads as an immediate everywhere instead). A spilled home's
    // fill stores the sign-extended imm32 — canonical by the slot invariant.
    for &hip in &plan.hoist_ips {
        if let Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } = proto.code[hip] {
            match map.get(&xh(plan, dst)) {
                Some(&Loc::R(h)) => dynasm!(ops ; mov Rq(h), hoist_c[&dst]),
                Some(&Loc::S(d)) => dynasm!(ops ; mov QWORD [rsp + d], hoist_c[&dst]),
                None => {}
            }
        }
    }
    // ── W7 hoisted pinned-STRING lengths ── identity entry-guarded above and
    // region-invariant, so the snapshot `units` fills the dst home once; the
    // body op is skipped. The dst is never a hoisted CONSTANT, so its home is
    // always mapped.
    for &hip in &plan.hoist_len_ips {
        if let Instr::GetProp { dst, .. } = proto.code[hip] {
            let j = ta_plan.access[&hip] as usize;
            match g(dst) {
                Loc::R(h) => dynasm!(ops ; mov Rq(h), [rsp + ta_base + 32 * j as i32 + 16]),
                Loc::S(d) => dynasm!(ops
                    ; mov rax, [rsp + ta_base + 32 * j as i32 + 16]
                    ; mov [rsp + d], rax
                ),
            }
        }
    }
    // (No addint_imm_home / gpr_const fills: immediates encode directly here.)
    dynasm!(ops ; jmp => lbl(start, &in_region));

    // ── body ── same fusion/DCE/metering structure as the xmm emitter.
    let mut flag_cmp: Option<(usize, u16, Cmp)> = None;
    for ip in s..=e {
        let rip_at = rip(ip);
        let rip_after = rip(ip + 1);
        dynasm!(ops ; => lbl(ip as u32, &in_region));
        let charged = crate::codegen::meter::charge_block(&mut ops, &blocks, ip, &mut exit_stubs);
        if let Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } = proto.code[ip] {
            if plan.hoisted.contains(&dst) {
                continue;
            }
        }
        // A W7-hoisted pinned-STRING length was prologue-filled — skip the
        // body op (nothing emitted, so flags survive like a hoisted const).
        if plan.hoist_len_ips.contains(&ip) {
            continue;
        }
        // W9: a DV-flag-fused Eq is ELIDED — the DV arm at ip+1 emits the
        // compare inline (nothing emitted here, so flags survive).
        if plan.dv_flag_elide.contains(&ip) {
            continue;
        }
        if let Some(d) = writes_reg(&proto.code[ip]) {
            if plan.dead.contains(&d) {
                continue;
            }
        }
        let prev_flag = flag_cmp.take().filter(|_| !charged);
        // Set by arms that emit their own B94 write-through BEFORE an i53 guard
        // (the guard's exit resumes at ip+1 expecting the result flushed); the
        // generic post-op hook below then skips the duplicate store.
        let mut wt_pre = false;
        match proto.code[ip] {
            Instr::LoadInt { dst, val } => {
                if plan.slot_consts.contains_key(&dst) {
                    // Slot-materialized: the boxed Int goes straight to the
                    // frame slot, exactly when the interpreter's def would —
                    // the reg has no home and the flush never touches it.
                    let bits = INT_TAG | val as u32 as u64;
                    dynasm!(ops ; mov rax, QWORD bits as i64 ; mov [rbx + dreg(dst)], rax);
                } else {
                    match g(dst) {
                        Loc::R(h) => dynasm!(ops ; mov Rq(h), val),
                        Loc::S(d) => dynasm!(ops ; mov QWORD [rsp + d], val), // sign-extends: canonical
                    }
                }
            }
            Instr::LoadConst { dst, idx } => {
                // region admission guaranteed an Int constant (i32 payload).
                let v = proto.constants[idx as usize].bits() as u32 as i32;
                if plan.slot_consts.contains_key(&dst) {
                    let bits = INT_TAG | v as u32 as u64;
                    dynasm!(ops ; mov rax, QWORD bits as i64 ; mov [rbx + dreg(dst)], rax);
                } else {
                    match g(dst) {
                        Loc::R(h) => dynasm!(ops ; mov Rq(h), v),
                        Loc::S(d) => dynasm!(ops ; mov QWORD [rsp + d], v),
                    }
                }
            }
            Instr::Move { dst, src: sr } => match home(plan, dst) {
                Home::Xmm(dx) => {
                    let d = gx(dx);
                    match (src(sr), d) {
                        (Src::R(sg), Loc::R(dg)) if sg == dg => flag_cmp = prev_flag, // nothing emitted
                        (Src::S(sd), Loc::S(dd)) if sd == dd => flag_cmp = prev_flag,
                        (s_, _) => emit_loc_copy(&mut ops, d, s_, &lazy),
                    }
                }
                Home::Gpr(d) => {
                    let sg = gh(plan, sr);
                    dynasm!(ops ; mov Rq(d), Rq(sg));
                }
            },
            // ── pinned receiver / B94 split receiver ── the object goes to the
            // register's memory slot, which stays authoritative for it through
            // the region (mirrors the xmm arm; `emit_recv_slot_store` carries the
            // reasoning). rax is outside this emitter's GPR pool.
            Instr::LoadGlobal { dst, idx }
                if plan.ta_recv_regs.contains(&dst) || plan.split_recv_lg.contains(&ip) =>
            {
                emit_recv_slot_store(&mut ops, dst, idx);
                flag_cmp = prev_flag;
            }
            Instr::LoadGlobal { dst, idx } => {
                let (d, gg) = (g(dst), gx(plan.glob_home[&idx]));
                if d != gg {
                    emit_loc_copy(&mut ops, d, loc_src(gg), &lazy);
                } else {
                    flag_cmp = prev_flag;
                }
            }
            Instr::StoreGlobal { idx, src: sr }
            | Instr::StoreGlobalStrict { idx, src: sr }
            | Instr::StoreGlobalResolved { idx, src: sr } => {
                let gg = gx(plan.glob_home[&idx]);
                let same = matches!((src(sr), gg), (Src::R(sg), Loc::R(gr)) if sg == gr)
                    || matches!((src(sr), gg), (Src::S(sd), Loc::S(gd)) if sd == gd);
                if !same {
                    emit_loc_copy(&mut ops, gg, src(sr), &lazy);
                }
                if plan.narrow_globs.contains(&idx) {
                    // Write-through: flush_exit skips this slot (at a
                    // mid-iteration exit the narrowed home may already belong
                    // to another value), so memory must receive the value HERE
                    // — after this, `[r12 + 8*idx]` holds the last-stored
                    // value on every path, exactly what the interpreter would
                    // have. Branchless: the stored value's magnitude is data
                    // (a swizzled u32 crosses i32::MAX at random), and the
                    // two-compare box mispredicted per iteration. Clobbers
                    // FLAGS: no compare fusion survives this arm.
                    emit_int_box_branchless_from_loc(&mut ops, gg, &lazy);
                    dynasm!(ops ; mov [r12 + (idx as i32) * 8], rax);
                } else if same {
                    flag_cmp = prev_flag;
                }
            }
            Instr::Add { dst, a, b } | Instr::Sub { dst, a, b } => {
                let add = matches!(proto.code[ip], Instr::Add { .. });
                let dl = g(dst);
                let (sa, sb) = (src(a), src(b));
                let spilled = matches!(dl, Loc::S(_))
                    || matches!(sa, Src::S(_))
                    || matches!(sb, Src::S(_));
                if spilled {
                    // ── W10.3 ── any spilled participant stages through rax:
                    // load a canonical (a slot IS canonical), fold b in, store
                    // the canonical i64 to the dst home or slot.
                    emit_src64_canon(&mut ops, sa, 0, &lazy); // rax = a
                    match sb {
                        Src::R(bg) => {
                            emit_canon_home(&mut ops, bg, &lazy);
                            if add {
                                dynasm!(ops ; add rax, Rq(bg));
                            } else {
                                dynasm!(ops ; sub rax, Rq(bg));
                            }
                        }
                        Src::I(bi) => {
                            if add {
                                dynasm!(ops ; add rax, bi);
                            } else {
                                dynasm!(ops ; sub rax, bi);
                            }
                        }
                        Src::S(bd) => {
                            if add {
                                dynasm!(ops ; add rax, [rsp + bd]);
                            } else {
                                dynasm!(ops ; sub rax, [rsp + bd]);
                            }
                        }
                    }
                    match dl {
                        Loc::R(dg) => dynasm!(ops ; mov Rq(dg), rax),
                        Loc::S(dd) => dynasm!(ops ; mov [rsp + dd], rax),
                    }
                    wt_pre = emit_int_wt_gpr(&mut ops, plan, &map, dst, false);
                    if !plan.elide_guard.contains(&ip) {
                        emit_i53_guard_gpr(&mut ops, dl, rip_after, ip_slot, inline_guards, flush_exit);
                    }
                } else {
                let d = dst_reg(dl); // resident (nothing spilled on this op)
                // W8: 64-bit arithmetic needs canonical operands (dst is
                // never lazy — an Add/Sub def is WIDE).
                if let Src::R(h) = src(a) {
                    emit_canon_home(&mut ops, h, &lazy);
                }
                if let Src::R(h) = src(b) {
                    emit_canon_home(&mut ops, h, &lazy);
                }
                match (src(a), src(b)) {
                    (Src::R(ag), Src::R(bg)) => {
                        if d == ag {
                            if add {
                                dynasm!(ops ; add Rq(d), Rq(bg));
                            } else {
                                dynasm!(ops ; sub Rq(d), Rq(bg));
                            }
                        } else if d == bg {
                            if add {
                                dynasm!(ops ; add Rq(d), Rq(ag)); // commutative
                            } else {
                                // dst == b (and ≠ a): compute in rax first.
                                dynasm!(ops ; mov rax, Rq(ag) ; sub rax, Rq(bg) ; mov Rq(d), rax);
                            }
                        } else if add {
                            dynasm!(ops ; mov Rq(d), Rq(ag) ; add Rq(d), Rq(bg));
                        } else {
                            dynasm!(ops ; mov Rq(d), Rq(ag) ; sub Rq(d), Rq(bg));
                        }
                    }
                    (a_, b_) => {
                        // At least one hoisted-const immediate.
                        match (a_, b_) {
                            (Src::R(ag), Src::I(bi)) => {
                                if d != ag {
                                    dynasm!(ops ; mov Rq(d), Rq(ag));
                                }
                                if add {
                                    dynasm!(ops ; add Rq(d), bi);
                                } else {
                                    dynasm!(ops ; sub Rq(d), bi);
                                }
                            }
                            (Src::I(ai), Src::R(bg)) => {
                                if add {
                                    if d != bg {
                                        dynasm!(ops ; mov Rq(d), Rq(bg));
                                    }
                                    dynasm!(ops ; add Rq(d), ai);
                                } else {
                                    dynasm!(ops ; mov rax, ai ; sub rax, Rq(bg) ; mov Rq(d), rax);
                                }
                            }
                            (Src::I(ai), Src::I(bi)) => {
                                // Two i32 immediates: fold (no i64 overflow possible).
                                let v = if add { ai as i64 + bi as i64 } else { ai as i64 - bi as i64 };
                                dynasm!(ops ; mov rax, QWORD v ; mov Rq(d), rax);
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                wt_pre = emit_int_wt_gpr(&mut ops, plan, &map, dst, false);
                if !plan.elide_guard.contains(&ip) {
                    emit_i53_guard_gpr(&mut ops, dl, rip_after, ip_slot, inline_guards, flush_exit);
                }
                }
            }
            Instr::Mul { dst, a, b } => {
                let dl = g(dst);
                if let Some(&(val_reg, shift)) = plan.mul_shift.get(&ip) {
                    // Guard-elided multiply by a constant power of two.
                    if let Some(&v) = hoist_c.get(&val_reg) {
                        // BOTH operands are hoisted constants (the swizzle
                        // bound `4096 * 4`): the shifted product is itself a
                        // compile-time constant, exact in i64 (the elided
                        // guard proved it fits i53). `g(val_reg)` must not
                        // run here — a hoisted-only home is deliberately
                        // unmapped, and the lookup panicked the first time a
                        // const×const Mul region engaged this tier.
                        let c = (v as i64) << shift;
                        match dl {
                            Loc::R(d) => dynasm!(ops ; mov Rq(d), QWORD c),
                            Loc::S(dd) => dynasm!(ops ; mov rax, QWORD c ; mov [rsp + dd], rax),
                        }
                    } else {
                    let vg = g(val_reg);
                    match (dl, vg) {
                        (Loc::R(d), Loc::R(vr)) => {
                            if d != vr {
                                emit_home_copy(&mut ops, d, Src::R(vr), &lazy); // d is never lazy (WIDE def)
                            } else {
                                emit_canon_home(&mut ops, d, &lazy);
                            }
                            dynasm!(ops ; shl Rq(d), shift as i8);
                        }
                        _ => {
                            // W10.3: stage through rax (slots are canonical).
                            emit_src64_canon(&mut ops, loc_src(vg), 0, &lazy);
                            dynasm!(ops ; shl rax, shift as i8);
                            match dl {
                                Loc::R(d) => dynasm!(ops ; mov Rq(d), rax),
                                Loc::S(dd) => dynasm!(ops ; mov [rsp + dd], rax),
                            }
                        }
                    }
                    }
                } else if plan.elide_guard.contains(&ip) {
                    emit_src64_canon(&mut ops, src(a), 0, &lazy); // rax
                    match src(b) {
                        Src::R(bg) => {
                            emit_canon_home(&mut ops, bg, &lazy);
                            dynasm!(ops ; imul rax, Rq(bg));
                        }
                        Src::I(bi) => dynasm!(ops ; imul rax, rax, bi),
                        Src::S(bd) => dynasm!(ops ; imul rax, [rsp + bd]),
                    }
                    match dl {
                        Loc::R(d) => dynasm!(ops ; mov Rq(d), rax),
                        Loc::S(dd) => dynasm!(ops ; mov [rsp + dd], rax),
                    }
                } else {
                    // Same i64-overflow / i53 split as the xmm arm.
                    let ovf = ops.new_dynamic_label();
                    let done = ops.new_dynamic_label();
                    emit_src64_canon(&mut ops, src(a), 0, &lazy); // rax
                    match src(b) {
                        Src::R(bg) => {
                            emit_canon_home(&mut ops, bg, &lazy);
                            dynasm!(ops ; imul rax, Rq(bg));
                        }
                        Src::I(bi) => dynasm!(ops ; imul rax, rax, bi),
                        Src::S(bd) => dynasm!(ops ; imul rax, [rsp + bd]),
                    }
                    dynasm!(ops ; jo => ovf); // i64 overflow → redo in interp at THIS ip
                    match dl {
                        Loc::R(d) => dynasm!(ops ; mov Rq(d), rax),
                        Loc::S(dd) => dynasm!(ops ; mov [rsp + dd], rax),
                    }
                    dynasm!(ops
                        ; jmp => done
                        ; => ovf
                    );
                    emit_store_ip(&mut ops, ip_slot, rip_at); // dst not written
                    dynasm!(ops
                        ; jmp => flush_exit
                        ; => done
                    );
                    wt_pre = emit_int_wt_gpr(&mut ops, plan, &map, dst, false);
                    emit_i53_guard_gpr(&mut ops, dl, rip_after, ip_slot, inline_guards, flush_exit);
                }
            }
            Instr::Mod { dst, a, b } => {
                // Same semantics and bails as the xmm arm (see its comment).
                let dl = g(dst);
                let zbail = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                let store = ops.new_dynamic_label();
                emit_src64_canon(&mut ops, src(a), 0, &lazy); // rax = dividend
                emit_src64_canon(&mut ops, src(b), 1, &lazy); // rcx = divisor
                dynasm!(ops
                    ; test rcx, rcx
                    ; jz => zbail              // % 0 → NaN → redo in interp
                    ; cqo                       // sign-extend rax into rdx:rax
                    ; idiv rcx                  // rdx = remainder
                    ; test rdx, rdx
                    ; jnz => store
                );
                emit_src64_canon(&mut ops, src(a), 1, &lazy); // zero rem from a negative dividend is -0
                dynasm!(ops
                    ; test rcx, rcx
                    ; js => zbail
                    ; => store
                );
                match dl {
                    Loc::R(d) => dynasm!(ops ; mov Rq(d), rdx),
                    Loc::S(dd) => dynasm!(ops ; mov [rsp + dd], rdx),
                }
                dynasm!(ops
                    ; jmp => done
                    ; => zbail
                );
                emit_store_ip(&mut ops, ip_slot, rip_at); // resume at THIS op (dst unwritten)
                dynasm!(ops
                    ; jmp => flush_exit
                    ; => done
                );
            }
            Instr::AddInt { dst, a, imm, .. } => {
                let dl = g(dst);
                match (src(a), dl) {
                    (Src::R(ag), Loc::R(d)) => {
                        if imm == 0 {
                            if d != ag {
                                emit_home_copy(&mut ops, d, Src::R(ag), &lazy);
                            } else {
                                flag_cmp = prev_flag;
                            }
                        } else {
                            // W8: 64-bit add needs a canonical operand (dst
                            // is never lazy — a real AddInt def is WIDE).
                            emit_canon_home(&mut ops, ag, &lazy);
                            if d != ag {
                                dynasm!(ops ; mov Rq(d), Rq(ag));
                            }
                            dynasm!(ops ; add Rq(d), imm); // sign-extended imm32 == i64 add
                            wt_pre = emit_int_wt_gpr(&mut ops, plan, &map, dst, false);
                            if !plan.elide_guard.contains(&ip) {
                                emit_i53_guard_gpr(&mut ops, dl, rip_after, ip_slot, inline_guards, flush_exit);
                            }
                        }
                    }
                    (Src::I(ai), Loc::R(d)) => {
                        // Fold const + imm (both i32 — cannot overflow i64).
                        let v = ai as i64 + imm as i64;
                        dynasm!(ops ; mov rax, QWORD v ; mov Rq(d), rax);
                    }
                    // ── W10.3: a spilled participant ──
                    (sa_, _) => {
                        if imm == 0 {
                            match (sa_, dl) {
                                (Src::S(sd), Loc::S(dd)) if sd == dd => flag_cmp = prev_flag,
                                _ => emit_loc_copy(&mut ops, dl, sa_, &lazy),
                            }
                        } else {
                            // Stage through rax (exact for the folded-const
                            // case too: i32 + i32 cannot overflow i64).
                            emit_src64_canon(&mut ops, sa_, 0, &lazy);
                            dynasm!(ops ; add rax, imm);
                            store_dst(&mut ops, dl);
                            if let Loc::R(d) = dl {
                                dynasm!(ops ; mov Rq(d), rax);
                            }
                            wt_pre = emit_int_wt_gpr(&mut ops, plan, &map, dst, false);
                            if !plan.elide_guard.contains(&ip) {
                                emit_i53_guard_gpr(&mut ops, dl, rip_after, ip_slot, inline_guards, flush_exit);
                            }
                        }
                    }
                }
            }
            Instr::Neg { dst, a } => {
                // -0 is not representable in an i64 home — bail on a zero operand
                // (same as the xmm arm; Neg is pure, resuming AT this ip is sound).
                let dl = g(dst);
                let nonzero = ops.new_dynamic_label();
                emit_src64_canon(&mut ops, src(a), 0, &lazy); // rax
                dynasm!(ops
                    ; test rax, rax
                    ; jnz => nonzero
                );
                emit_store_ip(&mut ops, ip_slot, rip_at);
                dynasm!(ops
                    ; jmp => flush_exit
                    ; => nonzero
                    ; neg rax
                );
                match dl {
                    Loc::R(d) => dynasm!(ops ; mov Rq(d), rax),
                    Loc::S(dd) => dynasm!(ops ; mov [rsp + dd], rax),
                }
                wt_pre = emit_int_wt_gpr(&mut ops, plan, &map, dst, false);
                if !plan.elide_guard.contains(&ip) {
                    emit_i53_guard_gpr(&mut ops, dl, rip_after, ip_slot, inline_guards, flush_exit);
                }
            }
            Instr::Lt { dst, a, b }
            | Instr::Le { dst, a, b }
            | Instr::Gt { dst, a, b }
            | Instr::Ge { dst, a, b }
            | Instr::Eq { dst, a, b }
            | Instr::Ne { dst, a, b } => {
                let cmp = match proto.code[ip] {
                    Instr::Lt { .. } => Cmp::Lt,
                    Instr::Le { .. } => Cmp::Le,
                    Instr::Gt { .. } => Cmp::Gt,
                    Instr::Ge { .. } => Cmp::Ge,
                    Instr::Eq { .. } => Cmp::Eq,
                    _ => Cmp::Ne,
                };
                let d = gh(plan, dst);
                emit_icmp_flags_gpr(&mut ops, src(a), src(b), &lazy);
                match cmp {
                    Cmp::Lt => dynasm!(ops ; setl al),
                    Cmp::Le => dynasm!(ops ; setle al),
                    Cmp::Gt => dynasm!(ops ; setg al),
                    Cmp::Ge => dynasm!(ops ; setge al),
                    Cmp::Eq => dynasm!(ops ; sete al),
                    Cmp::Ne => dynasm!(ops ; setne al),
                }
                dynasm!(ops ; movzx Rq(d), al);
                flag_cmp = Some((ip, dst, cmp));
            }
            Instr::Jump { target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; jmp => t);
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                let if_false = matches!(proto.code[ip], Instr::JumpIfFalse { .. });
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                // Same flag-fusion predicate as the xmm emitter (the setcc/movzx
                // that boxed the bool home don't touch flags).
                let fused = match prev_flag {
                    Some((cip, creg, op))
                        if creg == cond
                            && !(cip + 1..=ip).any(|p| plan.jump_targets.contains(&p)) =>
                    {
                        Some(op)
                    }
                    _ => None,
                };
                match fused {
                    Some(op) => match (op, if_false) {
                        (Cmp::Lt, true) => dynasm!(ops ; jge => t),
                        (Cmp::Le, true) => dynasm!(ops ; jg => t),
                        (Cmp::Gt, true) => dynasm!(ops ; jle => t),
                        (Cmp::Ge, true) => dynasm!(ops ; jl => t),
                        (Cmp::Eq, true) => dynasm!(ops ; jne => t),
                        (Cmp::Ne, true) => dynasm!(ops ; je => t),
                        (Cmp::Lt, false) => dynasm!(ops ; jl => t),
                        (Cmp::Le, false) => dynasm!(ops ; jle => t),
                        (Cmp::Gt, false) => dynasm!(ops ; jg => t),
                        (Cmp::Ge, false) => dynasm!(ops ; jge => t),
                        (Cmp::Eq, false) => dynasm!(ops ; je => t),
                        (Cmp::Ne, false) => dynasm!(ops ; jne => t),
                    },
                    None => {
                        let c = gh(plan, cond);
                        if if_false {
                            dynasm!(ops ; test Rq(c), Rq(c) ; jz => t);
                        } else {
                            dynasm!(ops ; test Rq(c), Rq(c) ; jnz => t);
                        }
                    }
                }
            }
            Instr::JumpIfNotLt { a, b, target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                emit_icmp_flags_gpr(&mut ops, src(a), src(b), &lazy);
                dynasm!(ops ; jge => t); // !(a<b), SIGNED
            }
            Instr::JumpIfNotLe { a, b, target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                emit_icmp_flags_gpr(&mut ops, src(a), src(b), &lazy);
                dynasm!(ops ; jg => t); // !(a<=b), SIGNED
            }
            // ── pinned element read ── same guards and deopt contract as the
            // xmm arm; only the home moves differ (and the dense-array tag
            // check scratches rdx, not r10 — rdx's base value is dead by then).
            Instr::GetIndex { dst, key, .. } => {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let dl = g(dst);
                let d = dst_reg(dl); // W10.3: a spilled dst stages in rax
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                // W7: identity hoisted to the entry guard for a hoisted pin;
                // only the semantic bounds/tag guards remain per access.
                if !plan.hoist_pins.contains(&(j as u8)) {
                    match ta_plan.pins[j].src {
                        TaPinSrc::Global(gi) => dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]),
                        TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                    }
                    dynasm!(ops
                        ; cmp rax, [rsp + off]           // receiver vs snapshot obj_bits
                        ; jne => deopt
                    );
                }
                emit_src64_canon(&mut ops, src(key), 1, &lazy); // rcx = index (i64, integral)
                dynasm!(ops
                    ; cmp rcx, [rsp + off + 16]          // unsigned: i < len (catches <0)
                    ; jae => deopt
                    ; mov rdx, [rsp + off + 8]           // pinned base
                );
                if ta_plan.pins[j].kind == ARR_INT_PIN_KIND {
                    dynasm!(ops
                        ; mov rax, [rdx + rcx * 8]       // items[i] (Value bits)
                        ; mov rdx, rax
                        ; shr rdx, 48
                        ; cmp edx, INT_TAG_HI as i32
                        ; jne => deopt                   // double / HOLE / heap → deopt
                        ; movsxd Rq(d), eax              // Int payload, sign-extended
                    );
                } else {
                    dynasm!(ops
                        ; movsxd Rq(d), DWORD [rdx + rcx * 4] // sign-extend i32 element
                    );
                }
                store_dst(&mut ops, dl); // W10.3 spilled dst: park the canonical qword
                dynasm!(ops
                    ; jmp => done
                    ; => deopt
                );
                emit_store_ip(&mut ops, ip_slot, rip_at); // resume AT this ip
                dynasm!(ops
                    ; jmp => flush_exit
                    ; => done
                );
            }
            Instr::SetIndex { key, val, .. } => {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                // W7: identity hoisted to entry for a hoisted pin (see GetIndex).
                if !plan.hoist_pins.contains(&(j as u8)) {
                    match ta_plan.pins[j].src {
                        TaPinSrc::Global(gi) => dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]),
                        TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                    }
                    dynasm!(ops
                        ; cmp rax, [rsp + off]
                        ; jne => deopt
                    );
                }
                emit_src64_canon(&mut ops, src(key), 1, &lazy); // rcx = index
                dynasm!(ops
                    ; cmp rcx, [rsp + off + 16]
                    ; jae => deopt
                    ; mov rdx, [rsp + off + 8]
                );
                emit_src64(&mut ops, src(val), 0);       // rax = value (low 32 used;
                                                         // a lazy home's low 32 is right)
                dynasm!(ops
                    ; mov DWORD [rdx + rcx * 4], eax     // store low 32 (== ToInt32(v))
                    ; jmp => done
                    ; => deopt
                );
                emit_store_ip(&mut ops, ip_slot, rip_at);
                dynasm!(ops
                    ; jmp => flush_exit
                    ; => done
                );
            }
            Instr::Bitwise { dst, a, b, op } => {
                use crate::bytecode::BitwiseOp as B;
                // THE point of this emitter: the homes are already GPRs, so the
                // int32-lane op needs no xmm↔gpr transfers at all. A home's low
                // 32 bits ARE ToInt32 of its value in EVERY representation
                // (canonical i64 or W8 lazy zext), so operands read as Rd()
                // directly and — W8 — the op lands IN the dst home's 32-bit
                // lane, no rax staging. Shifts stage the count in cl (x86 masks
                // it to 5 bits, matching JS's `& 31`) or take a hoisted-const
                // count as an immediate. `>>>`'s 32-bit write zero-extends (a
                // u32, within ±2^53 — exit boxing picks Int vs double) and is
                // canonical as written; the others sign-extend at the def only
                // when the dst home is not lazy.
                let dl = g(dst);
                // W10.3: a spilled dst stages in eax/rax (`store_dst` parks the
                // full qword; no home is ever rax, so the `== d` in-place
                // shortcuts below can never fire spuriously). Spilled OPERANDS
                // read through 32-bit mem forms — a slot's low 32 IS ToInt32
                // of its value (canonical i64), same as any home's low 32.
                let d = dst_reg(dl);
                let (sa, sb) = (src(a), src(b));
                if let (Src::I(ai), Src::I(bi)) = (sa, sb) {
                    // Two hoisted i32 constants: fold. `>>>` folds to a u32
                    // (32-bit write zero-extends); the rest to an i32
                    // (64-bit imm32 mov sign-extends). Both canonical.
                    match op {
                        B::And => dynasm!(ops ; mov Rq(d), ai & bi),
                        B::Or => dynasm!(ops ; mov Rq(d), ai | bi),
                        B::Xor => dynasm!(ops ; mov Rq(d), ai ^ bi),
                        B::Shl => dynasm!(ops ; mov Rq(d), ai.wrapping_shl(bi as u32)),
                        B::Shr => dynasm!(ops ; mov Rq(d), ai >> (bi & 31)),
                        B::Ushr => dynasm!(ops ; mov Rd(d), ((ai as u32) >> (bi & 31)) as i32),
                    }
                } else {
                    match op {
                        B::And | B::Or | B::Xor => {
                            // Commutative: land the other operand on d, op in
                            // place (d == both homes degenerates fine).
                            let (dst_src, other) = match (sa, sb) {
                                (Src::R(ag), b_) if ag == d => (None, b_),
                                (a_, Src::R(bg)) if bg == d => (None, a_),
                                (a_, b_) => (Some(a_), b_),
                            };
                            if let Some(fill) = dst_src {
                                match fill {
                                    Src::R(ag) => dynasm!(ops ; mov Rd(d), Rd(ag)),
                                    Src::I(ai) => dynasm!(ops ; mov Rd(d), ai),
                                    Src::S(sd) => dynasm!(ops ; mov Rd(d), [rsp + sd]),
                                }
                            }
                            match (op, other) {
                                (B::And, Src::R(h)) => dynasm!(ops ; and Rd(d), Rd(h)),
                                (B::And, Src::I(v)) => dynasm!(ops ; and Rd(d), v),
                                (B::And, Src::S(sd)) => dynasm!(ops ; and Rd(d), [rsp + sd]),
                                (B::Or, Src::R(h)) => dynasm!(ops ; or Rd(d), Rd(h)),
                                (B::Or, Src::I(v)) => dynasm!(ops ; or Rd(d), v),
                                (B::Or, Src::S(sd)) => dynasm!(ops ; or Rd(d), [rsp + sd]),
                                (B::Xor, Src::R(h)) => dynasm!(ops ; xor Rd(d), Rd(h)),
                                (B::Xor, Src::I(v)) => dynasm!(ops ; xor Rd(d), v),
                                (B::Xor, Src::S(sd)) => dynasm!(ops ; xor Rd(d), [rsp + sd]),
                                _ => unreachable!(),
                            }
                        }
                        B::Shl | B::Shr | B::Ushr => {
                            // Count to cl FIRST (d may BE the count home),
                            // then the value onto d, then shift in place.
                            match sb {
                                Src::R(bg) => dynasm!(ops ; mov ecx, Rd(bg)),
                                Src::S(sd) => dynasm!(ops ; mov ecx, [rsp + sd]),
                                Src::I(_) => {}
                            }
                            match sa {
                                Src::R(ag) if ag != d => dynasm!(ops ; mov Rd(d), Rd(ag)),
                                Src::R(_) => {}
                                Src::I(ai) => dynasm!(ops ; mov Rd(d), ai),
                                Src::S(sd) => dynasm!(ops ; mov Rd(d), [rsp + sd]),
                            }
                            match (op, sb) {
                                (B::Shl, Src::I(bi)) => dynasm!(ops ; shl Rd(d), (bi & 31) as i8),
                                (B::Shl, _) => dynasm!(ops ; shl Rd(d), cl),
                                (B::Shr, Src::I(bi)) => dynasm!(ops ; sar Rd(d), (bi & 31) as i8),
                                (B::Shr, _) => dynasm!(ops ; sar Rd(d), cl),
                                (B::Ushr, Src::I(bi)) => dynasm!(ops ; shr Rd(d), (bi & 31) as i8),
                                (B::Ushr, _) => dynasm!(ops ; shr Rd(d), cl),
                                _ => unreachable!(),
                            }
                        }
                    }
                    // W8: a lazy dst keeps the zero-extended 32-bit result —
                    // the movsxd this elides was ON the accumulator chain.
                    // `>>>` is canonical as written and never sign-extends.
                    // (A split/write-through dst is never lazy, so its
                    // post-op boxing below always reads a canonical home; a
                    // W10.3 staged dst — rax, never in `lazy` — always
                    // canonicalizes here, then `store_dst` parks the qword.)
                    if !matches!(op, B::Ushr) && !lazy.contains(&d) {
                        dynasm!(ops ; movsxd Rq(d), Rd(d));
                    }
                }
                store_dst(&mut ops, dl);
            }
            // ── pinned-string charCodeAt ── same guards/deopt as the xmm arm.
            Instr::CallMethod { dst, arg_base, .. }
                if ta_plan
                    .access
                    .get(&ip)
                    .map_or(false, |&j| ta_plan.pins[j as usize].kind == STR_PIN_KIND) =>
            {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let dl = g(dst);
                let d = dst_reg(dl); // W10.3: a spilled dst stages in eax
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                // W7: identity hoisted to entry for a hoisted pin (see GetIndex).
                if !plan.hoist_pins.contains(&(j as u8)) {
                    match ta_plan.pins[j].src {
                        TaPinSrc::Global(gi) => dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]),
                        TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                    }
                    dynasm!(ops
                        ; cmp rax, [rsp + off]           // receiver identity vs snapshot
                        ; jne => deopt
                    );
                }
                emit_src64_canon(&mut ops, src(arg_base), 1, &lazy); // rcx = index
                dynasm!(ops
                    ; cmp rcx, [rsp + off + 16]          // unsigned: i < units
                    ; jae => deopt                       // OOB → deopt (interp yields NaN)
                    ; mov rdx, [rsp + off + 8]           // pinned bytes base
                    ; movzx Rd(d), BYTE [rdx + rcx]      // ASCII code unit, 0..255 —
                );                                       // zext == sext: canonical
                store_dst(&mut ops, dl);
                dynasm!(ops
                    ; jmp => done
                    ; => deopt
                );
                emit_store_ip(&mut ops, ip_slot, rip_at); // resume AT this ip
                dynasm!(ops
                    ; jmp => flush_exit
                    ; => done
                );
                // W9: writes_reg is blind to CallMethod, so the generic
                // write-through hook below never fires for this def — emit it
                // explicitly (a membership no-op unless dst is a split or
                // W9-wt-shared register). 0..255 is provably i32.
                emit_int_wt_gpr(&mut ops, plan, &map, dst, true);
            }
            // ── W9: pinned-DataView get* on GPR homes ── the B115 DOUBLE arm's
            // guard set MINUS the integral-pos cvttsd2si round-trip (every home
            // here is an integral i64 by the tier's own invariant — the priced
            // saving) and MINUS the cvtsi2sd landing (the load lands directly
            // in the dst home, 64-bit canonical). Int-lane kinds only — the
            // planner declines 7/8 on this path. All-or-nothing: nothing is
            // written before the deopt, so re-execution is sound and the
            // interpreter raises whatever the miss stands for (RangeError for
            // OOB pos, TypeError for a detached buffer — a detached/shrunk/
            // non-DV receiver snapshots {0,0,0} and misses identity). Scratch:
            // rax/rcx/rdx only (r8-r11 may be Bool or numeric homes).
            Instr::CallMethod { dst, arg_base, argc, name, .. }
                if ta_plan
                    .access
                    .get(&ip)
                    .map_or(false, |&j| ta_plan.pins[j as usize].kind == DV_PIN_KIND) =>
            {
                let kindid = match proto
                    .string_constants
                    .get(name as usize)
                    .and_then(|k| dv_get_kind(k))
                {
                    Some(k) if k <= 6 => k,
                    _ => {
                        decline_emit(format_args!(
                            "int-gpr-emit-unhandled: {:?}",
                            proto.code[ip]
                        ));
                        return GprAttempt::OutOfScope;
                    }
                };
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let size = [1i32, 1, 1, 2, 2, 4, 4, 4, 8][kindid as usize];
                let dl = g(dst);
                // W10.3: a spilled dst stages in rax — every LE/BE load form
                // below is legal with rax as the landing register (the BE
                // 16-bit paths already stage through eax), and `store_dst`
                // parks the FULL qword (the kind-6 zext u32 included — a dword
                // slot store would leave stale high bits).
                let d = dst_reg(dl);
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                // W7: identity hoisted to entry for a hoisted pin (see GetIndex).
                if !plan.hoist_pins.contains(&(j as u8)) {
                    match ta_plan.pins[j].src {
                        TaPinSrc::Global(gi) => dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]),
                        TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                    }
                    dynasm!(ops
                        ; cmp rax, [rsp + off]           // receiver identity vs snapshot
                        ; jne => deopt
                    );
                }
                emit_src64_canon(&mut ops, src(arg_base), 1, &lazy); // rcx = pos, canonical i64
                dynasm!(ops
                    ; test rcx, rcx
                    ; js => deopt                        // negative → RangeError (interp)
                    ; mov rdx, [rsp + off + 16]          // byteLength
                    ; sub rdx, size
                    ; cmp rcx, rdx                       // signed: pos > byteLength - size
                    ; jg => deopt                        //  (incl. byteLength < size)
                    ; mov rdx, [rsp + off + 8]           // pinned base (data + byteOffset)
                );
                let le_big = ops.new_dynamic_label();
                let loaded = ops.new_dynamic_label();
                if size > 1 {
                    if let Some(&(fa, fb)) = plan.dv_flag_fuse.get(&ip) {
                        // Fused adjacent Eq on INTEGER homes: a canonical
                        // signed compare — no NaN case, so no `jp` (the DOUBLE
                        // arm's ucomisd needs one; integer homes cannot hold
                        // NaN). Unequal → false → big-endian. Scratches only
                        // rax (immediate forms) — rcx/rdx stay live.
                        emit_icmp_flags_gpr(&mut ops, src(fa), src(fb), &lazy);
                        dynasm!(ops ; jne => le_big);
                    } else if argc == 2 {
                        // The flag is a Bool gpr home holding 0/1 — `test` IS
                        // ToBoolean here, exactly as on the DOUBLE tier (B22).
                        let lg = gh(plan, arg_base + 1);
                        dynasm!(ops ; test Rq(lg), Rq(lg) ; jz => le_big);
                    } else {
                        // Absent flag = undefined = big-endian.
                        dynasm!(ops ; jmp => le_big);
                    }
                }
                // ── little-endian load ── directly into the dst home, 64-bit
                // canonical: movsx/movsxd sign-extend the signed kinds; a
                // 32-bit-register write zero-extends the unsigned ones (zext ==
                // canonical for u8/u16/u32 — all within ±2^53, exit boxing
                // picks Int vs double).
                match kindid {
                    0 => dynasm!(ops ; movsx Rq(d), BYTE [rdx + rcx]),
                    1 => dynasm!(ops ; movzx Rd(d), BYTE [rdx + rcx]),
                    3 => dynasm!(ops ; movsx Rq(d), WORD [rdx + rcx]),
                    4 => dynasm!(ops ; movzx Rd(d), WORD [rdx + rcx]),
                    5 => dynasm!(ops ; movsxd Rq(d), DWORD [rdx + rcx]),
                    _ => dynasm!(ops ; mov Rd(d), DWORD [rdx + rcx]), // 6: u32
                }
                if size > 1 {
                    dynasm!(ops ; jmp => loaded ; => le_big);
                    // ── big-endian load (byte-swapped) ── staged through eax
                    // for the 16-bit rol, direct for the 32-bit bswap. The
                    // final write is always a 64-bit-canonical form.
                    match kindid {
                        3 => dynasm!(ops
                            ; movzx eax, WORD [rdx + rcx]
                            ; rol ax, 8
                            ; movsx Rq(d), ax
                        ),
                        4 => dynasm!(ops
                            ; movzx eax, WORD [rdx + rcx]
                            ; rol ax, 8
                            ; movzx Rd(d), ax
                        ),
                        5 => dynasm!(ops
                            ; mov eax, [rdx + rcx]
                            ; bswap eax
                            ; movsxd Rq(d), eax
                        ),
                        _ => dynasm!(ops
                            ; mov Rd(d), [rdx + rcx]
                            ; bswap Rd(d)                // 32-bit op: zero-extends
                        ),
                    }
                    dynasm!(ops ; => loaded);
                }
                store_dst(&mut ops, dl); // W10.3 spilled dst: park the qword
                // A FUSED access resumes at the ELIDED Eq (ip-1): the
                // interpreter recomputes the flag into the frame slot — which
                // native code never writes — then re-runs the call. flush_exit
                // canonicalizes lazy operand homes first, so the re-executed
                // Eq reads the values it would have read.
                let resume_ip =
                    if plan.dv_flag_fuse.contains_key(&ip) { rip(ip - 1) } else { rip_at };
                dynasm!(ops
                    ; jmp => done
                    ; => deopt
                );
                emit_store_ip(&mut ops, ip_slot, resume_ip);
                dynasm!(ops
                    ; jmp => flush_exit
                    ; => done
                );
                // A DV dst can be a split receiver's numeric half, and
                // writes_reg is blind to CallMethod so the generic post-op
                // write-through hook never fires — emit it explicitly (a
                // membership no-op otherwise). Kinds 0-5 are provably i32;
                // kind 6 (u32) takes the generic box.
                emit_int_wt_gpr(&mut ops, plan, &map, dst, kindid <= 5);
            }
            // ── pinned length ── str.length / arr.length from the snapshot.
            Instr::GetProp { dst, .. }
                if ta_plan.access.get(&ip).map_or(false, |&j| {
                    matches!(ta_plan.pins[j as usize].kind, STR_PIN_KIND | ARR_INT_PIN_KIND)
                }) =>
            {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let dl = g(dst);
                let d = dst_reg(dl); // W10.3: a spilled dst stages in rax
                // W7: a hoisted pin's length read collapses to a bare snapshot
                // load (identity entry-guarded; length region-invariant). The
                // fully hoisted STRING case never reaches here (body op
                // skipped); this is the multi-def / on-a-branch / Array residue.
                if plan.hoist_pins.contains(&(j as u8)) {
                    dynasm!(ops ; mov Rq(d), [rsp + off + 16]);
                    store_dst(&mut ops, dl);
                } else {
                    let deopt = ops.new_dynamic_label();
                    let done = ops.new_dynamic_label();
                    match ta_plan.pins[j].src {
                        TaPinSrc::Global(gi) => dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]),
                        TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                    }
                    dynasm!(ops
                        ; cmp rax, [rsp + off]           // receiver identity vs snapshot
                        ; jne => deopt
                        ; mov Rq(d), [rsp + off + 16]    // units / len
                    );
                    store_dst(&mut ops, dl);
                    dynasm!(ops
                        ; jmp => done
                        ; => deopt
                    );
                    emit_store_ip(&mut ops, ip_slot, rip_at);
                    dynasm!(ops
                        ; jmp => flush_exit
                        ; => done
                    );
                }
            }
            // ── Math.imul ── low 32 of the product, sign-extended at the def
            // only when the dst home is not W8-lazy (see the Bitwise arm; imul
            // reads operands' low 32s, so any representation serves). The
            // 3-operand imm form and the in-place 2-operand form keep the
            // whole op to ONE instruction on the chain.
            Instr::MathOp { dst, arg_base, op: MathFn::Imul, argc: 2, .. } => {
                let dl = g(dst);
                // W10.3: a spilled dst stages in eax (imul has no mem-dst
                // form); spilled operands use the r32, r/m32 (,imm) forms —
                // a slot's low 32 is ToInt32 of its value.
                let d = dst_reg(dl);
                match (src(arg_base), src(arg_base + 1)) {
                    (Src::I(ai), Src::I(bi)) => {
                        // Two hoisted i32 constants: fold (canonical store,
                        // so no def-site movsxd applies below).
                        dynasm!(ops ; mov Rq(d), ai.wrapping_mul(bi));
                    }
                    (Src::R(ag), Src::I(bi)) => dynasm!(ops ; imul Rd(d), Rd(ag), bi),
                    (Src::I(ai), Src::R(bg)) => dynasm!(ops ; imul Rd(d), Rd(bg), ai),
                    (Src::S(ad), Src::I(bi)) => dynasm!(ops ; imul Rd(d), [rsp + ad], bi),
                    (Src::I(ai), Src::S(bd)) => dynasm!(ops ; imul Rd(d), [rsp + bd], ai),
                    (Src::R(ag), Src::R(bg)) => {
                        if d == ag {
                            dynasm!(ops ; imul Rd(d), Rd(bg));
                        } else if d == bg {
                            dynasm!(ops ; imul Rd(d), Rd(ag));
                        } else {
                            dynasm!(ops ; mov Rd(d), Rd(ag) ; imul Rd(d), Rd(bg));
                        }
                    }
                    (Src::R(ag), Src::S(bd)) => {
                        if d != ag {
                            dynasm!(ops ; mov Rd(d), Rd(ag));
                        }
                        dynasm!(ops ; imul Rd(d), [rsp + bd]);
                    }
                    (Src::S(ad), Src::R(bg)) => {
                        if d != bg {
                            dynasm!(ops ; mov Rd(d), Rd(bg)); // commutative
                        }
                        dynasm!(ops ; imul Rd(d), [rsp + ad]);
                    }
                    (Src::S(ad), Src::S(bd)) => {
                        dynasm!(ops ; mov Rd(d), [rsp + ad] ; imul Rd(d), [rsp + bd]);
                    }
                }
                let folded = matches!((src(arg_base), src(arg_base + 1)), (Src::I(_), Src::I(_)));
                if !folded && !lazy.contains(&d) {
                    dynasm!(ops ; movsxd Rq(d), Rd(d));
                }
                store_dst(&mut ops, dl); // W10.3 spilled dst: park the qword
                // The generic writes_reg hook covers MathOp dsts, but this arm
                // writes through explicitly like the DV arm (membership no-op
                // otherwise). An imul result is ToInt32 — provably i32.
                emit_int_wt_gpr(&mut ops, plan, &map, dst, true);
            }
            Instr::Return { .. } | Instr::ReturnUndefined => {
                emit_store_ip(&mut ops, ip_slot, rip_at);
                dynasm!(ops ; jmp => flush_exit);
            }
            // POST-PLAN hole — same contract as the xmm emitter's twin arm.
            _ => {
                decline_emit(format_args!("int-gpr-emit-unhandled: {:?}", proto.code[ip]));
                return GprAttempt::OutOfScope;
            }
        }
        // ── B94 write-through (GPR) ── a numeric def of a split receiver must
        // reach MEMORY (boxed) as well as its home, because `flush_exit`
        // deliberately skips the register and memory is what the interpreter
        // reads on any exit. Arms with an i53 guard already stored (`wt_pre`;
        // the guard resumes at ip+1 expecting the result flushed), and the
        // receiver LoadGlobal half stored the object itself — `wt_def_at` holds
        // that second exclusion for all three tiers.
        if !wt_pre {
            if let Some(d) = wt_def_at(proto, plan, ip) {
                // A non-`>>>` Bitwise result and Math.imul are PROVABLY i32
                // (`>>>` yields a u32 that can exceed i32) — their write-through
                // is a branchless int-tag instead of the two-compare generic box.
                let known_i32 = matches!(
                    proto.code[ip],
                    Instr::Bitwise { op, .. } if !matches!(op, crate::bytecode::BitwiseOp::Ushr)
                ) || matches!(
                    proto.code[ip],
                    Instr::MathOp { op: MathFn::Imul, argc: 2, .. }
                );
                if emit_int_wt_gpr(&mut ops, plan, &map, d, known_i32) {
                    flag_cmp = None; // the boxing clobbered FLAGS
                }
            }
        }
    }

    // ── exit stubs ──
    for (target, label) in &exit_stubs {
        dynasm!(ops ; => *label);
        emit_store_ip(&mut ops, ip_slot, *target as i32);
        dynasm!(ops ; jmp => flush_exit);
    }

    // ── flush_exit ── box each GPR home back to an Int/double Value and write
    // it to the reg file / globals; the frame's ip slot points at the resume
    // ip, already stored. An unmapped (hoisted-const) reg flushes its
    // compile-time-boxed Int Value — i32 by construction, so the Int tag always
    // applies.
    //
    // W8 INVARIANT — canonicalization at exits: EVERY flushing path funnels
    // here (Return, out-of-region jump stubs, meter-exhaustion stubs, i53
    // guard fails, Mul-overflow / Mod-zero / Neg-zero bails, pinned-access
    // deopts), so the deferred sign-extensions are repaired FIRST: a lazy
    // home's value fits i32 by the `lazy_sx_sets` invariant, so one movsxd
    // recovers the canonical i64 no matter which representation the exiting
    // path left, and every exit boxes the same canonical i32-in-i64 Int Value
    // wave 7 boxed. Split/write-through homes are never lazy (their memory
    // slot is already current and the loop below skips them); `entry_bail`
    // flushes nothing and needs no fix-up.
    dynasm!(ops ; => flush_exit);
    let mut lazy_fix: Vec<u8> = lazy.iter().copied().collect();
    lazy_fix.sort_unstable();
    for h in lazy_fix {
        dynasm!(ops ; movsxd Rq(h), Rd(h));
    }
    for &(r, x) in &plan.num_regs {
        // A B94 split receiver is written through (boxed) at each numeric def,
        // so memory is already current; flushing its home here would overwrite
        // the receiver object at any exit taken inside the receiver range.
        if plan.split_recvs.contains(&r) || plan.write_through.contains(&r) {
            continue;
        }
        match map.get(&x) {
            Some(&l) => emit_int_box_from_loc(&mut ops, l), // W10.3: spilled homes box from their slot
            None => {
                let bits = INT_TAG | hoist_c[&r] as u32 as u64;
                dynasm!(ops ; mov rax, QWORD bits as i64);
            }
        }
        dynasm!(ops ; mov [rbx + dreg(r)], rax);
    }
    for &(r, gb) in &plan.bool_regs {
        dynasm!(ops ; mov rax, QWORD BOOL_TAG as i64 ; or rax, Rq(gb) ; mov [rbx + dreg(r)], rax);
    }
    for &(gi, x) in &plan.globs {
        // A narrowed global's slot is written through at every StoreGlobal and
        // its home may be another value's by now — flushing it here would put
        // a stale value where the interpreter reads (the B9-class failure).
        if plan.narrow_globs.contains(&gi) {
            continue;
        }
        emit_int_box_from_loc(&mut ops, gx(x));
        dynasm!(ops ; mov [r12 + (gi as i32) * 8], rax);
    }
    emit_gpr_region_restore(&mut ops, frame);

    // ── entry_bail ── a live-in wasn't admissible; nothing computed, so
    // restore (NO flush) and resume at the header (interpreted).
    dynasm!(ops ; => entry_bail);
    emit_store_ip(&mut ops, ip_slot, rip(s));
    emit_gpr_region_restore(&mut ops, frame);

    let buf = match ops.finalize() {
        Ok(b) => b,
        Err(_) => {
            decline_emit("int-gpr-emit: assembler finalize failed");
            return GprAttempt::OutOfScope;
        }
    };
    // W7 attribution: code-byte length with pins, so the hoist's size delta is
    // one grep away (run again under ZIPP_NO_GUARD_HOIST=1 and diff the line).
    if !ta_plan.pins.is_empty() && std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!(
            "[jit] INT-GPR region [{start},{end}] guard-hoist pins={}/{} len-fills={} code={}b",
            plan.hoist_pins.len(),
            ta_plan.pins.len(),
            plan.hoist_len_ips.len(),
            buf.len()
        );
        log_pinned_recvs("INT-GPR", start, end, proto, plan);
    }
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    GprAttempt::Emitted(JitFn { _buf: buf, entry: entry_ptr, self_binding: None })
}

/// Compare flags for the GPR/immediate operand forms (SIGNED, i64 — an i32
/// immediate sign-extends, exactly the i64 the home would hold). W8: lazy
/// operand homes are canonicalized in place first (movsxd never touches
/// flags, and the fix precedes the cmp anyway).
fn emit_icmp_flags_gpr(ops: &mut dynasmrt::x64::Assembler, a: Src, b: Src, lazy: &FxHashSet<u8>) {
    if let Src::R(h) = a {
        emit_canon_home(ops, h, lazy);
    }
    if let Src::R(h) = b {
        emit_canon_home(ops, h, lazy);
    }
    // W10.3 spill-slot operands use qword mem-operand forms (slots are always
    // canonical), staging through rax ONLY — the DV fused-Eq call site needs
    // rcx/rdx to stay live across this compare.
    match (a, b) {
        (Src::R(ag), Src::R(bg)) => dynasm!(ops ; cmp Rq(ag), Rq(bg)),
        (Src::R(ag), Src::I(bi)) => dynasm!(ops ; cmp Rq(ag), bi),
        (Src::R(ag), Src::S(bd)) => dynasm!(ops ; cmp Rq(ag), [rsp + bd]),
        (Src::I(ai), Src::R(bg)) => dynasm!(ops ; mov rax, ai ; cmp rax, Rq(bg)),
        (Src::I(ai), Src::I(bi)) => dynasm!(ops ; mov rax, ai ; cmp rax, bi),
        (Src::I(ai), Src::S(bd)) => dynasm!(ops ; mov rax, ai ; cmp rax, [rsp + bd]),
        (Src::S(ad), Src::R(bg)) => dynasm!(ops ; mov rax, [rsp + ad] ; cmp rax, Rq(bg)),
        (Src::S(ad), Src::I(bi)) => dynasm!(ops ; mov rax, [rsp + ad] ; cmp rax, bi),
        (Src::S(ad), Src::S(bd)) => dynasm!(ops ; mov rax, [rsp + ad] ; cmp rax, [rsp + bd]),
    }
}

/// Epilogue for the GPR-home frame: undo `frame`, pop the eight saved gprs
/// (rbp/r15 included — they are home-pool members here), `ret`.
fn emit_gpr_region_restore(ops: &mut dynasmrt::x64::Assembler, frame: i32) {
    dynasm!(ops
        ; add rsp, frame
        ; pop rbp
        ; pop r15
        ; pop r14
        ; pop r13
        ; pop r12
        ; pop rdi
        ; pop rsi
        ; pop rbx
        ; ret
    );
}
