// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

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
            mi_class_epoch: 0,
            mi_recv: rustc_hash::FxHashMap::default(),
            idx_key_scratch: String::new(),
            json_default_tj: None,
            site_ics: Vec::new(),
            heap,
            globals,
            regs: Vec::new(),
            frames: Vec::new(),
            #[cfg(feature = "instrument")]
            instr_rec: None,
            #[cfg(feature = "instrument")]
            jit_steps: 0,
            output: Vec::new(),
            errput: Vec::new(),
            host: None,
            start_mono_ms: crate::vm::clock::now_mono_ms(),
            pending_throw: None,
            pending_new_target: Value::UNDEFINED,
            pending_eval_frame: false,
            pending_fn_ctor_eval: false,
            pending_yield: None,
            pending_yield_handlers: Vec::new(),
            pending_yield_eval_scope: u32::MAX,
            pending_yield_raw: false,
            pending_await: None,
            cap_capture: None,
            microtasks: std::collections::VecDeque::new(),
            template_raws: std::collections::HashMap::new(),
            template_cache: std::collections::HashMap::new(),
            jit_shape_slot: rustc_hash::FxHashMap::default(),
            for_in_barren: rustc_hash::FxHashMap::default(),
            regexp_string_iters: rustc_hash::FxHashMap::default(),
            regexp_last: Vec::new(),
            typeof_strs: [Value::UNDEFINED; 8],
            regexp_last_lazy: None,
            run_loop_depth: 0,
            tail_reuse_streak: 0,
            regexp_exact_source: std::collections::HashMap::new(),
            regex_compile_cache: rustc_hash::FxHashMap::default(),
            collection_index: rustc_hash::FxHashMap::default(),
            next_private_brand: 1,
            method_brand: std::collections::HashMap::new(),
            instance_brand: std::collections::HashMap::new(),
            brand_private_names: std::collections::HashMap::new(),
            brand_owner: std::collections::HashMap::new(),
            prototypes: std::collections::HashMap::new(),
            proto_of: crate::slot_table::SlotTable::default(),
            ctor_field_hint: Vec::new(),
            fn_props: crate::slot_table::SlotTable::default(),
            arr_props: crate::slot_table::SlotTable::default(),
            regexp_result_props: crate::slot_table::SlotTable::default(),
            ab_max: std::collections::HashMap::new(),
            ta_tracking: std::collections::HashSet::new(),
            async_activations: Vec::new(),
            dv_tracking: std::collections::HashSet::new(),
            deleted_callable_intrinsics: std::collections::HashSet::new(),
            deleted_globals: std::collections::HashSet::new(),
            global_route_epoch: 0,
            jit_global_route_ok: rustc_hash::FxHashMap::default(),
            strict_unresolvable_globals: Vec::new(),
            array_length_nonwritable: std::collections::HashSet::new(),
            array_js_len: crate::slot_table::SlotTable::default(),
            array_proto_has_index: false,
            super_called: std::collections::HashSet::new(),
            this_tdz: std::collections::HashSet::new(),
            super_this: std::collections::HashMap::new(),
            private_fields: std::collections::HashMap::new(),
            eval_fn_idx: 0,
            closure_eval_scope: std::collections::HashMap::new(),
            eval_scope_parent: std::collections::HashMap::new(),
            module_ambiguous: std::collections::HashMap::new(),
            module_ns_slots: std::collections::HashMap::new(),
            module_source_slots: std::collections::HashMap::new(),
            module_metas: std::collections::HashMap::new(),
            module_func_ranges: Vec::new(),
            import_meta: 0,
            can_block: std::env::var("ZIPP_CAN_BLOCK").map_or(true, |v| v != "0"),
            module_loading: std::collections::HashSet::new(),
            pending_module_body: None,
            module_body_promise: std::collections::HashMap::new(),
            pending_module_body_marker: false,
            module_body_results: std::collections::HashSet::new(),
            link_pending_deps: Vec::new(),
            deferred_ns_cache: std::collections::HashMap::new(),
            deferred_ns_state: std::collections::HashMap::new(),
            executing_modules: std::collections::HashSet::new(),
            module_errors: std::collections::HashMap::new(),
            active_realm: None,
            realm_globals: std::collections::HashMap::new(),
            realm_global_objs: std::collections::HashMap::new(),
            realm_fns: std::collections::HashMap::new(),
            native_callee_realm: None,
            realm_throw_type_errors: std::collections::HashMap::new(),
            async_waiters: Vec::new(),
            timer_queue: Vec::new(),
            vm_start_mono_ms: crate::vm::clock::now_mono_ms(),
            agent_shared: None,
            agent_role: agents::AgentRole::Main,
            broadcast_cb: Value::UNDEFINED,
            mailbox: std::sync::Arc::new(agents::Mailbox::default()),
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
            ta_ctors: [0; 12],
            ta_protos: [0; 12],
            arraybuffer_ctor: 0,
            arraybuffer_proto: 0,
            dataview_ctor: 0,
            dataview_proto: 0,
            sab_ctor: 0,
            sab_proto: 0,
            shared_buffers: std::collections::HashSet::new(),
            immutable_buffers: std::collections::HashSet::new(),
            error_data: std::collections::HashSet::new(),
            fn_name_cells: std::collections::HashSet::new(),
            const_cells: std::collections::HashSet::new(),
            typed_module_cache: std::collections::HashMap::new(),
            pending_gen_callee: Value::UNDEFINED,
            gen_callee: std::collections::HashMap::new(),
            eval_script_gdi: false,
            eval_lexical_globals: std::collections::HashSet::new(),
            eval_const_globals: std::collections::HashSet::new(),
            eval_var_globals: std::collections::HashSet::new(),
            eval_prelude_mode: false,
            arguments_objs: crate::slot_table::SlotTable::default(),
            gen_args_obj: std::collections::HashMap::new(),
            module_base_dir: None,
            module_cache: std::collections::HashMap::new(),
            module_namespaces: std::collections::HashMap::new(),
            module_own: std::collections::HashMap::new(),
            closure_home: std::collections::HashMap::new(),
            closure_new_target: std::collections::HashMap::new(),
            from_async_fn: None,
            async_dispose_fn: None,
            sync_dispose_shim_fn: None,
            using_resources: std::collections::HashMap::new(),
            using_next_id: 0,
            weakref_ctor: 0,
            finreg_ctor: 0,
            weakmap_ctor: 0,
            weakset_ctor: 0,
            disposablestack_ctor: 0,
            disposablestack_proto: 0,
            dispose_stacks: std::collections::HashMap::new(),
            dispose_async_state: std::collections::HashMap::new(),
            asyncdisposablestack_ctor: 0,
            asyncdisposablestack_proto: 0,
            async_stacks: std::collections::HashSet::new(),
            suppressederror_ctor: 0,
            suppressederror_proto: 0,
            shadowrealm_ctor: 0,
            shadowrealm_proto: 0,
            shadow_realms: std::collections::HashSet::new(),
            shadow_fn_realm: std::collections::HashMap::new(),
            abstractmodulesource_ctor: 0,
            abstractmodulesource_proto: 0,
            resolver_pair_next: 1,
            resolved_pairs: std::collections::HashSet::new(),
            promise_ctor_intrinsic: 0,
            promise_then_intrinsic: 0,
            promise_pristine_slots: None,
            matchall_fast_slots: None,
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
            temporal_cal: std::collections::HashMap::new(),
            intl_ns: 0,
            intl_ctors: [0; crate::vm::native::INTL_KINDS],
            intl_protos: [0; crate::vm::native::INTL_KINDS],
            intl_fallback_syms: std::collections::HashMap::new(),
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
            host_262: true,
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
            jit_call_depth: 0,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            osr_deopt_exempt: false,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_const_strings: Vec::new(),
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            mi_cache: Vec::new(),
            // The sentinel version never equals a live object's, so the first
            // use recomputes against the real `%Function.prototype%`.
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            ci_pristine: (u32::MAX, u32::MAX, u32::MAX),
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

    /// Whether the fused array kernels and the off-frame method inliner may
    /// run.
    ///
    /// Both execute user code natively or in Rust with no interpreter loop and
    /// no VM pointer to charge against — the fused kernels take neither
    /// (`codegen::kernels`), and `try_method_inline` evaluates a callee body
    /// off-frame entirely outside `run_loop`. The compiled-code metering does
    /// not reach either of them, so a metered VM declines them rather than
    /// leaving a native path a script can spend unbounded work in. Both are
    /// throughput optimisations; correctness never depended on them.
    #[inline]
    pub(crate) fn jit_fused_ok(&self) -> bool {
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        {
            #[cfg(feature = "instrument")]
            if self.jit.metered() {
                return false;
            }
            self.jit_enabled
        }
        #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
        {
            false
        }
    }

    /// Approximate resident heap in bytes — see `embed::ScriptState::heap_bytes`.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.heap.len() * std::mem::size_of::<crate::heap::HeapObj>()
    }

    /// Force the JIT on/off (overrides the `ZIPP_NOJIT` default). Used by the
    /// test suite to run a program both ways and assert the outputs match.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[allow(dead_code)] // used by the differential test harness (run_nojit)
    pub(crate) fn set_jit_enabled(&mut self, on: bool) {
        self.jit_enabled = on;
    }

    /// Attach a step budget / abort flag / trace recorder to this VM.
    ///
    /// The JIT stays ON. Compiled code charges the budget itself — once per
    /// basic block, by that block's exact instruction count, against
    /// `Vm::jit_steps` (see `codegen::meter`) — so a native loop is bounded by
    /// the same counter the interpreter uses. Code compiled BEFORE this call
    /// carries no charge, so `Jit::set_meter` throws it away.
    ///
    /// Tracing is the exception, and `enter_trace_mode` below is where the JIT
    /// actually goes off: a trace has to be a complete instruction-by-instruction
    /// record, and native code produces no rows at all.
    #[cfg(feature = "instrument")]
    pub(crate) fn set_instrumentation(&mut self, rec: super::super::instrument::Recorder) {
        self.instr_rec = Some(Box::new(rec));
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        {
            // Safe to read now that `instr_rec` is set: the offset is relative to
            // the VM pointer, which every compiled body is handed afresh on each
            // native entry, so it survives this `Vm` being moved.
            if let Some(off) = self.meter_offset() {
                self.jit.set_meter(crate::codegen::meter::Meter { steps_off: off });
            }
        }
    }

    /// Switch the JIT off for this VM's lifetime, because a trace is being
    /// recorded.
    ///
    /// `try_run_jit` runs a whole function activation natively and `try_run_osr`
    /// a whole loop region; neither iterates the interpreter loop the trace hook
    /// lives in. A JIT'd hot loop would therefore leave NO rows behind while the
    /// program still returned the right answer — a proof over that trace would
    /// attest to an execution that never happened. Metering can be made to work
    /// natively (it is a counter); a trace cannot.
    #[cfg(feature = "instrument")]
    pub(crate) fn enter_trace_mode(&mut self) {
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        {
            self.jit_enabled = false;
        }
    }
}
