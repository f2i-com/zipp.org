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

/// An operand as this emitter reads it: a GPR home, or a hoisted-constant
/// immediate (always i32 — `LoadInt`'s payload and `LoadConst`-Int's payload
/// both are, which is what makes every imm form below encodable).
#[derive(Clone, Copy)]
enum Src {
    R(u8),
    I(i32),
}

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
/// NOTE r10: the xmm tier uses r10 as scratch in entry loads and the dense-array
/// tag check; every arm in THIS file scratches only rax/rcx/rdx, which is what
/// makes handing r8..r11 out as numeric homes sound.
fn gpr_home_map(
    proto: &FuncProto,
    plan: &RegionPlan,
    s: usize,
    e: usize,
    metered: bool,
) -> Result<(FxHashMap<u8, u8>, FxHashMap<u16, i32>, bool), bool> {
    // Out of scope: any plan feature whose write-through/flush interplay was
    // only ever proven against the other emitters.
    if !plan.split_recvs.is_empty()
        || !plan.write_through.is_empty()
        || !plan.split_recv_lg.is_empty()
        || !plan.dv_flag_elide.is_empty()
        || !plan.dv_flag_fuse.is_empty()
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
    if used.len() > pool.len() {
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!(
                "[jit] INT-GPR decline [{s},{e}]: {} homes > {} gprs",
                used.len(),
                pool.len()
            );
        }
        return Err(true); // the retryable decline — see `GprAttempt::PoolOverflow`
    }
    Ok((used.into_iter().zip(pool).collect(), hoist_c, inline_guards))
}

/// Entry load into a GPR home: the Value bits are in `rax`. Same admission as
/// `emit_int_entry_load` (Int tag sign-extends; an exactly-integral double in
/// [-2^53, 2^53], except -0.0, converts; everything else takes `entry_bail`).
/// Scratch is rcx/rdx/xmm0/xmm1 — NOT r10, which may itself be a numeric home
/// here.
fn emit_int_entry_load_gpr(
    ops: &mut dynasmrt::x64::Assembler,
    home: u8,
    entry_bail: dynasmrt::DynamicLabel,
) {
    let as_double = ops.new_dynamic_label();
    let store = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rdx, rax
        ; shr rdx, 48
        ; cmp edx, INT_TAG_HI as i32
        ; jne => as_double         // not Int-tagged — try the integral-double form
        ; movsxd Rq(home), eax     // sign-extend the i32 payload to i64
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
        ; test rcx, rcx            // reject -0.0 (same invariant as the xmm tier)
        ; jnz => store
        ; test rax, rax
        ; js => entry_bail
        ; => store
        ; mov Rq(home), rcx
        ; => done
    );
}

