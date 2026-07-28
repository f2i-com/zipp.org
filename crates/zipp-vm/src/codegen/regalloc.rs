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
    let plan = plan_region(proto, start, end, ta_plan, false)?;
    let mut ops = dynasmrt::x64::Assembler::new().ok()?;
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
            Instr::Add { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Add, &mut lc),
            Instr::Sub { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Sub, &mut lc),
            Instr::Mul { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Mul, &mut lc),
            Instr::Div { dst, a, b } => emit_dbin(&mut ops, &plan, dst, a, b, DOp::Div, &mut lc),
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
                    ; movsd Rx(d), [rdx + rcx * 8]    // f64 element → home
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
            _ => return None,
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

    let buf = ops.finalize().ok()?;
    let entry_ptr = buf.ptr(dynasmrt::AssemblyOffset(0));
    Some(JitFn { _buf: buf, entry: entry_ptr })
}

