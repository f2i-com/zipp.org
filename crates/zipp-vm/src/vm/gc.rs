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
//!
//! Two collection kinds since the nursery (NURSERY_DESIGN.md §§1-2;
//! `ZIPP_NO_NURSERY=1` restores majors-only exactly):
//!   * MAJOR — the historical collection, unchanged: full mark, sweep
//!     `floor..len`, whole-table side-table retains.
//!   * MINOR (stage 3) — a YOUNG-ONLY trace: `marks` starts as "every
//!     non-young slot is presumed live" (old objects are boundary nodes), the
//!     shared root walk and the unchanged `trace_edges` therefore push only
//!     young objects, and the remembered set supplies the young referents of
//!     old holders. The sweep walks only the alloc log; side-table pruning
//!     touches only the slots the sweep freed. An unreachable OLD object
//!     floats until a major, and majors run on the PRE-nursery schedule
//!     (`Heap::major_at`), so floats are bounded by construction.
//!
//! MINOR COMPLETENESS (why `roots ∪ remset` finds every live young object):
//! a young object Y is live iff some path of edges reaches it from a root.
//! Walk that path backwards from Y to the first OLD node H (if none exists,
//! the whole path is young and the young trace finds Y from the root
//! directly). H's edge on the path points at a young object Y', and every
//! object H can reference was allocated BEFORE the edge to it was written —
//! so that edge was STORED after Y' was born, i.e. within the current epoch
//! (Y' young ⇒ born after the last collection). H is old at minor time, and
//! ages only change AT collections, which only run at safe points: H was
//! already old when the store ran (H old now means H survived the last
//! collection, so it was old for the whole epoch). Therefore the store was
//! an old-holder/young-value store, and every such store runs a write
//! barrier (`Heap::write_barrier[_val]` at the enumerated VM chokepoints) or
//! targets a registered scan root (`Heap::register_scan_root` — receivers
//! JIT caches store into call-free), or lands in a root-like VM side table,
//! which the root walk re-scans wholesale each minor. Holder-keyed side tables
//! such as `closure_home` and `closure_new_target` are directed edges instead:
//! their record helpers run this same barrier and tracing visits the value only
//! from a reachable holder. W10 (B123) splits the barrier
//! disjunct in two: the value-tested form RECORDS Y' ITSELF (`Heap::vremset`,
//! `GEN_VLOG`-deduped) and the minor marks it directly — no holder
//! re-trace; the value-BLIND card form (`Heap::write_barrier` — batch
//! mutators, register re-parks, `Heap::replace`) still dirties H for a full
//! re-trace. Either way Y' is found, and the (young) path suffix Y'→…→Y is
//! traced normally. A recorded value OVERWRITTEN before the minor is kept
//! one epoch (a conservative float, reclaimed on the `major_at` schedule).
//! `tests/nursery_minor.rs` pins this per edge idiom, and
//! `ZIPP_NURSERY_VERIFY=1` re-runs the FULL mark beside every minor and
//! panics on the first young object the minor trace missed.

use super::{Microtask, Resume, Vm};
use crate::heap::{GenState, HeapObj};
use crate::value::Value;

/// RAII guard that suspends collection for its scope (decrements on drop, even
/// across `?` early returns). Held by native built-ins that keep an un-rooted
/// `Vec<Value>` of freshly-allocated objects while re-entering the interpreter
/// (a callback), where a safe point could otherwise free that working set.
pub(crate) struct GcGuard {
    #[cfg(not(feature = "safe-sandbox"))]
    lock: *mut u32,
    #[cfg(feature = "safe-sandbox")]
    lock: std::rc::Rc<std::cell::Cell<u32>>,
}

impl Drop for GcGuard {
    fn drop(&mut self) {
        #[cfg(feature = "safe-sandbox")]
        {
            self.lock.set(self.lock.get().saturating_sub(1));
        }
        #[cfg(not(feature = "safe-sandbox"))]
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
        // The boot allocations just pinned are also all over the young log;
        // drop them so the first minor doesn't walk them to find them marked.
        self.heap.young_reset();
    }

    /// Suspend GC for the returned guard's scope (see [`GcGuard`]).
    #[inline]
    pub(crate) fn gc_lock_guard(&mut self) -> GcGuard {
        #[cfg(feature = "safe-sandbox")]
        {
            self.gc_lock.set(self.gc_lock.get().saturating_add(1));
            return GcGuard {
                lock: std::rc::Rc::clone(&self.gc_lock),
            };
        }
        #[cfg(not(feature = "safe-sandbox"))]
        {
            self.gc_lock += 1;
            GcGuard {
                lock: &mut self.gc_lock as *mut u32,
            }
        }
    }

    /// Run a collection if one is due (or always, under stress) and it is safe.
    #[inline]
    pub(crate) fn maybe_gc(&mut self) {
        #[cfg(feature = "safe-sandbox")]
        let unlocked = self.gc_lock.get() == 0;
        #[cfg(not(feature = "safe-sandbox"))]
        let unlocked = self.gc_lock == 0;
        if unlocked && self.gc_floor != 0 && (self.heap.gc_requested() || self.gc_stress) {
            self.gc();
        }
    }

    /// Stage-3 write barrier + B6 oracle, one latched call per store
    /// chokepoint (NURSERY_DESIGN.md §1). With the nursery on, an old-holder/
    /// young-value store enters `holder` into the remembered set (holder-
    /// grain, deduped by the generation byte); with `ZIPP_GCSTATS=1` the
    /// same condition is counted per site. Both latches off — the default —
    /// this is two predicted bool loads.
    #[inline]
    pub(crate) fn store_barrier(&mut self, site: usize, holder: u32, val: Value) {
        self.heap.write_barrier_val(holder, val);
        if self.heap.oracle_on()
            && val.is_heap()
            && !self.heap.oracle_young(holder)
            && self.heap.oracle_young(val.heap_index())
        {
            crate::heap::gcoracle::hit(site);
        }
    }

