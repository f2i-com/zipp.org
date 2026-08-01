//! Mark-sweep garbage collector for the VM heap.
//!
//! The heap is a `Vec<HeapObj>` indexed by `u32`; `Value` references objects by
//! INDEX, so this collector is NON-MOVING — it only frees unreachable slots back
//! to a free list (reused by `Heap::alloc`), never relocating live objects, so
//! every existing index stays valid. That sidesteps having to rewrite indices in
//! the ~dozens of side tables, the register file, and the JIT.
//!
//! Safety rests on three invariants:
//!   1. `gc_floor` pins the interned strings + every built-in allocated during
//!      setup — they are always marked and never swept.
//!   2. The root set below is COMPLETE: every live `Value`/index reachable by the
//!      running program is enumerated here or reachable by tracing from it. A
//!      missed root would free a live object; `ZIPP_GC_STRESS` (collect at every
//!      safe point) turns any such miss into an immediate corpus/test262 failure.
//!   3. Collection only happens at a dispatch-loop safe point with `gc_lock == 0`,
//!      so no native built-in is holding an un-rooted `Vec<Value>` working set.

use super::{Microtask, Resume, Vm};
use crate::heap::{GenState, HeapObj};
use crate::value::Value;

/// RAII guard that suspends collection for its scope (decrements on drop, even
/// across `?` early returns). Held by native built-ins that keep an un-rooted
/// `Vec<Value>` of freshly-allocated objects while re-entering the interpreter
/// (a callback), where a safe point could otherwise free that working set.
pub(crate) struct GcGuard {
    lock: *mut u32,
}

impl Drop for GcGuard {
    fn drop(&mut self) {
        // SAFETY: the guard never outlives the `&mut Vm` it was created from, and
        // a `Vm` behind `&mut self` is not relocated for the guard's lifetime.
        unsafe {
            *self.lock = (*self.lock).saturating_sub(1);
        }
    }
}

