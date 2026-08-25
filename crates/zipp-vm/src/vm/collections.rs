//! Lazy SameValueZero hash index for the keyed collections (Map / Set /
//! WeakMap / WeakSet).
//!
//! The backing storage stays the spec-insertion-ordered `Vec`s inside
//! `HeapObj::Map { keys, vals }` / `HeapObj::Set(items)` — iterators, the
//! tombstone (HOLE) delete protocol, GC tracing and the subclass-branding
//! clone paths all depend on that layout. What this module adds is a SIDE
//! TABLE on the Vm (`collection_index`): per collection heap index, a hash
//! table from a SameValueZero key representation to the slot position
//! carrying a key with that representation.
//!
//! The index is built LAZILY: a collection below `INDEX_THRESHOLD` keeps the
//! original linear scan (no per-op overhead for small maps); the first
//! indexed operation at or past the threshold builds the table once, and the
//! mutation helpers maintain it from then on. The safety invariant is
//! "ABSENT = linear": any mutation path that cannot cheaply keep the index
//! in sync (clear(), a position-shifting WeakMap/WeakSet delete, re-branding
//! an instance slot) simply drops the entry via `coll_index_invalidate` and
//! the index rebuilds on the next lookup. Tombstoning a slot does NOT shift
//! positions, so Map/Set delete only removes the dead key's index entry.
//!
//! GC: the index holds NO `Value`s (u32 hash tags + u32 slots), so it is not
//! a root; gc.rs sweep hygiene-retains it by marks so a dead collection's
//! entry is dropped before its heap slot can be recycled.

use super::*;

#[cfg(feature = "safe-sandbox")]
use std::collections::{btree_map::Entry, hash_map::RandomState, BTreeMap};
#[cfg(feature = "safe-sandbox")]
use std::hash::{BuildHasher, Hash, Hasher};

/// Collection size at which an indexed operation switches from the linear
/// scan to building (and from then on maintaining) the hash index. Small
/// collections never touch the index machinery.
pub(crate) const INDEX_THRESHOLD: usize = 48;

/// The SameValueZero key representation: a u64 that SameValueZero-equal keys
/// map to (a table hit is always CONFIRMED against the stored key with
/// `svz_eq`, so the repr only has to be equal-consistent, not injective).
///
/// * numbers (int AND double): canonical f64 bits with -0 folded to +0 and
///   every NaN folded to the canonical NaN pattern, so `Value::int(1)` reprs
///   equal to a heap-computed double `1.0`.
/// * strings (flat or rope): FNV-1a over the WTF-8 bytes — `str_wtf8_cow`
///   materializes a rope through the same `write_wtf8` walk `str_eq`
///   compares with, so equal strings repr equal in any representation.
/// * BigInt: VALUE-based, mirroring `strict_eq`'s by-value arms — the i128
///   tier folds its two halves, the beyond-i128 tier hashes the `num_bigint`
///   digits (its `Hash` is consistent with `Eq`); the canonical-form
///   invariant (bigint.rs) means the tiers can never hold equal values, and
///   a per-tier salt keeps them apart.
/// * everything else (objects, symbols, functions, bool, null, undefined):
///   identity — the raw `Value` bits, exactly the `a.bits() == b.bits()` arm
///   of `strict_eq`.
#[inline]
pub(crate) fn svz_repr(heap: &Heap, v: Value) -> u64 {
    if v.is_number() {
        let n = v.as_f64();
        return if n.is_nan() {
            f64::NAN.to_bits() // one canonical NaN
        } else if n == 0.0 {
            0 // -0 and +0 unify
        } else {
            n.to_bits()
        };
    }
    if v.is_heap() {
        let i = v.heap_index();
        match heap.get(i) {
            HeapObj::Str(_) | HeapObj::Cons { .. } => {
                return fnv1a(&heap.str_wtf8_cow(i).unwrap());
            }
            HeapObj::BigInt(x) => {
                let b = *x as u128;
                return mix64(b as u64 ^ 0xB16_1) ^ (b >> 64) as u64;
            }
            HeapObj::BigIntBig(x) => {
                use std::hash::{Hash, Hasher};
                let mut h = rustc_hash::FxHasher::default();
                x.hash(&mut h);
                return h.finish() ^ 0xB16_2;
            }
            _ => {}
        }
    }
    v.bits()
}

