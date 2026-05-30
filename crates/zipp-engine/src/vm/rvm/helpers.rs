//! Cold helpers called from the register dispatch loop.
//!
//! The [`crate::vm::rvm`] dispatch match is intentionally all-inline so
//! rustc can lower it to a jump table; anything that only runs on
//! fallback paths — full add/mul coercion, string-rope concatenation,
//! slow equality/comparison, mutual-recursion call trampolines, cross-
//! frame try/catch unwinding — moved here in 0.7. Every function is
//! `#[cold]` (or effectively so) and the register VM only jumps here
//! after its fast-path type checks fail.
//!
//! Split out of `rvm.rs` so the 3 000-line hot dispatch loop can sit
//! alone in `mod.rs`. `impl VM` in a sibling submodule.

use std::rc::Rc;

use crate::object::{make_array, make_hash, BuiltinFunction, Object, PromiseState};
use crate::rcode::ROp;
use crate::value::{obj_into_val, val_as_obj_ref, val_inspect, val_to_obj, Value};
use crate::vm::{VMError, VM};

impl VM {
    // ── Cold slow-path helpers ────────────────────────────────────────
    // ── Cold helpers ────────────────────────────────────────────────
    // Outlined from the dispatch loop to reduce register pressure in hot paths.

    /// Create a generator from a Call/CallGlobal/CallMethod opcode.
    /// Outlined from the dispatch loop since generators are rare.
    #[cold]
    #[inline(never)]
    pub(super) fn create_generator_from_call(
        &mut self,
        callee_val: Value,
        receiver: Option<Value>,
        args_ptr: *const Value,
        nargs: usize,
    ) -> Value {
        let func_clone = val_to_obj(callee_val, &self.heap);
        let func_obj = match func_clone {
            Object::CompiledFunction(f) => (*f).clone(),
            Object::BoundMethod(b) => b.function.clone(),
            _ => return Value::UNDEFINED,
        };
        let mut gen_args = Vec::with_capacity(nargs);
        for i in 0..nargs {
            gen_args.push(unsafe { *args_ptr.add(i) });
        }
        self.create_generator(func_obj, gen_args, receiver)
    }

    #[cold]
    #[inline(never)]
    pub(super) fn add_slow(&mut self, left: Value, right: Value) -> Result<Value, VMError> {
        // Check for Instance with valueOf/toString before falling to immutable path
        let left = self.coerce_instance_for_arithmetic(left)?;
        let right = self.coerce_instance_for_arithmetic(right)?;
        let lo = val_as_obj_ref(left, &self.heap);
        let ro = val_as_obj_ref(right, &self.heap);
        let result = self.add_objects(&lo, &ro)?;
        Ok(obj_into_val(result, &mut self.heap))
    }

    /// Handle Add when left is a heap ref or inline string.
    /// Checks if left is a string (heap or inline) and dispatches to
    /// string concat with scratch buffer, or falls back to add_slow.
    /// NOT #[cold] — called frequently in string concatenation benchmarks.
    #[inline(never)]
    pub(crate) fn add_string_or_object(&mut self, left: Value, right: Value) -> Result<Value, VMError> {
        // ── Bitwise fast path: inline_str + inline_str → inline_str ──
        // When both are NaN-boxed inline strings and combined length ≤ 6,
        // merge them with pure bitwise ops — zero allocation, zero function calls.
        if left.is_inline_str() && right.is_inline_str() {
            let left_len = left.inline_str_len();
            let right_len = right.inline_str_len();
            let total = left_len + right_len;
            if total <= 6 {
                // Left payload has bytes at bits [47:47-left_len*8+1], right-padded with zeros.
                // Right payload needs to be shifted right by left_len bytes and OR'd in.
                const PAYLOAD_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
                let left_payload = left.bits() & PAYLOAD_MASK;
                let right_payload = right.bits() & PAYLOAD_MASK;
                let merged = left_payload | (right_payload >> (left_len * 8));
                return Ok(Value::from_bits(
                    (left.bits() & !PAYLOAD_MASK) | merged,
                ));
            }
        }

        // ── Rope fast path: string + string → deferred concatenation ──
        // Check if both sides are string-like (String, StringRope, or inline str).
        // If so, create a rope node instead of materializing.
        let left_is_str = left.is_inline_str()
            || (left.is_heap()
                && matches!(
                    unsafe { &*self.heap.objects.as_ptr().add(left.heap_index() as usize) },
                    Object::String(_) | Object::StringRope(_)
                ));
        if left_is_str {
            let right_is_str = right.is_inline_str()
                || (right.is_heap()
                    && matches!(
                        unsafe { &*self.heap.objects.as_ptr().add(right.heap_index() as usize) },
                        Object::String(_) | Object::StringRope(_)
                    ));
            if right_is_str {
                let left_len = crate::object::string_val_len(left, &self.heap);
                let right_len = crate::object::string_val_len(right, &self.heap);
                let total = left_len + right_len;
                if total > crate::vm::MAX_STRING_LENGTH {
                    return Err(VMError::TypeError(crate::vm::ERR_STRING_LEN.to_string()));
                }
                // Materialize threshold: was 16 (justified by Map keys
                // wanting a flat string anyway), but the `s = s + 'a'`
                // pattern then pays N heap allocs to climb past it.
                // Drop to 8 — covers single-call key concatenations while
                // letting tight-loop concats rope from much earlier.
                if total <= 8 {
                    return self.add_string_materialize(left, right);
                }
                let rope = Object::StringRope(crate::object::StringRopeNode {
                    left,
                    right,
                    total_len: total,
                });
                return Ok(obj_into_val(rope, &mut self.heap));
            }
            // Right is not a string — try string + i32 fast path (very common: "key" + i)
            if right.is_i32() {
                let b = unsafe { right.as_i32_unchecked() };
                let mut ibuf = itoa::Buffer::new();
                let right_str = ibuf.format(b);
                if let Some(v) = Value::from_inline_str(right_str.as_bytes()) {
                    let left_len = crate::object::string_val_len(left, &self.heap);
                    let total = left_len + right_str.len();
                    if total <= 8 {
                        return self.add_string_materialize(left, v);
                    }
                    let rope = Object::StringRope(crate::object::StringRopeNode {
                        left,
                        right: v,
                        total_len: total,
                    });
                    return Ok(obj_into_val(rope, &mut self.heap));
                }
                // Number string > 5 bytes inline: materialize directly for short results
                let left_len = crate::object::string_val_len(left, &self.heap);
                let total = left_len + right_str.len();
                if total <= 8 {
                    let right_val = obj_into_val(
                        Object::String(Rc::from(right_str)),
                        &mut self.heap,
                    );
                    return self.add_string_materialize(left, right_val);
                }
                let right_val = obj_into_val(
                    Object::String(Rc::from(right_str)),
                    &mut self.heap,
                );
                let rope = Object::StringRope(crate::object::StringRopeNode {
                    left,
                    right: right_val,
                    total_len: total,
                });
                return Ok(obj_into_val(rope, &mut self.heap));
            }
        }
        // ── Fallback: materialize via buffer ──
        self.add_string_materialize(left, right)
    }