impl Vm<'_> {
    /// Pin everything allocated so far (called once, after setup + hoisting).
    pub(crate) fn set_gc_floor(&mut self) {
        self.gc_floor = self.heap.len() as u32;
    }

    /// Suspend GC for the returned guard's scope (see [`GcGuard`]).
    #[inline]
    pub(crate) fn gc_lock_guard(&mut self) -> GcGuard {
        self.gc_lock += 1;
        GcGuard { lock: &mut self.gc_lock as *mut u32 }
    }

    /// Run a collection if one is due (or always, under stress) and it is safe.
    #[inline]
    pub(crate) fn maybe_gc(&mut self) {
        if self.gc_lock == 0
            && self.gc_floor != 0
            && (self.heap.gc_requested() || self.gc_stress)
        {
            self.gc();
        }
    }

    fn gc(&mut self) {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::Gc);
        let n = self.heap.len();
        let floor = self.gc_floor as usize;
        if floor == 0 || n <= floor {
            self.heap.note_gc_done(n);
            return;
        }
        // `ZIPP_GCSTATS=1`: per-phase timing, printed at exit. B81 measured the
        // per-allocation cost rising 74.5 -> 122.5ns purely from a larger live
        // set, which says the collector dominates — but NOT which phase of it.
        // Tracing, the sweep and the ~30 side-table `retain` passes are three
        // very different fixes, so measure before choosing (B5.3's lesson).
        let stats = gcstats::enabled();
        let t_start = gcstats::now(stats);
        let mut marks = vec![false; n];
        let mut stack: Vec<u32> = Vec::with_capacity(1024);

        // Already-free slots stay free: mark them so the sweep does not push a
        // duplicate onto the free list, but do NOT trace them (they are tombstones).
        for &idx in self.heap.free_indices() {
            marks[idx as usize] = true;
        }
        let free_before = self.heap.free_indices().len();

        // --- Roots ---------------------------------------------------------
        // Pinned built-ins: marked AND traced (a built-in proto may hold a user
        // object, e.g. `Array.prototype.foo = userObj`).
        for i in 0..floor {
            if !marks[i] {
                marks[i] = true;
                stack.push(i as u32);
            }
        }
        macro_rules! root_val {
            ($v:expr) => {{
                let v: Value = $v;
                if v.is_heap() {
                    let i = v.heap_index() as usize;
                    if i < n && !marks[i] {
                        marks[i] = true;
                        stack.push(v.heap_index());
                    }
                }
            }};
        }
        macro_rules! root_idx {
            ($i:expr) => {{
                let i = $i as usize;
                if i < n && !marks[i] {
                    marks[i] = true;
                    stack.push(i as u32);
                }
            }};
        }

        for &v in &self.regs {
            root_val!(v);
        }
        for &v in &self.globals {
            root_val!(v);
        }
        // Strings interned at region-compile time whose bits are embedded in
        // native code (LoadConst immediates) — see `jit_const_strings`.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        for &v in &self.jit_const_strings {
            root_val!(v);
        }
        for v in self.class_values.iter().flatten() {
            root_val!(*v);
        }
        if let Some(v) = self.pending_throw {
            root_val!(v);
        }
        for (v, _) in self.module_body_promise.values() {
            root_val!(*v);
        }
        for &p in &self.module_body_results {
            root_idx!(p);
        }
        for v in self.typed_module_cache.values() {
            root_val!(*v);
        }
        for v in self.module_cache.values() {
            root_val!(*v);
        }
        if let Some(v) = self.pending_module_body {
            root_val!(v);
        }
        for &v in &self.link_pending_deps {
            root_val!(v);
        }
        for (&cap, st) in &self.deferred_mods {
            root_idx!(cap);
            root_idx!(st.ns_idx);
        }
        for v in self.deferred_ns_cache.values() {
            root_val!(*v);
        }
        for v in self.module_errors.values() {
            root_val!(*v);
        }
        for &(buf, _, p, _, _) in &self.async_waiters {
            root_idx!(buf);
            root_idx!(p);
        }
        for (_, cb) in &self.timer_queue {
            root_val!(*cb);
        }
        // Worker-side $262.agent.receiveBroadcast callback (invoked per broadcast).
        root_val!(self.broadcast_cb);
        for v in self.closure_home.values() {
            root_val!(*v);
        }
        for v in self.closure_new_target.values() {
            root_val!(*v);
        }
        for v in self.gen_callee.values() {
            root_val!(*v);
        }
        if let Some(v) = self.from_async_fn {
            root_val!(v);
        }
        if let Some(v) = self.async_dispose_fn {
            root_val!(v);
        }
        if let Some(v) = self.sync_dispose_shim_fn {
            root_val!(v);
        }
        if let Some((v, _)) = self.pending_yield {
            root_val!(v);
        }
        if self.pending_yield_eval_scope != u32::MAX {
            root_idx!(self.pending_yield_eval_scope);
        }
        if self.import_meta != 0 {
            root_idx!(self.import_meta);
        }
        // Per-module import.meta objects (persist for the Vm's lifetime, like
        // the module cache itself).
        for &m in self.module_metas.values() {
            root_idx!(m);
        }
        if let Some((v, _, _)) = &self.pending_await {
            root_val!(*v);
        }
        for mt in &self.microtasks {
            match mt {
                Microtask::Reaction { callback, arg, dependent, .. } => {
                    root_val!(*callback);
                    root_val!(*arg);
                    root_idx!(*dependent);
                }
                Microtask::AsyncResume { activation, input } => {
                    root_idx!(*activation);
                    match input {
                        Resume::Value(v) | Resume::Throw(v) | Resume::Return(v) => root_val!(*v),
                    }
                }
                Microtask::ThenableJob { thenable, then, promise } => {
                    root_val!(*thenable);
                    root_val!(*then);
                    root_idx!(*promise);
                }
            }
        }
        if let Some((resolve, reject)) = &self.cap_capture {
            root_val!(*resolve);
            root_val!(*reject);
        }
        for f in &self.frames {
            root_idx!(f.closure);
            root_val!(f.new_target);
            root_val!(f.callee);
            if f.eval_scope != u32::MAX {
                root_idx!(f.eval_scope);
            }
        }
        // Eval scopes stamped on closures (created in frames that had one).
        for &s in self.closure_eval_scope.values() {
            root_idx!(s);
        }
        // An EvalScope's enclosing EvalScope: still reachable through the chain
        // walk (`frame_eval_scope_chain`), so it has to be rooted from the child.
        for &s in self.eval_scope_parent.values() {
            root_idx!(s);
        }
        // Realm registry ($262.createRealm): keep every realm constructor /
        // prototype / object reachable so the `obj_realm` and `realms` heap-index
        // mappings never go stale (a freed-then-reused slot would misattribute a
        // realm). See [[vm-gc]].
        for &k in self.obj_realm.keys() {
            root_idx!(k);
        }
        // ShadowRealm-born callables: root the realm INSTANCES so the
        // `shadow_fn_realm` values (and the realm's `realm_globals` table) never
        // go stale; the fn KEYS are retained post-sweep against live marks.
        for &r in self.shadow_fn_realm.values() {
            root_idx!(r);
        }
        for m in &self.realms {
            for (&main_p, &realm_p) in m {
                root_idx!(main_p);
                root_idx!(realm_p);
            }
        }
        // Child-realm global objects and their realm-bound eval/evalScript
        // functions (also obj_realm-tagged, but rooted explicitly so the
        // `realm_globals` binding tables and `realm_fns` keys never go stale).
        for &g in self.realm_global_objs.keys() {
            root_idx!(g);
        }
        for (&f, &(g, _)) in &self.realm_fns {
            root_idx!(f);
            root_idx!(g);
        }
        for &t in self.realm_throw_type_errors.values() {
            root_idx!(t);
        }
        // Explicit `fn.prototype = obj` overrides keep the assigned object alive.
        for (&k, &v) in &self.fn_proto_override {
            root_idx!(k);
            root_val!(v);
        }
        // Error / TypedArray constructor + prototype tables (mostly < floor, but
        // root them unconditionally to be safe).
        for &i in self.error_protos.iter().chain(self.error_ctors.iter()) {
            root_idx!(i);
        }
        for &i in self.ta_protos.iter().chain(self.ta_ctors.iter()) {
            root_idx!(i);
        }
        // Builtin globals (Object/Array/Error/…): permanent roots so a builtin the
        // program never referenced — reachable only via this map for eval — survives.
        for &i in self.builtin_globals.values() {
            root_idx!(i);
        }
        // Side tables: their VALUES are roots (an entry keyed by a live object
        // must keep its payload alive). Over-approximate by rooting all values;
        // entries whose KEY is dead are pruned after the sweep.
        for v in self.proto_of.values() {
            root_val!(*v);
        }
        // In-flight super() return-override instances (held across the rest of
        // the derived ctor body until construct() consumes them).
        for v in self.super_this.values() {
            root_val!(*v);
        }
        // Private field values: an entry keyed by a live instance must keep its
        // payload alive (over-approximate; dead-instance entries pruned below).
        for m in self.private_fields.values() {
            for v in m.values() {
                root_val!(*v);
            }
        }
        for &p in self.prototypes.values() {
            root_idx!(p);
        }
        for (disposers, _) in self.dispose_stacks.values() {
            for &v in disposers {
                root_val!(v);
            }
        }
        // In-flight disposeAsync drivers: the capability promise (key), the
        // not-yet-run disposers, and the accumulating error chain are live
        // across reaction jobs.
        for (&cap, st) in &self.dispose_async_state {
            root_idx!(cap);
            for &v in &st.remaining {
                root_val!(v);
            }
            if let Some(e) = st.error_chain {
                root_val!(e);
            }
        }
        for disposers in self.using_resources.values() {
            for &v in disposers {
                root_val!(v);
            }
        }
        for m in self.fn_props.values().chain(self.arr_props.values()) {
            for &v in &m.vals {
                root_val!(v);
            }
            for a in &m.attrs {
                root_val!(a.setter);
            }
        }
        // A pristine RegExp match result keeps the values of its standard
        // named properties here rather than in an ObjMap.
        for props in self.regexp_result_props.values() {
            for &v in &props.values {
                root_val!(v);
            }
        }
        for v in self.template_raws.values() {
            root_val!(*v);
        }
        // Memoized tagged-template objects are permanent roots (one per source
        // site, live for the realm's lifetime); their keys are func/site ids, not
        // heap indices, so the map is never pruned.
        for v in self.template_cache.values() {
            root_val!(*v);
        }
        // A live lazy RegExpStringIterator keeps its matcher + subject string alive.
        for (regexp, string, _, _) in self.regexp_string_iters.values() {
            root_idx!(*regexp);
            root_val!(*string);
        }
        // The legacy RegExp statics' last-match record (input/match/capture
        // strings) is live until the next match replaces it.
        for v in &self.regexp_last {
            root_val!(*v);
        }
        // The deferred half of those statics: slots 2..=13 are unit RANGES into
        // this subject rather than strings, so the subject itself is what has to
        // survive until something reads them (or the next match replaces it).
        if let Some(lazy) = &self.regexp_last_lazy {
            root_val!(lazy.subj);
        }
        for v in self.zdt_tz.values() {
            root_val!(*v);
        }
        for v in self.symbol_registry.values() {
            root_val!(*v);
        }
        for v in self.symbol_keys.values() {
            root_val!(*v);
        }
        // Suspended async activations are pending event-loop JOBS: an async
        // function's `AsyncState` is referenced only by the await-reaction it
        // registered (a cycle the program can't otherwise reach), so the event
        // loop conceptually keeps it alive until it resumes. Pin every
        // non-completed async activation; tracing it then keeps its awaited
        // promises / saved registers alive. A completed activation is unrooted
        // here and reclaimed normally.
        // Walk the REGISTRY, not the heap. This used to be a linear scan of
        // every slot on every collection — ~2.8 ns/slot paid by every program,
        // including those with no async code at all (measured at 1.9% of the
        // benchmark suite; async-promise-chain 4.9%, markdown-render 4.4%).
        // The same pass prunes: an entry whose slot is no longer a suspended
        // activation (completed, or the slot was reclaimed and reused) is
        // dropped, so none of the 11 sites that finish an activation has to
        // deregister. Sorting/deduping keeps a reused slot from being held
        // twice after its predecessor was collected.
        let mut acts = std::mem::take(&mut self.async_activations);
        acts.sort_unstable();
        acts.dedup();
        acts.retain(|&i| {
            let live = matches!(self.heap.get(i),
                HeapObj::AsyncState(s) if !matches!(s.state, GenState::Completed))
                || matches!(self.heap.get(i),
                    HeapObj::AsyncGenerator(s) if !matches!(s.state, GenState::Completed));
            if live {
                root_idx!(i as usize);
            }
            live
        });
        self.async_activations = acts;

        let t_roots = gcstats::now(stats);
        // --- Trace ---------------------------------------------------------
        while let Some(idx) = stack.pop() {
            self.trace_edges(idx, &mut marks, &mut stack, n);
        }
        let t_trace = gcstats::now(stats);

        // --- Sweep + prune -------------------------------------------------
        let mut swept = 0usize;
        for i in floor..n {
            if !marks[i] {
                self.heap.free_slot(i as u32);
                swept += 1;
            }
        }
        let t_sweep = gcstats::now(stats);
        // Drop side-table entries whose keyed object was reclaimed.
        self.proto_of.retain(|&k, _| marks[k as usize]);
        // Derived-ctor `this` state: a thrown-off constructor leaves these
        // entries behind deliberately (an escaped arrow may still complete the
        // super() later); once the placeholder itself is dead the entries must
        // go, or a recycled heap slot would inherit a TDZ mark / super() flag.
        self.this_tdz.retain(|&k| marks[k as usize]);
        self.super_called.retain(|&k| marks[k as usize]);
        self.super_this.retain(|&k, _| marks[k as usize]);
        self.prototypes.retain(|&k, _| marks[k as usize]);
        self.fn_props.retain(|&k, _| marks[k as usize]);
        self.arr_props.retain(|&k, _| marks[k as usize]);
        self.regexp_result_props.retain(|&k, _| marks[k as usize]);
        self.zdt_tz.retain(|&k, _| marks[k as usize]);
        self.temporal_cal.retain(|&k, _| marks[k as usize]);
        // Idx-keyed FLAG sets must drop dead keys too, or a recycled slot
        // inherits the previous occupant's state (a fresh function would
        // report its name/length intrinsic deleted; a fresh array a frozen
        // length).
        self.deleted_callable_intrinsics.retain(|&(k, _)| marks[k as usize]);
        self.array_length_nonwritable.retain(|&k| marks[k as usize]);
        // Virtual lengths of sparse arrays (u32 values — nothing to trace; the
        // sparse ELEMENTS live in arr_props, rooted above).
        self.array_js_len.retain(|&k, _| marks[k as usize]);
        self.ab_max.retain(|&k, _| marks[k as usize]);
        self.ta_tracking.retain(|&k| marks[k as usize]);
        self.dv_tracking.retain(|&k| marks[k as usize]);
        self.regexp_string_iters.retain(|&k, _| marks[k as usize]);
        self.method_brand.retain(|&k, _| marks[k as usize]);
        self.instance_brand.retain(|&k, _| marks[k as usize]);
        self.brand_owner.retain(|_, &mut c| marks[c as usize]);
        self.closure_eval_scope.retain(|&k, _| marks[k as usize]);
        self.eval_scope_parent.retain(|&k, _| marks[k as usize]);
        self.shadow_fn_realm.retain(|&k, _| marks[k as usize]);
        self.private_fields.retain(|&k, _| marks[k as usize]);
        // Keep declared-name records only for brands still referenced by a live
        // lexical chain or instance brand (these maps were just pruned by marks).
        if !self.brand_private_names.is_empty() {
            let mut live_brands: std::collections::HashSet<u64> = std::collections::HashSet::new();
            for bs in self.method_brand.values() {
                live_brands.extend(bs.iter().copied());
            }
            for bs in self.instance_brand.values() {
                live_brands.extend(bs.iter().copied());
            }
            self.brand_private_names.retain(|b, _| live_brands.contains(b));
        }
        self.shared_buffers.retain(|&k| marks[k as usize]);
        self.immutable_buffers.retain(|&k| marks[k as usize]);
        self.error_data.retain(|&k| marks[k as usize]);
        self.arguments_objs.retain(|&k, _| marks[k as usize]);
        self.gen_args_obj.retain(|&k, &mut v| marks[k as usize] && marks[v as usize]);
        self.fn_name_cells.retain(|&k| marks[k as usize]);
        self.const_cells.retain(|&k| marks[k as usize]);
        self.gen_callee.retain(|&k, _| marks[k as usize]);
        self.module_body_results.retain(|&k| marks[k as usize]);
        self.module_namespaces.retain(|&k, _| marks[k as usize]);
        self.closure_home.retain(|&k, _| marks[k as usize]);
        self.closure_new_target.retain(|&k, _| marks[k as usize]);
        self.dispose_stacks.retain(|&k, _| marks[k as usize]);
        // Pure-bytes side table (no Values to trace): hygiene-retain only, so a
        // recycled RegExp slot can't inherit a dead pattern's exact source.
        self.regexp_exact_source.retain(|&k, _| marks[k as usize]);
        // Collection hash index (u32 hash tags -> u32 slots, no Values): drop
        // a dead Map/Set's index so a recycled slot can't inherit a stale one.
        self.collection_index.retain(|&k, _| marks[k as usize]);
        self.async_stacks.retain(|&k| marks[k as usize]);
        self.shadow_realms.retain(|&k| marks[k as usize]);
        self.realm_globals.retain(|&k, _| marks[k as usize]);

        let free_after = self.heap.free_indices().len();
        let _ = free_before;
        gcstats::record(stats, n, n - free_after, swept, t_start, t_roots, t_trace, t_sweep);
        self.heap.note_gc_done(n - free_after);
        if shape_verify::enabled() {
            self.verify_all_shapes();
        }
    }

    /// Check every live object's shape against its actual layout, panicking on
    /// the first disagreement. `ZIPP_SHAPE_VERIFY=1`.
    ///
    /// A shape is a CLAIM — "my keys are these, in this order, with these
    /// descriptor bits" — and nothing native reads that claim today, so a stale
    /// one is invisible: every emitted guard is receiver identity plus a version.
    /// The moment a probe guards on a shape instead, a stale shape is a call-free
    /// read of the wrong slot. This is the check that has to exist BEFORE
    /// anything depends on it, which is why it lands ahead of the shape-keyed IC
    /// rather than with it.
    ///
    /// Runs at every collection so a desync surfaces at the first GC after the
    /// write that caused it, with the offending key named, rather than as a wrong
    /// answer much later. Panics rather than reports: a disagreement means the
    /// engine's own metadata is lying, and continuing past it produces exactly
    /// the class of bug that is hardest to trace back.
    pub(crate) fn verify_all_shapes(&self) {
        let free: std::collections::HashSet<u32> =
            self.heap.free_indices().iter().copied().collect();
        for idx in 0..self.heap.len() as u32 {
            if free.contains(&idx) {
                continue;
            }
            if let HeapObj::Object(m) = self.heap.get(idx) {
                if let Err(why) = m.verify_shape() {
                    panic!("shape/layout disagreement on heap slot {idx}: {why}");
                }
            }
        }
    }

    /// Push every heap reference held by object `idx` onto the mark stack.
    fn trace_edges(&self, idx: u32, marks: &mut [bool], stack: &mut Vec<u32>, n: usize) {
        macro_rules! m_val {
            ($v:expr) => {{
                let v: Value = $v;
                if v.is_heap() {
                    let i = v.heap_index() as usize;
                    if i < n && !marks[i] {
                        marks[i] = true;
                        stack.push(v.heap_index());
                    }
                }
            }};
        }
        macro_rules! m_idx {
            ($i:expr) => {{
                let i = $i as usize;
                if i < n && !marks[i] {
                    marks[i] = true;
                    stack.push(i as u32);
                }
            }};
        }
        // Trace an ObjMap's edges (own values, accessor setters, class link).
        macro_rules! m_objmap {
            ($map:expr) => {{
                let map = $map;
                for &v in &map.vals {
                    m_val!(v);
                }
                for a in &map.attrs {
                    m_val!(a.setter);
                }
                if let Some(c) = map.class {
                    m_idx!(c);
                }
            }};
        }

        match self.heap.get(idx) {
            HeapObj::Str(_)
            | HeapObj::Func(_)
            | HeapObj::Native(_)
            | HeapObj::Date(_)
            | HeapObj::RegExp { .. }
            | HeapObj::ArrayBuffer { .. }
            | HeapObj::Temporal { .. }
            // A BigInt (either representation) holds no heap references.
            | HeapObj::BigInt(_)
            | HeapObj::BigIntBig(_) => {}
            HeapObj::Cons { left, right, .. } => {
                m_idx!(*left);
                m_idx!(*right);
            }
            HeapObj::Closure { upvalues, this_val, .. } => {
                for &u in upvalues {
                    m_idx!(u);
                }
                m_val!(*this_val);
            }
            // A NativeClosure's whole point is the `Value`s it captured — a
            // decorator context's `addInitializer` holds the class value, its
            // `access.get` holds the class and the key. Missing this arm sweeps
            // the class out from under a context object the user kept.
            HeapObj::NativeClosure { state, .. } => {
                for &v in state {
                    m_val!(v);
                }
            }
            HeapObj::Cell(v) => m_val!(*v),
            HeapObj::EvalScope(m) => {
                for v in m.values() {
                    m_val!(*v);
                }
            }
            HeapObj::Bound { target, this, args } => {
                m_val!(*target);
                m_val!(*this);
                for &a in args {
                    m_val!(a);
                }
            }
            HeapObj::Wrapped { target, .. } => m_val!(*target),
            HeapObj::Array(items) => {
                for &v in items {
                    m_val!(v);
                }
            }
            HeapObj::Object(map) => m_objmap!(map),
            HeapObj::Promise { result, reactions, .. } => {
                m_val!(*result);
                for r in reactions.as_slice() {
                    m_val!(r.on_fulfilled);
                    m_val!(r.on_rejected);
                    m_idx!(r.dependent);
                }
            }
            HeapObj::BoundResolver { promise, .. } => m_idx!(*promise),
            HeapObj::Combinator(__c) => {
                let crate::heap::CombinatorData { results, result, cap_resolve, cap_reject, .. } = &**__c;
                for &v in results {
                    m_val!(v);
                }
                m_idx!(*result);
                m_val!(*cap_resolve);
                m_val!(*cap_reject);
            }
            HeapObj::CombinatorResolver { combinator, .. } => m_idx!(*combinator),
            HeapObj::Generator { closure, regs, .. } => {
                m_idx!(*closure);
                for &v in regs {
                    m_val!(v);
                }
            }
            HeapObj::AsyncGenerator(s) => {
                m_idx!(s.closure);
                for &v in &s.regs {
                    m_val!(v);
                }
                for r in &s.queue {
                    m_val!(r.arg);
                    m_idx!(r.promise);
                }
            }
            HeapObj::AsyncState(s) => {
                m_idx!(s.closure);
                for &v in &s.regs {
                    m_val!(v);
                }
                m_idx!(s.result);
            }
            HeapObj::Map { keys, vals } | HeapObj::WeakMap { keys, vals } => {
                for &v in keys.iter().chain(vals.iter()) {
                    m_val!(v);
                }
            }
            HeapObj::Set(items) | HeapObj::WeakSet(items) => {
                for &v in items {
                    m_val!(v);
                }
            }
            HeapObj::WeakRef(v) => m_val!(*v),
            HeapObj::FinalizationRegistry { cleanup, tokens } => {
                m_val!(*cleanup);
                for &t in tokens {
                    m_val!(t);
                }
            }
            HeapObj::Boxed { value, .. } => m_val!(*value),
            HeapObj::TypedArray { buffer, .. } => m_idx!(*buffer),
            HeapObj::DataView { buffer, .. } => m_idx!(*buffer),
            HeapObj::Proxy { target, handler, .. } => {
                m_val!(*target);
                m_val!(*handler);
            }
            HeapObj::Intl { resolved, .. } => m_idx!(*resolved),
            HeapObj::Symbol { desc, .. } => m_val!(*desc),
            HeapObj::Iterator { items, proto, live, .. } => {
                for &v in items {
                    m_val!(v);
                }
                m_idx!(*proto);
                // A live Map/Set iterator keeps its backing collection alive.
                if let Some((coll, _)) = live {
                    m_idx!(*coll);
                }
            }
            HeapObj::IterHelper { source, arg, inner, next, .. } => {
                m_val!(*source);
                m_val!(*arg);
                m_val!(*inner);
                m_val!(*next);
            }
            HeapObj::Class(c) => {
                for (_, v) in c
                    .methods
                    .iter()
                    .chain(c.getters.iter())
                    .chain(c.setters.iter())
                    .chain(c.static_getters.iter())
                    .chain(c.static_setters.iter())
                {
                    m_val!(*v);
                }
                m_objmap!(&c.statics);
                for &v in &c.computed_field_keys {
                    m_val!(v);
                }
                // The ctor / field-thunk upvalue CELLS are roots exactly like a
                // Closure's `upvalues` (marked at the Closure arm above): nothing
                // else references them once the defining frame has returned, so
                // leaving them out swept the cells a nested class closed over and
                // `new C()` then read a recycled slot (`this.t === undefined`).
                for &u in &c.ctor_upvalues {
                    m_idx!(u);
                }
                for &u in &c.field_thunk_upvalues {
                    m_idx!(u);
                }
                // Decoration state: every entry is a user-supplied Value the
                // class still owes work to (field-initializer chains that run at
                // the next `new`, addInitializer callbacks, the resolved keys and
                // the metadata object). Unreachable from anywhere else once the
                // defining frame has returned.
                if let Some(d) = &c.dec {
                    for chain in d.field_inits.iter().chain(d.elem_extra.iter()) {
                        for &v in chain {
                            m_val!(v);
                        }
                    }
                    for &v in d
                        .keys
                        .iter()
                        .chain(d.instance_extra.iter())
                        .chain(d.static_extra.iter())
                        .chain(d.class_extra.iter())
                    {
                        m_val!(v);
                    }
                    m_val!(d.metadata);
                    // The decorated class is reachable from the ORIGINAL only
                    // through here once the defining frame has returned — but
                    // `LoadClassValue` still resolves the inner binding to it.
                    m_val!(d.replacement);
                }
                if let Some(p) = c.parent {
                    m_idx!(p);
                }
            }
        }
    }
}

