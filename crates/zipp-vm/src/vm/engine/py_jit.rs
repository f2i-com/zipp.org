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
        // `PyGenNext` and `PyCall` run guest code on a nested interpreter
        // loop: bounded like a frame call from compiled code
        // (`jit_frame_call`), and a throw from it is not a region-quality
        // signal. So does a `PyGetAttr` / `PySetAttr` whose site holds a
        // property's accessor (it pushes a frame only then).
        let guest = match instr {
            Instr::PyGenNext { .. } | Instr::PyCall { .. } => true,
            Instr::PyGetAttr { .. } | Instr::PySetAttr { .. } => vm.py_attr_site_calls(func_id, ip),
            _ => false,
        };
        if guest {
            if vm.jit_call_depth >= JIT_REGION_CALL_MAX {
                vm.osr_deopt_exempt = true;
                return PY_BAIL;
            }
            vm.jit_call_depth += 1;
        }
        let mut r = vm.py_exec(func_id, base, ip, instr);
        // `PyCall` pushed its callee's frame, or an attribute instruction a
        // property's accessor's (the interpreter's loop would re-enter it):
        // run it to completion here, then fall through. A setter's result
        // is dropped.
        if matches!(r, Some(Ok(usize::MAX))) {
            let dst = match *instr {
                Instr::PyCall { dst, .. } | Instr::PyGetAttr { dst, .. } => Some(dst),
                _ => None,
            };
            r = Some(vm.jit_py_call_run(base, dst).map(|()| ip + 1));
        }
        if guest {
            vm.jit_call_depth -= 1;
            if matches!(r, Some(Err(_))) {
                vm.osr_deopt_exempt = true;
            }
        }
        let Some(r) = r else {
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

    /// Before a loop of `func_id` compiles: hand the JIT the function's
    /// `PyCall` sites whose cache entries name a native callee
    /// (`Vm::py_call_site_native`), for the call-bound gate
    /// (`codegen::set_py_native_calls`). Nothing for a body with no fused
    /// Python instruction, or outside a Python program.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn py_native_call_sites(&self, func_id: u32) {
        if self.py_rt.is_none() || func_id as usize >= self.main_func_count.saturating_add(self.eval_funcs.len()) {
            return;
        }
        let sites: Vec<u32> = self
            .func(func_id as usize)
            .code
            .iter()
            .enumerate()
            .filter(|&(ip, i)| matches!(i, Instr::PyCall { .. }) && self.py_call_site_native(func_id, ip))
            .filter_map(|(ip, _)| u32::try_from(ip).ok())
            .collect();
        crate::codegen::set_py_native_calls(&self.jit as *const crate::codegen::Jit as usize, func_id, sites);
    }

    /// The rest of a `PyCall` from compiled code once `py_step_call` pushed
    /// the callee's frame (or of a `PyGetAttr` / `PySetAttr` that pushed a
    /// property's accessor's): run it to completion as `jit_frame_call`
    /// does (on its compiled whole-function body when it has one, else on a
    /// nested interpreter loop), then store its result in the caller's
    /// `dst` when it has one (`run_loop` returns the stop frame's value
    /// without delivering it). On a throw the callee's frames are unwound
    /// and the exception is pending.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn jit_py_call_run(&mut self, base: usize, dst: Option<u16>) -> Result<(), Thrown> {
        let stop = self.frames.len() - 1;
        let fid = self.frames[stop].func;
        let v = match self.jit_frame_run_native(fid, stop) {
            Some(r) => r,
            None => self.run_loop(stop),
        }?;
        if let Some(dst) = dst {
            self.regs[base + dst as usize] = v;
        }
        Ok(())
    }

    /// `LoadBigInt` beyond the immediate range (compiled code stores an
    /// immediate itself): `make_bigint` of the literal at `packed = (func_id
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
        // The common case: an int in the i128 range whose sum stays one.
        if let Some(n) = vm.bigint_i128(va) {
            if let Some(r) = n.checked_add(imm as i128) {
                vm.maybe_gc();
                return vm.make_bigint(r).bits();
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
        // The common case: two ints in the i128 range.
        if let (Some(x), Some(y)) = (vm.bigint_i128(va), vm.bigint_i128(vb)) {
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

    /// `PyGetItem` from compiled code: `py_get_item` over the operands' bits,
    /// the step's own answer without finding the instruction again: the
    /// item's bits, `PY_SLOW_BITS` for the `slow` edge, `PY_ABSENT_BITS`
    /// for the `absent` edge (a key the dict lacks), `CALL_THREW` when it
    /// threw. A GC safe point, as `jit_py_op` is.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_get_item(vm: *mut core::ffi::c_void, o: u64, k: u64, seq: u64, dict: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        match vm.py_get_item(Value::from_bits(o), Value::from_bits(k), Value::from_bits(seq), Value::from_bits(dict)) {
            Ok(Some(Some(v))) => v.bits(),
            Ok(Some(None)) => crate::codegen::PY_ABSENT_BITS,
            Ok(None) => crate::codegen::PY_SLOW_BITS,
            Err(t) => vm.jit_thrown_to_sentinel(t),
        }
    }

    // The fused instructions that only answer a value or take their slow
    // edge (they run no guest code, raise nothing and change nothing when
    // they decline), from compiled code: each helper below is the step's own
    // function for one of them over the operands' bits, with the site
    // (`packed = (func_id << 32) | ip`, for its cache entry) where that
    // function takes one, and answers the value's bits or `PY_SLOW_BITS`
    // for the `slow` edge. `jit_py_op` reaches the same function after
    // finding the code object and the instruction again, decoding its
    // edges, checking the window and routing it through `py_exec` and the
    // step's match. Each is a GC safe point, as `jit_py_op` is (the
    // operands are also in the frame's registers).

    /// `PyGlobal` from compiled code: `py_global` (see above).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_global(vm: *mut core::ffi::c_void, g: u64, rt: u64, packed: u64, key: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        match vm.py_global((packed >> 32) as u32, packed as u32 as usize, Value::from_bits(g), Value::from_bits(rt), key as u32) {
            Some(v) => v.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyModGet` from compiled code: `py_mod_get` (see above).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_mod_get(vm: *mut core::ffi::c_void, o: u64, rt: u64, packed: u64, key: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        match vm.py_mod_get((packed >> 32) as u32, packed as u32 as usize, Value::from_bits(o), Value::from_bits(rt), key as u32) {
            Some(v) => v.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyMethod` from compiled code: `py_method` (see above); `keys` is
    /// `key | gb << 32`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_method(vm: *mut core::ffi::c_void, o: u64, rt: u64, packed: u64, keys: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        match vm.py_method((packed >> 32) as u32, packed as u32 as usize, Value::from_bits(o), Value::from_bits(rt), keys as u32, (keys >> 32) as u32) {
            Some(f) => f.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyIsInstance` from compiled code: `py_isinstance` (see above), its
    /// answer as the bool the step stores.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_isinstance(vm: *mut core::ffi::c_void, v: u64, t: u64, rt: u64, packed: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        match vm.py_isinstance((packed >> 32) as u32, packed as u32 as usize, Value::from_bits(v), Value::from_bits(t), Value::from_bits(rt)) {
            Some(r) => Value::bool(r).bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyLen` from compiled code: `py_len` (see above), its answer as the
    /// int the step stores.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_len(vm: *mut core::ffi::c_void, v: u64, rt: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        match vm.py_len(Value::from_bits(v), Value::from_bits(rt)) {
            Some(n) => vm.make_bigint(n as i128).bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PySeq` from compiled code: `py_seq` (see above); `tuple` is 0 or 1.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_seq(vm: *mut core::ffi::c_void, items: u64, rt: u64, tuple: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        match vm.py_seq(Value::from_bits(items), Value::from_bits(rt), tuple != 0) {
            Some(v) => v.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyUnpack` from compiled code: `py_unpack` (see above).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_unpack(vm: *mut core::ffi::c_void, v: u64, rt: u64, n: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        match vm.py_unpack(Value::from_bits(v), Value::from_bits(rt), n as u32) {
            Some(items) => items.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyMakeExc` from compiled code: `py_make_exc` (see above).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_make_exc(vm: *mut core::ffi::c_void, cls: u64, args: u64, rt: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        vm.maybe_gc();
        match vm.py_make_exc(Value::from_bits(cls), Value::from_bits(args), Value::from_bits(rt)) {
            Some(e) => e.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PySetItem` from compiled code: `py_set_item` over the registers the
    /// instruction names in the window at `regs` (`packed` = `o | k << 16 |
    /// v << 32 | seq << 48`, then `dict`), the step's own answer without
    /// finding the instruction again: 0 done, `PY_SLOW_BITS` for the `slow`
    /// edge, `CALL_THREW` when it threw (`SELF_CALL_DEOPT`, nothing done,
    /// for a window that is not this VM's). A GC safe point, as `jit_py_op`
    /// is.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_set_item(vm: *mut core::ffi::c_void, regs: *const u64, packed: u64, dict: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let start = vm.regs.as_ptr() as usize;
        let addr = regs as usize;
        if addr < start || (addr - start) % 8 != 0 {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let base = (addr - start) / 8;
        let reg = |shift: u32| (packed >> shift) as u16;
        let (o, k, v, seq, dict) = (reg(0), reg(16), reg(32), reg(48), dict as u16);
        if [o, k, v, seq, dict].iter().any(|&r| base + r as usize >= vm.regs.len()) {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        vm.maybe_gc();
        let (ov, kv, vv) = (vm.get(base, o), vm.get(base, k), vm.get(base, v));
        let (sc, dc) = (vm.get(base, seq), vm.get(base, dict));
        match vm.py_set_item(ov, kv, vv, sc, dc) {
            Ok(true) => 0,
            Ok(false) => crate::codegen::PY_SLOW_BITS,
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
    /// them, `py_step_ext` for the second, `py_step_ext2` for the third,
    /// exactly as the dispatch loop
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
            | Instr::PyGenNext { .. }
            | Instr::PyUnpack { .. } => Some(self.py_step_ext(func_id, base, ip, instr)),
            #[cfg(feature = "python")]
            Instr::PySeq { .. } => Some(self.py_step_ext(func_id, base, ip, instr)),
            Instr::PyMakeExc { .. } | Instr::PyExcPop { .. } | Instr::PyNew { .. } => {
                Some(self.py_step_ext2(func_id, base, ip, instr))
            }
            // `Ok(usize::MAX)` when it pushed the callee's frame.
            Instr::PyCall { .. } => Some(self.py_step_call(func_id, base, ip, instr)),
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
