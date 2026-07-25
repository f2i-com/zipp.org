// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Emit a generic `CallMethod` / `Call` region op as a `jit_call_method_ic` /
/// `jit_call_ic` helper call (vm/helpers_misc.rs). The helper consults the
/// interpreter's per-site inline cache, frame-calls the resolved plain user
/// function to completion, and returns the result bits — or `SELF_CALL_DEOPT`
/// (nothing happened: IC miss / megamorphic / depth limit → the interpreter
/// re-executes this op) or `CALL_THREW` (the call ran and THREW:
/// `pending_throw` is set; the OSR caller unwinds instead of resuming). Both
/// sentinels bail at this ip. ABI: rcx=vm, rdx=caller window base (rbx),
/// r8=(func_id<<32)|ip, r9=op-specific packing, [rsp+32]=argc (5th arg).
/// After a successful call, `refetch` re-derives r13/r14 (only needed when the
/// region has GetProp/SetProp sites — the only r13/r14 consumers).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_region_call_ic(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    helper: usize,
    packed_fip: u64,
    packed_args: u64,
    argc: u16,
    dst: u16,
    refetch: Option<(usize, usize)>,
    ta_refetch: Option<(usize, &TaPinPlan)>,
) {
    dynasm!(ops
        ; mov rcx, rdi                          // vm
        ; mov rdx, rbx                          // caller window base ptr
        ; mov r8, QWORD packed_fip as i64       // (func_id << 32) | ip
        ; mov r9, QWORD packed_args as i64      // name/callee/obj/arg_base packing
        ; mov DWORD [rsp + 32], argc as i32     // 5th arg: argc
        ; mov rax, QWORD helper as i64
        ; call rax
        ; mov r10, QWORD SELF_CALL_DEOPT as i64
        ; cmp rax, r10
        ; je => bail                            // IC miss/depth → redo in interp
        ; mov r10, QWORD CALL_THREW as i64
        ; cmp rax, r10
        ; je => bail                            // threw → exit; caller unwinds
        ; mov [rbx + dreg(dst)], rax
    );
    if let Some((vb, icb)) = refetch {
        emit_refetch_pinned(ops, vb, Some(icb));
    }
    // The call ran user code, which may have detached/resized a pinned
    // TypedArray's buffer (or reassigned its source) — re-derive the snapshots.
    if let Some((snap, plan)) = ta_refetch {
        emit_refetch_ta(ops, snap, plan);
    }
    emit_region_bail(ops, ip, bail, epilogue);
}

