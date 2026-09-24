//! The JIT's Python support: the Python frontend's fused instructions
//! (`Instr::Py*`, executed by `Vm::py_step`/`py_step_ext` in
//! `vm/py_ops.rs`) in the memory-backed tiers, and the policy for what a
//! Python program compiles.
//!
//! # What compiles
//!
//! In a Python program (`Program::python_natives`) only bodies the Python
//! frontend emitted compile: the JavaScript runtime they call into stays
//! interpreted (`Jit::set_python_program`). A Python loop compiles as an
//! EXTENDED memory region (`compile_py_region`): the loop itself plus the
//! out-of-line code it reaches (slow paths, guard fallbacks, the loop's exit
//! code), so a slow path taken every iteration stays native instead of
//! leaving and re-entering the region. A whole Python function compiles
//! (Tier C) only under `ZIPP_PY_TIERC=1` (see `Jit::fn_body_eligible`).
//!
//! Everything here is keyed on a body that CONTAINS a fused Python
//! instruction: only the Python emitter produces those, so JavaScript
//! functions and loops compile exactly as before. Such a body additionally
//! admits the few ops the Python emitter wraps every function in (the
//! traceback handler's `PushHandler`/`PopHandler`, the re-raising `Throw`,
//! int literals' `LoadBigInt`, list displays' `NewArray`/`ArrayAppend`), and
//! it takes only the plain emission paths: the register tiers, local scalar
//! replacement and the live-state plans whose control-flow proofs list jump
//! targets by hand (and would not see a fused instruction's `slow` edge)
//! decline it. Every analysis the memory tiers do run on such a body sees
//! those edges ([`control_targets`], the meter's block map).
//!
//! # The fused instructions
//!
//! Each one either completes (falls through to `ip + 1`, or for
//! `PyJumpCompare` also jumps to its `target`) or jumps to `slow`, the
//! emitter's generic code, having changed nothing. Compiled code runs an
//! inline fast path for the commonest operand shapes (Numbers, which are
//! Python floats; interned small ints; Int-tagged loop counters) and
//! otherwise calls a helper: `Vm::jit_py_arith`/`jit_py_add_imm`/`jit_py_cmp`
//! for the arithmetic and comparisons (the step's own `py_arith`/
//! `py_add_imm`/`py_compare` over operand bits), `Vm::jit_py_op` for the rest,
//! which runs the interpreter's own step for that instruction and names the
//! successor it chose; the native code then branches to the same successor.
//! So the semantics live in one place, and an inline path only ever
//! reproduces a result the step computes bit for bit (`py_store_num` boxes
//! a float exactly as `Value::num` does).
#![allow(unused_imports)]
use super::*;

/// `Vm::jit_py_op` outcomes.
pub(crate) const PY_NEXT: u64 = 0;
/// A `PyJumpCompare` took its branch.
pub(crate) const PY_TAKEN: u64 = 1;
/// The instruction's `slow` edge.
pub(crate) const PY_SLOW: u64 = 2;
/// Nothing ran; the interpreter must execute the instruction itself.
pub(crate) const PY_BAIL: u64 = 3;
/// The instruction threw (the exception is pending); unwind.
pub(crate) const PY_THREW: u64 = 4;

/// `(slow, target)` for a fused Python instruction: its `slow` edge and, for
/// `PyJumpCompare`, its branch target (for `PyDictLookup`, its `absent`
/// edge). `None` for every other instruction,
/// including a fused instruction this tier does not know (an unknown one in
/// a Python loop's out-of-line code compiles to an exit, and one in the loop
/// itself keeps the loop interpreted). Every instruction listed here must be
/// one `Vm::py_exec` routes to the interpreter's own step for it. One runs
/// guest code: `PyGenNext` resumes a generator to its next yield on a nested
/// interpreter loop (`Vm::py_gen_next`, run to completion exactly as the
/// interpreter runs it, like a frame call from compiled code; the register
/// file is pinned while native code runs, see `reserve_jit_regs`). `PyRaise`
/// always ends in a throw, which the native code unwinds like any helper's.
#[inline]
pub(crate) fn py_op_edges(i: &Instr) -> Option<(u32, Option<u32>)> {
    Some(match *i {
        Instr::PyArith { slow, .. }
        | Instr::PyAddImm { slow, .. }
        | Instr::PyCompare { slow, .. }
        | Instr::PyClassOf { slow, .. }
        | Instr::PyDictGet { slow, .. }
        | Instr::PyDictSet { slow, .. }
        | Instr::PyCallEntry { slow, .. }
        | Instr::PyGetItem { slow, .. }
        | Instr::PySetItem { slow, .. }
        | Instr::PyGlobal { slow, .. }
        | Instr::PyStrItem { slow, .. }
        | Instr::PyStrLen { slow, .. }
        | Instr::PyGetAttr { slow, .. }
        | Instr::PySetAttr { slow, .. }
        | Instr::PyIsInstance { slow, .. }
        | Instr::PyMethod { slow, .. }
        | Instr::PyModGet { slow, .. }
        | Instr::PyLen { slow, .. }
        | Instr::PyAttrFn { slow, .. }
        | Instr::PyRaise { slow, .. }
        | Instr::PyCaught { slow, .. }
        | Instr::PyClassAttr { slow, .. }
        | Instr::PyUnpack { slow, .. }
        | Instr::PyGenNext { slow, .. }
        | Instr::PyMakeExc { slow, .. }
        | Instr::PyExcPop { slow, .. }
        | Instr::PyNew { slow, .. } => (slow, None),
        // Its second edge: the key's absence (a `.get` default).
        Instr::PyDictLookup { slow, absent, .. } => (slow, Some(absent)),
        #[cfg(feature = "python")]
        Instr::PySeq { slow, .. } => (slow, None),
        Instr::PyJumpCompare { slow, target, .. } => (slow, Some(target)),
        _ => return None,
    })
}

