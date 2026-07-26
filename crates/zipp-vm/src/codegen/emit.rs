// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Entry load for the int path: the Value bits are in `rax`. An Int-tagged Value
/// sign-extends its i32 payload; a DOUBLE holding an exact integer in
/// [-2^53, 2^53] is also admitted, converted with `cvttsd2si`. Anything else
/// takes `entry_bail`.
///
/// Admitting the integral double is what lets a loop RE-ENTER the integer tier
/// after its accumulator crosses 2^31. Region exit boxes an i64 home as Int only
/// when it fits i32 and as a double otherwise — so `for (k…) for (i…) s += a[i]`
/// over a large array would exit the inner region with `s` boxed as a double, and
/// an Int-only entry guard then rejected every subsequent entry: 64 deopts, then
/// eviction to the boxed memory path for the rest of the run. Measured on a
/// 40M-iteration nested sum, that was 425ms against 50ms for the same loop whose
/// accumulator happened to stay under 2^31.
///
/// The round-trip (`cvttsd2si` then back, compare equal) is what makes this
/// sound, and it needs no separate "is this really a double" test: every
/// non-double Value is NaN-boxed, so reading its bits as f64 gives a NaN, the
/// comparison is unordered, and `jp` bails. It also rejects a fractional double
/// (unequal) and ±Inf (`cvttsd2si` yields the i64::MIN sentinel, which fails both
/// the round-trip and the range check). Entry code runs once per region entry, so
/// the extra ~8 instructions never touch the loop body.
pub(crate) fn emit_int_entry_load(ops: &mut dynasmrt::x64::Assembler, home: u8, entry_bail: dynasmrt::DynamicLabel) {
    let as_double = ops.new_dynamic_label();
    let store = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => as_double         // not Int-tagged — try the integral-double form
        ; movsxd rax, eax          // sign-extend the i32 payload to i64
        ; movq Rx(home), rax
        ; jmp => done
        ; => as_double
        ; movq xmm0, rax
        ; cvttsd2si rcx, xmm0      // truncate toward zero (i64::MIN on NaN/Inf/overflow)
        ; cvtsi2sd xmm1, rcx
        ; ucomisd xmm0, xmm1
        ; jp => entry_bail         // unordered ⇒ NaN ⇒ a NaN-boxed non-double
        ; jne => entry_bail        // not exactly integral
        ; mov r10, QWORD 1i64 << 53
        ; cmp rcx, r10
        ; jg => entry_bail
        ; neg r10
        ; cmp rcx, r10
        ; jl => entry_bail         // outside [-2^53, 2^53] — an i64 home is exact only there
        // `ucomisd` reports -0.0 == +0.0, so the round-trip above ACCEPTS -0.0 and
        // would land it in the home as 0 — which exits boxed as Int +0, turning
        // `1/s` from -Infinity into +Infinity. An i64 home cannot represent -0 at
        // all (the same reason `Neg` bails on a zero operand), so reject it here
        // and keep that invariant true of every value entering the tier.
        ; test rcx, rcx
        ; jnz => store
        ; test rax, rax            // rax still holds the original Value bits
        ; js => entry_bail         // sign bit set with a zero magnitude ⇒ -0.0
        ; => store
        ; movq Rx(home), rcx
        ; => done
    );
}

/// Entry load for a bool gpr home: the Value bits are in `rax`. Guard Bool-tagged
/// (else `bail`), then put the 0/1 payload in the home. Scratch is `rdx`, NOT
/// `r10` — `r10` is itself one of `BOOL_GPRS`, so using it here would clobber an
/// already-loaded bool home on the next iteration of the load loop.
pub(crate) fn emit_bool_entry_load(ops: &mut dynasmrt::x64::Assembler, home: u8, bail: dynasmrt::DynamicLabel) {
    dynasm!(ops
        ; mov rdx, rax
        ; shr rdx, 48
        ; cmp edx, (INT_TAG_HI + 1) as i32 // 0x7FFA — BOOL_TAG's high 16 bits
        ; jne => bail                      // not Bool-tagged
        ; and eax, 1                       // payload is 0/1; zero-extends to rax
        ; mov Rq(home), rax
    );
}

