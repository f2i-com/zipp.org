//! The native tiers' out-of-line step for the Python frontend's fused
//! instructions (`Instr::Py*`, see `lang/python/vm/py_ops.rs`).
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
            if vm.at_call_cap() {
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

    /// `PyCall` from compiled code: `jit_py_op(vm, regs, packed)` for the
    /// instruction at `packed = (func_id << 32) | ip`, whose operands come
    /// packed in `ops` (`f | arg_base << 16 | argc << 32 | dst << 48`), with
    /// the same answers. A site whose cache entry holds the runtime's
    /// `__zipp_py_strm` native (a str method, `hash`, `heapq`, `bisect`,
    /// `math.sqrt`: [`Vm::py_strm_hit`]) calls it here, as the step's arm
    /// for it does, without the instruction found and decoded again, the
    /// step's match and the callee's kind found by elimination; any other
    /// call is `jit_py_op`'s. Bounded and exempted as `jit_py_op` bounds a
    /// `PyCall` (the native's fallback `j<n>` is guest code), a GC safe
    /// point as it is, and a raise leaves the frame's ip at the instruction
    /// as the step does.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_call(vm: *mut core::ffi::c_void, regs: *const u64, packed: u64, ops: u64) -> u64 {
        use crate::codegen::{PY_BAIL, PY_NEXT, PY_THREW};
        if vm.is_null() {
            return PY_BAIL;
        }
        let v = unsafe { &mut *(vm as *mut Vm) };
        let mut skip_leaf = v.py_rt.as_deref().is_some_and(|p| p.skip_leaf_once.replace(false));
        let (f, arg_base, argc, dst) = (ops as u16, (ops >> 16) as u16, (ops >> 32) as u16, (ops >> 48) as u16);
        let start = v.regs.as_ptr() as usize;
        let addr = regs as usize;
        if addr >= start && (addr - start) % 8 == 0 {
            let base = (addr - start) / 8;
            let len = v.regs.len();
            if base + (f.max(dst) as usize) < len && base + arg_base as usize + argc as usize <= len {
                let ip = packed as u32 as usize;
                let fv = v.get(base, f);
                let e = v.py_ic((packed >> 32) as u32, ip);
                if let Some((at, op)) = v.py_strm_hit(&e, fv) {
                    if !matches!(op, 6 | 10 | 11 | 12..=19 | 22..=25 | 28..=34) {
                        v.maybe_gc();
                    }
                    // Keep the builtin record's hot mirror settled so planned
                    // inline `PyStrmInline` guards hit on subsequent iterations.
                    let fi = fv.heap_index();
                    if v.heap.hot_mirror_of(fi).shape == crate::shape::DICT && v.ic_obj_ok(fi) {
                        if let HeapObj::Object(m) = v.heap.get(fi) {
                            if !m.is_ctor && m.shape() != crate::shape::DICT {
                                v.heap.refresh_mirror(fi);
                            }
                        }
                    }
                    // Fast path: `py_strm_try_fast_op` never runs guest code,
                    // so avoid `at_call_cap`, `jit_call_depth`, and `with_argv`
                    // closure overhead when it answers directly.
                    let n = argc as usize;
                    let arg_start = base + arg_base as usize;
                    if n <= 4 {
                        let mut argv = [Value::UNDEFINED; 4];
                        argv[..n].copy_from_slice(&v.regs[arg_start..arg_start + n]);
                        if let Some(x) = v.py_strm_try_fast_op(fv, &argv[..n], at, op) {
                            v.set(base, dst, x);
                            return PY_NEXT;
                        }
                    } else {
                        let res = v.with_argv(base, arg_base, argc, |vm, argv| vm.py_strm_try_fast_op(fv, argv, at, op));
                        if let Some(x) = res {
                            v.set(base, dst, x);
                            return PY_NEXT;
                        }
                    }
                    if v.at_call_cap() {
                        v.osr_deopt_exempt = true;
                        return PY_BAIL;
                    }
                    v.jit_call_depth += 1;
                    let r = v.with_argv(base, arg_base, argc, |vm, argv| vm.py_str_method_fallback_entry(fv, argv));
                    v.jit_call_depth -= 1;
                    return match r {
                        Ok(x) => {
                            v.set(base, dst, x);
                            PY_NEXT
                        }
                        Err(t) => {
                            v.py_step_failed(ip);
                            v.osr_deopt_exempt = true;
                            v.jit_thrown_to_sentinel(t);
                            PY_THREW
                        }
                    };
                }
                // Single-byte dispatch by IC classification: `CALL_PLAIN` skips
                // straight to `jit_py_op` with zero extra checks.
                match e.kind {
                    super::py_rt::ic::CALL_STORE => {
                        if v.py_store_call_site(&e, fv, base, arg_base, argc) {
                            v.set(base, dst, Value::NULL);
                            return PY_NEXT;
                        }
                    }
                    super::py_rt::ic::CALL_REC => {
                        if let Some(x) = v.py_rec_call_site(&e, fv, base, arg_base, argc) {
                            v.set(base, dst, x);
                            return PY_NEXT;
                        }
                    }
                    super::py_rt::ic::CALL_LEAF => {
                        if !skip_leaf {
                            if fv.is_heap() {
                                let fi = fv.heap_index();
                                if v.heap.hot_mirror_of(fi).shape == crate::shape::DICT && v.ic_obj_ok(fi) {
                                    if let HeapObj::Object(m) = v.heap.get(fi) {
                                        if !m.is_ctor && m.shape() != crate::shape::DICT {
                                            v.heap.refresh_mirror(fi);
                                        }
                                    }
                                }
                            }
                            if let Some(x) = v.py_leaf_call_site(&e, fv, base, arg_base, argc) {
                                v.set(base, dst, x);
                                return PY_NEXT;
                            }
                            if e.a == fv.bits() {
                                skip_leaf = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if skip_leaf {
            if let Some(p) = v.py_rt.as_deref() {
                p.skip_leaf_once.set(true);
            }
        }
        Self::jit_py_op(vm, regs, packed)
    }

    /// `PyRaise` from compiled code: the step's own `py_raise_step` over the
    /// operands packed in `ops` (`e | rt << 16 | catch << 32`; `slow` comes
    /// back as `PY_SLOW`), without the instruction found and decoded again
    /// and routed through `py_exec`: `PY_SLOW`, `PY_TAKEN` for the catch
    /// target (the frame's own innermost handler caught it), `PY_THREW` (the
    /// throw pending), as `jit_py_op` answers for it. A GC safe point, as
    /// `jit_py_op` is.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_raise(vm: *mut core::ffi::c_void, regs: *const u64, packed: u64, ops: u64) -> u64 {
        use crate::codegen::{PY_BAIL, PY_SLOW, PY_TAKEN, PY_THREW};
        if vm.is_null() {
            return PY_BAIL;
        }
        let v = unsafe { &mut *(vm as *mut Vm) };
        let (e, rt, catch) = (ops as u16, (ops >> 16) as u16, (ops >> 32) as u32);
        let start = v.regs.as_ptr() as usize;
        let addr = regs as usize;
        if addr < start || (addr - start) % 8 != 0 {
            return PY_BAIL;
        }
        let base = (addr - start) / 8;
        if base + (e.max(rt) as usize) >= v.regs.len() {
            return PY_BAIL;
        }
        let ip = packed as u32 as usize;
        // `u32::MAX - 1` stands for the slow edge (never an ip a catch
        // target can be: the catch, when there is one, is a real ip).
        const SLOW: u32 = u32::MAX - 1;
        match v.py_raise_step(base, ip, e, rt, SLOW, catch) {
            Ok(next) if next == SLOW as usize => PY_SLOW,
            Ok(next) if catch != u32::MAX && next == catch as usize => PY_TAKEN,
            Ok(_) => PY_BAIL,
            Err(t) => {
                v.py_step_failed(ip);
                v.jit_thrown_to_sentinel(t);
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
        vm.make_bigint(value.get()).bits()
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
    /// threw. Never allocates on the VM heap.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_get_item(vm: *mut core::ffi::c_void, o: u64, k: u64, seq: u64, dict: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        match vm.py_get_item(Value::from_bits(o), Value::from_bits(k), Value::from_bits(seq), Value::from_bits(dict)) {
            Ok(Some(Some(v))) => v.bits(),
            Ok(Some(None)) => crate::codegen::PY_ABSENT_BITS,
            Ok(None) => crate::codegen::PY_SLOW_BITS,
            Err(t) => vm.jit_thrown_to_sentinel(t),
        }
    }

    /// Non-allocating `PyGlobal` fast hit (`py_global_hit` only): takes all 4
    /// arguments in registers and never allocates (caller skips `refetch`).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_global_hit(vm: *mut core::ffi::c_void, g: u64, rt: u64, packed: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let (func_id, ip) = ((packed >> 32) as u32, packed as u32 as usize);
        match vm.py_global_hit(func_id, ip, Value::from_bits(g), Value::from_bits(rt)) {
            Some(v) => v.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyGlobal` from compiled code: `py_global_hit` first (no allocation),
    /// else `py_global`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_global(vm: *mut core::ffi::c_void, g: u64, rt: u64, packed: u64, key: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let (func_id, ip) = ((packed >> 32) as u32, packed as u32 as usize);
        let (gv, rv) = (Value::from_bits(g), Value::from_bits(rt));
        if let Some(v) = vm.py_global_hit(func_id, ip, gv, rv) {
            return v.bits();
        }
        vm.maybe_gc();
        match vm.py_global(func_id, ip, gv, rv, key as u32) {
            Some(v) => v.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyModGet` from compiled code: `py_mod_get` (see above).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_mod_get(vm: *mut core::ffi::c_void, o: u64, rt: u64, packed: u64, key: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
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
        // An inline site's guard found the receiver's class's mirror
        // cleared (`codegen::emit_py_method_inline`): settled first.
        if keys & crate::codegen::PY_METHOD_SETTLE != 0 {
            vm.py_settle_class_of(Value::from_bits(o));
        }
        let keys = keys & !crate::codegen::PY_METHOD_SETTLE;
        let (func_id, ip) = ((packed >> 32) as u32, packed as u32 as usize);
        let (ov, rv) = (Value::from_bits(o), Value::from_bits(rt));
        if let Some(f) = vm.py_method_hit(func_id, ip, ov, rv) {
            return f.bits();
        }
        vm.maybe_gc();
        match vm.py_method(func_id, ip, ov, rv, keys as u32, (keys >> 32) as u32) {
            Some(f) => f.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyCallEntry` from compiled code: `py_call_entry` (never allocates).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_call_entry(vm: *mut core::ffi::c_void, f: u64, packed: u64, name: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        match vm.py_call_entry((packed >> 32) as u32, packed as u32 as usize, Value::from_bits(f), name as u32) {
            Some(e) => e.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyClassOf` from compiled code: `py_class_of` (never allocates).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_class_of(vm: *mut core::ffi::c_void, o: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        match vm.py_class_of(Value::from_bits(o)) {
            Some(c) => c.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyDictGet` from compiled code: `py_dict_get` (see above); `keys` is
    /// `key | absent << 32`. The value's bits, `PY_ABSENT_BITS` for the
    /// fall-through that leaves `dst` untouched, `PY_SLOW_BITS`, or
    /// `CALL_THREW` (the frame's ip the instruction's, as the step leaves
    /// it).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_dict_get(vm: *mut core::ffi::c_void, o: u64, packed: u64, keys: u64) -> u64 {
        use crate::vm::py_ops::PyDictGot;
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let (func_id, ip) = ((packed >> 32) as u32, packed as u32 as usize);
        let ov = Value::from_bits(o);
        let absent = (keys >> 32) != 0;
        // Zero-GC fast path for proved-absent layouts (`ic::DICT_GET_ABSENT`).
        if let Some(map) = vm.py_inst_map(ov) {
            if let HeapObj::PyAttrs { layout, .. } = vm.heap.get(map) {
                let e = vm.py_ic(func_id, ip);
                if e.kind == super::py_rt::ic::DICT_GET_ABSENT && e.d == *layout {
                    return if absent {
                        crate::codegen::PY_ABSENT_BITS
                    } else {
                        crate::codegen::PY_SLOW_BITS
                    };
                }
            }
        }
        vm.maybe_gc();
        match vm.py_dict_get(func_id, ip, ov, keys as u32, absent) {
            Ok(PyDictGot::Value(v)) => v.bits(),
            Ok(PyDictGot::Absent) => crate::codegen::PY_ABSENT_BITS,
            Ok(PyDictGot::Slow) => crate::codegen::PY_SLOW_BITS,
            Err(t) => {
                vm.py_step_failed(ip);
                vm.jit_thrown_to_sentinel(t)
            }
        }
    }

    /// Direct thin helper for planned non-allocating `__zipp_py_strm` ops
    /// (`6 | 10 | 11 | 12..=19 | 22..=25 | 28..=34`): the caller's inline
    /// x86-64 guard already verified `this`, its shape, heap version, entry
    /// slot, and `strop == op`. `packed = (op as u16) | (argc << 16) | (at << 32)`.
    /// Returns the result bits or `PY_SLOW_BITS` to fall back to `jit_py_call`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_strm_noalloc(
        vm: *mut core::ffi::c_void,
        this: u64,
        packed: u64,
        argv: *const u64,
    ) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let op = (packed as u16) as i32;
        let argc = ((packed >> 16) & 0xFFFF) as usize;
        if argc == 2 && matches!(op, 6 | 18 | 19) {
            let a0 = Value::from_bits(unsafe { *argv });
            let a1 = Value::from_bits(unsafe { *argv.add(1) });
            if let Some(v) = vm.py_strm_affix_find_2(op, a0, a1) {
                return v.bits();
            }
        }
        let at = (packed >> 32) as usize;
        if argc == 0 || argc > 8 {
            return crate::codegen::PY_SLOW_BITS;
        }
        let res = if argc <= 4 {
            let mut args = [Value::UNDEFINED; 4];
            for (i, slot) in args[..argc].iter_mut().enumerate() {
                *slot = Value::from_bits(unsafe { *argv.add(i) });
            }
            vm.py_strm_try_fast_op(Value::from_bits(this), &args[..argc], at, op)
        } else {
            let mut args = [Value::UNDEFINED; 8];
            for (i, slot) in args[..argc].iter_mut().enumerate() {
                *slot = Value::from_bits(unsafe { *argv.add(i) });
            }
            vm.py_strm_try_fast_op(Value::from_bits(this), &args[..argc], at, op)
        };
        match res {
            Some(v) => v.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// Direct thin helper for planned allocating `__zipp_py_strm` ops
    /// (e.g. `strip`, `split`, `replace`, `join`, `lower`, `upper`, `fjoin`):
    /// polls GC once then dispatches directly to `py_strm_try_fast_op`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_strm_alloc(
        vm: *mut core::ffi::c_void,
        this: u64,
        packed: u64,
        argv: *const u64,
    ) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let op = (packed as u16) as i32;
        let argc = ((packed >> 16) & 0xFFFF) as usize;
        let at = (packed >> 32) as usize;
        if op == 20 && argc == 2 {
            let a0 = Value::from_bits(unsafe { *argv });
            let a1 = Value::from_bits(unsafe { *argv.add(1) });
            vm.maybe_gc();
            return match vm.py_fstr_join2(a0, a1) {
                Some(v) => v.bits(),
                None => crate::codegen::PY_SLOW_BITS,
            };
        }
        if argc == 0 || argc > 8 {
            return crate::codegen::PY_SLOW_BITS;
        }
        let res = if argc <= 4 {
            let mut args = [Value::UNDEFINED; 4];
            for (i, slot) in args[..argc].iter_mut().enumerate() {
                *slot = Value::from_bits(unsafe { *argv.add(i) });
            }
            vm.maybe_gc();
            vm.py_strm_try_fast_op(Value::from_bits(this), &args[..argc], at, op)
        } else {
            let mut args = [Value::UNDEFINED; 8];
            for (i, slot) in args[..argc].iter_mut().enumerate() {
                *slot = Value::from_bits(unsafe { *argv.add(i) });
            }
            vm.maybe_gc();
            vm.py_strm_try_fast_op(Value::from_bits(this), &args[..argc], at, op)
        };
        match res {
            Some(v) => v.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// Direct thin helper for planned frameless Python leaf calls
    /// (`emit_py_leaf_inline`): inline guard already verified `fv`, shape,
    /// heap version, and entry slot. Never allocates or pushes a frame (so no
    /// GC poll and no `refetch`). Returns result bits or `PY_SLOW_BITS`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_leaf_noalloc(
        vm: *mut core::ffi::c_void,
        fv_bits: u64,
        packed: u64,
        argv: *const u64,
    ) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let fid = packed as u32;
        let main_end = (packed >> 32) as u16;
        let reg_slots = (packed >> 48) as u8;
        let argc = ((packed >> 56) & 0xF) as usize;
        let params = ((packed >> 60) & 0xF) as u8;
        if argc > 12 {
            return crate::codegen::PY_SLOW_BITS;
        }
        let raw_args = if argc == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(argv, argc) }
        };
        let info = super::py_attr::py_leaf::LeafInfo {
            main_end,
            params,
            reg_slots,
            native_eligible: true,
        };
        match vm.py_leaf_exec_bits(fid, Value::from_bits(fv_bits), info, raw_args) {
            Some(v) => v.bits(),
            None => {
                if let Some(p) = vm.py_rt.as_deref() {
                    p.skip_leaf_once.set(true);
                }
                crate::codegen::PY_SLOW_BITS
            }
        }
    }

    /// Fast non-allocating generator step (`py_gen_next_fast`) called before
    /// `jit_py_gen_next` so hot generator loops skip `maybe_gc()` and `refetch`.
    /// Returns yielded value bits or `PY_SLOW_BITS` to fall back.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_gen_next_fast(vm: *mut core::ffi::c_void, next: u64, this: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let nv = Value::from_bits(next);
        if !nv.is_heap() || !matches!(vm.heap.get(nv.heap_index()), HeapObj::Native(id) if *id == crate::vm::native::PY_GEN) {
            return crate::codegen::PY_SLOW_BITS;
        }
        match vm.py_gen_next_fast(Value::from_bits(this)) {
            Some(v) => v.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// Pre-planned fused `PyNew` + `StorePlan` `__init__` helper: verifies the
    /// cached class/init/template versions directly from `PyRt::new_fused_plans`
    /// and allocates `PyAttrs` + instance `Object` via `alloc_finalized` in one
    /// shot. Returns the new instance bits or `PY_SLOW_BITS` on guard miss.
    #[cfg(all(feature = "jit", target_arch = "x86_64", not(feature = "safe-sandbox")))]
    pub(crate) extern "win64" fn jit_py_new_fused_fast(
        vm: *mut core::ffi::c_void,
        plan_id: u64,
        cls_bits: u64,
        argv: *const u64,
    ) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let (final_layout, count, arg_idx, plan_idx, inst_shape) = {
            let Some(p) = vm.py_rt.as_deref_mut() else {
                return crate::codegen::PY_SLOW_BITS;
            };
            let Some(pl) = p.new_fused_plans.get_mut().get(plan_id as usize) else {
                return crate::codegen::PY_SLOW_BITS;
            };
            if cls_bits != pl.cls_bits {
                return crate::codegen::PY_SLOW_BITS;
            }
            let ci = Value::from_bits(cls_bits).heap_index();
            if vm.heap.version_of(ci) != pl.cls_hver {
                return crate::codegen::PY_SLOW_BITS;
            }
            let cls_ok = match vm.heap.get(ci) {
                HeapObj::Object(m) => {
                    m.val_at(pl.ctor_slot as usize).bits() == pl.want_bits
                        && m.val_at(pl.init_slot as usize).bits() == pl.init_bits
                        && m.val_at(pl.ver_slot as usize).bits() == pl.cls_ver
                }
                _ => false,
            };
            if !cls_ok {
                return crate::codegen::PY_SLOW_BITS;
            }
            let init_idx = Value::from_bits(pl.init_bits).heap_index();
            if vm.heap.version_of(init_idx) != pl.init_hver {
                return crate::codegen::PY_SLOW_BITS;
            }
            let init_ok = match vm.heap.get(init_idx) {
                HeapObj::Object(m) => m.val_at(pl.init_entry_slot as usize).bits() == pl.entry_bits,
                _ => false,
            };
            if !init_ok || vm.heap.version_of(pl.inst_tmpl.heap_index()) != pl.inst_tmpl_hver {
                return crate::codegen::PY_SLOW_BITS;
            }
            (pl.final_layout, pl.count as usize, pl.arg_idx, pl.plan_idx as usize, pl.inst_shape)
        };
        vm.maybe_gc();
        let mut vals = vm.heap.take_arr_buf(count);
        for &ai in &arg_idx[..count] {
            vals.push(Value::from_bits(unsafe { *argv.add(ai as usize - 1) }));
        }
        let dict = Value::heap(vm.heap.alloc_py_attrs(final_layout, vals));
        let Some(p) = vm.py_rt.as_deref() else {
            return crate::codegen::PY_SLOW_BITS;
        };
        let idx = vm.heap.alloc_finalized(
            &p.plans[plan_idx],
            &[Value::from_bits(cls_bits), dict],
            inst_shape,
        );
        vm.realm_born(idx, vm.obj_proto);
        Value::heap(idx).bits()
    }

    /// `PyDictLookup` from compiled code: `py_dict_lookup` (never allocates).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_dict_lookup(vm: *mut core::ffi::c_void, d: u64, k: u64, rt: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        match vm.py_dict_lookup(Value::from_bits(d), Value::from_bits(k), Value::from_bits(rt)) {
            Some(Some(v)) => v.bits(),
            Some(None) => crate::codegen::PY_ABSENT_BITS,
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyCaught` from compiled code: `py_caught` (never allocates).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_caught(vm: *mut core::ffi::c_void, e: u64, line: u64, rt: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        if vm.py_caught(Value::from_bits(e), Value::from_bits(line), Value::from_bits(rt)) {
            e
        } else {
            crate::codegen::PY_SLOW_BITS
        }
    }

    /// `PyExcPop` from compiled code: `py_exc_pop` (never allocates).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_exc_pop(vm: *mut core::ffi::c_void, rt: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        match vm.py_exc_pop(Value::from_bits(rt)) {
            Some(()) => 0,
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyIsInstance` from compiled code: `py_isinstance` (never allocates).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_isinstance(vm: *mut core::ffi::c_void, v: u64, t: u64, rt: u64, packed: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        match vm.py_isinstance((packed >> 32) as u32, packed as u32 as usize, Value::from_bits(v), Value::from_bits(t), Value::from_bits(rt)) {
            Some(r) => Value::bool(r).bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyLen` from compiled code: `py_len` (never allocates; repairs
    /// `arr_mirror` for plain sequences so subsequent iterations hit inline).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_len(vm: *mut core::ffi::c_void, v: u64, rt: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let vv = Value::from_bits(v);
        if let Some((_, items)) = vm.py_seq_parts(vv) {
            if items.is_heap() {
                vm.py_repair_seq_arr_mirror(items.heap_index());
            }
        }
        match vm.py_len(vv, Value::from_bits(rt)).and_then(|n| Value::small_bigint(n as i128)) {
            Some(b) => b.bits(),
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// `PyTruth` from compiled code: `py_truth` (never allocates).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_truth(vm: *mut core::ffi::c_void, v: u64, rt: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        match vm.py_truth(Value::from_bits(v), Value::from_bits(rt)) {
            Some(b) => Value::bool(b).bits(),
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

    /// `PyUnpack` from compiled code: `py_unpack` (never allocates; repairs
    /// `arr_mirror` so subsequent iterations hit inline).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_unpack(vm: *mut core::ffi::c_void, v: u64, rt: u64, n: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        match vm.py_unpack(Value::from_bits(v), Value::from_bits(rt), n as u32) {
            Some(items) => {
                if items.is_heap() {
                    vm.py_repair_seq_arr_mirror(items.heap_index());
                }
                items.bits()
            }
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

    /// `PyGenNext` from compiled code: the step's arm over the operands'
    /// bits (`next`, the generator's step function, and `this`, its
    /// record), for the instruction at `ip` of the running frame. The
    /// yielded value's bits (the STOP sentinel when it finished);
    /// `PY_SLOW_BITS` for the `slow` edge (`next` is not the native step);
    /// `CALL_THREW` when the body raised (the exception pending, the frame's
    /// ip the instruction's, as the step leaves it), and also, with nothing
    /// pending and nothing run, when compiled code's calls are nested too
    /// deep: the emitted code bails on it either way, and a bail with
    /// nothing pending resumes the interpreter at the instruction. The
    /// resume runs guest code on a nested interpreter loop, so it is bounded
    /// and exempted exactly as `jit_py_op` bounds a guest instruction.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_gen_next(vm: *mut core::ffi::c_void, next: u64, this: u64, ip: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let nv = Value::from_bits(next);
        if !nv.is_heap() || !matches!(vm.heap.get(nv.heap_index()), HeapObj::Native(id) if *id == crate::vm::native::PY_GEN) {
            return crate::codegen::PY_SLOW_BITS;
        }
        // Pure non-allocating in-register fast step: no GC safe point or
        // nested interpreter frame needed.
        if let Some(v) = vm.py_gen_next_fast(Value::from_bits(this)) {
            return v.bits();
        }
        vm.maybe_gc();
        if vm.at_call_cap() {
            vm.osr_deopt_exempt = true;
            return crate::codegen::CALL_THREW;
        }
        vm.jit_call_depth += 1;
        let r = vm.py_gen_next(Value::from_bits(this));
        vm.jit_call_depth -= 1;
        match r {
            Ok(v) => v.bits(),
            Err(t) => {
                vm.py_step_failed(ip as usize);
                vm.osr_deopt_exempt = true;
                vm.jit_thrown_to_sentinel(t)
            }
        }
    }

    /// Fast non-allocating `PySetItem` for exact `dict` records (`d[k] = v`):
    /// takes operands directly in registers, skips `maybe_gc()` and `refetch`.
    /// Returns 0 on success or `PY_SLOW_BITS` to fall back to `jit_py_set_item`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_set_item_dict(
        vm: *mut core::ffi::c_void,
        o: u64,
        k: u64,
        v: u64,
        dict: u64,
    ) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        if vm.py_set_item_dict_fast(
            Value::from_bits(o),
            Value::from_bits(k),
            Value::from_bits(v),
            Value::from_bits(dict),
        ) {
            0
        } else {
            crate::codegen::PY_SLOW_BITS
        }
    }

    /// Fused `NewArray { dst: arr_dst, arg_base, argc }` + `PySeq { dst: seq_dst,
    /// items: arr_dst, rt, tuple }` for small literals (`argc <= 8`): allocates
    /// the backing `Array` and the `{cls, items}` sequence record in one FFI
    /// call with a single GC poll and refetch. Writes both `regs[arr_dst]` and
    /// `regs[seq_dst]` and returns `0` on success, or `PY_SLOW_BITS` (with zero
    /// mutations) to fall through to the unfused pair.
    /// `packed = arr_dst | (seq_dst << 16) | (arg_base << 32) | (argc << 48) | (tuple << 52)`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_new_seq(
        vm: *mut core::ffi::c_void,
        regs: *const u64,
        packed: u64,
        rt: u64,
    ) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let start = vm.regs.as_ptr() as usize;
        let addr = regs as usize;
        if addr < start || (addr - start) % 8 != 0 {
            return crate::codegen::PY_SLOW_BITS;
        }
        let base = (addr - start) / 8;
        let arr_dst = packed as u16;
        let seq_dst = (packed >> 16) as u16;
        let arg_base = (packed >> 32) as u16;
        let argc = ((packed >> 48) & 0xF) as usize;
        let tuple = (packed >> 52) != 0;
        if argc > 8 {
            return crate::codegen::PY_SLOW_BITS;
        }
        let arg_end = base + arg_base as usize + argc;
        if arg_end > vm.regs.len()
            || base + arr_dst as usize >= vm.regs.len()
            || base + seq_dst as usize >= vm.regs.len()
        {
            return crate::codegen::PY_SLOW_BITS;
        }
        vm.maybe_gc();
        let (cls, shape, plan_idx) = match vm.py_rt_for(Value::from_bits(rt)) {
            Some(p) => {
                if p.seq_tmpl_hver == u32::MAX
                    || !p.seq_tmpl.is_heap()
                    || vm.heap.version_of(p.seq_tmpl.heap_index()) != p.seq_tmpl_hver
                {
                    return crate::codegen::PY_SLOW_BITS;
                }
                let cls = if tuple { p.t_tuple } else { p.t_list };
                (cls, p.seq_shape, p.seq_plan_idx)
            }
            None => return crate::codegen::PY_SLOW_BITS,
        };
        let mut items = vm.heap.take_arr_buf(argc);
        let astart = base + arg_base as usize;
        items.extend_from_slice(&vm.regs[astart..astart + argc]);
        let arr_idx = vm.heap.alloc_array_settled(items);
        vm.realm_born(arr_idx, vm.arr_proto);
        let arr_val = Value::heap(arr_idx);
        #[cfg(not(feature = "safe-sandbox"))]
        let seq_val = if plan_idx != u16::MAX {
            let Some(p) = vm.py_rt.as_deref() else {
                return crate::codegen::PY_SLOW_BITS;
            };
            let idx = vm.heap.alloc_finalized(&p.plans[plan_idx as usize], &[cls, arr_val], shape);
            vm.realm_born(idx, vm.obj_proto);
            Value::heap(idx)
        } else {
            let Some(v) = vm.py_alloc_seq_fast(cls, arr_val) else {
                return crate::codegen::PY_SLOW_BITS;
            };
            v
        };
        #[cfg(feature = "safe-sandbox")]
        let seq_val = {
            let _ = (shape, plan_idx);
            let Some(v) = vm.py_alloc_seq_fast(cls, arr_val) else {
                return crate::codegen::PY_SLOW_BITS;
            };
            v
        };
        vm.set(base, arr_dst, arr_val);
        vm.set(base, seq_dst, seq_val);
        0
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

    /// `PyNew` from compiled code: the step's arm (`py_new` over the class
    /// and the runtime read from the window at `regs`, its three results
    /// stored there) for the instruction at `site = (func_id << 32) | ip`,
    /// without finding the instruction again: `outs` = `dst | entry << 16 |
    /// this_f << 32`, `ins` = `cls | rt << 16 | n << 32`. 0 done,
    /// `PY_SLOW_BITS` for the `slow` edge (nothing done), `SELF_CALL_DEOPT`
    /// (nothing done) for a window that is not this VM's. A GC safe point,
    /// as `jit_py_op` is.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_new(vm: *mut core::ffi::c_void, regs: *const u64, site: u64, outs: u64, ins: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let start = vm.regs.as_ptr() as usize;
        let addr = regs as usize;
        if addr < start || (addr - start) % 8 != 0 {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let base = (addr - start) / 8;
        let (dst, entry, this_f) = (outs as u16, (outs >> 16) as u16, (outs >> 32) as u16);
        let (cls, rt, n) = (ins as u16, (ins >> 16) as u16, (ins >> 32) as u16);
        if [dst, entry, this_f, cls, rt].iter().any(|&r| base + r as usize >= vm.regs.len()) {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        vm.maybe_gc();
        let (c, r) = (vm.get(base, cls), vm.get(base, rt));
        match vm.py_new((site >> 32) as u32, site as u32 as usize, c, r, n as usize) {
            Some((obj, e, t)) => {
                vm.set(base, dst, obj);
                vm.set(base, entry, e);
                vm.set(base, this_f, t);
                0
            }
            None => crate::codegen::PY_SLOW_BITS,
        }
    }

    /// Fused `PyNew` + immediate `PyCall` of a `StorePlan` `__init__`:
    /// `outs = dst | entry << 16 | this_f << 32 | init_dst << 48`,
    /// `ins = cls | rt << 16 | n << 32`.
    /// Returns `1` when fused construction+init completed (caller jumps past
    /// the following `PyCall`), `0` when plain `py_new` completed (caller
    /// executes `PyCall`), `PY_SLOW_BITS` for slow edge, `SELF_CALL_DEOPT` to bail.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) extern "win64" fn jit_py_new_init(vm: *mut core::ffi::c_void, regs: *const u64, site: u64, outs: u64, ins: u64) -> u64 {
        let vm = unsafe { &mut *(vm as *mut Vm) };
        let start = vm.regs.as_ptr() as usize;
        let addr = regs as usize;
        if addr < start || (addr - start) % 8 != 0 {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let base = (addr - start) / 8;
        let (dst, entry, this_f, init_dst) = (
            outs as u16,
            (outs >> 16) as u16,
            (outs >> 32) as u16,
            (outs >> 48) as u16,
        );
        let (cls, rt, n) = (ins as u16, (ins >> 16) as u16, (ins >> 32) as u16);
        let max_reg = dst.max(entry).max(this_f).max(init_dst).max(cls).max(rt);
        if base + max_reg as usize >= vm.regs.len() || base + dst as usize + n as usize >= vm.regs.len() {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        vm.maybe_gc();
        let (c, r) = (vm.get(base, cls), vm.get(base, rt));
        let (func_id, ip) = ((site >> 32) as u32, site as u32 as usize);
        if let Some(obj) = vm.py_new_store_fused(func_id, ip, c, r, n as usize, base, dst) {
            vm.set(base, dst, obj);
            vm.set(base, init_dst, Value::NULL);
            return 1;
        }
        match vm.py_new(func_id, ip, c, r, n as usize) {
            Some((obj, e, t)) => {
                vm.set(base, dst, obj);
                vm.set(base, entry, e);
                vm.set(base, this_f, t);
                0
            }
            None => crate::codegen::PY_SLOW_BITS,
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
        let was_live = vm.heap.arr_mirror_live(aidx);
        let val = Value::from_bits(val_bits);
        if val.is_heap() {
            vm.heap.write_barrier_val(aidx, val);
        }
        if let HeapObj::Array(items) = vm.heap.get_mut(aidx) {
            items.push(val);
        }
        if was_live {
            vm.heap.repair_arr_mirror(aidx);
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
            | Instr::PyTruth { .. }
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
