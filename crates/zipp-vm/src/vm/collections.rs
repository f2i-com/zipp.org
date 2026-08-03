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

/// The per-collection index: a flat OPEN-ADDRESSING table of
/// `(tag, meta)` pairs with linear probing — `tag` is the high half of the
/// mixed repr and ALSO picks the bucket (`tag & mask`), `meta` the slot
/// position. Chosen over a general HashMap because a 1M-entry index is far
/// larger than cache and the per-op cost is memory traffic: 8-byte slots
/// pack EIGHT to a cache line (a whole miss chain at 3/4 load is typically
/// one line), and a lookup costs one random line plus one confirming read of
/// the backing key — the V8 OrderedHashTable shape, whose entry data is
/// hot/sequential for insertion-ordered access while only the small bucket
/// probe is random. Bucket-from-tag means growth rehashes STRAIGHT from the
/// stored tags — no key reads, no content re-hashing (`mix64` avalanches, so
/// the tag's low bits index as well as the full hash's would).
///
/// A tag match is always CONFIRMED with `svz_eq` against `keys[meta]` (the
/// 32-bit tag is lossy). Two `meta` sentinels: `META_EMPTY` (never occupied
/// — terminates probe chains) and `META_TOMB` (removed — chains walk past
/// it, inserts may reuse it).
pub(crate) struct CollIndex {
    table: Vec<(u32, u32)>,
    mask: usize,
    /// Live entries (excludes tombstones).
    live: usize,
    /// Occupied slots INCLUDING tombstones — what bounds probe-chain length,
    /// so growth/rehash triggers on this.
    used: usize,
}

const META_EMPTY: u32 = u32::MAX;
const META_TOMB: u32 = u32::MAX - 1;

impl CollIndex {
    fn with_capacity(n: usize) -> CollIndex {
        // Capacity for `n` entries at < 3/4 load, minimum 128, power of two.
        let cap = (n * 4 / 3 + 1).next_power_of_two().max(128);
        CollIndex { table: vec![(0, META_EMPTY); cap], mask: cap - 1, live: 0, used: 0 }
    }

    /// Index every LIVE (non-tombstoned) slot of `keys`.
    fn build(heap: &Heap, keys: &[Value]) -> CollIndex {
        let mut ix = CollIndex::with_capacity(keys.len());
        for (i, &k) in keys.iter().enumerate() {
            if !k.is_hole() {
                ix.insert(svz_repr(heap, k), i as u32);
            }
        }
        ix
    }

    /// The table hash of a repr: `mix64`'s high half — bucket = `tag & mask`.
    #[inline]
    fn tag(repr: u64) -> u32 {
        (mix64(repr) >> 32) as u32
    }

    /// Position of the live slot whose key is SameValueZero-equal to `key`
    /// (repr `repr`). A tag hit is CONFIRMED with `svz_eq` against the
    /// stored key (collisions + the tag's lossiness).
    #[inline]
    fn find(&self, heap: &Heap, keys: &[Value], key: Value, repr: u64) -> Option<usize> {
        let tag = CollIndex::tag(repr);
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

    /// Record `pos` under `repr`. The caller guarantees the key is absent
    /// (every insertion path `find`s first), so no duplicate check is needed.
    fn insert(&mut self, repr: u64, pos: u32) {
        if (self.used + 1) * 4 >= self.table.len() * 3 {
            self.grow();
        }
        self.insert_raw(CollIndex::tag(repr), pos);
        self.live += 1;
    }

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

    /// Tombstone the entry for (`repr`, `pos`): probe chains keep walking
    /// past it, and a later insert may reuse the slot.
    fn remove(&mut self, repr: u64, pos: u32) {
        let tag = CollIndex::tag(repr);
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

    /// Rehash: double when genuinely full, same-size when mostly tombstones
    /// (a delete-heavy phase purges them without growing the table). The
    /// stored tags re-index directly (bucket = `tag & mask`) — no key reads.
    fn grow(&mut self) {
        let cap =
            if self.live * 2 >= self.table.len() { self.table.len() * 2 } else { self.table.len() };
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
        self.index.as_ref().unwrap().find(heap, keys, key, svz_repr(heap, key))
    }

    /// Report `key` about to be pushed at `keys.len()` (call BEFORE the push).
    pub(crate) fn record_push(&mut self, heap: &Heap, keys: &[Value], key: Value) {
        if let Some(ix) = &mut self.index {
            ix.insert(svz_repr(heap, key), keys.len() as u32);
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
            return ix.find(heap, keys, key, svz_repr(heap, key));
        }
        // A user key can never be HOLE (its bit pattern is engine-internal),
        // so tombstoned slots never match — same as the pre-index scans.
        if keys.len() < INDEX_THRESHOLD {
            return keys.iter().position(|&k| svz_eq(heap, k, key));
        }
        // Crossed the threshold: build the index once; maintained from now on.
        let ix = CollIndex::build(heap, keys);
        let found = ix.find(heap, keys, key, svz_repr(heap, key));
        self.collection_index.insert(idx, ix);
        found
    }

    /// Record `key` newly PUSHED at slot `pos` of collection `idx`. No-op
    /// while the collection has no index (it is built lazily by `coll_find`,
    /// which every insertion path calls first).
    pub(crate) fn coll_index_insert(&mut self, idx: u32, key: Value, pos: usize) {
        // B6 oracle: NURSERY_DESIGN.md §1 case 6 — Map/Set/WeakMap/WeakSet
        // insert on an old collection (key-grain; a Map's VALUE rides the same
        // insert and is not double-counted).
        self.oracle_store(crate::heap::gcoracle::COLL_INSERT, idx, key);
        if key.is_heap() {
            self.heap.flatten(key.heap_index()); // usually a no-op: coll_find ran first
        }
        let Vm { heap, collection_index, .. } = self;
        if let Some(ix) = collection_index.get_mut(&idx) {
            ix.insert(svz_repr(heap, key), pos as u32);
        }
    }

    /// Drop `key`'s entry (slot `pos`, just tombstoned) from `idx`'s index.
    /// Tombstoning shifts no positions, so the rest of the index stays valid.
    pub(crate) fn coll_index_remove(&mut self, idx: u32, key: Value, pos: usize) {
        if key.is_heap() {
            self.heap.flatten(key.heap_index()); // usually a no-op: coll_find ran first
        }
        let Vm { heap, collection_index, .. } = self;
        if let Some(ix) = collection_index.get_mut(&idx) {
            ix.remove(svz_repr(heap, key), pos as u32);
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