/// Materialise an integer constant (`LoadInt`/`LoadConst`-Int) into its i64 home:
/// the FULL sign-extended i64 immediate, then `movq` (NOT cvtsi2sd — we want the
/// integer bit pattern, not its f64 form).
pub(crate) fn emit_int_const(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, instr: &Instr, proto: &FuncProto) {
    let (h, v) = match *instr {
        Instr::LoadInt { dst, val } => (xh(plan, dst), val as i64),
        Instr::LoadConst { dst, idx } => {
            let c = proto.constants[idx as usize];
            // region_is_int guaranteed c.is_int(); payload is the i32, sign-extend.
            (xh(plan, dst), (c.bits() as u32 as i32) as i64)
        }
        _ => unreachable!("emit_int_const on non-constant"),
    };
    dynasm!(ops ; mov rax, QWORD v ; movq Rx(h), rax);
}

/// Guard that the i64 in xmm home `h` is within `[-2^53, 2^53]` (signed); if not,
/// flush all homes and resume the interpreter at the NEXT ip (the overflowed
/// value flushes via cvtsi2sd to exactly JS's rounded result, so ip+1 is sound).
pub(crate) fn emit_i53_guard(ops: &mut dynasmrt::x64::Assembler, h: u8, ip: usize, flush_exit: dynasmrt::DynamicLabel) {
    // Range trick: x ∈ [-2^53, 2^53] ⟺ (x + 2^53) ≤ 2^54 as UNSIGNED (a value
    // below -2^53 wraps to a huge unsigned and fails too). The two constants are
    // pre-loaded once in the prologue (r13 = 2^53, r14 = 2^54) — avoiding two
    // 10-byte `movabs` per guard, which profiling showed dominated the loop.
    let ovf = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; movq rax, Rx(h)
        ; add rax, r13           // + 2^53 (no i64 overflow: |x| ≤ 2^54 here)
        ; cmp rax, r14           // 2^54
        ; jbe => done            // in range → continue
        ; => ovf
        ; mov DWORD [rsi], (ip + 1) as i32   // resume AFTER this op (result flushed)
        ; jmp => flush_exit
        ; => done
    );
}

/// Tracker for the most recent register-to-register home copy along the linear
/// emission path: `Some((d, s))` means homes `d` and `s` currently hold the SAME
/// value, so a pending `mov* d2, s2` over the same pair (either order) is a
/// no-op and can be skipped — this typically deletes the `tmp ← g; g ← tmp + x`
/// round-trip from a loop's carried dependency chain. Reset at every jump-target
/// ip (control may arrive with different contents) and invalidated whenever
/// either home is rewritten.
pub(crate) type LastCopy = Option<(u8, u8)>;

/// Would `movdqa/movaps Rx(d), Rx(s)` be a no-op given the tracked copy?
#[inline]
pub(crate) fn copy_is_noop(lc: LastCopy, d: u8, s: u8) -> bool {
    lc == Some((d, s)) || lc == Some((s, d))
}

/// Invalidate the tracker after home `h` is rewritten.
#[inline]
pub(crate) fn copy_clobber(lc: &mut LastCopy, h: u8) {
    if let Some((a, b)) = *lc {
        if a == h || b == h {
            *lc = None;
        }
    }
}

/// `home[dst] = home[a] <±> home[b]` as i64 (paddq/psubq), with aliasing handled
/// and a 2^53 guard (skipped when the interval analysis proved the result is
/// always in range). `add = true` ⇒ paddq (commutative); else psubq.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_ibin(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, ip: usize, flush_exit: dynasmrt::DynamicLabel, dst: u16, a: u16, b: u16, add: bool, lc: &mut LastCopy) {
    let (d, ax, bx) = (xh(plan, dst), xh(plan, a), xh(plan, b));
    if add {
        if d == ax || copy_is_noop(*lc, d, ax) {
            dynasm!(ops ; paddq Rx(d), Rx(bx));
        } else if d == bx || copy_is_noop(*lc, d, bx) {
            dynasm!(ops ; paddq Rx(d), Rx(ax)); // commutative
        } else {
            dynasm!(ops ; movdqa Rx(d), Rx(ax) ; paddq Rx(d), Rx(bx));
        }
    } else if d == ax || copy_is_noop(*lc, d, ax) {
        dynasm!(ops ; psubq Rx(d), Rx(bx));
    } else if d == bx {
        // dst == b (and ≠ a): use xmm0 to avoid clobbering b before reading it.
        dynasm!(ops ; movdqa xmm0, Rx(ax) ; psubq xmm0, Rx(bx) ; movdqa Rx(d), xmm0);
    } else {
        dynasm!(ops ; movdqa Rx(d), Rx(ax) ; psubq Rx(d), Rx(bx));
    }
    copy_clobber(lc, d);
    if !plan.elide_guard.contains(&ip) {
        emit_i53_guard(ops, d, ip, flush_exit);
    }
}