/// Q4 leaf-call inlining (v1): emit a guarded INLINE expansion of a monomorphic
/// plain-leaf callee for a region `Call` op, with a fallback to the unchanged
/// per-call helper. Emitted INSTEAD of `emit_region_call_ic` when a `LeafInlinePlan`
/// exists for this ip.
///
/// Shape:
/// ```text
///   ; guard: regs[callee] == callee_bits     ; miss → fallback
///   ; headroom flag == 0                      ; tight → fallback
///   ; regs[W+0] = undefined ; regs[W+1+i] = regs[arg_base+i]   (arg copy)
///   ; <inlined body over scratch window W>    ; any bail → resume at CALL IP
///   ; regs[dst] = <return value>
///   ; jmp done
/// fallback:
///   ; <emit_region_call_ic — the existing helper, a pure prefix>
/// done:
/// ```
/// SOUNDNESS: the guard miss and the headroom-tight case both fall to the helper
/// (a PURE PREFIX — never deopts/evicts the region). The body is straight-line
/// (`callee_leaf_ok`); any inlined-op bail records the CALL IP and exits to the
/// epilogue, so the interpreter re-runs the WHOLE call (the side-effect-freedom-
/// before-deopt rule guarantees no global write happened yet). The body touches
/// only the scratch window (regs `W..W+callee_reg_count`, inside the pinned,
/// headroom-checked register file) and globals (r12); it runs no GC safepoint,
/// allocates nothing, and calls nothing — so r12/r13/r14 and the TA pins stay
/// valid and need no re-fetch, and the scratch slots need no zero-fill (the body
/// writes every reg it reads — see `callee_leaf_ok`: a leaf reads only its params
/// (copied above), globals, and its own freshly-computed regs).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_inline_leaf_call(
    ops: &mut dynasmrt::x64::Assembler,
    call_ip: usize,
    epilogue: dynasmrt::DynamicLabel,
    leaf_flag_off: i32,
    plan: &LeafInlinePlan,
    callee: u16,
    arg_base: u16,
    argc: u16,
    dst: u16,
    math_unary: usize,
    math_two: usize,
    // Helpers for the v2 body ops (DISTINCT names — do NOT confuse with `helper`
    // below, which is the fallback call_ic; a literal copy of a region template's
    // `QWORD helper` would emit `call call_ic` with the wrong ABI). Param order is
    // load-bearing (all usize → a mis-order is a silent miscompile): keep it in
    // sync with BOTH call sites.
    gidx_helper: usize, // jit_get_index   (dense-array / string element read)
    cc_helper: usize,   // jit_char_code_at
    strict_eq: usize,   // jit_strict_eq   (Eq/Ne slow path)
    truthy: usize,      // jit_truthy      (JumpIf* non-Int/Bool condition)
    // Fallback emission (the unchanged per-call helper = call_ic).
    helper: usize,
    packed_fip: u64,
    packed_args: u64,
    refetch: Option<(usize, usize)>,
    ta_refetch: Option<(usize, &TaPinPlan)>,
) {
    let fallback = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    let w = plan.reg_window;
    // ── identity guard ── the callee register must hold EXACTLY the cached
    // function value. A miss (callee reassigned, or a 2nd shape appears at this
    // now-not-really-mono site) takes the helper — never evicts the region.
    dynasm!(ops
        ; mov rax, [rbx + dreg(callee)]
        ; mov r10, QWORD plan.callee_bits as i64
        ; cmp rax, r10
        ; jne => fallback
        // ── version guard ── heap Value bits are pure `TAG_HEAP|idx`; a GC'd +
        // reused callee slot keeps IDENTICAL bits but bumps its `versions[idx]`.
        // The bits compare alone would then PASS and run the STALE old callee
        // body. Re-check the live slot version against the baked one (exactly the
        // `(bits, version)` tuple `ic_call` checks) — a mismatch falls to the
        // helper, which re-resolves the call correctly. `rax` still holds the
        // callee bits; its low 32 bits are the heap index. r13 = pinned heap
        // version-array base (re-derived after any allocating helper because the
        // region inlines a call — see `refetch_pinned`). The read is in-bounds:
        // the index came from a live heap Value (the bits matched) and `versions`
        // never shrinks; staleness is caught by this very compare.
        ; mov ecx, eax                          // recv heap idx (low 32 of bits)
        ; mov edx, [r13 + rcx*4]                // live slot version
        ; cmp edx, DWORD plan.callee_ver as i32
        ; jne => fallback
        // ── headroom flag ── 0 ⇒ the scratch window might overflow the pinned
        // register file (near-MAX_FRAMES recursion) → take the helper.
        ; cmp QWORD [rsp + leaf_flag_off], 0
        ; je => fallback
        // ── arg binding ── reg 0 (callee `this`) = undefined; positional args
        // into W+1.. (a leaf with simple_params binds args positionally). Args
        // beyond `param_count`/`argc` are ignored by a leaf body (no
        // `arguments`); params beyond `argc` stay undefined (the slot is zeroed
        // here so a stale scratch value can't leak in).
        ; mov rax, QWORD Value::UNDEFINED.bits() as i64
        ; mov [rbx + dreg(w)], rax
    );
    let n = argc.min(plan.param_count);
    for i in 0..plan.param_count {
        if i < n {
            dynasm!(ops
                ; mov rax, [rbx + dreg(arg_base + i)]
                ; mov [rbx + dreg(w + 1 + i)], rax
            );
        } else {
            dynasm!(ops
                ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                ; mov [rbx + dreg(w + 1 + i)], rax
            );
        }
    }
    // ── zero-fill the callee's LOCALS (regs past `this`+params) to undefined ──
    // exactly as `setup_call` resizes the whole callee window to UNDEFINED. The
    // leaf body may read a local before writing it (e.g. `var x; return a + x;`
    // reads the uninitialized `x`); without this, that read would pick up a
    // STALE Value left in the carved scratch window by a prior call's expansion.
    {
        let undef = Value::UNDEFINED.bits() as i64;
        let first_local = 1 + plan.param_count; // reg index past `this` + params
        if first_local < plan.callee_reg_count {
            dynasm!(ops ; mov rax, QWORD undef);
            for r in first_local..plan.callee_reg_count {
                dynasm!(ops ; mov [rbx + dreg(w + r)], rax);
            }
        }
    }
    // ── inline the body over the scratch window ── every register `r` maps to
    // `w + r`. Each op that can bail uses a FRESH bail label whose block resumes
    // the interpreter at the CALL IP (so the whole call re-runs cleanly).
    let rg = |r: u16| w + r; // scratch-window register mapping
    // One label per body ip (plus a fall-off sink at `body.len()`), so FORWARD
    // in-body branches (`callee_leaf_ok` admits only `> i && <= term`) re-base to
    // `blabels[target]`. Body index == callee ip (the body is the contiguous
    // truncated prefix `full[..=term]`). Control converges on the single trailing
    // Return; the sink is never reached for a well-formed body (no target == len).
    let blabels: Vec<dynasmrt::DynamicLabel> =
        (0..=plan.body.len()).map(|_| ops.new_dynamic_label()).collect();
    let mut ret_reg: Option<u16> = None;
    for (bi, instr) in plan.body.iter().enumerate() {
        dynasm!(ops ; => blabels[bi]);
        match *instr {
            Instr::LoadInt { dst: d, val } => {
                let boxed = INT_TAG | (val as u32 as u64);
                dynasm!(ops
                    ; mov rax, QWORD boxed as i64
                    ; mov [rbx + dreg(rg(d))], rax
                );
            }
            Instr::LoadConst { dst: d, idx } => {
                // `callee_leaf_ok` already restricted these to numeric consts.
                let bits = plan.const_bits(idx);
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(rg(d))], rax
                );
            }
            Instr::LoadBool { dst: d, val } => {
                let bits = BOOL_TAG | (val as u64);
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(rg(d))], rax
                );
            }
            Instr::Move { dst: d, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(rg(src))]
                    ; mov [rbx + dreg(rg(d))], rax
                );
            }
            Instr::LoadGlobal { dst: d, idx } => {
                dynasm!(ops
                    ; mov rax, [r12 + (idx as i32) * 8]
                    ; mov [rbx + dreg(rg(d))], rax
                );
            }
            Instr::StoreGlobal { idx, src } | Instr::StoreGlobalStrict { idx, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(rg(src))]
                    ; mov [r12 + (idx as i32) * 8], rax
                );
            }
            Instr::Add { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), DOp::Add, true)
            }
            Instr::Sub { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), DOp::Sub, true)
            }
            Instr::Mul { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), DOp::Mul, true)
            }
            Instr::Div { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), DOp::Div, false)
            }
            Instr::Mod { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                let as_dbl = ops.new_dynamic_label();
                let mod_done = ops.new_dynamic_label();
                load_num_xmm(ops, rg(a), 0, bail);
                load_num_xmm(ops, rg(b), 1, bail);
                dynasm!(ops
                    ; cvttsd2si rax, xmm0
                    ; cvttsd2si rcx, xmm1
                    ; test rcx, rcx
                    ; jz => bail
                    ; cvtsi2sd xmm2, rax
                    ; ucomisd xmm2, xmm0
                    ; jp => bail                     // NaN: unordered, `jne` misses
                    ; jne => bail
                    ; cvtsi2sd xmm2, rcx
                    ; ucomisd xmm2, xmm1
                    ; jp => bail
                    ; jne => bail
                    ; cqo
                    ; idiv rcx
                    ; movsxd r8, edx
                    ; cmp r8, rdx
                    ; jne => as_dbl
                    ; mov r8, QWORD INT_TAG as i64
                    ; mov eax, edx
                    ; or rax, r8
                    ; mov [rbx + dreg(rg(d))], rax
                    ; jmp => mod_done
                    ; => as_dbl
                    ; cvtsi2sd xmm0, rdx
                    ; movq rax, xmm0
                    ; mov [rbx + dreg(rg(d))], rax
                    ; => mod_done
                );
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            Instr::AddInt { dst: d, a, imm, .. } => {
                let bail = ops.new_dynamic_label();
                let f64_path = ops.new_dynamic_label();
                let done_ai = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(rg(a))]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32
                    ; jne => f64_path
                    ; add eax, imm
                    ; jo => f64_path
                );
                box_eax(ops, rg(d));
                dynasm!(ops ; jmp => done_ai ; => f64_path);
                load_num_xmm(ops, rg(a), 0, bail);
                dynasm!(ops
                    ; mov eax, imm
                    ; cvtsi2sd xmm1, eax
                    ; addsd xmm0, xmm1
                );
                store_xmm(ops, rg(d));
                dynasm!(ops ; => done_ai);
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            Instr::Neg { dst: d, a } => {
                let bail = ops.new_dynamic_label();
                load_num_xmm(ops, rg(a), 1, bail);
                dynasm!(ops
                    ; xorps xmm0, xmm0
                    ; subsd xmm0, xmm1
                );
                store_xmm(ops, rg(d));
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            Instr::Bitwise { dst: d, a, b, op } => {
                use crate::bytecode::BitwiseOp as B;
                let bail = ops.new_dynamic_label();
                load_toint32(ops, rg(a), bail);
                dynasm!(ops ; mov r8d, eax);
                load_toint32(ops, rg(b), bail);
                dynasm!(ops ; mov ecx, eax ; mov eax, r8d);
                match op {
                    B::And => { dynasm!(ops ; and eax, ecx); box_eax(ops, rg(d)); }
                    B::Or => { dynasm!(ops ; or eax, ecx); box_eax(ops, rg(d)); }
                    B::Xor => { dynasm!(ops ; xor eax, ecx); box_eax(ops, rg(d)); }
                    B::Shl => { dynasm!(ops ; shl eax, cl); box_eax(ops, rg(d)); }
                    B::Shr => { dynasm!(ops ; sar eax, cl); box_eax(ops, rg(d)); }
                    B::Ushr => {
                        let as_dbl = ops.new_dynamic_label();
                        let done_u = ops.new_dynamic_label();
                        dynasm!(ops
                            ; shr eax, cl
                            ; test eax, eax
                            ; js => as_dbl
                        );
                        box_eax(ops, rg(d));
                        dynasm!(ops
                            ; jmp => done_u
                            ; => as_dbl
                            ; mov eax, eax
                            ; cvtsi2sd xmm0, rax
                            ; movq rax, xmm0
                            ; mov [rbx + dreg(rg(d))], rax
                            ; => done_u
                        );
                    }
                }
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            Instr::MathOp { dst: d, op, arg_base: ab, argc: ac } => {
                let bail = ops.new_dynamic_label();
                if ac == 1 {
                    load_num_xmm(ops, rg(ab), 0, bail);
                    dynasm!(ops
                        ; movq rdx, xmm0
                        ; mov ecx, op as i32
                        ; mov rax, QWORD math_unary as i64
                        ; call rax
                        ; movq xmm0, rax
                    );
                    emit_box_num(ops, rg(d));
                } else if matches!(op, MathFn::Imul) {
                    // `Math.imul` INLINE (native 32-bit signed multiply, no FFI) —
                    // see the mem-path MathOp arm for the soundness rationale. This
                    // makes an inlined leaf's imul (e.g. the FNV/PRNG `mix` body)
                    // native too.
                    load_toint32(ops, rg(ab), bail);
                    dynasm!(ops ; mov r8d, eax);
                    load_toint32(ops, rg(ab + 1), bail);
                    dynasm!(ops ; mov ecx, eax ; mov eax, r8d ; imul eax, ecx);
                    box_eax(ops, rg(d));
                } else {
                    load_num_xmm(ops, rg(ab), 0, bail);
                    load_num_xmm(ops, rg(ab + 1), 1, bail);
                    dynasm!(ops
                        ; movq rdx, xmm0
                        ; movq r8, xmm1
                        ; mov ecx, op as i32
                        ; mov rax, QWORD math_two as i64
                        ; call rax
                        ; movq xmm0, rax
                    );
                    emit_box_num(ops, rg(d));
                }
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            // ── forward in-body control flow (re-based to the body labels) ──
            Instr::Jump { target } => {
                dynasm!(ops ; jmp => blabels[target as usize]);
            }
            Instr::JumpIfFalse { cond, target } | Instr::JumpIfTrue { cond, target } => {
                // Int/Bool condition tests its payload; anything else asks the
                // read-only `jit_truthy` helper (alloc-free, no user code → no
                // refetch). Mirrors the region JumpIf arm.
                let if_false = matches!(*instr, Instr::JumpIfFalse { .. });
                let t = blabels[target as usize];
                let testit = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(rg(cond))]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32
                    ; je => testit
                    ; cmp r10d, (INT_TAG_HI + 1) as i32
                    ; je => testit
                    ; mov rcx, rdi
                    ; mov rdx, rax
                    ; mov rax, QWORD truthy as i64
                    ; call rax
                    ; => testit
                    ; test eax, eax
                );
                if if_false {
                    dynasm!(ops ; jz => t);
                } else {
                    dynasm!(ops ; jnz => t);
                }
            }
            // ── dense-array / string element read `a[i]` ── generic win64 helper
            // (read-only, no alloc, no user code → no r13/r14/TA refetch; matches
            // the region GetIndex generic tail). Deopt sentinel → re-run the call.
            Instr::GetIndex { dst: d, obj, key } => {
                let bail = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(rg(obj))]
                    ; mov r8, [rbx + dreg(rg(key))]
                    ; mov rax, QWORD gidx_helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(rg(d))], rax
                );
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            // ── str.charCodeAt(i) ── `callee_leaf_ok` admitted only this 1-arg
            // method. Uses `cc_helper` (jit_char_code_at) — NEVER the fallback
            // `helper` (call_ic). Read-only, no alloc → no refetch.
            Instr::CallMethod { dst: d, obj, arg_base: ab, .. } => {
                let bail = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, [rbx + dreg(rg(obj))]
                    ; mov r8, [rbx + dreg(rg(ab))]
                    ; mov rax, QWORD cc_helper as i64
                    ; call rax
                    ; mov r10, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, r10
                    ; je => bail
                    ; mov [rbx + dreg(rg(d))], rax
                );
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            // ── comparisons ── region_poly_eq / dcmp both emit their own bail
            // block internally (record CALL ip → epilogue).
            Instr::Eq { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                region_poly_eq(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), false, strict_eq);
            }
            Instr::Ne { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                region_poly_eq(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), true, strict_eq);
            }
            Instr::Lt { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dcmp(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), Cmp::Lt);
            }
            Instr::Le { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dcmp(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), Cmp::Le);
            }
            Instr::Gt { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dcmp(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), Cmp::Gt);
            }
            Instr::Ge { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dcmp(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), Cmp::Ge);
            }
            Instr::Return { src } => {
                ret_reg = Some(rg(src));
            }
            Instr::ReturnUndefined => {
                ret_reg = None;
            }
            // `callee_leaf_ok` guarantees the body contains only the ops above.
            ref other => unreachable!("inline leaf body op not admitted: {other:?}"),
        }
    }
    // Fall-off sink: a target == body.len() (none for a well-formed single-return
    // body) lands here and falls through to the return-store.
    dynasm!(ops ; => blabels[plan.body.len()]);
    // ── store the return value into the caller's `dst` ──
    match ret_reg {
        Some(r) => dynasm!(ops
            ; mov rax, [rbx + dreg(r)]
            ; mov [rbx + dreg(dst)], rax
        ),
        None => dynasm!(ops
            ; mov rax, QWORD Value::UNDEFINED.bits() as i64
            ; mov [rbx + dreg(dst)], rax
        ),
    }
    dynasm!(ops
        ; jmp => done
        ; => fallback
    );
    // ── fallback ── the UNCHANGED per-call helper (a pure prefix). On a clean
    // return it writes `dst`; on its own deopt/throw sentinel it bails at this
    // ip via the bail label `emit_region_call_ic` creates internally.
    let helper_bail = ops.new_dynamic_label();
    emit_region_call_ic(
        ops, call_ip, helper_bail, epilogue, helper, packed_fip, packed_args, argc, dst, refetch,
        ta_refetch,
    );
    dynasm!(ops ; => done);
}