/// The registers a fused Python instruction reads and the ones it writes
/// (none for a `PyJumpCompare`/`PyDictSet`/`PySetItem`/`PyRaise`/
/// `PyExcPop`; three for `PyNew`). Exact: the operand-bounds check
/// (`tierc_operands_in_bounds`) relies on it.
pub(crate) fn py_op_regs(i: &Instr) -> Option<(Vec<u16>, Vec<u16>)> {
    let (uses, dst) = match *i {
        Instr::PyNew { dst, entry, this_f, cls, rt, .. } => return Some((vec![cls, rt], vec![dst, entry, this_f])),
        Instr::PyMakeExc { dst, cls, args, rt, .. } => (vec![cls, args, rt], Some(dst)),
        Instr::PyExcPop { rt, .. } => (vec![rt], None),
        Instr::PyArith { dst, a, b, .. } | Instr::PyCompare { dst, a, b, .. } => (vec![a, b], Some(dst)),
        Instr::PyAddImm { dst, a, .. } => (vec![a], Some(dst)),
        Instr::PyJumpCompare { a, b, .. } => (vec![a, b], None),
        Instr::PyClassOf { dst, obj, .. } | Instr::PyDictGet { dst, obj, .. } => (vec![obj], Some(dst)),
        Instr::PyDictSet { obj, val, .. } => (vec![obj, val], None),
        Instr::PyCallEntry { dst, f, .. } => (vec![f], Some(dst)),
        Instr::PyGetItem { dst, o, k, seq, dict, .. } => (vec![o, k, seq, dict], Some(dst)),
        Instr::PySetItem { o, k, v, seq, dict, .. } => (vec![o, k, v, seq, dict], None),
        Instr::PyGlobal { dst, globals, rt, .. } => (vec![globals, rt], Some(dst)),
        Instr::PyStrItem { dst, s, k, .. } => (vec![s, k], Some(dst)),
        Instr::PyStrLen { dst, s, .. } => (vec![s], Some(dst)),
        Instr::PyGetAttr { dst, obj, .. } => (vec![obj], Some(dst)),
        Instr::PySetAttr { obj, val, .. } => (vec![obj, val], None),
        Instr::PyIsInstance { dst, v, t, rt, .. } => (vec![v, t, rt], Some(dst)),
        Instr::PyMethod { dst, obj, rt, .. } | Instr::PyModGet { dst, obj, rt, .. } => (vec![obj, rt], Some(dst)),
        Instr::PyLen { dst, v, rt, .. } => (vec![v, rt], Some(dst)),
        Instr::PyAttrFn { dst, obj, .. } => (vec![obj], Some(dst)),
        Instr::PySeq { dst, items, rt, .. } => (vec![items, rt], Some(dst)),
        Instr::PyRaise { e, rt, .. } => (vec![e, rt], None),
        Instr::PyCaught { dst, e, line, rt, .. } => (vec![e, line, rt], Some(dst)),
        Instr::PyClassAttr { dst, obj, .. } => (vec![obj], Some(dst)),
        Instr::PyDictLookup { dst, d, k, rt, .. } => (vec![d, k, rt], Some(dst)),
        Instr::PyUnpack { dst, v, rt, .. } => (vec![v, rt], Some(dst)),
        Instr::PyGenNext { dst, next, this, .. } => (vec![next, this], Some(dst)),
        _ => return None,
    };
    Some((uses, dst.into_iter().collect()))
}

/// Whether `code` holds a fused Python instruction (a body the Python
/// frontend emitted).
pub(crate) fn has_py_ops(code: &[Instr]) -> bool {
    code.iter().any(|i| py_op_edges(i).is_some())
}

/// Every control-flow destination of `i`: the ordinary jump/handler target
/// (see [`bytecode_control_target`]) plus a fused Python instruction's `slow`
/// edge and branch target. The fall-through is not included.
#[inline]
pub(crate) fn control_targets(i: &Instr) -> [Option<u32>; 2] {
    match py_op_edges(i) {
        Some((slow, target)) => [Some(slow), target],
        None => [bytecode_control_target(i), None],
    }
}

/// The extra ops a Python body admits in the memory tiers beyond the fused
/// instructions: `LoadBigInt` (an int literal), the per-call traceback
/// handler's `PushHandler`/`PopHandler`, `Throw` (re-raising from that
/// handler, or raising from a statement), which compiles to a bail so the
/// interpreter performs it, a `try` statement's `EndFinally` (its normal
/// completion falls through; an abrupt one bails to the interpreter's
/// routing), and the list/tuple builders `NewArray` and a plain
/// `ArrayAppend`.
pub(crate) fn py_body_op(i: &Instr) -> bool {
    matches!(
        i,
        Instr::LoadBigInt { .. }
            | Instr::PushHandler { .. }
            | Instr::PopHandler
            | Instr::Throw { .. }
            | Instr::EndFinally { .. }
            | Instr::NewArray { .. }
            | Instr::ArrayAppend { spread: false, .. }
    )
}

/// The heap value of an interned small int (`make_bigint`'s pinned table),
/// or `None` outside the table's range.
pub(crate) fn interned_bigint_bits(v: i128) -> Option<u64> {
    use crate::heap::{INTERN_BIGINT_MAX, INTERN_BIGINT_MIN, INTERN_BIGINT_START};
    (INTERN_BIGINT_MIN..=INTERN_BIGINT_MAX)
        .contains(&v)
        .then(|| Value::heap(INTERN_BIGINT_START + (v - INTERN_BIGINT_MIN) as u32).bits())
}

/// Where a fused Python instruction's successors live in the emitted body.
pub(crate) struct PyLabels {
    /// The `slow` edge (an in-body label or an exit stub).
    pub slow: dynasmrt::DynamicLabel,
    /// `PyJumpCompare`'s branch target.
    pub target: Option<dynasmrt::DynamicLabel>,
    /// Resume the interpreter AT this instruction (nothing ran), or unwind
    /// when an exception is pending.
    pub bail: dynasmrt::DynamicLabel,
}

/// Emit the out-of-line step for the fused Python instruction at `ip`: call
/// `Vm::jit_py_op` over the frame window (`rbx`) and branch on its outcome;
/// falls through for `PY_NEXT`. `refetch` re-derives the body's pinned
/// pointers (the helper may allocate, which moves them); it may clobber any
/// volatile register. The outcome is parked in the 5th-argument slot
/// (`[rsp + 32]`: both tiers' frames reserve it below their own slots, and it
/// is scratch between calls) around it. That relies on `refetch` making no
/// 5-argument call, which holds for what the tiers pass: the pinned-pointer
/// refresh (plain loads, or 1-argument helpers) and the typed-array pin
/// refresh (4-argument helpers; empty in a Python loop anyway).
pub(crate) fn emit_py_step(
    ops: &mut dynasmrt::x64::Assembler,
    func_id: u32,
    ip: usize,
    labels: &PyLabels,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
) {
    let packed = ((func_id as u64) << 32) | ip as u64;
    let helper = crate::vm::Vm::jit_py_op as usize;
    let cont = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rcx, rdi
        ; mov rdx, rbx
        ; mov r8, QWORD packed as i64
        ; mov rax, QWORD helper as i64
        ; call rax
        ; mov [rsp + 32], rax
    );
    refetch(ops);
    dynasm!(ops
        ; mov rax, [rsp + 32]
        ; test eax, eax
        ; jz => cont
        ; cmp eax, PY_SLOW as i32
        ; je => labels.slow
    );
    if let Some(t) = labels.target {
        dynasm!(ops
            ; cmp eax, PY_TAKEN as i32
            ; je => t
        );
    }
    dynasm!(ops
        ; jmp => labels.bail
        ; => cont
    );
}

