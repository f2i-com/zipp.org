#![allow(unused_imports)]
//! Interpreter inline caches (ICs) for the hot call / property paths.
//!
//! Every `CallMethod` / `Call` / `GetProp` / `SetProp` / `SuperMethod` site in
//! a MAIN-program function (never eval-compiled ones) owns a lazily-allocated
//! per-ip cache of up to [`IC_WAYS`] entries. A hit skips the slow machinery
//! (the `try_builtin_method` probe, the proto/class chain walk with its
//! per-level string finds, `resolve_callable` + the generator/async flag
//! loads) and goes straight to the action: a register read/write for a data
//! property, or `setup_call` for a resolved plain user function (method,
//! getter, setter) — so accessor round-trips run on the SAME dispatch-loop
//! frame machinery as ordinary calls instead of a nested `run_loop`.
//!
//! ── Correctness model (what makes a cached entry valid) ──
//!
//! The caches are *bug-compatible* with `get_member`'s fast path / `set_prop`:
//! a fill records provenance from a side-effect-free re-walk of EXACTLY the
//! conditions those paths test, and every hit re-validates the guards below.
//! Anything the fast paths would bail on (the global object, `%Array.
//! prototype%`, module namespaces, realm globals, builtin-ctor objects,
//! Proxies, non-`Object` receivers, deferred-namespace state) is excluded at
//! fill time and re-excluded cheaply at hit time.
//!
//! * `OwnData`/`OwnAcc` entries cache a SHAPE plus SLOT. A guardable first-way
//!   shape match proves the key → slot mapping without a name lookup; other
//!   ways validate `keys[slot] == key`. Every usable hit re-reads
//!   `attrs[slot]` — so in-place value writes, `freeze`, and data⇄accessor
//!   redefinition are all observed fresh. Nothing cached can go stale.
//! * `Class*` entries rely on ClassData being IMMUTABLE after class
//!   definition (methods/getters/setters/parent are only written by the
//!   MakeClass / computed-member ops; `C.prototype.m = …` does not feed back
//!   into ClassData in this engine). The guard is `m.class == class` plus
//!   `heap.version_of(class)` (a swept-and-reused class slot bumps the
//!   version), plus an own-shadow `pos(key)` miss. A live receiver whose
//!   `class` link matches keeps the class — and through `trace_edges`, the
//!   cached method/accessor Values — alive, so the cached Values can never be
//!   used after a sweep.
//! * `Proto*` entries guard the receiver by an own-`pos` miss plus a re-read
//!   of its FIRST proto link, and every chain object by its heap VERSION.
//!   Every mutation that can change chain resolution bumps a guarded
//!   version: key-add (`set_prop`/`define_property`), key-remove
//!   (`delete_prop`), and prototype replacement (`ordinary_set_prototype_of`
//!   — the bump lives there). Values are re-read from the live holder slot on
//!   every hit, and callables are re-resolved (`Func`/`Closure` → fid +
//!   flags) from the freshly-read Value, so slot reuse after a sweep can
//!   never call through a stale function id.
//! * `Callee` entries (plain `f(x)` sites) guard on the callee Value BITS plus
//!   the heap version of its slot — a swept-and-reused slot misses.
//! * `Super*` entries guard on the home-class VALUE (`class_values[id]`, so a
//!   re-evaluated class declaration misses) plus the version chain from the
//!   class's synthesized prototype object down to the holder.
//!
//! GC: the cache is intentionally NOT a GC root. Entries may hold dangling
//! Values after a sweep, but every dereference above is preceded by a guard
//! that fails for swept/reused slots (version bump on reuse) or re-reads the
//! value from a live, guard-validated holder — a stale entry can be PRESENT
//! but never USED.

use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{ClassData, Handler, Heap, HeapObj, ObjMap, PropAttr};
use crate::value::Value;

/// Ways per call-site cache. The class benches cycle 4 receiver shapes and the
/// megamorphic property benches cycle 8 layouts; 8 ways covers both.
pub(crate) const IC_WAYS: usize = 8;
/// Non-cacheable misses (or full-set evictions) before a site is disabled and
/// stops paying the fill walk. Existing entries keep validating.
const IC_MISS_LIMIT: u8 = 16;
/// Maximum proto-chain hops a `Proto*`/`Super*` entry can guard.
const IC_MAX_HOPS: usize = 6;

/// Sentinel `ret_dst` for a frame whose return value is DISCARDED (an
/// IC-pushed SETTER activation): `pop_frame_with` skips the caller-register
/// write. Real `ret_dst` values are register indices < `reg_count`, far below
/// `u16::MAX`.
pub(crate) const RET_DISCARD: u16 = u16::MAX;

/// First-link discriminant for `Proto*` entries: how the RECEIVER's own proto
/// step resolved at fill time (re-checked on every hit, because the receiver
/// itself is not version-guarded).
pub(crate) const FIRST_HEAP: u8 = 0; // explicit proto_of entry → hops[0]
pub(crate) const FIRST_DEFAULT: u8 = 1; // no proto_of entry → %Object.prototype% (hops[0])
pub(crate) const FIRST_NULL: u8 = 2; // explicit non-heap (null) proto → chain ends

/// Guarded chain: `(heap index, heap version at fill)` per chain object, in
/// walk order (`hops[0]` = the object after the receiver; the last live hop is
/// the HOLDER for data/accessor entries, or the chain END for a miss entry).
pub(crate) type IcHops = ([(u32, u32); IC_MAX_HOPS], u8);

/// One validated resolution for a site. See the module doc for the guard each
/// variant re-checks on a hit.
#[derive(Clone, Copy, Debug)]
pub(crate) enum IcEntry {
    /// `recv.key` is an own property of a plain/instance Object at `slot`
    /// (data for `OwnData`, accessor for `OwnAcc`).
    ///
    /// `shape` is the receiver's hidden class at fill time, or
    /// [`crate::shape::DICT`] if it had none. When it matches, the key -> slot
    /// mapping is proven and the key lookup is SKIPPED — see the fast path in
    /// `ic_get_prop`. When it does not, the entry still validates the old way
    /// (`own == Some(slot)`), so a dictionary-mode receiver loses no ground.
    OwnData { shape: u32, slot: u32 },
    OwnAcc { shape: u32, slot: u32 },
    /// Method (`is_getter == false`) or getter resolved on the receiver's
    /// class chain. `callee` is the materialized member function (stable for
    /// the life of the class — ClassData is immutable post-definition).
    ClassMethod { class: u32, ver: u32, callee: Value, fid: u32, closure: u32 },
    ClassGetter { class: u32, ver: u32, getter: Value },
    /// `set key(v)` resolved on the receiver's class chain (SetProp sites).
    ClassSetter { class: u32, ver: u32, setter: Value },
    /// Data property / accessor found on the proto_of chain at
    /// `hops[last].slot`, or (`slot == u32::MAX` for `ProtoData`) a full-chain
    /// MISS — the read yields `undefined`.
    ProtoData { first: u8, hops: IcHops, slot: u32 },
    ProtoAcc { first: u8, hops: IcHops, slot: u32 },
    /// Plain `Call` site: callee identity (bits) + slot version → resolved
    /// (fid, closure) for a plain (non-generator, non-async) Func/Closure.
    Callee { bits: u64, ver: u32, fid: u32, closure: u32 },
    /// `super.key(…)` / `super.key` site: `home` is the class VALUE the site's
    /// `home_class_id` resolved to at fill; `hops[0]` is the derivation anchor
    /// (the home's synthesized prototype object — or the class itself for a
    /// static member), `hops[1..]` the chain from the super base to the
    /// holder. `slot` indexes the holder's map.
    SuperData { home: Value, hops: IcHops, slot: u32 },
    SuperAcc { home: Value, hops: IcHops, slot: u32 },
}

/// Read-only resolution of a `super.m()` op for Stage 3 method inlining
/// (`ic_super_method_baked`): the resolved super-method `fid`, the hop
/// `(heap_idx, version)` guards to re-check, and the holder slot + baked fn bits
/// (a same-slot reassignment value guard, in case an overwrite doesn't bump the
/// hop version).
pub(crate) struct MiSuperResolved {
    pub(crate) fid: u32,
    pub(crate) hops: Vec<(u32, u32)>,
    pub(crate) holder_vals_ptr: u64,
    pub(crate) holder_slot: u32,
    pub(crate) fn_bits: u64,
}

