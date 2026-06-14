#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

/// Cached `ZIPP_CALLLOG` flag (an env read per call-site miss would dominate
/// the miss path — Windows scans the whole environment block per query).
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn jit_call_log() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("ZIPP_CALLLOG").is_some())
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
            site_ics: Vec::new(),
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
            pending_yield_raw: false,
            pending_await: None,
            cap_capture: None,
            microtasks: std::collections::VecDeque::new(),
            template_raws: std::collections::HashMap::new(),
            template_cache: std::collections::HashMap::new(),
            regexp_string_iters: std::collections::HashMap::new(),
            regexp_exact_source: std::collections::HashMap::new(),
            regex_compile_cache: rustc_hash::FxHashMap::default(),
            collection_index: rustc_hash::FxHashMap::default(),
            next_private_brand: 1,
            method_brand: std::collections::HashMap::new(),
            instance_brand: std::collections::HashMap::new(),
            brand_private_names: std::collections::HashMap::new(),
            brand_owner: std::collections::HashMap::new(),
            prototypes: std::collections::HashMap::new(),
            proto_of: rustc_hash::FxHashMap::default(),
            fn_props: rustc_hash::FxHashMap::default(),
            arr_props: rustc_hash::FxHashMap::default(),
            ab_max: std::collections::HashMap::new(),
            ta_tracking: std::collections::HashSet::new(),
            dv_tracking: std::collections::HashSet::new(),
            deleted_callable_intrinsics: std::collections::HashSet::new(),
            deleted_globals: std::collections::HashSet::new(),
            array_length_nonwritable: std::collections::HashSet::new(),
            array_js_len: std::collections::HashMap::new(),
            array_proto_has_index: false,
            super_called: std::collections::HashSet::new(),
            this_tdz: std::collections::HashSet::new(),
            super_this: std::collections::HashMap::new(),
            private_fields: std::collections::HashMap::new(),
            eval_fn_idx: 0,
            closure_eval_scope: std::collections::HashMap::new(),
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
            vm_start: std::time::Instant::now(),
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
            fn_name_cells: std::collections::HashSet::new(),
            typed_module_cache: std::collections::HashMap::new(),
            pending_gen_callee: Value::UNDEFINED,
            gen_callee: std::collections::HashMap::new(),
            eval_script_gdi: false,
            eval_lexical_globals: std::collections::HashSet::new(),
            eval_const_globals: std::collections::HashSet::new(),
            eval_var_globals: std::collections::HashSet::new(),
            arguments_objs: std::collections::HashMap::new(),
            gen_args_obj: std::collections::HashMap::new(),
            module_base_dir: None,
            module_cache: std::collections::HashMap::new(),
            module_namespaces: std::collections::HashMap::new(),
            module_own: std::collections::HashMap::new(),
            closure_home: std::collections::HashMap::new(),
            closure_new_target: std::collections::HashMap::new(),
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
            jit_call_depth: 0,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            osr_deopt_exempt: false,
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            jit_const_strings: Vec::new(),
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            mi_cache: Vec::new(),
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

    /// Build the TypedArray pin plan for the OSR region `[start, end]` from
    /// LIVE VM state (called right before `compile_region`, frame `base` on
    /// top): for each `GetIndex`/`SetIndex`, find the receiver's nearest
    /// preceding in-region writer — a `LoadGlobal g` with `g` never stored in
    /// the region pins via `Global(g)`; a receiver register never written in
    /// the region pins via `Reg(r)`; anything else is left to the generic
    /// helper. A source qualifies only if it holds a non-BigInt TypedArray
    /// RIGHT NOW (the emitted code is kind-specialised). The hint is purely an
    /// OPTIMISATION: every fast-path access re-checks receiver identity against
    /// the snapshot at runtime, and the snapshot helper re-validates kind /
    /// detach / bounds — a wrong or stale hint degrades to the helper path,
    /// never to a wrong answer.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn build_ta_pin_plan(
        &self,
        func_id: u32,
        start: u32,
        end: u32,
        base: usize,
    ) -> crate::codegen::TaPinPlan {
        use crate::codegen::{TaPin, TaPinPlan, TaPinSrc};
        // Conservative "does this instruction write register r" cover. An op
        // missing here only weakens the hint (see above) — never soundness.
        fn writes(i: &Instr, r: u16) -> bool {
            let dst = match *i {
                Instr::LoadInt { dst, .. }
                | Instr::LoadConst { dst, .. }
                | Instr::Move { dst, .. }
                | Instr::LoadGlobal { dst, .. }
                | Instr::LoadGlobalOrUndefined { dst, .. }
                | Instr::AddInt { dst, .. }
                | Instr::Add { dst, .. }
                | Instr::Sub { dst, .. }
                | Instr::Mul { dst, .. }
                | Instr::Div { dst, .. }
                | Instr::Mod { dst, .. }
                | Instr::Neg { dst, .. }
                | Instr::Not { dst, .. }
                | Instr::Bitwise { dst, .. }
                | Instr::Lt { dst, .. }
                | Instr::Le { dst, .. }
                | Instr::Gt { dst, .. }
                | Instr::Ge { dst, .. }
                | Instr::Eq { dst, .. }
                | Instr::Ne { dst, .. }
                | Instr::GetProp { dst, .. }
                | Instr::GetIndex { dst, .. }
                | Instr::HasProp { dst, .. }
                | Instr::StrConcat { dst, .. }
                | Instr::StrAppendInPlace { dst, .. }
                | Instr::Call { dst, .. }
                | Instr::CallMethod { dst, .. } => dst,
                _ => return false,
            };
            dst == r
        }
        let mut plan = TaPinPlan::default();
        let proto = self.func(func_id as usize);
        let (s, e) = (start as usize, end as usize);
        if e <= s || e >= proto.code.len() {
            return plan;
        }
        let mut stored_globals: rustc_hash::FxHashSet<u32> = rustc_hash::FxHashSet::default();
        for ins in &proto.code[s..=e] {
            if let Instr::StoreGlobal { idx, .. } | Instr::StoreGlobalStrict { idx, .. } = *ins {
                stored_globals.insert(idx);
            }
        }
        // Per-access pin selector: a TA-or-dense-Array element access (kind
        // taken from the LIVE receiver — a TypedArray's element kind, or
        // ARR_PIN_KIND for a dense Array), a DataView `get*`, or a flat-ASCII
        // `charCodeAt` string.
        enum Recv {
            Ta,
            Dv,
            Str,
        }
        for aip in s..=e {
            let (obj, recv) = match proto.code[aip] {
                // `arr[i]` (GetIndex), `arr[i]=v` (SetIndex), and `i in arr`
                // (HasProp, brand=false) all pin their receiver the same way; the
                // LIVE heap object decides TA-kind vs ARR_PIN_KIND. SetIndex pins
                // only a TypedArray (its inline store path); a dense-Array store
                // is left to the generic helper (it can grow/realloc), but its
                // receiver is still observed here and resolves to ARR_PIN_KIND
                // only when the inline GetIndex/HasProp can use it.
                Instr::GetIndex { obj, .. } | Instr::SetIndex { obj, .. } => (obj, Recv::Ta),
                Instr::HasProp { obj, brand: false, .. } => (obj, Recv::Ta),
                // A whitelisted DataView `get*` receiver pins the same way
                // (snapshot: data+byteOffset / byteLength).
                Instr::CallMethod { obj, name, argc, .. }
                    if (argc == 1 || argc == 2)
                        && proto
                            .string_constants
                            .get(name as usize)
                            .is_some_and(|k| crate::codegen::dv_get_kind(k).is_some()) =>
                {
                    (obj, Recv::Dv)
                }
                // A `str.charCodeAt(i)` receiver pins as a flat-ASCII string
                // (snapshot: bytes ptr + units), so the access inlines to a
                // direct byte load instead of the per-op `jit_char_code_at` call.
                Instr::CallMethod { obj, name, argc, .. }
                    if argc == 1
                        && proto
                            .string_constants
                            .get(name as usize)
                            .is_some_and(|k| k == "charCodeAt") =>
                {
                    (obj, Recv::Str)
                }
                _ => continue,
            };
            let writer = (s..aip).rev().find(|&wip| writes(&proto.code[wip], obj));
            let src = match writer.map(|wip| &proto.code[wip]) {
                Some(&Instr::LoadGlobal { idx, .. }) if !stored_globals.contains(&idx) => {
                    TaPinSrc::Global(idx)
                }
                Some(_) => continue,
                None => {
                    // Live-in receiver: pin only if NOTHING in the region
                    // writes it (so the prologue/refetch reg read stays the
                    // value the accesses see).
                    if proto.code[s..=e].iter().any(|i| writes(i, obj)) {
                        continue;
                    }
                    TaPinSrc::Reg(obj)
                }
            };
            let live = match src {
                TaPinSrc::Global(g) => {
                    self.globals.get(g as usize).copied().unwrap_or(Value::UNDEFINED)
                }
                TaPinSrc::Reg(r) => self.get(base, r),
            };
            if !live.is_heap() {
                continue;
            }
            let kind = match (self.heap.get(live.heap_index()), &recv) {
                (HeapObj::TypedArray { kind, .. }, Recv::Ta) if *kind < 9 => *kind,
                // A dense Array pins for inline `arr[i]` / `i in arr`. Decline
                // when it carries an `arr_props` overlay (defineProperty'd /
                // sparse-overlay index) or is a mapped-`arguments` object — both
                // need the interpreter's override-aware path, so a pin would be
                // wasted (the snapshot helper also declines at runtime → all-zero
                // → identity miss → generic helper, so this is an optimisation,
                // never a soundness gate).
                (HeapObj::Array(_), Recv::Ta)
                    if !self.arr_props.contains_key(&live.heap_index())
                        && !self.arguments_objs.contains_key(&live.heap_index()) =>
                {
                    crate::codegen::ARR_PIN_KIND
                }
                (HeapObj::DataView { .. }, Recv::Dv) => crate::codegen::DV_PIN_KIND,
                // Pin only a FLAT ASCII string — the inline byte load needs
                // byte i == UTF-16 unit i (a rope/non-ASCII string snapshots
                // zero and falls to the generic helper, so a wrong pin is safe;
                // we just skip pinning it here when it can't help).
                (HeapObj::Str(js), Recv::Str) if js.is_ascii() => crate::codegen::STR_PIN_KIND,
                _ => continue,
            };
            let slot = match plan.pins.iter().position(|p| p.src == src && p.kind == kind) {
                Some(j) => j,
                None => {
                    if plan.pins.len() >= 8 {
                        continue; // slot budget — extra accesses use the helper
                    }
                    plan.pins.push(TaPin { src, kind });
                    plan.pins.len() - 1
                }
            };
            plan.access.insert(aip, slot as u8);
        }
        plan
    }

    /// Q4 v1: build the leaf-call inline plan for a memory-path region — the set
    /// of `Call` sites in `[start, end]` whose monomorphic cached callee is a
    /// PLAIN LEAF (`callee_leaf_ok`) the region emitter can inline straight-line.
    /// Each entry carries the callee's identity bits (the runtime guard), the
    /// scratch-window offset (the caller's `reg_count`), and the body to emit.
    /// A site not in the map keeps the per-call `jit_call_ic` helper.
    ///
    /// Resolution uses the LIVE per-site IC (`ic_call_mono`, read-only): the
    /// loop has executed `OSR_THRESHOLD` times by OSR-compile, so a hot
    /// monomorphic call already has its `Callee` way filled. A polymorphic /
    /// unfilled site simply isn't inlined.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn build_leaf_inline_plan(
        &self,
        func_id: u32,
        start: u32,
        end: u32,
    ) -> rustc_hash::FxHashMap<usize, crate::codegen::LeafInlinePlan> {
        use crate::codegen::{callee_leaf_ok, LeafInlinePlan};
        let mut plan = rustc_hash::FxHashMap::default();
        let caller = self.func(func_id as usize);
        let reg_window = caller.reg_count;
        let log = std::env::var_os("ZIPP_JITLOG").is_some();
        for ip in start as usize..=end as usize {
            let Instr::Call { argc, .. } = caller.code[ip] else {
                continue;
            };
            // Monomorphic plain-callee from the live IC (with the cached slot
            // version — the inline guard re-checks it to defeat GC slot-reuse ABA).
            let Some((callee_bits, callee_ver, fid, closure)) = self.ic_call_mono(func_id, ip)
            else {
                if log {
                    eprintln!("[leaf] fn{func_id}@{ip} NOT-MONO (no single Callee IC way)");
                }
                continue;
            };
            // v1: only callees with NO captured upvalues (the body has no
            // Upval/Cell ops anyway, but a closure value with upvalues whose
            // body somehow reads them would be unsound — exclude by construction).
            let callee = self.func(fid as usize);
            if closure != NO_CLOSURE && !callee.upvalues.is_empty() {
                if log {
                    eprintln!("[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE (closure w/ upvalues)");
                }
                continue;
            }
            // The inline emitter forces `this = undefined` for the callee window.
            // That is ONLY correct for a STRICT, NON-ARROW leaf (its `this` is
            // genuinely undefined when called as `f(args)`). An arrow captures
            // `this` lexically (stored in the Closure, NOT undefined); a sloppy
            // function substitutes the global object (OrdinaryCallBindThis). Both
            // would observe the wrong `this` if inlined — decline them.
            if callee.lexical_this || !callee.is_strict {
                if log {
                    eprintln!(
                        "[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE \
                         (lexical_this={} strict={})",
                        callee.lexical_this, callee.is_strict
                    );
                }
                continue;
            }
            // The carved scratch window must hold the callee's whole register
            // file; the headroom (vs MAX_FRAMES recursion) is checked at the
            // region entry by `jit_regs_fits`.
            let Some(body) = callee_leaf_ok(callee) else {
                if log {
                    eprintln!("[leaf] fn{func_id}@{ip} callee fn{fid} DECLINE (not leaf-eligible)");
                }
                continue;
            };
            // Pre-resolve the numeric constants the body's `LoadConst` ops read
            // (callee_leaf_ok rejected any non-numeric constant).
            let mut consts = rustc_hash::FxHashMap::default();
            for instr in &body {
                if let Instr::LoadConst { idx, .. } = *instr {
                    if let Some(c) = callee.constants.get(idx as usize) {
                        consts.insert(idx, c.bits());
                    }
                }
            }
            if log {
                eprintln!(
                    "[leaf] fn{func_id}@{ip} callee fn{fid} INLINE-ELIGIBLE \
                     (argc={argc} callee_regs={} params={} body_ops={})",
                    callee.reg_count,
                    callee.param_count,
                    body.len()
                );
            }
            plan.insert(
                ip,
                LeafInlinePlan {
                    callee_bits,
                    callee_ver,
                    reg_window,
                    callee_reg_count: callee.reg_count,
                    param_count: callee.param_count,
                    body,
                    consts,
                },
            );
        }
        plan
    }

    /// Q7 method-inline plan: in-region `CallMethod` sites whose LIVE receiver is
    /// a class instance with a trivial NO-`super` method body (own-`this` field
    /// reads + numeric arithmetic). Read-only — built (like the leaf plan) BEFORE
    /// the `&proto` borrow at the OSR-compile site. `base` is the caller frame
    /// base, used to read the live receiver exemplar from `self.regs` (the
    /// class-keyed IC doesn't record receiver instances). The emitted code guards
    /// the receiver identity+version and falls to the helper on ANY miss, so a
    /// stale/partial/wrong-shape plan is always safe (just slower). v1 is
    /// monomorphic per site (one exemplar receiver baked); other receivers /
    /// shapes miss to the helper. See [`crate::codegen::MethodInlinePlan`].
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn build_method_inline_plan(
        &self,
        func_id: u32,
        start: u32,
        end: u32,
        base: usize,
    ) -> rustc_hash::FxHashMap<usize, crate::codegen::MethodInlinePlan> {
        use crate::codegen::MethodInlinePlan;
        use crate::heap::HeapObj;
        const MAX_ARMS: usize = crate::codegen::JIT_IC_WAYS; // = 8
        let mut plan = rustc_hash::FxHashMap::default();
        if std::env::var_os("ZIPP_NO_METHOD_INLINE").is_some() {
            return plan; // kill-switch (live through all stages)
        }
        let log = std::env::var_os("ZIPP_JITLOG").is_some();
        let caller = self.func(func_id as usize);
        let reg_window = caller.reg_count;
        // The op at `ip` selects how the receiver's resolved member is inlined:
        // CallMethod -> class method; GetProp -> trivial class getter; SetProp ->
        // trivial class setter (Stage 5). All share receiver enumeration + the
        // guard tree; only the per-shape resolve/body/binding differ.
        #[derive(Clone, Copy)]
        enum MiKind {
            Method,
            Getter,
            Setter,
        }
        for ip in start as usize..=end as usize {
            let (obj, name, kind) = match caller.code[ip] {
                Instr::CallMethod { obj, name, .. } => (obj, name, MiKind::Method),
                Instr::GetProp { obj, name, .. } => (obj, name, MiKind::Getter),
                Instr::SetProp { obj, name, .. } => (obj, name, MiKind::Setter),
                _ => continue,
            };
            let key = &caller.string_constants[name as usize];
            // ── enumerate candidate receivers ── the live exemplar at the obj reg
            // (last iteration's value) PLUS, when `obj` was `arr[idx]`, the array's
            // dense elements (the `objs[i&3]` polymorphic shape). Every candidate
            // is independently identity+version-guarded, so an extra/wrong guess
            // just yields a dead arm — never a correctness risk.
            let mut cand_bits: Vec<u64> = Vec::new();
            let mut push_cand = |v: Value, cands: &mut Vec<Value>, bits: &mut Vec<u64>| {
                if v.is_heap() && !bits.contains(&v.bits()) && cands.len() < MAX_ARMS {
                    bits.push(v.bits());
                    cands.push(v);
                }
            };
            let mut cands: Vec<Value> = Vec::new();
            // PRIMARY source: receiver instances RECORDED at this site's Class*
            // IC fills during warmup — robust for `var o = arr[i]; o.m()` (where
            // `o` is loaded indirectly, defeating the obj-reg/array trace below).
            // Each is identity+version-guarded, so extras/stale are safe.
            if let Some(rset) = self.mi_recv.get(&(((func_id as u64) << 32) | ip as u64)) {
                for &b in rset {
                    push_cand(Value::from_bits(b), &mut cands, &mut cand_bits);
                }
            }
            // The live exemplar at the obj reg (always reliable — it's the op's
            // receiver, live at the op).
            if let Some(&v) = self.regs.get(base + obj as usize) {
                push_cand(v, &mut cands, &mut cand_bits);
            }
            // Best-effort: the dense elements of the array a `arr[idx]` receiver
            // came from (supplements recording; the temp may be reused).
            if let Some(arr_reg) = Self::mi_last_getindex_array(&caller.code, start as usize, ip, obj) {
                if let Some(&av) = self.regs.get(base + arr_reg as usize) {
                    if av.is_heap() {
                        if let HeapObj::Array(items) = self.heap.get(av.heap_index()) {
                            let snapshot: Vec<Value> = items.iter().copied().collect();
                            for el in snapshot {
                                push_cand(el, &mut cands, &mut cand_bits);
                            }
                        }
                    }
                }
            }
            // Build a guarded arm per candidate (any that declines is skipped).
            let mut shapes = Vec::new();
            let mut win_top = 0u16;
            for recv in cands {
                let built = match kind {
                    MiKind::Method => self.build_method_shape(func_id, ip, recv, key, reg_window),
                    MiKind::Getter => {
                        self.build_accessor_shape(func_id, ip, recv, key, reg_window, false)
                    }
                    MiKind::Setter => {
                        self.build_accessor_shape(func_id, ip, recv, key, reg_window, true)
                    }
                };
                if let Some((shape, shape_top)) = built {
                    win_top = win_top.max(shape_top);
                    shapes.push(shape);
                }
            }
            if shapes.is_empty() {
                continue;
            }
            if log {
                let k = match kind {
                    MiKind::Method => "method",
                    MiKind::Getter => "getter",
                    MiKind::Setter => "setter",
                };
                eprintln!(
                    "[mi] fn{func_id}@{ip} INLINE {k} arms={} win_top={win_top}",
                    shapes.len()
                );
            }
            plan.insert(ip, MethodInlinePlan { reg_window, win_top, shapes });
        }
        plan
    }

    /// The last `GetIndex{dst:obj_reg, obj:arr}` in `code[start..ip]` (the array a
    /// `arr[idx]` receiver came from), so the planner can bake an arm per array
    /// element. `None` if `obj_reg` was last produced by something else.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn mi_last_getindex_array(code: &[Instr], start: usize, ip: usize, obj_reg: u16) -> Option<u16> {
        let mut arr = None;
        for instr in &code[start..ip] {
            if let Instr::GetIndex { dst, obj, .. } = *instr {
                if dst == obj_reg {
                    arr = Some(obj);
                }
            }
        }
        arr
    }

    /// Build one receiver arm for a `CallMethod` inline: validate `recv` is a
    /// plain class instance with no own-shadow of `key`, resolve its class method
    /// (+ any `super.m()`), and bake the per-receiver guards/slots. Returns the
    /// arm and its scratch-window top, or `None` to skip this receiver. Read-only.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn build_method_shape(
        &self,
        func_id: u32,
        ip: usize,
        recv: Value,
        key: &str,
        reg_window: u16,
    ) -> Option<(crate::codegen::MethodInlineShape, u16)> {
        use crate::heap::HeapObj;
        let ridx = recv.heap_index();
        if !self.ic_obj_ok(ridx) {
            return None;
        }
        // Receiver must be a plain (non-ctor) class instance.
        let (recv_class, vals_ptr) = match self.heap.get(ridx) {
            HeapObj::Object(m) if !m.is_ctor => match m.class {
                Some(c) => (c, m.vals.as_ptr() as u64),
                None => return None,
            },
            _ => return None,
        };
        // G3b: an own property shadowing the method name → resolve to the own
        // prop, not the class method — decline this arm.
        if let HeapObj::Object(m) = self.heap.get(ridx) {
            if m.pos(key).is_some() {
                return None;
            }
        }
        let fid = self.ic_class_method_fid(func_id, ip, recv_class)?;
        let callee = self.func(fid as usize);
        // Outer body admits `super.m()` (Stage 3); super targets do not.
        let body_len = Self::method_inline_body_ok(callee, true, false)?;
        let body: Vec<Instr> = callee.code[..body_len].to_vec();
        let field_slots = self.mi_bake_fields(ridx, &body, &callee.string_constants)?;
        let consts = Self::mi_bake_consts(&callee.constants, &body);
        // ── bake each `super.m()` in the body (Stage 3) ──
        let super_win = reg_window + callee.reg_count;
        let mut supers = rustc_hash::FxHashMap::default();
        let mut max_super_regs = 0u16;
        for (bi, instr) in body.iter().enumerate() {
            if let Instr::SuperMethod { home_class_id, name: sname, argc: sargc, .. } = *instr {
                if sargc != 0 {
                    return None; // v1: 0-arg super only
                }
                let skey = &callee.string_constants[sname as usize];
                let sr = self.ic_super_method_baked(fid, bi, home_class_id, skey)?;
                let scallee = self.func(sr.fid as usize);
                // Super target must be inlinable AND have NO nested super (v1).
                let sblen = Self::method_inline_body_ok(scallee, false, false)?;
                let sbody: Vec<Instr> = scallee.code[..sblen].to_vec();
                let sfields = self.mi_bake_fields(ridx, &sbody, &scallee.string_constants)?;
                let sconsts = Self::mi_bake_consts(&scallee.constants, &sbody);
                max_super_regs = max_super_regs.max(scallee.reg_count);
                supers.insert(
                    bi,
                    crate::codegen::SuperInline {
                        // The VM `mi_class_epoch` scalar's address is stable for
                        // the run (Vm is not moved); bake a pointer + the value.
                        epoch_ptr: &self.mi_class_epoch as *const u32 as u64,
                        epoch_val: self.mi_class_epoch,
                        hops: sr.hops,
                        holder_vals_ptr: sr.holder_vals_ptr,
                        holder_slot: sr.holder_slot,
                        fn_bits: sr.fn_bits,
                        field_slots: sfields,
                        consts: sconsts,
                        body: sbody,
                        callee_reg_count: scallee.reg_count,
                        win_off: super_win,
                    },
                );
            }
        }
        let win_top = if supers.is_empty() {
            reg_window + callee.reg_count
        } else {
            super_win + max_super_regs
        };
        let recv_ver = self.heap.version_of(ridx);
        Some((
            crate::codegen::MethodInlineShape {
                recv_bits: recv.bits(),
                recv_ver,
                vals_ptr,
                field_slots,
                callee_reg_count: callee.reg_count,
                param_count: callee.param_count,
                body,
                consts,
                supers,
            },
            win_top,
        ))
    }

    /// Build one receiver arm for an ACCESSOR (getter/setter) inline (Stage 5):
    /// validate `recv` is a plain class instance with no own-shadow of `name`,
    /// resolve its TRIVIAL class getter/setter, bake the per-receiver guards +
    /// field slot(s). v1: NO super (Tri/Hex super-accessors decline → helper).
    /// zipp resolves class accessors via the class id (prototype reassignment
    /// ignored — verified JIT==NOJIT, model limit), so the receiver identity +
    /// version guard alone matches the interpreter (like methods; no class-version
    /// guard, and class redefinition keeps the old instance's old accessor).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn build_accessor_shape(
        &self,
        func_id: u32,
        ip: usize,
        recv: Value,
        name: &str,
        reg_window: u16,
        is_setter: bool,
    ) -> Option<(crate::codegen::MethodInlineShape, u16)> {
        use crate::heap::HeapObj;
        let ridx = recv.heap_index();
        if !self.ic_obj_ok(ridx) {
            return None;
        }
        let (recv_class, vals_ptr) = match self.heap.get(ridx) {
            HeapObj::Object(m) if !m.is_ctor => match m.class {
                Some(c) => (c, m.vals.as_ptr() as u64),
                None => return None,
            },
            _ => return None,
        };
        // G3b: an own property named `name` shadows the accessor → decline (the
        // recv-version guard catches a LATER own-add).
        if let HeapObj::Object(m) = self.heap.get(ridx) {
            if m.pos(name).is_some() {
                return None;
            }
        }
        let fid = if is_setter {
            self.ic_class_setter_fid(func_id, ip, recv_class)?
        } else {
            self.ic_class_getter_fid(func_id, ip, recv_class)?
        };
        let callee = self.func(fid as usize);
        // v1 accessors: NO super (allow_super=false); a setter body ends in a
        // SetProp{obj:0} store (allow_setprop=is_setter).
        let body_len = Self::method_inline_body_ok(callee, false, is_setter)?;
        let body: Vec<Instr> = callee.code[..body_len].to_vec();
        let field_slots = self.mi_bake_fields(ridx, &body, &callee.string_constants)?;
        let consts = Self::mi_bake_consts(&callee.constants, &body);
        let recv_ver = self.heap.version_of(ridx);
        Some((
            crate::codegen::MethodInlineShape {
                recv_bits: recv.bits(),
                recv_ver,
                vals_ptr,
                field_slots,
                callee_reg_count: callee.reg_count,
                param_count: callee.param_count,
                body,
                consts,
                supers: rustc_hash::FxHashMap::default(),
            },
            reg_window + callee.reg_count, // no super → window is just the body
        ))
    }

    /// Resolve every `this.<field>` (GetProp/SetProp `obj:0`) in `body` to the
    /// live receiver's own DATA slot (a store also requires it be WRITABLE).
    /// `None` if any field is missing / an accessor / (store) non-writable / the
    /// receiver isn't a plain Object (decline the inline).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn mi_bake_fields(
        &self,
        ridx: u32,
        body: &[Instr],
        strconsts: &[String],
    ) -> Option<rustc_hash::FxHashMap<u32, u32>> {
        let m = match self.heap.get(ridx) {
            crate::heap::HeapObj::Object(m) => m,
            _ => return None,
        };
        let mut fs = rustc_hash::FxHashMap::default();
        for instr in body {
            // GetProp{obj:0} reads need a non-accessor slot; SetProp{obj:0} (a
            // setter's store) needs a non-accessor WRITABLE slot.
            let (fname, need_writable) = match *instr {
                Instr::GetProp { obj: 0, name: fname, .. } => (fname, false),
                Instr::SetProp { obj: 0, name: fname, .. } => (fname, true),
                _ => continue,
            };
            let fkey = &strconsts[fname as usize];
            match m.pos(fkey) {
                Some(s) if !m.attrs[s].accessor && (!need_writable || m.attrs[s].writable) => {
                    fs.insert(fname, s as u32);
                }
                _ => return None,
            }
        }
        Some(fs)
    }

    /// Pre-resolve the numeric-constant bits a body's `LoadConst` ops read.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn mi_bake_consts(
        consts: &[Value],
        body: &[Instr],
    ) -> rustc_hash::FxHashMap<u32, u64> {
        let mut c = rustc_hash::FxHashMap::default();
        for instr in body {
            if let Instr::LoadConst { idx, .. } = *instr {
                if let Some(v) = consts.get(idx as usize) {
                    c.insert(idx, v.bits());
                }
            }
        }
        c
    }

    /// Trivial-method body scan for the Q7 in-region emitter. Returns the body
    /// length (ops up to and incl. the first `Return`/`ReturnUndefined`), or
    /// `None` to decline. `allow_super` admits `SuperMethod` (the outer body); a
    /// super TARGET is scanned with `allow_super=false` (v1 has no nested super).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn method_inline_body_ok(
        p: &crate::bytecode::FuncProto,
        allow_super: bool,
        allow_setprop: bool,
    ) -> Option<usize> {
        use crate::bytecode::Instr as I;
        if p.is_generator || p.is_async {
            return None;
        }
        if p.rest_reg.is_some() || p.arguments_reg.is_some() {
            return None;
        }
        // Bound the scratch window (≤16, matching the leaf inliner's headroom).
        if p.reg_count > 16 {
            return None;
        }
        let code = &p.code;
        let term = code
            .iter()
            .position(|i| matches!(i, I::Return { .. } | I::ReturnUndefined))?;
        for (ix, instr) in code[..term].iter().enumerate() {
            match *instr {
                I::LoadInt { .. } | I::LoadBool { .. } | I::Move { .. } => {}
                I::LoadConst { idx, .. } => match p.constants.get(idx as usize) {
                    Some(c) if c.is_number() => {}
                    _ => return None,
                },
                I::GetProp { obj: 0, .. } => {}
                I::Add { .. }
                | I::Sub { .. }
                | I::Mul { .. }
                | I::Div { .. }
                | I::Mod { .. }
                | I::AddInt { .. }
                | I::Neg { .. }
                | I::Bitwise { .. } => {}
                // `super.m()` admitted only in the outer body (Stage 3); the
                // resolved super target is re-scanned with allow_super=false.
                I::SuperMethod { .. } if allow_super => {}
                // A setter's `this.<field> = val` store (Stage 5): the body's ONLY
                // effect, so it must be the LAST op before the terminator (no later
                // op can decline AFTER the store commits — the no-deopt-after-effect
                // rule). emit_mi_body handles `obj: 0` only.
                I::SetProp { obj: 0, .. } if allow_setprop && ix + 1 == term => {}
                // Rejects SuperGet/SuperSet/MathOp/GetIndex/non-last-SetProp/calls.
                _ => return None,
            }
        }
        Some(term + 1)
    }

    /// Would growing `self.regs` to `needed` slots exceed the pinned capacity?
    /// (Interpreter-only builds: never — there is no pinned native pointer to
    /// protect, so the Vec may grow/reallocate freely.)
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn regs_would_overflow(&self, needed: usize) -> bool {
        self.reg_capacity != 0 && needed > self.reg_capacity
    }

    /// The pinned register-file capacity (slots) for the Q4 leaf-inline headroom
    /// check in `jit_regs_fits`. The reserved capacity never changes after
    /// `reserve_jit_regs`, so a scratch window inside it can't trigger a realloc.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn reg_capacity_pub(&self) -> usize {
        self.reg_capacity
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
            self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX,
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
        self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX,
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
    const METHOD_INLINE_MAX_SUPER: u32 = 6;

    /// Capacity of the off-frame method evaluator's STACK register window. A body
    /// with `reg_count` above this declines to the frame call. Trivial method
    /// bodies are tiny; the cap keeps the per-call stack array small and avoids
    /// any heap allocation on the hot path.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    const MI_MAX_REGS: usize = 24;

    /// MI (method inlining): if the resolved class/own/proto METHOD `fid` is a
    /// "trivial" straight-line body over `this`(=`recv`) + its formal params —
    /// arithmetic on numbers, own-data `this.<field>` reads, and nested
    /// `super.m(args)` calls — evaluate it DIRECTLY (no `setup_call`, no
    /// `run_loop`, no frame push, no per-call args Vec) and return the result
    /// bits. Returns `None` to fall back to the full frame call (any other body
    /// shape, an unrecognised op, a non-numeric arithmetic operand, a missing /
    /// accessor / inherited field, a non-instance receiver) and `Some(CALL_THREW)`
    /// when a nested super target threw.
    ///
    /// This is the call-floor collapse for the class-method benches: every
    /// `objs[i&3].area()` body is `return super.area() * k + …`, and `super.area()`
    /// resolves to `return this._v + 1` — so the whole two-deep call chain runs
    /// as a handful of Rust ops over `recv`'s own slots, no frame machinery.
    ///
    /// SOUNDNESS:
    /// * Reached ONLY from `jit_region_call_impl` (a JIT region helper), so the
    ///   interpreter / `ZIPP_NOJIT` path is byte-identical (never calls this).
    /// * `ic_call_method` ALREADY resolved `fid` with the full guard set incl.
    ///   the G3b own-shadow guard (its `ClassMethod` arm requires `own.is_none()`)
    ///   and the class-version guard — so an instance own-write `inst.m = fn`
    ///   misses the IC and never reaches here, and a stale class misses too. We
    ///   only need to evaluate the resolved body faithfully.
    /// * NO partial side effect before a `None`: a two-pass shape — pass 1
    ///   (`method_body_inlinable`) validates the ENTIRE straight-line body is
    ///   executable WITHOUT running anything; pass 2 executes. So an unsupported
    ///   op declines (pass 1) before any super call commits, and once pass 2
    ///   starts every op is known-executable.
    /// * Arithmetic delegates to the SAME value-level helpers the interpreter's
    ///   ops use (`add_values`, `numeric_binop`) so results are byte-identical;
    ///   it is admitted only on operands that are ALREADY numbers (else pass 1
    ///   declines), so no observable `valueOf`/`ToPrimitive` ever runs off-frame.
    /// * A nested `super.m()` is resolved via the SAME `ic_super_method` cache
    ///   the interpreter uses (live home-class value + version-guarded chain),
    ///   then evaluated off-frame recursively (depth-bounded) or, if its target
    ///   isn't trivial, by a real `jit_frame_call` — identical observable effect.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn try_method_inline(
        &mut self,
        fid: u32,
        recv: Value,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Option<u64> {
        // Pass 1: validate the body shape without executing anything.
        let body_len = self.method_body_inlinable(fid)?;
        // Pass 2: execute over a local register window.
        self.run_method_inline(fid, recv, caller_base, arg_base, argc, body_len, 0)
    }

    /// Pass 1 of method inlining: is `fid`'s body a straight-line prefix of ops
    /// the off-frame evaluator implements, ending at the FIRST `Return`/
    /// `ReturnUndefined`? Returns the body length (ops up to and incl. that
    /// terminator), or `None` to decline. Performs NO execution / side effect.
    /// Mirrors the eligibility of `callee_leaf_ok` (no generator/async, simple
    /// params, no rest/arguments, bounded regs) but ADDS own-`this` GetProp and
    /// `super.m()` to the admitted op set and binds `this = recv` (a class method
    /// is strict and uses its receiver — never the global-leaf `this=undefined`).
    ///
    /// MEMOIZED in `self.mi_cache` (a FuncProto's code is immutable for life), so
    /// the hot per-call path pays the body scan ONCE per fid.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn method_body_inlinable(&mut self, fid: u32) -> Option<usize> {
        let i = fid as usize;
        if i < self.mi_cache.len() {
            match self.mi_cache[i] {
                v if v == i32::MIN => {}        // not yet computed
                -1 => return None,              // memoized ineligible
                v => return Some(v as usize),   // memoized body length
            }
        } else {
            self.mi_cache.resize(i + 1, i32::MIN);
        }
        let res = self.method_body_inlinable_scan(fid);
        self.mi_cache[i] = match res {
            Some(len) => len as i32,
            None => -1,
        };
        res
    }

    /// The uncached body-shape scan behind `method_body_inlinable`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn method_body_inlinable_scan(&self, fid: u32) -> Option<usize> {
        use crate::bytecode::Instr as I;
        let p = self.func(fid as usize);
        if p.is_generator || p.is_async {
            return None;
        }
        // No rest/`arguments` object (binding past `param_count` must not be
        // observable). We do NOT require `simple_params`: that flag is purely
        // about a SLOPPY function's MAPPED arguments object and is deliberately
        // false for every (strict) class method. A default/destructuring
        // parameter prologue would emit a `Jump`/unsupported op before the first
        // `Return`, which the straight-line whitelist below rejects — so plain
        // positional binding is the only param shape that survives here.
        if p.rest_reg.is_some() || p.arguments_reg.is_some() {
            return None;
        }
        // A bounded local register window (kept small — these are tiny bodies;
        // the executor uses a fixed `[Value; MI_MAX_REGS]` stack array).
        if p.reg_count as usize > Self::MI_MAX_REGS {
            return None;
        }
        let code = &p.code;
        let term = code
            .iter()
            .position(|i| matches!(i, I::Return { .. } | I::ReturnUndefined))?;
        for (ix, instr) in code[..term].iter().enumerate() {
            // A `SuperSet` is the evaluator's ONLY committing side effect. To keep
            // the "DEOPT only before any side effect" guarantee airtight, it may be
            // followed ONLY by the terminator (Return/RetU) — never by another op
            // that could itself decline at run time (which, after the super-set had
            // committed, would double-run it on the frame-call fallback). A trivial
            // `set x(v){ super.x = … }` always has this shape; anything else
            // declines the whole body here, before any execution.
            if matches!(instr, I::SuperSet { .. }) && ix + 1 != term {
                return None;
            }
            match *instr {
                // Pure value ops the evaluator implements.
                I::LoadInt { .. } | I::LoadBool { .. } | I::Move { .. } => {}
                I::LoadConst { idx, .. } => {
                    // Only numeric constants (the arithmetic ops require numbers;
                    // a string/heap const would only be a `+` concat operand,
                    // which we decline — `add_values` on a heap operand could run
                    // user `valueOf`).
                    match p.constants.get(idx as usize) {
                        Some(c) if c.is_number() => {}
                        _ => return None,
                    }
                }
                // `this.<field>` (and ONLY `this`): an own-data read at run time
                // (validated per-execution); any other `obj` declines.
                I::GetProp { obj: 0, .. } => {}
                // Arithmetic — admitted; per-execution the evaluator declines to
                // a frame call if an operand isn't already a number.
                I::Add { .. }
                | I::Sub { .. }
                | I::Mul { .. }
                | I::Div { .. }
                | I::Mod { .. }
                | I::AddInt { .. }
                | I::Neg { .. }
                | I::Bitwise { .. } => {}
                // `super.m(args)` — resolved + evaluated at run time.
                I::SuperMethod { .. } => {}
                // `super.<name>` read — resolved + read off-frame at run time via
                // `ic_super_get` (live, version-guarded). Pure (a read), so admitting
                // it anywhere in the straight-line prefix is effect-free.
                I::SuperGet { .. } => {}
                // `super.<name> = val` write — the body's ONLY off-frame side
                // effect. Resolved via `ic_super_set` and committed exactly once at
                // run time (an inherited trivial setter over an own data slot); the
                // run-time arm commits ONLY on a known-trivial target, else declines
                // BEFORE committing. The check above guarantees this op is the LAST
                // before the terminator, so no later op can decline post-commit.
                I::SuperSet { .. } => {}
                _ => return None,
            }
        }
        Some(term + 1)
    }

    /// Pass 2 of method inlining: execute `fid`'s validated trivial body over a
    /// fresh local register window (`reg 0 = recv`, formals in `1..`, the rest
    /// undefined). `depth` bounds nested `super` recursion. Returns the result
    /// bits, `None` (a per-execution decline — an op's operand wasn't the
    /// expected number / own-data slot; the caller frame-calls the WHOLE method,
    /// and since no super op had committed yet this is effect-free), or
    /// `Some(CALL_THREW)` (a nested super target threw).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[allow(clippy::too_many_arguments)]
    fn run_method_inline(
        &mut self,
        fid: u32,
        recv: Value,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
        body_len: usize,
        depth: u32,
    ) -> Option<u64> {
        use crate::bytecode::Instr as I;
        use crate::vm::helpers_misc::BigOp;
        let p = self.func(fid as usize);
        // Local register window on the STACK — NO heap allocation per call (the
        // frame-call path it replaces reuses the pinned reg file; an allocation
        // here would be far slower than the frame call it elides). `reg_count`
        // is bounded ≤ MI_MAX_REGS in pass 1. this in reg 0, positional args in
        // 1.., the rest undefined (mirrors setup_call's zero-fill). `code`/
        // `constants`/`string_constants` are `&'p` — they outlive `&mut self`.
        let code: &'p [Instr] = &p.code;
        let consts = &p.constants;
        let mut regs = [Value::UNDEFINED; Self::MI_MAX_REGS];
        regs[0] = recv;
        let nargs = (argc as usize).min(p.param_count as usize);
        for i in 0..nargs {
            regs[1 + i] = self.get(caller_base, arg_base + i as u16);
        }
        // Helper: numeric fast paths matching the interpreter ops EXACTLY; a
        // non-numeric operand declines (None) so no observable coercion runs.
        for (body_ip, instr) in code[..body_len].iter().enumerate() {
            match *instr {
                I::LoadInt { dst, val } => regs[dst as usize] = Value::int(val),
                I::LoadBool { dst, val } => regs[dst as usize] = Value::bool(val),
                I::LoadConst { dst, idx } => {
                    regs[dst as usize] = *consts.get(idx as usize)?;
                }
                I::Move { dst, src } => regs[dst as usize] = regs[src as usize],
                // `obj: 0` ONLY (the `this` register): pass 1 admits a GetProp
                // solely when `obj == 0`, and this arm reads `recv` (= reg 0).
                // Matching `obj: 0` here ties the read to that guarantee — any
                // future pass-1 change admitting `obj != 0` falls through to the
                // `_ => return None` decline instead of silently reading `recv`.
                I::GetProp { dst, obj: 0, name } => {
                    // `this.<field>` — own DATA slot only (a missing / accessor /
                    // inherited field needs full get_member semantics → decline).
                    if !recv.is_heap() || !self.ic_obj_ok(recv.heap_index()) {
                        return None;
                    }
                    let key = &p.string_constants[name as usize];
                    let m = match self.heap.get(recv.heap_index()) {
                        HeapObj::Object(m) if !m.is_ctor => m,
                        _ => return None,
                    };
                    let s = m.pos(key)?;
                    if m.attrs[s].accessor {
                        return None;
                    }
                    regs[dst as usize] = m.vals[s];
                }
                I::Add { dst, a, b } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    regs[dst as usize] = self.mi_add(va, vb)?;
                }
                I::Sub { dst, a, b } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    regs[dst as usize] = self.mi_num_binop(BigOp::Sub, va, vb)?;
                }
                I::Mul { dst, a, b } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    regs[dst as usize] = self.mi_num_binop(BigOp::Mul, va, vb)?;
                }
                I::Div { dst, a, b } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    regs[dst as usize] = self.mi_num_binop(BigOp::Div, va, vb)?;
                }
                I::Mod { dst, a, b } => {
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    regs[dst as usize] = self.mi_num_binop(BigOp::Mod, va, vb)?;
                }
                I::Neg { dst, a } => {
                    let va = regs[a as usize];
                    regs[dst as usize] = if va.is_int() {
                        let i = va.as_int();
                        if i == 0 {
                            Value::num(-0.0)
                        } else {
                            match i.checked_neg() {
                                Some(v) => Value::int(v),
                                None => Value::num(-(i as f64)),
                            }
                        }
                    } else if va.is_double() {
                        Value::num(-va.as_f64())
                    } else {
                        return None;
                    };
                }
                I::AddInt { dst, a, imm, .. } => {
                    let va = regs[a as usize];
                    regs[dst as usize] = if va.is_int() {
                        match va.as_int().checked_add(imm) {
                            Some(v) => Value::int(v),
                            None => Value::num(va.as_int() as f64 + imm as f64),
                        }
                    } else if va.is_double() {
                        Value::num(va.as_f64() + imm as f64)
                    } else {
                        return None;
                    };
                }
                I::Bitwise { dst, a, b, op } => {
                    use crate::bytecode::BitwiseOp as B;
                    let (va, vb) = (regs[a as usize], regs[b as usize]);
                    // Int fast path only — a non-int operand needs ToNumeric
                    // (observable on objects / BigInt) → decline to the frame call.
                    if !va.is_int() || !vb.is_int() {
                        return None;
                    }
                    let (x, y) = (va.as_int(), vb.as_int());
                    regs[dst as usize] = match op {
                        B::And => Value::int(x & y),
                        B::Or => Value::int(x | y),
                        B::Xor => Value::int(x ^ y),
                        B::Shl => Value::int(x.wrapping_shl((y as u32) & 31)),
                        B::Shr => Value::int(x >> ((y as u32) & 31)),
                        B::Ushr => {
                            let u = (x as u32) >> ((y as u32) & 31);
                            if u <= i32::MAX as u32 {
                                Value::int(u as i32)
                            } else {
                                Value::num(u as f64)
                            }
                        }
                    };
                }
                I::SuperMethod { dst, home_class_id, name, argc: sargc, .. } => {
                    let bits = self.mi_super_call(
                        fid, body_ip, home_class_id, name, sargc, recv, depth,
                    )?;
                    // A nested super target threw — propagate (the region exits;
                    // never re-executed). `CALL_THREW`/`SELF_CALL_DEOPT` are NaN-
                    // tagged sentinels never produced as a real result.
                    if bits == crate::codegen::CALL_THREW
                        || bits == crate::codegen::SELF_CALL_DEOPT
                    {
                        // DEOPT here would re-run the WHOLE method (incl. the
                        // super call) in the interpreter — but a super target that
                        // declined off-frame was ALREADY run by a real frame call
                        // (a committed effect), so we must NOT redo it. The only
                        // SELF_CALL_DEOPT path inside mi_super_call is BEFORE it
                        // runs anything (resolution miss / depth cap), so a
                        // SELF_CALL_DEOPT here means nothing committed → safe to
                        // decline the whole method.
                        if bits == crate::codegen::SELF_CALL_DEOPT {
                            return None;
                        }
                        return Some(crate::codegen::CALL_THREW);
                    }
                    regs[dst as usize] = Value::from_bits(bits);
                }
                I::SuperGet { dst, home_class_id, name } => {
                    let v = self.mi_super_get(fid, body_ip, home_class_id, name, recv)?;
                    regs[dst as usize] = v;
                }
                I::SuperSet { home_class_id, name, val } => {
                    // The body's only off-frame side effect. Commits exactly once
                    // (an inherited trivial setter over recv's own data slot) or
                    // declines BEFORE committing (None).
                    let value = regs[val as usize];
                    self.mi_super_set(fid, body_ip, home_class_id, name, recv, value)?;
                }
                I::Return { src } => return Some(regs[src as usize].bits()),
                I::ReturnUndefined => return Some(Value::UNDEFINED.bits()),
                // Unreachable: pass 1 admitted only the ops above.
                _ => return None,
            }
        }
        // The body ended without an explicit Return op (terminator was the last
        // op handled above) — defensively return undefined.
        Some(Value::UNDEFINED.bits())
    }

    /// `+` for the off-frame method evaluator: the interpreter's `Add` number
    /// fast paths EXACTLY (int+int with overflow → double; double+double). A
    /// non-number operand declines (None) — full `add_values` would run
    /// observable `ToPrimitive`/`valueOf` / build a string, which belongs on the
    /// frame call (so a later op declining can never double-apply it).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn mi_add(&mut self, va: Value, vb: Value) -> Option<Value> {
        if va.is_int() && vb.is_int() {
            return Some(match va.as_int().checked_add(vb.as_int()) {
                Some(v) => Value::int(v),
                None => Value::num(va.as_int() as f64 + vb.as_int() as f64),
            });
        }
        if va.is_number() && vb.is_number() {
            return Some(Value::num(va.as_f64() + vb.as_f64()));
        }
        None
    }

    /// `Sub`/`Mul`/`Div`/`Mod` for the off-frame evaluator: the interpreter's
    /// number fast paths EXACTLY. A non-number operand declines (None) — its
    /// `numeric_binop` slow path can run observable coercion, so it belongs on
    /// the frame call.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn mi_num_binop(&mut self, op: crate::vm::helpers_misc::BigOp, va: Value, vb: Value) -> Option<Value> {
        use crate::vm::helpers_misc::BigOp;
        match op {
            BigOp::Sub => {
                if va.is_int() && vb.is_int() {
                    Some(match va.as_int().checked_sub(vb.as_int()) {
                        Some(v) => Value::int(v),
                        None => Value::num(va.as_int() as f64 - vb.as_int() as f64),
                    })
                } else if va.is_number() && vb.is_number() {
                    Some(Value::num(va.as_f64() - vb.as_f64()))
                } else {
                    None
                }
            }
            BigOp::Mul => {
                if va.is_int() && vb.is_int() {
                    Some(match va.as_int().checked_mul(vb.as_int()) {
                        Some(v) => Value::int(v),
                        None => Value::num(va.as_int() as f64 * vb.as_int() as f64),
                    })
                } else if va.is_number() && vb.is_number() {
                    Some(Value::num(va.as_f64() * vb.as_f64()))
                } else {
                    None
                }
            }
            BigOp::Div => {
                if va.is_number() && vb.is_number() {
                    Some(Value::num(va.as_f64() / vb.as_f64()))
                } else {
                    None
                }
            }
            BigOp::Mod => {
                if va.is_number() && vb.is_number() {
                    Some(Value::num(va.as_f64() % vb.as_f64()))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Resolve + evaluate a nested `super.m(args)` for the off-frame method
    /// evaluator. Resolution uses the SAME `ic_super_method` cache the
    /// interpreter uses; the resolved target runs off-frame (recursively,
    /// depth-bounded) when trivial, else via a real `jit_frame_call`. `home_fid`
    /// is the function whose body contains this `super` (its `super_static`
    /// flag + `string_constants` drive resolution). Returns result bits,
    /// `SELF_CALL_DEOPT` (resolution miss / depth cap — NOTHING committed, the
    /// caller may decline the whole method), or `CALL_THREW`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[allow(clippy::too_many_arguments)]
    fn mi_super_call(
        &mut self,
        home_fid: u32,
        super_ip: usize,
        home_class_id: u32,
        name: u32,
        s_argc: u16,
        recv: Value,
        depth: u32,
    ) -> Option<u64> {
        use crate::codegen::SELF_CALL_DEOPT;
        if depth >= Self::METHOD_INLINE_MAX_SUPER {
            return Some(SELF_CALL_DEOPT);
        }
        let hp = self.func(home_fid as usize);
        let is_static = hp.super_static;
        let key: &'p str = &hp.string_constants[name as usize];
        // Same per-site IC the interpreter's SuperMethod arm uses, keyed by the
        // ACTUAL `(home_fid, super_ip)` of this `super.m()` op — so it shares the
        // exact cache the interpreter fills when it runs this op via run_loop
        // (no synthetic-key collision with another site in the same function).
        // `ic_super_method` re-validates the full home-value + version-guarded
        // chain on every hit, so a miss/stale entry resolves correctly.
        let (fid, closure, _callee) =
            match self.ic_super_method(home_fid, super_ip, home_class_id, is_static, key) {
                Some(t) => t,
                // Resolution miss / not a plain user fn (accessor/builtin/native):
                // NOTHING committed yet — signal the caller to decline the whole
                // method to a clean frame call.
                None => return Some(SELF_CALL_DEOPT),
            };
        let _ = closure;
        // The super target MUST itself be inline-eligible, or we DECLINE the whole
        // method (SELF_CALL_DEOPT, nothing committed) so the caller frame-calls it
        // ONCE — we never commit a partial super effect off-frame and then risk a
        // later op declining (which would double-run the super target). 0-arg
        // super calls dominate (every `super.area()`); a target with formal args
        // is supported via the local args window.
        let blen = match self.method_body_inlinable(fid) {
            Some(b) => b,
            None => return Some(SELF_CALL_DEOPT),
        };
        // Only 0-arg super targets run off-frame (every `super.area()` is 0-arg).
        // A super call WITH arguments declines the whole method to a clean frame
        // call (nothing committed) rather than staging args into the pinned
        // register file (which could realloc near capacity). Rare in practice.
        if s_argc != 0 {
            return Some(SELF_CALL_DEOPT);
        }
        self.run_method_inline(fid, recv, 0, 0, 0, blen, depth + 1)
    }

    /// Resolve + read a nested `super.<name>` (a `SuperGet` op) for the off-frame
    /// accessor/method evaluator. Resolution uses the SAME `ic_super_get` cache the
    /// interpreter's `SuperGet` arm uses (live home-class value + version-guarded
    /// hop chain via `ic_super_chain_ok`), keyed by the ACTUAL `(home_fid,
    /// super_ip)` of this op. Serves the read OFF-FRAME only when the resolved super
    /// property is:
    ///   * a DATA slot on the super chain (`GetAct::Value` — byte-identical), or
    ///   * an ACCESSOR whose getter is the trivial `return this.<field>` shape over
    ///     `recv`'s own data slot (`accessor_fast_get`, evaluated with the SAME
    ///     `this = recv` the interpreter's `GetAct::Accessor` frame-call would use).
    /// Returns the value, or `None` to DECLINE the whole accessor/method to a clean
    /// frame call (resolution miss / a non-trivial getter / a non-instance recv).
    /// A `SuperGet` is a pure read — declining commits nothing.
    ///
    /// SOUNDNESS: a `super` reference ALWAYS reads from the home object's prototype
    /// (the super base), never the receiver, so an own property of `recv` cannot
    /// shadow it — correctness comes entirely from the version-guarded `ic_super_get`
    /// chain (e.g. `Object.setPrototypeOf(C.prototype, X)` bumps the anchor hop's
    /// version → the cached entry is rejected → re-resolved). The receiver-side G3b
    /// own-shadow guard on the OUTER accessor name was already enforced by
    /// `ic_get_prop`/`ic_call_method` before this evaluator ran.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn mi_super_get(
        &mut self,
        home_fid: u32,
        super_ip: usize,
        home_class_id: u32,
        name: u32,
        recv: Value,
    ) -> Option<Value> {
        use crate::vm::ic::GetAct;
        let hp = self.func(home_fid as usize);
        let is_static = hp.super_static;
        let key: &'p str = &hp.string_constants[name as usize];
        match self.ic_super_get(home_fid, super_ip, home_class_id, is_static, key) {
            // Inherited DATA slot — byte-identical to the interpreter's data read.
            GetAct::Value(v) => Some(v),
            // Inherited ACCESSOR resolved to a plain getter: serve it off-frame ONLY
            // if it is the trivial `return this.<field>` shape over recv's own data
            // slot. The interpreter frame-calls it with `this = recv`, so reading
            // recv's own field is byte-identical. Anything else → decline (the whole
            // accessor frame-calls; nothing committed).
            GetAct::Accessor { fid, .. } => {
                self.accessor_fast_get(fid, recv).map(Value::from_bits)
            }
            // No usable resolution (the interpreter would take its own slow path
            // which can differ) → decline. Nothing committed.
            GetAct::None => None,
        }
    }

    /// Resolve + perform a nested `super.<name> = value` (a `SuperSet` op) for the
    /// off-frame accessor/method evaluator — the body's ONLY off-frame side effect.
    /// Resolution uses the SAME `ic_super_set` cache the interpreter's `SuperSet`
    /// arm uses (live + version-guarded). Commits the write OFF-FRAME exactly once,
    /// and ONLY when the super chain exposes an inherited SETTER whose body is the
    /// trivial `this.<field> = arg` / `this.<field> = (arg | 0)` shape over `recv`'s
    /// own writable data slot (`accessor_fast_set`, with `this = recv` — exactly the
    /// interpreter's `SetAct::Setter` frame-call). Returns `Some(())` on the served
    /// write, or `None` to DECLINE to a clean frame call (resolution miss / a non-
    /// trivial setter / a non-number value where `arg | 0` would coerce / the spec's
    /// write-to-RECEIVER case where no inherited setter exists — that goes through
    /// full `set_prop` semantics).
    ///
    /// SOUNDNESS: `accessor_fast_set` declines (`None`) BEFORE any store when the
    /// field isn't an own writable data slot or when `arg | 0` would coerce a non-
    /// number (observable `valueOf`) — so the only committing path is an in-place
    /// data store, byte-identical to the frame-called setter, and a decline leaves
    /// the world untouched (the caller frame-calls the whole accessor once). `Done`
    /// from `ic_super_set` never happens for a write (only `Setter`/`None`): a super
    /// data write targets the RECEIVER, which `ic_super_set` reports as `None`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn mi_super_set(
        &mut self,
        home_fid: u32,
        super_ip: usize,
        home_class_id: u32,
        name: u32,
        recv: Value,
        value: Value,
    ) -> Option<()> {
        use crate::vm::ic::SetAct;
        let hp = self.func(home_fid as usize);
        let is_static = hp.super_static;
        let key: &'p str = &hp.string_constants[name as usize];
        match self.ic_super_set(home_fid, super_ip, home_class_id, is_static, key) {
            SetAct::Setter { fid, .. } => {
                // Trivial inherited setter over recv's own data slot only; else
                // decline. `accessor_fast_set` is the SAME single-commit helper the
                // non-super setter fast path uses (in-place store, no shape change).
                self.accessor_fast_set(fid, recv, value).map(|_| ())
            }
            // `Done` (an own data slot was written) never occurs for a SUPER set:
            // ic_super_set only caches inherited SETTERS; a data write goes to the
            // receiver and is reported as `None`. `None` → the receiver-write slow
            // path (could add a slot / hit a receiver setter / no-op when frozen) —
            // decline to the frame call. Nothing committed.
            SetAct::Done | SetAct::None => None,
        }
    }

    /// Recognise a trivial getter body `return this.<field>` and return the
    /// field name (a `'p`-lived string constant). The shape is exactly a single
    /// `GetProp` of register-0 (`this`) followed by its `Return`, optionally with
    /// the compiler's trailing dead `ReturnUndefined`. Anything else → `None`
    /// (the caller frame-calls). Excludes generators/async/non-strict-irrelevant
    /// — a class getter is always a concise method (strict, no rest/arguments).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn simple_getter_field(&self, fid: u32) -> Option<&'p str> {
        let p = self.func(fid as usize);
        if p.is_generator || p.is_async || p.param_count != 0 {
            return None;
        }
        let c = &p.code;
        // [GetProp{dst, obj:0, name:N}, Return{src:dst}, ...]
        let (dst0, name) = match c.first()? {
            Instr::GetProp { dst, obj: 0, name } => (*dst, *name),
            _ => return None,
        };
        match c.get(1)? {
            Instr::Return { src } if *src == dst0 => {}
            _ => return None,
        }
        Some(&p.string_constants[name as usize])
    }

    /// Recognise a trivial setter body `this.<field> = arg` or
    /// `this.<field> = (arg | 0)` (the `x | 0` int-coercion the bench uses) and
    /// return `(field_name, applies_ToInt32)`. The recognised shapes are exactly:
    ///   * `[SetProp{obj:0, name:N, val:1}, ReturnUndefined?]`        (plain)
    ///   * `[LoadInt{dst:D, val:0}, Bitwise{dst:S, a:1, b:D, op:Or},
    ///      SetProp{obj:0, name:N, val:S}, ReturnUndefined?]`         (`arg | 0`)
    /// where register 1 is the single formal parameter (`arg`). Anything else →
    /// `None`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn simple_setter_field(&self, fid: u32) -> Option<(&'p str, bool)> {
        use crate::bytecode::BitwiseOp;
        let p = self.func(fid as usize);
        if p.is_generator || p.is_async || p.param_count != 1 {
            return None;
        }
        let c = &p.code;
        // Plain `this.field = arg` (val register == the formal param, reg 1).
        if let Instr::SetProp { obj: 0, name, val: 1 } = c.first()? {
            return Some((&p.string_constants[*name as usize], false));
        }
        // `this.field = (arg | 0)`: LoadInt 0 → Bitwise Or(arg, 0) → SetProp.
        let (zero_dst, zero_val) = match c.first()? {
            Instr::LoadInt { dst, val } => (*dst, *val),
            _ => return None,
        };
        if zero_val != 0 {
            return None;
        }
        let or_dst = match c.get(1)? {
            Instr::Bitwise { dst, a: 1, b, op: BitwiseOp::Or } if *b == zero_dst => *dst,
            _ => return None,
        };
        match c.get(2)? {
            Instr::SetProp { obj: 0, name, val } if *val == or_dst => {
                Some((&p.string_constants[*name as usize], true))
            }
            _ => None,
        }
    }

    /// Frame-call a resolved plain user function FROM NATIVE REGION CODE and
    /// run it to completion: the shared tail of the region call helpers and the
    /// property-slow helpers (getters/setters). Returns the result bits,
    /// `SELF_CALL_DEOPT` (setup failed without materializing a JS error — the
    /// interpreter re-executes the op), or `CALL_THREW` (`pending_throw` set;
    /// the region exits and the interpreter unwinds). `dst` = 0 is never
    /// written: `run_loop` returns the stop frame's value BEFORE delivering to
    /// `ret_dst`; the native caller stores/discards it itself.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn jit_frame_call(
        &mut self,
        fid: u32,
        closure: u32,
        this_v: Value,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
        ip: usize,
        callee_v: Value,
    ) -> u64 {
        use crate::codegen::{CALL_THREW, SELF_CALL_DEOPT};
        if self
            .setup_call(fid, closure, this_v, caller_base, arg_base, argc, 0, ip + 1, callee_v)
            .is_err()
        {
            // MAX_FRAMES / regs overflow / this-boxing error. If a JS error
            // value was already materialized, unwind (never re-execute);
            // otherwise let the interpreter re-execute the op and surface the
            // identical error itself.
            return if self.pending_throw.is_some() {
                self.osr_deopt_exempt = true;
                CALL_THREW
            } else {
                SELF_CALL_DEOPT
            };
        }
        let stop = self.frames.len() - 1;
        self.jit_call_depth += 1;
        let r = self.run_loop(stop);
        self.jit_call_depth -= 1;
        match r {
            Ok(v) => v.bits(),
            Err(_) => {
                // The callee threw and nothing below `stop` handled it:
                // `pending_throw` is set, the callee frames are unwound, and the
                // region must EXIT so the interpreter unwinds the region's own
                // frame (its try handlers were pushed before the loop). A throw
                // is not a region-quality signal — exempt the deopt counter.
                self.osr_deopt_exempt = true;
                CALL_THREW
            }
        }
    }

    /// The implementation behind `jit_get_prop_slow` / `jit_set_prop_slow`: a
    /// region's prop-miss helper found a resolution only the interpreter's
    /// per-site IC machinery can serve (an accessor needing a frame call, or a
    /// class-instance receiver). Consults `ic_get_prop` / `ic_set_prop` — the
    /// SAME caches the interpreter uses — and frame-calls plain getters and
    /// setters to completion. Returns the value bits (get) or 0 (set),
    /// `SELF_CALL_DEOPT` (no IC resolution — the interpreter re-executes the
    /// op), or `CALL_THREW`. Reentrancy contract: as `jit_region_call_impl`
    /// (the calling region re-derives r13/r14 after this helper).
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) fn jit_prop_slow_impl(
        &mut self,
        caller_base_ptr: *const u64,
        packed_fip: u64,
        packed2: u64,
        is_set: bool,
    ) -> u64 {
        use crate::codegen::{CALL_THREW, SELF_CALL_DEOPT};
        use crate::vm::ic::{GetAct, SetAct};
        if self.jit_call_depth >= JIT_REGION_CALL_MAX {
            self.osr_deopt_exempt = true;
            return SELF_CALL_DEOPT;
        }
        let func_id = (packed_fip >> 32) as u32;
        let ip = (packed_fip as u32) as usize;
        let name = (packed2 >> 32) as u32;
        let regs_base = self.regs.as_ptr() as *const u64;
        // SAFETY: caller_base_ptr lies within self.regs' pinned buffer.
        let base = unsafe { caller_base_ptr.offset_from(regs_base) } as usize;
        let key: &str = &self.func(func_id as usize).string_constants[name as usize];
        if is_set {
            let obj_reg = ((packed2 >> 16) & 0xFFFF) as u16;
            let val_reg = (packed2 & 0xFFFF) as u16;
            let recv = self.get(base, obj_reg);
            let val = self.get(base, val_reg);
            match self.ic_set_prop(func_id, ip, recv, key, val) {
                SetAct::Done => 0,
                SetAct::Setter { fid, closure, setter } => {
                    // Q7 S-ACC: a trivial `this.field = arg|0` setter over an own
                    // writable data slot is served off-frame (no setup_call /
                    // run_loop) — the dominant cost of the rt loop.
                    if let Some(r) = self.accessor_fast_set(fid, recv, val) {
                        return r;
                    }
                    // S2 SUPER-ACC: a trivial setter body whose only effect is a
                    // `super.<name> = …` over an inherited trivial setter (e.g.
                    // `set v(x){ super.v = x }`) runs off-frame via the two-pass
                    // method-inline evaluator (pass 1 validates the WHOLE body before
                    // pass 2 commits the single super-set). `argc = 1`, the value in
                    // `val_reg`. `None` ⇒ a non-trivial body ⇒ frame-call below.
                    match self.try_method_inline(fid, recv, base, val_reg, 1) {
                        Some(CALL_THREW) | Some(SELF_CALL_DEOPT) => {
                            // A nested super target threw / declined mid-flight.
                            // CALL_THREW: a committed throw — propagate (region
                            // exits). SELF_CALL_DEOPT cannot escape try_method_inline
                            // (its arms convert it to None), but propagate defensively.
                            return CALL_THREW;
                        }
                        Some(_) => return 0, // served — the setter's return is discarded
                        None => {}           // not inlinable — fall through to frame call
                    }
                    let r = self.jit_frame_call(fid, closure, recv, base, val_reg, 1, ip, setter);
                    if r == CALL_THREW || r == SELF_CALL_DEOPT {
                        r
                    } else {
                        0 // the setter's return value is discarded
                    }
                }
                SetAct::None => SELF_CALL_DEOPT,
            }
        } else {
            let obj_reg = (packed2 & 0xFFFF) as u16;
            let recv = self.get(base, obj_reg);
            match self.ic_get_prop(func_id, ip, recv, key) {
                GetAct::Value(v) => v.bits(),
                GetAct::Accessor { fid, closure, getter } => {
                    // Q7 S-ACC: a trivial `return this.field` getter over an own
                    // data slot is served off-frame (no setup_call / run_loop).
                    if let Some(bits) = self.accessor_fast_get(fid, recv) {
                        return bits;
                    }
                    // S2 SUPER-ACC: a trivial getter body containing a `super.<name>`
                    // read (e.g. `get v(){ return super.v * 2 }`) runs off-frame via
                    // the two-pass method-inline evaluator (`argc = 0`). It resolves
                    // the super read with the version-guarded `ic_super_get` and reads
                    // an inherited data slot / trivial super-getter field directly.
                    // `None` ⇒ a non-trivial body ⇒ frame-call below.
                    if let Some(bits) = self.try_method_inline(fid, recv, base, 0, 0) {
                        return bits;
                    }
                    self.jit_frame_call(fid, closure, recv, base, 0, 0, ip, getter)
                }
                GetAct::None => SELF_CALL_DEOPT,
            }
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
        self.frames.push(Frame { super_done: false, args_obj: u32::MAX, eval_scope: u32::MAX, func: 0, base, ip: 0, ret_dst: 0, closure: NO_CLOSURE, handlers: Vec::new(), new_target: Value::UNDEFINED, callee: Value::UNDEFINED });
        // Everything allocated so far (interned strings, all built-ins, hoisted
        // top-level functions) is pinned: the GC never collects below this floor.
        self.set_gc_floor();
        // Run until the top-level frame returns (frames drains back to 0), then
        // run the event loop: drain queued microtasks (promise reactions, async
        // resumes) to empty. Drains even on a main throw (matches node ordering),
        // then returns the original result.
        let main = self.run_loop(0);
        self.run_event_loop();
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
        self.run_event_loop();
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
        self.run_event_loop();
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
                // The wrapper's CALLER realm: a wrapper built by a createRealm
                // child's `evaluate` re-wraps callable results (and throws its
                // boundary TypeErrors) with the CHILD's identities.
                let wr = self.get_function_realm(callee);
                let prev_ncr = self.native_callee_realm;
                let adopt = |vm: &mut Self, msg: &str| {
                    let e = vm.alloc_error_from_message(msg);
                    if wr != 0 {
                        vm.realm_adopt_error_to(e, wr);
                    }
                    vm.pending_throw = Some(e);
                };
                // ARGUMENTS wrap into the TARGET (shadow) realm — main-modeled,
                // so no realm context; boundary TypeErrors still carry the
                // CALLER realm's identity.
                self.native_callee_realm = None;
                let mut wargs = Vec::with_capacity(args.len());
                for &a in args {
                    match self.wrap_realm_value(a) {
                        Ok(w) => wargs.push(w),
                        Err(t) => {
                            self.native_callee_realm = prev_ncr;
                            if self.pending_throw.is_none() {
                                adopt(self, &t.0);
                            }
                            return Err(t);
                        }
                    }
                }
                self.native_callee_realm = prev_ncr;
                // OrdinaryCallEvaluateBody runs in the TARGET's realm: a
                // ShadowRealm-born callable re-enters with ITS realm active so
                // `globalThis.x` in the body binds that realm's slots; an
                // untagged (main-realm) target runs with no realm active.
                let prev_ar = self.active_realm;
                self.active_realm = self.shadow_fn_realm.get(&t.heap_index()).copied();
                let call_res = self.call_value(t, Value::UNDEFINED, &wargs);
                self.active_realm = prev_ar;
                let res = match call_res {
                    Ok(v) => {
                        // The RESULT wraps back into the CALLER realm — the
                        // realm of the wrapper being invoked.
                        self.native_callee_realm = (wr != 0).then_some(wr);
                        let w = self.wrap_realm_value(v);
                        self.native_callee_realm = prev_ncr;
                        match w {
                            Ok(w) => Ok(w),
                            Err(t) => {
                                if self.pending_throw.is_none() {
                                    adopt(self, &t.0);
                                }
                                Err(t)
                            }
                        }
                    }
                    Err(_) => {
                        self.pending_throw.take();
                        let msg =
                            "TypeError: WrappedFunction call threw (error wrapped at the realm boundary)";
                        adopt(self, msg);
                        Err(Thrown(msg.into()))
                    }
                };
                return res;
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
            // A createRealm child's `eval` / `evalScript`: run the code against
            // the CHILD realm's global bindings (the active_realm switch, the
            // same machinery ShadowRealm.prototype.evaluate uses).
            if !self.realm_fns.is_empty() {
                if let Some(&(gidx, kind)) = self.realm_fns.get(&callee.heap_index()) {
                    return self.realm_eval_call(gidx, kind, args);
                }
            }
            if let HeapObj::Native(id) = self.heap.get(callee.heap_index()) {
                let id = *id;
                if !self.realm_global_objs.is_empty() {
                    // A realm-COPIED builtin (`other.Function.prototype.apply`,
                    // `other.RegExp.prototype` flag getters, …): run with the
                    // COPY's realm as the native-callee context (HOME-object
                    // checks resolve against the realm's image), and an internal
                    // throw from it carries the CHILD's error-constructor
                    // identity (the spec's realm of the throwing function).
                    let r = self.get_function_realm(callee);
                    let prev = self.native_callee_realm;
                    self.native_callee_realm = (r != 0).then_some(r);
                    let res = self.call_native(id, this, args);
                    self.native_callee_realm = prev;
                    if r != 0 {
                        if let Err(ref t) = res {
                            if self.pending_throw.is_none() {
                                let e = self.alloc_error_from_message(&t.0);
                                self.realm_adopt_error_to(e, r);
                                self.pending_throw = Some(e);
                            }
                        }
                    }
                    return res;
                }
                return self.call_native(id, this, args);
            }
        }
        // A native resolve/reject function settles its bound promise.
        if callee.is_heap() {
            if let HeapObj::BoundResolver { promise, is_reject, pair } = self.heap.get(callee.heap_index()) {
                let (p, isr, pr) = (*promise, *is_reject, *pair);
                let arg = args.first().copied().unwrap_or(Value::UNDEFINED);
                // [[AlreadyResolved]]: only the pair's FIRST call acts.
                if self.resolver_pair_fire(pr) {
                    if isr {
                        self.reject(p, arg);
                    } else {
                        self.resolve(p, arg);
                    }
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
        // the result with the realm. `other.Function(src)` compiles `src` with
        // the CHILD realm active so its globals bind in the child's table.
        if callee.is_heap() {
            if let Some(&main) = self.realm_ctor_main.get(&callee.heap_index()) {
                let cr = self.get_function_realm(callee);
                let prev_realm = self.active_realm;
                if self.realm_main_ctor_is_fn_like(main) {
                    if let Some(g) = self.realm_global_obj(cr) {
                        self.active_realm = Some(g);
                    }
                }
                let r = self.call_value(Value::heap(main), this, args);
                self.active_realm = prev_realm;
                let r = r?;
                if r.is_heap() && cr != 0 {
                    self.obj_realm.insert(r.heap_index(), cr);
                    // `other.Object(primitive)` boxes with the REALM's wrapper
                    // prototype (no-op for a non-Boxed result).
                    self.realm_box_proto(r, cr);
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
        let (func_id, closure) = self.resolve_callable_realm(callee)?;
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
            // OrdinaryCallBindThis: the global of the CALLEE's realm — a function
            // born in a $262.createRealm child binds the CHILD's global object.
            Value::heap(self.callee_this_global(callee))
        } else if !is_strict && !self.is_object_value(this) && self.global_this != 0 {
            // OrdinaryCallBindThis: a sloppy function boxes a primitive `this`
            // (number/string/boolean/symbol/bigint) to its wrapper object —
            // with the CALLEE realm's wrapper prototype for a realm function.
            let b = self.to_object(this)?;
            self.realm_retag_boxed(callee, b);
            b
        } else {
            this
        };
        // An `async function*` builds a suspended AsyncGenerator (an async
        // iterator); it doesn't run until `.next()` (but its parameter prologue
        // runs eagerly here, so a destructuring throw propagates from the call).
        if is_gen && is_async {
            self.pending_gen_callee = callee;
            return self.alloc_async_generator(func_id, closure, this, args);
        }
        // Calling a generator function builds a suspended Generator, not a frame.
        // (The parameter prologue runs eagerly here, so a destructuring throw
        // propagates from the call.)
        if is_gen {
            self.pending_gen_callee = callee;
            return self.alloc_generator(func_id, closure, this, args);
        }
        // Calling an async function runs synchronously up to the first `await`,
        // then returns its result Promise.
        if is_async {
            self.pending_gen_callee = callee;
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
        let mut args_obj = u32::MAX;
        if let Some(areg) = arguments_reg {
            let (is_strict, simple) = {
                let p = self.func(func_id as usize);
                (p.is_strict, p.simple_params)
            };
            // Sloppy + simple params ⇒ MAPPED: aliases the formal registers of
            // the frame about to be pushed (frames.len() is its index).
            let mapinfo =
                (!is_strict && simple).then(|| (self.frames.len(), new_base, callee_params));
            let arr = self.build_arguments_object(args.to_vec(), callee, is_strict, mapinfo);
            self.regs[new_base + areg as usize] = arr;
            if mapinfo.is_some() {
                args_obj = arr.heap_index();
            }
        }

        let stop_depth = self.frames.len();
        let new_target = std::mem::replace(&mut self.pending_new_target, Value::UNDEFINED);
        self.frames.push(Frame { super_done: false, args_obj, eval_scope: u32::MAX, func: func_id, base: new_base, ip: 0, ret_dst: 0, closure, handlers: Vec::new(), new_target, callee });
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
    /// Get-or-create the live global slot for `name` in ShadowRealm `rid`'s
    /// own binding table (fresh slots start UNINITIALIZED). For a
    /// `$262.createRealm()` child (rid = its global object's heap index) a fresh
    /// slot is SEEDED from the child global object's own property — the facade
    /// intrinsics (`Object`, `TypeError`, the realm's own `eval`/`Function`) and
    /// any value main-realm code put there (`other.x = 1`) — falling back to the
    /// shared main-realm builtin (stage-1: intrinsics without realm identity),
    /// else UNINITIALIZED (a read before any write is a ReferenceError).
    pub(crate) fn realm_global_slot(&mut self, rid: u32, name: &str) -> Result<u32, Thrown> {
        if let Some(&s) = self.realm_globals.get(&rid).and_then(|m| m.get(name)) {
            return Ok(s);
        }
        let cap = self.program.global_count + (FIELD_POOL + EVAL_POOL) as u32;
        if self.eval_global_next >= cap {
            return Err(Thrown(
                "EvalError: too many distinct globals introduced by eval".into(),
            ));
        }
        let seed = if self.realm_global_objs.contains_key(&rid) {
            let own = match self.heap.get(rid) {
                HeapObj::Object(m) => m.get(name),
                _ => None,
            };
            match own {
                Some(v) => v,
                None => match self.builtin_globals.get(name) {
                    Some(&b) => Value::heap(b),
                    None => Value::UNINITIALIZED,
                },
            }
        } else {
            Value::UNINITIALIZED
        };
        let s = self.eval_global_next;
        self.eval_global_next += 1;
        self.globals[s as usize] = seed;
        self.realm_globals.entry(rid).or_default().insert(name.to_string(), s);
        Ok(s)
    }

    pub(crate) fn eval_global_slot(&mut self, name: &str) -> Result<u32, Thrown> {
        // Code evaluating inside a ShadowRealm binds NON-BUILTIN names to the
        // realm's OWN slot table — its `var x` never collides with (or sees)
        // the incubating realm's `x`. Builtins stay shared (single-intrinsics
        // model; per-realm intrinsics are a separate feature).
        if let Some(rid) = self.active_realm {
            // A $262.createRealm child binds EVERY name (builtins included) in
            // its own table — `realm_global_slot` seeds fresh slots from the
            // child's global object, so `TypeError` resolves to the CHILD's
            // facade constructor and `var x` lands on the child global.
            if self.realm_global_objs.contains_key(&rid) {
                return self.realm_global_slot(rid, name);
            }
            if !self.builtin_globals.contains_key(name) {
                return self.realm_global_slot(rid, name);
            }
        }
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
        lexical_collisions: Vec<String>,
        caller_scope: Option<(Vec<String>, Vec<Value>)>,
        eval_scope_idx: Option<u32>,
        exact_src: Option<&[u8]>,
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
        if param_collisions.is_some() || !lexical_collisions.is_empty() {
            let src_strict = force_strict
                || ret
                    .program
                    .directives
                    .iter()
                    .any(|d| d.directive.as_str() == "use strict");
            if !src_strict {
                for n in crate::compile::eval_var_and_fn_names(&ret.program) {
                    if param_collisions.as_ref().map_or(false, |c| c.iter().any(|c| *c == n)) {
                        return Err(Thrown(format!(
                            "SyntaxError: Identifier '{n}' has already been declared"
                        )));
                    }
                    // EvalDeclarationInstantiation step 5: a var/function name
                    // colliding with a LEXICAL binding between the eval's
                    // lexEnv and its varEnv (the calling function) is a
                    // SyntaxError, before any binding is created.
                    if lexical_collisions.iter().any(|c| *c == n) {
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
        // UsingDeclaration is not allowed at eval top level (eval-code is not
        // a "using"-eligible scope: spec UsingDeclaration static semantics).
        if ret.program.body.iter().any(|s| {
            matches!(s, oxc_ast::ast::Statement::VariableDeclaration(d)
                if matches!(d.kind,
                    oxc_ast::ast::VariableDeclarationKind::Using
                        | oxc_ast::ast::VariableDeclarationKind::AwaitUsing))
        }) {
            return Err(Thrown(
                "SyntaxError: using declarations may not appear at eval top level".into(),
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
            exact_src,
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
        let f = self.do_eval(SRC, false, false, None, None, false, false, Value::UNDEFINED, None, false, None, Vec::new(), None, None, None)?;
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
        let f = self.do_eval(SRC, false, false, None, None, false, false, Value::UNDEFINED, None, false, None, Vec::new(), None, None, None)?;
        self.async_dispose_fn = Some(f);
        Ok(f)
    }

    /// Allocate a fresh live global slot from the eval pool (UNINITIALIZED),
    /// for module-loader bookkeeping (canonical namespace/source binding slots).
    fn alloc_module_shared_slot(&mut self) -> Result<u32, Thrown> {
        let cap = self.program.global_count + (FIELD_POOL + EVAL_POOL) as u32;
        if self.eval_global_next >= cap {
            return Err(Thrown(
                "EvalError: too many distinct globals introduced by eval".into(),
            ));
        }
        let s = self.eval_global_next;
        self.eval_global_next += 1;
        self.globals[s as usize] = Value::UNINITIALIZED;
        Ok(s)
    }

    /// The CANONICAL live slot of `canon`'s namespace binding, created on
    /// demand (the VALUE fills in when the namespace is registered/imported).
    /// One slot per module ⇒ slot identity == the spec's (module, ~namespace~)
    /// ResolvedBinding identity, which the `export *` ambiguity check compares.
    fn module_ns_slot(&mut self, canon: &std::path::Path) -> Result<u32, Thrown> {
        if let Some(&s) = self.module_ns_slots.get(canon) {
            return Ok(s);
        }
        let s = self.alloc_module_shared_slot()?;
        self.module_ns_slots.insert(canon.to_path_buf(), s);
        Ok(s)
    }

    /// The CANONICAL live slot of `key`'s ModuleSource binding (`import
    /// source`), creating the %AbstractModuleSource%-prototype-linked source
    /// object on first request. `key` is the canonical target path, or the
    /// synthetic `<module source>` host-module key (test262: every
    /// `<module source>` request resolves to ONE shared module record).
    fn module_source_slot(&mut self, key: &std::path::Path) -> Result<u32, Thrown> {
        if let Some(&s) = self.module_source_slots.get(key) {
            return Ok(s);
        }
        let s = self.alloc_module_shared_slot()?;
        let idx = self.heap.alloc(HeapObj::Object(crate::heap::ObjMap::new()));
        if self.abstractmodulesource_proto != 0 {
            self.proto_of
                .insert(idx, Value::heap(self.abstractmodulesource_proto));
        }
        self.globals[s as usize] = Value::heap(idx);
        self.module_source_slots.insert(key.to_path_buf(), s);
        Ok(s)
    }

    /// OWN exports (exported name → live slot) of a PREPARED module program,
    /// in source order; registers the namespace's name→slot map and the
    /// in-flight own-exports map (cycle resolution reads both).
    fn register_module_own(
        &mut self,
        ns_idx: u32,
        path: &std::path::Path,
        exports: &[(String, String)],
        names: &[String],
        gmap: &[u32],
    ) -> Vec<(String, u32)> {
        let mut full: Vec<(String, u32)> = Vec::with_capacity(exports.len());
        let mut own_map: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();
        for (exported, local) in exports {
            if let Some(i) = names.iter().position(|n| n == local) {
                full.push((exported.clone(), gmap[i]));
                own_map.insert(exported.clone(), gmap[i]);
            }
        }
        self.module_namespaces.insert(ns_idx, own_map.clone());
        self.module_own.insert(path.to_path_buf(), own_map);
        full
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
        // A typed import is a DISTINCT module record from the same file's JS
        // module (e.g. a module importing ITSELF with { type: "text" }):
        // cached under (path, type), checked before the JS cache.
        if let Some(t) = mtype {
            if let Some(&ns) = self.typed_module_cache.get(&(path.clone(), t.to_string())) {
                return Ok(ns);
            }
        }
        if mtype.is_none() {
            if let Some(&ns) = self.module_cache.get(&path) {
                // A cached module that SUSPENDED (top-level await): later
                // importers settle from the same body promise — never from
                // the incomplete namespace directly.
                if let Some(&(bp, _)) = self.module_body_promise.get(&path) {
                    let pending = bp.is_heap()
                        && matches!(
                            self.heap.get(bp.heap_index()),
                            HeapObj::Promise {
                                state: crate::heap::PromiseState::Pending,
                                ..
                            }
                        );
                    // NOT for a SELF-import from the module's own (still
                    // executing) body: chaining its import promise on its own
                    // body promise is a deadlock cycle — the spec resolves a
                    // self-import against the in-progress record directly.
                    if pending && !self.executing_modules.contains(&path) {
                        // A suspended module inside a CYCLE completes only
                        // when its CYCLE ROOT does (InnerModuleEvaluation
                        // 11.c.iv waits on requiredModule.[[CycleRoot]]):
                        // prefer a pending ANCESTOR capability (a cap-kind
                        // registration) whose request graph reaches this
                        // module.
                        let mut chosen = bp;
                        let mut candidates: Vec<(std::path::PathBuf, Value)> = Vec::new();
                        for (p2, &(b2, cap2)) in &self.module_body_promise {
                            let p2_pending = cap2
                                && *p2 != path
                                && b2.is_heap()
                                && matches!(
                                    self.heap.get(b2.heap_index()),
                                    HeapObj::Promise {
                                        state: crate::heap::PromiseState::Pending,
                                        ..
                                    }
                                );
                            if p2_pending {
                                candidates.push((p2.clone(), b2));
                            }
                        }
                        candidates.sort_by(|a, b| a.0.cmp(&b.0));
                        for (p2, b2) in candidates {
                            let mut seen = std::collections::HashSet::new();
                            if self.module_graph_reaches(&p2, &path, &mut seen) {
                                chosen = b2;
                                break;
                            }
                        }
                        self.pending_module_body = Some(chosen);
                    }
                }
                return Ok(ns);
            }
        }
        // A module that already FAILED evaluation re-throws the SAME error on
        // every later import (its abrupt completion is permanent).
        if let Some(&e) = self.module_errors.get(&path) {
            let msg = self.throw_message(e);
            self.pending_throw = Some(e);
            return Err(Thrown(msg));
        }
        // A typed import ({type:'json'|'text'|'bytes'}) builds a synthetic
        // namespace with a single `default` export; unknown types reject.
        if let Some(t) = mtype {
            let val = match t {
                // CreateBytesModule: the raw file bytes as a Uint8Array over
                // an IMMUTABLE ArrayBuffer (read as binary — a PNG fixture is
                // not UTF-8).
                "bytes" => {
                    let bytes = std::fs::read(&path)
                        .map_err(|_| Thrown("TypeError: module not found".into()))?;
                    let buf = self
                        .heap
                        .alloc(HeapObj::ArrayBuffer { data: bytes.into(), detached: false });
                    if self.arraybuffer_proto != 0 {
                        self.proto_of.insert(buf, Value::heap(self.arraybuffer_proto));
                    }
                    self.immutable_buffers.insert(buf);
                    // kind 1 = Uint8Array (TA_KINDS); view over the whole buffer.
                    self.build_typed_array(1, &[Value::heap(buf)])?
                }
                "json" | "text" => {
                    let text = std::fs::read_to_string(&path)
                        .map_err(|_| Thrown("TypeError: module not found".into()))?;
                    match t {
                        "json" => self.json_parse(text.as_bytes())?,
                        _ => self.alloc_str(text),
                    }
                }
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
            self.typed_module_cache
                .insert((path.clone(), t.to_string()), Value::heap(ns_idx));
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
        let prog = match crate::compile::compile_eval(&ret.program, &code, true, false, None, false, std::collections::HashSet::new(), true, false, Vec::new(), false, None) {
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
        // Own (exported name -> compile slot), for self-import aliasing.
        let own_cslot = |name: &str| -> Option<u32> {
            exports.iter().find(|(e, _)| e == name).and_then(|(_, local)| {
                names
                    .iter()
                    .position(|n| n == local)
                    .map(|i| i as u32)
            })
        };
        // Pre-resolved canonical target of an import specifier. The
        // self/in-flight classification below is STABLE for the whole
        // dependency loop: `module_loading` holds exactly the in-flight
        // ancestors throughout it.
        let canon_of = |dir: Option<&std::path::Path>, spec: &str| -> std::path::PathBuf {
            let raw = match dir {
                Some(d) => d.join(spec),
                None => std::path::PathBuf::from(spec),
            };
            std::fs::canonicalize(&raw).unwrap_or(raw)
        };
        use crate::bytecode::ImportName as IN;
        // EARLY-PREPARE eligibility: every Named/Default import resolves
        // WITHOUT loading a dependency (i.e. is a self-import of an own
        // export). Such a module's environment instantiates BEFORE its
        // dependencies evaluate — the spec links the whole cycle before any
        // evaluation — so a cycle dependency that calls one of our hoisted
        // functions mid-evaluation (verify-dfs) finds the real binding.
        // Modules with external Named/Default imports keep the
        // resolve-then-prepare order (their live-slot aliases need the
        // dependency loaded first); cycle members calling THEIR hoisted
        // functions before they prepare stay a known limit.
        let early_prepare = imports.iter().all(|e| match &e.import {
            IN::Named(n) => {
                e.mtype.is_none()
                    && canon_of(dir.as_deref(), &e.specifier) == path
                    && own_cslot(n).is_some()
            }
            IN::Default => {
                e.mtype.is_none()
                    && canon_of(dir.as_deref(), &e.specifier) == path
                    && own_cslot("default").is_some()
            }
            _ => true,
        });
        // Aliases resolvable BEFORE any dependency loads: a SELF-import of an
        // own export aliases the module's own compile slot (prepare's second
        // pass maps both onto ONE live slot); `import * as ns` and
        // `import source x` locals alias the target's CANONICAL shared slot
        // (slot identity == spec binding identity; the VALUES fill in below).
        for e in &imports {
            match &e.import {
                IN::Named(n)
                    if e.mtype.is_none()
                        && canon_of(dir.as_deref(), &e.specifier) == path =>
                {
                    if let Some(c) = own_cslot(n) {
                        self_aliases.insert(e.local_slot, c);
                    }
                }
                IN::Default
                    if e.mtype.is_none()
                        && canon_of(dir.as_deref(), &e.specifier) == path =>
                {
                    if let Some(c) = own_cslot("default") {
                        self_aliases.insert(e.local_slot, c);
                    }
                }
                IN::Namespace if e.mtype.is_none() => {
                    let canon = canon_of(dir.as_deref(), &e.specifier);
                    let slot = self.module_ns_slot(&canon)?;
                    import_aliases.insert(e.local_slot, slot);
                }
                IN::Source => {
                    let key = if e.specifier == "<module source>" {
                        std::path::PathBuf::from("<module source>")
                    } else {
                        canon_of(dir.as_deref(), &e.specifier)
                    };
                    let slot = self.module_source_slot(&key)?;
                    import_aliases.insert(e.local_slot, slot);
                }
                _ => {}
            }
        }
        // PRE-REGISTER this module BEFORE any dependency loads (a CYCLIC
        // re-export back into this module — a dependency doing
        // `export { x } from './me'` while we are mid-load — must resolve to
        // the real binding instead of re-evaluating this module as a second
        // instance). EARLY mode prepares the whole environment now; LATE mode
        // pre-allocates live slots for the declared exports (prepare reuses
        // them). An export whose local is itself an IMPORT binding resolves
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
        if !early_prepare {
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
        }
        let mut prog_opt = Some(prog);
        let ns_idx = self.alloc_empty_namespace();
        self.module_cache.insert(path.clone(), Value::heap(ns_idx));
        self.module_pending_reexports.insert(
            path.clone(),
            (
                reexports.clone(),
                star_reexports.clone(),
                ns_reexports.clone(),
                dir.clone(),
            ),
        );
        // EARLY mode: instantiate the environment (per-module slots, function/
        // class install, hoisting) NOW — cycle dependencies see the real
        // bindings; LATE mode registers the pre-allocated own-export slots.
        let mut gmap_base: Option<(Vec<u32>, u32)> = None;
        let mut full: Vec<(String, u32)> = Vec::new();
        if early_prepare {
            let prog = prog_opt.take().expect("module program");
            let prepared = self.prepare_eval_program(
                prog,
                true,
                None,
                false,
                None,
                if import_aliases.is_empty() { None } else { Some(&import_aliases) },
                if self_aliases.is_empty() { None } else { Some(&self_aliases) },
                None,
            );
            match prepared {
                Ok((gmap, base_func)) => {
                    let end = (self.main_func_count + self.eval_funcs.len()) as u32;
                    self.module_func_ranges.push((base_func, end, ns_idx));
                    full = self.register_module_own(ns_idx, &path, &exports, &names, &gmap);
                    gmap_base = Some((gmap, base_func));
                }
                Err(e) => {
                    self.module_cache.remove(&path);
                    self.module_namespaces.remove(&ns_idx);
                    self.module_pending_reexports.remove(&path);
                    return Err(e);
                }
            }
        } else {
            self.module_namespaces.insert(ns_idx, own_pre.clone());
            self.module_own.insert(path.clone(), own_pre);
        }
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
                // The synthetic `<module source>` host module has no file.
                if matches!(e.import, IN::Source) && e.specifier == "<module source>" {
                    continue;
                }
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
                // A TYPED import (json/text) is a DISTINCT module record even
                // for the importing file itself — never self/in-flight.
                let is_self = dep_canon == path && e.mtype.is_none();
                let in_flight =
                    !is_self && e.mtype.is_none() && self.module_loading.contains(&dep_canon);
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
                    IN::Source => {
                        // Slot + ModuleSource object were created in the
                        // pre-pass. The LOADING phase still applies to a REAL
                        // target (its request graph must resolve); the
                        // synthetic `<module source>` host module needs
                        // nothing further.
                        if e.specifier != "<module source>" && !is_self && !in_flight {
                            let mut seen = std::collections::HashSet::new();
                            self.prescan_module_requests(&dep_canon, &mut seen)?;
                        }
                    }
                    IN::DeferNamespace => {
                        let ns = self.deferred_namespace_for(&dep_raw)?;
                        ns_writes.push((e.local_slot, ns));
                    }
                    IN::SideEffect => {
                        if !is_self && !in_flight {
                            self.import_module_sync(&dep_raw, e.mtype.as_deref())?;
                        }
                    }
                    IN::Named(n) => {
                        if is_self {
                            match own_cslot(n) {
                                // An own-export self-import was aliased in
                                // the pre-pass.
                                Some(_) => {}
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
                            match self.resolve_export(&dep_raw, n, e.mtype.as_deref())? {
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
                                // Aliased in the pre-pass.
                                Some(_) => {}
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
                            match self.resolve_export(&dep_raw, "default", e.mtype.as_deref())? {
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
                        if let Some(t) = e.mtype.as_deref() {
                            // A TYPED namespace import is a DISTINCT module
                            // record (even of the importing file itself):
                            // per-local value write after prepare.
                            let ns = self.import_module_sync(&dep_raw, Some(t))?;
                            ns_writes.push((e.local_slot, ns));
                        } else {
                            // The local aliases the dependency's CANONICAL
                            // namespace slot (pre-pass); here we ensure the
                            // slot's VALUE: a self/in-flight target's
                            // namespace is already pre-registered in the
                            // cache, anything else imports (evaluates) now.
                            let slot = self.module_ns_slot(&dep_canon)?;
                            let nsv = if is_self || in_flight {
                                self.module_cache.get(&dep_canon).copied()
                            } else {
                                Some(self.import_module_sync(&dep_raw, None)?)
                            };
                            if let Some(v) = nsv {
                                self.globals[slot as usize] = v;
                            }
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
        // install funcs/classes, hoist) WITHOUT running the body yet — unless the
        // EARLY path already did, before the dependency loop. `gmap[i]` is the
        // live slot for compile-time global slot `i`. The post-prepare
        // registration REFRESHES the pre-registered maps with the final own
        // exports (an export whose local is an import binding now has its real
        // aliased slot; the namespace itself was registered before the
        // dependency loop, so cyclic re-exports already resolved against the
        // pre-allocated slots).
        let (gmap, base_func) = match gmap_base {
            Some(p) => p,
            None => {
                let prog = prog_opt.take().expect("module program");
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
                match prepared {
                    Ok((gmap, base_func)) => {
                        let end = (self.main_func_count + self.eval_funcs.len()) as u32;
                        self.module_func_ranges.push((base_func, end, ns_idx));
                        full =
                            self.register_module_own(ns_idx, &path, &exports, &names, &gmap);
                        (gmap, base_func)
                    }
                    Err(e) => {
                        cleanup_on_err(self);
                        return Err(e);
                    }
                }
            }
        };
        // Typed/deferred namespace import locals are initialized PRIOR to
        // evaluation (plain namespace locals alias their canonical shared
        // slot, written during the dependency loop).
        for (cslot, ns) in ns_writes {
            let live = gmap[cslot as usize] as usize;
            self.globals[live] = ns;
        }
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
                    self.module_body_promise.insert(path.clone(), (Value::heap(cap), true));
                    return {
                        self.module_own.remove(&path);
                        self.module_pending_reexports.remove(&path);
                        Ok(Value::heap(ns_idx))
                    };
                }
                self.executing_modules.insert(path.clone());
                self.pending_module_body_marker = true;
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
                self.executing_modules.remove(&path);
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
                                self.module_errors.insert(path.clone(), r);
                                self.pending_throw = Some(r);
                                Err(Thrown(msg))
                            }
                            Some((crate::heap::PromiseState::Pending, _)) => {
                                self.pending_module_body = Some(v);
                                // Register for late importers ONLY when the
                                // suspension is a REAL top-level await. A body
                                // can also suspend at its own dynamic-import
                                // ops — chaining a self-import on that body
                                // promise is a deadlock cycle (the resumption
                                // depends on the import it would wait for).
                                if self.module_has_tla(&path) {
                                    self.module_body_promise.insert(path.clone(), (v, false));
                                }
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
        // NAMESPACE object under `name` through the dependency's CANONICAL
        // namespace slot (`self.globals` is a GC root) — the same slot every
        // `import * as` of that module aliases, so the binding identity
        // matches the spec's (module, ~namespace~) ResolvedBinding. Cycles
        // resolve through the loader's pre-registered cache entry.
        for (exported, spec) in ns_reexports {
            let dep = match dir {
                Some(d) => d.join(spec),
                None => std::path::PathBuf::from(spec),
            };
            let canon = std::fs::canonicalize(&dep).unwrap_or_else(|_| dep.clone());
            let slot = self.module_ns_slot(&canon)?;
            let ns = self.import_module_sync(&dep, None)?;
            self.globals[slot as usize] = ns;
            full.push((exported.clone(), slot));
        }
        for (exported, imported, spec) in reexports {
            let dep = match dir {
                Some(d) => d.join(spec),
                None => std::path::PathBuf::from(spec),
            };
            if let Some(slot) = self.resolve_export(&dep, imported, None)? {
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
        mtype: Option<&str>,
    ) -> Result<Option<u32>, Thrown> {
        let dep = std::fs::canonicalize(raw_path)
            .map_err(|_| Thrown("TypeError: module not found".into()))?;
        // A TYPED request (json/text) is its own module record: resolve via
        // the typed loader, never the (possibly in-flight) JS module's
        // registries — a file may import ITSELF as text.
        if mtype.is_some() {
            let ns = self.import_module_sync(&dep, mtype)?;
            return Ok(self
                .module_namespaces
                .get(&ns.heap_index())
                .and_then(|m| m.get(name).copied()));
        }
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
        let ns = self.import_module_sync(&dep, mtype)?;
        ambiguous_check(self, ns.heap_index())?;
        Ok(self
            .module_namespaces
            .get(&ns.heap_index())
            .and_then(|m| m.get(name).copied()))
    }

    /// The DEFERRED namespace singleton for the module at `raw_path`
    /// (`import defer * as ns` / `import.defer()`): the module's graph LOADS
    /// now (resolution/parse failures surface at link time) but evaluation
    /// waits for the first triggering access. The object starts as a sealed
    /// null-proto carrier of just @@toStringTag = "Deferred Module"; a
    /// trigger evaluates the module and copies the real namespace's bindings
    /// into it (the spec's [[Deferred]] namespace is a distinct object from
    /// the eager one, with its own tag).
    pub(crate) fn deferred_namespace_for(
        &mut self,
        raw_path: &std::path::Path,
    ) -> Result<Value, Thrown> {
        let path = std::fs::canonicalize(raw_path).map_err(|_| {
            Thrown(format!(
                "TypeError: Failed to resolve module specifier '{}'",
                raw_path.display()
            ))
        })?;
        if let Some(&ns) = self.deferred_ns_cache.get(&path) {
            return Ok(ns);
        }
        let mut seen = std::collections::HashSet::new();
        self.prescan_module_requests(&path, &mut seen)?;
        // The proposal evaluates a deferred graph's ASYNC subgraphs EAGERLY
        // at load time (so a later trigger can stay synchronous): import any
        // reachable module with top-level await now — its own dependencies
        // evaluate with it; sync-only parts of the graph stay deferred.
        // DFS request order (deterministic — evaluation logs are observable).
        let mut graph: Vec<std::path::PathBuf> = Vec::new();
        let mut gseen = std::collections::HashSet::new();
        let mut stack = vec![path.clone()];
        while let Some(m) = stack.pop() {
            if !gseen.insert(m.clone()) {
                continue;
            }
            graph.push(m.clone());
            if let Ok(reqs) = self.module_requests(&m) {
                // Reverse so the explicit stack pops requests in source order.
                for r in reqs.into_iter().rev() {
                    stack.push(r);
                }
            }
        }
        for m in graph {
            if !self.module_cache.contains_key(&m)
                && !self.module_loading.contains(&m)
                && !self.executing_modules.contains(&m)
                && self.module_has_tla(&m)
            {
                self.import_module_sync(&m, None)?;
            }
        }
        let _gc = self.gc_lock_guard();
        let tag = self.alloc_str("Deferred Module".to_string());
        let mut m = crate::heap::ObjMap::new();
        m.define(
            "@@toStringTag",
            tag,
            crate::heap::PropAttr {
                writable: false,
                enumerable: false,
                configurable: false,
                accessor: false,
                setter: Value::UNDEFINED,
            },
        );
        m.extensible = false;
        let idx = self.heap.alloc(HeapObj::Object(m));
        self.proto_of.insert(idx, Value::NULL);
        self.deferred_ns_cache.insert(path.clone(), Value::heap(idx));
        self.deferred_ns_state.insert(idx, path);
        Ok(Value::heap(idx))
    }

    /// A TRIGGERING access on a deferred namespace: evaluate the module (and
    /// its eager dependencies) now, then graft the real namespace's live-slot
    /// map and snapshot keys onto the deferred object — keeping ITS
    /// @@toStringTag. No-op for anything that is not an unevaluated deferred
    /// namespace. Call sites gate on `deferred_ns_state` being non-empty.
    pub(crate) fn defer_ns_trigger(&mut self, idx: u32) -> Result<(), Thrown> {
        let Some(path) = self.deferred_ns_state.get(&idx).cloned() else {
            return Ok(());
        };
        // ReadyForSyncExecution: the module — or ANYTHING its graph reaches —
        // mid-evaluation/loading (or an unevaluated async subgraph) cannot be
        // completed by a synchronous trigger: TypeError per the proposal,
        // BEFORE evaluating any part of the graph.
        let mut seen = std::collections::HashSet::new();
        if !self.ready_for_sync_execution(&path, &mut seen) {
            return Err(Thrown(
                "TypeError: Cannot synchronously evaluate a deferred module that is currently evaluating"
                    .into(),
            ));
        }
        let real = self.import_module(&path, None)?;
        // A TLA module's body may still be pending; its bindings update
        // through the shared live slots as it completes.
        self.pending_module_body = None;
        self.defer_ns_adopt(idx, real);
        Ok(())
    }

    /// Graft the REAL namespace's bindings onto deferred namespace `idx`
    /// (live-slot map + snapshot keys), keeping the DEFERRED tag, and mark it
    /// evaluated.
    pub(crate) fn defer_ns_adopt(&mut self, idx: u32, real: Value) {
        self.deferred_ns_state.remove(&idx);
        if real.is_heap() {
            let rid = real.heap_index();
            if let Some(map) = self.module_namespaces.get(&rid).cloned() {
                self.module_namespaces.insert(idx, map);
            }
            if let Some(amb) = self.module_ambiguous.get(&rid).cloned() {
                self.module_ambiguous.insert(idx, amb);
            }
            // Copy the snapshot ObjMap (export keys for reflection), then
            // restore the DEFERRED tag.
            let real_map = match self.heap.get(rid) {
                HeapObj::Object(m) => Some(m.clone()),
                _ => None,
            };
            if let Some(mut m) = real_map {
                let _gc = self.gc_lock_guard();
                let tag = self.alloc_str("Deferred Module".to_string());
                m.define(
                    "@@toStringTag",
                    tag,
                    crate::heap::PropAttr {
                        writable: false,
                        enumerable: false,
                        configurable: false,
                        accessor: false,
                        setter: Value::UNDEFINED,
                    },
                );
                m.extensible = false;
                if let HeapObj::Object(slot) = self.heap.get_mut(idx) {
                    *slot = m;
                }
                // Whole-map replacement: invalidate any JIT inline cache that
                // captured the old vals pointer.
                self.heap.bump_version(idx);
            }
        }
    }

    /// Whether the module at canonical `path` uses TOP-LEVEL await (its body
    /// compiles to an activation containing Await ops). Used by import.defer:
    /// the proposal evaluates a deferred graph's ASYNC modules eagerly.
    pub(crate) fn module_has_tla(&mut self, path: &std::path::Path) -> bool {
        let Ok(code) = std::fs::read_to_string(path) else {
            return false;
        };
        let allocator = oxc_allocator::Allocator::default();
        let ret =
            oxc_parser::Parser::new(&allocator, &code, oxc_span::SourceType::mjs()).parse();
        if !ret.errors.is_empty() {
            return false;
        }
        let Ok(prog) = crate::compile::compile_eval(&ret.program, &code, true, false, None, false, std::collections::HashSet::new(), true, false, Vec::new(), false, None) else {
            return false;
        };
        prog.functions
            .first()
            .map(|f| {
                f.code.iter().any(|i| {
                    matches!(
                        i,
                        crate::bytecode::Instr::Await { .. }
                            | crate::bytecode::Instr::ForAwaitNext { .. }
                    )
                })
            })
            .unwrap_or(false)
    }

    /// Whether a [[Get]]/[[Has]]/[[Delete]]/[[DefineOwnProperty]] key
    /// TRIGGERS deferred evaluation: every STRING key except "then" (symbol
    /// keys — "@@"-encoded — and "then" never trigger).
    pub(crate) fn defer_key_triggers(key: &str) -> bool {
        key != "then" && !key.starts_with("@@")
    }

    /// Trigger hook for KEYED operations on a possibly-deferred namespace.
    /// Zero-cost when no unevaluated deferred namespace exists.
    #[inline]
    pub(crate) fn defer_check(&mut self, obj: Value, key: &str) -> Result<(), Thrown> {
        if !self.deferred_ns_state.is_empty()
            && obj.is_heap()
            && Self::defer_key_triggers(key)
            && self.deferred_ns_state.contains_key(&obj.heap_index())
        {
            self.defer_ns_trigger(obj.heap_index())?;
        }
        Ok(())
    }

    /// Trigger hook for [[OwnPropertyKeys]]-class operations (always trigger).
    #[inline]
    pub(crate) fn defer_check_all(&mut self, obj: Value) -> Result<(), Thrown> {
        if !self.deferred_ns_state.is_empty()
            && obj.is_heap()
            && self.deferred_ns_state.contains_key(&obj.heap_index())
        {
            self.defer_ns_trigger(obj.heap_index())?;
        }
        Ok(())
    }

    /// The spec's LOADING phase for a module that is never evaluated (a phase
    /// import's target): read + parse it and resolve its requested specifiers
    /// recursively — an unreadable file is a host TypeError, a parse failure a
    /// SyntaxError. Already-cached / already-seen modules are done.
    /// The canonical paths of `path`'s DIRECT module requests (import /
    /// export-from / export-* sources), resolving each against its dir.
    fn module_requests(
        &mut self,
        path: &std::path::PathBuf,
    ) -> Result<Vec<std::path::PathBuf>, Thrown> {
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
        let mut out = Vec::new();
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
                // The synthetic `<module source>` host module (source-phase
                // imports) has no file and never evaluates — not a request.
                if spec == "<module source>" {
                    continue;
                }
                let raw = match dir.as_deref() {
                    Some(d) => d.join(&spec),
                    None => std::path::PathBuf::from(&spec),
                };
                if std::fs::metadata(&raw).is_err() {
                    return Err(Thrown(format!(
                        "TypeError: Failed to resolve module specifier '{spec}'"
                    )));
                }
                out.push(std::fs::canonicalize(&raw).unwrap_or(raw));
            }
        }
        Ok(out)
    }

    /// Whether `from`'s static request graph reaches `target` (cycle-root
    /// detection for late importers of a suspended cycle member).
    fn module_graph_reaches(
        &mut self,
        from: &std::path::PathBuf,
        target: &std::path::PathBuf,
        seen: &mut std::collections::HashSet<std::path::PathBuf>,
    ) -> bool {
        if !seen.insert(from.clone()) {
            return false;
        }
        let Ok(reqs) = self.module_requests(from) else {
            return false;
        };
        for dep in reqs {
            if dep == *target || self.module_graph_reaches(&dep, target, seen) {
                return true;
            }
        }
        false
    }

    fn prescan_module_requests(
        &mut self,
        path: &std::path::PathBuf,
        seen: &mut std::collections::HashSet<std::path::PathBuf>,
    ) -> Result<(), Thrown> {
        if !seen.insert(path.clone()) || self.module_cache.contains_key(path) {
            return Ok(());
        }
        for canon in self.module_requests(path)? {
            self.prescan_module_requests(&canon, seen)?;
        }
        Ok(())
    }

    /// The proposal's ReadyForSyncExecution: can `path`'s graph evaluate
    /// synchronously RIGHT NOW? False when any reachable unevaluated module
    /// is mid-evaluation/loading or contains top-level await.
    fn ready_for_sync_execution(
        &mut self,
        path: &std::path::PathBuf,
        seen: &mut std::collections::HashSet<std::path::PathBuf>,
    ) -> bool {
        if !seen.insert(path.clone()) {
            return true;
        }
        // BEFORE the cache check: a namespace is pre-registered (cached)
        // while its module is still evaluating / mid-link.
        if self.executing_modules.contains(path) || self.module_loading.contains(path) {
            return false; // evaluating / link in flight
        }
        // A body suspended at top-level await = evaluating-async.
        if let Some(&(bp, _)) = self.module_body_promise.get(path) {
            if bp.is_heap()
                && matches!(
                    self.heap.get(bp.heap_index()),
                    HeapObj::Promise { state: crate::heap::PromiseState::Pending, .. }
                )
            {
                return false;
            }
        }
        if self.module_cache.contains_key(path) {
            return true; // evaluated
        }
        if self.module_has_tla(path) {
            return false; // HasTLA, not yet evaluated
        }
        let Ok(reqs) = self.module_requests(path) else {
            return true; // resolution errors surface at evaluation
        };
        for dep in reqs {
            if !self.ready_for_sync_execution(&dep, seen) {
                return false;
            }
        }
        true
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
        let Some((reex, stars, nsreex, pdir)) =
            self.module_pending_reexports.get(dep).cloned()
        else {
            // Completed (or never in-flight): normal resolution.
            return self.resolve_export(dep, name, None);
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
        // `export * as name from spec`: an INDIRECT export whose binding is
        // the dependency's namespace — its canonical shared slot (created on
        // demand; the value fills in when the dependency links).
        for (exported, spec) in &nsreex {
            if exported == name {
                let target = join(pdir.as_deref(), spec);
                return Ok(Some(self.module_ns_slot(&target)?));
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
        // A $262.evalScript: SCRIPT GlobalDeclarationInstantiation semantics
        // for THIS program only (lexical-collision SyntaxErrors, realm-
        // persistent lexicals, non-configurable brandNew var/fn bindings).
        let script_gdi = std::mem::take(&mut self.eval_script_gdi);
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
                    | Instr::StoreGlobalDyn { idx, .. }
                    | Instr::EvalScopeHas { idx, .. }
                    | Instr::EvalScopeSet { idx, .. } => {
                        *idx = gmap[*idx as usize];
                    }
                    // `delete <global>`: the slot operand maps like every other
                    // global reference (the runtime checks the MAIN program's
                    // decl lists against the mapped slot).
                    Instr::DeleteGlobal { slot, .. } => {
                        *slot = gmap[*slot as usize];
                    }
                    // The upvalue index is the eval closure's own; only the
                    // NAME handle is a global slot.
                    Instr::LoadUpvalDyn { name, .. } | Instr::StoreUpvalDyn { name, .. } => {
                        *name = gmap[*name as usize];
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
                    Instr::FieldInit { class_id, .. } => {
                        if *class_id != u32::MAX {
                            *class_id += base_class;
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
            // SCRIPT GDI steps 3-4: every lexically-declared name of THIS
            // script must collide with NEITHER an existing var/function
            // declaration NOR an existing lexical NOR a non-configurable
            // global-object property — SyntaxError BEFORE any binding
            // (including this script's own vars) is created.
            if script_gdi {
                for &slot in &eval_prog.lexical_globals {
                    let rs = gmap[slot as usize];
                    let name = self.global_slot_name(rs).unwrap_or_default();
                    let has_var = self.program.hoisted_globals.contains(&rs)
                        || self.program.decl_globals.contains(&rs)
                        || self.eval_var_globals.contains(&rs);
                    let has_lex = self.program.lexical_globals.contains(&rs)
                        || self.eval_lexical_globals.contains(&rs);
                    let restricted = self.global_this != 0
                        && matches!(
                            self.heap.get(self.global_this),
                            HeapObj::Object(m)
                                if m.pos(&name).map_or(false, |i| !m.attrs[i].configurable)
                        );
                    if has_var || has_lex || restricted {
                        return Err(Thrown(format!(
                            "SyntaxError: Identifier '{name}' has already been declared"
                        )));
                    }
                }
            }
            let mut lex_clash: Option<String> = None;
            for &slot in &eval_prog.hoisted_globals {
                let rs = gmap[slot as usize];
                if self.program.lexical_globals.contains(&rs)
                    || self.eval_lexical_globals.contains(&rs)
                {
                    lex_clash = self.global_slot_name(rs);
                    break;
                }
            }
            if lex_clash.is_none() {
                for local in start..count {
                    if let Some(slot) = self.eval_funcs[local].name_global {
                        if self.program.lexical_globals.contains(&(slot as u32))
                            || self.eval_lexical_globals.contains(&(slot as u32))
                        {
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
                            } else if self.global_by_name(&name).is_none()
                                && !matches!(
                                    self.heap.get(self.global_this),
                                    HeapObj::Object(m) if m.extensible
                                )
                            {
                                // CanDeclareGlobalFunction step 5: an ABSENT name
                                // is only definable while the global object is
                                // extensible.
                                return Err(Thrown(format!(
                                    "TypeError: cannot declare global function {name}"
                                )));
                            }
                        }
                    }
                }
            }
        }
        // SCRIPT GDI bookkeeping: record this script's bindings in the realm
        // registries — later scripts' collision checks, const enforcement
        // (StoreGlobal* throw on a write to an INITIALIZED const slot), and
        // lexical invisibility to global-object property reflection.
        if script_gdi && var_env_global {
            for &slot in &eval_prog.lexical_globals {
                self.eval_lexical_globals.insert(gmap[slot as usize]);
            }
            for &slot in &eval_prog.const_globals {
                self.eval_const_globals.insert(gmap[slot as usize]);
            }
            for &slot in &eval_prog.hoisted_globals {
                self.eval_var_globals.insert(gmap[slot as usize]);
            }
            for local in start..count {
                if let Some(slot) = self.eval_funcs[local].name_global {
                    self.eval_var_globals.insert(slot as u32);
                }
            }
        }
        // 5. CreateGlobalVarBinding for eval `var` names: an ABSENT binding
        // becomes an own {writable, enumerable, CONFIGURABLE} property of the
        // global object (eval-created bindings are deletable and reflectable;
        // a $262.evalScript's are NON-configurable — script
        // GlobalDeclarationInstantiation passes deletable=false);
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
                                        configurable: !script_gdi,
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
                                configurable: !script_gdi,
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
    /// `$262.evalScript`: parse + compile `code` as a SCRIPT and run it in
    /// the current realm. Every top-level declaration — var, function, AND
    /// `let`/`const`/`class` — binds a persistent realm global (the eval
    /// pipeline's name-mapped slots), matching script
    /// GlobalDeclarationInstantiation rather than eval semantics.
    pub(crate) fn eval_script(&mut self, code: &str) -> Result<Value, Thrown> {
        let allocator = oxc_allocator::Allocator::default();
        // SCRIPT goal, not the oxc default (mjs): module mode would make the
        // whole program strict and silently disable Annex B.3.3 hoisting,
        // sloppy semantics, and HTML comments.
        let ret =
            oxc_parser::Parser::new(&allocator, code, oxc_span::SourceType::cjs()).parse();
        if !ret.errors.is_empty() {
            return Err(Thrown(format!("SyntaxError: {}", ret.errors[0])));
        }
        let prog = crate::compile::compile_program(&ret.program, code)
            .map_err(|e| Thrown(format!("SyntaxError: {e}")))?;
        // Dev aid (same flag as the main-program dump in lib.rs).
        if std::env::var_os("ZIPP_VM_DUMP").is_some() {
            eprintln!("── evalScript program (hoisted={:?}) ──", prog.hoisted_globals);
            for (fid, f) in prog.functions.iter().enumerate() {
                eprintln!("── eval fn {fid} (regs={}, params={}) ──", f.reg_count, f.param_count);
                for (ip, instr) in f.code.iter().enumerate() {
                    eprintln!("  {ip:4}  {instr:?}");
                }
            }
        }
        // Script GlobalDeclarationInstantiation (not eval semantics) for this
        // program: prepare_eval_program consumes the flag.
        self.eval_script_gdi = true;
        let (completion, _gmap) = self.run_eval_program(
            prog,
            None,
            false,
            None,
            None,
            Value::UNDEFINED,
            None,
            true,
            None,
            None,
        )?;
        Ok(completion)
    }

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