    /// [`Vm::store_barrier`] for a holder still in `Value` form (primitives
    /// are no one's old generation — skipped).
    #[inline]
    pub(crate) fn store_barrier_v(&mut self, site: usize, holder: Value, val: Value) {
        if holder.is_heap() {
            self.store_barrier(site, holder.heap_index(), val);
        }
    }

    /// Record the internal `[[HomeObject]]` edge owned by an object-literal
    /// function. The side table is storage only: liveness flows from a reachable
    /// function KEY to its home VALUE, exactly as if the value were a field on
    /// the function object. It is not a root in its own right.
    #[inline]
    pub(crate) fn record_closure_home(&mut self, closure: u32, home: Value) {
        // The key may already be old when SetHomeObject runs (GC stress can
        // collect between MakeFunc and the later SetHomeObject bytecode), while
        // the home can still be young. Treat the side-table entry as a real
        // holder edge so a minor cannot sweep the home out from under `super`.
        self.store_barrier(crate::heap::gcoracle::CLOSURE_HOME, closure, home);
        self.closure_home.insert(closure, home, self.heap.len());
    }

    /// Record an arrow closure's lexical `new.target`. Like `closure_home`, this
    /// is a strong edge from the keyed closure, not an unconditional GC root.
    #[inline]
    pub(crate) fn record_closure_new_target(&mut self, closure: u32, new_target: Value) {
        self.store_barrier(
            crate::heap::gcoracle::CLOSURE_NEW_TARGET,
            closure,
            new_target,
        );
        self.closure_new_target.insert(closure, new_target);
    }

