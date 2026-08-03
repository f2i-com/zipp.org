// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Register-promoting region codegen: each region value lives in a fixed xmm
/// (numbers) or gpr (booleans) home for the whole loop. Live-in values are
/// loaded + type-guarded ONCE at entry; the loop body is then pure register SSE
/// with NO per-op guards or memory traffic (this is what makes it competitive
/// with V8). All homes are flushed back to the reg file / globals on every exit.
pub(crate) fn compile_region_regalloc(
    proto: &FuncProto,
    start: u32,
    end: u32,
    globals_base_helper: usize,
    ta_plan: &TaPinPlan,
    ta_snapshot: usize,
    meter: Option<crate::codegen::meter::Meter>,
) -> Option<JitFn> {
    if !region_can_compile(proto, start, end, None) {
        return None;
    }
    // The regalloc path uses boxed-double semantics and cannot host Bitwise
    // (int32-lane) ops — they decline to the memory path here.
    let plan = plan_region(proto, start, end, ta_plan, false, true, true)?;
    if !plan.split_recvs.is_empty() && std::env::var_os("ZIPP_JITLOG").is_some() {
        let mut srs: Vec<u16> = plan.split_recvs.iter().copied().collect();
        srs.sort_unstable();
        for sr in srs {
            eprintln!("[jit] DOUBLE region [{start},{end}] B94 split receiver r{sr}");
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
        ; sub rsp, 40                 // shadow space (32) + 8 pad ⇒ rsp 16-aligned
        ; mov rcx, rdi
        ; mov rax, QWORD globals_base_helper as i64
        ; call rax
        ; mov r12, rax
        ; add rsp, 40
        ; sub rsp, frame              // [shadow][TA slots][xmm6..15 save][pad]
    );
    for k in 0..10u32 {
        let xi = 6 + k as u8;
        dynasm!(ops ; movdqu [rsp + xmm_off + (k as i32) * 16], Rx(xi));
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
    // Load live-in globals (guarded) and live-in registers (guarded).
    for &(gi, x) in &plan.live_in_globs {
        dynasm!(ops ; mov rax, [r12 + (gi as i32) * 8]);
        emit_box_to_home(&mut ops, x, entry_bail);
    }
    for &(r, x) in &plan.live_in_regs {
        dynasm!(ops ; mov rax, [rbx + dreg(r)]);
        emit_box_to_home(&mut ops, x, entry_bail);
    }
    // Bool homes last: the loads above use r10 as scratch and r10 is itself one
    // of BOOL_GPRS, so loading bools earlier would be undone here.
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
    for ip in s..=e {
        dynasm!(ops ; => lbl(ip as u32, &in_region));
        let charged =
            crate::codegen::meter::charge_block(&mut ops, &blocks, ip, &mut exit_stubs);
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
            Instr::Move { dst, src } | Instr::ToPropKey { dst, src, .. } => match home(&plan, dst) {
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
            },
            // A TA-receiver's LoadGlobal is a no-op: it has no numeric home; the
            // element-access emitter reads the receiver via the pin's source.
            Instr::LoadGlobal { dst, .. } if plan.ta_recv_regs.contains(&dst) => {
                flag_cmp = prev_flag; // nothing emitted; flags still live
            }
            // ── B94 split receiver ── this LoadGlobal is the RECEIVER half of a
            // recycled register. Its xmm home belongs to the register's numeric
            // half, so the object goes to the memory slot, which every pinned
            // access reads via `TaPinSrc::Reg` and which stays authoritative for
            // this register throughout the region.
            Instr::LoadGlobal { dst, idx } if plan.split_recv_lg.contains(&ip) => {
                dynasm!(ops
                    ; mov rax, [r12 + (idx as i32) * 8]
                    ; mov [rbx + dreg(dst)], rax
                );
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
            Instr::Bitwise { dst, a, b, op } => {
                use crate::bytecode::BitwiseOp as B;
                let (d, ax, bx) = (xh(&plan, dst), xh(&plan, a), xh(&plan, b));
                copy_clobber(&mut lc, d);
                let bw_bail = ops.new_dynamic_label();
                let bw_done = ops.new_dynamic_label();
                dynasm!(ops
                    ; cvttsd2si rax, Rx(ax)
                    ; mov r10, QWORD i64::MIN
                    ; cmp rax, r10
                    ; je => bw_bail
                    ; cvttsd2si rcx, Rx(bx)
                    ; cmp rcx, r10
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
                        (Cmp::Lt, true) => dynasm!(ops ; jbe => t),  // !(b > a)
                        (Cmp::Le, true) => dynasm!(ops ; jb => t),   // !(b >= a)
                        (Cmp::Gt, true) => dynasm!(ops ; jbe => t),  // !(a > b)
                        (Cmp::Ge, true) => dynasm!(ops ; jb => t),   // !(a >= b)
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
                match ta_plan.pins[j].src {
                    TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                    TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                }
                dynasm!(ops
                    ; cmp rax, [rsp + off]            // receiver vs snapshot obj_bits
                    ; jne => deopt
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
                match ta_plan.pins[j].src {
                    TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                    TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                }
                dynasm!(ops
                    ; cmp rax, [rsp + off]
                    ; jne => deopt
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
            Instr::CallMethod { dst, arg_base, argc, name, .. } => {
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
                match ta_plan.pins[j].src {
                    TaPinSrc::Global(g) => dynasm!(ops ; mov rax, [r12 + (g as i32) * 8]),
                    TaPinSrc::Reg(r) => dynasm!(ops ; mov rax, [rbx + dreg(r)]),
                }
                dynasm!(ops
                    ; cmp rax, [rsp + off]            // receiver vs snapshot obj_bits
                    ; jne => deopt
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
                let resume_ip =
                    if plan.dv_flag_fuse.contains_key(&ip) { ip - 1 } else { ip };
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
        // ── B94 write-through ── a numeric def of the split receiver must reach
        // MEMORY as well as its home, because `flush_exit` deliberately skips
        // this register and memory is what the interpreter reads on any exit.
        // Two instructions, once per def; the LoadGlobal half already stored.
        if let Some(d) = writes_reg(&proto.code[ip]) {
            let is_split = plan.split_recvs.contains(&d) && !plan.split_recv_lg.contains(&ip);
            if is_split || plan.write_through.contains(&d) {
                if let Home::Xmm(h) = plan.reg_home[&d] {
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
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn { _buf: buf, entry: entry_ptr, self_binding: None })
}