    #[inline(never)]
    pub(super) fn add_string_materialize(&mut self, left: Value, right: Value) -> Result<Value, VMError> {
        self.string_concat_buf.clear();
        if left.is_heap() {
            let lo_ref = unsafe { &*self.heap.objects.as_ptr().add(left.heap_index() as usize) };
            match lo_ref {
                Object::String(a) => self.string_concat_buf.push_str(a),
                Object::StringRope(rope) => {
                    self.string_concat_buf
                        .push_str(&crate::object::flatten_rope(rope, &self.heap));
                }
                _ => return self.add_slow(left, right),
            }
        } else if left.is_inline_str() {
            let (lbuf, llen) = left.inline_str_buf();
            let a = unsafe { std::str::from_utf8_unchecked(&lbuf[..llen]) };
            self.string_concat_buf.push_str(a);
        } else {
            return self.add_slow(left, right);
        }

        if right.is_heap() {
            let ro_ref = unsafe { &*self.heap.objects.as_ptr().add(right.heap_index() as usize) };
            match ro_ref {
                Object::String(b) => self.string_concat_buf.push_str(b),
                Object::StringRope(rope) => {
                    self.string_concat_buf
                        .push_str(&crate::object::flatten_rope(rope, &self.heap));
                }
                Object::Instance(_) => {
                    let saved_buf = std::mem::take(&mut self.string_concat_buf);
                    let coerced = self.coerce_to_string_val(right)?;
                    self.string_concat_buf = saved_buf;
                    self.string_concat_buf.push_str(&coerced);
                }
                _ => {
                    let lo = Object::String(Rc::from(self.string_concat_buf.as_str()));
                    let result = self.add_objects(&lo, ro_ref)?;
                    return Ok(obj_into_val(result, &mut self.heap));
                }
            }
        } else if right.is_inline_str() {
            let (rbuf, rlen) = right.inline_str_buf();
            let b = unsafe { std::str::from_utf8_unchecked(&rbuf[..rlen]) };
            self.string_concat_buf.push_str(b);
        } else if right.is_i32() {
            let b = unsafe { right.as_i32_unchecked() };
            let mut buf = itoa::Buffer::new();
            self.string_concat_buf.push_str(buf.format(b));
        } else {
            let lo = Object::String(Rc::from(self.string_concat_buf.as_str()));
            let ro = val_as_obj_ref(right, &self.heap);
            let result = self.add_objects(&lo, &ro)?;
            return Ok(obj_into_val(result, &mut self.heap));
        }

        let len = self.string_concat_buf.len();
        if len > crate::vm::MAX_STRING_LENGTH {
            return Err(VMError::TypeError(crate::vm::ERR_STRING_LEN.to_string()));
        }
        // Short strings: pack into NaN-boxed inline string (no heap alloc)
        if let Some(v) = Value::from_inline_str(self.string_concat_buf.as_bytes()) {
            return Ok(v);
        }
        // Intern from buffer: reuses canonical Rc, avoids creating temporary
        Ok(obj_into_val(
            Object::String(crate::intern::intern_str(self.string_concat_buf.as_str())),
            &mut self.heap,
        ))
    }

