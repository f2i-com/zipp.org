// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// Resolve a jump `target` to a label: an in-region ip uses its own label; an
/// out-of-region ip gets (or reuses) an exit stub label.
pub(crate) fn region_target(
    target: u32,
    start: u32,
    end: u32,
    in_region: &[dynasmrt::DynamicLabel],
    exit_stubs: &mut FxHashMap<u32, dynasmrt::DynamicLabel>,
    ops: &mut dynasmrt::x64::Assembler,
) -> dynasmrt::DynamicLabel {
    if target >= start && target <= end {
        in_region[(target - start) as usize]
    } else {
        *exit_stubs.entry(target).or_insert_with(|| ops.new_dynamic_label())
    }
}

#[derive(Clone, Copy)]
pub(crate) enum DOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// Load `regs[reg]` as ToInt32 into `eax` for the region's `Bitwise` ops: an
/// Int-tagged payload directly, or a DOUBLE that is exactly integral in
/// (-2^63, 2^63) — for which ToInt32 is simply the low 32 bits of the i64
/// (modulo-2^32 wrap, signed), exactly what `(x + y) | 0` accumulators rely on
/// when the f64 sum crosses i32 range. Anything else — fractional / NaN / Inf /
/// |x| ≥ 2^63 (rare; ToInt32 still defined but not via i64) / bool / null /
/// undefined / heap — jumps to `bail` so the interpreter applies complete
/// ToInt32 semantics. Clobbers rax/r10/xmm0/xmm1.
pub(crate) fn load_toint32(ops: &mut dynasmrt::x64::Assembler, reg: u16, bail: dynasmrt::DynamicLabel) {
    let int_path = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(reg)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; je => int_path
        ; sub r10d, (INT_TAG_HI + 1) as i32      // 0x7FFA (bool tag)
        ; cmp r10d, 3                            // high16 ∈ [0x7FFA,0x7FFD] ⇒ not a number
        ; jbe => bail
        // A double. ToInt32 TRUNCATES toward zero, so a fractional value is
        // perfectly representable — this used to demand an exactly-integral
        // double and bail otherwise, which sent the single most common
        // truncation idiom in JS (`(x * k) | 0`) to the interpreter on every
        // iteration: 127ms vs 15ms per 3M ops, against node's 3ms.
        //
        // Truncate to i64 and take the low 32 bits. That IS ToInt32 for every
        // |x| < 2^63: truncation toward zero followed by modulo 2^32, which is
        // exactly what discarding the high half does — for fractional values
        // (3.7 -> 3) and for large ones alike (5e9 -> 705032704, 2^31 ->
        // -2147483648, 2^32 -> 0).
        //
        // The one case that must bail is `cvttsd2si` OVERFLOWING, which it
        // signals with the 0x8000_0000_0000_0000 indefinite: NaN, ±Inf, and
        // |x| >= 2^63. Their low 32 bits would be 0, which is right for NaN/±Inf
        // by luck but wrong for e.g. 1e21 (-559939584), so all three go to the
        // interpreter. A true -2^63 is folded in with them; harmless, it is the
        // same value the sentinel denotes.
        ; movq xmm0, rax
        ; cvttsd2si rax, xmm0                    // i64 trunc toward zero
        ; mov r10, QWORD i64::MIN
        ; cmp rax, r10
        ; je => bail                             // NaN / ±Inf / |x| >= 2^63
        ; jmp => done
        ; => int_path
        // eax already holds the i32 payload (low 32 of the boxed Value).
        ; => done
    );
}

/// Load `regs[reg]` as an f64 into `xmm{which}` (0 or 1). Int-tagged → cvtsi2sd;
/// a real double → movq; bool/null/undef/heap → jump to `bail`.
pub(crate) fn load_num_xmm(ops: &mut dynasmrt::x64::Assembler, reg: u16, which: u8, bail: dynasmrt::DynamicLabel) {
    let int_path = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(reg)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; je => int_path
        ; sub r10d, (INT_TAG_HI + 1) as i32      // 0x7FFA (bool tag)
        ; cmp r10d, 3                            // high16 ∈ [0x7FFA, 0x7FFD] ⇒ not a number
        ; jbe => bail
        ; movq Rx(which), rax                    // double: raw f64 bits
        ; jmp => done
        ; => int_path
        ; xorps Rx(which), Rx(which)             // break cvtsi2sd's false dep
        ; cvtsi2sd Rx(which), eax                 // int: low-32 i32 payload
        ; => done
    );
}