/// Per-site cache: up to [`IC_WAYS`] entries, a fill-failure counter, and a
/// round-robin eviction cursor.
pub(crate) struct SiteIc {
    pub(crate) misses: u8,
    pub(crate) n: u8,
    rot: u8,
    pub(crate) entries: [IcEntry; IC_WAYS],
}

/// Action for a `GetProp`-shaped site (also `SuperGet`).
pub(crate) enum GetAct {
    /// Data hit: write the value to `dst` and continue.
    Value(Value),
    /// Accessor hit resolved to a plain user function: push its frame with
    /// `this` = receiver and `ret_dst` = the GetProp `dst`.
    Accessor { fid: u32, closure: u32, getter: Value },
    /// No usable entry — take the slow path.
    None,
}

/// Action for a `SetProp`-shaped site (also `SuperSet`).
pub(crate) enum SetAct {
    /// Own data slot written — the store is complete.
    Done,
    /// Setter hit resolved to a plain user function: push its frame with
    /// `this` = receiver, the value as the single argument, and
    /// `ret_dst` = [`RET_DISCARD`].
    Setter { fid: u32, closure: u32, setter: Value },
    None,
}

/// A validated `SetProp` action, produced borrow-free by the shape probe or
/// [`Vm::ic_validate_set`].
enum SetPlan {
    WriteOwn { idx: u32, slot: u32 },
    Setter { fid: u32, closure: u32, setter: Value },
}

/// Side-effect-free provenance of a property resolution (the fill walk).
pub(crate) enum Walked {
    OwnData { slot: usize },
    OwnAcc { slot: usize },
    /// Hit on the class chain: methods-before-getters per level, like
    /// `get_member`'s inline walk.
    ClassMethod { class: u32, callee: Value },
    ClassGetter { class: u32, getter: Value },
    ChainData { first: u8, hops: IcHops, slot: u32 },
    ChainAcc { first: u8, hops: IcHops, slot: u32 },
    /// The whole (guardable) chain misses → `undefined`.
    ChainMiss { first: u8, hops: IcHops },
    /// Not cacheable (exotic receiver/chain, too deep, non-Object hop, …).
    No,
}

/// Method names `dispatch_builtin_method` claims for ANY object receiver —
/// a `CallMethod` IC entry would bypass that claim, changing behavior, so
/// these names are never cached for method-call sites.
#[inline]
fn builtin_object_method(key: &str) -> bool {
    matches!(key, "hasOwnProperty" | "propertyIsEnumerable" | "isPrototypeOf")
}

impl<'p> Vm<'p> {
    /// Exclusions shared with `get_member`'s fast path: objects with live /
    /// exotic slot semantics layered over their ObjMap never take IC paths.
    /// (Also consulted by the JIT region prop-miss helpers in helpers_misc.rs.)
    #[inline]
    pub(crate) fn ic_obj_ok(&self, idx: u32) -> bool {
        !(idx == self.global_this && self.global_this != 0)
            && !(idx == self.arr_proto && self.arr_proto != 0)
            && (self.module_namespaces.is_empty()
                || !self.module_namespaces.contains_key(&idx))
            && (self.realm_global_objs.is_empty()
                || !self.realm_global_objs.contains_key(&idx))
    }

    /// The site cache for `(func_id, ip)`, if this function is IC-eligible
    /// (a MAIN-program function — eval functions never cache).
    #[inline]
    fn ic_site(&self, func_id: u32, ip: usize) -> Option<&SiteIc> {
        let f = func_id as usize;
        if f >= self.main_func_count {
            return None;
        }
        self.site_ics.get(f)?.as_ref()?.get(ip)?.as_deref()
    }

    /// Count a fill failure (or a full-set eviction) at a site, allocating the
    /// site cache if needed so the disable counter persists.
    fn ic_note_miss(&mut self, func_id: u32, ip: usize) {
        if let Some(site) = self.ic_site_mut(func_id, ip) {
            site.misses = site.misses.saturating_add(1);
        }
    }

    /// Mutable access to (and lazy allocation of) the site cache.
    fn ic_site_mut(&mut self, func_id: u32, ip: usize) -> Option<&mut SiteIc> {
        let f = func_id as usize;
        if f >= self.main_func_count {
            return None;
        }
        if self.site_ics.len() < self.main_func_count {
            self.site_ics.resize_with(self.main_func_count, || None);
        }
        let code_len = self.func(f).code.len();
        let slots = self.site_ics[f].get_or_insert_with(|| {
            let mut v = Vec::new();
            v.resize_with(code_len, || None);
            v.into_boxed_slice()
        });
        let slot = slots.get_mut(ip)?;
        Some(slot.get_or_insert_with(|| {
            Box::new(SiteIc {
                misses: 0,
                n: 0,
                rot: 0,
                entries: [IcEntry::OwnData { shape: crate::shape::DICT, slot: 0 }; IC_WAYS],
            })
        }))
    }

    /// Install `e` at the site (round-robin eviction when full; a full-set
    /// eviction also counts toward the disable limit so a megamorphic site
    /// eventually stops churning).
    fn ic_install(&mut self, func_id: u32, ip: usize, e: IcEntry) {
        if let Some(site) = self.ic_site_mut(func_id, ip) {
            if (site.n as usize) < IC_WAYS {
                site.entries[site.n as usize] = e;
                site.n += 1;
            } else {
                let r = site.rot as usize % IC_WAYS;
                site.entries[r] = e;
                site.rot = site.rot.wrapping_add(1);
                site.misses = site.misses.saturating_add(1);
            }
        }
    }