/// splitmix64 finalizer: full-avalanche mix of a repr into a table index.
#[inline]
fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// FNV-1a over a byte string (the string-content repr).
#[inline]
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Per-collection acceleration index. Ordinary builds retain the measured flat
/// open-addressing table: an 8-byte `(tag, slot)` entry and linear probing keep
/// its memory traffic low. The hostile-code profile instead uses a `BTreeMap`
/// from a full 128-bit composite content tag to the backing slot. Tree navigation
/// is O(log n) and has no attacker-selectable bucket chain even on
/// wasm32-unknown-unknown, where std's `RandomState` seed comes from predictable
/// linear-memory addresses. An exact collision in BOTH 64-bit tag halves falls
/// back to a confirmed slot list, so that astronomically difficult case remains
/// linear in the number of exact composite collisions rather than being claimed
/// as a strict worst-case logarithmic bound.
///
/// Both representations remain pure acceleration. A tag hit is always
/// CONFIRMED with `svz_eq` against `keys[slot]`, and the authoritative insertion
/// order and tombstones remain in the collection's backing `Vec`.
pub(crate) struct CollIndex {
    #[cfg(not(feature = "safe-sandbox"))]
    table: Vec<(u32, u32)>,
    #[cfg(not(feature = "safe-sandbox"))]
    mask: usize,
    /// Live entries (excludes tombstones).
    live: usize,
    /// Occupied slots INCLUDING tombstones — what bounds probe-chain length,
    /// so growth/rehash triggers on this.
    #[cfg(not(feature = "safe-sandbox"))]
    used: usize,
    #[cfg(feature = "safe-sandbox")]
    tree: BTreeMap<u128, CollSlots>,
    /// The keyed half makes an exact composite-tag collision require defeating
    /// both this content hash and the independent deterministic representation.
    /// Tree balancing, unlike a hash bucket layout, does not trust this seed.
    #[cfg(feature = "safe-sandbox")]
    hash_builder: RandomState,
}

#[cfg(feature = "safe-sandbox")]
enum CollSlots {
    One(u32),
    /// Exact composite-tag collisions are extraordinarily unlikely but cannot
    /// be treated as impossible: correctness still confirms each stored key.
    Many(Vec<u32>),
}

#[cfg(not(feature = "safe-sandbox"))]
const META_EMPTY: u32 = u32::MAX;
#[cfg(not(feature = "safe-sandbox"))]
const META_TOMB: u32 = u32::MAX - 1;

impl CollIndex {
    /// Reserved bytes owned by the separately allocated index storage. The
    /// header itself is inline in `Vm::collection_index` and is charged there.
    pub(crate) fn resident_bytes(&self) -> usize {
        #[cfg(not(feature = "safe-sandbox"))]
        {
            return self
                .table
                .capacity()
                .saturating_mul(std::mem::size_of::<(u32, u32)>());
        }
        #[cfg(feature = "safe-sandbox")]
        {
            // BTreeMap's node layout is private. Charge a deliberately generous
            // two entry-layouts plus eight pointers per live tree key, and a
            // root/node floor; separately include rare collision Vec capacity.
            let per_key = std::mem::size_of::<(u128, CollSlots)>()
                .saturating_add(8 * std::mem::size_of::<usize>())
                .saturating_mul(2);
            let nodes = 1_024usize.saturating_add(self.tree.len().saturating_mul(per_key));
            return self.tree.values().fold(nodes, |bytes, slots| {
                let collision_bytes = match slots {
                    CollSlots::One(_) => 0,
                    CollSlots::Many(slots) => {
                        slots.capacity().saturating_mul(std::mem::size_of::<u32>())
                    }
                };
                bytes.saturating_add(collision_bytes)
            });
        }
    }

