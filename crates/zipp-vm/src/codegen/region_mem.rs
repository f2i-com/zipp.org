// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Same-binary A/B switch for the guarded `hasOwnProperty.call` intrinsic.
/// Read while compiling a region, never on the generated hot path.
#[inline]
fn has_own_call_intrinsic_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_HASOWN_CALL_INTRINSIC").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Same-binary escape hatch for the guarded cyclic existing-field store
/// reducer. The switch is sampled while compiling a region, so the ordinary
/// loop pays no branch when the prefix is enabled.
#[inline]
fn field_write_stream_enabled() -> bool {
    std::env::var_os("ZIPP_NO_FIELD_WRITE_STREAM").is_none()
}

#[inline]
fn field_mixed_stream_enabled() -> bool {
    std::env::var_os("ZIPP_NO_FIELD_MIXED_STREAM").is_none()
}

/// Codegen-side copy of `vm::field_stream`'s exact bytecode recognition.  This
/// is only an admission filter: the FFI helper repeats every structural and
/// runtime guard before committing a write, and a miss falls through to the
/// unchanged MEM region.
fn field_write_stream_shape(proto: &FuncProto, start: usize, end: usize) -> bool {
    if end.checked_sub(start) != Some(13) || end + 1 >= proto.code.len() {
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
    let (receiver, k) = match &c[start + 2] {
        Instr::GetIndex { dst, key, .. } => (*dst, *key),
        _ => return false,
    };
    let value = match &c[start + 3] {
        Instr::Move { dst, src } if *src == i => *dst,
        _ => return false,
    };
    if !matches!(&c[start + 4],
        Instr::SetProp { obj, val, strict: false, .. }
            if *obj == receiver && *val == value)
        || !matches!(&c[start + 5],
            Instr::AddInt { dst, a, imm: 1, upd: true } if *dst == k && *a == k)
        || !matches!(&c[start + 6], Instr::Move { src, .. } if *src == k)
    {
        return false;
    }
    let (flag, n) = match &c[start + 7] {
        Instr::Eq { dst, a, b } if *a == k => (*dst, *b),
        _ => return false,
    };
    matches!(&c[start + 8], Instr::JumpIfFalse { cond, target }
            if *cond == flag && *target as usize == start + 11)
        && matches!(&c[start + 9], Instr::LoadInt { dst, val: 0 } if *dst == k)
        && matches!(&c[start + 10], Instr::Move { src, .. } if *src == k)
        && matches!(&c[start + 11],
            Instr::AddInt { dst, a, imm: 1, upd: true } if *dst == i && *a == i)
        && matches!(&c[start + 12], Instr::Move { src, .. } if *src == i)
        && matches!(&c[start + 13], Instr::Jump { target } if *target as usize == start)
        && limit != i
        && n != i
}

/// Cheap admission copy for the exact global cyclic read/write reducer.  The
/// FFI helper repeats the complete register/global/name relationship proof, so
/// this screen only prevents unrelated regions from paying a prefix call.
fn field_mixed_stream_shape(proto: &FuncProto, start: usize, end: usize) -> bool {
    use crate::bytecode::BitwiseOp;
    if end.checked_sub(start) != Some(26) || end + 1 >= proto.code.len() {
        return false;
    }
    let c = &proto.code;
    matches!(&c[start], Instr::LoadGlobal { .. })
        && matches!(&c[start + 1], Instr::LoadGlobal { .. })
        && matches!(&c[start + 2], Instr::JumpIfNotLt { target, .. }
            if *target as usize == end + 1)
        && matches!(&c[start + 3], Instr::LoadGlobal { .. })
        && matches!(
            &c[start + 6],
            Instr::Bitwise {
                op: BitwiseOp::And,
                ..
            }
        )
        && matches!(&c[start + 7], Instr::GetIndex { .. })
        && matches!(&c[start + 8], Instr::StoreGlobal { .. })
        && matches!(
            &c[start + 12],
            Instr::Bitwise {
                op: BitwiseOp::And,
                ..
            }
        )
        && matches!(&c[start + 15], Instr::SetProp { strict: false, .. })
        && matches!(&c[start + 18], Instr::GetProp { .. })
        && matches!(
            &c[start + 21],
            Instr::Bitwise {
                op: BitwiseOp::Or,
                ..
            }
        )
        && matches!(&c[start + 26], Instr::Jump { target } if *target as usize == start)
}

/// Memory-based region codegen: every op loads operands from the register file
/// (with a type guard) and stores results back, globals via the pinned base
/// pointer. Correct and simple; ~4x faster than the interpreter but leaves
/// per-iteration memory traffic on the table (the register-promoting path above
/// removes it). Kept as the fallback for regions the allocator declines.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_region_mem(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    heap: HeapHelpers,
    const_strs: &FxHashMap<u32, u64>,
    ta_plan: &TaPinPlan,
    leaf_plan: &FxHashMap<usize, LeafInlinePlan>,
    method_plan: &FxHashMap<usize, MethodInlinePlan>,
    // Tier-C cross-call plan (B83): `Call` ips that get the native→native
    // cross-call attempt (fallback: the unchanged `call_ic` helper).
    cross_plan: &CrossCallPlan,
    // Per-site accessor-arm emission flags (the SITE GATE — indexed by the
    // local site number, `ic_site - heap.ic_base_idx`); built by
    // `Jit::register_ic_sites` from the ops that have actually filled an
    // accessor way. `ZIPP_ACC_ALWAYS_EMIT=1` sets every flag (wave-2's shape).
    ic_emit: &[IcSiteEmit],
    meter: Option<crate::codegen::meter::Meter>,
) -> Option<JitFn> {
    if !region_can_compile(proto, start, end, Some(const_strs)) {
        return None;
    }
    let scalar_matchall = rx_scalar_matchall_plan(proto, start, end);
    let scalar_exec = rx_scalar_exec_plan(proto, start, end, ta_plan);
    // Scalarization elides source bytecodes and therefore their individual
    // meter charges. Metered execution retains the byte-for-byte ordinary
    // region/interpreter path rather than under-counting steps.
    if (scalar_matchall.is_some() || scalar_exec.is_some()) && meter.is_some() {
        return None;
    }
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let (s, e) = (start as usize, end as usize);

    if std::env::var_os("ZIPP_JITDUMP").is_some() {
        for ip in s..=e {
            eprintln!("[dump] {ip}: {:?}", proto.code[ip]);
        }
    }

    // Does the region use the r13/r14 inline-cache pointers at all? GetProp/
    // SetProp use both. The call-free ForInLive version guard below also reads
    // r13, but only when its engine-private snapshot Array already has a dense-
    // Array pin (normally supplied by the adjacent GetIndex of the key).
    let has_prop = proto.code[s..=e]
        .iter()
        .any(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }));
    let forin_snapshot_pin = |obj: u16| {
        ta_plan
            .pins
            .iter()
            .position(|p| p.src == TaPinSrc::Reg(obj) && is_arr_pin(p.kind))
    };
    let has_forin_version_inline = forin_version_fast_enabled()
        && proto.code[s..=e].iter().any(|instr| match *instr {
            Instr::ForInLive { obj, .. } => forin_snapshot_pin(obj).is_some(),
            _ => false,
        });
    // ── Q4 leaf-call inlining ── the highest scratch slot any inlined callee
    // uses above the caller window (`reg_window + callee_reg_count`). Checked
    // ONCE at entry by `jit_regs_fits`; the result gates each inlined Call (a
    // tight-headroom run falls back to the per-call helper for every site).
    let do_leaf = !leaf_plan.is_empty();
    let do_method = !method_plan.is_empty();
    // The Q4 leaf-inline identity guard re-checks the callee slot's live version
    // (read from r13, the pinned heap version-array base) to defeat GC slot-reuse
    // ABA. r13 is pinned at the prologue, but any intervening ALLOCATING / user-
    // code helper (jit_concat, a fallback call, …) can reallocate the versions
    // Vec and leave r13 STALE. So whenever the region inlines a call, the version
    // base must be re-derived after such helpers too — exactly where a GetProp/
    // SetProp region re-derives it. Fold `do_leaf` into the refetch gate.
    let refetch_pinned = has_prop || do_leaf || do_method || has_forin_version_inline;
    let max_scratch_top: u64 = leaf_plan
        .values()
        .map(|p| p.reg_window as u64 + p.callee_reg_count as u64)
        .chain(method_plan.values().map(|p| p.win_top as u64))
        .max()
        .unwrap_or(0);
    // Pinned-TypedArray snapshot slots: 32 bytes each, above the 32B shadow +
    // 8B 5th-arg slot. 32*n keeps the frame's 16-alignment. The leaf-inline
    // headroom flag adds one more 16B slot at the top of the frame.
    let n_ta = ta_plan.pins.len();
    let frame = 40 + 32 * n_ta as i32 + if do_leaf || do_method { 16 } else { 0 };
    // Byte offset (from post-prologue rsp) of the headroom flag slot (1 = the
    // scratch window fits → inline; 0 = fall back to the per-call helper).
    let leaf_flag_off = frame - 8;
    // Re-derive the pins after any helper that can run user code.
    let ta_refetch = (n_ta > 0).then_some((heap.ta_snapshot, ta_plan));
    // Registers fed by a DOUBLE constant (`x * 1.5`, `i * 2654435761`): their
    // arithmetic skips the Int+Int fast path (it would fail every iteration).
    // Pure perf heuristic — a multiply-defined reg merely keeps the check.
    let mut const_dbl_regs: FxHashSet<u16> = FxHashSet::default();
    for instr in &proto.code[s..=e] {
        if let Instr::LoadConst { dst, idx } = *instr {
            if proto
                .constants
                .get(idx as usize)
                .is_some_and(|c| c.is_double())
            {
                const_dbl_regs.insert(dst);
            }
        }
    }
    let int_hint = |a: u16, b: u16| !const_dbl_regs.contains(&a) && !const_dbl_regs.contains(&b);

    // One label per in-region ip (offset by `start`). Out-of-region jump targets
    // resolve to lazily-created exit stubs.
    let in_region: Vec<_> = (s..=e).map(|_| ops.new_dynamic_label()).collect();
    let mut exit_stubs: FxHashMap<u32, dynasmrt::DynamicLabel> = FxHashMap::default();
    let epilogue = ops.new_dynamic_label();
    // Resume in the interpreter at the loop header if the hoisted `.length`
    // compute deopts at entry (`g` isn't a string/array).
    let entry_len_bail = ops.new_dynamic_label();
    let lbl = |ip: u32, in_region: &[dynasmrt::DynamicLabel]| in_region[(ip - start) as usize];
    // Step metering (a metered VM only) — see codegen::meter.
    let blocks = crate::codegen::meter::block_map(meter, &proto.code, s, e);

    // ── prologue ── save callee-saved, stash inputs, fetch globals base, jump to
    // the loop header (OSR entry).
    dynasm!(ops
        ; push rbx
        ; push rsi
        ; push rdi
        ; push r12
        ; push r13
        ; push r14
        ; sub rsp, frame                  // 32B shadow + 8B 5th-arg slot + 32B/TA pin ⇒ rsp 16-aligned
        ; mov rbx, rcx                    // regs base
        ; mov rsi, rdx                    // resume_ip out-pointer
        ; mov rdi, r8                     // vm
        ; mov rcx, rdi                    // arg0 = vm
        ; mov rax, QWORD globals_base_helper as i64
        ; call rax
        ; mov r12, rax                    // pinned globals base pointer
        ; mov rcx, rdi
        ; mov rax, QWORD heap.versions_base as i64
        ; call rax
        ; mov r13, rax                    // pinned heap version-array base (IC)
        ; mov rcx, rdi
        ; mov rax, QWORD heap.ic_base as i64
        ; call rax
        ; mov r14, rax                    // pinned inline-cache table base
    );
    // A captured-limit cyclic field read cannot stay on the DOUBLE tier because
    // its ordinary fallback header executes `UpvalGet`. Host that exact shape
    // here, where the unchanged MEM region already implements the fallback op.
    // The helper repeats the complete recognition and receiver/effect preflight;
    // a miss has changed no JS state and falls through below.
    let captured_field_read_prefix = meter.is_none()
        && field_read_stream_enabled()
        && matches!(proto.code[s], Instr::UpvalGet { .. })
        && field_cyclic_read_stream_shape(proto, s, e);
    if captured_field_read_prefix {
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!(
                "[jit] MEM region fn{} [{start},{end}] upvalue-field-read-stream prefix",
                heap.func_id
            );
        }
        let packed = ((heap.func_id as u64) << 32) | ((s as u64) << 16) | e as u64;
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
            ; jmp => epilogue
            ; => fallback
        );
    }

    // The property-shape write phase is an exact cyclic loop whose only
    // observable effects are existing own-data-slot stores.  A guarded helper
    // preflights every receiver, then commits only the final value of each
    // touched slot and publishes the loop-carried locals.  It runs before any
    // TA pin or region-local state is materialized, so a guard miss has changed
    // nothing and simply enters the byte-identical ordinary region below.
    let field_write_prefix = (field_write_stream_enabled()
        && field_write_stream_shape(proto, s, e))
        || (field_mixed_stream_enabled() && field_mixed_stream_shape(proto, s, e));
    if meter.is_none() && field_write_prefix {
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!(
                "[jit] MEM region fn{} [{start},{end}] field-write-stream prefix",
                heap.func_id
            );
        }
        let packed = ((heap.func_id as u64) << 32) | ((s as u64) << 16) | e as u64;
        let fallback = ops.new_dynamic_label();
        dynasm!(ops
            ; mov rcx, rdi
            ; mov rdx, rbx
            ; mov r8, QWORD packed as i64
            ; mov rax, QWORD crate::vm::jit_field_write_loop as usize as i64
            ; call rax
            ; test rax, rax
            ; je => fallback
            ; mov DWORD [rsi], (e + 1) as i32
            ; jmp => epilogue
            ; => fallback
        );
    }
    // Pin each TypedArray's `{obj_bits, base, len}` snapshot (entry derivation).
    if let Some((snap, plan)) = ta_refetch {
        emit_refetch_ta(&mut ops, snap, plan);
    }
    // ── Q4 leaf-inline headroom check (once per entry) ── `jit_regs_fits(vm,
    // rbx, max_scratch_top)` → 1 if the carved scratch windows lie inside the
    // pinned register file (the common case). Stash the 0/1 in the flag slot;
    // each inlined Call site reads it and falls back to the helper on 0. rbx is
    // callee-saved (survives the call); rcx/rdx/r8 are volatile scratch here.
    if do_leaf || do_method {
        dynasm!(ops
            ; mov rcx, rdi                            // vm
            ; mov rdx, rbx                            // caller window base
            ; mov r8, QWORD max_scratch_top as i64    // highest scratch slot used
            ; mov rax, QWORD heap.regs_fits as i64
            ; call rax
            ; mov [rsp + leaf_flag_off], rax          // 1 = inline ok, 0 = helper
        );
    }

    // ── loop-invariant `g.length` hoist ── compute it ONCE here (reusing the
    // GetProp miss helper, which returns string/array `.length` directly) instead
    // of a helper call every iteration. The body skips the hoisted GetProp, so its
    // dst keeps this value. If `g` isn't a string/array at entry the helper deopts
    // → resume the loop in the interpreter (it recomputes `.length` correctly).
    let hoisted_len = hoistable_length(proto, start, end);
    if let Some((_get_ip, dst, g, name_idx)) = hoisted_len {
        let packed = ((heap.func_id as u64) << 32) | name_idx as u64;
        dynasm!(ops
            ; mov rdx, [r12 + (g as i32) * 8]     // obj bits = globals[g]
            ; mov rcx, rdi                         // vm
            // Pseudo-site: u32::MAX makes any fill a no-op (`set_ic` ignores
            // it). A real site id here could cross-pollute another site's ways
            // with a DIFFERENT KEY's slot (same receiver identity → wrong slot).
            ; mov r8d, -1                          // site_idx = u32::MAX

            ; mov r9, QWORD packed as i64
            ; mov rax, QWORD heap.get_prop_miss as i64
            ; call rax
            ; mov r10, QWORD SELF_CALL_DEOPT as i64
            ; cmp rax, r10
            ; je => entry_len_bail
            ; mov r10, QWORD PROP_VIA_IC as i64       // accessor `length` etc.
            ; cmp rax, r10
            ; je => entry_len_bail
            ; mov [rbx + dreg(dst)], rax
        );
    }
    dynasm!(ops ; jmp => lbl(start, &in_region));

    // B118 fused compare→branch: `cmp {dst} ; JumpIfTrue/False{cond: dst}` at
    // the very next ip fuses (see `emit_fused_cmp_branch_head`). Detection
    // only — the JumpIf stays emitted (it is a jump target of chained `||`/`&&`
    // arms). Declined under step metering: the fused branch would skip the
    // JumpIf block's charge.
    let cmp_branch_pair = |ip: usize, dst: u16| -> Option<(bool, u32)> {
        if !mem_cmp_fuse_enabled() || blocks.is_some() || ip + 2 > e {
            return None;
        }
        match proto.code[ip + 1] {
            Instr::JumpIfFalse { cond, target } if cond == dst => Some((true, target)),
            Instr::JumpIfTrue { cond, target } if cond == dst => Some((false, target)),
            _ => None,
        }
    };
    // The k-th GetProp/SetProp in the region uses inline-cache site `ic_site`.
    let mut ic_site = heap.ic_base_idx;
    for ip in s..=e {
        // Skip the hoisted `.length` GetProp — its dst already holds the value
        // (computed once in the prologue). The label is still emitted so jumps
        // into this ip resolve; the op itself is elided.
        if let Some((get_ip, ..)) = hoisted_len {
            if ip == get_ip {
                dynasm!(ops ; => lbl(ip as u32, &in_region));
                continue;
            }
        }
        let ipl = lbl(ip as u32, &in_region);
        dynasm!(ops ; => ipl);
        crate::codegen::meter::charge_block(&mut ops, &blocks, ip, &mut exit_stubs);
        if let Some(plan) = scalar_exec.filter(|p| p.result_reload_ip == ip) {
            // The call helper left its true/null control value in
            // call_result_reg. Mirror the skipped LoadGlobal into its original
            // destination (the allocator need not reuse the call register).
            dynasm!(ops
                ; mov rax, [rbx + dreg(plan.call_result_reg)]
                ; mov [rbx + dreg(plan.result_test_reg)], rax
            );
            continue;
        }
        if scalar_exec.is_some_and(|p| p.elides_capture_ip(ip)) {
            // The helper wrote the four future ToNum destinations; these exact
            // publication/capture-only bytecodes have no remaining source effect.
            continue;
        }
        let bail = ops.new_dynamic_label();
        match proto.code[ip] {
            Instr::LoadInt { dst, val } => {
                let boxed = INT_TAG | (val as u32 as u64);
                dynasm!(ops
                    ; mov rax, QWORD boxed as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadConst { dst, idx } => {
                // A single-ASCII-char string constant materialises as its
                // INTERNED slot (the same boxed Value `s[i]` yields), so a later
                // `=== "x"` is a bits compare; a multi-char string constant uses
                // the bits interned at REGION-COMPILE time (rooted for the VM's
                // life in `jit_const_strings`); numeric consts use raw bits.
                let c = proto.constants[idx as usize];
                let bits = single_char_const_bits(proto, c)
                    .or_else(|| const_strs.get(&idx).copied())
                    .unwrap_or_else(|| c.bits());
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::Move { dst, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadGlobal { dst, idx } => {
                dynasm!(ops
                    ; mov rax, [r12 + (idx as i32) * 8]
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::StoreGlobal { idx, src }
            | Instr::StoreGlobalStrict { idx, src }
            | Instr::StoreGlobalResolved { idx, src } => {
                if let Some(plan) = scalar_exec.filter(|p| p.result_store_ip == ip) {
                    let done = ops.new_dynamic_label();
                    // Success is represented by TRUE while the real Array is
                    // pending; a semantic miss is NULL and must publish now.
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(src)]
                        ; mov r10, QWORD Value::TRUE.bits() as i64
                        ; cmp rax, r10
                        ; je => done
                        ; mov [r12 + (plan.result_global as i32) * 8], rax
                        ; => done
                    );
                    continue;
                }
                if scalar_matchall.is_some_and(|p| p.result_store_ip == ip) {
                    // The match range is pending in RegexpIterRec. It becomes
                    // observable in this global only at the final flush (or
                    // before any exit/re-entry); the exact capture consumer
                    // below reads the range directly.
                    continue;
                }
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov [r12 + (idx as i32) * 8], rax
                );
            }
            Instr::Add { dst, a, b } => {
                // Int+Int fast path (32-bit add + overflow check, Int result —
                // the interpreter's `checked_add`), then the numeric f64 path;
                // non-number operands (strings, objects) fall back to
                // `jit_concat` — the SAME `add_values` the interpreter's Add
                // runs (concat / numeric / coercion). The helper may allocate
                // or run user coercion code, so the pinned pointers are
                // re-derived when the region reads them.
                let slow = ops.new_dynamic_label();
                let f64_path = ops.new_dynamic_label();
                let done_a = ops.new_dynamic_label();
                if int_hint(a, b) {
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(a)]
                        ; mov rcx, [rbx + dreg(b)]
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => f64_path
                        ; mov r10, rcx
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => f64_path
                        ; add eax, ecx
                        ; jo => f64_path          // overflow → f64 (reloads operands)
                    );
                    box_eax(&mut ops, dst);
                    dynasm!(ops ; jmp => done_a);
                }
                dynasm!(ops ; => f64_path);
                load_num_xmm(&mut ops, a, 0, slow);
                load_num_xmm(&mut ops, b, 1, slow);
                dynasm!(ops ; addsd xmm0, xmm1);
                store_xmm(&mut ops, dst);
                dynasm!(ops
                    ; jmp => done_a
                    ; => slow
                );
                if let Some(plan) = scalar_matchall.filter(|p| p.add_ip == ip) {
                    // A non-number would enter the ordinary Add helper, which
                    // can invoke valueOf/toString. Publish the pending km first,
                    // then exit and let the interpreter execute this Add once.
                    // No native re-entry means global-route and pin invariants
                    // cannot be invalidated behind the scalar plan.
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + dreg(plan.iter_reg)]
                        ; mov r8d, plan.result_global as i32
                        ; mov r9d, 1                         // slow/re-entry census
                        ; mov rax, QWORD heap.regexp_scalar_flush as i64
                        ; call rax
                        ; mov DWORD [rsi], ip as i32
                        ; jmp => epilogue
                    );
                } else if let Some(plan) = scalar_exec.filter(|p| p.add_ips.contains(&ip)) {
                    // Any non-number would enter observable ToPrimitive.  The
                    // exact result must exist in its global before replaying
                    // this Add in the interpreter.
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov edx, plan.result_global as i32
                        ; mov r8d, 1                         // slow/re-entry
                        ; mov rax, QWORD heap.regexp_scalar_exec_flush as i64
                        ; call rax
                        ; mov DWORD [rsi], ip as i32
                        ; jmp => epilogue
                    );
                } else {
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(a)]
                        ; mov r8, [rbx + dreg(b)]
                        ; mov rax, QWORD heap.concat as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail                          // IC-style redo (nothing ran)
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => bail                          // threw (pending_throw set) → unwind, NOT redo
                        ; mov [rbx + dreg(dst)], rax
                    );
                    if refetch_pinned {
                        emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                    }
                    // `add_values` can run user coercion code (valueOf) —
                    // re-derive the pinned TypedArray snapshots.
                    if let Some((snap, plan)) = ta_refetch {
                        emit_refetch_ta(&mut ops, snap, plan);
                    }
                }
                dynasm!(ops ; => done_a);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Sub { dst, a, b } => dbinop(
                &mut ops,
                ip,
                bail,
                epilogue,
                dst,
                a,
                b,
                DOp::Sub,
                int_hint(a, b),
            ),
            Instr::Mul { dst, a, b } => dbinop(
                &mut ops,
                ip,
                bail,
                epilogue,
                dst,
                a,
                b,
                DOp::Mul,
                int_hint(a, b),
            ),
            Instr::Div { dst, a, b } => {
                dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Div, false)
            }
            Instr::Mod { dst, a, b } => {
                // `a % b` for INTEGER-valued operands via i64 idiv (exact, and the
                // remainder takes the dividend's sign — JS `%` for integers).
                // Non-integer operands or `% 0` bail to the interpreter (true fmod
                // / NaN). xmm2/rax/rcx/rdx are scratch in this memory path.
                load_num_xmm(&mut ops, a, 0, bail); // xmm0 = a
                load_num_xmm(&mut ops, b, 1, bail); // xmm1 = b
                let as_dbl = ops.new_dynamic_label();
                let mod_done = ops.new_dynamic_label();
                let rem_signed = ops.new_dynamic_label();
                dynasm!(ops
                    ; cvttsd2si rax, xmm0            // a → i64 (trunc toward 0)
                    ; cvttsd2si rcx, xmm1            // b → i64
                    ; test rcx, rcx
                    ; jz => bail                     // % 0 → NaN (interp)
                    // idiv #DE guard: i64::MIN % -1 overflows the quotient and
                    // faults the process (reachable: a == -(2^63) round-trips the
                    // integer guard exactly). `a % -1` is ±0 and rare — interp.
                    ; cmp rcx, -1
                    ; je => bail
                    ; cvtsi2sd xmm2, rax
                    ; ucomisd xmm2, xmm0
                    // NaN compares UNORDERED (ZF=PF=CF=1), so `jne` does not
                    // take — the guard fell through and ran `idiv` on the
                    // integer-indefinite i64::MIN that `cvttsd2si` produced.
                    // `NaN % 1` returned 0, and `NaN % -1` raised #DE and killed
                    // the process (i64::MIN / -1 overflows the quotient). The
                    // rest of the codegen pairs `jne` with `jp` — these three
                    // copies of this block did not.
                    ; jp => bail                     // a is NaN → fmod (interp)
                    ; jne => bail                    // a not integer-valued → fmod
                    ; cvtsi2sd xmm2, rcx
                    ; ucomisd xmm2, xmm1
                    ; jp => bail                     // b is NaN → fmod (interp)
                    ; jne => bail                    // b not integer-valued → fmod
                    ; cqo                            // sign-extend rax into rdx:rax
                    ; idiv rcx                       // rdx = a % b (i64 remainder)
                    // Zero remainder from a NEGATIVE dividend (including -0.0,
                    // which passes the integer-valued guard because 0.0 == -0.0)
                    // is -0 in JS. Boxing it as Int(0) loses that. xmm0 still
                    // holds the original dividend; rax is dead after the idiv.
                    ; test rdx, rdx
                    ; jnz => rem_signed
                    ; movq rax, xmm0
                    ; test rax, rax
                    ; js => bail
                    ; => rem_signed
                    // Box the remainder as an Int Value when it fits i32 (it does
                    // for any |b| ≤ 2^31). Keeping it Int — not a double — means a
                    // downstream `s += (i%k)` concat hits the interned-digit fast
                    // path instead of allocating a string per element.
                    ; movsxd r8, edx
                    ; cmp r8, rdx
                    ; jne => as_dbl
                    ; mov r8, QWORD INT_TAG as i64
                    ; mov eax, edx                   // zero-extend i32 payload
                    ; or rax, r8
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => mod_done
                    ; => as_dbl
                    ; cvtsi2sd xmm0, rdx             // large remainder → double Value
                    ; movq rax, xmm0
                    ; mov [rbx + dreg(dst)], rax
                    ; => mod_done
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::AddInt { dst, a, imm, .. } => {
                // Int fast path (the interpreter's `checked_add` — keeps loop
                // counters Int so element-access keys stay on their cheap
                // path), f64 fallback otherwise / on overflow.
                let f64_path = ops.new_dynamic_label();
                let done_ai = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(a)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32
                    ; jne => f64_path
                    ; add eax, imm
                    ; jo => f64_path
                );
                box_eax(&mut ops, dst);
                dynasm!(ops ; jmp => done_ai ; => f64_path);
                load_num_xmm(&mut ops, a, 0, bail);
                dynasm!(ops
                    ; mov eax, imm
                    ; cvtsi2sd xmm1, eax
                    ; addsd xmm0, xmm1
                );
                store_xmm(&mut ops, dst);
                dynasm!(ops ; => done_ai);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Neg { dst, a } => {
                // FLIP THE SIGN BIT. `0.0 - a` looks equivalent but is not: under
                // round-to-nearest `0.0 - 0.0` is `+0.0`, so `-(+0)` produced `+0`
                // and `1 / -0` gave `Infinity`. JS negation is defined on the sign
                // bit, and the literal `-0` lowers to `LoadInt 0; Neg`.
                load_num_xmm(&mut ops, a, 1, bail);
                dynasm!(ops
                    ; mov rax, QWORD (1u64 << 63) as i64
                    ; movq xmm0, rax
                    ; xorpd xmm0, xmm1
                );
                store_xmm(&mut ops, dst);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Bitwise { dst, a, b, op } => {
                // ToInt32 both operands (Int payloads or exactly-integral
                // i32-range doubles — see `load_toint32`; everything else
                // bails), then the 32-bit op. x86 32-bit shifts mask the count
                // to 5 bits — exactly JS's `& 31`. Results always fit i32
                // (boxed Int) except `>>>`, whose u32 result may exceed
                // i32::MAX and is then boxed as an (exact) double.
                use crate::bytecode::BitwiseOp as B;
                load_toint32(&mut ops, a, bail);
                dynasm!(ops ; mov r8d, eax); // stash a
                load_toint32(&mut ops, b, bail);
                dynasm!(ops ; mov ecx, eax ; mov eax, r8d); // eax = a, ecx = b
                match op {
                    B::And => {
                        dynasm!(ops ; and eax, ecx);
                        box_eax(&mut ops, dst);
                    }
                    B::Or => {
                        dynasm!(ops ; or eax, ecx);
                        box_eax(&mut ops, dst);
                    }
                    B::Xor => {
                        dynasm!(ops ; xor eax, ecx);
                        box_eax(&mut ops, dst);
                    }
                    B::Shl => {
                        dynasm!(ops ; shl eax, cl);
                        box_eax(&mut ops, dst);
                    }
                    B::Shr => {
                        dynasm!(ops ; sar eax, cl);
                        box_eax(&mut ops, dst);
                    }
                    B::Ushr => {
                        let as_dbl = ops.new_dynamic_label();
                        let done_u = ops.new_dynamic_label();
                        dynasm!(ops
                            ; shr eax, cl
                            ; test eax, eax
                            ; js => as_dbl                // u32 > i32::MAX → double
                        );
                        box_eax(&mut ops, dst);
                        dynasm!(ops
                            ; jmp => done_u
                            ; => as_dbl
                            ; mov eax, eax                // zero-extend u32 into rax
                            ; cvtsi2sd xmm0, rax          // exact (< 2^32)
                            ; movq rax, xmm0
                            ; mov [rbx + dreg(dst)], rax
                            ; => done_u
                        );
                    }
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Not { dst, a } => {
                // `!x`: a Bool flips its payload bit in place (the tag survives
                // the xor); anything else asks the read-only `jit_truthy`
                // helper (handles Int/double/heap incl. empty strings and
                // [[IsHTMLDDA]]) and flips its 0/1.
                let slow = ops.new_dynamic_label();
                let done_n = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(a)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, (INT_TAG_HI + 1) as i32   // Bool tag 0x7FFA
                    ; jne => slow
                    ; xor rax, 1                          // flip the payload bit
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => done_n
                    ; => slow
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // value bits
                    ; mov rax, QWORD heap.truthy as i64
                    ; call rax
                    ; xor rax, 1                          // !truthy
                    ; mov r8, QWORD BOOL_TAG as i64
                    ; or rax, r8
                    ; mov [rbx + dreg(dst)], rax
                    ; => done_n
                );
            }
            Instr::LoadBool { dst, val } => {
                // Materialise the boolean Value bits (BOOL_TAG | 0/1) inline.
                let bits = BOOL_TAG | (val as u64);
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::LoadUndefined { dst } => {
                dynasm!(ops
                    ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::TypeOfIs { dst, a, code, neg } => {
                // Fused typeof compare (jit_typeof_is): PURE, total — no bail,
                // no refetch. `code_neg` packs code | (neg << 8).
                let code_neg = code as u32 | ((neg as u32) << 8);
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(a)]           // value bits
                    ; mov r8d, code_neg as i32           // code | neg<<8
                    ; mov rax, QWORD heap.typeof_is as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax         // Bool Value bits
                );
            }
            Instr::LoadNull { dst } => {
                dynasm!(ops
                    ; mov rax, QWORD Value::NULL.bits() as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            Instr::CheckCoercible { src } => {
                // RequireObjectCoercible: a null/undefined operand bails to the
                // interpreter (which throws the TypeError exactly); every other
                // value is a no-op. Pure, call-free, no alloc.
                dynasm!(ops
                    ; mov rax, [rbx + dreg(src)]
                    ; mov r10, QWORD Value::NULL.bits() as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD Value::UNDEFINED.bits() as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::MathOp {
                dst,
                op,
                arg_base,
                argc,
            } => emit_math_op(
                &mut ops,
                ip,
                bail,
                epilogue,
                dst,
                op,
                arg_base,
                argc,
                heap.math_unary,
                heap.math_two,
            ),
            Instr::CellGet { dst, cell } => {
                // Per-op captured-cell read (jit_cell_get). NEVER hoisted: a
                // Call/CallMethod earlier in the region may have run an inner
                // closure that mutated the cell, so the live value is re-read
                // here every execution. A TDZ cell returns SELF_CALL_DEOPT → bail
                // (the interpreter then throws the ReferenceError at this ip).
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(cell)]        // cell Value bits
                    ; mov rax, QWORD heap.cell_get as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                         // TDZ → interpreter throws
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::GetIndexConcat {
                dst,
                obj,
                name,
                key,
            } => {
                // `obj["name" + i]`. The helper answers only the own-DATA hit on
                // a plain object with an Int key — no allocation (the key goes
                // into a reused scratch buffer) and no user code — and deopts on
                // a miss so the interpreter runs the real computed read
                // (prototype chain, accessors, arrays). `packed` keeps the call
                // to four register args.
                let packed: u64 = ((heap.func_id as u64) << 32) | (name as u64);
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(obj)]         // receiver bits
                    ; mov r8, QWORD packed as i64        // (func_id << 32) | name
                    ; mov r9, [rbx + dreg(key)]          // key bits
                    ; mov rax, QWORD heap.get_index_concat as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                         // miss / exotic → interpreter
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::ToConcatKey { dst, src } => {
                // The fused write's evaluation-order shim: identity for
                // primitives and heap strings (pure helper), deopt for a heap
                // value whose ToPrimitive protocol is user code. No refetch.
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(src)]         // value bits
                    ; mov rax, QWORD heap.to_concat_key as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                         // object key → interpreter
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::DeleteIndex { dst, obj, key, .. } => {
                // Narrow ordinary computed delete. The helper accepts only an
                // exact descriptor-free, non-arguments Array and a non-negative
                // tagged Int. That served case always returns true, so `strict`
                // is immaterial; every shape that could return false, throw, run
                // coercion/user code, or need arguments unmapping deopts before
                // mutation and re-executes this ip in the interpreter.
                //
                // Success overwrites at most one existing element with HOLE and
                // bumps the live version word. It cannot allocate/re-enter or
                // reallocate the Vec, so r13/r14 and every pin's identity/base/len
                // remain valid and no post-call refetch is owed.
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!("[jit] MEM array DeleteIndex helper emitted at ip {ip}");
                }
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(obj)]         // receiver bits
                    ; mov r8, [rbx + dreg(key)]          // key bits
                    ; mov rax, QWORD heap.delete_index as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                         // untouched → interpreter
                    ; mov [rbx + dreg(dst)], rax         // Value::TRUE
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::DeleteIndexConcat {
                dst,
                obj,
                name,
                key,
                strict,
            } => {
                // `delete obj["name" + i]` (W19 M3). This is the op that kept
                // the whole delete loop off the JIT: there was no Delete arm at
                // all here, so `[145,155]` DECLINED and was blacklisted.
                //
                // The helper is a thin wrapper over `Vm::delete_index_concat`,
                // the exact function the interpreter arm calls, so there is no
                // guard chain to get wrong and NO deopt sentinel — a Proxy, a
                // global, a frozen object, a non-Int key all go through the
                // same shared waterfall they already did. The one non-Value
                // return is CALL_THREW (strict-mode `delete` of a
                // non-configurable property, a throwing Proxy trap, a throwing
                // key coercion): the delete attempt and any trap side effects
                // ALREADY happened, so the region unwinds and must never
                // re-execute the op.
                //
                // `strict` rides the stack as arg 5 (the SetIndexConcat shape).
                // `dst` is written BEFORE the refetch calls, which clobber the
                // caller-saved registers (the StrConcat ordering).
                //
                // The refetch is MANDATORY and for a stronger reason than the
                // key-add case: a successful delete `Vec::remove`s the slot,
                // shifting every later key down, and calls `bump_version`, so
                // the heap version array, the IC table and any pinned
                // TypedArray snapshot may all have moved or gone stale.
                let packed: u64 = ((heap.func_id as u64) << 32) | (name as u64);
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(obj)]         // receiver bits
                    ; mov r8, QWORD packed as i64        // (func_id << 32) | name
                    ; mov r9, [rbx + dreg(key)]          // key bits
                    ; mov rax, QWORD strict as i64
                    ; mov [rsp + 32], rax                // 5th arg: strict flag
                    ; mov rax, QWORD heap.delete_index_concat as i64
                    ; call rax
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail                         // threw: unwind, do NOT re-execute
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::SetIndexConcat {
                obj,
                name,
                key,
                val,
            } => {
                // `obj["name" + i] = v`. The selected pure helper returns a
                // distinct sentinel for an own writable-data HIT or a
                // proven-clean new append. Those two arms share the
                // interpreter's prebuilt-key proof and cannot allocate a VM
                // heap object, collect, or run user code, so their r13/r14 and
                // TypedArray snapshots remain valid. Every slow case keeps the
                // B86 delegation to `Vm::set_index_concat` and returns generic
                // success / throw / deopt, which retains the historical full
                // refetch. `ZIPP_NO_CONCAT_PURE_APPEND=1` selects the historical
                // helper and emits this arm without a pure-sentinel branch.
                // `val` rides the stack as arg 5 (the set_prop_miss shape).
                let packed: u64 = ((heap.func_id as u64) << 32) | (name as u64);
                let pure_done = heap.concat_pure_append.then(|| ops.new_dynamic_label());
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(obj)]         // receiver bits
                    ; mov r8, QWORD packed as i64        // (func_id << 32) | name
                    ; mov r9, [rbx + dreg(key)]          // key bits
                    ; mov rax, [rbx + dreg(val)]
                    ; mov [rsp + 32], rax                // 5th arg: value bits
                    ; mov rax, QWORD heap.set_index_concat as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                         // exotic/non-Int key → interpreter
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail                         // threw: unwind, do NOT re-execute
                );
                if let Some(done) = pure_done {
                    dynasm!(ops
                        ; mov r10, QWORD CONCAT_SET_PURE as i64
                        ; cmp rax, r10
                        ; je => done                     // pure hit/add: pins stayed valid
                    );
                }
                // Generic success may have allocated or frame-called an
                // inherited setter, so every pinned address/snapshot is stale.
                emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                if let Some(done) = pure_done {
                    dynasm!(ops ; => done);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::ToNum { dst, a } => {
                if scalar_matchall.is_some_and(|p| p.capture_tonum_ip == ip) {
                    // The exact preceding scalar capture helper wrote this
                    // instruction's destination directly.
                    continue;
                }
                // `+x`. A number passes through UNCHANGED — note the raw `mov`
                // rather than a round trip through xmm, which would re-tag an
                // Int as a double and diverge from the interpreter. A non-number
                // routes through `jit_to_num`, which serves a primitive STRING
                // (the pure StringToNumber grammar — the `+km[2]` capture-sum
                // idiom that used to bail the whole region per iteration) and
                // deopts everything else (bool/null/undefined as before, and an
                // object's ToNumber can run a user `valueOf`). Read-only + pure
                // ⇒ no refetch, and re-execution on deopt is always sound.
                // `ZIPP_NO_TONUM_STR=1` restores the plain bail (the pre-change
                // shape) so the string arm can be A/B'd on ONE binary.
                let tonum_str = std::env::var_os("ZIPP_NO_TONUM_STR").is_none();
                let is_num = ops.new_dynamic_label();
                let tn_done = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(a)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32
                    ; je => is_num                       // Int payload
                    ; sub r10d, (INT_TAG_HI + 1) as i32  // 0x7FFA (bool tag)
                    ; cmp r10d, 3                        // high16 in [0x7FFA,0x7FFD] ⇒ not a number
                    ; ja => is_num                       // double
                );
                if tonum_str {
                    dynasm!(ops
                        ; mov rcx, rdi                       // vm
                        ; mov rdx, rax                       // value bits
                        ; mov rax, QWORD heap.to_num as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail                         // non-string → interp ToNumber
                        ; mov [rbx + dreg(dst)], rax
                        ; jmp => tn_done
                    );
                } else {
                    dynasm!(ops ; jmp => bail);
                }
                dynasm!(ops
                    ; => is_num
                    ; mov [rbx + dreg(dst)], rax
                    ; => tn_done
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::CellSet { cell, src } => {
                // Per-op captured-cell write (jit_cell_set). Unconditional — a
                // cell is one heap slot and the store cannot fail, so unlike the
                // reads there is no TDZ sentinel to test. No alloc / no user
                // code, so no r13/r14/TA refetch.
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(cell)]        // cell Value bits
                    ; mov r8, [rbx + dreg(src)]          // value bits
                    ; mov rax, QWORD heap.cell_set as i64
                    ; call rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::UpvalSet { idx, src } => {
                // Per-op upvalue write; resolves the running closure from the TOP
                // frame exactly as UpvalGet does. A malformed closure, captured
                // const / named-function binding, or TDZ cell bails BEFORE the
                // store so the interpreter replays the op with full PutValue
                // semantics.
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov edx, idx as i32                // upvalue index
                    ; mov r8, [rbx + dreg(src)]          // value bits
                    ; mov rax, QWORD heap.upval_set as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                         // malformed closure → interp
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::UpvalGet { dst, idx } => {
                // Per-op upvalue read (jit_upval_get resolves the running closure
                // from the TOP frame). Same no-hoist soundness as CellGet.
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov edx, idx as i32                // upvalue index
                    ; mov rax, QWORD heap.upval_get as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                         // TDZ / malformed → interp
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::ForInLive { dst, obj, key } => {
                // Default-chain Array snapshots carry three (heap index,
                // expected version) pairs in their private prefix. GetIndex has
                // already pinned this immutable snapshot's Vec for the region,
                // so compare those u32 versions directly through r13 and answer
                // TRUE call-free. Identity/eligibility/version misses retain the
                // exact jit_forin_live path, which observes a key deleted between
                // snapshot and visit. ZIPP_NO_FORIN_VERSION_FAST omits this
                // entire prefix probe at compile time for a same-binary A/B.
                let inline_pin = if forin_version_fast_enabled() {
                    forin_snapshot_pin(obj)
                } else {
                    None
                };
                let slow = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                if let Some(slot) = inline_pin {
                    let off = ta_slot_off(slot);
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(obj)]      // snapshot Value bits
                        ; cmp rax, [rsp + off]            // pin still names it?
                        ; jne => slow
                        ; mov r9, [rsp + off + 8]         // snapshot items base
                        ; test r9, r9                     // declined pin
                        ; jz => slow
                        ; mov r10, [r9 + 8]               // receiver expected-version Value
                        ; mov r11, QWORD 0x7FFC_0000_0000_0000u64 as i64
                        ; cmp r10, r11                    // Undefined = ineligible
                        ; je => slow
                        ; mov ecx, [r9]                   // receiver heap index
                        ; mov edx, [r13 + rcx * 4]        // current receiver version
                        ; cmp edx, [r9 + 8]               // expected payload u32
                        ; jne => slow
                        ; mov ecx, [r9 + 16]              // %Array.prototype% index
                        ; mov edx, [r13 + rcx * 4]
                        ; cmp edx, [r9 + 24]
                        ; jne => slow
                        ; mov ecx, [r9 + 32]              // %Object.prototype% index
                        ; mov edx, [r13 + rcx * 4]
                        ; cmp edx, [r9 + 40]
                        ; jne => slow
                        ; mov r10, QWORD BOOL_TRUE_BITS as i64
                        ; mov [rbx + dreg(dst)], r10
                        ; jmp => done
                        ; => slow
                    );
                }
                // Slow/mismatch path: stores the Bool Value bits returned by
                // the shared interpreter implementation. It does no user code;
                // refetching is conservative against a future heap allocation.
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(obj)]         // snapshot bits
                    ; mov r8, [rbx + dreg(key)]          // key bits
                    ; mov rax, QWORD heap.forin_live as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax         // Bool Value bits
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
                if inline_pin.is_some() {
                    dynasm!(ops ; => done);
                }
            }
            Instr::HasProp {
                dst,
                key,
                obj,
                brand: _,
            } => {
                // `key in obj` (region_can_compile admitted only brand=false).
                // ── pinned dense-Array fast path ── when the OSR plan pinned
                // this receiver (ARR_PIN_KIND): identity-guard, then an INTEGER
                // key in `[0, len)` whose element is NOT a HOLE answers `true`
                // call-free (an in-range present element is unconditionally an
                // own property — the prototype chain is irrelevant). Every other
                // case (guard miss / declined-snapshot all-zero slot / non-Int
                // key / OOB / a HOLE) routes to the generic `jit_has_property`
                // helper, which walks the real prototype chain (an OOB/hole index
                // can still be inherited) — so the inline never INVENTS a `false`.
                // This is the 80%-present hot path of the hole-iter `if (i in
                // packed)` loop; the read-only inline neither allocates nor moves
                // the Vec, so no refetch.
                let pinned = ta_plan
                    .access
                    .get(&ip)
                    .map(|&j| (j as usize, ta_plan.pins[j as usize].kind))
                    .filter(|&(_, kind)| is_arr_pin(kind));
                let hp_slow = ops.new_dynamic_label();
                let hp_done = ops.new_dynamic_label();
                if let Some((slot, _)) = pinned {
                    let off = ta_slot_off(slot);
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(obj)]      // receiver bits
                        ; cmp rax, [rsp + off]            // identity vs snapshot
                        ; jne => hp_slow                  // miss/declined → helper
                        // Int-tag key only (a double / heap / other key → helper,
                        // which runs the full coercion / chain walk).
                        ; mov rcx, [rbx + dreg(key)]
                        ; mov r10, rcx
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => hp_slow                  // non-Int key → helper
                        ; movsxd rcx, ecx                 // Int payload (may be < 0)
                        ; cmp rcx, [rsp + off + 16]       // unsigned: i < len?
                        ; jae => hp_slow                  // OOB/negative → helper (proto walk)
                        ; mov rdx, [rsp + off + 8]        // pinned items base
                        ; mov rax, [rdx + rcx * 8]        // items[i] (Value bits)
                        ; mov r10, QWORD ARR_HOLE_BITS as i64
                        ; cmp rax, r10
                        ; je => hp_slow                   // HOLE (absent own) → helper (proto walk)
                        ; mov r10, QWORD BOOL_TRUE_BITS as i64
                        ; mov [rbx + dreg(dst)], r10      // in-range present → true
                    );
                    // W19 (M5): FUSED HasProp → JumpIf, B118's idiom extended.
                    // The inline above has already PROVEN the answer is `true`,
                    // and the very next ip is a `JumpIfTrue/False` on this exact
                    // `dst` — which would reload the bool it just stored, tag-
                    // dispatch it (Int? Bool? else call `jit_truthy`) and test it,
                    // ~8 instructions to re-derive a constant. Branch straight to
                    // the resolved destination instead: taken for `JumpIfTrue`,
                    // the ip after the pair for `JumpIfFalse`.
                    //
                    // B118's two constraints carry over VERBATIM and neither is
                    // relaxed: (1) the store to `dst` STAYS — `cmp_branch_pair`
                    // does not prove `dst` dead after the JumpIf, a chained
                    // `||`/`&&` arm can jump straight to the JumpIf ip, and the
                    // deopt contract wants the register file exact at every ip
                    // boundary; (2) the JumpIf is STILL EMITTED at ip+1 and stays
                    // reachable — this only skips it on the one path that already
                    // knows the answer. The helper path (`hp_slow`) is untouched
                    // and still falls into it, including its SELF_CALL_DEOPT bail.
                    // `cmp_branch_pair` itself declines under step metering, so
                    // the elided block cannot go uncharged.
                    let fused = if hasprop_jumpfuse_enabled() {
                        cmp_branch_pair(ip, dst)
                    } else {
                        None
                    };
                    match fused {
                        Some((true, tgt)) => {
                            // JumpIfFalse on a `true` condition: not taken.
                            let _ = tgt;
                            let ft = lbl((ip + 2) as u32, &in_region);
                            dynasm!(ops ; jmp => ft);
                        }
                        Some((false, tgt)) => {
                            // JumpIfTrue on a `true` condition: taken.
                            let t = region_target(
                                tgt,
                                start,
                                end,
                                &in_region,
                                &mut exit_stubs,
                                &mut ops,
                            );
                            dynasm!(ops ; jmp => t);
                        }
                        None => dynasm!(ops ; jmp => hp_done),
                    }
                    dynasm!(ops ; => hp_slow);
                }
                // The read-only `jit_has_property` helper returns the BOOL Value
                // bits, or SELF_CALL_DEOPT → bail (the interpreter re-executes the
                // op: throws on a non-object RHS, runs an object-key ToString, or
                // dispatches a Proxy `has` trap). PURE — no alloc, no user code,
                // so no r13/r14/TA refetch.
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(key)]         // key bits (arg1)
                    ; mov r8, [rbx + dreg(obj)]          // obj bits (arg2)
                    ; mov rax, QWORD heap.has_property as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                         // proxy / coercion / throw → interp
                    ; mov [rbx + dreg(dst)], rax         // Bool Value bits
                );
                if pinned.is_some() {
                    dynasm!(ops ; => hp_done);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
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
                // B118: Int+Int compare feeding the NEXT ip's JumpIf branches on
                // flags (bool still stored); non-Int falls through to the
                // unchanged generic sequence below.
                if let Some((iff, tgt)) = cmp_branch_pair(ip, dst) {
                    let t = region_target(tgt, start, end, &in_region, &mut exit_stubs, &mut ops);
                    let ft = lbl((ip + 2) as u32, &in_region);
                    emit_fused_cmp_branch_head(&mut ops, dst, a, b, cmp, iff, t, ft);
                }
                match cmp {
                    // `===` / `!==` are polymorphic: numeric operands compare as
                    // f64, interned single-char strings / Int / Bool / Null /
                    // Undefined compare by bits, non-interned heap operands bail
                    // to the interpreter.
                    Cmp::Eq => region_poly_eq(
                        &mut ops,
                        ip,
                        bail,
                        epilogue,
                        dst,
                        a,
                        b,
                        false,
                        heap.strict_eq,
                    ),
                    Cmp::Ne => region_poly_eq(
                        &mut ops,
                        ip,
                        bail,
                        epilogue,
                        dst,
                        a,
                        b,
                        true,
                        heap.strict_eq,
                    ),
                    _ => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, cmp),
                }
            }
            Instr::Jump { target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                dynasm!(ops ; jmp => t);
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                // An Int/Bool condition tests its payload directly; anything
                // else (double/heap/undefined/null) asks the read-only
                // `jit_truthy` helper — `while (obj)` / `if (!s)` loop shapes
                // stay native instead of deopting every iteration.
                let if_false = matches!(proto.code[ip], Instr::JumpIfFalse { .. });
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                let testit = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(cond)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32          // Int
                    ; je => testit
                    ; cmp r10d, (INT_TAG_HI + 1) as i32    // Bool
                    ; je => testit
                    ; mov rcx, rdi                         // vm
                    ; mov rdx, rax                         // value bits
                    ; mov rax, QWORD heap.truthy as i64
                    ; call rax                             // rax = 0/1
                    ; => testit
                    ; test eax, eax
                );
                if if_false {
                    dynasm!(ops ; jz => t);
                } else {
                    dynasm!(ops ; jnz => t);
                }
            }
            Instr::JumpIfNotLt { a, b, target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                djump_if_not_cmp(&mut ops, ip, bail, epilogue, a, b, Cmp::Lt, t);
            }
            Instr::JumpIfNotLe { a, b, target } => {
                let t = region_target(target, start, end, &in_region, &mut exit_stubs, &mut ops);
                djump_if_not_cmp(&mut ops, ip, bail, epilogue, a, b, Cmp::Le, t);
            }
            Instr::GetProp { dst, obj, name } => {
                // ── 8-way inline cache (CALL-FREE on hit) ── see `emit_ic_probe`
                // for the probe, its register contract and its safety argument.
                // All ways miss ⇒ fall through to the helper, which re-fills one.
                //
                // (The layout note that used to sit here described a superseded
                // entry: stride 40, hops at +24/+32, a `u32::MAX = none`
                // sentinel. The stride is 64, there are five hop pairs spanning
                // +24..+64, and the count lives in `slot_nhops >> 24`.)
                let off = (ic_site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
                let packed = ((heap.func_id as u64) << 32) | name as u64;
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                // The probe owns its own internal labels (`probe`/`next`/`hit`/`hop`);
                // only the two shared with the miss path survive here. `miss` went
                // with them -- it was reached solely by a `jmp` to the instruction
                // after it.
                let via_ic = ops.new_dynamic_label();
                let cont = ops.new_dynamic_label();
                // B114: the ACCESSOR way. `Some` gives the probe a dispatch
                // target for a tagged way hit (the helper call emitted after
                // the via_ic block); `None` emits the prior stream
                // byte-identically. SITE-GATED (the B115 follow-up): only a
                // site whose op has actually filled an accessor way pays the
                // arms — `acc_emit` carries the per-site decision, and the
                // fill helper (`Jit::acc_way_fill_ok`) refuses to tag a way
                // under an arm-less probe (it evicts for a recompile instead).
                let site_emit = ic_emit
                    .get((ic_site - heap.ic_base_idx) as usize)
                    .copied()
                    .unwrap_or_default();
                let acc = site_emit.acc.then(|| ops.new_dynamic_label());
                // Stage 5: inline a trivial class GETTER for this `o.v` site as a
                // per-receiver guard tree (a pure prefix). A hit writes `dst` and
                // jumps to `cont`; all-miss falls through to the IC probe below
                // (which routes a real accessor via PROP_VIA_IC → helper).
                if let Some(gp) = method_plan.get(&ip) {
                    emit_inline_accessor(
                        &mut ops,
                        ip,
                        epilogue,
                        leaf_flag_off,
                        gp,
                        obj,
                        dst,
                        false,
                        cont,
                    );
                }
                emit_ic_probe(
                    &mut ops,
                    IcProbe::Get { dst },
                    obj,
                    off,
                    cont,
                    acc,
                    site_emit.direct_miss,
                );
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // obj_bits (rax survives the probe)
                    ; mov r8d, ic_site as i32             // site_idx
                    ; mov r9, QWORD packed as i64         // (func_id<<32)|name_idx
                    ; mov rax, QWORD heap.get_prop_miss as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD PROP_VIA_IC as i64
                    ; cmp rax, r10
                    ; je => via_ic
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => cont
                    // ── accessor / class receiver: the interpreter-IC slow
                    // helper resolves it (and may frame-call a getter — user
                    // code, so r13/r14 are re-derived afterwards).
                    ; => via_ic
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rbx                        // caller window base
                    ; mov r8, QWORD packed_fip as i64     // (func_id<<32)|ip
                    ; mov r9, QWORD (((name as u64) << 32) | obj as u64) as i64
                    ; mov rax, QWORD heap.get_prop_slow as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if let Some(acc) = acc {
                    // ── accessor-WAY hit (B114): r9 = the matched way, whose
                    // identity/version/hop guards are all live. The helper
                    // dispatches the getter directly — no 8-way-miss + miss-
                    // helper rediscovery round trip. Same return protocol as
                    // get_prop_slow (may frame-call user code, hence the shared
                    // refetch below).
                    let join = ops.new_dynamic_label();
                    dynasm!(ops
                        ; jmp => join
                        ; => acc
                        ; mov [rsp + 32], r9                  // 5th arg: way ptr
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, rbx                        // caller window base
                        ; mov r8, QWORD packed_fip as i64     // (func_id<<32)|ip
                        ; mov r9, QWORD (((name as u64) << 32) | obj as u64) as i64
                        ; mov rax, QWORD heap.get_prop_acc as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                        ; => join
                    );
                }
                emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                // The slow helper may have frame-called user code (accessor) —
                // re-derive the pinned TypedArray snapshots too.
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                dynasm!(ops ; => cont);
                emit_region_bail(&mut ops, ip, bail, epilogue);
                ic_site += 1;
            }
            Instr::SetProp {
                obj,
                name,
                val,
                strict: _,
            } => {
                // ── 8-way inline cache (CALL-FREE write on hit) ── like
                // GetProp, but the helper only ever fills OWN ways here
                // (identity + receiver version fully guard an own writable
                // data slot: any redefinition/freeze/delete/proto change bumps
                // the version), so the probe skips the hop checks.
                let off = (ic_site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
                let packed = ((heap.func_id as u64) << 32) | name as u64;
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                let cont = ops.new_dynamic_label();
                // B114: as in the GetProp arm — `Some` adds the accessor-way
                // dispatch target, `None` keeps the prior byte stream.
                // SITE-GATED as in the GetProp arm above.
                let site_emit = ic_emit
                    .get((ic_site - heap.ic_base_idx) as usize)
                    .copied()
                    .unwrap_or_default();
                let acc = site_emit.acc.then(|| ops.new_dynamic_label());
                // Stage 5: inline a trivial class SETTER for this `o.v = x` site as
                // a per-receiver guard tree (a pure prefix). A hit does the baked
                // store and jumps to `cont`; all-miss falls through to the IC probe
                // (a real setter → PROP_VIA_IC → helper).
                if let Some(sp) = method_plan.get(&ip) {
                    emit_inline_accessor(
                        &mut ops,
                        ip,
                        epilogue,
                        leaf_flag_off,
                        sp,
                        obj,
                        val,
                        true,
                        cont,
                    );
                }
                emit_ic_probe(
                    &mut ops,
                    IcProbe::Set { val },
                    obj,
                    off,
                    cont,
                    acc,
                    site_emit.direct_miss,
                );
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rax                        // obj_bits
                    ; mov r8, [rbx + dreg(val)]           // val_bits
                    ; mov r9, QWORD packed as i64         // (func_id<<32)|name_idx
                    ; mov QWORD [rsp + 32], ic_site as i32 // 5th arg: site_idx (stack)
                    ; mov rax, QWORD heap.set_prop_miss as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD PROP_VIA_IC as i64
                    ; cmp rax, r10
                    ; jne => cont
                    // ── setter / class receiver: interpreter-IC slow helper
                    // (may frame-call a setter — re-derive r13/r14 after).
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rbx                        // caller window base
                    ; mov r8, QWORD packed_fip as i64     // (func_id<<32)|ip
                    ; mov r9, QWORD (((name as u64) << 32) | ((obj as u64) << 16) | val as u64) as i64
                    ; mov rax, QWORD heap.set_prop_slow as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                if let Some(acc) = acc {
                    // ── accessor-WAY hit (B114): r9 = the matched way. The
                    // helper dispatches the setter directly (0 = done), skipping
                    // the miss helper's rediscovery.
                    let join = ops.new_dynamic_label();
                    dynasm!(ops
                        ; jmp => join
                        ; => acc
                        ; mov [rsp + 32], r9                  // 5th arg: way ptr
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, rbx                        // caller window base
                        ; mov r8, QWORD packed_fip as i64     // (func_id<<32)|ip
                        ; mov r9, QWORD (((name as u64) << 32) | ((obj as u64) << 16) | val as u64) as i64
                        ; mov rax, QWORD heap.set_prop_acc as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; => join
                    );
                }
                emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                // The slow helper may have frame-called user code (accessor) —
                // re-derive the pinned TypedArray snapshots too.
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                dynasm!(ops ; => cont);
                emit_region_bail(&mut ops, ip, bail, epilogue);
                ic_site += 1;
            }
            Instr::ToPropKey { dst, obj, src } => {
                // `dst = ToPropertyKey(src)` for `o[k] op= v` / `o[k]++`: a
                // NUMBER key (Int or double) coerces to itself, so the op is a
                // move once the base is known non-nullish (the interpreter's
                // RequireObjectCoercible order). A nullish base (throw) or a
                // non-number key (observable toString/valueOf, or a heap
                // string/Symbol — rare in hot loops) bails to the interpreter.
                let tpk_ok = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(obj)]
                    ; shr rax, 48
                    ; cmp eax, (INT_TAG_HI + 2) as i32     // 0x7FFB Null
                    ; je => bail
                    ; cmp eax, (INT_TAG_HI + 3) as i32     // 0x7FFC Undefined
                    ; je => bail
                    ; mov rax, [rbx + dreg(src)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32          // Int key
                    ; je => tpk_ok
                    ; sub r10d, (INT_TAG_HI + 1) as i32
                    ; cmp r10d, 3                          // Bool/Null/Undef/Heap
                    ; jbe => bail                          //  → interpreter
                    ; => tpk_ok                            // double key
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::GetIndex { dst, obj, key } => {
                if let Some(plan) = scalar_exec.filter(|p| p.input_get_ip == ip) {
                    let decline = ops.new_dynamic_label();
                    let done = ops.new_dynamic_label();
                    let off = ta_slot_off(plan.input_pin_slot);
                    // Unlike the ordinary pinned-Array lane, every miss exits:
                    // falling through to jit_get_index could invoke a prototype
                    // getter while the previous logical `m` is still pending.
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(obj)]
                        ; cmp rax, [rsp + off]
                        ; jne => decline
                    );
                    emit_ta_key(&mut ops, key, decline);
                    dynasm!(ops
                        ; cmp rcx, [rsp + off + 16]
                        ; jae => decline
                        ; mov rdx, [rsp + off + 8]
                        ; mov rax, [rdx + rcx * 8]
                        ; mov r10, QWORD ARR_HOLE_BITS as i64
                        ; cmp rax, r10
                        ; je => decline
                        ; mov [rbx + dreg(dst)], rax
                        ; jmp => done
                        ; => decline
                        ; mov rcx, rdi
                        ; mov edx, plan.result_global as i32
                        ; mov r8d, 2                         // input-pin decline
                        ; mov rax, QWORD heap.regexp_scalar_exec_flush as i64
                        ; call rax
                        ; mov DWORD [rsi], ip as i32
                        ; jmp => epilogue
                        ; => done
                    );
                    continue;
                }
                if let Some(plan) = scalar_matchall.filter(|p| p.capture_get_ip == ip) {
                    let scalar_decline = ops.new_dynamic_label();
                    let scalar_done = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + dreg(plan.iter_reg)]
                        ; mov r8d, plan.capture as i32
                        ; mov rax, QWORD heap.regexp_scalar_capture_num as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => scalar_decline
                        // The helper already performed the exact unary-plus
                        // grammar, so write the following ToNum's destination.
                        ; mov [rbx + dreg(plan.tonum_dst)], rax
                        ; jmp => scalar_done
                        ; => scalar_decline
                        // Materialization allocates and updates result_global;
                        // resume at its LoadGlobal (393 in regex-log-scan), not
                        // this GetIndex, whose pre-flush object register is stale.
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + dreg(plan.iter_reg)]
                        ; mov r8d, plan.result_global as i32
                        ; xor r9d, r9d
                        ; mov rax, QWORD heap.regexp_scalar_flush as i64
                        ; call rax
                        ; mov DWORD [rsi], plan.result_load_ip as i32
                        ; jmp => epilogue
                        ; => scalar_done
                    );
                    continue;
                }
                // ── pinned-TypedArray fast path ── when the OSR-time plan tied
                // this access to a pin: identity-guard the receiver against the
                // pin's snapshot, bounds-check against the snapshot len, then a
                // DIRECT machine load + dtype conversion (no call). Guard miss →
                // the generic helper below; OOB / non-integer key / invalidated
                // snapshot → DEOPT (the interpreter re-executes this op with
                // full semantics — OOB reads are rare in real code).
                let pinned = ta_plan
                    .access
                    .get(&ip)
                    .map(|&j| (j as usize, ta_plan.pins[j as usize].kind));
                let (ta_slow, ta_done) = (ops.new_dynamic_label(), ops.new_dynamic_label());
                if let Some((slot, kind)) = pinned {
                    let off = ta_slot_off(slot);
                    if is_arr_pin(kind) {
                        // ── pinned dense-Array fast path ── identity guard, int
                        // key, bounds check against the snapshot len (==
                        // items.len()), then a DIRECT `Value` load (8 bytes — the
                        // element is already a NaN-boxed Value, so its bits store
                        // straight into the dst with no re-encoding / forgery
                        // risk). A HOLE element (an absent index) routes to the
                        // generic helper, EXACTLY mirroring `jit_get_index` (which
                        // deopts on a hole so the interpreter walks the prototype
                        // chain); OOB / non-int key likewise. A guard miss (the
                        // array variable was reassigned, or the snapshot DECLINED
                        // for an arr_props/arguments array → all-zero slot →
                        // rax never equals 0 for a real heap Value) → generic
                        // helper. The base is re-derived after every Vec-growth
                        // op (push / generic SetIndex / user-code helper), so a
                        // realloc cannot leave it stale across iterations.
                        let hole = ops.new_dynamic_label();
                        dynasm!(ops
                            ; mov rax, [rbx + dreg(obj)]      // receiver bits
                            ; cmp rax, [rsp + off]            // identity vs snapshot
                            ; jne => ta_slow                  // miss/declined → helper
                        );
                        emit_ta_key(&mut ops, key, bail); // rcx = i64 index
                        dynasm!(ops
                            ; cmp rcx, [rsp + off + 16]       // unsigned: i < len?
                            ; jae => ta_slow                  // OOB/negative → helper
                            ; mov rdx, [rsp + off + 8]        // pinned items base
                            ; mov rax, [rdx + rcx * 8]        // items[i] (Value bits)
                            ; mov r10, QWORD ARR_HOLE_BITS as i64
                            ; cmp rax, r10
                            ; je => hole                      // HOLE → helper (proto walk)
                            ; mov [rbx + dreg(dst)], rax
                            ; jmp => ta_done
                            ; => hole                         // HOLE lands here, then
                            ; => ta_slow                      // guard-miss/OOB join → generic helper
                        );
                        // Fall through to the generic helper (hole/miss/OOB).
                    } else {
                        dynasm!(ops
                            ; mov rax, [rbx + dreg(obj)]      // receiver bits
                            ; cmp rax, [rsp + off]            // identity vs snapshot
                            ; jne => ta_slow
                        );
                        emit_ta_key(&mut ops, key, bail); // rcx = i64 index
                        dynasm!(ops
                            ; cmp rcx, [rsp + off + 16]       // unsigned: i < len?
                            ; jae => bail                     // OOB/negative → deopt
                            ; mov rdx, [rsp + off + 8]        // pinned data base
                        );
                        match kind {
                            0 => {
                                dynasm!(ops ; movsx eax, BYTE [rdx + rcx]);
                                box_eax(&mut ops, dst);
                            }
                            1 | 2 => {
                                dynasm!(ops ; movzx eax, BYTE [rdx + rcx]);
                                box_eax(&mut ops, dst);
                            }
                            3 => {
                                dynasm!(ops ; movsx eax, WORD [rdx + rcx * 2]);
                                box_eax(&mut ops, dst);
                            }
                            4 => {
                                dynasm!(ops ; movzx eax, WORD [rdx + rcx * 2]);
                                box_eax(&mut ops, dst);
                            }
                            5 => {
                                dynasm!(ops ; mov eax, [rdx + rcx * 4]);
                                box_eax(&mut ops, dst);
                            }
                            6 => {
                                // u32: Int when it fits i32 (mirrors Value::num),
                                // else the exact double (same as the `>>>` boxing).
                                dynasm!(ops ; mov eax, [rdx + rcx * 4]);
                                emit_box_u32(&mut ops, dst);
                            }
                            _ => {
                                // 7/8 (f32/f64): box the double, NaN-canonicalised.
                                if kind == 7 {
                                    dynasm!(ops
                                        ; movss xmm0, [rdx + rcx * 4]
                                        ; cvtss2sd xmm0, xmm0
                                    );
                                } else {
                                    dynasm!(ops ; movsd xmm0, [rdx + rcx * 8]);
                                }
                                emit_box_f64_canon(&mut ops, dst);
                            }
                        }
                        dynasm!(ops ; jmp => ta_done ; => ta_slow);
                    } // end TA-kind branch
                }
                // Generic element read `a[i]` via a win64 helper (dense arrays,
                // flat-ASCII strings, and unpinned TypedArrays). Returns the
                // element bits, `undefined` for out-of-range, or the deopt
                // sentinel for receivers/keys needing interpreter semantics.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // array bits
                    ; mov r8, [rbx + dreg(key)]           // index bits
                    ; mov rax, QWORD heap.get_index as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if pinned.is_some() {
                    dynasm!(ops ; => ta_done);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::SetIndex { obj, key, val } => {
                // ── pinned-TypedArray fast path ── mirror of GetIndex: identity
                // guard, integer key, bounds check, then a direct dtype-encoded
                // store. The VALUE must already be a number (Int or double) —
                // anything else deopts, because ToNumber coercion is observable
                // user code the interpreter must run. OOB stores deopt (the
                // interpreter performs the spec'd coerce-then-silent-no-op).
                //
                // A dense-Array pin (`ARR_PIN_KIND`) has NO inline store path: a
                // store can append/grow (reallocating the Vec) and a hole-fill /
                // length-extend has bespoke semantics — so a SetIndex on a pinned
                // Array takes the generic `jit_set_index` helper (which then
                // re-derives the snapshot). Filter the ARR kind out of `pinned`
                // here so the TA store-encoding match below NEVER sees it (kind
                // 253 would otherwise fall into the int-dtype arm and write a
                // 4-byte int over an 8-byte Value — heap corruption).
                let pinned = ta_plan
                    .access
                    .get(&ip)
                    .map(|&j| (j as usize, ta_plan.pins[j as usize].kind))
                    .filter(|&(_, kind)| !is_arr_pin(kind));
                let (ta_slow, ta_done) = (ops.new_dynamic_label(), ops.new_dynamic_label());
                if let Some((slot, kind)) = pinned {
                    let off = ta_slot_off(slot);
                    let val_int = ops.new_dynamic_label();
                    let sdone = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(obj)]      // receiver bits
                        ; cmp rax, [rsp + off]            // identity vs snapshot
                        ; jne => ta_slow
                    );
                    emit_ta_key(&mut ops, key, bail); // rcx = i64 index
                    dynasm!(ops
                        ; cmp rcx, [rsp + off + 16]       // unsigned: i < len?
                        ; jae => bail                     // OOB store → deopt
                        ; mov rdx, [rsp + off + 8]        // pinned data base
                        ; mov rax, [rbx + dreg(val)]      // value bits
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; je => val_int
                        ; sub r10d, (INT_TAG_HI + 1) as i32
                        ; cmp r10d, 3                     // tagged non-number →
                        ; jbe => bail                     // observable coercion
                    );
                    // ── double value (raw f64 bits in rax) ──
                    match kind {
                        8 => dynasm!(ops ; mov [rdx + rcx * 8], rax),
                        7 => dynasm!(ops
                            ; movq xmm0, rax
                            ; cvtsd2ss xmm0, xmm0
                            ; movss [rdx + rcx * 4], xmm0
                        ),
                        2 => {
                            // Uint8Clamped: round-half-even clamp via the pure
                            // helper (stores the byte itself; clobbers only
                            // volatile regs, and the store is the op's end).
                            dynasm!(ops
                                ; lea rcx, [rdx + rcx]        // element address
                                ; mov rdx, rax                // f64 bits
                                ; mov rax, QWORD heap.ta_clamp_store as i64
                                ; call rax
                            );
                        }
                        _ => {
                            // Int dtypes: JS modular wrap = the low bits of the
                            // i64 truncation. NaN/±Inf/|x|≥2^63 hit the 0x8000…
                            // sentinel → deopt (interpreter wraps/zeroes).
                            dynasm!(ops
                                ; movq xmm0, rax
                                ; cvttsd2si r10, xmm0
                                ; mov r11, QWORD i64::MIN
                                ; cmp r10, r11
                                ; je => bail
                            );
                            match kind {
                                0 | 1 => dynasm!(ops ; mov [rdx + rcx], r10b),
                                3 | 4 => dynasm!(ops ; mov [rdx + rcx * 2], r10w),
                                _ => dynasm!(ops ; mov [rdx + rcx * 4], r10d),
                            }
                        }
                    }
                    dynasm!(ops ; jmp => sdone ; => val_int);
                    // ── Int value (i32 payload in eax) ──
                    match kind {
                        8 => dynasm!(ops
                            ; cvtsi2sd xmm0, eax
                            ; movsd [rdx + rcx * 8], xmm0
                        ),
                        7 => dynasm!(ops
                            ; cvtsi2ss xmm0, eax
                            ; movss [rdx + rcx * 4], xmm0
                        ),
                        2 => dynasm!(ops
                            // Integer clamp to [0,255] (no rounding needed).
                            ; xor r10d, r10d
                            ; test eax, eax
                            ; cmovs eax, r10d
                            ; mov r10d, 255
                            ; cmp eax, r10d
                            ; cmova eax, r10d
                            ; mov [rdx + rcx], al
                        ),
                        0 | 1 => dynasm!(ops ; mov [rdx + rcx], al),
                        3 | 4 => dynasm!(ops ; mov [rdx + rcx * 2], ax),
                        _ => dynasm!(ops ; mov [rdx + rcx * 4], eax),
                    }
                    dynasm!(ops ; => sdone ; jmp => ta_done ; => ta_slow);
                }
                // Generic element write `a[i] = v` via a win64 helper (dense
                // arrays — store/grow — and unpinned TypedArrays with number
                // values). Returns 0 (ok) or the deopt sentinel.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(obj)]          // array bits
                    ; mov r8, [rbx + dreg(key)]           // index bits
                    ; mov r9, [rbx + dreg(val)]           // value bits
                    ; mov rax, QWORD heap.set_index as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                );
                // The generic `jit_set_index` can GROW a dense Array's `Vec`
                // (an in-bounds store never moves it, but an append at `i==len`
                // does, reallocating the storage) — invalidating any pinned
                // Array base in this region. Re-derive every snapshot, exactly
                // as for a detach/resize. (Cheap when there are no array pins;
                // `ta_refetch` is None then.)
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                if pinned.is_some() {
                    dynasm!(ops ; => ta_done);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::CallMethodComputed {
                dst,
                obj,
                key,
                arg_base,
                argc,
            } => {
                // Narrow, semantics-first `array[index](args...)` support. The
                // helper re-reads every live operand, accepts only a present own
                // dense slot containing a plain Func/Closure, and frame-calls it
                // with the receiver as `this`. A miss is a PURE prefix, so the
                // interpreter can replay the complete computed lookup (getters,
                // prototypes, proxies, natives, non-callables) unchanged.
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!("[jit] MEM dense CallMethodComputed helper emitted at ip {ip}");
                }
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                let packed_args = ((obj as u64) << 32) | ((key as u64) << 16) | arg_base as u64;
                emit_region_call_ic(
                    &mut ops,
                    ip,
                    bail,
                    epilogue,
                    heap.call_method_computed_dense,
                    packed_fip,
                    packed_args,
                    argc,
                    dst,
                    refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                    ta_refetch,
                );
            }
            Instr::CallMethod {
                dst,
                obj,
                name,
                arg_base,
                argc,
            } => {
                let key = proto.string_constants[name as usize].as_str();
                if let Some(plan) = scalar_exec.filter(|p| p.call_ip == ip) {
                    debug_assert_eq!(key, "exec");
                    debug_assert_eq!(argc, 1);
                    debug_assert_eq!(obj, plan.re_reg);
                    debug_assert_eq!(arg_base, plan.input_reg);
                    debug_assert_eq!(dst, plan.call_result_reg);
                    let packed_dsts = plan
                        .tonum_dsts
                        .iter()
                        .enumerate()
                        .fold(0u64, |bits, (g, &reg)| bits | ((reg as u64) << (16 * g)));
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov rdx, rbx
                        ; mov r8, [rbx + dreg(obj)]
                        ; mov r9, [rbx + dreg(arg_base)]
                        ; mov rax, QWORD packed_dsts as i64
                        ; mov [rsp + 32], rax               // fifth Win64 arg
                        ; mov rax, QWORD heap.regexp_scalar_exec as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail                        // pure prefix; replay CallMethod
                        ; mov [rbx + dreg(dst)], rax        // TRUE success / NULL miss
                    );
                    // The helper runs the loop safe point and may alter every
                    // heap/pin side vector. Re-derive all snapshots before the
                    // next iteration's direct Array load.
                    if refetch_pinned {
                        emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                    }
                    if let Some((snap, pin_plan)) = ta_refetch {
                        emit_refetch_ta(&mut ops, snap, pin_plan);
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                    continue;
                }
                // Exact `Object.prototype.hasOwnProperty.call(array, numericKey)`
                // intrinsic. The helper proves both the `hasOwnProperty`
                // callable and its inherited `%Function.prototype%.call` slot,
                // then answers the existing allocation-free array-index probe.
                // A guard miss is a PURE prefix: fall through to the unchanged
                // generic CallMethod path rather than bailing/evicting the region.
                if argc == 2 && key == "call" && has_own_call_intrinsic_enabled() {
                    let hasown_slow = ops.new_dynamic_label();
                    let hasown_done = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + dreg(obj)]
                        ; mov r8, [rbx + dreg(arg_base)]
                        ; mov r9, [rbx + dreg(arg_base + 1)]
                        ; mov rax, QWORD heap.has_own_call as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => hasown_slow
                        ; mov [rbx + dreg(dst)], rax
                        ; jmp => hasown_done
                        ; => hasown_slow
                    );

                    let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                    let packed_args =
                        ((name as u64) << 32) | ((obj as u64) << 16) | arg_base as u64;
                    if let Some(mp) = method_plan.get(&ip) {
                        emit_inline_method_call(
                            &mut ops,
                            ip,
                            epilogue,
                            leaf_flag_off,
                            mp,
                            obj,
                            arg_base,
                            argc,
                            dst,
                            heap.call_method_ic,
                            packed_fip,
                            packed_args,
                            refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                            ta_refetch,
                        );
                    } else {
                        emit_region_call_ic(
                            &mut ops,
                            ip,
                            bail,
                            epilogue,
                            heap.call_method_ic,
                            packed_fip,
                            packed_args,
                            argc,
                            dst,
                            refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                            ta_refetch,
                        );
                    }
                    dynasm!(ops ; => hasown_done);
                    continue;
                }
                // Pristine primitive-string `matchAll(RegExp)` /
                // `replace(RegExp, string)`: the helper proves receiver kind,
                // active main realm, the live metadata-derived String method
                // slot, and every RegExp protocol dependency before doing any
                // observable operation. A miss is therefore a pure prefix and
                // falls through to the unchanged generic call site below.
                if ((key == "matchAll" && argc == 1) || (key == "replace" && argc == 2))
                    && crate::vm::string_regexp_call_direct_enabled()
                {
                    let srx_slow = ops.new_dynamic_label();
                    let srx_done = ops.new_dynamic_label();
                    let srx_bail = ops.new_dynamic_label();
                    // In the scalar outer plan, a direct-helper miss must
                    // resume the original CallMethod at 381. Falling through
                    // to the generic helper could execute observable work and
                    // then enter scalar protocol guards at 382, violating the
                    // pure-prefix proof.
                    let srx_decline = if scalar_matchall.is_some_and(|p| p.call_ip == ip) {
                        srx_bail
                    } else {
                        srx_slow
                    };
                    dynasm!(ops
                        ; mov rcx, rdi                          // vm
                        ; mov rdx, [rbx + dreg(obj)]            // receiver bits
                        ; lea r8, [rbx + dreg(arg_base)]        // &args[0..argc]
                        ; mov r9d, (key == "replace") as i32   // 0 matchAll, 1 replace
                        ; mov rax, QWORD heap.string_regexp_call_direct as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => srx_decline                     // pure guard miss
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => srx_bail                        // committed throw
                        ; mov [rbx + dreg(dst)], rax
                    );
                    // Matcher/iterator/result construction and replacement
                    // assembly allocate. Re-derive every potentially moved or
                    // patched pinned/TypedArray snapshot, matching the generic
                    // method helper's post-call contract.
                    if refetch_pinned {
                        emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                    }
                    if let Some((snap, plan)) = ta_refetch {
                        emit_refetch_ta(&mut ops, snap, plan);
                    }
                    emit_region_bail(&mut ops, ip, srx_bail, epilogue);
                    dynasm!(ops
                        ; jmp => srx_done
                        ; => srx_slow
                    );

                    let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                    let packed_args =
                        ((name as u64) << 32) | ((obj as u64) << 16) | arg_base as u64;
                    if let Some(mp) = method_plan.get(&ip) {
                        emit_inline_method_call(
                            &mut ops,
                            ip,
                            epilogue,
                            leaf_flag_off,
                            mp,
                            obj,
                            arg_base,
                            argc,
                            dst,
                            heap.call_method_ic,
                            packed_fip,
                            packed_args,
                            refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                            ta_refetch,
                        );
                    } else {
                        emit_region_call_ic(
                            &mut ops,
                            ip,
                            bail,
                            epilogue,
                            heap.call_method_ic,
                            packed_fip,
                            packed_args,
                            argc,
                            dst,
                            refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                            ta_refetch,
                        );
                    }
                    dynasm!(ops ; => srx_done);
                    continue;
                }
                // Pristine RegExp `test` / `exec`: collapse the generic
                // CallMethod helper, IC probe, builtin dispatch and native
                // wrapper into one guarded helper call. The helper reuses the
                // interpreter's exact per-call intrinsic proofs. A miss has
                // done no observable work and falls through to the unchanged
                // generic call emission below; a throw is committed and exits.
                if matches!(key, "test" | "exec") && crate::vm::regexp_call_direct_enabled() {
                    let rx_slow = ops.new_dynamic_label();
                    let rx_done = ops.new_dynamic_label();
                    let rx_bail = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rcx, rdi                          // vm
                        ; mov rdx, [rbx + dreg(obj)]            // receiver bits
                    );
                    if argc == 0 {
                        dynasm!(ops ; mov r8, QWORD Value::UNDEFINED.bits() as i64);
                    } else {
                        dynasm!(ops ; mov r8, [rbx + dreg(arg_base)]); // input bits
                    }
                    dynasm!(ops
                        ; mov r9d, (key == "test") as i32       // 0 exec, 1 test
                        ; mov rax, QWORD heap.regexp_call_direct as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => rx_slow                         // pure guard miss
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => rx_bail                         // committed throw
                        ; mov [rbx + dreg(dst)], rax
                    );
                    // Input coercion and `lastIndex.valueOf` can run arbitrary
                    // user code; exec/result construction allocates. Re-derive
                    // every potentially moved/patched region snapshot exactly
                    // as the generic method helper does.
                    if refetch_pinned {
                        emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                    }
                    if let Some((snap, plan)) = ta_refetch {
                        emit_refetch_ta(&mut ops, snap, plan);
                    }
                    emit_region_bail(&mut ops, ip, rx_bail, epilogue);
                    dynasm!(ops
                        ; jmp => rx_done
                        ; => rx_slow
                    );

                    let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                    let packed_args =
                        ((name as u64) << 32) | ((obj as u64) << 16) | arg_base as u64;
                    if let Some(mp) = method_plan.get(&ip) {
                        emit_inline_method_call(
                            &mut ops,
                            ip,
                            epilogue,
                            leaf_flag_off,
                            mp,
                            obj,
                            arg_base,
                            argc,
                            dst,
                            heap.call_method_ic,
                            packed_fip,
                            packed_args,
                            refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                            ta_refetch,
                        );
                    } else {
                        emit_region_call_ic(
                            &mut ops,
                            ip,
                            bail,
                            epilogue,
                            heap.call_method_ic,
                            packed_fip,
                            packed_args,
                            argc,
                            dst,
                            refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                            ta_refetch,
                        );
                    }
                    dynasm!(ops ; => rx_done);
                    continue;
                }
                // ── `s.indexOf(t)` intrinsic ──
                // A direct call to the search, skipping the whole builtin call
                // chain (jit_call_method_ic -> jit_region_call_impl ->
                // try_builtin_method -> dispatch_builtin_method -> string_method)
                // that costs ~47ns to reach ~5ns of work. This is the shape that
                // already puts `charCodeAt` and `.length` at node parity. The
                // helper deopts for anything but ASCII/ASCII, so the interpreter
                // runs the full method (fromIndex forms, non-ASCII, coercible
                // arguments, a non-string receiver) at this ip unchanged.
                // `s.substring(a[,b])` / `s.slice(a[,b])` intrinsic — same
                // shape as indexOf below. Args are read from the contiguous
                // window; mode bit 1 tells the helper not to read an absent end.
                let substring_arity_ok = argc == 2 || (argc == 1 && substring1_intrinsic_enabled());
                if substring_arity_ok && (key == "substring" || key == "slice") {
                    let bail = ops.new_dynamic_label();
                    let mode = (key == "slice") as i32 | (((argc == 1) as i32) << 1);
                    dynasm!(ops
                        ; mov rcx, rdi                          // vm
                        ; mov rdx, [rbx + dreg(obj)]            // receiver bits
                        ; lea r8, [rbx + dreg(arg_base)]        // &args[0..argc]
                        ; mov r9d, mode
                        ; mov rax, QWORD heap.str_substring as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    // A non-empty result allocates a Heap slot, whose parallel
                    // versions Vec may move. No user code or nested compilation
                    // runs here, so r14 and TypedArray snapshots stay valid.
                    if refetch_pinned {
                        emit_refetch_pinned(&mut ops, heap.versions_base, None);
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                    continue;
                }
                // `m.get(k)` / `m.has(k)` / `s.has(v)` intrinsic. The receiver
                // kind is checked in the helper (a wrong kind deopts), so a
                // same-named method on any other object is unaffected.
                if argc == 1 && matches!(key, "get" | "has") {
                    let bail = ops.new_dynamic_label();
                    // 0 = Map.get, 1 = Map.has, 2 = Set.has. Map and Set are
                    // distinguished at runtime, so `has` tries Map first and the
                    // helper falls through to Set on a kind mismatch.
                    let opsel: i32 = if key == "get" { 0 } else { 1 };
                    let set_try = ops.new_dynamic_label();
                    let coll_done = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rcx, rdi                          // vm
                        ; mov rdx, [rbx + dreg(obj)]            // receiver bits
                        ; mov r8, [rbx + dreg(arg_base)]        // key bits
                        ; mov r9d, opsel
                        ; mov rax, QWORD heap.coll_lookup as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; jne => coll_done
                    );
                    if opsel == 1 {
                        // `has` on a Set: retry with op = 2 before deopting.
                        dynasm!(ops
                            ; => set_try
                            ; mov rcx, rdi
                            ; mov rdx, [rbx + dreg(obj)]
                            ; mov r8, [rbx + dreg(arg_base)]
                            ; mov r9d, 2
                            ; mov rax, QWORD heap.coll_lookup as i64
                            ; call rax
                            ; mov r10, QWORD SELF_CALL_DEOPT as i64
                            ; cmp rax, r10
                            ; je => bail
                        );
                    } else {
                        dynasm!(ops ; => set_try ; jmp => bail);
                    }
                    dynasm!(ops
                        ; => coll_done
                        ; mov [rbx + dreg(dst)], rax
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                    continue;
                }
                if argc == 1 && key == "indexOf" {
                    let bail = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rcx, rdi                          // vm
                        ; mov rdx, [rbx + dreg(obj)]            // receiver bits
                        ; mov r8, [rbx + dreg(arg_base)]        // needle bits
                        ; mov rax, QWORD heap.str_index_of as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                    continue;
                }
                if (argc == 1 || argc == 2) && dv_get_kind(key).is_some() {
                    // Whitelisted DataView `get*(pos[, littleEndian])`.
                    // ── pinned-DataView fast path ── when the OSR plan pinned
                    // this receiver: identity guard, integral number pos,
                    // signed bounds check vs the pinned byteLength, then a
                    // direct (optionally byte-swapped) load. A double/heap
                    // littleEndian falls to the helper (full ToBoolean).
                    let kindid = dv_get_kind(key).unwrap();
                    let pinned = ta_plan
                        .access
                        .get(&ip)
                        .filter(|&&j| ta_plan.pins[j as usize].kind == DV_PIN_KIND)
                        .map(|&j| j as usize);
                    let (dv_slow, dv_done) = (ops.new_dynamic_label(), ops.new_dynamic_label());
                    if let Some(slot) = pinned {
                        let off = ta_slot_off(slot);
                        let size = [1i32, 1, 1, 2, 2, 4, 4, 4, 8][kindid as usize];
                        dynasm!(ops
                            ; mov rax, [rbx + dreg(obj)]      // receiver bits
                            ; cmp rax, [rsp + off]            // identity vs snapshot
                            ; jne => dv_slow
                        );
                        emit_ta_key(&mut ops, arg_base, bail); // rcx = i64 pos
                        dynasm!(ops
                            ; test rcx, rcx
                            ; js => bail                      // negative → RangeError
                            ; mov r10, [rsp + off + 16]       // byteLength
                            ; sub r10, size
                            ; cmp rcx, r10                    // signed: pos > len-size
                            ; jg => bail                      //  (incl. len < size)
                            ; mov rdx, [rsp + off + 8]        // pinned data base
                        );
                        // littleEndian: only multi-byte kinds look at it. The
                        // inline path accepts Int/Bool/Null/Undefined (payload
                        // ≠ 0 ⇔ true — exactly ToBoolean for those tags);
                        // a double/heap flag falls to the helper.
                        let le_big = ops.new_dynamic_label();
                        let loaded = ops.new_dynamic_label();
                        if size > 1 {
                            if argc == 2 {
                                dynasm!(ops
                                    ; mov rax, [rbx + dreg(arg_base + 1)]
                                    ; mov r10, rax
                                    ; shr r10, 48
                                    ; sub r10d, INT_TAG_HI as i32
                                    ; cmp r10d, 3             // Int/Bool/Null/Undef
                                    ; ja => dv_slow           // double/heap → helper
                                    ; test eax, eax           // payload ≠ 0 ⇔ true
                                    ; jz => le_big            // falsy → big-endian
                                );
                            } else {
                                // Absent flag = undefined = big-endian.
                                dynasm!(ops ; jmp => le_big);
                            }
                        }
                        // ── little-endian load ──
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
                        match kindid {
                            6 => emit_box_u32(&mut ops, dst),
                            7 | 8 => emit_box_f64_canon(&mut ops, dst),
                            _ => box_eax(&mut ops, dst),
                        }
                        dynasm!(ops ; jmp => dv_done ; => dv_slow);
                    }
                    // Generic path: the dedicated win64 helper (receiver + pos
                    // + le bits in, element kind via the 5th-arg slot; result
                    // bits out, deopt sentinel → bail). No alloc, no user code
                    // — no re-fetch.
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(obj)]          // receiver bits
                        ; mov r8, [rbx + dreg(arg_base)]      // pos bits
                    );
                    if argc == 2 {
                        dynasm!(ops ; mov r9, [rbx + dreg(arg_base + 1)]);
                    } else {
                        dynasm!(ops ; mov r9, QWORD Value::UNDEFINED.bits() as i64);
                    }
                    dynasm!(ops
                        ; mov QWORD [rsp + 32], kindid as i32 // 5th arg: kind
                        ; mov rax, QWORD heap.dv_get as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    if pinned.is_some() {
                        dynasm!(ops ; => dv_done);
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else if argc == 1 && matches!(key, "push" | "charCodeAt") {
                    // The whitelisted 1-arg builtins keep their dedicated win64
                    // helpers: receiver + arg0 bits in, result bits out, deopt
                    // sentinel → bail. Neither allocates a heap OBJECT (push
                    // grows the array's own Vec; the versions array is
                    // untouched), so no pinned-pointer re-fetch is needed.
                    let helper = match key {
                        "push" => heap.array_push,
                        _ => heap.char_code_at,
                    };
                    // ── pinned-string charCodeAt fast path ── when the OSR plan
                    // pinned this receiver as a flat ASCII string (snapshot
                    // {obj_bits, bytes_ptr, units}): identity-guard the receiver,
                    // materialise the index, then a DIRECT byte load (byte i ==
                    // UTF-16 unit i for ASCII). Out of range → NaN (charCodeAt
                    // OOB semantics, == the helper's `unit_at None → NaN`). A
                    // guard miss / non-integral index / a re-snapshot that found
                    // the string non-ASCII (slot {0,0,0} → identity miss) falls
                    // through to the UNCHANGED generic helper below.
                    let str_pin = (key == "charCodeAt")
                        .then(|| ta_plan.access.get(&ip))
                        .flatten()
                        .filter(|&&j| ta_plan.pins[j as usize].kind == STR_PIN_KIND)
                        .map(|&j| j as usize);
                    let cc_done = ops.new_dynamic_label();
                    if let Some(slot) = str_pin {
                        let off = ta_slot_off(slot);
                        let cc_slow = ops.new_dynamic_label();
                        let cc_oob = ops.new_dynamic_label();
                        dynasm!(ops
                            ; mov rax, [rbx + dreg(obj)]      // receiver bits
                            ; cmp rax, [rsp + off]            // identity vs snapshot
                            ; jne => cc_slow                  // miss → generic helper
                        );
                        // Index → rcx (signed i64). Non-int/fractional/NaN bails to
                        // the interpreter — exactly the helper's deopt for those.
                        emit_ta_key(&mut ops, arg_base, bail);
                        dynasm!(ops
                            ; test rcx, rcx
                            ; js => cc_slow                   // negative → helper (array_index None → deopt)
                            ; mov r10, [rsp + off + 16]       // units (== ASCII byte len)
                            ; cmp rcx, r10
                            ; jae => cc_oob                   // i >= len → NaN
                            ; mov rdx, [rsp + off + 8]        // pinned bytes base
                            ; movzx eax, BYTE [rdx + rcx]     // ASCII code unit
                        );
                        box_eax(&mut ops, dst);
                        dynasm!(ops
                            ; jmp => cc_done
                            ; => cc_oob
                            ; mov rax, QWORD QNAN_BITS as i64 // charCodeAt OOB → NaN
                            ; mov [rbx + dreg(dst)], rax
                            ; jmp => cc_done
                            ; => cc_slow
                        );
                    }
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(obj)]          // receiver bits
                        ; mov r8, [rbx + dreg(arg_base)]      // arg0 bits
                        ; mov rax, QWORD helper as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(dst)], rax
                    );
                    // `arr.push(x)` GROWS the array's `Vec` (reallocating its
                    // storage) — invalidating any pinned dense-Array base in this
                    // region. Re-derive the snapshots (charCodeAt never grows an
                    // array, so the refetch is push-only). The str-pin fast path
                    // above jumps to `cc_done` before this call, so it correctly
                    // skips the refetch.
                    if key == "push" {
                        if let Some((snap, plan)) = ta_refetch {
                            emit_refetch_ta(&mut ops, snap, plan);
                        }
                    }
                    if str_pin.is_some() {
                        dynasm!(ops ; => cc_done);
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else {
                    // Generic `obj.m(args…)`: the interpreter-IC call helper
                    // (see `emit_region_call_ic`). Packing: r9 = (name<<32) |
                    // (obj<<16) | arg_base; argc via the stack.
                    let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                    let packed_args =
                        ((name as u64) << 32) | ((obj as u64) << 16) | arg_base as u64;
                    // Q7 method inlining: a trivial class method on a known
                    // receiver shape at this site is inlined behind a per-receiver
                    // identity+version guard; a guard miss / tight headroom falls
                    // through to the SAME helper below (a pure prefix).
                    if let Some(mp) = method_plan.get(&ip) {
                        emit_inline_method_call(
                            &mut ops,
                            ip,
                            epilogue,
                            leaf_flag_off,
                            mp,
                            obj,
                            arg_base,
                            argc,
                            dst,
                            heap.call_method_ic,
                            packed_fip,
                            packed_args,
                            refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                            ta_refetch,
                        );
                    } else {
                        emit_region_call_ic(
                            &mut ops,
                            ip,
                            bail,
                            epilogue,
                            heap.call_method_ic,
                            packed_fip,
                            packed_args,
                            argc,
                            dst,
                            refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                            ta_refetch,
                        );
                    }
                }
            }
            Instr::Call {
                dst,
                callee,
                arg_base,
                argc,
            } => {
                // Generic `f(args…)` with `this = undefined`: the interpreter-IC
                // call helper. Packing: r9 = (callee<<16) | arg_base.
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                let packed_args = ((callee as u64) << 16) | arg_base as u64;
                // ── B83 cross-call fast path ── as in Tier C's Call arm
                // (proto_mem.rs): a Tier-C-compiled plain callee is dispatched
                // native→native; anything else falls through to the unchanged
                // helper. The region runs over the WHOLE function's frame, so
                // the caller window size is `proto.reg_count` — the same
                // contiguity invariant `setup_call` maintains.
                let cross_site = cross_plan.get(&ip).copied();
                let cross = cross_site.is_some() && leaf_plan.get(&ip).is_none();
                let cross_done = ops.new_dynamic_label();
                if cross {
                    let site = cross_site.expect("cross site disappeared during emission");
                    if let Some(record) = site.static_record_factory {
                        debug_assert_eq!(argc, 2);
                        let record_fallback = ops.new_dynamic_label();
                        let packed_record = ((record.fid as u64) << 32)
                            | ((proto.reg_count.max(1) as u64) << 16)
                            | u64::from(arg_base);
                        dynasm!(ops
                            ; mov rcx, rdi
                            ; mov rdx, rbx
                            ; lea r8, [rbx + dreg(arg_base)]
                            ; mov r9, QWORD packed_record as i64
                            ; mov rax, [rbx + dreg(callee)]
                            ; mov [rsp + 32], rax
                            ; mov rax, QWORD heap.static_record_factory as i64
                            ; call rax
                            ; mov r10, QWORD SELF_CALL_DEOPT as i64
                            ; cmp rax, r10
                            ; je => record_fallback
                            ; mov r10, QWORD CALL_THREW as i64
                            ; cmp rax, r10
                            ; je => bail
                            ; mov [rbx + dreg(dst)], rax
                        );
                        if refetch_pinned {
                            emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                        }
                        if let Some((snap, plan)) = ta_refetch {
                            emit_refetch_ta(&mut ops, snap, plan);
                        }
                        dynasm!(ops
                            ; jmp => cross_done
                            ; => record_fallback
                        );
                    }
                    let same_proto2 = site.same_proto2;
                    let packed_cross: u64 = match same_proto2 {
                        Some(plan) => {
                            ((plan.fid as u64) << 32)
                                | ((proto.reg_count.max(1) as u64) << 16)
                                | u64::from(plan.callee_regs)
                        }
                        None => (argc as u64) | ((proto.reg_count.max(1) as u64) << 16),
                    };
                    let cross_helper = if same_proto2.is_some() {
                        heap.cross_call_same_proto2
                    } else {
                        heap.cross_call
                    };
                    let cross_fallback = ops.new_dynamic_label();
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, rbx                        // caller window base
                        ; lea r8, [rbx + dreg(arg_base)]      // &args[0..argc]
                        ; mov r9, QWORD packed_cross as i64   // (caller_regs<<16)|argc
                        ; mov rax, [rbx + dreg(callee)]
                        ; mov [rsp + 32], rax                 // 5th arg: callee bits
                        ; mov rax, QWORD cross_helper as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => cross_fallback                // not eligible → call_ic
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => bail                          // threw → exit; unwind
                        ; mov [rbx + dreg(dst)], rax
                    );
                    // The callee ran user code: re-derive the pinned r13/r14 and
                    // the TypedArray snapshots, exactly as after call_ic.
                    if refetch_pinned {
                        emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                    }
                    if let Some((snap, plan)) = ta_refetch {
                        emit_refetch_ta(&mut ops, snap, plan);
                    }
                    dynasm!(ops
                        ; jmp => cross_done
                        ; => cross_fallback
                    );
                }
                // Q4 leaf-call inlining: a monomorphic plain-leaf callee at this
                // site is inlined with an identity guard; a guard miss / tight
                // headroom falls through to the SAME helper below (a pure prefix).
                if let Some(lp) = leaf_plan.get(&ip) {
                    // Pair fusion elides six caller ops.  Keep metered regions
                    // on the ordinary path so every bytecode remains charged.
                    let span_pair_resume = if blocks.is_none() {
                        lp.span_code_unit_pred
                            .and_then(|p| p.pair)
                            .map(|p| lbl(p.resume_ip, &in_region))
                    } else {
                        None
                    };
                    emit_inline_leaf_call(
                        &mut ops,
                        ip,
                        epilogue,
                        leaf_flag_off,
                        lp,
                        callee,
                        arg_base,
                        argc,
                        dst,
                        heap.math_unary,
                        heap.math_two,
                        // v2 body-op helpers (order matches the signature).
                        heap.get_index,
                        heap.char_code_at,
                        heap.strict_eq,
                        heap.truthy,
                        heap.call_ic,
                        packed_fip,
                        packed_args,
                        refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                        ta_refetch,
                        span_pair_resume,
                    );
                } else {
                    emit_region_call_ic(
                        &mut ops,
                        ip,
                        bail,
                        epilogue,
                        heap.call_ic,
                        packed_fip,
                        packed_args,
                        argc,
                        dst,
                        refetch_pinned.then_some((heap.versions_base, heap.ic_base)),
                        ta_refetch,
                    );
                }
                if cross {
                    dynasm!(ops ; => cross_done);
                }
            }
            Instr::StrConcat { dst, a, b } => {
                // `dst = a + b` via the win64 `jit_concat` helper (rope concat or
                // numeric add). Same ABI as the method helpers: vm + two operand
                // bits in, result bits out, deopt sentinel → bail. The helper
                // ALLOCATES (a rope node grows the heap's parallel version
                // array, which may reallocate) — so when the region also has
                // GetProp/SetProp (the r13 users), re-derive r13 after the
                // call. It never runs user code, so the IC table (r14) is safe.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // a bits
                    ; mov r8, [rbx + dreg(b)]             // b bits
                    ; mov rax, QWORD heap.concat as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail                          // threw (pending_throw set) → unwind, NOT redo
                    ; mov [rbx + dreg(dst)], rax
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::AddRightPair {
                dst,
                a,
                b,
                c,
                in_place,
            } => {
                // Exact `a + (b + c)` through the shared interpreter helper.
                // Its primitive ASCII arm allocates one flat result; the
                // fallback can run user coercion code while performing the two
                // ordinary Adds.  Consequently every served call gets the full
                // pinned/TA refetch discipline, and a committed throw exits as
                // CALL_THREW (never redo).
                let pair_helper = if in_place {
                    crate::vm::jit_append_right_pair as usize
                } else {
                    crate::vm::jit_add_right_pair as usize
                };
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // outer left bits
                    ; mov r8, [rbx + dreg(b)]             // inner left bits
                    ; mov r9, [rbx + dreg(c)]             // inner right bits
                    ; mov rax, QWORD pair_helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // helper never returns redo; defensive
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail                          // pending_throw set: unwind
                    ; mov [rbx + dreg(dst)], rax
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Pad2Concat { dst, src, zero } => {
                // Tagged-Int/range hits construct the pinned heap Value bits
                // call-free. A miss has performed no observable operation and
                // enters the shared exact helper; only that path needs the full
                // refetch/CALL_THREW protocol. Under ICSTATS route all cases to
                // the helper so its mechanism counters remain exact.
                let slow = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                if !crate::vm::pad2_concat_stats_enabled() {
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(src)]
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => slow
                        ; mov r10d, eax                    // signed i32 payload, upper bits clear
                    );
                    if zero {
                        // Unsigned `ja` rejects negative payloads as well as >9.
                        dynasm!(ops ; cmp r10d, 9 ; ja => slow);
                    } else {
                        dynasm!(ops
                            ; cmp r10d, 10
                            ; jb => slow
                            ; cmp r10d, 99
                            ; ja => slow
                        );
                    }
                    dynasm!(ops
                        ; add r10d, crate::heap::INTERN_PAD2_START as i32
                        ; mov rax, QWORD HEAP_TAG as i64
                        ; or rax, r10
                        ; mov [rbx + dreg(dst)], rax
                        ; jmp => done
                    );
                }
                dynasm!(ops
                    ; => slow
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(src)]          // RHS bits
                    ; mov r8d, zero as i32                // literal prefix selector
                    ; mov rax, QWORD crate::vm::jit_pad2_concat as usize as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // defensive; helper never redoes
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail                          // pending_throw set: unwind
                    ; mov [rbx + dreg(dst)], rax
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
                dynasm!(ops ; => done);
            }
            Instr::Pad2Conditional { dst, src } => {
                // The whole conditional maps every tagged Int 0..99 directly
                // to the same-numbered canonical slot. A miss is pristine and
                // calls the exact relational-then-Add helper. ICSTATS routes
                // hits through that helper so its counters remain exact.
                let slow = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                if !crate::vm::pad2_concat_stats_enabled() {
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(src)]
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => slow
                        ; mov r10d, eax
                        // Unsigned `ja` rejects negative payloads and >99.
                        ; cmp r10d, 99
                        ; ja => slow
                        ; add r10d, crate::heap::INTERN_PAD2_START as i32
                        ; mov rax, QWORD HEAP_TAG as i64
                        ; or rax, r10
                        ; mov [rbx + dreg(dst)], rax
                        ; jmp => done
                    );
                }
                dynasm!(ops
                    ; => slow
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(src)]
                    ; mov rax, QWORD crate::vm::jit_pad2_conditional as usize as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(dst)], rax
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
                dynasm!(ops ; => done);
            }
            Instr::StrConcatChain { dst, a, b } => {
                if crate::codegen::chain_fast_enabled() {
                    // Fused chain link via the single-dispatch fast sibling
                    // `jit_concat_chain_fast` (value-identical to
                    // `Vm::add_values_chain`, the interpreter arm's entry).
                    // r9d carries the first-link capacity hint (0 = none).
                    // The helper CAN allocate and run user code on its
                    // generic tail, but its in-place arms do neither and are
                    // the ONLY arms that return the accumulator's own bits
                    // for a heap accumulator — so `result == old acc bits &&
                    // old acc is heap-tagged` licenses skipping the
                    // r13/r14/TA refetch (dynamic evidence, not a guard
                    // removal). The heap-tag test is load-bearing: a numeric
                    // accumulator can get its own bits back from the generic
                    // tail AFTER user coercion code ran (e.g. int acc + an
                    // object leaf whose valueOf returns 0). A throw comes
                    // back as CALL_THREW (pending_throw set) → bail = UNWIND,
                    // never a redo; SELF_CALL_DEOPT is never returned, the
                    // check is kept for uniformity with the siblings.
                    let hint = chain_capacity_hint(&proto.code, ip, a, e);
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(a)]            // acc bits
                        ; mov r8, [rbx + dreg(b)]             // leaf bits
                        ; mov r9d, hint as i32                // capacity hint
                        ; mov rax, QWORD crate::vm::jit_concat_chain_fast as usize as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => bail                          // threw (pending_throw set) → unwind, NOT redo
                        ; mov r10, [rbx + dreg(a)]            // pre-call acc bits (dst not yet stored)
                        ; mov [rbx + dreg(dst)], rax
                    );
                    if refetch_pinned || ta_refetch.is_some() {
                        let refetch = ops.new_dynamic_label();
                        let skip = ops.new_dynamic_label();
                        dynasm!(ops
                            ; cmp rax, r10
                            ; jne => refetch
                            ; shr r10, 48
                            ; cmp r10d, TAG_HEAP_HI as i32
                            ; je => skip                      // in-place arm: no alloc, no user code
                            ; => refetch
                        );
                        if refetch_pinned {
                            emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                        }
                        if let Some((snap, plan)) = ta_refetch {
                            emit_refetch_ta(&mut ops, snap, plan);
                        }
                        dynasm!(ops ; => skip);
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                } else {
                    // W11 (B124) fused chain link: `dst = a + b` via the win64
                    // `jit_concat_chain` helper (in-place growth of the chain's
                    // fresh flat-Str accumulator, full pairwise `+` otherwise —
                    // the same `Vm::add_values_chain` the interpreter arm calls).
                    // The helper ALLOCATES and, unlike `jit_concat`'s expected
                    // targets, CAN run user code (an object RHS's ToPrimitive via
                    // the `add_values` fallback) — so refetch r13 AND r14 (and
                    // the TA snapshots) after the call. A throw comes back as
                    // CALL_THREW (pending_throw set) → bail = UNWIND, never a
                    // redo (the user side effects must not run twice); the
                    // helper never returns SELF_CALL_DEOPT, the check is kept
                    // for uniformity with its siblings.
                    dynasm!(ops
                        ; mov rcx, rdi                        // vm
                        ; mov rdx, [rbx + dreg(a)]            // acc bits
                        ; mov r8, [rbx + dreg(b)]             // leaf bits
                        ; mov rax, QWORD crate::vm::jit_concat_chain as usize as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov r10, QWORD CALL_THREW as i64
                        ; cmp rax, r10
                        ; je => bail                          // threw (pending_throw set) → unwind, NOT redo
                        ; mov [rbx + dreg(dst)], rax
                    );
                    if refetch_pinned {
                        emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                    }
                    if let Some((snap, plan)) = ta_refetch {
                        emit_refetch_ta(&mut ops, snap, plan);
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                }
            }
            Instr::StaticFn {
                dst,
                op,
                arg_base,
                argc: _,
            } => {
                // Bounded set (admission gated argc == 1). PromiseResolve
                // ALLOCATES ⇒ the StrConcat discipline: re-derive r13 (and the
                // TA snapshots) after the call; no user code ⇒ r14 safe. A
                // heap argument returns the deopt sentinel and the interpreter
                // runs the full identity/thenable protocol at this ip.
                use crate::bytecode::StaticFn as S;
                let code: u32 = match op {
                    S::PromiseResolve => 0,
                    S::NumberIsInteger => 1,
                    S::NumberIsNaN => 2,
                    S::NumberIsFinite => 3,
                    S::NumberIsSafeInteger => 4,
                    _ => unreachable!("StaticFn op not admitted by region_can_compile"),
                };
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov edx, code as i32                // op code (helper's own map)
                    ; mov r8, [rbx + dreg(arg_base)]      // a0 bits
                    ; mov rax, QWORD heap.static_fn as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // heap arg → interp protocol
                    ; mov [rbx + dreg(dst)], rax
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::StrAppendInPlace { dst, a, b } => {
                // In-place `dst = a + b` via `jit_str_append` (mutates a's buffer
                // when uniquely owned — the emitter proved linearity). DEOPTS
                // when the appended value needs real ToPrimitive (user hooks /
                // a Symbol's TypeError): the helper's purity gate runs BEFORE
                // any mutation, so the interpreter re-executes the op cleanly
                // with full semantics. Allocates/grows the heap on the pure
                // path, so (like StrConcat) re-derive r13 when the region
                // reads it.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // a (accumulator) bits
                    ; mov r8, [rbx + dreg(b)]             // b (appended) bits
                    ; mov rax, QWORD heap.str_append as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // needs ToPrimitive → interp
                    ; mov [rbx + dreg(dst)], rax
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::StrAppendIndex {
                dst, a, obj, key, ..
            } => {
                // Allocation-free fused ASCII prefix. A miss has not mutated
                // the accumulator, so bailing at this ip lets the interpreter
                // execute the exact GetIndex + append fallback once.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // accumulator bits
                    ; mov r8, [rbx + dreg(obj)]            // indexed receiver bits
                    ; mov r9, [rbx + dreg(key)]            // key bits
                    ; mov rax, QWORD heap.str_append_index as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // pristine miss
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::GetIterator { dst, src } => {
                let Some(_plan) = scalar_matchall.filter(|p| p.get_iterator_ip == ip) else {
                    return None;
                };
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(src)]
                    ; mov rax, QWORD heap.regexp_scalar_get_iterator as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // pure guard miss → original GetIterator
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::IterPrime { dst, iter } => {
                let Some(_plan) = scalar_matchall.filter(|p| p.iter_prime_ip == ip) else {
                    return None;
                };
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(iter)]
                    ; mov rax, QWORD heap.regexp_scalar_iter_prime as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // pure guard miss → original observable Get
                    ; mov [rbx + dreg(dst)], rax
                );
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::IterNext {
                value_dst,
                done_dst,
                iter,
                idx,
                next,
            } => {
                if let Some(plan) = scalar_matchall.filter(|p| p.iter_next_ip == ip) {
                    // Eight admission-proved u16 operands in the fourth/fifth
                    // Win64 arguments. Besides avoiding another stack slot
                    // (TA pins start immediately above the existing one), the
                    // second pack gives the helper enough context to collapse
                    // the remaining OUTER dense-string-array scan in one call.
                    let result_capture_count_sum = ((plan.result_global as u64) << 48)
                        | ((plan.capture as u64) << 32)
                        | ((plan.count_global as u64) << 16)
                        | plan.sum_global as u64;
                    let i_n_lines_re = ((plan.i_global as u64) << 48)
                        | ((plan.n_global as u64) << 32)
                        | ((plan.lines_global as u64) << 16)
                        | plan.re_global as u64;
                    dynasm!(ops
                        ; mov rcx, rdi
                        ; mov rdx, [rbx + dreg(iter)]
                        ; mov r8, [rbx + dreg(next)]
                        ; mov r9, QWORD result_capture_count_sum as i64
                        ; mov rax, QWORD i_n_lines_re as i64
                        ; mov [rsp + 32], rax
                        ; mov rax, QWORD heap.regexp_scalar_step as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail                      // no visible step committed
                        // Success returns false. Exhaustion returns true while
                        // retaining the range-only km across the unobservable
                        // outer-loop tail; the region epilogue flushes it.
                        ; mov [rbx + dreg(done_dst)], rax
                    );
                    // The step's safe point and Annex-B string publication can
                    // move every pinned side vector. Exhaustion additionally
                    // materializes the final observable result.
                    if refetch_pinned {
                        emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                    }
                    if let Some((snap, pin_plan)) = ta_refetch {
                        emit_refetch_ta(&mut ops, snap, pin_plan);
                    }
                    emit_region_bail(&mut ops, ip, bail, epilogue);
                    continue;
                }
                // The for-of step via `jit_iter_next` — serves the intrinsic
                // iterator kinds only (see the helper); anything else deopts
                // BEFORE state moves, so the interpreter re-executes this op
                // with full semantics (a persistent deopt evicts the region,
                // restoring the interpreted loop). The helper writes value/
                // done straight into the frame window (two outputs), so only
                // the status comes back in rax. `idx` (r9) is the dense-Array
                // positional cursor, advanced by the helper's plain-Array walk
                // and untouched on the intrinsic-iterator paths — exactly the
                // interpreter arm's behaviour.
                //
                // A match step ALLOCATES (result array + capture slices) and
                // runs `maybe_gc` (the loop's safe point), so re-derive the
                // pinned r13/r14 and TypedArray snapshots afterward — the
                // StrConcat discipline. No user code runs on the served paths.
                let packed = ((iter as u64) << 48)
                    | ((next as u64) << 32)
                    | ((value_dst as u64) << 16)
                    | done_dst as u64;
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, rbx                        // frame register window
                    ; mov r8, QWORD packed as i64         // iter/next/value/done regs
                    ; mov r9d, idx as i32                 // dense-Array cursor reg
                    ; mov rax, QWORD heap.iter_next as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail                          // non-intrinsic → redo in interp
                    ; mov r10, QWORD CALL_THREW as i64
                    ; cmp rax, r10
                    ; je => bail                          // threw (pending_throw set) → unwind, NOT redo
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::PushFinally {
                target,
                kind_reg,
                val_reg,
            } => {
                // Handler-stack push mirroring the interpreter arm, so the
                // unwind state stays identical whichever engine runs the loop
                // body. Total: no deopt, no alloc, no user code, no refetch.
                let packed = ((target as u64) << 32) | ((kind_reg as u64) << 16) | val_reg as u64;
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, QWORD packed as i64        // target/kind_reg/val_reg
                    ; mov rax, QWORD heap.push_finally as i64
                    ; call rax
                );
            }
            Instr::PopFinally => {
                // The pop half — same contract as PushFinally above.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rax, QWORD heap.pop_finally as i64
                    ; call rax
                );
            }
            Instr::IterCloseFinally { .. } => {
                if !scalar_matchall.is_some_and(|p| p.close_ip == ip) {
                    return None;
                }
                // Handler bodies remain interpreter-owned. No normal native
                // predecessor reaches this label, but fail closed if that CFG
                // invariant ever changes.
                dynasm!(ops
                    ; mov DWORD [rsi], ip as i32
                    ; jmp => epilogue
                );
            }
            Instr::EndFinally { .. } => {
                if !scalar_matchall.is_some_and(|p| p.end_finally_ip == ip) {
                    return None;
                }
                dynasm!(ops
                    ; mov DWORD [rsi], ip as i32
                    ; jmp => epilogue
                );
            }
            Instr::Return { .. } | Instr::ReturnUndefined => {
                // Resume interpreting at this ip so the interpreter performs the
                // return (popping frames is its job, not the region's).
                dynasm!(ops
                    ; mov DWORD [rsi], ip as i32
                    ; jmp => epilogue
                );
            }
            _ => return None, // region_can_compile already filtered; defensive
        }
    }

    // Hoisted-`.length` deopt landing: resume the loop in the interpreter.
    if hoisted_len.is_some() {
        dynasm!(ops
            ; => entry_len_bail
            ; mov DWORD [rsi], start as i32
            ; jmp => epilogue
        );
    }

    // ── exit stubs ── one per distinct out-of-region jump target: record the
    // resume ip and jump to the shared epilogue.
    for (target, label) in &exit_stubs {
        dynasm!(ops
            ; => *label
            ; mov DWORD [rsi], *target as i32
            ; jmp => epilogue
        );
    }

    // ── epilogue ── every native exit first publishes any pending scalar
    // result. This single closure covers guard deopts, committed throws,
    // eviction exits, out-of-region branches, and the defensive handler stubs.
    dynasm!(ops ; => epilogue);
    if let Some(plan) = scalar_matchall {
        dynasm!(ops
            ; mov rcx, rdi
            ; mov rdx, [rbx + dreg(plan.iter_reg)]
            ; mov r8d, plan.result_global as i32
            ; xor r9d, r9d
            ; mov rax, QWORD heap.regexp_scalar_flush as i64
            ; call rax
        );
    }
    if let Some(plan) = scalar_exec {
        dynasm!(ops
            ; mov rcx, rdi
            ; mov edx, plan.result_global as i32
            ; xor r8d, r8d
            ; mov rax, QWORD heap.regexp_scalar_exec_flush as i64
            ; call rax
        );
    }
    // Restore and return; [rsi] already holds the resume ip.
    dynasm!(ops
        ; add rsp, frame
        ; pop r14
        ; pop r13
        ; pop r12
        ; pop rdi
        ; pop rsi
        ; pop rbx
        ; ret
    );

    let buf = ops.finalize().ok()?;
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn {
        _buf: buf,
        entry: entry_ptr,
        self_binding: None,
    })
}

