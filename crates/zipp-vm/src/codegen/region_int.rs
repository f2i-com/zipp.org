// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// `2^53` — the largest magnitude where consecutive integers are all exactly
/// representable in f64. Above it, JS `+`/`-` round, so an exact i64 result would
/// diverge: the int path bails to the interpreter when a result leaves
/// `[-2^53, 2^53]`. (Too large for a `cmp r64, imm32`, so it goes via a register.)
pub(crate) const TWO_POW_53: i64 = 9_007_199_254_740_992;
/// `2^54` — the unsigned upper bound for the shifted range check `(x + 2^53) ≤ 2^54`.
pub(crate) const TWO_POW_54: i64 = 18_014_398_509_481_984;

/// Can the loop region `[start, end]` run on the INTEGER path? Stricter than
/// `region_is_int`: every op must be integer-valued (no Div — fractional; `Mod`
/// IS allowed, via integer `idiv`), and every `LoadConst` must be an Int-tagged
/// constant (a double constant would be misread as i64).
pub(crate) fn region_is_int(proto: &FuncProto, start: u32, end: u32, ta_plan: &TaPinPlan) -> bool {
    int_unadmitted_ips(proto, start, end, ta_plan).is_some_and(|v| v.is_empty())
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
pub(crate) fn int_unadmitted_ips(
    proto: &FuncProto,
    start: u32,
    end: u32,
    ta_plan: &TaPinPlan,
) -> Option<Vec<usize>> {
    if !region_can_compile(proto, start, end, None) {
        return None;
    }
    let mut unadmitted: Vec<usize> = Vec::new();
    let (s, e) = (start as usize, end as usize);
    // A pinned Int32Array (kind 5) element access runs inline on the int path: the
    // element is a signed i32 ⇒ sign-extends to an i64 home (GetIndex) / stores its
    // low 32 bits (SetIndex). Any other element kind (e.g. a Float64Array) declines
    // here so the region falls through to the regalloc/memory path.
    let pinned_i32 = |ip: usize| -> bool {
        ta_plan.access.get(&ip).map_or(false, |&j| ta_plan.pins[j as usize].kind == 5)
    };
    // A pinned flat-ASCII STRING (kind 254) access: `str.charCodeAt(i)` (a direct
    // byte load into an i64 home, OOB→deopt) and `str.length` (read from the pin's
    // `units`). Both gate on the per-access identity guard. Lets the fnv1a-style
    // `for (i<str.length) h=imul(h^str.charCodeAt(i),C)` loop run unboxed.
    let pinned_str = |ip: usize| -> bool {
        ta_plan.access.get(&ip).map_or(false, |&j| ta_plan.pins[j as usize].kind == STR_PIN_KIND)
    };
    // A dense Array observed all-Int (kind 252): `arr[i]` loads the element and
    // unboxes it into an i64 home under a per-access tag guard. READS only —
    // a store would have to re-box and can grow/realloc the Vec, so `SetIndex`
    // still falls to the memory path (see the catch-all below).
    let pinned_int_arr = |ip: usize| -> bool {
        ta_plan.access.get(&ip).map_or(false, |&j| ta_plan.pins[j as usize].kind == ARR_INT_PIN_KIND)
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
            // A kind-5 (Int32) pinned element op is admissible; any other index op
            // (non-pinned, or a different element kind) declines to the mem path.
            Instr::GetIndex { .. } | Instr::SetIndex { .. } if pinned_i32(s + off) => {}
            // Dense all-Int Array READ (see `pinned_int_arr`); the write is not
            // admitted and falls through to the catch-all.
            Instr::GetIndex { .. } if pinned_int_arr(s + off) => {}
            // Pinned flat-ASCII STRING `str.length` (GetProp) + `str.charCodeAt(i)`
            // (CallMethod). A non-length GetProp / non-charCodeAt or unpinned call
            // still hits the catch-all reject below.
            Instr::GetProp { .. } if pinned_str(s + off) => {}
            // Dense all-Int Array `.length` — read straight from the pin snapshot.
            // The name is re-checked (the pin planner registers a GetProp only for
            // `length`, but this keeps that an assertion rather than an assumption).
            Instr::GetProp { name, .. }
                if pinned_int_arr(s + off)
                    && proto.string_constants.get(name as usize).is_some_and(|k| k == "length") => {}
            Instr::CallMethod { .. } if pinned_str(s + off) => {}
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
pub(crate) fn compile_region_int(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    ta_plan: &TaPinPlan,
    ta_snapshot: usize,
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
        meter,
    )
}

/// `cold_exit`: compile ops the INT emitter has no arm for as SIDE EXITS rather
/// than declining the region (B9). Sound because every i64 home is loaded from
/// the register file at region entry and only ever updated by ops that actually
/// execute natively — an op we exit at never runs natively, so no home can hold
/// a value it did not produce, and `flush_exit` writes every home back before
/// the interpreter resumes at that exact ip.
pub(crate) fn compile_region_int_maybe_cold(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    ta_plan: &TaPinPlan,
    ta_snapshot: usize,
    cold_exit: bool,
    meter: Option<crate::codegen::meter::Meter>,
) -> Option<JitFn> {
    let unadmitted = int_unadmitted_ips(proto, start, end, ta_plan)?;
    let cold: FxHashSet<usize> = if unadmitted.is_empty() {
        FxHashSet::default()
    } else if !cold_exit {
        if std::env::var_os("ZIPP_JITLOG").is_some() {
            eprintln!("[jit] INT decline [{start},{end}]: region_is_int=false");
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
        (s_..=e_).filter(|&ip| cold_blocks.contains(&block_of[ip - s_])).collect()
    };
    // The i64 homes carry sign-extended integers, so Bitwise (int32-lane) ops run
    // inline here with no per-op reload/rebox — admit them (admit_bitwise=true), and
    // plan_region's pinned-element handling targets kind-5 (Int32) elements.
    let plan = match plan_region_cold(proto, start, end, ta_plan, true, &cold) {
        Some(p) => p,
        None => {
            if std::env::var_os("ZIPP_JITLOG").is_some() {
                eprintln!("[jit] INT decline [{start},{end}]: plan_region=None");
            }
            return None;
        }
    };
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
    let (s, e) = (start as usize, end as usize);

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
    let (frame, xmm_off, ta_base) = if n_ta > 0 {
        (200 + 32 * n_ta, 32 + 32 * n_ta, 32i32)
    } else {
        (160i32, 0i32, 0i32)
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
    // Live-in globals/regs into i64 homes: an Int-tagged Value sign-extends, an
    // integral double in [-2^53, 2^53] converts, anything else takes entry_bail.
    for &(gi, x) in &plan.live_in_globs {
        dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]);
        emit_int_entry_load(&mut ops, x, entry_bail);
    }
    for &(r, x) in &plan.live_in_regs {
        dynasm!(ops ; mov rax, [rbx + dreg(r)]);
        emit_int_entry_load(&mut ops, x, entry_bail);
    }
    // Bool homes last: the int/global loads above use r10 as scratch and r10 is
    // itself a bool home, so loading bools earlier would be undone here.
    for &(r, g) in &plan.live_in_bools {
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
    dynasm!(ops ; jmp => lbl(start, &in_region));

    // ── body ──
    // Compare→branch flag fusion: the last EMITTED op being a compare leaves its
    // flags live for an immediately following conditional jump (no re-`test`).
    let mut flag_cmp: Option<(usize, u16, Cmp)> = None;
    // Redundant-copy tracker (see `LastCopy`).
    let mut lc: LastCopy = None;
    for ip in s..=e {
        dynasm!(ops ; => lbl(ip as u32, &in_region));
        if plan.jump_targets.contains(&ip) {
            lc = None; // control may arrive here with different home contents
        }
        // Charge this basic block before running it. A block is straight-line,
        // so entering it means executing all of it — the same count the
        // interpreter would have made, in the same unit.
        let charged =
            crate::codegen::meter::charge_block(&mut ops, &blocks, ip, &mut exit_stubs);
        // B9: an ip in a cold block never runs natively — flush every home and
        // hand this exact ip back to the interpreter, which runs the block (and
        // the rest of the iteration) itself.
        if cold.contains(&ip) {
            dynasm!(ops ; mov DWORD [rsi], ip as i32 ; jmp => flush_exit);
            continue;
        }
        if let Instr::LoadInt { dst, .. } | Instr::LoadConst { dst, .. } = proto.code[ip] {
            if plan.hoisted.contains(&dst) {
                continue;
            }
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
        match proto.code[ip] {
            Instr::LoadInt { .. } | Instr::LoadConst { .. } => {
                emit_int_const(&mut ops, &plan, &proto.code[ip], proto);
                if let Some(d) = writes_reg(&proto.code[ip]) {
                    copy_clobber(&mut lc, xh(&plan, d));
                }
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
            // A pinned-TA receiver's LoadGlobal is a no-op: it has no numeric home;
            // the element-access emitter reads the receiver via the pin's source.
            Instr::LoadGlobal { dst, .. } if plan.ta_recv_regs.contains(&dst) => {
                flag_cmp = prev_flag; // nothing emitted; flags still live
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
            Instr::StoreGlobal { idx, src } | Instr::StoreGlobalStrict { idx, src } => {
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
                emit_ibin(&mut ops, &plan, ip, flush_exit, dst, a, b, true, &mut lc);
            }
            Instr::Sub { dst, a, b } => {
                emit_ibin(&mut ops, &plan, ip, flush_exit, dst, a, b, false, &mut lc);
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
                        ; mov DWORD [rsi], ip as i32 // resume at THIS op (dst not written)
                        ; jmp => flush_exit
                        ; => done
                    );
                    emit_i53_guard(&mut ops, d, ip, flush_exit);
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
                    ; mov DWORD [rsi], ip as i32 // resume at THIS op (dst unwritten)
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
                    if !plan.elide_guard.contains(&ip) {
                        emit_i53_guard(&mut ops, d, ip, flush_exit);
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
                    ; mov DWORD [rsi], ip as i32
                    ; jmp => flush_exit
                    ; => nonzero
                    ; pxor xmm0, xmm0
                    ; psubq xmm0, Rx(ax)
                    ; movdqa Rx(d), xmm0
                );
                copy_clobber(&mut lc, d);
                if !plan.elide_guard.contains(&ip) {
                    emit_i53_guard(&mut ops, d, ip, flush_exit);
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
                match ta_plan.pins[j].src {
                    TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                    TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                }
                dynasm!(ops
                    ; cmp rax, [rsp + off]               // receiver vs snapshot obj_bits
                    ; jne => deopt
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
                    dynasm!(ops
                        ; mov rax, [rdx + rcx * 8]       // items[i] (Value bits)
                        ; mov r10, rax
                        ; shr r10, 48
                        ; cmp r10d, INT_TAG_HI as i32
                        ; jne => deopt                   // double / HOLE / heap → deopt
                        ; movsxd rax, eax                // Int payload, sign-extended
                    );
                } else {
                    dynasm!(ops
                        ; movsxd rax, DWORD [rdx + rcx * 4] // sign-extend i32 element → home
                    );
                }
                dynasm!(ops
                    ; movq Rx(d), rax
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], ip as i32         // resume AT this ip
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
                match ta_plan.pins[j].src {
                    TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                    TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                }
                dynasm!(ops
                    ; cmp rax, [rsp + off]
                    ; jne => deopt
                    ; movq rcx, Rx(kx)
                    ; cmp rcx, [rsp + off + 16]
                    ; jae => deopt
                    ; mov rdx, [rsp + off + 8]
                    ; movq rax, Rx(vx)                   // value i64 home
                    ; mov DWORD [rdx + rcx * 4], eax     // store low 32 (== ToInt32(v))
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], ip as i32
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
                match ta_plan.pins[j].src {
                    TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                    TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                }
                dynasm!(ops
                    ; cmp rax, [rsp + off]               // receiver identity vs snapshot
                    ; jne => deopt
                    ; movq rcx, Rx(kx)                   // index (i64 home, integral)
                    ; cmp rcx, [rsp + off + 16]          // unsigned: i < units (catches <0/OOB)
                    ; jae => deopt                       // OOB → deopt (interp yields NaN)
                    ; mov rdx, [rsp + off + 8]           // pinned bytes base
                    ; movzx eax, BYTE [rdx + rcx]        // ASCII code unit, zero-extend 0..255
                    ; movq Rx(d), rax
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], ip as i32         // resume AT this ip
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
                    matches!(ta_plan.pins[j as usize].kind, STR_PIN_KIND | ARR_INT_PIN_KIND)
                }) =>
            {
                let j = ta_plan.access[&ip] as usize;
                let off = ta_base + 32 * j as i32;
                let d = xh(&plan, dst);
                let deopt = ops.new_dynamic_label();
                let done = ops.new_dynamic_label();
                match ta_plan.pins[j].src {
                    TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                    TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                }
                dynasm!(ops
                    ; cmp rax, [rsp + off]               // receiver identity vs snapshot
                    ; jne => deopt
                    ; mov rax, [rsp + off + 16]          // units == str.length
                    ; movq Rx(d), rax
                    ; jmp => done
                    ; => deopt
                    ; mov DWORD [rsi], ip as i32
                    ; jmp => flush_exit
                    ; => done
                );
                copy_clobber(&mut lc, d);
                lc = None;
            }
            // ── Math.imul(a, b) ── ToInt32 of the low 32 bits of the product. The
            // i64 homes' low 32 bits ARE ToInt32 of the operands, so `imul eax,ecx`
            // gives the low 32 of the product (signedness-agnostic); interpreted
            // signed it IS Math.imul → sign-extend to the home (fits i32, no guard).
            // MUST NOT route through the generic i64 Mul arm (it i53-guards a 64-bit
            // product and would box e.g. imul(0xFFFF,0xFFFF) as +4294836225 not -131071).
            Instr::MathOp { dst, arg_base, op: MathFn::Imul, argc: 2, .. } => {
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
                dynasm!(ops ; mov DWORD [rsi], ip as i32 ; jmp => flush_exit);
            }
            _ => return None,
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
        emit_int_box_from_home(&mut ops, x);
        dynasm!(ops ; mov [rbx + dreg(r)], rax);
    }
    for &(r, g) in &plan.bool_regs {
        dynasm!(ops ; mov rax, QWORD BOOL_TAG as i64 ; or rax, Rq(g) ; mov [rbx + dreg(r)], rax);
    }
    for &(gi, x) in &plan.globs {
        emit_int_box_from_home(&mut ops, x);
        dynasm!(ops ; mov [r12 + (gi as i32) * 8], rax);
    }
    emit_region_restore_n(&mut ops, xmm_off, frame);

    // ── entry_bail ── a live-in wasn't Int-tagged; nothing computed, so restore
    // (NO flush) and resume at the header (interpreted).
    dynasm!(ops ; => entry_bail ; mov DWORD [rsi], start as i32);
    emit_region_restore_n(&mut ops, xmm_off, frame);

    let buf = ops.finalize().ok()?;
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn { _buf: buf, entry: entry_ptr })
}