    fn with_capacity(n: usize) -> CollIndex {
        #[cfg(not(feature = "safe-sandbox"))]
        {
            // Capacity for `n` entries at < 3/4 load, minimum 128, power of two.
            let cap = (n * 4 / 3 + 1).next_power_of_two().max(128);
            return CollIndex {
                table: vec![(0, META_EMPTY); cap],
                mask: cap - 1,
                live: 0,
                used: 0,
            };
        }
        #[cfg(feature = "safe-sandbox")]
        {
            let _ = n;
            return CollIndex {
                live: 0,
                tree: BTreeMap::new(),
                hash_builder: RandomState::new(),
            };
        }
    }

    /// Index every LIVE (non-tombstoned) slot of `keys`.
    fn build(heap: &Heap, keys: &[Value]) -> CollIndex {
        let mut ix = CollIndex::with_capacity(keys.len());
        for (i, &k) in keys.iter().enumerate() {
            if !k.is_hole() {
                ix.insert(heap, k, i as u32);
            }
        }
        ix
    }

    /// The table hash in the ordinary profile: `mix64`'s high half — bucket =
    /// `tag & mask`.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline]
    fn tag(repr: u64) -> u32 {
        (mix64(repr) >> 32) as u32
    }

    /// Composite ordered key for the hostile-code tree. The high half hashes
    /// actual SameValueZero content; the low half is an independent
    /// equal-consistent representation. Predicting either half cannot unbalance
    /// the tree, and colliding both still falls back to confirmed slot storage.
    #[cfg(feature = "safe-sandbox")]
    fn key_tag(&self, heap: &Heap, v: Value) -> u128 {
        let mut hasher = self.hash_builder.build_hasher();
        if v.is_number() {
            0u8.hash(&mut hasher);
            let n = v.as_f64();
            let bits = if n.is_nan() {
                f64::NAN.to_bits()
            } else if n == 0.0 {
                0
            } else {
                n.to_bits()
            };
            bits.hash(&mut hasher);
        } else if v.is_heap() {
            match heap.get(v.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => {
                    1u8.hash(&mut hasher);
                    heap.str_wtf8_cow(v.heap_index())
                        .expect("string-like heap value")
                        .as_ref()
                        .hash(&mut hasher);
                }
                HeapObj::BigInt(x) => {
                    2u8.hash(&mut hasher);
                    x.hash(&mut hasher);
                }
                HeapObj::BigIntBig(x) => {
                    3u8.hash(&mut hasher);
                    x.hash(&mut hasher);
                }
                _ => {
                    4u8.hash(&mut hasher);
                    v.bits().hash(&mut hasher);
                }
            }
        } else {
            4u8.hash(&mut hasher);
            v.bits().hash(&mut hasher);
        }
        (u128::from(hasher.finish()) << 64) | u128::from(svz_repr(heap, v))
    }

    #[cfg(not(feature = "safe-sandbox"))]
    #[inline]
    fn key_tag(&self, heap: &Heap, v: Value) -> u32 {
        CollIndex::tag(svz_repr(heap, v))
    }

    /// Position of the live slot whose key is SameValueZero-equal to `key`.
    /// A tag hit is CONFIRMED with `svz_eq` against the stored key (collisions
    /// plus the tag's lossiness).
    #[inline]
    fn find(&self, heap: &Heap, keys: &[Value], key: Value) -> Option<usize> {
        #[cfg(feature = "safe-sandbox")]
        {
            let slots = self.tree.get(&self.key_tag(heap, key))?;
            return match slots {
                CollSlots::One(slot) => {
                    svz_eq(heap, keys[*slot as usize], key).then_some(*slot as usize)
                }
                CollSlots::Many(slots) => slots
                    .iter()
                    .copied()
                    .find(|&slot| svz_eq(heap, keys[slot as usize], key))
                    .map(|slot| slot as usize),
            };
        }
        #[cfg(not(feature = "safe-sandbox"))]
        {
            let tag = self.key_tag(heap, key);
            let mut i = tag as usize & self.mask;
            loop {
                let (st, sm) = self.table[i];
                if sm == META_EMPTY {
                    return None;
                }
                if st == tag && sm != META_TOMB && svz_eq(heap, keys[sm as usize], key) {
                    return Some(sm as usize);
                }
                i = (i + 1) & self.mask;
            }
        }
    }