/// Set the integer flags for `home[a] <cmp> home[b]` (SIGNED). Reads `b` from
/// its prologue-filled gpr mirror when it is a hoisted constant (one `movq`
/// fewer in the loop body); symmetric for a constant `a`.
pub(crate) fn emit_icmp_flags(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, a: u16, b: u16) {
    if let Some(&(g, _)) = plan.gpr_const.get(&b) {
        let ax = xh(plan, a);
        dynasm!(ops ; movq rax, Rx(ax) ; cmp rax, Rq(g));
    } else if let Some(&(g, _)) = plan.gpr_const.get(&a) {
        let bx = xh(plan, b);
        dynasm!(ops ; movq rax, Rx(bx) ; cmp Rq(g), rax);
    } else {
        let (ax, bx) = (xh(plan, a), xh(plan, b));
        dynasm!(ops ; movq rax, Rx(ax) ; movq rcx, Rx(bx) ; cmp rax, rcx);
    }
}

/// `bool_home[dst] = (home[a] <cmp> home[b])` as SIGNED i64 comparison.
pub(crate) fn emit_icmp(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, dst: u16, a: u16, b: u16, cmp: Cmp) {
    let d = gh(plan, dst);
    emit_icmp_flags(ops, plan, a, b);
    match cmp {
        Cmp::Lt => dynasm!(ops ; setl al),
        Cmp::Le => dynasm!(ops ; setle al),
        Cmp::Gt => dynasm!(ops ; setg al),
        Cmp::Ge => dynasm!(ops ; setge al),
        Cmp::Eq => dynasm!(ops ; sete al),
        Cmp::Ne => dynasm!(ops ; setne al),
    }
    dynasm!(ops ; movzx Rq(d), al);
}

/// Box the i64 in xmm home `h` into a Value, leaving the bits in `rax`: Int-tag
/// if it fits i32 (low 32 masked in), else a double via `cvtsi2sd` (exact since
/// |x| ≤ 2^53, enforced by the per-op guard). Used by flush_exit.
pub(crate) fn emit_int_box_from_home(ops: &mut dynasmrt::x64::Assembler, h: u8) {
    let big = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; movq rax, Rx(h)
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

/// Restore xmm6..15 from the save area and the saved gprs, then `ret`.
pub(crate) fn emit_region_restore(ops: &mut dynasmrt::x64::Assembler) {
    emit_region_restore_n(ops, 0, 160);
}

/// Restore the xmm6..15 saves (from `[rsp + xmm_off + k*16]`), pop the saved gprs,
/// and `ret`. `frame` is the post-prologue rsp adjustment to undo (`160` for the
/// legacy layout, `200 + 32·n_ta` for the pinned-TypedArray layout whose snapshot
/// slots sit below the xmm saves).
pub(crate) fn emit_region_restore_n(ops: &mut dynasmrt::x64::Assembler, xmm_off: i32, frame: i32) {
    for k in 0..10u32 {
        let xi = 6 + k as u8;
        dynasm!(ops ; movdqu Rx(xi), [rsp + xmm_off + (k as i32) * 16]);
    }
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
}

/// Materialise a numeric constant (a `LoadInt`/`LoadConst` op) into a value's
/// xmm home. Shared by the prologue (for hoisted loop-invariants) and the body.
pub(crate) fn emit_load_const(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, instr: &Instr, proto: &FuncProto) {
    match *instr {
        Instr::LoadInt { dst, val } => {
            let h = xh(plan, dst);
            dynasm!(ops ; mov eax, val ; cvtsi2sd Rx(h), eax);
        }
        Instr::LoadConst { dst, idx } => {
            let h = xh(plan, dst);
            let v = proto.constants[idx as usize];
            if v.is_int() {
                let payload = v.bits() as u32 as i32;
                dynasm!(ops ; mov eax, payload ; cvtsi2sd Rx(h), eax);
            } else {
                dynasm!(ops ; mov rax, QWORD v.bits() as i64 ; movq Rx(h), rax);
            }
        }
        _ => unreachable!("emit_load_const on non-constant op"),
    }
}

/// Guard that the Value bits already in `rax` are a number and load them into
/// xmm home `home` as f64 (Int → cvtsi2sd; double → movq); else jump to `bail`.
/// Used only at region entry for live-in values (the loop body is guard-free).
pub(crate) fn emit_box_to_home(ops: &mut dynasmrt::x64::Assembler, home: u8, bail: dynasmrt::DynamicLabel) {
    let int_path = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; je => int_path
        ; sub r10d, (INT_TAG_HI + 1) as i32      // 0x7FFA (bool tag)
        ; cmp r10d, 3                            // high16 ∈ [0x7FFA,0x7FFD] ⇒ not a number
        ; jbe => bail
        ; movq Rx(home), rax
        ; jmp => done
        ; => int_path
        ; cvtsi2sd Rx(home), eax
        ; => done
    );
}