/// `LoadBigInt` of a value outside the interned table: `make_bigint` through
/// `Vm::jit_py_load_bigint` (an allocation, so `refetch` follows). `bail`
/// resumes the interpreter at the instruction.
pub(crate) fn emit_py_load_bigint(
    ops: &mut dynasmrt::x64::Assembler,
    func_id: u32,
    ip: usize,
    dst: u16,
    bail: dynasmrt::DynamicLabel,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
) {
    let packed = ((func_id as u64) << 32) | ip as u64;
    let helper = crate::vm::Vm::jit_py_load_bigint as usize;
    dynasm!(ops
        ; mov rcx, rdi
        ; mov r8, QWORD packed as i64
        ; mov rdx, r8
        ; mov rax, QWORD helper as i64
        ; call rax
        ; mov r10, QWORD SELF_CALL_DEOPT as i64
        ; cmp rax, r10
        ; je => bail
        ; mov [rbx + dreg(dst)], rax
    );
    refetch(ops);
}

/// `(TAG_HEAP | INTERN_BIGINT_START)`: subtracting it from a Value's bits
/// leaves, for an interned small int, its table offset (`value - MIN`), and
/// for anything else a number above `INTERN_SPAN` (as unsigned).
const INTERN_BASE_BITS: u64 = HEAP_TAG | crate::heap::INTERN_BIGINT_START as u64;
/// The largest interned table offset.
const INTERN_SPAN: i32 = (crate::heap::INTERN_BIGINT_COUNT - 1) as i32;

/// `JumpIfNotLt`/`JumpIfNotLe` in a Python body, whose loop counters are
/// Python ints (BigInts): two Ints compare inline, two interned small ints
/// compare by their table offsets, and anything else asks the pure
/// `Vm::jit_py_rel` (a Number or BigInt pair; any other operand resumes the
/// interpreter at `bail`, which performs the coercing comparison).
pub(crate) fn emit_py_jump_if_not(
    ops: &mut dynasmrt::x64::Assembler,
    a: u16,
    b: u16,
    le: bool,
    target: dynasmrt::DynamicLabel,
    bail: dynasmrt::DynamicLabel,
) {
    let not_ii = ops.new_dynamic_label();
    let helper = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(a)]
        ; mov rcx, [rbx + dreg(b)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => not_ii
        ; mov r10, rcx
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => not_ii
        ; cmp eax, ecx
    );
    if le {
        dynasm!(ops ; jg => target);
    } else {
        dynasm!(ops ; jge => target);
    }
    dynasm!(ops
        ; jmp => done
        ; => not_ii
        ; mov r10, QWORD INTERN_BASE_BITS as i64
        ; mov r8, rax
        ; sub r8, r10
        ; cmp r8, INTERN_SPAN
        ; ja => helper
        ; mov r9, rcx
        ; sub r9, r10
        ; cmp r9, INTERN_SPAN
        ; ja => helper
        ; cmp r8, r9
    );
    if le {
        dynasm!(ops ; jg => target);
    } else {
        dynasm!(ops ; jge => target);
    }
    dynasm!(ops
        ; jmp => done
        ; => helper
        ; mov r8, rcx
        ; mov rdx, rax
        ; mov rcx, rdi
        ; mov r9d, le as i32
        ; mov rax, QWORD crate::vm::Vm::jit_py_rel as usize as i64
        ; call rax
        ; mov r10, QWORD SELF_CALL_DEOPT as i64
        ; cmp rax, r10
        ; je => bail
        ; test eax, eax
        ; jz => target
        ; => done
    );
}

/// `AddInt` in a Python body: an Int adds inline (overflow goes to the
/// helper), an interned small int whose sum stays interned (under `upd`,
/// the BigInt `+ 1n` a Python `i += 1` compiles to) is table arithmetic, and
/// anything else asks `Vm::jit_py_add_int` (which allocates, so `refetch`
/// follows it; a decline resumes the interpreter at `bail`).
pub(crate) fn emit_py_add_int(
    ops: &mut dynasmrt::x64::Assembler,
    dst: u16,
    a: u16,
    imm: i32,
    upd: bool,
    bail: dynasmrt::DynamicLabel,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
) {
    let not_int = ops.new_dynamic_label();
    let helper = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(a)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; jne => not_int
        ; mov ecx, eax
        ; add ecx, imm
        ; jo => helper
        ; mov eax, ecx
    );
    box_eax(ops, dst);
    dynasm!(ops
        ; jmp => done
        ; => not_int
    );
    if upd {
        dynasm!(ops
            ; mov r10, QWORD INTERN_BASE_BITS as i64
            ; mov r8, rax
            ; sub r8, r10
            ; cmp r8, INTERN_SPAN
            ; ja => helper
            ; add r8, imm
            ; cmp r8, INTERN_SPAN
            ; ja => helper
            ; add r8, r10
            ; mov [rbx + dreg(dst)], r8
            ; jmp => done
        );
    }
    let packed = (imm as u32 as u64) | ((upd as u64) << 32);
    dynasm!(ops
        ; => helper
        ; mov rcx, rdi
        ; mov rdx, rax
        ; mov r8, QWORD packed as i64
        ; mov rax, QWORD crate::vm::Vm::jit_py_add_int as usize as i64
        ; call rax
        ; mov r10, QWORD SELF_CALL_DEOPT as i64
        ; cmp rax, r10
        ; je => bail
        ; mov r10, QWORD CALL_THREW as i64
        ; cmp rax, r10
        ; je => bail
        ; mov [rbx + dreg(dst)], rax
    );
    refetch(ops);
    dynasm!(ops ; => done);
}

/// `Vm::jit_py_arith`/`jit_py_add_imm`'s "take the `slow` edge" answer (not a
/// `Value`: the tag is outside the value encoding, like `SELF_CALL_DEOPT`).
pub(crate) const PY_SLOW_BITS: u64 = 0x7FFE_DEAD_BEEF_0002;

