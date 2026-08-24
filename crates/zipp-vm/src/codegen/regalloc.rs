// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Same-binary escape hatch for the guarded pure cyclic field-read reducer.
/// Read only while compiling a hot loop; there is no per-iteration switch cost.
pub(crate) fn field_read_stream_enabled() -> bool {
    std::env::var_os("ZIPP_NO_FIELD_READ_STREAM").is_none()
}

fn field_sum_stream_enabled() -> bool {
    std::env::var_os("ZIPP_NO_FIELD_SUM_STREAM").is_none()
}

/// Codegen-side copy of the helper's exact bytecode screen.  Returning false
/// merely omits the prefix; the helper repeats the full recognition before it
/// can commit, so this function is a profitability/admission filter, never a
/// correctness assumption.
pub(crate) fn field_cyclic_read_stream_shape(proto: &FuncProto, start: usize, end: usize) -> bool {
    use crate::bytecode::BitwiseOp;
    if end.checked_sub(start) != Some(16) || end + 1 >= proto.code.len() {
        return false;
    }
    let c = &proto.code;
    let limit = match &c[start] {
        Instr::LoadGlobal { dst, .. } => *dst,
        Instr::UpvalGet { dst, idx } if (*idx as usize) < proto.upvalues.len() => *dst,
        _ => return false,
    };
    let i = match &c[start + 1] {
        Instr::JumpIfNotLt { a, b, target } if *b == limit && *target as usize == end + 1 => *a,
        _ => return false,
    };
    let (elem, k) = match &c[start + 2] {
        Instr::GetIndex { dst, key, .. } => (*dst, *key),
        _ => return false,
    };
    let (field, sum) = match (&c[start + 3], &c[start + 4]) {
        (
            Instr::GetProp {
                dst: field, obj, ..
            },
            Instr::Add {
                dst: add,
                a: sum,
                b,
            },
        ) if *obj == elem && b == field => (*add, *sum),
        _ => return false,
    };
    let zero = match &c[start + 5] {
        Instr::LoadInt { dst, val: 0 } => *dst,
        _ => return false,
    };
    if !matches!(&c[start + 6], Instr::Bitwise { dst, a, b, op: BitwiseOp::Or }
        if *dst == sum && *a == field && *b == zero)
        || !matches!(&c[start + 7], Instr::Move { src, .. } if *src == sum)
        || !matches!(&c[start + 8], Instr::AddInt { dst, a, imm: 1, upd: true } if *dst == k && *a == k)
        || !matches!(&c[start + 9], Instr::Move { src, .. } if *src == k)
    {
        return false;
    }
    let (flag, n) = match &c[start + 10] {
        Instr::Eq { dst, a, b } if *a == k => (*dst, *b),
        _ => return false,
    };
    matches!(&c[start + 11], Instr::JumpIfFalse { cond, target }
            if *cond == flag && *target as usize == start + 14)
        && matches!(&c[start + 12], Instr::LoadInt { dst, val: 0 } if *dst == k)
        && matches!(&c[start + 13], Instr::Move { src, .. } if *src == k)
        && matches!(&c[start + 14], Instr::AddInt { dst, a, imm: 1, upd: true } if *dst == i && *a == i)
        && matches!(&c[start + 15], Instr::Move { src, .. } if *src == i)
        && matches!(&c[start + 16], Instr::Jump { target } if *target as usize == start)
        && limit != sum
        && n != sum
}

/// Script-body sibling: `sum = (sum + objects[i & mask].field) | 0` with a
/// power-of-two mask and all loop-carried state in globals. Runtime object,
/// descriptor and pure-accessor checks remain in the helper.
fn field_mask_read_stream_shape(proto: &FuncProto, start: usize, end: usize) -> bool {
    use crate::bytecode::BitwiseOp;
    if end.checked_sub(start) != Some(17) || end + 1 >= proto.code.len() {
        return false;
    }
    let c = &proto.code;
    let (i_head, i_global) = match &c[start] {
        Instr::LoadGlobal { dst, idx } => (*dst, *idx),
        _ => return false,
    };
    let (limit, limit_global) = match &c[start + 1] {
        Instr::LoadGlobal { dst, idx } => (*dst, *idx),
        _ => return false,
    };
    if !matches!(&c[start + 2], Instr::JumpIfNotLt { a, b, target }
            if *a == i_head && *b == limit && *target as usize == end + 1)
    {
        return false;
    }
    let (sum, sum_global) = match &c[start + 3] {
        Instr::LoadGlobal { dst, idx } => (*dst, *idx),
        _ => return false,
    };
    let (array, array_global) = match &c[start + 4] {
        Instr::LoadGlobal { dst, idx } => (*dst, *idx),
        _ => return false,
    };
    let i_index = match &c[start + 5] {
        Instr::LoadGlobal { dst, idx } if *idx == i_global => *dst,
        _ => return false,
    };
    let (mask_reg, mask) = match &c[start + 6] {
        Instr::LoadInt { dst, val }
            if *val >= 0 && ((*val as u32).wrapping_add(1)).is_power_of_two() => (*dst, *val),
        _ => return false,
    };
    let index = match &c[start + 7] {
        Instr::Bitwise { dst, a, b, op: BitwiseOp::And }
            if *a == i_index && *b == mask_reg => *dst,
        _ => return false,
    };
    let receiver = match &c[start + 8] {
        Instr::GetIndex { dst, obj, key } if *obj == array && *key == index => *dst,
        _ => return false,
    };
    let field = match &c[start + 9] {
        Instr::GetProp { dst, obj, .. } if *obj == receiver => *dst,
        _ => return false,
    };
    let add = match &c[start + 10] {
        Instr::Add { dst, a, b } if *a == sum && *b == field => *dst,
        _ => return false,
    };
    let zero = match &c[start + 11] {
        Instr::LoadInt { dst, val: 0 } => *dst,
        _ => return false,
    };
    let reduced = match &c[start + 12] {
        Instr::Bitwise { dst, a, b, op: BitwiseOp::Or } if *a == add && *b == zero => *dst,
        _ => return false,
    };
    if !matches!(&c[start + 13],
        Instr::StoreGlobalStrict { idx, src } | Instr::StoreGlobal { idx, src }
            if *idx == sum_global && *src == reduced)
    {
        return false;
    }
    let i_tail = match &c[start + 14] {
        Instr::LoadGlobal { dst, idx } if *idx == i_global => *dst,
        _ => return false,
    };
    matches!(&c[start + 15], Instr::AddInt { dst, a, imm: 1, upd: true }
            if *dst == i_tail && *a == i_tail)
        && matches!(&c[start + 16], Instr::StoreGlobalResolved { idx, src }
            if *idx == i_global && *src == i_tail)
        && matches!(&c[start + 17], Instr::Jump { target } if *target as usize == start)
        && array_global != sum_global
        && array_global != i_global
        && array_global != limit_global
        && sum_global != i_global
        && sum_global != limit_global
        && i_global != limit_global
        && mask >= 0
}

fn global_field_sum_stream_shape(proto: &FuncProto, start: usize, end: usize) -> bool {
    use crate::bytecode::BitwiseOp;
    if end.checked_sub(start).is_none_or(|n| n < 13) || end + 1 >= proto.code.len() {
        return false;
    }
    let c = &proto.code;
    let (i_head, i_global) = match &c[start] {
        Instr::LoadGlobal { dst, idx } => (*dst, *idx),
        _ => return false,
    };
    let (limit, limit_global) = match &c[start + 1] {
        Instr::LoadGlobal { dst, idx } => (*dst, *idx),
        _ => return false,
    };
    if !matches!(&c[start + 2], Instr::JumpIfNotLt { a, b, target }
            if *a == i_head && *b == limit && *target as usize == end + 1)
    {
        return false;
    }
    let (mut acc, sum_global) = match &c[start + 3] {
        Instr::LoadGlobal { dst, idx } => (*dst, *idx),
        _ => return false,
    };
    if sum_global == i_global || sum_global == limit_global || i_global == limit_global {
        return false;
    }
    let mut cursor = start + 4;
    let mut terms = 0usize;
    while cursor + 6 < end && terms < 8 {
        let receiver = match &c[cursor] {
            Instr::LoadGlobal { dst, .. } => *dst,
            _ => break,
        };
        let field = match &c[cursor + 1] {
            Instr::GetProp { dst, obj, .. } if *obj == receiver => *dst,
            _ => break,
        };
        let next_acc = match &c[cursor + 2] {
            Instr::Add { dst, a, b } if *a == acc && *b == field => *dst,
            _ => break,
        };
        acc = next_acc;
        terms += 1;
        cursor += 3;
    }
    if terms == 0 || cursor + 6 != end {
        return false;
    }
    let zero = match &c[cursor] {
        Instr::LoadInt { dst, val: 0 } => *dst,
        _ => return false,
    };
    let reduced = match &c[cursor + 1] {
        Instr::Bitwise { dst, a, b, op: BitwiseOp::Or } if *a == acc && *b == zero => *dst,
        _ => return false,
    };
    if !matches!(&c[cursor + 2],
        Instr::StoreGlobalStrict { idx, src } | Instr::StoreGlobal { idx, src }
            if *idx == sum_global && *src == reduced)
    {
        return false;
    }
    let i_tail = match &c[cursor + 3] {
        Instr::LoadGlobal { dst, idx } if *idx == i_global => *dst,
        _ => return false,
    };
    matches!(&c[cursor + 4], Instr::AddInt { dst, a, imm: 1, upd: true }
            if *dst == i_tail && *a == i_tail)
        && matches!(&c[cursor + 5], Instr::StoreGlobalResolved { idx, src }
            if *idx == i_global && *src == i_tail)
        && matches!(&c[cursor + 6], Instr::Jump { target } if *target as usize == start)
}