/// Emit one inlined method body's ops over the scratch window based at `win`
/// (callee reg `r` -> rbx-reg `win + r`). The caller has already bound reg 0
/// (`this`), the params, and zero-filled the locals. Handles the straight-line
/// no-`super` ops directly, and recursively expands a `SuperMethod` (looked up in
/// `supers` by its body index) over its own `win_off` sub-window (reg 0 = the
/// SAME receiver). `vals_ptr` is the receiver's baked ObjMap `vals` base — shared
/// by this body AND its inlined super bodies (same receiver). Returns the body's
/// return register (in the `win` window), or `None` for `ReturnUndefined`. A
/// number-guard bail re-runs the WHOLE call via `epilogue` (nothing committed); a
/// super-chain version-guard miss jumps to `fallback` (the per-call helper).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_mi_body(
    ops: &mut dynasmrt::x64::Assembler,
    call_ip: usize,
    epilogue: dynasmrt::DynamicLabel,
    fallback: dynasmrt::DynamicLabel,
    body: &[Instr],
    supers: &FxHashMap<usize, SuperInline>,
    field_slots: &FxHashMap<u32, u32>,
    consts: &FxHashMap<u32, u64>,
    vals_ptr: u64,
    win: u16,
) -> Option<u16> {
    let rg = |r: u16| win + r;
    let mut ret_reg: Option<u16> = None;
    for (bi, instr) in body.iter().enumerate() {
        match *instr {
            Instr::LoadInt { dst: d, val } => {
                let boxed = INT_TAG | (val as u32 as u64);
                dynasm!(ops ; mov rax, QWORD boxed as i64 ; mov [rbx + dreg(rg(d))], rax);
            }
            Instr::LoadConst { dst: d, idx } => {
                let bits = consts.get(&idx).copied().unwrap_or(0);
                dynasm!(ops ; mov rax, QWORD bits as i64 ; mov [rbx + dreg(rg(d))], rax);
            }
            Instr::LoadBool { dst: d, val } => {
                let bits = BOOL_TAG | (val as u64);
                dynasm!(ops ; mov rax, QWORD bits as i64 ; mov [rbx + dreg(rg(d))], rax);
            }
            Instr::Move { dst: d, src } => {
                dynasm!(ops ; mov rax, [rbx + dreg(rg(src))] ; mov [rbx + dreg(rg(d))], rax);
            }
            // `this.<field>` — an own DATA slot, baked behind the receiver guard
            // (the version guard guarantees `vals_ptr`/`slot` are still valid).
            Instr::GetProp { dst: d, obj: 0, name } => {
                let slot = field_slots.get(&name).copied().unwrap_or(0);
                dynasm!(ops
                    ; mov rcx, QWORD vals_ptr as i64
                    ; mov rax, [rcx + (slot as i32) * 8]
                    ; mov [rbx + dreg(rg(d))], rax
                );
            }
            Instr::Add { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), DOp::Add, true)
            }
            Instr::Sub { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), DOp::Sub, true)
            }
            Instr::Mul { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), DOp::Mul, true)
            }
            Instr::Div { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(ops, call_ip, bail, epilogue, rg(d), rg(a), rg(b), DOp::Div, false)
            }
            Instr::Mod { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                let as_dbl = ops.new_dynamic_label();
                let mod_done = ops.new_dynamic_label();
                load_num_xmm(ops, rg(a), 0, bail);
                load_num_xmm(ops, rg(b), 1, bail);
                dynasm!(ops
                    ; cvttsd2si rax, xmm0
                    ; cvttsd2si rcx, xmm1
                    ; test rcx, rcx
                    ; jz => bail
                    ; cvtsi2sd xmm2, rax
                    ; ucomisd xmm2, xmm0
                    ; jp => bail                     // NaN: unordered, `jne` misses
                    ; jne => bail
                    ; cvtsi2sd xmm2, rcx
                    ; ucomisd xmm2, xmm1
                    ; jp => bail
                    ; jne => bail
                    ; cqo
                    ; idiv rcx
                    ; movsxd r8, edx
                    ; cmp r8, rdx
                    ; jne => as_dbl
                    ; mov r8, QWORD INT_TAG as i64
                    ; mov eax, edx
                    ; or rax, r8
                    ; mov [rbx + dreg(rg(d))], rax
                    ; jmp => mod_done
                    ; => as_dbl
                    ; cvtsi2sd xmm0, rdx
                    ; movq rax, xmm0
                    ; mov [rbx + dreg(rg(d))], rax
                    ; => mod_done
                );
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            Instr::AddInt { dst: d, a, imm, .. } => {
                let bail = ops.new_dynamic_label();
                let f64_path = ops.new_dynamic_label();
                let done_ai = ops.new_dynamic_label();
                dynasm!(ops
                    ; mov rax, [rbx + dreg(rg(a))]
                    ; mov r10, rax
                    ; shr r10, 48
                    ; cmp r10d, INT_TAG_HI as i32
                    ; jne => f64_path
                    ; add eax, imm
                    ; jo => f64_path
                );
                box_eax(ops, rg(d));
                dynasm!(ops ; jmp => done_ai ; => f64_path);
                load_num_xmm(ops, rg(a), 0, bail);
                dynasm!(ops
                    ; mov eax, imm
                    ; cvtsi2sd xmm1, eax
                    ; addsd xmm0, xmm1
                );
                store_xmm(ops, rg(d));
                dynasm!(ops ; => done_ai);
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            Instr::Neg { dst: d, a } => {
                let bail = ops.new_dynamic_label();
                load_num_xmm(ops, rg(a), 1, bail);
                dynasm!(ops
                    ; xorps xmm0, xmm0
                    ; subsd xmm0, xmm1
                );
                store_xmm(ops, rg(d));
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            Instr::Bitwise { dst: d, a, b, op } => {
                use crate::bytecode::BitwiseOp as B;
                let bail = ops.new_dynamic_label();
                load_toint32(ops, rg(a), bail);
                dynasm!(ops ; mov r8d, eax);
                load_toint32(ops, rg(b), bail);
                dynasm!(ops ; mov ecx, eax ; mov eax, r8d);
                match op {
                    B::And => { dynasm!(ops ; and eax, ecx); box_eax(ops, rg(d)); }
                    B::Or => { dynasm!(ops ; or eax, ecx); box_eax(ops, rg(d)); }
                    B::Xor => { dynasm!(ops ; xor eax, ecx); box_eax(ops, rg(d)); }
                    B::Shl => { dynasm!(ops ; shl eax, cl); box_eax(ops, rg(d)); }
                    B::Shr => { dynasm!(ops ; sar eax, cl); box_eax(ops, rg(d)); }
                    B::Ushr => {
                        let as_dbl = ops.new_dynamic_label();
                        let done_u = ops.new_dynamic_label();
                        dynasm!(ops
                            ; shr eax, cl
                            ; test eax, eax
                            ; js => as_dbl
                        );
                        box_eax(ops, rg(d));
                        dynasm!(ops
                            ; jmp => done_u
                            ; => as_dbl
                            ; mov eax, eax
                            ; cvtsi2sd xmm0, rax
                            ; movq rax, xmm0
                            ; mov [rbx + dreg(rg(d))], rax
                            ; => done_u
                        );
                    }
                }
                emit_region_bail(ops, call_ip, bail, epilogue);
            }
            // `super.m()` — inline the resolved super body over its sub-window,
            // behind the baked super-chain hop version guards (a chain mutation
            // misses → helper). The super body runs over the SAME receiver.
            Instr::SuperMethod { dst: d, .. } => {
                let s = supers
                    .get(&bi)
                    .expect("build_method_inline_plan baked a SuperInline for this op");
                // ── class-redefinition guard ── a re-executed class declaration
                // swaps class_values[home_class_id] to a new class (+ new proto
                // chain) without touching the old prototypes the hop guards watch;
                // the interpreter re-resolves super via the live class_values, so
                // catch the swap via the VM epoch (→ helper). One load + compare.
                dynasm!(ops
                    ; mov rcx, QWORD s.epoch_ptr as i64
                    ; mov ecx, [rcx]
                    ; cmp ecx, DWORD s.epoch_val as i32
                    ; jne => fallback
                );
                // ── super-chain hop version guards (catch setPrototypeOf / a
                // chain realloc; the holder hop catches a key-add realloc of the
                // holder before its vals_ptr is dereferenced below) ──
                for &(idx, ver) in &s.hops {
                    dynasm!(ops
                        ; mov edx, [r13 + (idx as i32) * 4]
                        ; cmp edx, DWORD ver as i32
                        ; jne => fallback
                    );
                }
                // ── super-method REASSIGNMENT guard ── re-read the holder slot
                // and confirm it still holds the baked super function (matches the
                // interpreter, which re-reads the holder each call). Safe to deref:
                // the holder hop version guard above proved no realloc.
                dynasm!(ops
                    ; mov rcx, QWORD s.holder_vals_ptr as i64
                    ; mov rax, [rcx + (s.holder_slot as i32) * 8]
                    ; mov r10, QWORD s.fn_bits as i64
                    ; cmp rax, r10
                    ; jne => fallback
                );
                // Super reg 0 = the SAME receiver (this body's reg 0 = `win`).
                dynasm!(ops
                    ; mov rax, [rbx + dreg(win)]
                    ; mov [rbx + dreg(s.win_off)], rax
                );
                // Zero-fill the super body's locals (0-arg → regs 1.. are locals).
                if s.callee_reg_count > 1 {
                    dynasm!(ops ; mov rax, QWORD Value::UNDEFINED.bits() as i64);
                    for r in 1..s.callee_reg_count {
                        dynasm!(ops ; mov [rbx + dreg(s.win_off + r)], rax);
                    }
                }
                // Emit the super body (v1: no nested super → empty supers map).
                let no_supers: FxHashMap<usize, SuperInline> = FxHashMap::default();
                let sret = emit_mi_body(
                    ops, call_ip, epilogue, fallback, &s.body, &no_supers, &s.field_slots,
                    &s.consts, vals_ptr, s.win_off,
                );
                match sret {
                    Some(r) => dynasm!(ops
                        ; mov rax, [rbx + dreg(r)]
                        ; mov [rbx + dreg(rg(d))], rax
                    ),
                    None => dynasm!(ops
                        ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                        ; mov [rbx + dreg(rg(d))], rax
                    ),
                }
            }
            // `this.<field> = val` (a trivial setter's store) — a baked in-place
            // store to the receiver's own data slot (no version bump, matching
            // accessor_fast_set). The body's ONLY effect; method_inline_body_ok
            // admits it ONLY as the last op before the terminator, so any earlier
            // op's number-guard bail re-runs the whole call cleanly.
            Instr::SetProp { obj: 0, name, val } => {
                let slot = field_slots.get(&name).copied().unwrap_or(0);
                dynasm!(ops
                    ; mov rax, [rbx + dreg(rg(val))]
                    ; mov rcx, QWORD vals_ptr as i64
                    ; mov [rcx + (slot as i32) * 8], rax
                );
            }
            Instr::Return { src } => {
                ret_reg = Some(rg(src));
            }
            Instr::ReturnUndefined => {
                ret_reg = None;
            }
            // `build_method_inline_plan` / `method_inline_body_ok` admit only the
            // ops above.
            ref other => unreachable!("inline method body op not admitted: {other:?}"),
        }
    }
    ret_reg
}