const ARITH_OPS: [crate::bytecode::PyArithOp; 9] = {
    use crate::bytecode::PyArithOp as A;
    [A::Add, A::Sub, A::Mul, A::TrueDiv, A::FloorDiv, A::Mod, A::BitAnd, A::BitOr, A::BitXor]
};
const CMP_OPS: [crate::bytecode::PyCmpOp; 6] = {
    use crate::bytecode::PyCmpOp as C;
    [C::Lt, C::Le, C::Gt, C::Ge, C::Eq, C::Ne]
};

/// The helper-argument encoding of a `PyArithOp`.
pub(crate) fn py_arith_op_code(op: crate::bytecode::PyArithOp) -> u64 {
    ARITH_OPS.iter().position(|&o| o == op).unwrap_or(usize::MAX) as u64
}
/// The `PyArithOp` a helper argument encodes.
pub(crate) fn py_arith_op_from(code: u64) -> Option<crate::bytecode::PyArithOp> {
    ARITH_OPS.get(code as usize).copied()
}
/// The helper-argument encoding of a `PyCmpOp`.
pub(crate) fn py_cmp_op_code(op: crate::bytecode::PyCmpOp) -> u64 {
    CMP_OPS.iter().position(|&o| o == op).unwrap_or(usize::MAX) as u64
}
/// The `PyCmpOp` a helper argument encodes.
pub(crate) fn py_cmp_op_from(code: u64) -> Option<crate::bytecode::PyCmpOp> {
    CMP_OPS.get(code as usize).copied()
}

/// The `Value::bool(false)` bits (`true` is one more).
const FALSE_BITS: u64 = BOOL_TAG;
/// The canonical NaN (`Value::num(NaN)`).
const QNAN_BITS: u64 = 0x7FF8_0000_0000_0000;

/// Load `regs[reg]` as a Number into `xmm{which}` (an Int converts
/// exactly), or jump to `not_num` for any other value. Clobbers rax, r10.
fn py_load_num(ops: &mut dynasmrt::x64::Assembler, reg: u16, which: u8, not_num: dynasmrt::DynamicLabel) {
    let int_path = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; mov rax, [rbx + dreg(reg)]
        ; mov r10, rax
        ; shr r10, 48
        ; cmp r10d, INT_TAG_HI as i32
        ; je => int_path
        ; sub r10d, (INT_TAG_HI + 1) as i32
        ; cmp r10d, 3
        ; jbe => not_num
        ; movq Rx(which), rax
        ; jmp => done
        ; => int_path
        ; xorps Rx(which), Rx(which)
        ; cvtsi2sd Rx(which), eax
        ; => done
    );
}

/// Store `xmm0` into `regs[dst]` exactly as `Value::num` boxes it: an
/// integral value in i32 range other than -0 as an Int, NaN as the
/// canonical NaN, anything else as its double bits. So a compiled result is
/// bit-identical to the interpreter's. Clobbers rax, r8, xmm1.
fn py_store_num(ops: &mut dynasmrt::x64::Assembler, dst: u16) {
    let nan = ops.new_dynamic_label();
    let dbl = ops.new_dynamic_label();
    let int = ops.new_dynamic_label();
    let done = ops.new_dynamic_label();
    dynasm!(ops
        ; ucomisd xmm0, xmm0
        ; jp => nan
        ; cvttsd2si eax, xmm0
        ; xorps xmm1, xmm1
        ; cvtsi2sd xmm1, eax
        ; ucomisd xmm0, xmm1
        ; jne => dbl
        ; test eax, eax
        ; jnz => int
        ; movq r8, xmm0
        ; test r8, r8
        ; js => dbl
        ; => int
    );
    box_eax(ops, dst);
    dynasm!(ops
        ; jmp => done
        ; => nan
        ; mov rax, QWORD QNAN_BITS as i64
        ; mov [rbx + dreg(dst)], rax
        ; jmp => done
        ; => dbl
        ; movq rax, xmm0
        ; mov [rbx + dreg(dst)], rax
        ; => done
    );
}

/// Both operands interned small ints: their table offsets in r8 (a) and r9
/// (b), else a jump to `no`. Clobbers r10.
fn py_load_interned_pair(ops: &mut dynasmrt::x64::Assembler, a: u16, b: u16, no: dynasmrt::DynamicLabel) {
    dynasm!(ops
        ; mov r10, QWORD INTERN_BASE_BITS as i64
        ; mov r8, [rbx + dreg(a)]
        ; sub r8, r10
        ; cmp r8, INTERN_SPAN
        ; ja => no
        ; mov r9, [rbx + dreg(b)]
        ; sub r9, r10
        ; cmp r9, INTERN_SPAN
        ; ja => no
    );
}

/// Store the interned small int at table offset r8, if r8 is one, else jump
/// to `no`. Clobbers r10.
fn py_store_interned(ops: &mut dynasmrt::x64::Assembler, dst: u16, no: dynasmrt::DynamicLabel) {
    dynasm!(ops
        ; cmp r8, INTERN_SPAN
        ; ja => no
        ; mov r10, QWORD INTERN_BASE_BITS as i64
        ; add r8, r10
        ; mov [rbx + dreg(dst)], r8
    );
}

/// After `cmp a, b` (signed): branch to `to` when `a <op> b` is `holds`.
fn int_cond_jump(ops: &mut dynasmrt::x64::Assembler, op: crate::bytecode::PyCmpOp, holds: bool, to: dynasmrt::DynamicLabel) {
    use crate::bytecode::PyCmpOp as C;
    match (op, holds) {
        (C::Lt, true) | (C::Ge, false) => dynasm!(ops ; jl => to),
        (C::Le, true) | (C::Gt, false) => dynasm!(ops ; jle => to),
        (C::Gt, true) | (C::Le, false) => dynasm!(ops ; jg => to),
        (C::Ge, true) | (C::Lt, false) => dynasm!(ops ; jge => to),
        (C::Eq, true) | (C::Ne, false) => dynasm!(ops ; je => to),
        (C::Ne, true) | (C::Eq, false) => dynasm!(ops ; jne => to),
    }
}

