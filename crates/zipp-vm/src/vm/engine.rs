#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

impl<'p> Vm<'p> {
    /// Resolve a (unified) function id to its FuncProto: a compile-time program
    /// function for `id < main_func_count`, else a runtime `eval`/`new Function`
    /// function. Both sources have stable addresses (the leaked eval boxes and
    /// the borrowed program), so raw pointers taken from the result stay valid.
    /// Returns `&'p` (the program lifetime), NOT a `&self`-bound borrow: program
    /// functions live in the `&'p Program`, and eval functions are leaked
    /// (`&'static`, which coerces to `&'p`). This lets callers hold the FuncProto
    /// reference across `&mut self` operations and use it where a `'p`-lived
    /// string constant (e.g. an interned method-name key) is needed.
    #[inline]
    pub(crate) fn func(&self, id: usize) -> &'p crate::bytecode::FuncProto {
        if id < self.main_func_count {
            &self.program.functions[id]
        } else {
            self.eval_funcs[id - self.main_func_count]
        }
    }

    /// Resolve a (unified) class id to its ClassDef: a compile-time program class
    /// for `id < main_class_count`, else a runtime `eval` class. Mirrors `func`.
    #[inline]
    pub(crate) fn class_def(&self, id: usize) -> &crate::bytecode::ClassDef {
        if id < self.main_class_count {
            &self.program.classes[id]
        } else {
            self.eval_classes[id - self.main_class_count]
        }
    }

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
        let mut globals =
            vec![Value::UNDEFINED; program.global_count as usize + FIELD_POOL + EVAL_POOL];
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
            eval_funcs: Vec::new(),
            main_func_count: program.functions.len(),
            eval_classes: Vec::new(),
            main_class_count: program.classes.len(),
            eval_global_map: std::collections::HashMap::new(),
            eval_global_next: program.global_count + FIELD_POOL as u32,
            builtin_globals: std::collections::HashMap::new(),
            class_values: vec![None; program.classes.len()],
            heap,
            globals,
            regs: Vec::new(),
            frames: Vec::new(),
            output: Vec::new(),
            errput: Vec::new(),
            start: std::time::Instant::now(),
            pending_throw: None,
            pending_new_target: Value::UNDEFINED,
            pending_yield: None,
            pending_yield_handlers: Vec::new(),
            pending_yield_eval_scope: u32::MAX,
            pending_await: None,
            cap_capture: None,
            microtasks: std::collections::VecDeque::new(),
            template_raws: std::collections::HashMap::new(),
            template_cache: std::collections::HashMap::new(),
            regexp_string_iters: std::collections::HashMap::new(),
            next_private_brand: 1,
            method_brand: std::collections::HashMap::new(),
            instance_brand: std::collections::HashMap::new(),
            brand_private_names: std::collections::HashMap::new(),
            brand_owner: std::collections::HashMap::new(),
            prototypes: std::collections::HashMap::new(),
            proto_of: std::collections::HashMap::new(),
            fn_props: std::collections::HashMap::new(),
            arr_props: std::collections::HashMap::new(),
            ab_max: std::collections::HashMap::new(),
            ta_tracking: std::collections::HashSet::new(),
            dv_tracking: std::collections::HashSet::new(),
            deleted_callable_intrinsics: std::collections::HashSet::new(),
            deleted_globals: std::collections::HashSet::new(),
            array_length_nonwritable: std::collections::HashSet::new(),
            array_proto_has_index: false,
            super_called: std::collections::HashSet::new(),
            this_tdz: std::collections::HashSet::new(),
            super_this: std::collections::HashMap::new(),
            private_fields: std::collections::HashMap::new(),
            eval_fn_idx: 0,
            closure_eval_scope: std::collections::HashMap::new(),
            module_ambiguous: std::collections::HashMap::new(),
            import_meta: 0,
            can_block: std::env::var("ZIPP_CAN_BLOCK").map_or(true, |v| v != "0"),
            module_loading: std::collections::HashSet::new(),
            pending_module_body: None,
            link_pending_deps: Vec::new(),
            deferred_mods: std::collections::HashMap::new(),
            module_pending_reexports: std::collections::HashMap::new(),
            sloppy_eval_memo: Vec::new(),
            obj_proto: 0,
            fn_proto: 0,
            function_ctor: 0,
            gen_fn_ctor: 0,
            gen_fn_proto: 0,
            async_fn_ctor: 0,
            async_fn_proto: 0,
            asyncgen_fn_ctor: 0,
            asyncgen_fn_proto: 0,
            arr_proto: 0,
            arr_proto_len: 0,
            array_ctor: 0,
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
            regexp_string_iter_proto: 0,
            ta_base_ctor: 0,
            ta_base_proto: 0,
            ta_ctors: [0; 11],
            ta_protos: [0; 11],
            arraybuffer_ctor: 0,
            arraybuffer_proto: 0,
            dataview_ctor: 0,
            dataview_proto: 0,
            sab_ctor: 0,
            sab_proto: 0,
            shared_buffers: std::collections::HashSet::new(),
            immutable_buffers: std::collections::HashSet::new(),
            error_data: std::collections::HashSet::new(),
            arguments_objs: std::collections::HashSet::new(),
            module_base_dir: None,
            module_cache: std::collections::HashMap::new(),
            module_namespaces: std::collections::HashMap::new(),
            module_own: std::collections::HashMap::new(),
            closure_home: std::collections::HashMap::new(),
            from_async_fn: None,
            async_dispose_fn: None,
            using_resources: std::collections::HashMap::new(),
            using_next_id: 0,
            weakref_ctor: 0,
            finreg_ctor: 0,
            weakmap_ctor: 0,
            weakset_ctor: 0,
            disposablestack_ctor: 0,
            disposablestack_proto: 0,
            dispose_stacks: std::collections::HashMap::new(),
            asyncdisposablestack_ctor: 0,
            asyncdisposablestack_proto: 0,
            async_stacks: std::collections::HashSet::new(),
            suppressederror_ctor: 0,
            suppressederror_proto: 0,
            shadowrealm_ctor: 0,
            shadowrealm_proto: 0,
            shadow_realms: std::collections::HashSet::new(),
            realms: vec![std::collections::HashMap::new()], // realm 0 = main
            obj_realm: std::collections::HashMap::new(),
            realm_ctor_main: std::collections::HashMap::new(),
            fn_proto_override: std::collections::HashMap::new(),
            is_htmldda: std::collections::HashSet::new(),
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
            zoneddatetime_ctor: 0,
            zoneddatetime_proto: 0,
            zdt_tz: std::collections::HashMap::new(),
            intl_ns: 0,
            intl_ctors: [0; 10],
            intl_protos: [0; 10],
            symbol_counter: 0,
            symbol_registry: std::collections::HashMap::new(),
            symbol_keys: std::collections::HashMap::new(),
            iterator_proto_root: 0,
            iterator_helper_proto: 0,
            gen_proto: 0,
            asyncgen_proto: 0,
            default_array_iter: Value::UNDEFINED,
            default_array_iter_next: Value::UNDEFINED,
            throw_type_error: Value::UNDEFINED,
            iterator_ctor: 0,
            dollar262: 0,
            array_iter_proto: 0,
            map_iter_proto: 0,
            set_iter_proto: 0,
            string_iter_proto: 0,
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
            // 0 until set_gc_floor() runs after setup; until then nothing is
            // collectable, so an early GC (if any) is a no-op.
            gc_floor: 0,
            gc_lock: 0,
            gc_stress: std::env::var_os("ZIPP_GC_STRESS").is_some(),
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
            self.frames.push(Frame { super_done: false, eval_scope: u32::MAX,
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
        self.frames.push(Frame { super_done: false, eval_scope: u32::MAX,
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

    /// Set the directory used to resolve a dynamic `import(specifier)` against the
    /// filesystem (the running script's directory). Without it, `import()` rejects.
    pub fn set_module_base_dir(&mut self, dir: Option<std::path::PathBuf>) {
        self.module_base_dir = dir;
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

        let top = self.func(0);
        let base = 0usize;
        let top_regs = top.reg_count as usize;
        self.regs.resize(top_regs, Value::UNDEFINED);
        // A Script's top-level `this` is the global object (a Module's would be
        // undefined). Reg 0 is `this`; seed it with globalThis so sloppy code like
        // `this.x = 1` at the top level targets the global object.
        if self.global_this != 0 {
            self.regs[base] = Value::heap(self.global_this);
        }
        // Reserve register-file capacity up front so JIT self-recursion can
        // append callee windows without reallocating `self.regs` (which would
        // dangle the native code's window pointer). Must happen while regs holds
        // only the top frame so the reservation math is relative to a known base.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        self.reserve_jit_regs();
        self.frames.push(Frame { super_done: false, eval_scope: u32::MAX, func: 0, base, ip: 0, ret_dst: 0, closure: NO_CLOSURE, handlers: Vec::new(), new_target: Value::UNDEFINED, callee: Value::UNDEFINED });
        // Everything allocated so far (interned strings, all built-ins, hoisted
        // top-level functions) is pinned: the GC never collects below this floor.
        self.set_gc_floor();
        // Run until the top-level frame returns (frames drains back to 0), then
        // run the event loop: drain queued microtasks (promise reactions, async
        // resumes) to empty. Drains even on a main throw (matches node ordering),
        // then returns the original result.
        let main = self.run_loop(0);
        self.drain_microtasks();
        main
    }

    /// Run a MODULE as the program entry. The top-level body (func 0) is an async
    /// activation so top-level `await` works; the event loop then drains to
    /// completion (settling promises, running queued async tests). An uncaught
    /// top-level rejection is surfaced as the program error.
    /// Run an ENTRY module that contains STATIC IMPORTS through the module
    /// loader: dependencies link before the body evaluates. The loader path
    /// is synchronous, so a top-level await in such an entry surfaces the
    /// explicit not-yet-supported TypeError (B15 lifts that).
    pub fn run_module_entry(&mut self, path: &std::path::Path) -> Result<Value, Thrown> {
        // The host (harness) script may have already run on this Vm — do NOT
        // re-setup (re-hoisting would re-materialize host functions, losing
        // properties assigned to them, e.g. assert.sameValue).
        if self.global_this == 0 {
            self.setup_globals();
            self.hoist_functions();
            self.set_gc_floor();
        }
        let r = self.import_module(path, None);
        // An ENTRY whose top-level await suspended finishes through the
        // microtask drain below; if its body promise then REJECTED, that
        // rejection IS the program's error (the entry module failed).
        let body = self.pending_module_body.take();
        self.drain_microtasks();
        if let Some(bp) = body {
            if bp.is_heap() {
                let st = match self.heap.get(bp.heap_index()) {
                    HeapObj::Promise { state, result, .. } => Some((*state, *result)),
                    _ => None,
                };
                if let Some((crate::heap::PromiseState::Rejected, reason)) = st {
                    if let HeapObj::Promise { handled, .. } =
                        self.heap.get_mut(bp.heap_index())
                    {
                        *handled = true;
                    }
                    let msg = self.throw_message(reason);
                    self.pending_throw = Some(reason);
                    return Err(Thrown(msg));
                }
            }
        }
        r.map(|_| Value::UNDEFINED)
    }

    /// `import_module` for a STATIC link site: a dependency that SUSPENDED at
    /// top-level await is COLLECTED (the importer defers its own body until
    /// every pending dependency settles — async module evaluation). Bindings
    /// are already linked; the values arrive through the shared live slots.
    fn import_module_sync(
        &mut self,
        raw_path: &std::path::Path,
        mtype: Option<&str>,
    ) -> Result<Value, Thrown> {
        let r = self.import_module(raw_path, mtype)?;
        if let Some(bp) = self.pending_module_body.take() {
            self.link_pending_deps.push(bp);
        }
        Ok(r)
    }

    /// Execute a module body whose dependencies have all settled, and settle
    /// its capability promise: a fulfilled body refreshes the namespace
    /// snapshot and resolves; a rejected/thrown body rejects; a body that
    /// itself suspends at top-level await is ADOPTED (pass-through reactions).
    pub(crate) fn run_deferred_module(&mut self, cap: u32, st: DeferredModuleExec) {
        let exec = self.execute_eval_program(
            st.base_func,
            Some(Value::UNDEFINED),
            None,
            Value::UNDEFINED,
            None,
            None,
            None,
        );
        match exec {
            Ok(v) => {
                let state = if v.is_heap() {
                    match self.heap.get(v.heap_index()) {
                        HeapObj::Promise { state, result, .. } => Some((*state, *result)),
                        _ => None,
                    }
                } else {
                    None
                };
                match state {
                    Some((crate::heap::PromiseState::Rejected, r)) => {
                        if let HeapObj::Promise { handled, .. } =
                            self.heap.get_mut(v.heap_index())
                        {
                            *handled = true;
                        }
                        self.reject(cap, r);
                    }
                    Some((crate::heap::PromiseState::Pending, _)) => {
                        self.then_internal(
                            v.heap_index(),
                            Value::UNDEFINED,
                            Value::UNDEFINED,
                            Some(cap),
                        );
                    }
                    _ => {
                        self.populate_module_namespace(st.ns_idx, &st.full2);
                        self.resolve(cap, Value::UNDEFINED);
                    }
                }
            }
            Err(Thrown(msg)) => {
                let reason = self
                    .pending_throw
                    .take()
                    .unwrap_or_else(|| self.error_from_thrown(&msg));
                self.reject(cap, reason);
            }
        }
    }

    pub fn run_module(&mut self) -> Result<Value, Thrown> {
        use crate::heap::PromiseState;
        self.setup_globals();
        self.hoist_functions();
        self.set_gc_floor();
        // Module top-level `this` is undefined. alloc_async builds + drives the
        // activation to its first await; drain_microtasks runs it to completion.
        let p = self.alloc_async(0, NO_CLOSURE, Value::UNDEFINED, &[]);
        self.drain_microtasks();
        if p.is_heap() {
            if let HeapObj::Promise { state: PromiseState::Rejected, result, .. } =
                self.heap.get(p.heap_index())
            {
                let reason = *result;
                // Render the rejection like an uncaught throw ("Name: message")
                // rather than display() (which gives "[object Object]" for an Error).
                let msg = self.throw_message(reason);
                return Err(Thrown(msg));
            }
        }
        Ok(Value::UNDEFINED)
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
    /// For an arrow callee (`lexical_this` proto), return the `this` it captured
    /// lexically — which replaces any `this` the caller supplied and suppresses
    /// OrdinaryCallBindThis. Returns `this` unchanged for non-arrows. `closure`
    /// is the callee's `Closure` heap index (arrows are always closures) or
    /// `NO_CLOSURE`.
    pub(crate) fn rebind_arrow_this(&self, func_id: u32, closure: u32, this: Value) -> Value {
        if closure != NO_CLOSURE && self.func(func_id as usize).lexical_this {
            if let HeapObj::Closure { this_val, .. } = self.heap.get(closure) {
                return *this_val;
            }
        }
        this
    }

    /// If `callee` is an arrow function value, its lexically-captured `this`; else
    /// `None`. Used by call paths that hold the callee Value rather than its
    /// `(func_id, closure)` pair.
    pub(crate) fn arrow_captured_this(&self, callee: Value) -> Option<Value> {
        if callee.is_heap() {
            if let HeapObj::Closure { func, this_val, .. } = self.heap.get(callee.heap_index()) {
                if self.func(*func as usize).lexical_this {
                    return Some(*this_val);
                }
            }
        }
        None
    }

    pub(crate) fn call_value(&mut self, callee: Value, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        // An [[IsHTMLDDA]] exotic (`document.all`) is callable: its [[Call]] returns
        // null when called with NO arguments or a first argument that is the empty
        // String, and undefined otherwise (Annex B).
        if callee.is_heap() && !self.is_htmldda.is_empty() && self.is_htmldda.contains(&callee.heap_index()) {
            let first_is_empty_str = args.first().is_some_and(|a| {
                a.is_heap()
                    && self.heap.is_str_like(a.heap_index())
                    && self.heap.str_cow(a.heap_index()).is_some_and(|s| s.is_empty())
            });
            return Ok(if args.is_empty() || first_is_empty_str {
                Value::NULL
            } else {
                Value::UNDEFINED
            });
        }
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
        // A ShadowRealm WrappedFunction: wrap each argument across the
        // boundary (a non-primitive non-callable argument is a TypeError),
        // call the target with `this` = undefined, wrap the result, and map
        // ANY abrupt target completion to a caller-realm TypeError.
        if callee.is_heap() {
            if let HeapObj::Wrapped { target, .. } = self.heap.get(callee.heap_index()) {
                let t = *target;
                let _gc = self.gc_lock_guard(); // wargs held across allocating calls
                let mut wargs = Vec::with_capacity(args.len());
                for &a in args {
                    wargs.push(self.wrap_realm_value(a)?);
                }
                return match self.call_value(t, Value::UNDEFINED, &wargs) {
                    Ok(v) => self.wrap_realm_value(v),
                    Err(_) => {
                        self.pending_throw.take();
                        Err(Thrown(
                            "TypeError: WrappedFunction call threw (error wrapped at the realm boundary)"
                                .into(),
                        ))
                    }
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
            // A combinator resolve/reject element invoked directly (a custom
            // thenable calling the `then` callback): run the combinator step.
            if let HeapObj::CombinatorResolver { combinator, index, is_reject } =
                self.heap.get(callee.heap_index())
            {
                let (c, i, isr) = (*combinator, *index, *is_reject);
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                let kind = if isr { ReactionKind::Reject } else { ReactionKind::Fulfill };
                self.combinator_step(c, i, kind, arg);
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
        // A realm constructor called as a plain function (`other.Symbol('x')`,
        // `other.Array(1, 2)`): route to the MAIN ctor's call behaviour, tagging
        // the result with the realm.
        if callee.is_heap() {
            if let Some(&main) = self.realm_ctor_main.get(&callee.heap_index()) {
                let cr = self.get_function_realm(callee);
                let r = self.call_value(Value::heap(main), this, args)?;
                if r.is_heap() && cr != 0 {
                    self.obj_realm.insert(r.heap_index(), cr);
                }
                return Ok(r);
            }
        }
        // A built-in constructor object called as a plain function (passed as a
        // callback, or via .call/.apply). String/Number/Boolean coerce; the rest
        // construct. (Direct `String(x)` calls are compiler-lowered, so this only
        // fires for indirect invocations.)
        if callee.is_heap() {
            if let HeapObj::Object(m) = self.heap.get(callee.heap_index()) {
                if m.is_ctor {
                    return self.call_ctor_as_function(callee, args);
                }
            }
        }
        let (func_id, closure) = self.resolve_callable(callee)?;
        let (is_gen, is_async, is_strict) = {
            let p = self.func(func_id as usize);
            (p.is_generator, p.is_async, p.is_strict)
        };
        // An arrow ignores the supplied `this` and uses the one it captured
        // lexically (and skips OrdinaryCallBindThis). Otherwise OrdinaryCallBindThis:
        // a sloppy (non-strict) function called with a nullish `this` binds the
        // global object instead. Strict functions — and built-ins, which never
        // reach here — receive `this` as passed.
        let this = if closure != NO_CLOSURE && self.func(func_id as usize).lexical_this {
            self.rebind_arrow_this(func_id, closure, this)
        } else if !is_strict && this.is_nullish() && self.global_this != 0 {
            Value::heap(self.global_this)
        } else if !is_strict && !self.is_object_value(this) && self.global_this != 0 {
            // OrdinaryCallBindThis: a sloppy function boxes a primitive `this`
            // (number/string/boolean/symbol/bigint) to its wrapper object.
            self.to_object(this)?
        } else {
            this
        };
        // An `async function*` builds a suspended AsyncGenerator (an async
        // iterator); it doesn't run until `.next()` (but its parameter prologue
        // runs eagerly here, so a destructuring throw propagates from the call).
        if is_gen && is_async {
            return self.alloc_async_generator(func_id, closure, this, args);
        }
        // Calling a generator function builds a suspended Generator, not a frame.
        // (The parameter prologue runs eagerly here, so a destructuring throw
        // propagates from the call.)
        if is_gen {
            return self.alloc_generator(func_id, closure, this, args);
        }
        // Calling an async function runs synchronously up to the first `await`,
        // then returns its result Promise.
        if is_async {
            return Ok(self.alloc_async(func_id, closure, this, args));
        }
        if self.frames.len() >= MAX_FRAMES {
            return Err(Thrown("RangeError: Maximum call stack size exceeded".into()));
        }
        // Copy the scalar layout fields out so the FuncProto borrow (which now
        // spans the whole `&self` via `func()`) ends before the `self.regs` /
        // `self.heap` mutations below.
        let (callee_regs, callee_params, rest_reg, arguments_reg) = {
            let proto = self.func(func_id as usize);
            ((proto.reg_count as usize).max(1), proto.param_count as usize, proto.rest_reg, proto.arguments_reg)
        };

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
        if let Some(rreg) = rest_reg {
            let extra: Vec<Value> = args.get(callee_params..).unwrap_or(&[]).to_vec();
            let arr = Value::heap(self.heap.alloc(HeapObj::Array(extra)));
            self.regs[new_base + rreg as usize] = arr;
        }
        // `arguments`: ALL actual args (not just the declared params), so a
        // callback invoked here (e.g. an array-method callback that reads
        // `arguments[2]`) sees every argument — matching the direct Call op.
        if let Some(areg) = arguments_reg {
            let is_strict = self.func(func_id as usize).is_strict;
            let arr = self.build_arguments_object(args.to_vec(), callee, is_strict);
            self.regs[new_base + areg as usize] = arr;
        }

        let stop_depth = self.frames.len();
        let new_target = std::mem::replace(&mut self.pending_new_target, Value::UNDEFINED);
        self.frames.push(Frame { super_done: false, eval_scope: u32::MAX, func: func_id, base: new_base, ip: 0, ret_dst: 0, closure, handlers: Vec::new(), new_target, callee });
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

    /// Resolve a global NAME referenced inside an `eval` to a live global slot.
    /// Names already in the compile-time program reuse their slot; genuinely new
    /// names (sloppy `x = 1`, `var x`, hoisted fns, or builtins the program never
    /// named) draw a fresh EVAL_POOL slot, seeded UNINITIALIZED so a read before a
    /// write is a ReferenceError (matching sloppy global-scope semantics).
    pub(crate) fn eval_global_slot(&mut self, name: &str) -> Result<u32, Thrown> {
        if let Some(i) = self.program.global_names.iter().position(|n| n == name) {
            return Ok(i as u32);
        }
        if let Some(&s) = self.eval_global_map.get(name) {
            return Ok(s);
        }
        let cap = self.program.global_count + (FIELD_POOL + EVAL_POOL) as u32;
        if self.eval_global_next >= cap {
            return Err(Thrown(
                "EvalError: too many distinct globals introduced by eval".into(),
            ));
        }
        let s = self.eval_global_next;
        self.eval_global_next += 1;
        self.eval_global_map.insert(name.to_string(), s);
        // A builtin the main program never referenced still resolves in eval'd
        // code (`eval("new RangeError()")`, `eval("Object.keys(x)")`): seed the
        // fresh slot with the builtin value rather than the never-declared
        // sentinel. A genuinely-undeclared name stays UNINITIALIZED → ReferenceError.
        self.globals[s as usize] = match self.builtin_globals.get(name) {
            Some(&v) => Value::heap(v),
            None => Value::UNINITIALIZED,
        };
        Ok(s)
    }

    /// Parse, compile, and run an `eval` code string (indirect eval — global,
    /// sloppy scope), returning its completion value. ADDITIVE: the broader suite
    /// never reaches this (calling `eval` previously threw ReferenceError), so it
    /// cannot regress non-eval programs. Classes inside eval are supported via the
    /// `eval_classes` runtime class table (class-id operands re-indexed like funcs).
    /// The NAME behind a global slot: a main-program global, or an
    /// EVAL_POOL slot recorded in eval_global_map.
    pub(crate) fn global_slot_name(&self, idx: u32) -> Option<String> {
        self.program
            .global_names
            .get(idx as usize)
            .cloned()
            .or_else(|| {
                self.eval_global_map
                    .iter()
                    .find(|(_, &v)| v == idx)
                    .map(|(k, _)| k.clone())
            })
    }

    pub(crate) fn do_eval(
        &mut self,
        code: &str,
        force_strict: bool,
        force_new_target_ok: bool,
        this_override: Option<Value>,
        inherit_super: Option<(u32, bool)>,
        ban_arguments: bool,
        direct: bool,
        caller_new_target: Value,
        caller_home_obj: Option<Value>,
        var_env_global: bool,
        param_collisions: Option<Vec<String>>,
        caller_scope: Option<(Vec<String>, Vec<Value>)>,
        eval_scope_idx: Option<u32>,
    ) -> Result<Value, Thrown> {
        // 1. Parse.
        let allocator = oxc_allocator::Allocator::default();
        // eval code is a Script (never a module), so `await` is a valid identifier
        // and the body runs in sloppy mode unless it carries a "use strict".
        let ret = oxc_parser::Parser::new(&allocator, code, oxc_span::SourceType::script()).parse();
        if !ret.errors.is_empty() {
            return Err(Thrown(format!("SyntaxError: {}", ret.errors[0])));
        }
        // A direct eval in a PARAMETER DEFAULT: its sloppy var/function names
        // may not collide with the param-scope bindings (params + implicit
        // `arguments`) — SyntaxError BEFORE anything runs or is declared.
        if let Some(cols) = &param_collisions {
            let src_strict = force_strict
                || ret
                    .program
                    .directives
                    .iter()
                    .any(|d| d.directive.as_str() == "use strict");
            if !src_strict {
                for n in crate::compile::eval_var_and_fn_names(&ret.program) {
                    if cols.iter().any(|c| *c == n) {
                        return Err(Thrown(format!(
                            "SyntaxError: Identifier '{n}' has already been declared"
                        )));
                    }
                }
            }
        }
        // Eval code is never module code: import/export declarations are a
        // SyntaxError (spec PerformEval parses goal Script; oxc's script goal
        // still produces the module-decl statement variants).
        if ret.program.body.iter().any(|s| {
            matches!(
                s,
                oxc_ast::ast::Statement::ImportDeclaration(_)
                    | oxc_ast::ast::Statement::ExportNamedDeclaration(_)
                    | oxc_ast::ast::Statement::ExportDefaultDeclaration(_)
                    | oxc_ast::ast::Statement::ExportAllDeclaration(_)
            )
        }) {
            return Err(Thrown(
                "SyntaxError: import/export declarations may only appear in modules".into(),
            ));
        }
        // A DIRECT eval sees the caller's lexical private scope: the declared
        // NAMES gate the compile-time early error; the brand CHAIN drives the
        // runtime declaring-class resolution inside the eval'd code.
        let (visible, caller_chain) = if direct {
            match self.current_private_brands() {
                Some(ch) => {
                    let mut s = std::collections::HashSet::new();
                    for b in ch {
                        if let Some(names) = self.brand_private_names.get(b) {
                            for (n, _) in names {
                                s.insert(n.clone());
                            }
                        }
                    }
                    (s, Some(ch.clone()))
                }
                None => (std::collections::HashSet::new(), None),
            }
        } else {
            (std::collections::HashSet::new(), None)
        };
        // 2. Compile in eval mode (top-level returns its completion value).
        let eval_prog = match crate::compile::compile_eval(
            &ret.program,
            code,
            force_strict,
            force_new_target_ok,
            inherit_super.map(|(_, s)| s),
            ban_arguments,
            visible,
            false,
            caller_home_obj.is_some(),
            caller_scope
                .as_ref()
                .map(|(n, _)| n.clone())
                .unwrap_or_default(),
            eval_scope_idx.is_some(),
        ) {
            Ok(p) => p,
            Err(e) => return Err(Thrown(format!("SyntaxError: {e}"))),
        };
        self.run_eval_program(
            eval_prog,
            this_override,
            false,
            inherit_super.map(|(h, _)| h),
            caller_chain,
            caller_new_target,
            caller_home_obj,
            var_env_global,
            caller_scope.map(|(_, c)| c),
            eval_scope_idx,
        )
        .map(|(v, _)| v)
    }

    /// The `Array.fromAsync` implementation, as a lazily-compiled JS polyfill
    /// (an async function value). Spec behaviour expressed in JS so it reuses the
    /// engine's `for await`/`await` machinery; compiled once via `do_eval`, then
    /// cached + GC-rooted. Called with `this` = the receiver constructor C; returns
    /// a Promise the top-level microtask drain progresses.
    pub(crate) fn from_async_polyfill(&mut self) -> Result<Value, Thrown> {
        if let Some(f) = self.from_async_fn {
            return Ok(f);
        }
        // Drives iteration manually (it.next()/await) instead of `for await`, so
        // the observable Get/Call sequence matches the proposal exactly: ONE
        // GetMethod per iterator symbol, the async-from-sync VALUE await, and
        // AsyncIteratorClose on exactly the abrupt completions the spec closes on
        // (k-limit, sync-value await, mapfn, define) — never on next() itself.
        const SRC: &str = r#"(async function fromAsync(items, mapfn, thisArg) {
  'use strict';
  var C = this;
  if (items === undefined || items === null)
    throw new TypeError('Array.fromAsync requires an array-like or iterable object');
  var mapping = mapfn !== undefined;
  if (mapping && typeof mapfn !== 'function')
    throw new TypeError('Array.fromAsync mapper is not a function');
  var method = items[Symbol.asyncIterator];
  if (method === undefined || method === null) method = undefined;
  else if (typeof method !== 'function') throw new TypeError('@@asyncIterator is not a function');
  var isSync = false;
  if (method === undefined) {
    var syncMethod = items[Symbol.iterator];
    if (syncMethod === undefined || syncMethod === null) syncMethod = undefined;
    else if (typeof syncMethod !== 'function') throw new TypeError('@@iterator is not a function');
    if (syncMethod !== undefined) { method = syncMethod; isSync = true; }
  }
  if (method !== undefined) {
    var it = method.call(items);
    if (Object(it) !== it) throw new TypeError('iterator is not an object');
    var A = (typeof C === 'function') ? new C() : [];
    var k = 0;
    var closing = false;
    try {
      for (;;) {
        closing = false;
        if (k >= 9007199254740991) {
          closing = true;
          throw new TypeError('Array.fromAsync result exceeds the maximum length');
        }
        var res = await it.next();
        if (Object(res) !== res) throw new TypeError('iterator result is not an object');
        if (res.done) break;
        var v = res.value;
        closing = true;
        if (isSync) v = await v;
        var mapped = mapping ? await mapfn.call(thisArg, v, k) : v;
        Object.defineProperty(A, k, { value: mapped, writable: true, enumerable: true, configurable: true });
        k = k + 1;
      }
    } catch (e) {
      if (closing) {
        try {
          var ret = it.return;
          if (ret !== undefined && ret !== null) await ret.call(it);
        } catch (_ignored) {}
      }
      throw e;
    }
    A.length = k;
    return A;
  } else {
    var arrayLike = Object(items);
    var ln = +arrayLike.length;
    var len = ln !== ln ? 0 : Math.max(0, Math.min(Math.trunc(ln), 9007199254740991));
    var A = (typeof C === 'function') ? new C(len) : new Array(len);
    var k = 0;
    while (k < len) {
      var kValue = await arrayLike[k];
      var mapped = mapping ? await mapfn.call(thisArg, kValue, k) : kValue;
      Object.defineProperty(A, k, { value: mapped, writable: true, enumerable: true, configurable: true });
      k = k + 1;
    }
    A.length = len;
    return A;
  }
})"#;
        let f = self.do_eval(SRC, false, false, None, None, false, false, Value::UNDEFINED, None, false, None, None, None)?;
        self.from_async_fn = Some(f);
        Ok(f)
    }

    /// `%AsyncIteratorPrototype%[@@asyncDispose]`, as a lazily-compiled JS polyfill
    /// (an async function). Reads `this.return`; if nullish, resolves to undefined;
    /// a present non-callable `return` rejects with a TypeError; otherwise calls it
    /// and awaits the result (so a rejected result rejects), resolving to undefined.
    /// Compiled once via `do_eval`, cached + GC-rooted; called with `this` = the
    /// iterator and returns a Promise.
    pub(crate) fn async_dispose_polyfill(&mut self) -> Result<Value, Thrown> {
        if let Some(f) = self.async_dispose_fn {
            return Ok(f);
        }
        const SRC: &str = r#"(async function() {
  var O = this;
  var ret = O.return;
  if (ret === undefined || ret === null) return undefined;
  if (typeof ret !== 'function')
    throw new TypeError('the iterator [Symbol.iterator] return method is not callable');
  await ret.call(O);
  return undefined;
})"#;
        let f = self.do_eval(SRC, false, false, None, None, false, false, Value::UNDEFINED, None, false, None, None, None)?;
        self.async_dispose_fn = Some(f);
        Ok(f)
    }

    /// Recursively load + LINK a MODULE file for a dynamic `import()`, returning its
    /// (fully-linked) Module Namespace exotic. Cached by canonical path so a re-import
    /// (or a cycle) yields the SAME namespace. Steps: (1) cache hit → return; (2) read
    /// + compile as a module (strict); a real `import` decl / `export * as ns` → reject
    /// (unlinkable); (3) run the body in its own per-module env → OWN export live slots;
    /// (4) mark in-progress (module_own) and resolve `export … from` / `export *`
    /// re-exports by recursively loading the dependency and pointing this namespace's
    /// names at the dependency's live slots; (5) build + cache the namespace. A parse/
    /// compile error, a missing dependency, or a throw during evaluation propagates as
    /// `Err`. The given `path` is canonicalized; relative re-export specifiers resolve
    /// against the module's own directory.
    pub(crate) fn import_module(
        &mut self,
        raw_path: &std::path::Path,
        mtype: Option<&str>,
    ) -> Result<Value, Thrown> {
        let path = std::fs::canonicalize(raw_path)
            .map_err(|_| Thrown("TypeError: module not found".into()))?;
        if let Some(&ns) = self.module_cache.get(&path) {
            return Ok(ns);
        }
        // A typed import ({type:'json'|'text'}) builds a synthetic namespace
        // with a single `default` export; unknown types reject.
        if let Some(t) = mtype {
            let text = std::fs::read_to_string(&path)
                .map_err(|_| Thrown("TypeError: module not found".into()))?;
            let val = match t {
                "json" => self.json_parse(&text)?,
                "text" => self.alloc_str(text),
                _ => {
                    return Err(Thrown(format!(
                        "TypeError: unsupported module type '{t}'"
                    )))
                }
            };
            self.globals.push(val);
            let slot = (self.globals.len() - 1) as u32;
            let ns_idx = self.alloc_empty_namespace();
            self.populate_module_namespace(ns_idx, &[("default".to_string(), slot)]);
            self.module_cache.insert(path.clone(), Value::heap(ns_idx));
            return Ok(Value::heap(ns_idx));
        }
        let code = std::fs::read_to_string(&path)
            .map_err(|_| Thrown("TypeError: module not found".into()))?;
        let allocator = oxc_allocator::Allocator::default();
        let ret =
            oxc_parser::Parser::new(&allocator, &code, oxc_span::SourceType::mjs()).parse();
        if !ret.errors.is_empty() {
            return Err(Thrown(format!("SyntaxError: {}", ret.errors[0])));
        }
        let prog = match crate::compile::compile_eval(&ret.program, &code, true, false, None, false, std::collections::HashSet::new(), true, false, Vec::new(), false) {
            Ok(p) => p,
            // Top-level await in an IMPORTED module needs the async-module
            // evaluation pipeline (not built yet): surface a host TypeError —
            // a SyntaxError would misreport a spec-VALID module as malformed.
            Err(e) if e.contains("only valid inside an async function")
                || e.contains("only valid in an async function") => {
                return Err(Thrown(
                    "TypeError: top-level await is not supported in imported modules yet".into(),
                ));
            }
            Err(e) => return Err(Thrown(format!("SyntaxError: {e}"))),
        };
        let exports = prog.module_exports.clone();
        let names = prog.global_names.clone();
        let reexports = prog.module_reexports.clone();
        let star_reexports = prog.module_star_reexports.clone();
        let ns_reexports = prog.module_ns_reexports.clone();
        let imports = prog.module_imports.clone();
        let dir = path.parent().map(|p| p.to_path_buf());
        // STATIC imports resolve BEFORE this module's body prepares/runs
        // (dependencies instantiate + evaluate first, per the link order):
        // Named/Default locals alias the dependency's live export slot;
        // Namespace locals get the namespace value written post-prepare;
        // SideEffect just evaluates the dependency. Resolution failures are
        // link-time SyntaxErrors. A SELF-import (the dominant test262 cycle)
        // aliases the module's OWN exported local by COMPILE slot — resolved
        // to the live slot inside prepare's second pass. Other in-flight
        // cycles are guarded (no recursion blowup).
        let mut import_aliases: std::collections::HashMap<u32, u32> =
            std::collections::HashMap::new();
        let mut self_aliases: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut ns_writes: Vec<(u32, Value)> = Vec::new();
        let mut self_ns_locals: Vec<u32> = Vec::new();
        // Own (exported name -> compile slot), for self-import aliasing.
        let own_cslot = |name: &str| -> Option<u32> {
            prog.module_exports.iter().find(|(e, _)| e == name).and_then(|(_, local)| {
                prog.global_names
                    .iter()
                    .position(|n| n == local)
                    .map(|i| i as u32)
            })
        };
        // PRE-REGISTER this module BEFORE any dependency loads: live slots for
        // its declared exports are PRE-ALLOCATED (prepare reuses them), so a
        // CYCLIC re-export back into this module (a dependency doing
        // `export { x } from './me'` while we are mid-load) resolves to the
        // real binding instead of re-evaluating this module as a second
        // instance. An export whose local is itself an IMPORT binding resolves
        // through its dependency; its map entry is patched after prepare.
        let import_locals: std::collections::HashSet<u32> = imports
            .iter()
            .filter(|e| e.local_slot != u32::MAX)
            .map(|e| e.local_slot)
            .collect();
        let decl_set: std::collections::HashSet<u32> =
            prog.module_decl_globals.iter().copied().collect();
        let cap = self.program.global_count + (FIELD_POOL + EVAL_POOL) as u32;
        let mut prealloc: std::collections::HashMap<u32, u32> =
            std::collections::HashMap::new();
        let mut own_pre: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        for (exported, local) in &exports {
            if let Some(i) = names.iter().position(|n| n == local) {
                let c = i as u32;
                if import_locals.contains(&c) || !decl_set.contains(&c) {
                    continue;
                }
                if let Some(&live) = prealloc.get(&c) {
                    own_pre.insert(exported.clone(), live);
                    continue;
                }
                if self.eval_global_next >= cap {
                    return Err(Thrown(
                        "EvalError: too many distinct globals introduced by eval".into(),
                    ));
                }
                let live = self.eval_global_next;
                self.eval_global_next += 1;
                self.globals[live as usize] = Value::UNINITIALIZED;
                prealloc.insert(c, live);
                own_pre.insert(exported.clone(), live);
            }
        }
        let ns_idx = self.alloc_empty_namespace();
        self.module_namespaces.insert(ns_idx, own_pre.clone());
        self.module_cache.insert(path.clone(), Value::heap(ns_idx));
        self.module_own.insert(path.clone(), own_pre);
        self.module_pending_reexports.insert(
            path.clone(),
            (reexports.clone(), star_reexports.clone(), dir.clone()),
        );
        // Dependencies that suspend at top-level await get collected past this
        // mark (nested links use their own marks via the same discipline).
        let lp_mark = self.link_pending_deps.len();
        self.module_loading.insert(path.clone());
        let import_res = (|| -> Result<(), Thrown> {
            // LOADING phase: every requested module (including phase-import
            // requests) must resolve to a readable file BEFORE linking starts.
            // A failure here is a HOST error (TypeError) — it takes precedence
            // over any link-time SyntaxError a sibling request would raise.
            for e in &imports {
                let dep_raw = match dir.as_deref() {
                    Some(d) => d.join(&e.specifier),
                    None => std::path::PathBuf::from(&e.specifier),
                };
                let dep_canon = std::fs::canonicalize(&dep_raw).unwrap_or_else(|_| dep_raw.clone());
                if dep_canon == path {
                    continue; // self-reference: already loaded
                }
                if std::fs::metadata(&dep_raw).is_err() {
                    return Err(Thrown(format!(
                        "TypeError: Failed to resolve module specifier '{}'",
                        e.specifier
                    )));
                }
            }
            for e in &imports {
                let dep_raw = match dir.as_deref() {
                    Some(d) => d.join(&e.specifier),
                    None => std::path::PathBuf::from(&e.specifier),
                };
                let dep_canon = std::fs::canonicalize(&dep_raw).unwrap_or_else(|_| dep_raw.clone());
                let is_self = dep_canon == path;
                let in_flight = !is_self && self.module_loading.contains(&dep_canon);
                use crate::bytecode::ImportName as IN;
                match &e.import {
                    IN::LoadOnly => {
                        // A phase import's dep is LOADED (recursively — its own
                        // requests resolve eagerly per LoadRequestedModules)
                        // but never linked or evaluated.
                        if !is_self && !in_flight {
                            let mut seen = std::collections::HashSet::new();
                            self.prescan_module_requests(&dep_canon, &mut seen)?;
                        }
                    }
                    IN::SideEffect => {
                        if !is_self && !in_flight {
                            self.import_module_sync(&dep_raw, None)?;
                        }
                    }
                    IN::Named(n) => {
                        if is_self {
                            match own_cslot(n) {
                                Some(c) => {
                                    self_aliases.insert(e.local_slot, c);
                                }
                                None => {
                                    // Not an own local: an INDIRECT export of
                                    // ourselves (`export {x as n} from dep`) —
                                    // walk the pending re-export chain.
                                    let mut seen = std::collections::HashSet::new();
                                    match self.resolve_pending_export(&path, n, &mut seen)? {
                                        Some(slot) => {
                                            import_aliases.insert(e.local_slot, slot);
                                        }
                                        None => {
                                            return Err(Thrown(format!(
                                                "SyntaxError: The requested module '{}' does not provide an export named '{n}'",
                                                e.specifier
                                            )))
                                        }
                                    }
                                }
                            }
                        } else if in_flight {
                            // The dependency is mid-load (a cycle): resolve
                            // through its registered bindings + pending
                            // re-exports instead of evaluating it again.
                            let mut seen = std::collections::HashSet::new();
                            match self.resolve_pending_export(&dep_canon, n, &mut seen)? {
                                Some(slot) => {
                                    import_aliases.insert(e.local_slot, slot);
                                }
                                None => {
                                    return Err(Thrown(format!(
                                        "SyntaxError: The requested module '{}' does not provide an export named '{n}'",
                                        e.specifier
                                    )))
                                }
                            }
                        } else {
                            match self.resolve_export(&dep_raw, n)? {
                                Some(slot) => {
                                    import_aliases.insert(e.local_slot, slot);
                                }
                                None => {
                                    return Err(Thrown(format!(
                                        "SyntaxError: The requested module '{}' does not provide an export named '{n}'",
                                        e.specifier
                                    )))
                                }
                            }
                        }
                    }
                    IN::Default => {
                        if is_self {
                            match own_cslot("default") {
                                Some(c) => {
                                    self_aliases.insert(e.local_slot, c);
                                }
                                None => {
                                    let mut seen = std::collections::HashSet::new();
                                    match self.resolve_pending_export(&path, "default", &mut seen)? {
                                        Some(slot) => {
                                            import_aliases.insert(e.local_slot, slot);
                                        }
                                        None => {
                                            return Err(Thrown(format!(
                                                "SyntaxError: The requested module '{}' does not provide an export named 'default'",
                                                e.specifier
                                            )))
                                        }
                                    }
                                }
                            }
                        } else if in_flight {
                            let mut seen = std::collections::HashSet::new();
                            match self.resolve_pending_export(&dep_canon, "default", &mut seen)? {
                                Some(slot) => {
                                    import_aliases.insert(e.local_slot, slot);
                                }
                                None => {
                                    return Err(Thrown(format!(
                                        "SyntaxError: The requested module '{}' does not provide an export named 'default'",
                                        e.specifier
                                    )))
                                }
                            }
                        } else {
                            match self.resolve_export(&dep_raw, "default")? {
                                Some(slot) => {
                                    import_aliases.insert(e.local_slot, slot);
                                }
                                None => {
                                    return Err(Thrown(format!(
                                        "SyntaxError: The requested module '{}' does not provide an export named 'default'",
                                        e.specifier
                                    )))
                                }
                            }
                        }
                    }
                    IN::Namespace => {
                        if is_self || in_flight {
                            // Our own namespace: pre-registered below; written
                            // after it exists.
                            self_ns_locals.push(e.local_slot);
                        } else {
                            let ns = self.import_module_sync(&dep_raw, None)?;
                            ns_writes.push((e.local_slot, ns));
                        }
                    }
                }
            }
            Ok(())
        })();
        self.module_loading.remove(&path);
        let cleanup_on_err = |vm: &mut Self| {
            vm.module_cache.remove(&path);
            vm.module_namespaces.remove(&ns_idx);
            vm.module_own.remove(&path);
            vm.module_pending_reexports.remove(&path);
            vm.link_pending_deps.truncate(lp_mark);
        };
        if let Err(e) = import_res {
            cleanup_on_err(self);
            return Err(e);
        }
        // PREPARE the module's environment (declared globals → fresh per-module slots,
        // install funcs/classes, hoist) WITHOUT running the body yet. `gmap[i]` is the
        // live slot for compile-time global slot `i`.
        let prepared = self.prepare_eval_program(
            prog,
            true,
            None,
            false,
            None,
            if import_aliases.is_empty() { None } else { Some(&import_aliases) },
            if self_aliases.is_empty() { None } else { Some(&self_aliases) },
            if prealloc.is_empty() { None } else { Some(&prealloc) },
        );
        let (gmap, base_func) = match prepared {
            Ok(p) => p,
            Err(e) => {
                cleanup_on_err(self);
                return Err(e);
            }
        };
        // Namespace import locals are initialized PRIOR to evaluation.
        for (cslot, ns) in ns_writes {
            let live = gmap[cslot as usize] as usize;
            self.globals[live] = ns;
        }
        // OWN exports (exported name → live slot), in source order.
        let mut full: Vec<(String, u32)> = Vec::with_capacity(exports.len());
        let mut own_map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for (exported, local) in exports {
            if let Some(i) = names.iter().position(|n| *n == local) {
                full.push((exported.clone(), gmap[i]));
                own_map.insert(exported, gmap[i]);
            }
        }
        // REFRESH the pre-registered maps with the final own exports (an export
        // whose local is an import binding now has its real aliased slot; the
        // namespace itself was registered before the dependency loop, so cyclic
        // re-exports already resolved against the pre-allocated slots).
        self.module_namespaces.insert(ns_idx, own_map.clone());
        // A self-imported `import * as ns from './me'` local gets OUR OWN
        // pre-registered namespace object.
        for cslot in &self_ns_locals {
            let live = gmap[*cslot as usize] as usize;
            self.globals[live] = Value::heap(ns_idx);
        }
        self.module_own.insert(path.clone(), own_map);
        // Re-exports LINK (and their dependencies evaluate) BEFORE this body
        // runs — the namespace must be complete at evaluation start (a TDZ
        // read of an indirect export during the body must find its binding).
        let linked = self.link_module_reexports(
            full,
            &reexports,
            &star_reexports,
            &ns_reexports,
            dir.as_deref(),
        );
        let linked = match linked {
            Ok((full2, ambiguous)) => {
                // EARLY ObjMap fill: reflection DURING the body (Object.keys,
                // defineProperty of @@toStringTag, descriptors) sees the real
                // key set + tag; snapshot values refresh after execution.
                self.populate_module_namespace(ns_idx, &full2);
                if !ambiguous.is_empty() {
                    self.module_ambiguous.insert(ns_idx, ambiguous.clone());
                }
                // Dependencies that SUSPENDED at top-level await: defer this
                // body until every one settles (spec AsyncEvaluating). The
                // capability promise stands in as OUR body promise — importers
                // up the chain wait on it transitively.
                let pending_deps: Vec<Value> = self.link_pending_deps.split_off(lp_mark);
                if !pending_deps.is_empty() {
                    let cap = self.alloc_promise();
                    self.deferred_mods.insert(
                        cap,
                        DeferredModuleExec {
                            remaining: pending_deps.len(),
                            base_func,
                            ns_idx,
                            full2: full2.clone(),
                        },
                    );
                    for bp in pending_deps {
                        let _gc = self.gc_lock_guard();
                        let ok_t =
                            Value::heap(self.heap.alloc(HeapObj::Native(native::MODULE_DEP_OK)));
                        let on_ok = Value::heap(self.heap.alloc(HeapObj::Bound {
                            target: ok_t,
                            this: Value::num(cap as f64),
                            args: Vec::new(),
                        }));
                        let fail_t = Value::heap(
                            self.heap.alloc(HeapObj::Native(native::MODULE_DEP_FAIL)),
                        );
                        let on_fail = Value::heap(self.heap.alloc(HeapObj::Bound {
                            target: fail_t,
                            this: Value::num(cap as f64),
                            args: Vec::new(),
                        }));
                        self.then_internal(bp.heap_index(), on_ok, on_fail, None);
                    }
                    self.pending_module_body = Some(Value::heap(cap));
                    return {
                        self.module_own.remove(&path);
                        self.module_pending_reexports.remove(&path);
                        Ok(Value::heap(ns_idx))
                    };
                }
                let exec = self
                    .execute_eval_program(
                        base_func,
                        // Module code's top-level `this` is UNDEFINED (never
                        // the global object).
                        Some(Value::UNDEFINED),
                        None,
                        Value::UNDEFINED,
                        None,
                        None,
                        None,
                    );
                match exec {
                    Ok(v) => {
                        // The body is an ASYNC activation: the result is its
                        // body promise. A no-await body settles synchronously;
                        // a REJECTED body is the module's thrown error; a
                        // PENDING one suspended at top-level await — the
                        // namespace is linked, the promise is published for
                        // the importer to settle from (stage-1 TLA).
                        let st = if v.is_heap() {
                            match self.heap.get(v.heap_index()) {
                                HeapObj::Promise { state, result, .. } => {
                                    Some((*state, *result))
                                }
                                _ => None,
                            }
                        } else {
                            None
                        };
                        match st {
                            Some((crate::heap::PromiseState::Rejected, r)) => {
                                if let HeapObj::Promise { handled, .. } =
                                    self.heap.get_mut(v.heap_index())
                                {
                                    *handled = true; // the loader consumes it
                                }
                                let msg = self.throw_message(r);
                                self.pending_throw = Some(r);
                                Err(Thrown(msg))
                            }
                            Some((crate::heap::PromiseState::Pending, _)) => {
                                self.pending_module_body = Some(v);
                                Ok((full2, ambiguous))
                            }
                            _ => Ok((full2, ambiguous)),
                        }
                    }
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        self.module_own.remove(&path);
        self.module_pending_reexports.remove(&path);
        match linked {
            Ok((full, ambiguous)) => {
                self.populate_module_namespace(ns_idx, &full);
                if !ambiguous.is_empty() {
                    self.module_ambiguous.insert(ns_idx, ambiguous);
                }
                Ok(Value::heap(ns_idx))
            }
            Err(e) => {
                // The module threw / a dependency failed: discard the half-built entry
                // so a later import re-evaluates rather than seeing a partial namespace.
                self.module_cache.remove(&path);
                self.module_namespaces.remove(&ns_idx);
                Err(e)
            }
        }
    }

    /// Resolve a module's re-exports into `full` (the export name→slot list). Split out
    /// of `import_module` so the in-progress (`module_own`) marker is cleaned up by the
    /// caller on every exit path. A name an `export {x} from` dependency doesn't export
    /// (incl. a circular chain that never grounds) is a link-time SyntaxError per
    /// ResolveExport. `export *` copies the dependency's exports except `default`; a
    /// name supplied by TWO different star sources is AMBIGUOUS — excluded from the
    /// namespace and a SyntaxError to resolve by name (the second tuple element).
    fn link_module_reexports(
        &mut self,
        mut full: Vec<(String, u32)>,
        reexports: &[(String, String, String)],
        star_reexports: &[String],
        ns_reexports: &[(String, String)],
        dir: Option<&std::path::Path>,
    ) -> Result<(Vec<(String, u32)>, std::collections::HashSet<String>), Thrown> {
        // `export * as name from`: import the dependency and export its
        // NAMESPACE object under `name` (a fresh runtime global slot holds the
        // value; `self.globals` is a GC root). Cycles resolve through the
        // loader's pre-registered cache entry.
        for (exported, spec) in ns_reexports {
            let dep = match dir {
                Some(d) => d.join(spec),
                None => std::path::PathBuf::from(spec),
            };
            let ns = self.import_module_sync(&dep, None)?;
            self.globals.push(ns);
            let slot = (self.globals.len() - 1) as u32;
            full.push((exported.clone(), slot));
        }
        for (exported, imported, spec) in reexports {
            let dep = match dir {
                Some(d) => d.join(spec),
                None => std::path::PathBuf::from(spec),
            };
            if let Some(slot) = self.resolve_export(&dep, imported)? {
                full.push((exported.clone(), slot));
            } else {
                return Err(Thrown(format!(
                    "SyntaxError: The requested module '{spec}' does not provide an export named '{imported}'"
                )));
            }
        }
        let own: std::collections::HashSet<String> =
            full.iter().map(|(n, _)| n.clone()).collect();
        let mut star_seen: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        let mut ambiguous: std::collections::HashSet<String> = std::collections::HashSet::new();
        for spec in star_reexports {
            let dep = match dir {
                Some(d) => d.join(spec),
                None => std::path::PathBuf::from(spec),
            };
            for (name, slot) in self.all_exports(&dep)? {
                if own.contains(&name) {
                    continue; // a local/indirect export shadows star names
                }
                match star_seen.get(&name) {
                    Some(&prev) if prev != slot => {
                        ambiguous.insert(name);
                    }
                    Some(_) => {}
                    None => {
                        star_seen.insert(name.clone(), slot);
                        full.push((name, slot));
                    }
                }
            }
        }
        if !ambiguous.is_empty() {
            full.retain(|(n, _)| !ambiguous.contains(n));
        }
        Ok((full, ambiguous))
    }

    /// Resolve a single exported `name` from the module at `raw_path` to its live
    /// global slot (for `export … from` linking). Consults the namespace cache, then
    /// the in-progress own-exports map (cycle break), else recursively loads it.
    /// `Ok(None)` if the module doesn't export `name`.
    fn resolve_export(
        &mut self,
        raw_path: &std::path::Path,
        name: &str,
    ) -> Result<Option<u32>, Thrown> {
        let dep = std::fs::canonicalize(raw_path)
            .map_err(|_| Thrown("TypeError: module not found".into()))?;
        let ambiguous_check = |vm: &Self, ns_idx: u32| -> Result<(), Thrown> {
            if vm.module_ambiguous.get(&ns_idx).is_some_and(|s| s.contains(name)) {
                return Err(Thrown(format!(
                    "SyntaxError: The requested module contains conflicting star exports for name '{name}'"
                )));
            }
            Ok(())
        };
        if let Some(&ns) = self.module_cache.get(&dep) {
            ambiguous_check(self, ns.heap_index())?;
            if let Some(slot) =
                self.module_namespaces.get(&ns.heap_index()).and_then(|m| m.get(name).copied())
            {
                return Ok(Some(slot));
            }
            // IN-FLIGHT module (pre-registered, link incomplete): its
            // `export … from` entries haven't resolved into the namespace —
            // walk them statically (spec ResolveExport through a cycle; a
            // chain that never grounds yields None → the requester's
            // SyntaxError).
            if self.module_pending_reexports.contains_key(&dep) {
                let mut seen = std::collections::HashSet::new();
                return self.resolve_pending_export(&dep, name, &mut seen);
            }
            return Ok(None);
        }
        if let Some(m) = self.module_own.get(&dep) {
            return Ok(m.get(name).copied());
        }
        let ns = self.import_module_sync(&dep, None)?;
        ambiguous_check(self, ns.heap_index())?;
        Ok(self
            .module_namespaces
            .get(&ns.heap_index())
            .and_then(|m| m.get(name).copied()))
    }

    /// The spec's LOADING phase for a module that is never evaluated (a phase
    /// import's target): read + parse it and resolve its requested specifiers
    /// recursively — an unreadable file is a host TypeError, a parse failure a
    /// SyntaxError. Already-cached / already-seen modules are done.
    fn prescan_module_requests(
        &mut self,
        path: &std::path::PathBuf,
        seen: &mut std::collections::HashSet<std::path::PathBuf>,
    ) -> Result<(), Thrown> {
        if !seen.insert(path.clone()) || self.module_cache.contains_key(path) {
            return Ok(());
        }
        let code = std::fs::read_to_string(path).map_err(|_| {
            Thrown(format!(
                "TypeError: Failed to resolve module specifier '{}'",
                path.display()
            ))
        })?;
        let allocator = oxc_allocator::Allocator::default();
        let ret =
            oxc_parser::Parser::new(&allocator, &code, oxc_span::SourceType::mjs()).parse();
        if !ret.errors.is_empty() {
            return Err(Thrown(format!("SyntaxError: {}", ret.errors[0])));
        }
        let dir = path.parent().map(|p| p.to_path_buf());
        for s in &ret.program.body {
            use oxc_ast::ast::Statement as S;
            let spec: Option<String> = match s {
                S::ImportDeclaration(d) => Some(d.source.value.to_string()),
                S::ExportNamedDeclaration(e) => {
                    e.source.as_ref().map(|src| src.value.to_string())
                }
                S::ExportAllDeclaration(e) => Some(e.source.value.to_string()),
                _ => None,
            };
            if let Some(spec) = spec {
                let raw = match dir.as_deref() {
                    Some(d) => d.join(&spec),
                    None => std::path::PathBuf::from(&spec),
                };
                if std::fs::metadata(&raw).is_err() {
                    return Err(Thrown(format!(
                        "TypeError: Failed to resolve module specifier '{spec}'"
                    )));
                }
                let canon = std::fs::canonicalize(&raw).unwrap_or(raw);
                self.prescan_module_requests(&canon, seen)?;
            }
        }
        Ok(())
    }

    /// ResolveExport through modules whose link is IN FLIGHT: own bindings
    /// resolve directly; `export {x as y} from` / `export *` entries recurse.
    /// `seen` is the spec's resolveSet — a repeated (module, name) request is
    /// a circular chain that never grounds → None.
    fn resolve_pending_export(
        &mut self,
        dep: &std::path::PathBuf,
        name: &str,
        seen: &mut std::collections::HashSet<(std::path::PathBuf, String)>,
    ) -> Result<Option<u32>, Thrown> {
        if !seen.insert((dep.clone(), name.to_string())) {
            return Ok(None);
        }
        if let Some(slot) = self.module_own.get(dep).and_then(|m| m.get(name).copied()) {
            return Ok(Some(slot));
        }
        let Some((reex, stars, pdir)) = self.module_pending_reexports.get(dep).cloned() else {
            // Completed (or never in-flight): normal resolution.
            return self.resolve_export(dep, name);
        };
        let join = |pdir: Option<&std::path::Path>, spec: &str| -> std::path::PathBuf {
            let raw = match pdir {
                Some(d) => d.join(spec),
                None => std::path::PathBuf::from(spec),
            };
            std::fs::canonicalize(&raw).unwrap_or(raw)
        };
        for (exported, imported, spec) in &reex {
            if exported == name {
                let target = join(pdir.as_deref(), spec);
                return self.resolve_pending_export(&target, imported, seen);
            }
        }
        if name != "default" {
            for spec in &stars {
                let target = join(pdir.as_deref(), spec);
                if let Some(slot) = self.resolve_pending_export(&target, name, seen)? {
                    return Ok(Some(slot));
                }
            }
        }
        Ok(None)
    }

    /// Enumerate all exports of the module at `raw_path` (excluding `default`) as
    /// (name, live slot), for `export * from`. Uses the cache / in-progress map, else
    /// recursively loads it.
    fn all_exports(
        &mut self,
        raw_path: &std::path::Path,
    ) -> Result<Vec<(String, u32)>, Thrown> {
        let dep = std::fs::canonicalize(raw_path)
            .map_err(|_| Thrown("TypeError: module not found".into()))?;
        let collect = |m: &std::collections::HashMap<String, u32>| -> Vec<(String, u32)> {
            m.iter()
                .filter(|(n, _)| n.as_str() != "default")
                .map(|(n, s)| (n.clone(), *s))
                .collect()
        };
        if let Some(&ns) = self.module_cache.get(&dep) {
            return Ok(self
                .module_namespaces
                .get(&ns.heap_index())
                .map(collect)
                .unwrap_or_default());
        }
        if let Some(m) = self.module_own.get(&dep) {
            return Ok(collect(m));
        }
        let ns = self.import_module_sync(&dep, None)?;
        Ok(self
            .module_namespaces
            .get(&ns.heap_index())
            .map(collect)
            .unwrap_or_default())
    }

    /// Install a compiled eval/module Program into the live realm (remap its global
    /// slots, function ids, and class ids onto the running tables; hoist `var`s and
    /// top-level functions) and run its top-level function to completion, returning
    /// the completion value.
    /// Phases 1-5: remap the program's global slots onto live slots (the `gmap`),
    /// install its functions + classes, and hoist vars/functions — WITHOUT running
    /// the top-level body. Returns `(gmap, base_func)`; the caller runs the body via
    /// `execute_eval_program` (split out so a module can register its namespace in the
    /// loader cache between prepare and execute, for self/cyclic imports).
    fn prepare_eval_program(
        &mut self,
        eval_prog: crate::bytecode::Program,
        module: bool,
        caller_home: Option<u32>,
        var_env_global: bool,
        eval_scope_idx: Option<u32>,
        import_aliases: Option<&std::collections::HashMap<u32, u32>>,
        self_aliases: Option<&std::collections::HashMap<u32, u32>>,
        prealloc: Option<&std::collections::HashMap<u32, u32>>,
    ) -> Result<(Vec<u32>, u32), Thrown> {
        use crate::bytecode::{FuncProto, Instr};
        // Runtime base ids: eval functions and classes are appended past the
        // compile-time tables (parallel to global slots).
        let base_func = (self.main_func_count + self.eval_funcs.len()) as u32;
        let base_class = (self.main_class_count + self.eval_classes.len()) as u32;
        // 3. Remap the eval program's own global-slot numbering onto live slots.
        //    For a MODULE, each slot it DECLARES (var/let/const/function/class +
        //    `*default*`) draws a FRESH per-module slot so two modules' same-named
        //    exports don't collide — the foundation for correct live bindings. A
        //    free reference (builtin or import) still resolves realm-shared by name.
        let decl: std::collections::HashSet<u32> = if module {
            eval_prog.module_decl_globals.iter().copied().collect()
        } else {
            std::collections::HashSet::new()
        };
        let cap = self.program.global_count + (FIELD_POOL + EVAL_POOL) as u32;
        let mut gmap: Vec<u32> = Vec::with_capacity(eval_prog.global_names.len());
        for (i, name) in eval_prog.global_names.iter().enumerate() {
            // A static-import LOCAL aliases the dependency's resolved live
            // export slot — sharing one flat slot IS the live binding.
            if let Some(&alias) = import_aliases.and_then(|m| m.get(&(i as u32))) {
                gmap.push(alias);
                continue;
            }
            if module && decl.contains(&(i as u32)) {
                // A slot the module loader PRE-ALLOCATED (for cyclic re-export
                // resolution) is reused — it is already UNINITIALIZED.
                if let Some(&pre) = prealloc.and_then(|m| m.get(&(i as u32))) {
                    gmap.push(pre);
                    continue;
                }
                if self.eval_global_next >= cap {
                    return Err(Thrown(
                        "EvalError: too many distinct globals introduced by eval".into(),
                    ));
                }
                let s = self.eval_global_next;
                self.eval_global_next += 1;
                self.globals[s as usize] = Value::UNINITIALIZED;
                gmap.push(s);
            } else {
                gmap.push(self.eval_global_slot(name)?);
            }
        }
        // Second pass: a SELF-import local aliases the module's own exported
        // local — both compile slots map to ONE live slot (live binding).
        if let Some(sa) = self_aliases {
            for (&import_local, &target) in sa {
                let live = gmap[target as usize];
                gmap[import_local as usize] = live;
            }
        }
        // 4. Install eval classes: re-index their member func ids (which point into
        //    the eval functions installed below) by base_func, leak each ClassDef,
        //    and reserve a class_values slot per class (MakeClass writes it). A
        //    ClassDef holds no class-id references, so only func ids are offset.
        for mut cd in eval_prog.classes {
            if let Some(c) = cd.ctor.as_mut() {
                *c += base_func;
            }
            for lst in [
                &mut cd.methods,
                &mut cd.getters,
                &mut cd.setters,
                &mut cd.statics,
                &mut cd.static_getters,
                &mut cd.static_setters,
            ] {
                for (_, fid) in lst.iter_mut() {
                    *fid += base_func;
                }
            }
            self.eval_classes.push(Box::leak(Box::new(cd)));
            self.class_values.push(None);
        }
        // 5. Re-index function-id, global-slot, and class-id operands, leak each
        //    FuncProto (stable address — raw pointers live into it), append.
        let mut new_funcs: Vec<&'static FuncProto> =
            Vec::with_capacity(eval_prog.functions.len());
        for mut f in eval_prog.functions {
            for ins in f.code.iter_mut() {
                match ins {
                    Instr::MakeFunc { func_id, .. }
                    | Instr::MakeClosure { func_id, .. }
                    | Instr::MakeArrow { func_id, .. } => {
                        *func_id += base_func;
                    }
                    Instr::LoadGlobal { idx, .. }
                    | Instr::LoadGlobalOrUndefined { idx, .. }
                    | Instr::StoreGlobal { idx, .. }
                    | Instr::StoreGlobalStrict { idx, .. }
                    | Instr::LoadGlobalDyn { idx, .. }
                    | Instr::LoadGlobalOrUndefinedDyn { idx, .. }
                    | Instr::StoreGlobalDyn { idx, .. } => {
                        *idx = gmap[*idx as usize];
                    }
                    // Class-id operands: the class itself, and every `super`
                    // reference (which names its home class).
                    Instr::MakeClass { class_id, .. } => *class_id += base_class,
                    Instr::SuperCtor { home_class_id, .. }
                    | Instr::SuperCtorSpread { home_class_id, .. }
                    | Instr::SuperMethod { home_class_id, .. }
                    | Instr::SuperGet { home_class_id, .. }
                    | Instr::SuperGetComputed { home_class_id, .. }
                    | Instr::SuperMethodComputed { home_class_id, .. }
                    | Instr::SuperSet { home_class_id, .. }
                    | Instr::SuperSetComputed { home_class_id, .. } => {
                        // The SENTINEL marks "the eval caller's home class": swap
                        // in its RUNTIME class id (already absolute); real ids
                        // shift past the main program's class table.
                        if *home_class_id == u32::MAX {
                            if let Some(h) = caller_home {
                                *home_class_id = h;
                            }
                        } else {
                            *home_class_id += base_class;
                        }
                    }
                    Instr::DirectEval { home_class, .. } => {
                        if *home_class == u32::MAX {
                            if let Some(h) = caller_home {
                                *home_class = h;
                            }
                        } else {
                            *home_class += base_class;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(s) = f.name_global {
                f.name_global = Some(gmap[s as usize] as u16);
            }
            new_funcs.push(Box::leak(Box::new(f)));
        }
        for r in new_funcs {
            self.eval_funcs.push(r);
        }
        // EvalDeclarationInstantiation step 5.a: a sloppy eval may not
        // var/function-declare a name that is a GLOBAL lexical (let/const) —
        // SyntaxError BEFORE any binding is created.
        let count = self.eval_funcs.len();
        let start = (base_func as usize) - self.main_func_count;
        if var_env_global {
            let mut lex_clash: Option<String> = None;
            for &slot in &eval_prog.hoisted_globals {
                let rs = gmap[slot as usize];
                if self.program.lexical_globals.contains(&rs) {
                    lex_clash = self.global_slot_name(rs);
                    break;
                }
            }
            if lex_clash.is_none() {
                for local in start..count {
                    if let Some(slot) = self.eval_funcs[local].name_global {
                        if self.program.lexical_globals.contains(&(slot as u32)) {
                            lex_clash = self.global_slot_name(slot as u32);
                            break;
                        }
                    }
                }
            }
            if let Some(name) = lex_clash {
                return Err(Thrown(format!(
                    "SyntaxError: Identifier '{name}' has already been declared"
                )));
            }
            // CanDeclareGlobalVar: an ABSENT binding can only be created while
            // the global object is extensible — else TypeError (before any
            // binding is created).
            let global_extensible = self.global_this == 0
                || matches!(self.heap.get(self.global_this), HeapObj::Object(m) if m.extensible);
            if !global_extensible {
                for &slot in &eval_prog.hoisted_globals {
                    let rs = gmap[slot as usize];
                    if self.globals[rs as usize].bits() != Value::UNINITIALIZED.bits() {
                        continue;
                    }
                    if let Some(name) = self.global_slot_name(rs) {
                        let has_own = matches!(
                            self.heap.get(self.global_this),
                            HeapObj::Object(m) if m.pos(&name).is_some()
                        );
                        if !has_own && self.global_by_name(&name).is_none() {
                            return Err(Thrown(format!(
                                "TypeError: cannot declare global variable {name}"
                            )));
                        }
                    }
                }
            }
        }
        // Validation pass: CanDeclareGlobalFunction for EVERY function
        // name before ANY binding (var or function) is created — a later
        // non-definable function must leave earlier vars/functions undeclared.
        if var_env_global {
            for local in start..count {
                if let Some(slot) = self.eval_funcs[local].name_global {
                    if (slot as usize) >= self.globals.len()
                        || self.globals[slot as usize].bits() != Value::UNINITIALIZED.bits()
                    {
                        continue;
                    }
                    if let Some(name) = self.global_slot_name(slot as u32) {
                        if matches!(name.as_str(), "NaN" | "Infinity" | "undefined") {
                            return Err(Thrown(format!(
                                "TypeError: cannot declare global function {name}"
                            )));
                        }
                        if self.global_this != 0 {
                            let pos_attrs = match self.heap.get(self.global_this) {
                                HeapObj::Object(m) => m.pos(&name).map(|i| m.attrs[i]),
                                _ => None,
                            };
                            if let Some(a) = pos_attrs {
                                if !a.configurable
                                    && (a.accessor || !a.writable || !a.enumerable)
                                {
                                    return Err(Thrown(format!(
                                        "TypeError: cannot declare global function {name}"
                                    )));
                                }
                            }
                        }
                    }
                }
            }
        }
        // 5. CreateGlobalVarBinding for eval `var` names: an ABSENT binding
        // becomes an own {writable, enumerable, CONFIGURABLE} property of the
        // global object (eval-created bindings are deletable and reflectable);
        // the slot stays UNINITIALIZED so reads/writes route through the own
        // prop (the Load/StoreGlobal fallbacks). Existing bindings untouched.
        // FUNCTION-context dynamic names: CreateMutableBinding(undefined) in
        // the caller's EvalScope (functions get their values in step 6).
        if let Some(sc) = eval_scope_idx {
            let names: Vec<String> = eval_prog.eval_dynamic_names.clone();
            if let HeapObj::EvalScope(m) = self.heap.get_mut(sc) {
                for n in names {
                    m.entry(n).or_insert(Value::UNDEFINED);
                }
            }
        }
        for &slot in &eval_prog.hoisted_globals {
            let rs = gmap[slot as usize] as usize;
            if self.globals[rs].bits() == Value::UNINITIALIZED.bits() {
                let mut own_backed = false;
                if var_env_global {
                    if let Some(name) = self.global_slot_name(rs as u32) {
                        // A builtin binding of this name already exists — leave it.
                        if self.global_by_name(&name).is_some() {
                            continue;
                        }
                        if self.global_this != 0 {
                            let gi = self.global_this;
                            let has_own = matches!(
                                self.heap.get(gi),
                                HeapObj::Object(m) if m.pos(&name).is_some()
                            );
                            if has_own {
                                own_backed = true;
                            } else if let HeapObj::Object(m) = self.heap.get_mut(gi) {
                                m.define(
                                    &name,
                                    Value::UNDEFINED,
                                    crate::heap::PropAttr {
                                        writable: true,
                                        enumerable: true,
                                        configurable: true,
                                        accessor: false,
                                        setter: Value::UNDEFINED,
                                    },
                                );
                                own_backed = true;
                            }
                        }
                    }
                }
                if !own_backed {
                    self.globals[rs] = Value::UNDEFINED;
                }
            }
        }
        // 6. CreateGlobalFunctionBinding for eval top-level function decls:
        // when the slot is uninitialized, the binding lives as a global-object
        // own property — absent: define {w, e, configurable: true}; existing
        // configurable: redefine to that shape with the new value; existing
        // non-configurable: write the value, keep the attributes. An
        // initialized slot (a main-program binding) is written directly.
        for local in start..count {
            let global_id = (self.main_func_count + local) as u32;
            if let Some(slot) = self.eval_funcs[local].name_global {
                let v = Value::heap(self.heap.alloc(HeapObj::Func(global_id)));
                if (slot as usize) >= self.globals.len() {
                    continue;
                }
                // A dynamic (EvalScope) function: bind in the caller's scope,
                // stamp the scope on the value so its body resolves siblings.
                if let Some(sc) = eval_scope_idx {
                    if let Some(name) = self.global_slot_name(slot as u32) {
                        if eval_prog.eval_dynamic_names.iter().any(|n| *n == name) {
                            self.closure_eval_scope.insert(v.heap_index(), sc);
                            if let HeapObj::EvalScope(m) = self.heap.get_mut(sc) {
                                m.insert(name, v);
                            }
                            continue;
                        }
                    }
                }
                if var_env_global
                    && self.globals[slot as usize].bits() == Value::UNINITIALIZED.bits()
                {
                    if let Some(name) = self.global_slot_name(slot as u32) {
                        if self.global_this != 0 {
                            let gi = self.global_this;
                            let attr = crate::heap::PropAttr {
                                writable: true,
                                enumerable: true,
                                configurable: true,
                                accessor: false,
                                setter: Value::UNDEFINED,
                            };
                            if let HeapObj::Object(m) = self.heap.get_mut(gi) {
                                if let Some(i) = m.pos(&name) {
                                    if m.attrs[i].configurable {
                                        m.attrs[i] = attr;
                                    }
                                    m.vals[i] = v;
                                } else {
                                    m.define(&name, v, attr);
                                }
                                continue;
                            }
                        }
                    }
                }
                self.globals[slot as usize] = v;
            }
        }
        Ok((gmap, base_func))
    }

    /// Phase 6: run a prepared eval/module top-level function (`base_func`) to
    /// completion, returning its completion value. `this_override` is the caller's
    /// `this` for a DIRECT eval; otherwise the top level runs with `this` = globalThis.
    fn execute_eval_program(
        &mut self,
        base_func: u32,
        this_override: Option<Value>,
        caller_chain: Option<Vec<u64>>,
        caller_new_target: Value,
        caller_home_obj: Option<Value>,
        caller_cells: Option<Vec<Value>>,
        eval_scope_idx: Option<u32>,
    ) -> Result<Value, Thrown> {
        // With caller bindings, the eval script is a CLOSURE over their cells
        // (UpvalGet/UpvalSet in the eval code address them directly).
        let script = match caller_cells {
            Some(cells) => {
                let ups: Vec<u32> = cells.iter().map(|v| v.heap_index()).collect();
                Value::heap(self.heap.alloc(HeapObj::Closure {
                    func: base_func,
                    upvalues: ups,
                    this_val: Value::UNDEFINED,
                }))
            }
            None => Value::heap(self.heap.alloc(HeapObj::Func(base_func))),
        };
        // A direct eval's code resolves the CALLER's private brand chain
        // (frame.callee = this script value).
        if let Some(ch) = caller_chain {
            self.method_brand.insert(script.heap_index(), ch);
        }
        // Object-method direct eval: super.x resolves via the caller's
        // [[HomeObject]] (same stamp pattern as the brand chain above).
        if let Some(home) = caller_home_obj {
            self.closure_home.insert(script.heap_index(), home);
        }
        // The eval frame resolves the caller's dynamic EvalScope through
        // the same stamp the Dyn ops use for closures.
        if let Some(sc) = eval_scope_idx {
            self.closure_eval_scope.insert(script.heap_index(), sc);
        }
        // The eval frame's new.target is the CALLER's (consumed at frame setup).
        self.pending_new_target = caller_new_target;
        let this = this_override.unwrap_or_else(|| {
            if self.global_this != 0 {
                Value::heap(self.global_this)
            } else {
                Value::UNDEFINED
            }
        });
        self.call_value(script, this, &[])
    }

    /// Install a compiled eval/module program and run its top-level body, returning
    /// `(completion, gmap)`. (prepare + execute; modules that need namespace
    /// pre-registration call the two halves directly — see `import_module`.)
    fn run_eval_program(
        &mut self,
        eval_prog: crate::bytecode::Program,
        this_override: Option<Value>,
        module: bool,
        caller_home: Option<u32>,
        caller_chain: Option<Vec<u64>>,
        caller_new_target: Value,
        caller_home_obj: Option<Value>,
        var_env_global: bool,
        caller_cells: Option<Vec<Value>>,
        eval_scope_idx: Option<u32>,
    ) -> Result<(Value, Vec<u32>), Thrown> {
        let (gmap, base_func) =
            self.prepare_eval_program(
                eval_prog,
                module,
                caller_home,
                var_env_global,
                eval_scope_idx,
                None,
                None,
                None,
            )?;
        let completion = self.execute_eval_program(
            base_func,
            this_override,
            caller_chain,
            caller_new_target,
            caller_home_obj,
            caller_cells,
            eval_scope_idx,
        )?;
        Ok((completion, gmap))
    }
}