/// Q7 method-call inlining: emit a guarded INLINE expansion of a trivial class
/// method for a region `CallMethod` op, with a fallback to the unchanged per-call
/// helper. Emitted INSTEAD of `emit_region_call_ic` when a `MethodInlinePlan`
/// exists for this ip. Mirrors `emit_inline_leaf_call`, but the guard is on the
/// RECEIVER (`obj` reg) not a callee function, reg 0 is bound to the receiver
/// (`this`), `this.<field>` reads are baked `vals_ptr[slot]` loads, and a
/// `super.m()` is inlined recursively (Stage 3). See `MethodInlinePlan`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_inline_method_call(
    ops: &mut dynasmrt::x64::Assembler,
    call_ip: usize,
    epilogue: dynasmrt::DynamicLabel,
    method_flag_off: i32,
    plan: &MethodInlinePlan,
    obj: u16,
    arg_base: u16,
    argc: u16,
    dst: u16,
    // Fallback emission (the unchanged per-call helper).
    helper: usize,
    packed_fip: u64,
    packed_args: u64,
    refetch: Option<(usize, usize)>,
    ta_refetch: Option<(usize, &TaPinPlan)>,
) {
    let fallback = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    let w = plan.reg_window;
    // Load the receiver ONCE into rax. On a per-shape guard MISS we `jne` before
    // running any body, so rax still holds the receiver for the next arm; only a
    // HIT clobbers rax (then jumps to `done`). The headroom flag is checked once.
    dynasm!(ops
        ; mov rax, [rbx + dreg(obj)]
        // ── headroom flag ── 0 ⇒ a scratch window might overflow the pinned
        // register file → take the helper (covers every arm's window).
        ; cmp QWORD [rsp + method_flag_off], 0
        ; je => fallback
    );
    // ── per-receiver guard tree (≤ JIT_IC_WAYS arms) ── each arm guards a
    // specific instance's identity+version (the ABA / own-shadow / freeze /
    // setPrototypeOf / vals-realloc discriminator); a miss tries the next arm,
    // all-miss falls to the helper, which re-resolves correctly.
    let arm_labels: Vec<_> = (0..plan.shapes.len()).map(|_| ops.new_dynamic_label()).collect();
    for (si, shape) in plan.shapes.iter().enumerate() {
        let miss = if si + 1 < plan.shapes.len() { arm_labels[si + 1] } else { fallback };
        dynasm!(ops
            ; => arm_labels[si]
            ; mov r10, QWORD shape.recv_bits as i64
            ; cmp rax, r10
            ; jne => miss
            ; mov ecx, eax                          // recv heap idx (low 32 of bits)
            ; mov edx, [r13 + rcx*4]                // live slot version
            ; cmp edx, DWORD shape.recv_ver as i32
            ; jne => miss
            // ── bind reg 0 = `this` (rax still = receiver) ──
            ; mov [rbx + dreg(w)], rax
        );
        // ── positional args into W+1.. (simple positional params) ──
        let n = argc.min(shape.param_count);
        for i in 0..shape.param_count {
            if i < n {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(arg_base + i)]
                    ; mov [rbx + dreg(w + 1 + i)], rax
                );
            } else {
                dynasm!(ops
                    ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                    ; mov [rbx + dreg(w + 1 + i)], rax
                );
            }
        }
        // ── zero-fill the callee LOCALS (regs past `this`+params) ──
        {
            let undef = Value::UNDEFINED.bits() as i64;
            let first_local = 1 + shape.param_count;
            if first_local < shape.callee_reg_count {
                dynasm!(ops ; mov rax, QWORD undef);
                for r in first_local..shape.callee_reg_count {
                    dynasm!(ops ; mov [rbx + dreg(w + r)], rax);
                }
            }
        }
        // ── inline this arm's body (incl. any `super.m()` sub-bodies) ──
        let ret_reg = emit_mi_body(
            ops,
            call_ip,
            epilogue,
            fallback,
            &shape.body,
            &shape.supers,
            &shape.field_slots,
            &shape.consts,
            shape.vals_ptr,
            w,
        );
        match ret_reg {
            Some(r) => dynasm!(ops
                ; mov rax, [rbx + dreg(r)]
                ; mov [rbx + dreg(dst)], rax
            ),
            None => dynasm!(ops
                ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                ; mov [rbx + dreg(dst)], rax
            ),
        }
        dynasm!(ops ; jmp => done);
    }
    // ── fallback ── the UNCHANGED per-call helper (a pure prefix).
    dynasm!(ops ; => fallback);
    let helper_bail = ops.new_dynamic_label();
    emit_region_call_ic(
        ops, call_ip, helper_bail, epilogue, helper, packed_fip, packed_args, argc, dst, refetch,
        ta_refetch,
    );
    dynasm!(ops ; => done);
}