/// Branch to `to` when the IEEE comparison `xmm0 <op> xmm1` has the truth
/// value `holds` (a NaN makes every ordering and `==` false, `!=` true);
/// fall through otherwise.
fn float_cond_jump(ops: &mut dynasmrt::x64::Assembler, op: crate::bytecode::PyCmpOp, holds: bool, to: dynasmrt::DynamicLabel) {
    use crate::bytecode::PyCmpOp as C;
    let skip = ops.new_dynamic_label();
    match op {
        // a < b is b > a; unordered sets CF=ZF=PF=1, so `ja`/`jae` are false.
        C::Lt | C::Le => dynasm!(ops ; ucomisd xmm1, xmm0),
        C::Gt | C::Ge | C::Eq | C::Ne => dynasm!(ops ; ucomisd xmm0, xmm1),
    }
    match (op, holds) {
        (C::Lt | C::Gt, true) => dynasm!(ops ; ja => to),
        (C::Lt | C::Gt, false) => dynasm!(ops ; jbe => to),
        (C::Le | C::Ge, true) => dynasm!(ops ; jae => to),
        (C::Le | C::Ge, false) => dynasm!(ops ; jb => to),
        (C::Eq, true) | (C::Ne, false) => dynasm!(ops ; jp => skip ; je => to),
        (C::Eq, false) | (C::Ne, true) => dynasm!(ops ; jp => to ; jne => to),
    }
    dynasm!(ops ; => skip);
}

/// Emit a fused Python instruction: its inline fast paths where it has
/// them, then the out-of-line step (a direct helper for the arithmetic and
/// comparisons, `Vm::jit_py_op` for the rest). `refetch` follows every helper
/// that can allocate.
pub(crate) fn emit_py_op(
    ops: &mut dynasmrt::x64::Assembler,
    func_id: u32,
    ip: usize,
    instr: &Instr,
    labels: &PyLabels,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
) {
    use crate::bytecode::PyArithOp as A;
    let done = ops.new_dynamic_label();
    let helper = ops.new_dynamic_label();
    match *instr {
        Instr::PyArith { op, dst, a, b, .. } => {
            if matches!(op, A::Add | A::Sub | A::Mul | A::TrueDiv) {
                // Two Numbers (Python floats): the f64 operation, boxed as
                // `Value::num` boxes it.
                let not_num = ops.new_dynamic_label();
                py_load_num(ops, a, 0, not_num);
                py_load_num(ops, b, 1, not_num);
                match op {
                    A::Add => dynasm!(ops ; addsd xmm0, xmm1),
                    A::Sub => dynasm!(ops ; subsd xmm0, xmm1),
                    A::Mul => dynasm!(ops ; mulsd xmm0, xmm1),
                    _ => {
                        // A zero divisor (either sign) is Python's
                        // ZeroDivisionError: the slow edge.
                        dynasm!(ops
                            ; xorps xmm2, xmm2
                            ; ucomisd xmm1, xmm2
                            ; jp => helper
                            ; je => labels.slow
                            ; divsd xmm0, xmm1
                        );
                    }
                }
                py_store_num(ops, dst);
                dynasm!(ops ; jmp => done ; => not_num);
            }
            if matches!(op, A::Add | A::Sub | A::Mul) {
                // Two interned small ints whose result stays interned.
                py_load_interned_pair(ops, a, b, helper);
                let off = -(crate::heap::INTERN_BIGINT_MIN as i32);
                match op {
                    A::Add => dynasm!(ops ; add r8, r9 ; sub r8, off),
                    A::Sub => dynasm!(ops ; sub r8, r9 ; add r8, off),
                    _ => dynasm!(ops
                        ; sub r8, off
                        ; sub r9, off
                        ; imul r8, r9
                        ; add r8, off
                    ),
                }
                py_store_interned(ops, dst, helper);
                dynasm!(ops ; jmp => done);
            }
            dynasm!(ops
                ; => helper
                ; mov rcx, rdi
                ; mov rdx, [rbx + dreg(a)]
                ; mov r8, [rbx + dreg(b)]
                ; mov r9d, py_arith_op_code(op) as i32
                ; mov rax, QWORD crate::vm::Vm::jit_py_arith as usize as i64
                ; call rax
            );
            emit_value_outcome(ops, dst, labels, refetch);
        }
        Instr::PyAddImm { dst, a, imm, .. } => {
            let not_int = ops.new_dynamic_label();
            let not_num = ops.new_dynamic_label();
            // An Int (a float with an integral value): `checked_add`, an
            // overflow taking the f64 sum below.
            dynasm!(ops
                ; mov rax, [rbx + dreg(a)]
                ; mov r10, rax
                ; shr r10, 48
                ; cmp r10d, INT_TAG_HI as i32
                ; jne => not_int
                ; mov ecx, eax
                ; add ecx, imm
                ; jo => not_int
                ; mov eax, ecx
            );
            box_eax(ops, dst);
            dynasm!(ops
                ; jmp => done
                ; => not_int
            );
            py_load_num(ops, a, 0, not_num);
            dynasm!(ops
                ; mov eax, imm
                ; xorps xmm1, xmm1
                ; cvtsi2sd xmm1, eax
                ; addsd xmm0, xmm1
            );
            py_store_num(ops, dst);
            dynasm!(ops
                ; jmp => done
                ; => not_num
                ; mov r10, QWORD INTERN_BASE_BITS as i64
                ; mov r8, [rbx + dreg(a)]
                ; sub r8, r10
                ; cmp r8, INTERN_SPAN
                ; ja => helper
                ; add r8, imm
            );
            py_store_interned(ops, dst, helper);
            dynasm!(ops
                ; jmp => done
                ; => helper
                ; mov rcx, rdi
                ; mov rdx, [rbx + dreg(a)]
                ; mov r8d, imm
                ; mov rax, QWORD crate::vm::Vm::jit_py_add_imm as usize as i64
                ; call rax
            );
            emit_value_outcome(ops, dst, labels, refetch);
        }
        Instr::PyCompare { op, a, b, .. } | Instr::PyJumpCompare { op, a, b, .. } => {
            // `when`: the truth value that goes to `yes` (the branch of a
            // jump form; `True` stored by a value form).
            let when = match *instr {
                Instr::PyJumpCompare { when, .. } => when,
                _ => true,
            };
            let yes = ops.new_dynamic_label();
            let no = ops.new_dynamic_label();
            let not_ii = ops.new_dynamic_label();
            let not_num = ops.new_dynamic_label();
            dynasm!(ops
                ; mov rax, [rbx + dreg(a)]
                ; mov rcx, [rbx + dreg(b)]
                ; mov r10, rax
                ; shr r10, 48
                ; cmp r10d, INT_TAG_HI as i32
                ; jne => not_ii
                ; mov r10, rcx
                ; shr r10, 48
                ; cmp r10d, INT_TAG_HI as i32
                ; jne => not_ii
                ; cmp eax, ecx
            );
            int_cond_jump(ops, op, when, yes);
            dynasm!(ops
                ; jmp => no
                ; => not_ii
            );
            py_load_num(ops, a, 0, not_num);
            py_load_num(ops, b, 1, not_num);
            float_cond_jump(ops, op, when, yes);
            dynasm!(ops
                ; jmp => no
                ; => not_num
            );
            py_load_interned_pair(ops, a, b, helper);
            dynasm!(ops ; cmp r8, r9);
            int_cond_jump(ops, op, when, yes);
            dynasm!(ops
                ; jmp => no
                ; => helper
                ; mov rcx, rdi
                ; mov rdx, [rbx + dreg(a)]
                ; mov r8, [rbx + dreg(b)]
                ; mov r9d, py_cmp_op_code(op) as i32
                ; mov rax, QWORD crate::vm::Vm::jit_py_cmp as usize as i64
                ; call rax
                ; cmp eax, PY_SLOW as i32
                ; je => labels.slow
                ; cmp eax, PY_BAIL as i32
                ; je => labels.bail
                ; cmp eax, when as i32
                ; je => yes
                ; => no
            );
            match *instr {
                Instr::PyCompare { dst, .. } => {
                    dynasm!(ops
                        ; mov rax, QWORD FALSE_BITS as i64
                        ; mov [rbx + dreg(dst)], rax
                        ; jmp => done
                        ; => yes
                        ; mov rax, QWORD (FALSE_BITS | 1) as i64
                        ; mov [rbx + dreg(dst)], rax
                    );
                }
                _ => {
                    // `no` falls through to ip + 1; `yes` is the branch.
                    let Some(target) = labels.target else {
                        unreachable!("PyJumpCompare carries a target label");
                    };
                    dynasm!(ops
                        ; jmp => done
                        ; => yes
                        ; jmp => target
                    );
                }
            }
        }
        _ => {
            emit_py_step(ops, func_id, ip, labels, refetch);
        }
    }
    dynasm!(ops ; => done);
}