/// Store `xmm0` (an f64 result) into `regs[dst]` as a double `Value`.
pub(crate) fn store_xmm(ops: &mut dynasmrt::x64::Assembler, dst: u16) {
    dynasm!(ops
        ; movq rax, xmm0
        ; mov [rbx + dreg(dst)], rax
    );
}

/// `regs[dst] = Math.<op>(args…)` for the mem paths. Operands are loaded as
/// numbers (Int/double); a non-numeric operand BAILS to the interpreter, which
/// runs the full ToNumber coercion (a user `valueOf`). So the helpers here never
/// run user code and never allocate — no r13/r14/TA refetch is owed. The result
/// is boxed by `emit_box_num`, which mirrors the interpreter's `Value::num(r)`
/// exactly (exact-int narrows, `-0`/NaN preserved).
///
/// SHARED by the region path (Tier B) and the whole-function path (Tier C).
/// It lived only in `region_mem.rs`, so Tier C rejected every `Math.*` call and
/// blacklisted the containing function for the whole run — which cost
/// markdown-render, parse-large-js and json-large one function each. Copying it
/// would have widened the divergence PERF_ROADMAP B43 already warns about
/// ("the emitter exists in TWO byte-identical copies — factor before editing"),
/// so it is factored here instead.
///
/// The caller must have gated the op/arity set (`argc == 1` and not `Imul`, or
/// `argc == 2` and one of Pow/Atan2/Imul/Min/Max/Hypot) — the arms below assume
/// it, exactly as the region admission check guarantees.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_math_op(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    dst: u16,
    op: MathFn,
    arg_base: u16,
    argc: u16,
    math_unary: usize,
    math_two: usize,
) {
    if argc == 1 {
        load_num_xmm(ops, arg_base, 0, bail);
        dynasm!(ops
            ; movq rdx, xmm0                  // arg f64 bits (arg1)
            ; mov ecx, op as i32              // MathFn code (repr(u8), arg0)
            ; mov rax, QWORD math_unary as i64
            ; call rax
            ; movq xmm0, rax                  // result f64 bits
        );
        emit_box_num(ops, dst);
    } else if matches!(op, MathFn::Imul) {
        // `Math.imul(a,b)` INLINE — a 32-bit signed multiply, no FFI: ToInt32
        // both operands (a non-int-coercible operand BAILS, so the interpreter
        // runs the full ToNumber incl. a user valueOf — matching the helper),
        // then `imul` (low 32 bits, signed) boxed as Int. The low 32 bits of the
        // product are identical whether the inputs were ToInt32 or ToUint32, so
        // this equals the interpreter's `math_two(Imul)` exactly.
        load_toint32(ops, arg_base, bail);
        dynasm!(ops ; mov r8d, eax);
        load_toint32(ops, arg_base + 1, bail);
        dynasm!(ops ; mov ecx, eax ; mov eax, r8d ; imul eax, ecx);
        box_eax(ops, dst);
    } else {
        // EXACTLY two args (the admission check gated the op set).
        load_num_xmm(ops, arg_base, 0, bail);
        load_num_xmm(ops, arg_base + 1, 1, bail);
        dynasm!(ops
            ; movq rdx, xmm0                  // arg0 f64 bits (arg1)
            ; movq r8, xmm1                   // arg1 f64 bits (arg2)
            ; mov ecx, op as i32              // MathFn code (arg0)
            ; mov rax, QWORD math_two as i64
            ; call rax
            ; movq xmm0, rax
        );
        emit_box_num(ops, dst);
    }
    emit_region_bail(ops, ip, bail, epilogue);
}

/// The op/arity subset `emit_math_op` implements. Shared by both mem paths'
/// admission checks so they cannot drift apart again.
///
/// `Math.imul(x)` (ONE arg) diverges and is excluded: the unary helper returns
/// NaN, but the interpreter coerces the missing 2nd arg to `to_uint32(NaN) == 0`
/// and yields 0. Every other unary op agrees at argc == 1.
pub(crate) fn math_op_emittable(op: MathFn, argc: u16) -> bool {
    match argc {
        1 => !matches!(op, MathFn::Imul),
        2 => matches!(
            op,
            MathFn::Pow | MathFn::Atan2 | MathFn::Imul | MathFn::Min | MathFn::Max | MathFn::Hypot
        ),
        _ => false,
    }
}