/// The xmm home index of numeric register `r` (panics only on an allocator bug).
pub(crate) fn xh(plan: &RegionPlan, r: u16) -> u8 {
    match plan.reg_home[&r] {
        Home::Xmm(x) => x,
        Home::Gpr(_) => unreachable!("numeric use of a bool-homed register"),
    }
}
/// The gpr home index of bool register `r`.
pub(crate) fn gh(plan: &RegionPlan, r: u16) -> u8 {
    match plan.reg_home[&r] {
        Home::Gpr(g) => g,
        Home::Xmm(_) => unreachable!("bool use of a number-homed register"),
    }
}
pub(crate) fn home(plan: &RegionPlan, r: u16) -> Home {
    plan.reg_home[&r]
}

/// Emit a register-to-register f64 binop into the dst home, handling aliasing.
pub(crate) fn emit_dbin(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, dst: u16, a: u16, b: u16, op: DOp, lc: &mut LastCopy) {
    let (d, ax, bx) = (xh(plan, dst), xh(plan, a), xh(plan, b));
    let commutative = matches!(op, DOp::Add | DOp::Mul);
    // Arrange operands so the accumulator is `d`. For non-commutative ops where
    // d == b (and d != a), use xmm0 as a temp to avoid clobbering b.
    if d == ax || copy_is_noop(*lc, d, ax) {
        emit_dop(ops, d, bx, op);
    } else if d == bx || (commutative && copy_is_noop(*lc, d, bx)) {
        if commutative {
            emit_dop(ops, d, ax, op); // d holds b; d = b op a == a op b
        } else {
            // movaps: full-register copies (rename-eliminated, no false dep).
            dynasm!(ops ; movaps xmm0, Rx(ax));
            emit_dop_xmm0(ops, bx, op); // xmm0 = a op b
            dynasm!(ops ; movaps Rx(d), xmm0);
        }
    } else {
        dynasm!(ops ; movaps Rx(d), Rx(ax));
        emit_dop(ops, d, bx, op);
    }
    copy_clobber(lc, d);
}

/// `xmm[d] <op>= xmm[src]`.
pub(crate) fn emit_dop(ops: &mut dynasmrt::x64::Assembler, d: u8, src: u8, op: DOp) {
    match op {
        DOp::Add => dynasm!(ops ; addsd Rx(d), Rx(src)),
        DOp::Sub => dynasm!(ops ; subsd Rx(d), Rx(src)),
        DOp::Mul => dynasm!(ops ; mulsd Rx(d), Rx(src)),
        DOp::Div => dynasm!(ops ; divsd Rx(d), Rx(src)),
    }
}
/// `xmm0 <op>= xmm[src]`.
pub(crate) fn emit_dop_xmm0(ops: &mut dynasmrt::x64::Assembler, src: u8, op: DOp) {
    match op {
        DOp::Add => dynasm!(ops ; addsd xmm0, Rx(src)),
        DOp::Sub => dynasm!(ops ; subsd xmm0, Rx(src)),
        DOp::Mul => dynasm!(ops ; mulsd xmm0, Rx(src)),
        DOp::Div => dynasm!(ops ; divsd xmm0, Rx(src)),
    }
}

/// Emit `bool_home[dst] = (a <cmp> b)` using f64 ordered comparison.
pub(crate) fn emit_dcmp(ops: &mut dynasmrt::x64::Assembler, plan: &RegionPlan, dst: u16, a: u16, b: u16, cmp: Cmp) {
    let (ax, bx) = (xh(plan, a), xh(plan, b));
    let d = gh(plan, dst);
    match cmp {
        Cmp::Lt => dynasm!(ops ; ucomisd Rx(bx), Rx(ax) ; seta al),
        Cmp::Le => dynasm!(ops ; ucomisd Rx(bx), Rx(ax) ; setae al),
        Cmp::Gt => dynasm!(ops ; ucomisd Rx(ax), Rx(bx) ; seta al),
        Cmp::Ge => dynasm!(ops ; ucomisd Rx(ax), Rx(bx) ; setae al),
        Cmp::Eq => dynasm!(ops ; ucomisd Rx(ax), Rx(bx) ; sete al ; setnp cl ; and al, cl),
        Cmp::Ne => dynasm!(ops ; ucomisd Rx(ax), Rx(bx) ; setne al ; setp cl ; or al, cl),
    }
    dynasm!(ops ; movzx Rq(d), al);
}