    fn gc(&mut self) {
        let _prof = crate::vm::prof::enter(crate::vm::prof::Phase::Gc);
        let n = self.heap.len();
        let floor = self.gc_floor as usize;
        if floor == 0 || n <= floor {
            self.heap.note_gc_done(n);
            return;
        }
        // Minor or major? `Heap::minor_due` holds the whole policy. The minor
        // is its own routine (young-only trace, young-only sweep) so the
        // major body below stays exactly the historical collector.
        if self.heap.minor_due(self.gc_stress) {
            self.gc_minor(n);
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
        self.mark_roots(&mut marks, &mut stack, n);
        self.gc_major_tail(n, floor, free_before, marks, stack, stats, t_start);
    }

    /// Enumerate the COMPLETE root set into `marks`/`stack` — shared verbatim
    /// by the major, the minor and the `ZIPP_NURSERY_VERIFY` full-mark check,
    /// so the three can never drift. For a minor every OLD slot arrives
    /// pre-marked (`Heap::gen_nonyoung_marks`), so this identical enumeration
    /// pushes — and the trace then walks — only YOUNG objects; old side-table
    /// holders and pinned built-ins are boundary nodes whose young referents
    /// the remembered set supplies (see the module doc's completeness
    /// argument).
    fn mark_roots(&mut self, marks: &mut Vec<bool>, stack: &mut Vec<u32>, n: usize) {
        let floor = self.gc_floor as usize;
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
        // Short interpreter string constants memoized by immutable
        // (function, constant-slot). The cache is bounded and its Values are
        // roots just like the region-embedded constant strings below.
        for &v in self.const_string_cache.values() {
            root_val!(v);
        }
        // Strings interned at region-compile time whose bits are embedded in
        // native code (LoadConst immediates) — see `jit_const_strings`.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        for &v in &self.jit_const_strings {
            root_val!(v);
        }
        // Frame-free Tier-C activations have no `Frame` entry. Trace exact
        // closure/callee identities for the current activation AND every
        // suspended native caller: a nested body may detach its caller from all
        // JS-visible objects before allocating/collecting.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        for state in std::iter::once(&self.jit_tierc_activation)
            .chain(self.jit_tierc_activation_stack.iter())
        {
            if state.active {
                root_idx!(state.closure);
                root_idx!(state.callee);
            }
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
                Microtask::Reaction {
                    callback,
                    arg,
                    dependent,
                    ..
                } => {
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
                Microtask::CombinatorStep {
                    combinator, arg, ..
                } => {
                    root_idx!(*combinator);
                    root_val!(*arg);
                }
                Microtask::ThenableJob {
                    thenable,
                    then,
                    promise,
                } => {
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
            for &v in m.vals_slice() {
                root_val!(v);
            }
            if m.may_deviate_attrs() {
                for a in m.attrs_iter() {
                    root_val!(a.setter);
                }
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
        for rec in self.regexp_string_iters.values() {
            root_idx!(rec.matcher);
            root_val!(rec.subject);
        }
        // A scalarized direct exec result is still logically held by its
        // skipped global store.  Its capture ranges therefore root the one
        // immutable subject until the region overwrites or materializes it.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if let Some(pending) = &self.regexp_scalar_exec_pending {
            root_val!(pending.subject);
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
    }

    /// The MAJOR collection's trace + sweep + retain — the historical
    /// collector, unchanged (the root walk was shared out into `mark_roots`).
    #[allow(clippy::too_many_arguments)]
    fn gc_major_tail(
        &mut self,
        n: usize,
        floor: usize,
        free_before: usize,
        mut marks: Vec<bool>,
        mut stack: Vec<u32>,
        stats: bool,
        t_start: Option<std::time::Instant>,
    ) {
        let t_roots = gcstats::now(stats);
        // --- Trace ---------------------------------------------------------
        // B6 oracle (stats only): attribute marking work to the generation of
        // the object it was done FROM. Work units = 1 per object visited plus 1
        // per newly-marked edge pushed; the old share is the exact part a
        // young-only minor trace (old objects treated as pre-marked) would skip.
        let (mut trace_young, mut trace_old) = (0u64, 0u64);
        let oracle = stats && self.heap.oracle_on();
        if oracle {
            while let Some(idx) = stack.pop() {
                let before = stack.len();
                self.trace_edges(idx, &mut marks, &mut stack, n);
                let work = (stack.len() - before) as u64 + 1;
                if self.heap.oracle_young(idx) {
                    trace_young += work;
                } else {
                    trace_old += work;
                }
            }
        } else {
            while let Some(idx) = stack.pop() {
                self.trace_edges(idx, &mut marks, &mut stack, n);
            }
        }
        let t_trace = gcstats::now(stats);

        // --- Sweep + prune -------------------------------------------------
        // B196a: the major's dead walk restocks the recycle pool too (the
        // sorted, demand-trimmed pool made that safe; the pre-sort balloon
        // and scatter costs are the B194/B196 ledger rows' story).
        #[cfg(not(feature = "safe-sandbox"))]
        self.heap.obj_pool_refill_scope(major_refill_enabled());
        let mut swept = 0usize;
        if oracle {
            // B6 oracle: split the swept walk by generation. `alloc_log`-style
            // young slot count comes from the walk itself (free tombstones read
            // old — they were freed at a previous collection, so their stale
            // `born` predates the current epoch); a young-only sweep would walk
            // exactly `walk_young` of the `n - floor` slots walked today.
            let allocs = self.heap.oracle_allocs();
            let (mut marked_young, mut marked_old) = (0u64, 0u64);
            let (mut swept_young, mut swept_old) = (0u64, 0u64);
            let mut walk_young = 0u64;
            for i in floor..n {
                let young = self.heap.oracle_young(i as u32);
                if young {
                    walk_young += 1;
                }
                if !marks[i] {
                    self.heap.free_slot(i as u32);
                    swept += 1;
                    if young {
                        swept_young += 1;
                    } else {
                        swept_old += 1;
                    }
                } else if young {
                    marked_young += 1;
                } else {
                    marked_old += 1;
                }
            }
            // Pre-existing free tombstones were force-marked above and are not
            // live old objects — take them back out of the marked-old count.
            marked_old -= free_before as u64;
            gcstats::record_gen(
                marked_young,
                marked_old,
                swept_young,
                swept_old,
                walk_young,
                (n - floor) as u64,
                allocs,
                trace_young,
                trace_old,
            );
        } else {
            for i in floor..n {
                if !marks[i] {
                    self.heap.free_slot(i as u32);
                    swept += 1;
                }
            }
        }
        #[cfg(not(feature = "safe-sandbox"))]
        self.heap.obj_pool_refill_scope(false);
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
        self.deleted_callable_intrinsics
            .retain(|&(k, _)| marks[k as usize]);
        self.array_length_nonwritable.retain(|&k| marks[k as usize]);
        // Virtual lengths of sparse arrays (u32 values — nothing to trace; the
        // sparse ELEMENTS live in arr_props, rooted above).
        self.array_js_len.retain(|&k, _| marks[k as usize]);
        self.ab_max.retain(|&k, _| marks[k as usize]);
        self.ta_tracking.retain(|&k| marks[k as usize]);
        self.dv_tracking.retain(|&k| marks[k as usize]);
        self.regexp_string_iters.retain(|&k, _| marks[k as usize]);
        self.matchall_batches.retain(|&k, _| marks[k as usize]);
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
            self.brand_private_names
                .retain(|b, _| live_brands.contains(b));
        }
        self.shared_buffers.retain(|&k| marks[k as usize]);
        self.immutable_buffers.retain(|&k| marks[k as usize]);
        self.error_data.retain(|&k| marks[k as usize]);
        self.arguments_objs.retain(|&k, _| marks[k as usize]);
        self.gen_args_obj
            .retain(|&k, &mut v| marks[k as usize] && marks[v as usize]);
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
        gcstats::record(
            stats,
            false,
            n,
            n - free_after,
            swept,
            t_start,
            t_roots,
            t_trace,
            t_sweep,
        );
        // Floated-swept estimate (stats only): what this major freed that was
        // NOT allocated in the current epoch — i.e. garbage the minors before
        // it had already been unable to reclaim. Read before `note_gc_done`
        // clears the young log.
        let swept_floated = if stats && self.heap.nursery_on() {
            let swept_young = self
                .heap
                .young_log()
                .iter()
                .filter(|&&i| !marks[i as usize])
                .count();
            (swept - swept_young) as u64
        } else {
            0
        };
        gcstats::record_major(swept_floated, n);
        self.heap.note_gc_done(n - free_after);
        if self.heap.oracle_on() {
            // B6 oracle: this collection is now "the last GC" — survivors age out.
            self.heap.oracle_next_epoch();
        }
        if shape_verify::enabled() {
            self.verify_all_shapes();
        }
    }

    /// Stage-3 MINOR collection (NURSERY_DESIGN.md §§1-2): minor liveness =
    /// reachable-from(roots ∪ dirty old holders) ∩ young. Old objects are
    /// BOUNDARY nodes — pre-marked from the generation bytes, presumed live,
    /// never traced; the remembered set (this epoch's write-barrier hits) and
    /// the persistent scan roots (call-free JIT store targets) supply their
    /// young referents. Root-like VM side tables are re-scanned by the shared
    /// `mark_roots`; keyed directed edges such as `closure_home` are traced
    /// from their reachable holder instead. Cost is O(roots + young live +
    /// dirty edge lists), independent of the old heap — the term the stage-1
    /// full mark
    /// still paid on every minor (B120's refutation), and the whole
    /// economics flip of stage 3: regex-log-scan's ~128ms/run of 95.8%-old
    /// trace work simply stops happening at minors.
    fn gc_minor(&mut self, n: usize) {
        let stats = gcstats::enabled();
        let t_start = gcstats::now(stats);
        // Every non-young slot (old, pinned prefix, free tombstone) is
        // presumed live: the shared root walk and the unchanged `trace_edges`
        // push only unmarked — i.e. YOUNG — objects from here on. W10: the
        // vector is CACHED between minors (a fresh O(heap) build per minor
        // measured 25.4ms/run on async-promise-chain — B123); the take
        // re-derives it in O(young log). Equivalence with the fresh build is
        // re-proven per minor under the verifier.
        let mut marks = self.heap.take_nonyoung_marks();
        if nursery_verify::enabled() {
            let fresh = self.heap.gen_nonyoung_marks();
            assert!(
                marks == fresh,
                "nonyoung-cache drift: cached mark vector != fresh gen_nonyoung_marks"
            );
        }
        let mut stack: Vec<u32> = Vec::with_capacity(1024);
        self.mark_roots(&mut marks, &mut stack, n);
        let t_roots = gcstats::now(stats);
        // Dirty OLD holders: scan their edge lists — the one thing an old
        // object contributes to young liveness. `trace_edges` on a marked
        // holder pushes its unmarked (young) referents and nothing else, so
        // a holder appearing twice (remset ∩ scan roots) costs a re-scan,
        // never re-tracing.
        let dirty = self.heap.dirty_for_trace();
        for &h in &dirty {
            self.trace_edges(h, &mut marks, &mut stack, n);
        }
        // W10 value-grain records (B123): each entry is a young value some
        // old-clean holder received this epoch — mark it directly and trace
        // from it. This is what replaced the dirty-holder full edge-list
        // re-trace for value-form stores (59.3ms/run on regex-log-scan).
        for i in 0..self.heap.value_remset().len() {
            let v = self.heap.value_remset()[i];
            if !marks[v as usize] {
                marks[v as usize] = true;
                stack.push(v);
            }
        }
        while let Some(idx) = stack.pop() {
            self.trace_edges(idx, &mut marks, &mut stack, n);
        }
        let t_trace = gcstats::now(stats);
        if nursery_verify::enabled() {
            // Before the sweep, while the young log is intact.
            self.verify_minor_marks(&marks, n);
        }
        let log_len = self.heap.young_log().len();
        // `free_slot` appends every reclaimed young slot to `Heap::free`.
        // No heap slot can be allocated or reused during this collector-only
        // window, so the appended suffix is the exact freed set in sweep order.
        // Reuse it rather than allocating and filling a duplicate `Vec<u32>`.
        let free_before = self.heap.free_indices().len();
        let swept = self.heap.sweep_young(&marks);
        debug_assert_eq!(self.heap.free_indices().len() - free_before, swept);
        let t_sweep = gcstats::now(stats);
        // W10 survival-adaptive budget (B123) — before note_minor_done reads
        // it for the next threshold. Skipped under stress: epochs of ~1 alloc
        // make the ratio noise, and stress ignores gc_threshold anyway.
        if !self.gc_stress {
            self.heap.adapt_young_budget(log_len - swept, log_len);
        }
        // A minor prunes only what it FREED (see `prune_freed`); the
        // whole-table retains are the major's.
        self.prune_freed(free_before, &marks);
        // W10: restore the freed slots to TRUE (their tombstones are OLD) —
        // survivors were already set true by the trace — and the vector is
        // all-true again: exactly the next minor's base. Stash it for reuse.
        for &i in &self.heap.free_indices()[free_before..] {
            marks[i as usize] = true;
        }
        self.heap.stash_nonyoung_marks(marks);
        // Dirty holders' young referents were just promoted: their edges are
        // old→old now, so they go back to clean (the scan roots persist).
        self.heap.remset_reset();
        self.heap.vremset_reset();
        let free_after = self.heap.free_indices().len();
        gcstats::record(
            stats,
            true,
            n,
            n - free_after,
            swept,
            t_start,
            t_roots,
            t_trace,
            t_sweep,
        );
        gcstats::record_minor(swept as u64, dirty.len() as u64, n);
        gcstats::record_budget(self.heap.young_budget() as u64);
        self.heap.note_minor_done(n - free_after);
        if self.heap.oracle_on() {
            self.heap.oracle_next_epoch();
        }
        if shape_verify::enabled() {
            self.verify_all_shapes();
        }
    }

    /// `ZIPP_NURSERY_VERIFY=1`: run the FULL mark beside a minor's young-only
    /// trace and panic on the first live young object the minor missed — a
    /// write-barrier hole caught AT the collection where it opened, with the
    /// slot named, instead of surfacing as silent corruption arbitrarily
    /// later (the failure mode NURSERY_DESIGN.md names as this design's
    /// biggest risk). Test/stress use only: it doubles the mark.
    fn verify_minor_marks(&mut self, minor_marks: &[bool], n: usize) {
        let mut marks = vec![false; n];
        for &idx in self.heap.free_indices() {
            marks[idx as usize] = true;
        }
        let mut stack: Vec<u32> = Vec::with_capacity(1024);
        self.mark_roots(&mut marks, &mut stack, n);
        while let Some(idx) = stack.pop() {
            self.trace_edges(idx, &mut marks, &mut stack, n);
        }
        for &y in self.heap.young_log() {
            if marks[y as usize] && !minor_marks[y as usize] {
                panic!(
                    "nursery write-barrier hole: young slot {y} ({:?}) is live under \
                     the full mark but the minor trace missed it — an old→young \
                     store site is not barriered",
                    std::mem::discriminant(self.heap.get(y))
                );
            }
        }
    }

    /// Minor-collection side-table prune: drop entries keyed by a slot FREED
    /// BY THIS MINOR — the only pruning a minor needs. The major's whole-table
    /// `retain` passes exist so a slot returned to the free list never
    /// resurfaces with a dead occupant's state (a recycled slot inheriting a
    /// TDZ mark, a frozen length, a brand). A minor frees only unmarked YOUNG
    /// slots, so: an entry keyed by a live object must stay (identical to the
    /// major — `marks` is exact), and an entry keyed by floated garbage
    /// CANNOT need pruning here, because its slot was not freed, cannot be
    /// reused before some major frees it, and that major prunes it through
    /// the unchanged retain path.
    ///
    /// Cost shape: each table pays `min(table, freed)` instead of the major's
    /// unconditional whole-table walk — scan whichever side is smaller. The
    /// std tables compare `capacity()` because their `retain` walks bucket
    /// capacity (which never shrinks after a burst); `SlotTable::retain`
    /// walks live entries, so those compare `len()`.
    fn prune_freed(&mut self, free_before: usize, live_bits: &[bool]) {
        // `sweep_young` is the only operation between `free_before`'s capture
        // and this call. It only appends to the free list, so this suffix is
        // stable and contains each slot reclaimed by this minor exactly once.
        let freed = &self.heap.free_indices()[free_before..];
        if freed.is_empty() {
            return;
        }
        // Before `gc_minor` restores the cache base, `live_bits` is true for
        // every non-young slot and every traced live young slot, and false
        // exactly for the unreachable young slots in `freed`. It is therefore
        // the complement of the old heap-sized `freed_bits` allocation.
        debug_assert!(freed.iter().all(|&i| !live_bits[i as usize]));
        // Slot-keyed std HashMap: per-slot removes, unless the table's own
        // walk is cheaper.
        macro_rules! prune_map {
            ($t:expr) => {
                if !$t.is_empty() {
                    if $t.capacity() <= freed.len() {
                        $t.retain(|&k, _| live_bits[k as usize]);
                    } else {
                        for i in freed {
                            $t.remove(i);
                        }
                    }
                }
            };
        }
        // Same, for slot-keyed std HashSet (one-argument retain closure).
        macro_rules! prune_set {
            ($t:expr) => {
                if !$t.is_empty() {
                    if $t.capacity() <= freed.len() {
                        $t.retain(|&k| live_bits[k as usize]);
                    } else {
                        for i in freed {
                            $t.remove(i);
                        }
                    }
                }
            };
        }
        // Same, for `SlotTable` (dense retain walk, O(1) removes).
        macro_rules! prune_slots {
            ($t:expr) => {
                if !$t.is_empty() {
                    if $t.len() <= freed.len() {
                        $t.retain(|&k, _| live_bits[k as usize]);
                    } else {
                        for i in freed {
                            $t.remove(i);
                        }
                    }
                }
            };
        }
        // One entry per retain in the major block above, same order, so the
        // two lists can be diffed against each other. A table missing here
        // would hand a recycled slot its dead occupant's state.
        prune_slots!(self.proto_of);
        prune_set!(self.this_tdz);
        prune_set!(self.super_called);
        prune_map!(self.super_this);
        prune_map!(self.prototypes);
        prune_slots!(self.fn_props);
        prune_slots!(self.arr_props);
        prune_slots!(self.regexp_result_props);
        prune_map!(self.zdt_tz);
        prune_map!(self.temporal_cal);
        // Tuple-keyed — cannot be removed by slot alone; the set holds at
        // most a few deleted name/length intrinsics, so scan it.
        if !self.deleted_callable_intrinsics.is_empty() {
            self.deleted_callable_intrinsics
                .retain(|&(k, _)| live_bits[k as usize]);
        }
        prune_set!(self.array_length_nonwritable);
        prune_slots!(self.array_js_len);
        prune_map!(self.ab_max);
        prune_set!(self.ta_tracking);
        prune_set!(self.dv_tracking);
        prune_map!(self.regexp_string_iters);
        prune_map!(self.matchall_batches);
        let brand_entries = self.method_brand.len() + self.instance_brand.len();
        prune_map!(self.method_brand);
        prune_map!(self.instance_brand);
        // Keyed by brand id, pruned on its VALUE (the owning class slot).
        if !self.brand_owner.is_empty() {
            self.brand_owner.retain(|_, &mut c| live_bits[c as usize]);
        }
        prune_map!(self.closure_eval_scope);
        prune_map!(self.eval_scope_parent);
        prune_map!(self.shadow_fn_realm);
        prune_map!(self.private_fields);
        // The declared-name recompute (the major runs it whenever the table
        // is non-empty) only shrinks when its source maps did; when no brand
        // entry died this minor it is the identity and is skipped. Deferring it further is safe regardless:
        // brand ids are minted from a monotone counter (`next_private_brand`)
        // and never recycled, so a stale record can never be misread — it is
        // memory, reclaimed at the next recompute.
        if !self.brand_private_names.is_empty()
            && self.method_brand.len() + self.instance_brand.len() != brand_entries
        {
            let mut live_brands: std::collections::HashSet<u64> = std::collections::HashSet::new();
            for bs in self.method_brand.values() {
                live_brands.extend(bs.iter().copied());
            }
            for bs in self.instance_brand.values() {
                live_brands.extend(bs.iter().copied());
            }
            self.brand_private_names
                .retain(|b, _| live_brands.contains(b));
        }
        prune_set!(self.shared_buffers);
        prune_set!(self.immutable_buffers);
        prune_set!(self.error_data);
        prune_slots!(self.arguments_objs);
        // Dies with EITHER endpoint: the generator (key) or its arguments
        // object (value).
        if !self.gen_args_obj.is_empty() {
            self.gen_args_obj
                .retain(|&k, &mut v| live_bits[k as usize] && live_bits[v as usize]);
        }
        prune_set!(self.fn_name_cells);
        prune_set!(self.const_cells);
        prune_map!(self.gen_callee);
        prune_set!(self.module_body_results);
        prune_map!(self.module_namespaces);
        self.closure_home.prune_freed(freed, live_bits);
        prune_map!(self.closure_new_target);
        prune_map!(self.dispose_stacks);
        prune_map!(self.regexp_exact_source);
        prune_map!(self.collection_index);
        prune_set!(self.async_stacks);
        prune_set!(self.shadow_realms);
        prune_map!(self.realm_globals);
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
                for &v in map.vals_slice() {
                    m_val!(v);
                }
                if map.may_deviate_attrs() {
                    for a in map.attrs_iter() {
                        m_val!(a.setter);
                    }
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
            HeapObj::Cell => m_val!(Value::from_bits(self.heap.cell_mirror_bits(idx))),
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

        // These side tables represent internal fields of the keyed function.
        // Trace them only when that function is itself reachable. Rooting every
        // value from `mark_roots` makes a dead object-literal method cycle
        // immortal: home -> method (own property), method -> home (this table).
        if let Some(&home) = self.closure_home.get(&idx) {
            m_val!(home);
        }
        if let Some(&new_target) = self.closure_new_target.get(&idx) {
            m_val!(new_target);
        }
    }
}

#[cfg(test)]
mod closure_side_table_gc_tests {
    use super::*;
    use crate::heap::{HeapObj, ObjMap};
    use crate::vm::ClosureHomeTable;

    fn program_with_keep_global() -> crate::bytecode::Program {
        let src = "var keep;";
        let ast = crate::front::parse_script(src).expect("source parses");
        crate::compile::compile_program(&ast, src).expect("source compiles")
    }

    fn keep_slot(program: &crate::bytecode::Program) -> usize {
        program
            .global_names
            .iter()
            .position(|name| name == "keep")
            .expect("keep global")
    }

    fn object_owning(method: u32) -> HeapObj {
        let mut map = ObjMap::with_capacity(1);
        map.push_data("method".into(), Value::heap(method));
        HeapObj::Object(Box::new(map))
    }

    fn home_tables() -> [(&'static str, ClosureHomeTable); 2] {
        [
            ("dense", ClosureHomeTable::dense_for_test()),
            ("map", ClosureHomeTable::map_for_test()),
        ]
    }

    #[test]
    fn dense_home_presence_is_not_a_value_sentinel() {
        let mut table = ClosureHomeTable::dense_for_test();

        // `undefined` is a real stored edge. Presence, replacement and length
        // must therefore not depend on any distinguished Value bit pattern.
        assert_eq!(table.insert(1, Value::UNDEFINED, 130), None);
        assert_eq!(table.get(&1), Some(&Value::UNDEFINED));
        assert_eq!(table.len(), 1);

        assert_eq!(table.insert(65, Value::int(7), 130), None);
        assert_eq!(table.insert(65, Value::int(8), 130), Some(Value::int(7)));
        assert_eq!(table.len(), 2);
        assert_eq!(table.remove(&1), Some(Value::UNDEFINED));
        assert!(!table.contains_key(&1));
        assert_eq!(table.len(), 1);

        // Exercise a third bitmap word and both retain outcomes. Removed slots
        // clear the bit and their stale bits; retained values remain mutable,
        // matching HashMap::retain's contract used by major GC.
        assert_eq!(table.insert(129, Value::int(9), 130), None);
        table.retain(|&key, value| {
            if key == 129 {
                *value = Value::int(10);
                true
            } else {
                false
            }
        });
        assert!(!table.contains_key(&65));
        assert_eq!(table.get(&129), Some(&Value::int(10)));
        assert_eq!(table.len(), 1);

        let ClosureHomeTable::Dense(dense) = table else {
            unreachable!()
        };
        assert_eq!(dense.values[1], Value::UNDEFINED);
        assert_eq!(dense.values[65], Value::UNDEFINED);
        assert_ne!(dense.present[0] & (1 << 1), 1 << 1);
        assert_ne!(dense.present[1] & (1 << 1), 1 << 1);
        assert_eq!(dense.present[2] & (1 << 1), 1 << 1);
    }

    #[test]
    fn dead_method_home_cycle_is_collected() {
        for (label, table) in home_tables() {
            let program = program_with_keep_global();
            let mut vm = Vm::new(&program);
            vm.run().expect("program runs");
            vm.closure_home = table;
            // Force a major so the assertion tests the complete graph,
            // independent of the process-wide nursery environment.
            vm.heap.set_nursery(false);

            let method = vm.heap.alloc(HeapObj::Func(0));
            let home = vm.heap.alloc(object_owning(method));
            vm.record_closure_home(method, Value::heap(home));

            vm.gc();

            assert!(vm.heap.free_indices().contains(&method), "{label}");
            assert!(vm.heap.free_indices().contains(&home), "{label}");
            assert!(!vm.closure_home.contains_key(&method), "{label}");
        }
    }

    #[test]
    fn reachable_old_method_keeps_young_home_across_minor() {
        for (label, table) in home_tables() {
            let program = program_with_keep_global();
            let slot = keep_slot(&program);
            let mut vm = Vm::new(&program);
            vm.run().expect("program runs");
            vm.closure_home = table;
            vm.heap.set_nursery(true);

            let method = vm.heap.alloc(HeapObj::Func(0));
            vm.globals[slot] = Value::heap(method);
            vm.gc(); // rooted survivor is now old

            let home = vm.heap.alloc(HeapObj::Object(Box::new(ObjMap::new())));
            vm.record_closure_home(method, Value::heap(home));
            vm.gc(); // must see the side-table write barrier

            assert!(!vm.heap.free_indices().contains(&method), "{label}");
            assert!(!vm.heap.free_indices().contains(&home), "{label}");
            assert_eq!(
                vm.closure_home.get(&method),
                Some(&Value::heap(home)),
                "{label}"
            );

            // Once the only true root disappears, the directed edge must not
            // turn back into a root: a following major reclaims both endpoints.
            vm.globals[slot] = Value::UNDEFINED;
            vm.heap.set_nursery(false);
            vm.gc();
            assert!(vm.heap.free_indices().contains(&method), "{label}");
            assert!(vm.heap.free_indices().contains(&home), "{label}");
        }
    }

    #[test]
    fn extracted_no_super_method_keeps_home_for_weak_observation_across_major() {
        let src = r#"
            var keep, observer, registry;
            function build() { return { plain() { return 1; } }; }
        "#;
        let ast = crate::front::parse_script(src).expect("source parses");
        let program = crate::compile::compile_program(&ast, src).expect("source compiles");
        let slot = |name: &str| {
            program
                .global_names
                .iter()
                .position(|candidate| candidate == name)
                .unwrap_or_else(|| panic!("global {name}"))
        };
        let method_func = program
            .functions
            .iter()
            .position(|proto| proto.name == "plain")
            .expect("plain method proto") as u32;

        for (label, table) in home_tables() {
            let mut vm = Vm::new(&program);
            vm.run().expect("program runs");
            vm.closure_home = table;
            vm.heap.set_nursery(false);

            let method = vm.heap.alloc(HeapObj::Func(method_func));
            let home = vm.heap.alloc(HeapObj::Object(Box::new(ObjMap::new())));
            vm.record_closure_home(method, Value::heap(home));
            vm.globals[slot("keep")] = Value::heap(method);

            // Registering is a weak-observer-style operation: the registry is
            // rooted, but its target is deliberately not. The method's internal
            // [[HomeObject]] edge must therefore keep `home` alive even though
            // this method's bytecode contains no `super`.
            let cleanup = vm.heap.alloc(HeapObj::Func(0));
            let registry = vm.heap.alloc(HeapObj::FinalizationRegistry {
                cleanup: Value::heap(cleanup),
                tokens: Vec::new(),
            });
            vm.globals[slot("registry")] = Value::heap(registry);
            vm.finreg_method(
                Value::heap(registry),
                "register",
                &[Value::heap(home), Value::int(7)],
            )
            .expect("register accepts the home");

            vm.gc();
            assert!(!vm.heap.free_indices().contains(&home), "{label}");
            assert_eq!(
                vm.closure_home.get(&method),
                Some(&Value::heap(home)),
                "{label}"
            );

            // The current WeakRef implementation conservatively keeps its
            // target strong. This second phase still pins the future contract:
            // once WeakRef becomes truly weak, the already-rooted method edge
            // above must continue to make deref observe `home` after a major.
            let observer = vm.heap.alloc(HeapObj::WeakRef(Value::heap(home)));
            vm.globals[slot("observer")] = Value::heap(observer);
            vm.gc();
            assert!(
                matches!(
                    vm.heap.get(observer),
                    HeapObj::WeakRef(target) if *target == Value::heap(home)
                ),
                "{label}"
            );
            assert!(!vm.heap.free_indices().contains(&home), "{label}");
        }
    }

    #[test]
    fn reclaimed_method_slot_never_inherits_a_stale_home() {
        for nursery in [false, true] {
            for (label, table) in home_tables() {
                let program = program_with_keep_global();
                let mut vm = Vm::new(&program);
                vm.run().expect("program runs");
                vm.closure_home = table;
                vm.heap.set_nursery(nursery);

                let method = vm.heap.alloc(HeapObj::Func(0));
                let home = vm.heap.alloc(object_owning(method));
                vm.record_closure_home(method, Value::heap(home));
                vm.gc();
                assert!(
                    vm.heap.free_indices().contains(&method),
                    "{label}/{nursery}"
                );
                assert!(!vm.closure_home.contains_key(&method), "{label}/{nursery}");

                let mut reused = false;
                for _ in 0..4 {
                    let fresh = vm.heap.alloc(HeapObj::Func(0));
                    if fresh == method {
                        reused = true;
                        assert!(
                            !vm.closure_home.contains_key(&fresh),
                            "{label}/{nursery}: recycled slot inherited its old home"
                        );
                        break;
                    }
                }
                assert!(
                    reused,
                    "{label}/{nursery}: fixture did not reuse method slot"
                );
            }
        }
    }

    #[test]
    fn closure_new_target_is_a_keyed_edge_not_a_root() {
        let program = program_with_keep_global();
        let mut vm = Vm::new(&program);
        vm.run().expect("program runs");
        vm.heap.set_nursery(false);

        let arrow = vm.heap.alloc(HeapObj::Func(0));
        let new_target = vm.heap.alloc(object_owning(arrow));
        vm.record_closure_new_target(arrow, Value::heap(new_target));

        vm.gc();

        assert!(vm.heap.free_indices().contains(&arrow));
        assert!(vm.heap.free_indices().contains(&new_target));
        assert!(!vm.closure_new_target.contains_key(&arrow));
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
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
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
    // B6 generational-oracle splits (NURSERY_DESIGN.md §6): young = allocated
    // since the previous collection. Summed over all collections.
    static MARKED_YOUNG: AtomicU64 = AtomicU64::new(0);
    static MARKED_OLD: AtomicU64 = AtomicU64::new(0);
    static SWEPT_YOUNG: AtomicU64 = AtomicU64::new(0);
    static SWEPT_OLD: AtomicU64 = AtomicU64::new(0);
    static WALK_YOUNG: AtomicU64 = AtomicU64::new(0);
    static WALK_TOTAL: AtomicU64 = AtomicU64::new(0);
    static ALLOCED: AtomicU64 = AtomicU64::new(0);
    static TRACE_YOUNG: AtomicU64 = AtomicU64::new(0);
    static TRACE_OLD: AtomicU64 = AtomicU64::new(0);
    // Nursery split. The counters/peaks update UNCONDITIONALLY — a handful
    // of relaxed atomics per COLLECTION (collections are rare; this is
    // nothing next to the mark), which lets the bounded-heap tests read them
    // without env plumbing. The ns/floated-swept pieces ride the
    // `ZIPP_GCSTATS` gate like every other timing.
    static MINORS: AtomicU64 = AtomicU64::new(0);
    static MAJORS: AtomicU64 = AtomicU64::new(0);
    static PEAK_SLOTS: AtomicU64 = AtomicU64::new(0);
    static MINOR_SWEPT: AtomicU64 = AtomicU64::new(0);
    // Stage 3: peak dirty-holder count a single minor re-traced (remset +
    // persistent scan roots) — the remembered set's size, i.e. the barrier's
    // cost denominator. (Stage 1 recorded the float census here; a
    // young-only trace cannot measure floats, and `Heap::major_at` bounds
    // them instead.)
    static DIRTY_PEAK: AtomicU64 = AtomicU64::new(0);
    static FLOATED_SWEPT: AtomicU64 = AtomicU64::new(0);
    static NS_MINOR: AtomicU64 = AtomicU64::new(0);
    static NS_MAJOR: AtomicU64 = AtomicU64::new(0);
    // W10: the survival-adaptive young budget's last value and peak.
    static BUDGET_LAST: AtomicU64 = AtomicU64::new(0);
    static BUDGET_PEAK: AtomicU64 = AtomicU64::new(0);

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
        minor: bool,
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
        let kind = if minor { &NS_MINOR } else { &NS_MAJOR };
        kind.fetch_add((end - a).as_nanos() as u64, Ordering::Relaxed);
        SLOTS.fetch_add(slots as u64, Ordering::Relaxed);
        LIVE.fetch_add(live as u64, Ordering::Relaxed);
        SWEPT.fetch_add(swept as u64, Ordering::Relaxed);
    }

    /// Unconditional per-MINOR accounting (see the statics' comment).
    /// `dirty` = old holders whose edge lists this minor re-traced.
    pub(super) fn record_minor(swept_young: u64, dirty: u64, slots: usize) {
        MINORS.fetch_add(1, Ordering::Relaxed);
        MINOR_SWEPT.fetch_add(swept_young, Ordering::Relaxed);
        DIRTY_PEAK.fetch_max(dirty, Ordering::Relaxed);
        PEAK_SLOTS.fetch_max(slots as u64, Ordering::Relaxed);
    }

    /// W10: the live young budget after each minor (last + peak), for the
    /// `[gc-nursery]` report — the adaptive controller's observable.
    pub(super) fn record_budget(budget: u64) {
        BUDGET_LAST.store(budget, Ordering::Relaxed);
        BUDGET_PEAK.fetch_max(budget, Ordering::Relaxed);
    }

    /// Unconditional per-MAJOR accounting (`floated_swept` is stats-gated by
    /// the caller — computing it costs a young-log walk).
    pub(super) fn record_major(floated_swept: u64, slots: usize) {
        MAJORS.fetch_add(1, Ordering::Relaxed);
        FLOATED_SWEPT.fetch_add(floated_swept, Ordering::Relaxed);
        PEAK_SLOTS.fetch_max(slots as u64, Ordering::Relaxed);
    }

    /// Nursery totals: `(minors, majors, minor_ms, major_ms, swept_young,
    /// floated_swept, dirty_peak, peak_slots)`. The ms and floated-swept
    /// fields are 0 unless `ZIPP_GCSTATS=1`.
    pub fn dump_nursery() -> (u64, u64, f64, f64, u64, u64, u64, u64) {
        let g = |x: &AtomicU64| x.load(Ordering::Relaxed);
        let ms = |x: u64| x as f64 / 1.0e6;
        (
            g(&MINORS),
            g(&MAJORS),
            ms(g(&NS_MINOR)),
            ms(g(&NS_MAJOR)),
            g(&MINOR_SWEPT),
            g(&FLOATED_SWEPT),
            g(&DIRTY_PEAK),
            g(&PEAK_SLOTS),
        )
    }

    /// W10: `(last, peak)` of the survival-adaptive young budget — a
    /// separate accessor so `dump_nursery`'s 8-tuple (destructured by the
    /// CLI and the nursery tests) keeps its shape.
    pub fn dump_budget() -> (u64, u64) {
        (
            BUDGET_LAST.load(Ordering::Relaxed),
            BUDGET_PEAK.load(Ordering::Relaxed),
        )
    }

    /// B6 oracle: record one collection's generational split. Only called with
    /// the flag latched on, so no `on` gate here.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_gen(
        marked_young: u64,
        marked_old: u64,
        swept_young: u64,
        swept_old: u64,
        walk_young: u64,
        walk_total: u64,
        alloced: u64,
        trace_young: u64,
        trace_old: u64,
    ) {
        MARKED_YOUNG.fetch_add(marked_young, Ordering::Relaxed);
        MARKED_OLD.fetch_add(marked_old, Ordering::Relaxed);
        SWEPT_YOUNG.fetch_add(swept_young, Ordering::Relaxed);
        SWEPT_OLD.fetch_add(swept_old, Ordering::Relaxed);
        WALK_YOUNG.fetch_add(walk_young, Ordering::Relaxed);
        WALK_TOTAL.fetch_add(walk_total, Ordering::Relaxed);
        ALLOCED.fetch_add(alloced, Ordering::Relaxed);
        TRACE_YOUNG.fetch_add(trace_young, Ordering::Relaxed);
        TRACE_OLD.fetch_add(trace_old, Ordering::Relaxed);
    }

    /// B6 oracle totals: `(marked_young, marked_old, swept_young, swept_old,
    /// walk_young, walk_total, alloced, trace_young, trace_old)`.
    pub fn dump_gen() -> (u64, u64, u64, u64, u64, u64, u64, u64, u64) {
        let g = |x: &AtomicU64| x.load(Ordering::Relaxed);
        (
            g(&MARKED_YOUNG),
            g(&MARKED_OLD),
            g(&SWEPT_YOUNG),
            g(&SWEPT_OLD),
            g(&WALK_YOUNG),
            g(&WALK_TOTAL),
            g(&ALLOCED),
            g(&TRACE_YOUNG),
            g(&TRACE_OLD),
        )
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

/// `ZIPP_NURSERY_VERIFY=1` — run the FULL mark beside every minor and panic
/// on the first live young object the young-only trace missed (a write-
/// barrier hole). See `Vm::verify_minor_marks`.
///
/// Off, this costs one relaxed atomic load per minor.
mod nursery_verify {
    use std::sync::atomic::{AtomicU8, Ordering};

    static ON: AtomicU8 = AtomicU8::new(2);

    #[inline]
    pub(super) fn enabled() -> bool {
        match ON.load(Ordering::Relaxed) {
            0 => false,
            1 => true,
            _ => {
                let v = std::env::var_os("ZIPP_NURSERY_VERIFY").is_some() as u8;
                ON.store(v, Ordering::Relaxed);
                v == 1
            }
        }
    }
}

pub use gcstats::dump as gc_stats;
pub use gcstats::dump_budget as gc_young_budget_stats;
pub use gcstats::dump_gen as gc_gen_stats;
pub use gcstats::dump_nursery as gc_nursery_stats;


/// `ZIPP_NO_OBJ_POOL_MAJOR=1` keeps MAJOR sweeps off the recycle pool
/// (minor-only refill, the B194-landed behavior) — the single-binary A/B
/// for the B196a major-refill widening. Latched on first use.
#[cfg(not(feature = "safe-sandbox"))]
#[inline]
fn major_refill_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_OBJ_POOL_MAJOR").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}