    #[cold]
    #[inline(never)]
    pub(super) fn exec_append_spread(&mut self, arr_val: Value, spread_val: Value) -> Result<(), VMError> {
        if !arr_val.is_heap() {
            return Err(VMError::TypeError(crate::vm::ERR_SPREAD_TARGET.to_string()));
        }
        let arr_heap = unsafe { &*self.heap.objects.as_ptr().add(arr_val.heap_index() as usize) };
        let Object::Array(arr) = arr_heap else {
            return Err(VMError::TypeError(crate::vm::ERR_SPREAD_TARGET.to_string()));
        };
        // Every push through this function caps the target array at
        // MAX_ARRAY_SIZE. Without this, `[...g()]` for an infinite
        // generator or `[...hugeSet]` grows unboundedly — the heap-
        // bytes check only fires every ~16 K instructions and a
        // simple generator yielding an inline integer allocates
        // nothing on the heap per yield.
        let cap_err =
            || VMError::TypeError(crate::vm::ERR_ARRAY_SIZE.to_string());
        if spread_val.is_inline_str() {
            let (buf, len) = spread_val.inline_str_buf();
            let s = unsafe { std::str::from_utf8_unchecked(&buf[..len]) };
            let borrowed = unsafe { arr.borrow_mut() };
            for ch in s.chars() {
                if borrowed.len() >= crate::vm::MAX_ARRAY_SIZE {
                    return Err(cap_err());
                }
                borrowed.push(obj_into_val(Object::String(ch.to_string().into()), &mut self.heap));
            }
            return Ok(());
        }
        if !spread_val.is_heap() {
            return Err(VMError::TypeError("spread source is not iterable".to_string()));
        }
        let spread_heap = unsafe { &*self.heap.objects.as_ptr().add(spread_val.heap_index() as usize) };
        match spread_heap {
            Object::Array(items) => {
                let src = items.borrow();
                let dst = unsafe { arr.borrow_mut() };
                if dst.len().saturating_add(src.len()) > crate::vm::MAX_ARRAY_SIZE {
                    return Err(cap_err());
                }
                dst.extend(src.iter().copied());
            }
            Object::String(s) => {
                let borrowed = unsafe { arr.borrow_mut() };
                for ch in s.chars() {
                    if borrowed.len() >= crate::vm::MAX_ARRAY_SIZE {
                        return Err(cap_err());
                    }
                    borrowed.push(obj_into_val(Object::String(ch.to_string().into()), &mut self.heap));
                }
            }
            Object::Set(set_obj) => {
                let borrowed = unsafe { arr.borrow_mut() };
                for key in set_obj.entries.borrow().iter() {
                    if borrowed.len() >= crate::vm::MAX_ARRAY_SIZE {
                        return Err(cap_err());
                    }
                    borrowed.push(obj_into_val(self.object_from_hash_key(key), &mut self.heap));
                }
            }
            Object::Map(map_obj) => {
                let borrowed = unsafe { arr.borrow_mut() };
                for (k, v) in map_obj.entries.borrow().iter() {
                    if borrowed.len() >= crate::vm::MAX_ARRAY_SIZE {
                        return Err(cap_err());
                    }
                    borrowed.push(obj_into_val(
                        make_array(vec![obj_into_val(self.object_from_hash_key(k), &mut self.heap), *v]),
                        &mut self.heap,
                    ));
                }
            }
            Object::Generator(gen_rc) => {
                let gen_rc = gen_rc.clone();
                loop {
                    let result = self.execute_generator_next(&gen_rc, Value::UNDEFINED)?;
                    let result_obj = val_to_obj(result, &self.heap);
                    match result_obj {
                        Object::Hash(h) => {
                            let hb = h.borrow();
                            let done = hb.get_by_str("done").map(|v| { let obj = val_to_obj(v, &self.heap); self.is_truthy(&obj) }).unwrap_or(false);
                            if done { break; }
                            let value = hb.get_by_str("value").unwrap_or(Value::UNDEFINED);
                            {
                                let borrowed = unsafe { arr.borrow_mut() };
                                if borrowed.len() >= crate::vm::MAX_ARRAY_SIZE {
                                    return Err(cap_err());
                                }
                                borrowed.push(value);
                            }
                        }
                        _ => break,
                    }
                }
            }
            Object::Null | Object::Undefined => {
                // Spreading null/undefined is a no-op (graceful)
            }
            _ => {
                // Non-iterable spread: treat as empty (graceful degradation)
            }
        }
        Ok(())
    }

    /// Coerce both operands to f64, apply the operation, and re-box the result
    /// (as i64 if the result is an exact integer, f64 otherwise).
    #[cold]
    #[inline(never)]
    pub(super) fn numeric_binop_slow(&mut self, left: Value, right: Value, op: fn(f64, f64) -> f64) -> Result<Value, VMError> {
        let a = self.coerce_to_number_val(left)?;
        let b = self.coerce_to_number_val(right)?;
        let out = op(a, b);
        Ok(if out.is_finite() && out.fract() == 0.0 {
            Value::from_i64(out as i64)
        } else {
            Value::from_f64(out)
        })
    }

    #[cold] #[inline(never)]
    pub(super) fn sub_slow(&mut self, left: Value, right: Value) -> Result<Value, VMError> {
        self.numeric_binop_slow(left, right, |a, b| a - b)
    }

    #[cold] #[inline(never)]
    pub(super) fn mul_slow(&mut self, left: Value, right: Value) -> Result<Value, VMError> {
        self.numeric_binop_slow(left, right, |a, b| a * b)
    }

    #[cold] #[inline(never)]
    pub(super) fn div_slow(&mut self, left: Value, right: Value) -> Result<Value, VMError> {
        // Division always returns f64 (JS semantics: 10 / 2 → 5.0, not 5)
        let a = self.coerce_to_number_val(left)?;
        let b = self.coerce_to_number_val(right)?;
        Ok(Value::from_f64(a / b))
    }

    #[cold] #[inline(never)]
    pub(super) fn mod_slow(&mut self, left: Value, right: Value) -> Result<Value, VMError> {
        self.numeric_binop_slow(left, right, |a, b| a % b)
    }

