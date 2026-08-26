// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

#[inline]
fn xmm_home_id_is_physical(home: u8) -> bool {
    (HOME_XMM_FIRST..=HOME_XMM_LAST).contains(&home)
}

/// Virtual numeric-home colours are valid only while being consumed by the
/// GPR mapper.  Keep an explicit fail-closed boundary in front of the hardware
/// XMM emitter so a future refactor cannot accidentally interpret colour 16 as
/// an architectural register number.
fn xmm_plan_is_physical(plan: &RegionPlan) -> bool {
    plan.reg_home.values().all(|h| match *h {
        Home::Xmm(x) => xmm_home_id_is_physical(x),
        Home::Gpr(_) => true,
    }) && plan
        .glob_home
        .values()
        .chain(plan.addint_imm_home.values())
        .all(|&x| xmm_home_id_is_physical(x))
}

/// `2^53` — the largest magnitude where consecutive integers are all exactly
/// representable in f64. Above it, JS `+`/`-` round, so an exact i64 result would
/// diverge: the int path bails to the interpreter when a result leaves
/// `[-2^53, 2^53]`. (Too large for a `cmp r64, imm32`, so it goes via a register.)
pub(crate) const TWO_POW_53: i64 = 9_007_199_254_740_992;
/// `2^54` — the unsigned upper bound for the shifted range check `(x + 2^53) ≤ 2^54`.
pub(crate) const TWO_POW_54: i64 = 18_014_398_509_481_984;

/// Exact load shape for a non-BigInt integer TypedArray whose complete value
/// range fits the INTEGER tier's signed-i64/i53 home contract.
///
/// Keep the dtype decision in this one place: admission, the xmm-home emitter
/// and the GPR-home emitter all consume this enum. The metadata check against
/// `TA_KINDS` makes the numeric kind ids fail closed if that authoritative
/// table is ever reordered or extended. Uint32 is intentionally absent: its
/// values do fit i53, but the GPR tier's deferred-sign-extension analysis has
/// historically treated every pinned GetIndex as i32-range. Widening that
/// proof is separate work; silently admitting kind 6 here would sign-mangle
/// values above `i32::MAX` on some downstream paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntTaLoadKind {
    I8,
    U8,
    I16,
    U16,
    I32,
}

/// Kill switch for the narrow fixed-width integer TypedArray read widening.
/// Int32 (the pre-existing path) remains enabled when the switch is set, so an
/// off run is an exact semantic fallback rather than a JIT-wide disable.
fn int_ta_narrow_reads_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_INT_TA_NARROW_READS").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

pub(crate) fn int_ta_load_kind(kind: u8) -> Option<IntTaLoadKind> {
    use crate::vm::native::TA_KINDS;

    let &(name, size, is_bigint, is_float) = TA_KINDS.get(kind as usize)?;
    if is_bigint || is_float {
        return None;
    }
    let load = match (kind, name, size) {
        (0, "Int8Array", 1) => IntTaLoadKind::I8,
        (1, "Uint8Array", 1) | (2, "Uint8ClampedArray", 1) => IntTaLoadKind::U8,
        (3, "Int16Array", 2) => IntTaLoadKind::I16,
        (4, "Uint16Array", 2) => IntTaLoadKind::U16,
        (5, "Int32Array", 4) => IntTaLoadKind::I32,
        // Uint32 and every future non-float numeric kind decline until their
        // full range is proven through lazy-sx, compares, exits and boxing.
        _ => return None,
    };
    if load != IntTaLoadKind::I32 && !int_ta_narrow_reads_enabled() {
        return None;
    }
    Some(load)
}

/// Dense Array `.length` is independent of element representation. The old
/// lane admitted only `ARR_INT_PIN_KIND` because it shared a predicate with
/// element reads; object-, hole- and double-bearing dense arrays use the same
/// snapshot `{identity, items base, items.len()}` and are equally safe for a
/// length-only access. The snapshot helper already rejects overlays and mapped
/// arguments, and every emitted read still guards the live receiver identity.
pub(crate) fn int_arr_length_kind(kind: u8) -> bool {
    if kind == ARR_INT_PIN_KIND {
        return true; // pre-existing path remains live under the off switch
    }
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    let on = match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_INT_DENSE_ARRAY_LENGTH").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    };
    on && is_arr_pin(kind)
}

/// Pin kinds whose third snapshot word is an exact INTEGER-tier `.length`.
/// The TypedArray marker is length-only and therefore does not change any raw
/// element read/store admission (in particular, Uint32 remains excluded from
/// the integer element lane).
pub(crate) fn int_length_pin_kind(kind: u8) -> bool {
    int_arr_length_kind(kind) || is_ta_len_pin(kind)
}

/// W20 M1: zero every bool gpr home that gets NO entry load.
///
/// `plan_region` drops a bool from `live_in_bools` exactly when its gpr is
/// SHARED with another bool (two entry loads into one register would overwrite
/// each other, and `bool_shareable` already proved no path from entry reads the
/// register before a def). But `flush_exit` boxes EVERY bool home at EVERY exit
/// as `BOOL_TAG | gpr`, so an exit taken before the first def would `or` the
/// tag with whatever the caller left in r8..r11 — not merely a wrong bool, but
/// a Value whose TAG bits are arbitrary, which the GC would then walk as a heap
/// pointer. One `xor` per such register in the prologue makes every bool home
/// 0/1 from entry on (every bool def is a `movzx Rq(d), al`), which is the
/// invariant `flush_exit` has always relied on.
///
/// Byte-identical to the pre-wave prologue whenever no bool shares a home:
/// every bool is then in `live_in_bools` and nothing is emitted.
pub(crate) fn emit_bool_home_zero(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan) {
    let loaded: FxHashSet<u8> = plan.live_in_bools.iter().map(|&(_, g)| g).collect();
    // W28: a type-split register's bool gpr is neither entry-loaded nor
    // flushed, and its range's opening def dominates every read of it — so the
    // zero buys no correctness on the value. It buys TAG hygiene: the split's
    // own bool write-through does `or rax, Rq(g)` against `BOOL_TAG`, and a
    // register inherited from the caller could make that a Value whose tag bits
    // are arbitrary, which the GC would walk as a heap pointer.
    let mut gs: Vec<u8> = plan
        .bool_regs
        .iter()
        .map(|&(_, g)| g)
        .chain(plan.ty_splits.values().map(|sp| sp.gpr))
        .collect();
    gs.sort_unstable();
    gs.dedup();
    for g in gs {
        if !loaded.contains(&g) {
            dynasm!(ops ; xor Rq(g), Rq(g));
        }
    }
}

/// W20 M2 -- the pin slot of an INT-tier-admissible `arr.push(int)` at `ip`.
///
/// Admissible means: the mechanism's latch is on, the op is a one-argument
/// `push` `CallMethod`, and the OSR pin planner pinned its receiver as a dense
/// Array observed all-Int (`ARR_INT_PIN_KIND`). The pin does three jobs at
/// once -- it keeps the receiver register out of the numeric home set
/// (`ta_recv_regs`), it supplies the identity guard the emitted arm checks
/// before it touches anything, and its snapshot slot is where the helper writes
/// the array's new `{base, len}` back, which is what keeps a sibling `arr[i]`
/// or `arr.length` in the same region reading the truth after an append.
///
/// A NON-all-Int dense Array (`ARR_PIN_KIND` / `ARR_NUM_PIN_KIND`) is
/// deliberately excluded: B102's 11x regression is the standing warning against
/// loosening the pin kind, and the arm boxes from an i64 home, so the all-Int
/// observation is also what makes the array's own contents match what it will
/// keep pushing.
pub(crate) fn arr_push_pin(proto: &FuncProto, ip: usize, ta_plan: &TaPinPlan) -> Option<usize> {
    if !int_push_enabled() {
        return None;
    }
    let j = *ta_plan.access.get(&ip)? as usize;
    if ta_plan.pins.get(j)?.kind != ARR_INT_PIN_KIND {
        return None;
    }
    match proto.code.get(ip)? {
        Instr::CallMethod { name, argc: 1, .. }
            if proto
                .string_constants
                .get(*name as usize)
                .is_some_and(|k| k == "push") =>
        {
            Some(j)
        }
        _ => None,
    }
}

/// Does `[start, end]` contain an admitted `arr.push(int)`? The INT-GPR
/// sub-mode uses this to stay out of scope: its home pool mixes volatile and
/// non-volatile gprs (r15/rbp/rsi/rdi/r13/r14 plus whichever `BOOL_GPRS` the
/// bools left free), so the call-save set the xmm emitter states in three lines
/// would have to be re-derived there per plan. The xmm emitter hosts these
/// regions instead; `gpr_home_map` also requires a Bitwise/imul to engage at
/// all, which a tokenizer scan does not have.
pub(crate) fn region_has_arr_push(
    proto: &FuncProto,
    start: usize,
    end: usize,
    ta_plan: &TaPinPlan,
) -> bool {
    (start..=end).any(|ip| arr_push_pin(proto, ip, ta_plan).is_some())
}

/// One leg of an exact `a.push(x); b.push(y); c.push(z)` batch. The bytecode
/// still executes each receiver/argument setup in order; the first two call
/// sites only stage their numeric argument and the third performs all three
/// guarded appends in one leaf helper call.
#[derive(Clone, Copy)]
struct ArrPush3Step {
    first_ip: usize,
    stage: u8,
    pins: [usize; 3],
    args: [u16; 3],
}

/// Independent latch for the three-push batching peephole. `ZIPP_NO_INT_PUSH`
/// continues to disable the underlying admission as well.
fn int_push3_enabled() -> bool {
    static ON: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);
    match ON.load(std::sync::atomic::Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = int_push_enabled() && std::env::var_os("ZIPP_NO_INT_PUSH3").is_none();
            ON.store(v as u8, std::sync::atomic::Ordering::Relaxed);
            v
        }
    }
}

