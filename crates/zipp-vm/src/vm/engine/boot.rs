// Split out of the former monolithic parent file by tools/split_rs.py.
// Pure code move: `use super::*` keeps the parent module's imports in
// scope, and items are widened to pub(crate) so the pieces still see
// each other. No logic changed.
#![allow(unused_imports)]
use super::*;

#[inline]
fn module_func_range_contains(ranges: &[(u32, u32, u32)], func_id: u32) -> bool {
    ranges
        .binary_search_by(|&(start, end, _)| {
            if func_id < start {
                std::cmp::Ordering::Greater
            } else if func_id >= end {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

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

    /// Whether `func_id` belongs to a loader-installed ES module rather than
    /// to an ordinary `eval` / `new Function` program. Both kinds share the
    /// runtime function table, so every optimization that accepts immutable
    /// module code must use these exact loader-recorded ranges as its boundary.
    #[inline]
    pub(crate) fn loader_module_func(&self, func_id: u32) -> bool {
        module_func_range_contains(&self.module_func_ranges, func_id)
    }

    /// Whether `func_id` belongs to immutable code the native tiers may compile.
    ///
    /// Main-program functions have always been eligible. Loader-installed ES
    /// modules are eligible too: `prepare_eval_program` leaks their protos at
    /// stable addresses and `module_func_ranges` records the exact half-open id
    /// range for each module. Ordinary `eval`/`new Function` can be interleaved
    /// in the same `eval_funcs` table, so accepting every id past
    /// `main_func_count` would be unsound; only a recorded module range passes.
    #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
    #[inline]
    pub(crate) fn jit_func_eligible(&self, func_id: u32) -> bool {
        if (func_id as usize) < self.main_func_count {
            return true;
        }
        if !jit_module_functions_enabled() {
            return false;
        }

        // Ranges are appended as the monotonically-growing runtime function
        // table is installed. Failed eval/module preparations can leave gaps,
        // but cannot reorder later ids, so binary search is valid and avoids a
        // module-count-linear tax at every hot backedge.
        self.loader_module_func(func_id)
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
        // Slot-generation table (same length/no-realloc contract as `globals`)
        // and the fail-closed set of slots bytecode stores can reach — the
        // complement is what `slot_guard` keying may bake a generation for.
        // Eval/Function registration extends the set at runtime (`eval_prog`).
        let global_gens = vec![0u32; globals.len()];
        let mut bytecode_stored_slots = rustc_hash::FxHashSet::default();
        for f in &program.functions {
            for ins in &f.code {
                match *ins {
                    crate::bytecode::Instr::StoreGlobal { idx, .. }
                    | crate::bytecode::Instr::StoreGlobalStrict { idx, .. }
                    | crate::bytecode::Instr::StoreGlobalResolved { idx, .. }
                    | crate::bytecode::Instr::StoreGlobalDyn { idx, .. }
                    | crate::bytecode::Instr::EvalScopeSet { idx, .. } => {
                        bytecode_stored_slots.insert(idx);
                    }
                    _ => {}
                }
            }
        }
        let _ = &mut heap;
        let (static_key_plan_sites, static_key_plan_retained_bytes) =
            crate::bytecode::static_key_plan_usage(&program.functions).unwrap_or((
                crate::bytecode::STATIC_KEY_PLAN_VM_MAX_SITES,
                crate::bytecode::STATIC_KEY_PLAN_MAX_RETAINED_BYTES,
            ));
        Vm {
            program,
            eval_funcs: Vec::new(),
            static_key_plan_sites,
            static_key_plan_retained_bytes,
            main_func_count: program.functions.len(),
            eval_classes: Vec::new(),
            main_class_count: program.classes.len(),
            eval_global_map: std::collections::HashMap::new(),
            eval_global_next: program.global_count + FIELD_POOL as u32,
            builtin_globals: std::collections::HashMap::new(),
            class_values: vec![None; program.classes.len()],
            mi_class_epoch: 0,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            mi_recv: rustc_hash::FxHashMap::default(),
            idx_key_scratch: String::new(),
            json_default_tj: None,
            site_ics: Vec::new(),
            const_string_cache: rustc_hash::FxHashMap::default(),
            const_string_cache_funcs: rustc_hash::FxHashMap::default(),
            const_string_cache_enabled: std::env::var_os("ZIPP_NO_CONST_STRING_CACHE").is_none(),
            heap,
            globals,
            global_gens,
            bytecode_stored_slots,
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
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_shape_slot: rustc_hash::FxHashMap::default(),
            for_in_barren: rustc_hash::FxHashMap::default(),
            regexp_string_iters: rustc_hash::FxHashMap::default(),
            matchall_batches: rustc_hash::FxHashMap::default(),
            matchall_caps_scratch: Vec::new(),
            matchall_flat_scratch: Vec::new(),
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            regexp_scalar_exec_pending: None,
            regexp_last: Vec::new(),
            typeof_strs: [Value::UNDEFINED; 8],
            regexp_last_lazy: None,
            run_loop_depth: 0,
            native_recursion_depth: 0,
            tail_reuse_streak: 0,
            array_stringify_active: Vec::new(),
            regexp_exact_source: std::collections::HashMap::new(),
            regex_compile_cache: rustc_hash::FxHashMap::default(),
            regex_program_audit_scratch: std::cell::RefCell::new(Vec::new()),
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
            module_root: None,
            module_max_bytes: None,
            module_read_bytes: std::collections::HashMap::new(),
            module_total_bytes: 0,
            module_load_depth: 0,
            module_cache: std::collections::HashMap::new(),
            module_namespaces: std::collections::HashMap::new(),
            module_own: std::collections::HashMap::new(),
            closure_home: ClosureHomeTable::default(),
            closure_new_target: std::collections::HashMap::new(),
            finalize_shapes: rustc_hash::FxHashMap::default(),
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
            #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
            jit: crate::codegen::Jit::new(),
            #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
            jit_enabled: std::env::var_os("ZIPP_NOJIT").is_none(),
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_recurse_depth: 0,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_call_depth: 0,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_tierc_activation: TiercActivationState::EMPTY,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_tierc_activation_stack: Vec::new(),
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
            #[cfg(not(feature = "safe-sandbox"))]
            gc_lock: 0,
            #[cfg(feature = "safe-sandbox")]
            gc_lock: std::rc::Rc::new(std::cell::Cell::new(0)),
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
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn jit_fused_ok(&self) -> bool {
        #[cfg(feature = "instrument")]
        if self.jit.metered() {
            return false;
        }
        self.jit_enabled
    }

    fn vm_core_resident_bytes(&self) -> usize {
        self.regs
            .capacity()
            .saturating_mul(std::mem::size_of::<Value>())
            .saturating_add(
                self.frames
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Frame>()),
            )
            .saturating_add(
                self.globals
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Value>()),
            )
            .saturating_add(
                self.eval_funcs
                    .capacity()
                    .saturating_mul(std::mem::size_of::<&crate::bytecode::FuncProto>()),
            )
            .saturating_add(
                self.eval_classes
                    .capacity()
                    .saturating_mul(std::mem::size_of::<&crate::bytecode::ClassDef>()),
            )
            .saturating_add(
                self.class_values
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Option<Value>>()),
            )
            .saturating_add(
                self.microtasks
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Microtask>()),
            )
            .saturating_add(
                self.pending_yield_handlers
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Handler>()),
            )
            .saturating_add(
                self.async_activations
                    .capacity()
                    .saturating_mul(std::mem::size_of::<u32>()),
            )
            .saturating_add(
                self.global_gens
                    .capacity()
                    .saturating_mul(std::mem::size_of::<u32>()),
            )
            .saturating_add(
                self.array_stringify_active
                    .capacity()
                    .saturating_mul(std::mem::size_of::<u32>()),
            )
            .saturating_add(
                self.module_func_ranges
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(u32, u32, u32)>()),
            )
            .saturating_add(
                self.link_pending_deps
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Value>()),
            )
            .saturating_add(
                self.timer_queue
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(clock::Instant, Value)>()),
            )
            .saturating_add(self.idx_key_scratch.capacity())
            .saturating_add(
                self.regex_program_audit_scratch
                    .borrow()
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(usize, usize)>()),
            )
            .saturating_add(self.sloppy_eval_memo.capacity())
    }

    /// Conservative bytes reserved by a hash table's bucket allocation.
    ///
    /// `HashMap::capacity` intentionally hides its raw bucket/control layout.
    /// Two entry widths per reported slot is the same stable, conservative
    /// approximation used by `ObjMap`'s numeric index: it covers load-factor
    /// slack and control bytes without depending on libstd/hashbrown internals.
    #[inline]
    fn hash_map_resident_bytes<K, V, S>(map: &std::collections::HashMap<K, V, S>) -> usize {
        map.capacity()
            .saturating_mul(std::mem::size_of::<(K, V)>().saturating_mul(2))
    }

    #[inline]
    fn hash_set_resident_bytes<T, S>(set: &std::collections::HashSet<T, S>) -> usize {
        set.capacity()
            .saturating_mul(std::mem::size_of::<T>().saturating_mul(2))
    }

    /// Reserved allocations in the private-element side tables.  These are
    /// especially important for a sandbox: every instance clones every field
    /// name, so one long source identifier can otherwise amplify into an
    /// unmetered allocation per constructed object.
    fn private_side_table_resident_bytes(&self) -> usize {
        let mut n = Self::hash_map_resident_bytes(&self.private_fields)
            .saturating_add(Self::hash_map_resident_bytes(&self.method_brand))
            .saturating_add(Self::hash_map_resident_bytes(&self.instance_brand))
            .saturating_add(Self::hash_map_resident_bytes(&self.brand_private_names))
            .saturating_add(Self::hash_map_resident_bytes(&self.brand_owner));

        for fields in self.private_fields.values() {
            n = n.saturating_add(Self::hash_map_resident_bytes(fields));
            n = fields
                .keys()
                .fold(n, |n, (_, name)| n.saturating_add(name.capacity()));
        }
        for brands in self.method_brand.values() {
            n = n.saturating_add(brands.capacity().saturating_mul(std::mem::size_of::<u64>()));
        }
        for brands in self.instance_brand.values() {
            n = n.saturating_add(brands.capacity().saturating_mul(std::mem::size_of::<u64>()));
        }
        for names in self.brand_private_names.values() {
            n = n.saturating_add(
                names
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(String, u8)>()),
            );
            n = names
                .iter()
                .fold(n, |n, (name, _)| n.saturating_add(name.capacity()));
        }
        n
    }

    /// Reserved allocations in the lazy Map/Set SameValueZero indexes.  The
    /// backing keys/values remain charged on their heap objects; this covers
    /// only the VM-owned hash directory and each index's flat bucket Vec.
    fn collection_index_resident_bytes(&self) -> usize {
        self.collection_index.values().fold(
            Self::hash_map_resident_bytes(&self.collection_index),
            |n, index| n.saturating_add(index.resident_bytes()),
        )
    }

    /// Bytes retained by unique compiled RegExp programs reachable from heap
    /// objects (including ASCII twins) or the compile cache. `Arc` identities
    /// are sorted and deduplicated so species clones and cache references are
    /// charged once, matching the allocator rather than the JS object count.
    fn regex_program_resident_bytes(&self) -> usize {
        let mut programs = self.regex_program_audit_scratch.borrow_mut();
        programs.clear();
        self.heap.visit_regexp_programs(|program| {
            programs.push((
                std::sync::Arc::as_ptr(program) as usize,
                program.resident_bytes(),
            ));
        });
        for program in self.regex_compile_cache.values() {
            programs.push((
                std::sync::Arc::as_ptr(program) as usize,
                program.resident_bytes(),
            ));
        }
        programs.sort_unstable_by_key(|&(identity, _)| identity);
        programs.dedup_by_key(|entry| entry.0);
        programs.iter().fold(0usize, |bytes, &(_, program_bytes)| {
            bytes.saturating_add(program_bytes)
        })
    }

    /// Reconcile capacity growth within existing heap objects and side tables.
    /// This is also the public resident-byte figure: reporting and enforcement
    /// must not disagree about allocations that only the periodic audit sees.
    pub(crate) fn audit_heap_bytes(&self) -> usize {
        // Grow/reuse the RegExp audit scratch first so vm_core_resident_bytes
        // below includes its current capacity in this same audit.
        let regex_program_bytes = self.regex_program_resident_bytes();
        let mut n = self
            .heap
            .audit_resident_bytes()
            .saturating_add(self.vm_core_resident_bytes())
            .saturating_add(self.private_side_table_resident_bytes())
            .saturating_add(self.collection_index_resident_bytes())
            .saturating_add(regex_program_bytes);
        n = self.frames.iter().fold(n, |n, frame| {
            n.saturating_add(
                frame
                    .handlers
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Handler>()),
            )
        });
        if let Some((_, _, handlers)) = &self.pending_await {
            n = n.saturating_add(
                handlers
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Handler>()),
            );
        }
        for table in [&self.fn_props, &self.arr_props] {
            n = n.saturating_add(table.resident_bytes());
            n = table
                .values()
                .fold(n, |n, map| n.saturating_add(map.resident_bytes()));
        }
        n = n
            .saturating_add(self.proto_of.resident_bytes())
            .saturating_add(self.regexp_result_props.resident_bytes())
            .saturating_add(self.array_js_len.resident_bytes())
            .saturating_add(self.arguments_objs.resident_bytes());
        n = self.regexp_exact_source.values().fold(
            n.saturating_add(Self::hash_map_resident_bytes(&self.regexp_exact_source)),
            |n, bytes| n.saturating_add(bytes.capacity()),
        );
        n = self.regex_compile_cache.keys().fold(
            n.saturating_add(Self::hash_map_resident_bytes(&self.regex_compile_cache)),
            |n, (source, flags, _)| {
                n.saturating_add(source.capacity())
                    .saturating_add(flags.capacity())
            },
        );
        n = self.eval_global_map.keys().fold(
            n.saturating_add(Self::hash_map_resident_bytes(&self.eval_global_map)),
            |n, key| n.saturating_add(key.capacity()),
        );
        n = self.deleted_globals.iter().fold(
            n.saturating_add(Self::hash_set_resident_bytes(&self.deleted_globals)),
            |n, key| n.saturating_add(key.capacity()),
        );
        n = self.symbol_registry.keys().fold(
            n.saturating_add(Self::hash_map_resident_bytes(&self.symbol_registry)),
            |n, key| n.saturating_add(key.capacity()),
        );
        self.symbol_keys.keys().fold(
            n.saturating_add(Self::hash_map_resident_bytes(&self.symbol_keys)),
            |n, key| n.saturating_add(key.capacity()),
        )
    }

    /// Payload-aware resident VM estimate — see `embed::ScriptState::heap_bytes`.
    /// Kept as a named entry point for cheap call-site readability; the audit
    /// itself takes `&self` and is the single source of truth.
    pub(crate) fn heap_bytes(&self) -> usize {
        self.audit_heap_bytes()
    }

    /// Force the JIT on/off (overrides the `ZIPP_NOJIT` default). Used by the
    /// test suite to run a program both ways and assert the outputs match.
    #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
    #[allow(dead_code)] // used by the differential test harness (run_nojit)
    pub(crate) fn set_jit_enabled(&mut self, on: bool) {
        // The first ARM64 tier has no native step-meter yet. Once a recorder or
        // budget is attached, it must remain interpreted even if a caller later
        // tries to re-enable the JIT through the differential-test hook.
        #[cfg(all(feature = "instrument", target_arch = "aarch64"))]
        if self.instr_rec.is_some() {
            self.jit_enabled = false;
            return;
        }
        self.jit_enabled = on;
    }

    #[cfg(all(test, feature = "jit", target_arch = "aarch64"))]
    pub(crate) fn arm_jit_compiled_count(&self) -> usize {
        self.jit.compiled_count()
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
                self.jit
                    .set_meter(crate::codegen::meter::Meter { steps_off: off });
            }
        }
        #[cfg(all(feature = "jit", target_arch = "aarch64"))]
        {
            // ARM64 native metering is intentionally not claimed by the
            // baseline backend. Fail closed until the emitted block meter lands.
            self.jit_enabled = false;
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
        #[cfg(all(feature = "jit", any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            self.jit_enabled = false;
        }
    }
}

#[cfg(all(
    test,
    feature = "jit",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod module_jit_eligibility_tests {
    use super::module_func_range_contains;

    #[test]
    fn only_recorded_half_open_module_ranges_accept_runtime_function_ids() {
        // Model module A, two eval/new-Function gaps, module B, another gap,
        // and module C. Dynamic functions interleaved in those gaps must never
        // become eligible merely because a later module was installed.
        let ranges = [(10, 13, 101), (17, 19, 102), (25, 30, 103)];
        for id in [10, 11, 12, 17, 18, 25, 26, 27, 28, 29] {
            assert!(module_func_range_contains(&ranges, id), "module fn{id}");
        }
        for id in [9, 13, 14, 15, 16, 19, 20, 24, 30, 31] {
            assert!(
                !module_func_range_contains(&ranges, id),
                "eval/new-Function gap fn{id}"
            );
        }
    }
}
