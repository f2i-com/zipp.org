// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// `ZIPP_NO_SPAN_CODEUNIT_PRED=1` restores the generic boxed leaf expansion.
/// Read once while plans are built; never consulted by emitted hot code.
#[inline]
pub(crate) fn span_code_unit_pred_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_SPAN_CODEUNIT_PRED").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_SPAN_CODEUNIT_PAIR=1` keeps the single-predicate fusion but drops
/// the caller-level `pred(i, a) || pred(i, b)` collapse.  Like the parent gate,
/// this is latched once at plan time and is absent from emitted hot code.
#[inline]
pub(crate) fn span_code_unit_pair_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_SPAN_CODEUNIT_PAIR").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

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

/// The split-member-call intrinsic lane for a `CallWithThis` at `ip`, if its
/// captured callee was (syntactically) loaded by a `GetProp` of `push` /
/// `charCodeAt`: `(boot intrinsic bits, dedicated helper, grows_array)`. The
/// pairing is a HINT that picks the lane; soundness rests on the emitted
/// runtime bits compare alone (`emit_captured_builtin_lane`).
pub(crate) fn captured_builtin_lane(
    proto: &FuncProto,
    ip: usize,
    callee: u16,
    argc: u16,
    heap: &HeapHelpers,
) -> Option<(u64, usize, bool)> {
    if argc != 1 {
        return None;
    }
    let name = proto.code[..ip].iter().rev().find_map(|i| match *i {
        Instr::GetProp { dst, name, .. } if dst == callee => Some(name),
        _ => None,
    })?;
    match proto.string_constants.get(name as usize).map(|s| s.as_str()) {
        Some("push") if heap.push_intrinsic_bits != 0 => {
            Some((heap.push_intrinsic_bits, heap.array_push, true))
        }
        Some("charCodeAt") if heap.char_code_at_intrinsic_bits != 0 => {
            Some((heap.char_code_at_intrinsic_bits, heap.char_code_at, false))
        }
        _ => None,
    }
}

