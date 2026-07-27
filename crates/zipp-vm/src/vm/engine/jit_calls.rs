// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

impl<'p> Vm<'p> {
    /// The native self-recursive-call implementation behind the `jit_self_call`
    /// FFI trampoline. Runs `self`-recursion natively on a fresh register window
    /// appended to `self.regs`. Returns result Value bits, or
    /// `codegen::SELF_CALL_DEOPT` to make the native caller bail to the interp.
    ///
    /// Register-stability invariant: `self.regs` has reserved capacity for the
    /// whole recursion (`reserve_jit_regs`), so appending the callee window
    /// NEVER reallocates — the native CALLER's window pointer (`rbx`) therefore
    /// stays valid across this call. We `truncate` back to the caller's length
    /// before returning so the register file is exactly as the caller left it.
    ///
    /// NOTE: superseded by the inline native→native fast path + `jit_self_call_at`
    /// (the codegen now calls its own entry directly, no per-call Rust). Retained
    /// for reference / potential reuse; not on any hot path.
    #[allow(dead_code)]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_self_call_impl(&mut self, func_id: u32, args: *const u64, argc: usize) -> u64 {
        // An arrow needs its lexically-captured `this` bound at reg 0; this fast
        // path sets reg 0 = UNDEFINED, so deopt arrows to the interpreter (which
        // rebinds correctly). Recursive arrows that read `this` are rare.
        if self.func(func_id as usize).lexical_this {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        // Depth guard: deopt (not crash) past the native recursion budget; the
        // interpreter path then enforces MAX_FRAMES / throws RangeError.
        if self.jit_recurse_depth >= JIT_SELF_RECURSE_MAX {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        // Native entry via the one-entry self-call cache (skips the HashMap
        // lookup on the hot recursive path — it always targets the same func_id).
        let entry = match self.jit.self_call_entry(func_id) {
            Some(e) => e,
            None => return crate::codegen::SELF_CALL_DEOPT,
        };
        let proto = self.func(func_id as usize);
        let reg_count = (proto.reg_count as usize).max(1);
        let params = proto.param_count as usize;

        // Fresh window appended to regs. Reserved capacity guarantees no realloc.
        let new_base = self.regs.len();
        let needed = new_base + reg_count;
        if needed > self.regs.capacity() {
            // Out of reserved headroom — deopt rather than risk a realloc that
            // would invalidate the caller's live `rbx`.
            return crate::codegen::SELF_CALL_DEOPT;
        }
        if needed > self.regs_hw {
            // New ground: zero-fill the freshly exposed slots and advance the mark.
            self.regs.resize(needed, Value::UNDEFINED);
            self.regs_hw = needed;
        } else {
            // Window lies within already-initialized memory (a previous recursion
            // reached at least this deep). Slots hold valid Value bits (stale, but
            // the compiled code writes before it reads), so skip the zero-fill —
            // this is the hot path for all but the deepest recursive call.
            // SAFETY: needed ≤ regs_hw ≤ a prior len ≤ capacity; [0..regs_hw] was
            // initialized by an earlier resize and the buffer is pinned, so these
            // slots are live, valid `Value`s.
            unsafe {
                self.regs.set_len(needed);
            }
        }
        // reg 0 = `this` (undefined for a plain self-call); params at 1..
        self.regs[new_base] = Value::UNDEFINED;
        let n = argc.min(params);
        for i in 0..n {
            // SAFETY: args points to `argc` valid Value bits (the caller's reg
            // window); n ≤ argc.
            self.regs[new_base + 1 + i] = Value::from_bits(unsafe { *args.add(i) });
        }

        self.jit_recurse_depth += 1;
        let regs_ptr = unsafe { self.regs.as_mut_ptr().add(new_base) } as *mut u64;
        let vm_ptr = self as *mut Vm as *mut core::ffi::c_void;
        // Call the cached native entry directly (same win64 ABI as JitFn::run).
        // SAFETY: `entry` is this function's compiled win64 code (stable across
        // HashMap rehashes); the window has `reg_count` valid slots; vm is valid.
        let (bits, bail) = unsafe {
            let f: extern "win64" fn(*mut u64, *mut u32, *mut core::ffi::c_void) -> u64 =
                core::mem::transmute(entry);
            let mut bail: u32 = crate::codegen::NO_BAIL;
            let r = f(regs_ptr, &mut bail as *mut u32, vm_ptr);
            (r, bail)
        };

        let result_bits = if bail == crate::codegen::NO_BAIL {
            bits
        } else {
            // The native callee bailed mid-body: finish this activation on the
            // interpreter over the SAME window via a transient frame. The frame
            // base is `new_base` into self.regs (stable — reserved capacity).
            self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX, arg_win: u32::MAX, argc: 0, is_eval: false,
                func: func_id,
                base: new_base,
                ip: bail as usize,
                ret_dst: 0,
                closure: NO_CLOSURE,
                handlers: Vec::new(),
                new_target: Value::UNDEFINED,
                callee: Value::UNDEFINED,
            });
            let stop = self.frames.len() - 1;
            match self.run_loop(stop) {
                Ok(v) => v.bits(),
                // A throw inside the recursion: there is no JS-level way to
                // surface it through the native ABI here, so deopt the whole
                // self-call. pending_throw stays set; the interpreter caller
                // (the original top-level run_loop) re-raises it. We restore
                // regs and return the sentinel.
                Err(_) => {
                    self.jit_recurse_depth -= 1;
                    self.regs.truncate(new_base);
                    return crate::codegen::SELF_CALL_DEOPT;
                }
            }
        };