// ════════════════════════════════════════════════════════════════════════════
// Tier C — whole-FUNCTION memory-path JIT (`compile_proto_mem`)
//
// The whole-function JIT (`compile_proto`, Tier A) admits only int-arithmetic +
// SELF-recursive calls (fib-shaped). The OSR region JIT (Tier B,
// `compile_region_mem`) admits the full call-heavy op set (globals via the r12
// pin, GetIndex, charCodeAt, GENERAL calls via `emit_region_call_ic`) but ONLY
// fires on a canonical while-loop back-edge. Recursive-descent functions
// (`parse`'s tokIs/pFactor/pTerm/pExpr) have no hot LOOP and aren't self-only —
// neither tier reaches them, so they run entirely in the interpreter.
//
// Tier C closes that gap: it LIFTS Tier B's proven op emitters into Tier A's
// WHOLE-FUNCTION structure (a label per ip, a dedicated per-op bail recording
// that ip, a whole-fn prologue/epilogue). Each function is compiled
// independently; mutual recursion "just works" because a general call goes
// `emit_region_call_ic` → `jit_call_ic` → `setup_call` + `run_loop` the callee,
// which re-enters native at ITS own ip==0 gate. No direct Tier-C→Tier-C native
// calls — every cross-function call routes through the depth-capped
// (`JIT_REGION_CALL_MAX`) interpreter helper, so deep/mutual recursion deopts to
// flat interpreter frames (→ catchable RangeError) instead of overflowing the
// native stack.
//
// FRAME / ABI: identical to `compile_region_mem` — the win64 `JitFn` ABI
// (rcx=regs base, rdx=bail_ip out-ptr, r8=vm), a 6-push prologue (rbx/rsi/rdi
// pinned + r12=globals, r13/r14 saved-but-unused in v1), a 40-byte frame (32B
// shadow + 8B 5th-arg slot, 16-aligned), and a SHARED epilogue every Return/bail
// jumps to. This lets the region emitters (`dbinop`/`dcmp`/`region_poly_eq`/
// `emit_region_call_ic`/`emit_region_bail`) be reused VERBATIM. The ONE
// divergence from Tier B: `Return` produces a function RESULT (rax + NO_BAIL),
// not a resume-ip, because Tier C is entered at function entry (try_run_jit at
// ip==0) and its clean return pops the frame — not at a loop back-edge.
//
// v1 OP SET (exactly what the parse quartet needs): LoadInt/LoadBool/Move,
// general LoadGlobal/StoreGlobal[Strict], GetIndex (generic helper), AddInt/Sub,
// int/poly Eq/Ne + Lt/Le/Gt/Ge, Jump/JumpIf*, general Call, one charCodeAt
// CallMethod, Return/ReturnUndefined. Anything else declines (the function stays
// interpreted). Gated behind `ZIPP_FNJIT_MEM` until validated (see `Jit::compile`).