/// Box the i64 in GPR home `h` into a Value in `rax` (Int if it fits i32, else
/// a double — exact, |x| ≤ 2^53). The GPR twin of `emit_int_box_from_home`.
fn emit_int_box_from_gpr(ops: &mut dynasmrt::x64::Assembler, h: u8) {
    let big = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, Rq(h)
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
    h: u8,
    ip: usize,
    ip_slot: i32,
    inline: bool,
    flush_exit: dynasmrt::DynamicLabel,
) {
    let done = ops.new_dynamic_label();
    if inline {
        dynasm!(ops
            ; mov rax, Rq(h)
            ; mov rcx, QWORD TWO_POW_53
            ; add rax, rcx           // + 2^53 (no i64 overflow: |x| ≤ 2^54 here)
            ; mov rcx, QWORD TWO_POW_54
            ; cmp rax, rcx
            ; jbe => done
        );
    } else {
        dynasm!(ops
            ; mov rax, Rq(h)
            ; add rax, r13           // + 2^53 (no i64 overflow: |x| ≤ 2^54 here)
            ; cmp rax, r14           // 2^54
            ; jbe => done
        );
    }
    emit_store_ip(ops, ip_slot, (ip + 1) as i32); // resume AFTER this op (result flushed)
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
    meter: Option<crate::codegen::meter::Meter>,
) -> GprAttempt {
    let (s, e) = (start as usize, end as usize);
    let (map, hoist_c, inline_guards) = match gpr_home_map(proto, plan, s, e, meter.is_some()) {
        Ok(m) => m,
        Err(true) => return GprAttempt::PoolOverflow,
        Err(false) => return GprAttempt::OutOfScope,
    };
    // GPR home of raw xmm-index `x` / dst register `r` (dsts are never
    // hoisted-const, so their home is always mapped).
    let gx = |x: u8| map[&x];
    let g = |r: u16| gx(xh(plan, r));
    // Operand register `r` as this emitter reads it.
    let src = |r: u16| -> Src {
        match hoist_c.get(&r) {
            Some(&v) => Src::I(v),
            None => Src::R(g(r)),
        }
    };
    if std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!("[jit] INT region [{start},{end}] GPR homes engaged ({} homes)", map.len());
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
    // [TA snapshot slots 32·n_ta][resume-ip pointer 8]; the last slot frees rsi.
    let n_ta = ta_plan.pins.len() as i32;
    let frame = 40 + 32 * n_ta;
    let ta_base = 32i32;
    let ip_slot = 32 + 32 * n_ta;
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
    // the xmm tier; the bool loader's rdx scratch never aliases a home).
    for &(gi, x) in &plan.live_in_globs {
        dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]);
        emit_int_entry_load_gpr(&mut ops, gx(x), entry_bail);
    }
    for &(r, x) in &plan.live_in_regs {
        dynasm!(ops ; mov rax, [rbx + dreg(r)]);
        emit_int_entry_load_gpr(&mut ops, gx(x), entry_bail);
    }
    for &(r, gb) in &plan.live_in_bools {
        dynasm!(ops ; mov rax, [rbx + dreg(r)]);
        emit_bool_entry_load(&mut ops, gb, entry_bail);
    }
    // Hoisted constants: only a MAPPED home needs the fill (a home is mapped
    // when a non-hoisted reg — e.g. a Move alias — shares it; an unmapped
    // hoisted reg reads as an immediate everywhere instead).
    for &hip in &plan.hoist_ips {
        if let Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } = proto.code[hip] {
            if let Some(&h) = map.get(&xh(plan, dst)) {
                dynasm!(ops ; mov Rq(h), hoist_c[&dst]);
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
            let h = g(dst);
            dynasm!(ops ; mov Rq(h), [rsp + ta_base + 32 * j as i32 + 16]);
        }
    }
    // (No addint_imm_home / gpr_const fills: immediates encode directly here.)
    dynasm!(ops ; jmp => lbl(start, &in_region));

    // ── body ── same fusion/DCE/metering structure as the xmm emitter.
    let mut flag_cmp: Option<(usize, u16, Cmp)> = None;
    for ip in s..=e {
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
        if let Some(d) = writes_reg(&proto.code[ip]) {
            if plan.dead.contains(&d) {
                continue;
            }
        }
        let prev_flag = flag_cmp.take().filter(|_| !charged);
        match proto.code[ip] {
            Instr::LoadInt { dst, val } => {
                dynasm!(ops ; mov Rq(g(dst)), val);
            }
            Instr::LoadConst { dst, idx } => {
                // region admission guaranteed an Int constant (i32 payload).
                let v = proto.constants[idx as usize].bits() as u32 as i32;
                dynasm!(ops ; mov Rq(g(dst)), v);
            }
            Instr::Move { dst, src: sr } => match home(plan, dst) {
                Home::Xmm(dx) => {
                    let d = gx(dx);
                    match src(sr) {
                        Src::R(sg) if sg == d => flag_cmp = prev_flag, // nothing emitted
                        s_ => emit_src64(&mut ops, s_, d),
                    }
                }
                Home::Gpr(d) => {
                    let sg = gh(plan, sr);
                    dynasm!(ops ; mov Rq(d), Rq(sg));
                }
            },
            // A pinned-view receiver's LoadGlobal is a no-op (see the xmm arm).
            Instr::LoadGlobal { dst, .. } if plan.ta_recv_regs.contains(&dst) => {
                flag_cmp = prev_flag;
            }
            Instr::LoadGlobal { dst, idx } => {
                let (d, gg) = (g(dst), gx(plan.glob_home[&idx]));
                if d != gg {
                    dynasm!(ops ; mov Rq(d), Rq(gg));
                } else {
                    flag_cmp = prev_flag;
                }
            }
            Instr::StoreGlobal { idx, src: sr }
            | Instr::StoreGlobalStrict { idx, src: sr }
            | Instr::StoreGlobalResolved { idx, src: sr } => {
                let gg = gx(plan.glob_home[&idx]);
                match src(sr) {
                    Src::R(sg) if sg == gg => flag_cmp = prev_flag,
                    s_ => emit_src64(&mut ops, s_, gg),
                }
            }
            Instr::Add { dst, a, b } | Instr::Sub { dst, a, b } => {
                let add = matches!(proto.code[ip], Instr::Add { .. });
                let d = g(dst);
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
                if !plan.elide_guard.contains(&ip) {
                    emit_i53_guard_gpr(&mut ops, d, ip, ip_slot, inline_guards, flush_exit);
                }
            }
            Instr::Mul { dst, a, b } => {
                let d = g(dst);
                if let Some(&(val_reg, shift)) = plan.mul_shift.get(&ip) {
                    // Guard-elided multiply by a constant power of two.
                    let vg = g(val_reg);
                    if d != vg {
                        dynasm!(ops ; mov Rq(d), Rq(vg));
                    }
                    dynasm!(ops ; shl Rq(d), shift as i8);
                } else if plan.elide_guard.contains(&ip) {
                    emit_src64(&mut ops, src(a), 0); // rax
                    match src(b) {
                        Src::R(bg) => dynasm!(ops ; imul rax, Rq(bg)),
                        Src::I(bi) => dynasm!(ops ; imul rax, rax, bi),
                    }
                    dynasm!(ops ; mov Rq(d), rax);
                } else {
                    // Same i64-overflow / i53 split as the xmm arm.
                    let ovf = ops.new_dynamic_label();
                    let done = ops.new_dynamic_label();
                    emit_src64(&mut ops, src(a), 0); // rax
                    match src(b) {
                        Src::R(bg) => dynasm!(ops ; imul rax, Rq(bg)),
                        Src::I(bi) => dynasm!(ops ; imul rax, rax, bi),
                    }
                    dynasm!(ops
                        ; jo => ovf            // i64 overflow → redo in interp at THIS ip
                        ; mov Rq(d), rax
                        ; jmp => done
                        ; => ovf
                    );
                    emit_store_ip(&mut ops, ip_slot, ip as i32); // dst not written
                    dynasm!(ops
                        ; jmp => flush_exit
                        ; => done
                    );
                    emit_i53_guard_gpr(&mut ops, d, ip, ip_slot, inline_guards, flush_exit);
                }
            }
            Instr::Mod { dst, a, b } => {
                // Same semantics and bails as the xmm arm (see its comment).
                let d = g(dst);
                let zbail = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                let store = ops.new_dynamic_label();
                emit_src64(&mut ops, src(a), 0); // rax = dividend
                emit_src64(&mut ops, src(b), 1); // rcx = divisor
                dynasm!(ops
                    ; test rcx, rcx
                    ; jz => zbail              // % 0 → NaN → redo in interp
                    ; cqo                       // sign-extend rax into rdx:rax
                    ; idiv rcx                  // rdx = remainder
                    ; test rdx, rdx
                    ; jnz => store
                );
                emit_src64(&mut ops, src(a), 1); // zero rem from a negative dividend is -0
                dynasm!(ops
                    ; test rcx, rcx
                    ; js => zbail
                    ; => store
                    ; mov Rq(d), rdx
                    ; jmp => done
                    ; => zbail
                );
                emit_store_ip(&mut ops, ip_slot, ip as i32); // resume at THIS op (dst unwritten)
                dynasm!(ops
                    ; jmp => flush_exit
                    ; => done
                );
            }
            Instr::AddInt { dst, a, imm, .. } => {
                let d = g(dst);
                match src(a) {
                    Src::R(ag) => {
                        if imm == 0 {
                            if d != ag {
                                dynasm!(ops ; mov Rq(d), Rq(ag));
                            } else {
                                flag_cmp = prev_flag;
                            }
                        } else {
                            if d != ag {
                                dynasm!(ops ; mov Rq(d), Rq(ag));
                            }
                            dynasm!(ops ; add Rq(d), imm); // sign-extended imm32 == i64 add
                            if !plan.elide_guard.contains(&ip) {
                                emit_i53_guard_gpr(&mut ops, d, ip, ip_slot, inline_guards, flush_exit);
                            }
                        }
                    }
                    Src::I(ai) => {
                        // Fold const + imm (both i32 — cannot overflow i64).
                        let v = ai as i64 + imm as i64;
                        dynasm!(ops ; mov rax, QWORD v ; mov Rq(d), rax);
                    }
                }
            }
            Instr::Neg { dst, a } => {
                // -0 is not representable in an i64 home — bail on a zero operand
                // (same as the xmm arm; Neg is pure, resuming AT this ip is sound).
                let d = g(dst);
                let nonzero = ops.new_dynamic_label();
                emit_src64(&mut ops, src(a), 0); // rax
                dynasm!(ops
                    ; test rax, rax
                    ; jnz => nonzero
                );
                emit_store_ip(&mut ops, ip_slot, ip as i32);
                dynasm!(ops
                    ; jmp => flush_exit
                    ; => nonzero
                    ; neg rax
                    ; mov Rq(d), rax
                );
                if !plan.elide_guard.contains(&ip) {
                    emit_i53_guard_gpr(&mut ops, d, ip, ip_slot, inline_guards, flush_exit);
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
                emit_icmp_flags_gpr(&mut ops, src(a), src(b));
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
                emit_icmp_flags_gpr(&mut ops, src(a), src(b));
                dynasm!(ops ; jge => t); // !(a<b), SIGNED
            }
            Instr::JumpIfNotLe { a, b, target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                emit_icmp_flags_gpr(&mut ops, src(a), src(b));
                dynasm!(ops ; jg => t); // !(a<=b), SIGNED
            }
            // ── pinned element read ── same guards and deopt contract as the
            // xmm arm; only the home moves differ (and the dense-array tag
            // check scratches rdx, not r10 — rdx's base value is dead by then).
            Instr::GetIndex { dst, key, .. } => {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let d = g(dst);
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
                emit_src64(&mut ops, src(key), 1);       // rcx = index (i64, integral)
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
                        ; movsxd rax, eax                // Int payload, sign-extended
                    );
                } else {
                    dynasm!(ops
                        ; movsxd rax, DWORD [rdx + rcx * 4] // sign-extend i32 element
                    );
                }
                dynasm!(ops
                    ; mov Rq(d), rax
                    ; jmp => done
                    ; => deopt
                );
                emit_store_ip(&mut ops, ip_slot, ip as i32); // resume AT this ip
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
                emit_src64(&mut ops, src(key), 1);       // rcx = index
                dynasm!(ops
                    ; cmp rcx, [rsp + off + 16]
                    ; jae => deopt
                    ; mov rdx, [rsp + off + 8]
                );
                emit_src64(&mut ops, src(val), 0);       // rax = value i64
                dynasm!(ops
                    ; mov DWORD [rdx + rcx * 4], eax     // store low 32 (== ToInt32(v))
                    ; jmp => done
                    ; => deopt
                );
                emit_store_ip(&mut ops, ip_slot, ip as i32);
                dynasm!(ops
                    ; jmp => flush_exit
                    ; => done
                );
            }
            Instr::Bitwise { dst, a, b, op } => {
                use crate::bytecode::BitwiseOp as B;
                // THE point of this emitter: the homes are already GPRs, so the
                // int32-lane op needs no xmm↔gpr transfers at all. The homes
                // hold sign-extended i64s: the low 32 bits ARE ToInt32 (unsigned
                // read: ToUint32). Shifts stage the count in cl (x86 masks it to
                // 5 bits, matching JS's `& 31`) or take a hoisted-const count as
                // an immediate. `>>>`'s 32-bit write zero-extends (a u32, within
                // ±2^53 — exit boxing picks Int vs double).
                let d = g(dst);
                match src(a) {
                    Src::R(ag) => dynasm!(ops ; mov eax, Rd(ag)),
                    Src::I(ai) => dynasm!(ops ; mov eax, ai),
                }
                match (op, src(b)) {
                    (B::And, Src::R(bg)) => dynasm!(ops ; and eax, Rd(bg)),
                    (B::And, Src::I(bi)) => dynasm!(ops ; and eax, bi),
                    (B::Or, Src::R(bg)) => dynasm!(ops ; or eax, Rd(bg)),
                    (B::Or, Src::I(bi)) => dynasm!(ops ; or eax, bi),
                    (B::Xor, Src::R(bg)) => dynasm!(ops ; xor eax, Rd(bg)),
                    (B::Xor, Src::I(bi)) => dynasm!(ops ; xor eax, bi),
                    (B::Shl, Src::R(bg)) => dynasm!(ops ; mov ecx, Rd(bg) ; shl eax, cl),
                    (B::Shl, Src::I(bi)) => dynasm!(ops ; shl eax, (bi & 31) as i8),
                    (B::Shr, Src::R(bg)) => dynasm!(ops ; mov ecx, Rd(bg) ; sar eax, cl),
                    (B::Shr, Src::I(bi)) => dynasm!(ops ; sar eax, (bi & 31) as i8),
                    (B::Ushr, Src::R(bg)) => dynasm!(ops ; mov ecx, Rd(bg) ; shr eax, cl),
                    (B::Ushr, Src::I(bi)) => dynasm!(ops ; shr eax, (bi & 31) as i8),
                }
                if matches!(op, B::Ushr) {
                    dynasm!(ops ; mov Rd(d), eax); // 32-bit write zero-extends
                } else {
                    dynasm!(ops ; movsxd Rq(d), eax);
                }
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
                let d = g(dst);
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
                emit_src64(&mut ops, src(arg_base), 1);  // rcx = index
                dynasm!(ops
                    ; cmp rcx, [rsp + off + 16]          // unsigned: i < units
                    ; jae => deopt                       // OOB → deopt (interp yields NaN)
                    ; mov rdx, [rsp + off + 8]           // pinned bytes base
                    ; movzx eax, BYTE [rdx + rcx]        // ASCII code unit, 0..255
                    ; mov Rq(d), rax
                    ; jmp => done
                    ; => deopt
                );
                emit_store_ip(&mut ops, ip_slot, ip as i32); // resume AT this ip
                dynasm!(ops
                    ; jmp => flush_exit
                    ; => done
                );
            }
            // ── pinned length ── str.length / arr.length from the snapshot.
            Instr::GetProp { dst, .. }
                if ta_plan.access.get(&ip).map_or(false, |&j| {
                    matches!(ta_plan.pins[j as usize].kind, STR_PIN_KIND | ARR_INT_PIN_KIND)
                }) =>
            {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let d = g(dst);
                // W7: a hoisted pin's length read collapses to a bare snapshot
                // load (identity entry-guarded; length region-invariant). The
                // fully hoisted STRING case never reaches here (body op
                // skipped); this is the multi-def / on-a-branch / Array residue.
                if plan.hoist_pins.contains(&(j as u8)) {
                    dynasm!(ops ; mov Rq(d), [rsp + off + 16]);
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
                        ; mov rax, [rsp + off + 16]      // units / len
                        ; mov Rq(d), rax
                        ; jmp => done
                        ; => deopt
                    );
                    emit_store_ip(&mut ops, ip_slot, ip as i32);
                    dynasm!(ops
                        ; jmp => flush_exit
                        ; => done
                    );
                }
            }
            // ── Math.imul ── low 32 of the product, sign-extended (see xmm arm).
            Instr::MathOp { dst, arg_base, op: MathFn::Imul, argc: 2, .. } => {
                let d = g(dst);
                match src(arg_base) {
                    Src::R(ag) => dynasm!(ops ; mov eax, Rd(ag)),
                    Src::I(ai) => dynasm!(ops ; mov eax, ai),
                }
                match src(arg_base + 1) {
                    Src::R(bg) => dynasm!(ops ; imul eax, Rd(bg)),
                    Src::I(bi) => dynasm!(ops ; imul eax, eax, bi),
                }
                dynasm!(ops ; movsxd Rq(d), eax);
            }
            Instr::Return { .. } | Instr::ReturnUndefined => {
                emit_store_ip(&mut ops, ip_slot, ip as i32);
                dynasm!(ops ; jmp => flush_exit);
            }
            // POST-PLAN hole — same contract as the xmm emitter's twin arm.
            _ => {
                decline_emit(format_args!("int-gpr-emit-unhandled: {:?}", proto.code[ip]));
                return GprAttempt::OutOfScope;
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
    // applies. (No split/wt registers exist in this mode — `gpr_home_map`
    // requires those sets empty.)
    dynasm!(ops ; => flush_exit);
    for &(r, x) in &plan.num_regs {
        match map.get(&x) {
            Some(&h) => emit_int_box_from_gpr(&mut ops, h),
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
        emit_int_box_from_gpr(&mut ops, gx(x));
        dynasm!(ops ; mov [r12 + (gi as i32) * 8], rax);
    }
    emit_gpr_region_restore(&mut ops, frame);

    // ── entry_bail ── a live-in wasn't admissible; nothing computed, so
    // restore (NO flush) and resume at the header (interpreted).
    dynasm!(ops ; => entry_bail);
    emit_store_ip(&mut ops, ip_slot, start as i32);
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
    }
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    GprAttempt::Emitted(JitFn { _buf: buf, entry: entry_ptr, self_binding: None })
}

/// Compare flags for the GPR/immediate operand forms (SIGNED, i64 — an i32
/// immediate sign-extends, exactly the i64 the home would hold).
fn emit_icmp_flags_gpr(ops: &mut dynasmrt::x64::Assembler, a: Src, b: Src) {
    match (a, b) {
        (Src::R(ag), Src::R(bg)) => dynasm!(ops ; cmp Rq(ag), Rq(bg)),
        (Src::R(ag), Src::I(bi)) => dynasm!(ops ; cmp Rq(ag), bi),
        (Src::I(ai), Src::R(bg)) => dynasm!(ops ; mov rax, ai ; cmp rax, Rq(bg)),
        (Src::I(ai), Src::I(bi)) => dynasm!(ops ; mov rax, ai ; cmp rax, bi),
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