    /// Record `pos` under `key`. The caller guarantees the key is absent
    /// (every insertion path `find`s first), so no duplicate check is needed.
    fn insert(&mut self, heap: &Heap, key: Value, pos: u32) {
        #[cfg(feature = "safe-sandbox")]
        {
            let tag = self.key_tag(heap, key);
            match self.tree.entry(tag) {
                Entry::Vacant(entry) => {
                    entry.insert(CollSlots::One(pos));
                }
                Entry::Occupied(mut entry) => match entry.get_mut() {
                    CollSlots::One(first) => {
                        let first = *first;
                        entry.insert(CollSlots::Many(vec![first, pos]));
                    }
                    CollSlots::Many(slots) => slots.push(pos),
                },
            }
            self.live += 1;
            return;
        }
        #[cfg(not(feature = "safe-sandbox"))]
        {
            if (self.used + 1) * 4 >= self.table.len() * 3 {
                self.grow();
            }
            let tag = self.key_tag(heap, key);
            self.insert_raw(tag, pos);
            self.live += 1;
        }
    }

    #[cfg(not(feature = "safe-sandbox"))]
    fn insert_raw(&mut self, tag: u32, meta: u32) {
        let mut i = tag as usize & self.mask;
        loop {
            let slot = &mut self.table[i];
            if slot.1 == META_EMPTY {
                *slot = (tag, meta);
                self.used += 1;
                return;
            }
            if slot.1 == META_TOMB {
                // Reuse a tombstone: safe because probes only stop at EMPTY.
                *slot = (tag, meta);
                return;
            }
            i = (i + 1) & self.mask;
        }
    }

    /// Tombstone the entry for (`key`, `pos`): probe chains keep walking
    /// past it, and a later insert may reuse the slot.
    fn remove(&mut self, heap: &Heap, key: Value, pos: u32) {
        #[cfg(feature = "safe-sandbox")]
        {
            let tag = self.key_tag(heap, key);
            let mut remove_tag = false;
            let mut removed = false;
            if let Some(entry) = self.tree.get_mut(&tag) {
                match entry {
                    CollSlots::One(slot) => {
                        if *slot == pos {
                            remove_tag = true;
                            removed = true;
                        }
                    }
                    CollSlots::Many(slots) => {
                        if let Some(at) = slots.iter().position(|&slot| slot == pos) {
                            slots.swap_remove(at);
                            removed = true;
                            if slots.len() == 1 {
                                *entry = CollSlots::One(slots[0]);
                            }
                        }
                    }
                }
            }
            if remove_tag {
                self.tree.remove(&tag);
            }
            if removed {
                self.live -= 1;
            }
            return;
        }
        #[cfg(not(feature = "safe-sandbox"))]
        {
            let tag = self.key_tag(heap, key);
            let mut i = tag as usize & self.mask;
            loop {
                let slot = &mut self.table[i];
                if slot.1 == META_EMPTY {
                    return; // absent — remove is only called for indexed entries
                }
                if slot.0 == tag && slot.1 == pos {
                    slot.1 = META_TOMB;
                    self.live -= 1;
                    return;
                }
                i = (i + 1) & self.mask;
            }
        }
    }

    /// Rehash: double when genuinely full, same-size when mostly tombstones
    /// (a delete-heavy phase purges them without growing the table). The
    /// stored tags re-index directly (bucket = `tag & mask`) — no key reads.
    #[cfg(not(feature = "safe-sandbox"))]
    fn grow(&mut self) {
        let cap = if self.live * 2 >= self.table.len() {
            self.table.len() * 2
        } else {
            self.table.len()
        };
        let old = std::mem::replace(&mut self.table, vec![(0, META_EMPTY); cap]);
        self.mask = cap - 1;
        self.used = 0;
        for (tag, meta) in old {
            if meta != META_EMPTY && meta != META_TOMB {
                self.insert_raw(tag, meta);
            }
        }
    }
}