/// After a `jit_py_arith`/`jit_py_add_imm` call: `PY_SLOW_BITS` takes the
/// slow edge, `CALL_THREW` the bail (unwind), anything else is the result.
/// The pinned pointers are re-derived first (the result is parked in the
/// 5th-argument slot around `refetch`).
fn emit_value_outcome(
    ops: &mut dynasmrt::x64::Assembler,
    dst: u16,
    labels: &PyLabels,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
) {
    dynasm!(ops ; mov [rsp + 32], rax);
    refetch(ops);
    dynasm!(ops
        ; mov rax, [rsp + 32]
        ; mov r10, QWORD PY_SLOW_BITS as i64
        ; cmp rax, r10
        ; je => labels.slow
        ; mov r10, QWORD CALL_THREW as i64
        ; cmp rax, r10
        ; je => labels.bail
        ; mov [rbx + dreg(dst)], rax
    );
}


/// Most out-of-line ips an extended Python region takes in.
const PY_REGION_EXTRA_MAX: usize = 512;

/// The blocks an extended Python region compiles with the loop `[start,
/// end]`: the loop, plus the out-of-line code it reaches (a fused
/// instruction's slow path, a guard's generic code — the code the Python
/// emitter lays out after the body's main code, see [`py_main_end`]) until
/// control returns into the loop, leaves through a `Return`/`Throw`, or goes
/// anywhere else (an exit, such as the loop's own exit into the rest of the
/// body), up to [`PY_REGION_EXTRA_MAX`] extra ips. Without them a loop whose
/// slow path runs every iteration would leave and re-enter its region every
/// iteration. A handler's catch target is not followed: only the
/// interpreter's unwind reaches it. Returns the last member ip and the
/// membership flags of `[start, that ip]`.
pub(crate) fn py_region_members(proto: &FuncProto, start: u32, end: u32) -> (u32, Vec<bool>) {
    let code = &proto.code;
    let n = code.len();
    let (s, e) = (start as usize, end as usize);
    let cold = py_main_end(proto).max(e);
    let mut member = vec![false; n];
    for m in &mut member[s..=e] {
        *m = true;
    }
    let succs = |ip: usize| -> Vec<usize> {
        let mut out: Vec<usize> = match code[ip] {
            Instr::PushHandler { .. } => Vec::new(),
            _ => control_targets(&code[ip]).into_iter().flatten().map(|t| t as usize).collect(),
        };
        if !matches!(
            code[ip],
            Instr::Jump { .. } | Instr::Return { .. } | Instr::ReturnUndefined | Instr::Throw { .. }
        ) {
            out.push(ip + 1);
        }
        // Only out-of-line code joins; the loop is already in.
        out.retain(|&t| t > cold && t < n);
        out
    };
    let mut stack: Vec<usize> = Vec::new();
    if py_region_ext_enabled() {
        for ip in s..=e {
            stack.extend(succs(ip));
        }
    }
    stack.reverse();
    let mut added = 0usize;
    while let Some(ip) = stack.pop() {
        if member[ip] {
            continue;
        }
        if added >= PY_REGION_EXTRA_MAX {
            break;
        }
        member[ip] = true;
        added += 1;
        for t in succs(ip).into_iter().rev() {
            if !member[t] {
                stack.push(t);
            }
        }
    }
    let last = (s..n).rev().find(|&i| member[i]).unwrap_or(e);
    (last as u32, member[s..=last].to_vec())
}

