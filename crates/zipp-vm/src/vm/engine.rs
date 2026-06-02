#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    pub fn new(program: &'p Program) -> Vm<'p> {
        let mut heap = Heap::new();
        // Pre-load string constants of every function into the heap so
        // `LoadConst` of a string resolves to a stable heap index. We rewrite
        // string-constant slots to carry their heap index as an Int payload
        // marker is avoided — instead the compiler emits heap Values directly
        // (see `intern_strings`).
        // `global_count` real slots, plus a fixed POOL of extra slots the JIT uses
        // as scratch "field globals" for object scalar-replacement (SROA): a
        // field-promoted region's GetProp/SetProp are rewritten to Load/StoreGlobal
        // on pool slots, and the interpreter syncs object.field ↔ pool slot around
        // the native run. Sized once here so the globals Vec never reallocates at
        // runtime (the JIT pins its base pointer).
        let mut globals = vec![Value::UNDEFINED; program.global_count as usize + FIELD_POOL];
        // Real global slots start as the never-declared sentinel: a LoadGlobal of
        // one throws ReferenceError unless a builtin (setup_globals), a hoisted
        // function, a top-level `var` (hoisted to undefined just below), or a
        // StoreGlobal writes it first. The JIT scratch pool (past global_count)
        // stays undefined.
        for slot in globals.iter_mut().take(program.global_count as usize) {
            *slot = Value::UNINITIALIZED;
        }
        for &slot in &program.hoisted_globals {
            if (slot as usize) < globals.len() {
                globals[slot as usize] = Value::UNDEFINED;
            }
        }
        let _ = &mut heap;
        Vm {
            program,
            class_values: vec![None; program.classes.len()],
            heap,
            globals,
            regs: Vec::new(),
            frames: Vec::new(),
            output: Vec::new(),
            errput: Vec::new(),
            start: std::time::Instant::now(),
            pending_throw: None,
            pending_yield: None,
            pending_await: None,
            microtasks: std::collections::VecDeque::new(),
            template_raws: std::collections::HashMap::new(),
            prototypes: std::collections::HashMap::new(),
            proto_of: std::collections::HashMap::new(),
            fn_props: std::collections::HashMap::new(),
            deleted_callable_intrinsics: std::collections::HashSet::new(),
            obj_proto: 0,
            fn_proto: 0,
            arr_proto: 0,
            str_proto: 0,
            map_proto: 0,
            set_proto: 0,
            date_proto: 0,
            promise_proto: 0,
            num_proto: 0,
            bool_proto: 0,
            weakmap_proto: 0,
            weakset_proto: 0,
            weakref_proto: 0,
            finreg_proto: 0,
            error_protos: [0; 8],
            error_ctors: [0; 8],
            symbol_proto: 0,
            symbol_ctor: 0,
            bigint_proto: 0,
            bigint_ctor: 0,
            regexp_proto: 0,
            regexp_ctor: 0,
            regexp_match_extras: std::collections::HashMap::new(),
            ta_base_ctor: 0,
            ta_base_proto: 0,
            ta_ctors: [0; 11],
            ta_protos: [0; 11],
            arraybuffer_ctor: 0,
            arraybuffer_proto: 0,
            dataview_ctor: 0,
            dataview_proto: 0,
            proxy_ctor: 0,
            temporal_ns: 0,
            duration_ctor: 0,
            duration_proto: 0,
            plaindate_ctor: 0,
            plaindate_proto: 0,
            plaintime_ctor: 0,
            plaintime_proto: 0,
            plaindatetime_ctor: 0,
            plaindatetime_proto: 0,
            instant_ctor: 0,
            instant_proto: 0,
            plainyearmonth_ctor: 0,
            plainyearmonth_proto: 0,
            plainmonthday_ctor: 0,
            plainmonthday_proto: 0,
            intl_ns: 0,
            intl_ctors: [0; 10],
            intl_protos: [0; 10],
            symbol_counter: 0,
            symbol_registry: std::collections::HashMap::new(),
            symbol_keys: std::collections::HashMap::new(),
            iterator_proto_root: 0,
            iterator_helper_proto: 0,
            iterator_ctor: 0,
            dollar262: 0,
            array_iter_proto: 0,
            map_iter_proto: 0,
            set_iter_proto: 0,
            global_this: 0,
            rng_state: 0x9E37_79B9_7F4A_7C15, // fixed seed (golden-ratio constant)
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit: crate::codegen::Jit::new(),
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_enabled: std::env::var_os("ZIPP_NOJIT").is_none(),
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_recurse_depth: 0,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            reg_capacity: 0,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            regs_hw: 0,
        }
    }

    /// Force the JIT on/off (overrides the `ZIPP_NOJIT` default). Used by the
    /// test suite to run a program both ways and assert the outputs match.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[allow(dead_code)] // used by the differential test harness (run_nojit)
    pub(crate) fn set_jit_enabled(&mut self, on: bool) {
        self.jit_enabled = on;
    }

    /// Would growing `self.regs` to `needed` slots exceed the pinned capacity?
    /// (Interpreter-only builds: never — there is no pinned native pointer to
    /// protect, so the Vec may grow/reallocate freely.)
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn regs_would_overflow(&self, needed: usize) -> bool {
        self.reg_capacity != 0 && needed > self.reg_capacity
    }
    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    #[inline]
    pub(crate) fn regs_would_overflow(&self, _needed: usize) -> bool {
        false
    }

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
        let proto = &self.program.functions[func_id as usize];
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
            self.frames.push(Frame {
                func: func_id,
                base: new_base,
                ip: bail as usize,
                ret_dst: 0,
                closure: NO_CLOSURE,
                handlers: Vec::new(),
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
        let proto = &self.program.functions[func_id as usize];
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
        self.frames.push(Frame {
            func: func_id,
            base: new_base,
            ip: 0,
            ret_dst: 0,
            closure: NO_CLOSURE,
            handlers: Vec::new(),
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

    /// Reserve enough register-file capacity that a full JIT self-recursion
    /// never reallocates `self.regs` (which would dangle native window
    /// pointers). Called before entering the top-level run.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn reserve_jit_regs(&mut self) {
        // The register file must NEVER reallocate while a native JIT frame holds
        // a raw pointer into it (the caller's window pointer lives in a callee-
        // saved register across the self-call helper). A realloc there dangles
        // it → memory corruption. So reserve the absolute worst case up front:
        // every possible frame (`MAX_FRAMES`) holding the largest window. Then
        // no growth site can ever exceed capacity (each is also guarded), so the
        // Vec is pinned for the VM's lifetime.
        //
        // Cost is bounded: capacity is clamped so the reserve can't exceed
        // ~256 MiB even for a pathological max_window; if the cap is hit, deep
        // recursion simply throws RangeError sooner (a `reg_capacity` field
        // records the real limit so the growth guards agree).
        let max_window = self
            .program
            .functions
            .iter()
            .map(|f| (f.reg_count as usize).max(1))
            .max()
            .unwrap_or(1);
        const MAX_REGS_BYTES: usize = 256 * 1024 * 1024; // 256 MiB ceiling
        let worst_case = max_window.saturating_mul(MAX_FRAMES);
        let capped = worst_case.min(MAX_REGS_BYTES / std::mem::size_of::<Value>());
        let target = self.regs.len() + capped;
        self.regs.reserve(target - self.regs.len());
        // Record the pinned capacity: growth sites must not exceed it (else the
        // Vec would realloc). Use the ACTUAL capacity Rust gave us (≥ requested).
        self.reg_capacity = self.regs.capacity();
    }

    /// Allocate a string on the heap and return its boxed Value.
    pub fn alloc_str(&mut self, s: String) -> Value {
        Value::heap(self.heap.alloc_str(s))
    }

    /// Run the top-level function (id 0) to completion.
    pub fn run(&mut self) -> Result<Value, Thrown> {
        // Inject the built-in global objects (Object/Array/Function + their
        // prototypes) into their reserved slots BEFORE hoisting, so a user
        // declaration of the same name shadows the builtin.
        self.setup_globals();
        // Materialise function objects for every top-level function into the
        // globals that the compiler reserved for them. The compiler records,
        // per function, the global slot its name binds to (or u32::MAX if it is
        // an anonymous/nested function not hoisted to a global).
        self.hoist_functions();

        let top = &self.program.functions[0];
        let base = 0usize;
        let top_regs = top.reg_count as usize;
        self.regs.resize(top_regs, Value::UNDEFINED);
        // Reserve register-file capacity up front so JIT self-recursion can
        // append callee windows without reallocating `self.regs` (which would
        // dangle the native code's window pointer). Must happen while regs holds
        // only the top frame so the reservation math is relative to a known base.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        self.reserve_jit_regs();
        self.frames.push(Frame { func: 0, base, ip: 0, ret_dst: 0, closure: NO_CLOSURE, handlers: Vec::new() });
        // Run until the top-level frame returns (frames drains back to 0), then
        // run the event loop: drain queued microtasks (promise reactions, async
        // resumes) to empty. Drains even on a main throw (matches node ordering),
        // then returns the original result.
        let main = self.run_loop(0);
        self.drain_microtasks();
        main
    }

    /// Invoke a callable `Value` with `this` and `args`, running it to
    /// completion, and return its result. Used by builtin methods that take
    /// callbacks (`map`/`filter`/`reduce`/`sort`). The callee executes on the
    /// explicit frame stack like any other call; we run a nested dispatch loop
    /// that returns when the callee's frame pops back to the current depth.
    ///
    /// Note: this re-enters `run_loop` on the native stack, so deeply *nested
    /// callbacks* use native recursion. Ordinary JS recursion (a function
    /// calling itself) does NOT — it stays on the frame stack. The frame cap
    /// still bounds total depth.
    pub(crate) fn call_value(&mut self, callee: Value, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        // A callable Proxy: `apply` trap (or call the target).
        if callee.is_heap() {
            if let Some((target, handler, revoked)) = self.proxy_parts(callee.heap_index()) {
                if revoked {
                    return Err(Thrown("TypeError: Cannot perform 'apply' on a revoked proxy".into()));
                }
                return match self.proxy_trap(handler, "apply")? {
                    Some(trap) => {
                        let arr = Value::heap(self.heap.alloc(HeapObj::Array(args.to_vec())));
                        self.call_value(trap, handler, &[target, this, arr])
                    }
                    None => self.call_value(target, this, args),
                };
            }
        }
        // A bound function: invoke its target with the fixed `this` and the bound
        // arguments prepended (handles bind-of-bind by recursing).
        if callee.is_heap() {
            if let HeapObj::Bound { target, this: bthis, args: bargs } = self.heap.get(callee.heap_index()) {
                let (t, th) = (*target, *bthis);
                let mut all = bargs.clone();
                all.extend_from_slice(args);
                return self.call_value(t, th, &all);
            }
            if let HeapObj::Native(id) = self.heap.get(callee.heap_index()) {
                let id = *id;
                return self.call_native(id, this, args);
            }
        }
        // A native resolve/reject function settles its bound promise.
        if callee.is_heap() {
            if let HeapObj::BoundResolver { promise, is_reject } = self.heap.get(callee.heap_index()) {
                let (p, isr) = (*promise, *is_reject);
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                if isr {
                    self.reject(p, arg);
                } else {
                    self.resolve(p, arg);
                }
                return Ok(Value::UNDEFINED);
            }
        }
        // %Function.prototype% is itself a callable that returns undefined.
        if callee.is_heap() && self.fn_proto != 0 && callee.heap_index() == self.fn_proto {
            return Ok(Value::UNDEFINED);
        }
        // An Intl constructor invoked without `new`: NumberFormat/DateTimeFormat/
        // Collator are spec'd to construct anyway; the other Intl services throw.
        if self.intl_ctors[0] != 0 && callee.is_heap() {
            let ci = callee.heap_index();
            if let Some(kind) = self.intl_ctors.iter().position(|&c| c == ci) {
                if matches!(
                    kind as u8,
                    native::INTL_NUMBERFORMAT | native::INTL_DATETIMEFORMAT | native::INTL_COLLATOR
                ) {
                    return self.construct(callee, args);
                }
                return Err(Thrown(
                    "TypeError: Constructor Intl service requires 'new'".into(),
                ));
            }
        }
        let (func_id, closure) = self.resolve_callable(callee)?;
        let (is_gen, is_async) = {
            let p = &self.program.functions[func_id as usize];
            (p.is_generator, p.is_async)
        };
        // An `async function*` builds a suspended AsyncGenerator (an async
        // iterator); it doesn't run until `.next()`.
        if is_gen && is_async {
            return Ok(self.alloc_async_generator(func_id, closure, this, args));
        }
        // Calling a generator function builds a suspended Generator, not a frame.
        if is_gen {
            return Ok(self.alloc_generator(func_id, closure, this, args));
        }
        // Calling an async function runs synchronously up to the first `await`,
        // then returns its result Promise.
        if is_async {
            return Ok(self.alloc_async(func_id, closure, this, args));
        }
        if self.frames.len() >= MAX_FRAMES {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        let proto = &self.program.functions[func_id as usize];
        let callee_regs = (proto.reg_count as usize).max(1);
        let callee_params = proto.param_count as usize;

        let new_base = self.regs.len();
        // Never grow past the pinned capacity (would realloc and dangle a live
        // native window pointer) — throw a catchable RangeError instead.
        if self.regs_would_overflow(new_base + callee_regs) {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        self.regs.resize(new_base + callee_regs, Value::UNDEFINED);
        self.regs[new_base] = this; // reg 0 = this
        let n = args.len().min(callee_params);
        for i in 0..n {
            self.regs[new_base + 1 + i] = args[i];
        }
        // Rest parameter: gather any args beyond the fixed params into an array.
        if let Some(rreg) = proto.rest_reg {
            let extra: Vec<Value> = args.get(callee_params..).unwrap_or(&[]).to_vec();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
            self.regs[new_base + rreg as usize] = arr;
        }

        let stop_depth = self.frames.len();
        self.frames.push(Frame { func: func_id, base: new_base, ip: 0, ret_dst: 0, closure, handlers: Vec::new() });
        self.run_loop(stop_depth)
    }

    /// Bind each named top-level function to its reserved global slot as a
    /// heap function object, so `Call` of a global resolves correctly. The
    /// compiler marks function-name globals; here we fill them.
    pub(crate) fn hoist_functions(&mut self) {
        for (id, f) in self.program.functions.iter().enumerate() {
            if let Some(slot) = function_global_slot(f) {
                let v = Value::heap(self.heap.alloc(HeapObj::Func(id as u32)));
                if (slot as usize) < self.globals.len() {
                    self.globals[slot as usize] = v;
                }
            }
        }
    }

}