/// A split member call on a builtin receiver — `arr.push(x)` / `s.charCodeAt(i)`
/// lowered in spec order as `GetProp; <args>; CallWithThis` — reaches the
/// generic captured-callee helper with a NATIVE callee: a helper crossing, a
/// `call_value` dispatch and the name-keyed builtin lookup per call, where the
/// fused `CallMethod` form ran the dedicated win64 helper directly. This lane
/// restores that: when the captured callee's bits ARE the boot intrinsic's
/// (the natives live in pinned slots below the GC floor, so bits-identity is
/// the same tamper-proof "still the boot intrinsic" proof
/// `capture_proto_baselines` licenses), call the dedicated helper with the
/// captured receiver — exactly `callee.[[Call]](this_v, arg0)` for that
/// intrinsic. The helper re-proves the receiver kind and the live prototype
/// slot on every call and answers `SELF_CALL_DEOPT` for anything it will not
/// handle BEFORE mutating; here that is a pure-prefix MISS that falls through
/// to the unchanged generic call (never a region deopt). Returns the join
/// label the caller must define after its generic path.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_captured_builtin_lane(
    ops: &mut dynasmrt::x64::Assembler,
    callee: u16,
    this_v: u16,
    arg_base: u16,
    dst: u16,
    intrinsic_bits: u64,
    helper: usize,
    ta_refetch: Option<(usize, &TaPinPlan)>,
) -> dynasmrt::DynamicLabel {
    let done = ops.new_dynamic_label();
    let miss = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(callee)]         // captured callee bits
        ; mov r10, QWORD intrinsic_bits as i64  // the boot intrinsic
        ; cmp rax, r10
        ; jne => miss
        ; mov rcx, rdi                          // vm
        ; mov rdx, [rbx + dreg(this_v)]         // receiver bits
        ; mov r8, [rbx + dreg(arg_base)]        // arg0 bits
        ; mov rax, QWORD helper as i64
        ; call rax
        ; mov r10, QWORD SELF_CALL_DEOPT as i64
        ; cmp rax, r10
        ; je => miss                            // helper declined -> generic call
        ; mov [rbx + dreg(dst)], rax
    );
    // `arr.push(x)` grows the array's own Vec — re-derive any pinned
    // dense-Array snapshot (the caller passes `None` for `charCodeAt`).
    if let Some((snap, plan)) = ta_refetch {
        emit_refetch_ta(ops, snap, plan);
    }
    dynasm!(ops
        ; jmp => done
        ; => miss
    );
    done
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
    math_imul_guard: Option<MathIntrinsicGuard>,
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
    // `Some` only for an exact adjacent predicate pair.  A fused helper result
    // resumes at the pair's shared branch; helper fallback still falls through
    // to the first call's next bytecode.
    span_pair_resume: Option<dynasmrt::DynamicLabel>,
) {
    let fallback = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    let w = plan.reg_window;
    if let Some(fid) = plan.same_proto_fid {
        // The live callee may be any of the rotating function identities seen
        // at this site, but it must still be an ordinary capture-free function
        // for the planned FuncProto. The read-only helper rejects bound/native/
        // wrapped/proxy/non-callable/cross-proto replacements to the real call.
        dynasm!(ops
            ; mov rcx, rdi
            ; mov rdx, [rbx + dreg(callee)]
            ; mov r8d, fid as i32
            ; mov rax, QWORD plan.same_proto_guard as i64
            ; call rax
            ; test rax, rax
            ; jz => fallback
        );
    } else if let Some((gen_addr, gen_val)) = plan.slot_guard {
        // ── W12 slot-generation guard ── the planner proved the callee
        // register holds global slot g's value at this call and every write
        // to g bumps `global_gens[g]` (`slot_guard_key`'s conditions), so ONE
        // 32-bit generation compare witnesses the same (bits, version) tuple
        // the identity+version block below re-checks per execution — rooting
        // plus the non-moving collector freeze a live callee's index AND
        // version while the slot holds it, transferring the version guard's
        // ABA job to the bump audit. A miss means the slot was rebound: fall
        // to the helper (a real, correct call) forever — exactly what a baked
        // bits-guard miss does today. Mirrors the SuperMethod epoch-guard
        // shape. No downstream op reads rax from the replaced block.
        dynasm!(ops
            ; mov r10, QWORD gen_addr as i64
            ; mov r10d, [r10]
            ; cmp r10d, DWORD gen_val as i32
            ; jne => fallback
        );
    } else {
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
        );
    }
    let body_has_direct_global = plan.body.iter().any(|ins| {
        matches!(
            ins,
            Instr::LoadGlobal { .. }
                | Instr::LoadGlobalOrUndefined { .. }
                | Instr::StoreGlobal { .. }
                | Instr::StoreGlobalStrict { .. }
                | Instr::StoreGlobalResolved { .. }
        )
    });
    debug_assert!(
        !body_has_direct_global || plan.direct_global_route_epoch.is_some(),
        "a raw-global leaf reached emission without a route proof"
    );
    // The generic boxed expansion emits raw r12 loads/stores and the fused
    // span helper reads the same slots through Vm. They do not consume the
    // typed schedule's GlobalRouteGuard, so perform the equivalent VM-relative
    // comparison before either path can commit. A miss runs the real call.
    // A typed-only body already emits this check as its first lane step.
    if let Some(epoch) = plan
        .direct_global_route_epoch
        .filter(|_| plan.typed_lane.is_none() || plan.span_code_unit_pred.is_some())
    {
        let epoch_off = crate::vm::host_api::JIT_GLOBAL_ROUTE_EPOCH_OFFSET as i32;
        dynasm!(ops
            ; cmp DWORD [rdi + epoch_off], epoch as i32
            ; jne => fallback
        );
    }
    // A specialized non-simple callee has only its exact default-parameter
    // prologue removed. Each supplied live argument must be non-undefined;
    // explicit undefined (and, conservatively, any future unsupported shape)
    // performs the real call so initializer ordering/throws/re-entry stay exact.
    if plan.default_arg_mask != 0 {
        let undefined = Value::UNDEFINED.bits();
        for arg in 0..64u16 {
            if plan.default_arg_mask & (1u64 << arg) == 0 {
                continue;
            }
            dynasm!(ops
                ; mov rax, [rbx + dreg(arg_base + arg)]
                ; mov r10, QWORD undefined as i64
                ; cmp rax, r10
                ; je => fallback
            );
        }
    }
    dynasm!(ops
        // ── headroom flag ── 0 ⇒ the scratch window might overflow the pinned
        // register file (near-MAX_FRAMES recursion) → take the helper.
        ; cmp QWORD [rsp + leaf_flag_off], 0
        ; je => fallback
    );
    // A common pure scanner predicate has four helper crossings in its generic
    // expansion: three dense-array reads and one `charCodeAt`.  The exact body
    // recognizer records its four live global slots; one read-only helper can
    // evaluate the complete predicate without scratch binding or boxed
    // intermediates.  Any non-pristine receiver/key/value returns the deopt
    // sentinel before observable work, so the unchanged call helper replays the
    // original short-circuit program.
    if let Some(pred) = plan.span_code_unit_pred {
        let pair = pred.pair.zip(span_pair_resume);
        dynasm!(ops
            ; mov rcx, rdi
            ; mov rdx, QWORD pred.packed_globals as i64
            ; mov r8, [rbx + dreg(arg_base)]
        );
        let span_helper = if let Some((pair, _)) = pair {
            dynasm!(ops ; mov r9, QWORD pair.packed_units as i64);
            pair.helper
        } else {
            dynasm!(ops ; mov r9, [rbx + dreg(arg_base + 1)]);
            pred.helper
        };
        dynasm!(ops
            ; mov rax, QWORD span_helper as i64
            ; call rax
            ; mov r10, QWORD SELF_CALL_DEOPT as i64
            ; cmp rax, r10
            ; je => fallback
            ; mov [rbx + dreg(dst)], rax
        );
        if let Some((_, resume)) = pair {
            dynasm!(ops ; jmp => resume);
        } else {
            dynasm!(ops ; jmp => done);
        }
        dynasm!(ops ; => fallback);
        let helper_bail = ops.new_dynamic_label();
        emit_region_call_ic(
            ops,
            call_ip,
            helper_bail,
            epilogue,
            helper,
            packed_fip,
            packed_args,
            argc,
            dst,
            refetch,
            ta_refetch,
        );
        dynasm!(ops ; => done);
        return;
    }
    // ── typed lane ── a fully scheduled register-resident emission replaces
    // the boxed loop below when the planner proved the body numeric; every
    // guard/bail inside it jumps to `fallback` (a pure prefix — nothing is
    // committed before the lane's exit steps). `None` keeps the generic
    // emission below byte-identical.
    if let Some(lane) = &plan.typed_lane {
        emit_typed_lane(ops, lane, plan.cell_get, plan.cell_set, dst, fallback);
        dynasm!(ops
            ; jmp => done
            ; => fallback
        );
        let helper_bail = ops.new_dynamic_label();
        emit_region_call_ic(
            ops,
            call_ip,
            helper_bail,
            epilogue,
            helper,
            packed_fip,
            packed_args,
            argc,
            dst,
            refetch,
            ta_refetch,
        );
        dynasm!(ops ; => done);
        return;
    }
    dynasm!(ops
        // ── arg binding ── reg 0 (callee `this`) = undefined; positional args
        // into W+1.. (a leaf with simple_params binds args positionally). Args
        // beyond `param_count`/`argc` are ignored by a leaf body (no
        // `arguments`); params beyond `argc` stay undefined (the slot is zeroed
        // here so a stale scratch value can't leak in).
        ; mov rax, QWORD plan.this_bits as i64
        ; mov [rbx + dreg(w)], rax
    );
    let n = argc.min(plan.param_count);
    for i in 0..plan.param_count {
        if i < n {
            // W11 (B124): an ALIASED param's reads are remapped straight to
            // the caller's arg slot (see `rg`), so no copy is staged. Sound
            // because the plan proved no body op writes the param, the arg
            // slot is private to the caller frame during the splice, and a
            // mid-body bail re-runs the whole call reading the intact args.
            if (plan.alias_params >> i) & 1 == 1 {
                continue;
            }
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
    // W11 (B124): only the regs the body can actually read-before-write need
    // the store (`plan.uninit_mask` — a may-read-before-write union over all
    // paths, `splice_uninit_mask`); tokIs' 19-store fill per 2.89M executions
    // measured ~25-30ms of parse-large-js. `u64::MAX` = the full pre-W11 fill.
    {
        let undef = Value::UNDEFINED.bits() as i64;
        let first_local = 1 + plan.param_count; // reg index past `this` + params
        let locals_mask: u64 = if first_local < 64 {
            plan.uninit_mask >> first_local
        } else {
            0
        };
        if first_local < plan.callee_reg_count && locals_mask != 0 {
            dynasm!(ops ; mov rax, QWORD undef);
            for r in first_local..plan.callee_reg_count {
                if (plan.uninit_mask >> r) & 1 == 1 {
                    dynasm!(ops ; mov [rbx + dreg(w + r)], rax);
                }
            }
        }
    }
    // ── inline the body over the scratch window ── every register `r` maps to
    // `w + r` — EXCEPT a W11-aliased param, which maps to the caller's own
    // `arg_base + i` slot (read-only by the alias proof). Each op that can
    // bail uses a FRESH bail label whose block resumes the interpreter at the
    // CALL IP (so the whole call re-runs cleanly).
    let alias = plan.alias_params;
    let pc = plan.param_count;
    let rg = move |r: u16| -> u16 {
        if r >= 1 && r <= pc && (alias >> (r - 1)) & 1 == 1 {
            arg_base + (r - 1)
        } else {
            w + r
        }
    };
    // One label per body ip (plus a fall-off sink at `body.len()`), so FORWARD
    // in-body branches (`callee_leaf_ok` admits only `> i && <= term`) re-base to
    // `blabels[target]`. Body index == callee ip (the body is the contiguous
    // truncated prefix `full[..=term]`). Control converges on the single trailing
    // Return; the sink is never reached for a well-formed body (no target == len).
    let blabels: Vec<dynasmrt::DynamicLabel> = (0..=plan.body.len())
        .map(|_| ops.new_dynamic_label())
        .collect();
    let mut ret_reg: Option<u16> = None;
    // Pending captured-variable writes: upvalue index → the scratch register
    // holding the value to commit after the body. See the `UpvalSet` arm.
    let mut upval_buf: FxHashMap<u16, u16> = FxHashMap::default();
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
            // A void inner return in a nested splice materialises as
            // `LoadUndefined { dst }` — which had NO arm here, so a spliced inner
            // ending in `ReturnUndefined` hit the `unreachable!` below at region
            // compile time, under `panic = "abort"`. Latent until B76's
            // args-passing splice widened what reaches this emitter.
            Instr::LoadUndefined { dst: d } => {
                dynasm!(ops
                    ; mov rax, QWORD Value::UNDEFINED.bits() as i64
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
            Instr::StoreGlobal { idx, src }
            | Instr::StoreGlobalStrict { idx, src }
            | Instr::StoreGlobalResolved { idx, src } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(rg(src))]
                    ; mov [r12 + (idx as i32) * 8], rax
                );
            }
            Instr::Add { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(
                    ops,
                    call_ip,
                    bail,
                    epilogue,
                    rg(d),
                    rg(a),
                    rg(b),
                    DOp::Add,
                    true,
                )
            }
            Instr::Sub { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(
                    ops,
                    call_ip,
                    bail,
                    epilogue,
                    rg(d),
                    rg(a),
                    rg(b),
                    DOp::Sub,
                    true,
                )
            }
            Instr::Mul { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(
                    ops,
                    call_ip,
                    bail,
                    epilogue,
                    rg(d),
                    rg(a),
                    rg(b),
                    DOp::Mul,
                    true,
                )
            }
            Instr::Div { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(
                    ops,
                    call_ip,
                    bail,
                    epilogue,
                    rg(d),
                    rg(a),
                    rg(b),
                    DOp::Div,
                    false,
                )
            }
            Instr::Mod { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                let as_dbl = ops.new_dynamic_label();
                let mod_done = ops.new_dynamic_label();
                let rem_nz = ops.new_dynamic_label();
                load_num_xmm(ops, rg(a), 0, bail);
                load_num_xmm(ops, rg(b), 1, bail);
                dynasm!(ops
                    ; cvttsd2si rax, xmm0
                    ; cvttsd2si rcx, xmm1
                    ; test rcx, rcx
                    ; jz => bail
                    // idiv #DE guard: i64::MIN % -1 overflows the quotient and
                    // faults the process. `a % -1` is ±0 and rare — bail and let
                    // the interpreter get the sign right (the B116 hazard).
                    ; cmp rcx, -1
                    ; je => bail
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
                    // Zero remainder from a NEGATIVE dividend (incl. -0.0, which
                    // passes the integer guard: 0.0 == -0.0) is -0 in JS — boxing
                    // Int(0) loses the sign. xmm0 still holds the original
                    // dividend; bail and let the interpreter make the double.
                    ; test rdx, rdx
                    ; jnz => rem_nz
                    ; movq rax, xmm0
                    ; test rax, rax
                    ; js => bail
                    ; => rem_nz
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
                // Sign-bit flip, not `0.0 - x`: `0.0 - 0.0` is `+0.0` under
                // round-to-nearest, so negating zero lost the sign. Same defect
                // as the region/regalloc emitters carried.
                dynasm!(ops
                    ; mov rax, QWORD (1u64 << 63) as i64
                    ; movq xmm0, rax
                    ; xorpd xmm0, xmm1
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
                    B::And => {
                        dynasm!(ops ; and eax, ecx);
                        box_eax(ops, rg(d));
                    }
                    B::Or => {
                        dynasm!(ops ; or eax, ecx);
                        box_eax(ops, rg(d));
                    }
                    B::Xor => {
                        dynasm!(ops ; xor eax, ecx);
                        box_eax(ops, rg(d));
                    }
                    B::Shl => {
                        dynasm!(ops ; shl eax, cl);
                        box_eax(ops, rg(d));
                    }
                    B::Shr => {
                        dynasm!(ops ; sar eax, cl);
                        box_eax(ops, rg(d));
                    }
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
            Instr::MathOp {
                dst: d,
                op,
                callee,
                this_v,
                arg_base: ab,
                argc: ac,
            } => {
                let bail = ops.new_dynamic_label();
                // A bare op's `this_v` is a global index, not a register.
                let (gc, gt) = if callee == crate::bytecode::NO_REG {
                    (callee, this_v)
                } else {
                    (rg(callee), rg(this_v))
                };
                emit_math_identity_guard(ops, op, gc, gt, bail, math_imul_guard);
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
            // ── fused compare-and-branch ── same number guard as the `Lt`/`Le`
            // dcmp arms below (a non-numeric operand bails, re-running the whole
            // call), no boolean materialised. Mirrors the region_mem arm.
            Instr::JumpIfNotLt { a, b, target } => {
                let bail = ops.new_dynamic_label();
                let t = blabels[target as usize];
                djump_if_not_cmp(ops, call_ip, bail, epilogue, rg(a), rg(b), Cmp::Lt, t);
            }
            Instr::JumpIfNotLe { a, b, target } => {
                let bail = ops.new_dynamic_label();
                let t = blabels[target as usize];
                djump_if_not_cmp(ops, call_ip, bail, epilogue, rg(a), rg(b), Cmp::Le, t);
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
            // ── o.k ── a NAMED property read, through the site-free
            // `jit_get_prop_leaf` (plan.prop_get). Exactly the `GetIndex` shape
            // above: three register args, deopt sentinel → region bail. `packed`
            // carries the CALLEE's func id, because `name` indexes the callee's own
            // string constants and the caller's id would resolve a different string.
            //
            // Read-only and allocation-free on the hit path (it returns a Value
            // already in the map), so no pinned-pointer refetch is needed — same
            // reasoning as `GetIndex`.
            Instr::GetProp { dst: d, obj, name } => {
                let bail = ops.new_dynamic_label();
                let packed: u64 = ((plan.callee_fid as u64) << 32) | name as u64;
                dynasm!(ops
                    ; mov rcx, rdi                              // vm
                    ; mov rdx, [rbx + dreg(rg(obj))]            // receiver bits
                    ; mov r8, QWORD packed as i64               // (callee_fid<<32)|name
                    ; mov rax, QWORD plan.prop_get as i64
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
            Instr::CallMethod {
                dst: d,
                obj,
                arg_base: ab,
                ..
            } => {
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
            // ── captured-variable read ── the cell was resolved at plan time from
            // the closure the identity guard pins, so it is an immediate here; the
            // VALUE still has to be loaded, since the cell is mutable. Cannot use
            // `jit_upval_get`: that walks to the running closure from the TOP
            // frame, and an inlined body does not have one. A TDZ cell returns the
            // deopt sentinel, so the interpreter re-runs the call and throws.
            Instr::UpvalGet { dst: d, idx } => {
                // Once this body has written the upvalue, the live value is in the
                // buffer register, NOT the cell (the cell is written after the last
                // op). Read whichever is current.
                if let Some(&buf) = upval_buf.get(&idx) {
                    dynasm!(ops
                        ; mov rax, [rbx + dreg(buf)]
                        ; mov [rbx + dreg(rg(d))], rax
                    );
                } else {
                    let bail = ops.new_dynamic_label();
                    let cell_bits = plan.upvals.get(&idx).copied().unwrap_or(0);
                    dynasm!(ops
                        ; mov rcx, rdi                          // vm
                        ; mov rdx, QWORD cell_bits as i64       // baked cell Value
                        ; mov rax, QWORD plan.cell_get as i64
                        ; call rax
                        ; mov r10, QWORD SELF_CALL_DEOPT as i64
                        ; cmp rax, r10
                        ; je => bail
                        ; mov [rbx + dreg(rg(d))], rax
                    );
                    emit_region_bail(ops, call_ip, bail, epilogue);
                }
            }
            // ── captured-variable write ── BUFFERED: emit nothing here, record
            // which scratch register holds the pending value, and commit the cell
            // once after the body. Until then every bail is idempotent, so the
            // deopt-capable arithmetic that follows the write in a PRNG-style body
            // is safe. `callee_leaf_ok` restricted this to branch-free bodies, so
            // the write is unconditional and one commit per index is enough.
            Instr::UpvalSet { idx, src } => {
                upval_buf.insert(idx, rg(src));
            }
            // ── nested (wrapper) inline ── the spliced-in callee's body follows
            // immediately in this same flat body; all this op emits is the
            // identity guard for it. A miss jumps to the OUTER fallback, which
            // re-runs the whole outer call: sound because `callee_leaf_ok_one_call`
            // only admits this before any committed effect, and everything written
            // so far lives in the scratch window.
            Instr::Call { .. } => {
                let g = plan
                    .nested
                    .get(&bi)
                    .expect("callee_leaf_ok_one_call admitted a Call with no nested guard");
                dynasm!(ops
                    // `callee_reg` is the WRAPPER's own register number, so it
                    // must go through the scratch-window mapping like every other
                    // body operand. Reading the caller's slot instead made the
                    // guard miss every time: results stayed correct (the fallback
                    // is a real call) and the inline was silently never used.
                    ; mov rax, [rbx + dreg(rg(g.callee_reg))]
                    ; mov r10, QWORD g.bits as i64
                    ; cmp rax, r10
                    ; jne => fallback
                    ; mov ecx, eax                   // heap index (low 32 of bits)
                    ; mov edx, [r13 + rcx*4]         // live slot version
                    ; cmp edx, DWORD g.ver as i32
                    ; jne => fallback
                );
            }
            // ── comparisons ── region_poly_eq / dcmp both emit their own bail
            // block internally (record CALL ip → epilogue).
            Instr::Eq { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                region_poly_eq(
                    ops,
                    call_ip,
                    bail,
                    epilogue,
                    rg(d),
                    rg(a),
                    rg(b),
                    false,
                    strict_eq,
                );
            }
            Instr::Ne { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                region_poly_eq(
                    ops,
                    call_ip,
                    bail,
                    epilogue,
                    rg(d),
                    rg(a),
                    rg(b),
                    true,
                    strict_eq,
                );
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
    // ── commit buffered captured-variable writes ──
    // Deliberately the LAST thing the inlined body does. Every bail above this
    // point leaves the cell untouched, so the interpreter re-running the whole
    // call from the call ip reproduces the same state — which is what makes it
    // sound to inline a body whose upvalue write precedes deopt-capable ops.
    // Bodies that reach here are branch-free (`callee_leaf_ok`), so each write is
    // unconditional. `jit_cell_set` cannot fail: a cell is one heap slot.
    if !upval_buf.is_empty() {
        let mut pending: Vec<(u16, u16)> = upval_buf.iter().map(|(&i, &r)| (i, r)).collect();
        pending.sort_unstable(); // deterministic emission order
        for (idx, src) in pending {
            let cell_bits = plan.upvals.get(&idx).copied().unwrap_or(0);
            dynasm!(ops
                ; mov rcx, rdi                          // vm
                ; mov rdx, QWORD cell_bits as i64       // baked cell Value
                ; mov r8, [rbx + dreg(src)]             // value bits
                ; mov rax, QWORD plan.cell_set as i64
                ; call rax
            );
        }
    }
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
        ops,
        call_ip,
        helper_bail,
        epilogue,
        helper,
        packed_fip,
        packed_args,
        argc,
        dst,
        refetch,
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
            Instr::GetProp {
                dst: d,
                obj: 0,
                name,
            } => {
                let slot = field_slots.get(&name).copied().unwrap_or(0);
                dynasm!(ops
                    ; mov rcx, QWORD vals_ptr as i64
                    ; mov rax, [rcx + (slot as i32) * 8]
                    ; mov [rbx + dreg(rg(d))], rax
                );
            }
            Instr::Add { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(
                    ops,
                    call_ip,
                    bail,
                    epilogue,
                    rg(d),
                    rg(a),
                    rg(b),
                    DOp::Add,
                    true,
                )
            }
            Instr::Sub { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(
                    ops,
                    call_ip,
                    bail,
                    epilogue,
                    rg(d),
                    rg(a),
                    rg(b),
                    DOp::Sub,
                    true,
                )
            }
            Instr::Mul { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(
                    ops,
                    call_ip,
                    bail,
                    epilogue,
                    rg(d),
                    rg(a),
                    rg(b),
                    DOp::Mul,
                    true,
                )
            }
            Instr::Div { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                dbinop(
                    ops,
                    call_ip,
                    bail,
                    epilogue,
                    rg(d),
                    rg(a),
                    rg(b),
                    DOp::Div,
                    false,
                )
            }
            Instr::Mod { dst: d, a, b } => {
                let bail = ops.new_dynamic_label();
                let as_dbl = ops.new_dynamic_label();
                let mod_done = ops.new_dynamic_label();
                let rem_nz = ops.new_dynamic_label();
                load_num_xmm(ops, rg(a), 0, bail);
                load_num_xmm(ops, rg(b), 1, bail);
                dynasm!(ops
                    ; cvttsd2si rax, xmm0
                    ; cvttsd2si rcx, xmm1
                    ; test rcx, rcx
                    ; jz => bail
                    // idiv #DE guard: i64::MIN % -1 overflows the quotient and
                    // faults the process. `a % -1` is ±0 and rare — bail and let
                    // the interpreter get the sign right (the B116 hazard).
                    ; cmp rcx, -1
                    ; je => bail
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
                    // Zero remainder from a NEGATIVE dividend (incl. -0.0, which
                    // passes the integer guard: 0.0 == -0.0) is -0 in JS — boxing
                    // Int(0) loses the sign. xmm0 still holds the original
                    // dividend; bail and let the interpreter make the double.
                    ; test rdx, rdx
                    ; jnz => rem_nz
                    ; movq rax, xmm0
                    ; test rax, rax
                    ; js => bail
                    ; => rem_nz
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
                // Sign-bit flip, not `0.0 - x`: `0.0 - 0.0` is `+0.0` under
                // round-to-nearest, so negating zero lost the sign. Same defect
                // as the region/regalloc emitters carried.
                dynasm!(ops
                    ; mov rax, QWORD (1u64 << 63) as i64
                    ; movq xmm0, rax
                    ; xorpd xmm0, xmm1
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
                    B::And => {
                        dynasm!(ops ; and eax, ecx);
                        box_eax(ops, rg(d));
                    }
                    B::Or => {
                        dynasm!(ops ; or eax, ecx);
                        box_eax(ops, rg(d));
                    }
                    B::Xor => {
                        dynasm!(ops ; xor eax, ecx);
                        box_eax(ops, rg(d));
                    }
                    B::Shl => {
                        dynasm!(ops ; shl eax, cl);
                        box_eax(ops, rg(d));
                    }
                    B::Shr => {
                        dynasm!(ops ; sar eax, cl);
                        box_eax(ops, rg(d));
                    }
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
            //
            // `super.v` (a getter READ inside a class getter, Stage 6) is the
            // same emission with nothing changed: an accessor slot keeps its
            // getter in `vals[slot]`, so the holder re-check below reads the
            // right word, and invoking the getter IS running its body with
            // `this` = this receiver — exactly what the method case does.
            // Resolution is what differs, and that happens in the planner
            // (`ic_super_getter_baked`). The SETTER direction is the separate
            // arm below.
            Instr::SuperMethod { dst: d, .. } | Instr::SuperGet { dst: d, .. } => {
                let s = supers
                    .get(&bi)
                    .expect("build_method_inline_plan baked a SuperInline for this op");
                // ── class-redefinition guard ── a re-executed class declaration
                // swaps class_values[home_class_id] to a new class (+ new proto
                // chain) without touching the old prototypes the hop guards watch;
                // the interpreter re-resolves super via the live class_values, so
                // catch the swap via the VM epoch (→ helper). One load + compare.
                let epoch_off = crate::vm::host_api::JIT_MI_CLASS_EPOCH_OFFSET as i32;
                dynasm!(ops
                    ; mov ecx, [rdi + epoch_off]
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
                    ops,
                    call_ip,
                    epilogue,
                    fallback,
                    &s.body,
                    &no_supers,
                    &s.field_slots,
                    &s.consts,
                    vals_ptr,
                    s.win_off,
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
            // `super.v = x` inside a class SETTER (Stage 7) — the parent
            // SETTER's body inlined over the sub-window. The guard block is the
            // method/getter one verbatim, with one difference hidden in the
            // baked plan rather than the emission: a setter lives in
            // `attrs[slot].setter`, not `vals[slot]`, so
            // `ic_super_setter_baked` bakes the ABSOLUTE address of that word
            // into `holder_vals_ptr` (holder_slot = 0) and the same
            // `[ptr + slot*8]` re-read below checks the right half of the
            // accessor. Effectful — method_inline_body_ok admits this op only
            // LAST, so every earlier op's bail re-runs the whole call before
            // any store commits; within the sub-body, the parent setter's own
            // store is likewise its last op.
            Instr::SuperSet { val, .. } => {
                let s = supers
                    .get(&bi)
                    .expect("build_accessor_shape baked a SuperInline for this op");
                let epoch_off = crate::vm::host_api::JIT_MI_CLASS_EPOCH_OFFSET as i32;
                dynasm!(ops
                    ; mov ecx, [rdi + epoch_off]
                    ; cmp ecx, DWORD s.epoch_val as i32
                    ; jne => fallback
                );
                for &(idx, ver) in &s.hops {
                    dynasm!(ops
                        ; mov edx, [r13 + (idx as i32) * 4]
                        ; cmp edx, DWORD ver as i32
                        ; jne => fallback
                    );
                }
                // Setter REASSIGNMENT guard: `defineProperty` can swap the
                // setter half in place (no version bump, no realloc), so only
                // this value compare catches it — the version guards above
                // only prove the address is safe to dereference.
                dynasm!(ops
                    ; mov rcx, QWORD s.holder_vals_ptr as i64
                    ; mov rax, [rcx + (s.holder_slot as i32) * 8]
                    ; mov r10, QWORD s.fn_bits as i64
                    ; cmp rax, r10
                    ; jne => fallback
                );
                // Sub-window: reg 0 = the SAME receiver, reg 1 = the value
                // (the parent setter's one formal parameter).
                dynasm!(ops
                    ; mov rax, [rbx + dreg(win)]
                    ; mov [rbx + dreg(s.win_off)], rax
                    ; mov rax, [rbx + dreg(rg(val))]
                    ; mov [rbx + dreg(s.win_off + 1)], rax
                );
                if s.callee_reg_count > 2 {
                    dynasm!(ops ; mov rax, QWORD Value::UNDEFINED.bits() as i64);
                    for r in 2..s.callee_reg_count {
                        dynasm!(ops ; mov [rbx + dreg(s.win_off + r)], rax);
                    }
                }
                // The setter's return value is ignored (an assignment's value
                // is the RHS, which the caller already holds), so the body's
                // ret_reg is discarded.
                let no_supers: FxHashMap<usize, SuperInline> = FxHashMap::default();
                let _ = emit_mi_body(
                    ops,
                    call_ip,
                    epilogue,
                    fallback,
                    &s.body,
                    &no_supers,
                    &s.field_slots,
                    &s.consts,
                    vals_ptr,
                    s.win_off,
                );
            }
            // `this.<field> = val` (a trivial setter's store) — a baked in-place
            // store to the receiver's own data slot (no version bump, matching
            // accessor_fast_set). The body's ONLY effect; method_inline_body_ok
            // admits it ONLY as the last op before the terminator, so any earlier
            // op's number-guard bail re-runs the whole call cleanly.
            Instr::SetProp {
                obj: 0,
                name,
                val,
                strict: _,
            } => {
                let slot = field_slots.get(&name).copied().unwrap_or(0);
                dynasm!(ops
                    ; mov rax, [rbx + dreg(rg(val))]
                    ; mov rcx, QWORD vals_ptr as i64
                    ; mov [rcx + (slot as i32) * 8], rax
                );
            }
            // `GetSuperBase` — DROPPED, no code emitted. It captures the home
            // object's live [[Prototype]] into a temp so MakeSuperPropertyReference
            // happens before the argument list / RHS runs; the Super* arms above
            // resolve through their BAKED plan (class epoch + per-hop version
            // guards + a holder-slot re-read) and never dereference `base`, so the
            // temp has no consumer here. `method_inline_body_ok` admits the op only
            // after `mi_super_base_dst_dead` proves no other body op reads that
            // register, which is what makes dropping it observably identical to the
            // interpreter's pure read.
            Instr::SuperBase { .. } => {}
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
    // A fused captured GetProp prefix supplies `(fallback, success)` labels:
    // miss continues with the original GetProp, while success skips its paired
    // CallWithThis. `None` emits the ordinary helper fallback locally.
    prefix_control: Option<(dynasmrt::DynamicLabel, dynasmrt::DynamicLabel)>,
) {
    let is_prefix = prefix_control.is_some();
    let (fallback, done) = match prefix_control {
        Some(labels) => labels,
        None => (ops.new_dynamic_label(), ops.new_dynamic_label()),
    };
    let w = plan.reg_window;
    // Load the receiver ONCE into rax. On a per-shape guard MISS we `jne` before
    // running any body, so rax still holds the receiver for the next arm; only a
    // HIT clobbers rax (then jumps to `done`). The headroom flag is checked once.
    dynasm!(ops ; mov rax, [rbx + dreg(obj)]);
    // A typed-only plan uses no scratch window. Its Tier-C wrapper sets
    // win_top == reg_window, allowing it to skip both the entry helper and this
    // per-call flag load. Boxed/super plans retain the historical guard.
    if plan.win_top > plan.reg_window {
        dynasm!(ops
            ; cmp QWORD [rsp + method_flag_off], 0
            ; je => fallback
        );
    }
    // ── per-receiver guard tree (≤ JIT_IC_WAYS arms) ── each arm guards a
    // specific instance's identity+version (the ABA / own-shadow / freeze /
    // setPrototypeOf / vals-realloc discriminator); a miss tries the next arm,
    // all-miss falls to the helper, which re-resolves correctly.
    let arm_labels: Vec<_> = (0..plan.shapes.len())
        .map(|_| ops.new_dynamic_label())
        .collect();
    for (si, shape) in plan.shapes.iter().enumerate() {
        let miss = if si + 1 < plan.shapes.len() {
            arm_labels[si + 1]
        } else {
            fallback
        };
        dynasm!(ops
            ; => arm_labels[si]
            ; mov r10, QWORD shape.recv_bits as i64
            ; cmp rax, r10
            ; jne => miss
            ; mov ecx, eax                          // recv heap idx (low 32 of bits)
            ; mov edx, [r13 + rcx*4]                // live slot version
            ; cmp edx, DWORD shape.recv_ver as i32
            ; jne => miss
        );
        // A fused captured-Get prefix has not executed the ordinary class IC
        // probe. Match its live class-version guard before materializing the
        // immutable class method Value baked from that IC.
        if let Some((class, ver)) = shape.class_method {
            dynasm!(ops
                ; mov ecx, [r13 + (class as i32) * 4]
                ; cmp ecx, DWORD ver as i32
                ; jne => miss
            );
        }
        // ── plain-object receiver: the method is an own property, so its SLOT
        // VALUE must still be the callee we baked. An in-place overwrite does
        // not change the shape and so does not bump the version above.
        if let Some((slot, bits)) = shape.method_slot {
            dynasm!(ops
                ; mov rcx, QWORD shape.vals_ptr as i64
                ; mov rcx, [rcx + (slot as i32) * 8]
                ; mov r10, QWORD bits as i64
                ; cmp rcx, r10
                ; jne => miss
            );
        }
        // ── B78: INHERITED method ── the receiver's own version guard above
        // already covers "no own shadow was added" and "the first proto link
        // was not re-pointed" (`ordinary_set_prototype_of` bumps it). What is
        // left is the rest of the chain and the holder's slot value — the same
        // two guards, emitted the same way, that an inlined `super.m()` uses.
        if let Some(pm) = &shape.proto_method {
            for &(idx, ver) in &pm.hops {
                dynasm!(ops
                    ; mov edx, [r13 + (idx as i32) * 4]
                    ; cmp edx, DWORD ver as i32
                    ; jne => miss
                );
            }
            dynasm!(ops
                ; mov rcx, QWORD pm.holder_vals_ptr as i64
                ; mov rcx, [rcx + (pm.holder_slot as i32) * 8]
                ; mov r10, QWORD pm.fn_bits as i64
                ; cmp rcx, r10
                ; jne => miss
            );
        }
        // The split reference-order form carries the exact callable through
        // arguments in a register. Receiver/member guards alone are
        // insufficient: an argument may have replaced the property, while this
        // call must still invoke the earlier Value. At the fused Get prefix the
        // pure structural guards above prove that Value and materialize it;
        // at the later Call site compare it explicitly. Any mismatch falls to
        // the exact CallWithThis helper, never to a name-based method lookup.
        if let Some(callee_reg) = plan.captured_callee {
            let callee_bits = shape
                .captured_callee_bits
                .expect("captured method plan missing exact callee bits");
            if is_prefix {
                dynasm!(ops
                    ; mov r10, QWORD callee_bits as i64
                    ; mov [rbx + dreg(callee_reg)], r10
                );
            } else {
                dynasm!(ops
                    ; mov rcx, [rbx + dreg(callee_reg)]
                    ; mov r10, QWORD callee_bits as i64
                    ; cmp rcx, r10
                    ; jne => miss
                );
            }
        }
        // ── W19 (MI-LANE) ── a scheduled register-resident emission replaces
        // the boxed body below. It needs NO scratch window at all: no `this`
        // bind (the receiver is baked into every field load), no arg copy
        // (params read the caller's arg slots directly), and no zero-fill
        // (there are no callee locals in memory to stale-read). Every guard
        // inside it jumps to `fallback` — the unchanged per-call helper — and
        // the v1 gate admits only effect-free bodies, so the lane is a pure
        // prefix: nothing is committed before it writes `dst` last.
        if let Some(lane) = &shape.typed_lane {
            emit_typed_lane(ops, lane, 0, 0, dst, fallback);
            dynasm!(ops ; jmp => done);
            continue;
        }
        dynasm!(ops
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
    if !is_prefix {
        // ── fallback ── the UNCHANGED per-call helper (a pure prefix).
        dynasm!(ops ; => fallback);
        let helper_bail = ops.new_dynamic_label();
        emit_region_call_ic(
            ops,
            call_ip,
            helper_bail,
            epilogue,
            helper,
            packed_fip,
            packed_args,
            argc,
            dst,
            refetch,
            ta_refetch,
        );
        dynasm!(ops ; => done);
    }
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
    let arm_labels: Vec<_> = (0..plan.shapes.len())
        .map(|_| ops.new_dynamic_label())
        .collect();
    for (si, shape) in plan.shapes.iter().enumerate() {
        let miss = if si + 1 < plan.shapes.len() {
            arm_labels[si + 1]
        } else {
            after
        };
        dynasm!(ops
            ; => arm_labels[si]
            ; mov r10, QWORD shape.recv_bits as i64
            ; cmp rax, r10
            ; jne => miss
            ; mov ecx, eax
            ; mov edx, [r13 + rcx*4]
            ; cmp edx, DWORD shape.recv_ver as i32
            ; jne => miss
        );
        // ── W20 (M2): OWN-ACCESSOR arm ── the accessor function lives in the
        // receiver's own slot (`vals[slot]` for a getter, `attrs[slot].setter`
        // for a setter), so the baked callee must still BE there. The version
        // guard above covers a `defineProperty` redefinition (props/define.rs
        // bumps it on every define) and a delete/realloc; this covers an
        // in-place replacement that does not, and — because a data/accessor
        // flip goes through `defineProperty` — it is a second, independent
        // check on the flip. The version guard is emitted FIRST, so the baked
        // address is only dereferenced after the vectors are proven un-realloc'd.
        if let Some((addr, bits)) = shape.own_acc {
            dynasm!(ops
                ; mov rcx, QWORD addr as i64
                ; mov rcx, [rcx]
                ; mov r10, QWORD bits as i64
                ; cmp rcx, r10
                ; jne => miss
            );
        }
        // W19 (MI-LANE): a GETTER arm whose body scheduled a lane writes the
        // result straight into `payload` and joins the site's IC continuation
        // — no window binding at all. `after` (fall through to the unchanged
        // IC probe) is the fallback, so a guard miss degrades exactly like an
        // all-arm miss does today. `build_accessor_shape` never schedules a
        // lane for a SETTER (its body ends in the store the v1 gate excludes).
        if let Some(lane) = &shape.typed_lane {
            debug_assert!(!is_setter, "a setter arm must not carry a v1 lane");
            emit_typed_lane(ops, lane, 0, 0, payload, after);
            dynasm!(ops ; jmp => cont);
            continue;
        }
        dynasm!(ops ; mov [rbx + dreg(w)], rax); // reg 0 = this (receiver)
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
            ops,
            call_ip,
            epilogue,
            after,
            &shape.body,
            &shape.supers,
            &shape.field_slots,
            &shape.consts,
            shape.vals_ptr,
            w,
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

// ───────────────────────── typed splice lane ─────────────────────────
//
// A leaf-splice body whose every value op is provably numeric is emitted as a
// REGISTER-RESIDENT lane instead of the boxed per-op loop above: params and
// captured cells are tag-guarded once at entry and unboxed into GPRs, the
// straight-line body runs on exact i64 integers (magnitude-bounded ≤ 2^53 so
// i64 arithmetic equals f64 semantics exactly) and scalar doubles in xmm
// homes, and ONE box happens at exit (plus the buffered upval commit). Every
// runtime check — entry tag guard, the nested callee guard, a ToInt32 of an
// out-of-i32 double — jumps to the per-call helper `fallback`, which re-runs
// the whole call: sound because the lane is a PURE PREFIX until its exit
// (no scratch-slot state is architectural, upval writes are buffered in
// registers, `dst` is written last), exactly the contract the buffered
// `UpvalSet` arm of the generic emitter already relies on.
//
// The schedule (types, magnitude bounds, physical registers, in-place
// choices) is computed ONCE at plan time by `build_typed_lane` — fail-closed:
// any op outside the closed numeric set, an unbounded add/sub chain, an
// undefined/`this` read, or a blown register budget returns `Err` and the
// generic loop is emitted byte-identically to today.
//
// Register budget: values live in GPR homes r8/r9/r10/r11/rdx and xmm homes
// xmm2..xmm5; rax/rcx and xmm0/xmm1 stay scratch for the op templates (rcx
// also serves variable shift counts). rbx/rsi/rdi/r12/r13/r14 remain pinned
// as in the surrounding region body. The two helper calls (cell_get at entry,
// cell_set at exit) clobber every volatile register, so they are scheduled
// strictly before any home is live / after every home is dead.

/// GPR value homes (dynasm register codes): r8, r9, r10, r11, rdx.
const LANE_GPR_HOMES: [u8; 5] = [8, 9, 10, 11, 2];
/// Every lane step's operand template scratches rax and rcx and nothing else,
/// which is only sound while neither is a VALUE HOME. W19 made that load-bearing
/// in a new way: `LaneStep::SuperGuard` re-emits a block that `emit_mi_body`
/// writes with rdx and r10 — both homes — so copying that emission verbatim
/// silently destroys any intermediate live across the guard (measured: a wrong
/// accumulator, not a crash). Assert the disjointness where the table is
/// defined, so re-ordering the homes fails the build rather than the answer.
const _: () = {
    let mut i = 0;
    while i < LANE_GPR_HOMES.len() {
        assert!(
            LANE_GPR_HOMES[i] != 0 && LANE_GPR_HOMES[i] != 1,
            "rax/rcx are lane operand scratch and must never be value homes"
        );
        i += 1;
    }
};
/// XMM value homes: xmm2..xmm5 (xmm0/xmm1 are operand-conversion scratch).
const LANE_XMM_HOMES: [u8; 4] = [2, 3, 4, 5];
/// Defensive cap on scheduled body length (splice bodies are ≤ ~64 ops).
const LANE_MAX_BODY: usize = 96;

env_off_switch! {
    /// `ZIPP_NO_TYPED_GLOBAL_LOAD=1` restores the typed splice planner's old
    /// rule that a `LoadGlobal` may only feed a nested-callee identity guard.
    /// The default path may also consume it numerically through a live Int-tagged
    /// load, behind the VM's global-route epoch guard; any other representation
    /// or a later observable routing change falls back to the real call.
    fn typed_global_load_enabled() = "ZIPP_NO_TYPED_GLOBAL_LOAD"
}

/// A 32-bit operand of a lane ALU op: a GPR home's low 32 bits (which ARE the
/// ToInt32 of the exact i64 value it holds — |v| ≤ 2^53 keeps truncation
/// trivial) or a compile-time i32 immediate.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum LaneOp32 {
    R(u8),
    I(i32),
}

/// One scheduled lane instruction. Emission is a dumb match — every decision
/// (types, homes, folds, in-place forms) was made by `build_typed_lane`.
pub(crate) enum LaneStep {
    /// `jit_cell_get(cell)`; deopt sentinel → fallback; optionally park the
    /// boxed bits in a scratch-window slot (multi-upval entry).
    UpvalCall {
        cell_bits: u64,
        park_slot: Option<u16>,
    },
    /// Tag-guard the fetched cell bits as Int and sign-extend into a home.
    UpvalBind {
        park_slot: Option<u16>,
        d: u8,
    },
    /// Load a caller arg slot, tag-guard Int, sign-extend into a home.
    ParamLoad {
        slot: u16,
        d: u8,
    },
    /// Guard the VM-wide direct-global routing assumption before any raw
    /// `[r12 + slot*8]` read in the lane. Ordinary leaf plans only pass this
    /// guard while the epoch is still zero; `delete` / `defineProperty` makes
    /// it non-zero and permanently sends the compiled prefix to the real call.
    GlobalRouteGuard {
        epoch_val: u32,
    },
    /// A real method call at the native-call nesting cap must return to the
    /// interpreter's flat-frame path. The transactional inline therefore
    /// shares the same cap as every Tier-C cross-call prefix.
    CallDepthGuard,
    /// Load a directly-routable live global slot, tag-guard it as Int and
    /// sign-extend it into a home. Used only by ordinary leaf plans, whose VM
    /// planner has already proved every referenced global is slot-backed.
    GlobalLoadInt {
        slot: u32,
        d: u8,
    },
    /// Transactional method-lane exit commits. The scheduler appends these
    /// only after every tag/range/identity guard and after the return value has
    /// been materialized in the caller window. They contain no branch, helper
    /// call or allocation, so once the first one runs there is no replay path.
    GlobalCommitImm {
        slot: u32,
        bits: u64,
    },
    GlobalCommitInt {
        slot: u32,
        s: u8,
        narrow: bool,
    },
    GlobalCommitF64 {
        slot: u32,
        s: u8,
    },
    /// The nested-splice identity guard, fused with its feeding `LoadGlobal`:
    /// re-read `globals[g]` (the value the guarded register would hold) and
    /// compare bits + heap slot version, exactly the generic `Call` arm.
    CalleeGuard {
        gidx: u32,
        bits: u64,
        ver: u32,
    },
    /// Captured `Math.imul` reference.  This replaces the source GetProp only
    /// after validating the live global receiver, receiver layout generation,
    /// exact own data slot value, and callable generation.  Every miss replays
    /// the unchanged outer call before any transactional lane store commits.
    MathImulGuard {
        gidx: u32,
        guard: MathIntrinsicGuard,
    },
    /// `Rq(d) = imm64` (an int immediate too wide for an ALU imm32 field).
    GImm {
        d: u8,
        v: i64,
    },
    /// Exact i64 add/sub (bounds proven ≤ 2^53 at plan time).
    IAdd {
        d: u8,
        a: u8,
        b: LaneOp32,
        sub: bool,
    },
    /// `d = imm - b` (the one non-commutative imm-lhs shape).
    IAddImmRev {
        d: u8,
        imm: i64,
        b: u8,
    },
    /// 32-bit ALU op on ToInt32'd operands; result re-extended per op
    /// (movsxd for signed results, the 32-bit write's zero-extension for
    /// `>>>`). Shift counts are pre-masked (&31) when immediate.
    Bit32 {
        d: u8,
        a: LaneOp32,
        b: LaneOp32,
        op: crate::bytecode::BitwiseOp,
    },
    /// `Math.imul`: low signed 32 bits of the product.
    Imul32 {
        d: u8,
        a: LaneOp32,
        b: LaneOp32,
    },
    /// `movsxd Rq(d), low32(s)` — the `|0` wrap of a wide exact integer.
    SignExt {
        d: u8,
        s: u8,
    },
    /// `mov Rd(d), low32(s)` — zero-extend (the `>>>0` wrap).
    ZeroExt {
        d: u8,
        s: u8,
    },
    /// ToInt32 of an f64 home, IN-RANGE ONLY: out-of-i32 (the modular-wrap
    /// case, NaN, ±Inf) jumps to fallback and the whole call re-runs.
    ToI32F64 {
        d: u8,
        s: u8,
    },
    /// `Rx(d) = f64(Rq(s))` — exact, |v| ≤ 2^53 by the Int invariant.
    CvtIX {
        d: u8,
        s: u8,
    },
    /// `Rx(d) = <raw f64 bits>` via rax.
    XImm {
        d: u8,
        bits: u64,
    },
    /// Scalar f64 op, bytecode-op-for-op (never algebraically fused).
    FBin {
        d: u8,
        a: u8,
        b: u8,
        op: DOp,
    },
    /// Exit: store pre-folded boxed bits into the caller's `dst`.
    RetImm {
        bits: u64,
    },
    /// Exit: box an Int home (`narrow` = proven i32 ⇒ direct Int tag).
    RetInt {
        s: u8,
        narrow: bool,
    },
    /// Exit: store an f64 home's raw bits (the `store_xmm` convention).
    RetF64 {
        s: u8,
    },
    /// Exit upval commit staging: box into a scratch-window slot.
    BoxIntToSlot {
        s: u8,
        slot: u16,
        narrow: bool,
    },
    BoxF64ToSlot {
        s: u8,
        slot: u16,
    },
    ImmToSlot {
        bits: u64,
        slot: u16,
    },
    /// Exit upval commit: `jit_cell_set(cell, [slot])`.
    CellCommit {
        cell_bits: u64,
        slot: u16,
    },
    /// W19 (MI-LANE): `this.<field>` — a baked `[vals_ptr + slot*8]` load,
    /// tag-guarded Int and sign-extended into a GPR home. Shape-identical to
    /// `ParamLoad`, with a baked absolute address in place of a window slot
    /// (the receiver's identity+version guard, already emitted by the arm,
    /// is what makes `vals_ptr`/`slot` valid).
    FieldLoadInt {
        vals_ptr: u64,
        slot: u32,
        d: u8,
    },
    /// The same load guarded as a boxed DOUBLE — high-16 OUTSIDE the tag band
    /// `[INT_TAG_HI, TAG_HI]`, i.e. exactly `load_num_xmm`'s non-Int arm —
    /// into an xmm home. Which of the two is emitted is decided at plan time
    /// from the slot's live representation; the other tagging misses the
    /// guard and re-runs the call through the helper.
    FieldLoadF64 {
        vals_ptr: u64,
        slot: u32,
        d: u8,
    },
    /// W19 (MI-LANE): the `super.m()` / `super.v` guard block — class epoch,
    /// one version compare per super-chain hop (`hop_pool[at..at+len]`), and
    /// the holder-slot re-read. Produces NO value: the flattened super body
    /// follows it and its `Return` was rewritten to a `Move` into the call's
    /// dst. Emitted with rax/rcx only — the boxed emitter's copy of this
    /// block clobbers rdx and r10, which ARE lane value homes.
    SuperGuard {
        epoch_val: u32,
        hops_at: u16,
        hops_len: u16,
        holder_vals_ptr: u64,
        holder_slot: u32,
        fn_bits: u64,
    },
}

/// The scheduled lane carried by a `LeafInlinePlan` or a `MethodInlineShape`.
pub struct TypedLanePlan {
    pub(crate) steps: Vec<LaneStep>,
    /// Side table for `LaneStep::SuperGuard`'s hop version list (keeps every
    /// `LaneStep` a plain scalar record, so the emitter can keep matching by
    /// value).
    pub(crate) hop_pool: Vec<(u32, u32)>,
    /// Census counts for the plan-time JITLOG line.
    pub n_ops: u16,
    pub n_guards: u16,
}

/// Abstract value of one body register during scheduling. `Int` carries the
/// exact-integer magnitude interval (the ≤ 2^53 invariant every Int home
/// obeys); `ImmI`/`ImmF` are compile-time folds that never occupy a home.
#[derive(Clone, Copy)]
enum Av {
    ImmI(i64),
    ImmF(u64),
    Int { h: u8, lo: i64, hi: i64 },
    F64 { h: u8 },
    Callee { g: u32 },
    MathCallee { g: u32 },
}

const I53: i64 = 1i64 << 53;
const IV32: (i64, i64) = (i32::MIN as i64, i32::MAX as i64);

/// Sources / def of a body op, for the straight-line liveness scans. `None`
/// = an op outside the modelled set (treated as using everything — the walk
/// declines on it anyway, this only keeps the scans conservative).
#[allow(clippy::type_complexity)]
fn lane_use_def(ins: &Instr) -> Option<(([u16; 4], u8), Option<u16>)> {
    let u0 = |d| Some((([0u16; 4], 0u8), d));
    let u1 = |a, d| Some((([a, 0, 0, 0], 1u8), d));
    let u2 = |a, b, d| Some((([a, b, 0, 0], 2u8), d));
    let u4 = |a, b, c, d, out| Some((([a, b, c, d], 4u8), out));
    match *ins {
        Instr::LoadInt { dst, .. }
        | Instr::LoadConst { dst, .. }
        | Instr::LoadGlobal { dst, .. }
        | Instr::LoadGlobalOrUndefined { dst, .. }
        | Instr::UpvalGet { dst, .. } => u0(Some(dst)),
        Instr::Move { dst, src } => u1(src, Some(dst)),
        Instr::Add { dst, a, b }
        | Instr::Sub { dst, a, b }
        | Instr::Mul { dst, a, b }
        | Instr::Div { dst, a, b }
        | Instr::Bitwise { dst, a, b, .. } => u2(a, b, Some(dst)),
        Instr::AddInt { dst, a, .. } => u1(a, Some(dst)),
        Instr::MathOp {
            dst,
            callee,
            this_v,
            arg_base,
            argc: 2,
            ..
        } => {
            if callee == crate::bytecode::NO_REG {
                u2(arg_base, arg_base + 1, Some(dst))
            } else {
                u4(callee, this_v, arg_base, arg_base + 1, Some(dst))
            }
        }
        Instr::UpvalSet { src, .. }
        | Instr::StoreGlobal { src, .. }
        | Instr::StoreGlobalStrict { src, .. }
        | Instr::StoreGlobalResolved { src, .. } => u1(src, None),
        // W19 (MI-LANE): `this.<field>` reads NO vreg — the receiver is baked
        // into the step as an absolute `vals` address behind the arm's
        // identity+version guard. `obj != 0` stays unmodelled (`None`).
        Instr::GetProp { dst, obj: 0, .. } => u0(Some(dst)),
        // A captured Math reference reads its namespace receiver.  The body
        // walker below admits only an exact GetProp/MathOp pairing.
        Instr::GetProp { dst, obj, .. } => u1(obj, Some(dst)),
        // W19 (MI-LANE): the flattened super marker. Like the nested-splice
        // `Call` below it is a guard site, not a def: the super body's ops
        // follow and a rewritten `Move` writes the call's dst.
        Instr::SuperMethod { .. } | Instr::SuperGet { .. } => u0(None),
        // The nested-splice guard marker: consumes the callee register,
        // defines nothing (the rewritten inner Return writes the dst later).
        Instr::Call { callee, .. } => u1(callee, None),
        Instr::Return { src } => u1(src, None),
        Instr::ReturnUndefined => u0(None),
        _ => None,
    }
}

/// Is vreg `v` read at or after body index `from` (before being redefined)?
fn lane_used_from(body: &[Instr], from: usize, v: u16) -> bool {
    for ins in &body[from.min(body.len())..] {
        match lane_use_def(ins) {
            None => return true, // unmodelled op: conservative
            Some(((uses, n), def)) => {
                if uses[..n as usize].contains(&v) {
                    return true;
                }
                if def == Some(v) {
                    return false;
                }
            }
        }
    }
    false
}

/// Is upval `idx` read (UpvalGet) at or after `from`, before an UpvalSet of
/// the same index?
fn lane_upval_get_from(body: &[Instr], from: usize, idx: u16) -> bool {
    for ins in &body[from.min(body.len())..] {
        match *ins {
            Instr::UpvalGet { idx: k, .. } if k == idx => return true,
            Instr::UpvalSet { idx: k, .. } if k == idx => return false,
            _ => {}
        }
    }
    false
}

/// Is the CURRENT buffered value of upval `idx` still needed at or after
/// `from`? (Read before the next UpvalSet, or committed at exit if no later
/// UpvalSet supersedes it.)
fn lane_buffer_live_from(body: &[Instr], from: usize, idx: u16) -> bool {
    for ins in &body[from.min(body.len())..] {
        match *ins {
            Instr::UpvalGet { idx: k, .. } if k == idx => return true,
            Instr::UpvalSet { idx: k, .. } if k == idx => return false,
            _ => {}
        }
    }
    true // survives to the exit commit
}

/// Is the CURRENT buffered value of global slot `idx` still needed at or after
/// `from`? A later read observes it; a later write supersedes it; otherwise it
/// remains live until the transactional exit commit.
fn lane_global_buffer_live_from(body: &[Instr], from: usize, idx: u32) -> bool {
    for ins in &body[from.min(body.len())..] {
        match *ins {
            Instr::LoadGlobal { idx: k, .. } | Instr::LoadGlobalOrUndefined { idx: k, .. }
                if k == idx =>
            {
                return true;
            }
            Instr::StoreGlobal { idx: k, .. }
            | Instr::StoreGlobalStrict { idx: k, .. }
            | Instr::StoreGlobalResolved { idx: k, .. }
                if k == idx =>
            {
                return false;
            }
            _ => {}
        }
    }
    true
}

/// ToInt32 of an exact integer ≤ 2^53 in magnitude: the low 32 bits.
fn to_i32_exact(v: i64) -> i32 {
    v as u32 as i32
}

/// Full spec ToInt32 of an f64 (plan-time fold only).
fn to_i32_f64(x: f64) -> i32 {
    if !x.is_finite() {
        return 0;
    }
    let t = x.trunc();
    // fmod is exact; the shifted result is an integer < 2^32, also exact.
    let m = t % 4294967296.0;
    let m = if m < 0.0 { m + 4294967296.0 } else { m };
    (m as u32) as i32
}

/// Box an exact numeric fold with `Value::num` semantics (i32 narrows, NaN
/// canonicalises, -0/±Inf/wide stay doubles).
fn lane_box_imm(av: Av) -> u64 {
    match av {
        Av::ImmI(v) => {
            if v >= i32::MIN as i64 && v <= i32::MAX as i64 {
                Value::int(v as i32).bits()
            } else {
                Value::num(v as f64).bits()
            }
        }
        Av::ImmF(bits) => Value::num(f64::from_bits(bits)).bits(),
        _ => unreachable!("lane_box_imm on a non-immediate"),
    }
}

struct LaneBuilder<'a> {
    body: &'a [Instr],
    param_count: u16,
    argc: u16,
    arg_base: u16,
    slots: Vec<Av>,
    binds: FxHashMap<u16, usize>,
    buffer: FxHashMap<u16, usize>,
    gbuffer: FxHashMap<u32, usize>,
    uentry: FxHashMap<u16, usize>,
    steps: Vec<LaneStep>,
    n_guards: u16,
    /// Ordinary leaf planning pre-validates direct global routing. Method lanes
    /// do not, so they retain the historical callee-guard-only treatment.
    allow_global_values: bool,
    /// Exact main-realm Math.imul slot proof.  `None` makes every captured
    /// Math reference fail closed to the boxed body.
    math_imul_guard: Option<MathIntrinsicGuard>,
    /// Side storage for `LaneStep::SuperGuard` hop lists.
    hop_pool: Vec<(u32, u32)>,
}

impl LaneBuilder<'_> {
    fn push_slot(&mut self, av: Av) -> usize {
        self.slots.push(av);
        self.slots.len() - 1
    }

    /// Is physical home `h` (GPR when `gpr`, else XMM) referenced by any
    /// value still live when scanning from body index `from`?
    fn home_live(&self, from: usize, gpr: bool, h: u8) -> bool {
        let hits = |sid: usize| match self.slots[sid] {
            Av::Int { h: hh, .. } => gpr && hh == h,
            Av::F64 { h: hh } => !gpr && hh == h,
            _ => false,
        };
        for (&v, &sid) in &self.binds {
            if hits(sid) && lane_used_from(self.body, from, v) {
                return true;
            }
        }
        for (&idx, &sid) in &self.uentry {
            if hits(sid) && lane_upval_get_from(self.body, from, idx) {
                return true;
            }
        }
        for (&idx, &sid) in &self.buffer {
            if hits(sid) && lane_buffer_live_from(self.body, from, idx) {
                return true;
            }
        }
        for (&idx, &sid) in &self.gbuffer {
            if hits(sid) && lane_global_buffer_live_from(self.body, from, idx) {
                return true;
            }
        }
        false
    }

    /// Allocate a GPR home, preferring `prefer` (dying source homes → the
    /// in-place forms). `from` selects the liveness horizon: pass the current
    /// op index while resolving that op's OTHER sources (inclusive — they are
    /// still needed), or the next index for the op's own dst (exclusive —
    /// dying sources may be reused). `exclude` shields an UNTRACKED temp
    /// (a just-allocated ToInt32 destination has no binder yet, so liveness
    /// alone would hand its home out again within the same op).
    fn alloc_gpr(
        &self,
        from: usize,
        prefer: &[u8],
        exclude: Option<u8>,
    ) -> Result<u8, &'static str> {
        for &h in prefer.iter().chain(LANE_GPR_HOMES.iter()) {
            if Some(h) != exclude && !self.home_live(from, true, h) {
                return Ok(h);
            }
        }
        Err("gpr-budget")
    }

    fn alloc_xmm(
        &self,
        from: usize,
        prefer: &[u8],
        exclude: Option<u8>,
    ) -> Result<u8, &'static str> {
        for &h in prefer.iter().chain(LANE_XMM_HOMES.iter()) {
            if Some(h) != exclude && !self.home_live(from, false, h) {
                return Ok(h);
            }
        }
        Err("xmm-budget")
    }

    /// Resolve a body vreg to its abstract value, lazily entry-loading a
    /// param on first read (pure prefix: the tag guard jumps to fallback and
    /// nothing has been committed at any body position).
    fn resolve(&mut self, i: usize, v: u16) -> Result<usize, &'static str> {
        if let Some(&sid) = self.binds.get(&v) {
            return match self.slots[sid] {
                Av::Callee { g } if self.allow_global_values => {
                    let h = self.alloc_gpr(i, &[], None)?;
                    self.steps.push(LaneStep::GlobalLoadInt { slot: g, d: h });
                    self.n_guards += 1;
                    let sid = self.push_slot(Av::Int {
                        h,
                        lo: IV32.0,
                        hi: IV32.1,
                    });
                    self.binds.insert(v, sid);
                    Ok(sid)
                }
                Av::Callee { .. } | Av::MathCallee { .. } => Err("callee-value-escapes"),
                _ => Ok(sid),
            };
        }
        if v >= 1 && v <= self.param_count {
            let pi = v - 1;
            if pi >= self.argc {
                return Err("param-not-passed");
            }
            let h = self.alloc_gpr(i, &[], None)?;
            self.steps.push(LaneStep::ParamLoad {
                slot: self.arg_base + pi,
                d: h,
            });
            self.n_guards += 1;
            let sid = self.push_slot(Av::Int {
                h,
                lo: IV32.0,
                hi: IV32.1,
            });
            self.binds.insert(v, sid);
            return Ok(sid);
        }
        Err("read-undefined-reg") // `this`, an uninitialized local, …
    }

    /// Homes of op sources that die at op `i` (nothing reads them at i+1 or
    /// later) — preferred reuse targets for the op's dst.
    fn dying_homes(&self, i: usize, srcs: &[usize], gpr: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for &sid in srcs {
            let h = match self.slots[sid] {
                Av::Int { h, .. } if gpr => h,
                Av::F64 { h } if !gpr => h,
                _ => continue,
            };
            if !self.home_live(i + 1, gpr, h) && !out.contains(&h) {
                out.push(h);
            }
        }
        out
    }

    fn iv_of(&self, sid: usize) -> (i64, i64) {
        match self.slots[sid] {
            Av::ImmI(v) => (v, v),
            Av::Int { lo, hi, .. } => (lo, hi),
            _ => unreachable!("iv_of on a non-int"),
        }
    }

    /// Int-or-float add/sub over two resolved slots.
    fn arith(
        &mut self,
        i: usize,
        dst: u16,
        sa: usize,
        sb: usize,
        sub: bool,
    ) -> Result<(), &'static str> {
        let is_f = |av: Av| matches!(av, Av::F64 { .. } | Av::ImmF(_));
        if is_f(self.slots[sa]) || is_f(self.slots[sb]) {
            return self.fbin(i, dst, sa, sb, if sub { DOp::Sub } else { DOp::Add });
        }
        // Exact-integer path: magnitude bounds must stay ≤ 2^53 or i64
        // arithmetic diverges from f64 rounding — FAIL CLOSED.
        if let (Av::ImmI(va), Av::ImmI(vb)) = (self.slots[sa], self.slots[sb]) {
            let r = if sub { va - vb } else { va + vb };
            if r.abs() > I53 {
                return Err("i53-overflow");
            }
            let sid = self.push_slot(Av::ImmI(r));
            self.binds.insert(dst, sid);
            return Ok(());
        }
        let (alo, ahi) = self.iv_of(sa);
        let (blo, bhi) = self.iv_of(sb);
        let iv = if sub {
            (alo - bhi, ahi - blo)
        } else {
            (alo + blo, ahi + bhi)
        };
        if iv.0 < -I53 || iv.1 > I53 {
            return Err("i53-overflow");
        }
        self.binds.remove(&dst);
        let step = match (self.slots[sa], self.slots[sb]) {
            (Av::Int { h: ha, .. }, Av::Int { h: hb, .. }) => {
                let prefer = self.dying_homes(i, &[sa, sb], true);
                let d = self.alloc_gpr(i + 1, &prefer, None)?;
                LaneStep::IAdd {
                    d,
                    a: ha,
                    b: LaneOp32::R(hb),
                    sub,
                }
            }
            (Av::Int { h: ha, .. }, Av::ImmI(vb)) => {
                let prefer = self.dying_homes(i, &[sa], true);
                if vb >= i32::MIN as i64 && vb <= i32::MAX as i64 {
                    let d = self.alloc_gpr(i + 1, &prefer, None)?;
                    LaneStep::IAdd {
                        d,
                        a: ha,
                        b: LaneOp32::I(vb as i32),
                        sub,
                    }
                } else {
                    // Materialize the wide imm, then reg-reg (the template
                    // reads both sources before writing d, so d may even
                    // reuse the temp's home).
                    let t = self.alloc_gpr(i, &[], None)?;
                    self.steps.push(LaneStep::GImm { d: t, v: vb });
                    let d = self.alloc_gpr(i + 1, &prefer, None)?;
                    LaneStep::IAdd {
                        d,
                        a: ha,
                        b: LaneOp32::R(t),
                        sub,
                    }
                }
            }
            (Av::ImmI(va), Av::Int { h: hb, .. }) => {
                let prefer = self.dying_homes(i, &[sb], true);
                let narrow = va >= i32::MIN as i64 && va <= i32::MAX as i64;
                if !sub && !narrow {
                    // IAddImmRev is `imm - b`, i.e. Sub's form only. Add is
                    // commutative, so materialize the wide imm and go reg-reg.
                    let t = self.alloc_gpr(i, &[], None)?;
                    self.steps.push(LaneStep::GImm { d: t, v: va });
                    let d = self.alloc_gpr(i + 1, &prefer, None)?;
                    LaneStep::IAdd {
                        d,
                        a: hb,
                        b: LaneOp32::R(t),
                        sub: false,
                    }
                } else {
                    let d = self.alloc_gpr(i + 1, &prefer, None)?;
                    if !sub {
                        LaneStep::IAdd {
                            d,
                            a: hb,
                            b: LaneOp32::I(va as i32),
                            sub: false,
                        }
                    } else {
                        LaneStep::IAddImmRev { d, imm: va, b: hb }
                    }
                }
            }
            _ => unreachable!("non-int reached the int arith path"),
        };
        let d = match step {
            LaneStep::IAdd { d, .. } | LaneStep::IAddImmRev { d, .. } => d,
            _ => unreachable!(),
        };
        self.steps.push(step);
        let sid = self.push_slot(Av::Int {
            h: d,
            lo: iv.0,
            hi: iv.1,
        });
        self.binds.insert(dst, sid);
        Ok(())
    }

    /// Scalar f64 op, IEEE bytecode-op-for-op. Int operands convert via
    /// cvtsi2sd (exact ≤ 2^53); immediates fold or materialize into the
    /// xmm0/xmm1 scratch pair.
    fn fbin(
        &mut self,
        i: usize,
        dst: u16,
        sa: usize,
        sb: usize,
        op: DOp,
    ) -> Result<(), &'static str> {
        let comm = matches!(op, DOp::Add | DOp::Mul);
        let as_f = |av: Av| -> Option<f64> {
            match av {
                Av::ImmI(v) => Some(v as f64),
                Av::ImmF(bits) => Some(f64::from_bits(bits)),
                _ => None,
            }
        };
        if let (Some(fa), Some(fb)) = (as_f(self.slots[sa]), as_f(self.slots[sb])) {
            let r = match op {
                DOp::Add => fa + fb,
                DOp::Sub => fa - fb,
                DOp::Mul => fa * fb,
                DOp::Div => fa / fb,
            };
            let sid = self.push_slot(Av::ImmF(r.to_bits()));
            self.binds.insert(dst, sid);
            return Ok(());
        }
        // Operand conversions into the scratch pair (a → xmm0, b → xmm1).
        let conv = |slf: &mut Self, sid: usize, scratch: u8| -> u8 {
            match slf.slots[sid] {
                Av::F64 { h } => h,
                Av::Int { h, .. } => {
                    slf.steps.push(LaneStep::CvtIX { d: scratch, s: h });
                    scratch
                }
                Av::ImmI(v) => {
                    slf.steps.push(LaneStep::XImm {
                        d: scratch,
                        bits: (v as f64).to_bits(),
                    });
                    scratch
                }
                Av::ImmF(bits) => {
                    slf.steps.push(LaneStep::XImm { d: scratch, bits });
                    scratch
                }
                Av::Callee { .. } | Av::MathCallee { .. } => {
                    unreachable!("resolve rejected a callee value")
                }
            }
        };
        let xa = conv(self, sa, 0);
        let xb = conv(self, sb, 1);
        self.binds.remove(&dst);
        let mut prefer = self.dying_homes(i, &[sa], false);
        if comm {
            prefer.extend(self.dying_homes(i, &[sb], false));
        }
        // A non-commutative op must never compute in-place over b.
        let d = self.alloc_xmm(
            i + 1,
            &prefer,
            if !comm && xb >= 2 { Some(xb) } else { None },
        )?;
        self.steps.push(LaneStep::FBin {
            d,
            a: xa,
            b: xb,
            op,
        });
        let sid = self.push_slot(Av::F64 { h: d });
        self.binds.insert(dst, sid);
        Ok(())
    }

    /// A source in ToInt32 position: free for Int homes/imms; an f64 home
    /// takes the in-range-only cvttsd2si (out-of-i32 → fallback). `avoid`
    /// carries the other operand's already-materialized temp so two f64
    /// operands of one op never share a temp home.
    fn to32(&mut self, i: usize, sid: usize, avoid: Option<u8>) -> Result<LaneOp32, &'static str> {
        Ok(match self.slots[sid] {
            Av::ImmI(v) => LaneOp32::I(to_i32_exact(v)),
            Av::ImmF(bits) => LaneOp32::I(to_i32_f64(f64::from_bits(bits))),
            Av::Int { h, .. } => LaneOp32::R(h),
            Av::F64 { h } => {
                let t = self.alloc_gpr(i, &[], avoid)?;
                self.steps.push(LaneStep::ToI32F64 { d: t, s: h });
                LaneOp32::R(t)
            }
            Av::Callee { .. } | Av::MathCallee { .. } => {
                unreachable!("resolve rejected a callee value")
            }
        })
    }

    fn bind_int(&mut self, dst: u16, h: u8, iv: (i64, i64)) {
        let sid = self.push_slot(Av::Int {
            h,
            lo: iv.0,
            hi: iv.1,
        });
        self.binds.insert(dst, sid);
    }
}

// ───────────────────── W19: the METHOD-inline lane ─────────────────────
//
// `MethodInlinePlan` was consumable by exactly one emitter — the boxed one
// (`region_mem.rs` → `emit_mi_body`) — so every intermediate of every inlined
// class-method body went through a memory home with a NaN-box tag test, and a
// four-op `area()` body cost ~100 instructions. This gives the mi path the
// same register-resident lane wave 13 gave leaf splices, over the SAME
// scheduler: the only additions to the closed set are `this.<field>` (a baked
// absolute load, structurally `ParamLoad`) and `super.m()` / `super.v` (the
// existing guard block, then the super body scheduled inline).
//
// SOUNDNESS is the wave-13 contract unchanged. v1 admits only EFFECT-FREE
// bodies (no `SetProp{obj:0}`, no `SuperSet`), so the lane is a pure prefix
// end to end: every guard — the field tag test, the super epoch/hop/holder
// block, a ToInt32 range bail — jumps to `fallback`, the unchanged per-call
// helper, which re-runs the whole call with nothing committed. Magnitude
// bounds stay fail-closed under 2^53 and IEEE ops keep bytecode order.

/// The `super.m()` / `super.v` guard block, lifted out of `SuperInline` (whose
/// other fields — body, field slots, window offset — are consumed by the
/// flattener instead of by the emitter).
#[derive(Clone)]
pub(crate) struct MiSuperGuard {
    pub(crate) epoch_val: u32,
    pub(crate) hops: Vec<(u32, u32)>,
    pub(crate) holder_vals_ptr: u64,
    pub(crate) holder_slot: u32,
    pub(crate) fn_bits: u64,
}

/// The extra plan context a METHOD-inline lane needs beyond the leaf lane's.
pub(crate) struct MiLaneCtx {
    /// The arm's baked receiver `vals` base — shared by the outer body and
    /// every inlined super body, which all run over the SAME receiver.
    vals_ptr: u64,
    /// (namespaced) `GetProp` name index → (own data slot, `true` = the slot
    /// held an Int at plan time, `false` = a boxed double).
    fields: FxHashMap<u32, (u32, bool)>,
    /// Flattened-body index of a `SuperMethod`/`SuperGet` marker → its guards.
    guards: FxHashMap<usize, MiSuperGuard>,
}

/// Namespace shift for a flattened super body's name/const indices. The outer
/// body keeps namespace 0; super body `k` gets `k+1`. Both index spaces are
/// per-function constant pools, so a real index at or above this bound is
/// impossible in practice — it declines rather than aliasing.
const MI_NS_SHIFT: u32 = 20;
const MI_NS_MAX: u32 = 1 << MI_NS_SHIFT;

/// Shift every REGISTER operand of an admitted super-body op by `off`. The
/// admitted set is `method_inline_body_ok(.., allow_super=false, ..)`; anything
/// outside it declines (the lane would decline on it anyway, but a silently
/// unshifted operand would alias the OUTER body's registers, which is a
/// wrong-answer shape rather than a missed optimisation).
fn mi_shift_regs(ins: &Instr, off: u16) -> Result<Instr, &'static str> {
    let s = |r: u16| off + r;
    Ok(match *ins {
        Instr::LoadInt { dst, val } => Instr::LoadInt { dst: s(dst), val },
        Instr::LoadBool { dst, val } => Instr::LoadBool { dst: s(dst), val },
        Instr::Move { dst, src } => Instr::Move {
            dst: s(dst),
            src: s(src),
        },
        Instr::Add { dst, a, b } => Instr::Add {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::Sub { dst, a, b } => Instr::Sub {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::Mul { dst, a, b } => Instr::Mul {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::Div { dst, a, b } => Instr::Div {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::Mod { dst, a, b } => Instr::Mod {
            dst: s(dst),
            a: s(a),
            b: s(b),
        },
        Instr::AddInt { dst, a, imm, upd } => Instr::AddInt {
            dst: s(dst),
            a: s(a),
            imm,
            upd,
        },
        Instr::Neg { dst, a } => Instr::Neg {
            dst: s(dst),
            a: s(a),
        },
        Instr::Bitwise { dst, a, b, op } => Instr::Bitwise {
            dst: s(dst),
            a: s(a),
            b: s(b),
            op,
        },
        _ => return Err("mi-super-op-unshiftable"),
    })
}

/// Flatten a method-inline shape's body and its baked super bodies into ONE
/// straight-line, register-disjoint sequence the lane scheduler can walk.
///
/// * `SuperBase` is dropped — `mi_super_base_dst_dead` already proved its dst
///   has no reader in an inlined body, which is why `emit_mi_body` drops it too.
/// * `SuperMethod`/`SuperGet` stay in place as a GUARD MARKER (exactly the role
///   `Call` plays in a nested leaf splice), followed by the super body with
///   every register shifted above the outer body's and its `Return` rewritten
///   to `Move { dst: <the call's dst>, src: <shifted return reg> }`.
/// * Name and constant indices of super body `k` are shifted into namespace
///   `k+1` so two functions' constant pools cannot alias in the merged maps.
#[allow(clippy::type_complexity)]
fn mi_flatten(
    body: &[Instr],
    supers: &FxHashMap<usize, SuperInline>,
    outer_fields: &FxHashMap<u32, (u32, bool)>,
    outer_consts: &FxHashMap<u32, u64>,
    super_fields: &FxHashMap<usize, FxHashMap<u32, (u32, bool)>>,
    outer_reg_count: u16,
    param_count: u16,
    vals_ptr: u64,
) -> Result<(Vec<Instr>, FxHashMap<u32, u64>, MiLaneCtx), &'static str> {
    let mut flat: Vec<Instr> = Vec::with_capacity(body.len() + 8);
    let mut consts: FxHashMap<u32, u64> = outer_consts.clone();
    let mut fields: FxHashMap<u32, (u32, bool)> = outer_fields.clone();
    let mut guards: FxHashMap<usize, MiSuperGuard> = FxHashMap::default();
    // The super sub-window base in VREG space. It starts at the outer body's
    // register count, which is > param_count, so no shifted super register can
    // ever be mistaken for a formal parameter by `LaneBuilder::resolve`.
    let mut next_off = outer_reg_count;
    if next_off <= param_count {
        return Err("mi-window-overlaps-params");
    }
    for (&name, _) in outer_fields.iter() {
        if name >= MI_NS_MAX {
            return Err("mi-name-space");
        }
    }
    for (&idx, _) in outer_consts.iter() {
        if idx >= MI_NS_MAX {
            return Err("mi-const-space");
        }
    }
    let mut n_super = 0u32;
    for (bi, ins) in body.iter().enumerate() {
        match *ins {
            // Dropped: no inlined consumer (the Super* arms resolve through
            // their baked plan and never dereference `base`).
            Instr::SuperBase { .. } => {}
            Instr::SuperMethod { dst, .. } | Instr::SuperGet { dst, .. } => {
                let s = supers.get(&bi).ok_or("mi-super-unbaked")?;
                let sf = super_fields.get(&bi).ok_or("mi-super-fields-missing")?;
                let ns = n_super + 1;
                n_super += 1;
                if ns >= 16 {
                    return Err("mi-too-many-supers");
                }
                let off = next_off;
                next_off = off
                    .checked_add(s.callee_reg_count)
                    .ok_or("mi-reg-overflow")?;
                if next_off > 512 {
                    return Err("mi-reg-overflow");
                }
                for (&name, &(slot, is_int)) in sf.iter() {
                    if name >= MI_NS_MAX {
                        return Err("mi-name-space");
                    }
                    fields.insert((ns << MI_NS_SHIFT) | name, (slot, is_int));
                }
                for (&idx, &bits) in s.consts.iter() {
                    if idx >= MI_NS_MAX {
                        return Err("mi-const-space");
                    }
                    consts.insert((ns << MI_NS_SHIFT) | idx, bits);
                }
                guards.insert(
                    flat.len(),
                    MiSuperGuard {
                        epoch_val: s.epoch_val,
                        hops: s.hops.clone(),
                        holder_vals_ptr: s.holder_vals_ptr,
                        holder_slot: s.holder_slot,
                        fn_bits: s.fn_bits,
                    },
                );
                flat.push(ins.clone()); // the marker, at the guard's index
                let mut sret: Option<u16> = None;
                for sins in s.body.iter() {
                    match *sins {
                        Instr::Return { src } => {
                            sret = Some(off + src);
                            break;
                        }
                        // A super body that falls off the end yields undefined,
                        // which the lane has no representation for.
                        Instr::ReturnUndefined => return Err("mi-super-undefined"),
                        // `this.<field>` keeps `obj: 0` (the SAME receiver) and
                        // takes the super's namespaced name.
                        Instr::GetProp { dst, obj: 0, name } => flat.push(Instr::GetProp {
                            dst: off + dst,
                            obj: 0,
                            name: (ns << MI_NS_SHIFT) | name,
                        }),
                        Instr::LoadConst { dst, idx } => flat.push(Instr::LoadConst {
                            dst: off + dst,
                            idx: (ns << MI_NS_SHIFT) | idx,
                        }),
                        ref other => flat.push(mi_shift_regs(other, off)?),
                    }
                }
                let src = sret.ok_or("mi-super-no-return")?;
                flat.push(Instr::Move { dst, src });
            }
            ref other => flat.push(other.clone()),
        }
    }
    Ok((
        flat,
        consts,
        MiLaneCtx {
            vals_ptr,
            fields,
            guards,
        },
    ))
}

/// Schedule a typed lane for one METHOD-inline arm. `Err` declines with the
/// JITLOG reason and `emit_inline_method_call` keeps today's boxed emission
/// byte-identically.
///
/// v1 gate, fail-closed (all checked here or by the flattener): the body must
/// be EFFECT-FREE (`SetProp{obj:0}` and `SuperSet` are the only effects
/// `method_inline_body_ok` admits, and both are excluded), every admitted
/// `this.<field>` must hold a number at plan time, and every super body must
/// return a value.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_mi_lane(
    body: &[Instr],
    supers: &FxHashMap<usize, SuperInline>,
    outer_fields: &FxHashMap<u32, (u32, bool)>,
    super_fields: &FxHashMap<usize, FxHashMap<u32, (u32, bool)>>,
    outer_consts: &FxHashMap<u32, u64>,
    vals_ptr: u64,
    callee_reg_count: u16,
    param_count: u16,
    argc: u16,
    arg_base: u16,
    global_route_guard: Option<u32>,
) -> Result<TypedLanePlan, &'static str> {
    // ── v1: EFFECT-FREE bodies only ── an effectful body's mid-lane guard bail
    // would re-run the whole call through the helper and DOUBLE-APPLY a store
    // that already committed. `method_inline_body_ok` places the store last
    // precisely so the boxed path can re-run cleanly; a lane has guards after
    // it too (the exit box has none, but a later op's would), so exclude the
    // shape outright rather than reason about sinking.
    if body
        .iter()
        .any(|i| matches!(i, Instr::SetProp { obj: 0, .. } | Instr::SuperSet { .. }))
    {
        return Err("mi-effectful-body");
    }
    // ── v1: the lane must have something to WIN ── its value is removing the
    // per-op boxing of intermediates and the sub-window bind + zero-fill of an
    // inlined super body. A body with neither — `get v() { return this._v; }`,
    // the bare pass-through — trades one boxed move for a tag guard, a
    // sign-extend and a re-box, and buys a failure mode the boxed path does not
    // have: a field that OSCILLATES between Int and double now misses the baked
    // representation guard on every call, where a move copied the bits either
    // way. Measured on a 24M-iteration pass-through getter: +1.2% [-2.2, +3.1]
    // — no proven regression, but no win either, and the oscillation case has
    // no upside at all. Decline it; `super.v` (one guard block, whose window
    // work the lane still deletes) measured -4.3% and stays.
    if !body.iter().any(|i| {
        matches!(
            i,
            Instr::Add { .. }
                | Instr::Sub { .. }
                | Instr::Mul { .. }
                | Instr::Div { .. }
                | Instr::Mod { .. }
                | Instr::AddInt { .. }
                | Instr::Neg { .. }
                | Instr::Bitwise { .. }
                | Instr::SuperMethod { .. }
                | Instr::SuperGet { .. }
        )
    }) {
        return Err("mi-nothing-to-unbox");
    }
    let (flat, consts, ctx) = mi_flatten(
        body,
        supers,
        outer_fields,
        outer_consts,
        super_fields,
        callee_reg_count,
        param_count,
        vals_ptr,
    )?;
    let no_upvals: FxHashMap<u16, u64> = FxHashMap::default();
    let no_nested: FxHashMap<usize, NestedGuard> = FxHashMap::default();
    build_lane_inner(
        &flat,
        param_count,
        argc,
        arg_base,
        // A method lane needs NO scratch window: no `this` bind, no arg copy,
        // no zero-fill, and (having no upvals) no park/commit slots. Pass the
        // callee's own count so the two overflow checks stay well-defined.
        0,
        callee_reg_count,
        &no_upvals,
        &consts,
        &no_nested,
        Some(&ctx),
        global_route_guard,
        global_route_guard.is_some(),
        None,
    )
}

/// Schedule a typed lane for a (possibly nested-flattened) leaf-splice body.
/// `Err` declines with the JITLOG reason; the generic boxed loop is then
/// emitted byte-identically to today. See the module comment above for the
/// invariants; the closed op set is exactly the match below.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn build_typed_lane(
    body: &[Instr],
    param_count: u16,
    argc: u16,
    arg_base: u16,
    reg_window: u16,
    callee_reg_count: u16,
    upvals: &FxHashMap<u16, u64>,
    consts: &FxHashMap<u32, u64>,
    nested: &FxHashMap<usize, NestedGuard>,
) -> Result<TypedLanePlan, &'static str> {
    build_typed_lane_guarded(
        body,
        param_count,
        argc,
        arg_base,
        reg_window,
        callee_reg_count,
        upvals,
        consts,
        nested,
        None,
        None,
    )
}

/// VM-facing typed-lane builder. `global_route_guard` is the expected value of
/// the VM's global-route epoch; emitted code reads it relative to the live VM
/// argument so a persistent `ScriptState` may move safely. Passing `None` is
/// deliberately fail-closed when the body contains a direct global access;
/// the unit-facing wrapper above therefore cannot accidentally construct an
/// unguarded raw-global lane.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_typed_lane_guarded(
    body: &[Instr],
    param_count: u16,
    argc: u16,
    arg_base: u16,
    reg_window: u16,
    callee_reg_count: u16,
    upvals: &FxHashMap<u16, u64>,
    consts: &FxHashMap<u32, u64>,
    nested: &FxHashMap<usize, NestedGuard>,
    global_route_guard: Option<u32>,
    math_imul_guard: Option<MathIntrinsicGuard>,
) -> Result<TypedLanePlan, &'static str> {
    build_lane_inner(
        body,
        param_count,
        argc,
        arg_base,
        reg_window,
        callee_reg_count,
        upvals,
        consts,
        nested,
        None,
        global_route_guard,
        false,
        math_imul_guard,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_lane_inner(
    body: &[Instr],
    param_count: u16,
    argc: u16,
    arg_base: u16,
    reg_window: u16,
    callee_reg_count: u16,
    upvals: &FxHashMap<u16, u64>,
    consts: &FxHashMap<u32, u64>,
    nested: &FxHashMap<usize, NestedGuard>,
    mi: Option<&MiLaneCtx>,
    global_route_guard: Option<u32>,
    allow_global_stores: bool,
    math_imul_guard: Option<MathIntrinsicGuard>,
) -> Result<TypedLanePlan, &'static str> {
    use crate::bytecode::BitwiseOp as B;
    if body.len() > LANE_MAX_BODY {
        return Err("body-too-long");
    }
    if body.iter().any(|ins| {
        matches!(
            ins,
            Instr::Jump { .. }
                | Instr::JumpIfFalse { .. }
                | Instr::JumpIfTrue { .. }
                | Instr::JumpIfNotLt { .. }
                | Instr::JumpIfNotLe { .. }
        )
    }) {
        return Err("branchy-body");
    }
    let has_direct_global = body.iter().any(|ins| {
        matches!(
            ins,
            Instr::LoadGlobal { .. }
                | Instr::LoadGlobalOrUndefined { .. }
                | Instr::StoreGlobal { .. }
                | Instr::StoreGlobalStrict { .. }
                | Instr::StoreGlobalResolved { .. }
        )
    });
    if has_direct_global && global_route_guard.is_none() {
        return Err("global-route-unguarded");
    }
    let mut steps = Vec::new();
    let mut n_guards = 0;
    if mi.is_some() && allow_global_stores {
        // This lane replaces a real CallMethod activation. At the native-call
        // nesting cap the ordinary helper declines to the flat interpreter;
        // doing the same here preserves its catchable recursion contract.
        steps.push(LaneStep::CallDepthGuard);
        n_guards += 1;
    }
    if has_direct_global {
        let epoch_val = global_route_guard.expect("checked above");
        steps.push(LaneStep::GlobalRouteGuard { epoch_val });
        n_guards += 1;
    }
    let mut b = LaneBuilder {
        body,
        param_count,
        argc,
        arg_base,
        slots: Vec::new(),
        binds: FxHashMap::default(),
        buffer: FxHashMap::default(),
        gbuffer: FxHashMap::default(),
        uentry: FxHashMap::default(),
        steps,
        n_guards,
        allow_global_values: global_route_guard.is_some() && typed_global_load_enabled(),
        math_imul_guard,
        hop_pool: Vec::new(),
    };
    // ── entry: hoisted upval loads ── every upval index whose FIRST body op
    // is a read gets one cell_get + Int guard at entry. Hoisting is sound:
    // the body is straight-line and runs no user code, so nothing can write
    // the cell between entry and the read (a body write is buffered and the
    // buffered value is read instead). The helper calls clobber every
    // volatile register, so they ALL run before any home is live — with more
    // than one upval, earlier results park in scratch-window slots (inside
    // the headroom-validated window).
    let mut entry_idx: Vec<u16> = Vec::new();
    {
        let mut set_seen: FxHashSet<u16> = FxHashSet::default();
        for ins in body {
            match *ins {
                Instr::UpvalGet { idx, .. }
                    if !set_seen.contains(&idx) && !entry_idx.contains(&idx) =>
                {
                    entry_idx.push(idx)
                }
                Instr::UpvalSet { idx, .. } => {
                    set_seen.insert(idx);
                }
                _ => {}
            }
        }
    }
    entry_idx.sort_unstable();
    if entry_idx.len() > 4 {
        return Err("too-many-upvals");
    }
    let parked = entry_idx.len() > 1;
    for (k, &idx) in entry_idx.iter().enumerate() {
        if k as u16 >= callee_reg_count {
            return Err("park-slot-overflow");
        }
        let cell_bits = *upvals.get(&idx).ok_or("upval-cell-missing")?;
        let park = if parked {
            Some(reg_window + k as u16)
        } else {
            None
        };
        b.steps.push(LaneStep::UpvalCall {
            cell_bits,
            park_slot: park,
        });
        b.n_guards += 1;
    }
    for (k, &idx) in entry_idx.iter().enumerate() {
        let park = if parked {
            Some(reg_window + k as u16)
        } else {
            None
        };
        let h = b.alloc_gpr(0, &[], None)?;
        b.steps.push(LaneStep::UpvalBind {
            park_slot: park,
            d: h,
        });
        b.n_guards += 1;
        let sid = b.push_slot(Av::Int {
            h,
            lo: IV32.0,
            hi: IV32.1,
        });
        b.uentry.insert(idx, sid);
    }
    // ── the straight-line body walk ──
    let mut returned = false;
    for (i, ins) in body.iter().enumerate() {
        match *ins {
            Instr::LoadInt { dst, val } => {
                let sid = b.push_slot(Av::ImmI(val as i64));
                b.binds.insert(dst, sid);
            }
            Instr::LoadConst { dst, idx } => {
                let bits = *consts.get(&idx).ok_or("const-missing")?;
                let v = Value::from_bits(bits);
                let sid = if v.is_int() {
                    b.push_slot(Av::ImmI(v.as_int() as i64))
                } else {
                    b.push_slot(Av::ImmF(bits))
                };
                b.binds.insert(dst, sid);
            }
            Instr::Move { dst, src } => {
                let sid = b.resolve(i, src)?;
                b.binds.insert(dst, sid);
            }
            Instr::LoadGlobal { dst, idx } | Instr::LoadGlobalOrUndefined { dst, idx } => {
                let sid = if let Some(&sid) = b.gbuffer.get(&idx) {
                    sid
                } else {
                    b.push_slot(Av::Callee { g: idx })
                };
                b.binds.insert(dst, sid);
            }
            // ── W19 (MI-LANE): `this.<field>` ── a baked absolute load behind
            // the arm's identity+version guard, tag-checked into a home. The
            // representation was chosen at plan time from the slot's live
            // value; the other tagging misses and re-runs the call. Reads no
            // vreg, so nothing here can alias the outer body's registers.
            Instr::GetProp { dst, obj: 0, name } => {
                let ctx = mi.ok_or("op-outside-lane-set")?;
                let &(slot, is_int) = ctx.fields.get(&name).ok_or("mi-field-unbaked")?;
                if is_int {
                    let h = b.alloc_gpr(i, &[], None)?;
                    b.steps.push(LaneStep::FieldLoadInt {
                        vals_ptr: ctx.vals_ptr,
                        slot,
                        d: h,
                    });
                    b.n_guards += 1;
                    // The Int tag's payload IS an i32, so the interval is exact.
                    b.binds.remove(&dst);
                    b.bind_int(dst, h, IV32);
                } else {
                    let h = b.alloc_xmm(i, &[], None)?;
                    b.steps.push(LaneStep::FieldLoadF64 {
                        vals_ptr: ctx.vals_ptr,
                        slot,
                        d: h,
                    });
                    b.n_guards += 1;
                    b.binds.remove(&dst);
                    let sid = b.push_slot(Av::F64 { h });
                    b.binds.insert(dst, sid);
                }
            }
            // Captured `Math.imul`: the compiler emits `LoadGlobal Math;
            // GetProp imul` before evaluating either argument.  A typed lane
            // replaces that lookup only when the exact Math data slot is still
            // pristine.  The guard is emitted here (the source Get position),
            // while the arithmetic remains at the later MathOp position.
            Instr::GetProp { dst, obj, .. } => {
                let paired = body[i + 1..]
                    .iter()
                    .take_while(|next| {
                        crate::codegen::writes_reg(next) != Some(dst)
                            && crate::codegen::writes_reg(next) != Some(obj)
                    })
                    .any(|next| {
                        matches!(
                            next,
                            Instr::MathOp {
                                op: MathFn::Imul,
                                callee,
                                this_v,
                                argc: 2,
                                ..
                            } if *callee == dst && *this_v == obj
                        )
                    });
                if !paired {
                    return Err("getprop-outside-captured-math");
                }
                let gidx = match b.binds.get(&obj).map(|&sid| b.slots[sid]) {
                    Some(Av::Callee { g }) => g,
                    _ => return Err("math-receiver-not-a-global-load"),
                };
                let guard = b.math_imul_guard.ok_or("math-guard-missing")?;
                b.steps.push(LaneStep::MathImulGuard { gidx, guard });
                b.n_guards += 1;
                let sid = b.push_slot(Av::MathCallee { g: gidx });
                b.binds.insert(dst, sid);
            }
            // ── W19 (MI-LANE): the flattened `super.m()` / `super.v` marker ──
            // emit the guard block; the super body's scheduled steps follow and
            // a rewritten `Move` binds the call's dst. No value is produced
            // here and no home is touched, so live outer values survive it.
            Instr::SuperMethod { .. } | Instr::SuperGet { .. } => {
                let ctx = mi.ok_or("op-outside-lane-set")?;
                let g = ctx.guards.get(&i).ok_or("mi-super-guard-missing")?;
                let at = b.hop_pool.len();
                if at + g.hops.len() > u16::MAX as usize {
                    return Err("mi-hop-pool-overflow");
                }
                b.hop_pool.extend_from_slice(&g.hops);
                b.steps.push(LaneStep::SuperGuard {
                    epoch_val: g.epoch_val,
                    hops_at: at as u16,
                    hops_len: g.hops.len() as u16,
                    holder_vals_ptr: g.holder_vals_ptr,
                    holder_slot: g.holder_slot,
                    fn_bits: g.fn_bits,
                });
                b.n_guards += 1;
            }
            Instr::Call { callee, .. } => {
                let g = nested.get(&i).ok_or("call-without-nested-guard")?;
                debug_assert_eq!(g.callee_reg, callee);
                let gidx = match b.binds.get(&callee).map(|&sid| b.slots[sid]) {
                    Some(Av::Callee { g }) => g,
                    _ => return Err("callee-not-a-global-load"),
                };
                b.steps.push(LaneStep::CalleeGuard {
                    gidx,
                    bits: g.bits,
                    ver: g.ver,
                });
                b.n_guards += 1;
                b.binds.remove(&callee);
            }
            Instr::Add { dst, a, b: rb } => {
                let sa = b.resolve(i, a)?;
                let sb = b.resolve(i, rb)?;
                b.arith(i, dst, sa, sb, false)?;
            }
            Instr::Sub { dst, a, b: rb } => {
                let sa = b.resolve(i, a)?;
                let sb = b.resolve(i, rb)?;
                b.arith(i, dst, sa, sb, true)?;
            }
            Instr::AddInt { dst, a, imm, .. } => {
                let sa = b.resolve(i, a)?;
                let sb = b.push_slot(Av::ImmI(imm as i64));
                b.arith(i, dst, sa, sb, false)?;
            }
            // JS `*` and `/` ARE the f64 ops — emitting mulsd/divsd over the
            // (exactly converted) operands is the semantics, -0 and rounding
            // included; the generic splice's dbinop routes Mul/Div the same
            // way. Bytecode order is preserved op-for-op — never fused.
            Instr::Mul { dst, a, b: rb } => {
                let sa = b.resolve(i, a)?;
                let sb = b.resolve(i, rb)?;
                b.fbin(i, dst, sa, sb, DOp::Mul)?;
            }
            Instr::Div { dst, a, b: rb } => {
                let sa = b.resolve(i, a)?;
                let sb = b.resolve(i, rb)?;
                b.fbin(i, dst, sa, sb, DOp::Div)?;
            }
            Instr::Bitwise { dst, a, b: rb, op } => {
                let sa = b.resolve(i, a)?;
                let sb = b.resolve(i, rb)?;
                // ── wrap elisions: `x|0`, `x^0`, `x<<0`, `x>>0`, `x>>>0` ──
                let b_wrap_zero = match (op, b.slots[sb]) {
                    (B::Or | B::Xor, Av::ImmI(v)) => to_i32_exact(v) == 0,
                    (B::Shl | B::Shr | B::Ushr, Av::ImmI(v)) => to_i32_exact(v) & 31 == 0,
                    _ => false,
                };
                if b_wrap_zero {
                    let signed = !matches!(op, B::Ushr);
                    match b.slots[sa] {
                        Av::ImmI(v) => {
                            let t = to_i32_exact(v);
                            let f = if signed { t as i64 } else { t as u32 as i64 };
                            let sid = b.push_slot(Av::ImmI(f));
                            b.binds.insert(dst, sid);
                        }
                        Av::ImmF(bits) => {
                            let t = to_i32_f64(f64::from_bits(bits));
                            let f = if signed { t as i64 } else { t as u32 as i64 };
                            let sid = b.push_slot(Av::ImmI(f));
                            b.binds.insert(dst, sid);
                        }
                        Av::Int { lo, hi, .. }
                            if (signed && lo >= IV32.0 && hi <= IV32.1)
                                || (!signed && lo >= 0 && hi <= u32::MAX as i64) =>
                        {
                            // Already its own wrap: pure alias, zero code.
                            b.binds.insert(dst, sa);
                        }
                        Av::Int { h, .. } => {
                            b.binds.remove(&dst);
                            let prefer = b.dying_homes(i, &[sa], true);
                            let d = b.alloc_gpr(i + 1, &prefer, None)?;
                            b.steps.push(if signed {
                                LaneStep::SignExt { d, s: h }
                            } else {
                                LaneStep::ZeroExt { d, s: h }
                            });
                            b.bind_int(dst, d, if signed { IV32 } else { (0, u32::MAX as i64) });
                        }
                        Av::F64 { h } => {
                            b.binds.remove(&dst);
                            let d = b.alloc_gpr(i + 1, &[], None)?;
                            b.steps.push(LaneStep::ToI32F64 { d, s: h });
                            if signed {
                                b.bind_int(dst, d, IV32);
                            } else {
                                b.steps.push(LaneStep::ZeroExt { d, s: d });
                                b.bind_int(dst, d, (0, u32::MAX as i64));
                            }
                        }
                        Av::Callee { .. } | Av::MathCallee { .. } => {
                            unreachable!("resolve rejected a callee value")
                        }
                    }
                    continue;
                }
                let a32 = b.to32(i, sa, None)?;
                let avoid = if let LaneOp32::R(r) = a32 {
                    Some(r)
                } else {
                    None
                };
                let mut b32 = b.to32(i, sb, avoid)?;
                if let (B::Shl | B::Shr | B::Ushr, LaneOp32::I(k)) = (op, b32) {
                    b32 = LaneOp32::I(k & 31);
                }
                if let (LaneOp32::I(va), LaneOp32::I(vb)) = (a32, b32) {
                    let r: i64 = match op {
                        B::And => (va & vb) as i64,
                        B::Or => (va | vb) as i64,
                        B::Xor => (va ^ vb) as i64,
                        B::Shl => (va << (vb & 31)) as i64,
                        B::Shr => (va >> (vb & 31)) as i64,
                        B::Ushr => (((va as u32) >> (vb & 31)) as u32) as i64,
                    };
                    let sid = b.push_slot(Av::ImmI(r));
                    b.binds.insert(dst, sid);
                    continue;
                }
                b.binds.remove(&dst);
                let prefer = b.dying_homes(i, &[sa, sb], true);
                let d = b.alloc_gpr(i + 1, &prefer, None)?;
                b.steps.push(LaneStep::Bit32 {
                    d,
                    a: a32,
                    b: b32,
                    op,
                });
                let iv = if matches!(op, B::Ushr) {
                    (0, u32::MAX as i64)
                } else {
                    IV32
                };
                b.bind_int(dst, d, iv);
            }
            Instr::MathOp {
                dst,
                op: MathFn::Imul,
                callee,
                this_v,
                arg_base: ab,
                argc: 2,
            } => {
                if callee == crate::bytecode::NO_REG {
                    // BARE form: the guard is scheduled at the op itself (the
                    // lane is straight-line and call-free, so the position is
                    // immaterial); `this_v` is the global index.
                    let guard = b.math_imul_guard.ok_or("math-guard-missing")?;
                    b.steps.push(LaneStep::MathImulGuard {
                        gidx: this_v as u32,
                        guard,
                    });
                    b.n_guards += 1;
                } else {
                    let receiver_g = match b.binds.get(&this_v).map(|&sid| b.slots[sid]) {
                        Some(Av::Callee { g }) => g,
                        _ => return Err("math-receiver-not-captured"),
                    };
                    match b.binds.get(&callee).map(|&sid| b.slots[sid]) {
                        Some(Av::MathCallee { g }) if g == receiver_g => {}
                        _ => return Err("math-callee-not-captured"),
                    }
                }
                let sa = b.resolve(i, ab)?;
                let sb = b.resolve(i, ab + 1)?;
                let a32 = b.to32(i, sa, None)?;
                let avoid = if let LaneOp32::R(r) = a32 {
                    Some(r)
                } else {
                    None
                };
                let b32 = b.to32(i, sb, avoid)?;
                if let (LaneOp32::I(va), LaneOp32::I(vb)) = (a32, b32) {
                    let sid = b.push_slot(Av::ImmI(va.wrapping_mul(vb) as i64));
                    b.binds.insert(dst, sid);
                    continue;
                }
                b.binds.remove(&dst);
                let prefer = b.dying_homes(i, &[sa, sb], true);
                let d = b.alloc_gpr(i + 1, &prefer, None)?;
                b.steps.push(LaneStep::Imul32 { d, a: a32, b: b32 });
                b.bind_int(dst, d, IV32);
            }
            Instr::UpvalGet { dst, idx } => {
                let sid = b
                    .buffer
                    .get(&idx)
                    .or_else(|| b.uentry.get(&idx))
                    .copied()
                    .ok_or("upval-read-order")?;
                b.binds.insert(dst, sid);
            }
            Instr::UpvalSet { idx, src } => {
                let sid = b.resolve(i, src)?;
                if !upvals.contains_key(&idx) {
                    return Err("upval-cell-missing");
                }
                b.buffer.insert(idx, sid);
            }
            Instr::StoreGlobal { idx, src } | Instr::StoreGlobalStrict { idx, src }
                if allow_global_stores =>
            {
                let sid = b.resolve(i, src)?;
                b.gbuffer.insert(idx, sid);
            }
            Instr::Return { src } => {
                if i != body.len() - 1 {
                    return Err("return-not-terminal");
                }
                let sid = b.resolve(i, src)?;
                match b.slots[sid] {
                    Av::ImmI(_) | Av::ImmF(_) => {
                        let bits = lane_box_imm(b.slots[sid]);
                        b.steps.push(LaneStep::RetImm { bits });
                    }
                    Av::Int { h, lo, hi } => {
                        b.steps.push(LaneStep::RetInt {
                            s: h,
                            narrow: lo >= IV32.0 && hi <= IV32.1,
                        });
                    }
                    Av::F64 { h } => b.steps.push(LaneStep::RetF64 { s: h }),
                    Av::Callee { .. } | Av::MathCallee { .. } => {
                        return Err("callee-value-escapes")
                    }
                }
                returned = true;
            }
            // The closed set is deliberately small: anything else (Mod, Neg,
            // comparisons, heap loads/stores, non-Imul MathOps, StoreGlobal,
            // ReturnUndefined, …) keeps the generic boxed loop.
            _ => return Err("op-outside-lane-set"),
        }
    }
    if !returned {
        return Err("no-return");
    }
    // ── exit upval commit: box everything into scratch slots FIRST (reads
    // the homes), then run the cell_set calls (which clobber the homes).
    let mut pending: Vec<(u16, usize)> = b.buffer.iter().map(|(&k, &s)| (k, s)).collect();
    pending.sort_unstable();
    for (k, &(_, sid)) in pending.iter().enumerate() {
        if k as u16 >= callee_reg_count {
            return Err("commit-slot-overflow");
        }
        let slot = reg_window + k as u16;
        match b.slots[sid] {
            Av::ImmI(_) | Av::ImmF(_) => {
                let bits = lane_box_imm(b.slots[sid]);
                b.steps.push(LaneStep::ImmToSlot { bits, slot });
            }
            Av::Int { h, lo, hi } => {
                b.steps.push(LaneStep::BoxIntToSlot {
                    s: h,
                    slot,
                    narrow: lo >= IV32.0 && hi <= IV32.1,
                });
            }
            Av::F64 { h } => b.steps.push(LaneStep::BoxF64ToSlot { s: h, slot }),
            Av::Callee { .. } | Av::MathCallee { .. } => return Err("callee-value-escapes"),
        }
    }
    for (k, &(idx, _)) in pending.iter().enumerate() {
        let cell_bits = *upvals.get(&idx).ok_or("upval-cell-missing")?;
        b.steps.push(LaneStep::CellCommit {
            cell_bits,
            slot: reg_window + k as u16,
        });
    }
    // ── exit global commit ── unlike cell commits these are raw stores of
    // already-boxed numeric values to planner-proved direct slots. They are
    // deliberately LAST: no guard, helper, allocation or fallback follows the
    // first commit, so interpreter replay can never double-apply a write.
    let mut gpending: Vec<(u32, usize)> = b.gbuffer.iter().map(|(&k, &s)| (k, s)).collect();
    gpending.sort_unstable();
    for (slot, sid) in gpending {
        match b.slots[sid] {
            Av::ImmI(_) | Av::ImmF(_) => b.steps.push(LaneStep::GlobalCommitImm {
                slot,
                bits: lane_box_imm(b.slots[sid]),
            }),
            Av::Int { h, lo, hi } => b.steps.push(LaneStep::GlobalCommitInt {
                slot,
                s: h,
                narrow: lo >= IV32.0 && hi <= IV32.1,
            }),
            Av::F64 { h } => b.steps.push(LaneStep::GlobalCommitF64 { slot, s: h }),
            Av::Callee { .. } | Av::MathCallee { .. } => return Err("global-value-unresolved"),
        }
    }
    let n_ops = b.steps.len() as u16;
    let n_guards = b.n_guards;
    Ok(TypedLanePlan {
        steps: b.steps,
        hop_pool: b.hop_pool,
        n_ops,
        n_guards,
    })
}

/// Emit a scheduled typed lane. Every guard/bail jumps to `fallback` (the
/// unchanged per-call helper — a pure prefix; nothing is committed before the
/// exit steps). `dst` is the caller's destination register.
fn emit_typed_lane(
    ops: &mut dynasmrt::x64::Assembler,
    lane: &TypedLanePlan,
    cell_get: usize,
    cell_set: usize,
    dst: u16,
    fallback: dynasmrt::DynamicLabel,
) {
    use crate::bytecode::BitwiseOp as B;
    for step in &lane.steps {
        match *step {
            LaneStep::UpvalCall {
                cell_bits,
                park_slot,
            } => {
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, QWORD cell_bits as i64
                    ; mov rax, QWORD cell_get as i64
                    ; call rax
                    ; mov rcx, QWORD SELF_CALL_DEOPT as i64
                    ; cmp rax, rcx
                    ; je => fallback
                );
                if let Some(slot) = park_slot {
                    dynasm!(ops ; mov [rbx + dreg(slot)], rax);
                }
            }
            LaneStep::UpvalBind { park_slot, d } => {
                if let Some(slot) = park_slot {
                    dynasm!(ops ; mov rax, [rbx + dreg(slot)]);
                }
                dynasm!(ops
                    ; mov rcx, rax
                    ; shr rcx, 48
                    ; cmp ecx, INT_TAG_HI as i32
                    ; jne => fallback
                    ; movsxd Rq(d), eax
                );
            }
            LaneStep::ParamLoad { slot, d } => {
                dynasm!(ops
                    ; mov rax, [rbx + dreg(slot)]
                    ; mov rcx, rax
                    ; shr rcx, 48
                    ; cmp ecx, INT_TAG_HI as i32
                    ; jne => fallback
                    ; movsxd Rq(d), eax
                );
            }
            LaneStep::GlobalRouteGuard { epoch_val } => {
                let epoch_off = crate::vm::host_api::JIT_GLOBAL_ROUTE_EPOCH_OFFSET as i32;
                dynasm!(ops
                    ; cmp DWORD [rdi + epoch_off], epoch_val as i32
                    ; jne => fallback
                );
            }
            LaneStep::CallDepthGuard => {
                let depth_off = crate::vm::host_api::JIT_CALL_DEPTH_OFFSET as i32;
                dynasm!(ops
                    ; mov eax, [rdi + depth_off]
                    ; cmp eax, crate::vm::JIT_REGION_CALL_MAX as i32
                    ; jae => fallback
                );
            }
            LaneStep::GlobalLoadInt { slot, d } => {
                dynasm!(ops
                    ; mov rax, [r12 + (slot as i32) * 8]
                    ; mov rcx, rax
                    ; shr rcx, 48
                    ; cmp ecx, INT_TAG_HI as i32
                    ; jne => fallback
                    ; movsxd Rq(d), eax
                );
            }
            LaneStep::GlobalCommitImm { slot, bits } => {
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [r12 + (slot as i32) * 8], rax
                );
            }
            LaneStep::GlobalCommitInt { slot, s, narrow } => {
                if narrow {
                    dynasm!(ops
                        ; mov eax, Rd(s)
                        ; mov rcx, QWORD INT_TAG as i64
                        ; or rax, rcx
                        ; mov [r12 + (slot as i32) * 8], rax
                    );
                } else {
                    let wide = ops.new_dynamic_label();
                    let store = ops.new_dynamic_label();
                    dynasm!(ops
                        ; movsxd rax, Rd(s)
                        ; cmp rax, Rq(s)
                        ; jne => wide
                        ; mov eax, eax
                        ; mov rcx, QWORD INT_TAG as i64
                        ; or rax, rcx
                        ; jmp => store
                        ; => wide
                        ; xorps xmm0, xmm0
                        ; cvtsi2sd xmm0, Rq(s)
                        ; movq rax, xmm0
                        ; => store
                        ; mov [r12 + (slot as i32) * 8], rax
                    );
                }
            }
            LaneStep::GlobalCommitF64 { slot, s } => {
                dynasm!(ops
                    ; movq rax, Rx(s)
                    ; mov [r12 + (slot as i32) * 8], rax
                );
            }
            LaneStep::CalleeGuard { gidx, bits, ver } => {
                // Reads the LIVE global slot (the value the guarded register
                // was loaded from — no store op is admitted between the load
                // and this guard, so the slot cannot have changed in between)
                // and re-checks the same (bits, version) tuple the generic
                // nested-Call arm checks. r13 is valid: the lane runs no
                // allocating helper before this point.
                dynasm!(ops
                    ; mov rax, [r12 + (gidx as i32) * 8]
                    ; mov rcx, QWORD bits as i64
                    ; cmp rax, rcx
                    ; jne => fallback
                    ; mov ecx, eax
                    ; mov eax, [r13 + rcx * 4]
                    ; cmp eax, ver as i32
                    ; jne => fallback
                );
            }
            LaneStep::MathImulGuard { gidx, guard } => {
                dynasm!(ops
                    ; mov rax, [r12 + (gidx as i32) * 8]
                    ; mov rcx, QWORD guard.receiver_bits as i64
                    ; cmp rax, rcx
                    ; jne => fallback
                    ; mov ecx, eax
                    ; cmp DWORD [r13 + rcx * 4], guard.receiver_ver as i32
                    ; jne => fallback
                    ; mov rcx, QWORD guard.receiver_vals as i64
                    ; mov rax, [rcx + (guard.receiver_slot as i32) * 8]
                    ; mov rcx, QWORD guard.callee_bits as i64
                    ; cmp rax, rcx
                    ; jne => fallback
                    ; mov ecx, eax
                    ; cmp DWORD [r13 + rcx * 4], guard.callee_ver as i32
                    ; jne => fallback
                );
            }
            LaneStep::GImm { d, v } => {
                dynasm!(ops ; mov Rq(d), QWORD v);
            }
            LaneStep::IAdd { d, a, b, sub } => match b {
                LaneOp32::R(rb) => {
                    if d == a {
                        if sub {
                            dynasm!(ops ; sub Rq(d), Rq(rb));
                        } else {
                            dynasm!(ops ; add Rq(d), Rq(rb));
                        }
                    } else if d == rb && !sub {
                        dynasm!(ops ; add Rq(d), Rq(a));
                    } else {
                        dynasm!(ops ; mov rax, Rq(a));
                        if sub {
                            dynasm!(ops ; sub rax, Rq(rb));
                        } else {
                            dynasm!(ops ; add rax, Rq(rb));
                        }
                        dynasm!(ops ; mov Rq(d), rax);
                    }
                }
                LaneOp32::I(imm) => {
                    if d == a {
                        if sub {
                            dynasm!(ops ; sub Rq(d), imm);
                        } else {
                            dynasm!(ops ; add Rq(d), imm);
                        }
                    } else {
                        dynasm!(ops ; mov rax, Rq(a));
                        if sub {
                            dynasm!(ops ; sub rax, imm);
                        } else {
                            dynasm!(ops ; add rax, imm);
                        }
                        dynasm!(ops ; mov Rq(d), rax);
                    }
                }
            },
            LaneStep::IAddImmRev { d, imm, b } => {
                dynasm!(ops
                    ; mov rax, QWORD imm
                    ; sub rax, Rq(b)
                    ; mov Rq(d), rax
                );
            }
            LaneStep::Bit32 { d, a, b, op } => match op {
                B::And | B::Or | B::Xor => {
                    // In-place when the dst reuses a (or, commutatively, b);
                    // otherwise via eax. The 32-bit op zeroes the upper half,
                    // so the trailing movsxd restores the exact-i64
                    // invariant.
                    let inplace_rhs = match (a, b) {
                        (LaneOp32::R(ra), _) if ra == d => Some(b),
                        (_, LaneOp32::R(rb)) if rb == d => Some(a),
                        _ => None,
                    };
                    if let Some(rhs) = inplace_rhs {
                        match rhs {
                            LaneOp32::R(r) => match op {
                                B::And => dynasm!(ops ; and Rd(d), Rd(r)),
                                B::Or => dynasm!(ops ; or Rd(d), Rd(r)),
                                _ => dynasm!(ops ; xor Rd(d), Rd(r)),
                            },
                            LaneOp32::I(v) => match op {
                                B::And => dynasm!(ops ; and Rd(d), v),
                                B::Or => dynasm!(ops ; or Rd(d), v),
                                _ => dynasm!(ops ; xor Rd(d), v),
                            },
                        }
                        dynasm!(ops ; movsxd Rq(d), Rd(d));
                    } else {
                        match a {
                            LaneOp32::R(r) => dynasm!(ops ; mov eax, Rd(r)),
                            LaneOp32::I(v) => dynasm!(ops ; mov eax, v),
                        }
                        match b {
                            LaneOp32::R(r) => match op {
                                B::And => dynasm!(ops ; and eax, Rd(r)),
                                B::Or => dynasm!(ops ; or eax, Rd(r)),
                                _ => dynasm!(ops ; xor eax, Rd(r)),
                            },
                            LaneOp32::I(v) => match op {
                                B::And => dynasm!(ops ; and eax, v),
                                B::Or => dynasm!(ops ; or eax, v),
                                _ => dynasm!(ops ; xor eax, v),
                            },
                        }
                        dynasm!(ops ; movsxd Rq(d), eax);
                    }
                }
                B::Shl | B::Shr | B::Ushr => {
                    // Variable count goes through cl (rcx is scratch); the
                    // value moves into d's low 32 first (a 32-bit mov, so
                    // `>>>` results stay zero-extended for free — emitted
                    // even when d == a for Ushr, where `mov r,r` IS the
                    // zero-extension of the count-0 case).
                    if let LaneOp32::R(rc) = b {
                        dynasm!(ops ; mov ecx, Rd(rc));
                    }
                    match a {
                        LaneOp32::R(ra) => {
                            if ra != d || matches!(op, B::Ushr) {
                                dynasm!(ops ; mov Rd(d), Rd(ra));
                            }
                        }
                        LaneOp32::I(v) => dynasm!(ops ; mov Rd(d), v),
                    }
                    match b {
                        LaneOp32::R(_) => match op {
                            B::Shl => dynasm!(ops ; shl Rd(d), cl),
                            B::Shr => dynasm!(ops ; sar Rd(d), cl),
                            _ => dynasm!(ops ; shr Rd(d), cl),
                        },
                        LaneOp32::I(k) => {
                            if k != 0 {
                                match op {
                                    B::Shl => dynasm!(ops ; shl Rd(d), k as i8),
                                    B::Shr => dynasm!(ops ; sar Rd(d), k as i8),
                                    _ => dynasm!(ops ; shr Rd(d), k as i8),
                                }
                            }
                        }
                    }
                    if !matches!(op, B::Ushr) {
                        dynasm!(ops ; movsxd Rq(d), Rd(d));
                    }
                }
            },
            LaneStep::Imul32 { d, a, b } => {
                match a {
                    LaneOp32::R(r) => dynasm!(ops ; mov eax, Rd(r)),
                    LaneOp32::I(v) => dynasm!(ops ; mov eax, v),
                }
                match b {
                    LaneOp32::R(r) => dynasm!(ops ; imul eax, Rd(r)),
                    LaneOp32::I(v) => dynasm!(ops ; mov ecx, v ; imul eax, ecx),
                }
                dynasm!(ops ; movsxd Rq(d), eax);
            }
            LaneStep::SignExt { d, s } => {
                dynasm!(ops ; movsxd Rq(d), Rd(s));
            }
            LaneStep::ZeroExt { d, s } => {
                dynasm!(ops ; mov Rd(d), Rd(s));
            }
            LaneStep::ToI32F64 { d, s } => {
                // cvttsd2si's indefinite (NaN/±Inf/|x| ≥ 2^63) fails the
                // round-trip compare like every other out-of-i32 value, so
                // one branch covers the whole bail set. Out-of-range means
                // the modular ToInt32 wrap — the fallback re-runs the call
                // with full semantics.
                dynasm!(ops
                    ; cvttsd2si rax, Rx(s)
                    ; movsxd rcx, eax
                    ; cmp rcx, rax
                    ; jne => fallback
                    ; mov Rq(d), rcx
                );
            }
            LaneStep::CvtIX { d, s } => {
                dynasm!(ops
                    ; xorps Rx(d), Rx(d)
                    ; cvtsi2sd Rx(d), Rq(s)
                );
            }
            LaneStep::XImm { d, bits } => {
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; movq Rx(d), rax
                );
            }
            LaneStep::FBin { d, a, b, op } => {
                debug_assert!(matches!(op, DOp::Add | DOp::Mul) || d != b || d == a);
                if d == a {
                    match op {
                        DOp::Add => dynasm!(ops ; addsd Rx(d), Rx(b)),
                        DOp::Sub => dynasm!(ops ; subsd Rx(d), Rx(b)),
                        DOp::Mul => dynasm!(ops ; mulsd Rx(d), Rx(b)),
                        DOp::Div => dynasm!(ops ; divsd Rx(d), Rx(b)),
                    }
                } else if d == b {
                    match op {
                        DOp::Add => dynasm!(ops ; addsd Rx(d), Rx(a)),
                        DOp::Mul => dynasm!(ops ; mulsd Rx(d), Rx(a)),
                        // The builder never routes a non-commutative op here.
                        _ => unreachable!("non-commutative FBin with d == b"),
                    }
                } else {
                    dynasm!(ops ; movaps Rx(d), Rx(a));
                    match op {
                        DOp::Add => dynasm!(ops ; addsd Rx(d), Rx(b)),
                        DOp::Sub => dynasm!(ops ; subsd Rx(d), Rx(b)),
                        DOp::Mul => dynasm!(ops ; mulsd Rx(d), Rx(b)),
                        DOp::Div => dynasm!(ops ; divsd Rx(d), Rx(b)),
                    }
                }
            }
            LaneStep::RetImm { bits } => {
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            LaneStep::RetInt { s, narrow } => {
                emit_lane_box_int(ops, s, narrow, dst);
            }
            LaneStep::RetF64 { s } => {
                dynasm!(ops
                    ; movq rax, Rx(s)
                    ; mov [rbx + dreg(dst)], rax
                );
            }
            LaneStep::BoxIntToSlot { s, slot, narrow } => {
                emit_lane_box_int(ops, s, narrow, slot);
            }
            LaneStep::BoxF64ToSlot { s, slot } => {
                dynasm!(ops
                    ; movq rax, Rx(s)
                    ; mov [rbx + dreg(slot)], rax
                );
            }
            LaneStep::ImmToSlot { bits, slot } => {
                dynasm!(ops
                    ; mov rax, QWORD bits as i64
                    ; mov [rbx + dreg(slot)], rax
                );
            }
            LaneStep::CellCommit { cell_bits, slot } => {
                dynasm!(ops
                    ; mov rcx, rdi
                    ; mov rdx, QWORD cell_bits as i64
                    ; mov r8, [rbx + dreg(slot)]
                    ; mov rax, QWORD cell_set as i64
                    ; call rax
                );
            }
            // ── W19 (MI-LANE) ── `this.<field>`, Int representation. The
            // absolute address is valid behind the arm's identity+version
            // guard (a `vals` realloc or an own-key add bumps the version);
            // the tag test is `ParamLoad`'s, on a baked address.
            LaneStep::FieldLoadInt { vals_ptr, slot, d } => {
                dynasm!(ops
                    ; mov rcx, QWORD vals_ptr as i64
                    ; mov rax, [rcx + (slot as i32) * 8]
                    ; mov rcx, rax
                    ; shr rcx, 48
                    ; cmp ecx, INT_TAG_HI as i32
                    ; jne => fallback
                    ; movsxd Rq(d), eax
                );
            }
            // Double representation: the high 16 bits must fall OUTSIDE the
            // tag band [INT_TAG_HI, TAG_HI] — the same discriminator
            // `load_num_xmm` uses to separate a real f64 from Int/bool/null/
            // undefined/heap. An in-band value (including an Int, if the slot
            // was re-typed since plan time) misses and re-runs the call.
            LaneStep::FieldLoadF64 { vals_ptr, slot, d } => {
                dynasm!(ops
                    ; mov rcx, QWORD vals_ptr as i64
                    ; mov rax, [rcx + (slot as i32) * 8]
                    ; mov rcx, rax
                    ; shr rcx, 48
                    ; sub ecx, INT_TAG_HI as i32
                    ; cmp ecx, (TAG_HI - INT_TAG_HI) as i32
                    ; jbe => fallback
                    ; movq Rx(d), rax
                );
            }
            // ── W19 (MI-LANE) ── the super guard block. Same three checks as
            // `emit_mi_body`'s copy, in the same order, but written with
            // rax/rcx ONLY: that copy uses rdx and r10 as scratch and both are
            // lane value homes (`LANE_GPR_HOMES`), so reusing it verbatim
            // would silently corrupt live intermediates. r13 (the version
            // array base) is valid — a v1 lane runs no allocating helper.
            LaneStep::SuperGuard {
                epoch_val,
                hops_at,
                hops_len,
                holder_vals_ptr,
                holder_slot,
                fn_bits,
            } => {
                let epoch_off = crate::vm::host_api::JIT_MI_CLASS_EPOCH_OFFSET as i32;
                dynasm!(ops
                    ; mov ecx, [rdi + epoch_off]
                    ; cmp ecx, DWORD epoch_val as i32
                    ; jne => fallback
                );
                for &(idx, ver) in
                    &lane.hop_pool[hops_at as usize..hops_at as usize + hops_len as usize]
                {
                    dynasm!(ops
                        ; mov ecx, [r13 + (idx as i32) * 4]
                        ; cmp ecx, DWORD ver as i32
                        ; jne => fallback
                    );
                }
                dynasm!(ops
                    ; mov rcx, QWORD holder_vals_ptr as i64
                    ; mov rax, [rcx + (holder_slot as i32) * 8]
                    ; mov rcx, QWORD fn_bits as i64
                    ; cmp rax, rcx
                    ; jne => fallback
                );
            }
        }
    }
}

/// Box the exact i64 in home `s` into window register `dst` with `Value::num`
/// shaping: in-i32 narrows to the Int tag (proven at plan time when
/// `narrow`), a wider exact integer converts to its (exact, ≤ 2^53) double.
fn emit_lane_box_int(ops: &mut dynasmrt::x64::Assembler, s: u8, narrow: bool, dst: u16) {
    if narrow {
        dynasm!(ops
            ; mov eax, Rd(s)
            ; mov rcx, QWORD INT_TAG as i64
            ; or rax, rcx
            ; mov [rbx + dreg(dst)], rax
        );
    } else {
        let wide = ops.new_dynamic_label();
        let store = ops.new_dynamic_label();
        dynasm!(ops
            ; movsxd rax, Rd(s)
            ; cmp rax, Rq(s)
            ; jne => wide
            ; mov eax, eax
            ; mov rcx, QWORD INT_TAG as i64
            ; or rax, rcx
            ; jmp => store
            ; => wide
            ; xorps xmm0, xmm0
            ; cvtsi2sd xmm0, Rq(s)
            ; movq rax, xmm0
            ; => store
            ; mov [rbx + dreg(dst)], rax
        );
    }
}

#[cfg(test)]
mod lane_tests {
    use super::*;

    fn no_upvals() -> FxHashMap<u16, u64> {
        FxHashMap::default()
    }
    fn no_consts() -> FxHashMap<u32, u64> {
        FxHashMap::default()
    }
    fn no_nested() -> FxHashMap<usize, NestedGuard> {
        FxHashMap::default()
    }

    /// The fail-closed magnitude bound: an add chain whose interval could
    /// pass 2^53 must DECLINE (i64 adds would stay exact where f64 rounds).
    /// Synthetic body — the bytecode compiler's per-statement temps push a
    /// source-level chain this long past the leaf register cap before the
    /// lane ever sees it, but the bound check must hold regardless of where
    /// the body came from.
    #[test]
    fn add_chain_beyond_2p53_declines() {
        let mut body = vec![Instr::Add { dst: 3, a: 1, b: 2 }];
        for _ in 0..23 {
            body.push(Instr::Add { dst: 3, a: 3, b: 3 });
        }
        body.push(Instr::Return { src: 3 });
        let r = build_typed_lane(
            &body,
            2,
            2,
            10,
            20,
            8,
            &no_upvals(),
            &no_consts(),
            &no_nested(),
        );
        assert_eq!(r.err(), Some("i53-overflow"));
        // Control: one doubling fewer keeps every bound ≤ 2^53 and schedules.
        let mut ok = vec![Instr::Add { dst: 3, a: 1, b: 2 }];
        for _ in 0..21 {
            ok.push(Instr::Add { dst: 3, a: 3, b: 3 });
        }
        ok.push(Instr::Return { src: 3 });
        let r = build_typed_lane(
            &ok,
            2,
            2,
            10,
            20,
            8,
            &no_upvals(),
            &no_consts(),
            &no_nested(),
        );
        assert!(r.is_ok(), "bounded chain declined: {:?}", r.err());
    }

    /// Register budget: seven simultaneously-live integers exceed the five
    /// GPR homes — decline, never a silent spill.
    #[test]
    fn register_budget_declines() {
        let body = vec![
            Instr::Bitwise {
                dst: 7,
                a: 1,
                b: 2,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::Bitwise {
                dst: 8,
                a: 3,
                b: 4,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::Bitwise {
                dst: 9,
                a: 5,
                b: 6,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::Bitwise {
                dst: 10,
                a: 1,
                b: 3,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::Bitwise {
                dst: 11,
                a: 2,
                b: 5,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::Bitwise {
                dst: 12,
                a: 4,
                b: 6,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::Bitwise {
                dst: 7,
                a: 7,
                b: 8,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::Bitwise {
                dst: 7,
                a: 7,
                b: 9,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::Bitwise {
                dst: 7,
                a: 7,
                b: 10,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::Bitwise {
                dst: 7,
                a: 7,
                b: 11,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::Bitwise {
                dst: 7,
                a: 7,
                b: 12,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::Return { src: 7 },
        ];
        let r = build_typed_lane(
            &body,
            6,
            6,
            10,
            20,
            16,
            &no_upvals(),
            &no_consts(),
            &no_nested(),
        );
        assert_eq!(r.err(), Some("gpr-budget"));
    }

    /// A param the site never passes reads `undefined` — decline at plan
    /// time (the generic path's NaN arithmetic takes over).
    #[test]
    fn unpassed_param_declines() {
        let body = vec![Instr::Add { dst: 3, a: 1, b: 2 }, Instr::Return { src: 3 }];
        let r = build_typed_lane(
            &body,
            2,
            0,
            10,
            20,
            8,
            &no_upvals(),
            &no_consts(),
            &no_nested(),
        );
        assert_eq!(r.err(), Some("param-not-passed"));
    }

    /// Ops outside the closed numeric set decline (fail-closed).
    #[test]
    fn out_of_set_op_declines() {
        let body = vec![Instr::Mod { dst: 3, a: 1, b: 2 }, Instr::Return { src: 3 }];
        let r = build_typed_lane(
            &body,
            2,
            2,
            10,
            20,
            8,
            &no_upvals(),
            &no_consts(),
            &no_nested(),
        );
        assert_eq!(r.err(), Some("op-outside-lane-set"));
    }

    /// Two f64 operands in ONE bitwise op each need their own ToInt32 temp —
    /// the second allocation must not hand back the first temp's home (it is
    /// untracked, so liveness alone would).
    #[test]
    fn two_f64_toint32_temps_are_distinct() {
        let body = vec![
            Instr::Div { dst: 3, a: 1, b: 2 },
            Instr::Div { dst: 4, a: 2, b: 1 },
            Instr::Bitwise {
                dst: 5,
                a: 3,
                b: 4,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::Return { src: 5 },
        ];
        let r = build_typed_lane(
            &body,
            2,
            2,
            10,
            20,
            8,
            &no_upvals(),
            &no_consts(),
            &no_nested(),
        );
        let lane = r.expect("two-div xor must schedule");
        let temps: Vec<u8> = lane
            .steps
            .iter()
            .filter_map(|s| match *s {
                LaneStep::ToI32F64 { d, .. } => Some(d),
                _ => None,
            })
            .collect();
        assert_eq!(temps.len(), 2, "both operands convert");
        assert_ne!(
            temps[0], temps[1],
            "shared temp home would clobber operand a"
        );
    }

    /// A typed lane must decline guarded Math until its register model carries
    /// the captured callee and receiver identity inputs.
    #[test]
    fn guarded_math_declines_typed_lane() {
        let mut upvals = FxHashMap::default();
        upvals.insert(0u16, Value::heap(1234).bits());
        let mut consts = FxHashMap::default();
        consts.insert(0u32, Value::num(4294967296.0).bits());
        let body = vec![
            Instr::UpvalGet { dst: 3, idx: 0 },
            Instr::AddInt {
                dst: 4,
                a: 3,
                imm: 0x6D2B79F5u32 as i32,
                upd: false,
            },
            Instr::LoadInt { dst: 5, val: 0 },
            Instr::Bitwise {
                dst: 4,
                a: 4,
                b: 5,
                op: crate::bytecode::BitwiseOp::Or,
            },
            Instr::UpvalSet { idx: 0, src: 4 },
            Instr::UpvalGet { dst: 6, idx: 0 },
            Instr::LoadInt { dst: 7, val: 15 },
            Instr::Bitwise {
                dst: 8,
                a: 6,
                b: 7,
                op: crate::bytecode::BitwiseOp::Ushr,
            },
            Instr::Bitwise {
                dst: 9,
                a: 6,
                b: 8,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            Instr::MathOp {
                dst: 10,
                op: MathFn::Imul,
                callee: 1,
                this_v: 2,
                arg_base: 8,
                argc: 2,
            },
            Instr::LoadInt { dst: 11, val: 0 },
            Instr::Bitwise {
                dst: 12,
                a: 10,
                b: 11,
                op: crate::bytecode::BitwiseOp::Ushr,
            },
            Instr::LoadConst { dst: 13, idx: 0 },
            Instr::Div {
                dst: 14,
                a: 12,
                b: 13,
            },
            Instr::Mul {
                dst: 15,
                a: 14,
                b: 1,
            },
            Instr::LoadInt { dst: 16, val: 0 },
            Instr::Bitwise {
                dst: 17,
                a: 15,
                b: 16,
                op: crate::bytecode::BitwiseOp::Or,
            },
            Instr::Return { src: 17 },
        ];
        let r = build_typed_lane(&body, 1, 1, 10, 20, 18, &upvals, &consts, &no_nested());
        assert_eq!(r.err(), Some("math-receiver-not-captured"));
    }

    /// Transactional global writes are never emitted in source order. Even a
    /// late type guard must precede the first store; repeated writes collapse
    /// to last-write-wins, and a read after a buffered write forwards that
    /// buffered value instead of re-reading the live global table.
    #[test]
    fn method_global_commits_are_a_guard_free_tail() {
        let body = vec![
            Instr::LoadGlobal { dst: 1, idx: 7 },
            Instr::LoadInt { dst: 2, val: 1 },
            Instr::Add { dst: 3, a: 1, b: 2 },
            Instr::StoreGlobalStrict { idx: 7, src: 3 },
            // Must forward the buffered value written above.
            Instr::LoadGlobal { dst: 4, idx: 7 },
            // Its Int tag check is deliberately late, after the first textual
            // StoreGlobal, and must still run before any physical commit.
            Instr::LoadGlobal { dst: 5, idx: 8 },
            Instr::Bitwise {
                dst: 6,
                a: 4,
                b: 5,
                op: crate::bytecode::BitwiseOp::Xor,
            },
            // Repeat slot 7 and alias the buffered slot-7 value into slot 9.
            Instr::StoreGlobal { idx: 7, src: 6 },
            Instr::StoreGlobal { idx: 9, src: 4 },
            Instr::Return { src: 6 },
        ];
        let mi = MiLaneCtx {
            vals_ptr: 0,
            fields: FxHashMap::default(),
            guards: FxHashMap::default(),
        };
        let lane = build_lane_inner(
            &body,
            0,
            0,
            0,
            0,
            10,
            &no_upvals(),
            &no_consts(),
            &no_nested(),
            Some(&mi),
            Some(0),
            true,
            None,
        )
        .expect("closed global method body must schedule");

        let first_commit = lane
            .steps
            .iter()
            .position(|s| {
                matches!(
                    s,
                    LaneStep::GlobalCommitImm { .. }
                        | LaneStep::GlobalCommitInt { .. }
                        | LaneStep::GlobalCommitF64 { .. }
                )
            })
            .expect("buffered stores must commit");
        assert!(lane.steps[..first_commit]
            .iter()
            .any(|s| matches!(s, LaneStep::CallDepthGuard)));
        assert!(lane.steps[..first_commit]
            .iter()
            .any(|s| matches!(s, LaneStep::GlobalRouteGuard { .. })));
        assert!(lane.steps[..first_commit]
            .iter()
            .any(|s| matches!(s, LaneStep::GlobalLoadInt { slot: 8, .. })));
        assert!(lane.steps[first_commit..].iter().all(|s| matches!(
            s,
            LaneStep::GlobalCommitImm { .. }
                | LaneStep::GlobalCommitInt { .. }
                | LaneStep::GlobalCommitF64 { .. }
        )));

        let loads_7 = lane
            .steps
            .iter()
            .filter(|s| matches!(s, LaneStep::GlobalLoadInt { slot: 7, .. }))
            .count();
        assert_eq!(loads_7, 1, "read-after-write must use the buffered value");
        let committed: Vec<u32> = lane.steps[first_commit..]
            .iter()
            .map(|s| match *s {
                LaneStep::GlobalCommitImm { slot, .. }
                | LaneStep::GlobalCommitInt { slot, .. }
                | LaneStep::GlobalCommitF64 { slot, .. } => slot,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(committed, vec![7, 9], "last write per slot, sorted commit");
    }

    #[test]
    fn method_globals_fail_closed_without_exact_route_or_store_kind() {
        let mi = MiLaneCtx {
            vals_ptr: 0,
            fields: FxHashMap::default(),
            guards: FxHashMap::default(),
        };
        let load = [
            Instr::LoadGlobal { dst: 1, idx: 7 },
            Instr::Return { src: 1 },
        ];
        let unguarded = build_lane_inner(
            &load,
            0,
            0,
            0,
            0,
            2,
            &no_upvals(),
            &no_consts(),
            &no_nested(),
            Some(&mi),
            None,
            true,
            None,
        );
        assert_eq!(unguarded.err(), Some("global-route-unguarded"));

        // StoreGlobalResolved carries environment-routing semantics that this
        // root-slot transaction deliberately does not reproduce.
        let resolved = [
            Instr::LoadInt { dst: 1, val: 1 },
            Instr::StoreGlobalResolved { idx: 7, src: 1 },
            Instr::Return { src: 1 },
        ];
        let unsupported = build_lane_inner(
            &resolved,
            0,
            0,
            0,
            0,
            2,
            &no_upvals(),
            &no_consts(),
            &no_nested(),
            Some(&mi),
            Some(0),
            true,
            None,
        );
        assert_eq!(unsupported.err(), Some("op-outside-lane-set"));
    }
}