/// Find fail-closed three-push batches in an ordinary, unmetered INT region.
///
/// The exact nine-op shape is three repetitions of
/// `LoadGlobal receiver; LoadInt|Move argument; CallMethod push`. Every call
/// result must be bytecode-dead across the whole function, because the batched
/// helper intentionally does not materialise the three otherwise discarded
/// length results. No interior op may be a jump target, all receiver pins must
/// be distinct stable globals, and every argument must already have an xmm
/// home. These restrictions make moving the three pure argument reads ahead
/// of the appends unobservable: the push gate proved the appends cannot invoke
/// user code, and a helper decline mutates nothing and replays the first setup.
fn arr_push3_steps(
    proto: &FuncProto,
    s: usize,
    e: usize,
    ta_plan: &TaPinPlan,
    plan: &RegionPlan,
    cold: &FxHashSet<usize>,
    enabled: bool,
) -> FxHashMap<usize, ArrPush3Step> {
    let mut out = FxHashMap::default();
    if !enabled || e.saturating_sub(s) < 8 {
        return out;
    }

    let result_is_unread = |r: u16| {
        !proto
            .code
            .iter()
            .any(|ins| instr_uses(ins).into_iter().any(|u| u == r))
    };
    // `TaPinSrc::Global` currently carries this same proof from jit_plans, but
    // batching reorders later receiver loads ahead of earlier appends. Keep the
    // source-stability licence local so a future pin-planner relaxation cannot
    // silently turn the snapshots into stale push targets.
    let global_is_stable = |g: u32| {
        !proto.code[s..=e].iter().any(|ins| {
            matches!(
                *ins,
                Instr::StoreGlobal { idx, .. }
                    | Instr::StoreGlobalStrict { idx, .. }
                    | Instr::StoreGlobalResolved { idx, .. }
                    if idx == g
            )
        })
    };
    let receiver_is_private = |r: u16, def_ip: usize, use_ip: usize| {
        proto.code.iter().enumerate().all(|(ip, ins)| {
            (writes_reg(ins) != Some(r) || ip == def_ip)
                && (!instr_uses(ins).into_iter().any(|u| u == r) || ip == use_ip)
        })
    };
    let mut base = s;
    while base + 8 <= e {
        let mut pins = [0usize; 3];
        let mut calls = [0usize; 3];
        let mut args = [0u16; 3];
        let mut ok = true;
        for leg in 0..3usize {
            let b = base + 3 * leg;
            let (recv, global) = match proto.code[b] {
                Instr::LoadGlobal { dst, idx } => (dst, idx),
                _ => {
                    ok = false;
                    break;
                }
            };
            let arg = match proto.code[b + 1] {
                Instr::LoadInt { dst, .. } | Instr::Move { dst, .. } => dst,
                _ => {
                    ok = false;
                    break;
                }
            };
            let (dst, obj, arg_base) = match proto.code[b + 2] {
                Instr::CallMethod {
                    dst,
                    obj,
                    arg_base,
                    argc: 1,
                    ..
                } => (dst, obj, arg_base),
                _ => {
                    ok = false;
                    break;
                }
            };
            let Some(j) = arr_push_pin(proto, b + 2, ta_plan) else {
                ok = false;
                break;
            };
            if obj != recv
                || arg_base != arg
                || ta_plan.pins[j].src != TaPinSrc::Global(global)
                || !global_is_stable(global)
                // Later receiver LoadGlobals write frame slots speculatively.
                // Prove those slots are compiler-private call temporaries, so
                // rollback need only restore the two numeric physical homes.
                || !receiver_is_private(recv, b, b + 2)
                || !result_is_unread(dst)
                || plan.split_recvs.contains(&dst)
                || plan.write_through.contains(&dst)
                // A future setup is speculative until the helper commits.
                // Split/write-through argument registers make their boxed
                // frame slot authoritative immediately, which the XMM-only
                // rollback cannot undo before replaying the first setup.
                || plan.split_recvs.contains(&arg)
                || plan.write_through.contains(&arg)
                || plan.slot_consts.contains_key(&arg)
                || !matches!(plan.reg_home.get(&arg), Some(Home::Xmm(_)))
            {
                ok = false;
                break;
            }
            pins[leg] = j;
            calls[leg] = b + 2;
            args[leg] = arg;
        }
        if ok
            && pins[0] != pins[1]
            && pins[0] != pins[2]
            && pins[1] != pins[2]
            && !(base..=base + 8).any(|ip| cold.contains(&ip))
            // The first receiver load may itself be a branch target; it is the
            // semantic start of the batch. No control flow may enter later.
            && !(base + 1..=base + 8).any(|ip| plan.jump_targets.contains(&ip))
        {
            for stage in 0..3usize {
                out.insert(
                    calls[stage],
                    ArrPush3Step {
                        first_ip: base,
                        stage: stage as u8,
                        pins,
                        args,
                    },
                );
            }
            base += 9;
        } else {
            base += 1;
        }
    }
    out
}

/// Can the loop region `[start, end]` run on the INTEGER path? Stricter than
/// `region_is_int`: every op must be integer-valued (no Div — fractional; `Mod`
/// IS allowed, via integer `idiv`), and every `LoadConst` must be an Int-tagged
/// constant (a double constant would be misread as i64).
pub(crate) fn region_is_int(proto: &FuncProto, start: u32, end: u32, ta_plan: &TaPinPlan) -> bool {
    int_unadmitted_ips(proto, start, end, ta_plan, false).is_some_and(|v| v.is_empty())
}

/// The ips in `[start, end]` the INT emitter has no arm for, or `None` when the
/// region cannot be compiled at all.
///
/// An empty vec is the strict case — exactly the old `region_is_int == true`.
/// A non-empty vec is compilable only in COLD-EXIT mode (B9): each such ip
/// becomes a side exit (`mov [rsi], ip ; jmp flush_exit`) instead of
/// disqualifying the whole region from the integer tier. That matters because a
/// single `substring`/`+=` in a branch that never runs was demoting an entire
/// `charCodeAt` scan loop from 1.7 ns/iteration (parity with V8) to 8.0 ns.
/// B192: registers written by an in-region `LoadUndefined` and NEVER READ
/// in-region — the module wrapper's statement-completion regs. The admission
/// scan, the planner, and both INT emitters all consume this ONE definition:
/// the planner leaves the regs untyped/unhomed, and the emitters write every
/// def of them through to the frame slot (canonical UNDEFINED bits for the
/// LoadUndefined; a boxed home store for a Move), keeping interpreter-resume
/// state exact at every bail ip.
pub(crate) fn undef_dead_regs(
    proto: &FuncProto,
    s: usize,
    e: usize,
) -> rustc_hash::FxHashSet<u16> {
    let mut dsts: rustc_hash::FxHashSet<u16> = rustc_hash::FxHashSet::default();
    for ins in &proto.code[s..=e] {
        if let Instr::LoadUndefined { dst } = *ins {
            dsts.insert(dst);
        }
    }
    if !dsts.is_empty() {
        for ins in &proto.code[s..=e] {
            for u in instr_uses(ins) {
                dsts.remove(&u);
            }
        }
    }
    dsts
}

pub(crate) fn int_unadmitted_ips(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
    // W9: also admit pinned-DataView get* CallMethods (int-lane kinds <= 6).
    // `true` only on `compile_region_int_maybe_cold`'s DV retry, whose plan is
    // routed exclusively into the GPR emitter — the xmm INT emitter has no DV
    // arm and must never see a plan this widening produced (B119 contract).
    admit_dv: bool,
) -> Option<Vec<usize>> {
    if !region_can_compile(proto, start, end, None) {
        return None;
    }
    let mut unadmitted: Vec<usize> = Vec::new();
    let (s, e) = (start as usize, end as usize);
    // B192: `LoadUndefined` into a reg that is NEVER READ in-region (module
    // top-level statement-COMPLETION bookkeeping — `LoadUndefined dst` at a
    // loop head, `Move dst, value` per statement, no in-region reader). Such
    // a reg is deliberately NOT typed or homed (see `undef_dead_regs` in the
    // planner, which uses the same definition); every def of it emits as a
    // write-through store to the frame slot, so an interpreter resume at any
    // bail ip reads exactly the values it would have computed itself. A
    // LoadUndefined whose dst IS read in-region stays unadmitted — the region
    // declines to the MEM tier exactly as before.
    let undef_dead = if crate::codegen::undef_admit_enabled() {
        undef_dead_regs(proto, s, e)
    } else {
        rustc_hash::FxHashSet::default()
    };
    // Reads from the exact fixed-width integer kinds whose full range is i32
    // fit the lane; writes remain Int32-only because width/clamp conversion is
    // dtype-specific and is deliberately not part of this read-only widening.
    let pinned_int_read = |ip: usize| -> bool {
        ta_plan
            .access
            .get(&ip)
            .is_some_and(|&j| int_ta_load_kind(ta_plan.pins[j as usize].kind).is_some())
    };
    let pinned_i32_write = |ip: usize| -> bool {
        ta_plan
            .access
            .get(&ip)
            .is_some_and(|&j| ta_plan.pins[j as usize].kind == 5)
    };
    // A pinned flat-ASCII STRING (kind 254) access: `str.charCodeAt(i)` (a direct
    // byte load into an i64 home, OOB→deopt) and `str.length` (read from the pin's
    // `units`). Both gate on the per-access identity guard. Lets the fnv1a-style
    // `for (i<str.length) h=imul(h^str.charCodeAt(i),C)` loop run unboxed.
    let pinned_str = |ip: usize| -> bool {
        ta_plan
            .access
            .get(&ip)
            .map_or(false, |&j| ta_plan.pins[j as usize].kind == STR_PIN_KIND)
    };
    // W9: a pinned-DataView get* of an INT-LANE kind (<= 6 — never a float,
    // which cannot inhabit an i64 home). Only under `admit_dv` (the GPR-routed
    // retry); the strict pass keeps declining these to the DOUBLE tier.
    let pinned_dv_int = |ip: usize| -> bool {
        admit_dv
            && ta_plan
                .access
                .get(&ip)
                .map_or(false, |&j| ta_plan.pins[j as usize].kind == DV_PIN_KIND)
            && matches!(proto.code[ip], Instr::CallMethod { name, argc, .. }
                if (argc == 1 || argc == 2)
                    && proto
                        .string_constants
                        .get(name as usize)
                        .is_some_and(|k| dv_get_kind(k).is_some_and(|kid| kid <= 6)))
    };
    // A dense Array observed all-Int (kind 252): `arr[i]` loads the element and
    // unboxes it into an i64 home under a per-access tag guard. READS only —
    // a store would have to re-box and can grow/realloc the Vec, so `SetIndex`
    // still falls to the memory path (see the catch-all below).
    let pinned_int_arr = |ip: usize| -> bool {
        ta_plan.access.get(&ip).map_or(false, |&j| {
            ta_plan.pins[j as usize].kind == ARR_INT_PIN_KIND
        })
    };
    let pinned_len = |ip: usize| -> bool {
        ta_plan
            .access
            .get(&ip)
            .is_some_and(|&j| int_length_pin_kind(ta_plan.pins[j as usize].kind))
    };
    for (off, instr) in proto.code[s..=e].iter().enumerate() {
        match *instr {
            Instr::LoadInt { .. }
            | Instr::Move { .. }
            | Instr::LoadGlobal { .. }
            | Instr::StoreGlobal { .. }
            // `let`/`const` global write: inside a hot loop region the binding is
            // already initialized, so it's treated like StoreGlobal.
            | Instr::StoreGlobalStrict { .. }
            | Instr::StoreGlobalResolved { .. }
            | Instr::Add { .. }
            | Instr::Sub { .. }
            | Instr::Mul { .. }
            | Instr::Mod { .. }
            | Instr::AddInt { .. }
            | Instr::Neg { .. }
            // Bitwise/shift: the i64 home's low 32 bits are ToInt32, so these run
            // inline on the int path (compile_region_int emits the int32-lane op).
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
            | Instr::ReturnUndefined => {}
            // Fixed-width integer reads (i8/u8/i16/u16/i32) and the pre-existing
            // Int32 write are admissible; all other index ops decline to MEM.
            Instr::GetIndex { .. } if pinned_int_read(s + off) => {}
            Instr::SetIndex { .. } if pinned_i32_write(s + off) => {}
            // Dense all-Int Array READ (see `pinned_int_arr`); the write is not
            // admitted and falls through to the catch-all.
            Instr::GetIndex { .. } if pinned_int_arr(s + off) => {}
            // Pinned flat-ASCII STRING `str.length` (GetProp) + `str.charCodeAt(i)`
            // (CallMethod). A non-length GetProp / non-charCodeAt or unpinned call
            // still hits the catch-all reject below.
            Instr::GetProp { .. } if pinned_str(s + off) => {}
            // Dense Array `.length` — element representation is irrelevant;
            // read `items.len()` straight from the guarded pin snapshot.
            // The name is re-checked (the pin planner registers a GetProp only for
            // `length`, but this keeps that an assertion rather than an assumption).
            Instr::GetProp { name, .. }
                if pinned_len(s + off)
                    && proto.string_constants.get(name as usize).is_some_and(|k| k == "length") => {}
            Instr::CallMethod { .. } if pinned_str(s + off) => {}
            // W9: pinned-DV get* (int-lane kinds) under the GPR-routed retry.
            Instr::CallMethod { .. } if pinned_dv_int(s + off) => {}
            // W20 M2: `arr.push(int)` on a dense all-Int Array pin. The ONE
            // admitted op that issues a call; see `int_push_enabled` for what
            // re-establishes the tier's register contract around it.
            Instr::CallMethod { .. } if arr_push_pin(proto, s + off, ta_plan).is_some() => {}
            // W20 M4: `!b` on a Bool home is `xor home, 1` -- a bool home holds
            // 0 or 1 by construction (every def is a `movzx home, al` off a
            // `set<cc>`, and the prologue zeroes any home it does not entry
            // load). Worth ZERO on its own and measured so: with `push` still
            // declining, removing `Not` from the tokenizer moved `region_is_int`
            // to true and bought nothing, because the bool pool declined one
            // rung later. It rides the package's latch for exactly that reason
            // -- it is the third rung of one ladder, not a mechanism.
            Instr::Not { .. } if int_push_enabled() => {}
            // B192: dead-in-region completion writes (see `undef_dead` above).
            Instr::LoadUndefined { dst } if undef_dead.contains(&dst) => {}
            // `Math.imul(a, b)` — a 2-arg int32 multiply (ToInt32 of the low 32 of
            // the product); the int path emits a native `imul eax, ecx`.
            Instr::MathOp { op: MathFn::Imul, argc: 2, .. } => {}
            Instr::LoadConst { idx, .. } => {
                // Only Int-tagged constants; a double const can't be an i64 home.
                match proto.constants.get(idx as usize) {
                    Some(c) if c.is_int() => {}
                    _ => unadmitted.push(s + off),
                }
            }
            _ => unadmitted.push(s + off), // Div / non-int32 / non-pinned index / anything else
        }
    }
    Some(unadmitted)
}