/// JS `===` over the heap — the single implementation; `Vm::values_strict_eq`
/// delegates here so the index's equality can never diverge from the VM's.
pub(crate) fn strict_eq(heap: &Heap, a: Value, b: Value) -> bool {
    if a.bits() == b.bits() {
        if a.is_double() && a.as_f64().is_nan() {
            return false;
        }
        return true;
    }
    if a.is_number() && b.is_number() {
        return a.as_f64() == b.as_f64();
    }
    if a.is_heap() && b.is_heap() {
        let (ai, bi) = (a.heap_index(), b.heap_index());
        if heap.is_str_like(ai) && heap.is_str_like(bi) {
            return heap.str_eq(ai, bi);
        }
        // BigInts compare by VALUE; canonical form means a Small (i128) and
        // a Big (beyond-i128) can never be equal, so same-tier suffices.
        match (heap.get(ai), heap.get(bi)) {
            (HeapObj::BigInt(x), HeapObj::BigInt(y)) => return x == y,
            (HeapObj::BigIntBig(x), HeapObj::BigIntBig(y)) => return x == y,
            _ => {}
        }
    }
    false
}

/// JS `SameValueZero` (the Map/Set key equality) — `Vm::same_value_zero`
/// delegates here.
pub(crate) fn svz_eq(heap: &Heap, a: Value, b: Value) -> bool {
    if a.is_number() && b.is_number() {
        let (na, nb) = (a.as_f64(), b.as_f64());
        return na == nb || (na.is_nan() && nb.is_nan());
    }
    strict_eq(heap, a, b)
}

/// Incremental SameValueZero finder over a LOCAL keys Vec still being built
/// (Map.groupBy's key list, the `extends Set` constructor dedup): linear
/// below the index threshold, hash-indexed once the Vec grows past it. The
/// caller reports every push via `record_push` so the index stays in sync.
pub(crate) struct LocalFinder {
    index: Option<CollIndex>,
}

impl LocalFinder {
    pub(crate) fn new() -> LocalFinder {
        LocalFinder { index: None }
    }

    pub(crate) fn find(&mut self, heap: &Heap, keys: &[Value], key: Value) -> Option<usize> {
        if self.index.is_none() {
            if keys.len() < INDEX_THRESHOLD {
                return keys.iter().position(|&k| svz_eq(heap, k, key));
            }
            self.index = Some(CollIndex::build(heap, keys));
        }
        self.index.as_ref().unwrap().find(heap, keys, key)
    }

    /// Report `key` about to be pushed at `keys.len()` (call BEFORE the push).
    pub(crate) fn record_push(&mut self, heap: &Heap, keys: &[Value], key: Value) {
        if let Some(ix) = &mut self.index {
            ix.insert(heap, key, keys.len() as u32);
        }
    }
}

impl<'p> Vm<'p> {
    /// Position of the live entry whose key/value is SameValueZero-equal to
    /// `key` in collection `idx` (Map/WeakMap keys; Set/WeakSet items):
    /// linear below the threshold, through the lazy hash index at/past it
    /// (building it on first use). This is THE lookup every Map/Set/WeakMap/
    /// WeakSet get/has/set/add/delete/upsert routes through.
    pub(crate) fn coll_find(&mut self, idx: u32, key: Value) -> Option<usize> {
        // Flatten a rope key up front (no-op tag check when already flat):
        // hashing reads the bytes without materializing a copy, every stored
        // key is flat (it was a probe key once), and `str_eq` confirms hits
        // on the flat-flat byte-compare fast path.
        if key.is_heap() {
            self.heap.flatten(key.heap_index());
        }
        let heap = &self.heap;
        let keys: &[Value] = match heap.get(idx) {
            HeapObj::Map { keys, .. } | HeapObj::WeakMap { keys, .. } => keys,
            HeapObj::Set(items) | HeapObj::WeakSet(items) => items,
            _ => return None,
        };
        if let Some(ix) = self.collection_index.get(&idx) {
            return ix.find(heap, keys, key);
        }
        // A user key can never be HOLE (its bit pattern is engine-internal),
        // so tombstoned slots never match — same as the pre-index scans.
        if keys.len() < INDEX_THRESHOLD {
            return keys.iter().position(|&k| svz_eq(heap, k, key));
        }
        // Crossed the threshold: build the index once; maintained from now on.
        let ix = CollIndex::build(heap, keys);
        let found = ix.find(heap, keys, key);
        self.collection_index.insert(idx, ix);
        found
    }

