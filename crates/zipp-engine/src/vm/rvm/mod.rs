//! Register-based VM dispatch loop.
//!
//! This module adds `run_register()` and `rdispatch_loop()` to the existing VM
//! struct. Registers are mapped into the VM's value stack: `stack[reg_base + i]`.
//! The stack, globals, heap, config, and all helper methods are shared with
//! the stack-based dispatch.

use std::rc::Rc;

mod helpers;

use crate::object::{make_array, make_hash, Object, PromiseState, SuperRefObject};
use crate::rcode::ROp;
use crate::value::{obj_into_val, val_as_obj_ref, val_to_obj, Value};
use crate::vm::{VMError, MAX_ARRAY_SIZE, STACK_SIZE, VM};

impl VM {
    /// Pre-convert Object constants to NaN-boxed Values, caching by raw pointer
    /// to avoid repeated heap allocation for string/function constants.
    /// On cache hit, sets `constants_values_ptr` directly — no Vec copy.
    pub(crate) fn preconvert_constants(&mut self) {
        // Composite cache key: Vec address + Vec data pointer + length.
        // Detects Rc address reuse (same Vec address but different data pointer
        // or different length after reallocation).
        let constants = unsafe { &*self.constants_raw };
        let key = (self.constants_raw as usize)
            ^ (constants.as_ptr() as usize).wrapping_mul(7)
            ^ constants.len().wrapping_mul(0x9E3779B9);
        // Fast path: same function as last lookup (e.g. add() called 1000× in a loop).
        if key == self.last_preconvert_key {
            self.constants_values_ptr = self.last_preconvert_values_ptr;
            self.constants_syms_ptr = self.last_preconvert_syms_ptr;
            return;
        }
        // Check cache (linear scan — typically <10 unique functions)
        for (i, entry) in self.constants_values_cache.iter().enumerate() {
            if entry.0 == key {
                self.constants_values_ptr = entry.1.as_ptr();
                self.constants_syms_ptr = self.constants_syms_cache[i].1.as_ptr();
                self.last_preconvert_key = key;
                self.last_preconvert_values_ptr = self.constants_values_ptr;
                self.last_preconvert_syms_ptr = self.constants_syms_ptr;
                return;
            }
        }
        // Cache miss: convert all constants into scratch buffer
        self.constants_values_buf.clear();
        self.constants_values_buf.reserve(constants.len());
        self.constants_syms_buf.clear();
        self.constants_syms_buf.reserve(constants.len());
        for obj in constants.iter() {
            // Migrate local_objects in compiler-constructed hashes to VM heap
            if let Object::Hash(hash_rc) = obj {
                let hash = unsafe { hash_rc.borrow_mut() };
                hash.migrate_local_objects(&mut self.heap);
            }
            let val = match obj {
                Object::Integer(v) => Value::from_i64(*v),
                Object::Float(v) => Value::from_f64(*v),
                Object::Boolean(v) => Value::from_bool(*v),
                Object::Null => Value::NULL,
                Object::Undefined => Value::UNDEFINED,
                other => obj_into_val(VM::clone_object_fast(other), &mut self.heap),
            };
            self.constants_values_buf.push(val);
            // Pre-intern string constants as symbol IDs
            let sym = match obj {
                Object::String(s) => crate::intern::intern_rc(s),
                _ => 0,
            };
            self.constants_syms_buf.push(sym);
        }
        self.constants_values_cache
            .push((key, self.constants_values_buf.clone()));
        self.constants_syms_cache
            .push((key, self.constants_syms_buf.clone()));
        // SAFETY: Cache entries are never removed during VM execution, so the
        // pointer into the last cache entry remains valid.
        let entry = self.constants_values_cache.last().unwrap();
        self.constants_values_ptr = entry.1.as_ptr();
        let sym_entry = self.constants_syms_cache.last().unwrap();
        self.constants_syms_ptr = sym_entry.1.as_ptr();
        self.last_preconvert_key = key;
        self.last_preconvert_values_ptr = self.constants_values_ptr;
        self.last_preconvert_syms_ptr = self.constants_syms_ptr;
    }

