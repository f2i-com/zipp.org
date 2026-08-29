// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

/// `ZIPP_NO_POLYEQ_FAST=1` restores the pre-B88 `region_poly_eq`, where either
/// operand being a double jumped to the numeric path unconditionally — and that
/// path bails for a tagged non-number, so `x !== undefined` deopted the region
/// on every iteration whenever `x` held a double. Read once per region compile.
#[inline]
fn poly_eq_fast_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_POLYEQ_FAST").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `ZIPP_NO_MEM_CMPJUMP=1` disables the memory-path fused compare→branch head
/// (`emit_fused_cmp_branch_head`), restoring the unfused compare + JumpIf pair
/// byte-identically. Read once per region compile.
#[inline]
pub(crate) fn mem_cmp_fuse_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_MEM_CMPJUMP").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Fast Int/Int head for the memory-path fused relational jumps. The generic
/// path below has to convert both numeric operands to f64 for mixed Int/double
/// semantics; two tagged Ints can compare their signed i32 payloads directly.
/// `ZIPP_NO_MEM_INT_CMPJUMP=1` restores the conversion-only emission.
#[inline]
fn mem_int_cmpjump_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_MEM_INT_CMPJUMP").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Fused compare→branch fast head for the MEMORY path (B118): when a
/// `Lt/Le/Gt/Ge/Eq/Ne {dst,a,b}` is IMMEDIATELY followed by a
/// `JumpIfTrue/False{cond: dst}`, two Int-tagged operands compare and branch on
/// FLAGS — skipping the boxed-bool store→load round trip and the generic
/// JumpIf's tag dispatch. That serial `setcc → box → store → reload → test`
/// chain was the dominant per-character cost of the `parse-large-js` tokenize
/// loop (a charCodeAt scanner is Eq+JumpIf chains over Int char codes).
///
/// The boolean is STILL stored to `dst` before branching: a chained `a || b`
/// condition jumps straight to the JumpIf ip from an earlier arm (so the pair's
/// second op must stay emitted and reachable — the caller keeps it), and the
/// deopt contract wants the register file exact at every ip boundary.
///
/// Non-Int operands fall through to the UNCHANGED generic compare sequence the
/// caller emits right after this head (then into the still-emitted JumpIf).
/// No calls, no bails, no allocation — safe-point schedule unchanged.
/// Off switch: `ZIPP_NO_MEM_CMPJUMP=1` (`mem_cmp_fuse_enabled`).
pub(crate) fn emit_fused_cmp_branch_head(
    ops: &mut dynasmrt::x64::Assembler,
    dst: u16,
    a: u16,
    b: u16,
    cmp: Cmp,
    if_false: bool,
    target: dynasmrt::DynamicLabel,
    fallthrough: dynasmrt::DynamicLabel,
) {
    let generic = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(a)]
        ; mov rcx, [rbx + dreg(b)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => generic                      // a not Int → generic pair
        ; mov r10, rcx
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => generic                      // b not Int → generic pair
        ; cmp eax, ecx                        // signed i32 payload compare
    );
    // setcc preserves flags; the branch below re-tests dl (the `or` boxing the
    // Bool clobbers flags in between).
    match cmp {
        Cmp::Eq => dynasm!(ops ; sete dl),
        Cmp::Ne => dynasm!(ops ; setne dl),
        Cmp::Lt => dynasm!(ops ; setl dl),
        Cmp::Le => dynasm!(ops ; setle dl),
        Cmp::Gt => dynasm!(ops ; setg dl),
        Cmp::Ge => dynasm!(ops ; setge dl),
    }
    dynasm!(ops
        ; movzx edx, dl
        ; mov r10, QWORD BOOL_TAG as i64
        ; or r10, rdx
        ; mov [rbx + dreg(dst)], r10          // dst gets the Bool Value regardless
        ; test dl, dl
    );
    if if_false {
        dynasm!(ops ; jz => target);
    } else {
        dynasm!(ops ; jnz => target);
    }
    dynasm!(ops
        ; jmp => fallthrough                  // not taken → skip the JumpIf ip
        ; => generic
    );
}

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
        *exit_stubs
            .entry(target)
            .or_insert_with(|| ops.new_dynamic_label())
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
pub(crate) fn load_toint32(
    ops: &mut dynasmrt::x64::Assembler,
    reg: u16,
    bail: dynasmrt::DynamicLabel,
) {
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
pub(crate) fn load_num_xmm(
    ops: &mut dynasmrt::x64::Assembler,
    reg: u16,
    which: u8,
    bail: dynasmrt::DynamicLabel,
) {
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

/// Validate the exact callable reference consumed by a JIT `MathOp`.
///
/// The `Math.imul` specialization replaces only the allocation-free Rust
/// identity helper. Its source `LoadGlobal` and `GetProp` have already executed
/// at this point on every activation, as have all arguments. Exact receiver and
/// callee compares therefore preserve lookup/accessor/Proxy effects and
/// replacement during argument evaluation. The heap generation compare closes
/// the remaining GC slot-reuse ABA hole. Any mismatch bails at the MathOp, whose
/// interpreter arm calls the already-captured callee with the captured receiver.
pub(crate) fn emit_math_identity_guard(
    ops: &mut dynasmrt::x64::Assembler,
    op: MathFn,
    callee: u16,
    this_v: u16,
    bail: dynasmrt::DynamicLabel,
    imul_guard: Option<MathIntrinsicGuard>,
) {
    // Every op whose intrinsic callable was baked at compile time takes the
    // same three-compare guard `Math.imul` always had (the `imul` entry of the
    // table equals `callee_bits`/`callee_ver`); an op that was not a plain
    // intrinsic data property then keeps the helper-call guard below.
    if let Some(guard) = imul_guard {
        let (callee_bits, callee_ver) = (
            guard.op_callee_bits[op as usize],
            guard.op_callee_ver[op as usize],
        );
        if callee_bits != 0 {
            dynasm!(ops
                ; mov rax, [rbx + dreg(this_v)]
                ; mov r10, QWORD guard.receiver_bits as i64
                ; cmp rax, r10
                ; jne => bail
                ; mov rax, [rbx + dreg(callee)]
                ; mov r10, QWORD callee_bits as i64
                ; cmp rax, r10
                ; jne => bail
                // Exact heap bits make eax the validated live slot index.
                ; mov ecx, eax
                ; cmp DWORD [r13 + rcx * 4], callee_ver as i32
                ; jne => bail
            );
            return;
        }
    }

    let identity = crate::vm::jit_math_is_intrinsic as usize;
    dynasm!(ops
        ; mov rcx, rdi
        ; mov edx, op as i32
        ; mov r8, [rbx + dreg(callee)]
        ; mov r9, [rbx + dreg(this_v)]
        ; mov rax, QWORD identity as i64
        ; call rax
        ; test rax, rax
        ; jz => bail
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
    callee: u16,
    this_v: u16,
    arg_base: u16,
    argc: u16,
    math_unary: usize,
    math_two: usize,
    math_imul_guard: Option<MathIntrinsicGuard>,
) {
    emit_math_identity_guard(ops, op, callee, this_v, bail, math_imul_guard);
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
///   2. else EITHER operand is HEAP (high16 == 0x7FFD) with index at/above
///      USER_OBJ_START (a dynamic string or user object — NOT an immutable
///      prefix string) → the read-only `jit_strict_eq` helper (full `strict_eq`
///      semantics: equal-content strings, BigInts, identity for objects) —
///      `line === "##"` scans stay native instead of deopting.
///   3. else → 64-bit BITS equality. Exactly JS `===` for Int, Bool, Null,
///      Undefined, and immutable prefix strings (indices < USER_OBJ_START). This
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
    // B88: reached when exactly ONE operand is a double and the other is a
    // TAGGED non-number (undefined / null / bool / heap). `===` is false and
    // `!==` is true for every such pair, by definition — a Number is never
    // SameValueZero with a non-Number — so the answer is a constant and the
    // region does not have to leave.
    let definitely_ne = ops.new_dynamic_label();
    let a_is_dbl = ops.new_dynamic_label();
    let b_is_dbl = ops.new_dynamic_label();
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
        ; ja => a_is_dbl                      // a is a double (tag out of tagged range)
        // is b a double?
        ; mov rdx, rcx
        ; shr rdx, 48
        ; sub edx, TAG_LO as i32
        ; cmp edx, (TAG_HI - TAG_LO) as i32
        ; ja => b_is_dbl                      // b is a double, a is tagged
        // Neither is a double. Bail if EITHER is a heap value outside the
        // immutable prefix (a dynamic string / object — needs full strict_eq).
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
    // ── one operand is a double ──────────────────────────────────────────────
    // The numeric path below calls `load_num_xmm` on BOTH operands, and that
    // bails for any tagged non-number. Jumping there on "a is a double" alone —
    // which is what this did — meant `x === undefined` / `!== null` / `!== "s"`
    // deopted the region on EVERY iteration whenever `x` held a double. Measured
    // in isolation: an Int-valued arm 9ms against a double-valued one 62ms
    // (6.9x, node 2ms for both), with 64 deopts and then eviction on the double
    // arm only — so the whole enclosing loop fell back to the interpreter
    // permanently. It is what killed `map-set-heavy`'s 400,000-iteration lookup
    // loop, where `m.get(k) !== undefined` reads a double because the map was
    // filled with `i * 2 + 1`.
    //
    // Only a NUMBER can compare equal to a double, so the other operand needs a
    // real f64 compare when it is a double or an Int, and has a constant answer
    // otherwise.
    if poly_eq_fast_enabled() {
        dynasm!(ops
            ; => a_is_dbl
            ; mov rdx, rcx
            ; shr rdx, 48
            ; sub edx, TAG_LO as i32
            ; cmp edx, (TAG_HI - TAG_LO) as i32
            ; ja => numeric                       // b is a double too
            ; mov rdx, rcx
            ; shr rdx, 48
            ; cmp edx, INT_TAG_HI as i32
            ; je => numeric                       // b is an Int: 1.0 === 1 is true
            ; jmp => definitely_ne                // b is undefined/null/bool/heap
            ; => b_is_dbl
            ; mov rdx, rax
            ; shr rdx, 48
            ; cmp edx, INT_TAG_HI as i32
            ; je => numeric                       // a is an Int
            ; jmp => definitely_ne                // a is undefined/null/bool/heap
            ; => definitely_ne
            ; mov al, ne as i8                    // `!==` → true, `===` → false
            ; jmp => store
        );
    } else {
        // `ZIPP_NO_POLYEQ_FAST=1`: the pre-B88 shape — either operand being a
        // double goes straight to the numeric path, which bails on a tagged
        // non-number. Kept so the change is A/B-able on ONE binary.
        dynasm!(ops ; => a_is_dbl ; jmp => numeric ; => b_is_dbl ; jmp => numeric ; => definitely_ne ; jmp => numeric);
    }
    // ── numeric path (both operands numeric): f64 compare, identical to dcmp. ──
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
        Cmp::Lt => dynasm!(ops ; ucomisd xmm1, xmm0 ; seta al), // a<b  ⇔ b>a ordered
        Cmp::Le => dynasm!(ops ; ucomisd xmm1, xmm0 ; setae al), // a<=b ⇔ b>=a ordered
        Cmp::Gt => dynasm!(ops ; ucomisd xmm0, xmm1 ; seta al), // a>b
        Cmp::Ge => dynasm!(ops ; ucomisd xmm0, xmm1 ; setae al), // a>=b
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
    let generic = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    if mem_int_cmpjump_enabled() {
        // Mirror `emit_fused_cmp_branch_head`'s representation proof: an Int
        // has high16 == INT_TAG_HI and its low32 is the exact signed payload.
        // The not-taken arm jumps over the generic f64 conversion below; any
        // mixed/double/non-number input reaches that old path byte-for-byte.
        dynasm!(ops
            ; mov rax, [rbx + dreg(a)]
            ; mov rcx, [rbx + dreg(b)]
            ; mov r10, rax
            ; shr r10, 48
            ; cmp r10d, INT_TAG_HI as i32
            ; jne => generic
            ; mov r10, rcx
            ; shr r10, 48
            ; cmp r10d, INT_TAG_HI as i32
            ; jne => generic
            ; cmp eax, ecx                    // signed i32 payload comparison
        );
        match cmp {
            Cmp::Lt => dynasm!(ops ; jge => target), // !(a < b)
            Cmp::Le => dynasm!(ops ; jg => target),  // !(a <= b)
            _ => {}
        }
        dynasm!(ops
            ; jmp => done
            ; => generic
        );
    } else {
        // Keep the off-switch stream label-complete without emitting a branch
        // or any fast-head instructions.
        dynasm!(ops ; => generic);
    }
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
    dynasm!(ops ; => done);
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