    /// Record `key` newly PUSHED at slot `pos` of collection `idx`. No-op
    /// while the collection has no index (it is built lazily by `coll_find`,
    /// which every insertion path calls first).
    pub(crate) fn coll_index_insert(&mut self, idx: u32, key: Value, pos: usize) {
        // Nursery barrier + B6 oracle: NURSERY_DESIGN.md §1 case 6 —
        // Map/Set/WeakMap/WeakSet insert on an old collection. Every insert
        // path calls this right after its push (no safe point in between, so
        // the age cannot change between store and barrier). Key-grain here;
        // a Map's VALUE rides the same insert — the holder-grain remset entry
        // re-traces keys AND vals, and the value-update arms in `builtins.rs`
        // carry their own barrier.
        self.store_barrier(crate::heap::gcoracle::COLL_INSERT, idx, key);
        if key.is_heap() {
            self.heap.flatten(key.heap_index()); // usually a no-op: coll_find ran first
        }
        let Vm {
            heap,
            collection_index,
            ..
        } = self;
        if let Some(ix) = collection_index.get_mut(&idx) {
            ix.insert(heap, key, pos as u32);
        }
    }

    /// Drop `key`'s entry (slot `pos`, just tombstoned) from `idx`'s index.
    /// Tombstoning shifts no positions, so the rest of the index stays valid.
    pub(crate) fn coll_index_remove(&mut self, idx: u32, key: Value, pos: usize) {
        if key.is_heap() {
            self.heap.flatten(key.heap_index()); // usually a no-op: coll_find ran first
        }
        let Vm {
            heap,
            collection_index,
            ..
        } = self;
        if let Some(ix) = collection_index.get_mut(&idx) {
            ix.remove(heap, key, pos as u32);
        }
    }

    /// Drop collection `idx`'s index entirely. The correct-by-default escape
    /// hatch for any mutation the insert/remove helpers can't describe:
    /// clear() (positions reset), a WeakMap/WeakSet delete (Vec::remove
    /// SHIFTS positions), re-branding an instance slot. Absent = linear; the
    /// index rebuilds lazily on the next indexed lookup.
    pub(crate) fn coll_index_invalidate(&mut self, idx: u32) {
        self.collection_index.remove(&idx);
    }
}

#[cfg(all(test, feature = "safe-sandbox"))]
mod safe_index_tests {
    use super::*;
    use std::collections::HashSet;

