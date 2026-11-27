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
pub(crate) const INDEX_THRESHOLD: usize = 9;

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
    if v.is_int() {
        let n = v.as_int();
        return if n == 0 { 0 } else { (n as f64).to_bits() };
    }
    if v.is_double() {
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
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        {
            let mir = heap.hot_mirror_of(i);
            if mir.shape != crate::shape::DICT
                || (mir.fid != crate::heap::FID_MIRROR_NONE
                    && mir.fid != crate::heap::FID_MIRROR_STR)
            {
                return v.bits();
            }
        }
        match heap.get(i) {
            HeapObj::Str(s) => {
                return s.seeded_hash();
            }
            HeapObj::Cons { .. } => {
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

/// The hash of a byte string (the string-content repr), from a basis keyed by
/// the per-process [`crate::heap::hash_seed`]: the one a property tag starts
/// from, eight bytes a step where it was FNV-1a's one.
#[inline]
fn fnv1a(bytes: &[u8]) -> u64 {
    crate::heap::seeded_bytes_hash(bytes)
}

/// Per-collection acceleration index. Ordinary builds retain the measured flat
/// open-addressing table: an 8-byte `(tag, slot)` entry and linear probing keep
/// its memory traffic low. The hostile-code profile starts with bucket chains
/// ([`Chains`]) that no insert may make longer than [`CHAIN_MAX`], so a lookup
/// confirms at most that many entries; an insert that would lengthen one
/// rebuilds the index as a `BTreeMap` from a full 128-bit composite content tag
/// to the backing slot, for the collection's life. Tree navigation is O(log n)
/// and has no attacker-selectable bucket chain even on wasm32-unknown-unknown,
/// where std's `RandomState` seed comes from predictable linear-memory
/// addresses: crafting a long chain buys the tree, and nothing slower. An
/// exact collision in BOTH 64-bit tag halves falls back to a confirmed slot
/// list, so that astronomically difficult case remains linear in the number
/// of exact composite collisions rather than being claimed as a strict
/// worst-case logarithmic bound.
///
/// The tree alone cost a cache miss a level: in a Map of 800,000 keys a
/// lookup was most of map-set-heavy's run.
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
    /// True for WeakMap/WeakSet identity indexes, where `table[i].0` stores
    /// the exact `key.heap_index()` instead of a lossy hash tag so lookups
    /// confirm matches without dereferencing `keys[slot]`.
    #[cfg(not(feature = "safe-sandbox"))]
    identity: bool,
    /// For identity WeakMap/WeakSet indexes: prefix `0..indexed_len` of the
    /// backing `keys` Vec has been inserted into `table`. Entries pushed past
    /// `indexed_len` by the fresh-key (`!GEN_WEAK_KEY`) fast path are lazily
    /// flushed on the next lookup or duplicate-key insert (`sync_identity`).
    #[cfg(not(feature = "safe-sandbox"))]
    indexed_len: usize,
    /// Number of entries at `pos >= indexed_len` tombstoned by GC before
    /// `sync_identity` flushed them into `table`.
    #[cfg(not(feature = "safe-sandbox"))]
    unindexed_dead: usize,
    /// The bucket chains, until an insert would make one too long.
    #[cfg(feature = "safe-sandbox")]
    chains: Option<Chains>,
    /// The tree, once `chains` is gone; empty until then.
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

/// Longest chain an insert may make in [`Chains`]. Buckets are kept at most
/// half full, where an honest chain this long comes up about once in 10^17
/// buckets: only keys chosen to collide reach it, and they get the tree.
#[cfg(feature = "safe-sandbox")]
const CHAIN_MAX: usize = 16;

/// End of a chain, and the slot of a vacated entry.
#[cfg(feature = "safe-sandbox")]
const NO_ENTRY: u32 = u32::MAX;

/// The hostile-code profile's index while every chain is short: buckets of
/// entries linked in insertion order, picked by the seeded mix of the key's
/// `svz_repr` (the ordinary profile's table hash).
#[cfg(feature = "safe-sandbox")]
struct Chains {
    /// Bucket -> its first entry, `NO_ENTRY` when empty. A power of two, at
    /// least twice the live entries.
    heads: Vec<u32>,
    entries: Vec<ChainEntry>,
    /// Entries removals vacated, for the next inserts.
    vacant: Vec<u32>,
}

#[cfg(feature = "safe-sandbox")]
#[derive(Clone, Copy)]
struct ChainEntry {
    /// `svz_repr` of the key: equal keys have equal reprs.
    repr: u64,
    /// The key itself, to rebuild the index as the tree, whose tags hash the
    /// key's content. It is also in the collection's backing at `slot`.
    key: Value,
    slot: u32,
    next: u32,
}

#[cfg(feature = "safe-sandbox")]
impl Chains {
    fn with_buckets(buckets: usize) -> Chains {
        Chains {
            heads: vec![NO_ENTRY; buckets],
            entries: Vec::new(),
            vacant: Vec::new(),
        }
    }

    #[inline]
    fn bucket(&self, repr: u64) -> usize {
        (mix64(repr ^ crate::heap::hash_seed()) as usize) & (self.heads.len() - 1)
    }

    #[inline]
    fn find(&self, heap: &Heap, keys: &[Value], key: Value, repr: u64) -> Option<usize> {
        let mut at = self.heads[self.bucket(repr)];
        while at != NO_ENTRY {
            let entry = &self.entries[at as usize];
            if entry.repr == repr && svz_eq(heap, keys[entry.slot as usize], key) {
                return Some(entry.slot as usize);
            }
            at = entry.next;
        }
        None
    }

    /// Link `key` at `slot`, unless its chain already holds [`CHAIN_MAX`]
    /// entries: then nothing is linked and the caller turns to the tree.
    fn insert(&mut self, repr: u64, key: Value, slot: u32, live: usize) -> bool {
        if (live + 1) * 2 > self.heads.len() {
            self.grow();
        }
        let bucket = self.bucket(repr);
        let mut len = 0;
        let mut at = self.heads[bucket];
        while at != NO_ENTRY {
            len += 1;
            at = self.entries[at as usize].next;
        }
        if len >= CHAIN_MAX {
            return false;
        }
        let entry = ChainEntry {
            repr,
            key,
            slot,
            next: self.heads[bucket],
        };
        let at = match self.vacant.pop() {
            Some(at) => {
                self.entries[at as usize] = entry;
                at
            }
            None => {
                self.entries.push(entry);
                (self.entries.len() - 1) as u32
            }
        };
        self.heads[bucket] = at;
        true
    }

    /// Unlink the entry for `slot` (whose key has `repr`); whether one was.
    fn remove(&mut self, repr: u64, slot: u32) -> bool {
        let bucket = self.bucket(repr);
        let mut prev = NO_ENTRY;
        let mut at = self.heads[bucket];
        while at != NO_ENTRY {
            let entry = self.entries[at as usize];
            if entry.slot == slot {
                if prev == NO_ENTRY {
                    self.heads[bucket] = entry.next;
                } else {
                    self.entries[prev as usize].next = entry.next;
                }
                self.entries[at as usize].slot = NO_ENTRY;
                self.vacant.push(at);
                return true;
            }
            prev = at;
            at = entry.next;
        }
        false
    }

    /// Double the buckets and relink every live entry. A chain only splits,
    /// so none grows past what it was.
    fn grow(&mut self) {
        let buckets = self.heads.len() * 2;
        self.heads = vec![NO_ENTRY; buckets];
        for at in 0..self.entries.len() {
            if self.entries[at].slot == NO_ENTRY {
                continue;
            }
            let bucket = self.bucket(self.entries[at].repr);
            self.entries[at].next = self.heads[bucket];
            self.heads[bucket] = at as u32;
        }
    }

    fn resident_bytes(&self) -> usize {
        (self.heads.capacity() * std::mem::size_of::<u32>())
            .saturating_add(self.entries.capacity() * std::mem::size_of::<ChainEntry>())
            .saturating_add(self.vacant.capacity() * std::mem::size_of::<u32>())
    }
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
        if let Some(chains) = &self.chains {
            return chains.resident_bytes();
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
            // Capacity for `n` entries below half load, minimum 128, power of
            // two (see `insert`).
            let cap = (n * 2 + 1).next_power_of_two().max(128);
            return CollIndex {
                table: vec![(0, META_EMPTY); cap],
                mask: cap - 1,
                live: 0,
                used: 0,
                identity: false,
                indexed_len: 0,
                unindexed_dead: 0,
            };
        }
        #[cfg(feature = "safe-sandbox")]
        {
            // At most half full: room for `n` entries before the first growth.
            let buckets = (n * 2 + 1).next_power_of_two().max(16);
            return CollIndex {
                live: 0,
                chains: Some(Chains::with_buckets(buckets)),
                tree: BTreeMap::new(),
                hash_builder: RandomState::new(),
            };
        }
    }

    #[cfg(not(feature = "safe-sandbox"))]
    fn with_capacity_identity(n: usize) -> CollIndex {
        let cap = (n * 2 + 1).next_power_of_two().max(64);
        CollIndex {
            table: vec![(0, META_EMPTY); cap],
            mask: cap - 1,
            live: 0,
            used: 0,
            identity: true,
            indexed_len: 0,
            unindexed_dead: 0,
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

    /// Identity-only `CollIndex::build` for WeakMap/WeakSet keys: stores
    /// `(key.heap_index(), pos)` directly so lookups never touch `keys[slot]`.
    #[cfg(not(feature = "safe-sandbox"))]
    fn build_identity(keys: &[Value]) -> CollIndex {
        let mut ix = CollIndex::with_capacity_identity(keys.len());
        for (i, &k) in keys.iter().enumerate() {
            if !k.is_hole() {
                ix.insert_identity(k, i as u32);
            }
        }
        ix.indexed_len = keys.len();
        ix
    }

    /// Flush any tail entries pushed via the `!GEN_WEAK_KEY` fast path into
    /// this identity table before probing it.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline]
    fn sync_identity(&mut self, keys: &[Value]) {
        if self.indexed_len < keys.len() {
            self.sync_identity_cold(keys);
        }
    }

    #[cfg(not(feature = "safe-sandbox"))]
    #[cold]
    #[inline(never)]
    fn sync_identity_cold(&mut self, keys: &[Value]) {
        let start = self.indexed_len;
        for (offset, &k) in keys[start..].iter().enumerate() {
            if !k.is_hole() {
                self.insert_identity(k, (start + offset) as u32);
            }
        }
        self.indexed_len = keys.len();
        self.unindexed_dead = 0;
    }

    #[inline]
    fn live_total(&self, backing_len: usize) -> usize {
        #[cfg(not(feature = "safe-sandbox"))]
        if self.identity {
            return self.live
                + backing_len
                    .saturating_sub(self.indexed_len)
                    .saturating_sub(self.unindexed_dead);
        }
        let _ = backing_len;
        self.live
    }

    #[cfg(not(feature = "safe-sandbox"))]
    #[inline]
    fn insert_identity_raw(&mut self, kidx: u32, meta: u32) {
        let mut i = CollIndex::tag(Value::heap(kidx).bits()) as usize & self.mask;
        loop {
            let slot = &mut self.table[i];
            if slot.1 == META_EMPTY {
                *slot = (kidx, meta);
                self.used += 1;
                return;
            }
            if slot.1 == META_TOMB {
                *slot = (kidx, meta);
                return;
            }
            i = (i + 1) & self.mask;
        }
    }

    /// Identity-only `CollIndex::insert` for WeakMap/WeakSet keys.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline]
    fn insert_identity(&mut self, key: Value, pos: u32) {
        if (self.used + 1) * 2 >= self.table.len() {
            self.grow();
        }
        self.insert_identity_raw(key.heap_index(), pos);
        self.live += 1;
    }

    /// Identity-only `CollIndex::remove` for WeakMap/WeakSet keys.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline]
    fn remove_identity(&mut self, key: Value, pos: u32) {
        if (pos as usize) >= self.indexed_len {
            self.unindexed_dead += 1;
            return;
        }
        let kidx = key.heap_index();
        let mut i = CollIndex::tag(key.bits()) as usize & self.mask;
        loop {
            let slot = &mut self.table[i];
            if slot.1 == META_EMPTY {
                return;
            }
            if slot.0 == kidx && slot.1 == pos {
                slot.1 = META_TOMB;
                self.live -= 1;
                return;
            }
            i = (i + 1) & self.mask;
        }
    }

    /// The table hash in the ordinary profile: `mix64`'s high half — bucket =
    /// `tag & mask`. Keyed by the per-process seed (numbers and object
    /// identities hash their raw bits), so the bucket of a key is not
    /// computable offline.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline]
    fn tag(repr: u64) -> u32 {
        (mix64(repr ^ crate::heap::hash_seed()) >> 32) as u32
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
        if let Some(chains) = &self.chains {
            return chains.find(heap, keys, key, svz_repr(heap, key));
        }
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

    /// Fast identity-only lookup for WeakMap / WeakSet keys (which are always
    /// heap objects or non-registered symbols, so `table[i].0 == key.heap_index()`
    /// confirms an exact match without dereferencing `keys[slot]`).
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline]
    fn find_identity(&self, _keys: &[Value], key: Value) -> Option<usize> {
        let kidx = key.heap_index();
        let mut i = CollIndex::tag(key.bits()) as usize & self.mask;
        loop {
            let (st, sm) = self.table[i];
            if sm == META_EMPTY {
                return None;
            }
            if sm != META_TOMB && st == kidx {
                return Some(sm as usize);
            }
            i = (i + 1) & self.mask;
        }
    }

    /// Record `pos` under `key`. The caller guarantees the key is absent
    /// (every insertion path `find`s first), so no duplicate check is needed.
    fn insert(&mut self, heap: &Heap, key: Value, pos: u32) {
        #[cfg(feature = "safe-sandbox")]
        {
            let live = self.live;
            if let Some(chains) = &mut self.chains {
                if chains.insert(svz_repr(heap, key), key, pos, live) {
                    self.live += 1;
                    return;
                }
                // A chain at its limit: these keys were chosen to collide.
                self.to_tree(heap);
            }
            self.tree_insert(heap, key, pos);
            self.live += 1;
            return;
        }
        #[cfg(not(feature = "safe-sandbox"))]
        {
            // Linear probing stays short only well below full: at three
            // quarters an absent key (what every `set` of a new key and
            // every miss looks for first) walks eight and a half slots on
            // average, and a Map filled with `set` spends half its life
            // between a half and three quarters. Grown at half, a miss
            // walks two and a half.
            if (self.used + 1) * 2 >= self.table.len() {
                self.grow();
            }
            let tag = self.key_tag(heap, key);
            self.insert_raw(tag, pos);
            self.live += 1;
        }
    }

    /// Record `pos` under `key` in the tree.
    #[cfg(feature = "safe-sandbox")]
    fn tree_insert(&mut self, heap: &Heap, key: Value, pos: u32) {
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
    }

    /// Move every chained entry into the tree, which serves the collection
    /// from now on.
    #[cfg(feature = "safe-sandbox")]
    fn to_tree(&mut self, heap: &Heap) {
        let Some(chains) = self.chains.take() else {
            return;
        };
        for entry in &chains.entries {
            if entry.slot != NO_ENTRY {
                self.tree_insert(heap, entry.key, entry.slot);
            }
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
        if let Some(chains) = &mut self.chains {
            if chains.remove(svz_repr(heap, key), pos) {
                self.live -= 1;
            }
            return;
        }
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
        // Double when live entries fill a quarter; otherwise the slots are
        // mostly tombstones and a same-size rehash leaves room until half.
        let cap = if self.live * 4 >= self.table.len() {
            self.table.len() * 2
        } else {
            self.table.len()
        };
        let old = std::mem::replace(&mut self.table, vec![(0, META_EMPTY); cap]);
        self.mask = cap - 1;
        self.used = 0;
        for (tag, meta) in old {
            if meta != META_EMPTY && meta != META_TOMB {
                if self.identity {
                    self.insert_identity_raw(tag, meta);
                } else {
                    self.insert_raw(tag, meta);
                }
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
        // Two different atoms have different texts (`Heap::atoms`).
        if heap.is_atom(ai) && heap.is_atom(bi) {
            return false;
        }
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

/// The first position in `items` (the last, when `rev`) whose element is
/// `=== target`, a hole matching nothing: `indexOf` / `lastIndexOf` over a
/// dense store. Split by the kind of `target` so each element costs what its
/// comparison needs: a number compares by value against numbers only (an
/// Int target equals an Int element only by its bits), a string or BigInt
/// keeps `strict_eq`, and anything else (object, symbol, boolean, null,
/// undefined) is equal only to its own bits. `strict_eq` per element, a call
/// with its tag tests, scanned at a third of V8's rate.
pub(crate) fn strict_eq_position(heap: &Heap, items: &[Value], target: Value, rev: bool) -> Option<usize> {
    #[inline(always)]
    fn find(items: &[Value], rev: bool, f: impl Fn(Value) -> bool) -> Option<usize> {
        if rev {
            items.iter().rposition(|&v| f(v))
        } else {
            items.iter().position(|&v| f(v))
        }
    }
    let t = target.bits();
    if target.is_int() {
        let tf = target.as_f64();
        return find(items, rev, |v| v.bits() == t || (v.is_double() && v.as_f64() == tf));
    }
    if target.is_double() {
        let tf = target.as_f64();
        if tf.is_nan() {
            return None;
        }
        return find(items, rev, |v| v.is_number() && v.as_f64() == tf);
    }
    if !target.is_heap() {
        return find(items, rev, |v| v.bits() == t);
    }
    let ti = target.heap_index();
    match heap.get(ti) {
        HeapObj::Str(ts) => {
            // Single-pass scan in index order (preserving first/last match
            // when a non-atom copy precedes an atom with the same bytes).
            let want = ts.as_bytes();
            let target_atom = heap.is_atom(ti);
            find(items, rev, |v| {
                if v.bits() == t {
                    return true;
                }
                if !v.is_heap() {
                    return false;
                }
                let vi = v.heap_index();
                if target_atom && heap.is_atom(vi) {
                    return false;
                }
                match heap.get(vi) {
                    HeapObj::Str(vs) => vs.as_bytes() == want,
                    HeapObj::Cons { .. } => heap.str_eq(vi, ti),
                    _ => false,
                }
            })
        }
        HeapObj::Cons { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_) => {
            find(items, rev, |v| !v.is_hole() && strict_eq(heap, v, target))
        }
        _ => find(items, rev, |v| v.bits() == t),
    }
}

/// JS `SameValueZero` (the Map/Set key equality) — `Vm::same_value_zero`
/// delegates here.
#[inline]
pub(crate) fn svz_eq(heap: &Heap, a: Value, b: Value) -> bool {
    // Under SameValueZero, identical raw NaN-boxed bits are ALWAYS equal
    // (including ints, doubles, NaN === NaN, objects, symbols, same-slot strings).
    if a.bits() == b.bits() {
        return true;
    }
    if a.is_number() && b.is_number() {
        let (na, nb) = (a.as_f64(), b.as_f64());
        return na == nb || (na.is_nan() && nb.is_nan());
    }
    strict_eq(heap, a, b)
}

/// `keys.iter().position(|&k| svz_eq(heap, k, key))`, split by the kind of
/// `key` so each entry costs what its comparison needs: a flat string's
/// bytes are fetched once and compared against each flat string entry (a
/// rope entry keeps `str_eq`), a number compares by value against number
/// entries, a BigInt keeps `svz_eq`, and every other key (object, symbol,
/// boolean, null, undefined) is equal only to its own bits, which is what
/// `strict_eq` answers for it. A small Set of strings probed with a string
/// paid two kind tests and a second lookup of the probe per entry.
#[inline]
pub(crate) fn svz_position(heap: &Heap, keys: &[Value], key: Value) -> Option<usize> {
    if key.is_number() {
        let n = key.as_f64();
        if n.is_nan() {
            return keys.iter().position(|&k| k.is_number() && k.as_f64().is_nan());
        }
        return keys.iter().position(|&k| k.is_number() && k.as_f64() == n);
    }
    // Map/Set keys are unique under SameValueZero, so an exact bits match is
    // ALWAYS the unique position without touching `heap.objs` at all.
    if let Some(pos) = keys.iter().position(|&k| k.bits() == key.bits()) {
        return Some(pos);
    }
    if key.is_heap() {
        let key_idx = key.heap_index();
        // Fast non-string/non-BigInt miss short-circuit via HotMirror:
        // shaped Objects, Funcs/Closures, Class instances, and Ctors compare
        // purely by identity (already checked above), avoiding cold `heap.objs`
        // loads on every small-Map/Set miss.
        #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
        {
            let mir = heap.hot_mirror_of(key_idx);
            if mir.shape != crate::shape::DICT
                || (mir.fid != crate::heap::FID_MIRROR_NONE
                    && mir.fid != crate::heap::FID_MIRROR_STR)
            {
                return None;
            }
        }
        match heap.get(key_idx) {
            HeapObj::Str(s) => {
                let want = s.as_bytes();
                let key_atom = heap.is_atom(key_idx);
                return keys.iter().position(|&k| {
                    if !k.is_heap() {
                        return false;
                    }
                    let ki = k.heap_index();
                    if key_atom && heap.is_atom(ki) {
                        return false;
                    }
                    match heap.get(ki) {
                        HeapObj::Str(t) => t.as_bytes() == want,
                        HeapObj::Cons { .. } => heap.str_eq(ki, key_idx),
                        _ => false,
                    }
                });
            }
            HeapObj::Cons { .. } | HeapObj::BigInt(_) | HeapObj::BigIntBig(_) => {
                return keys.iter().position(|&k| svz_eq(heap, k, key));
            }
            _ => {}
        }
    }
    None
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
    /// Number of live entries in an ordinary Map/Set. Indexed collections
    /// already maintain this count, which keeps `.size` and LenOf constant-time
    /// after delete-heavy or repeated clear/add workloads. Small unindexed
    /// collections scan at most the lazy-index threshold in normal operation.
    pub(crate) fn coll_live_len(&self, idx: u32) -> usize {
        if let Some(index) = self.collection_index.get(&idx) {
            return index.live;
        }
        match self.heap.get(idx) {
            HeapObj::Map { keys, .. } => keys.iter().filter(|key| !key.is_hole()).count(),
            HeapObj::Set(items) => items.iter().filter(|value| !value.is_hole()).count(),
            _ => 0,
        }
    }

    /// Position of the live entry whose key/value is SameValueZero-equal to
    /// `key` in collection `idx` (Map/WeakMap keys; Set/WeakSet items):
    /// linear below the threshold, through the lazy hash index at/past it
    /// (building it on first use). This is THE lookup every Map/Set/WeakMap/
    /// WeakSet get/has/set/add/delete/upsert routes through.
    pub(crate) fn coll_find(&mut self, idx: u32, key: Value) -> Option<usize> {
        // Flatten a rope key up front (skip when HotMirror proves non-Cons:
        // non-empty flat Str, shaped Object, Func/Closure, Class instance).
        if key.is_heap() {
            let kidx = key.heap_index();
            #[cfg(all(feature = "jit", not(feature = "safe-sandbox")))]
            let may_be_cons = {
                let mir = self.heap.hot_mirror_of(kidx);
                mir.shape == crate::shape::DICT && mir.fid == crate::heap::FID_MIRROR_NONE
            };
            #[cfg(not(all(feature = "jit", not(feature = "safe-sandbox"))))]
            let may_be_cons = true;
            if may_be_cons {
                self.heap.flatten(kidx);
            }
        }
        let heap = &self.heap;
        let keys: &[Value] = match heap.get(idx) {
            HeapObj::Map { keys, .. } | HeapObj::WeakMap { keys, .. } => keys,
            HeapObj::Set(items) | HeapObj::WeakSet(items) => items,
            _ => return None,
        };
        // Tiny collections (e.g. small route maps) are faster to scan directly
        // than probing `collection_index` — and `keys` is always authoritative.
        if keys.len() <= 8 {
            return svz_position(heap, keys, key);
        }
        if let Some(ix) = self.collection_index.get(&idx) {
            return ix.find(heap, keys, key);
        }
        // A user key can never be HOLE (its bit pattern is engine-internal),
        // so tombstoned slots never match — same as the pre-index scans.
        if keys.len() < INDEX_THRESHOLD {
            return svz_position(heap, keys, key);
        }
        // Crossed the threshold: build the index once; maintained from now on.
        let ix = CollIndex::build(heap, keys);
        let found = ix.find(heap, keys, key);
        self.collection_index.insert(idx, ix);
        found
    }

    /// Fast WeakMap / WeakSet key lookup: keys can only be heap objects or
    /// non-registered symbols, so non-heap probes immediately return `None`,
    /// no rope flattening is needed, and lookups compare raw `Value` bits
    /// directly without touching `heap.objs[key_idx]`.
    #[inline]
    pub(crate) fn coll_find_weak(&mut self, idx: u32, key: Value) -> Option<usize> {
        if !key.is_heap() || !self.heap.is_weak_key(key.heap_index()) {
            return None;
        }
        let Vm {
            heap,
            collection_index,
            ..
        } = self;
        let keys: &[Value] = match heap.get(idx) {
            HeapObj::WeakMap { keys, .. } => keys,
            HeapObj::WeakSet(items) => items,
            _ => return None,
        };
        let bits = key.bits();
        if keys.len() <= 8 {
            return keys.iter().position(|&k| k.bits() == bits);
        }
        if let Some(ix) = collection_index.get_mut(&idx) {
            #[cfg(not(feature = "safe-sandbox"))]
            {
                ix.sync_identity(keys);
                return ix.find_identity(keys, key);
            }
            #[cfg(feature = "safe-sandbox")]
            return ix.find(heap, keys, key);
        }
        if keys.len() < INDEX_THRESHOLD {
            return keys.iter().position(|&k| k.bits() == bits);
        }
        #[cfg(not(feature = "safe-sandbox"))]
        let ix = CollIndex::build_identity(keys);
        #[cfg(feature = "safe-sandbox")]
        let ix = CollIndex::build(heap, keys);
        #[cfg(not(feature = "safe-sandbox"))]
        let found = ix.find_identity(keys, key);
        #[cfg(feature = "safe-sandbox")]
        let found = ix.find(heap, keys, key);
        collection_index.insert(idx, ix);
        found
    }

    /// Fused WeakMap.get / WeakMap.has / WeakSet.has lookup for proven JIT lanes:
    /// reads `keys` and `vals` in a single `heap.get(idx)` with zero `heap.objs[key_idx]`
    /// probes. `op`: 0 = WeakMap.get, 1 = WeakMap.has, 2 = WeakSet.has.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn weak_lookup_proven_fast(&mut self, idx: u32, key: Value, op: u64) -> u64 {
        if !key.is_heap() || !self.heap.is_weak_key(key.heap_index()) {
            return if op == 0 { Value::UNDEFINED.bits() } else { Value::FALSE.bits() };
        }
        let Vm {
            heap,
            collection_index,
            ..
        } = self;
        let (keys, vals): (&[Value], Option<&[Value]>) = match heap.get(idx) {
            HeapObj::WeakMap { keys, vals } => (keys.as_slice(), Some(vals.as_slice())),
            HeapObj::WeakSet(items) => (items.as_slice(), None),
            _ => return if op == 0 { Value::UNDEFINED.bits() } else { Value::FALSE.bits() },
        };
        let bits = key.bits();
        let found = if keys.len() <= 8 {
            keys.iter().position(|&k| k.bits() == bits)
        } else if let Some(ix) = collection_index.get_mut(&idx) {
            #[cfg(not(feature = "safe-sandbox"))]
            {
                ix.sync_identity(keys);
                ix.find_identity(keys, key)
            }
            #[cfg(feature = "safe-sandbox")]
            {
                ix.find(heap, keys, key)
            }
        } else if keys.len() < INDEX_THRESHOLD {
            keys.iter().position(|&k| k.bits() == bits)
        } else {
            #[cfg(not(feature = "safe-sandbox"))]
            let ix = CollIndex::build_identity(keys);
            #[cfg(feature = "safe-sandbox")]
            let ix = CollIndex::build(heap, keys);
            #[cfg(not(feature = "safe-sandbox"))]
            let f = ix.find_identity(keys, key);
            #[cfg(feature = "safe-sandbox")]
            let f = ix.find(heap, keys, key);
            collection_index.insert(idx, ix);
            f
        };
        match op {
            0 => match (found, vals) {
                (Some(i), Some(vs)) => vs[i].bits(),
                _ => Value::UNDEFINED.bits(),
            },
            _ => Value::bool(found.is_some()).bits(),
        }
    }

    /// Fused `WeakMap.prototype.set(key, val)` for proven JIT lanes where
    /// `idx` is already verified `HeapObj::WeakMap` and `key` is verified
    /// `can_be_held_weakly`. Avoids string dispatch and duplicate checks.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn weak_set_proven_fast(
        &mut self,
        idx: u32,
        this: Value,
        key: Value,
        val: Value,
    ) -> u64 {
        #[cfg(not(feature = "safe-sandbox"))]
        {
            let kidx = key.heap_index();
            let Vm {
                heap,
                collection_index,
                weak_young_entries,
                ..
            } = self;
            // Fresh key never inserted into any WeakMap/WeakSet: guaranteed
            // absent, so append sequentially without probing or touching
            // `collection_index` (eliminates L3 hash-table misses on
            // `DOMEventTarget` construction).
            if !heap.is_weak_key(kidx) {
                heap.set_weak_key(kidx);
                let mut compacted = false;
                let pos = if let HeapObj::WeakMap { keys, vals } = heap.get_mut(idx) {
                    if keys.len() == keys.capacity()
                        && keys.len() >= 64
                        && !collection_index.contains_key(&idx)
                    {
                        let mut write = 0usize;
                        for read in 0..keys.len() {
                            if !keys[read].is_hole() {
                                if write != read {
                                    keys[write] = keys[read];
                                    vals[write] = vals[read];
                                }
                                write += 1;
                            }
                        }
                        if write != keys.len() {
                            keys.truncate(write);
                            vals.truncate(write);
                            compacted = true;
                        }
                    }
                    let p = keys.len();
                    keys.push(key);
                    vals.push(val);
                    p
                } else {
                    return this.bits();
                };
                if compacted {
                    weak_young_entries.push((idx, usize::MAX));
                } else if heap.is_young(kidx)
                    || (val.is_heap() && heap.is_young(val.heap_index()))
                {
                    weak_young_entries.push((idx, pos));
                }
                return this.bits();
            }
            // Key has been in at least one WeakMap/WeakSet: ensure the identity
            // index exists (for `keys.len() >= INDEX_THRESHOLD`) and is synced.
            if !collection_index.contains_key(&idx) {
                if let HeapObj::WeakMap { keys, .. } = heap.get(idx) {
                    if keys.len() >= INDEX_THRESHOLD {
                        collection_index.insert(idx, CollIndex::build_identity(keys));
                    }
                }
            }
            if let Some(ix) = collection_index.get_mut(&idx) {
                if let HeapObj::WeakMap { keys, .. } = heap.get(idx) {
                    ix.sync_identity(keys);
                }
                let mut i = CollIndex::tag(key.bits()) as usize & ix.mask;
                let mut first_tomb: Option<usize> = None;
                let found_or_slot = loop {
                    let (st, sm) = ix.table[i];
                    if sm == META_EMPTY {
                        break Err(first_tomb.unwrap_or(i));
                    }
                    if sm == META_TOMB {
                        if first_tomb.is_none() {
                            first_tomb = Some(i);
                        }
                    } else if st == kidx {
                        break Ok(sm as usize);
                    }
                    i = (i + 1) & ix.mask;
                };
                match found_or_slot {
                    Ok(pos) => {
                        if let HeapObj::WeakMap { vals, .. } = heap.get_mut(idx) {
                            vals[pos] = val;
                        }
                        if heap.is_young(kidx) || (val.is_heap() && heap.is_young(val.heap_index())) {
                            weak_young_entries.push((idx, pos));
                        }
                    }
                    Err(insert_slot) => {
                        let pos = if let HeapObj::WeakMap { keys, vals } = heap.get_mut(idx) {
                            let p = keys.len();
                            keys.push(key);
                            vals.push(val);
                            p
                        } else {
                            return this.bits();
                        };
                        let was_empty = ix.table[insert_slot].1 == META_EMPTY;
                        if was_empty && (ix.used + 1) * 2 >= ix.table.len() {
                            ix.grow();
                            ix.insert_identity_raw(kidx, pos as u32);
                        } else {
                            ix.table[insert_slot] = (kidx, pos as u32);
                            if was_empty {
                                ix.used += 1;
                            }
                        }
                        ix.live += 1;
                        ix.indexed_len = pos + 1;
                        if heap.is_young(kidx) || (val.is_heap() && heap.is_young(val.heap_index())) {
                            weak_young_entries.push((idx, pos));
                        }
                    }
                }
                return this.bits();
            }
            // Small unindexed WeakMap (`keys.len() < INDEX_THRESHOLD`): scan,
            // mutate, and lazily build the identity index in a single borrow.
            let bits = key.bits();
            let mut touched_pos = None;
            let mut build_ix = None;
            if let HeapObj::WeakMap { keys, vals } = heap.get_mut(idx) {
                if let Some(pos) = keys.iter().position(|&k| k.bits() == bits) {
                    vals[pos] = val;
                    touched_pos = Some(pos);
                } else {
                    let pos = keys.len();
                    keys.push(key);
                    vals.push(val);
                    touched_pos = Some(pos);
                    if keys.len() >= INDEX_THRESHOLD {
                        build_ix = Some(CollIndex::build_identity(keys));
                    }
                }
            }
            if let Some(ix) = build_ix {
                collection_index.insert(idx, ix);
            }
            if let Some(p) = touched_pos {
                if heap.is_young(kidx) || (val.is_heap() && heap.is_young(val.heap_index())) {
                    weak_young_entries.push((idx, p));
                }
            }
            return this.bits();
        }
        #[cfg(feature = "safe-sandbox")]
        {
            let pos = self.coll_find_weak(idx, key);
            let mut pushed = None;
            let mut touched_pos = None;
            if let HeapObj::WeakMap { keys, vals } = self.heap.get_mut(idx) {
                match pos {
                    Some(i) => {
                        vals[i] = val;
                        touched_pos = Some(i);
                    }
                    None => {
                        let p = keys.len();
                        pushed = Some(p);
                        touched_pos = Some(p);
                        keys.push(key);
                        vals.push(val);
                    }
                }
            }
            if pushed.is_some() {
                self.heap.set_weak_key(key.heap_index());
            }
            if let Some(p) = touched_pos {
                self.weak_young_entries.push((idx, p));
            }
            if let Some(p) = pushed {
                self.weak_coll_index_insert(idx, key, p);
            }
            this.bits()
        }
    }

    /// Fused Map.get / Map.has / Set.has lookup for proven JIT collection lanes:
    /// reads `keys` and `vals` in a single `heap.get(idx)` without re-fetching
    /// `HeapObj::Map` after `coll_find`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn coll_lookup_proven_fast(&mut self, idx: u32, key: Value, op: u64) -> u64 {
        if key.is_heap() {
            let kidx = key.heap_index();
            #[cfg(not(feature = "safe-sandbox"))]
            let may_be_cons = {
                let mir = self.heap.hot_mirror_of(kidx);
                mir.shape == crate::shape::DICT && mir.fid == crate::heap::FID_MIRROR_NONE
            };
            #[cfg(feature = "safe-sandbox")]
            let may_be_cons = true;
            if may_be_cons {
                self.heap.flatten(kidx);
            }
        }
        let heap = &self.heap;
        let (keys, vals): (&[Value], Option<&[Value]>) = match heap.get(idx) {
            HeapObj::Map { keys, vals } => (keys.as_slice(), Some(vals.as_slice())),
            HeapObj::Set(items) => (items.as_slice(), None),
            _ => return if op == 0 { Value::UNDEFINED.bits() } else { Value::FALSE.bits() },
        };
        let found = if keys.len() <= 8 {
            svz_position(heap, keys, key)
        } else if let Some(ix) = self.collection_index.get(&idx) {
            ix.find(heap, keys, key)
        } else if keys.len() < INDEX_THRESHOLD {
            svz_position(heap, keys, key)
        } else {
            let ix = CollIndex::build(heap, keys);
            let f = ix.find(heap, keys, key);
            self.collection_index.insert(idx, ix);
            f
        };
        match op {
            0 => match (found, vals) {
                (Some(i), Some(vs)) => vs[i].bits(),
                _ => Value::UNDEFINED.bits(),
            },
            _ => Value::bool(found.is_some()).bits(),
        }
    }

    /// Give the `Map` `idx` its index now when it has at least 8 slots and
    /// none yet, rather than at [`INDEX_THRESHOLD`] (the index is pure
    /// acceleration: every hit is confirmed against the backing, which stays
    /// authoritative). For a Map read far more often than it changes and
    /// often probed for keys it lacks: the Python frontend's module globals,
    /// asked for every builtin name before the builtins are.
    #[cfg(feature = "python")]
    pub(crate) fn coll_index_early(&mut self, idx: u32) {
        if self.collection_index.contains_key(&idx) {
            return;
        }
        if let HeapObj::Map { keys, .. } = self.heap.get(idx) {
            if keys.len() >= 8 {
                let ix = CollIndex::build(&self.heap, keys);
                self.collection_index.insert(idx, ix);
            }
        }
    }

    /// Record `key` newly PUSHED at slot `pos` of collection `idx`. No-op
    /// while the collection has no index (it is built lazily by `coll_find`,
    /// which every insertion path calls first).
    pub(crate) fn coll_index_insert(&mut self, idx: u32, key: Value, pos: usize) {
        // Nursery barrier + B6 oracle: docs/archive/NURSERY_DESIGN.md §1 case 6 —
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

    /// Single-probe find-or-insert for `Map.prototype.set(key, val)`:
    /// computes `svz_repr`, looks up `collection_index`, and walks the probe
    /// chain ONCE instead of twice on every new-key insertion.
    #[cfg(not(feature = "safe-sandbox"))]
    pub(crate) fn map_set_fast(&mut self, idx: u32, key: Value, val: Value) {
        if key.is_heap() {
            self.heap.flatten(key.heap_index());
        }
        // Nursery barrier for the VALUE (both update and insert paths).
        self.heap.write_barrier_val(idx, val);
        let indexed_outcome: Option<bool> = {
            let Vm {
                heap,
                collection_index,
                ..
            } = self;
            if let Some(ix) = collection_index.get_mut(&idx) {
                let tag = ix.key_tag(heap, key);
                let found_or_slot = {
                    let HeapObj::Map { keys, .. } = heap.get(idx) else {
                        return;
                    };
                    let mut i = tag as usize & ix.mask;
                    let mut first_tomb: Option<usize> = None;
                    loop {
                        let (st, sm) = ix.table[i];
                        if sm == META_EMPTY {
                            break Err(first_tomb.unwrap_or(i));
                        }
                        if sm == META_TOMB {
                            if first_tomb.is_none() {
                                first_tomb = Some(i);
                            }
                        } else if st == tag && svz_eq(heap, keys[sm as usize], key) {
                            break Ok(sm as usize);
                        }
                        i = (i + 1) & ix.mask;
                    }
                };
                match found_or_slot {
                    Ok(pos) => {
                        if let HeapObj::Map { vals, .. } = heap.get_mut(idx) {
                            vals[pos] = val;
                        }
                        Some(false)
                    }
                    Err(insert_slot) => {
                        let pos = if let HeapObj::Map { keys, vals } = heap.get_mut(idx) {
                            let p = keys.len();
                            keys.push(key);
                            vals.push(val);
                            p as u32
                        } else {
                            return;
                        };
                        let was_empty = ix.table[insert_slot].1 == META_EMPTY;
                        if was_empty && (ix.used + 1) * 2 >= ix.table.len() {
                            ix.grow();
                            ix.insert_raw(tag, pos);
                        } else {
                            ix.table[insert_slot] = (tag, pos);
                            if was_empty {
                                ix.used += 1;
                            }
                        }
                        ix.live += 1;
                        Some(true)
                    }
                }
            } else {
                None
            }
        };
        if let Some(inserted) = indexed_outcome {
            self.heap.version_step(idx);
            if inserted {
                self.store_barrier(crate::heap::gcoracle::COLL_INSERT, idx, key);
            }
            return;
        }
        // Small / not-yet-indexed Map: standard path (builds index once threshold crossed).
        let pos = self.coll_find(idx, key);
        let mut pushed = None;
        if let HeapObj::Map { keys, vals } = self.heap.get_mut(idx) {
            match pos {
                Some(i) => vals[i] = val,
                None => {
                    pushed = Some(keys.len());
                    keys.push(key);
                    vals.push(val);
                }
            }
        }
        self.heap.version_step(idx);
        if let Some(p) = pushed {
            self.coll_index_insert(idx, key, p);
        }
    }

    /// Single-probe find-or-insert for `Set.prototype.add(val)`:
    /// computes the hash tag and walks the probe chain ONCE instead of twice
    /// (`coll_find` + `coll_index_insert`) on every new insertion.
    #[cfg(not(feature = "safe-sandbox"))]
    pub(crate) fn set_add_fast(&mut self, idx: u32, val: Value) {
        if val.is_heap() {
            self.heap.flatten(val.heap_index());
        }
        let indexed_outcome: Option<bool> = {
            let Vm {
                heap,
                collection_index,
                ..
            } = self;
            if let Some(ix) = collection_index.get_mut(&idx) {
                let tag = ix.key_tag(heap, val);
                let found_or_slot = {
                    let HeapObj::Set(items) = heap.get(idx) else {
                        return;
                    };
                    let mut i = tag as usize & ix.mask;
                    let mut first_tomb: Option<usize> = None;
                    loop {
                        let (st, sm) = ix.table[i];
                        if sm == META_EMPTY {
                            break Err(first_tomb.unwrap_or(i));
                        }
                        if sm == META_TOMB {
                            if first_tomb.is_none() {
                                first_tomb = Some(i);
                            }
                        } else if st == tag && svz_eq(heap, items[sm as usize], val) {
                            break Ok(sm as usize);
                        }
                        i = (i + 1) & ix.mask;
                    }
                };
                match found_or_slot {
                    Ok(_) => Some(false),
                    Err(insert_slot) => {
                        let pos = if let HeapObj::Set(items) = heap.get_mut(idx) {
                            let p = items.len();
                            items.push(val);
                            p as u32
                        } else {
                            return;
                        };
                        let was_empty = ix.table[insert_slot].1 == META_EMPTY;
                        if was_empty && (ix.used + 1) * 2 >= ix.table.len() {
                            ix.grow();
                            ix.insert_raw(tag, pos);
                        } else {
                            ix.table[insert_slot] = (tag, pos);
                            if was_empty {
                                ix.used += 1;
                            }
                        }
                        ix.live += 1;
                        Some(true)
                    }
                }
            } else {
                None
            }
        };
        if let Some(inserted) = indexed_outcome {
            if inserted {
                self.store_barrier(crate::heap::gcoracle::COLL_INSERT, idx, val);
            }
            return;
        }
        if self.coll_find(idx, val).is_none() {
            let mut pushed = None;
            if let HeapObj::Set(items) = self.heap.get_mut(idx) {
                pushed = Some(items.len());
                items.push(val);
            }
            if let Some(p) = pushed {
                self.coll_index_insert(idx, val, p);
            }
        }
    }

    /// Index maintenance for WeakMap/WeakSet. The key is deliberately not sent
    /// through the ordinary collection insert barrier: recording it as a young
    /// value would turn the weak edge into a strong nursery root for the epoch.
    pub(crate) fn weak_coll_index_insert(&mut self, idx: u32, key: Value, pos: usize) {
        let Vm {
            heap,
            collection_index,
            ..
        } = self;
        if let Some(ix) = collection_index.get_mut(&idx) {
            #[cfg(not(feature = "safe-sandbox"))]
            {
                let _ = heap;
                if ix.indexed_len == pos {
                    ix.insert_identity(key, pos as u32);
                    ix.indexed_len = pos + 1;
                }
            }
            #[cfg(feature = "safe-sandbox")]
            ix.insert(heap, key, pos as u32);
        }
    }

    /// Identity-only index removal for WeakMap/WeakSet keys (no rope flatten or
    /// `heap.objs` lookup needed).
    pub(crate) fn weak_coll_index_remove(&mut self, idx: u32, key: Value, pos: usize) {
        let Vm {
            heap,
            collection_index,
            ..
        } = self;
        if let Some(ix) = collection_index.get_mut(&idx) {
            #[cfg(not(feature = "safe-sandbox"))]
            {
                let _ = heap;
                ix.remove_identity(key, pos as u32);
            }
            #[cfg(feature = "safe-sandbox")]
            ix.remove(heap, key, pos as u32);
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

    /// Reset a Map/Set index after `clear()`. The backing slots stay as
    /// tombstones for live iterator cursors, but none can match a later lookup.
    /// Keeping an empty index once the backing is large avoids rebuilding over
    /// every old tombstone after each clear/add cycle.
    pub(crate) fn coll_index_clear(&mut self, idx: u32, backing_len: usize) {
        if backing_len >= INDEX_THRESHOLD || self.collection_index.contains_key(&idx) {
            self.collection_index.insert(idx, CollIndex::with_capacity(0));
        } else {
            self.collection_index.remove(&idx);
        }
    }

    /// Drop collection `idx`'s index entirely. The correct-by-default escape
    /// hatch for a mutation that replaces a collection instance slot. Absent =
    /// linear; the index rebuilds lazily on the next indexed lookup.
    pub(crate) fn coll_index_invalidate(&mut self, idx: u32) {
        self.collection_index.remove(&idx);
    }

    /// Delete the entry at `pos` (its key `key`) of the WeakMap/WeakSet `idx`.
    /// An indexed collection tombstones the slot and drops just that key from
    /// its index, as a Map does; shifting the later slots instead meant
    /// dropping the index and rebuilding it over every entry at the next
    /// lookup, so alternating `delete` and `get` cost O(n) each. A small
    /// unindexed collection (under [`INDEX_THRESHOLD`] slots) still shifts.
    /// A weak collection is never iterated, so its tombstones need not keep
    /// their places: [`Vm::weak_coll_compact_if_sparse`] drops them in bulk.
    pub(crate) fn weak_coll_delete(&mut self, idx: u32, pos: usize, key: Value) {
        if !self.collection_index.contains_key(&idx) {
            match self.heap.get_mut(idx) {
                HeapObj::WeakMap { keys, vals } => {
                    keys.remove(pos);
                    vals.remove(pos);
                }
                HeapObj::WeakSet(items) => {
                    items.remove(pos);
                }
                _ => {}
            }
            // Positions shifted; mark container for full scan on the next minor.
            self.weak_young_entries.push((idx, usize::MAX));
            return;
        }
        match self.heap.get_mut(idx) {
            HeapObj::WeakMap { keys, vals } => {
                keys[pos] = Value::HOLE;
                vals[pos] = Value::UNDEFINED;
            }
            HeapObj::WeakSet(items) => items[pos] = Value::HOLE,
            _ => return,
        }
        self.weak_coll_index_remove(idx, key, pos);
        self.weak_coll_compact_if_sparse(idx);
    }

    /// Drop an indexed WeakMap/WeakSet's tombstones once they outnumber its
    /// live entries (and the index threshold), rebuilding the index over the
    /// survivors. Each compaction follows at least as many deletions as it
    /// moves entries, so a delete costs O(1) amortized.
    pub(crate) fn weak_coll_compact_if_sparse(&mut self, idx: u32) {
        let len = match self.heap.get(idx) {
            HeapObj::WeakMap { keys, .. } => keys.len(),
            HeapObj::WeakSet(items) => items.len(),
            _ => return,
        };
        let Some(live) = self.collection_index.get(&idx).map(|ix| ix.live_total(len)) else {
            return;
        };
        if len - live <= live.max(INDEX_THRESHOLD) {
            return;
        }
        match self.heap.get_mut(idx) {
            HeapObj::WeakMap { keys, vals } => {
                let mut write = 0;
                for read in 0..keys.len() {
                    if !keys[read].is_hole() {
                        keys[write] = keys[read];
                        vals[write] = vals[read];
                        write += 1;
                    }
                }
                keys.truncate(write);
                vals.truncate(write);
            }
            HeapObj::WeakSet(items) => items.retain(|v| !v.is_hole()),
            _ => return,
        }
        let rebuilt = match self.heap.get(idx) {
            HeapObj::WeakMap { keys, .. } => {
                #[cfg(not(feature = "safe-sandbox"))]
                {
                    CollIndex::build_identity(keys)
                }
                #[cfg(feature = "safe-sandbox")]
                {
                    CollIndex::build(&self.heap, keys)
                }
            }
            HeapObj::WeakSet(items) => {
                #[cfg(not(feature = "safe-sandbox"))]
                {
                    CollIndex::build_identity(items)
                }
                #[cfg(feature = "safe-sandbox")]
                {
                    CollIndex::build(&self.heap, items)
                }
            }
            _ => return,
        };
        self.collection_index.insert(idx, rebuilt);
        // Compaction shifted surviving positions; request a full scan on the next minor.
        self.weak_young_entries.push((idx, usize::MAX));
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
        assert_eq!(index.live, keys.len());
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

    /// Reprs that the seeded mix sends to one bucket of 16 can be found in
    /// a few hundred tries: a chain takes `CHAIN_MAX` of them and refuses the
    /// next, which is what sends an index to the tree.
    #[test]
    fn a_chain_refuses_an_entry_past_its_limit() {
        let mut chains = Chains::with_buckets(16);
        let target = chains.bucket(0);
        let colliding: Vec<u64> = (0u64..)
            .filter(|&repr| chains.bucket(repr) == target)
            .take(CHAIN_MAX + 1)
            .collect();
        for (slot, &repr) in colliding[..CHAIN_MAX].iter().enumerate() {
            // `live` stays below the growth point: the buckets do not double.
            assert!(chains.insert(repr, Value::int(slot as i32), slot as u32, 0));
        }
        assert!(!chains.insert(colliding[CHAIN_MAX], Value::UNDEFINED, 99, 0));
        // A removal makes room again.
        assert!(chains.remove(colliding[3], 3));
        assert!(chains.insert(colliding[CHAIN_MAX], Value::UNDEFINED, 99, 0));
    }

    /// An index moved to the tree keeps every entry, and serves finds,
    /// removals and later inserts from there.
    #[test]
    fn an_index_moved_to_the_tree_keeps_its_entries() {
        let mut heap = Heap::new();
        let mut keys: Vec<Value> = (0..300)
            .map(|i| {
                if i % 2 == 0 {
                    Value::heap(heap.alloc_str(format!("moved-key-{i}")))
                } else {
                    Value::int(i)
                }
            })
            .collect();
        let mut index = CollIndex::build(&heap, &keys);
        for (pos, &key) in keys.iter().enumerate().step_by(5) {
            index.remove(&heap, key, pos as u32);
        }
        index.to_tree(&heap);
        assert!(index.chains.is_none());
        assert_eq!(index.tree.len(), index.live);
        for (pos, &key) in keys.iter().enumerate() {
            assert_eq!(index.find(&heap, &keys, key), (pos % 5 != 0).then_some(pos));
        }
        let late = Value::heap(heap.alloc_str("late-key".to_string()));
        index.insert(&heap, late, keys.len() as u32);
        keys.push(late);
        assert_eq!(index.find(&heap, &keys, late), Some(keys.len() - 1));
        index.remove(&heap, keys[1], 1);
        assert_eq!(index.find(&heap, &keys, keys[1]), None);
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

#[cfg(all(test, not(feature = "safe-sandbox")))]
mod seeded_index_tests {
    use super::*;
    use std::collections::HashSet;

    /// The unkeyed FNV-1a/splitmix tag every VM used before the per-process
    /// seed: what an attacker can precompute offline.
    fn public_tag(key: &str) -> u32 {
        let mut h = 0xCBF2_9CE4_8422_2325u64;
        for &b in key.as_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3);
        }
        h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h = (h ^ (h >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((h ^ (h >> 31)) >> 32) as u32
    }

    #[test]
    fn a_precomputed_bucket_collision_family_spreads_under_the_process_seed() {
        // 256 keys that all share bucket 0 of a 1,024-bucket table under the
        // public mapping — a probe chain 256 long in every VM, before.
        let mut keys = Vec::new();
        let mut candidate = 0u64;
        while keys.len() < 256 {
            let key = format!("h{candidate:09}");
            if public_tag(&key) & 1_023 == 0 {
                keys.push(key);
            }
            candidate += 1;
        }
        let mut heap = Heap::new();
        let vals: Vec<Value> = keys
            .iter()
            .map(|k| Value::heap(heap.alloc_str(k.clone())))
            .collect();
        let mut index = CollIndex::with_capacity(500);
        assert_eq!(index.mask, 1_023);
        let buckets: HashSet<usize> = vals
            .iter()
            .map(|&v| index.key_tag(&heap, v) as usize & index.mask)
            .collect();
        assert!(buckets.len() > keys.len() / 2, "Map/Set buckets: {}", buckets.len());
        let tags: HashSet<u32> = keys
            .iter()
            .map(|k| crate::heap::prop_tag_of(k) & 1_023)
            .collect();
        assert!(tags.len() > keys.len() / 2, "property buckets: {}", tags.len());
        // Still an exact index: every key found at its slot.
        for (pos, &v) in vals.iter().enumerate() {
            index.insert(&heap, v, pos as u32);
        }
        for (pos, &v) in vals.iter().enumerate() {
            assert_eq!(index.find(&heap, &vals, v), Some(pos));
        }
    }
}