/// `regs[dst] = regs[a] <op> regs[b]`. Add/Sub/Mul take an INT fast path when
/// both operands are Int-tagged (32-bit op + overflow check, result boxed Int —
/// exactly the interpreter's `checked_add/sub/mul` fast path), falling to the
/// f64 path on a non-Int operand or overflow. Keeping Int results Int matters
/// downstream: `(x+y)|0` accumulators and `a[i+1]` keys then take their cheap
/// Int paths instead of the double→int round-trip. Div is always f64 (JS `/`
/// has no integer form — mirrors the interpreter). Guards operands are numbers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn dbinop(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
    op: DOp,
    int_hint: bool,
) {
    let f64_path = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    // Mul is EXCLUDED from the int fast path: hot integer multiplies (hash
    // mixing `i * 40503`) overflow i32 after a few thousand iterations and
    // would then pay the failed int attempt PLUS the f64 redo every time.
    let int_ok = int_hint && matches!(op, DOp::Add | DOp::Sub);
    if int_ok {
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
        );
        match op {
            DOp::Add => dynasm!(ops ; add eax, ecx ; jo => f64_path),
            DOp::Sub => dynasm!(ops ; sub eax, ecx ; jo => f64_path),
            DOp::Mul => dynasm!(ops ; imul eax, ecx ; jo => f64_path),
            DOp::Div => unreachable!(),
        }
        box_eax(ops, dst);
        // f64 fallback re-loads both operands from the register file, so the
        // clobbered eax (wrapped overflow value) is irrelevant.
        dynasm!(ops ; jmp => done ; => f64_path);
    }
    load_num_xmm(ops, a, 0, bail);
    load_num_xmm(ops, b, 1, bail);
    match op {
        DOp::Add => dynasm!(ops ; addsd xmm0, xmm1),
        DOp::Sub => dynasm!(ops ; subsd xmm0, xmm1),
        DOp::Mul => dynasm!(ops ; mulsd xmm0, xmm1),
        DOp::Div => dynasm!(ops ; divsd xmm0, xmm1),
    }
    store_xmm(ops, dst);
    if int_ok {
        dynasm!(ops ; => done);
    }
    emit_region_bail(ops, ip, bail, epilogue);
}

/// `regs[dst] = (regs[a] <cmp> regs[b]) as Bool` using f64 ordered comparison
/// (NaN compares false for </<=/>/>=/==, true for !=). Guards both are numbers.
#[allow(clippy::too_many_arguments)]
/// If `c` is a "pending string" constant (`Value::heap(STRING_CONST_BIT | si)`,
/// the form the compiler emits for a string literal) whose text is exactly ONE
/// ASCII byte, return that char's INTERNED Value bits (`Value::heap(byte)` —
/// single ASCII chars live at heap index == their byte; see `Heap::new`). This
/// lets the region materialise `"7"` as the same boxed value `s[i]` yields, so
/// `s[i] === "7"` is a bits compare. Returns `None` for numeric / multi-char /
/// non-ASCII / non-string constants (the region handles numbers; others decline).
/// Element-kind id for a whitelisted DataView `get*` method name (the kinds the
/// `jit_dv_get` helper decodes without allocating). `None` for everything else
/// (set*, BigInt64/BigUint64 and Float16 getters stay on the generic path).
pub fn dv_get_kind(key: &str) -> Option<u8> {
    match key {
        "getInt8" => Some(0),
        "getUint8" => Some(1),
        "getInt16" => Some(3),
        "getUint16" => Some(4),
        "getInt32" => Some(5),
        "getUint32" => Some(6),
        "getFloat32" => Some(7),
        "getFloat64" => Some(8),
        _ => None,
    }
}

pub(crate) fn single_char_const_bits(proto: &FuncProto, c: Value) -> Option<u64> {
    if !c.is_heap() {
        return None;
    }
    let raw = c.heap_index();
    if raw & crate::vm::STRING_CONST_BIT == 0 {
        return None; // a real heap value, not a pending string constant
    }
    let si = (raw & !crate::vm::STRING_CONST_BIT) as usize;
    let bytes = proto.string_constants.get(si)?.as_bytes();
    if bytes.len() == 1 && bytes[0] < 128 {
        Some(Value::heap(bytes[0] as u32).bits())
    } else {
        None
    }
}