    /// Run register-based bytecode. Call this instead of `run()` when the
    /// bytecode was emitted by `RCompiler`.
    pub fn run_register(&mut self) -> Result<(), VMError> {
        // Allow JIT code compiled in a previous run to be used now.
        #[cfg(feature = "djit")]
        {
            self.djit.clear_deferred();
            // Invalidate `djit_call_helper`'s monomorphic callee cache:
            // a formerly-deferred function's fn_ptr flips from None →
            // Some across this boundary, and the cache may have
            // captured the pre-flip value.
            self.last_call_callee_bits = 0;
        }
        #[cfg(all(feature = "djit", target_arch = "x86_64"))]
        self.tier2.clear_deferred();

        let entry_depth = self.rframes.len();
        let reg_base = self.sp;
        let reg_window = (self.register_count as usize).max(1);

        // Ensure stack has capacity for register window
        if reg_base + reg_window > STACK_SIZE {
            return Err(VMError::StackOverflow);
        }
        // Pre-reserve full stack capacity to avoid reallocation in recursive calls.
        // resize is faster than push-loop (single memset vs N push calls).
        let needed = reg_base + reg_window;
        if self.stack.len() < needed {
            self.stack.reserve(STACK_SIZE.saturating_sub(self.stack.len()));
            self.stack.resize(needed, Value::UNDEFINED);
        }

        // Set raw constants pointer for the register dispatch loop
        self.constants_raw = &*self.constants as *const Vec<Object>;
        // Pre-convert constants to Values (cached by Rc pointer)
        self.preconvert_constants();

        // Set sp past the register window so stack-pushing helper methods
        // (get_property_fast_path, execute_index_expression, etc.) operate
        // above our register window and don't clobber registers.
        self.sp = reg_base + reg_window;

        let try_handler_depth = self.try_handlers.len();
        match self.rdispatch_loop(entry_depth, reg_base) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Only catch handlers pushed by THIS run_register (not ancestors)
                if self.try_handlers.len() > try_handler_depth {
                    let (catch_ip, exc_reg, h_inst, h_len, h_base, h_rframes, h_consts) =
                        self.try_handlers.pop().unwrap();
                    // Unwind rframes back to the try level
                    while self.rframes.len() > h_rframes {
                        let frame = self.rframes.pop().unwrap();
                        for &(slot, old_val) in &frame.closure_saves {
                            unsafe { self.globals.set_unchecked(slot as usize, old_val) };
                        }
                    }
                    // Store exception in the exception register.
                    // For VMError::Throw, preserve the original thrown Value.
                    // For other errors, wrap a sanitised string — we
                    // deliberately *don't* Debug-format the whole
                    // error: `VMError::Yield(v)` / `InstructionOutOfBounds`
                    // would otherwise leak raw NaN-box bits / internal
                    // IPs into script-observable state.
                    let exc_val = match &e {
                        VMError::Throw(val) => *val,
                        VMError::TypeError(s) | VMError::ExecutionTimeout(s) => obj_into_val(
                            Object::String(Rc::from(s.as_str())),
                            &mut self.heap,
                        ),
                        VMError::StackOverflow => obj_into_val(
                            Object::String(Rc::from("RangeError: maximum call stack size exceeded")),
                            &mut self.heap,
                        ),
                        VMError::StackUnderflow => obj_into_val(
                            Object::String(Rc::from("InternalError: stack underflow")),
                            &mut self.heap,
                        ),
                        VMError::InvalidOpcode(_) | VMError::InstructionOutOfBounds(_) => obj_into_val(
                            Object::String(Rc::from("InternalError: invalid bytecode")),
                            &mut self.heap,
                        ),
                        // Yield should never be caught — it's an
                        // internal sentinel for generator suspension.
                        // If it somehow surfaces here, expose only
                        // the fact, not the carried Value bits.
                        VMError::Yield(_) => obj_into_val(
                            Object::String(Rc::from("InternalError: unexpected generator yield")),
                            &mut self.heap,
                        ),
                    };
                    if h_base + exc_reg < self.stack.len() {
                        self.stack[h_base + exc_reg] = exc_val;
                    }
                    // Restore VM state to the try handler's context
                    self.ip = catch_ip;
                    self.inst_ptr = h_inst;
                    self.inst_len = h_len;
                    self.sp = h_base + (self.register_count as usize).max(1);
                    // Re-enter dispatch at the catch handler
                    let saved_instructions = std::mem::replace(
                        &mut self.instructions, Rc::from(
                            unsafe { std::slice::from_raw_parts(h_inst, h_len) }.to_vec()));
                    self.constants_raw = h_consts;
                    self.preconvert_constants();
                    let result = self.rdispatch_loop(entry_depth, h_base);
                    self.instructions = saved_instructions;
                    match result {
                        Ok(()) => Ok(()),
                        Err(e2) => {
                            self.unwind_rframes(entry_depth);
                            self.unwind_frames(self.frames.len());
                            Err(e2)
                        }
                    }
                } else {
                    self.unwind_rframes(entry_depth);
                    self.unwind_frames(self.frames.len());
                    Err(e)
                }
            }
        }
    }

    /// Unwind iterative register call frames on error, restoring closure captures.
    fn unwind_rframes(&mut self, entry_depth: usize) {
        while self.rframes.len() > entry_depth {
            let frame = self.rframes.pop().unwrap();
            // Restore closure captures
            for &(slot, old_val) in &frame.closure_saves {
                unsafe { self.globals.set_unchecked(slot as usize, old_val) };
            }
            // Write back inline cache
            if frame.num_cache_slots > 0 {
                let our_cache = std::mem::replace(&mut self.inline_cache, frame.inline_cache);
                let fc = unsafe { unsafe { &*frame.func_cache }.borrow_mut() };
                if fc.is_empty() {
                    *fc = our_cache;
                }
            }
        }
    }

    // ── Instruction-pointer-local operand readers ──────────────────
    // These use a cached `inst` pointer (kept in a CPU register) instead
    // of reloading `self.inst_ptr` from the VM struct on every access.
    // Avoids aliasing concerns when the compiler can't prove that raw
    // pointer writes through `regs` don't modify `self.inst_ptr`.

    #[inline(always)]
    fn rd3(inst: *const u8, ip: usize) -> (usize, usize, usize) {
        unsafe {
            let base = inst.add(ip + 1);
            let a = u16::from_be((base as *const u16).read_unaligned());
            let b = u16::from_be((base.add(2) as *const u16).read_unaligned());
            let c = u16::from_be((base.add(4) as *const u16).read_unaligned());
            (a as usize, b as usize, c as usize)
        }
    }

    #[inline(always)]
    fn rd2(inst: *const u8, ip: usize) -> (usize, usize) {
        unsafe {
            let base = inst.add(ip + 1);
            let a = u16::from_be((base as *const u16).read_unaligned());
            let b = u16::from_be((base.add(2) as *const u16).read_unaligned());
            (a as usize, b as usize)
        }
    }

    #[inline(always)]
    fn rd1(inst: *const u8, offset: usize) -> usize {
        unsafe { u16::from_be((inst.add(offset) as *const u16).read_unaligned()) as usize }
    }

    #[inline(always)]
    fn rd1_u32(inst: *const u8, offset: usize) -> usize {
        unsafe { u32::from_be((inst.add(offset) as *const u32).read_unaligned()) as usize }
    }

    #[inline(always)]
    fn rd1_u8(inst: *const u8, offset: usize) -> usize {
        unsafe { *inst.add(offset) as usize }
    }

    /// Write a boolean comparison result, threading into JumpIfNot when possible.
    /// Returns true if the next JumpIfNot was consumed (caller should `continue`).
    #[inline(always)]
    fn store_cmp(inst: *const u8, ip: &mut usize, regs: *mut Value, dst: usize, result: bool) -> bool {
        let next = unsafe { *inst.add(*ip) };
        if next == ROp::JumpIfNot as u8 {
            let cond_r = Self::rd1(inst, *ip + 1);
            let target = Self::rd1_u32(inst, *ip + 3);
            if cond_r == dst {
                *ip = if result { *ip + 7 } else { target };
                return true;
            }
        }
        unsafe { *regs.add(dst) = if result { Value::TRUE } else { Value::FALSE } };
        false
    }

    /// Register dispatch loop. `reg_base` is the stack offset of register 0.
    /// Uses iterative dispatch for register→register calls (push/pop RCallFrame)
    /// instead of recursing. `entry_depth` is `self.rframes.len()` at call time.
    #[inline(never)]
    pub(crate) fn rdispatch_loop(
        &mut self,
        entry_depth: usize,
        initial_reg_base: usize,
    ) -> Result<(), VMError> {
        let mut ip = self.ip;
        let mut reg_base = initial_reg_base;
        // SAFETY: Stack is pre-allocated to STACK_SIZE and never reallocates.
        // Using a raw pointer lets the compiler keep it in a register instead
        // of reloading Vec metadata on every access.
        let mut regs: *mut Value = unsafe { self.stack.as_mut_ptr().add(reg_base) };
        // Cache enforce_limits as local to avoid field dereference on every backward jump.
        let enforce_limits = self.enforce_limits;
        // Cache inst_ptr as local to keep it in a CPU register across dispatch iterations.
        // Updated on Call/Return when the instruction buffer changes.
        let mut inst: *const u8 = self.inst_ptr;
        // Cache constants_values_ptr as local — accessed on every LoadConst and fused opcode.
        let mut cvals: *const Value = self.constants_values_ptr;
        // Cache constants_syms_ptr as local — accessed on every GetProp/SetProp/CallMethod.
        let mut csyms: *const u32 = self.constants_syms_ptr;
        // Cache inline_cache raw pointer — accessed on every GetProp/SetProp cache hit check.
        // Synced on Call/Return when the inline_cache Vec is swapped.
        let mut icache: *const (u32, u32) = self.inline_cache.as_ptr();
        // Cache globals data pointer — eliminates Rc→UnsafeCell→Vec indirection on GetGlobal.
        // SAFETY: globals Vec is pre-allocated to GLOBALS_SIZE and never reallocates.
        let globals_data: *mut Value = unsafe { (*self.globals.inner.get()).as_mut_ptr() };
        // Cache hot symbol IDs — these u32s are set once at VM init and never change.
        // Avoids struct field dereference on every GetProp (.length) and CallMethod (Map/Array).
        let sym_length = self.sym_length;
        let sym_set = self.sym_set;
        let sym_get = self.sym_get;
        // Raw pointer to the thread-local Interner, captured once per
        // dispatch-loop entry. Lets the hot Map / property-access arms
        // skip the `thread_local!` access (~5–10 cycles of LLVM-emitted
        // __tls_get_addr / Windows-TLS prologue) on every intern call.
        // SAFETY: the pointer stays valid for this thread's lifetime and
        // we never send it elsewhere; the dispatch loop is single-threaded.
        let interner_ptr: *mut crate::intern::Interner =
            unsafe { crate::intern::raw_interner_ptr() };
        // The direct-mapped property cache (prop_cache_obj/values/shape) was
        // removed in Round 9 because keying on `obj_val.bits()` (a heap
        // index) is unsafe: the heap reuses indices through `free_list`,
        // and `Object.freeze` / `defineProperty` accessors don't bump
        // `shape_version` — so a cache hit could write through a stale
        // `*mut Value`, ignore a freeze, or bypass a getter/setter. The
        // inline-cache path below already does the equivalent dispatch
        // and re-validates `frozen` and `has_accessors()` on every access.
        // JIT call-site cache: avoid HashMap lookup on repeated calls to same function
        #[cfg(feature = "djit")]
        let mut jit_cache_key: usize = 0;
        #[cfg(feature = "djit")]
        let mut jit_cache_ptr: Option<*const u8> = None;
        #[cfg(feature = "djit")]
        let mut jit_cache_has_calls: bool = false;
        // Callee metadata cache — keyed by the callee Value's bit pattern.
        // A tight loop calling the same function hits this every time and
        // skips the full CompiledFunction/BoundMethod destructure. The
        // cache is invalidated whenever the callee register's bits change.
        //
        // Layout matches the tuple extracted in the Call arm: instr ptr +
        // len, consts ptr, rest_idx, takes_this, is_async, num_cache_slots,
        // max_stack_depth, register_count, inline_cache Rc ptr, is_generator
        // + the bound receiver + captured-values pointer/len. A zero bit
        // pattern marks the slot as empty.
        let mut callee_cache_bits: u64 = 0;
        let mut callee_cache_instr: *const u8 = std::ptr::null();
        let mut callee_cache_instr_len: usize = 0;
        let mut callee_cache_consts: *const Vec<Object> = std::ptr::null();
        let mut callee_cache_rest_idx: Option<usize> = None;
        let mut callee_cache_takes_this: bool = false;
        let mut callee_cache_is_async: bool = false;
        let mut callee_cache_num_cache_slots: u16 = 0;
        let mut callee_cache_max_stack_depth: u16 = 0;
        let mut callee_cache_reg_count: u16 = 0;
        let mut callee_cache_func_cache: *const crate::object::VmCell<Vec<(u32, u32)>> =
            std::ptr::null();
        let mut callee_cache_is_generator: bool = false;
        let mut callee_cache_cv_ptr: *const (u16, Value) = std::ptr::null();
        let mut callee_cache_cv_len: usize = 0;
        // receiver is small enough to clone; None for plain compiled fn.
        let mut callee_cache_receiver: Option<Object> = None;
        let sym_has = self.sym_has;
        let sym_push = self.sym_push;
        let sym_pop = self.sym_pop;
        // Check accumulated quota at entry (from previous short loops).
        // Also do a wall-time check at every entry to catch deep recursion.
        if enforce_limits {
            self.quota.instructions += 1;
            if let Some(max) = self.config.max_instructions {
                if self.quota.instructions > max {
                    return Err(crate::vm::VMError::ExecutionTimeout(
                        format!("Exceeded {} instructions", max)));
                }
            }
            if let Some(max_ms) = self.config.max_wall_time_ms {
                if self.wall_time_exceeded(max_ms) {
                    return Err(crate::vm::VMError::ExecutionTimeout(
                        format!("Exceeded {}ms wall time", max_ms)));
                }
            }
        }
        let max_inst = self.config.max_instructions.unwrap_or(u64::MAX);
        let wall_limit_ms = self.config.max_wall_time_ms.unwrap_or(u64::MAX);
        // Prime wall-time tracking so subsequent `wall_time_exceeded` calls
        // measure from this point. The helper is cross-platform (native,
        // wasm32, riscv32) and handles the missing start-timestamp itself.
        self.wall_time_exceeded(u64::MAX);
        // Cache inst_len locally — only changes on Call/Return (synced there).
        let mut inst_len = self.inst_len;
        let mut loop_counter: u64 = 0;
        loop {
            loop_counter += 1;
            // Batch-sync instruction count every 64K iterations.
            // Avoids a heap memory store on every single opcode dispatch.
            if enforce_limits && (loop_counter & 0xffff) == 0 {
                self.quota.instructions += 0x10000;
                if self.quota.instructions >= max_inst {
                    return Err(crate::vm::VMError::ExecutionTimeout(format!(
                        "Exceeded {} instructions", max_inst)));
                }
                if wall_limit_ms != u64::MAX && self.wall_time_exceeded(wall_limit_ms) {
                    return Err(crate::vm::VMError::ExecutionTimeout(format!(
                        "Exceeded {}ms wall time ({}inst)",
                        wall_limit_ms, self.quota.instructions)));
                }
            }

            // SAFETY: The bytecode compiler guarantees valid instruction sequences
            // terminated by Return/Halt. Bounds check in debug mode only.
            debug_assert!(ip < inst_len, "IP {} out of bounds (bytecode len={})", ip, inst_len);
            let byte = unsafe { *inst.add(ip) };
            let op: ROp = unsafe { std::mem::transmute(byte) };

            // Save pre-execution state for ZK trace (only when feature enabled)
            #[cfg(feature = "zkvm")]
            let trace_ip = ip;
            #[cfg(feature = "zkvm")]
            let trace_op = op as u8;

            match op {
                ROp::LoadConst => {
                    let (dst, idx) = Self::rd2(inst, ip);
                    // Pre-converted: single 8-byte copy, zero allocation
                    unsafe { *regs.add(dst) = *cvals.add(idx) };
                    ip += 5;
                }
                ROp::LoadTrue => {
                    let dst = Self::rd1(inst, ip + 1);
                    unsafe { *regs.add(dst) = Value::TRUE };
                    ip += 3;
                }
                ROp::LoadFalse => {
                    let dst = Self::rd1(inst, ip + 1);
                    unsafe { *regs.add(dst) = Value::FALSE };
                    ip += 3;
                }
                ROp::LoadNull => {
                    let dst = Self::rd1(inst, ip + 1);
                    unsafe { *regs.add(dst) = Value::NULL };
                    ip += 3;
                }
                ROp::LoadUndef => {
                    let dst = Self::rd1(inst, ip + 1);
                    unsafe { *regs.add(dst) = Value::UNDEFINED };
                    ip += 3;
                }
                ROp::Move => {
                    let (dst, src) = Self::rd2(inst, ip);
                    unsafe { *regs.add(dst) = *regs.add(src) };
                    ip += 5;
                }
                ROp::GetGlobal => {
                    let (dst, idx) = Self::rd2(inst, ip);
                    unsafe { *regs.add(dst) = *globals_data.add(idx) };
                    ip += 5;
                }
                ROp::SetGlobal => {
                    let (idx, src) = Self::rd2(inst, ip);
                    let val = unsafe { *regs.add(src) };


                    unsafe { self.globals.set_unchecked(idx, val) };
                    ip += 5;
                }

                // ── Arithmetic ──────────────────────────────────────────
                ROp::Add => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    // SAFETY: register indices from compiler are within pre-allocated window
                    let left = unsafe { *regs.add(left_r) };
                    let right = unsafe { *regs.add(right_r) };
                    if Value::both_i32(left, right) {
                        let a = unsafe { left.as_i32_unchecked() };
                        let b = unsafe { right.as_i32_unchecked() };
                        unsafe {
                            *regs.add(dst) = match a.checked_add(b) {
                                Some(sum) => Value::from_i32(sum),
                                None => Value::from_f64(a as f64 + b as f64),
                            }
                        };
                    } else if left.is_number() && right.is_number() {
                        unsafe {
                            *regs.add(dst) = Value::from_f64(left.to_number() + right.to_number())
                        };
                    } else if left.is_inline_str() {
                        if right.is_inline_str() {
                            // Bitwise inline string concat — zero allocation
                            let ll = left.inline_str_len();
                            let rl = right.inline_str_len();
                            if ll + rl <= 6 {
                                const PM: u64 = 0x0000_FFFF_FFFF_FFFF;
                                let merged = (left.bits() & PM) | ((right.bits() & PM) >> (ll * 8));
                                unsafe { *regs.add(dst) = Value::from_bits((left.bits() & !PM) | merged) };
                            } else {
                                unsafe { *regs.add(dst) = self.add_string_or_object(left, right)? };
                            }
                        } else if right.is_i32() {
                            // inline_str + i32 fast path ("key" + i pattern)
                            let ll = left.inline_str_len();
                            let b = unsafe { right.as_i32_unchecked() };
                            let mut ibuf = itoa::Buffer::new();
                            let int_str = ibuf.format(b);
                            let rl = int_str.len();
                            if ll + rl <= 6 {
                                // Bitwise merge: build right payload from int bytes.
                                // Unrolled by `rl` — itoa produces strings of
                                // length 1–11 but this branch only fires for
                                // totals ≤ 6, so rl ≤ 5 (which for i32 covers
                                // every integer down to -9999). A match on rl
                                // inlines to branchless byte-pack code on x86-64
                                // instead of LLVM leaving a loop in place.
                                const PM: u64 = 0x0000_FFFF_FFFF_FFFF;
                                let rb = int_str.as_bytes();
                                let rp: u64 = match rl {
                                    1 => (rb[0] as u64) << 40,
                                    2 => (rb[0] as u64) << 40 | (rb[1] as u64) << 32,
                                    3 => {
                                        (rb[0] as u64) << 40
                                            | (rb[1] as u64) << 32
                                            | (rb[2] as u64) << 24
                                    }
                                    4 => {
                                        (rb[0] as u64) << 40
                                            | (rb[1] as u64) << 32
                                            | (rb[2] as u64) << 24
                                            | (rb[3] as u64) << 16
                                    }
                                    5 => {
                                        (rb[0] as u64) << 40
                                            | (rb[1] as u64) << 32
                                            | (rb[2] as u64) << 24
                                            | (rb[3] as u64) << 16
                                            | (rb[4] as u64) << 8
                                    }
                                    _ => 0,
                                };
                                let merged = (left.bits() & PM) | (rp >> (ll * 8));
                                unsafe { *regs.add(dst) = Value::from_bits((left.bits() & !PM) | merged) };
                            } else {
                                // Materialize directly using stack buffer: zero heap allocation
                                // for the concat itself. Re-use a cached heap slot for the
                                // same sym_id so repeated `"prefix" + i` in a hot loop
                                // doesn't grow the heap on every iteration.
                                let (lbuf, llen) = left.inline_str_buf();
                                let rb = int_str.as_bytes();
                                let total = llen + rb.len();
                                let mut sbuf = [0u8; 16]; // inline_str max 6 + itoa max 10
                                sbuf[..llen].copy_from_slice(&lbuf[..llen]);
                                sbuf[llen..total].copy_from_slice(rb);
                                let s = unsafe { std::str::from_utf8_unchecked(&sbuf[..total]) };
                                let result = self.intern_short_str_value(s, interner_ptr);
                                unsafe { *regs.add(dst) = result };
                            }
                        } else {
                            unsafe { *regs.add(dst) = self.add_string_or_object(left, right)? };
                        }
                    } else if left.is_heap() {
                        // Fast rope-creation path: `heap_string + inline_str`
                        // and `heap_string + heap_string` are the hot shapes
                        // in the `s = s + x` loop. Inline the rope node
                        // construction here to skip the function call into
                        // add_string_or_object (which is #[inline(never)]
                        // and otherwise dominates this bench).
                        let left_idx = left.heap_index() as usize;
                        let hp = self.heap.objects.as_ptr();
                        let left_obj_ref = unsafe { &*hp.add(left_idx) };
                        let left_is_str = matches!(
                            left_obj_ref,
                            Object::String(_) | Object::StringRope(_)
                        );
                        if left_is_str {
                            // Compute left length directly (avoid function call).
                            let left_len = match left_obj_ref {
                                Object::String(s) => s.len(),
                                Object::StringRope(r) => r.total_len,
                                _ => 0,
                            };
                            // Right side: inline_str / heap string / i32 — the
                            // three cases that produce a rope or materialize.
                            let mut fast_rope_handled = false;
                            if right.is_inline_str() {
                                let right_len = right.inline_str_len();
                                let total = left_len + right_len;
                                if total <= crate::vm::MAX_STRING_LENGTH && total > 8 {
                                    let rope = Object::StringRope(
                                        crate::object::StringRopeNode {
                                            left,
                                            right,
                                            total_len: total,
                                        },
                                    );
                                    unsafe {
                                        *regs.add(dst) = obj_into_val(rope, &mut self.heap)
                                    };
                                    fast_rope_handled = true;
                                }
                            } else if right.is_heap() {
                                let right_idx = right.heap_index() as usize;
                                let right_obj_ref = unsafe { &*hp.add(right_idx) };
                                let right_is_str = matches!(
                                    right_obj_ref,
                                    Object::String(_) | Object::StringRope(_)
                                );
                                if right_is_str {
                                    let right_len = match right_obj_ref {
                                        Object::String(s) => s.len(),
                                        Object::StringRope(r) => r.total_len,
                                        _ => 0,
                                    };
                                    let total = left_len + right_len;
                                    if total <= crate::vm::MAX_STRING_LENGTH && total > 8 {
                                        let rope = Object::StringRope(
                                            crate::object::StringRopeNode {
                                                left,
                                                right,
                                                total_len: total,
                                            },
                                        );
                                        unsafe {
                                            *regs.add(dst) =
                                                obj_into_val(rope, &mut self.heap)
                                        };
                                        fast_rope_handled = true;
                                    }
                                }
                            }
                            if !fast_rope_handled {
                                unsafe {
                                    *regs.add(dst) =
                                        self.add_string_or_object(left, right)?
                                };
                            }
                        } else {
                            unsafe {
                                *regs.add(dst) = self.add_string_or_object(left, right)?
                            };
                        }
                    } else {
                        unsafe { *regs.add(dst) = self.add_slow(left, right)? };
                    }
                    ip += 7;
                }
                ROp::Sub => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let left = unsafe { *regs.add(left_r) };
                    let right = unsafe { *regs.add(right_r) };
                    if Value::both_i32(left, right) {
                        let a = unsafe { left.as_i32_unchecked() };
                        let b = unsafe { right.as_i32_unchecked() };
                        unsafe {
                            *regs.add(dst) = match a.checked_sub(b) {
                                Some(diff) => Value::from_i32(diff),
                                None => Value::from_f64(a as f64 - b as f64),
                            }
                        };
                    } else if left.is_number() && right.is_number() {
                        unsafe {
                            *regs.add(dst) = Value::from_f64(left.to_number() - right.to_number())
                        };
                    } else {
                        unsafe { *regs.add(dst) = self.sub_slow(left, right)? };
                    }
                    ip += 7;
                }
                ROp::Mul => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let left = unsafe { *regs.add(left_r) };
                    let right = unsafe { *regs.add(right_r) };
                    if Value::both_i32(left, right) {
                        let a = unsafe { left.as_i32_unchecked() };
                        let b = unsafe { right.as_i32_unchecked() };
                        unsafe {
                            *regs.add(dst) = match a.checked_mul(b) {
                                Some(prod) => Value::from_i32(prod),
                                None => Value::from_f64(a as f64 * b as f64),
                            }
                        };
                    } else if left.is_number() && right.is_number() {
                        unsafe {
                            *regs.add(dst) = Value::from_f64(left.to_number() * right.to_number())
                        };
                    } else {
                        unsafe { *regs.add(dst) = self.mul_slow(left, right)? };
                    }
                    ip += 7;
                }
                ROp::Div => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let left = unsafe { *regs.add(left_r) };
                    let right = unsafe { *regs.add(right_r) };
                    if Value::both_i32(left, right) {
                        let a = unsafe { left.as_i32_unchecked() };
                        let b = unsafe { right.as_i32_unchecked() };
                        if b != 0 && a % b == 0 {
                            unsafe { *regs.add(dst) = Value::from_i32(a / b) };
                        } else {
                            unsafe { *regs.add(dst) = Value::from_f64(a as f64 / b as f64) };
                        }
                    } else if left.is_number() && right.is_number() {
                        unsafe {
                            *regs.add(dst) = Value::from_f64(left.to_number() / right.to_number())
                        };
                    } else {
                        unsafe { *regs.add(dst) = self.div_slow(left, right)? };
                    }
                    ip += 7;
                }
                ROp::Mod => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let left = unsafe { *regs.add(left_r) };
                    let right = unsafe { *regs.add(right_r) };
                    if Value::both_i32(left, right) {
                        let a = unsafe { left.as_i32_unchecked() };
                        let b = unsafe { right.as_i32_unchecked() };
                        if b != 0 {
                            // Power-of-2 fast path: bitwise AND (~1 cycle vs ~30 for DIV)
                            let r = if b > 0 && (b & (b - 1)) == 0 {
                                // a % pow2 for non-negative a
                                if a >= 0 { a & (b - 1) } else { a % b }
                            } else {
                                a % b
                            };
                            unsafe { *regs.add(dst) = Value::from_i32(r) };
                        } else {
                            unsafe { *regs.add(dst) = Value::from_f64(f64::NAN) };
                        }
                    } else if left.is_number() && right.is_number() {
                        unsafe {
                            *regs.add(dst) = Value::from_f64(left.to_number() % right.to_number())
                        };
                    } else {
                        unsafe { *regs.add(dst) = self.mod_slow(left, right)? };
                    }
                    ip += 7;
                }
                ROp::Pow => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(left_r) };
                    let rv = unsafe { *regs.add(right_r) };
                    unsafe { *regs.add(dst) = self.pow_impl(lv, rv)? };
                    ip += 7;
                }

                // ── Strict equality with i32 + heap-pointer fast paths ──
                ROp::StrictEqual => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(left_r) };
                    let rv = unsafe { *regs.add(right_r) };
                    let result = if lv.bits() == rv.bits() {
                        !lv.is_f64() || !lv.as_f64().is_nan()
                    } else if Value::both_i32(lv, rv) {
                        false
                    } else if lv.is_f64() && rv.is_f64() {
                        lv.as_f64() == rv.as_f64()
                    } else if lv.is_number() && rv.is_number() {
                        lv.to_number() == rv.to_number()
                    } else {
                        self.strict_equality_slow(lv, rv)
                    };
                    ip += 7;
                    if Self::store_cmp(inst, &mut ip, regs, dst, result) { continue; }
                }
                ROp::StrictNotEqual => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(left_r) };
                    let rv = unsafe { *regs.add(right_r) };
                    let result = if lv.bits() == rv.bits() {
                        !lv.is_f64() || !lv.as_f64().is_nan()
                    } else if Value::both_i32(lv, rv) {
                        false
                    } else if lv.is_f64() && rv.is_f64() {
                        lv.as_f64() == rv.as_f64()
                    } else if lv.is_number() && rv.is_number() {
                        lv.to_number() == rv.to_number()
                    } else {
                        self.strict_equality_slow(lv, rv)
                    };
                    ip += 7;
                    if Self::store_cmp(inst, &mut ip, regs, dst, !result) { continue; }
                }

                // ── Numeric comparison with i32/f64 fast paths ──────────
                // Split into separate arms to eliminate inner match dispatch
                ROp::LessThan => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(left_r) };
                    let rv = unsafe { *regs.add(right_r) };
                    let result = if Value::both_i32(lv, rv) {
                        unsafe { lv.as_i32_unchecked() < rv.as_i32_unchecked() }
                    } else if lv.is_number() && rv.is_number() {
                        lv.to_number() < rv.to_number()
                    } else {
                        self.comparison_slow(ROp::LessThan, lv, rv)?
                    };
                    ip += 7;
                    if Self::store_cmp(inst, &mut ip, regs, dst, result) { continue; }
                }
                ROp::LessOrEqual => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(left_r) };
                    let rv = unsafe { *regs.add(right_r) };
                    let result = if Value::both_i32(lv, rv) {
                        unsafe { lv.as_i32_unchecked() <= rv.as_i32_unchecked() }
                    } else if lv.is_number() && rv.is_number() {
                        lv.to_number() <= rv.to_number()
                    } else {
                        self.comparison_slow(ROp::LessOrEqual, lv, rv)?
                    };
                    ip += 7;
                    if Self::store_cmp(inst, &mut ip, regs, dst, result) { continue; }
                }
                ROp::GreaterThan => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(left_r) };
                    let rv = unsafe { *regs.add(right_r) };
                    let result = if Value::both_i32(lv, rv) {
                        unsafe { lv.as_i32_unchecked() > rv.as_i32_unchecked() }
                    } else if lv.is_number() && rv.is_number() {
                        lv.to_number() > rv.to_number()
                    } else {
                        self.comparison_slow(ROp::GreaterThan, lv, rv)?
                    };
                    ip += 7;
                    if Self::store_cmp(inst, &mut ip, regs, dst, result) { continue; }
                }
                ROp::GreaterOrEqual => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(left_r) };
                    let rv = unsafe { *regs.add(right_r) };
                    let result = if Value::both_i32(lv, rv) {
                        unsafe { lv.as_i32_unchecked() >= rv.as_i32_unchecked() }
                    } else if lv.is_number() && rv.is_number() {
                        lv.to_number() >= rv.to_number()
                    } else {
                        self.comparison_slow(ROp::GreaterOrEqual, lv, rv)?
                    };
                    ip += 7;
                    if Self::store_cmp(inst, &mut ip, regs, dst, result) { continue; }
                }

                // ── Equality / other comparison ─────────────────────────
                ROp::Equal => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(left_r) };
                    let rv = unsafe { *regs.add(right_r) };
                    let result = if lv.bits() == rv.bits() {
                        !lv.is_f64() || !lv.as_f64().is_nan()
                    } else if Value::both_i32(lv, rv) {
                        false
                    } else if lv.is_number() && rv.is_number() {
                        lv.to_number() == rv.to_number()
                    } else {
                        self.equality_slow(lv, rv)
                    };
                    ip += 7;
                    if Self::store_cmp(inst, &mut ip, regs, dst, result) { continue; }
                }
                ROp::NotEqual => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(left_r) };
                    let rv = unsafe { *regs.add(right_r) };
                    let result = if lv.bits() == rv.bits() {
                        !lv.is_f64() || !lv.as_f64().is_nan()
                    } else if Value::both_i32(lv, rv) {
                        false
                    } else if lv.is_number() && rv.is_number() {
                        lv.to_number() == rv.to_number()
                    } else {
                        self.equality_slow(lv, rv)
                    };
                    ip += 7;
                    if Self::store_cmp(inst, &mut ip, regs, dst, !result) { continue; }
                }
                ROp::Instanceof | ROp::In => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(left_r) };
                    let rv = unsafe { *regs.add(right_r) };

                    // Fast path: "key" in hash — peek heap, no val_to_obj
                    if op == ROp::In && rv.is_heap() {
                        let heap_obj =
                            unsafe { &*self.heap.objects.as_ptr().add(rv.heap_index() as usize) };
                        if let Object::Hash(hash_rc) = heap_obj {
                            let hash = hash_rc.borrow();
                            let found = if lv.is_heap() {
                                let key_obj = unsafe {
                                    &*self.heap.objects.as_ptr().add(lv.heap_index() as usize)
                                };
                                if let Object::String(s) = key_obj {
                                    hash.contains_str(s)
                                } else {
                                    let k = self.hash_key_from_value(lv);
                                    hash.pairs.contains_key(&k)
                                }
                            } else if lv.is_inline_str() {
                                let (buf, len) = lv.inline_str_buf();
                                let s = unsafe { std::str::from_utf8_unchecked(&buf[..len]) };
                                let sym = crate::intern::intern(s);
                                hash.pairs.contains_key(&crate::object::HashKey::Sym(sym))
                            } else {
                                let k = self.hash_key_from_value(lv);
                                hash.pairs.contains_key(&k)
                            };
                            unsafe {
                                *regs.add(dst) = if found { Value::TRUE } else { Value::FALSE }
                            };
                            ip += 7;
                            continue;
                        }
                    }

                    // Slow path
                    let lo = val_to_obj(lv, &self.heap);
                    let ro = val_to_obj(rv, &self.heap);
                    let result = self.eval_comparison(op, &lo, &ro)?;
                    unsafe { *regs.add(dst) = result };
                    ip += 7;
                }

                // ── Bitwise ─────────────────────────────────────────────
                ROp::BitwiseAnd
                | ROp::BitwiseOr
                | ROp::BitwiseXor
                | ROp::LeftShift
                | ROp::RightShift
                | ROp::UnsignedRightShift => {
                    let (dst, left_r, right_r) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(left_r) };
                    let rv = unsafe { *regs.add(right_r) };
                    // Fast path: i32 or f64-that-fits-i32 operands
                    if let (Some(a), Some(b)) = (lv.try_as_i32(), rv.try_as_i32()) {
                        let result = match op {
                            ROp::BitwiseAnd => Value::from_i32(a & b),
                            ROp::BitwiseOr => Value::from_i32(a | b),
                            ROp::BitwiseXor => Value::from_i32(a ^ b),
                            ROp::LeftShift => Value::from_i32(a << (b & 31)),
                            ROp::RightShift => Value::from_i32(a >> (b & 31)),
                            ROp::UnsignedRightShift => {
                                Value::from_i64(((a as u32) >> (b as u32 & 31)) as i64)
                            }
                            _ => unreachable!(),
                        };
                        unsafe { *regs.add(dst) = result };
                    } else {
                        unsafe { *regs.add(dst) = self.bitwise_slow(op, lv, rv)? };
                    }
                    ip += 7;
                }

                // ── Unary ───────────────────────────────────────────────
                ROp::Neg => {
                    let (dst, src) = Self::rd2(inst, ip);
                    let val = unsafe { *regs.add(src) };
                    if val.is_i32() {
                        let v = unsafe { val.as_i32_unchecked() };
                        let r = if v == 0 {
                            Value::from_f64(-0.0)
                        } else {
                            match v.checked_neg() {
                                Some(n) => Value::from_i32(n),
                                None => Value::from_f64(-(v as f64)),
                            }
                        };
                        unsafe { *regs.add(dst) = r };
                    } else if val.is_f64() {
                        unsafe { *regs.add(dst) = Value::from_f64(-val.as_f64()) };
                    } else {
                        let obj = val_to_obj(val, &self.heap);
                        let n = self.to_number(&obj)?;
                        unsafe { *regs.add(dst) = Value::from_f64(-n) };
                    }
                    ip += 5;
                }
                ROp::Not => {
                    let (dst, src) = Self::rd2(inst, ip);
                    let val = unsafe { *regs.add(src) };
                    // Fast inline: bool from comparison is the most common case
                    let truthy = if val.is_bool() {
                        unsafe { val.as_bool_unchecked() }
                    } else {
                        val.is_truthy_full(&self.heap)
                    };
                    unsafe { *regs.add(dst) = if truthy { Value::FALSE } else { Value::TRUE } };
                    ip += 5;
                }
                ROp::UnaryPlus => {
                    let (dst, src) = Self::rd2(inst, ip);
                    let val = unsafe { *regs.add(src) };
                    if val.is_i32() || val.is_f64() {
                        unsafe { *regs.add(dst) = val };
                    } else {
                        let n = self.to_number_val(val)?;
                        unsafe { *regs.add(dst) = Value::from_f64(n) };
                    }
                    ip += 5;
                }
                ROp::Typeof => {
                    let dst = Self::rd1(inst, ip + 1);
                    let src = Self::rd1(inst, ip + 3);
                    let value = unsafe { *regs.add(src) };
                    // Lazily initialize typeof cache on first use
                    if self.typeof_undefined.is_undefined() {
                        self.typeof_undefined =
                            obj_into_val(Object::String(Rc::from("undefined")), &mut self.heap);
                        self.typeof_number =
                            obj_into_val(Object::String(Rc::from("number")), &mut self.heap);
                        self.typeof_string =
                            obj_into_val(Object::String(Rc::from("string")), &mut self.heap);
                        self.typeof_boolean =
                            obj_into_val(Object::String(Rc::from("boolean")), &mut self.heap);
                        self.typeof_function =
                            obj_into_val(Object::String(Rc::from("function")), &mut self.heap);
                        self.typeof_object =
                            obj_into_val(Object::String(Rc::from("object")), &mut self.heap);
                        self.typeof_symbol =
                            obj_into_val(Object::String(Rc::from("symbol")), &mut self.heap);
                    }
                    let result = if value.is_undefined() {
                        self.typeof_undefined
                    } else if value.is_null() {
                        self.typeof_object
                    } else if value.is_bool() {
                        self.typeof_boolean
                    } else if value.is_i32() || value.is_f64() {
                        self.typeof_number
                    } else if value.is_inline_str() {
                        self.typeof_string
                    } else if value.is_heap() {
                        let heap_obj = unsafe {
                            &*self.heap.objects.as_ptr().add(value.heap_index() as usize)
                        };
                        match heap_obj {
                            Object::String(_) | Object::StringRope(_) => self.typeof_string,
                            Object::CompiledFunction(_) | Object::BoundMethod(_) | Object::Class(_)
                            | Object::BuiltinFunction(_) => {
                                self.typeof_function
                            }
                            Object::Symbol(_, _) => self.typeof_symbol,
                            Object::BigInt(_) => obj_into_val(
                                Object::String(Rc::from("bigint")),
                                &mut self.heap,
                            ),
                            _ => self.typeof_object,
                        }
                    } else {
                        self.typeof_object
                    };
                    unsafe { *regs.add(dst) = result };
                    ip += 5;
                }
                ROp::IsNullish => {
                    let (dst, src) = Self::rd2(inst, ip);
                    let val = unsafe { *regs.add(src) };
                    let is_nullish = val.is_null() || val.is_undefined();
                    unsafe {
                        *regs.add(dst) = if is_nullish {
                            Value::TRUE
                        } else {
                            Value::FALSE
                        }
                    };
                    ip += 5;
                }

                // ── Control flow ────────────────────────────────────────
                ROp::Jump => {
                    let target = Self::rd1_u32(inst, ip + 1);
                    if enforce_limits && target <= ip {
                        self.check_execution_limits()?;
                    }
                    ip = target;
                }
                ROp::JumpIfNot => {
                    let cond_r = Self::rd1(inst, ip + 1);
                    let target = Self::rd1_u32(inst, ip + 3);
                    let cond = unsafe { *regs.add(cond_r) };
                    let truthy = if cond.is_bool() {
                        unsafe { cond.as_bool_unchecked() }
                    } else {
                        cond.is_truthy_full(&self.heap)
                    };
                    if truthy {
                        ip += 7;
                    } else {
                        if enforce_limits && target <= ip {
                            self.check_execution_limits()?;
                        }
                        ip = target;
                    }
                }
                ROp::JumpIfTruthy => {
                    let cond_r = Self::rd1(inst, ip + 1);
                    let target = Self::rd1_u32(inst, ip + 3);
                    let cond = unsafe { *regs.add(cond_r) };
                    let truthy = if cond.is_bool() {
                        unsafe { cond.as_bool_unchecked() }
                    } else {
                        cond.is_truthy_full(&self.heap)
                    };
                    if truthy {
                        if enforce_limits && target <= ip {
                            self.check_execution_limits()?;
                        }
                        ip = target;
                    } else {
                        ip += 7;
                    }
                }

                // ── Function calls ──────────────────────────────────────
                ROp::Call => {
                    let dst = Self::rd1(inst, ip + 1);
                    let base = Self::rd1(inst, ip + 3);
                    let nargs = Self::rd1_u8(inst, ip + 5);
                    let _call_ip = ip;
                    ip += 6;

                    let callee_val = unsafe { *regs.add(base) };
                    if callee_val.is_undefined() || callee_val.is_null() {
                        // Temporarily log ALL undef calls to find the one in render()
                        unsafe { *regs.add(dst) = Value::UNDEFINED };
                        continue;
                    }
                    let arg_stack_start = reg_base + base + 1;

                    // Fast path: register→register compiled function call.
                    // Passes Values directly without Object↔Value conversion.
                    if callee_val.is_heap() {
                        let idx = callee_val.heap_index();
                        // Call-site metadata cache: if the same callee value
                        // was used on the prior Call at this dispatch-loop
                        // instance (i.e. tight-loop same-function calls), skip
                        // the whole destructure. Saves ~11 field reads +
                        // match-arm dispatch per call.
                        let (fast, receiver, cv_ptr, cv_len) =
                            if callee_val.bits() == callee_cache_bits && callee_cache_bits != 0 {
                                (
                                    Some((
                                        callee_cache_instr,
                                        callee_cache_instr_len,
                                        callee_cache_consts,
                                        callee_cache_rest_idx,
                                        callee_cache_takes_this,
                                        callee_cache_is_async,
                                        callee_cache_num_cache_slots,
                                        callee_cache_max_stack_depth,
                                        callee_cache_reg_count,
                                        callee_cache_func_cache,
                                        callee_cache_is_generator,
                                    )),
                                    callee_cache_receiver.clone(),
                                    callee_cache_cv_ptr,
                                    callee_cache_cv_len,
                                )
                            } else {
                                // Extract function metadata via raw pointers
                                // — zero allocation.
                                // SAFETY: CompiledFunctionObject lives in a Box
                                // on the heap. Box contents and their Vec data
                                // buffers don't move even if heap.objects Vec
                                // reallocates, so raw pointers remain valid.
                                let tup = {
                                    let obj = self.heap.get(idx);
                                    match obj {
                                        Object::CompiledFunction(func)
                                            if func.register_count > 0 =>
                                        {
                                            (
                                                Some((
                                                    func.instructions.as_ptr(),
                                                    func.instructions.len(),
                                                    &*func.constants as *const Vec<Object>,
                                                    func.rest_parameter_index,
                                                    func.takes_this,
                                                    func.is_async,
                                                    func.num_cache_slots,
                                                    func.max_stack_depth,
                                                    func.register_count,
                                                    Rc::as_ptr(&func.inline_cache),
                                                    func.is_generator,
                                                )),
                                                None,
                                                func.captured_values.as_ptr(),
                                                func.captured_values.len(),
                                            )
                                        }
                                        Object::BoundMethod(bound)
                                            if bound.function.register_count > 0 =>
                                        {
                                            // Check for .bind() pattern: receiver has __boundArgs
                                            let ba_sym = crate::intern::intern("__boundArgs");
                                            let has_bound_args = match &*bound.receiver {
                                                Object::Hash(h) => {
                                                    h.borrow().get_by_sym(ba_sym).is_some()
                                                }
                                                _ => false,
                                            };
                                            if has_bound_args {
                                                // Bound args → use slow path to prepend them
                                                (None, None, std::ptr::null(), 0)
                                            } else {
                                                let receiver_obj = *bound.receiver.clone();
                                                (
                                                    Some((
                                                        bound.function.instructions.as_ptr(),
                                                        bound.function.instructions.len(),
                                                        &*bound.function.constants
                                                            as *const Vec<Object>,
                                                        bound.function.rest_parameter_index,
                                                        bound.function.takes_this,
                                                        bound.function.is_async,
                                                        bound.function.num_cache_slots,
                                                        bound.function.max_stack_depth,
                                                        bound.function.register_count,
                                                        Rc::as_ptr(&bound.function.inline_cache),
                                                        bound.function.is_generator,
                                                    )),
                                                    Some(receiver_obj),
                                                    bound.function.captured_values.as_ptr(),
                                                    bound.function.captured_values.len(),
                                                )
                                            }
                                        }
                                        _ => (None, None, std::ptr::null(), 0),
                                    }
                                };
                                // Populate the cache on success. Skip BoundMethod
                                // with bind-args — those go through the slow path
                                // and we don't want to cache a partial.
                                if let Some((
                                    ci, cil, cc, cri, cti, cia, cncs, cmsd, crc, cfc, cig,
                                )) = tup.0
                                {
                                    callee_cache_bits = callee_val.bits();
                                    callee_cache_instr = ci;
                                    callee_cache_instr_len = cil;
                                    callee_cache_consts = cc;
                                    callee_cache_rest_idx = cri;
                                    callee_cache_takes_this = cti;
                                    callee_cache_is_async = cia;
                                    callee_cache_num_cache_slots = cncs;
                                    callee_cache_max_stack_depth = cmsd;
                                    callee_cache_reg_count = crc;
                                    callee_cache_func_cache = cfc;
                                    callee_cache_is_generator = cig;
                                    callee_cache_receiver = tup.1.clone();
                                    callee_cache_cv_ptr = tup.2;
                                    callee_cache_cv_len = tup.3;
                                } else {
                                    // Unreachable function shape: invalidate.
                                    callee_cache_bits = 0;
                                }
                                tup
                            };
                        // Immutable borrow on self.heap is dropped here.

                        if let Some((
                            instr,
                            instr_len,
                            consts,
                            rest_idx,
                            takes_this,
                            is_async,
                            cache_slots,
                            max_depth,
                            reg_count,
                            func_cache,
                            is_generator,
                        )) = fast
                        {
                            if is_generator {
                                self.ip = ip;
                                let recv_val = receiver.map(|r| obj_into_val(r, &mut self.heap));
                                let gen_val = self.create_generator_from_call(
                                    callee_val, recv_val,
                                    unsafe { regs.add(base + 1) }, nargs,
                                );
                                unsafe { *regs.add(dst) = gen_val };
                                continue;
                            }

                            // Convert receiver (if BoundMethod) now that borrow is released
                            let receiver_val = receiver.map(|r| obj_into_val(r, &mut self.heap));

                            // Inject captured closure values into globals, saving originals.
                            // For self-calls (recursive), skip closure save/restore since
                            // the values are already in the correct global slots.
                            // Fast path: zero captures (the common case for plain
                            // `function f(){}` definitions) — skip the Vec allocation
                            // entirely so the tight call loop pays no closure cost.
                            let is_self_call = instr == self.inst_ptr;
                            let has_closure_saves = !is_self_call && cv_len > 0;
                            let closure_saves: Vec<(u16, Value)> = if has_closure_saves {
                                let mut saves = Vec::with_capacity(cv_len);
                                for i in 0..cv_len {
                                    let (slot, val) = unsafe { *cv_ptr.add(i) };
                                    let old = self.get_global_as_value(slot as usize);
                                    unsafe { self.globals.set_unchecked(slot as usize, val) };
                                    saves.push((slot, old));
                                }
                                saves
                            } else {
                                Vec::new()
                            };

                            // ── dynasm JIT: call native code if available ──
                            //
                            // Tier-2 optimising code beats tier-1 when both
                            // are present; its emitted functions are
                            // ABI-compatible with `execute_ptr_with_vm`
                            // (4-arg Win64), so the dispatch below just
                            // uses whichever pointer wins with
                            // `has_calls = true`. The ultra-fast
                            // `!has_calls` path is tier-1-only and gets
                            // skipped when tier-2 is in effect.
                            //
                            // `deopt_fell_through`: set inside the tier-2
                            // branch when a speculation guard trips. The
                            // JIT block restores caller state and skips
                            // the normal result-store, so the interpreter
                            // dispatch below picks up the call via tier-0.
                            #[cfg(feature = "djit")]
                            let mut deopt_fell_through = false;
                            #[cfg(feature = "djit")]
                            if !is_async && rest_idx.is_none() {
                                let func_key = instr as usize;
                                // Call-site cache: avoid HashMap lookup for repeated calls
                                let djit_cached = if func_key == jit_cache_key {
                                    jit_cache_ptr
                                } else {
                                    let p = self.djit.get_fn_ptr(func_key);
                                    jit_cache_key = func_key;
                                    jit_cache_ptr = p;
                                    if p.is_some() {
                                        jit_cache_has_calls = self.djit.has_calls(func_key);
                                    }
                                    p
                                };
                                #[cfg(target_arch = "x86_64")]
                                let tier2_cached = self.tier2.get_fn_ptr(func_key);
                                #[cfg(not(target_arch = "x86_64"))]
                                let tier2_cached: Option<*const u8> = None;
                                let is_tier2 = tier2_cached.is_some();
                                if is_tier2 {
                                    jit_cache_has_calls = true;
                                }
                                let cached_fn = tier2_cached.or(djit_cached);
                                if let Some(fn_ptr) = cached_fn {
                                    let new_reg_base = self.sp;
                                    let reg_window = (reg_count as usize).max(1);
                                    let needed = new_reg_base + reg_window;
                                    if self.stack.len() < needed {
                                        self.stack.resize(needed, Value::UNDEFINED);
                                    }
                                    let arg_offset = if takes_this { 1 } else { 0 };
                                    if takes_this {
                                        unsafe {
                                            *self.stack.get_unchecked_mut(new_reg_base) =
                                                receiver_val.unwrap_or(Value::UNDEFINED)
                                        };
                                    }
                                    if nargs > 0 {
                                        unsafe {
                                            std::ptr::copy_nonoverlapping(
                                                self.stack.as_ptr().add(arg_stack_start),
                                                self.stack.as_mut_ptr().add(new_reg_base + arg_offset),
                                                nargs,
                                            );
                                        }
                                    }
                                    // Ultra-fast JIT call: skip constants/cache swap for
                                    // pure compute functions (no heap interaction)
                                    if !jit_cache_has_calls && cache_slots == 0 && !takes_this {
                                        // Zero remaining regs
                                        for i in (nargs + arg_offset)..reg_window {
                                            unsafe { *self.stack.get_unchecked_mut(new_reg_base + i) = Value::UNDEFINED };
                                        }
                                        let saved_cr = self.constants_raw;
                                        let saved_cvp = self.constants_values_ptr;
                                        let saved_csp = self.constants_syms_ptr;
                                        let is_same_func = std::ptr::eq(consts, self.constants_raw);
                                        if !is_same_func {
                                            self.constants_raw = consts;
                                            self.preconvert_constants();
                                        }
                                        let result = unsafe {
                                            crate::djit::DynasmJit::execute_ptr(
                                                fn_ptr,
                                                self.stack.as_mut_ptr().add(new_reg_base) as *mut u64,
                                                self.constants_values_ptr as *const u64,
                                                self.globals.raw_ptr() as *mut u64,
                                            )
                                        };
                                        if !is_same_func {
                                            self.constants_raw = saved_cr;
                                            self.constants_values_ptr = saved_cvp;
                                            cvals = saved_cvp;
                                            self.constants_syms_ptr = saved_csp;
                                            csyms = saved_csp;
                                        }
                                        // Skip the iterator entirely when there are no
                                        // closures to restore — `for ... in &empty_vec`
                                        // still emits the loop check + bounds setup.
                                        if has_closure_saves {
                                            for &(slot, old_val) in &closure_saves {
                                                unsafe { self.globals.set_unchecked(slot as usize, old_val) };
                                            }
                                        }
                                        unsafe { *regs.add(dst) = Value::from_bits(result) };
                                        // Tier-2 promotion: the ultra-fast path
                                        // is tier-1-only (it passes no vm_ptr),
                                        // so is_tier2 is always false here, but
                                        // the call counter still ticks for the
                                        // promotion threshold.
                                        #[cfg(target_arch = "x86_64")]
                                        if self.tier2.record_call(func_key) {
                                            let consts_slice = unsafe { &*consts };
                                            let instrs_slice = unsafe {
                                                std::slice::from_raw_parts(instr, instr_len)
                                            };
                                            if self.tier2.try_compile(
                                                func_key, instrs_slice, consts_slice,
                                                reg_count, nargs as u16,
                                            ) {
                                                self.tier2.set_deferred(func_key);
                                            }
                                        }
                                        continue;
                                    }
                                    for i in (nargs + arg_offset)..reg_window {
                                        unsafe {
                                            *self.stack.get_unchecked_mut(new_reg_base + i) =
                                                Value::UNDEFINED
                                        };
                                    }
                                    // Switch to callee's constants + inline cache
                                    let saved_cr = self.constants_raw;
                                    let saved_cvp = self.constants_values_ptr;
                                    let saved_csp = self.constants_syms_ptr;
                                    self.constants_raw = consts;
                                    self.preconvert_constants();
                                    // Swap inline cache for callee (enables property caching)
                                    let saved_ic = if cache_slots > 0 {
                                        let taken = unsafe { unsafe { &*func_cache }.borrow_mut() };
                                        if taken.is_empty() {
                                            std::mem::replace(
                                                &mut self.inline_cache,
                                                vec![(0, 0); cache_slots as usize],
                                            )
                                        } else {
                                            std::mem::replace(&mut self.inline_cache, std::mem::take(&mut *taken))
                                        }
                                    } else { Vec::new() };
                                    // Set jit_regs_ptr so helpers can reload r12 after stack realloc
                                    self.jit_regs_ptr = unsafe { self.stack.as_mut_ptr().add(new_reg_base) as *mut u64 };
                                    let mut result = unsafe {
                                        if jit_cache_has_calls {
                                            crate::djit::DynasmJit::execute_ptr_with_vm(
                                                fn_ptr,
                                                self.jit_regs_ptr,
                                                self.constants_values_ptr as *const u64,
                                                self.globals.raw_ptr() as *mut u64,
                                                self as *mut VM as *mut u8,
                                            )
                                        } else {
                                            crate::djit::DynasmJit::execute_ptr(
                                                fn_ptr,
                                                self.stack.as_mut_ptr().add(new_reg_base) as *mut u64,
                                                self.constants_values_ptr as *const u64,
                                                self.globals.raw_ptr() as *mut u64,
                                            )
                                        }
                                    };
                                    // Tier-2 soft deopt: speculation guard
                                    // failed. Unlike the tier-1 retry this
                                    // used to try (which was broken for
                                    // pure-compute tier-1 on mixed types),
                                    // fall through to the interpreter
                                    // dispatch below, which runs the callee
                                    // via tier-0 — correct for any input.
                                    #[cfg(target_arch = "x86_64")]
                                    {
                                        if is_tier2 && self.deopt_pending {
                                            deopt_fell_through = true;
                                        }
                                    }
                                    if deopt_fell_through {
                                        #[cfg(target_arch = "x86_64")]
                                        {
                                            self.deopt_pending = false;
                                            self.tier2.blacklist(func_key);
                                        }
                                        // Restore caller state before
                                        // bailing out to the interpreter.
                                        if cache_slots > 0 {
                                            let _ = std::mem::replace(
                                                &mut self.inline_cache,
                                                saved_ic,
                                            );
                                        }
                                        self.constants_raw = saved_cr;
                                        self.constants_values_ptr = saved_cvp;
                                        cvals = saved_cvp;
                                        self.constants_syms_ptr = saved_csp;
                                        csyms = saved_csp;
                                        icache = self.inline_cache.as_ptr();
                                        self.cached_hash_obj = 0;
                                        self.cached_map_obj = 0;
                                        for &(slot, old_val) in &closure_saves {
                                            unsafe {
                                                self.globals.set_unchecked(
                                                    slot as usize, old_val,
                                                )
                                            };
                                        }
                                        // A speculation-guard deopt is a
                                        // type-misprediction signal, not a
                                        // script error — the interpreter
                                        // retry below handles it. But if a
                                        // helper happened to *also* error
                                        // before the deopt fired, we still
                                        // want that error to surface rather
                                        // than vanish into the retry.
                                        if let Some(err) = self.take_jit_error() {
                                            return Err(err);
                                        }
                                    } else {
                                    // Write back inline cache
                                    if cache_slots > 0 {
                                        let our_cache = std::mem::replace(&mut self.inline_cache, saved_ic);
                                        let fc = unsafe { unsafe { &*func_cache }.borrow_mut() };
                                        if fc.is_empty() { *fc = our_cache; }
                                    }
                                    // Restore caller's constants
                                    self.constants_raw = saved_cr;
                                    self.constants_values_ptr = saved_cvp;
                                    cvals = saved_cvp;
                                    self.constants_syms_ptr = saved_csp;
                                    csyms = saved_csp;
                                    icache = self.inline_cache.as_ptr();
                                    // Invalidate cached hash — JIT callee may have allocated
                                    // new objects that reuse the same heap slot, making
                                    // cached_values_ptr a dangling pointer.
                                    self.cached_hash_obj = 0;
                                    self.cached_map_obj = 0;
                                    for &(slot, old_val) in &closure_saves {
                                        unsafe { self.globals.set_unchecked(slot as usize, old_val) };
                                    }
                                    // A JIT helper may have stashed an error
                                    // (thrown JS exception, type error, …) and
                                    // returned a UNDEFINED sentinel. Surface
                                    // it before we write the sentinel into the
                                    // dst register, so the caller sees the
                                    // error instead of a phantom undefined.
                                    if let Some(err) = self.take_jit_error() {
                                        return Err(err);
                                    }
                                    unsafe { *regs.add(dst) = Value::from_bits(result) };
                                    // Tier-2 promotion: tick the counter when
                                    // tier-1 ran. Already-tier-2 calls skip
                                    // this (they'd never promote themselves).
                                    #[cfg(target_arch = "x86_64")]
                                    if !is_tier2 && self.tier2.record_call(func_key) {
                                        let consts_slice = unsafe { &*consts };
                                        let instrs_slice = unsafe {
                                            std::slice::from_raw_parts(instr, instr_len)
                                        };
                                        if self.tier2.try_compile(
                                            func_key, instrs_slice, consts_slice,
                                            reg_count, nargs as u16,
                                        ) {
                                            self.tier2.set_deferred(func_key);
                                        }
                                    }
                                    continue;
                                    } // end else (normal tier-1/tier-2 path)
                                }
                                if !deopt_fell_through && self.djit.record_call(func_key) {
                                    if self.intern_cache.is_empty() {
                                        self.intern_cache = vec![(0u64, i32::MIN, u32::MAX); 2048];
                                    }
                                    let consts_slice = unsafe { &*consts };
                                    let layout = crate::vm::jit_layout();
                                    let compiled = self.djit.try_compile(func_key,
                                        unsafe { std::slice::from_raw_parts(instr, instr_len) },
                                        consts_slice, callee_val.bits(), reg_count, &layout,
                                        self.globals.raw_ptr() as *const u64, self.globals.high_water_mark());
                                    if compiled {
                                        self.djit.set_deferred(func_key);
                                    } else {
                                        // Tier-1 rejected this function shape (e.g.
                                        // contains user Call / MakeClosure that the
                                        // djit frontend doesn't handle). Tier-2's
                                        // translator + emit accept a broader set,
                                        // so try tier-2 directly. Saves the 100-
                                        // call tier-2 warmup for functions tier-1
                                        // could never compile anyway.
                                        #[cfg(target_arch = "x86_64")]
                                        {
                                            let instrs_slice = unsafe {
                                                std::slice::from_raw_parts(instr, instr_len)
                                            };
                                            if self.tier2.try_compile(
                                                func_key, instrs_slice, consts_slice,
                                                reg_count, nargs as u16,
                                            ) {
                                                self.tier2.set_deferred(func_key);
                                            }
                                        }
                                    }
                                }
                            }

                            // ── Iterative dispatch: push frame and continue ──
                            // For async/rest-param functions, fall back to recursive call.
                            if !is_async && rest_idx.is_none() {
                                let new_reg_base = self.sp;
                                let reg_window = (reg_count as usize).max(1);
                                if new_reg_base + reg_window > STACK_SIZE {
                                    for &(slot, old_val) in &closure_saves {
                                        unsafe { self.globals.set_unchecked(slot as usize, old_val) };
                                    }
                                    return Err(VMError::StackOverflow);
                                }

                                // Self-recursion detection: skip cache swap and constants setup
                                // is_self_call already computed above
                                if is_self_call {
                                    // FAST self-call push: write only essential fields + zero Vec fields.
                                    // Saves ~80 bytes vs full RCallFrame (skip pointer/state fields).
                                    let len = self.rframes.len();
                                    if len == self.rframes.capacity() {
                                        self.rframes.reserve(32);
                                    }
                                    unsafe {
                                        let ptr = self.rframes.as_mut_ptr().add(len);
                                        std::ptr::addr_of_mut!((*ptr).ip).write(ip);
                                        std::ptr::addr_of_mut!((*ptr).sp).write(self.sp);
                                        std::ptr::addr_of_mut!((*ptr).reg_base).write(reg_base);
                                        std::ptr::addr_of_mut!((*ptr).dst_reg).write(dst);
                                        std::ptr::addr_of_mut!((*ptr).is_self_call).write(true);
                                        std::ptr::addr_of_mut!((*ptr).num_cache_slots).write(0);
                                        // Zero-init Vec fields to prevent UB on drop/unwind
                                        std::ptr::addr_of_mut!((*ptr).inline_cache).write(Vec::new());
                                        std::ptr::addr_of_mut!((*ptr).closure_saves).write(Vec::new());
                                        self.rframes.set_len(len + 1);
                                    }
                                } else if cache_slots == 0 && closure_saves.is_empty() {
                                // Fast non-self call: no inline cache swap, no closure save.
                                // Uses compact frame push like self-call path.
                                let len = self.rframes.len();
                                if len == self.rframes.capacity() {
                                    self.rframes.reserve(32);
                                }
                                unsafe {
                                    let ptr = self.rframes.as_mut_ptr().add(len);
                                    std::ptr::addr_of_mut!((*ptr).ip).write(ip);
                                    std::ptr::addr_of_mut!((*ptr).inst_ptr).write(self.inst_ptr);
                                    std::ptr::addr_of_mut!((*ptr).inst_len).write(self.inst_len);
                                    std::ptr::addr_of_mut!((*ptr).constants_raw).write(self.constants_raw);
                                    std::ptr::addr_of_mut!((*ptr).constants_values_ptr).write(self.constants_values_ptr);
                                    std::ptr::addr_of_mut!((*ptr).constants_syms_ptr).write(self.constants_syms_ptr);
                                    std::ptr::addr_of_mut!((*ptr).sp).write(self.sp);
                                    std::ptr::addr_of_mut!((*ptr).reg_base).write(reg_base);
                                    std::ptr::addr_of_mut!((*ptr).max_stack_depth).write(self.max_stack_depth);
                                    std::ptr::addr_of_mut!((*ptr).dst_reg).write(dst);
                                    std::ptr::addr_of_mut!((*ptr).is_self_call).write(false);
                                    std::ptr::addr_of_mut!((*ptr).num_cache_slots).write(0);
                                    std::ptr::addr_of_mut!((*ptr).inline_cache).write(Vec::new());
                                    std::ptr::addr_of_mut!((*ptr).closure_saves).write(Vec::new());
                                    std::ptr::addr_of_mut!((*ptr).func_cache).write(func_cache);
                                    self.rframes.set_len(len + 1);
                                }

                                    self.inst_ptr = instr;
                                    inst = instr;
                                    self.inst_len = instr_len;
                                    inst_len = instr_len;
                                    self.constants_raw = consts;
                                    self.preconvert_constants();
                                    cvals = self.constants_values_ptr;
                                    csyms = self.constants_syms_ptr;
                                    icache = self.inline_cache.as_ptr();
                                    self.max_stack_depth = max_depth as usize;
                                } else {
                                // Swap inline cache (skip for self-calls — same cache)
                                let saved_ic = if cache_slots > 0 {
                                    let taken = std::mem::take(unsafe { unsafe { &*func_cache }.borrow_mut() });
                                    if taken.is_empty() {
                                        std::mem::replace(
                                            &mut self.inline_cache,
                                            vec![(0, 0); cache_slots as usize],
                                        )
                                    } else {
                                        std::mem::replace(&mut self.inline_cache, taken)
                                    }
                                } else {
                                    Vec::new()
                                };

                                self.rframes.push(crate::vm::RCallFrame {
                                    ip,
                                    inst_ptr: self.inst_ptr,
                                    inst_len: self.inst_len,
                                    constants_raw: self.constants_raw,
                                    constants_values_ptr: self.constants_values_ptr,
                                    constants_syms_ptr: self.constants_syms_ptr,
                                    sp: self.sp,
                                    reg_base,
                                    max_stack_depth: self.max_stack_depth,
                                    inline_cache: saved_ic,
                                    func_cache,
                                    num_cache_slots: cache_slots,
                                    closure_saves,
                                    dst_reg: dst,
                                    is_self_call: false,
                                });

                                    self.inst_ptr = instr;
                                    inst = instr;
                                    self.inst_len = instr_len;
                                    inst_len = instr_len;
                                    self.constants_raw = consts;
                                    self.preconvert_constants();
                                    cvals = self.constants_values_ptr;
                                    csyms = self.constants_syms_ptr;
                                    icache = self.inline_cache.as_ptr();
                                    self.max_stack_depth = max_depth as usize;
                                } // end !is_self_call

                                // Ensure stack fits callee register window
                                let needed = new_reg_base + reg_window;
                                if self.stack.len() < needed {
                                    self.stack.resize(needed, Value::UNDEFINED);
                                }

                                // Copy args via memcpy
                                let arg_offset = if takes_this { 1 } else { 0 };
                                if takes_this {
                                    unsafe {
                                        *self.stack.get_unchecked_mut(new_reg_base) =
                                            receiver_val.unwrap_or(Value::UNDEFINED)
                                    };
                                }
                                if nargs > 0 {
                                    unsafe {
                                        std::ptr::copy_nonoverlapping(
                                            self.stack.as_ptr().add(arg_stack_start),
                                            self.stack.as_mut_ptr().add(new_reg_base + arg_offset),
                                            nargs,
                                        );
                                    }
                                }
                                // Init remaining regs to undefined (bulk fill)
                                let first_uninit = nargs + arg_offset;
                                let uninit_count = reg_window.saturating_sub(first_uninit);
                                if uninit_count > 0 {
                                    unsafe {
                                        let dst = self.stack.as_mut_ptr().add(new_reg_base + first_uninit);
                                        std::slice::from_raw_parts_mut(dst, uninit_count)
                                            .fill(Value::UNDEFINED);
                                    }
                                }

                                self.sp = new_reg_base + reg_window;
                                self.last_call_nargs = nargs as u16;

                                // Update loop locals — stay in same loop!
                                ip = 0;
                                reg_base = new_reg_base;
                                regs = unsafe { self.stack.as_mut_ptr().add(reg_base) };
                                continue;
                            }

                            // ── Recursive fallback for async/rest-param ──
                            self.ip = ip;
                            let result = unsafe {
                                self.call_register_direct(
                                    instr,
                                    instr_len,
                                    consts,
                                    rest_idx,
                                    takes_this,
                                    is_async,
                                    cache_slots,
                                    max_depth,
                                    reg_count,
                                    func_cache,
                                    arg_stack_start,
                                    nargs,
                                    receiver_val,
                                )
                            };

                            // Restore original global values
                            for &(slot, old_val) in &closure_saves {
                                unsafe { self.globals.set_unchecked(slot as usize, old_val) };
                            }

                            let result = match result {
                                Ok(v) => v,
                                Err(e) => match self.try_catch_error(e) {
                                    Ok(Some((cip, cb))) => {
                                        ip = cip; reg_base = cb;
                                        regs = unsafe { self.stack.as_mut_ptr().add(reg_base) };
                                        inst = self.inst_ptr;
                                        cvals = self.constants_values_ptr;
                                        csyms = self.constants_syms_ptr;
                                        icache = self.inline_cache.as_ptr();
                                        continue;
                                    }
                                    Err(e) => return Err(e),
                                    _ => unreachable!(),
                                },
                            };

                            unsafe { *regs.add(dst) = result };
                            continue;
                        }
                    }

                    // Slow path: builtins, stack-based functions, etc.
                    self.ip = ip;
                    // Check if callee is a super() constructor call.  When it is,
                    // the return value is the updated `this` and must also be
                    // written back to register 0 so the derived constructor sees
                    // properties set by the parent.
                    let is_super_call = callee_val.is_heap()
                        && matches!(self.heap.get(callee_val.heap_index()), Object::SuperRef(_));
                    match self.call_slow(callee_val, arg_stack_start, nargs) {
                        Ok(result) => {
                            unsafe { *regs.add(dst) = result };
                            if is_super_call {
                                unsafe { *regs.add(0) = result };
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }
                ROp::CallSpread => {
                    let dst = Self::rd1(inst, ip + 1);
                    let func_r = Self::rd1(inst, ip + 3);
                    let args_r = Self::rd1(inst, ip + 5);
                    ip += 7;

                    let callee_val = unsafe { *regs.add(func_r) };
                    let args_val = unsafe { *regs.add(args_r) };
                    let args: Vec<Value> = if args_val.is_heap() {
                        let heap_obj = unsafe {
                            &*self
                                .heap
                                .objects
                                .as_ptr()
                                .add(args_val.heap_index() as usize)
                        };
                        match heap_obj {
                            Object::Array(arr) => arr.borrow().to_vec(),
                            _ => vec![],
                        }
                    } else {
                        vec![]
                    };

                    if callee_val.is_undefined() || callee_val.is_null() {
                        unsafe { *regs.add(dst) = Value::UNDEFINED };
                    } else {
                        self.ip = ip;
                        match self.call_value_slice(callee_val, &args) {
                            Ok(v) => unsafe { *regs.add(dst) = v },
                            Err(e) => return Err(e),
                        }
                    }
                }
                ROp::CallGlobal => {
                    let dst = Self::rd1(inst, ip + 1);
                    let global_idx = Self::rd1(inst, ip + 3);
                    let base = Self::rd1(inst, ip + 5);
                    let nargs = Self::rd1_u8(inst, ip + 7);
                    let _cg_ip = ip;
                    ip += 8;

                    let arg_stack_start = reg_base + base + 1;

                    let gval = unsafe { self.globals.get_unchecked(global_idx) };


                    if gval.is_undefined() || gval.is_null() {
                        unsafe { *regs.add(dst) = Value::UNDEFINED };
                        continue;
                    }
                    let fast = if gval.is_heap() {
                        let heap_obj =
                            unsafe { &*self.heap.objects.as_ptr().add(gval.heap_index() as usize) };
                        match heap_obj {
                            Object::CompiledFunction(func) if func.register_count > 0 => {
                                // Raw pointers — zero Rc clones, zero val_to_obj
                                Some((
                                    func.instructions.as_ptr(),
                                    func.instructions.len(),
                                    &*func.constants as *const Vec<Object>,
                                    func.rest_parameter_index,
                                    func.takes_this,
                                    func.is_async,
                                    func.num_cache_slots,
                                    func.max_stack_depth,
                                    func.register_count,
                                    Rc::as_ptr(&func.inline_cache),
                                    func.is_generator,
                                ))
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };

                    if let Some((
                        instr,
                        instr_len,
                        consts,
                        rest_idx,
                        takes_this,
                        is_async,
                        cache_slots,
                        max_depth,
                        reg_count,
                        func_cache,
                        is_generator,
                    )) = fast
                    {
                        if is_generator {
                            self.ip = ip;
                            let gen_val = self.create_generator_from_call(
                                gval, None,
                                unsafe { self.stack.as_ptr().add(arg_stack_start) },
                                nargs,
                            );
                            unsafe { *regs.add(dst) = gen_val };
                            continue;
                        }

                        // ── dynasm JIT (tier 2 preferred, falls back to tier 1) ──
                        //
                        // `deopt_fell_through` is set inside the tier-2 branch
                        // when a speculation guard failed. After the JIT block
                        // we use it to skip djit-promotion bookkeeping and
                        // hand the call to the interpreter dispatch below,
                        // which runs the callee via tier-0 — correct for any
                        // input type, including the one that tripped the guard.
                        #[cfg(feature = "djit")]
                        let mut deopt_fell_through = false;
                        #[cfg(feature = "djit")]
                        if !is_async && rest_idx.is_none() {
                            let func_key = instr as usize;
                            // Tier-2 beats tier-1 when both are present.
                            // Tier-2 always takes the vm_ptr form — its
                            // emission reserves env slots that the
                            // runtime-helper calls reload from.
                            #[cfg(target_arch = "x86_64")]
                            let tier2_ptr = self.tier2.get_fn_ptr(func_key);
                            #[cfg(not(target_arch = "x86_64"))]
                            let tier2_ptr: Option<*const u8> = None;
                            let djit_ptr = self.djit.get_fn_ptr(func_key);
                            if let Some(fn_ptr) = tier2_ptr.or(djit_ptr) {
                                let is_tier2 = tier2_ptr.is_some();
                                let has_calls = is_tier2 || self.djit.has_calls(func_key);
                                let new_reg_base = self.sp;
                                let reg_window = (reg_count as usize).max(1);
                                let needed = new_reg_base + reg_window;
                                if self.stack.len() < needed {
                                    self.stack.resize(needed, Value::UNDEFINED);
                                }
                                let arg_offset = if takes_this { 1 } else { 0 };
                                if nargs > 0 {
                                    unsafe {
                                        std::ptr::copy_nonoverlapping(
                                            self.stack.as_ptr().add(arg_stack_start),
                                            self.stack.as_mut_ptr().add(new_reg_base + arg_offset),
                                            nargs,
                                        );
                                    }
                                }
                                for i in (nargs + arg_offset)..reg_window {
                                    unsafe { *self.stack.get_unchecked_mut(new_reg_base + i) = Value::UNDEFINED };
                                }
                                // Switch to callee's constants before executing JIT code
                                let saved_cr = self.constants_raw;
                                let saved_cvp = self.constants_values_ptr;
                                let saved_csp = self.constants_syms_ptr;
                                self.constants_raw = consts;
                                self.preconvert_constants();
                                let mut result = unsafe {
                                    if has_calls {
                                        crate::djit::DynasmJit::execute_ptr_with_vm(
                                            fn_ptr,
                                            self.stack.as_mut_ptr().add(new_reg_base) as *mut u64,
                                            self.constants_values_ptr as *const u64,
                                            self.globals.raw_ptr() as *mut u64,
                                            self as *mut VM as *mut u8)
                                    } else {
                                        crate::djit::DynasmJit::execute_ptr(
                                            fn_ptr,
                                            self.stack.as_mut_ptr().add(new_reg_base) as *mut u64,
                                            self.constants_values_ptr as *const u64,
                                            self.globals.raw_ptr() as *mut u64)
                                    }
                                };
                                // Tier-2 soft deopt: if the just-returned function
                                // tripped a speculation guard, its result is a
                                // sentinel. Clear the flag, blacklist the tier-2
                                // code, restore caller state, and fall through
                                // to the interpreter dispatch below so tier-0
                                // re-runs the callee with correct semantics for
                                // any input type. Tier-1 retry is deliberately
                                // NOT used here because pure-compute tier-1
                                // entries (`!has_calls`) inline a strict i32
                                // path that produces garbage on f64 operands.
                                #[cfg(target_arch = "x86_64")]
                                {
                                    if is_tier2 && self.deopt_pending {
                                        deopt_fell_through = true;
                                    }
                                }
                                if deopt_fell_through {
                                    #[cfg(target_arch = "x86_64")]
                                    {
                                        self.deopt_pending = false;
                                        self.tier2.blacklist(func_key);
                                    }
                                    // Restore caller's constants before bailing
                                    // out of the JIT branch.
                                    self.constants_raw = saved_cr;
                                    self.constants_values_ptr = saved_cvp;
                                    cvals = saved_cvp;
                                    self.constants_syms_ptr = saved_csp;
                                    csyms = saved_csp;
                                    self.cached_hash_obj = 0;
                                    self.cached_map_obj = 0;
                                    // Mirror the Call-opcode deopt branch: an
                                    // error from a helper before the deopt fired
                                    // must not vanish into the tier-0 retry.
                                    if let Some(err) = self.take_jit_error() {
                                        return Err(err);
                                    }
                                    // Don't store the sentinel; don't `continue`.
                                    // The iterative-dispatch section below
                                    // handles the call via tier-0.
                                } else {
                                // Restore caller's constants
                                self.constants_raw = saved_cr;
                                self.constants_values_ptr = saved_cvp;
                                cvals = saved_cvp;
                                self.constants_syms_ptr = saved_csp;
                                csyms = saved_csp;
                                // Invalidate cached hash — JIT callee may have allocated
                                // objects reusing same heap slot (stale cached_values_ptr).
                                self.cached_hash_obj = 0;
                                self.cached_map_obj = 0;
                                // See the matching check in the Call-opcode JIT
                                // path: errors from JIT helpers come back through
                                // the side-channel as UNDEFINED, and need to be
                                // surfaced before we treat the result as success.
                                if let Some(err) = self.take_jit_error() {
                                    return Err(err);
                                }
                                unsafe { *regs.add(dst) = Value::from_bits(result) };
                                // Tier-2 promotion: after tier-1 executes, charge
                                // one to the tier-2 counter; on threshold, try
                                // the optimising compile and defer it to the
                                // next run_register so we don't jump into
                                // just-materialised code mid-recursion.
                                #[cfg(target_arch = "x86_64")]
                                if !is_tier2 && self.tier2.record_call(func_key) {
                                    let consts_slice = unsafe { &*consts };
                                    let instrs_slice = unsafe {
                                        std::slice::from_raw_parts(instr, instr_len)
                                    };
                                    if self.tier2.try_compile(
                                        func_key, instrs_slice, consts_slice,
                                        reg_count, nargs as u16,
                                    ) {
                                        self.tier2.set_deferred(func_key);
                                    }
                                }
                                continue;
                                } // end else (normal JIT path)
                                // If we got here, deopt_fired was true and we
                                // intentionally fall through to the interpreter
                                // dispatch block below.
                            }
                            if !deopt_fell_through && self.djit.record_call(func_key) {
                                if self.intern_cache.is_empty() {
                                    self.intern_cache = vec![(0u64, i32::MIN, u32::MAX); 2048];
                                }
                                let consts_slice = unsafe { &*consts };
                                let layout = crate::vm::jit_layout();
                                let compiled = self.djit.try_compile(func_key,
                                    unsafe { std::slice::from_raw_parts(instr, instr_len) },
                                    consts_slice, gval.bits(), reg_count, &layout,
                                    self.globals.raw_ptr() as *const u64, self.globals.high_water_mark());
                                if compiled {
                                    self.djit.set_deferred(func_key);
                                } else {
                                    // Tier-1 rejected (see site-1 comment). Try
                                    // tier-2 directly so functions with user
                                    // Call / MakeClosureNoCapture get native
                                    // code without waiting for the 100-call
                                    // tier-2 warmup.
                                    #[cfg(target_arch = "x86_64")]
                                    {
                                        let instrs_slice = unsafe {
                                            std::slice::from_raw_parts(instr, instr_len)
                                        };
                                        if self.tier2.try_compile(
                                            func_key, instrs_slice, consts_slice,
                                            reg_count, nargs as u16,
                                        ) {
                                            self.tier2.set_deferred(func_key);
                                        }
                                    }
                                }
                            }
                        }

                        // ── Iterative dispatch for CallGlobal ──
                        if !is_async && rest_idx.is_none() {
                            let new_reg_base = self.sp;
                            let reg_window = (reg_count as usize).max(1);
                            if new_reg_base + reg_window > STACK_SIZE {
                                return Err(VMError::StackOverflow);
                            }

                            // Self-recursion detection: skip cache swap and constants setup
                            let is_self_call = instr == self.inst_ptr;

                            if is_self_call {
                                // FAST self-call push: only store essential fields
                                let len = self.rframes.len();
                                if len == self.rframes.capacity() {
                                    self.rframes.reserve(32);
                                }
                                unsafe {
                                    let ptr = self.rframes.as_mut_ptr().add(len);
                                    std::ptr::addr_of_mut!((*ptr).ip).write(ip);
                                    std::ptr::addr_of_mut!((*ptr).sp).write(self.sp);
                                    std::ptr::addr_of_mut!((*ptr).reg_base).write(reg_base);
                                    std::ptr::addr_of_mut!((*ptr).dst_reg).write(dst);
                                    std::ptr::addr_of_mut!((*ptr).is_self_call).write(true);
                                    std::ptr::addr_of_mut!((*ptr).num_cache_slots).write(0);
                                    // Zero-init Vec fields to prevent UB on drop/unwind
                                    std::ptr::addr_of_mut!((*ptr).inline_cache).write(Vec::new());
                                    std::ptr::addr_of_mut!((*ptr).closure_saves).write(Vec::new());
                                    self.rframes.set_len(len + 1);
                                }
                            } else {
                            let saved_ic = if cache_slots > 0 {
                                let taken =
                                    std::mem::take(unsafe { unsafe { &*func_cache }.borrow_mut() });
                                if taken.is_empty() {
                                    std::mem::replace(
                                        &mut self.inline_cache,
                                        vec![(0, 0); cache_slots as usize],
                                    )
                                } else {
                                    std::mem::replace(&mut self.inline_cache, taken)
                                }
                            } else {
                                Vec::new()
                            };
                            self.rframes.push(crate::vm::RCallFrame {
                                ip,
                                inst_ptr: self.inst_ptr,
                                inst_len: self.inst_len,
                                constants_raw: self.constants_raw,
                                constants_values_ptr: self.constants_values_ptr,
                                constants_syms_ptr: self.constants_syms_ptr,
                                sp: self.sp,
                                reg_base,
                                max_stack_depth: self.max_stack_depth,
                                inline_cache: saved_ic,
                                func_cache,
                                num_cache_slots: cache_slots,
                                closure_saves: Vec::new(),
                                dst_reg: dst,
                                is_self_call: false,
                            });
                            self.inst_ptr = instr;
                            inst = instr;
                            self.inst_len = instr_len;
                            inst_len = instr_len;
                            self.constants_raw = consts;
                            self.preconvert_constants();
                            cvals = self.constants_values_ptr;
                            csyms = self.constants_syms_ptr;
                            icache = self.inline_cache.as_ptr();
                            self.max_stack_depth = max_depth as usize;
                            } // end !is_self_call

                            let needed = new_reg_base + reg_window;
                            if self.stack.len() < needed {
                                self.stack.resize(needed, Value::UNDEFINED);
                            }
                            let arg_offset = if takes_this { 1 } else { 0 };
                            if nargs > 0 {
                                unsafe {
                                    std::ptr::copy_nonoverlapping(
                                        self.stack.as_ptr().add(arg_stack_start),
                                        self.stack.as_mut_ptr().add(new_reg_base + arg_offset),
                                        nargs,
                                    );
                                }
                            }
                            let uninit_count = reg_window - (nargs + arg_offset);
                            if uninit_count > 0 {
                                unsafe {
                                    let dst_ptr = self.stack.as_mut_ptr().add(new_reg_base + nargs + arg_offset);
                                    std::slice::from_raw_parts_mut(dst_ptr, uninit_count)
                                        .fill(Value::UNDEFINED);
                                }
                            }
                            self.sp = new_reg_base + reg_window;
                            self.last_call_nargs = nargs as u16;
                            ip = 0;
                            reg_base = new_reg_base;
                            regs = unsafe { self.stack.as_mut_ptr().add(reg_base) };
                            continue;
                        }

                        // Recursive fallback for async/rest-param
                        self.ip = ip;
                        let result = unsafe {
                            self.call_register_direct(
                                instr,
                                instr_len,
                                consts,
                                rest_idx,
                                takes_this,
                                is_async,
                                cache_slots,
                                max_depth,
                                reg_count,
                                func_cache,
                                arg_stack_start,
                                nargs,
                                None,
                            )
                        };
                        match result {
                            Ok(v) => { unsafe { *regs.add(dst) = v }; continue; }
                            Err(e) => return Err(e),
                        }
                    }

                    // Slow path: builtins, stack-based functions, etc.
                    self.ip = ip;
                    match self.call_slow(gval, arg_stack_start, nargs) {
                        Ok(v) => unsafe { *regs.add(dst) = v },
                        Err(e) => return Err(e),
                    };
                }
                ROp::CallMethod => {
                    let dst = Self::rd1(inst, ip + 1);
                    let base = Self::rd1(inst, ip + 3);
                    let nargs = Self::rd1_u8(inst, ip + 5);
                    let prop_idx = Self::rd1(inst, ip + 6);
                    let cache_slot = Self::rd1(inst, ip + 8);
                    ip += 10;

                    let obj_val = unsafe { *regs.add(base) };
                    let arg_start = reg_base + base + 1;

                    // Fast path: direct Map/Hash method dispatch
                    if obj_val.is_heap() {
                        let heap_idx = obj_val.heap_index() as usize;
                        let prop_sym = unsafe { *csyms.add(prop_idx) };
                        let heap_obj = unsafe { &*self.heap.objects.as_ptr().add(heap_idx) };

                        match heap_obj {
                            Object::Map(map_obj) => {
                                if prop_sym == sym_set && nargs >= 2 {
                                    let key_val = unsafe { *regs.add(base + 1) };
                                    let value = unsafe { *regs.add(base + 2) };
                                    let key = self.intern_inline_str_key(key_val, interner_ptr);
                                    let entries = unsafe { map_obj.entries.borrow_mut() };
                                    let indices = unsafe { map_obj.indices.borrow_mut() };
                                    VM::map_insert_or_replace(entries, indices, key, value);
                                    unsafe { *regs.add(dst) = obj_val };
                                    continue;
                                } else if prop_sym == sym_get && nargs >= 1 {
                                    let key_val = unsafe { *regs.add(base + 1) };
                                    let key = self.intern_inline_str_key(key_val, interner_ptr);
                                    let result = VM::map_get(
                                        map_obj.entries.borrow(),
                                        map_obj.indices.borrow(),
                                        &key,
                                    );
                                    unsafe { *regs.add(dst) = result.unwrap_or(Value::UNDEFINED) };
                                    continue;
                                } else if prop_sym == sym_has && nargs >= 1 {
                                    let key_val = unsafe { *self.stack.get_unchecked(arg_start) };
                                    let key = self.intern_inline_str_key(key_val, interner_ptr);
                                    let has = VM::map_contains(map_obj.entries.borrow(), map_obj.indices.borrow(), &key);
                                    unsafe { *regs.add(dst) = Value::from_bool(has) };
                                    continue;
                                } else if prop_sym == self.sym_size {
                                    let len = map_obj.entries.borrow().len() as i64;
                                    unsafe { *regs.add(dst) = Value::from_i64(len) };
                                    continue;
                                }
                                // fall through to slow path
                            }
                            Object::Array(arr_rc) => {
                                if prop_sym == sym_push && nargs >= 1 {
                                    let items = unsafe { arr_rc.borrow_mut() };
                                    for i in 0..nargs {
                                        let arg =
                                            unsafe { *self.stack.get_unchecked(arg_start + i) };
                                        items.push(arg);
                                    }
                                    let len = items.len() as i64;
                                    if len as usize > MAX_ARRAY_SIZE {
                                        return Err(VMError::TypeError(
                                            crate::vm::ERR_ARRAY_SIZE.to_string(),
                                        ));
                                    }
                                    unsafe { *regs.add(dst) = Value::from_i64(len) };
                                    continue;
                                } else if prop_sym == sym_pop && nargs == 0 {
                                    let items = unsafe { arr_rc.borrow_mut() };
                                    match items.pop() {
                                        Some(val) => {
                                            unsafe { *regs.add(dst) = val };
                                        }
                                        None => {
                                            unsafe { *regs.add(dst) = Value::UNDEFINED };
                                        }
                                    }
                                    continue;
                                } else if prop_sym == sym_length {
                                    let len = arr_rc.borrow().len() as i64;
                                    unsafe { *regs.add(dst) = Value::from_i64(len) };
                                    continue;
                                } else if prop_sym == self.sym_shift && nargs == 0 {
                                    let items = unsafe { arr_rc.borrow_mut() };
                                    if items.is_empty() {
                                        unsafe { *regs.add(dst) = Value::UNDEFINED };
                                    } else {
                                        let first = items.remove(0);
                                        unsafe { *regs.add(dst) = first };
                                    }
                                    continue;
                                } else if prop_sym == self.sym_unshift && nargs >= 1 {
                                    let items = unsafe { arr_rc.borrow_mut() };
                                    for i in (0..nargs).rev() {
                                        let arg =
                                            unsafe { *self.stack.get_unchecked(arg_start + i) };
                                        items.insert(0, arg);
                                    }
                                    let len = items.len() as i64;
                                    unsafe { *regs.add(dst) = Value::from_i64(len) };
                                    continue;
                                } else if prop_sym == self.sym_splice {
                                    let items = unsafe { arr_rc.borrow_mut() };
                                    let len = items.len() as i64;
                                    let start_raw = if nargs >= 1 {
                                        self.to_i32_val(unsafe {
                                            *self.stack.get_unchecked(arg_start)
                                        })? as i64
                                    } else {
                                        0
                                    };
                                    let start = if start_raw < 0 {
                                        (len + start_raw).max(0) as usize
                                    } else {
                                        (start_raw as usize).min(items.len())
                                    };
                                    let delete_count = if nargs >= 2 {
                                        self.to_i32_val(unsafe {
                                            *self.stack.get_unchecked(arg_start + 1)
                                        })?
                                        .max(0) as usize
                                    } else {
                                        items.len() - start
                                    };
                                    let delete_count = delete_count.min(items.len() - start);
                                    let removed: Vec<Value> =
                                        items.drain(start..start + delete_count).collect();
                                    // Insert new items
                                    for i in 0..(nargs.saturating_sub(2)) {
                                        let arg = unsafe {
                                            *self.stack.get_unchecked(arg_start + 2 + i)
                                        };
                                        items.insert(start + i, arg);
                                    }
                                    let _ = items;
                                    unsafe {
                                        *regs.add(dst) = obj_into_val(
                                            make_array(removed),
                                            &mut self.heap,
                                        )
                                    };
                                    continue;
                                }
                                // fall through to slow path
                            }
                            Object::Hash(hash_rc) => {
                                // Try inline cache for compiled function methods on Hash
                                debug_assert!(cache_slot < self.inline_cache.len());
                                let fast = {
                                    let (cached_shape, cached_offset) =
                                        unsafe { *icache.add(cache_slot) };
                                    if cached_shape != 0 {
                                        let hash = hash_rc.borrow();
                                        if cached_shape == hash.shape_version {
                                            let slot = cached_offset as usize;
                                            let prop_val =
                                                unsafe { hash.get_value_at_slot_unchecked(slot) };
                                            // Value-native: check if it's a heap ref to CompiledFunction
                                            if prop_val.is_heap() {
                                                let func_obj = self.heap.get(prop_val.heap_index());
                                                if let Object::CompiledFunction(func) = func_obj {
                                                    if func.register_count > 0 {
                                                        // Raw pointers — zero Rc clones
                                                        Some((
                                                            func.instructions.as_ptr(),
                                                            func.instructions.len(),
                                                            &*func.constants as *const Vec<Object>,
                                                            func.rest_parameter_index,
                                                            func.takes_this,
                                                            func.is_async,
                                                            func.num_cache_slots,
                                                            func.max_stack_depth,
                                                            func.register_count,
                                                            Rc::as_ptr(&func.inline_cache),
                                                            func.is_generator,
                                                        ))
                                                    } else {
                                                        None
                                                    }
                                                } else {
                                                    None
                                                }
                                            } else {
                                                None
                                            }
                                        } else {
                                            None
                                        }
                                    } else {
                                        None
                                    }
                                };
                                // hash borrow dropped here
                                if let Some((
                                    instr,
                                    instr_len,
                                    consts,
                                    rest_idx,
                                    takes_this,
                                    is_async,
                                    cache_slots,
                                    max_depth,
                                    reg_count,
                                    func_cache,
                                    is_generator,
                                )) = fast
                                {
                                    if is_generator {
                                        // Re-fetch the property value for generator creation
                                        let prop_val_again = {
                                            let hash = hash_rc.borrow();
                                            let (cs, co) = unsafe { *icache.add(cache_slot) };
                                            if cs == hash.shape_version {
                                                unsafe { hash.get_value_at_slot_unchecked(co as usize) }
                                            } else { Value::UNDEFINED }
                                        };
                                        self.ip = ip;
                                        let gen_val = self.create_generator_from_call(
                                            prop_val_again, Some(obj_val),
                                            unsafe { self.stack.as_ptr().add(arg_start) },
                                            nargs,
                                        );
                                        unsafe { *regs.add(dst) = gen_val };
                                        continue;
                                    }

                                    self.ip = ip;
                                    // SAFETY: pointers derived from heap-allocated CompiledFunctionObject
                                    let result = unsafe {
                                        self.call_register_direct(
                                            instr,
                                            instr_len,
                                            consts,
                                            rest_idx,
                                            takes_this,
                                            is_async,
                                            cache_slots,
                                            max_depth,
                                            reg_count,
                                            func_cache,
                                            arg_start,
                                            nargs,
                                            Some(obj_val),
                                        )
                                    };
                                    let result = result?;
                                    unsafe { *regs.add(dst) = result };
                                    continue;
                                }
                            }
                            _ => {} // fall through to slow path
                        }
                    }

                    // Slow path: resolve property + call
                    self.ip = ip;
                    match self.call_method_slow(obj_val, prop_idx, cache_slot, nargs, arg_start) {
                        Ok(result) => unsafe { *regs.add(dst) = result },
                        Err(e) => return Err(e),
                    }
                }
                ROp::Return => {
                    let src = Self::rd1(inst, ip + 1);
                    let rv = unsafe { *regs.add(src) };

                    // ── Iterative return: pop RCallFrame if available ──
                    if self.rframes.len() > entry_depth {
                        let frame = unsafe { self.rframes.pop().unwrap_unchecked() };
                        if frame.is_self_call {
                            // Fast return: only restore ip, reg_base, sp, dst
                            self.sp = frame.sp;
                            ip = frame.ip;
                            reg_base = frame.reg_base;
                            regs = unsafe { self.stack.as_mut_ptr().add(reg_base) };
                            unsafe { *regs.add(frame.dst_reg) = rv };
                            continue;
                        }
                        // Full return: restore all state
                        if frame.num_cache_slots > 0 {
                            let our_cache =
                                std::mem::replace(&mut self.inline_cache, frame.inline_cache);
                            let fc = unsafe { unsafe { &*frame.func_cache }.borrow_mut() };
                            if fc.is_empty() {
                                *fc = our_cache;
                            }
                        }
                        for &(slot, old_val) in &frame.closure_saves {
                            unsafe { self.globals.set_unchecked(slot as usize, old_val) };
                        }
                        self.inst_ptr = frame.inst_ptr;
                        inst = frame.inst_ptr;
                        self.inst_len = frame.inst_len;
                        inst_len = frame.inst_len;
                        self.constants_raw = frame.constants_raw;
                        self.constants_values_ptr = frame.constants_values_ptr;
                        cvals = frame.constants_values_ptr;
                        self.constants_syms_ptr = frame.constants_syms_ptr;
                        csyms = frame.constants_syms_ptr;
                        icache = self.inline_cache.as_ptr();
                        self.max_stack_depth = frame.max_stack_depth;
                        self.sp = frame.sp;
                        ip = frame.ip;
                        reg_base = frame.reg_base;
                        regs = unsafe { self.stack.as_mut_ptr().add(reg_base) };
                        unsafe { *regs.add(frame.dst_reg) = rv };
                        continue;
                    }
                    // Top-level or recursive boundary return
                    self.quota.instructions += loop_counter;
                    if enforce_limits && self.quota.instructions > max_inst {
                        return Err(crate::vm::VMError::ExecutionTimeout(format!(
                            "Exceeded {} instructions", max_inst)));
                    }
                    self.last_popped = Some(rv);
                    return Ok(());
                }
                ROp::ReturnUndef => {
                    // ── Iterative return: pop RCallFrame if available ──
                    if self.rframes.len() > entry_depth {
                        let frame = unsafe { self.rframes.pop().unwrap_unchecked() };
                        if frame.is_self_call {
                            // Fast return: only restore ip, reg_base, sp, dst
                            self.sp = frame.sp;
                            ip = frame.ip;
                            reg_base = frame.reg_base;
                            regs = unsafe { self.stack.as_mut_ptr().add(reg_base) };
                            unsafe { *regs.add(frame.dst_reg) = Value::UNDEFINED };
                            continue;
                        }
                        // Full return: restore all state
                        if frame.num_cache_slots > 0 {
                            let our_cache =
                                std::mem::replace(&mut self.inline_cache, frame.inline_cache);
                            let fc = unsafe { unsafe { &*frame.func_cache }.borrow_mut() };
                            if fc.is_empty() {
                                *fc = our_cache;
                            }
                        }
                        for &(slot, old_val) in &frame.closure_saves {
                            unsafe { self.globals.set_unchecked(slot as usize, old_val) };
                        }
                        self.inst_ptr = frame.inst_ptr;
                        inst = frame.inst_ptr;
                        self.inst_len = frame.inst_len;
                        inst_len = frame.inst_len;
                        self.constants_raw = frame.constants_raw;
                        self.constants_values_ptr = frame.constants_values_ptr;
                        cvals = frame.constants_values_ptr;
                        self.constants_syms_ptr = frame.constants_syms_ptr;
                        csyms = frame.constants_syms_ptr;
                        icache = self.inline_cache.as_ptr();
                        self.max_stack_depth = frame.max_stack_depth;
                        self.sp = frame.sp;
                        ip = frame.ip;
                        reg_base = frame.reg_base;
                        regs = unsafe { self.stack.as_mut_ptr().add(reg_base) };
                        unsafe { *regs.add(frame.dst_reg) = Value::UNDEFINED };
                        continue;
                    }
                    self.quota.instructions += loop_counter;
                    if enforce_limits && self.quota.instructions > max_inst {
                        return Err(crate::vm::VMError::ExecutionTimeout(format!(
                            "Exceeded {} instructions", max_inst)));
                    }
                    self.last_popped = Some(Value::UNDEFINED);
                    return Ok(());
                }

                // ── Constructors ────────────────────────────────────────
                ROp::New => {
                    let dst = Self::rd1(inst, ip + 1);
                    let base = Self::rd1(inst, ip + 3);
                    let nargs = Self::rd1_u8(inst, ip + 5);
                    ip += 6;

                    let callee = val_to_obj(unsafe { *regs.add(base) }, &self.heap);
                    let mut args = std::mem::take(&mut self.arg_buffer);
                    args.clear();
                    for i in 0..nargs {
                        args.push(unsafe { *regs.add(base + 1 + i) });
                    }

                    // execute_new_with_args_slice pushes result to stack
                    self.ip = ip;
                    let new_result = self.execute_new_with_args_slice(callee, &args);
                    args.clear();
                    self.arg_buffer = args;
                    new_result?;
                    let result = self.pop_val()?;
                    unsafe { *regs.add(dst) = result };
                }
                ROp::NewSpread => {
                    let dst = Self::rd1(inst, ip + 1);
                    let cls_r = Self::rd1(inst, ip + 3);
                    let args_r = Self::rd1(inst, ip + 5);
                    ip += 7;

                    let callee = val_to_obj(unsafe { *regs.add(cls_r) }, &self.heap);
                    let args_val = unsafe { *regs.add(args_r) };
                    let args: Vec<Value> = if args_val.is_heap() {
                        let heap_obj = unsafe {
                            &*self
                                .heap
                                .objects
                                .as_ptr()
                                .add(args_val.heap_index() as usize)
                        };
                        match heap_obj {
                            Object::Array(arr) => arr.borrow().to_vec(),
                            _ => vec![],
                        }
                    } else {
                        vec![]
                    };

                    self.ip = ip;
                    match self.execute_new_with_args_slice(callee, &args)
                        .and_then(|_| self.pop_val()) {
                        Ok(v) => unsafe { *regs.add(dst) = v },
                        Err(e) => return Err(e),
                    }
                }
                ROp::Super => {
                    let dst = Self::rd1(inst, ip + 1);
                    // In register VM, register 0 is "this" (first local)
                    let this_val =
                        val_to_obj(unsafe { *self.stack.get_unchecked(reg_base) }, &self.heap);
                    if let Object::Instance(instance) = this_val {
                        let result = Object::SuperRef(Box::new(SuperRefObject {
                            receiver: Box::new(Object::Instance(Box::new((*instance).clone()))),
                            methods: instance.super_methods.clone(),
                            getters: instance.super_getters.clone(),
                            setters: instance.super_setters.clone(),
                            constructor_chain: instance.super_constructor_chain.clone(),
                        }));
                        unsafe { *regs.add(dst) = obj_into_val(result, &mut self.heap) };
                    } else {
                        unsafe { *regs.add(dst) = Value::UNDEFINED };
                    }
                    ip += 3;
                }

                // ── Collections ─────────────────────────────────────────
                ROp::Array => {
                    let dst = Self::rd1(inst, ip + 1);
                    let base = Self::rd1(inst, ip + 3);
                    let count = Self::rd1(inst, ip + 5);
                    ip += 7;

                    let mut items: Vec<Value> = Vec::with_capacity(count);
                    for i in 0..count {
                        items.push(unsafe { *regs.add(base + i) });
                    }
                    let arr = make_array(items);
                    unsafe { *regs.add(dst) = obj_into_val(arr, &mut self.heap) };
                }
                ROp::Hash => {
                    let dst = Self::rd1(inst, ip + 1);
                    let base = Self::rd1(inst, ip + 3);
                    let count = Self::rd1(inst, ip + 5);
                    ip += 7;

                    let mut hash = crate::object::HashObject::default();
                    let num_pairs = count / 2;
                    // Intern the spread marker symbol once before the loop
                    let rest_sym = crate::intern::intern("__fl_rest__");
                    for i in 0..num_pairs {
                        let key_val = unsafe { *regs.add(base + i * 2) };
                        let val = unsafe { *regs.add(base + i * 2 + 1) };
                        // Value-native key: avoids val_to_obj clone per key
                        let key = self.hash_key_from_value(key_val);
                        // Handle __fl_rest__ spread marker by symbol ID
                        if matches!(&key, crate::object::HashKey::Sym(s) if *s == rest_sym) {
                            if val.is_heap() {
                                let spread_obj = self.heap.get(val.heap_index());
                                if let Object::Hash(spread_hash) = spread_obj {
                                    let spread = unsafe { spread_hash.borrow_mut() };
                                    spread.sync_pairs_if_dirty();
                                    for (k, v) in spread.pairs.iter() {
                                        hash.insert_pair(k.clone(), *v);
                                    }
                                }
                            }
                            continue;
                        }
                        hash.insert_pair(key, val);
                    }
                    let result = make_hash(hash);
                    unsafe { *regs.add(dst) = obj_into_val(result, &mut self.heap) };
                }
                ROp::AppendElement => {
                    let arr_r = Self::rd1(inst, ip + 1);
                    let val_r = Self::rd1(inst, ip + 3);
                    let arr_val = unsafe { *regs.add(arr_r) };
                    let val_v = unsafe { *regs.add(val_r) };
                    if arr_val.is_heap() {
                        let heap_obj = unsafe {
                            &*self
                                .heap
                                .objects
                                .as_ptr()
                                .add(arr_val.heap_index() as usize)
                        };
                        if let Object::Array(arr) = heap_obj {
                            let borrowed = unsafe { arr.borrow_mut() };
                            borrowed.push(val_v);
                            if borrowed.len() > MAX_ARRAY_SIZE {
                                return Err(VMError::TypeError(
                                    crate::vm::ERR_ARRAY_SIZE.to_string(),
                                ));
                            }
                        } else {
                            return Err(VMError::TypeError(
                                crate::vm::ERR_APPEND_TARGET.to_string(),
                            ));
                        }
                    } else {
                        return Err(VMError::TypeError(
                            crate::vm::ERR_APPEND_TARGET.to_string(),
                        ));
                    }
                    ip += 5;
                }
                ROp::AppendSpread => {
                    let (arr_r, iter_r) = Self::rd2(inst, ip);
                    let arr_val = unsafe { *regs.add(arr_r) };
                    let spread_val = unsafe { *regs.add(iter_r) };
                    self.exec_append_spread(arr_val, spread_val)?;
                    ip += 5;
                }

                // ── Property access ─────────────────────────────────────
                ROp::GetProp => {
                    let (dst, obj_r) = Self::rd2(inst, ip);
                    let prop_idx = Self::rd1(inst, ip + 5);
                    let cache_slot = Self::rd1(inst, ip + 7);
                    ip += 9;

                    let obj_val = unsafe { *regs.add(obj_r) };
                    if obj_val.is_heap() {
                        let heap_idx = obj_val.heap_index() as usize;
                        let heap_obj = unsafe { &*self.heap.objects.as_ptr().add(heap_idx) };
                        let (cached_shape, cached_offset) =
                            unsafe { *icache.add(cache_slot) };
                        if cached_shape != 0 {
                            if let Object::Hash(hash_rc) = heap_obj {
                                let hash = hash_rc.borrow();
                                if cached_shape == hash.shape_version && !hash.has_accessors() {
                                    unsafe {
                                        *regs.add(dst) = *hash.values.get_unchecked(cached_offset as usize)
                                    };
                                    continue;
                                }
                            }
                        }

                        // .length fast path: u32 symbol compare instead of string match
                        let prop_sym = unsafe { *csyms.add(prop_idx) };
                        if prop_sym == sym_length {
                            match heap_obj {
                                Object::String(s) => {
                                    unsafe { *regs.add(dst) = Value::from_i64(s.len() as i64) };
                                    continue;
                                }
                                Object::StringRope(r) => {
                                    unsafe {
                                        *regs.add(dst) = Value::from_i64(r.total_len as i64)
                                    };
                                    continue;
                                }
                                Object::Array(arr) => {
                                    unsafe {
                                        *regs.add(dst) = Value::from_i64(arr.borrow().len() as i64)
                                    };
                                    continue;
                                }
                                _ => {}
                            }
                        }

                        // Slow path — Value-native (no Object conversion for Hash)
                        self.ip = ip;
                        let result = self.get_property_val(obj_val, prop_sym, cache_slot)?;
                        unsafe { *regs.add(dst) = result };
                        continue;
                    }

                    // Non-heap: handle inline strings (e.g. "abc".length)
                    if obj_val.is_inline_str() {
                        let prop_sym = unsafe { *csyms.add(prop_idx) };
                        if prop_sym == sym_length {
                            unsafe {
                                *regs.add(dst) = Value::from_i32(obj_val.inline_str_len() as i32)
                            };
                            continue;
                        }
                        self.ip = ip;
                        let result = self.get_property_val(obj_val, prop_sym, cache_slot)?;
                        unsafe { *regs.add(dst) = result };
                        continue;
                    }
                    // Other non-heap slow path
                    let prop_sym = unsafe { *csyms.add(prop_idx) };
                    self.ip = ip;
                    let result = self.get_property_val(obj_val, prop_sym, cache_slot)?;
                    unsafe { *regs.add(dst) = result };
                }
                ROp::SetProp => {
                    let (obj_r, prop_idx) = Self::rd2(inst, ip);
                    let src = Self::rd1(inst, ip + 5);
                    let cache_slot = Self::rd1(inst, ip + 7);
                    ip += 9;

                    let obj_val = unsafe { *regs.add(obj_r) };
                    if obj_val.is_heap() {
                        let (cached_shape, cached_offset) =
                            unsafe { *icache.add(cache_slot) };
                        if cached_shape != 0 {
                            let heap_obj = unsafe {
                                &*self.heap.objects.as_ptr().add(obj_val.heap_index() as usize)
                            };
                            if let Object::Hash(hash_rc) = heap_obj {
                                let hash = unsafe { hash_rc.borrow_mut() };
                                if cached_shape == hash.shape_version
                                    && !hash.frozen
                                    && !hash.has_accessors()
                                {
                                    let src_val = unsafe { *regs.add(src) };
                                    unsafe {
                                        *hash.values.get_unchecked_mut(cached_offset as usize) = src_val
                                    };
                                    hash.pairs_dirty = true;
                                    continue;
                                }
                            }
                        }
                    }

                    // Slow path — Value-native (no Object conversion for Hash)
                    let src_val = unsafe { *regs.add(src) };
                    let prop_sym = unsafe { *csyms.add(prop_idx) };

                    self.ip = ip;
                    if let Some(updated) =
                        self.set_property_val(obj_val, prop_sym, src_val, cache_slot)?
                    {
                        unsafe { *regs.add(obj_r) = updated };
                    }
                }
                ROp::GetGlobalProp => {
                    let dst = Self::rd1(inst, ip + 1);
                    let global_idx = Self::rd1(inst, ip + 3);
                    let prop_idx = Self::rd1(inst, ip + 5);
                    let cache_slot = Self::rd1(inst, ip + 7);
                    ip += 9;

                    let gval = unsafe { self.globals.get_unchecked(global_idx) };
                    let prop_sym = unsafe { *csyms.add(prop_idx) };

                    self.ip = ip;
                    let result = self.get_property_val(gval, prop_sym, cache_slot)?;
                    unsafe { *regs.add(dst) = result };
                }
                ROp::SetGlobalProp => {
                    let global_idx = Self::rd1(inst, ip + 1);
                    let prop_idx = Self::rd1(inst, ip + 3);
                    let src = Self::rd1(inst, ip + 5);
                    let cache_slot = Self::rd1(inst, ip + 7);
                    ip += 9;

                    let gval = unsafe { self.globals.get_unchecked(global_idx) };
                    let src_val = unsafe { *regs.add(src) };
                    let prop_sym = unsafe { *csyms.add(prop_idx) };

                    self.ip = ip;
                    if let Some(updated) =
                        self.set_property_val(gval, prop_sym, src_val, cache_slot)?
                    {
                        unsafe { self.globals.set_unchecked(global_idx, updated) };
                    }
                }

                // ── Index access ────────────────────────────────────────
                ROp::Index => {
                    let (dst, obj_r, key_r) = Self::rd3(inst, ip);
                    ip += 7;

                    let obj_val = unsafe { *regs.add(obj_r) };
                    let key_val = unsafe { *regs.add(key_r) };

                    // Fast path: array[i32] or hash[string] — direct access without val_to_obj
                    if obj_val.is_heap() {
                        let hp = self.heap.objects.as_ptr();
                        let heap_obj = unsafe { &*hp.add(obj_val.heap_index() as usize) };
                        if key_val.is_i32() {
                            if let Object::Array(arr_rc) = heap_obj {
                                let idx = unsafe { key_val.as_i32_unchecked() };
                                if idx >= 0 {
                                    let arr = arr_rc.borrow();
                                    let i = idx as usize;
                                    if i < arr.len() {
                                        let val = unsafe { *arr.get_unchecked(i) };
                                        unsafe { *regs.add(dst) = val };
                                        continue;
                                    } else {
                                        unsafe { *regs.add(dst) = Value::UNDEFINED };
                                        continue;
                                    }
                                }
                            }
                        } else if key_val.is_heap() {
                            // Hash[string] fast path
                            if let Object::Hash(hash_rc) = heap_obj {
                                let key_heap = unsafe { &*hp.add(key_val.heap_index() as usize) };
                                if let Object::String(s) = key_heap {
                                    let sym = crate::intern::intern(s);
                                    let val = hash_rc
                                        .borrow()
                                        .get_by_sym(sym)
                                        .unwrap_or(Value::UNDEFINED);
                                    let result = self.maybe_bind_method_val(val, obj_val)?;
                                    unsafe { *regs.add(dst) = result };
                                    continue;
                                }
                            }
                        } else if key_val.is_inline_str() {
                            // Hash[inline_string] fast path
                            if let Object::Hash(hash_rc) = heap_obj {
                                let (buf, len) = key_val.inline_str_buf();
                                let s = unsafe { std::str::from_utf8_unchecked(&buf[..len]) };
                                let sym = crate::intern::intern(s);
                                let val =
                                    hash_rc.borrow().get_by_sym(sym).unwrap_or(Value::UNDEFINED);
                                let result = self.maybe_bind_method_val(val, obj_val)?;
                                unsafe { *regs.add(dst) = result };
                                continue;
                            }
                        }
                    }

                    // Slow path — pop_val avoids Value→Object→Value roundtrip
                    let obj = val_to_obj(obj_val, &self.heap);
                    let key = val_to_obj(key_val, &self.heap);
                    self.ip = ip;
                    self.execute_index_expression(obj, key)?;
                    unsafe { *regs.add(dst) = self.pop_val()? };
                }
                ROp::SetIndex => {
                    let (obj_r, key_r, val_r) = Self::rd3(inst, ip);
                    ip += 7;

                    let obj_val = unsafe { *regs.add(obj_r) };
                    let key_val = unsafe { *regs.add(key_r) };

                    // Fast path: array[i32] or hash[string] = val — direct write
                    if obj_val.is_heap() {
                        let hp = self.heap.objects.as_ptr();
                        let heap_obj = unsafe { &*hp.add(obj_val.heap_index() as usize) };
                        if key_val.is_i32() {
                            if let Object::Array(arr_rc) = heap_obj {
                                let idx = unsafe { key_val.as_i32_unchecked() };
                                if idx >= 0 {
                                    let i = idx as usize;
                                    let val_v = unsafe { *regs.add(val_r) };
                                    let arr = unsafe { arr_rc.borrow_mut() };
                                    if i < arr.len() {
                                        unsafe { *arr.get_unchecked_mut(i) = val_v };
                                    } else {
                                        if i > MAX_ARRAY_SIZE {
                                            return Err(VMError::TypeError(
                                                crate::vm::ERR_ARRAY_SIZE.to_string(),
                                            ));
                                        }
                                        if i > arr.len() + crate::vm::SPARSE_ARRAY_THRESHOLD {
                                            // Sparse-array guard. JS allows the write
                                            // (V8 backs it with a sparse map); we use
                                            // dense `Vec` storage and don't, so writes
                                            // far beyond `len` are silently dropped to
                                            // keep the program running. This is a
                                            // documented divergence from spec — see
                                            // `SECURITY-AUDIT.md` and the same branch
                                            // in `vm::indexing::execute_set_index` and
                                            // `djit_set_index_helper`.
                                            continue;
                                        }
                                        arr.resize(i + 1, Value::UNDEFINED);
                                        unsafe { *arr.get_unchecked_mut(i) = val_v };
                                    }
                                    continue;
                                }
                            }
                        } else if key_val.is_heap() {
                            // Hash[string] = val fast path
                            if let Object::Hash(hash_rc) = heap_obj {
                                let key_heap = unsafe { &*hp.add(key_val.heap_index() as usize) };
                                if let Object::String(s) = key_heap {
                                    let sym = crate::intern::intern(s);
                                    let val_v = unsafe { *regs.add(val_r) };
                                    unsafe { hash_rc.borrow_mut() }.set_by_sym(sym, val_v);
                                    continue;
                                }
                            }
                        } else if key_val.is_inline_str() {
                            // Hash[inline_string] = val fast path
                            if let Object::Hash(hash_rc) = heap_obj {
                                let (buf, len) = key_val.inline_str_buf();
                                let s = unsafe { std::str::from_utf8_unchecked(&buf[..len]) };
                                let sym = crate::intern::intern(s);
                                let val_v = unsafe { *regs.add(val_r) };
                                unsafe { hash_rc.borrow_mut() }.set_by_sym(sym, val_v);
                                continue;
                            }
                        }
                    }

                    // Instance fast path: mutate fields in-place on the heap
                    // (Instance is Box<InstanceObject>, not Rc, so we must
                    //  write directly to avoid clone-and-lose semantics.)
                    if obj_val.is_heap() {
                        let heap_idx = obj_val.heap_index() as usize;
                        let is_instance = matches!(
                            self.heap.objects.get(heap_idx),
                            Some(Object::Instance(_))
                        );
                        if is_instance {
                            // Extract the string key
                            let key_str: Option<String> = if key_val.is_heap() {
                                let key_obj = unsafe {
                                    &*self.heap.objects.as_ptr().add(key_val.heap_index() as usize)
                                };
                                if let Object::String(s) = key_obj {
                                    Some(s.to_string())
                                } else {
                                    None
                                }
                            } else if key_val.is_inline_str() {
                                let (buf, len) = key_val.inline_str_buf();
                                let s = unsafe { std::str::from_utf8_unchecked(&buf[..len]) };
                                Some(s.to_string())
                            } else {
                                None
                            };
                            if let Some(prop_name) = key_str {
                                let val_v = unsafe { *regs.add(val_r) };
                                if let Object::Instance(inst) = &mut self.heap.objects[heap_idx] {
                                    inst.fields.insert(prop_name, val_v);
                                }
                                continue;
                            }
                        }
                    }

                    // Slow path — pop_val avoids Value→Object→Value roundtrip
                    let obj = val_to_obj(obj_val, &self.heap);
                    let key = val_to_obj(key_val, &self.heap);
                    let val = val_to_obj(unsafe { *regs.add(val_r) }, &self.heap);
                    self.ip = ip;
                    self.execute_set_index(obj, key, val)?;
                    unsafe { *regs.add(obj_r) = self.pop_val()? };
                }
                ROp::DeleteProp => {
                    let (dst, obj_r, key_r) = Self::rd3(inst, ip);
                    ip += 7;

                    let obj_val = unsafe { *regs.add(obj_r) };
                    let key_val = unsafe { *regs.add(key_r) };

                    // Fast path: Hash deletion without val_to_obj
                    if obj_val.is_heap() {
                        let heap_obj = unsafe {
                            &*self
                                .heap
                                .objects
                                .as_ptr()
                                .add(obj_val.heap_index() as usize)
                        };
                        if let Object::Hash(hash_rc) = heap_obj {
                            let k = self.hash_key_from_value(key_val);
                            unsafe { hash_rc.borrow_mut() }.remove_pair(&k);
                            // Hash is Rc, mutated in-place — no store-back needed
                            unsafe { *regs.add(dst) = Value::TRUE };
                            continue;
                        }
                    }

                    // Slow path for non-Hash types — pop_val avoids roundtrip
                    let obj = val_to_obj(obj_val, &self.heap);
                    let key = val_to_obj(key_val, &self.heap);
                    self.ip = ip;
                    self.execute_delete_property(obj, key)?;
                    unsafe { *regs.add(obj_r) = self.pop_val()? };
                    unsafe { *regs.add(dst) = Value::TRUE };
                }

                // ── Iterator / destructuring ────────────────────────────
                ROp::IteratorRest => {
                    let dst = Self::rd1(inst, ip + 1);
                    let iter_r = Self::rd1(inst, ip + 3);
                    let skip = Self::rd1(inst, ip + 5);
                    ip += 7;

                    let iter_val = unsafe { *regs.add(iter_r) };
                    let rest: Vec<Value> = if iter_val.is_heap() {
                        let heap_obj = unsafe {
                            &*self
                                .heap
                                .objects
                                .as_ptr()
                                .add(iter_val.heap_index() as usize)
                        };
                        if let Object::Array(arr_rc) = heap_obj {
                            let items = arr_rc.borrow();
                            if skip < items.len() {
                                items[skip..].to_vec()
                            } else {
                                vec![]
                            }
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    };
                    let result = make_array(rest);
                    unsafe { *regs.add(dst) = obj_into_val(result, &mut self.heap) };
                }
                ROp::GetKeysIter => {
                    let dst = Self::rd1(inst, ip + 1);
                    let obj_r = Self::rd1(inst, ip + 3);
                    ip += 5;

                    let obj_val = unsafe { *regs.add(obj_r) };
                    // get_keys_array takes Object; peek heap to avoid clone for Hash
                    let keys = if obj_val.is_heap() {
                        let heap_obj = unsafe {
                            &*self
                                .heap
                                .objects
                                .as_ptr()
                                .add(obj_val.heap_index() as usize)
                        };
                        if let Object::Hash(hash_rc) = heap_obj {
                            let hash_b = hash_rc.borrow();
                            let ordered = self.ordered_hash_keys_js(hash_b);
                            let mut out = Vec::with_capacity(ordered.len());
                            for key in ordered {
                                out.push(obj_into_val(
                                    self.object_from_hash_key(&key),
                                    &mut self.heap,
                                ));
                            }
                            out
                        } else {
                            self.get_keys_array(val_to_obj(obj_val, &self.heap))
                        }
                    } else {
                        vec![]
                    };
                    let result = make_array(keys);
                    unsafe { *regs.add(dst) = obj_into_val(result, &mut self.heap) };
                }
                ROp::ObjectRest => {
                    let dst = Self::rd1(inst, ip + 1);
                    let obj_r = Self::rd1(inst, ip + 3);
                    let keys_base = Self::rd1(inst, ip + 5);
                    let count = Self::rd1(inst, ip + 7);
                    ip += 9;

                    // Collect excluded keys — Value-native, no val_to_obj
                    let mut excluded = rustc_hash::FxHashSet::default();
                    excluded.reserve(count);
                    for i in 0..count {
                        let key_val = unsafe { *regs.add(keys_base + i) };
                        excluded.insert(self.hash_key_from_value(key_val));
                    }

                    // Peek heap for source hash — no val_to_obj
                    let source_val = unsafe { *regs.add(obj_r) };
                    let mut out = crate::object::HashObject::default();
                    if source_val.is_heap() {
                        let heap_obj = unsafe {
                            &*self
                                .heap
                                .objects
                                .as_ptr()
                                .add(source_val.heap_index() as usize)
                        };
                        if let Object::Hash(h) = heap_obj {
                            let h = unsafe { h.borrow_mut() };
                            h.sync_pairs_if_dirty();
                            for k in h.ordered_keys_ref() {
                                if !excluded.contains(&k) {
                                    let v = *h.pairs.get(&k).expect("hash key_order out of sync");
                                    out.insert_pair(k.clone(), v);
                                }
                            }
                        }
                    }

                    unsafe { *regs.add(dst) = obj_into_val(make_hash(out), &mut self.heap) };
                }

                // ── Async ───────────────────────────────────────────────
                ROp::Await => {
                    let (dst, src) = Self::rd2(inst, ip);
                    let val = unsafe { *regs.add(src) };
                    // Heap-peek: only inspect Promise objects, pass everything else through
                    let result = if val.is_heap() {
                        let heap_obj = self.heap.get(val.heap_index());
                        if let Object::Promise(p) = heap_obj {
                            let state = p.borrow().settled.clone();
                            match state {
                                PromiseState::Fulfilled(v) => {
                                    obj_into_val((*v).clone(), &mut self.heap)
                                }
                                PromiseState::Rejected(v) => {
                                    return Err(VMError::TypeError(format!(
                                        "Await rejected: {}",
                                        v.inspect()
                                    )));
                                }
                                PromiseState::Pending => {
                                    // Engine has no real fiber/coroutine pause
                                    // mechanism here; returning undefined
                                    // matches the existing yield-based async
                                    // semantics where a still-pending Promise
                                    // resolves to undefined.
                                    Value::UNDEFINED
                                }
                            }
                        } else {
                            val // non-Promise heap object — pass through as-is
                        }
                    } else {
                        val // inline value (i32/f64/bool/null/undefined) — zero work
                    };
                    unsafe { *regs.add(dst) = result };
                    ip += 5;
                }

                // ── Exception handling ───────────────────────────────────
                ROp::EnterTry => {
                    // EnterTry: [catch_target:u32, exception_dst:u16] = 7 bytes
                    let catch_target = Self::rd1_u32(inst, ip + 1) as usize;
                    let exc_reg = Self::rd1(inst, ip + 5);
                    self.try_handlers.push((
                        catch_target,     // catch IP
                        exc_reg,          // exception register
                        inst,             // instruction buffer pointer (local, not self.inst_ptr)
                        self.inst_len,    // instruction length
                        reg_base,         // register base
                        self.rframes.len(), // rframe depth at try entry
                        self.constants_raw, // constants pointer
                    ));
                    ip += 7;
                }
                ROp::LeaveTry => {
                    if !self.try_handlers.is_empty() {
                        self.try_handlers.pop();
                    }
                    ip += 1;
                }
                ROp::MakeArguments => {
                    let dst = Self::rd1(inst, ip + 1);
                    let arg_start = Self::rd1(inst, ip + 3);
                    let _num_formal = Self::rd1(inst, ip + 5);
                    ip += 7;
                    let nargs = self.last_call_nargs as usize;
                    let mut args: Vec<Value> = Vec::with_capacity(nargs);
                    for i in 0..nargs {
                        let val = unsafe { *regs.add(arg_start + i) };
                        args.push(val);
                    }
                    let arr_val = obj_into_val(make_array(args), &mut self.heap);
                    unsafe { *regs.add(dst) = arr_val };
                }
                ROp::Throw => {
                    let src = Self::rd1(inst, ip + 1);
                    let val = unsafe { *regs.add(src) };
                    match self.try_catch_error(VMError::Throw(val)) {
                        Ok(Some((cip, cb))) => {
                            ip = cip; reg_base = cb;
                            regs = unsafe { self.stack.as_mut_ptr().add(reg_base) };
                            inst = self.inst_ptr;
                            // When the throw unwound across function frames,
                            // try_catch_error swapped self.inst_ptr / self.inst_len
                            // back to the handler's function. The local `inst_len`
                            // must follow, otherwise the next bounds check in the
                            // dispatch loop compares the new `ip` against the
                            // callee's (shorter) instruction length.
                            inst_len = self.inst_len;
                            cvals = self.constants_values_ptr;
                            csyms = self.constants_syms_ptr;
                            icache = self.inline_cache.as_ptr();
                            continue;
                        }
                        Err(e) => return Err(e),
                        _ => unreachable!(),
                    }
                }

                // ── Halt ────────────────────────────────────────────────
                // ── Fused opcodes ───────────────────────────────────────
                ROp::AddRegConst => {
                    let (dst, src, const_idx) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(src) };
                    let cv = unsafe { *cvals.add(const_idx) };
                    // Both must be i32 — a register i32 plus an f64 constant
                    // (e.g. `r + 3.14`) would otherwise reinterpret the f64
                    // bits as an i32 and produce garbage.
                    if lv.is_i32() && cv.is_i32() {
                        let a = unsafe { lv.as_i32_unchecked() };
                        let b = unsafe { cv.as_i32_unchecked() };
                        unsafe {
                            *regs.add(dst) = match a.checked_add(b) {
                                Some(sum) => Value::from_i32(sum),
                                None => Value::from_f64(a as f64 + b as f64),
                            }
                        };
                    } else if lv.is_number() {
                        unsafe {
                            *regs.add(dst) = Value::from_f64(lv.to_number() + cv.to_number())
                        };
                    } else if lv.is_heap() || lv.is_inline_str() {
                        unsafe { *regs.add(dst) = self.add_string_or_object(lv, cv)? };
                    } else {
                        unsafe { *regs.add(dst) = self.add_slow(lv, cv)? };
                    }
                    ip += 7;
                    // ── Opcode threading: inline Jump after AddRegConst ──
                    // Common in if/else bodies: count += N; jump past_else.
                    let next = unsafe { *inst.add(ip) };
                    if next == ROp::Jump as u8 {
                        ip = Self::rd1_u32(inst, ip + 1);
                        continue;
                    }
                }
                ROp::SubRegConst => {
                    let (dst, src, const_idx) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(src) };
                    let cv = unsafe { *cvals.add(const_idx) };
                    if lv.is_i32() && cv.is_i32() {
                        let a = unsafe { lv.as_i32_unchecked() };
                        let b = unsafe { cv.as_i32_unchecked() };
                        unsafe {
                            *regs.add(dst) = match a.checked_sub(b) {
                                Some(diff) => Value::from_i32(diff),
                                None => Value::from_f64(a as f64 - b as f64),
                            }
                        };
                    } else if lv.is_number() {
                        unsafe {
                            *regs.add(dst) = Value::from_f64(lv.to_number() - cv.to_number())
                        };
                    } else {
                        unsafe { *regs.add(dst) = self.fused_sub_const_slow(lv, const_idx)? };
                    }
                    ip += 7;
                }
                ROp::MulRegConst => {
                    let (dst, src, const_idx) = Self::rd3(inst, ip);
                    let lv = unsafe { *regs.add(src) };
                    let cv = unsafe { *cvals.add(const_idx) };
                    if lv.is_i32() && cv.is_i32() {
                        let a = unsafe { lv.as_i32_unchecked() };
                        let b = unsafe { cv.as_i32_unchecked() };
                        unsafe {
                            *regs.add(dst) = match a.checked_mul(b) {
                                Some(prod) => Value::from_i32(prod),
                                None => Value::from_f64(a as f64 * b as f64),
                            }
                        };
                    } else if lv.is_number() {
                        unsafe {
                            *regs.add(dst) = Value::from_f64(lv.to_number() * cv.to_number())
                        };
                    } else {
                        unsafe { *regs.add(dst) = self.fused_mul_const_slow(lv, const_idx)? };
                    }
                    ip += 7;
                }
                ROp::TestLtConstJump => {
                    let (r, const_idx) = Self::rd2(inst, ip);
                    let target = Self::rd1_u32(inst, ip + 5);
                    let lv = unsafe { *regs.add(r) };
                    let cv = unsafe { *cvals.add(const_idx) };
                    // Both-i32 guard: an f64 constant would otherwise be
                    // reinterpreted through `as_i32_unchecked`, giving
                    // garbage. Falls through to the .is_number() path when
                    // the constant is a float literal (e.g. `i < 1.5`).
                    let passes = if lv.is_i32() && cv.is_i32() {
                        unsafe { lv.as_i32_unchecked() < cv.as_i32_unchecked() }
                    } else if lv.is_number() {
                        lv.to_number() < cv.to_number()
                    } else {
                        self.fused_test_lt_slow(lv, const_idx)?
                    };
                    if passes {
                        ip += 9;
                    } else {
                        // Backward branch: full limit check (abort_flag,
                        // heap bytes, interner cap). Matches the plain
                        // `Jump` arm. The top-of-loop 64 K batched check
                        // only covers instruction count + wall time, so
                        // without this a fused loop bypasses
                        // abort_flag / max_heap_bytes / interner cap.
                        if enforce_limits && target <= ip {
                            self.check_execution_limits()?;
                        }
                        ip = target;
                    }
                }
                ROp::TestLeConstJump => {
                    let (r, const_idx) = Self::rd2(inst, ip);
                    let target = Self::rd1_u32(inst, ip + 5);
                    let lv = unsafe { *regs.add(r) };
                    let cv = unsafe { *cvals.add(const_idx) };
                    let passes = if lv.is_i32() && cv.is_i32() {
                        unsafe { lv.as_i32_unchecked() <= cv.as_i32_unchecked() }
                    } else if lv.is_number() {
                        lv.to_number() <= cv.to_number()
                    } else {
                        self.fused_test_le_slow(lv, const_idx)?
                    };
                    if passes {
                        ip += 9;
                    } else {
                        if enforce_limits && target <= ip {
                            self.check_execution_limits()?;
                        }
                        ip = target;
                    }
                }
                ROp::IncrementRegAndJump => {
                    let (r, const_idx) = Self::rd2(inst, ip);
                    let target = Self::rd1_u32(inst, ip + 5);
                    let lv = unsafe { *regs.add(r) };
                    let cv = unsafe { *cvals.add(const_idx) };
                    if lv.is_i32() && cv.is_i32() {
                        let a = unsafe { lv.as_i32_unchecked() };
                        let b = unsafe { cv.as_i32_unchecked() };
                        unsafe {
                            *regs.add(r) = match a.checked_add(b) {
                                Some(sum) => Value::from_i32(sum),
                                None => Value::from_f64(a as f64 + b as f64),
                            }
                        };
                    } else if lv.is_number() {
                        unsafe { *regs.add(r) = Value::from_f64(lv.to_number() + cv.to_number()) };
                    } else {
                        unsafe { *regs.add(r) = self.fused_add_const_slow(lv, const_idx)? };
                    }
                    // Loop back-edge: always a backward branch by construction.
                    // See `TestLtConstJump` for why we still need this even with
                    // the top-of-loop 64 K batched check.
                    if enforce_limits && target <= ip {
                        self.check_execution_limits()?;
                    }
                    ip = target;

                    // ── Opcode threading: inline the condition test at loop start ──
                    // After backward jump, the next opcode is almost always
                    // TestLe/LtConstJump. Inlining it saves one match dispatch per
                    // loop iteration.
                    let next_op = unsafe { *inst.add(ip) };
                    if next_op == ROp::TestLeConstJump as u8
                        || next_op == ROp::TestLtConstJump as u8
                    {
                        let cmp_r = Self::rd1(inst, ip + 1);
                        let cmp_const = Self::rd1(inst, ip + 3);
                        let cmp_target = Self::rd1_u32(inst, ip + 5);
                        let cmp_lv = unsafe { *regs.add(cmp_r) };
                        let cmp_cv = unsafe { *cvals.add(cmp_const) };
                        if cmp_lv.is_i32() && cmp_cv.is_i32() {
                            let a = unsafe { cmp_lv.as_i32_unchecked() };
                            let b = unsafe { cmp_cv.as_i32_unchecked() };
                            if (next_op == ROp::TestLeConstJump as u8 && a <= b)
                                || (next_op != ROp::TestLeConstJump as u8 && a < b)
                            {
                                ip += 9;
                            } else {
                                if enforce_limits && cmp_target <= ip {
                                    self.check_execution_limits()?;
                                }
                                ip = cmp_target;
                            }
                            continue;
                        }
                        // f64 path: still worth threading to avoid re-dispatch
                        if cmp_lv.is_number() {
                            let a = cmp_lv.to_number();
                            let b = cmp_cv.to_number();
                            if (next_op == ROp::TestLeConstJump as u8 && a <= b)
                                || (next_op != ROp::TestLeConstJump as u8 && a < b)
                            {
                                ip += 9;
                            } else {
                                if enforce_limits && cmp_target <= ip {
                                    self.check_execution_limits()?;
                                }
                                ip = cmp_target;
                            }
                            continue;
                        }
                        // Non-number: fall through to normal dispatch at ip
                    }
                }
                ROp::ModRegConstStrictEqConstJump => {
                    let r = Self::rd1(inst, ip + 1);
                    let mod_const_idx = Self::rd1(inst, ip + 3);
                    let cmp_const_idx = Self::rd1(inst, ip + 5);
                    let target = Self::rd1_u32(inst, ip + 7);
                    let lv = unsafe { *regs.add(r) };
                    let mod_cv = unsafe { *cvals.add(mod_const_idx) };
                    let cmp_cv = unsafe { *cvals.add(cmp_const_idx) };
                    // Both constants and the register must be i32 — a float
                    // modulo (`i % 2.5`) would garbage-read the f64 bits.
                    let passes = if lv.is_i32() && mod_cv.is_i32() && cmp_cv.is_i32() {
                        let a = unsafe { lv.as_i32_unchecked() };
                        let b = unsafe { mod_cv.as_i32_unchecked() };
                        let c = unsafe { cmp_cv.as_i32_unchecked() };
                        if b == 3 {
                            (a % 3) == c
                        } else if b == 5 {
                            (a % 5) == c
                        } else if b > 0 && (b & (b - 1)) == 0 {
                            (a & (b - 1)) == c
                        } else {
                            b != 0 && (a % b) == c
                        }
                    } else {
                        self.fused_mod_strict_eq_slow(lv, mod_const_idx, cmp_const_idx)?
                    };
                    if passes {
                        ip += 11;
                    } else {
                        if enforce_limits && target <= ip {
                            self.check_execution_limits()?;
                        }
                        ip = target;
                    }
                }
                ROp::TestLtRegJump => {
                    let (a_r, b_r) = Self::rd2(inst, ip);
                    let target = Self::rd1_u32(inst, ip + 5);
                    let lv = unsafe { *regs.add(a_r) };
                    let rv = unsafe { *regs.add(b_r) };
                    let passes = if Value::both_i32(lv, rv) {
                        unsafe { lv.as_i32_unchecked() < rv.as_i32_unchecked() }
                    } else if lv.is_number() && rv.is_number() {
                        lv.to_number() < rv.to_number()
                    } else {
                        self.comparison_slow(ROp::LessThan, lv, rv)?
                    };
                    if passes {
                        ip += 9;
                    } else {
                        if enforce_limits && target <= ip {
                            self.check_execution_limits()?;
                        }
                        ip = target;
                    }
                }
                ROp::TestLeRegJump => {
                    let (a_r, b_r) = Self::rd2(inst, ip);
                    let target = Self::rd1_u32(inst, ip + 5);
                    let lv = unsafe { *regs.add(a_r) };
                    let rv = unsafe { *regs.add(b_r) };
                    let passes = if Value::both_i32(lv, rv) {
                        unsafe { lv.as_i32_unchecked() <= rv.as_i32_unchecked() }
                    } else if lv.is_number() && rv.is_number() {
                        lv.to_number() <= rv.to_number()
                    } else {
                        self.comparison_slow(ROp::LessOrEqual, lv, rv)?
                    };
                    if passes {
                        ip += 9;
                    } else {
                        if enforce_limits && target <= ip {
                            self.check_execution_limits()?;
                        }
                        ip = target;
                    }
                }
                ROp::TestModRegStrictEqConstJump => {
                    let a_r = Self::rd1(inst, ip + 1);
                    let b_r = Self::rd1(inst, ip + 3);
                    let cmp_const_idx = Self::rd1(inst, ip + 5);
                    let target = Self::rd1_u32(inst, ip + 7);
                    let lv = unsafe { *regs.add(a_r) };
                    let rv = unsafe { *regs.add(b_r) };
                    let cmp_cv = unsafe { *cvals.add(cmp_const_idx) };
                    let passes = if lv.is_i32() && rv.is_i32() && cmp_cv.is_i32() {
                        let a = unsafe { lv.as_i32_unchecked() };
                        let b = unsafe { rv.as_i32_unchecked() };
                        let c = unsafe { cmp_cv.as_i32_unchecked() };
                        b != 0 && (a % b) == c
                    } else {
                        false
                    };
                    if passes {
                        ip += 11;
                    } else {
                        if enforce_limits && target <= ip {
                            self.check_execution_limits()?;
                        }
                        ip = target;
                    }
                }
                ROp::AddConstToRegProp => {
                    let obj_r = Self::rd1(inst, ip + 1);
                    let prop_const_idx = Self::rd1(inst, ip + 3);
                    let val_const_idx = Self::rd1(inst, ip + 5);
                    let cache_slot = Self::rd1(inst, ip + 7);

                    let obj_val = unsafe { *regs.add(obj_r) };
                    if obj_val.is_heap() {
                        let heap_obj = unsafe {
                            &*self.heap.objects.as_ptr().add(obj_val.heap_index() as usize)
                        };
                        if let Object::Hash(hash_rc) = heap_obj {
                            let hash = unsafe { hash_rc.borrow_mut() };
                            let (cached_shape, cached_offset) =
                                unsafe { *icache.add(cache_slot) };
                            if cached_shape == hash.shape_version
                                && !hash.frozen
                                && !hash.has_accessors()
                            {
                                let slot = cached_offset as usize;
                                let prop_val = unsafe { *hash.values.get_unchecked(slot) };
                                let add_cv = unsafe { *cvals.add(val_const_idx) };
                                let result = if Value::both_i32(prop_val, add_cv) {
                                    let a = unsafe { prop_val.as_i32_unchecked() };
                                    let b = unsafe { add_cv.as_i32_unchecked() };
                                    match a.checked_add(b) {
                                        Some(sum) => Value::from_i32(sum),
                                        None => Value::from_f64(a as f64 + b as f64),
                                    }
                                } else if prop_val.is_number() && add_cv.is_number() {
                                    Value::from_f64(prop_val.to_number() + add_cv.to_number())
                                } else {
                                    let lo = val_as_obj_ref(prop_val, &self.heap);
                                    let ro = val_as_obj_ref(add_cv, &self.heap);
                                    obj_into_val(self.add_objects(&lo, &ro)?, &mut self.heap)
                                };
                                unsafe { *hash.values.get_unchecked_mut(slot) = result };
                                hash.pairs_dirty = true;
                                ip += 9;
                                continue;
                            }
                        }
                    }

                    // Cache miss: cold slow path
                    self.ip = ip;
                    self.fused_add_const_to_prop_slow(
                        obj_val,
                        obj_r,
                        prop_const_idx,
                        val_const_idx,
                        cache_slot,
                        regs,
                    )?;
                    ip += 9;
                }
                ROp::AddRegPropsToRegProp => {
                    let obj_r = Self::rd1(inst, ip + 1);
                    let s1_cache = Self::rd1(inst, ip + 5);
                    let s2_cache = Self::rd1(inst, ip + 9);
                    let dst_cache = Self::rd1(inst, ip + 13);

                    let obj_val = unsafe { *regs.add(obj_r) };
                    if obj_val.is_heap() {
                        let heap_obj = unsafe {
                            &*self.heap.objects.as_ptr().add(obj_val.heap_index() as usize)
                        };
                        if let Object::Hash(hash_rc) = heap_obj {
                            let hash = unsafe { hash_rc.borrow_mut() };
                            let (s1_shape, s1_slot) =
                                unsafe { *icache.add(s1_cache) };
                            let (s2_shape, s2_slot) =
                                unsafe { *icache.add(s2_cache) };
                            let (dst_shape, dst_slot) =
                                unsafe { *icache.add(dst_cache) };
                            let shape = hash.shape_version;
                            if s1_shape == shape
                                && s2_shape == shape
                                && dst_shape == shape
                                && !hash.frozen
                                && !hash.has_accessors()
                            {
                                let s1 = s1_slot as usize;
                                let s2 = s2_slot as usize;
                                let d = dst_slot as usize;
                                debug_assert!(s1 < hash.values.len());
                                debug_assert!(s2 < hash.values.len());
                                debug_assert!(d < hash.values.len());
                                let val1 = unsafe { *hash.values.get_unchecked(s1) };
                                let val2 = unsafe { *hash.values.get_unchecked(s2) };
                                // Value-native arithmetic
                                let result = if Value::both_i32(val1, val2) {
                                    let a = unsafe { val1.as_i32_unchecked() };
                                    let b = unsafe { val2.as_i32_unchecked() };
                                    match a.checked_add(b) {
                                        Some(sum) => Value::from_i32(sum),
                                        None => Value::from_f64(a as f64 + b as f64),
                                    }
                                } else if val1.is_number() && val2.is_number() {
                                    Value::from_f64(val1.to_number() + val2.to_number())
                                } else {
                                    let lo = val_as_obj_ref(val1, &self.heap);
                                    let ro = val_as_obj_ref(val2, &self.heap);
                                    obj_into_val(self.add_objects(&lo, &ro)?, &mut self.heap)
                                };
                                unsafe { *hash.values.get_unchecked_mut(d) = result };
                                hash.pairs_dirty = true;
                                ip += 15;
                                continue;
                            }
                        }
                    }

                    // Cache miss: cold slow path
                    let s1_prop_idx = Self::rd1(inst, ip + 3);
                    let s2_prop_idx = Self::rd1(inst, ip + 7);
                    let dst_prop_idx = Self::rd1(inst, ip + 11);
                    self.ip = ip;
                    self.fused_add_reg_props_slow(
                        obj_val,
                        obj_r,
                        s1_prop_idx,
                        s2_prop_idx,
                        dst_prop_idx,
                        s1_cache,
                        s2_cache,
                        dst_cache,
                        regs,
                    )?;
                    ip += 15;
                }

                ROp::DefineAccessor => {
                    // Operands: [hash_r: u16, func_r: u16, prop_const_idx: u16, kind: u8]
                    let hash_r = Self::rd1(inst, ip + 1);
                    let func_r = Self::rd1(inst, ip + 3);
                    let prop_idx = Self::rd1(inst, ip + 5);
                    let kind = unsafe { *inst.add(ip + 7) };

                    let func_val = unsafe { *regs.add(func_r) };
                    let hash_val = unsafe { *regs.add(hash_r) };

                    // Use function-local constants (not program-level self.constants)
                    let local_constants = unsafe { &*self.constants_raw };
                    let prop_name = match local_constants.get(prop_idx) {
                        Some(Object::String(s)) => s.to_string(),
                        _ => {
                            return Err(VMError::TypeError(
                                format!("DefineAccessor: expected string constant at idx {} (len={})",
                                    prop_idx, local_constants.len()),
                            ))
                        }
                    };
                    let func_obj = val_to_obj(func_val, &self.heap);
                    let compiled_fn = match func_obj {
                        Object::CompiledFunction(f) => (*f).clone(),
                        _ => {
                            return Err(VMError::TypeError(
                                "DefineAccessor: expected function".to_string(),
                            ))
                        }
                    };
                    let hash_obj = val_to_obj(hash_val, &self.heap);
                    match hash_obj {
                        Object::Hash(h) => {
                            let ho = unsafe { h.borrow_mut() };
                            if kind == 0 {
                                ho.define_getter(prop_name, compiled_fn);
                            } else {
                                ho.define_setter(prop_name, compiled_fn);
                            }
                        }
                        _ => {
                            return Err(VMError::TypeError(
                                "DefineAccessor: expected hash object".to_string(),
                            ))
                        }
                    }
                    ip += 8;
                }

                ROp::InitClass => {
                    // Operands: [dst:2] — class register to init in-place
                    let dst = Self::rd1(inst, ip + 1);
                    let class_val = unsafe { *regs.add(dst) };
                    let class_obj = val_to_obj(class_val, &self.heap);
                    match class_obj {
                        Object::Class(mut class_box) => {
                            let inits = std::mem::take(&mut class_box.static_initializers);
                            for init in &inits {
                                match init {
                                    crate::object::StaticInitializer::Field { name, thunk } => {
                                        let receiver_val = obj_into_val(
                                            Object::Class(class_box.clone()),
                                            &mut self.heap,
                                        );
                                        let (result, _) = self.execute_compiled_function_slice(
                                            thunk.clone(),
                                            &[],
                                            Some(receiver_val),
                                        )?;
                                        let result_obj = val_to_obj(result, &self.heap);
                                        class_box.static_fields.insert(name.clone(), result_obj);
                                    }
                                    crate::object::StaticInitializer::Block { thunk } => {
                                        let receiver_val = obj_into_val(
                                            Object::Class(class_box.clone()),
                                            &mut self.heap,
                                        );
                                        self.execute_compiled_function_slice(
                                            thunk.clone(),
                                            &[],
                                            Some(receiver_val),
                                        )?;
                                    }
                                }
                            }
                            class_box.static_initializers = inits;
                            // Write updated class back to register and heap
                            let updated_val =
                                obj_into_val(Object::Class(class_box), &mut self.heap);
                            unsafe { *regs.add(dst) = updated_val };
                            // Also need to refresh regs pointer since
                            // execute_compiled_function_slice may have extended the stack
                            regs = unsafe { self.stack.as_mut_ptr().add(reg_base) };
                        }
                        other => {
                            return Err(VMError::TypeError(format!(
                                "InitClass: expected class, got {:?}",
                                other.object_type()
                            )));
                        }
                    }
                    ip += 3;
                }

                ROp::NewTarget => {
                    // Operands: [dst:2]
                    let dst = Self::rd1(inst, ip + 1);
                    unsafe { *regs.add(dst) = self.new_target };
                    ip += 3;
                }
                ROp::ImportMeta => {
                    // Operands: [dst:2]
                    let dst = Self::rd1(inst, ip + 1);
                    let empty_hash = Object::Hash(Rc::new(crate::object::VmCell::new(
                        crate::object::HashObject::with_capacity(0),
                    )));
                    unsafe { *regs.add(dst) = obj_into_val(empty_hash, &mut self.heap) };
                    ip += 3;
                }

                ROp::Yield => {
                    // Operands: [dst:2, src:2]
                    // dst (at ip+1) = register where the resume value goes (recovered on resume)
                    // src = register containing the yielded value
                    let src = Self::rd1(inst, ip + 3);
                    let yielded = unsafe { *regs.add(src) };
                    ip += 5;
                    // Save ip AFTER the yield instruction so resume continues past it.
                    // The dst register index can be recovered from instruction bytes at
                    // saved_ip - 2 when resuming.
                    self.ip = ip;
                    return Err(VMError::Yield(yielded));
                }

                ROp::MakeClosure => {
                    let dst = Self::rd1(inst, ip + 1);
                    let const_idx = Self::rd1(inst, ip + 3);
                    let count = Self::rd1_u8(inst, ip + 5);
                    ip += 6; // past fixed part

                    // Read the function from constants and clone it
                    let func_obj = unsafe { &*self.constants_raw };
                    let mut func = match &func_obj[const_idx] {
                        Object::CompiledFunction(f) => (**f).clone(),
                        _ => {
                            return Err(VMError::TypeError(
                                "MakeClosure: not a function".to_string(),
                            ))
                        }
                    };

                    // Snapshot captured global slot values
                    let mut captured = Vec::with_capacity(count);
                    for _ in 0..count {
                        let slot = Self::rd1(inst, ip);
                        ip += 2;
                        let val = self.get_global_as_value(slot);
                        captured.push((slot as u16, val));
                    }
                    func.captured_values = captured;

                    // Allocate on heap and store in register
                    let val = obj_into_val(
                        Object::CompiledFunction(Box::new(func)),
                        &mut self.heap,
                    );
                    unsafe { *regs.add(dst) = val };
                }

                ROp::Halt => {
                    self.quota.instructions += loop_counter;
                    if enforce_limits && self.quota.instructions > max_inst {
                        return Err(crate::vm::VMError::ExecutionTimeout(format!(
                            "Exceeded {} instructions", max_inst)));
                    }
                    #[cfg(feature = "zkvm")]
                    if self.trace_enabled {
                        self.trace_steps.push((self.trace_clk, trace_ip as u64, 76, 0, 0, 0, 0, 0));
                        self.trace_clk += 1;
                    }
                    return Ok(());
                }
                ROp::HaltValue => {
                    let src = Self::rd1(inst, ip + 1);
                    let val = unsafe { *regs.add(src) };
                    self.quota.instructions += loop_counter;
                    if enforce_limits && self.quota.instructions > max_inst {
                        return Err(crate::vm::VMError::ExecutionTimeout(format!(
                            "Exceeded {} instructions", max_inst)));
                    }
                    self.last_popped = Some(val);
                    #[cfg(feature = "zkvm")]
                    if self.trace_enabled {
                        let vd = if val.is_i32() { (unsafe { val.as_i32_unchecked() }) as u64 } else { val.bits() };
                        self.trace_steps.push((self.trace_clk, trace_ip as u64, 77, 0, 0, vd, 0, 0));
                        self.trace_clk += 1;
                    }
                    return Ok(());
                }
            }

            // Post-execution ZK trace capture — records the state AFTER the instruction ran.
            // Gated behind zkvm feature to eliminate ~220 lines of code from the hot dispatch
            // function, dramatically improving icache utilization.
            #[cfg(feature = "zkvm")]
            if self.trace_enabled {
                let (va, vb, vd, cv, ax) = match trace_op {
                    // Binary arithmetic: store SEMANTIC numeric values (not NaN-boxed bits)
                    // so field element arithmetic in the STARK constraint matches.
                    // Excludes 12 (MOD) and 13 (POW) which have specialized handling below.
                    8..=11 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let left_r = self.read_u16(trace_ip + 3) as usize;
                        let right_r = self.read_u16(trace_ip + 5) as usize;
                        unsafe {
                            let lv = *regs.add(left_r);
                            let rv = *regs.add(right_r);
                            let dv = *regs.add(dst_r);
                            // Extract semantic numeric values for ZK constraints
                            let la = if lv.is_i32() { lv.as_i32_unchecked() as u64 } else { lv.as_f64().to_bits() };
                            let ra = if rv.is_i32() { rv.as_i32_unchecked() as u64 } else { rv.as_f64().to_bits() };
                            let da = if dv.is_i32() { dv.as_i32_unchecked() as u64 } else { dv.as_f64().to_bits() };
                            (la, ra, da, 0u64, 0u64)
                        }
                    }
                    // Comparisons: result is 1 (true) or 0 (false) in AUX
                    14..=21 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let left_r = self.read_u16(trace_ip + 3) as usize;
                        let right_r = self.read_u16(trace_ip + 5) as usize;
                        let dst_val = unsafe { (*regs.add(dst_r)).bits() };
                        let cmp_bool = if dst_val == Value::TRUE.bits() { 1u64 } else { 0u64 };
                        unsafe {
                            let lv = *regs.add(left_r);
                            let rv = *regs.add(right_r);
                            let la = if lv.is_i32() { lv.as_i32_unchecked() as u64 } else { lv.bits() };
                            let ra = if rv.is_i32() { rv.as_i32_unchecked() as u64 } else { rv.bits() };
                            (la, ra, dst_val, 0u64, cmp_bool)
                        }
                    }
                    // LoadConst: dst = const (semantic value for integers)
                    0 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let dv = unsafe { *regs.add(dst_r) };
                        let da = if dv.is_i32() { unsafe { dv.as_i32_unchecked() as u64 } } else { dv.bits() };
                        (0, 0, da, da, 0)
                    }
                    // LoadTrue/False/Null/Undef: dst = literal (use NaN-boxed since constraints match)
                    1..=4 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let dv = unsafe { *regs.add(dst_r) };
                        let da = if dv.is_i32() { unsafe { dv.as_i32_unchecked() as u64 } } else { dv.bits() };
                        (0, 0, da, da, 0)
                    }
                    // Move: dst = src (semantic)
                    5 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let src_r = self.read_u16(trace_ip + 3) as usize;
                        let dv = unsafe { *regs.add(dst_r) };
                        let sv = unsafe { *regs.add(src_r) };
                        let da = if dv.is_i32() { unsafe { dv.as_i32_unchecked() as u64 } } else { dv.bits() };
                        let sa = if sv.is_i32() { unsafe { sv.as_i32_unchecked() as u64 } } else { sv.bits() };
                        (sa, 0, da, 0, 0)
                    }
                    // Jump
                    35 => {
                        (0, 0, 0, 0, ip as u64) // aux = actual jump target (new ip)
                    }
                    // JumpIfNot, JumpIfTruthy
                    36 | 37 => {
                        let cond_r = self.read_u16(trace_ip + 1) as usize;
                        let cv = unsafe { (*regs.add(cond_r)).bits() };
                        (cv, 0, 0, 0, ip as u64)
                    }
                    // Unary ops: NEG (30), NOT (31)
                    30 | 31 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let src_r = self.read_u16(trace_ip + 3) as usize;
                        let dv = unsafe { *regs.add(dst_r) };
                        let sv = unsafe { *regs.add(src_r) };
                        let da = if dv.is_i32() { unsafe { dv.as_i32_unchecked() as u64 } } else { dv.bits() };
                        let sa = if sv.is_i32() { unsafe { sv.as_i32_unchecked() as u64 } } else { sv.bits() };
                        // For NOT: dst is boolean (0 or 1)
                        let da = if trace_op == 31 {
                            if dv == Value::TRUE { 1u64 } else { 0u64 }
                        } else { da };
                        (sa, 0, da, 0, 0)
                    }
                    // GetGlobal (6): dst = loaded value, const = global index
                    6 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let idx = self.read_u16(trace_ip + 3) as u64;
                        let dv = unsafe { *regs.add(dst_r) };
                        let da = if dv.is_i32() { unsafe { dv.as_i32_unchecked() as u64 } } else { dv.bits() };
                        (da, 0, da, idx, 0) // val_a = val_dst = loaded value, const = index
                    }
                    // SetGlobal (7): val_a = stored value, const = global index
                    7 => {
                        let idx = self.read_u16(trace_ip + 1) as u64;
                        let src_r = self.read_u16(trace_ip + 3) as usize;
                        let sv = unsafe { *regs.add(src_r) };
                        let sa = if sv.is_i32() { unsafe { sv.as_i32_unchecked() as u64 } } else { sv.bits() };
                        (sa, 0, 0, idx, 0)
                    }
                    // MOD (12): a % b = dst, quotient in AUX
                    12 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let left_r = self.read_u16(trace_ip + 3) as usize;
                        let right_r = self.read_u16(trace_ip + 5) as usize;
                        unsafe {
                            let lv = *regs.add(left_r);
                            let rv = *regs.add(right_r);
                            let dv = *regs.add(dst_r);
                            let la = if lv.is_i32() { lv.as_i32_unchecked() as u64 } else { lv.bits() };
                            let ra = if rv.is_i32() { rv.as_i32_unchecked() as u64 } else { rv.bits() };
                            let da = if dv.is_i32() { dv.as_i32_unchecked() as u64 } else { dv.bits() };
                            // Compute quotient for the constraint: a = b * quotient + dst
                            let quotient = if ra != 0 { la / ra } else { 0 };
                            (la, ra, da, 0, quotient)
                        }
                    }
                    // Call (38), CallGlobal (62): capture func ref + nargs
                    38 | 62 => {
                        let nargs = self.read_u8_at(trace_ip + 5) as u64;
                        (0, nargs, 0, 0, 0)
                    }
                    // Return (41): capture return value
                    41 => {
                        let src_r = self.read_u16(trace_ip + 1) as usize;
                        let rv = unsafe { *regs.add(src_r) };
                        let ra = if rv.is_i32() { (unsafe { rv.as_i32_unchecked() }) as u64 } else { rv.bits() };
                        (0, 0, ra, 0, 0)
                    }
                    // ReturnUndef (42)
                    42 => (0, 0, 0, 0, 0),
                    // POW (13): binary op like MUL
                    13 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let left_r = self.read_u16(trace_ip + 3) as usize;
                        let right_r = self.read_u16(trace_ip + 5) as usize;
                        unsafe {
                            let lv = *regs.add(left_r);
                            let rv = *regs.add(right_r);
                            let dv = *regs.add(dst_r);
                            let la = if lv.is_i32() { lv.as_i32_unchecked() as u64 } else { lv.bits() };
                            let ra = if rv.is_i32() { rv.as_i32_unchecked() as u64 } else { rv.bits() };
                            let da = if dv.is_i32() { dv.as_i32_unchecked() as u64 } else { dv.bits() };
                            (la, ra, da, 0, 0)
                        }
                    }
                    // Bitwise ops (24-29): capture operands + result
                    24..=29 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let left_r = self.read_u16(trace_ip + 3) as usize;
                        let right_r = self.read_u16(trace_ip + 5) as usize;
                        unsafe {
                            let lv = *regs.add(left_r);
                            let rv = *regs.add(right_r);
                            let dv = *regs.add(dst_r);
                            let la = if lv.is_i32() { lv.as_i32_unchecked() as u64 } else { lv.bits() };
                            let ra = if rv.is_i32() { rv.as_i32_unchecked() as u64 } else { rv.bits() };
                            let da = if dv.is_i32() { dv.as_i32_unchecked() as u64 } else { dv.bits() };
                            (la, ra, da, 0, 0)
                        }
                    }
                    // Typeof (33): result in dst
                    33 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let src_r = self.read_u16(trace_ip + 3) as usize;
                        unsafe {
                            ((*regs.add(src_r)).bits(), 0, (*regs.add(dst_r)).bits(), 0, 0)
                        }
                    }
                    // GetProp (50): capture object, result
                    50 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let obj_r = self.read_u16(trace_ip + 3) as usize;
                        unsafe {
                            ((*regs.add(obj_r)).bits(), 0, (*regs.add(dst_r)).bits(), 0, 0)
                        }
                    }
                    // SetProp (51): capture object, value
                    51 => {
                        let obj_r = self.read_u16(trace_ip + 1) as usize;
                        let src_r = self.read_u16(trace_ip + 5) as usize;
                        unsafe {
                            ((*regs.add(obj_r)).bits(), (*regs.add(src_r)).bits(), 0, 0, 0)
                        }
                    }
                    // Index read (54): obj[key] -> dst
                    54 => {
                        let dst_r = self.read_u16(trace_ip + 1) as usize;
                        let obj_r = self.read_u16(trace_ip + 3) as usize;
                        let key_r = self.read_u16(trace_ip + 5) as usize;
                        unsafe {
                            ((*regs.add(obj_r)).bits(), (*regs.add(key_r)).bits(),
                             (*regs.add(dst_r)).bits(), 0, 0)
                        }
                    }
                    // Array (46): capture count
                    46 => {
                        let count = self.read_u16(trace_ip + 5) as u64;
                        (0, count, 0, 0, 0)
                    }
                    // Hash (47): capture count
                    47 => {
                        let count = self.read_u16(trace_ip + 5) as u64;
                        (0, count, 0, 0, 0)
                    }
                    // Fused: TestLtConstJump (64), TestLeConstJump (65)
                    64 | 65 => {
                        let r = self.read_u16(trace_ip + 1) as usize;
                        let rv = unsafe { *regs.add(r) };
                        let ra = if rv.is_i32() { (unsafe { rv.as_i32_unchecked() }) as u64 } else { rv.bits() };
                        (ra, 0, 0, 0, ip as u64)
                    }
                    // All other opcodes
                    _ => (0, 0, 0, 0, 0),
                };
                self.trace_steps.push((self.trace_clk, trace_ip as u64, trace_op, va, vb, vd, cv, ax));
                self.trace_clk += 1;
            }
        }
    }

    // ── Inline helpers ────────────────────────────────────────────────

    #[inline(always)]
    fn get_global_as_value(&self, idx: usize) -> Value {
        unsafe { self.globals.get_unchecked(idx) }
    }

}