/// `ZIPP_GCSTATS=1` — per-collection phase timing, summed and printed at exit.
///
/// Exists for the same reason `ZIPP_BUILTINSTATS` does: B81 established that the
/// COLLECTOR is the dominant systemic cost (the same allocation loop goes 74.5 ->
/// 122.5ns purely from a larger live set) without establishing WHICH PHASE of it.
/// Root scan, tracing, the sweep and the ~30 side-table `retain` passes have four
/// completely different fixes, and this file's standing lesson is that reasoning
/// about which one *ought* to be expensive has been wrong every time it was tried.
///
/// Off, this costs one relaxed atomic load per collection.
mod gcstats {
    use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
    use std::time::Instant;

    static ON: AtomicU8 = AtomicU8::new(2);
    static COLLECTIONS: AtomicU64 = AtomicU64::new(0);
    static NS_ROOTS: AtomicU64 = AtomicU64::new(0);
    static NS_TRACE: AtomicU64 = AtomicU64::new(0);
    static NS_SWEEP: AtomicU64 = AtomicU64::new(0);
    static NS_RETAIN: AtomicU64 = AtomicU64::new(0);
    static SLOTS: AtomicU64 = AtomicU64::new(0);
    static LIVE: AtomicU64 = AtomicU64::new(0);
    static SWEPT: AtomicU64 = AtomicU64::new(0);

