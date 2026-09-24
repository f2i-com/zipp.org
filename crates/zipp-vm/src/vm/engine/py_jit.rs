//! The native tiers' out-of-line step for the Python frontend's fused
//! instructions (`Instr::Py*`, see `vm/py_ops.rs`).
//!
//! Compiled code calls [`Vm::jit_py_op`] for any such instruction its inline
//! fast path does not settle. The helper runs exactly the interpreter's step
//! for the instruction at `ip` (`py_step` or `py_step_ext`, routed by
//! [`Vm::py_exec`]) over the caller's register window and reports which of
//! the instruction's successors the interpreter would have taken, so every
//! Python semantic stays in one place.
//!
//! A new fused instruction compiles once it is listed in
//! `codegen::py::{py_op_edges, py_op_regs}` and routed by `py_exec`; until
//! then a loop holding it stays interpreted.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// `jit_py_op(vm, regs, packed)`: run the fused Python instruction at
    /// `packed = (func_id << 32) | ip` over the register window starting at
    /// `regs` (a pointer into the VM's register file), and answer one of the
    /// `codegen::PY_*` outcome codes: fall through, the branch target (a
    /// `PyJumpCompare` that took it), the `slow` edge, a pure decline (nothing
    /// ran: resume the interpreter AT this instruction), or a throw (the
    /// exception is pending; unwind).
    ///
    /// A GC safe point, like every allocating JIT helper: the frame's values
    /// all live in the register file.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_op(vm: *mut core::ffi::c_void, regs: *const u64, packed: u64) -> u64 {
        use crate::codegen::{PY_BAIL, PY_NEXT, PY_SLOW, PY_TAKEN, PY_THREW};
        if vm.is_null() {
            return PY_BAIL;
        }
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let func_id = (packed >> 32) as u32;
        let ip = packed as u32 as usize;
        let func_count = vm.main_func_count.saturating_add(vm.eval_funcs.len());
        if func_id as usize >= func_count {
            return PY_BAIL;
        }
        let proto = vm.func(func_id as usize);
        let Some(instr) = proto.code.get(ip) else {
            return PY_BAIL;
        };
        let Some((slow, target)) = crate::codegen::py_op_edges(instr) else {
            return PY_BAIL;
        };
        // The window must be this VM's register file, register-aligned, with
        // the whole frame inside the live prefix.
        let start = vm.regs.as_ptr() as usize;
        let addr = regs as usize;
        if addr < start || (addr - start) % 8 != 0 {
            return PY_BAIL;
        }
        let base = (addr - start) / 8;
        if base.saturating_add(proto.reg_count as usize) > vm.regs.len() {
            return PY_BAIL;
        }
        vm.maybe_gc();
        let Some(r) = vm.py_exec(func_id, base, ip, instr) else {
            return PY_BAIL;
        };
        match r {
            Ok(next) if next == ip + 1 => PY_NEXT,
            Ok(next) if next == slow as usize => PY_SLOW,
            Ok(next) if target.is_some_and(|t| t as usize == next) => PY_TAKEN,
            // `py_step` answers only the instruction's own successors; any
            // other ip would be a desync, so hand the interpreter the
            // instruction instead (it has had no effect: every fast path
            // either completes and names a successor or changes nothing).
            Ok(_) => PY_BAIL,
            Err(t) => {
                vm.jit_thrown_to_sentinel(t);
                PY_THREW
            }
        }
    }

    /// `LoadBigInt` beyond the interned table (compiled code inlines the
    /// table's values): `make_bigint` of the literal at `packed = (func_id
    /// << 32) | ip`. An allocation, so a GC safe point. `SELF_CALL_DEOPT`
    /// (nothing ran) for anything but that instruction.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_load_bigint(vm: *mut core::ffi::c_void, packed: u64) -> u64 {
        if vm.is_null() {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let func_id = (packed >> 32) as usize;
        let ip = packed as u32 as usize;
        if func_id >= vm.main_func_count.saturating_add(vm.eval_funcs.len()) {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let Some(&Instr::LoadBigInt { value, .. }) = vm.func(func_id).code.get(ip) else {
            return crate::codegen::SELF_CALL_DEOPT;
        };
        vm.maybe_gc();
        vm.make_bigint(value).bits()
    }

    /// `PushHandler` in a compiled Python body: the interpreter arm verbatim
    /// (a catch handler on the active frame, which is this activation's own:
    /// a body with handler ops is only ever entered frame-backed).
    /// `packed = (catch_target << 16) | catch_reg`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_push_handler(vm: *mut core::ffi::c_void, packed: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let top = vm.frames.len() - 1;
        vm.frames[top].handlers.push(Handler::Catch {
            target: (packed >> 16) as u32,
            reg: packed as u16,
        });
        0
    }

    /// `AddInt` in a compiled Python body whose operand is not an Int the
    /// inline path served: the interpreter arm for a Number, or for a BigInt
    /// under `upd` (`i += 1` on a Python int), as `bigint_op`. Anything else
    /// (a BigInt without `upd` is the interpreter's TypeError; an object
    /// needs ToNumeric) is `SELF_CALL_DEOPT`: nothing ran. `packed` is
    /// `imm as u32 | upd << 32`. May allocate: a GC safe point.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_add_int(vm: *mut core::ffi::c_void, a_bits: u64, packed: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let va = Value::from_bits(a_bits);
        let imm = packed as u32 as i32;
        let upd = packed >> 32 != 0;
        if va.is_int() {
            return match va.as_int().checked_add(imm) {
                Some(v) => Value::int(v),
                None => Value::num(va.as_int() as f64 + imm as f64),
            }
            .bits();
        }
        if va.is_double() {
            return Value::num(va.as_f64() + imm as f64).bits();
        }
        if !upd {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        // The common case: a fast-tier int whose sum stays one.
        if va.is_heap() {
            if let HeapObj::BigInt(n) = vm.heap.get(va.heap_index()) {
                if let Some(r) = n.checked_add(imm as i128) {
                    vm.maybe_gc();
                    return vm.make_bigint(r).bits();
                }
            }
        }
        let Some(b) = vm.bigint_val(va) else {
            return crate::codegen::SELF_CALL_DEOPT;
        };
        vm.maybe_gc();
        match vm.bigint_op(
            crate::vm::helpers_misc::BigOp::Add,
            b,
            crate::vm::bigint::BigVal::Small(imm as i128),
        ) {
            Ok(v) => v.bits(),
            Err(t) => vm.jit_thrown_to_sentinel(t),
        }
    }

    /// `JumpIfNotLt`/`JumpIfNotLe` in a compiled Python body whose operands
    /// the inline paths did not settle: 1 when `a < b` (`a <= b` for `le`),
    /// 0 when not, for two primitive numerics (Numbers and BigInts of either
    /// tier, which compare without any observable coercion);
    /// `SELF_CALL_DEOPT` (nothing ran) for anything else.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_rel(vm: *mut core::ffi::c_void, a_bits: u64, b_bits: u64, le: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let (va, vb) = (Value::from_bits(a_bits), Value::from_bits(b_bits));
        // The common case: two fast-tier ints.
        let small = |vm: &Vm, v: Value| -> Option<i128> {
            match v.is_heap().then(|| vm.heap.get(v.heap_index())) {
                Some(HeapObj::BigInt(n)) => Some(*n),
                _ => None,
            }
        };
        if let (Some(x), Some(y)) = (small(vm, va), small(vm, vb)) {
            return if le == 0 { (x < y) as u64 } else { (x <= y) as u64 };
        }
        let numeric = |vm: &Vm, v: Value| v.is_number() || vm.bigint_val(v).is_some();
        if !numeric(vm, va) || !numeric(vm, vb) {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let r = if le == 0 {
            vm.cmp_lt_values(va, vb, true)
        } else if (va.is_number() && va.as_f64().is_nan()) || (vb.is_number() && vb.as_f64().is_nan()) {
            Ok(false)
        } else {
            // Two non-NaN numerics are totally ordered: `a <= b` is `!(b < a)`.
            vm.cmp_lt_values(vb, va, false).map(|lt| !lt)
        };
        match r {
            Ok(b) => b as u64,
            Err(_) => crate::codegen::SELF_CALL_DEOPT,
        }
    }

    /// `PyArith` whose operands the inline paths did not settle:
    /// `py_arith` over the operand bits. The result bits, `PY_SLOW_BITS`
    /// (take the `slow` edge; nothing ran) or `CALL_THREW`. `op` is the
    /// `PyArithOp` discriminant. May allocate: a GC safe point (the operands
    /// are also in the frame's registers).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_arith(vm: *mut core::ffi::c_void, a_bits: u64, b_bits: u64, op: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let Some(op) = crate::codegen::py_arith_op_from(op) else {
            return crate::codegen::PY_SLOW_BITS;
        };
        vm.maybe_gc();
        match vm.py_arith(op, Value::from_bits(a_bits), Value::from_bits(b_bits)) {
            Ok(Some(v)) => v.bits(),
            Ok(None) => crate::codegen::PY_SLOW_BITS,
            Err(t) => vm.jit_thrown_to_sentinel(t),
        }
    }

    /// `PyAddImm` beyond the inline paths: `py_add_imm`. Outcomes as
    /// [`Vm::jit_py_arith`].
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_add_imm(vm: *mut core::ffi::c_void, a_bits: u64, imm: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        match vm.py_add_imm(Value::from_bits(a_bits), imm as u32 as i32) {
            Ok(Some(v)) => v.bits(),
            Ok(None) => crate::codegen::PY_SLOW_BITS,
            Err(t) => vm.jit_thrown_to_sentinel(t),
        }
    }

    /// `PyCompare`/`PyJumpCompare` beyond the inline paths: `py_compare`.
    /// 0 or 1 for the comparison's result, `PY_SLOW` (2) for the `slow` edge
    /// (nothing ran), `PY_BAIL` (3) when it threw (the exception is pending).
    /// `op` is the `PyCmpOp` discriminant. Never allocates.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_cmp(vm: *mut core::ffi::c_void, a_bits: u64, b_bits: u64, op: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let Some(op) = crate::codegen::py_cmp_op_from(op) else {
            return crate::codegen::PY_SLOW;
        };
        match vm.py_compare(op, Value::from_bits(a_bits), Value::from_bits(b_bits)) {
            Ok(Some(r)) => r as u64,
            Ok(None) => crate::codegen::PY_SLOW,
            Err(t) => {
                vm.jit_thrown_to_sentinel(t);
                crate::codegen::PY_BAIL
            }
        }
    }

    /// A plain `ArrayAppend` in a compiled Python body: the interpreter arm
    /// (a write barrier, then a push) when the literal is an Array, else
    /// `SELF_CALL_DEOPT` with nothing done.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_array_append(vm: *mut core::ffi::c_void, arr_bits: u64, val_bits: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let arr = Value::from_bits(arr_bits);
        if !arr.is_heap() || !matches!(vm.heap.get(arr.heap_index()), HeapObj::Array(_)) {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let aidx = arr.heap_index();
        vm.heap.write_barrier(aidx);
        if let HeapObj::Array(items) = vm.heap.get_mut(aidx) {
            items.push(Value::from_bits(val_bits));
        }
        0
    }

    /// The interpreter's own step for a fused Python instruction the native
    /// tiers admit (`codegen::py_op_edges`): `py_step` for the first round of
    /// them, `py_step_ext` for the second, exactly as the dispatch loop
    /// routes them. `None` for any other instruction (each step function
    /// answers an instruction it does not know by falling through, so the
    /// routing must be explicit).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn py_exec(&mut self, func_id: u32, base: usize, ip: usize, instr: &Instr) -> Option<Result<usize, Thrown>> {
        match instr {
            Instr::PyClassOf { .. }
            | Instr::PyDictGet { .. }
            | Instr::PyCallEntry { .. }
            | Instr::PyGetItem { .. }
            | Instr::PySetItem { .. }
            | Instr::PyDictSet { .. }
            | Instr::PyArith { .. }
            | Instr::PyAddImm { .. }
            | Instr::PyCompare { .. }
            | Instr::PyJumpCompare { .. } => Some(self.py_step(func_id, base, ip, instr)),
            Instr::PyGlobal { .. }
            | Instr::PyStrItem { .. }
            | Instr::PyStrLen { .. }
            | Instr::PyGetAttr { .. }
            | Instr::PySetAttr { .. }
            | Instr::PyIsInstance { .. }
            | Instr::PyMethod { .. }
            | Instr::PyModGet { .. }
            | Instr::PyLen { .. }
            | Instr::PyAttrFn { .. }
            | Instr::PyRaise { .. }
            | Instr::PyCaught { .. }
            | Instr::PyClassAttr { .. }
            | Instr::PyDictLookup { .. }
            | Instr::PyUnpack { .. } => Some(self.py_step_ext(func_id, base, ip, instr)),
            #[cfg(feature = "python")]
            Instr::PySeq { .. } => Some(self.py_step_ext(func_id, base, ip, instr)),
            _ => None,
        }
    }

    /// A string constant of `packed = (func_id << 32) | const_idx` for an
    /// extended Python region's out-of-line code: the interpreter's own
    /// memoized `resolve_const_slot` (the value `LoadConst` stores). May
    /// allocate the first time: a GC safe point.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_load_const(vm: *mut core::ffi::c_void, packed: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        vm.resolve_const_slot((packed >> 32) as u32, packed as u32).bits()
    }
}