impl Jit {
    /// Compile the Python loop `[start, end]` of `func_id` as an extended
    /// memory region (see [`py_region_members`]), or blacklist it. The
    /// region's recorded bounds stay the loop's, so an exit into its
    /// out-of-line blocks' uncompiled ops is a clean exit, and only a bail
    /// inside the loop itself counts toward eviction.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compile_py_region(
        &mut self,
        func_id: u32,
        proto: &FuncProto,
        start: u32,
        end: u32,
        globals_base_helper: usize,
        heap_helpers: HeapHelperAddrs,
        const_strs: &FxHashMap<u32, u64>,
        leaf_plan: &FxHashMap<usize, LeafInlinePlan>,
        method_plan: &FxHashMap<usize, MethodInlinePlan>,
        cross_plan: &CrossCallPlan,
    ) {
        let key = (func_id, start);
        let plan = match py_region_plan(proto, start, end) {
            Ok(plan) => plan,
            Err(why) => {
                if std::env::var_os("ZIPP_JITLOG").is_some() {
                    eprintln!("[jit] PY region fn{func_id} [{start},{end}] DECLINED ({why})");
                }
                self.region_blacklist.insert(key);
                self.set_region_dead(key.0, key.1);
                return;
            }
        };
        let PyRegionPlan {
            end,
            ext_end,
            members,
            calls,
            work,
        } = plan;
        // The dispatcher interned the string constants of the loop proper
        // (`[start, end]` of its own range); the out-of-line code's string
        // loads resolve through `Vm::jit_py_load_const` instead.
        let code = &proto.code[start as usize..=ext_end as usize];
        let n_sites = code
            .iter()
            .filter(|i| matches!(i, Instr::GetProp { .. } | Instr::SetProp { .. }))
            .count();
        let ic_base_idx = self.reserve_ic_sites(n_sites);
        let acc_emit = self.register_ic_sites(ic_base_idx, func_id, start, code, start);
        let helpers = heap_helpers.to_heap_helpers(func_id, ic_base_idx);
        let compiled = compile_region_mem(
            proto,
            start,
            ext_end,
            globals_base_helper,
            helpers,
            const_strs,
            &TaPinPlan::default(),
            leaf_plan,
            method_plan,
            cross_plan,
            &acc_emit,
            self.meter,
            Some(&members),
        );
        let log = std::env::var_os("ZIPP_JITLOG").is_some();
        match compiled {
            Some(code) => {
                if log {
                    eprintln!(
                        "[jit] PY region fn{func_id} [{start},{end}] compiled through {ext_end} ({} ips) calls={calls} work={work}",
                        members.iter().filter(|&&m| m).count(),
                    );
                }
                let (a, n) = code.code_span();
                crate::vm::prof::pc::register(a, n, || format!("fn{func_id} py region [{start},{end}]"));
                self.regions.insert(
                    key,
                    Region {
                        code,
                        start,
                        end,
                        deopts: 0,
                        ok_runs: 0,
                        is_int: false,
                        is_mem: true,
                        field_plan: None,
                        local_sroa: None,
                        is_boxref: false,
                    },
                );
                self.note_reg_region_installed(func_id, true);
            }
            None => {
                if log {
                    eprintln!("[jit] PY region fn{func_id} [{start},{end}] DECLINED (blacklisted)");
                }
                self.region_blacklist.insert(key);
                self.set_region_dead(key.0, key.1);
            }
        }
    }
}

/// A plain `ArrayAppend` (a list display's element) in a Python body:
/// `Vm::jit_py_array_append`, the interpreter arm for an Array (anything
/// else resumes the interpreter at `bail`, nothing done).
pub(crate) fn emit_py_array_append(
    ops: &mut dynasmrt::x64::Assembler,
    arr: u16,
    val: u16,
    bail: dynasmrt::DynamicLabel,
) {
    dynasm!(ops
        ; mov rcx, rdi
        ; mov rdx, [rbx + dreg(arr)]
        ; mov r8, [rbx + dreg(val)]
        ; mov rax, QWORD crate::vm::Vm::jit_py_array_append as usize as i64
        ; call rax
        ; mov r10, QWORD SELF_CALL_DEOPT as i64
        ; cmp rax, r10
        ; je => bail
    );
}

/// `NewArray` in a Python loop: the whole-function tier's `jit_new_array`
/// (an allocation; `refetch` follows).
pub(crate) fn emit_py_new_array(
    ops: &mut dynasmrt::x64::Assembler,
    reg_count: u16,
    dst: u16,
    arg_base: u16,
    argc: u16,
    bail: dynasmrt::DynamicLabel,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
) {
    let packed = ((reg_count as u64) << 32) | ((arg_base as u64) << 16) | argc as u64;
    dynasm!(ops
        ; mov rcx, rdi
        ; mov rdx, rbx
        ; mov r8, QWORD packed as i64
        ; mov rax, QWORD crate::vm::jit_new_array as usize as i64
        ; call rax
        ; mov r10, QWORD SELF_CALL_DEOPT as i64
        ; cmp rax, r10
        ; je => bail
        ; mov [rbx + dreg(dst)], rax
    );
    refetch(ops);
}

/// The last ip of a Python body's main code: the ip before its frame guard's
/// handler (the first `PushHandler`'s catch target), after which the emitter
/// lays out only out-of-line code (handlers and slow paths); the last ip for
/// a body without one.
pub(crate) fn py_main_end(proto: &FuncProto) -> usize {
    proto
        .code
        .iter()
        .find_map(|i| match *i {
            Instr::PushHandler { catch_target, .. } => Some(catch_target as usize),
            _ => None,
        })
        .filter(|&t| t > 0 && t <= proto.code.len())
        .map(|t| t - 1)
        .unwrap_or(proto.code.len() - 1)
}

/// Whether `i` calls through a frame when compiled (the region's call
/// helpers push the callee's frame and run it on a nested interpreter loop).
pub(crate) fn py_frame_call(i: &Instr) -> bool {
    matches!(
        i,
        Instr::Call { .. }
            | Instr::CallWithThis { .. }
            | Instr::CallMethod { .. }
            | Instr::CallMethodComputed { .. }
            | Instr::New { .. }
    )
}

env_off_switch! {
    /// `ZIPP_PY_TIERC=1` lets whole Python functions compile (see
    /// `Jit::fn_body_eligible`); latched. Named as an off switch because the
    /// macro reads "unset": this one is on when SET.
    fn py_tierc_disabled() = "ZIPP_PY_TIERC"
}

/// Whether whole Python functions may compile (`ZIPP_PY_TIERC=1`).
pub(crate) fn py_tierc_enabled() -> bool {
    !py_tierc_disabled()
}

env_off_switch! {
    /// `ZIPP_NO_PY_REGION_EXT=1` compiles a Python loop without its
    /// out-of-line blocks (every slow path then exits the region), the
    /// same-binary A/B for [`py_region_members`]'s extension.
    fn py_region_ext_enabled() = "ZIPP_NO_PY_REGION_EXT"
}