/// INTEGER region codegen: each numeric region value is stored as a raw i64 in
/// the low quadword of its xmm home; arithmetic uses `paddq`/`psubq` (~1-cycle
/// latency, vs `addsd`'s ~4), so the carried accumulator chain runs far faster
/// than the double path — the goal being to beat V8 on integer loops.
///
/// Correctness (mirrors JS f64 semantics exactly): every Int Value's i32 payload
/// is SIGN-EXTENDED to i64 on load. After every add/sub the result is checked
/// against `[-2^53, 2^53]`; if it leaves that range (where JS would round) the
/// region flushes all homes and bails to the interpreter at the NEXT ip — the
/// just-overflowed value flushed via `cvtsi2sd` equals JS's rounded result, so
/// resuming is sound. On exit each i64 home is boxed back to an Int Value (if it
/// fits i32) or a double (else, exact since |x| ≤ 2^53). All comparisons are
/// SIGNED. Live-ins are guarded Int-tagged at entry (bail otherwise, no flush).
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_region_int(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    ta_plan: &TaPinPlan,
    ta_snapshot: usize,
    entry: &IntEntry<'_>,
    meter: Option<crate::codegen::meter::Meter>,
) -> Option<JitFn> {
    compile_region_int_maybe_cold(
        proto,
        start,
        end,
        globals_base_helper,
        ta_plan,
        ta_snapshot,
        false,
        entry,
        meter,
    )
}