    #[inline]
    pub(super) fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => {
                let v = std::env::var_os("ZIPP_GCSTATS").is_some() as u8;
                ON.store(v, Ordering::Relaxed);
                v == 1
            }
        }
    }

    #[inline]
    pub(super) fn now(on: bool) -> Option<Instant> {
        on.then(Instant::now)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn record(
        on: bool,
        slots: usize,
        live: usize,
        swept: usize,
        t_start: Option<Instant>,
        t_roots: Option<Instant>,
        t_trace: Option<Instant>,
        t_sweep: Option<Instant>,
    ) {
        if !on {
            return;
        }
        let (Some(a), Some(b), Some(c), Some(d)) = (t_start, t_roots, t_trace, t_sweep) else {
            return;
        };
        let end = Instant::now();
        COLLECTIONS.fetch_add(1, Ordering::Relaxed);
        NS_ROOTS.fetch_add((b - a).as_nanos() as u64, Ordering::Relaxed);
        NS_TRACE.fetch_add((c - b).as_nanos() as u64, Ordering::Relaxed);
        NS_SWEEP.fetch_add((d - c).as_nanos() as u64, Ordering::Relaxed);
        NS_RETAIN.fetch_add((end - d).as_nanos() as u64, Ordering::Relaxed);
        SLOTS.fetch_add(slots as u64, Ordering::Relaxed);
        LIVE.fetch_add(live as u64, Ordering::Relaxed);
        SWEPT.fetch_add(swept as u64, Ordering::Relaxed);
    }

    /// `(collections, roots_ms, trace_ms, sweep_ms, retain_ms, avg_slots, avg_live, total_swept)`
    pub fn dump() -> (u64, f64, f64, f64, f64, u64, u64, u64) {
        let c = COLLECTIONS.load(Ordering::Relaxed);
        let ms = |x: u64| x as f64 / 1.0e6;
        let per = |x: u64| if c == 0 { 0 } else { x / c };
        (
            c,
            ms(NS_ROOTS.load(Ordering::Relaxed)),
            ms(NS_TRACE.load(Ordering::Relaxed)),
            ms(NS_SWEEP.load(Ordering::Relaxed)),
            ms(NS_RETAIN.load(Ordering::Relaxed)),
            per(SLOTS.load(Ordering::Relaxed)),
            per(LIVE.load(Ordering::Relaxed)),
            SWEPT.load(Ordering::Relaxed),
        )
    }
}

/// `ZIPP_SHAPE_VERIFY=1` — check every live object's shape against its layout at
/// every collection. See `Vm::verify_all_shapes`.
///
/// Off, this costs one relaxed atomic load per collection.
mod shape_verify {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);

    #[inline]
    pub(super) fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => {
                let v = std::env::var_os("ZIPP_SHAPE_VERIFY").is_some() as u8;
                ON.store(v, Ordering::Relaxed);
                v == 1
            }
        }
    }
}

pub use gcstats::dump as gc_stats;