/// Polymorphic strict `===` / `!==` (`ne` selects `!==`) for the region's MEMORY
/// path. Operand types are unknown at compile time, so the emitted code branches
/// at runtime:
///   1. EITHER operand is a DOUBLE (NaN-box high16 ∉ [TAG_LO, TAG_HI]) → the f64
///      numeric compare (identical to `dcmp` Eq/Ne) — keeps `0.5===0.5`,
///      `NaN!==NaN`, `0===-0` correct, and bails on a num-vs-non-num operand mix.
///   2. else EITHER operand is HEAP (high16 == 0x7FFD) with index ≥ 129 (a
///      multi-char string or a user object — NOT an interned single-char/empty
///      string) → the read-only `jit_strict_eq` helper (full `strict_eq`
///      semantics: equal-content strings, BigInts, identity for objects) —
///      `line === "##"` scans stay native instead of deopting.
///   3. else → 64-bit BITS equality. Exactly JS `===` for Int, Bool, Null,
///      Undefined, and interned single-char/empty strings (indices < 129). This
///      is the `s[i] === "7"` and `charCodeAt === 55` hot path (call-free).
#[allow(clippy::too_many_arguments)]
pub(crate) fn region_poly_eq(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
    ne: bool,
    strict_eq_helper: usize,
) {
    let numeric = ops.new_dynamic_label();
    let a_not_heap = ops.new_dynamic_label();
    let do_bits = ops.new_dynamic_label();
    let store = ops.new_dynamic_label();
    let slow = ops.new_dynamic_label();
    let after = ops.new_dynamic_label();
    // rax = a_bits, rcx = b_bits (kept live across the type checks).
    dynasm!(ops
        ; mov rax, [rbx + dreg(a)]
        ; mov rcx, [rbx + dreg(b)]
        // is a a double?  high16 = rax>>48; double ⇔ (high16 - TAG_LO) > (TAG_HI-TAG_LO)
        ; mov rdx, rax
        ; shr rdx, 48
        ; sub edx, TAG_LO as i32
        ; cmp edx, (TAG_HI - TAG_LO) as i32
        ; ja => numeric                       // a is a double (tag out of tagged range)
        // is b a double?
        ; mov rdx, rcx
        ; shr rdx, 48
        ; sub edx, TAG_LO as i32
        ; cmp edx, (TAG_HI - TAG_LO) as i32
        ; ja => numeric                       // b is a double
        // Neither is a double. Bail if EITHER is a heap value with index ≥ 129
        // (a non-interned string / object — needs full strict_eq).
        ; mov rdx, rax
        ; shr rdx, 48
        ; cmp edx, TAG_HEAP_HI as i32
        ; jne => a_not_heap
        ; mov rdx, rax
        ; mov r9, QWORD PAYLOAD_MASK as i64
        ; and rdx, r9
        ; cmp rdx, USER_OBJ_START as i32
        ; jae => slow                          // a: non-interned heap → helper
        ; => a_not_heap
        ; mov rdx, rcx
        ; shr rdx, 48
        ; cmp edx, TAG_HEAP_HI as i32
        ; jne => do_bits
        ; mov rdx, rcx
        ; mov r9, QWORD PAYLOAD_MASK as i64
        ; and rdx, r9
        ; cmp rdx, USER_OBJ_START as i32
        ; jae => slow                          // b: non-interned heap → helper
        ; jmp => do_bits
    );
    // ── numeric path (a or b is a double): f64 compare, identical to dcmp. ──
    dynasm!(ops ; => numeric);
    load_num_xmm(ops, a, 0, bail);
    load_num_xmm(ops, b, 1, bail);
    if ne {
        dynasm!(ops ; ucomisd xmm0, xmm1 ; setne al ; setp cl ; or al, cl);
    } else {
        dynasm!(ops ; ucomisd xmm0, xmm1 ; sete al ; setnp cl ; and al, cl);
    }
    dynasm!(ops ; jmp => store);
    // ── bits path: result = (a_bits <op> b_bits) as Bool. ──
    dynasm!(ops
        ; => do_bits
        ; mov rax, [rbx + dreg(a)]
        ; mov rcx, [rbx + dreg(b)]
        ; cmp rax, rcx
    );
    if ne {
        dynasm!(ops ; setne al);
    } else {
        dynasm!(ops ; sete al);
    }
    dynasm!(ops
        ; => store
        ; movzx rax, al
        ; mov r8, QWORD BOOL_TAG as i64
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
        ; jmp => after
        // ── slow path: full strict_eq via the read-only helper. ──
        ; => slow
        ; mov rcx, rdi                         // vm
        ; mov rdx, [rbx + dreg(a)]
        ; mov r8, [rbx + dreg(b)]
        ; mov rax, QWORD strict_eq_helper as i64
        ; call rax                             // rax = 0/1 (a === b)
    );
    if ne {
        dynasm!(ops ; xor rax, 1);
    }
    dynasm!(ops
        ; mov r8, QWORD BOOL_TAG as i64
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
        ; => after
    );
    emit_region_bail(ops, ip, bail, epilogue);
}