        self.jit_recurse_depth -= 1;
        self.regs.truncate(new_base);
        result_bits
    }

    /// Slow/finish path for the JIT's inline native→native self-call. Called
    /// when the inline fast path can't complete a recursive call purely natively:
    /// either the native depth limit was hit, or the callee bailed mid-body. The
    /// caller passes its window base EXPLICITLY (`caller_base_ptr`, the native
    /// `rbx`) because the fast path tracks windows by raw pointer, not
    /// `self.regs.len()`. Runs the activation on the interpreter over a transient
    /// frame at the callee window, holding `jit_recurse_depth` ELEVATED for the
    /// duration so the dispatch JIT-entry gate (`== 0`) stays closed and the
    /// recursion can't re-enter native and livelock — frames then accumulate
    /// monotonically to `MAX_FRAMES` → catchable RangeError. Returns the result
    /// bits, or `SELF_CALL_DEOPT` if the activation threw (the throw is left in
    /// `pending_throw`; the native chain unwinds and the top-level interpreter
    /// re-raises it).
    ///
    /// # Safety
    /// `caller_base_ptr` is the caller's window base within `self.regs`; `args`
    /// points to `argc` valid `Value` bits.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_self_call_at_impl(
        &mut self,
        func_id: u32,
        caller_base_ptr: *const u64,
        args: *const u64,
        argc: usize,
    ) -> u64 {
        // Arrows need their lexical `this` at reg 0 (this path sets UNDEFINED) —
        // deopt them to the interpreter, which rebinds correctly.
        if self.func(func_id as usize).lexical_this {
            return crate::codegen::SELF_CALL_DEOPT;
        }
        let proto = self.func(func_id as usize);
        let reg_count = (proto.reg_count as usize).max(1);
        let params = proto.param_count as usize;

        // Caller window base as a slot index (the fast path placed it by raw
        // pointer); the callee window sits contiguously above it.
        let regs_base = self.regs.as_ptr() as *const u64;
        // SAFETY: caller_base_ptr lies within self.regs' (non-reallocating) buffer.
        let caller_base =
            unsafe { (caller_base_ptr).offset_from(regs_base) } as usize;
        let new_base = caller_base + reg_count;
        let needed = new_base + reg_count;
        if self.regs_would_overflow(needed) {
            // Out of reserved register headroom (very deep): treat as stack
            // overflow — throw so the interpreter surfaces a catchable RangeError.
            let e =
                self.alloc_error_from_message("RangeError: Maximum call stack size exceeded");
            self.pending_throw = Some(e);
            return crate::codegen::SELF_CALL_DEOPT;
        }

        // RESYNC self.regs.len() to span the callee window so the transient
        // interpreter frame + MAX_FRAMES accounting are consistent. Save the
        // entry length and restore it on the way out (the native caller doesn't
        // use `len`, but the eventual return to the dispatch loop expects it
        // unchanged).
        let saved_len = self.regs.len();
        // CRITICAL: grow `len` with `set_len`, NOT `resize`. The native fast path
        // advanced the register windows by raw pointer WITHOUT touching
        // `self.regs.len()`, so on entry here `len` (≈ the warmup top) is far below
        // the live native windows, which occupy slots up to `new_base`. A
        // `resize(needed, UNDEFINED)` would ZERO-FILL `[len, needed)` — overwriting
        // every parked native frame's registers with `undefined` and corrupting the
        // recursion (this was the bug that capped JIT recursion below the
        // interpreter). The native windows hold valid `Value`s already (each native
        // frame defs its registers before reading — the same def-before-use
        // invariant the leaf JIT relies on), and the buffer is pinned to capacity
        // by `reserve_jit_regs`, so simply exposing them via `set_len` is correct.
        // Bounds: `needed ≤ capacity` (guarded above by `regs_would_overflow`).
        // SAFETY: `needed ≤ capacity`; slots `[0, needed)` are live `Value`s —
        // `[0, len)` from the interpreter, `[len, new_base+reg_count)` written by
        // the native frames whose windows we're spanning.
        unsafe { self.regs.set_len(needed); }
        if needed > self.regs_hw {
            self.regs_hw = needed;
        }
        self.regs[new_base] = Value::UNDEFINED;
        let n = argc.min(params);
        for i in 0..n {
            self.regs[new_base + 1 + i] = Value::from_bits(unsafe { *args.add(i) });
        }

        // Run this activation on the interpreter via a transient frame. Depth is
        // held ELEVATED across the whole run (we only restore it after), so any
        // self-call inside stays interpreted (the dispatch gate sees depth != 0)
        // and the recursion can't re-enter native → no livelock; frames grow to
        // MAX_FRAMES → RangeError on runaway.
        self.jit_recurse_depth += 1;
        self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX, arg_win: u32::MAX, argc: 0, is_eval: false,
            func: func_id,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure: NO_CLOSURE,
            handlers: Vec::new(),
            new_target: Value::UNDEFINED,
            callee: Value::UNDEFINED,
        });
        let stop = self.frames.len() - 1;
        let r = self.run_loop(stop);
        self.jit_recurse_depth -= 1;
        // SAFETY: restore the entry length (allocation unchanged, slots valid).
        unsafe { self.regs.set_len(saved_len); }
        match r {
            Ok(v) => v.bits(),
            // Threw (e.g. RangeError): leave it in pending_throw and signal the
            // native caller to unwind; the top-level interpreter re-raises it.
            Err(_) => crate::codegen::SELF_CALL_DEOPT,
        }
    }

    /// The implementation behind the region call helpers `jit_call_method_ic` /
    /// `jit_call_ic` (`is_method` selects which). A compiled OSR region reached
    /// a `CallMethod`/`Call` op: consult the SAME per-site inline cache the
    /// interpreter uses, push the resolved plain user function with FULL
    /// `setup_call` semantics (this-binding, rest, arguments object,
    /// MAX_FRAMES), and run it to completion via `run_loop`. Three-state
    /// result (see the helper docs): result bits / `SELF_CALL_DEOPT` /
    /// `CALL_THREW`.
    ///
    /// Reentrancy contract (what the calling region may rely on):
    /// * `self.regs` never reallocates (pinned by `reserve_jit_regs`; every
    ///   growth site checks `regs_would_overflow`) — the region's window base
    ///   (rbx) stays valid. The callee windows are appended above the region's
    ///   frame and truncated back before this returns.
    /// * `self.globals` never reallocates — r12 stays valid.
    /// * The heap's versions array and the JIT IC table MAY move (the callee
    ///   can allocate, and can trigger a nested region compile — nested
    ///   execution runs at `jit_recurse_depth == 0`, so the JIT gates are
    ///   open) — the region RE-FETCHES r13/r14 after this helper returns.
    /// * A nested deopt can EVICT the (still-running) calling region; evicted
    ///   regions are parked in `Jit::retired`, never dropped mid-run.
    /// * No `Value` is held in native registers across this call (the memory
    ///   path stores every result to the reg file before the next op), and
    ///   this function holds no `Vec<Value>` across the callee run — all live
    ///   values sit in regs/frames/heap, the GC root set.
    ///
    /// Depth: each level nests `run_loop` on the Rust stack, so recursion
    /// through region call sites is capped at `JIT_REGION_CALL_MAX`; past it
    /// the call deopts to the interpreter's flat frames (and sets
    /// `osr_deopt_exempt` so the region isn't punished for legal recursion).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_region_call_impl(
        &mut self,
        caller_base_ptr: *const u64,
        packed_fip: u64,
        packed_args: u64,
        argc: u16,
        is_method: bool,
    ) -> u64 {
        use crate::codegen::{CALL_THREW, SELF_CALL_DEOPT};
        if self.jit_call_depth >= JIT_REGION_CALL_MAX {
            // Legal-but-deep recursion: not a region-quality signal — don't
            // count it toward eviction.
            self.osr_deopt_exempt = true;
            return SELF_CALL_DEOPT;
        }
        let func_id = (packed_fip >> 32) as u32;
        let ip = (packed_fip as u32) as usize;
        let arg_base = (packed_args & 0xFFFF) as u16;
        // Caller window base as a slot index (the region tracks it by raw
        // pointer; the buffer is pinned, so the offset is stable).
        let regs_base = self.regs.as_ptr() as *const u64;
        // SAFETY: caller_base_ptr lies within self.regs' pinned buffer.
        let base = unsafe { caller_base_ptr.offset_from(regs_base) } as usize;

        // Resolve through the interpreter's per-site IC — IDENTICAL resolution
        // order to the interpreter (which consults the IC first; everything the
        // IC won't claim, incl. '#private' names, builtins, natives, accessors,
        // generators/async, exotic receivers, deopts to the interpreter).
        let (fid, closure, this_v, callee_v) = if is_method {
            let obj = ((packed_args >> 16) & 0xFFFF) as u16;
            let name = (packed_args >> 32) as u32;
            let recv = self.get(base, obj);
            // `func()` borrows &'p (program lifetime) — outlives `&mut self`.
            let key: &str = &self.func(func_id as usize).string_constants[name as usize];
            match self.ic_call_method(func_id, ip, recv, key) {
                Some((fid, closure, callee)) => (fid, closure, recv, callee),
                None => {
                    if jit_call_log() {
                        eprintln!("[call] METHOD MISS fn{func_id}@{ip} key={key}");
                    }
                    // Builtin fallback: the EXACT paths the interpreter runs
                    // next for this op (`try_builtin_method`, then a
                    // ctor-object native like `Math.floor`) — run to
                    // completion, never deopting after a side effect.
                    return self.jit_method_builtin_fallback(recv, key, base, arg_base, argc);
                }
            }
        } else {
            let callee_reg = ((packed_args >> 16) & 0xFFFF) as u16;
            let cv = self.get(base, callee_reg);
            match self.ic_call(func_id, ip, cv) {
                Some((fid, closure)) => (fid, closure, Value::UNDEFINED, cv),
                None => {
                    if jit_call_log() {
                        eprintln!("[call] CALL MISS fn{func_id}@{ip}");
                    }
                    // A plain NATIVE callee (parseInt, …): invoke via
                    // call_value with this=undefined, exactly like the
                    // interpreter's Call op. Everything else deopts.
                    if cv.is_heap()
                        && matches!(self.heap.get(cv.heap_index()), HeapObj::Native(_))
                    {
                        let argv: Vec<Value> =
                            (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                        return match self.call_value(cv, Value::UNDEFINED, &argv) {
                            Ok(v) => v.bits(),
                            Err(t) => self.jit_thrown_to_sentinel(t),
                        };
                    }
                    return SELF_CALL_DEOPT;
                }
            }
        };

        // MI (method inlining): for a `CallMethod` whose resolved target is a
        // trivial straight-line body over `this` + params (incl. nested
        // `super.m()`), evaluate it OFF-FRAME — no `setup_call`, no `run_loop`,
        // no per-call args Vec. This collapses the dominant class-method call
        // floor (`objs[i&3].area()` over `super.area()*k`). `None` falls through
        // to the full frame call (any non-trivial body / non-numeric operand /
        // non-instance receiver). Only for method calls — `this = recv` is
        // load-bearing; a plain `Call` binds `this = undefined`.
        if is_method {
            if let Some(bits) = self.try_method_inline(fid, this_v, base, arg_base, argc) {
                return bits;
            }
        }
        // Push the callee frame exactly like the interpreter's Call/CallMethod
        // IC-hit path, and run it to completion.
        self.jit_frame_call(fid, closure, this_v, base, arg_base, argc, ip, callee_v)
    }

    /// The interpreter's post-IC `CallMethod` fallbacks, run from a region call
    /// helper TO COMPLETION (a path that has side effects must never deopt
    /// afterwards): `try_builtin_method` (array/string/… builtins, exactly the
    /// interpreter's next step), then a ctor-object data prop holding a NATIVE
    /// (`Math.floor`, `JSON.parse`, … — a side-effect-free resolution) invoked
    /// via `call_value`. Anything else deopts BEFORE doing anything.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_method_builtin_fallback(
        &mut self,
        recv: Value,
        key: &str,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> u64 {
        match self.try_builtin_method(recv, key, base, arg_base, argc) {
            Ok(Some(v)) => return v.bits(),
            Ok(None) => {}
            Err(t) => return self.jit_thrown_to_sentinel(t),
        }
        if recv.is_heap() {
            if let HeapObj::Object(m) = self.heap.get(recv.heap_index()) {
                if m.is_ctor {
                    if let Some(i) = m.pos(key) {
                        if !m.attrs[i].accessor {
                            let f = m.vals[i];
                            if f.is_heap()
                                && matches!(self.heap.get(f.heap_index()), HeapObj::Native(_))
                            {
                                let argv: Vec<Value> =
                                    (0..argc).map(|i| self.get(base, arg_base + i)).collect();
                                return match self.call_value(f, recv, &argv) {
                                    Ok(v) => v.bits(),
                                    Err(t) => self.jit_thrown_to_sentinel(t),
                                };
                            }
                        }
                    }
                }
            }
        }
        crate::codegen::SELF_CALL_DEOPT
    }

    /// Materialize a `Thrown` from a region-helper-run operation exactly like
    /// `run_loop`'s Err path does (synthesize the Error object if no JS value
    /// is pending), then signal `CALL_THREW` so the region exits and the
    /// interpreter unwinds. Exempt from deopt accounting (a throw is not a
    /// region-quality signal).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_thrown_to_sentinel(&mut self, t: Thrown) -> u64 {
        if self.pending_throw.is_none() {
            let v = self.alloc_error_from_message(&t.0);
            self.realm_adopt_error(v);
            self.pending_throw = Some(v);
        }
        self.osr_deopt_exempt = true;
        crate::codegen::CALL_THREW
    }

    /// Q7 S-ACC: if the getter `fid` is the trivial shape `return this.<field>`
    /// AND `<field>` is a plain own writable/non-accessor DATA slot of `recv`,
    /// serve the read DIRECTLY (no frame push, no `setup_call`, no `run_loop`) —
    /// returning the slot's Value bits. Returns `None` to fall back to the full
    /// frame call (any other body shape, a non-instance receiver, a missing /
    /// accessor / inherited field).
    ///
    /// SOUNDNESS: the trivial body `[GetProp{obj:0, name:N}, Return{src}]` reads
    /// exactly `this.<field>` with `this == recv`. We only take the fast path
    /// when `recv.<field>` is an OWN DATA slot (so the read is a pure slot load
    /// with no further accessor / proto-chain semantics — byte-identical to what
    /// the frame-called getter would compute). Reg numbers are irrelevant: the
    /// body's single observable effect is the field load it returns. No side
    /// effect, so no deopt/re-execution hazard. The outer own-shadow guard (G3b)
    /// is ALREADY enforced by `ic_get_prop` (the `ClassGetter` validate arm
    /// requires `own.is_none()`), which ran before this — so a same-name instance
    /// own-write correctly never reaches here.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn accessor_fast_get(&self, fid: u32, recv: Value) -> Option<u64> {
        let field = self.simple_getter_field(fid)?;
        if !recv.is_heap() {
            return None;
        }
        let idx = recv.heap_index();
        if !self.ic_obj_ok(idx) {
            return None;
        }
        let m = match self.heap.get(idx) {
            HeapObj::Object(m) if !m.is_ctor => m,
            _ => return None,
        };
        let s = m.pos(field)?;
        // OWN, DATA (non-accessor) slot only — a nested accessor / inherited
        // field would need real semantics; defer those to the frame call.
        if m.attrs[s].accessor {
            return None;
        }
        Some(m.vals[s].bits())
    }

    /// Q7 S-ACC: if the setter `fid` is the trivial shape `this.<field> = arg`
    /// or `this.<field> = (arg | 0)` AND `<field>` is a plain own writable
    /// non-accessor DATA slot of `recv`, perform the write DIRECTLY (no frame).
    /// Returns `Some(0)` on a served write, or `None` to fall back to the frame
    /// call (any other shape, or a field that is missing / an accessor / non-
    /// writable / inherited — those need full `set_prop` semantics).
    ///
    /// SOUNDNESS: the body writes `this.<field>` with `this == recv`, optionally
    /// applying `ToInt32` (`x | 0`) to the value first. We serve it ONLY when the
    /// field is already an OWN WRITABLE DATA slot of `recv`, so the write is a
    /// pure in-place slot store — byte-identical to the frame-called setter. The
    /// slot value changes but the OBJECT SHAPE does not (no add/delete/redefine),
    /// so NO version bump is needed (mirrors the JIT SetProp IC fast write, which
    /// also stores in place without a bump). The outer own-shadow guard (G3b) is
    /// already enforced by `ic_set_prop`'s `ClassSetter` validate arm
    /// (`own.is_none()`), which ran before this.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn accessor_fast_set(&mut self, fid: u32, recv: Value, val: Value) -> Option<u64> {
        let (field, to_int32) = self.simple_setter_field(fid)?;
        if !recv.is_heap() {
            return None;
        }
        let idx = recv.heap_index();
        if !self.ic_obj_ok(idx) {
            return None;
        }
        // The stored value: optionally ToInt32 (`x | 0`). ToInt32 of a plain
        // number is unobservable (no user code), so it is safe off-frame; a
        // non-number value would need ToNumber (potentially observable
        // valueOf) — defer that to the frame call.
        let stored = if to_int32 {
            if !val.is_number() {
                return None;
            }
            let f = if val.is_int() { val.as_int() as f64 } else { val.as_f64() };
            Value::int(crate::vm::helpers_num2::to_int32(f))
        } else {
            val
        };
        // Re-borrow mutably and verify the field is an own writable data slot.
        match self.heap.get_mut(idx) {
            HeapObj::Object(m) if !m.is_ctor => {
                let s = m.pos(field)?;
                if m.attrs[s].accessor || !m.attrs[s].writable {
                    return None;
                }
                m.vals[s] = stored; // in-place data store — shape unchanged
                Some(0)
            }
            _ => None,
        }
    }

    /// Maximum nested `super.m()` hops the off-frame method evaluator follows
    /// before deopting to the full frame call. Class hierarchies deeper than
    /// this are rare; the bound keeps the recursion (a plain Rust call per hop)
    /// trivially safe and bounds the validation cost.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) const METHOD_INLINE_MAX_SUPER: u32 = 6;

    /// Capacity of the off-frame method evaluator's STACK register window. A body
    /// with `reg_count` above this declines to the frame call. Trivial method
    /// bodies are tiny; the cap keeps the per-call stack array small and avoids
    /// any heap allocation on the hot path.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) const MI_MAX_REGS: usize = 24;

}