    /// Q7: record a receiver INSTANCE seen at a Class method/getter/setter site
    /// (called at IC-fill time — rare, so ~free). The JIT planner reads the ≤8
    /// recorded receiver-bits to bake per-instance inline arms (the class-keyed
    /// IC records no instances). Deduped + capped; stale entries are harmless
    /// (rebuilt-from-live + runtime-guarded). See `mi_recv`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    fn mi_record_recv(&mut self, func_id: u32, ip: usize, recv: Value) {
        if !recv.is_heap() {
            return;
        }
        let bits = recv.bits();
        let set = self.mi_recv.entry(((func_id as u64) << 32) | ip as u64).or_default();
        if set.len() < IC_WAYS && !set.contains(&bits) {
            set.push(bits);
        }
    }
    #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
    fn mi_record_recv(&mut self, _func_id: u32, _ip: usize, _recv: Value) {}

    // ── the fill walk ──

    /// Resolve `recv.key` with provenance, mirroring `get_member`'s fast path
    /// EXACTLY and performing no side effects. Anything the fast path would
    /// delegate to the slow path reports `Walked::No`.
    pub(crate) fn ic_walk(&self, recv: Value, key: &str) -> Walked {
        if !self.deferred_ns_state.is_empty() || !recv.is_heap() {
            return Walked::No;
        }
        // Private members live in side tables with brand semantics — never IC.
        if key.as_bytes().first() == Some(&b'#') {
            return Walked::No;
        }
        let idx = recv.heap_index();
        if !self.ic_obj_ok(idx) {
            return Walked::No;
        }
        let m = match self.heap.get(idx) {
            HeapObj::Object(m) if !m.is_ctor => m,
            _ => return Walked::No,
        };
        if let Some(i) = m.pos(key) {
            return if m.attrs[i].accessor {
                Walked::OwnAcc { slot: i }
            } else {
                Walked::OwnData { slot: i }
            };
        }
        if let Some(class) = m.class {
            // Class-instance own miss: the inline class-chain walk
            // (methods before getters per level; a non-Class link breaks to
            // the slow path → not cacheable).
            let mut c2 = Some(class);
            while let Some(cidx) = c2 {
                match self.heap.get(cidx) {
                    HeapObj::Class(c) => {
                        if let Some((_, v)) = c.methods.iter().find(|(k, _)| k == key) {
                            return Walked::ClassMethod { class, callee: *v };
                        }
                        if let Some((_, v)) = c.getters.iter().find(|(k, _)| k == key) {
                            return Walked::ClassGetter { class, getter: *v };
                        }
                        c2 = c.parent;
                    }
                    _ => break,
                }
            }
            return Walked::No; // chain miss → slow path
        }
        // Plain-object proto_of chain, recording (idx, version) per hop.
        self.ic_walk_chain(idx, key)
    }

    /// The proto-chain leg of [`Vm::ic_walk`], starting from the link OUT of
    /// `idx` (whose own map already missed). Also used for `super` fills with
    /// the synthesized prototype object as the anchor.
    fn ic_walk_chain(&self, idx: u32, key: &str) -> Walked {
        let mut hops: IcHops = ([(0, 0); IC_MAX_HOPS], 0);
        let mut cur = idx;
        let mut first = FIRST_HEAP;
        loop {
            let next = match self.proto_of.get(&cur) {
                Some(&p) => {
                    if !p.is_heap() {
                        // Null-prototype chain end → undefined.
                        if cur == idx {
                            first = FIRST_NULL;
                        }
                        return Walked::ChainMiss { first, hops };
                    }
                    p.heap_index()
                }
                None => {
                    if self.obj_proto == 0 || cur == self.obj_proto {
                        // End at %Object.prototype% (guarded as the last hop;
                        // `cur == idx` — reading off obj_proto itself with an
                        // own miss — is excluded below to keep `first` simple).
                        if cur == idx {
                            return Walked::No;
                        }
                        return Walked::ChainMiss { first, hops };
                    }
                    if cur == idx {
                        first = FIRST_DEFAULT;
                    }
                    self.obj_proto
                }
            };
            if hops.1 as usize >= IC_MAX_HOPS {
                return Walked::No; // too deep to guard
            }
            if !self.ic_obj_ok(next) {
                return Walked::No;
            }
            let m2 = match self.heap.get(next) {
                HeapObj::Object(m2) if !m2.is_ctor && m2.class.is_none() => m2,
                _ => return Walked::No,
            };
            hops.0[hops.1 as usize] = (next, self.heap.version_of(next));
            hops.1 += 1;
            if let Some(i) = m2.pos(key) {
                return if m2.attrs[i].accessor {
                    Walked::ChainAcc { first, hops, slot: i as u32 }
                } else {
                    Walked::ChainData { first, hops, slot: i as u32 }
                };
            }
            cur = next;
        }
    }

    // ── hit validation ──

    /// Validate the receiver-side guards shared by every `Own*`/`Class*`/
    /// `Proto*` hit: a plain Object receiver outside the exotic set. Returns
    /// its map.
    #[inline]
    fn ic_recv_map(&self, recv: Value) -> Option<(u32, &ObjMap)> {
        if !recv.is_heap() || !self.deferred_ns_state.is_empty() {
            return None;
        }
        let idx = recv.heap_index();
        if !self.ic_obj_ok(idx) {
            return None;
        }
        match self.heap.get(idx) {
            HeapObj::Object(m) if !m.is_ctor => Some((idx, m)),
            _ => None,
        }
    }

    /// The receiver's hidden class, or [`crate::shape::DICT`] when it has none
    /// (or is not the kind of object the caches guard). `DICT` never matches a
    /// live receiver's shape, so an entry filled with it simply falls through to
    /// the old validation.
    #[inline]
    fn ic_recv_shape(&self, recv: Value) -> u32 {
        match self.ic_recv_map(recv) {
            Some((_, m)) => m.shape(),
            None => crate::shape::DICT,
        }
    }

    /// Validate a `Proto*` entry's first link + hop versions against `recv`
    /// (whose own map must already have missed `key`). Returns the holder's
    /// map when the chain still stands.
    #[inline]
    fn ic_chain_ok(&self, recv_idx: u32, first: u8, hops: &IcHops) -> Option<&ObjMap> {
        // First link re-read (the receiver itself is not version-guarded).
        match first {
            FIRST_HEAP => {
                let p = self.proto_of.get(&recv_idx)?;
                if !p.is_heap() || p.heap_index() != hops.0[0].0 {
                    return None;
                }
            }
            FIRST_DEFAULT => {
                if self.proto_of.get(&recv_idx).is_some()
                    || hops.1 == 0
                    || hops.0[0].0 != self.obj_proto
                {
                    return None;
                }
            }
            _ => {
                // FIRST_NULL: the receiver's explicit proto must still be
                // non-heap (a miss entry with no hops).
                match self.proto_of.get(&recv_idx) {
                    Some(p) if !p.is_heap() => return Some(self.ic_empty_map()),
                    _ => return None,
                }
            }
        }
        // Hop versions: any key add/remove/defineProperty/setPrototypeOf on a
        // chain object bumps its version (see the module doc).
        let n = hops.1 as usize;
        for &(h, v) in &hops.0[..n] {
            if self.heap.version_of(h) != v {
                return None;
            }
        }
        match self.heap.get(hops.0[n - 1].0) {
            HeapObj::Object(hm) => Some(hm),
            _ => None,
        }
    }

    /// A shared empty map for `FIRST_NULL` chain-miss validation (no holder).
    #[inline]
    fn ic_empty_map(&self) -> &'static ObjMap {
        static EMPTY: std::sync::OnceLock<ObjMap> = std::sync::OnceLock::new();
        EMPTY.get_or_init(ObjMap::new)
    }

    /// Re-resolve a freshly-read callable Value to a PLAIN (non-generator,
    /// non-async) user function. `None` for natives/bound/generators/… —
    /// the caller falls back to the slow path. (Also used by the JIT prop-miss
    /// helpers to pre-screen a bakeable accessor fn for a B114 ACCESSOR way.)
    #[inline]
    pub(crate) fn ic_plain_fn(&self, v: Value) -> Option<(u32, u32)> {
        if !v.is_heap() {
            return None;
        }
        let i = v.heap_index();
        let (fid, closure) = match self.heap.get(i) {
            HeapObj::Func(id) => (*id, NO_CLOSURE),
            HeapObj::Closure { func, .. } => (*func, i),
            _ => return None,
        };
        let p = self.func(fid as usize);
        if p.is_generator || p.is_async {
            return None;
        }
        Some((fid, closure))
    }

    // ── GetProp sites ──

    /// IC lookup + fill for a `GetProp` site. `GetAct::None` ⇒ slow path.
    pub(crate) fn ic_get_prop(
        &mut self,
        func_id: u32,
        ip: usize,
        recv: Value,
        key: &str,
    ) -> GetAct {
        // Receiver guards + own-`pos` are entry-independent: derive ONCE per
        // probe (a 4-way site otherwise pays the heap fetch + key scan per
        // entry). The site is fetched once for both probe and disable check.
        if let Some(site) = self.ic_site(func_id, ip) {
            if site.n > 0 {
                if let Some((idx, m)) = self.ic_recv_map(recv) {
                    // ── shape-keyed fast path ──────────────────────────────
                    // A matching hidden class proves the receiver's key -> slot
                    // mapping is the one this entry was filled against, so the
                    // slot is usable WITHOUT looking the key up. That matters
                    // twice over:
                    //
                    //   * `m.pos(key)` below is unconditional — the cache never
                    //     avoided the key scan, only the proto/class walk, which
                    //     is why the interpreter measured flat at ~34ns per read
                    //     no matter how well the site was cached; and
                    //   * shapes are shared, so a site reading a property from
                    //     ten thousand distinct objects built the same way needs
                    //     ONE way instead of overflowing all eight. Measured on
                    //     identically-shaped receivers, the old identity guard
                    //     went 4.25ns at 12 receivers to 14.75ns at 16.
                    //
                    // Sound because a site's key is a compile-time constant
                    // (`Instr::GetProp` reads `string_constants[name]`), so the
                    // key that filled the entry is the key being asked for now.
                    // Only the FIRST way is probed by shape. Scanning all eight
                    // was measurably worse: a site whose receivers genuinely do
                    // not share shapes — `json-large` builds 54,390 of them, so
                    // the guard never hits — paid eight failed compares before
                    // every access, for +9% on that bench. Property sites are
                    // overwhelmingly monomorphic, so way 0 is where the shape
                    // will be if it is anywhere, and a single failed compare is
                    // affordable when it is not.
                    let sh = m.shape();
                    if sh != crate::shape::DICT && site.n > 0 {
                        match site.entries[0] {
                            IcEntry::OwnData { shape, slot } if shape == sh => {
                                return GetAct::Value(m.val_at(slot as usize));
                            }
                            IcEntry::OwnAcc { shape, slot } if shape == sh => {
                                return self.ic_acc_get_from(m, slot as usize);
                            }
                            _ => {}
                        }
                    }
                    let own = m.pos(key);
                    for e in &site.entries[..site.n as usize] {
                        match self.ic_validate_get(e, idx, m, own, key) {
                            GetAct::None => {}
                            act => return act,
                        }
                    }
                }
            }
            if site.misses >= IC_MISS_LIMIT {
                return GetAct::None;
            }
        }
        match self.ic_walk(recv, key) {
            Walked::OwnData { slot } => {
                // Deliver from the freshly-validated walk (same read the
                // entry would perform).
                let (v, shape) = match self.ic_recv_map(recv) {
                    Some((_, m)) => (m.vals[slot], m.shape()),
                    None => return GetAct::None,
                };
                self.ic_install(func_id, ip, IcEntry::OwnData { shape, slot: slot as u32 });
                GetAct::Value(v)
            }
            Walked::OwnAcc { slot } => {
                let shape = self.ic_recv_shape(recv);
                self.ic_install(func_id, ip, IcEntry::OwnAcc { shape, slot: slot as u32 });
                self.ic_own_acc_get(recv, key, slot as u32)
            }
            Walked::ClassMethod { class, callee } => {
                if let Some((fid, closure)) = self.ic_plain_fn(callee) {
                    self.ic_install(
                        func_id,
                        ip,
                        IcEntry::ClassMethod {
                            class,
                            ver: self.heap.version_of(class),
                            callee,
                            fid,
                            closure,
                        },
                    );
                } else {
                    self.ic_note_miss(func_id, ip);
                }
                // A method READ yields the member value either way.
                GetAct::Value(callee)
            }
            Walked::ClassGetter { class, getter } => {
                let ver = self.heap.version_of(class);
                self.mi_record_recv(func_id, ip, recv);
                self.ic_install(func_id, ip, IcEntry::ClassGetter { class, ver, getter });
                match self.ic_plain_fn(getter) {
                    Some((fid, closure)) => GetAct::Accessor { fid, closure, getter },
                    None => GetAct::None, // native/generator getter → slow path
                }
            }
            Walked::ChainData { first, hops, slot } => {
                self.ic_install(func_id, ip, IcEntry::ProtoData { first, hops, slot });
                let hm = match self.heap.get(hops.0[hops.1 as usize - 1].0) {
                    HeapObj::Object(hm) => hm,
                    _ => return GetAct::None,
                };
                GetAct::Value(hm.vals[slot as usize])
            }
            Walked::ChainAcc { first, hops, slot } => {
                self.ic_install(func_id, ip, IcEntry::ProtoAcc { first, hops, slot });
                self.ic_chain_acc_get(hops, slot)
            }
            Walked::ChainMiss { first, hops } => {
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::ProtoData { first, hops, slot: u32::MAX },
                );
                GetAct::Value(Value::UNDEFINED)
            }
            Walked::No => {
                self.ic_note_miss(func_id, ip);
                GetAct::None
            }
        }
    }

    /// Validate one entry against a `GetProp` access. `idx`/`m` are the
    /// already-guarded receiver (see `ic_recv_map`); `own` its `pos(key)`.
    fn ic_validate_get(
        &self,
        e: &IcEntry,
        idx: u32,
        m: &ObjMap,
        own: Option<usize>,
        key: &str,
    ) -> GetAct {
        match *e {
            IcEntry::OwnData { slot, .. } => {
                let s = slot as usize;
                if own == Some(s) && !m.attrs[s].accessor {
                    GetAct::Value(m.vals[s])
                } else {
                    GetAct::None
                }
            }
            IcEntry::OwnAcc { slot, .. } => {
                let s = slot as usize;
                if own == Some(s) && m.attrs[s].accessor {
                    self.ic_acc_get_from(m, s)
                } else {
                    GetAct::None
                }
            }
            IcEntry::ClassMethod { class, ver, callee, .. } => {
                if own.is_none()
                    && m.class == Some(class)
                    && self.heap.version_of(class) == ver
                {
                    GetAct::Value(callee)
                } else {
                    GetAct::None
                }
            }
            IcEntry::ClassGetter { class, ver, getter } => {
                if own.is_none()
                    && m.class == Some(class)
                    && self.heap.version_of(class) == ver
                {
                    match self.ic_plain_fn(getter) {
                        Some((fid, closure)) => GetAct::Accessor { fid, closure, getter },
                        None => GetAct::None,
                    }
                } else {
                    GetAct::None
                }
            }
            IcEntry::ProtoData { first, hops, slot } => {
                if own.is_some() || m.class.is_some() {
                    return GetAct::None;
                }
                let Some(hm) = self.ic_chain_ok(idx, first, &hops) else {
                    return GetAct::None;
                };
                if slot == u32::MAX {
                    return GetAct::Value(Value::UNDEFINED); // guarded chain miss
                }
                let s = slot as usize;
                if s < hm.keys.len() && hm.keys[s] == key && !hm.attrs[s].accessor {
                    GetAct::Value(hm.vals[s])
                } else {
                    GetAct::None
                }
            }
            IcEntry::ProtoAcc { first, hops, slot } => {
                if own.is_some() || m.class.is_some() {
                    return GetAct::None;
                }
                if self.ic_chain_ok(idx, first, &hops).is_none() {
                    return GetAct::None;
                }
                self.ic_chain_acc_get(hops, slot)
            }
            _ => GetAct::None,
        }
    }

    /// Accessor GET from an already-validated map slot: re-read the getter and
    /// re-resolve it (nothing cached survives a slot redefinition).
    #[inline]
    fn ic_acc_get_from(&self, m: &ObjMap, s: usize) -> GetAct {
        let g = m.vals[s];
        if g == Value::UNDEFINED {
            // No getter ⇒ undefined (matches the fast path).
            return GetAct::Value(Value::UNDEFINED);
        }
        match self.ic_plain_fn(g) {
            Some((fid, closure)) => GetAct::Accessor { fid, closure, getter: g },
            None => GetAct::None,
        }
    }

    /// Resolve an OWN accessor slot for a GET: re-read the getter and
    /// re-resolve it (nothing cached survives a slot redefinition).
    #[inline]
    fn ic_own_acc_get(&self, recv: Value, key: &str, slot: u32) -> GetAct {
        let Some((_, m)) = self.ic_recv_map(recv) else { return GetAct::None };
        let s = slot as usize;
        if s >= m.keys.len() || m.keys[s] != key || !m.attrs[s].accessor {
            return GetAct::None;
        }
        self.ic_acc_get_from(m, s)
    }

    /// Resolve a CHAIN accessor slot for a GET (chain already validated).
    #[inline]
    fn ic_chain_acc_get(&self, hops: IcHops, slot: u32) -> GetAct {
        let holder = hops.0[hops.1 as usize - 1].0;
        let hm = match self.heap.get(holder) {
            HeapObj::Object(hm) => hm,
            _ => return GetAct::None,
        };
        let s = slot as usize;
        if s >= hm.vals.len() || !hm.attrs[s].accessor {
            return GetAct::None;
        }
        let g = hm.vals[s];
        if g == Value::UNDEFINED {
            return GetAct::Value(Value::UNDEFINED);
        }
        match self.ic_plain_fn(g) {
            Some((fid, closure)) => GetAct::Accessor { fid, closure, getter: g },
            None => GetAct::None,
        }
    }

    // ── SetProp sites ──

    /// IC lookup + fill for a `SetProp` site. `SetAct::Done` means the own
    /// data slot was written; `Setter` hands a plain setter to frame-call.
    pub(crate) fn ic_set_prop(
        &mut self,
        func_id: u32,
        ip: usize,
        recv: Value,
        key: &str,
        val: Value,
    ) -> SetAct {
        let mut plan: Option<SetPlan> = None;
        let mut disabled = false;
        if let Some(site) = self.ic_site(func_id, ip) {
            disabled = site.misses >= IC_MISS_LIMIT;
            if site.n > 0 {
                if let Some((idx, m)) = self.ic_recv_map(recv) {
                    // Probe ONLY the first way by hidden class before paying for
                    // `pos(key)`. A shape match proves this site's constant key
                    // still occupies the recorded slot, but not that the live
                    // descriptor is safe to write: re-read it so freeze and
                    // data<->accessor redefinitions cannot bypass ordinary Set
                    // semantics. Keep only indices in the plan so the mutable
                    // store happens after the map/site borrows end.
                    //
                    // Do not scan every way by shape here. GetProp tried that
                    // policy already; failed compares on genuinely polymorphic
                    // JSON sites made it materially slower.
                    let shape = m.shape();
                    if m.shape_guardable() {
                        if let IcEntry::OwnData { shape: cached_shape, slot } = site.entries[0] {
                            if cached_shape == shape {
                                if let Some(attr) = m.attr_get(slot as usize) {
                                    if !attr.accessor && attr.writable {
                                        plan = Some(SetPlan::WriteOwn { idx, slot });
                                    }
                                }
                            }
                        }
                    }

                    // Dictionary receivers, unsafe/invalidated first ways, and
                    // shape misses retain the complete old lookup and way scan.
                    if plan.is_none() {
                        let own = m.pos(key);
                        for e in &site.entries[..site.n as usize] {
                            if let Some(p) = self.ic_validate_set(e, idx, m, own) {
                                plan = Some(p);
                                break;
                            }
                        }
                    }
                }
            }
        }
        if let Some(p) = plan {
            return self.ic_apply_set(p, val);
        }
        if disabled {
            return SetAct::None;
        }
        // Keys with exotic write interception never cache: `__proto__`
        // (the inherited setter), restricted names, and canonical-index-ish
        // keys (`note_array_proto_index`).
        if key == "__proto__"
            || key == "caller"
            || key == "arguments"
            || key.as_bytes().first().is_some_and(|b| b.is_ascii_digit() || *b == b'-')
        {
            self.ic_note_miss(func_id, ip);
            return SetAct::None;
        }
        match self.ic_walk(recv, key) {
            Walked::OwnData { slot } => {
                let shape = self.ic_recv_shape(recv);
                self.ic_install(func_id, ip, IcEntry::OwnData { shape, slot: slot as u32 });
                match self.ic_own_set_plan(recv, key, slot as u32) {
                    Some(p) => self.ic_apply_set(p, val),
                    None => SetAct::None,
                }
            }
            Walked::OwnAcc { slot } => {
                let shape = self.ic_recv_shape(recv);
                self.ic_install(func_id, ip, IcEntry::OwnAcc { shape, slot: slot as u32 });
                self.ic_own_acc_set(recv, key, slot as u32)
            }
            Walked::ClassMethod { .. } | Walked::ClassGetter { .. } => {
                // A write to a class-resolved member: only a SETTER on the
                // chain is cacheable (set_prop's class arm).
                let (_, m) = match self.ic_recv_map(recv) {
                    Some(x) => x,
                    None => return SetAct::None,
                };
                let class = m.class.expect("class walk provenance");
                match self.lookup_setter(Some(class), key) {
                    Some(setter) => {
                        let ver = self.heap.version_of(class);
                        self.mi_record_recv(func_id, ip, recv);
                        self.ic_install(
                            func_id,
                            ip,
                            IcEntry::ClassSetter { class, ver, setter },
                        );
                        match self.ic_plain_fn(setter) {
                            Some((fid, closure)) => SetAct::Setter { fid, closure, setter },
                            None => SetAct::None,
                        }
                    }
                    None => {
                        self.ic_note_miss(func_id, ip);
                        SetAct::None
                    }
                }
            }
            // Chain data / chain miss writes create own properties (adds) or
            // route through proto_chain_set — both stay on the slow path.
            // But a class instance with NO method/getter hit may still have a
            // chain SETTER; resolve it directly.
            Walked::No | Walked::ChainData { .. } | Walked::ChainMiss { .. } => {
                if let Some((_, m)) = self.ic_recv_map(recv) {
                    if let Some(class) = m.class {
                        if m.pos(key).is_none() {
                            if let Some(setter) = self.lookup_setter(Some(class), key) {
                                let ver = self.heap.version_of(class);
                                self.mi_record_recv(func_id, ip, recv);
                                self.ic_install(
                                    func_id,
                                    ip,
                                    IcEntry::ClassSetter { class, ver, setter },
                                );
                                return match self.ic_plain_fn(setter) {
                                    Some((fid, closure)) => {
                                        SetAct::Setter { fid, closure, setter }
                                    }
                                    None => SetAct::None,
                                };
                            }
                        }
                    }
                }
                self.ic_note_miss(func_id, ip);
                SetAct::None
            }
            Walked::ChainAcc { first, hops, slot } => {
                // Inherited accessor governs the write (proto_chain_set):
                // cache it; a getter-only accessor stays slow (reject path).
                self.ic_install(func_id, ip, IcEntry::ProtoAcc { first, hops, slot });
                self.ic_chain_acc_set(hops, slot)
            }
        }
    }

    /// Pure validation of one entry against a `SetProp` access; the returned
    /// plan is applied by [`Vm::ic_apply_set`] once all borrows end. `idx`/`m`
    /// are the already-guarded receiver; `own` its `pos(key)`.
    fn ic_validate_set(
        &self,
        e: &IcEntry,
        idx: u32,
        m: &ObjMap,
        own: Option<usize>,
    ) -> Option<SetPlan> {
        match *e {
            IcEntry::OwnData { slot, .. } => {
                let s = slot as usize;
                if own == Some(s) && !m.attrs[s].accessor && m.attrs[s].writable {
                    Some(SetPlan::WriteOwn { idx, slot })
                } else {
                    None
                }
            }
            IcEntry::OwnAcc { slot, .. } => {
                let s = slot as usize;
                if own == Some(s) && m.attrs[s].accessor {
                    let setter = m.attrs[s].setter;
                    let (fid, closure) = self.ic_plain_fn(setter)?;
                    Some(SetPlan::Setter { fid, closure, setter })
                } else {
                    None
                }
            }
            IcEntry::ClassSetter { class, ver, setter } => {
                if own.is_none()
                    && m.class == Some(class)
                    && self.heap.version_of(class) == ver
                {
                    let (fid, closure) = self.ic_plain_fn(setter)?;
                    Some(SetPlan::Setter { fid, closure, setter })
                } else {
                    None
                }
            }
            IcEntry::ProtoAcc { first, hops, slot } => {
                if own.is_some() || m.class.is_some() {
                    return None;
                }
                self.ic_chain_ok(idx, first, &hops)?;
                match self.ic_chain_acc_set(hops, slot) {
                    SetAct::Setter { fid, closure, setter } => {
                        Some(SetPlan::Setter { fid, closure, setter })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Apply a validated set plan (the write, or hand back the setter).
    #[inline]
    fn ic_apply_set(&mut self, p: SetPlan, val: Value) -> SetAct {
        match p {
            SetPlan::WriteOwn { idx, slot } => {
                if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                    m.vals[slot as usize] = val;
                }
                SetAct::Done
            }
            SetPlan::Setter { fid, closure, setter } => SetAct::Setter { fid, closure, setter },
        }
    }

    /// Validate an OWN data slot for a write (still a writable data property
    /// under the same key — redefined/frozen slots fall to the slow path).
    #[inline]
    fn ic_own_set_plan(&self, recv: Value, key: &str, slot: u32) -> Option<SetPlan> {
        let (idx, m) = self.ic_recv_map(recv)?;
        let s = slot as usize;
        if s < m.keys.len() && m.keys[s] == key && !m.attrs[s].accessor && m.attrs[s].writable
        {
            Some(SetPlan::WriteOwn { idx, slot })
        } else {
            None
        }
    }

    /// Resolve an OWN accessor slot for a SET: the setter must exist and be a
    /// plain user function (getter-only / native → slow path).
    #[inline]
    fn ic_own_acc_set(&self, recv: Value, key: &str, slot: u32) -> SetAct {
        let Some((_, m)) = self.ic_recv_map(recv) else { return SetAct::None };
        let s = slot as usize;
        if s >= m.keys.len() || m.keys[s] != key || !m.attrs[s].accessor {
            return SetAct::None;
        }
        let setter = m.attrs[s].setter;
        match self.ic_plain_fn(setter) {
            Some((fid, closure)) => SetAct::Setter { fid, closure, setter },
            None => SetAct::None,
        }
    }

    /// Resolve a CHAIN accessor slot for a SET (chain already validated).
    #[inline]
    fn ic_chain_acc_set(&self, hops: IcHops, slot: u32) -> SetAct {
        let holder = hops.0[hops.1 as usize - 1].0;
        let hm = match self.heap.get(holder) {
            HeapObj::Object(hm) => hm,
            _ => return SetAct::None,
        };
        let s = slot as usize;
        if s >= hm.vals.len() || !hm.attrs[s].accessor {
            return SetAct::None;
        }
        let setter = hm.attrs[s].setter;
        match self.ic_plain_fn(setter) {
            Some((fid, closure)) => SetAct::Setter { fid, closure, setter },
            None => SetAct::None,
        }
    }

    // ── CallMethod sites ──

    /// IC lookup + fill for a `CallMethod` site: resolves `recv.key` to a
    /// PLAIN user function to frame-call with `this = recv`. `None` ⇒ slow
    /// path (builtins, natives, accessors, generators, exotic receivers).
    pub(crate) fn ic_call_method(
        &mut self,
        func_id: u32,
        ip: usize,
        recv: Value,
        key: &str,
    ) -> Option<(u32, u32, Value)> {
        if let Some(site) = self.ic_site(func_id, ip) {
            if site.n > 0 {
                if let Some((idx, m)) = self.ic_recv_map(recv) {
                    let own = m.pos(key);
                    for e in &site.entries[..site.n as usize] {
                        if let Some(hit) = self.ic_validate_method(e, idx, m, own, key) {
                            return Some(hit);
                        }
                    }
                }
            }
            if site.misses >= IC_MISS_LIMIT {
                return None;
            }
        }
        // Never claim names the builtin-method probe would handle for an
        // object receiver (bug-compatibility with try_builtin_method).
        if builtin_object_method(key) {
            self.ic_note_miss(func_id, ip);
            return None;
        }
        match self.ic_walk(recv, key) {
            Walked::OwnData { slot } => {
                let (_, m) = self.ic_recv_map(recv)?;
                let v = m.vals[slot];
                let shape = m.shape();
                match self.ic_plain_fn(v) {
                    Some((fid, closure)) => {
                        self.ic_install(func_id, ip, IcEntry::OwnData { shape, slot: slot as u32 });
                        Some((fid, closure, v))
                    }
                    None => {
                        self.ic_note_miss(func_id, ip);
                        None
                    }
                }
            }
            Walked::ClassMethod { class, callee } => match self.ic_plain_fn(callee) {
                Some((fid, closure)) => {
                    let ver = self.heap.version_of(class);
                    self.mi_record_recv(func_id, ip, recv);
                    self.ic_install(
                        func_id,
                        ip,
                        IcEntry::ClassMethod { class, ver, callee, fid, closure },
                    );
                    Some((fid, closure, callee))
                }
                None => {
                    self.ic_note_miss(func_id, ip);
                    None
                }
            },
            Walked::ChainData { first, hops, slot } => {
                let hm = match self.heap.get(hops.0[hops.1 as usize - 1].0) {
                    HeapObj::Object(hm) => hm,
                    _ => return None,
                };
                let v = hm.vals[slot as usize];
                match self.ic_plain_fn(v) {
                    Some((fid, closure)) => {
                        // B78: the method inliner now bakes arms for INHERITED
                        // methods too, so this fill is worth recording for the
                        // same reason the class one is. Without it the planner
                        // sees only the live exemplar at the receiver register
                        // plus the `arr[idx]` dense scan, so `var o = list[i];
                        // o.m()` — the receiver loaded indirectly — would get
                        // exactly ONE arm.
                        self.mi_record_recv(func_id, ip, recv);
                        self.ic_install(func_id, ip, IcEntry::ProtoData { first, hops, slot });
                        Some((fid, closure, v))
                    }
                    None => {
                        self.ic_note_miss(func_id, ip);
                        None
                    }
                }
            }
            // Accessors / class getters / chain misses: slow path resolves
            // (and may call a getter, which the IC must never do).
            _ => {
                self.ic_note_miss(func_id, ip);
                None
            }
        }
    }

    fn ic_validate_method(
        &self,
        e: &IcEntry,
        idx: u32,
        m: &ObjMap,
        own: Option<usize>,
        key: &str,
    ) -> Option<(u32, u32, Value)> {
        match *e {
            IcEntry::OwnData { slot, .. } => {
                let s = slot as usize;
                if own == Some(s) && !m.attrs[s].accessor {
                    let v = m.vals[s];
                    let (fid, closure) = self.ic_plain_fn(v)?;
                    Some((fid, closure, v))
                } else {
                    None
                }
            }
            IcEntry::ClassMethod { class, ver, callee, fid, closure } => {
                if own.is_none()
                    && m.class == Some(class)
                    && self.heap.version_of(class) == ver
                {
                    Some((fid, closure, callee))
                } else {
                    None
                }
            }
            IcEntry::ProtoData { first, hops, slot } => {
                if slot == u32::MAX || own.is_some() || m.class.is_some() {
                    return None;
                }
                let hm = self.ic_chain_ok(idx, first, &hops)?;
                let s = slot as usize;
                if s < hm.keys.len() && hm.keys[s] == key && !hm.attrs[s].accessor {
                    let v = hm.vals[s];
                    let (fid, closure) = self.ic_plain_fn(v)?;
                    Some((fid, closure, v))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    // ── plain Call sites ──

    /// Read-only probe of a `Call` site's inline cache for the Q4 leaf-inline
    /// planner: if the site is MONOMORPHIC (exactly one filled way) and that way
    /// is a `Callee` entry, return `(callee_bits, ver, fid, closure)` (`ver` is
    /// the cached slot version — the inline guard re-checks it to catch a GC'd +
    /// reused callee slot whose bits collide but version differs). Returns `None`
    /// for an empty / polymorphic / disabled site — the planner then declines to
    /// inline this call (it keeps the per-call helper). Performs NO fill and NO
    /// side effect (unlike `ic_call`), so it's safe to call at compile time.
    pub(crate) fn ic_call_mono(&self, func_id: u32, ip: usize) -> Option<(u64, u32, u32, u32)> {
        let site = self.ic_site(func_id, ip)?;
        if site.n != 1 {
            return None;
        }
        match site.entries[0] {
            IcEntry::Callee { bits, ver, fid, closure } => Some((bits, ver, fid, closure)),
            _ => None,
        }
    }

    /// Read-only: the resolved class-method `fid` for a `CallMethod` site whose
    /// receiver belongs to `class`, taken from a FILLED `ClassMethod` IC way. For
    /// the Q7 method-inline planner (`build_method_inline_plan`). `None` if no
    /// such way exists yet (unfilled site, or the class resolves a different
    /// entry kind / a different class). Performs no fill / side effect.
    pub(crate) fn ic_class_method_fid(&self, func_id: u32, ip: usize, class: u32) -> Option<u32> {
        let site = self.ic_site(func_id, ip)?;
        for e in &site.entries[..site.n as usize] {
            if let IcEntry::ClassMethod { class: c, fid, .. } = *e {
                if c == class {
                    return Some(fid);
                }
            }
        }
        None
    }

    /// Read-only: the trivial class GETTER `fid` for a `GetProp` site whose
    /// receiver belongs to `class` (Stage 5 accessor inlining), from a filled
    /// `ClassGetter` IC way. zipp resolves class accessors via the class id
    /// (prototype-accessor reassignment ignored — verified JIT==NOJIT), so an arm
    /// baked off this fid + the receiver identity/version guard matches the
    /// interpreter. `None` if no such way / not a plain user fn.
    pub(crate) fn ic_class_getter_fid(&self, func_id: u32, ip: usize, class: u32) -> Option<u32> {
        let site = self.ic_site(func_id, ip)?;
        for e in &site.entries[..site.n as usize] {
            if let IcEntry::ClassGetter { class: c, getter, .. } = *e {
                if c == class {
                    return self.ic_plain_fn(getter).map(|(fid, _)| fid);
                }
            }
        }
        None
    }

    /// Read-only: the trivial class SETTER `fid` for a `SetProp` site whose
    /// receiver belongs to `class` (Stage 5), from a filled `ClassSetter` IC way.
    pub(crate) fn ic_class_setter_fid(&self, func_id: u32, ip: usize, class: u32) -> Option<u32> {
        let site = self.ic_site(func_id, ip)?;
        for e in &site.entries[..site.n as usize] {
            if let IcEntry::ClassSetter { class: c, setter, .. } = *e {
                if c == class {
                    return self.ic_plain_fn(setter).map(|(fid, _)| fid);
                }
            }
        }
        None
    }

    /// Read-only resolver for a `super.m()` op (Stage 3 method inlining): from the
    /// FILLED `SuperData` IC way at site `(func_id, ip)` whose `home` matches the
    /// live `class_values[home_class_id]` and whose hop chain still validates,
    /// return the resolved plain super-method `fid` and the hop `(heap_idx,
    /// version)` guards the inline must re-check each call (anchor..holder; a
    /// `setPrototypeOf` / method reassignment on the chain bumps one). `None` if
    /// no usable way (unfilled / chain mutated / accessor / not a plain fn).
    /// Performs no fill / side effect. Mirrors `ic_super_method`'s hit validation.
    pub(crate) fn ic_super_method_baked(
        &self,
        func_id: u32,
        ip: usize,
        home_class_id: u32,
        key: &str,
    ) -> Option<MiSuperResolved> {
        let home = self.class_values.get(home_class_id as usize).copied().flatten()?;
        let site = self.ic_site(func_id, ip)?;
        for e in &site.entries[..site.n as usize] {
            if let IcEntry::SuperData { home: h, hops, slot } = *e {
                if h == home && self.ic_super_chain_ok(&hops) {
                    if let HeapObj::Object(hm) = self.heap.get(hops.0[hops.1 as usize - 1].0) {
                        let s = slot as usize;
                        if s < hm.keys.len() && hm.keys[s] == key && !hm.attrs[s].accessor {
                            let v = hm.vals[s];
                            if let Some((fid, _closure)) = self.ic_plain_fn(v) {
                                return Some(MiSuperResolved {
                                    fid,
                                    hops: hops.0[..hops.1 as usize].to_vec(),
                                    holder_vals_ptr: hm.vals.as_ptr() as u64,
                                    holder_slot: s as u32,
                                    fn_bits: v.bits(),
                                });
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// FILLED `SuperAcc` IC way at site `(func_id, ip)` whose `home` matches the
    /// baked class — the `super.v` (accessor) twin of `ic_super_method_baked`,
    /// for inlining a `SuperGet` inside a class GETTER body.
    ///
    /// The guard set is IDENTICAL to the method case, and that is not a
    /// coincidence to be re-derived later: for an accessor slot `heap.rs` stores
    /// the GETTER in `vals[i]` (`attrs[i].setter` holds the other half), so
    /// `holder_vals_ptr[holder_slot] == fn_bits` re-checks exactly the function
    /// this baked — the same load, at the same address, for the same reason.
    /// A SETTER would need a second baked pointer into `attrs`, which is why
    /// only the getter direction is resolved here.
    ///
    /// Everything else transfers verbatim: the hop version guards catch
    /// `setPrototypeOf` and a holder realloc, and `mi_class_epoch` catches a
    /// re-executed class declaration.
    pub(crate) fn ic_super_getter_baked(
        &self,
        func_id: u32,
        ip: usize,
        home_class_id: u32,
        key: &str,
    ) -> Option<MiSuperResolved> {
        let home = self.class_values.get(home_class_id as usize).copied().flatten()?;
        let site = self.ic_site(func_id, ip)?;
        for e in &site.entries[..site.n as usize] {
            if let IcEntry::SuperAcc { home: h, hops, slot } = *e {
                if h == home && self.ic_super_chain_ok(&hops) {
                    if let HeapObj::Object(hm) = self.heap.get(hops.0[hops.1 as usize - 1].0) {
                        let s = slot as usize;
                        // The key must still name THIS slot and still be an
                        // accessor: a `delete` + re-add shifts slots, and a
                        // redefine to a data property changes what `vals[s]` means.
                        if s < hm.keys.len() && hm.keys[s] == key && hm.attrs[s].accessor {
                            let g = hm.vals[s];
                            // A getter-less accessor (`set` only) reads as
                            // `undefined` rather than calling anything — not
                            // inlinable as a body, so decline to the helper.
                            if let Some((fid, _closure)) = self.ic_plain_fn(g) {
                                return Some(MiSuperResolved {
                                    fid,
                                    hops: hops.0[..hops.1 as usize].to_vec(),
                                    holder_vals_ptr: hm.vals.as_ptr() as u64,
                                    holder_slot: s as u32,
                                    fn_bits: g.bits(),
                                });
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// FILLED `SuperAcc` IC way for a `super.v = x` site — the SETTER twin of
    /// `ic_super_getter_baked`, and the one place the getter/setter asymmetry
    /// is load-bearing: an accessor slot keeps its getter in `vals[slot]` but
    /// its setter in `attrs[slot].setter` (heap.rs:257), so the getter's
    /// `holder_vals_ptr[holder_slot]` re-check reads the WRONG word for a
    /// setter. Instead of a vals base + slot, this bakes the ABSOLUTE address
    /// of `attrs[slot].setter` into `holder_vals_ptr` with `holder_slot = 0`,
    /// so the emitter's re-check (`[ptr + slot*8] == fn_bits`) reads exactly
    /// the live setter half.
    ///
    /// Deref safety is the same argument as vals: `attrs` reallocates only on
    /// a key add/delete, both of which bump the holder's version, and the hop
    /// version guards run before this address is dereferenced. An in-place
    /// swap of the setter half (`defineProperty` with a new `set`, keeping
    /// `get`) does NOT move the buffer — and is caught by the VALUE compare,
    /// which is the whole point of the re-check.
    pub(crate) fn ic_super_setter_baked(
        &self,
        func_id: u32,
        ip: usize,
        home_class_id: u32,
        key: &str,
    ) -> Option<MiSuperResolved> {
        let home = self.class_values.get(home_class_id as usize).copied().flatten()?;
        let site = self.ic_site(func_id, ip)?;
        for e in &site.entries[..site.n as usize] {
            if let IcEntry::SuperAcc { home: h, hops, slot } = *e {
                if h == home && self.ic_super_chain_ok(&hops) {
                    if let HeapObj::Object(hm) = self.heap.get(hops.0[hops.1 as usize - 1].0) {
                        let s = slot as usize;
                        if s < hm.keys.len() && hm.keys[s] == key && hm.attrs[s].accessor {
                            let setter = hm.attrs[s].setter;
                            // A setter-less accessor (`get` only) is a strict
                            // TypeError / sloppy no-op — helper only.
                            if let Some((fid, _closure)) = self.ic_plain_fn(setter) {
                                return Some(MiSuperResolved {
                                    fid,
                                    hops: hops.0[..hops.1 as usize].to_vec(),
                                    holder_vals_ptr: &hm.attrs[s].setter as *const Value
                                        as u64,
                                    holder_slot: 0,
                                    fn_bits: setter.bits(),
                                });
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// IC for a `Call` site: callee identity (+ slot version) → (fid,
    /// closure) for a plain user function, skipping the Proxy/native/bound/
    /// ctor probes and flag loads. `None` ⇒ slow path.
    #[inline]
    pub(crate) fn ic_call(
        &mut self,
        func_id: u32,
        ip: usize,
        callee: Value,
    ) -> Option<(u32, u32)> {
        if !callee.is_heap() {
            return None;
        }
        if let Some(site) = self.ic_site(func_id, ip) {
            for e in &site.entries[..site.n as usize] {
                if let IcEntry::Callee { bits, ver, fid, closure } = *e {
                    if callee.bits() == bits && self.heap.version_of(callee.heap_index()) == ver
                    {
                        return Some((fid, closure));
                    }
                }
            }
            if site.misses >= IC_MISS_LIMIT {
                return None;
            }
        }
        match self.ic_plain_fn(callee) {
            Some((fid, closure)) => {
                let ver = self.heap.version_of(callee.heap_index());
                self.ic_install(
                    func_id,
                    ip,
                    IcEntry::Callee { bits: callee.bits(), ver, fid, closure },
                );
                Some((fid, closure))
            }
            None => {
                self.ic_note_miss(func_id, ip);
                None
            }
        }
    }

    // ── super sites ──

    /// IC for `SuperMethod` (`hit ⇒ (fid, closure, callee)` to frame-call
    /// with `this` = the current receiver) — and, with `want_acc`, the shared
    /// resolver for `SuperGet`/`SuperSet` accessor entries.
    pub(crate) fn ic_super_method(
        &mut self,
        func_id: u32,
        ip: usize,
        home_class_id: u32,
        is_static: bool,
        key: &str,
    ) -> Option<(u32, u32, Value)> {
        let home = self.class_values.get(home_class_id as usize).copied().flatten()?;
        if let Some(site) = self.ic_site(func_id, ip) {
            for e in &site.entries[..site.n as usize] {
                if let IcEntry::SuperData { home: h, hops, slot } = *e {
                    if h == home && self.ic_super_chain_ok(&hops) {
                        let hm = match self.heap.get(hops.0[hops.1 as usize - 1].0) {
                            HeapObj::Object(hm) => hm,
                            _ => continue,
                        };
                        let s = slot as usize;
                        if s < hm.keys.len() && hm.keys[s] == key && !hm.attrs[s].accessor {
                            let v = hm.vals[s];
                            if let Some((fid, closure)) = self.ic_plain_fn(v) {
                                return Some((fid, closure, v));
                            }
                        }
                    }
                }
            }
            if site.misses >= IC_MISS_LIMIT {
                return None;
            }
        }
        match self.ic_super_walk(home, home_class_id, is_static, key) {
            Some((hops, slot, false)) => {
                let hm = match self.heap.get(hops.0[hops.1 as usize - 1].0) {
                    HeapObj::Object(hm) => hm,
                    _ => return None,
                };
                let v = hm.vals[slot as usize];
                match self.ic_plain_fn(v) {
                    Some((fid, closure)) => {
                        self.ic_install(func_id, ip, IcEntry::SuperData { home, hops, slot });
                        Some((fid, closure, v))
                    }
                    None => {
                        self.ic_note_miss(func_id, ip);
                        None
                    }
                }
            }
            _ => {
                self.ic_note_miss(func_id, ip);
                None
            }
        }
    }

    /// IC for `SuperGet`: a data hit yields the value; an accessor hit yields
    /// a plain getter to frame-call with `this` = the current receiver.
    pub(crate) fn ic_super_get(
        &mut self,
        func_id: u32,
        ip: usize,
        home_class_id: u32,
        is_static: bool,
        key: &str,
    ) -> GetAct {
        let Some(home) = self.class_values.get(home_class_id as usize).copied().flatten()
        else {
            return GetAct::None;
        };
        if let Some(site) = self.ic_site(func_id, ip) {
            for e in &site.entries[..site.n as usize] {
                match *e {
                    IcEntry::SuperData { home: h, hops, slot } if h == home => {
                        if self.ic_super_chain_ok(&hops) {
                            if let HeapObj::Object(hm) =
                                self.heap.get(hops.0[hops.1 as usize - 1].0)
                            {
                                let s = slot as usize;
                                if s < hm.keys.len()
                                    && hm.keys[s] == key
                                    && !hm.attrs[s].accessor
                                {
                                    return GetAct::Value(hm.vals[s]);
                                }
                            }
                        }
                    }
                    IcEntry::SuperAcc { home: h, hops, slot } if h == home => {
                        if self.ic_super_chain_ok(&hops) {
                            match self.ic_chain_acc_get(hops, slot) {
                                GetAct::None => {}
                                act => return act,
                            }
                        }
                    }
                    _ => {}
                }
            }
            if site.misses >= IC_MISS_LIMIT {
                return GetAct::None;
            }
        }
        match self.ic_super_walk(home, home_class_id, is_static, key) {
            Some((hops, slot, accessor)) => {
                if accessor {
                    self.ic_install(func_id, ip, IcEntry::SuperAcc { home, hops, slot });
                    self.ic_chain_acc_get(hops, slot)
                } else {
                    self.ic_install(func_id, ip, IcEntry::SuperData { home, hops, slot });
                    match self.heap.get(hops.0[hops.1 as usize - 1].0) {
                        HeapObj::Object(hm) => GetAct::Value(hm.vals[slot as usize]),
                        _ => GetAct::None,
                    }
                }
            }
            None => {
                self.ic_note_miss(func_id, ip);
                GetAct::None
            }
        }
    }

    /// IC for `SuperSet`: only an inherited SETTER on the super chain is
    /// cacheable (data writes go to the RECEIVER — slow path).
    pub(crate) fn ic_super_set(
        &mut self,
        func_id: u32,
        ip: usize,
        home_class_id: u32,
        is_static: bool,
        key: &str,
    ) -> SetAct {
        let Some(home) = self.class_values.get(home_class_id as usize).copied().flatten()
        else {
            return SetAct::None;
        };
        if let Some(site) = self.ic_site(func_id, ip) {
            for e in &site.entries[..site.n as usize] {
                if let IcEntry::SuperAcc { home: h, hops, slot } = *e {
                    if h == home && self.ic_super_chain_ok(&hops) {
                        match self.ic_chain_acc_set(hops, slot) {
                            SetAct::None => {}
                            act => return act,
                        }
                    }
                }
            }
            if site.misses >= IC_MISS_LIMIT {
                return SetAct::None;
            }
        }
        match self.ic_super_walk(home, home_class_id, is_static, key) {
            Some((hops, slot, true)) => {
                self.ic_install(func_id, ip, IcEntry::SuperAcc { home, hops, slot });
                self.ic_chain_acc_set(hops, slot)
            }
            _ => {
                self.ic_note_miss(func_id, ip);
                SetAct::None
            }
        }
    }

    /// Validate a `Super*` entry's hop versions (`hops[0]` is the derivation
    /// anchor — `setPrototypeOf` on the home's prototype object or on the
    /// class value bumps it; the rest guard the chain to the holder).
    #[inline]
    fn ic_super_chain_ok(&self, hops: &IcHops) -> bool {
        if !self.deferred_ns_state.is_empty() || hops.1 == 0 {
            return false;
        }
        let n = hops.1 as usize;
        for &(h, v) in &hops.0[..n] {
            if self.heap.version_of(h) != v {
                return false;
            }
        }
        true
    }

    /// Side-effect-light fill walk for super sites: derive the super base via
    /// the REAL `super_base` (it may lazily synthesize the prototype object —
    /// idempotent), then chain-walk from it with version snapshots. Returns
    /// `(hops, slot, is_accessor)`; the anchor occupies `hops[0]`.
    fn ic_super_walk(
        &mut self,
        home: Value,
        home_class_id: u32,
        is_static: bool,
        key: &str,
    ) -> Option<(IcHops, u32, bool)> {
        if !self.deferred_ns_state.is_empty() || key.as_bytes().first() == Some(&b'#') {
            return None;
        }
        // Anchor: what `super_base` derives FROM — the synthesized prototype
        // object (instance members) or the class value itself (static).
        // A version bump on it (setPrototypeOf) invalidates the entry.
        let anchor: u32 = if is_static {
            home.heap_index()
        } else {
            // `prototype_of` for a class home: the cached synthesized object.
            match self.prototype_of(home) {
                Some(p) if p.is_heap() => p.heap_index(),
                _ => return None,
            }
        };
        // `fn_proto_override` would re-route `prototype_of`; bail if any
        // override exists for the home (defensive — classes never get one).
        if !self.fn_proto_override.is_empty()
            && self.fn_proto_override.contains_key(&home.heap_index())
        {
            return None;
        }
        let base = self.super_base(home_class_id, is_static);
        if !base.is_heap() {
            return None;
        }
        let mut hops: IcHops = ([(0, 0); IC_MAX_HOPS], 0);
        hops.0[0] = (anchor, self.heap.version_of(anchor));
        hops.1 = 1;
        // Walk from the base (it IS the first chain object to test).
        let mut cur = base.heap_index();
        loop {
            if hops.1 as usize >= IC_MAX_HOPS {
                return None;
            }
            if !self.ic_obj_ok(cur) {
                return None;
            }
            let m = match self.heap.get(cur) {
                HeapObj::Object(m) if !m.is_ctor && m.class.is_none() => m,
                _ => return None,
            };
            hops.0[hops.1 as usize] = (cur, self.heap.version_of(cur));
            hops.1 += 1;
            if let Some(i) = m.pos(key) {
                return Some((hops, i as u32, m.attrs[i].accessor));
            }
            cur = match self.proto_of.get(&cur) {
                Some(&p) if p.is_heap() => p.heap_index(),
                Some(_) => return None, // null end — super misses stay slow
                None => {
                    if self.obj_proto == 0 || cur == self.obj_proto {
                        return None;
                    }
                    self.obj_proto
                }
            };
        }
    }
}