/// `cold_exit`: compile ops the INT emitter has no arm for as SIDE EXITS rather
/// than declining the region.
///
/// ⚠️ **ALWAYS PASSED `false`. DO NOT TURN IT ON — this is B9, and B9 SHIPPED
/// WRONG ANSWERS.** The soundness argument below is the one B9 shipped with, it
/// reads convincingly, and it is wrong; keep reading before acting on it.
///
/// The argument was: every i64 home is loaded from the register file at region
/// entry and only ever updated by ops that actually execute natively, so an op
/// we exit at never runs natively, no home can hold a value it did not produce,
/// and `flush_exit` writes every home back before the interpreter resumes at
/// that exact ip.
///
/// What it misses is that the REGISTER PLAN is built by skipping the cold
/// blocks, so the plan and the emitted code disagree about what those blocks do.
/// `PERF_ROADMAP.md` B9 has the ten-line reproduction (a `delete` + rebuild loop
/// that returns `s = 0` instead of `3050`). It was found only after
/// `GetIndexConcat` was admitted, which let a region shape reach the tier that
/// had never reached it before.
///
/// It passed the whole gate first: test262 byte-identical across 96,029
/// executions on both tiers, GC stress, and six hand-written cold-block shapes.
/// For codegen that changes TIER SELECTION, a green gate is not evidence of
/// correctness — only of not having produced the counterexample yet. The
/// regression test `fused_concat_key_in_a_branchy_loop` pins the shape.
///
/// The IDEA — one op in a cold block should not demote a whole region — is still
/// sound and still worth up to 4.7x locally. It needs a register plan that
/// accounts for the cold blocks, not block-granular exits over a plan that
/// ignored them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_region_int_maybe_cold(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    ta_plan: &TaPinPlan,
    ta_snapshot: usize,
    cold_exit: bool,
    // Deopt resume map + hoisted entry guards when `proto` is a SPLICE-FLATTENED
    // body (`IntEntry::default()` for an ordinary region — byte-identical).
    entry: &IntEntry<'_>,
    meter: Option<crate::codegen::meter::Meter>,
) -> Option<JitFn> {
    let unadmitted = int_unadmitted_ips(proto, start, end, ta_plan, false)?;
    let cold: FxHashSet<usize> = if unadmitted.is_empty() {
        FxHashSet::default()
    } else if !cold_exit {
        // ── W9: the DV retry ── when EVERY unadmitted ip is a pinned-DV get*
        // the GPR emitter can host (int-lane kinds — re-checked by running the
        // admission again with `admit_dv`), re-plan with DV admission and
        // route the result EXCLUSIVELY into the GPR emitter: the xmm INT
        // emitter has no DV arm and must never see the widened plan (B119
        // fallback contract). admit_split=true — the swizzle loop recycles its
        // receivers (B22's r96), and split-DV receivers are budget-exempt
        // (B115). Any decline falls to the DOUBLE tier's DV arm exactly as
        // today. Off-switch: ZIPP_NO_DV_GPR=1.
        if gpr_homes_enabled()
            && dv_gpr_enabled()
            && int_unadmitted_ips(proto, start, end, ta_plan, true).is_some_and(|v| v.is_empty())
        {
            let empty: FxHashSet<usize> = FxHashSet::default();
            // admit_wt_share=true: the DV swizzle regions carry ~20
            // read_outside registers (outer-phase temps), each pinning a
            // permanent home — without B97 sharing the plan declines on pool
            // exhaustion before any emitter runs. The GPR emitter's
            // write-through is def-complete since W9 (see `gpr_home_map`).
            if let Some(p) = plan_region_cold(
                proto, start, end, ta_plan, true, true, true, false, &empty, true,
            ) {
                match compile_region_int_gpr(
                    proto,
                    start,
                    end,
                    globals_base_helper,
                    ta_plan,
                    ta_snapshot,
                    &p,
                    entry,
                    meter,
                    false, // W10.3: never spill on the first attempt (B96 ordering)
                ) {
                    GprAttempt::Emitted(f) => return Some(f),
                    // The B119 relief valve: one shared-home re-plan when only
                    // the pool overflowed (the 43-op swizzle region against an
                    // 8-10 GPR pool is exactly the case it exists for).
                    GprAttempt::PoolOverflow if gpr_nest_enabled() => {
                        if let Some(shared) = plan_region_cold(
                            proto, start, end, ta_plan, true, true, true, true, &empty, true,
                        ) {
                            if std::env::var_os("ZIPP_JITLOG").is_some() {
                                eprintln!(
                                    "[jit] INT-GPR DV retry [{start},{end}]: shared-home re-plan"
                                );
                            }
                            // W10.3: the post-share attempt may spill the
                            // coldest homes to frame slots instead of
                            // declining (ZIPP_NO_GPR_SPILL_SLOTS=1 restores
                            // the decline byte-for-byte).
                            if let GprAttempt::Emitted(f) = compile_region_int_gpr(
                                proto,
                                start,
                                end,
                                globals_base_helper,
                                ta_plan,
                                ta_snapshot,
                                &shared,
                                entry,
                                meter,
                                true,
                            ) {
                                return Some(f);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!("[jit] INT decline [{start},{end}]: region_is_int=false");
        }
        // B74 discipline: a decline count without the disqualifying opcode makes
        // the next admit candidate a guess. Name every unadmitted ip.
        if std::env::var_os("ZIPP_JITDECLINE").is_some() {
            for &ip in &unadmitted {
                eprintln!("[int-reject] @{ip} {:?}", proto.code[ip]);
            }
        }
        return None;
    } else {
        // The cold unit is the BASIC BLOCK, not the instruction. Excluding only
        // the unadmitted op is unsound: `s += "x"` is
        // `LoadGlobal s; StrConcat; StoreGlobal s`, and LoadGlobal IS admitted —
        // so `s` would still be given an i64 home and the entry guard would
        // reject the string every iteration. Taking the whole block means no
        // admitted op inside it runs natively, so nothing it touches is homed.
        let (s_, e_) = (start as usize, end as usize);
        let targets = region_jump_targets(&proto.code, s_, e_);
        let mut is_block_start = vec![false; e_ - s_ + 1];
        is_block_start[0] = true;
        for &t in &targets {
            if (s_..=e_).contains(&t) {
                is_block_start[t - s_] = true;
            }
        }
        for ip in s_..=e_ {
            let branches = matches!(
                proto.code[ip],
                Instr::Jump { .. }
                    | Instr::JumpIfFalse { .. }
                    | Instr::JumpIfTrue { .. }
                    | Instr::JumpIfNotLt { .. }
                    | Instr::JumpIfNotLe { .. }
            );
            if branches && ip + 1 <= e_ {
                is_block_start[ip + 1 - s_] = true;
            }
        }
        let mut block_of = vec![s_; e_ - s_ + 1];
        let mut cur = s_;
        for ip in s_..=e_ {
            if is_block_start[ip - s_] {
                cur = ip;
            }
            block_of[ip - s_] = cur;
        }
        let cold_blocks: FxHashSet<usize> =
            unadmitted.iter().map(|&ip| block_of[ip - s_]).collect();
        // The header's block and the back-edge's block must run natively, or the
        // region exits every iteration and is worse than not compiling at all.
        if cold_blocks.contains(&block_of[0]) || cold_blocks.contains(&block_of[e_ - s_]) {
            if std::env::var_os("ZIPP_JITLOG").is_some() {
                eprintln!("[jit] INT-cold decline [{start},{end}]: header/back-edge block is cold");
            }
            return None;
        }
        (s_..=e_)
            .filter(|&ip| cold_blocks.contains(&block_of[ip - s_]))
            .collect()
    };
    // The i64 homes carry sign-extended integers, so Bitwise (int32-lane) ops run
    // inline here with no per-op reload/rebox — admit them (admit_bitwise=true), and
    // plan_region's pinned-element handling targets kind-5 (Int32) elements.
    // B94 receiver splitting is admitted here too, but OPT-IN (`ZIPP_INT_SPLIT=1`;
    // see `int_split_enabled` for the measured refutation that keeps it off by
    // default): the emitter below write-throughs every numeric def of a split
    // register BOXED — before any i53 guard, whose exit resumes at ip+1 expecting
    // the result flushed — and skips it in flush_exit. B97 wt-share stays OFF
    // (admit_wt_share=false): its shareable/no-entry-load allocation has only ever
    // been proven against the double emitter.
    let plan = match plan_region_cold(
        proto,
        start,
        end,
        ta_plan,
        true,
        int_split_enabled(),
        false,
        false,
        &cold,
        false,
    ) {
        Some(p) => p,
        None => {
            // A bounded dense-computed splice is deliberately much wider than
            // its source region: mutually-exclusive leaf bodies share one
            // scratch window, but the flattened caller still carries many
            // next-def-bounded temporaries whose ordinary one-home plan can
            // overflow the XMM pool before either emitter gets a chance to
            // price it.  Give ONLY that already-guarded/call-free synthetic
            // body one pass through the GPR emitter's proven write-through
            // sharing plan.  The original proto, ordinary Call splices, and
            // `ZIPP_NO_INT_COMPUTED_LEAF=1` never have `computed_guards`, so
            // their planning and emitted bytes are unchanged.
            //
            // `admit_wt_share` retains its existing next-def/confined-control
            // proof; `share_homes` merely forces the verified linear-scan
            // reuse.  No XMM emitter sees this plan.  The post-share GPR
            // attempt is also the only existing scope in which frame spilling
            // may be considered, and its own default/off switches still apply.
            if !entry.computed_guards.is_empty()
                && !entry.resume.is_empty()
                && cold.is_empty()
                && gpr_homes_enabled()
                && gpr_wt_share_enabled()
            {
                if let Some(shared) = plan_region_cold_gpr_virtual(
                    proto, start, end, ta_plan, true, false, true, true, &cold, false, 18,
                ) {
                    if std::env::var_os("ZIPP_JITLOG").is_some() {
                        eprintln!(
                            "[jit] INT-GPR computed retry [{start},{end}]: shared-home re-plan"
                        );
                    }
                    if let GprAttempt::Emitted(f) = compile_region_int_gpr(
                        proto,
                        start,
                        end,
                        globals_base_helper,
                        ta_plan,
                        ta_snapshot,
                        &shared,
                        entry,
                        meter,
                        true,
                    ) {
                        return Some(f);
                    }
                }
            }
            // ── W8: B94 split receivers on GPR homes ── with the split OFF by
            // default (`int_split_enabled`'s xmm-cost refutation) a recycled
            // pinned receiver (`iv[i] = st` with `st` also the xorshift temp)
            // fails the plan outright and the region falls to MEM. That
            // refutation priced the split on the XMM emitter — every Bitwise
            // op round-tripping xmm↔gpr on the serial chain — and named GPR
            // homes as the real fix. Now that they exist (B119), retry the
            // split plan ONLY into the GPR emitter: engaged → the region hosts
            // there; declined for ANY reason → fall to MEM exactly as before.
            // The xmm emitter still never sees a split plan unless
            // `ZIPP_INT_SPLIT=1`. Off-switch: `ZIPP_NO_GPR_SPLIT=1`.
            let mut split_plan = None;
            if !int_split_enabled() && gpr_split_enabled() && gpr_homes_enabled() && cold.is_empty()
            {
                if let Some(p2) = plan_region_cold(
                    proto, start, end, ta_plan, true, true, false, false, &cold, false,
                ) {
                    if !p2.split_recvs.is_empty() {
                        match compile_region_int_gpr(
                            proto,
                            start,
                            end,
                            globals_base_helper,
                            ta_plan,
                            ta_snapshot,
                            &p2,
                            entry,
                            meter,
                            false, // W10.3: never spill on the first attempt
                        ) {
                            GprAttempt::Emitted(f) => return Some(f),
                            // Same B119 relief valve as the main flow: one
                            // shared-home re-plan when only the pool overflowed.
                            GprAttempt::PoolOverflow if gpr_nest_enabled() => {
                                // W17 `admit_wt_share`: `shared` is handed to
                                // `compile_region_int_gpr` and to nothing else
                                // — `split_plan` below keeps `p2`, the
                                // distinct-homes plan, for the xmm fallback —
                                // which is exactly the licence B97 sharing
                                // needs. See `gpr_wt_share_enabled` for the
                                // contract; `ZIPP_NO_GPR_WT_SHARE=1` restores
                                // the pre-wave plan.
                                if let Some(shared) = plan_region_cold(
                                    proto,
                                    start,
                                    end,
                                    ta_plan,
                                    true,
                                    true,
                                    gpr_wt_share_enabled(),
                                    true,
                                    &cold,
                                    false,
                                ) {
                                    if std::env::var_os("ZIPP_JITLOG").is_some() {
                                        eprintln!(
                                            "[jit] INT-GPR nest retry [{start},{end}]: shared-home re-plan"
                                        );
                                    }
                                    // W10.3: spill on the post-share attempt.
                                    if let GprAttempt::Emitted(f) = compile_region_int_gpr(
                                        proto,
                                        start,
                                        end,
                                        globals_base_helper,
                                        ta_plan,
                                        ta_snapshot,
                                        &shared,
                                        entry,
                                        meter,
                                        true,
                                    ) {
                                        return Some(f);
                                    }
                                }
                            }
                            _ => {}
                        }
                        split_plan = Some(p2);
                    }
                }
            }
            // ── the SPLICED region's own fallback ── a flattened body is
            // wider than the GPR pool long before it is wider than the xmm
            // one (the mix loop plans 12 homes against 8 gprs), and for it the
            // alternative to an xmm split is not a GPR split — it is the
            // MEMORY tier, ~9x slower per op. B94's refutation priced the xmm
            // split against a GPR one on a bitwise chain and does not reach
            // this case, so take the split plan here rather than decline. Only
            // a flattened region qualifies: every other region reaches this
            // line exactly as it did before.
            match split_plan.filter(|_| !entry.resume.is_empty()) {
                Some(p2) => p2,
                None => {
                    if std::env::var_os("ZIPP_JITLOG").is_some() {
                        eprintln!("[jit] INT decline [{start},{end}]: plan_region=None");
                    }
                    return None;
                }
            }
        }
    };
    if !plan.split_recvs.is_empty() && std::env::var_os("ZIPP_JITLOG").is_some() {
        let mut srs: Vec<u16> = plan.split_recvs.iter().copied().collect();
        srs.sort_unstable();
        for sr in srs {
            eprintln!("[jit] INT region [{start},{end}] B94 split receiver r{sr}");
        }
    }
    // ── GPR-home sub-mode (B118) ── bitwise/imul chains pay 3 xmm↔gpr
    // transfers per op on the xmm-home emitter below; when the region is inside
    // the bounded scope of `region_int_gpr` (no cold blocks, no split/wt/DV
    // plan features, live set fits the GPR pool), emit with GPR homes instead.
    // An out-of-scope decline falls through to the xmm emitter unchanged.
    // Off-switch: ZIPP_NO_GPR_HOMES=1.
    if cold.is_empty() && gpr_homes_enabled() {
        match compile_region_int_gpr(
            proto,
            start,
            end,
            globals_base_helper,
            ta_plan,
            ta_snapshot,
            &plan,
            entry,
            meter,
            false, // W10.3 spilling is scoped to the DV / split-fallback retries above
        ) {
            GprAttempt::Emitted(f) => return Some(f),
            // ── B119 nested-loop residual ── the ENCLOSING region of a loop
            // nest carries the inner loop's counters and temps too, so
            // one-home-per-value overflows the small GPR pool even though most
            // of those homes never overlap (the nested xorshift outer counted
            // 13 homes against 8 gprs while the inner fit with 8). Re-plan
            // with forced home sharing — the same proven linear-scan reuse
            // that already serves >14-home xmm regions — and try once more:
            // this is what lets the OUTER region itself go GPR instead of
            // compiling on xmm homes and shadowing the engaged GPR inner
            // (OSR enters the outer back-edge once it compiles, so whichever
            // tier hosts the outer hosts the whole nest). The xmm fallback
            // below keeps the ORIGINAL distinct-homes plan either way.
            // Off-switch: ZIPP_NO_GPR_NEST=1 (retry only).
            GprAttempt::PoolOverflow if gpr_nest_enabled() => {
                if let Some(shared) = plan_region_cold(
                    proto,
                    start,
                    end,
                    ta_plan,
                    true,
                    int_split_enabled(),
                    // W17: same licence as the split-flow retry above —
                    // `shared` reaches `compile_region_int_gpr` alone, and the
                    // xmm fallback below keeps the ORIGINAL plan. See
                    // `gpr_wt_share_enabled`.
                    gpr_wt_share_enabled(),
                    true,
                    &cold,
                    false,
                ) {
                    if std::env::var_os("ZIPP_JITLOG").is_some() {
                        eprintln!("[jit] INT-GPR nest retry [{start},{end}]: shared-home re-plan");
                    }
                    if let GprAttempt::Emitted(f) = compile_region_int_gpr(
                        proto,
                        start,
                        end,
                        globals_base_helper,
                        ta_plan,
                        ta_snapshot,
                        &shared,
                        entry,
                        meter,
                        false, // W10.3 spilling is scoped to the DV / split-fallback retries
                    ) {
                        return Some(f);
                    }
                }
            }
            _ => {}
        }
    }
    if !xmm_plan_is_physical(&plan) {
        debug_assert!(
            false,
            "a symbolic GPR-only home plan reached the XMM emitter"
        );
        decline_emit("int-emit: virtual home reached xmm emitter");
        return None;
    }
    // W28: type splits are planned ONLY for `admit_dv`/`share_homes` plans,
    // which route exclusively into the GPR emitter — the plan this xmm emitter
    // holds passes neither, so the map is empty by construction. Refuse rather
    // than assume: this emitter's `gh`/`Move`/`flush_exit` contract has never
    // been proven against a register with two homes of different KINDS.
    if !plan.ty_splits.is_empty() {
        decline_emit("int-emit: type-split plan");
        return None;
    }
    let mut ops = match dynasmrt::x64::Assembler::new() {
        Ok(a) => a,
        Err(_) => {
            decline_emit("int-emit: assembler alloc failed");
            return None;
        }
    };
    let (s, e) = (start as usize, end as usize);
    // Where the interpreter resumes for an exit taken before `ip` runs. The
    // identity for an ordinary region; for a spliced body the ip that replays
    // the whole call (see `IntEntry::resume`).
    let rip = |ip: usize| -> i32 {
        // Out of range only if `entry.resume` is empty (an unspliced region,
        // where `ip` IS the resume ip) — the map is built one entry past the
        // body so that `ip + 1` is always in it.
        entry
            .resume
            .get(ip.wrapping_sub(s))
            .map_or(ip as i32, |&r| r as i32)
    };

    let in_region: Vec<_> = (s..=e).map(|_| ops.new_dynamic_label()).collect();
    let mut exit_stubs: FxHashMap<u32, dynasmrt::DynamicLabel> = FxHashMap::default();
    let flush_exit = ops.new_dynamic_label();
    let entry_bail = ops.new_dynamic_label();
    // Step metering (a metered VM only). One charge per basic block, of that
    // block's exact instruction count, against `[rdi + off]` — `rdi` is the VM
    // pointer this body already holds, so no register and no frame slot.
    let blocks = crate::codegen::meter::block_map(meter, &proto.code, s, e);
    let lbl = |ip: u32, in_region: &[dynasmrt::DynamicLabel]| in_region[(ip - start) as usize];

    // ── prologue ── mirrors the double path (save callee-saved, fetch globals base,
    // pinned-TypedArray snapshots, save xmm6..15) — only the live-in loads + body
    // differ. r13/r14 additionally hold the 2^53/2^54 guard constants (pre-loaded
    // once). Frame layout with pinned views matches the regalloc path exactly:
    // [shadow 32][TA snapshot slots 32·n_ta][xmm6..15 save 160][pad 8], shadow at the
    // bottom so the snapshot calls have shadow space and rsp stays 16-aligned.
    let n_ta = ta_plan.pins.len() as i32;
    // Batching deliberately stays out of spliced/self-call bodies and metered
    // VMs: a splice needs its call-resume map, while metering must retain one
    // charge for every original bytecode op.
    let push3 = arr_push3_steps(
        proto,
        s,
        e,
        ta_plan,
        &plan,
        &cold,
        int_push3_enabled() && entry.resume.is_empty() && meter.is_none(),
    );
    let has_push3 = !push3.is_empty();
    if has_push3 && std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!(
            "[jit] INT region [{start},{end}] array-push3 groups={}",
            push3.len() / 3
        );
    }
    // W20 M2: a region with an admitted `arr.push(int)` reserves 64 more bytes
    // above the xmm6..15 saves — the call-save area for the four `BOOL_GPRS`
    // and the four VOLATILE numeric homes (xmm2..xmm5). A batched group reserves
    // another 48 bytes for its three staged i64 arguments plus the two future
    // argument homes' rollback values. It is part of THIS
    // frame, above the shadow space, so no callee can write it. A push always
    // brings a pin, so the area only ever exists in the `n_ta > 0` layout
    // (whose `frame` keeps rsp 16-aligned for the call; +64 preserves that).
    let has_push = region_has_arr_push(proto, s, e, ta_plan);
    if has_push && n_ta == 0 {
        decline_emit("int-emit: arr.push without a pin slot");
        return None;
    }
    let (frame, xmm_off, ta_base) = if n_ta > 0 {
        (
            200 + 32 * n_ta + if has_push { 64 } else { 0 } + if has_push3 { 48 } else { 0 },
            32 + 32 * n_ta,
            32i32,
        )
    } else {
        (160i32, 0i32, 0i32)
    };
    // [shadow 32][pin slots 32·n_ta][xmm6..15 saves 160][push call-save 64]
    // [optional push3 values/rollback 48][pad]
    let psave_off = xmm_off + 160;
    let push3_vals_off = psave_off + 64;
    let push3_rollback_off = push3_vals_off + 24;
    // The registers the win64 callee may scratch that this planner OWNS:
    // the bool gprs in use (bool homes AND `gpr_const` compare mirrors), and
    // every numeric home in xmm2..xmm5. xmm6..xmm15 are callee-saved by the
    // ABI, and rbx/rsi/rdi/r12/r13/r14 are non-volatile.
    let (save_gprs, save_xmms): (Vec<u8>, Vec<u8>) = if has_push {
        let mut g: Vec<u8> = plan.bool_regs.iter().map(|&(_, gb)| gb).collect();
        g.extend(plan.gpr_const.values().map(|&(gb, _)| gb));
        g.sort_unstable();
        g.dedup();
        let mut x: Vec<u8> = Vec::new();
        for h in plan.reg_home.values() {
            if let Home::Xmm(xi) = *h {
                if xi < 6 {
                    x.push(xi);
                }
            }
        }
        x.extend(plan.glob_home.values().copied().filter(|&xi| xi < 6));
        x.extend(plan.addint_imm_home.values().copied().filter(|&xi| xi < 6));
        x.sort_unstable();
        x.dedup();
        (g, x)
    } else {
        (Vec::new(), Vec::new())
    };
    debug_assert!(save_gprs.len() <= 4 && save_xmms.len() <= 4);
    dynasm!(ops
        ; push rbx
        ; push rsi
        ; push rdi
        ; push r12
        ; push r13
        ; push r14
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
    );
    for k in 0..10u32 {
        let xi = 6 + k as u8;
        dynasm!(ops ; movdqu [rsp + xmm_off + (k as i32) * 16], Rx(xi));
    }
    emit_int_splice_entry_guards(&mut ops, entry, n_ta > 0, entry_bail);
    // ── pinned-TypedArray snapshots ── BEFORE loading any numeric home (jit_ta_snapshot
    // clobbers volatile xmm0..5, which double as homes; xmm6..15 are already saved and
    // no home is loaded yet). Each slot gets {obj_bits, base, len} (or {0,0,0} → the
    // per-access identity guard misses → deopt). r12/rbx/r13/r14 are callee-saved across
    // the call. This is the last call before the loop.
    for (j, pin) in ta_plan.pins.iter().enumerate() {
        match pin.src {
            TaPinSrc::Global(g) => dynasm!(ops ; mov rdx, [r12 + (g as i32) * 8]),
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
    // ── W20 M2 push eligibility, hoisted ── the seven conditions
    // `jit_array_push` re-tested on every single append are region-invariant
    // (nothing an INT region body can execute runs user code), so they are
    // settled here, once per pushed pin, and the per-append helper is left with
    // the append itself. A `no` takes `entry_bail`: no flush, resume at the
    // header, counts as a deopt -- the same contract as a failed live-in guard,
    // and the same outcome the per-access identity guard would have produced
    // one iteration later at 10.8M times the cost.
    if has_push {
        let mut pushed: Vec<usize> = (s..=e)
            .filter_map(|ip| arr_push_pin(proto, ip, ta_plan))
            .collect();
        pushed.sort_unstable();
        pushed.dedup();
        for j in pushed {
            dynasm!(ops
                ; mov rcx, rdi                                  // vm
                ; mov rdx, [rsp + ta_base + 32 * j as i32]      // pinned receiver bits
                ; mov rax, QWORD crate::vm::jit_array_push_gate as usize as i64
                ; call rax
                ; test rax, rax
                ; jz => entry_bail
            );
        }
    }
    // ── W20 M2 arr-pin disjointness ── the push arm repairs the snapshot of
    // the array it appends to, and nothing else. That is complete exactly when
    // no OTHER dense-Array pin in this region names the SAME array — otherwise
    // a `Vec` realloc would leave the sibling pin's `base` dangling. Identity
    // cannot change in-region (no user code runs), so ONE pairwise check here
    // settles it for the whole run; a match takes `entry_bail` (no flush,
    // resume at the header) exactly like a failed live-in guard. Declined
    // snapshots ({0,0,0}) are skipped: their accesses miss identity and deopt
    // on their own, and two of them are not evidence of aliasing.
    if has_push {
        let arr: Vec<usize> = (0..ta_plan.pins.len())
            .filter(|&j| crate::codegen::is_arr_pin(ta_plan.pins[j].kind))
            .collect();
        for (a, &j1) in arr.iter().enumerate() {
            for &j2 in &arr[a + 1..] {
                let skip = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rsp + ta_base + 32 * j1 as i32]
                    ; test rax, rax
                    ; jz => skip
                    ; cmp rax, [rsp + ta_base + 32 * j2 as i32]
                    ; je => entry_bail
                    ; => skip
                );
            }
        }
    }
    // ── W7 hoisted pin identity guards ── one check per hoisted pin, HERE,
    // instead of a source-load+compare at every access. The snapshot was just
    // taken FROM the source, so `source == obj_bits` holds by construction and
    // the region provably cannot change either (see `hoistable_pins`); the one
    // thing left to check is snapshot VALIDITY — a declined snapshot is
    // {0,0,0}. A miss takes `entry_bail` like any failed entry guard: no
    // flush, resume at the header, counts as a deopt (chronic → evict).
    for j in 0..ta_plan.pins.len() {
        if plan.hoist_pins.contains(&(j as u8)) {
            dynasm!(ops ; cmp QWORD [rsp + ta_base + 32 * j as i32], 0 ; je => entry_bail);
        }
    }
    // Live-in globals/regs into i64 homes: an Int-tagged Value sign-extends, an
    // integral double in [-2^53, 2^53] converts, anything else takes entry_bail.
    for &(gi, x) in &plan.live_in_globs {
        dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]);
        emit_int_entry_load(&mut ops, x, entry_bail);
    }
    for &(r, x) in &plan.live_in_regs {
        if entry.is_scratch(r) {
            continue;
        }
        dynasm!(ops ; mov rax, [rbx + dreg(r)]);
        emit_int_entry_load(&mut ops, x, entry_bail);
    }
    // Bool homes last. This ORDER is no longer load-bearing — no entry-load
    // helper scratches a BOOL_GPR any more (see the register contract on
    // `BOOL_GPRS`) — but it is kept: it is the order the other two tiers use.
    emit_bool_home_zero(&mut ops, &plan);
    for &(r, g) in &plan.live_in_bools {
        if entry.is_scratch(r) {
            continue;
        }
        dynasm!(ops ; mov rax, [rbx + dreg(r)]);
        emit_bool_entry_load(&mut ops, g, entry_bail);
    }
    for &hip in &plan.hoist_ips {
        emit_int_const(&mut ops, &plan, &proto.code[hip], proto);
    }
    // Spare-home constants: AddInt immediates as i64 xmm homes, and gpr mirrors
    // of hoisted compare constants (both filled once; the body reads them).
    {
        let mut imms: Vec<(i32, u8)> = plan.addint_imm_home.iter().map(|(&i, &h)| (i, h)).collect();
        imms.sort_unstable();
        for (imm, h) in imms {
            dynasm!(ops ; mov rax, QWORD imm as i64 ; movq Rx(h), rax);
        }
        let mut gcs: Vec<(u8, i64)> = plan.gpr_const.values().copied().collect();
        gcs.sort_unstable();
        for (g, v) in gcs {
            dynasm!(ops ; mov Rq(g), QWORD v);
        }
    }
    // ── W7 hoisted pinned-STRING lengths ── the pin's identity is entry-
    // guarded above and the region cannot change it, so the snapshot `units`
    // IS `str.length` for the whole run: fill the dst home once; the body op
    // is skipped (exactly like a hoisted constant).
    for &hip in &plan.hoist_len_ips {
        if let Instr::GetProp { dst, .. } = proto.code[hip] {
            let j = ta_plan.access[&hip] as usize;
            let d = xh(&plan, dst);
            dynasm!(ops
                ; mov rax, [rsp + ta_base + 32 * j as i32 + 16]
                ; movq Rx(d), rax
            );
        }
    }
    dynasm!(ops ; jmp => lbl(start, &in_region));

    // ── body ──
    // Compare→branch flag fusion: the last EMITTED op being a compare leaves its
    // flags live for an immediately following conditional jump (no re-`test`).
    let mut flag_cmp: Option<(usize, u16, Cmp)> = None;
    // Redundant-copy tracker (see `LastCopy`).
    let mut lc: LastCopy = None;
    for ip in s..=e {
        let rip_at = rip(ip);
        let rip_after = rip(ip + 1);
        dynasm!(ops ; => lbl(ip as u32, &in_region));
        if plan.jump_targets.contains(&ip) {
            lc = None; // control may arrive here with different home contents
        }
        // Charge this basic block before running it. A block is straight-line,
        // so entering it means executing all of it — the same count the
        // interpreter would have made, in the same unit.
        let charged = crate::codegen::meter::charge_block(&mut ops, &blocks, ip, &mut exit_stubs);
        // B9: an ip in a cold block never runs natively — flush every home and
        // hand this exact ip back to the interpreter, which runs the block (and
        // the rest of the iteration) itself.
        if cold.contains(&ip) {
            dynasm!(ops ; mov DWORD [rsi], rip_at ; jmp => flush_exit);
            continue;
        }
        if let Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } = proto.code[ip] {
            if plan.hoisted.contains(&dst) {
                continue;
            }
        }
        // A W7-hoisted pinned-STRING length: the prologue filled its home from
        // the snapshot once. Like a hoisted constant, nothing is emitted, so
        // flags and the copy tracker survive.
        if plan.hoist_len_ips.contains(&ip) {
            continue;
        }
        // Dead-code elimination: skip a pure value op whose result is never read
        // (a `dead` reg — see plan_region). All int-region ops are side-effect-free
        // (heap/calls decline the region), so this is sound. The label was already
        // emitted above so any jump still resolves. NOTE: jumps/stores/returns
        // aren't reg-defs, so `writes_reg` returns None for them — never skipped.
        if let Some(d) = writes_reg(&proto.code[ip]) {
            if plan.dead.contains(&d) {
                continue;
            }
        }
        // The charge's `sub` clobbers flags, so a compare from an earlier ip can
        // no longer drive this ip's branch. In practice fusion never reaches a
        // block head anyway (a branch consumes `flag_cmp` without restoring it,
        // and every block head follows a branch or is a jump target), but the
        // fusion predicate is subtle enough that stating the dependency beats
        // relying on it.
        let prev_flag = flag_cmp.take().filter(|_| !charged);
        // Set by arms that emit their own B94 write-through BEFORE an i53 guard
        // (the guard's exit resumes at ip+1 expecting the result flushed); the
        // generic post-op hook below then skips the duplicate store.
        let mut wt_pre = false;
        match proto.code[ip] {
            Instr::LoadInt { .. } | Instr::LoadConst { .. } => {
                emit_int_const(&mut ops, &plan, &proto.code[ip], proto);
                if let Some(d) = writes_reg(&proto.code[ip]) {
                    copy_clobber(&mut lc, xh(&plan, d));
                }
            }
            // ── B192: statement-completion regs (untyped, unhomed) ── every
            // def writes THROUGH to the frame slot (the GPR emitter's twin
            // arms carry the full reasoning). `mov` preserves FLAGS for the
            // LoadUndefined; the Move's boxing clobbers them.
            Instr::LoadUndefined { dst } => {
                debug_assert!(plan.undef_dead.contains(&dst));
                let bits = crate::value::Value::UNDEFINED.bits();
                dynasm!(ops ; mov rax, QWORD bits as i64 ; mov [rbx + dreg(dst)], rax);
                flag_cmp = prev_flag;
            }
            Instr::Move { dst, src } if plan.undef_dead.contains(&dst) => {
                let srx = xh(&plan, src);
                emit_int_box_from_home(&mut ops, srx);
                dynasm!(ops ; mov [rbx + dreg(dst)], rax);
                copy_clobber(&mut lc, srx); // rax/flags clobbered; drop any live copy fusion
                lc = None;
            }
            Instr::Move { dst, src } => match home(&plan, dst) {
                Home::Xmm(d) => {
                    let srx = xh(&plan, src);
                    if d != srx && !copy_is_noop(lc, d, srx) {
                        dynasm!(ops ; movdqa Rx(d), Rx(srx));
                        copy_clobber(&mut lc, d);
                        lc = Some((d, srx));
                    } else {
                        flag_cmp = prev_flag; // nothing emitted; flags still live
                    }
                }
                Home::Gpr(d) => {
                    let sg = gh(&plan, src);
                    dynasm!(ops ; mov Rq(d), Rq(sg));
                }
            },
            // ── pinned receiver (ta_recv_regs) / B94 split receiver ── the
            // object has no numeric home here (the element-access emitter reads
            // it via the pin's source; a split receiver's i64 home belongs to the
            // register's NUMERIC half), so it goes to the register's memory slot,
            // which stays authoritative for this register throughout the region.
            // `emit_recv_slot_store` carries why the ta_recv half is not a no-op.
            // Two `mov`s: no flag effects, so a fused compare stays live.
            Instr::LoadGlobal { dst, idx }
                if plan.ta_recv_regs.contains(&dst) || plan.split_recv_lg.contains(&ip) =>
            {
                emit_recv_slot_store(&mut ops, dst, idx);
                flag_cmp = prev_flag;
            }
            Instr::LoadGlobal { dst, idx } => {
                let d = xh(&plan, dst);
                let g = plan.glob_home[&idx];
                if d != g && !copy_is_noop(lc, d, g) {
                    dynasm!(ops ; movdqa Rx(d), Rx(g));
                    copy_clobber(&mut lc, d);
                    lc = Some((d, g));
                } else {
                    flag_cmp = prev_flag;
                }
            }
            Instr::StoreGlobal { idx, src }
            | Instr::StoreGlobalStrict { idx, src }
            | Instr::StoreGlobalResolved { idx, src } => {
                let g = plan.glob_home[&idx];
                let srx = xh(&plan, src);
                if g != srx && !copy_is_noop(lc, g, srx) {
                    dynasm!(ops ; movdqa Rx(g), Rx(srx));
                    copy_clobber(&mut lc, g);
                    lc = Some((g, srx));
                } else {
                    flag_cmp = prev_flag;
                }
            }
            Instr::Add { dst, a, b } => {
                emit_ibin(
                    &mut ops, &plan, ip, rip_after, flush_exit, dst, a, b, true, &mut lc,
                );
                wt_pre = true; // emit_ibin write-throughs before its own guard
            }
            Instr::Sub { dst, a, b } => {
                emit_ibin(
                    &mut ops, &plan, ip, rip_after, flush_exit, dst, a, b, false, &mut lc,
                );
                wt_pre = true; // emit_ibin write-throughs before its own guard
            }
            Instr::Mul { dst, a, b } => {
                let d = xh(&plan, dst);
                copy_clobber(&mut lc, d);
                if let Some(&(val_reg, shift)) = plan.mul_shift.get(&ip) {
                    // Guard-elided multiply by a constant power of two: a left
                    // shift (logical == arithmetic for the proven-in-range i64).
                    let vx = xh(&plan, val_reg);
                    if d != vx {
                        dynasm!(ops ; movdqa Rx(d), Rx(vx));
                    }
                    dynasm!(ops ; psllq Rx(d), shift as i8);
                } else if plan.elide_guard.contains(&ip) {
                    // Result proven within ±2^53 ⇒ no i64 overflow possible and
                    // no 2^53 guard needed; bare imul through the gprs.
                    let (ax, bx) = (xh(&plan, a), xh(&plan, b));
                    dynasm!(ops
                        ; movq rax, Rx(ax)
                        ; movq rcx, Rx(bx)
                        ; imul rax, rcx
                        ; movq Rx(d), rax
                    );
                } else {
                    // i64 multiply via imul (gpr). On i64 OVERFLOW (product ≥ 2^63)
                    // the result wrapped → bail at THIS ip WITHOUT storing dst, so the
                    // interpreter redoes it in f64 (reading the flushed operands). On a
                    // representable-but-large product the 2^53 guard handles it (like
                    // add): flush via cvtsi2sd (== JS's rounded product) + resume ip+1.
                    let (ax, bx) = (xh(&plan, a), xh(&plan, b));
                    let ovf = ops.new_dynamic_label();
                    let done = ops.new_dynamic_label();
                    dynasm!(ops
                        ; movq rax, Rx(ax)
                        ; movq rcx, Rx(bx)
                        ; imul rax, rcx
                        ; jo => ovf            // i64 overflow → can't represent; redo in interp
                        ; movq Rx(d), rax
                        ; jmp => done
                        ; => ovf
                        ; mov DWORD [rsi], rip_at // resume at THIS op (dst not written)
                        ; jmp => flush_exit
                        ; => done
                    );
                    wt_pre = emit_int_wt(&mut ops, &plan, dst, false) || wt_pre;
                    emit_i53_guard(&mut ops, d, rip_after, flush_exit);
                }
            }
            Instr::Mod { dst, a, b } => {
                // i64 remainder via idiv (gpr): `rem = a % b`, truncated toward
                // zero with the dividend's sign — exactly JS `%` for integer
                // operands (the region is all-int). `% 0` → NaN (not an Int) →
                // bail at THIS ip (the interpreter redoes it, yielding NaN). The
                // dividend is guaranteed |a| ≤ 2^53 (entry guard + per-op i53
                // guard) so it is never i64::MIN ⇒ idiv can't #DE; and
                // |rem| < |b| ≤ 2^53, so the result is always representable (no
                // i53 guard needed). rcx/rdx are scratch here (bool homes live in
                // r8..r11, never rcx/rdx).
                let (d, ax, bx) = (xh(&plan, dst), xh(&plan, a), xh(&plan, b));
                copy_clobber(&mut lc, d);
                let zbail = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                let store = ops.new_dynamic_label();
                dynasm!(ops
                    ; movq rax, Rx(ax)
                    ; movq rcx, Rx(bx)
                    ; test rcx, rcx
                    ; jz => zbail              // % 0 → NaN → redo in interp
                    ; cqo                       // sign-extend rax into rdx:rax
                    ; idiv rcx                  // rdx = remainder, rax = quotient
                    // A ZERO remainder from a NEGATIVE dividend is -0 in JS
                    // (`-20 % 5` is -0, not 0), and -0 has no i64 home. Bail so
                    // the interpreter produces the double. rcx is dead after the
                    // idiv, so reload the dividend through it to test its sign.
                    ; test rdx, rdx
                    ; jnz => store
                    ; movq rcx, Rx(ax)
                    ; test rcx, rcx
                    ; js => zbail
                    ; => store
                    ; movq Rx(d), rdx
                    ; jmp => done
                    ; => zbail
                    ; mov DWORD [rsi], rip_at // resume at THIS op (dst unwritten)
                    ; jmp => flush_exit
                    ; => done
                );
            }
            Instr::AddInt { dst, a, imm, .. } => {
                let d = xh(&plan, dst);
                let ax = xh(&plan, a);
                if imm == 0 {
                    // `a + 0` over i64 is the identity (`AddInt` is `Add`, and the
                    // region is integer-only — no -0.0 to preserve): a pure copy,
                    // never able to overflow.
                    if d != ax && !copy_is_noop(lc, d, ax) {
                        dynasm!(ops ; movdqa Rx(d), Rx(ax));
                        copy_clobber(&mut lc, d);
                        lc = Some((d, ax));
                    } else {
                        flag_cmp = prev_flag;
                    }
                } else {
                    let skip_copy = d == ax || copy_is_noop(lc, d, ax);
                    if let Some(&ch) = plan.addint_imm_home.get(&imm) {
                        // The immediate sits in a prologue-filled const home.
                        if !skip_copy {
                            dynasm!(ops ; movdqa Rx(d), Rx(ax));
                        }
                        dynasm!(ops ; paddq Rx(d), Rx(ch));
                    } else {
                        // Materialise the (sign-extended) immediate as i64 in xmm0.
                        dynasm!(ops ; mov rax, QWORD imm as i64 ; movq xmm0, rax);
                        if !skip_copy {
                            dynasm!(ops ; movdqa Rx(d), Rx(ax));
                        }
                        dynasm!(ops ; paddq Rx(d), xmm0);
                    }
                    copy_clobber(&mut lc, d);
                    wt_pre = emit_int_wt(&mut ops, &plan, dst, false);
                    if !plan.elide_guard.contains(&ip) {
                        emit_i53_guard(&mut ops, d, rip_after, flush_exit);
                    }
                }
            }
            Instr::Neg { dst, a } => {
                let d = xh(&plan, dst);
                let ax = xh(&plan, a);
                // `-0` is NOT representable in an i64 home, and `-(0)` must yield
                // the double -0: negating zero here silently produced integer 0,
                // so `Object.is(-0, -0)` came out false inside a compiled loop
                // (the literal `-0` lowers to `LoadInt 0; Neg`). Bail to the
                // interpreter for that one input. Neg is pure, so resuming AT this
                // ip is idempotent, and `dst`'s home is entry-loaded so the flush
                // writes back its pre-op value.
                let nonzero = ops.new_dynamic_label();
                dynasm!(ops
                    ; movq rax, Rx(ax)
                    ; test rax, rax
                    ; jnz => nonzero
                    ; mov DWORD [rsi], rip_at
                    ; jmp => flush_exit
                    ; => nonzero
                    ; pxor xmm0, xmm0
                    ; psubq xmm0, Rx(ax)
                    ; movdqa Rx(d), xmm0
                );
                copy_clobber(&mut lc, d);
                wt_pre = emit_int_wt(&mut ops, &plan, dst, false);
                if !plan.elide_guard.contains(&ip) {
                    emit_i53_guard(&mut ops, d, rip_after, flush_exit);
                }
            }
            Instr::Lt { dst, a, b } => {
                emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Lt);
                flag_cmp = Some((ip, dst, Cmp::Lt));
            }
            Instr::Le { dst, a, b } => {
                emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Le);
                flag_cmp = Some((ip, dst, Cmp::Le));
            }
            Instr::Gt { dst, a, b } => {
                emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Gt);
                flag_cmp = Some((ip, dst, Cmp::Gt));
            }
            Instr::Ge { dst, a, b } => {
                emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Ge);
                flag_cmp = Some((ip, dst, Cmp::Ge));
            }
            Instr::Eq { dst, a, b } => {
                emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Eq);
                flag_cmp = Some((ip, dst, Cmp::Eq));
            }
            Instr::Ne { dst, a, b } => {
                emit_icmp(&mut ops, &plan, dst, a, b, Cmp::Ne);
                flag_cmp = Some((ip, dst, Cmp::Ne));
            }
            // W20 M4: `!b` on bool gpr homes. A bool home holds 0 or 1 by
            // construction (every def is `movzx home, al` off a `set<cc>`, the
            // entry load validates a Bool tag, and the prologue zeroes any home
            // it does not entry-load), so the negation is one `xor`. Scratches
            // nothing: the BOOL_GPRS register contract forbids touching
            // r8..r11 that this region does not own, and both operands here ARE
            // its own bool homes.
            Instr::Not { dst, a } if int_push_enabled() => {
                let d = gh(&plan, dst);
                let sa = gh(&plan, a);
                if d != sa {
                    dynasm!(ops ; mov Rq(d), Rq(sa));
                }
                dynasm!(ops ; xor Rq(d), 1);
            }
            Instr::Jump { target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; jmp => t);
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                let if_false = matches!(proto.code[ip], Instr::JumpIfFalse { .. });
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                // Flag fusion: the integer compare that produced `cond` was the
                // last emitted op, so its flags directly drive this branch. The
                // setcc/movzx that boxed the bool home don't touch flags. Any ip
                // in (cmp_ip, ip] being a jump target would let a path arrive
                // here with foreign flags — bail out to the generic `test`.
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
                    Some(op) => {
                        // Jump when the comparison is false (JumpIfFalse) / true.
                        match (op, if_false) {
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
                        }
                    }
                    None => {
                        let c = gh(&plan, cond);
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
                emit_icmp_flags(&mut ops, &plan, a, b);
                // !(a<b) ⇔ a>=b (SIGNED).
                dynasm!(ops ; jge => t);
            }
            Instr::JumpIfNotLe { a, b, target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                emit_icmp_flags(&mut ops, &plan, a, b);
                // !(a<=b) ⇔ a>b (SIGNED).
                dynasm!(ops ; jg => t);
            }
            // ── pinned-Int32Array element read ── iv[i] → sign-extend the i32 element
            // into the dst i64 home (UNBOXED). Guards (any miss DEOPTs to the
            // interpreter AT this ip — index ops are all-or-nothing, so re-execution
            // is sound): (1) receiver identity vs the prologue snapshot; (2) unsigned
            // bounds (catches <0). The index home holds an integer already (no f64
            // round-trip needed — the int path proves every value integral).
            Instr::GetIndex { dst, key, .. } => {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let d = xh(&plan, dst);
                let kx = xh(&plan, key);
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                // W7: a hoisted pin's identity was checked ONCE at entry and
                // the region provably cannot change it — only the semantic
                // bounds/tag guards remain per access (see `hoistable_pins`).
                if !plan.hoist_pins.contains(&(j as u8)) {
                    match ta_plan.pins[j].src {
                        TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                        TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                    }
                    dynasm!(ops
                        ; cmp rax, [rsp + off]           // receiver vs snapshot obj_bits
                        ; jne => deopt
                    );
                }
                dynasm!(ops
                    ; movq rcx, Rx(kx)                   // index (i64 home, integral)
                    ; cmp rcx, [rsp + off + 16]          // unsigned: i < len (catches <0)
                    ; jae => deopt
                    ; mov rdx, [rsp + off + 8]           // pinned base
                );
                if ta_plan.pins[j].kind == ARR_INT_PIN_KIND {
                    // Dense Array: the element is a NaN-boxed `Value` (stride 8),
                    // so unlike a TypedArray's raw i32 it must be tag-checked
                    // before it can inhabit an i64 home. Anything not Int-tagged
                    // — a double, a HOLE (0x7FFC…), a heap value — deopts to the
                    // interpreter AT this ip. That single guard is what makes the
                    // all-Int sample at plan time a hint and not a soundness gate.
                    // Scratch is rdx (its base value is dead after the load),
                    // NOT r10: r10 is BOOL_GPRS[2], so inside the BODY it can hold
                    // a live Bool home or a prologue-filled `gpr_const` compare
                    // mirror, neither of which anything reloads per iteration.
                    dynasm!(ops
                        ; mov rax, [rdx + rcx * 8]       // items[i] (Value bits)
                        ; mov rdx, rax
                        ; shr rdx, 48
                        ; cmp edx, INT_TAG_HI as i32
                        ; jne => deopt                   // double / HOLE / heap → deopt
                        ; movsxd rax, eax                // Int payload, sign-extended
                    );
                } else {
                    match int_ta_load_kind(ta_plan.pins[j].kind)? {
                        IntTaLoadKind::I8 => dynasm!(ops ; movsx rax, BYTE [rdx + rcx]),
                        IntTaLoadKind::U8 => dynasm!(ops ; movzx eax, BYTE [rdx + rcx]),
                        IntTaLoadKind::I16 => dynasm!(ops ; movsx rax, WORD [rdx + rcx * 2]),
                        IntTaLoadKind::U16 => dynasm!(ops ; movzx eax, WORD [rdx + rcx * 2]),
                        IntTaLoadKind::I32 => {
                            dynasm!(ops ; movsxd rax, DWORD [rdx + rcx * 4])
                        }
                    }
                }
                dynasm!(ops
                    ; movq Rx(d), rax
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], rip_at         // resume AT this ip
                    ; jmp => flush_exit
                    ; => done
                );
                copy_clobber(&mut lc, d);
                lc = None;
            }
            // ── pinned-Int32Array element write ── iv[i] = v → store the val home's
            // low 32 bits (== ToInt32(v), the Int32Array store). Same guards; an OOB
            // store deopts (the interpreter does the spec coerce-then-silent-noop).
            Instr::SetIndex { key, val, .. } => {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let kx = xh(&plan, key);
                let vx = xh(&plan, val);
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                // W7: identity hoisted to entry for a hoisted pin (see GetIndex).
                if !plan.hoist_pins.contains(&(j as u8)) {
                    match ta_plan.pins[j].src {
                        TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                        TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                    }
                    dynasm!(ops
                        ; cmp rax, [rsp + off]
                        ; jne => deopt
                    );
                }
                dynasm!(ops
                    ; movq rcx, Rx(kx)
                    ; cmp rcx, [rsp + off + 16]
                    ; jae => deopt
                    ; mov rdx, [rsp + off + 8]
                    ; movq rax, Rx(vx)                   // value i64 home
                    ; mov DWORD [rdx + rcx * 4], eax     // store low 32 (== ToInt32(v))
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], rip_at
                    ; jmp => flush_exit
                    ; => done
                );
                lc = None;
            }
            Instr::Bitwise { dst, a, b, op } => {
                use crate::bytecode::BitwiseOp as B;
                let (d, ax, bx) = (xh(&plan, dst), xh(&plan, a), xh(&plan, b));
                copy_clobber(&mut lc, d);
                // The i64 homes hold sign-extended integers, so eax (the low 32 of
                // operand `a`) IS ToInt32(a), and read unsigned it is ToUint32(a) —
                // no per-op reload/tag-check/rebox (the mem path's cost). And/Or/Xor
                // and the signed shifts produce a signed i32 (sign-extended back to
                // the i64 home, always boxes as Int). `>>>` yields a u32 (0..2^32-1):
                // the 32-bit `shr` zero-extends it into rax, and it stays within
                // ±2^53, so exit-boxing picks Int-vs-double. x86 masks the shift
                // count (cl) to 5 bits, matching JS's `& 31`. rax/rcx are scratch.
                dynasm!(ops ; movq rax, Rx(ax) ; movq rcx, Rx(bx));
                match op {
                    B::And => dynasm!(ops ; and eax, ecx ; movsxd rax, eax),
                    B::Or => dynasm!(ops ; or eax, ecx ; movsxd rax, eax),
                    B::Xor => dynasm!(ops ; xor eax, ecx ; movsxd rax, eax),
                    B::Shl => dynasm!(ops ; shl eax, cl ; movsxd rax, eax),
                    B::Shr => dynasm!(ops ; sar eax, cl ; movsxd rax, eax),
                    B::Ushr => dynasm!(ops ; shr eax, cl), // 32-bit write zero-extends rax
                }
                dynasm!(ops ; movq Rx(d), rax);
            }
            // ── pinned-STRING charCodeAt ── str.charCodeAt(i) → a DIRECT ASCII byte
            // load into the dst i64 home (0..255, zero-extended). Guards: receiver
            // identity + unsigned bounds vs the pin's `units`. An OOB index (i >=
            // units) — where the interpreter returns NaN, which an i64 home CANNOT
            // represent — DEOPTs at this ip (flush + resume; the loop is pure so the
            // re-run is sound). Index read from the i64 home (already integral).
            Instr::CallMethod { dst, arg_base, .. }
                if ta_plan
                    .access
                    .get(&ip)
                    .map_or(false, |&j| ta_plan.pins[j as usize].kind == STR_PIN_KIND) =>
            {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let d = xh(&plan, dst);
                let kx = xh(&plan, arg_base);
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                // W7: identity hoisted to entry for a hoisted pin (see GetIndex).
                if !plan.hoist_pins.contains(&(j as u8)) {
                    match ta_plan.pins[j].src {
                        TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                        TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                    }
                    dynasm!(ops
                        ; cmp rax, [rsp + off]           // receiver identity vs snapshot
                        ; jne => deopt
                    );
                }
                dynasm!(ops
                    ; movq rcx, Rx(kx)                   // index (i64 home, integral)
                    ; cmp rcx, [rsp + off + 16]          // unsigned: i < units (catches <0/OOB)
                    ; jae => deopt                       // OOB → deopt (interp yields NaN)
                    ; mov rdx, [rsp + off + 8]           // pinned bytes base
                    ; movzx eax, BYTE [rdx + rcx]        // ASCII code unit, zero-extend 0..255
                    ; movq Rx(d), rax
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], rip_at         // resume AT this ip
                    ; jmp => flush_exit
                    ; => done
                );
                copy_clobber(&mut lc, d);
                lc = None;
            }
            // ── exact three-array push batch ── the receiver/argument setup
            // instructions remain emitted in source order. The first two
            // calls stage their already-unboxed argument; the third makes one
            // leaf call which preflights all three pins before mutating any,
            // then appends in the original order and refreshes every snapshot.
            // All three result registers are proven unread by `arr_push3_steps`.
            Instr::CallMethod { arg_base, .. } if push3.contains_key(&ip) => {
                let step = push3[&ip];
                let vx = xh(&plan, arg_base);
                dynasm!(ops ; movq [rsp + push3_vals_off + 8 * step.stage as i32], Rx(vx));
                if step.stage == 0 {
                    // A later setup may reuse a currently-live numeric home.
                    // Preserve the two destination homes as they stand at the
                    // first call so an atomic helper decline can restore the
                    // exact pre-batch state before `flush_exit` boxes aliases.
                    for (k, &arg) in step.args[1..].iter().enumerate() {
                        let ax = xh(&plan, arg);
                        dynasm!(ops ; movq [rsp + push3_rollback_off + 8 * k as i32], Rx(ax));
                    }
                }
                if step.stage == 2 {
                    for (k, &gb) in save_gprs.iter().enumerate() {
                        dynasm!(ops ; mov [rsp + psave_off + 8 * k as i32], Rq(gb));
                    }
                    for (k, &xi) in save_xmms.iter().enumerate() {
                        dynasm!(ops ; movq [rsp + psave_off + 32 + 8 * k as i32], Rx(xi));
                    }
                    let packed = step.pins[0] as u32
                        | ((step.pins[1] as u32) << 8)
                        | ((step.pins[2] as u32) << 16)
                        // Regression-only fault injection for the otherwise
                        // defensive atomic-decline path. It changes emitted
                        // code only in an explicitly marked child process.
                        | if std::env::var_os("ZIPP_TEST_FORCE_INT_PUSH3_DECLINE").is_some() {
                            1 << 31
                        } else {
                            0
                        };
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; lea rdx, [rsp + ta_base]
                        ; mov r8d, packed as i32
                        ; lea r9, [rsp + push3_vals_off]
                        ; mov rax, QWORD crate::vm::jit_array_push3_pinned as usize as i64
                        ; call rax
                    );
                    for (k, &gb) in save_gprs.iter().enumerate() {
                        dynasm!(ops ; mov Rq(gb), [rsp + psave_off + 8 * k as i32]);
                    }
                    for (k, &xi) in save_xmms.iter().enumerate() {
                        dynasm!(ops ; movq Rx(xi), [rsp + psave_off + 32 + 8 * k as i32]);
                    }
                    // Later receiver/argument setup may share a home with an
                    // earlier temporary. Replay from the first LoadGlobal, not
                    // from the first call, so the interpreter reconstructs all
                    // six setup registers after an atomic helper decline.
                    let replay = rip(step.first_ip);
                    let done = ops.new_dynamic_label();
                    dynasm!(ops
                        ; test rax, rax
                        ; jnz => done
                    );
                    for (k, &arg) in step.args[1..].iter().enumerate() {
                        let ax = xh(&plan, arg);
                        dynasm!(ops ; movq Rx(ax), [rsp + push3_rollback_off + 8 * k as i32]);
                    }
                    dynasm!(ops
                        ; mov DWORD [rsi], replay
                        ; jmp => flush_exit
                        ; => done
                    );
                }
                flag_cmp = None;
                lc = None;
            }
            // ── pinned dense-Array `arr.push(int)` (W20 M2) ── the ONE arm on
            // this tier that issues a call. Shape: identity-guard the receiver,
            // spill the planner-owned volatile registers into this frame's
            // call-save area, call `jit_array_push_pinned` (which appends and
            // rewrites THIS pin's `{obj_bits, base, len}`), restore, then take
            // the new length into the dst i64 home. A deopt sentinel — a frozen
            // / sparse / prototype-overridden array, or a receiver that is no
            // longer the pinned one — resumes the interpreter AT this ip, which
            // is sound because every early return in the helper happens before
            // it mutates anything.
            Instr::CallMethod { dst, arg_base, .. }
                if arr_push_pin(proto, ip, ta_plan).is_some() =>
            {
                let j = arr_push_pin(proto, ip, ta_plan).unwrap();
                let off = ta_base + 32 * j as i32;
                // Both the pushed value and the new-length dst must live in
                // xmm homes. `plan_region` types both Num and keeps the dst out
                // of `dead` (the append is a side effect, so the dead-code pass
                // must not skip the op), but a slot-materialized constant owns
                // no home at all -- decline rather than assume.
                if plan.slot_consts.contains_key(&arg_base)
                    || plan.slot_consts.contains_key(&dst)
                    || !matches!(plan.reg_home.get(&arg_base), Some(Home::Xmm(_)))
                    || !matches!(plan.reg_home.get(&dst), Some(Home::Xmm(_)))
                {
                    decline_emit("int-emit: arr.push operand/dst has no numeric home");
                    return None;
                }
                let vx = xh(&plan, arg_base);
                let d = xh(&plan, dst);
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                match ta_plan.pins[j].src {
                    TaPinSrc::Global(g) => dynasm!(ops ; mov rdx, [r12 + (g as i32) * 8]),
                    TaPinSrc::Reg(r) => dynasm!(ops ; mov rdx, [rbx + dreg(r)]),
                }
                dynasm!(ops
                    ; cmp rdx, [rsp + off]               // receiver identity vs snapshot
                    ; jne => deopt                       // (homes still intact here)
                );
                for (k, &gb) in save_gprs.iter().enumerate() {
                    dynasm!(ops ; mov [rsp + psave_off + 8 * k as i32], Rq(gb));
                }
                for (k, &xi) in save_xmms.iter().enumerate() {
                    dynasm!(ops ; movq [rsp + psave_off + 32 + 8 * k as i32], Rx(xi));
                }
                dynasm!(ops
                    ; movq r8, Rx(vx)                    // the raw i64 home value
                    ; mov rcx, rdi                       // vm
                    ; lea r9, [rsp + off]                // out: this pin's slot
                    ; mov rax, QWORD crate::vm::jit_array_push_pinned as usize as i64
                    ; call rax
                    // The sentinel goes into rcx BEFORE the restore, and the
                    // compare happens AFTER it. Two reasons, both load-bearing:
                    // r10/r11 are BOOL_GPRS and must not serve as scratch once
                    // they hold restored values again, and the deopt path jumps
                    // to `flush_exit`, which boxes every home -- so the homes
                    // have to be back before either branch is taken. rax (the
                    // result) and rcx survive the restores, which touch only
                    // r8..r11 and xmm2..xmm5.
                    ; mov rcx, QWORD SELF_CALL_DEOPT as i64
                );
                for (k, &gb) in save_gprs.iter().enumerate() {
                    dynasm!(ops ; mov Rq(gb), [rsp + psave_off + 8 * k as i32]);
                }
                for (k, &xi) in save_xmms.iter().enumerate() {
                    dynasm!(ops ; movq Rx(xi), [rsp + psave_off + 32 + 8 * k as i32]);
                }
                dynasm!(ops
                    ; cmp rax, rcx
                    ; je => deopt
                    ; movsxd rax, eax                    // Int payload → i64 home
                    ; movq Rx(d), rax
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], rip_at         // resume AT this ip
                    ; jmp => flush_exit
                    ; => done
                );
                copy_clobber(&mut lc, d);
                lc = None;
            }
            // ── pinned length ── `str.length` → the snapshot `units`, `arr.length`
            // → the snapshot `len`. Both pin families keep the length in the SAME
            // third slot, so one emitter serves both. Identity-guarded; a miss
            // deopts. Sound for an Array because the snapshot is declined outright
            // when the array carries an `arr_props` overlay (so `length` is exactly
            // `items.len()`), and nothing admitted on the integer tier can grow a
            // Vec — there are no calls, and dense-Array stores are not admitted.
            Instr::GetProp { dst, .. }
                if ta_plan.access.get(&ip).map_or(false, |&j| {
                    matches!(ta_plan.pins[j as usize].kind, STR_PIN_KIND)
                        || int_length_pin_kind(ta_plan.pins[j as usize].kind)
                }) =>
            {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let d = xh(&plan, dst);
                // W7: for a hoisted pin the identity is entry-guarded and the
                // length cannot change in-region, so this collapses to a bare
                // snapshot read — no guard, no deopt path. (A fully hoisted
                // STRING length never even reaches here — `hoist_len_ips`
                // skipped the op; this is the multi-def / on-a-branch / Array
                // length residue.)
                if plan.hoist_pins.contains(&(j as u8)) {
                    dynasm!(ops
                        ; mov rax, [rsp + off + 16]      // units / len
                        ; movq Rx(d), rax
                    );
                } else {
                    let deopt = ops.new_dynamic_label();
                    let done = ops.new_dynamic_label();
                    match ta_plan.pins[j].src {
                        TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                        TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                    }
                    dynasm!(ops
                        ; cmp rax, [rsp + off]           // receiver identity vs snapshot
                        ; jne => deopt
                        ; mov rax, [rsp + off + 16]      // units == str.length
                        ; movq Rx(d), rax
                        ; jmp => done
                        ; => deopt
                        ; mov DWORD [rsi], rip_at
                        ; jmp => flush_exit
                        ; => done
                    );
                }
                copy_clobber(&mut lc, d);
                lc = None;
            }
            // ── Math.imul(a, b) ── ToInt32 of the low 32 bits of the product. The
            // i64 homes' low 32 bits ARE ToInt32 of the operands, so `imul eax,ecx`
            // gives the low 32 of the product (signedness-agnostic); interpreted
            // signed it IS Math.imul → sign-extend to the home (fits i32, no guard).
            // MUST NOT route through the generic i64 Mul arm (it i53-guards a 64-bit
            // product and would box e.g. imul(0xFFFF,0xFFFF) as +4294836225 not -131071).
            Instr::MathOp {
                dst,
                arg_base,
                op: MathFn::Imul,
                argc: 2,
                ..
            } => {
                let d = xh(&plan, dst);
                let (ax, bx) = (xh(&plan, arg_base), xh(&plan, arg_base + 1));
                copy_clobber(&mut lc, d);
                dynasm!(ops
                    ; movq rax, Rx(ax)
                    ; movq rcx, Rx(bx)
                    ; imul eax, ecx
                    ; movsxd rax, eax
                    ; movq Rx(d), rax
                );
            }
            Instr::Return { .. } | Instr::ReturnUndefined => {
                dynasm!(ops ; mov DWORD [rsi], rip_at ; jmp => flush_exit);
            }
            // POST-PLAN hole: this region passed admission AND `plan_region`,
            // but this emitter has no arm for the op. Name the decline through
            // the planner's [decline-reason] channel — an unnamed fall to a
            // lower tier corrupts the tier-attribution reading (see the
            // regalloc emitter's twin arm). Behavior unchanged — still None.
            _ => {
                decline_emit(format_args!("int-emit-unhandled: {:?}", proto.code[ip]));
                return None;
            }
        }
        // ── B94 write-through (INT) ── a numeric def of a split receiver must
        // reach MEMORY (boxed) as well as its home, because `flush_exit`
        // deliberately skips the register and memory is what the interpreter
        // reads on any exit. Arms with an i53 guard already stored (`wt_pre`;
        // the guard resumes at ip+1 expecting the result flushed), and the
        // receiver LoadGlobal half stored the object itself — `wt_def_at` holds
        // that second exclusion for all three tiers.
        if !wt_pre {
            if let Some(d) = wt_def_at(proto, &plan, ip) {
                // A non-`>>>` Bitwise result and Math.imul are PROVABLY i32
                // (`>>>` yields a u32 that can exceed i32) — their write-through
                // is a branchless int-tag instead of the two-compare generic box.
                let known_i32 = matches!(
                    proto.code[ip],
                    Instr::Bitwise { op, .. } if !matches!(op, crate::bytecode::BitwiseOp::Ushr)
                ) || matches!(
                    proto.code[ip],
                    Instr::MathOp {
                        op: MathFn::Imul,
                        argc: 2,
                        ..
                    }
                );
                if emit_int_wt(&mut ops, &plan, d, known_i32) {
                    flag_cmp = None; // the boxing clobbered any fused flags
                }
            }
        }
    }

    // ── exit stubs ──
    for (target, label) in &exit_stubs {
        dynasm!(ops ; => *label ; mov DWORD [rsi], *target as i32 ; jmp => flush_exit);
    }

    // ── flush_exit ── box each i64 home back to an Int/double Value and write it
    // to the reg file / globals, restore, return. [rsi] holds the resume ip.
    dynasm!(ops ; => flush_exit);
    for &(r, x) in &plan.num_regs {
        if entry.is_scratch(r) {
            continue;
        }
        // A B94 split receiver is written through (boxed) at each numeric def,
        // so memory is already current; flushing its home here would overwrite
        // the receiver object at any exit taken inside the receiver range.
        if plan.split_recvs.contains(&r) || plan.write_through.contains(&r) {
            continue;
        }
        emit_int_box_from_home(&mut ops, x);
        dynasm!(ops ; mov [rbx + dreg(r)], rax);
    }
    for &(r, g) in &plan.bool_regs {
        if entry.is_scratch(r) {
            continue;
        }
        dynasm!(ops ; mov rax, QWORD BOOL_TAG as i64 ; or rax, Rq(g) ; mov [rbx + dreg(r)], rax);
    }
    for &(gi, x) in &plan.globs {
        emit_int_box_from_home(&mut ops, x);
        dynasm!(ops ; mov [r12 + (gi as i32) * 8], rax);
    }
    emit_region_restore_n(&mut ops, xmm_off, frame);

    // ── entry_bail ── a live-in wasn't Int-tagged; nothing computed, so restore
    // (NO flush) and resume at the header (interpreted).
    let rip_entry = rip(s);
    dynasm!(ops ; => entry_bail ; mov DWORD [rsi], rip_entry);
    emit_region_restore_n(&mut ops, xmm_off, frame);

    let buf = match ops.finalize() {
        Ok(b) => b,
        Err(_) => {
            decline_emit("int-emit: assembler finalize failed");
            return None;
        }
    };
    // W7 attribution: code-byte length with pins, so the hoist's size delta is
    // one grep away (run again under ZIPP_NO_GUARD_HOIST=1 and diff the line).
    if !ta_plan.pins.is_empty() && std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!(
            "[jit] INT region [{start},{end}] guard-hoist pins={}/{} len-fills={} code={}b",
            plan.hoist_pins.len(),
            ta_plan.pins.len(),
            plan.hoist_len_ips.len(),
            buf.len()
        );
        log_pinned_recvs("INT", start, end, proto, &plan);
    }
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn {
        _buf: buf,
        entry: entry_ptr,
        self_binding: None,
    })
}

#[cfg(test)]
mod virtual_home_boundary_tests {
    use super::*;

    #[test]
    fn xmm_consumer_rejects_the_first_virtual_home_id() {
        assert!(xmm_home_id_is_physical(HOME_XMM_FIRST));
        assert!(xmm_home_id_is_physical(HOME_XMM_LAST));
        assert!(!xmm_home_id_is_physical(HOME_XMM_LAST + 1));
        assert!(!xmm_home_id_is_physical(u8::MAX));
    }
}