/// Re-derive the region's pinned heap pointers after a helper call that can
/// move them: r13 (heap version-array base — `heap.alloc` pushes to the
/// parallel versions Vec, which can reallocate) and, when `ic_base` is given,
/// r14 (JIT IC-table base — a NESTED region compile triggered by user code the
/// helper ran grows `ic_table`). rbx (register file) and r12 (globals) are
/// pinned to capacity for the VM's lifetime and never need re-deriving.
/// Clobbers only volatile registers — emit AFTER storing the helper's result.
pub(crate) fn emit_refetch_pinned(
    ops: &mut dynasmrt::x64::Assembler,
    versions_base: usize,
    ic_base: Option<usize>,
) {
    dynasm!(ops
        ; mov rcx, rdi
        ; mov rax, QWORD versions_base as i64
        ; call rax
        ; mov r13, rax
    );
    if let Some(icb) = ic_base {
        dynasm!(ops
            ; mov rcx, rdi
            ; mov rax, QWORD icb as i64
            ; call rax
            ; mov r14, rax
        );
    }
}

/// Byte offset (from the post-prologue rsp) of pinned-TypedArray snapshot slot
/// `j`: the frame reserves 32 bytes per pin ABOVE the 32B shadow space + 8B
/// 5th-arg slot. Layout within a slot: `obj_bits @0`, `base @8`, `len @16`
/// (8 bytes pad — keeps rsp 16-aligned for helper calls).
pub(crate) fn ta_slot_off(j: usize) -> i32 {
    40 + 32 * j as i32
}

/// (Re)derive every pinned TypedArray snapshot: re-read the live Value from its
/// source (global slot / frame register) and call `jit_ta_snapshot`, which
/// re-validates kind/detach/resize and writes `{obj_bits, base, len}` into the
/// pin's stack slot (`{0,0,0}` when ineligible — the per-access identity guard
/// then never matches and the access takes the generic-helper fallback).
/// Emitted in the prologue and AFTER every helper that can run user code
/// (which may detach/resize a buffer or reassign the source) — the same
/// discipline as the r13/r14 re-fetch. Clobbers only volatile registers.
pub(crate) fn emit_refetch_ta(ops: &mut dynasmrt::x64::Assembler, snapshot_helper: usize, plan: &TaPinPlan) {
    for (j, pin) in plan.pins.iter().enumerate() {
        match pin.src {
            TaPinSrc::Global(g) => dynasm!(ops ; mov rdx, [r12 + (g as i32) * 8]),
            TaPinSrc::Reg(r) => dynasm!(ops ; mov rdx, [rbx + dreg(r)]),
        }
        dynasm!(ops
            ; mov rcx, rdi                      // vm
            ; mov r8d, pin.kind as i32          // expected element kind
            ; lea r9, [rsp + ta_slot_off(j)]    // out: snapshot slot
            ; mov rax, QWORD snapshot_helper as i64
            ; call rax
        );
    }
}

/// Materialise a TypedArray element index from `regs[key]` into `rcx` (i64):
/// an Int tag sign-extends its payload; a double must be exactly integral
/// (cvttsd2si round-trip — NaN/±Inf/huge yield the 0x8000… sentinel, which
/// fails the round-trip) or the op DEOPTS; any other tag deopts. A negative or
/// huge index survives here and is caught by the caller's unsigned bounds
/// check (len < 2^31, so any negative i64 compares above it).
/// Clobbers rcx/r10/xmm0/xmm1.
pub(crate) fn emit_ta_key(ops: &mut dynasmrt::x64::Assembler, key: u16, bail: dynasmrt::DynamicLabel) {
    let key_dbl = ops.new_dynamic_label();
    let key_ok = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rcx, [rbx + dreg(key)]
        ; mov r10, rcx
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => key_dbl
        ; movsxd rcx, ecx                       // Int payload (may be negative)
        ; jmp => key_ok
        ; => key_dbl
        ; sub r10d, (INT_TAG_HI + 1) as i32
        ; cmp r10d, 3                           // Bool/Null/Undefined/Heap → deopt
        ; jbe => bail
        ; movq xmm0, rcx
        ; cvttsd2si rcx, xmm0                   // i64 trunc (NaN/±Inf → sentinel)
        ; cvtsi2sd xmm1, rcx
        ; ucomisd xmm1, xmm0
        ; jne => bail                           // fractional / sentinel
        ; jp => bail                            // NaN
        ; => key_ok
    );
}