pub(crate) fn dcmp(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    dst: u16,
    a: u16,
    b: u16,
    cmp: Cmp,
) {
    load_num_xmm(ops, a, 0, bail);
    load_num_xmm(ops, b, 1, bail);
    // Compute the boolean into al. ucomisd sets CF/ZF/PF; ordering tricks below
    // keep NaN-false semantics (see jump variant for the rationale).
    match cmp {
        Cmp::Lt => dynasm!(ops ; ucomisd xmm1, xmm0 ; seta al),   // a<b  ⇔ b>a ordered
        Cmp::Le => dynasm!(ops ; ucomisd xmm1, xmm0 ; setae al),  // a<=b ⇔ b>=a ordered
        Cmp::Gt => dynasm!(ops ; ucomisd xmm0, xmm1 ; seta al),   // a>b
        Cmp::Ge => dynasm!(ops ; ucomisd xmm0, xmm1 ; setae al),  // a>=b
        Cmp::Eq => dynasm!(ops
            ; ucomisd xmm0, xmm1
            ; sete al            // ZF=1 (equal OR unordered)
            ; setnp cl           // PF=0 (ordered)
            ; and al, cl         // equal AND ordered
        ),
        Cmp::Ne => dynasm!(ops
            ; ucomisd xmm0, xmm1
            ; setne al           // ZF=0 (a≠b)
            ; setp cl            // PF=1 (unordered)
            ; or al, cl          // a≠b OR NaN
        ),
    }
    dynasm!(ops
        ; movzx rax, al
        ; mov r8, QWORD BOOL_TAG as i64
        ; or rax, r8
        ; mov [rbx + dreg(dst)], rax
    );
    emit_region_bail(ops, ip, bail, epilogue);
}

/// Fused `if !(regs[a] <cmp> regs[b]) goto target` in f64. Guards both numbers.
/// Only Lt/Le are emitted by the compiler (loop guards).
#[allow(clippy::too_many_arguments)]
pub(crate) fn djump_if_not_cmp(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
    a: u16,
    b: u16,
    cmp: Cmp,
    target: dynasmrt::DynamicLabel,
) {
    load_num_xmm(ops, a, 0, bail);
    load_num_xmm(ops, b, 1, bail);
    // Jump when the comparison is FALSE. ucomisd(b,a): CF=1 ⇔ b<a OR unordered.
    match cmp {
        // !(a<b): b<=a or NaN. ucomisd(b,a) then jbe (CF|ZF). NaN sets CF ⇒ jumps.
        Cmp::Lt => dynasm!(ops ; ucomisd xmm1, xmm0 ; jbe => target),
        // !(a<=b): b<a or NaN. ucomisd(b,a) then jb (CF). NaN sets CF ⇒ jumps.
        Cmp::Le => dynasm!(ops ; ucomisd xmm1, xmm0 ; jb => target),
        _ => {}
    }
    emit_region_bail(ops, ip, bail, epilogue);
}

/// Emit a region op's bail block: the success path skips it; the block records
/// the resume ip into `[rsi]` and jumps to the shared epilogue (which restores
/// the 4-push/40-byte frame). Unlike the function JIT no result is set — a
/// region's `run` ignores rax and reads only the resume ip.
pub(crate) fn emit_region_bail(
    ops: &mut dynasmrt::x64::Assembler,
    ip: usize,
    bail: dynasmrt::DynamicLabel,
    epilogue: dynasmrt::DynamicLabel,
) {
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; jmp => done
        ; => bail
        ; mov DWORD [rsi], ip as i32
        ; jmp => epilogue
        ; => done
    );
}