fn field_read_stream_shape(proto: &FuncProto, start: usize, end: usize) -> bool {
    field_cyclic_read_stream_shape(proto, start, end)
        || field_mask_read_stream_shape(proto, start, end)
        || (field_sum_stream_enabled() && global_field_sum_stream_shape(proto, start, end))
}

/// ── W20 ── the REGISTER tier's inline-cache probe for `GetProp`.
///
/// This is not `emit_ic_probe`, and the reason is the whole difficulty of the
/// mechanism. That probe clobbers `r8d/r9/r10/r11d` — which on this tier are
/// [`BOOL_GPRS`], the planner's register file for `Bool` homes, live across the
/// backedge and reloaded by nothing. Using it here would silently corrupt a JS
/// boolean for the rest of the region: the exact W14/W16 defect class the
/// register contract on `BOOL_GPRS` was written for, three times a silent wrong
/// answer.
///
/// So this one is written to a tighter contract:
///   * **reads** `rbx` (frame window), `r13` (heap version array), `r14` (IC
///     table base) — all pinned for the run;
///   * **clobbers** `rax`, `rcx`, `rdx` and 16 bytes of frame scratch at
///     `probe_off`, and NOTHING else. No `BOOL_GPRS`, no xmm home;
///   * on a hit, leaves the property's `Value` bits in `rax` and falls through;
///   * on any miss it calls `jit_get_prop_miss` (spilling the volatile homes
///     around the call, see below) and falls through with the answer in `rax`,
///     or jumps to `deopt`.
///
/// The 16 bytes of scratch buy the fourth and fifth registers a hop-walking
/// 8-way probe needs. `[probe_off]` holds the receiver bits (re-loaded per way
/// so `rax` is free inside the hop walk) and `[probe_off + 8]` holds the way
/// counter, re-used as the hop counter once the way loop can no longer be
/// resumed (a hop mismatch commits to the miss path: identity + receiver version
/// already matched, so no other way can answer for this receiver).
///
/// ── the miss must CALL, and this is a measured claim ──
/// The wave-20 map specified a DEOPT here, gated on a plan-time zero-miss check.
/// That cannot work. `Jit::reserve_ic_sites` hands every fresh compile eight
/// ZEROED ways and `Jit::set_ic` — the only writer of a way anywhere in the
/// engine — is reachable only from the miss helpers. A probe that never calls
/// one never fills a way, misses on every access for the life of the region, and
/// evicts it. `ZIPP_BOXREF_MISS=deopt` emits that form so the claim is a
/// measurement rather than an argument; the default is the call.
///
/// `jit_get_prop_miss` is the ONLY helper this tier may call, and the reason it
/// is safe is worth stating: it never runs user code (an accessor returns
/// `PROP_VIA_IC`, an exotic receiver `SELF_CALL_DEOPT` — both deopt here), it
/// never touches `vm.heap`, and it resizes neither the version array nor the IC
/// table. So `r13`, `r14`, every pinned-Array snapshot and every `items` base
/// survive it unchanged — which is why there is no re-fetch, exactly as on the
/// memory tier, whose own probe skips the re-fetch on this same path.
///
/// What does NOT survive is the volatile register file: win64 lets a callee
/// clobber `rax/rcx/rdx/r8..r11` and `xmm0..xmm5`, and homes 2..5 plus all four
/// `BOOL_GPRS` live there. They are spilled to the frame around the call —
/// unconditionally, all eight, because "which home did I forget" is not a
/// question a reviewer should have to answer — and restored on BOTH exits,
/// including the deopt exit, since `flush_exit` writes every home back.
#[allow(clippy::too_many_arguments)]
fn emit_regalloc_ic_probe(
    ops: &mut dynasmrt::x64::Assembler,
    heap: &HeapHelpers,
    ic_site: u32,
    obj: u16,
    name: u32,
    probe_off: i32,
    spill_off: i32,
    deopt: dynasmrt::DynamicLabel,
) {
    let off = (ic_site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
    let packed = ((heap.func_id as u64) << 32) | name as u64;
    let probe = ops.new_dynamic_label();
    let next = ops.new_dynamic_label();
    let hit = ops.new_dynamic_label();
    let hop = ops.new_dynamic_label();
    let miss = ops.new_dynamic_label();
    let got = ops.new_dynamic_label();
    let end = off + (JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
    dynasm!(ops
        ; lea rcx, [r14 + off]                 // way cursor
        ; lea rdx, [r14 + end]                 // one past the last way
        ; => probe
        // The receiver is re-loaded per way rather than parked in scratch: it is
        // an L1 hit, and it keeps `rax` free for the hop walk (which clobbers it
        // and can no longer return here -- see the `jne => miss` below).
        ; mov rax, [rbx + dreg(obj)]           // receiver bits (the box slot)
        ; cmp rax, [rcx]                       // identity (an empty 0 never matches)
        ; jne => next
        ; mov eax, eax                         // receiver heap index = low 32
        ; mov eax, [r13 + rax * 4]             // live receiver version
        ; cmp eax, [rcx + 16]
        ; jne => next
        ; mov eax, [rcx + 20]
        ; shr eax, 24                          // tag byte: hop count | acc tags
        ; jz => hit                            // 0 hops, untagged ⇒ own data
        ; test eax, 0xC0                       // IC_ACC_TAG | IC_ACC_BAKED (post-shr)
        ; jnz => miss                          // an accessor way is not ours
        // ── CHAIN way ── identity AND receiver version already matched, so this
        // way is THE answer for this receiver or there is none: a stale hop goes
        // straight to the miss path rather than trying the rest. That is what
        // frees `rdx` (the way-loop bound) to become the hop cursor and `rax` to
        // become the version scratch, and it leaves the 8 bytes of frame scratch
        // carrying only the hop counter.
        ; and eax, 0x3F                        // hop count, 1..=JIT_IC_MAX_HOPS
        ; mov [rsp + probe_off], eax
        ; lea rdx, [rcx + 24]                  // hop cursor
        ; => hop
        ; mov eax, [rdx]                       // hop heap index
        ; mov eax, [r13 + rax * 4]             // live hop version
        ; cmp eax, [rdx + 4]
        ; jne => miss                          // chain moved: no other way answers
        ; add rdx, 8
        ; dec DWORD [rsp + probe_off]
        ; jnz => hop
        ; => hit
        // `slot_nhops` packs the slot in the low 24 bits, so masking is part of
        // reading a slot at all.
        ; mov eax, [rcx + 20]
        ; and eax, 0x00FF_FFFF
        ; mov rdx, [rcx + 8]                   // holder vals_ptr
        ; mov rax, [rdx + rax * 8]             // vals[slot] (CALL-FREE)
        ; jmp => got
        ; => next
        ; add rcx, JIT_IC_STRIDE as i32
        ; cmp rcx, rdx
        ; jb => probe
        ; => miss
    );
    if boxref_miss_deopts() {
        // The map's form, kept for measurement only — see the doc above.
        dynasm!(ops ; jmp => deopt);
    } else {
        let restore_deopt = ops.new_dynamic_label();
        emit_probe_spill(ops, spill_off, true);
        dynasm!(ops
            ; mov rcx, rdi                     // vm
            ; mov rdx, [rbx + dreg(obj)]       // receiver bits
            ; mov r8d, ic_site as i32          // site_idx
            ; mov r9, QWORD packed as i64      // (func_id<<32)|name_idx
            ; mov rax, QWORD heap.get_prop_miss as i64
            ; call rax
            ; mov rcx, QWORD SELF_CALL_DEOPT as i64
            ; cmp rax, rcx
            ; je => restore_deopt
            ; mov rcx, QWORD PROP_VIA_IC as i64
            ; cmp rax, rcx                     // accessor/class ⇒ interpreter
            ; je => restore_deopt
            ; mov [rsp + probe_off], rax       // park the answer across the reloads
        );
        emit_probe_spill(ops, spill_off, false);
        dynasm!(ops
            ; mov rax, [rsp + probe_off]
            ; jmp => got
            ; => restore_deopt
        );
        // `flush_exit` writes every home back, so the homes must be whole before
        // the deopt jump — not only before the fall-through.
        emit_probe_spill(ops, spill_off, false);
        dynasm!(ops ; jmp => deopt);
    }
    dynasm!(ops ; => got);
}

/// Spill (`save`) or reload the volatile register homes around the register
/// tier's one permitted call. `xmm2..xmm5` are the volatile half of the numeric
/// home pool ([`HOME_XMM_FIRST`] is 2; xmm6..15 are saved by the prologue AND
/// callee-saved across a win64 call) and `r8..r11` are [`BOOL_GPRS`] in full.
///
/// All eight, always, whether or not the plan allocated them: the area is
/// already reserved, the path is the cold one, and an unconditional sequence has
/// no "is this home live here" question for a reviewer — or for the next arm
/// that reaches for a register.
fn emit_probe_spill(ops: &mut dynasmrt::x64::Assembler, spill_off: i32, save: bool) {
    for k in 0..4i32 {
        let x = 2 + k as u8;
        if save {
            dynasm!(ops ; movdqu [rsp + spill_off + k * 16], Rx(x));
        } else {
            dynasm!(ops ; movdqu Rx(x), [rsp + spill_off + k * 16]);
        }
    }
    for (i, &g) in BOOL_GPRS.iter().enumerate() {
        let o = spill_off + 64 + i as i32 * 8;
        if save {
            dynasm!(ops ; mov [rsp + o], Rq(g));
        } else {
            dynasm!(ops ; mov Rq(g), [rsp + o]);
        }
    }
}

/// The one accessor body the BOXREF tier can execute without running user code:
///
/// ```text
/// get v() { return this.field; }
/// ```
///
/// The accessor planner has already proved that this is a plain, non-arrow
/// getter and that `field` is an own DATA slot of the guarded receiver. Keep the
/// emitter gate exact anyway: widening it here would silently skip an accessor
/// body's effects. The returned tuple is the accessor-function address/bits
/// guard plus the baked DATA-field slot.
fn boxref_passthrough_getter(shape: &MethodInlineShape) -> Option<(u64, u64, u32)> {
    if shape.param_count != 0
        || !shape.supers.is_empty()
        || shape.method_slot.is_some()
        || shape.proto_method.is_some()
    {
        return None;
    }
    let name = match shape.body.as_slice() {
        [Instr::GetProp { dst, obj: 0, name }, Instr::Return { src }] if dst == src => *name,
        _ => return None,
    };
    let slot = *shape.field_slots.get(&name)?;
    let (acc_addr, acc_bits) = shape.own_acc?;
    Some((acc_addr, acc_bits, slot))
}

/// Same-binary A/B switch for the exact BOXREF own-getter prefix. Plan-time
/// only; OFF leaves the register tier's pre-bridge byte stream unchanged, so the
/// first accessor fill takes the existing site-gate eviction and MEM retry.
fn boxref_own_getter_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_BOXREF_OWN_GETTER").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Register-promoting region codegen: each region value lives in a fixed xmm
/// (numbers) or gpr (booleans) home for the whole loop. Live-in values are
/// loaded + type-guarded ONCE at entry; the loop body is then pure register SSE
/// with NO per-op guards or memory traffic (this is what makes it competitive
/// with V8). All homes are flushed back to the reg file / globals on every exit.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_region_regalloc(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    ta_plan: &TaPinPlan,
    ta_snapshot: usize,
    // W20 BOXREF: the heap helper block + this region's inline-cache site base.
    // `None` for the SROA/numeric caller, whose rewritten bytecode has no heap
    // op at all — so the boxed arms are unreachable there by construction.
    heap: Option<&HeapHelpers>,
    // The ordinary region compiler's guarded method/accessor plans. The
    // register tier consumes only the exact, call-free own-getter shape screened
    // by `boxref_passthrough_getter`; `None` for the numeric/SROA caller.
    method_plan: Option<&FxHashMap<usize, MethodInlinePlan>>,
    // Per-site accessor-arm flags, in the same order `register_ic_sites` built
    // them (the k-th GetProp/SetProp of the region).
    acc_emit: &[bool],
    // Cleared by `Jit::compile_region` for a region whose BOXREF compile has
    // already evicted once — see `region_boxref_blacklist`.
    boxref_ok: bool,
    meter: Option<crate::codegen::meter::Meter>,
    // `(code, engaged_boxref)`. The flag rides back so `Jit::compile_region` can
    // tag the installed region: a BOXREF region that evicts must retry WITHOUT
    // the boxed arms rather than be blacklisted, which for a DOUBLE region means
    // the loop runs interpreted (B102's 11x).
) -> Option<(JitFn, bool)> {
    if !region_can_compile(proto, start, end, None) {
        return None;
    }
    // ── W20 BOXREF admission ──
    // An ACCESSOR-armed site is excluded wholesale: its probe would have to
    // dispatch a getter, which is user code, which this tier cannot run. A site
    // that turns accessor LATER resolves the same way at runtime — the miss
    // helper answers `PROP_VIA_IC`, the arm deopts, and the region falls back —
    // so this gate is about not emitting a probe that is known to be useless,
    // not about soundness.
    let boxref = if heap.is_some() && boxref_ok && !acc_emit.iter().any(|&b| b) {
        BoxRefAdmit {
            elems: box_home_enabled(),
            ro_recv: regalloc_getprop_enabled(),
        }
    } else {
        BoxRefAdmit::NONE
    };
    // The regalloc path uses boxed-double semantics and cannot host Bitwise
    // (int32-lane) ops — they decline to the memory path here.
    let plan = plan_region(proto, start, end, ta_plan, false, true, true, boxref)?;
    // W28: type splits are planned ONLY for `admit_dv`/`share_homes` plans,
    // which route exclusively into the GPR emitter — this call passes neither,
    // so the map is empty by construction. Refuse rather than assume: this
    // emitter's `gh`/`Move`/`flush_exit` contract has never been proven against
    // a register with two homes of different KINDS.
    if !plan.ty_splits.is_empty() {
        decline_emit("regalloc-emit: type-split plan");
        return None;
    }
    // W20: does this region carry the inline-cache probe? That decides the frame
    // layout (a shadow window + a volatile-home spill area + 16 bytes of probe
    // scratch) and whether the prologue pins r13/r14. Empty ⇒ every byte below is
    // what the pre-wave emitter produced.
    let needs_ic = !plan.getprop_ips.is_empty();
    let heap = heap.filter(|_| needs_ic);
    if needs_ic && heap.is_none() {
        decline_emit("regalloc-emit: GetProp arm planned without heap helpers");
        return None;
    }
    if !plan.split_recvs.is_empty() && std::env::var_os("ZIPP_JITLOG").is_some() {
        let mut srs: Vec<u16> = plan.split_recvs.iter().copied().collect();
        srs.sort_unstable();
        for sr in srs {
            // The receiver `LoadGlobal` ips come with it: they are what tells an
            // exit taken INSIDE the receiver window from one outside it, which
            // is the only thing that makes a parity case on this shape
            // non-vacuous.
            let mut lg: Vec<usize> = plan
                .split_recv_lg
                .iter()
                .copied()
                .filter(|&i| matches!(proto.code[i], Instr::LoadGlobal { dst, .. } if dst == sr))
                .collect();
            lg.sort_unstable();
            eprintln!("[jit] DOUBLE region [{start},{end}] B94 split receiver r{sr} lg={lg:?}");
        }
    }
    let mut ops = match dynasmrt::x64::Assembler::new() {
        Ok(a) => a,
        Err(_) => {
            decline_emit("regalloc-emit: assembler alloc failed");
            return None;
        }
    };
    let (s, e) = (start as usize, end as usize);

    let in_region: Vec<_> = (s..=e).map(|_| ops.new_dynamic_label()).collect();
    let mut exit_stubs: FxHashMap<u32, dynasmrt::DynamicLabel> = FxHashMap::default();
    let flush_exit = ops.new_dynamic_label(); // flush homes, then restore + ret
    let entry_bail = ops.new_dynamic_label(); // entry guard failed: restore + ret, NO flush
    let lbl = |ip: u32, in_region: &[dynasmrt::DynamicLabel]| in_region[(ip - start) as usize];
    // Step metering (a metered VM only) — see codegen::meter.
    let blocks = crate::codegen::meter::block_map(meter, &proto.code, s, e);

    // ── prologue ── save callee-saved gprs, fetch globals base, save the
    // nonvolatile xmm6..15 (we may use them as homes), load live-in homes, jump
    // to the loop header. No call occurs after the globals-base fetch, so stack
    // alignment past that point is irrelevant and movdqu (unaligned) is fine.
    // r13/r14 are pushed (unused by the double path) to share the one restore
    // sequence with the int path, which uses them for guard constants.
    // Frame: with pinned TypedArrays, reserve [shadow 32][TA snapshot slots 32·n_ta]
    // [xmm6..15 save 160][pad 8] — the shadow sits at the BOTTOM so the prologue TA
    // snapshot calls (the only calls after the globals fetch) have their 32-byte
    // shadow space, and `frame ≡ 8 (mod 16)` keeps rsp 16-aligned for them. With no
    // pins this is exactly the legacy 160-byte xmm frame (xmm_off/ta_base = 0).
    // W20: with the inline-cache probe the frame grows by a 96-byte VOLATILE
    // HOME SPILL area and 16 bytes of PROBE SCRATCH, and the 32-byte shadow is
    // reserved even with no pins (the miss helper is a win64 call):
    //   [shadow 32][TA slots 32n][xmm6..15 save 160][spill 96][probe 16][pad 8]
    // 312 ≡ 8 (mod 16), the same residue the pinned layout already needs, so rsp
    // is 16-aligned at every call in both shapes.
    let n_ta = ta_plan.pins.len() as i32;
    let (frame, xmm_off, ta_base, spill_off, probe_off) = if needs_ic {
        let xo = 32 + 32 * n_ta;
        (312 + 32 * n_ta, xo, 32i32, xo + 160, xo + 256)
    } else if n_ta > 0 {
        (200 + 32 * n_ta, 32 + 32 * n_ta, 32i32, 0i32, 0i32)
    } else {
        (160i32, 0i32, 0i32, 0i32, 0i32)
    };
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
        ; sub rsp, 40                 // shadow space (32) + 8 pad ⇒ rsp 16-aligned
        ; mov rcx, rdi
        ; mov rax, QWORD globals_base_helper as i64
        ; call rax
        ; mov r12, rax
    );
    // W20: the inline-cache probe's two pinned bases, fetched in the SAME shadow
    // window as the globals base. r13/r14 are already pushed by the prologue
    // above (they were pushed only to share the int path's restore sequence) and
    // are callee-saved across the pin-snapshot calls below, so nothing else in
    // this frame changes. Both stay valid for the whole run: `jit_get_prop_miss`
    // — the only helper this tier may call — resizes neither the version array
    // nor the IC table (see the miss arm), which is why there is no re-fetch.
    if let Some(h) = heap {
        dynasm!(ops
            ; mov rcx, rdi
            ; mov rax, QWORD h.versions_base as i64
            ; call rax
            ; mov r13, rax
            ; mov rcx, rdi
            ; mov rax, QWORD h.ic_base as i64
            ; call rax
            ; mov r14, rax
        );
    }
    dynasm!(ops
        ; add rsp, 40
        ; sub rsp, frame              // [shadow][TA slots][xmm6..15 save][pad]
    );
    for k in 0..10u32 {
        let xi = 6 + k as u8;
        dynasm!(ops ; movdqu [rsp + xmm_off + (k as i32) * 16], Rx(xi));
    }
    // A pure cyclic field-read loop can finish as one guarded projection and
    // modular reduction.  This prefix runs before any home is loaded; a guard
    // miss has changed no JS state and falls through to the byte-identical
    // ordinary prologue.  Metered VMs must observe every bytecode charge, so
    // they never receive the prefix.
    if meter.is_none()
        && !matches!(proto.code[s], Instr::UpvalGet { .. })
        && field_read_stream_enabled()
        && field_read_stream_shape(proto, s, e)
    {
        if let Some(h) = heap {
            if std::env::var_os("ZIPP_JITLOG").is_some() {
                eprintln!(
                    "[jit] DOUBLE region fn{} [{start},{end}] field-read-stream prefix",
                    h.func_id
                );
            }
            let packed = ((h.func_id as u64) << 32) | ((s as u64) << 16) | e as u64;
            let fallback = ops.new_dynamic_label();
            dynasm!(ops
                ; mov rcx, rdi
                ; mov rdx, rbx
                ; mov r8, QWORD packed as i64
                ; mov rax, QWORD crate::vm::jit_field_read_loop as usize as i64
                ; call rax
                ; test rax, rax
                ; je => fallback
                ; mov DWORD [rsi], (e + 1) as i32
            );
            emit_region_restore_n(&mut ops, xmm_off, frame);
            dynasm!(ops ; => fallback);
        }
    }
    // ── pinned-TypedArray snapshots ── BEFORE loading any numeric home: jit_ta_snapshot
    // is a win64 call that clobbers volatile xmm0..5 (which double as home registers),
    // but xmm6..15 are already saved above and no home is loaded yet. Each slot gets
    // {obj_bits, base, len} (or {0,0,0} if the live receiver is no longer a kind-`kind`
    // view → the per-access identity guard then misses → deopt). r12/rbx are
    // callee-saved across the call. This is the last call before the loop.
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
    // ── W7 hoisted pin identity guards ── one snapshot-validity check per
    // hoisted pin replaces the per-access source load + compare: the snapshot
    // was just taken FROM the source, and `hoistable_pins` proved the region
    // cannot write the source nor run anything that could detach/resize/grow
    // the pinned object. A miss takes `entry_bail` exactly like a failed
    // live-in type guard (no flush, resume at the header, counts as a deopt).
    for j in 0..ta_plan.pins.len() {
        if plan.hoist_pins.contains(&(j as u8)) {
            dynasm!(ops ; cmp QWORD [rsp + ta_base + 32 * j as i32], 0 ; je => entry_bail);
        }
    }
    // Load live-in globals (guarded) and live-in registers (guarded).
    for &(gi, x) in &plan.live_in_globs {
        dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]);
        emit_box_to_home(&mut ops, x, entry_bail);
    }
    for &(r, x) in &plan.live_in_regs {
        dynasm!(ops ; mov rax, [rbx + dreg(r)]);
        emit_box_to_home(&mut ops, x, entry_bail);
    }
    // Bool homes last. This ORDER is no longer load-bearing — no entry-load
    // helper scratches a BOOL_GPR any more (see the register contract on
    // `BOOL_GPRS`) — but it is kept: it is the order the other two tiers use.
    region_int::emit_bool_home_zero(&mut ops, &plan);
    for &(r, g) in &plan.live_in_bools {
        dynasm!(ops ; mov rax, [rbx + dreg(r)]);
        emit_bool_entry_load(&mut ops, g, entry_bail);
    }
    // Hoisted loop-invariant constants: materialise once, here.
    for &hip in &plan.hoist_ips {
        emit_load_const(&mut ops, &plan, &proto.code[hip], proto);
    }
    // AddInt immediates as f64 const homes (an i32 converts to f64 exactly).
    {
        let mut imms: Vec<(i32, u8)> = plan.addint_imm_home.iter().map(|(&i, &h)| (i, h)).collect();
        imms.sort_unstable();
        for (imm, h) in imms {
            let bits = (imm as f64).to_bits();
            dynasm!(ops ; mov rax, QWORD bits as i64 ; movq Rx(h), rax);
        }
    }
    dynasm!(ops ; jmp => lbl(start, &in_region));

    // ── body ──
    // Compare→branch flag fusion (ordered f64 compares only — Eq/Ne need the
    // parity fix-up, so they keep the boxed-bool `test` path).
    let mut flag_cmp: Option<(usize, u16, Cmp)> = None;
    // Redundant-copy tracker (see `LastCopy`).
    let mut lc: LastCopy = None;
    // W20: the inline-cache site cursor. `register_ic_sites` numbered the k-th
    // GetProp/SetProp of the region `ic_base_idx + k`, so this must advance on
    // exactly the same ops in the same order. It advances on GetProp only —
    // which is sound because a SetProp DECLINES the whole region at the
    // catch-all below, so no site after one is ever emitted.
    let mut ic_site = heap.map_or(0, |h| h.ic_base_idx);
    let mut own_getter_arms = 0usize;
    for ip in s..=e {
        dynasm!(ops ; => lbl(ip as u32, &in_region));
        let charged = crate::codegen::meter::charge_block(&mut ops, &blocks, ip, &mut exit_stubs);
        if plan.jump_targets.contains(&ip) {
            lc = None; // control may arrive here with different home contents
        }
        // A hoisted constant's home was filled in the prologue; the body op is a
        // no-op (fall through to the next ip, its label preserved for jumps).
        if let Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } = proto.code[ip] {
            if plan.hoisted.contains(&dst) {
                continue;
            }
        }
        // A DV-flag-fused Eq emits NOTHING here: the adjacent pinned-DV call
        // computes ToBoolean(a === b) inline from the operands' homes, and its
        // deopt resumes AT this ip so the interpreter recomputes the flag into
        // the frame slot before re-running the call. Like a hoisted constant,
        // flags and the copy tracker survive (no instruction is emitted).
        if plan.dv_flag_elide.contains(&ip) {
            continue;
        }
        // Dead-code elimination: skip a pure op whose result is never read (see
        // plan_region `dead`). Sound — every regalloc-region op is side-effect-free.
        if let Some(d) = writes_reg(&proto.code[ip]) {
            if plan.dead.contains(&d) {
                continue;
            }
        }
        // A metering charge clobbers flags, so a compare from an earlier ip can
        // no longer drive this ip's branch. See the note in region_int.
        let prev_flag = flag_cmp.take().filter(|_| !charged);
        match proto.code[ip] {
            Instr::LoadInt { .. } | Instr::LoadConst { .. } => {
                emit_load_const(&mut ops, &plan, &proto.code[ip], proto);
                if let Some(d) = writes_reg(&proto.code[ip]) {
                    copy_clobber(&mut lc, xh(&plan, d));
                }
            }
            // Register copies use movaps (a FULL-register copy): unlike
            // `movsd xmm, xmm`, it has no false dependency on the destination's
            // old value and is eliminated at rename — this keeps the loop's
            // carried dependency chains down to the actual addsd/mulsd.
            // ToPropKey compiles as Move on this tier: the plan proved (or
            // entry-guarded) the key numeric, ToPropertyKey of a number is the
            // identity, and the receiver's nullish check is subsumed by the pin
            // (see the plan's idx_obj note). Same or-pattern homes, same copy
            // elision.
            Instr::Move { dst, src } | Instr::ToPropKey { dst, src, .. } => {
                match home(&plan, dst) {
                    Home::Xmm(d) => {
                        let srx = xh(&plan, src);
                        if d != srx && !copy_is_noop(lc, d, srx) {
                            dynasm!(ops ; movaps Rx(d), Rx(srx));
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
                }
            }
            // ── pinned receiver / B94 split receiver ── the object has no numeric
            // home (the element emitter reads it via the pin's source; a split
            // receiver's xmm home belongs to the register's NUMERIC half), so it
            // goes to the register's memory slot, which stays authoritative for
            // this register throughout the region. `emit_recv_slot_store` carries
            // why the ta_recv half is not a no-op.
            Instr::LoadGlobal { dst, idx }
                if plan.ta_recv_regs.contains(&dst)
                    || plan.split_recv_lg.contains(&ip)
                    || plan.box_regs.contains(&dst) =>
            {
                emit_recv_slot_store(&mut ops, dst, idx);
                flag_cmp = prev_flag;
            }
            Instr::LoadGlobal { dst, idx } => {
                let d = xh(&plan, dst);
                let g = plan.glob_home[&idx];
                if d != g && !copy_is_noop(lc, d, g) {
                    dynasm!(ops ; movaps Rx(d), Rx(g));
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
                    dynasm!(ops ; movaps Rx(g), Rx(srx));
                    copy_clobber(&mut lc, g);
                    lc = Some((g, srx));
                } else {
                    flag_cmp = prev_flag;
                }
            }
            // B92: Bitwise on the DOUBLE path. Previously one `&`/`|`/`>>>`/`|0`
            // demoted the whole region to the memory tier — measured on an
            // otherwise identical 20M-iteration loop, 0.75ns/iter -> 4.15ns/iter
            // (node: 0.75 either way), i.e. a 5.5x cliff behind a single op.
            //
            // The homes here are f64, not the int path's sign-extended i64, so
            // each operand needs ToInt32. `cvttsd2si` in its 64-BIT form
            // truncates toward zero exactly for |x| < 2^63 — which covers every
            // u32, the case that matters for `dv.getUint32(...) >>> 24` — and
            // ToInt32 is then just the low 32 bits of that i64, because ToInt32
            // is trunc-then-mod-2^32 and the mod is what taking `eax` does.
            //
            // The one value `cvttsd2si` cannot represent is the "integer
            // indefinite" INT64_MIN it returns for NaN, ±Infinity and |x| >=
            // 2^63. Those bail to the interpreter, which computes the real
            // answer (0 for NaN/Inf; a modular reduction for the huge case).
            // A legitimate operand of exactly INT64_MIN also bails — correct,
            // just slower, and unreachable from an f64 that is not already huge.
            //
            // Scratch is rax/rcx/rdx only. The INT64_MIN sentinel used to be
            // materialised in r10, which is `BOOL_GPRS[2]`: every `|`, `&`,
            // `^`, `<<`, `>>`, `>>>` — including the `| 0` an int-flavoured JS
            // loop writes on every line — destroyed the region's THIRD `Bool`
            // home, for the rest of the region and across the backedge. That
            // flushed a raw sentinel into a JS variable holding `false` (W16).
            Instr::Bitwise { dst, a, b, op } => {
                use crate::bytecode::BitwiseOp as B;
                let (d, ax, bx) = (xh(&plan, dst), xh(&plan, a), xh(&plan, b));
                copy_clobber(&mut lc, d);
                let bw_bail = ops.new_dynamic_label();
                let bw_done = ops.new_dynamic_label();
                dynasm!(ops
                    ; cvttsd2si rax, Rx(ax)
                    ; mov rdx, QWORD i64::MIN
                    ; cmp rax, rdx
                    ; je => bw_bail
                    ; cvttsd2si rcx, Rx(bx)
                    ; cmp rcx, rdx
                    ; je => bw_bail
                );
                // x86 masks the shift count in cl to 5 bits, which is exactly
                // JS's `& 31`.
                match op {
                    B::And => dynasm!(ops ; and eax, ecx ; movsxd rax, eax),
                    B::Or => dynasm!(ops ; or eax, ecx ; movsxd rax, eax),
                    B::Xor => dynasm!(ops ; xor eax, ecx ; movsxd rax, eax),
                    B::Shl => dynasm!(ops ; shl eax, cl ; movsxd rax, eax),
                    B::Shr => dynasm!(ops ; sar eax, cl ; movsxd rax, eax),
                    // `>>>` yields a u32 (0..2^32-1); the 32-bit `shr` zero-
                    // extends into rax, and converting the 64-bit rax gives the
                    // unsigned value as a double rather than a negative one.
                    B::Ushr => dynasm!(ops ; shr eax, cl),
                }
                // `xorps` first: `cvtsi2sd` merges into the low lane and would
                // otherwise carry a false dependency on the home's old contents.
                dynasm!(ops
                    ; xorps Rx(d), Rx(d)
                    ; cvtsi2sd Rx(d), rax
                    ; jmp => bw_done
                    // Resume at THIS ip: nothing has been written yet (the bail
                    // precedes the only store to `d`), so every home is still
                    // consistent and `flush_exit` writes them all back before
                    // the interpreter re-executes the op with full semantics.
                    ; => bw_bail
                    ; mov DWORD [rsi], ip as i32
                    ; jmp => flush_exit
                    ; => bw_done
                );
            }
            Instr::Add { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Add, &mut lc),
            Instr::Sub { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Sub, &mut lc),
            Instr::Mul { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Mul, &mut lc),
            Instr::Div { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Div, &mut lc),
            // `%` on the DOUBLE path. B113 named this the blocker that dropped
            // typedarray-math's fill region ([16,47]) to the memory tier
            // (`[decline-reason] regalloc-emit-unhandled: Mod`) — same one-op
            // cliff class as B92's Bitwise. The fast shape is the one the hot
            // loops actually run (`(x >>> 0) % 100000`): both operands EXACT
            // integers in their f64 homes, so the remainder is the i64 `idiv`
            // remainder, and fmod exactness (the true remainder of two f64s is
            // itself an f64) makes the cvtsi2sd back into the home exact.
            // Everything else DEOPTs to the interpreter AT this ip — nothing is
            // written before the deopt, the op is pure, re-execution is sound
            // and the interpreter computes the real `%` (fractional operands,
            // NaN/Inf, b == 0 → NaN, b == -1, |x| >= 2^63):
            //   (1) each operand round-trips cvttsd2si/cvtsi2sd (a non-integral,
            //       NaN, Inf or >= 2^63 value fails; the INT64_MIN "integer
            //       indefinite" fails its own round-trip except for a literal
            //       -2^63, which (3) then rejects);
            //   (2) b == 0 deopts (JS: NaN);
            //   (3) b == -1 deopts (the one `idiv` #DE case left after (1)-(2):
            //       INT64_MIN / -1 overflows; a % -1 is ±0 and rare — the
            //       interpreter gets the sign right).
            // A ZERO remainder takes its sign from the ORIGINAL f64 dividend
            // (still unclobbered in its home — `d` is not written yet), which
            // covers both `-0.0 % n` (round-trips as integer 0) and `-6 % 3`
            // (JS: -0). Scratch is rax/rcx/rdx/xmm0 only — r8-r11 are BOOL_GPRS
            // homes. `ZIPP_NO_DOUBLE_MOD=1` restores the decline for A/B.
            Instr::Mod { dst, a, b } if double_mod_enabled() => {
                let (d, ax, bx) = (xh(&plan, dst), xh(&plan, a), xh(&plan, b));
                copy_clobber(&mut lc, d);
                let m_bail = ops.new_dynamic_label();
                let m_zero = ops.new_dynamic_label();
                let m_done = ops.new_dynamic_label();
                dynasm!(ops
                    ; cvttsd2si rax, Rx(ax)
                    ; cvtsi2sd xmm0, rax
                    ; ucomisd xmm0, Rx(ax)
                    ; jp => m_bail                 // NaN operand (unordered)
                    ; jne => m_bail                // non-integral / out of i64 range
                    ; cvttsd2si rcx, Rx(bx)
                    ; cvtsi2sd xmm0, rcx
                    ; ucomisd xmm0, Rx(bx)
                    ; jp => m_bail
                    ; jne => m_bail
                    ; test rcx, rcx
                    ; jz => m_bail                 // b == 0 → NaN in the interpreter
                    ; cmp rcx, -1
                    ; je => m_bail                 // idiv #DE guard; a % -1 is ±0
                    ; cqo
                    ; idiv rcx                     // remainder → rdx, sign of dividend
                    ; test rdx, rdx
                    ; jz => m_zero
                    // `xorps` first: `cvtsi2sd` merges into the low lane and would
                    // otherwise carry a false dependency on the home's old contents.
                    ; xorps Rx(d), Rx(d)
                    ; cvtsi2sd Rx(d), rdx
                    ; jmp => m_done
                    ; => m_zero
                    // ±0 with the ORIGINAL dividend's sign: isolate its f64 sign bit.
                    ; movq rax, Rx(ax)
                    ; mov rcx, QWORD (1u64 << 63) as i64
                    ; and rax, rcx
                    ; movq Rx(d), rax
                    ; jmp => m_done
                    // Resume at THIS ip: nothing has been written yet (the bail
                    // precedes the only store to `d`), so every home is still
                    // consistent and `flush_exit` writes them all back before
                    // the interpreter re-executes the op with full semantics.
                    ; => m_bail
                    ; mov DWORD [rsi], ip as i32
                    ; jmp => flush_exit
                    ; => m_done
                );
            }
            Instr::AddInt { dst, a, imm, .. } => {
                let d = xh(&plan, dst);
                let ax = xh(&plan, a);
                let skip_copy = d == ax || copy_is_noop(lc, d, ax);
                if let Some(&ch) = plan.addint_imm_home.get(&imm) {
                    // The immediate sits (as f64) in a prologue-filled const home.
                    if !skip_copy {
                        dynasm!(ops ; movaps Rx(d), Rx(ax));
                    }
                    dynasm!(ops ; addsd Rx(d), Rx(ch));
                } else {
                    // Materialise the immediate's f64 bits via a gpr: `movq` writes
                    // the full register (no cvtsi2sd false dependency on xmm0).
                    let bits = (imm as f64).to_bits();
                    dynasm!(ops ; mov rax, QWORD bits as i64 ; movq xmm0, rax);
                    if !skip_copy {
                        dynasm!(ops ; movaps Rx(d), Rx(ax));
                    }
                    dynasm!(ops ; addsd Rx(d), xmm0);
                }
                copy_clobber(&mut lc, d);
            }
            Instr::Neg { dst, a } => {
                let d = xh(&plan, dst);
                let ax = xh(&plan, a);
                // FLIP THE SIGN BIT, don't subtract from zero. Under round-to-
                // nearest `0.0 - 0.0` is `+0.0`, so `-(+0)` came out `+0` and
                // `1 / -0` printed `Infinity` instead of `-Infinity`. JS negation
                // is defined on the sign bit, and `-0` is not exotic here: the
                // compiler lowers the literal `-0` itself to `LoadInt 0; Neg`.
                dynasm!(ops
                    ; mov rax, QWORD (1u64 << 63) as i64
                    ; movq xmm0, rax
                    ; movapd Rx(d), Rx(ax)
                    ; xorpd Rx(d), xmm0
                );
                copy_clobber(&mut lc, d);
            }
            Instr::Lt { dst, a, b } => {
                emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Lt);
                flag_cmp = Some((ip, dst, Cmp::Lt));
            }
            Instr::Le { dst, a, b } => {
                emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Le);
                flag_cmp = Some((ip, dst, Cmp::Le));
            }
            Instr::Gt { dst, a, b } => {
                emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Gt);
                flag_cmp = Some((ip, dst, Cmp::Gt));
            }
            Instr::Ge { dst, a, b } => {
                emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Ge);
                flag_cmp = Some((ip, dst, Cmp::Ge));
            }
            Instr::Eq { dst, a, b } => emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Eq),
            Instr::Ne { dst, a, b } => emit_dcmp(&mut ops, &plan, dst, a, b, Cmp::Ne),
            Instr::Jump { target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; jmp => t);
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                let if_false = matches!(proto.code[ip], Instr::JumpIfFalse { .. });
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                // Flag fusion off the preceding ucomisd (ordered compares only).
                // emit_dcmp computed `Lt/Le` as `ucomisd b, a` (seta/setae) and
                // `Gt/Ge` as `ucomisd a, b` — the unsigned-style jcc below mirror
                // that operand order, and NaN (CF=ZF=PF=1) makes every ordered
                // comparison FALSE: the `if_false` jcc is then taken, exactly as
                // the interpreter's NaN comparison semantics demand.
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
                        (Cmp::Lt, true) => dynasm!(ops ; jbe => t), // !(b > a)
                        (Cmp::Le, true) => dynasm!(ops ; jb => t),  // !(b >= a)
                        (Cmp::Gt, true) => dynasm!(ops ; jbe => t), // !(a > b)
                        (Cmp::Ge, true) => dynasm!(ops ; jb => t),  // !(a >= b)
                        (Cmp::Lt, false) => dynasm!(ops ; ja => t),
                        (Cmp::Le, false) => dynasm!(ops ; jae => t),
                        (Cmp::Gt, false) => dynasm!(ops ; ja => t),
                        (Cmp::Ge, false) => dynasm!(ops ; jae => t),
                        _ => unreachable!("flag fusion records ordered compares only"),
                    },
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
                let (ax, bx) = (xh(&plan, a), xh(&plan, b));
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; ucomisd Rx(bx), Rx(ax) ; jbe => t); // !(a<b)
            }
            Instr::JumpIfNotLe { a, b, target } => {
                let (ax, bx) = (xh(&plan, a), xh(&plan, b));
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; ucomisd Rx(bx), Rx(ax) ; jb => t); // !(a<=b)
            }
            Instr::Return { .. } | Instr::ReturnUndefined => {
                dynasm!(ops ; mov DWORD [rsi], ip as i32 ; jmp => flush_exit);
            }
            // ── W20 BOXREF element read ── `o = arr[i]` where the elements are
            // OBJECTS. Same three guards as the numeric arm below, and then the
            // element's `Value` BITS are stored verbatim into the register's
            // interpreter frame slot: that slot IS this register's home (see
            // `RegionPlan::box_regs`), so there is nothing to unbox, nothing to
            // re-encode, and no forgery risk.
            //
            // A HOLE deopts rather than storing, EXACTLY mirroring the memory
            // tier's arm and `jit_get_index`: an absent index resolves through the
            // PROTOTYPE CHAIN, which is a different answer from the bits sitting
            // in the dense Vec. OOB / negative / fractional / NaN keys and an
            // identity miss deopt for the same reason. Nothing is written before
            // any of those, so re-executing the op is sound.
            Instr::GetIndex { dst, key, .. } if plan.box_regs.contains(&dst) => {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let kx = xh(&plan, key);
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                if !plan.hoist_pins.contains(&(j as u8)) {
                    match ta_plan.pins[j].src {
                        TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                        TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                    }
                    dynasm!(ops
                        ; cmp rax, [rsp + off]        // receiver vs snapshot obj_bits
                        ; jne => deopt
                    );
                }
                dynasm!(ops
                    ; cvttsd2si rcx, Rx(kx)           // index = trunc(key home)
                    ; cvtsi2sd xmm0, rcx
                    ; ucomisd xmm0, Rx(kx)
                    ; jne => deopt                    // non-integral index
                    ; jp => deopt                     // NaN index
                    ; cmp rcx, [rsp + off + 16]       // unsigned: i < len (catches <0)
                    ; jae => deopt
                    ; mov rdx, [rsp + off + 8]        // pinned items base
                    ; mov rax, [rdx + rcx * 8]        // items[i] (Value bits)
                    ; mov rdx, QWORD ARR_HOLE_BITS as i64
                    ; cmp rax, rdx
                    ; je => deopt                     // HOLE → interpreter (proto walk)
                    ; mov [rbx + dreg(dst)], rax      // the frame slot IS the home
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], ip as i32      // resume AT this ip
                    ; jmp => flush_exit
                    ; => done
                );
                lc = None;
            }
            // ── pinned-Float64Array element read ── x[i] → a direct movsd into the
            // dst xmm home (UNBOXED). Guards (any miss DEOPTs to the interpreter AT
            // this ip — index ops are all-or-nothing, so re-execution is sound):
            // (1) receiver identity vs the prologue snapshot; (2) integral index
            // (cvttsd2si round-trip; fractional/NaN → deopt); (3) unsigned bounds.
            Instr::GetIndex { dst, key, .. } => {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let d = xh(&plan, dst);
                let kx = xh(&plan, key);
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                // W7: a hoisted pin's identity was checked ONCE at entry and
                // the region provably cannot change it — only the semantic
                // index/bounds/tag guards remain per access.
                if !plan.hoist_pins.contains(&(j as u8)) {
                    match ta_plan.pins[j].src {
                        TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                        TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                    }
                    dynasm!(ops
                        ; cmp rax, [rsp + off]        // receiver vs snapshot obj_bits
                        ; jne => deopt
                    );
                }
                dynasm!(ops
                    ; cvttsd2si rcx, Rx(kx)           // index = trunc(key home)
                    ; cvtsi2sd xmm0, rcx
                    ; ucomisd xmm0, Rx(kx)
                    ; jne => deopt                    // non-integral index
                    ; jp => deopt                     // NaN index
                    ; cmp rcx, [rsp + off + 16]       // unsigned: i < len (catches <0)
                    ; jae => deopt
                    ; mov rdx, [rsp + off + 8]        // pinned base
                );
                if is_arr_pin(ta_plan.pins[j].kind) {
                    // B95 dense ORDINARY Array: the element is a NaN-boxed Value
                    // (stride 8). `emit_box_to_home` is the same guard the prologue
                    // uses on a live-in — Int → cvtsi2sd, real double → movq,
                    // anything else (HOLE, bool, null/undefined, heap) → deopt.
                    dynasm!(ops ; mov rax, [rdx + rcx * 8]);
                    emit_box_to_home(&mut ops, d, deopt);
                } else {
                    // kind-8 Float64Array: the element IS a raw f64.
                    dynasm!(ops ; movsd Rx(d), [rdx + rcx * 8]);
                }
                dynasm!(ops
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], ip as i32      // resume AT this ip
                    ; jmp => flush_exit
                    ; => done
                );
                copy_clobber(&mut lc, d);
                lc = None;
            }
            // ── pinned-Float64Array element write ── x[i] = v → a direct movsd from
            // the val xmm home. Same guards; an OOB store deopts (the interpreter
            // does the spec coerce-then-silent-noop). `val` is already an f64 home,
            // so the store is exact for every number (integers store their double).
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
                    ; cvttsd2si rcx, Rx(kx)
                    ; cvtsi2sd xmm0, rcx
                    ; ucomisd xmm0, Rx(kx)
                    ; jne => deopt
                    ; jp => deopt
                    ; cmp rcx, [rsp + off + 16]
                    ; jae => deopt
                    ; mov rdx, [rsp + off + 8]
                    ; movsd [rdx + rcx * 8], Rx(vx)   // home f64 → element
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], ip as i32
                    ; jmp => flush_exit
                    ; => done
                );
                lc = None;
            }
            // ── pinned-DataView get* read ── dv.getUint32(pos[, le]) → an inline
            // byte load, UNBOXED into the dst f64 home. `plan_region` admits a
            // CallMethod on this path ONLY as a pinned-DV get* (`pinned_dv`), with
            // pos typed Num and any read flag typed Bool, so this is the only
            // CallMethod shape that reaches the emitter. Guards mirror the
            // pinned-Float64Array GetIndex arm above — any miss DEOPTs to the
            // interpreter AT this ip, and the access is all-or-nothing (nothing is
            // written before the deopt), so re-execution is sound and the
            // interpreter's re-run raises whatever the miss stands for (the
            // RangeError for an OOB pos, the TypeError for a detached buffer):
            //   (1) receiver identity vs the prologue snapshot (a detached /
            //       shrunk-resizable / non-DataView receiver snapshots {0,0,0});
            //   (2) integral pos via the cvttsd2si round-trip (fractional/NaN pos
            //       deopts — the interpreter runs the ToIndex truncation);
            //   (3) signed bounds: pos < 0 or pos > byteLength - size deopts.
            // The result lands directly in the f64 home: an int kind converts
            // (exact — a u32 <= 2^32-1), a float kind moves but CANONICALISES NaN,
            // because raw bytes flushed from a home as Value bits could otherwise
            // alias a NaN-box tag. Scratch stays rax/rcx/rdx/xmm0: r8-r11 are
            // BOOL_GPRS homes (the endian flags live there).
            Instr::CallMethod {
                dst,
                arg_base,
                argc,
                name,
                ..
            } => {
                let kindid = match proto
                    .string_constants
                    .get(name as usize)
                    .and_then(|k| dv_get_kind(k))
                {
                    Some(k) => k,
                    None => {
                        decline_emit(format_args!(
                            "regalloc-emit-unhandled: {:?}",
                            proto.code[ip]
                        ));
                        return None;
                    }
                };
                let j = match ta_plan.access.get(&ip) {
                    Some(&j) if ta_plan.pins[j as usize].kind == DV_PIN_KIND => j as usize,
                    _ => {
                        decline_emit("regalloc-emit: DV CallMethod without a DataView pin");
                        return None;
                    }
                };
                let off = ta_base + 32 * j as i32;
                let size = [1i32, 1, 1, 2, 2, 4, 4, 4, 8][kindid as usize];
                let d = xh(&plan, dst);
                let px = xh(&plan, arg_base);
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                // W7: identity hoisted to entry for a hoisted pin (see the
                // GetIndex arm); the pos/bounds guards — the RangeError
                // semantics — stay per access.
                if !plan.hoist_pins.contains(&(j as u8)) {
                    match ta_plan.pins[j].src {
                        TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                        TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                    }
                    dynasm!(ops
                        ; cmp rax, [rsp + off]        // receiver vs snapshot obj_bits
                        ; jne => deopt
                    );
                }
                dynasm!(ops
                    ; cvttsd2si rcx, Rx(px)           // pos = trunc(pos home)
                    ; cvtsi2sd xmm0, rcx
                    ; ucomisd xmm0, Rx(px)
                    ; jne => deopt                    // non-integral pos
                    ; jp => deopt                     // NaN pos
                    ; test rcx, rcx
                    ; js => deopt                     // negative → RangeError
                    ; mov rdx, [rsp + off + 16]       // byteLength
                    ; sub rdx, size
                    ; cmp rcx, rdx                    // signed: pos > byteLength-size
                    ; jg => deopt                     //  (incl. byteLength < size)
                    ; mov rdx, [rsp + off + 8]        // pinned base (data + byteOffset)
                );
                let le_big = ops.new_dynamic_label();
                let loaded = ops.new_dynamic_label();
                if size > 1 {
                    if let Some(&(fa, fb)) = plan.dv_flag_fuse.get(&ip) {
                        // Fused adjacent Eq: the flag is ToBoolean(a === b)
                        // over two Num homes. NaN (parity) or unequal → false
                        // → big-endian — exactly emit_dcmp's Eq (sete + setnp).
                        let (ax, bx) = (xh(&plan, fa), xh(&plan, fb));
                        dynasm!(ops
                            ; ucomisd Rx(ax), Rx(bx)
                            ; jp => le_big
                            ; jne => le_big
                        );
                    } else if argc == 2 {
                        // The flag is a Bool gpr home holding 0/1 — `test` IS
                        // ToBoolean here, exactly as on the INT tier (B22).
                        let lg = gh(&plan, arg_base + 1);
                        dynasm!(ops ; test Rq(lg), Rq(lg) ; jz => le_big);
                    } else {
                        // Absent flag = undefined = big-endian.
                        dynasm!(ops ; jmp => le_big);
                    }
                }
                // ── little-endian load ── ints land in eax, floats in xmm0.
                match kindid {
                    0 => dynasm!(ops ; movsx eax, BYTE [rdx + rcx]),
                    1 => dynasm!(ops ; movzx eax, BYTE [rdx + rcx]),
                    3 => dynasm!(ops ; movsx eax, WORD [rdx + rcx]),
                    4 => dynasm!(ops ; movzx eax, WORD [rdx + rcx]),
                    5 | 6 => dynasm!(ops ; mov eax, [rdx + rcx]),
                    7 => dynasm!(ops ; movss xmm0, [rdx + rcx] ; cvtss2sd xmm0, xmm0),
                    _ => dynasm!(ops ; movsd xmm0, [rdx + rcx]),
                }
                if size > 1 {
                    dynasm!(ops ; jmp => loaded ; => le_big);
                    // ── big-endian load (byte-swapped) ──
                    match kindid {
                        3 => dynasm!(ops
                            ; movzx eax, WORD [rdx + rcx]
                            ; rol ax, 8
                            ; movsx eax, ax
                        ),
                        4 => dynasm!(ops
                            ; movzx eax, WORD [rdx + rcx]
                            ; rol ax, 8
                        ),
                        5 | 6 => dynasm!(ops
                            ; mov eax, [rdx + rcx]
                            ; bswap eax
                        ),
                        7 => dynasm!(ops
                            ; mov eax, [rdx + rcx]
                            ; bswap eax
                            ; movd xmm0, eax
                            ; cvtss2sd xmm0, xmm0
                        ),
                        _ => dynasm!(ops
                            ; mov rax, [rdx + rcx]
                            ; bswap rax
                            ; movq xmm0, rax
                        ),
                    }
                    dynasm!(ops ; => loaded);
                }
                // ── land in the dst home ── `xorps` first: `cvtsi2sd` merges into
                // the low lane and would otherwise carry a false dependency on the
                // home's old contents (same idiom as the Bitwise arm above).
                match kindid {
                    // Uint32: the 32-bit load/bswap zero-extended eax into rax;
                    // the 64-bit convert is exact for every u32.
                    6 => dynasm!(ops
                        ; xorps Rx(d), Rx(d)
                        ; cvtsi2sd Rx(d), rax
                    ),
                    7 | 8 => {
                        // A loaded NaN keeps its payload through f64 arithmetic,
                        // and the exit flush writes home bits verbatim as a
                        // Value — so a payload aliasing a NaN-box tag would
                        // forge a tagged value. Canonicalise, exactly as the
                        // MEM tier's emit_box_f64_canon does.
                        let canon = ops.new_dynamic_label();
                        let fdone = ops.new_dynamic_label();
                        dynasm!(ops
                            ; ucomisd xmm0, xmm0
                            ; jp => canon                 // NaN → canonical QNAN
                            ; movapd Rx(d), xmm0
                            ; jmp => fdone
                            ; => canon
                            ; mov rax, QWORD QNAN_BITS as i64
                            ; movq Rx(d), rax
                            ; => fdone
                        );
                    }
                    // Int8/Uint8/Int16/Uint16/Int32: a signed i32 in eax.
                    _ => dynasm!(ops
                        ; xorps Rx(d), Rx(d)
                        ; cvtsi2sd Rx(d), eax
                    ),
                }
                // A FUSED access resumes at the ELIDED Eq (ip-1): the
                // interpreter recomputes the flag into the frame slot — which
                // native code never writes — then re-runs the call. The
                // re-executed window is exactly the pure Eq, whose operands'
                // homes were flushed holding the values it would have read.
                let resume_ip = if plan.dv_flag_fuse.contains_key(&ip) {
                    ip - 1
                } else {
                    ip
                };
                dynasm!(ops
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], resume_ip as i32
                    ; jmp => flush_exit
                    ; => done
                );
                copy_clobber(&mut lc, d);
                lc = None;
            }
            // ── W20 ── `o.p` on the REGISTER tier. The receiver comes from its
            // frame slot (a `box_regs` member: either the element this loop just
            // read, or a live-in the region never writes), the 8-way probe runs
            // call-free on a hit, and the result is tag-guarded into the dst's
            // f64 home. See `emit_regalloc_ic_probe` for the register contract —
            // it is the reason this is a bespoke probe and not `emit_ic_probe`.
            Instr::GetProp { dst, obj, name } if plan.getprop_ips.contains(&ip) => {
                let h = heap.expect("getprop_ips non-empty implies heap helpers");
                let d = xh(&plan, dst);
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();

                // W21: a BOXREF receiver may be the row's one OWN accessor.
                // Intercept only the already-planned, byte-for-byte pass-through
                // getter before the IC probe. The guards are the same guards the
                // memory-tier accessor prefix uses, rewritten to clobber only
                // rax/rcx/rdx: r8..r11 are live Bool homes on this tier.
                //
                // No call, allocation or other user code occurs between the
                // receiver-version check and either absolute pointer read. A key
                // mutation/realloc therefore misses before a stale pointer is
                // dereferenced; an in-place __defineGetter__ swap is caught by
                // the independent accessor-function re-read. The field result is
                // dynamically guarded into the numeric home, so Int and double
                // both hit while any other Value falls through to the unchanged
                // probe/site-gate path with no home modified.
                if boxref_own_getter_enabled() {
                    if let Some(gp) = method_plan.and_then(|plans| plans.get(&ip)) {
                        for shape in &gp.shapes {
                            let Some((acc_addr, acc_bits, field_slot)) =
                                boxref_passthrough_getter(shape)
                            else {
                                continue;
                            };
                            own_getter_arms += 1;
                            let next = ops.new_dynamic_label();
                            dynasm!(ops
                                ; mov rax, [rbx + dreg(obj)]
                                ; mov rcx, QWORD shape.recv_bits as i64
                                ; cmp rax, rcx
                                ; jne => next
                                ; mov ecx, eax
                                ; mov edx, [r13 + rcx * 4]
                                ; cmp edx, DWORD shape.recv_ver as i32
                                ; jne => next
                                // The version check MUST precede both baked-address
                                // reads: either Vec may have reallocated.
                                ; mov rcx, QWORD acc_addr as i64
                                ; mov rcx, [rcx]
                                ; mov rdx, QWORD acc_bits as i64
                                ; cmp rcx, rdx
                                ; jne => next
                                ; mov rcx, QWORD shape.vals_ptr as i64
                                ; mov rax, [rcx + (field_slot as i32) * 8]
                            );
                            emit_box_to_home(&mut ops, d, next);
                            dynasm!(ops
                                ; jmp => done
                                ; => next
                            );
                        }
                    }
                }
                emit_regalloc_ic_probe(
                    &mut ops, h, ic_site, obj, name, probe_off, spill_off, deopt,
                );
                // rax = the property's Value bits. Int → cvtsi2sd, double → movq,
                // anything else (a string, an object, undefined, a bool) DEOPTs:
                // the dst is an f64 home and cannot hold it. Nothing has been
                // written yet, so re-execution at this ip is sound.
                emit_box_to_home(&mut ops, d, deopt);
                dynasm!(ops
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], ip as i32
                    ; jmp => flush_exit
                    ; => done
                );
                ic_site += 1;
                copy_clobber(&mut lc, d);
                lc = None;
            }
            // POST-PLAN hole: this region passed `region_can_compile` AND
            // `plan_region` (which types e.g. `Mod` and `MathOp{Imul,2}` as Num
            // defs), but this emitter has no arm for the op. The decline must be
            // NAMED: an unnamed fall to the memory tier makes the ABSENT
            // [decline-reason] line read as "runs regalloc" under the documented
            // tier-attribution rule (B32/B53). Behavior unchanged — still None.
            _ => {
                decline_emit(format_args!(
                    "regalloc-emit-unhandled: {:?}",
                    proto.code[ip]
                ));
                return None;
            }
        }
        // ── B94/B97 write-through ── a numeric def of a split receiver (B94) or
        // of a shared home read after the region (B97) must reach MEMORY as well
        // as its home, because `flush_exit` deliberately skips these registers
        // and memory is what the interpreter reads on any exit. Two
        // instructions, once per def; the receiver `LoadGlobal` half already
        // stored the object, and `wt_def_at` is what keeps this store off that
        // ip — for EITHER set, since a register can be in both. A
        // `dv_flag_elide` ip is NOT that class and deliberately keeps its
        // write-through: plan_region's "DV endian-flag fusion" admission proves
        // every exit inside the fused window resumes inside it and re-runs the
        // killing def, so that store is load-bearing, not symmetrical noise.
        if let Some(d) = wt_def_at(proto, &plan, ip) {
            if plan.split_recvs.contains(&d) || plan.write_through.contains(&d) {
                if let Some(&Home::Xmm(h)) = plan.reg_home.get(&d) {
                    dynasm!(ops
                        ; movq rax, Rx(h)
                        ; mov [rbx + dreg(d)], rax
                    );
                }
            }
        }
    }

    // ── exit stubs ── set the resume ip, then flush+restore+ret.
    for (target, label) in &exit_stubs {
        dynasm!(ops
            ; => *label
            ; mov DWORD [rsi], *target as i32
            ; jmp => flush_exit
        );
    }

    // ── flush_exit ── write every home back to the reg file / globals (so the
    // interpreter resumes with consistent state), restore xmm6..15 + the stack,
    // and return. [rsi] already holds the resume ip.
    dynasm!(ops ; => flush_exit);
    for &(r, x) in &plan.num_regs {
        // The B94 split receiver is written through at each def, so memory is
        // already current; flushing its home here would overwrite the receiver
        // object at any exit taken inside the receiver range.
        // B97: a shared home may hold ANOTHER register's value by now; the
        // write-through at each def already put this one's value in its slot.
        if plan.split_recvs.contains(&r) || plan.write_through.contains(&r) {
            continue;
        }
        dynasm!(ops ; movq rax, Rx(x) ; mov [rbx + dreg(r)], rax);
    }
    for &(r, g) in &plan.bool_regs {
        // Box the 0/1 in the gpr into a Bool Value.
        dynasm!(ops
            ; mov rax, QWORD BOOL_TAG as i64
            ; or rax, Rq(g)
            ; mov [rbx + dreg(r)], rax
        );
    }
    for &(gi, x) in &plan.globs {
        dynasm!(ops ; movq rax, Rx(x) ; mov [r12 + (gi as i32) * 8], rax);
    }
    emit_region_restore_n(&mut ops, xmm_off, frame);

    // ── entry_bail ── a live-in type guard failed; nothing was computed yet, so
    // restore (NO flush — reg file / globals are still consistent) and resume at
    // the header. [rsi] is set here to the loop header.
    dynasm!(ops
        ; => entry_bail
        ; mov DWORD [rsi], start as i32
    );
    emit_region_restore_n(&mut ops, xmm_off, frame);

    let buf = match ops.finalize() {
        Ok(b) => b,
        Err(_) => {
            decline_emit("regalloc-emit: assembler finalize failed");
            return None;
        }
    };
    // W7 attribution: code-byte length with pins, so the hoist's size delta is
    // one grep away (run again under ZIPP_NO_GUARD_HOIST=1 and diff the line).
    if !ta_plan.pins.is_empty() && std::env::var_os("ZIPP_JITLOG").is_some() {
        eprintln!(
            "[jit] DOUBLE region [{start},{end}] guard-hoist pins={}/{} code={}b",
            plan.hoist_pins.len(),
            ta_plan.pins.len(),
            buf.len()
        );
        log_pinned_recvs("DOUBLE", start, end, proto, &plan);
    }
    if needs_ic && std::env::var_os("ZIPP_JITLOG").is_some() {
        let mut gps: Vec<usize> = plan.getprop_ips.iter().copied().collect();
        gps.sort_unstable();
        eprintln!(
            "[jit] DOUBLE region [{start},{end}] BOXREF box_regs={} getprops={gps:?} own_getters={own_getter_arms} code={}b",
            plan.box_regs.len(),
            buf.len()
        );
    }
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some((
        JitFn {
            _buf: buf,
            entry: entry_ptr,
            self_binding: None,
        },
        needs_ic,
    ))
}
