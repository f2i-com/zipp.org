// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

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
) -> Option<JitFn> {
    if !region_can_compile(proto, start, end, Some(const_strs)) {
        return None;
    }
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let (s, e) = (start as usize, end as usize);

    if std::env::var_os("ZIPP_JITDUMP").is_some() {
        for ip in s..=e {
            eprintln!("[dump] {ip}: {:?}", proto.code[ip]);
        }
    }

    // Does the region use the r13/r14 inline-cache pointers at all? Only
    // GetProp/SetProp read them; when absent, allocating/user-code helpers
    // skip the post-call re-fetch entirely.
    let has_prop = proto.code[s..=e]
        .iter()
        .any(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }));
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
    let refetch_pinned = has_prop || do_leaf || do_method;
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
            if proto.constants.get(idx as usize).is_some_and(|c| c.is_double()) {
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
            Instr::StoreGlobal { idx, src } | Instr::StoreGlobalStrict { idx, src } => {
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
                // `add_values` can run user coercion code (valueOf) — re-derive
                // the pinned TypedArray snapshots.
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
                dynasm!(ops ; => done_a);
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
            Instr::Sub { dst, a, b } => {
                dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Sub, int_hint(a, b))
            }
            Instr::Mul { dst, a, b } => {
                dbinop(&mut ops, ip, bail, epilogue, dst, a, b, DOp::Mul, int_hint(a, b))
            }
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
                dynasm!(ops
                    ; cvttsd2si rax, xmm0            // a → i64 (trunc toward 0)
                    ; cvttsd2si rcx, xmm1            // b → i64
                    ; test rcx, rcx
                    ; jz => bail                     // % 0 → NaN (interp)
                    ; cvtsi2sd xmm2, rax
                    ; ucomisd xmm2, xmm0
                    ; jne => bail                    // a not integer-valued → fmod
                    ; cvtsi2sd xmm2, rcx
                    ; ucomisd xmm2, xmm1
                    ; jne => bail                    // b not integer-valued → fmod
                    ; cqo                            // sign-extend rax into rdx:rax
                    ; idiv rcx                       // rdx = a % b (i64 remainder)
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
                // Negate via 0.0 - a (keeps it in the f64 domain).
                load_num_xmm(&mut ops, a, 1, bail);
                dynasm!(ops
                    ; xorps xmm0, xmm0
                    ; subsd xmm0, xmm1
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
                dynasm!(ops ; mov r8d, eax);             // stash a
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
            Instr::MathOp { dst, op, arg_base, argc } => {
                // Pure `Math.<op>`. Operands are loaded as numbers (Int/double);
                // a non-numeric operand BAILS to the interpreter, which runs the
                // full ToNumber coercion (a user valueOf). So the helpers below
                // never run user code and never allocate — no r13/r14/TA refetch.
                // Result boxed via `emit_box_num` (mirrors the interpreter's
                // `Value::num(r)` exactly: exact-int narrows, -0/NaN preserved).
                if argc == 1 {
                    load_num_xmm(&mut ops, arg_base, 0, bail);
                    dynasm!(ops
                        ; movq rdx, xmm0                  // arg f64 bits (arg1)
                        ; mov ecx, op as i32              // MathFn code (repr(u8), arg0)
                        ; mov rax, QWORD heap.math_unary as i64
                        ; call rax
                        ; movq xmm0, rax                  // result f64 bits
                    );
                    emit_box_num(&mut ops, dst);
                } else if matches!(op, MathFn::Imul) {
                    // `Math.imul(a,b)` INLINE — a 32-bit signed multiply, no FFI:
                    // ToInt32 both operands (a non-int-coercible operand BAILS, so
                    // the interpreter runs the full ToNumber incl. a user valueOf —
                    // matching the helper), then `imul` (low 32 bits, signed) boxed
                    // as Int. The low 32 bits of the product are identical whether
                    // the inputs were ToInt32 or ToUint32, so this equals the
                    // interpreter's `math_two(Imul)` exactly. (Twin of the Bitwise
                    // ops above — the dominant op in every hash/PRNG hot loop.)
                    load_toint32(&mut ops, arg_base, bail);
                    dynasm!(ops ; mov r8d, eax);
                    load_toint32(&mut ops, arg_base + 1, bail);
                    dynasm!(ops ; mov ecx, eax ; mov eax, r8d ; imul eax, ecx);
                    box_eax(&mut ops, dst);
                } else {
                    // EXACTLY two args (region_can_compile gated the op set).
                    load_num_xmm(&mut ops, arg_base, 0, bail);
                    load_num_xmm(&mut ops, arg_base + 1, 1, bail);
                    dynasm!(ops
                        ; movq rdx, xmm0                  // arg0 f64 bits (arg1)
                        ; movq r8, xmm1                   // arg1 f64 bits (arg2)
                        ; mov ecx, op as i32              // MathFn code (arg0)
                        ; mov rax, QWORD heap.math_two as i64
                        ; call rax
                        ; movq xmm0, rax
                    );
                    emit_box_num(&mut ops, dst);
                }
                emit_region_bail(&mut ops, ip, bail, epilogue);
            }
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
            Instr::ToNum { dst, a } => {
                // `+x`. A number passes through UNCHANGED — note the raw `mov`
                // rather than a round trip through xmm, which would re-tag an
                // Int as a double and diverge from the interpreter. Bool / null /
                // undefined / heap need ToNumber, which can run a user `valueOf`,
                // so they bail.
                let is_num = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(a)]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32
                    ; je => is_num                       // Int payload
                    ; sub r10d, (INT_TAG_HI + 1) as i32  // 0x7FFA (bool tag)
                    ; cmp r10d, 3                        // high16 in [0x7FFA,0x7FFD] ⇒ not a number
                    ; jbe => bail
                    ; => is_num                          // double falls through here
                    ; mov [rbx + dreg(dst)], rax
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
                // frame exactly as UpvalGet does. Bails on a malformed closure.
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
                // Per-op for-in liveness check (jit_forin_live → Vm::forin_live).
                // Re-reads the live shape each execution. Stores the Bool Value
                // bits the helper returns (matches the interpreter's
                // `Value::bool(live)`). Never deopts. The helper does no VM-heap
                // alloc on the common path, but `key_of`/proto-walk could grow the
                // heap in principle — so when the region also has GetProp/SetProp
                // (the only r13/r14 consumers), re-derive those pinned pointers
                // afterward (the StrConcat discipline). It runs NO user code, so
                // the TypedArray snapshots are unaffected.
                dynasm!(ops
                    ; mov rcx, rdi                       // vm
                    ; mov rdx, [rbx + dreg(obj)]         // obj bits
                    ; mov r8, [rbx + dreg(key)]          // key bits
                    ; mov rax, QWORD heap.forin_live as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax         // Bool Value bits
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
            }
            Instr::HasProp { dst, key, obj, brand: _ } => {
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
                    .filter(|&(_, kind)| kind == ARR_PIN_KIND);
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
                        ; jmp => hp_done
                        ; => hp_slow
                    );
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
            Instr::Lt { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Lt),
            Instr::Le { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Le),
            Instr::Gt { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Gt),
            Instr::Ge { dst, a, b } => dcmp(&mut ops, ip, bail, epilogue, dst, a, b, Cmp::Ge),
            // `===` / `!==` are polymorphic: numeric operands compare as f64,
            // interned single-char strings / Int / Bool / Null / Undefined
            // compare by bits, non-interned heap operands bail to the interpreter.
            Instr::Eq { dst, a, b } => {
                region_poly_eq(&mut ops, ip, bail, epilogue, dst, a, b, false, heap.strict_eq)
            }
            Instr::Ne { dst, a, b } => {
                region_poly_eq(&mut ops, ip, bail, epilogue, dst, a, b, true, heap.strict_eq)
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
                // ── 8-way inline cache (CALL-FREE on hit) ── probe the site's
                // ways: receiver identity (obj_bits) + live receiver version,
                // then (for proto-chain ways) the live version of each guarded
                // hop; a full match reads the HOLDER's `vals_ptr[slot]`
                // directly. All ways miss ⇒ the helper re-fills one way. r14 =
                // IC table base, r13 = heap version-array base. See `IcEntry`
                // for the layout (stride 40, hops at +24/+32, u32::MAX = none).
                //
                // SAFETY (`[r13 + idx*4]` reads are in-bounds): the receiver
                // version is read only after the identity match against a
                // FILLED way, whose obj_bits the helper validated as a live
                // heap Object ⇒ heap_idx < versions.len() (which never
                // shrinks). Hop indices were likewise valid heap indices at
                // fill. Staleness is harmless for the LOADS (in-bounds) and
                // caught by the version compares before any vals deref.
                let off = (ic_site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
                let packed = ((heap.func_id as u64) << 32) | name as u64;
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                let probe = ops.new_dynamic_label();
                let next = ops.new_dynamic_label();
                let hit = ops.new_dynamic_label();
                let miss = ops.new_dynamic_label();
                let via_ic = ops.new_dynamic_label();
                let cont = ops.new_dynamic_label();
                let hop = ops.new_dynamic_label();
                // Stage 5: inline a trivial class GETTER for this `o.v` site as a
                // per-receiver guard tree (a pure prefix). A hit writes `dst` and
                // jumps to `cont`; all-miss falls through to the IC probe below
                // (which routes a real accessor via PROP_VIA_IC → helper).
                if let Some(gp) = method_plan.get(&ip) {
                    emit_inline_accessor(
                        &mut ops, ip, epilogue, leaf_flag_off, gp, obj, dst, false, cont,
                    );
                }
                dynasm!(ops
                    ; mov rax, [rbx + dreg(obj)]          // receiver bits (probe-invariant)
                    ; lea r9, [r14 + off]                 // way 0 of this site
                    ; mov r8d, JIT_IC_WAYS as i32
                    ; => probe
                    ; cmp rax, [r9]                       // identity (empty 0 never matches)
                    ; jne => next
                    ; mov ecx, eax                        // recv heap idx (low 32)
                    ; mov edx, [r13 + rcx*4]              // live recv version
                    ; cmp edx, [r9 + 16]
                    ; jne => next
                    ; mov ecx, [r9 + 20]
                    ; shr ecx, 24                         // nhops (0 = own)
                    ; test ecx, ecx
                    ; jz => hit
                    ; lea r10, [r9 + 24]                  // hop cursor
                    ; => hop
                    ; mov edx, [r10]                      // hop heap idx
                    ; mov r11d, [r13 + rdx*4]             // live hop version
                    ; cmp r11d, [r10 + 4]
                    ; jne => next
                    ; add r10, 8
                    ; dec ecx
                    ; jnz => hop
                    ; => hit
                    ; mov rcx, [r9 + 8]                   // holder vals_ptr
                    ; mov edx, [r9 + 20]
                    ; and edx, 0x00FF_FFFF                // slot (low 24)
                    ; mov rax, [rcx + rdx*8]              // vals[slot] (CALL-FREE)
                    ; mov [rbx + dreg(dst)], rax
                    ; jmp => cont
                    ; => next
                    ; add r9, JIT_IC_STRIDE as i32
                    ; dec r8d
                    ; jnz => probe
                    ; jmp => miss
                    ; => miss
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
            Instr::SetProp { obj, name, val } => {
                // ── 8-way inline cache (CALL-FREE write on hit) ── like
                // GetProp, but the helper only ever fills OWN ways here
                // (identity + receiver version fully guard an own writable
                // data slot: any redefinition/freeze/delete/proto change bumps
                // the version), so the probe skips the hop checks.
                let off = (ic_site as usize * JIT_IC_WAYS * JIT_IC_STRIDE) as i32;
                let packed = ((heap.func_id as u64) << 32) | name as u64;
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                let probe = ops.new_dynamic_label();
                let next = ops.new_dynamic_label();
                let cont = ops.new_dynamic_label();
                // Stage 5: inline a trivial class SETTER for this `o.v = x` site as
                // a per-receiver guard tree (a pure prefix). A hit does the baked
                // store and jumps to `cont`; all-miss falls through to the IC probe
                // (a real setter → PROP_VIA_IC → helper).
                if let Some(sp) = method_plan.get(&ip) {
                    emit_inline_accessor(
                        &mut ops, ip, epilogue, leaf_flag_off, sp, obj, val, true, cont,
                    );
                }
                dynasm!(ops
                    ; mov rax, [rbx + dreg(obj)]          // receiver bits
                    ; lea r9, [r14 + off]
                    ; mov r8d, JIT_IC_WAYS as i32
                    ; => probe
                    ; cmp rax, [r9]                       // identity
                    ; jne => next
                    ; mov ecx, eax                        // recv heap idx
                    ; mov edx, [r13 + rcx*4]              // live recv version
                    ; cmp edx, [r9 + 16]
                    ; jne => next
                    ; mov rcx, [r9 + 8]                   // vals_ptr
                    ; mov edx, [r9 + 20]                  // slot
                    ; mov r10, [rbx + dreg(val)]          // val_bits
                    ; mov [rcx + rdx*8], r10              // vals[slot] = val (CALL-FREE)
                    ; jmp => cont
                    ; => next
                    ; add r9, JIT_IC_STRIDE as i32
                    ; dec r8d
                    ; jnz => probe
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
                    if kind == ARR_PIN_KIND {
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
                        emit_ta_key(&mut ops, key, bail);     // rcx = i64 index
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
                    emit_ta_key(&mut ops, key, bail);     // rcx = i64 index
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
                    .filter(|&(_, kind)| kind != ARR_PIN_KIND);
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
                    emit_ta_key(&mut ops, key, bail);     // rcx = i64 index
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
            Instr::CallMethod { dst, obj, name, arg_base, argc } => {
                let key = proto.string_constants[name as usize].as_str();
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
                    let (dv_slow, dv_done) =
                        (ops.new_dynamic_label(), ops.new_dynamic_label());
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
            Instr::Call { dst, callee, arg_base, argc } => {
                // Generic `f(args…)` with `this = undefined`: the interpreter-IC
                // call helper. Packing: r9 = (callee<<16) | arg_base.
                let packed_fip = ((heap.func_id as u64) << 32) | ip as u64;
                let packed_args = ((callee as u64) << 16) | arg_base as u64;
                // Q4 leaf-call inlining: a monomorphic plain-leaf callee at this
                // site is inlined with an identity guard; a guard miss / tight
                // headroom falls through to the SAME helper below (a pure prefix).
                if let Some(lp) = leaf_plan.get(&ip) {
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
            Instr::StrAppendInPlace { dst, a, b } => {
                // In-place `dst = a + b` via `jit_str_append` (mutates a's buffer
                // when uniquely owned — the emitter proved linearity). Never
                // deopts, but uses the same ABI; allocates/grows the heap, so
                // (like StrConcat) re-derive r13 when the region reads it.
                dynasm!(ops
                    ; mov rcx, rdi                        // vm
                    ; mov rdx, [rbx + dreg(a)]            // a (accumulator) bits
                    ; mov r8, [rbx + dreg(b)]             // b (appended) bits
                    ; mov rax, QWORD heap.str_append as i64
                    ; call rax
                    ; mov [rbx + dreg(dst)], rax
                );
                if refetch_pinned {
                    emit_refetch_pinned(&mut ops, heap.versions_base, Some(heap.ic_base));
                }
                if let Some((snap, plan)) = ta_refetch {
                    emit_refetch_ta(&mut ops, snap, plan);
                }
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

    // ── epilogue ── restore and return; [rsi] already holds the resume ip.
    dynasm!(ops
        ; => epilogue
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
    Some(JitFn { _buf: buf, entry: entry_ptr })
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