    fn old_deterministic_tag(key: &str) -> u32 {
        let mut h = 0xCBF2_9CE4_8422_2325u64;
        for &b in key.as_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3);
        }
        h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h = (h ^ (h >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((h ^ (h >> 31)) >> 32) as u32
    }

    #[test]
    fn ordered_tags_break_the_reusable_deterministic_string_cluster() {
        // These short strings all occupied bucket zero in a 2,048-bucket table
        // under the former public FNV/splitmix mapping. A single precomputed
        // list therefore attacked Object, Map, and Set indexes in every VM.
        let colliders = [
            "p244", "p687", "p4225", "p6240", "p13634", "p15844", "p16140", "p16375", "p17447",
            "p18159", "p19922", "p19997", "p20117", "p21684", "p23891", "p26786", "p27877",
            "p28218", "p32488", "p34067", "p38609", "p38820", "p39395", "p44096",
        ];
        assert!(colliders
            .iter()
            .all(|key| old_deterministic_tag(key) & 2_047 == 0));

        let mut heap = Heap::new();
        let keys: Vec<Value> = colliders
            .iter()
            .map(|key| Value::heap(heap.alloc_str((*key).to_string())))
            .collect();
        let mut first = CollIndex::with_capacity(1_024);
        let second = CollIndex::with_capacity(1_024);
        let tags: HashSet<u128> = keys.iter().map(|&key| first.key_tag(&heap, key)).collect();
        assert_eq!(tags.len(), colliders.len());
        assert!(
            keys.iter()
                .filter(|&&key| first.key_tag(&heap, key) != second.key_tag(&heap, key))
                .count()
                >= colliders.len() / 2,
            "independent collection indexes unexpectedly reused one hash key"
        );
        for (pos, &key) in keys.iter().enumerate() {
            first.insert(&heap, key, pos as u32);
        }
        for (pos, &key) in keys.iter().enumerate() {
            assert_eq!(first.find(&heap, &keys, key), Some(pos));
        }
    }

    #[test]
    fn keyed_tags_preserve_same_value_zero_equivalence() {
        let heap = Heap::new();
        let index = CollIndex::with_capacity(INDEX_THRESHOLD);
        assert_eq!(
            index.key_tag(&heap, Value::int(1)),
            index.key_tag(&heap, Value::from_bits(1.0f64.to_bits()))
        );
        assert_eq!(
            index.key_tag(&heap, Value::from_bits(0.0f64.to_bits())),
            index.key_tag(&heap, Value::num(-0.0))
        );
        assert_eq!(
            index.key_tag(&heap, Value::num(f64::NAN)),
            index.key_tag(&heap, Value::num(-f64::NAN))
        );
    }

    #[test]
    fn ordered_index_keeps_tags_valid_across_growth_and_removal() {
        let mut heap = Heap::new();
        let keys: Vec<Value> = (0..200)
            .map(|i| Value::heap(heap.alloc_str(format!("collection-key-{i}"))))
            .collect();
        let mut index = CollIndex::with_capacity(0);
        for (pos, &key) in keys.iter().enumerate() {
            index.insert(&heap, key, pos as u32);
        }
        assert_eq!(index.tree.len(), keys.len());
        for (pos, &key) in keys.iter().enumerate() {
            assert_eq!(index.find(&heap, &keys, key), Some(pos));
        }

        for (pos, &key) in keys.iter().enumerate().step_by(3) {
            index.remove(&heap, key, pos as u32);
        }
        for (pos, &key) in keys.iter().enumerate() {
            assert_eq!(
                index.find(&heap, &keys, key),
                (pos % 3 != 0).then_some(pos),
                "wrong result for slot {pos} after tombstoning"
            );
        }
    }

    #[test]
    fn ten_thousand_legacy_low_bit_colliders_stay_structurally_bounded() {
        // Finding 10k names with seven chosen legacy bucket bits takes about
        // 1.28m deterministic trials, keeping this regression quick while
        // presenting an adversarial insertion order. BTree lookup cost depends
        // only on entry count, never on those attacker-selected hash bits.
        let mut names = Vec::with_capacity(10_000);
        let mut candidate = 0u64;
        while names.len() < 10_000 {
            let key = format!("tree-flood-{candidate:x}");
            if old_deterministic_tag(&key) & 127 == 0 {
                names.push(key);
            }
            candidate += 1;
        }

        let mut heap = Heap::new();
        let keys: Vec<Value> = names
            .into_iter()
            .map(|key| Value::heap(heap.alloc_str(key)))
            .collect();
        let mut index = CollIndex::build(&heap, &keys);
        assert_eq!(index.live, keys.len());
        for (pos, &key) in keys.iter().enumerate() {
            assert_eq!(index.find(&heap, &keys, key), Some(pos));
        }

        for (pos, &key) in keys.iter().enumerate().step_by(97) {
            index.remove(&heap, key, pos as u32);
        }
        for (pos, &key) in keys.iter().enumerate().step_by(97) {
            assert_eq!(index.find(&heap, &keys, key), None, "slot {pos} survived");
        }
    }
}