/// Box the u32 in `eax` into `regs[dst]`: Int when it fits i32 (mirrors
/// `Value::num`'s narrowing), else the exact double (the `>>>` boxing pattern).
pub(crate) fn emit_box_u32(ops: &mut dynasmrt::x64::Assembler, dst: u16) {
    let as_dbl = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; test eax, eax
        ; js => as_dbl
    );
    box_eax(ops, dst);
    dynasm!(ops
        ; jmp => done
        ; => as_dbl
        ; mov eax, eax                // zero-extend u32
        ; cvtsi2sd xmm0, rax          // exact (< 2^32)
        ; movq rax, xmm0
        ; mov [rbx + dreg(dst)], rax
        ; => done
    );
}

/// Box the double in `xmm0` into `regs[dst]` EXACTLY as `Value::num` does:
/// an exact-integer in [i32::MIN, i32::MAX] (but NOT -0.0) narrows to an Int
/// tag; NaN canonicalises to the QNAN double; everything else (incl. -0.0,
/// ±Inf, non-integral, out-of-range integers) stays the raw f64 bits. Used for
/// `MathOp` results, whose interpreter arm stores `Value::num(r)` — so a
/// `Math.floor(x)===3` downstream bits-compare against Int(3) matches.
/// Clobbers rax/rcx/r10/xmm1.
pub(crate) fn emit_box_num(ops: &mut dynasmrt::x64::Assembler, dst: u16) {
    let as_dbl = ops.new_dynamic_label();
    let store_int = ops.new_dynamic_label();
    let canon = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    // Every path computes the final boxed bits into `rax`, then falls through to
    // the single store at `done`.
    dynasm!(ops
        // Truncate-to-i32; cvttsd2si yields 0x8000_0000 for NaN / |x|>=2^31 /
        // ±Inf — those all fail the exact round-trip below, so they fall to the
        // double path. (i32::MIN itself round-trips and narrows correctly.)
        ; cvttsd2si ecx, xmm0
        ; xorps xmm1, xmm1
        ; cvtsi2sd xmm1, ecx               // back to f64 (exact for any i32)
        ; ucomisd xmm1, xmm0
        ; jp => as_dbl                     // NaN operand → not integral
        ; jne => as_dbl                    // non-integral / out-of-range → double
        // Integral and in i32 range. Reject -0.0 (its int form 0 loses the sign):
        // -0.0 narrows to ecx==0 but has the original sign bit set.
        ; test ecx, ecx
        ; jnz => store_int                 // non-zero int: narrows
        ; movq rax, xmm0                   // zero: inspect the original sign bit
        ; bt rax, 63
        ; jc => as_dbl                     // -0.0 → keep as double
        ; => store_int
        ; mov eax, ecx                     // zero-extend the i32 payload
        ; mov r10, QWORD INT_TAG as i64
        ; or rax, r10                      // rax = INT_TAG | (payload as u32)
        ; jmp => done
        ; => as_dbl
        ; ucomisd xmm0, xmm0
        ; jp => canon                      // NaN → canonical QNAN
        ; movq rax, xmm0                   // finite/±Inf/-0 → raw f64 bits
        ; jmp => done
        ; => canon
        ; mov rax, QWORD QNAN_BITS as i64
        ; => done
        ; mov [rbx + dreg(dst)], rax
    );
}

/// Box the double in `xmm0` into `regs[dst]`, CANONICALISING any NaN — raw
/// TypedArray/DataView bytes could otherwise alias a NaN-box tag (heap-index
/// forgery). Not int-narrowed (the f64 mem tier's established representation).
pub(crate) fn emit_box_f64_canon(ops: &mut dynasmrt::x64::Assembler, dst: u16) {
    let canon = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; ucomisd xmm0, xmm0
        ; jp => canon                 // NaN → canonical
        ; movq rax, xmm0
        ; jmp => done
        ; => canon
        ; mov rax, QWORD QNAN_BITS as i64
        ; => done
        ; mov [rbx + dreg(dst)], rax
    );
}