/// The loop proper `[start, end]`'s per-iteration shape: the frame calls
/// every iteration makes (on every path from the header to the back-edge
/// `end`), and the other ops every iteration runs. Edges out of the loop
/// (slow paths, exits) are ignored: they are the rare case.
pub(crate) fn py_loop_profile(code: &[Instr], start: usize, end: usize) -> (usize, usize) {
    let n = end - start + 1;
    let succ = |ip: usize| -> Vec<usize> {
        let mut out: Vec<usize> = control_targets(&code[ip])
            .into_iter()
            .flatten()
            .map(|t| t as usize)
            .filter(|&t| t >= start && t <= end)
            .collect();
        if ip < end
            && !matches!(
                code[ip],
                Instr::Jump { .. } | Instr::Return { .. } | Instr::ReturnUndefined | Instr::Throw { .. }
            )
        {
            out.push(ip + 1);
        }
        // The handler a `PushHandler` names is reached by unwinding only.
        if let Instr::PushHandler { catch_target, .. } = code[ip] {
            out.retain(|&t| t != catch_target as usize);
        }
        out
    };
    // Whether `end` is reachable from `start` without passing `avoid`.
    let reaches_latch_avoiding = |avoid: usize| -> bool {
        if avoid == start {
            return false;
        }
        let mut seen = vec![false; n];
        let mut stack = vec![start];
        seen[0] = true;
        while let Some(ip) = stack.pop() {
            if ip == end {
                return true;
            }
            for t in succ(ip) {
                if t != avoid && !seen[t - start] {
                    seen[t - start] = true;
                    stack.push(t);
                }
            }
        }
        false
    };
    let (mut calls, mut work) = (0, 0);
    for ip in start..=end {
        if !reaches_latch_avoiding(ip) {
            if py_frame_call(&code[ip]) {
                calls += 1;
            } else {
                work += 1;
            }
        }
    }
    (calls, work)
}

/// See `compile_py_region`'s call-bound gate.
const PY_WORK_PER_CALL: usize = 11;

env_off_switch! {
    /// `ZIPP_NO_PY_REGIONS=1` keeps every Python loop interpreted (the
    /// same-binary A/B for Python regions; whole functions follow
    /// `ZIPP_PY_TIERC`).
    fn py_regions_enabled() = "ZIPP_NO_PY_REGIONS"
}

/// A Python loop the extended region compiles (see [`py_region_plan`]).
pub(crate) struct PyRegionPlan {
    /// The loop proper's back-edge.
    pub end: u32,
    /// The last member ip.
    pub ext_end: u32,
    /// Membership of `[start, ext_end]`.
    pub members: Vec<bool>,
    /// The loop's per-iteration frame calls and other ops (JITLOG only).
    pub calls: usize,
    pub work: usize,
}

/// Decide whether the Python loop headed at `start` (the dispatcher's
/// region `[start, end]`) compiles, and as what. Pure: it reads only the
/// bytecode. A string-constant load counts as compilable here: the
/// dispatcher interns the loop's, and the region resolves the rest at run
/// time.
pub(crate) fn py_region_plan(proto: &FuncProto, start: u32, end: u32) -> Result<PyRegionPlan, String> {
    if !py_regions_enabled() {
        return Err("ZIPP_NO_PY_REGIONS".into());
    }
    // The loop proper: the dispatcher extends a region to the last `Jump`
    // back to its header, and the Python emitter's out-of-line code (after
    // the body's final return, where the frame guard's handler begins)
    // jumps back into loops too. A header only such code jumps to is no
    // loop at all (a slow path's return into the middle of a body).
    let main_end = py_main_end(proto);
    let end = (start as usize..=(end as usize).min(main_end))
        .rev()
        .find(|&ip| matches!(proto.code[ip], Instr::Jump { target } if target == start))
        .map(|ip| ip as u32)
        .ok_or_else(|| "no back-edge in the body".to_string())?;
    // Every string constant counts as loadable (see above).
    let strings: FxHashMap<u32, u64> = proto
        .code
        .iter()
        .filter_map(|i| match *i {
            Instr::LoadConst { idx, .. } => Some((idx, 0)),
            _ => None,
        })
        .collect();
    // An op the region cannot compile inside the loop proper would exit the
    // region on (nearly) every iteration: keep such a loop interpreted.
    // (Out-of-line code may hold them: it runs rarely.)
    if let Some(ip) = (start as usize..=end as usize).find(|&ip| {
        let i = &proto.code[ip];
        py_op_edges(i).is_none()
            && !py_body_op(i)
            && !region_op_admitted(proto, ip, i, start, end, Some(&strings), None, false)
    }) {
        return Err(format!("loop op at {ip}: {:?}", proto.code[ip]));
    }
    // A frame call from compiled code runs its callee on a nested
    // interpreter loop, which costs more than the interpreter's own call; a
    // loop that mostly calls loses more there than the rest of its body
    // gains. Measured on the Python suite: a loop making every iteration's
    // call among fewer than `PY_WORK_PER_CALL` other per-iteration ops ran
    // slower compiled.
    let (calls, work) = py_loop_profile(&proto.code, start as usize, end as usize);
    if calls > 0 && work < PY_WORK_PER_CALL * calls {
        return Err(format!("call-bound: calls={calls} work={work}"));
    }
    let (ext_end, members) = py_region_members(proto, start, end);
    Ok(PyRegionPlan {
        end,
        ext_end,
        members,
        calls,
        work,
    })
}


/// `LoadConst` of a string constant the dispatcher did not intern (out-of-
/// line code of an extended Python region): `Vm::jit_py_load_const`, the
/// interpreter's own memoized `resolve_const_slot` (an allocation the first
/// time, so `refetch` follows).
pub(crate) fn emit_py_load_const(
    ops: &mut dynasmrt::x64::Assembler,
    func_id: u32,
    dst: u16,
    idx: u32,
    refetch: &mut dyn FnMut(&mut dynasmrt::x64::Assembler),
) {
    let packed = ((func_id as u64) << 32) | idx as u64;
    dynasm!(ops
        ; mov rcx, rdi
        ; mov rdx, QWORD packed as i64
        ; mov rax, QWORD crate::vm::Vm::jit_py_load_const as usize as i64
        ; call rax
        ; mov [rbx + dreg(dst)], rax
    );
    refetch(ops);
}

/// A Python body's `PushHandler` (the per-call traceback handler, or a
/// `try`): `Vm::jit_py_push_handler`, the interpreter's arm verbatim on the
/// body's own frame.
pub(crate) fn emit_py_push_handler(ops: &mut dynasmrt::x64::Assembler, catch_target: u32, catch_reg: u16) {
    let packed = ((catch_target as u64) << 16) | catch_reg as u64;
    dynasm!(ops
        ; mov rcx, rdi
        ; mov rdx, QWORD packed as i64
        ; mov rax, QWORD crate::vm::Vm::jit_py_push_handler as usize as i64
        ; call rax
    );
}