/// Set in `chain_capacity_hint`'s result at a chain's LAST link — the request
/// to hand back whatever the first link's estimate over-reserved. Kept clear
/// of the capacity's value range (a capacity is <= 256) and below bit 31, so
/// `hint as i32` stays positive and r9d zero-extends cleanly.
pub(crate) const CHAIN_HINT_LAST: u32 = 1 << 30;

/// Byte-capacity hint for `jit_concat_chain_fast` (r9d at each fused link).
/// A chain's statically-recognised FIRST link — the one whose accumulator's
/// nearest preceding writer in the instruction stream is the link-1 `Add` —
/// carries the estimate for the FINISHED chain (12 bytes per link, capped at
/// 256, the small-flat threshold) so the helper pre-sizes the builder once
/// instead of climbing the realloc ladder. The chain's LAST link — the one
/// after which `acc` is `Move`d out rather than concatenated again — carries
/// the SAME estimate plus `CHAIN_HINT_LAST`, which asks the helper to trim
/// the slack once the string is finished. Without the trim the estimate's
/// over-reservation is retained for the whole lifetime of every chain-built
/// string (nothing else ever shrinks a `JsStr`'s buffer): a 26-leaf chain of
/// one-char leaves holds a 256-byte buffer for 25 bytes of content, measured
/// at +194 MB of steady-state RSS over 1.2M retained strings. Every other
/// link carries 0. The forward count ends at the chain's trailing
/// `Move{src: acc}` (the lowering always emits one). Purely advisory in BOTH
/// directions: a misclassified link changes a buffer's capacity, never
/// content — so the linear scans may ignore control flow.
pub(crate) fn chain_capacity_hint(code: &[Instr], ip: usize, acc: u16, end: usize) -> u32 {
    debug_assert!(matches!(code[ip], Instr::StrConcatChain { .. }));
    debug_assert!(ip <= end && end < code.len());
    // Backward: walk to the chain's head (the link-1 `Add` that seeded `acc`),
    // stepping over this chain's own earlier links and counting them. `before
    // == 0` is exactly the old first-link test ("the nearest writer of `acc`
    // is an `Add`"), since a link that accumulates in place writes `acc`.
    let mut head = false;
    let mut before = 0u32;
    for j in (0..ip).rev() {
        match code[j] {
            Instr::Add { dst, .. } if dst == acc => {
                head = true;
                break;
            }
            Instr::StrConcatChain { a, .. } if a == acc => before += 1,
            ref i if crate::codegen::fn_int::writes_reg(i) == Some(acc) => break,
            _ => {}
        }
    }
    if !head {
        return 0;
    }
    // Forward: count this chain's remaining links up to the trailing Move.
    let mut after = 0u32;
    for j in ip + 1..=end.min(code.len() - 1) {
        match code[j] {
            Instr::StrConcatChain { a, .. } if a == acc => after += 1,
            Instr::Move { src, .. } if src == acc => break,
            ref i if crate::codegen::fn_int::writes_reg(i) == Some(acc) => break,
            _ => {}
        }
    }
    // the link-1 Add + this link + the links on either side of it
    let cap = ((before + after + 2) * 12).min(256);
    match (before, after) {
        // First link (a single-link chain included: it pre-sizes exactly as
        // before, and has no slack worth a second buffer move).
        (0, _) => cap,
        // Last link of a chain whose first link pre-sized the builder.
        (_, 0) => CHAIN_HINT_LAST | cap,
        _ => 0,
    }
}