    /// Coerce an Instance to a primitive via valueOf()/toString() for arithmetic.
    /// For `+` operator which can result in either string concat or numeric add,
    /// we need to return the coerced Value (not just f64).
    pub(super) fn coerce_instance_for_arithmetic(&mut self, val: Value) -> Result<Value, VMError> {
        if val.is_heap() {
            let heap_idx = val.heap_index() as usize;
            let heap_obj = unsafe { &*self.heap.objects.as_ptr().add(heap_idx) };
            if let Object::Instance(inst) = heap_obj {
                // Try valueOf first (default hint for +)
                if let Some(value_of_func) = inst.methods.get("valueOf").cloned() {
                    let (result, _) = self.execute_compiled_function_slice(
                        value_of_func,
                        &[],
                        Some(val),
                    )?;
                    return Ok(result);
                }
                // Fall back to toString
                if let Some(to_str_func) = inst.methods.get("toString").cloned() {
                    let (result, _) = self.execute_compiled_function_slice(
                        to_str_func,
                        &[],
                        Some(val),
                    )?;
                    return Ok(result);
                }
            }
        }
        Ok(val)
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn comparison_slow(&mut self, op: ROp, left: Value, right: Value) -> Result<bool, VMError> {
        // Coerce Instance objects via valueOf before comparison
        let left = self.coerce_instance_for_arithmetic(left)?;
        let right = self.coerce_instance_for_arithmetic(right)?;
        let lo = val_to_obj(left, &self.heap);
        let ro = val_to_obj(right, &self.heap);
        let result = self.eval_comparison(op, &lo, &ro)?;
        Ok(if result.is_bool() {
            unsafe { result.as_bool_unchecked() }
        } else {
            false
        })
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn equality_slow(&mut self, left: Value, right: Value) -> bool {
        // Coerce Instance objects via valueOf for loose equality
        let left = match self.coerce_instance_for_arithmetic(left) {
            Ok(v) => v,
            Err(_) => left,
        };
        let right = match self.coerce_instance_for_arithmetic(right) {
            Ok(v) => v,
            Err(_) => right,
        };
        let lo = val_as_obj_ref(left, &self.heap);
        let ro = val_as_obj_ref(right, &self.heap);
        self.equals(&lo, &ro)
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn strict_equality_slow(&mut self, left: Value, right: Value) -> bool {
        let lo = val_as_obj_ref(left, &self.heap);
        let ro = val_as_obj_ref(right, &self.heap);
        Self::strict_equal(&lo, &ro)
    }

    #[cold]
    #[inline(never)]
    pub(super) fn bitwise_slow(&mut self, op: ROp, left: Value, right: Value) -> Result<Value, VMError> {
        let lo = val_as_obj_ref(left, &self.heap);
        let ro = val_as_obj_ref(right, &self.heap);
        let result = self.eval_bitwise(op, &lo, &ro)?;
        Ok(obj_into_val(result, &mut self.heap))
    }

    #[cold]
    #[inline(never)]
    pub(super) fn pow_impl(&mut self, lv: Value, rv: Value) -> Result<Value, VMError> {
        let (base_n, exp_n) = if lv.is_number() && rv.is_number() {
            (lv.to_number(), rv.to_number())
        } else {
            let lo = val_as_obj_ref(lv, &self.heap);
            let ro = val_as_obj_ref(rv, &self.heap);
            (self.to_number(&lo)?, self.to_number(&ro)?)
        };
        let out = base_n.powf(exp_n);
        Ok(if out.is_finite() && out.fract() == 0.0 {
            Value::from_i64(out as i64)
        } else {
            Value::from_f64(out)
        })
    }

    #[cold]
    #[inline(never)]
    pub(super) fn fused_add_const_slow(&mut self, lv: Value, const_idx: usize) -> Result<Value, VMError> {
        let lo = val_as_obj_ref(lv, &self.heap);
        let cv = unsafe { (&*self.constants_raw).get_unchecked(const_idx) };
        let result = self.add_objects(&lo, cv)?;
        Ok(obj_into_val(result, &mut self.heap))
    }

    #[cold]
    #[inline(never)]
    pub(super) fn fused_sub_const_slow(&mut self, lv: Value, const_idx: usize) -> Result<Value, VMError> {
        let cv = unsafe { *self.constants_values_ptr.add(const_idx) };
        self.sub_slow(lv, cv)
    }

    #[cold]
    #[inline(never)]
    pub(super) fn fused_mul_const_slow(&mut self, lv: Value, const_idx: usize) -> Result<Value, VMError> {
        let cv = unsafe { *self.constants_values_ptr.add(const_idx) };
        self.mul_slow(lv, cv)
    }

    #[cold]
    #[inline(never)]
    pub(super) fn fused_test_lt_slow(&mut self, lv: Value, const_idx: usize) -> Result<bool, VMError> {
        let lo = val_as_obj_ref(lv, &self.heap);
        let cv = unsafe { (&*self.constants_raw).get_unchecked(const_idx) };
        self.compare_numeric(&lo, cv, crate::vm::NumericCmp::Lt)
    }

    #[cold]
    #[inline(never)]
    pub(super) fn fused_test_le_slow(&mut self, lv: Value, const_idx: usize) -> Result<bool, VMError> {
        let lo = val_as_obj_ref(lv, &self.heap);
        let cv = unsafe { (&*self.constants_raw).get_unchecked(const_idx) };
        self.compare_numeric(&lo, cv, crate::vm::NumericCmp::Le)
    }

    #[cold]
    #[inline(never)]
    pub(super) fn fused_mod_strict_eq_slow(
        &mut self,
        lv: Value,
        mod_idx: usize,
        cmp_idx: usize,
    ) -> Result<bool, VMError> {
        let lo = val_as_obj_ref(lv, &self.heap);
        let mod_const = unsafe { (&*self.constants_raw).get_unchecked(mod_idx) };
        let mod_result = self.mod_objects(&lo, mod_const)?;
        let cmp_const = unsafe { (&*self.constants_raw).get_unchecked(cmp_idx) };
        Ok(self.strict_equals(&mod_result, cmp_const))
    }

    /// Check if there's an active try handler for the given error.
    /// If so, store the exception and return Ok((catch_ip, reg_base)).
    /// The caller should set ip, reg_base, regs, inst accordingly.
    #[cold]
    /// Attempt to catch an error using the current try_handlers.
    /// Returns Ok(Some((catch_ip, reg_base))) if caught (caller should jump to catch_ip).
    /// Returns Err(e) if no handler is available (propagate up).
    /// Attempt to catch an error using the current try_handlers.
    /// Returns Ok(Some((catch_ip, reg_base))) if caught — caller should jump to catch_ip
    /// and update all cached local variables (inst, cvals, csyms, icache, regs, reg_base).
    /// Returns Err(e) if no handler is available (propagate up).
    pub(super) fn try_catch_error(
        &mut self,
        err: VMError,
    ) -> Result<Option<(usize, usize)>, VMError> {
        if self.try_handlers.is_empty() {
            return Err(err);
        }
        let (catch_ip, exc_reg, h_inst, h_len, h_base, h_rframes, h_consts) =
            self.try_handlers.pop().unwrap();
        // Unwind rframes to the try entry point
        while self.rframes.len() > h_rframes {
            let frame = self.rframes.pop().unwrap();
            for &(slot, old_val) in &frame.closure_saves {
                unsafe { self.globals.set_unchecked(slot as usize, old_val) };
            }
        }
        // Store the exception value in the catch register
        let exc_val = match &err {
            VMError::Throw(val) => *val,
            other => {
                let msg = format!("{:?}", other);
                obj_into_val(Object::String(Rc::from(msg.as_str())), &mut self.heap)
            }
        };
        if h_base + (exc_reg as usize) < self.stack.len() {
            self.stack[h_base + exc_reg as usize] = exc_val;
        }
        // Restore instruction context to the try handler's function
        self.inst_ptr = h_inst;
        self.inst_len = h_len;
        self.constants_raw = h_consts;
        // Reconvert constants for the restored function
        self.preconvert_constants();
        self.sp = h_base + (self.register_count as usize).max(1);
        Ok(Some((catch_ip, h_base)))
    }

    // ── Cold dispatch-arm slow paths ─────────────────────────────────────
    // These extract the cold/fallback bodies from large match arms (Call,
    // CallMethod, AddConstToRegProp, AddRegPropsToRegProp) so the hot fast
    // paths stay tight in the L1 i-cache.

    #[cold]
    #[inline(never)]
    pub(super) fn call_slow(
        &mut self,
        callee_val: Value,
        arg_start: usize,
        nargs: usize,
    ) -> Result<Value, VMError> {
        let arg_slice =
            unsafe { std::slice::from_raw_parts(self.stack.as_ptr().add(arg_start), nargs) };
        self.call_value_slice(callee_val, arg_slice)
    }

    #[cold]
    #[inline(never)]
    pub(super) fn call_method_slow(
        &mut self,
        obj_val: Value,
        prop_idx: usize,
        cache_slot: usize,
        nargs: usize,
        arg_start: usize,
    ) -> Result<Value, VMError> {
        let prop_sym = unsafe { *self.constants_syms_ptr.add(prop_idx) };

        // Instance: look up method directly and call with the original heap
        // reference as receiver so that `this` mutations persist in-place.
        if obj_val.is_heap() {
            let heap_idx = obj_val.heap_index() as usize;
            let method_opt = match self.heap.objects.get(heap_idx) {
                Some(Object::Instance(inst)) => {
                    let prop_name = crate::intern::resolve(prop_sym);
                    inst.methods.get(&*prop_name).cloned()
                }
                _ => None,
            };
            if let Some(method) = method_opt {
                let arg_slice = unsafe {
                    std::slice::from_raw_parts(self.stack.as_ptr().add(arg_start), nargs)
                };
                let (result, _) =
                    self.execute_compiled_function_slice(method, arg_slice, Some(obj_val))?;
                return Ok(result);
            }
        }

        // Built-in methods on Array objects (indexOf, splice, etc.)
        // These aren't in the fast path above, so handle them here via the stack VM's
        // builtin function dispatch.
        if obj_val.is_heap() {
            let heap_idx = obj_val.heap_index() as usize;
            if let Some(Object::Array(_)) = self.heap.objects.get(heap_idx) {
                let prop_name = crate::intern::resolve(prop_sym);
                let builtin_opt = match &*prop_name {
                    "indexOf" => Some(true),
                    "lastIndexOf" => Some(true),
                    "includes" => Some(true),
                    "find" => Some(true),
                    "findIndex" => Some(true),
                    "findLast" => Some(true),
                    "findLastIndex" => Some(true),
                    "filter" => Some(true),
                    "map" => Some(true),
                    "reduce" => Some(true),
                    "reduceRight" => Some(true),
                    "forEach" => Some(true),
                    "every" => Some(true),
                    "some" => Some(true),
                    "flat" => Some(true),
                    "flatMap" => Some(true),
                    "sort" => Some(true),
                    "reverse" => Some(true),
                    "fill" => Some(true),
                    "join" => Some(true),
                    "slice" => Some(true),
                    "concat" => Some(true),
                    "keys" => Some(true),
                    "values" => Some(true),
                    "entries" => Some(true),
                    "at" => Some(true),
                    "copyWithin" => Some(true),
                    "toString" => Some(true),
                    "push" => Some(true),
                    "pop" => Some(true),
                    "shift" => Some(true),
                    "unshift" => Some(true),
                    "splice" => Some(true),
                    _ => None,
                };
                if let Some(_) = builtin_opt {
                    let arr_rc = if let Some(Object::Array(rc)) = self.heap.objects.get(heap_idx) {
                        rc.clone()
                    } else {
                        return Ok(Value::UNDEFINED);
                    };
                    let args_vec: Vec<Value> = (0..nargs)
                        .map(|i| unsafe { *self.stack.get_unchecked(arg_start + i) })
                        .collect();

                    match &*prop_name {
                        "indexOf" => {
                            // JS spec: indexOf(searchElement, fromIndex).
                            // Negative fromIndex counts from the end; values
                            // past the length return -1 immediately.
                            let needle = args_vec.first().copied().unwrap_or(Value::UNDEFINED);
                            let needle_obj = val_to_obj(needle, &self.heap);
                            let items = arr_rc.borrow();
                            let len = items.len() as i64;
                            let from_raw = args_vec.get(1).map(|v| {
                                let o = val_to_obj(*v, &self.heap);
                                match o {
                                    Object::Integer(n) => n,
                                    Object::Float(f) => f as i64,
                                    _ => 0,
                                }
                            }).unwrap_or(0);
                            let from = if from_raw < 0 {
                                (len + from_raw).max(0) as usize
                            } else {
                                (from_raw as usize).min(items.len())
                            };
                            for (i, item) in items.iter().enumerate().skip(from) {
                                let item_obj = val_to_obj(*item, &self.heap);
                                if crate::vm::VM::strict_equal(&item_obj, &needle_obj) {
                                    return Ok(Value::from_i64(i as i64));
                                }
                            }
                            return Ok(Value::from_i64(-1));
                        }
                        "includes" => {
                            // JS spec: Array.prototype.includes uses
                            // SameValueZero, which differs from `===` for
                            // NaN (NaN.includes(NaN) must be true).
                            let needle = args_vec.first().copied().unwrap_or(Value::UNDEFINED);
                            let needle_obj = val_to_obj(needle, &self.heap);
                            let items = arr_rc.borrow();
                            for item in items.iter() {
                                let item_obj = val_to_obj(*item, &self.heap);
                                if crate::vm::VM::same_value_zero(&item_obj, &needle_obj) {
                                    return Ok(Value::from_bool(true));
                                }
                            }
                            return Ok(Value::from_bool(false));
                        }
                        "join" => {
                            let sep = if let Some(v) = args_vec.first() {
                                let o = val_to_obj(*v, &self.heap);
                                match o { Object::String(s) => s.to_string(), _ => o.inspect() }
                            } else { ",".to_string() };
                            let items = arr_rc.borrow();
                            let parts: Vec<String> = items.iter().map(|v| {
                                let o = val_to_obj(*v, &self.heap);
                                match o {
                                    Object::Null | Object::Undefined => String::new(),
                                    Object::String(s) => s.to_string(),
                                    // Per the JS spec, Array#join stringifies each
                                    // element via ToString, which for a nested array
                                    // is itself Array.prototype.toString — a
                                    // recursive, comma-separated flat join. Without
                                    // this arm, `[1, [5]].join(",")` previously
                                    // fell through to `inspect()` and returned
                                    // `"1,[5]"` instead of the expected `"1,5"`.
                                    Object::Array(nested) => self.array_to_js_string(&nested.borrow()),
                                    _ => o.inspect(),
                                }
                            }).collect();
                            return Ok(obj_into_val(Object::String(Rc::from(parts.join(&sep).as_str())), &mut self.heap));
                        }
                        "slice" => {
                            let items = arr_rc.borrow();
                            let len = items.len() as i32;
                            let start = args_vec.first().map(|v| { let s = { let o = val_to_obj(*v, &self.heap); match o { Object::Integer(n) => n as i32, Object::Float(f) => f as i32, _ => 0 } }; if s < 0 { (len + s).max(0) as usize } else { s.min(len) as usize } }).unwrap_or(0);
                            let end = if args_vec.len() >= 2 { let e = { let o = val_to_obj(args_vec[1], &self.heap); match o { Object::Integer(n) => n as i32, Object::Float(f) => f as i32, _ => 0 } }; if e < 0 { (len + e).max(0) as usize } else { e.min(len) as usize } } else { len as usize };
                            return Ok(obj_into_val(make_array(items[start..end].to_vec()), &mut self.heap));
                        }
                        "push" => {
                            let borrowed = unsafe { arr_rc.borrow_mut() };
                            for arg in &args_vec { borrowed.push(*arg); }
                            return Ok(Value::from_i64(borrowed.len() as i64));
                        }
                        "pop" => {
                            return Ok(unsafe { arr_rc.borrow_mut() }.pop().unwrap_or(Value::UNDEFINED));
                        }
                        "splice" => {
                            let borrowed = unsafe { arr_rc.borrow_mut() };
                            let len = borrowed.len() as i64;
                            let start_raw = args_vec.first().map(|v| { let o = val_to_obj(*v, &self.heap); match o { Object::Integer(n) => n as i32, Object::Float(f) => f as i32, _ => 0 } } as i64).unwrap_or(0);
                            let start = if start_raw < 0 { (len + start_raw).max(0) as usize } else { (start_raw as usize).min(borrowed.len()) };
                            let dc = if args_vec.len() >= 2 { { let o = val_to_obj(args_vec[1], &self.heap); match o { Object::Integer(n) => n as i32, Object::Float(f) => f as i32, _ => 0 } }.max(0) as usize } else { borrowed.len() - start };
                            let dc = dc.min(borrowed.len() - start);
                            let removed: Vec<Value> = borrowed.drain(start..start+dc).collect();
                            for (i, arg) in args_vec[2..].iter().enumerate() { borrowed.insert(start+i, *arg); }
                            return Ok(obj_into_val(make_array(removed), &mut self.heap));
                        }
                        "shift" => {
                            let borrowed = unsafe { arr_rc.borrow_mut() };
                            return Ok(if borrowed.is_empty() { Value::UNDEFINED } else { borrowed.remove(0) });
                        }
                        "unshift" => {
                            let borrowed = unsafe { arr_rc.borrow_mut() };
                            for (i, arg) in args_vec.iter().enumerate() { borrowed.insert(i, *arg); }
                            return Ok(Value::from_i64(borrowed.len() as i64));
                        }
                        "reverse" => {
                            unsafe { arr_rc.borrow_mut() }.reverse();
                            return Ok(obj_into_val(Object::Array(arr_rc), &mut self.heap));
                        }
                        _ => {} // fall through
                    }
                }
            }
        }

        // Built-in methods on Hash objects (hasOwnProperty)
        if prop_sym == self.sym_has_own_property && obj_val.is_heap() {
            let heap_idx = obj_val.heap_index() as usize;
            if let Some(Object::Hash(h)) = self.heap.objects.get(heap_idx) {
                let key_val = if nargs >= 1 {
                    unsafe { *self.stack.get_unchecked(arg_start) }
                } else {
                    Value::UNDEFINED
                };
                let key_str = {
                    let obj = val_to_obj(key_val, &self.heap);
                    match obj {
                        Object::String(s) => s.to_string(),
                        _ => obj.inspect(),
                    }
                };
                let has = h.borrow().contains_str(&key_str);
                return Ok(Value::from_bool(has));
            }
        }

        // Promise.then() / .catch() fast-path — route through the same
        // PromiseThen/Catch builtins the normal property-access path uses
        // so pending state + chain registration stay in one place.
        if obj_val.is_heap() {
            let heap_obj = self.heap.get(obj_val.heap_index());
            if let Object::Promise(p) = heap_obj {
                let prom_rc = p.clone();
                if prop_sym == self.sym_then && nargs >= 1 {
                    let callback = unsafe { *self.stack.get_unchecked(arg_start) };
                    let second = if nargs >= 2 {
                        unsafe { *self.stack.get_unchecked(arg_start + 1) }
                    } else {
                        Value::UNDEFINED
                    };
                    let builtin = crate::object::BuiltinFunctionObject {
                        function: BuiltinFunction::PromiseThen,
                        receiver: Some(Object::Promise(prom_rc)),
                    };
                    return self.execute_builtin_function_slice(builtin, &[callback, second]);
                } else if prop_sym == self.sym_catch && nargs >= 1 {
                    let callback = unsafe { *self.stack.get_unchecked(arg_start) };
                    let builtin = crate::object::BuiltinFunctionObject {
                        function: BuiltinFunction::PromiseCatch,
                        receiver: Some(Object::Promise(prom_rc)),
                    };
                    return self.execute_builtin_function_slice(builtin, &[callback]);
                }
            }
        }

        // Function.prototype.call/apply/bind
        let prop_name = crate::intern::resolve(prop_sym);
        if &*prop_name == "call" || &*prop_name == "apply" || &*prop_name == "bind" {
            let fn_obj = val_to_obj(obj_val, &self.heap);
            match fn_obj {
                Object::CompiledFunction(_) | Object::BuiltinFunction(_) | Object::BoundMethod(_) => {
                    let arg_slice = unsafe {
                        std::slice::from_raw_parts(self.stack.as_ptr().add(arg_start), nargs)
                    };
                    if &*prop_name == "call" {
                        // .call(thisArg, arg1, arg2, ...)
                        let this_arg = if nargs >= 1 { arg_slice[0] } else { Value::UNDEFINED };
                        let call_args = if nargs > 1 { &arg_slice[1..] } else { &[] };

                        // Special case: hasOwnProperty.call(obj, key)
                        if let Object::BuiltinFunction(bf) = &fn_obj {
                            if matches!(bf.function, crate::object::BuiltinFunction::HashHasOwnProperty) {
                                // Check if thisArg (the object) has the property
                                if this_arg.is_heap() {
                                    let key_val = call_args.first().copied().unwrap_or(Value::UNDEFINED);
                                    let key_str = val_inspect(key_val, &self.heap);
                                    let heap_obj = self.heap.get(this_arg.heap_index());
                                    let has = match heap_obj {
                                        Object::Hash(h) => h.borrow().contains_str(&key_str),
                                        Object::Instance(inst) => inst.fields.contains_key(&key_str),
                                        _ => false,
                                    };
                                    return Ok(Value::from_bool(has));
                                }
                                return Ok(Value::FALSE);
                            }
                        }

                        // Special case: Object.prototype.toString.call(value)
                        if let Object::BuiltinFunction(bf) = &fn_obj {
                            if matches!(bf.function, crate::object::BuiltinFunction::ObjectPrototypeToString) {
                                let val_obj = val_to_obj(this_arg, &self.heap);
                                let tag: &str = match &val_obj {
                                    Object::Undefined => "[object Undefined]",
                                    Object::Null => "[object Null]",
                                    Object::Boolean(_) => "[object Boolean]",
                                    Object::Integer(_) | Object::Float(_) => "[object Number]",
                                    Object::String(_) => "[object String]",
                                    Object::Array(_) => "[object Array]",
                                    Object::CompiledFunction(_) | Object::BuiltinFunction(_)
                                    | Object::BoundMethod(_) => "[object Function]",
                                    Object::Instance(_) => "[object Object]",
                                    Object::Error(_) => "[object Error]",
                                    _ => "[object Object]",
                                };
                                return Ok(obj_into_val(Object::String(tag.into()), &mut self.heap));
                            }
                        }

                        // General case: call function with args (ignore this binding for now)
                        return self.call_value_slice(obj_val, call_args);
                    } else if &*prop_name == "apply" {
                        // .apply(thisArg, argsArray)
                        let apply_args = if nargs >= 2 {
                            let arr_val = arg_slice[1];
                            let arr_obj = val_to_obj(arr_val, &self.heap);
                            match arr_obj {
                                Object::Array(a) => a.borrow().to_vec(),
                                _ => vec![],
                            }
                        } else {
                            vec![]
                        };
                        let this_arg = if nargs >= 1 { Some(arg_slice[0]) } else { None };
                        let callee_obj = val_to_obj(obj_val, &self.heap);
                        match callee_obj {
                            Object::CompiledFunction(func) => {
                                let (result, _) = self.execute_compiled_function_slice(
                                    *func, &apply_args, this_arg)?;
                                return Ok(result);
                            }
                            _ => return self.call_value_slice(obj_val, &apply_args),
                        }
                    } else {
                        // .bind(thisArg, ...boundArgs) → create a wrapper that
                        // prepends bound args when called
                        let bound_args: Vec<Value> = if nargs > 1 {
                            arg_slice[1..].to_vec()
                        } else {
                            vec![]
                        };
                        if bound_args.is_empty() {
                            return Ok(obj_val);
                        }
                        // Create a BoundMethod-like wrapper. Store bound args in
                        // a receiver hash so that calling the BoundMethod prepends them.
                        let callee_obj = val_to_obj(obj_val, &self.heap);
                        match callee_obj {
                            Object::CompiledFunction(func) => {
                                // Store bound args as __boundArgs on a receiver hash
                                let mut recv_hash = crate::object::HashObject::default();
                                let arr_val = obj_into_val(make_array(bound_args), &mut self.heap);
                                recv_hash.set_by_sym(
                                    crate::intern::intern("__boundArgs"),
                                    arr_val,
                                );
                                let recv = make_hash(recv_hash);
                                let bm = Object::BoundMethod(Box::new(
                                    crate::object::BoundMethodObject {
                                        function: *func,
                                        receiver: Box::new(recv),
                                    },
                                ));
                                return Ok(obj_into_val(bm, &mut self.heap));
                            }
                            _ => return Ok(obj_val),
                        }
                    }
                }
                _ => {}
            }
        }

        let callee_val = self.get_property_val(obj_val, prop_sym, cache_slot)?;
        // Trace: detect when render method is called (it should call Hc/updateContainer)
        {
            let mn = crate::intern::resolve(prop_sym);
            if (&*mn == "render" || &*mn == "unmount") && obj_val.is_heap() {
                if let Object::Hash(h) = self.heap.get(obj_val.heap_index()) {
                    if h.borrow().get_by_str("_internalRoot").is_some() {
                        let ir = h.borrow().get_by_str("_internalRoot").unwrap();
                        let ir_obj = val_to_obj(ir, &self.heap);
                        let has_current = if let Object::Hash(fh) = &ir_obj {
                            fh.borrow().get_by_str("current").is_some()
                        } else { false };
                        let takes = if let Object::BoundMethod(bm) = val_to_obj(callee_val, &self.heap) {
                            bm.function.takes_this
                        } else if let Object::CompiledFunction(f) = val_to_obj(callee_val, &self.heap) {
                            f.takes_this
                        } else { false };
                        eprintln!("[VM] .{}() on ReactDOMRoot: callee={:?} takes_this={} _internalRoot.current={}",
                            mn, val_to_obj(callee_val, &self.heap).object_type(), takes, has_current);
                    }
                }
            }
        }
        if callee_val.is_undefined() {
            return Ok(Value::UNDEFINED);
        }
        let arg_slice =
            unsafe { std::slice::from_raw_parts(self.stack.as_ptr().add(arg_start), nargs) };
        // Pass the object as 'this' receiver for method calls.
        // This is critical: obj.method() must have this=obj, even when
        // the method was copied from a prototype (BoundMethod).
        let callee_obj = val_to_obj(callee_val, &self.heap);
        match callee_obj {
            Object::CompiledFunction(func) if func.takes_this => {
                let (result, _) = self.execute_compiled_function_slice(
                    *func, arg_slice, Some(obj_val))?;
                Ok(result)
            }
            Object::BoundMethod(bound) => {
                // Normally `this` is the object the method was called on
                // (obj_val) rather than bound.receiver — that's the right
                // answer for Hash/prototype lookups, where the BoundMethod
                // was created purely to carry the function value.
                //
                // SuperRef is the exception: `execute_index_expression`
                // calls `shift_receiver` while resolving super.X and stores
                // the already-shifted instance on the BoundMethod. If we
                // overwrote that with `obj_val` (the SuperRef itself),
                // `super.super.foo()` would fail — the inner `super`
                // would look at the SuperRef's methods instead of the
                // grandparent's, and three-level `super.super` chains
                // would return undefined.
                let was_super = obj_val.is_heap()
                    && matches!(
                        self.heap.get(obj_val.heap_index()),
                        Object::SuperRef(_)
                    );
                let this_val = if was_super {
                    obj_into_val(*bound.receiver, &mut self.heap)
                } else {
                    obj_val
                };
                let (result, _) = self.execute_compiled_function_slice(
                    bound.function, arg_slice, Some(this_val))?;
                Ok(result)
            }
            _ => self.call_value_slice(callee_val, arg_slice),
        }
    }

    /// Like call_method_slow but takes a pre-resolved prop_sym directly.
    /// Uses the SAME dispatch logic as call_method_slow but without reading
    /// from constants_syms_ptr (which may be stale in JIT context).
    pub(crate) fn call_method_with_sym(
        &mut self,
        obj_val: Value,
        prop_sym: u32,
        nargs: usize,
        arg_start: usize,
    ) -> Result<Value, VMError> {
        // Temporarily set constants_syms_ptr to a local buffer with prop_sym at index 0.
        // This avoids corrupting the shared preconvert cache.
        let local_syms = [prop_sym; 1];
        let saved_syms = self.constants_syms_ptr;
        self.constants_syms_ptr = local_syms.as_ptr();
        let result = self.call_method_slow(obj_val, 0, 0, nargs, arg_start);
        self.constants_syms_ptr = saved_syms;
        result
    }

    #[cold]
    #[inline(never)]
    pub(super) fn fused_add_const_to_prop_slow(
        &mut self,
        obj_val: Value,
        obj_r: usize,
        prop_const_idx: usize,
        val_const_idx: usize,
        cache_slot: usize,
        regs: *mut Value,
    ) -> Result<(), VMError> {
        let add_val = unsafe { &*(&*self.constants_raw).as_ptr().add(val_const_idx) };
        let prop_sym = unsafe { *self.constants_syms_ptr.add(prop_const_idx) };
        let prop_val = self.get_property_val(obj_val, prop_sym, cache_slot)?;
        let prop_ref = val_as_obj_ref(prop_val, &self.heap);
        let result = self.add_objects(&prop_ref, add_val)?;
        let result_val = obj_into_val(result, &mut self.heap);
        if let Some(updated) = self.set_property_val(obj_val, prop_sym, result_val, cache_slot)? {
            unsafe { *regs.add(obj_r) = updated };
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn fused_add_reg_props_slow(
        &mut self,
        obj_val: Value,
        obj_r: usize,
        s1_prop_idx: usize,
        s2_prop_idx: usize,
        dst_prop_idx: usize,
        s1_cache: usize,
        s2_cache: usize,
        dst_cache: usize,
        regs: *mut Value,
    ) -> Result<(), VMError> {
        let s1_sym = unsafe { *self.constants_syms_ptr.add(s1_prop_idx) };
        let s2_sym = unsafe { *self.constants_syms_ptr.add(s2_prop_idx) };
        let v1 = self.get_property_val(obj_val, s1_sym, s1_cache)?;
        let v2 = self.get_property_val(obj_val, s2_sym, s2_cache)?;
        let v1_ref = val_as_obj_ref(v1, &self.heap);
        let v2_ref = val_as_obj_ref(v2, &self.heap);
        let result = self.add_objects(&v1_ref, &v2_ref)?;
        let result_val = obj_into_val(result, &mut self.heap);
        let dst_sym = unsafe { *self.constants_syms_ptr.add(dst_prop_idx) };
        if let Some(updated) = self.set_property_val(obj_val, dst_sym, result_val, dst_cache)? {
            unsafe { *regs.add(obj_r) = updated };
        }
        Ok(())
    }

    // ── Helper methods needed by register dispatch ──────────────────────

    pub(super) fn eval_comparison(
        &mut self,
        op: ROp,
        left: &Object,
        right: &Object,
    ) -> Result<Value, VMError> {
        use ROp::*;
        let result = match op {
            Equal => self.equals(left, right),
            NotEqual => !self.equals(left, right),
            StrictEqual => Self::strict_equal(left, right),
            StrictNotEqual => !Self::strict_equal(left, right),
            GreaterThan => self.compare_numeric(left, right, crate::vm::NumericCmp::Gt)?,
            GreaterOrEqual => {
                self.compare_numeric(left, right, crate::vm::NumericCmp::Ge)?
            }
            LessThan => self.compare_numeric(left, right, crate::vm::NumericCmp::Lt)?,
            LessOrEqual => self.compare_numeric(left, right, crate::vm::NumericCmp::Le)?,
            Instanceof => self.op_instanceof(left, right),
            In => self.op_in(left, right),
            _ => false,
        };
        Ok(if result { Value::TRUE } else { Value::FALSE })
    }

    pub(super) fn eval_bitwise(&self, op: ROp, left: &Object, right: &Object) -> Result<Object, VMError> {
        use ROp::*;
        let a = self.to_i32(left)?;
        let b = self.to_i32(right)?;
        if matches!(op, UnsignedRightShift) {
            // Unsigned right shift: result is u32 (always non-negative)
            let result = (a as u32) >> (b as u32 & 31);
            return Ok(Object::Integer(result as i64));
        }
        let result = match op {
            BitwiseAnd => a & b,
            BitwiseOr => a | b,
            BitwiseXor => a ^ b,
            LeftShift => a << (b & 31),
            RightShift => a >> (b & 31),
            _ => return Err(VMError::TypeError("invalid bitwise op".to_string())),
        };
        Ok(Object::Integer(result as i64))
    }
}
