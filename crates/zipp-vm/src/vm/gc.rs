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
        let n = self.heap.len();
        let floor = self.gc_floor as usize;
        if floor == 0 || n <= floor {
            self.heap.note_gc_done(n);
            return;
        }
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
        for v in self.gen_callee.values() {
            root_val!(*v);
        }
        if let Some(v) = self.from_async_fn {
            root_val!(v);
        }
        if let Some(v) = self.async_dispose_fn {
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
        for i in floor..n {
            match self.heap.get(i as u32) {
                HeapObj::AsyncState(s) if !matches!(s.state, GenState::Completed) => {
                    root_idx!(i)
                }
                HeapObj::AsyncGenerator(s) if !matches!(s.state, GenState::Completed) => {
                    root_idx!(i)
                }
                _ => {}
            }
        }

        // --- Trace ---------------------------------------------------------
        while let Some(idx) = stack.pop() {
            self.trace_edges(idx, &mut marks, &mut stack, n);
        }

        // --- Sweep + prune -------------------------------------------------
        for i in floor..n {
            if !marks[i] {
                self.heap.free_slot(i as u32);
            }
        }
        // Drop side-table entries whose keyed object was reclaimed.
        self.proto_of.retain(|&k, _| marks[k as usize]);
        self.prototypes.retain(|&k, _| marks[k as usize]);
        self.fn_props.retain(|&k, _| marks[k as usize]);
        self.arr_props.retain(|&k, _| marks[k as usize]);
        self.zdt_tz.retain(|&k, _| marks[k as usize]);
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
        self.fn_name_cells.retain(|&k| marks[k as usize]);
        self.gen_callee.retain(|&k, _| marks[k as usize]);
        self.module_body_results.retain(|&k| marks[k as usize]);
        self.module_namespaces.retain(|&k, _| marks[k as usize]);
        self.closure_home.retain(|&k, _| marks[k as usize]);
        self.dispose_stacks.retain(|&k, _| marks[k as usize]);
        // Pure-bytes side table (no Values to trace): hygiene-retain only, so a
        // recycled RegExp slot can't inherit a dead pattern's exact source.
        self.regexp_exact_source.retain(|&k, _| marks[k as usize]);
        self.async_stacks.retain(|&k| marks[k as usize]);
        self.shadow_realms.retain(|&k| marks[k as usize]);
        self.realm_globals.retain(|&k, _| marks[k as usize]);

        let free_after = self.heap.free_indices().len();
        let _ = free_before;
        self.heap.note_gc_done(n - free_after);
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
            | HeapObj::BigInt(_) => {}
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
            HeapObj::Promise { result, fulfill, reject, .. } => {
                m_val!(*result);
                for r in fulfill.iter().chain(reject.iter()) {
                    m_val!(r.callback);
                    m_idx!(r.dependent);
                }
            }
            HeapObj::BoundResolver { promise, .. } => m_idx!(*promise),
            HeapObj::Combinator { results, result, cap_resolve, cap_reject, .. } => {
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
                if let Some(p) = c.parent {
                    m_idx!(p);
                }
            }
        }
    }
}