/// Q7 Stage 5: inline a trivial class GETTER (`o.v`) or SETTER (`o.v = x`) for a
/// region GetProp/SetProp op, as a per-receiver guard tree (like the method
/// emitter). Emitted as a PREFIX before the site's existing inline-cache probe:
/// on an arm HIT it writes the result (getter → `payload`=dst) or performs the
/// store (setter, `payload`=value reg) and jumps to `cont` (the site's IC
/// continuation); on ALL-MISS (or tight headroom) it falls through to the
/// existing IC probe (the unchanged fallback — a real accessor → PROP_VIA_IC
/// helper). reg 0 = receiver; for a setter reg 1 = the value. Body via emit_mi_body.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_inline_accessor(
    ops: &mut dynasmrt::x64::Assembler,
    call_ip: usize,
    epilogue: dynasmrt::DynamicLabel,
    method_flag_off: i32,
    plan: &MethodInlinePlan,
    obj: u16,
    payload: u16,
    is_setter: bool,
    cont: dynasmrt::DynamicLabel,
) {
    let after = ops.new_dynamic_label();
    let w = plan.reg_window;
    dynasm!(ops
        ; mov rax, [rbx + dreg(obj)]
        ; cmp QWORD [rsp + method_flag_off], 0
        ; je => after
    );
    let arm_labels: Vec<_> = (0..plan.shapes.len()).map(|_| ops.new_dynamic_label()).collect();
    for (si, shape) in plan.shapes.iter().enumerate() {
        let miss = if si + 1 < plan.shapes.len() { arm_labels[si + 1] } else { after };
        dynasm!(ops
            ; => arm_labels[si]
            ; mov r10, QWORD shape.recv_bits as i64
            ; cmp rax, r10
            ; jne => miss
            ; mov ecx, eax
            ; mov edx, [r13 + rcx*4]
            ; cmp edx, DWORD shape.recv_ver as i32
            ; jne => miss
            ; mov [rbx + dreg(w)], rax          // reg 0 = this (receiver)
        );
        if is_setter {
            dynasm!(ops
                ; mov rax, [rbx + dreg(payload)]
                ; mov [rbx + dreg(w + 1)], rax  // reg 1 = the value
            );
        }
        let first_local = if is_setter { 2 } else { 1 };
        if first_local < shape.callee_reg_count {
            dynasm!(ops ; mov rax, QWORD Value::UNDEFINED.bits() as i64);
            for r in first_local..shape.callee_reg_count {
                dynasm!(ops ; mov [rbx + dreg(w + r)], rax);
            }
        }
        let ret = emit_mi_body(
            ops, call_ip, epilogue, after, &shape.body, &shape.supers, &shape.field_slots,
            &shape.consts, shape.vals_ptr, w,
        );
        if !is_setter {
            match ret {
                Some(r) => dynasm!(ops
                    ; mov rax, [rbx + dreg(r)]
                    ; mov [rbx + dreg(payload)], rax
                ),
                None => dynasm!(ops
                    ; mov rax, QWORD Value::UNDEFINED.bits() as i64
                    ; mov [rbx + dreg(payload)], rax
                ),
            }
        }
        dynasm!(ops ; jmp => cont);
    }
    dynasm!(ops ; => after);
}

