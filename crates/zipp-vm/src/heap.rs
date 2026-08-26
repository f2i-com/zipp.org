//! Heap object storage.
//!
//! Heap values are referenced by a `u32` index packed into a [`crate::value::Value`].
//! Reference semantics fall out naturally: copying a `Value` copies the index,
//! so `let b = a` makes `a` and `b` alias the same heap slot, and a mutation
//! through either is visible through both — exactly JS object/array semantics.
//!
//! v1 does not reclaim memory (programs are short-lived per `eval`); a real GC
//! slots in here later without touching the value representation. Objects use a
//! simple insertion-ordered property list, which preserves JS string-key
//! enumeration order and is correct (if not yet fast — shapes/inline-caches are
//! a later tier).

use crate::bytecode::{static_key_plans_enabled, StaticKeyPlan};
use crate::value::Value;
use std::borrow::Cow;
use std::cell::Cell;
#[cfg(not(feature = "safe-sandbox"))]
use std::cell::UnsafeCell;
#[cfg(feature = "safe-sandbox")]
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

#[inline]
fn vec_capacity_bytes<T>(v: &Vec<T>) -> usize {
    v.capacity().saturating_mul(std::mem::size_of::<T>())
}

fn string_vec_payload(v: &Vec<String>) -> usize {
    vec_capacity_bytes(v)
        .saturating_add(v.iter().fold(0usize, |n, s| n.saturating_add(s.capacity())))
}

fn named_value_vec_payload(v: &Vec<(String, Value)>) -> usize {
    vec_capacity_bytes(v).saturating_add(
        v.iter()
            .fold(0usize, |n, (s, _)| n.saturating_add(s.capacity())),
    )
}

/// Number of keys at which an [`ObjMap`] builds its hash [`PropIndex`].
/// Measured on 2M-op probes (interpreter): LOOKUPS through the index win
/// from ~4 keys up (n=12: -17%, n=64: -42%), but mass CONSTRUCTION
/// (allocate-read-once, the JSON.parse shape) pays the per-object build —
/// +40% at 2 keys, +5% at 8-12, break-even at 16, a WIN from ~24 (the
/// linear pos() per set() turns quadratic). 12 balances the two: small
/// literals never pay, read-heavy/dictionary maps index early, worst-case
/// mass-build loss is ~5% in the narrow 12-15 band. Maintained on append
/// and on remove (backward-shift delete + flat slot sweep — no key bytes
/// re-hashed); dropped only when the map shrinks to half the threshold,
/// the hysteresis keeping a map oscillating at the boundary from
/// rebuilding every time it crosses.
pub const PROP_INDEX_THRESHOLD: usize = 12;

/// Slot sentinel for an empty [`PropIndex`] bucket (a real slot is a `keys`
/// position, far below `u32::MAX`).
const PROP_EMPTY: u32 = u32::MAX;

/// The table hash of a property key: FNV-1a over the bytes, finished with a
/// splitmix64-style avalanche; the HIGH half is the stored tag and also picks
/// the bucket (`tag & mask`), so growth rehashes straight from stored tags —
/// no key re-reads (same scheme as vm/collections.rs' CollIndex).
#[inline]
fn prop_tag(key: &str) -> u32 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for &b in key.as_bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3);
    }
    h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((h ^ (h >> 31)) >> 32) as u32
}

/// Hash index over an [`ObjMap`]'s `keys`: a flat open-addressing table of
/// `(tag, slot)` pairs with linear probing (the CollIndex shape — 8-byte
/// entries pack eight to a cache line, so a lookup is typically one random
/// line plus one confirming key read). A tag hit is always CONFIRMED with a
/// real compare against `keys[slot]` (the 32-bit tag is lossy). The index is
/// pure acceleration: the `keys`/`vals`/`attrs` Vecs stay authoritative
/// (insertion order — for-in/Object.keys — is untouched), it holds no
/// `Value`s (nothing for the GC to trace), and `None` = linear scan
/// (correct by default). No tombstones: a removal backward-shift-deletes
/// its table entry (probe chains stay contiguous) and then decrements
/// every stored slot above the removed one in a flat integer sweep — the
/// owning map's `Vec::remove` shifted those positions down. Both passes
/// touch only this table; no key is re-hashed (see [`PropIndex::remove_slot`]).
/// W19 M1 — the SPLIT representation latch. `ZIPP_NO_SPLIT_PROPINDEX=1` builds
/// every new index in the pre-wave INTERLEAVED layout, which is retained
/// verbatim as `PropIndex::Inter` — so OFF is not an alternate code path but
/// literally the old data structure and the old loops.
///
/// Read ONCE per index construction (`with_capacity`), never per operation: the
/// variant is fixed for the life of an index, and `grow` preserves it.
#[cfg_attr(feature = "safe-sandbox", allow(dead_code))]
fn split_propindex_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_SPLIT_PROPINDEX").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// The two physical layouts of the table. `Inter` is the pre-W19 one, kept so
/// `ZIPP_NO_SPLIT_PROPINDEX=1` restores the old memory layout exactly (a
/// data-layout change has no "code path" to switch off). `Split` is W19 M1.
///
/// WHY SPLIT. `remove_slot` ends in a sweep over EVERY entry, decrementing the
/// stored slots above the removed one. Over an interleaved `Vec<(u32, u32)>`
/// that writes 4 bytes out of every 8 — a strided store LLVM cannot vectorize —
/// and its cost tracks table CAPACITY, not the live key count and not the shift
/// distance: 28.3 ns at cap 128, 104 ns at cap 512 (measured). Over a
/// contiguous `Vec<u32>` the identical predicate vectorizes to 7.4 ns at cap 128
/// and 11.2 ns at cap 512. That sweep is `polymorphic-objects`' single largest
/// interpreter cost: 900k deletes against a 60-key (capacity 128) object.
///
/// A BRANCHLESS-BUT-INTERLEAVED rewrite was measured and is WORSE (28.3 → 30.6
/// ns): the branch predictor was already handling the test, and the strided
/// 4-of-8-byte store is the whole bottleneck. Do not ship that instead.
///
/// The read path is unchanged in cost (interleaved 8.97 ns/find vs split 8.80,
/// 20M lookups over the row's real 60-key / capacity-128 shape): a probe reads
/// `slots[i]` for the empty test and `tags[i]` only to screen a candidate, and
/// the two arrays together are the same bytes as the one they replace but split
/// so the hot sweep touches only half of them.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "safe-sandbox", allow(dead_code))]
pub enum PropIndex {
    /// Hostile-code profile: exact string ordering gives a deterministic
    /// logarithmic lookup bound even when wasm32 cannot supply secret hash
    /// entropy. Keys are cloned only after the map crosses the index threshold;
    /// the authoritative insertion-ordered vectors remain unchanged.
    #[cfg(feature = "safe-sandbox")]
    Tree { entries: BTreeMap<String, u32> },
    /// Pre-W19: one flat `Vec<(tag, slot)>`.
    Inter {
        table: Vec<(u32, u32)>,
        mask: usize,
        len: usize,
    },
    /// W19 M1: parallel `tags` / `slots`, same length, same bucket index.
    /// INVARIANT: `tags.len() == slots.len() == mask + 1`, a bucket is occupied
    /// iff `slots[i] != PROP_EMPTY`, and `tags[i]` is meaningful only then.
    Split {
        tags: Vec<u32>,
        slots: Vec<u32>,
        mask: usize,
        len: usize,
    },
}

impl PropIndex {
    fn with_capacity(n: usize) -> PropIndex {
        #[cfg(feature = "safe-sandbox")]
        {
            let _ = n;
            return PropIndex::Tree {
                entries: BTreeMap::new(),
            };
        }
        #[cfg(not(feature = "safe-sandbox"))]
        {
            // Capacity for `n` entries at < 3/4 load, minimum 32, power of two.
            let cap = (n * 4 / 3 + 1).next_power_of_two().max(32);
            Self::with_capacity_kind(cap, split_propindex_enabled())
        }
    }

    /// `with_capacity` with the layout NAMED rather than latched — the seam the
    /// differential test drives both representations through in one process.
    #[cfg_attr(feature = "safe-sandbox", allow(dead_code))]
    fn with_capacity_kind(cap: usize, split: bool) -> PropIndex {
        debug_assert!(cap.is_power_of_two() && cap >= 32);
        if split {
            PropIndex::Split {
                tags: vec![0; cap],
                slots: vec![PROP_EMPTY; cap],
                mask: cap - 1,
                len: 0,
            }
        } else {
            PropIndex::Inter {
                table: vec![(0, PROP_EMPTY); cap],
                mask: cap - 1,
                len: 0,
            }
        }
    }

    fn build(keys: &[String]) -> Box<PropIndex> {
        let mut ix = PropIndex::with_capacity(keys.len());
        for (i, k) in keys.iter().enumerate() {
            ix.insert(k, i as u32);
        }
        Box::new(ix)
    }

    /// Position in `keys` of the entry equal to `key`, confirmed by compare.
    #[inline]
    fn find(&self, keys: &[String], key: &str) -> Option<usize> {
        #[cfg(feature = "safe-sandbox")]
        if let PropIndex::Tree { entries } = self {
            return entries.get(key).copied().map(|slot| slot as usize);
        }
        let tag = prop_tag(key);
        match self {
            #[cfg(feature = "safe-sandbox")]
            PropIndex::Tree { .. } => unreachable!(),
            PropIndex::Inter { table, mask, .. } => {
                let mut i = tag as usize & mask;
                loop {
                    let (st, ss) = table[i];
                    if ss == PROP_EMPTY {
                        return None;
                    }
                    if st == tag && keys[ss as usize] == key {
                        return Some(ss as usize);
                    }
                    i = (i + 1) & mask;
                }
            }
            PropIndex::Split {
                tags, slots, mask, ..
            } => {
                let mut i = tag as usize & mask;
                loop {
                    let ss = slots[i];
                    if ss == PROP_EMPTY {
                        return None;
                    }
                    if tags[i] == tag && keys[ss as usize] == key {
                        return Some(ss as usize);
                    }
                    i = (i + 1) & mask;
                }
            }
        }
    }

    #[inline]
    fn cap(&self) -> usize {
        match self {
            #[cfg(feature = "safe-sandbox")]
            PropIndex::Tree { entries } => entries.len(),
            PropIndex::Inter { mask, .. } | PropIndex::Split { mask, .. } => mask + 1,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        match self {
            #[cfg(feature = "safe-sandbox")]
            PropIndex::Tree { entries } => entries.len(),
            PropIndex::Inter { len, .. } | PropIndex::Split { len, .. } => *len,
        }
    }

    fn resident_bytes(&self) -> usize {
        match self {
            #[cfg(feature = "safe-sandbox")]
            PropIndex::Tree { entries } => {
                // std's B-tree node fanout/layout is private. 128 bytes per
                // entry plus owned string capacity deliberately overcounts its
                // current node overhead while keeping heap audits stable.
                std::mem::size_of::<Self>()
                    .saturating_add(entries.len().saturating_mul(128))
                    .saturating_add(
                        entries
                            .keys()
                            .fold(0usize, |n, key| n.saturating_add(key.capacity())),
                    )
            }
            PropIndex::Inter { table, .. } => vec_capacity_bytes(table),
            PropIndex::Split { tags, slots, .. } => {
                vec_capacity_bytes(tags).saturating_add(vec_capacity_bytes(slots))
            }
        }
    }

    /// Record `slot` under `key`. The caller guarantees the key is absent
    /// (every insertion path misses in `pos()` first).
    fn insert(&mut self, key: &str, slot: u32) {
        #[cfg(feature = "safe-sandbox")]
        if let PropIndex::Tree { entries } = self {
            // The mutation must not live inside `debug_assert!`: its argument
            // is removed in release builds, which would leave every production
            // safe-profile index empty.
            let previous = entries.insert(key.to_owned(), slot);
            debug_assert!(previous.is_none());
            return;
        }
        if (self.len() + 1) * 4 >= self.cap() * 3 {
            self.grow();
        }
        self.insert_raw(prop_tag(key), slot);
        match self {
            #[cfg(feature = "safe-sandbox")]
            PropIndex::Tree { .. } => unreachable!(),
            PropIndex::Inter { len, .. } | PropIndex::Split { len, .. } => *len += 1,
        }
    }

    fn insert_raw(&mut self, tag: u32, slot: u32) {
        match self {
            #[cfg(feature = "safe-sandbox")]
            PropIndex::Tree { .. } => unreachable!(),
            PropIndex::Inter { table, mask, .. } => {
                let mut i = tag as usize & *mask;
                while table[i].1 != PROP_EMPTY {
                    i = (i + 1) & *mask;
                }
                table[i] = (tag, slot);
            }
            PropIndex::Split {
                tags, slots, mask, ..
            } => {
                let mut i = tag as usize & *mask;
                while slots[i] != PROP_EMPTY {
                    i = (i + 1) & *mask;
                }
                tags[i] = tag;
                slots[i] = slot;
            }
        }
    }

    /// Unrecord `slot` (whose key hashes to `tag`) after the owning map's
    /// `Vec::remove(slot)`: backward-shift-delete its table entry, then
    /// decrement every stored slot above `slot` (those keys all shifted down one
    /// position). The caller guarantees the entry exists — `pos()` just found the
    /// key through this very index.
    ///
    /// Both arms run the SAME two passes over the SAME buckets in the same
    /// order; only the physical layout differs.
    fn remove_slot(&mut self, tag: u32, slot: u32) {
        match self {
            #[cfg(feature = "safe-sandbox")]
            PropIndex::Tree { .. } => unreachable!(),
            PropIndex::Inter {
                table, mask, len, ..
            } => {
                let mask = *mask;
                // Walk the probe chain to the entry recording `slot` (tags can
                // collide; slot values are unique across the table).
                let mut j = tag as usize & mask;
                while table[j].1 != slot {
                    debug_assert!(table[j].1 != PROP_EMPTY, "remove_slot: entry missing");
                    j = (j + 1) & mask;
                }
                // Backward-shift deletion: free j, then pull forward any later
                // chain entry whose probe path runs through j (an entry at k with
                // ideal bucket b may move iff j ∈ [b, k) cyclically — otherwise a
                // find() probing from b would stop at the new hole before reaching it).
                table[j] = (0, PROP_EMPTY);
                let mut k = (j + 1) & mask;
                loop {
                    let (kt, ks) = table[k];
                    if ks == PROP_EMPTY {
                        break;
                    }
                    let ideal = kt as usize & mask;
                    if (j.wrapping_sub(ideal) & mask) < (k.wrapping_sub(ideal) & mask) {
                        table[j] = (kt, ks);
                        table[k] = (0, PROP_EMPTY);
                        j = k;
                    }
                    k = (k + 1) & mask;
                }
                *len -= 1;
                for e in table.iter_mut() {
                    if e.1 != PROP_EMPTY && e.1 > slot {
                        e.1 -= 1;
                    }
                }
            }
            PropIndex::Split {
                tags,
                slots,
                mask,
                len,
                ..
            } => {
                let mask = *mask;
                let mut j = tag as usize & mask;
                while slots[j] != slot {
                    debug_assert!(slots[j] != PROP_EMPTY, "remove_slot: entry missing");
                    j = (j + 1) & mask;
                }
                tags[j] = 0;
                slots[j] = PROP_EMPTY;
                let mut k = (j + 1) & mask;
                loop {
                    let ks = slots[k];
                    if ks == PROP_EMPTY {
                        break;
                    }
                    let kt = tags[k];
                    let ideal = kt as usize & mask;
                    if (j.wrapping_sub(ideal) & mask) < (k.wrapping_sub(ideal) & mask) {
                        tags[j] = kt;
                        slots[j] = ks;
                        tags[k] = 0;
                        slots[k] = PROP_EMPTY;
                        j = k;
                    }
                    k = (k + 1) & mask;
                }
                *len -= 1;
                // The renumber sweep, and the whole point of the split: one
                // contiguous u32 store per entry, written UNCONDITIONALLY so the
                // loop vectorizes. `PROP_EMPTY` is `u32::MAX`, so it satisfies
                // `v > slot` and must be excluded explicitly — the second
                // conjunct is not redundant.
                for s in slots.iter_mut() {
                    let v = *s;
                    let dec = ((v > slot) as u32) & ((v != PROP_EMPTY) as u32);
                    *s = v - dec;
                }
            }
        }
    }

    /// Remove one authoritative property slot. The balanced safe-profile
    /// index owns the exact key; the ordinary flat layouts retain their stored
    /// tag/backward-shift protocol. Every later authoritative Vec slot shifts
    /// down in either representation.
    fn remove(&mut self, key: &str, slot: u32) {
        #[cfg(feature = "safe-sandbox")]
        if let PropIndex::Tree { entries } = self {
            // As above, perform the state change outside the debug-only check.
            let removed = entries.remove(key);
            debug_assert_eq!(removed, Some(slot));
            for value in entries.values_mut() {
                if *value > slot {
                    *value -= 1;
                }
            }
            return;
        }
        self.remove_slot(prop_tag(key), slot);
    }

    /// Double and rehash from the stored tags (bucket = `tag & mask`).
    /// Preserves the variant — an index never changes layout mid-life.
    fn grow(&mut self) {
        match self {
            #[cfg(feature = "safe-sandbox")]
            PropIndex::Tree { .. } => return,
            PropIndex::Inter { table, mask, .. } => {
                let cap = table.len() * 2;
                let old = std::mem::replace(table, vec![(0, PROP_EMPTY); cap]);
                *mask = cap - 1;
                for (tag, slot) in old {
                    if slot != PROP_EMPTY {
                        self.insert_raw(tag, slot);
                    }
                }
            }
            PropIndex::Split {
                tags, slots, mask, ..
            } => {
                let cap = slots.len() * 2;
                let old_tags = std::mem::replace(tags, vec![0; cap]);
                let old_slots = std::mem::replace(slots, vec![PROP_EMPTY; cap]);
                *mask = cap - 1;
                for (tag, slot) in old_tags.into_iter().zip(old_slots) {
                    if slot != PROP_EMPTY {
                        self.insert_raw(tag, slot);
                    }
                }
            }
        }
    }

    /// Debug-only structural check: the arrays are in lockstep, `len` counts the
    /// occupied buckets, every occupied bucket's tag is its key's tag, and every
    /// key round-trips through `find`. A tags/slots desync is a HIT ON THE WRONG
    /// SLOT — a silent wrong answer, not a crash — so this is the assertion that
    /// catches it.
    #[cfg(test)]
    fn verify(&self, keys: &[String]) {
        #[cfg(feature = "safe-sandbox")]
        if let PropIndex::Tree { entries } = self {
            assert_eq!(entries.len(), keys.len());
            for (slot, key) in keys.iter().enumerate() {
                assert_eq!(entries.get(key), Some(&(slot as u32)));
                assert_eq!(self.find(keys, key), Some(slot));
            }
            return;
        }
        let cap = self.cap();
        if let PropIndex::Split {
            tags, slots, mask, ..
        } = self
        {
            assert_eq!(tags.len(), slots.len(), "tags/slots length desync");
            assert_eq!(tags.len(), mask + 1, "arrays do not match mask");
        }
        let mut live = 0usize;
        for i in 0..cap {
            let (t, sl) = match self {
                #[cfg(feature = "safe-sandbox")]
                PropIndex::Tree { .. } => unreachable!(),
                PropIndex::Inter { table, .. } => table[i],
                PropIndex::Split { tags, slots, .. } => (tags[i], slots[i]),
            };
            if sl == PROP_EMPTY {
                continue;
            }
            live += 1;
            assert!((sl as usize) < keys.len(), "slot {sl} out of range");
            assert_eq!(
                t,
                prop_tag(&keys[sl as usize]),
                "tag/slot desync at bucket {i}"
            );
        }
        assert_eq!(live, self.len(), "len disagrees with occupancy");
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(self.find(keys, k), Some(i), "key {k:?} lost");
        }
    }
}

/// Does `key` name an ELEMENT of an exotic object — a canonical decimal index,
/// or `"length"`? Deliberately allocation-free (unlike `canonical_index_str`,
/// which round-trips through `to_string`), because it runs on every structural
/// append to every `ObjMap` in the engine.
///
/// Conservative: it accepts any all-digit key with no redundant leading zero,
/// so `"4294967295"` and larger — which are ordinary NAMED properties, not
/// array indices — answer `true`. Over-reporting only costs a fast path.
#[inline]
pub(crate) fn key_names_element(key: &str) -> bool {
    let b = key.as_bytes();
    match b.first() {
        Some(&c) if c.is_ascii_digit() => {
            (b.len() == 1 || c != b'0') && b[1..].iter().all(|c| c.is_ascii_digit())
        }
        _ => key == "length",
    }
}

/// Parse the canonical decimal spelling of an ECMAScript Array index without
/// allocating.  `2^32 - 1` is deliberately excluded: it is a named property,
/// not an Array index, even though it is all decimal digits.
#[inline]
fn canonical_array_index_key(key: &str) -> Option<u32> {
    let b = key.as_bytes();
    if b.is_empty() || (b.len() > 1 && b[0] == b'0') || b.len() > 10 {
        return None;
    }
    let mut n = 0u32;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add((c - b'0') as u32)?;
    }
    (n != u32::MAX).then_some(n)
}

/// W25 sparse numeric side-index latch. `ZIPP_NO_SPARSE_NUM_INDEX=1` restores
/// the old stack-format + string-hash lookup path in the same binary.
#[inline]
fn sparse_num_index_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_SPARSE_NUM_INDEX").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// `ZIPP_NO_ATTRS_ELIDE=1` makes every map carry the explicit attribute
/// vector from birth (`PropAttrs::Mixed`), the pre-elision representation —
/// the single-binary A/B for the all-default elision. Latched on first use.
#[inline]
fn attrs_elide_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_ATTRS_ELIDE").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

pub fn index_key(buf: &mut [u8; 20], i: usize) -> &str {
    let mut n = i;
    let mut p = buf.len();
    loop {
        p -= 1;
        buf[p] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    // Every byte written is ASCII, so this cannot fail.
    std::str::from_utf8(&buf[p..]).unwrap_or("")
}

mod static_key_stats {
    use super::*;

    static STATE: AtomicU8 = AtomicU8::new(0);
    static OBJECTS: AtomicU64 = AtomicU64::new(0);
    static APPENDS: AtomicU64 = AtomicU64::new(0);
    static MATERIALIZATIONS: AtomicU64 = AtomicU64::new(0);
    static JIT_OBJECTS: AtomicU64 = AtomicU64::new(0);

    #[inline]
    fn enabled() -> bool {
        match STATE.load(Ordering::Relaxed) {
            1 => true,
            2 => false,
            _ => {
                let on = std::env::var_os("ZIPP_STATIC_KEY_STATS").is_some();
                STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
                on
            }
        }
    }

    #[inline]
    pub(super) fn object() {
        if enabled() {
            OBJECTS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(super) fn append() {
        if enabled() {
            APPENDS.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// A one-step `FinalizeObject` build commits `n` allocation-free appends
    /// at once; count them so the append figure keeps its per-field meaning
    /// across both lowerings.
    #[inline]
    pub(super) fn bulk_appends(n: usize) {
        if enabled() {
            APPENDS.fetch_add(n as u64, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(super) fn materialize() {
        if enabled() {
            MATERIALIZATIONS.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(super) fn jit_object() {
        if enabled() {
            JIT_OBJECTS.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn dump() -> (u64, u64, u64, u64) {
        (
            OBJECTS.load(Ordering::Relaxed),
            APPENDS.load(Ordering::Relaxed),
            MATERIALIZATIONS.load(Ordering::Relaxed),
            JIT_OBJECTS.load(Ordering::Relaxed),
        )
    }
}

/// `(planned object allocations, allocation-free planned appends, defensive
/// materializations, Tier-C planned-object helper allocations)`. Counters are active only with
/// `ZIPP_STATIC_KEY_STATS=1`, so the normal path pays one relaxed latch load.
pub fn static_key_plan_stats() -> (u64, u64, u64, u64) {
    static_key_stats::dump()
}

/// Record that Tier C executed its planned allocation helper. Kept distinct
/// from `OBJECTS` so tests can prove native execution rather than only native
/// compilation; the comparator may still route that helper to owned storage.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
pub(crate) fn note_static_key_jit_object() {
    static_key_stats::jit_object();
}

/// Authoritative insertion-ordered property names. Eligible object literals
/// share an immutable compiler-owned list while independently exposing only
/// the prefix whose values/attributes have actually been appended. Any
/// structural mismatch or mutation materializes that visible prefix first.
#[derive(Clone)]
pub enum PropKeys {
    Owned(Vec<String>),
    Planned {
        all: StaticKeyPlan,
        visible_len: usize,
    },
}

impl Default for PropKeys {
    fn default() -> Self {
        Self::Owned(Vec::new())
    }
}

impl std::fmt::Debug for PropKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_ref().fmt(f)
    }
}

impl PartialEq for PropKeys {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}

impl Eq for PropKeys {}

impl AsRef<[String]> for PropKeys {
    fn as_ref(&self) -> &[String] {
        match self {
            Self::Owned(keys) => keys,
            Self::Planned { all, visible_len } => &all.keys()[..*visible_len],
        }
    }
}

impl Deref for PropKeys {
    type Target = [String];

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl DerefMut for PropKeys {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.make_owned().as_mut_slice()
    }
}

impl<'a> IntoIterator for &'a PropKeys {
    type Item = &'a String;
    type IntoIter = std::slice::Iter<'a, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut PropKeys {
    type Item = &'a mut String;
    type IntoIter = std::slice::IterMut<'a, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.make_owned().iter_mut()
    }
}

impl PropKeys {
    fn resident_bytes(&self) -> usize {
        match self {
            // A planned list is compiler-owned and shared by every object made
            // from that literal; charging it per object would multiply the same
            // allocation. It is covered by the separate source/code limits.
            Self::Planned { .. } => 0,
            Self::Owned(keys) => vec_capacity_bytes(keys).saturating_add(
                keys.iter()
                    .fold(0usize, |n, key| n.saturating_add(key.capacity())),
            ),
        }
    }

    fn planned(plan: StaticKeyPlan) -> Self {
        Self::Planned {
            all: plan,
            visible_len: 0,
        }
    }

    fn make_owned(&mut self) -> &mut Vec<String> {
        let materialized = match self {
            Self::Owned(_) => None,
            Self::Planned { all, visible_len } => Some(all.keys()[..*visible_len].to_vec()),
        };
        if let Some(keys) = materialized {
            *self = Self::Owned(keys);
            static_key_stats::materialize();
        }
        let Self::Owned(keys) = self else {
            unreachable!("planned keys were materialized")
        };
        keys
    }

    fn reserve_exact(&mut self, additional: usize) {
        self.make_owned().reserve_exact(additional);
    }

    fn push(&mut self, key: String) {
        self.make_owned().push(key);
    }

    fn remove(&mut self, i: usize) -> String {
        self.make_owned().remove(i)
    }

    /// Advance an exact compiler-planned key. A mismatch leaves the plan
    /// untouched so the caller can materialize via its ordinary append path.
    fn advance_if_next(&mut self, key: &str) -> bool {
        let Self::Planned { all, visible_len } = self else {
            return false;
        };
        if all.keys().get(*visible_len).map(String::as_str) != Some(key) {
            return false;
        }
        *visible_len += 1;
        static_key_stats::append();
        true
    }

    fn is_planned(&self) -> bool {
        matches!(self, Self::Planned { .. })
    }
}

/// A JS object: insertion-ordered string-keyed properties.
#[derive(Clone, Debug, Default)]
pub struct ObjMap {
    pub keys: PropKeys,
    pub vals: Vec<Value>,
    /// Per-property attributes, parallel to `keys`/`vals` (a property descriptor's
    /// writable/enumerable/configurable + accessor get/set). For a DATA property
    /// `vals[i]` is the value; for an ACCESSOR `vals[i]` is the getter and
    /// `attrs[i].setter` the setter. PRIVATE (like `index`): all access goes
    /// through the `attr_*`/`attrs_*` methods below, which is what lets the
    /// common all-default case store nothing — see [`PropAttrs`].
    attrs: PropAttrs,
    /// Heap index of the class this object is an instance of (`new C()`), used
    /// for prototype-style method lookup and `instanceof`. `None` for a plain
    /// object literal. Own properties (the fields) live in `keys`/`vals`;
    /// methods are resolved through the class, so they stay non-enumerable.
    pub class: Option<u32>,
    /// `[[Extensible]]`: whether new own properties may be added. Cleared by
    /// `Object.preventExtensions`/`seal`/`freeze`. Default `true`.
    pub extensible: bool,
    /// True for the built-in constructor globals (Object/Array/Map/…), which are
    /// modelled as objects but are callable constructors: `typeof` reports
    /// "function" and they satisfy IsConstructor. False for ordinary objects and
    /// the namespace globals (Reflect/Math/JSON).
    pub is_ctor: bool,
    /// `[[IsRawJSON]]`: set only on the frozen objects returned by
    /// `JSON.rawJSON`. `JSON.isRawJSON` reports it, and `JSON.stringify` emits
    /// the object's `"rawJSON"` string property verbatim instead of serialising.
    pub is_raw_json: bool,
    /// Explicit Object.seal / Object.freeze markers. For a PLAIN object the
    /// per-property `attrs` already encode sealed/frozen, but an exotic object
    /// whose elements live OUTSIDE this map (a dense Array's Vec, a TypedArray's
    /// buffer) has no per-element attrs, so seal/freeze on it is recorded here.
    pub sealed: bool,
    pub frozen: bool,
    /// Lazy hash index over `keys` (see [`PropIndex`]): `None` (linear scan)
    /// until the map reaches [`PROP_INDEX_THRESHOLD`] keys, maintained by
    /// `set`/`define` on append and rebuilt/dropped by `remove`. PRIVATE so
    /// the keys⇄index invariant is owned entirely by this impl block — code
    /// that mutates `keys` structurally must go through these methods.
    /// `Clone` clones the table verbatim, which is valid because the clone
    /// has identical keys at identical slots.
    index: Option<Box<PropIndex>>,
    /// Allocation-free lookup for canonical numeric keys. Sparse Arrays store
    /// present elements in this string-keyed map; hashing a freshly formatted
    /// decimal string on every read dominated their hot path. The table is
    /// created lazily only for maps that actually acquire an Array index, so an
    /// ordinary named-property object pays one nullable pointer and no heap
    /// allocation. Slots point into the authoritative parallel vectors.
    numeric_index: Option<Box<rustc_hash::FxHashMap<u32, u32>>>,
    /// Does any key in this map name an ELEMENT of the exotic object it hangs
    /// off — a canonical decimal index, or `"length"`? Maintained by the same
    /// three appends and one removal that maintain `shape`, and read only
    /// through [`ObjMap::overlays_elements`].
    ///
    /// It exists because ~15 dense-array fast paths ask "can an element of this
    /// array be shadowed?" and answer it with `arr_props.contains_key(idx)` —
    /// the presence of ANY entry. A RegExp match result always has one (it is
    /// where `index`/`input`/`groups` live) and so falls off every one of them:
    /// `m.map(…)`, `m.slice(1)`, `for (const x of m)`, `JSON.stringify(m)` and
    /// every JIT'd `m[i]` (which deopts to the interpreter). None of those four
    /// names can shadow an element, so the precise question is this bit.
    has_element_key: bool,
    /// A planned literal observed a mutation or an out-of-order AppendDataProp.
    /// From that point malformed/replayed appends use descriptor-safe `define`
    /// rather than the compiler-proof-only unchecked push. Fits existing bool
    /// padding, preserving ObjMap's pinned size.
    planned_append_failed: bool,
    /// This object's hidden class — see [`crate::shape`]. A redundant summary of
    /// `keys` + `attrs`, maintained by the same methods that mutate them, so an
    /// inline cache can ask "same layout?" with one integer compare instead of
    /// pinning an object IDENTITY (which is what makes the current caches fall
    /// off a cliff past a handful of instances).
    ///
    /// [`shape::DICT`] means "not describable as a sequence of appends" — a key
    /// was deleted, or an existing property's attributes were redefined. Nothing
    /// depends on a shape being present, only on it being correct when it is.
    shape: u32,
}

/// The attribute column of an [`ObjMap`], with the all-default case elided.
///
/// Almost every object ever built — literals, class instances, JSON — has
/// nothing but writable/enumerable/configurable data properties, i.e. every
/// slot is exactly [`PropAttr::data()`]. Storing a 16-byte `PropAttr` per
/// property for that case cost one heap allocation (and one free at sweep)
/// per object, ~a third of a small object's allocator traffic — priced by
/// B176's finding that the allocator is where the object rows' time went.
/// `AllData { len }` stores only the count; the first deviating write
/// materializes the real vector and the map stays `Mixed` for life.
///
/// The IC caches raw `&attrs[i].setter` pointers ([`ObjMap::setter_ref`]).
/// That stays sound: a setter can only be cached AFTER an accessor property
/// exists, an accessor is a non-default attribute, so the map materialized
/// BEFORE the pointer was taken — `Mixed` never re-allocates on attribute
/// writes, and key adds (which can grow the vector) bump the object version
/// exactly as they did when they could grow `vals`.
#[derive(Clone, Debug)]
enum PropAttrs {
    /// Every slot is `PropAttr::data()`; only the count is stored.
    AllData { len: u32 },
    /// At least one slot deviated (or the elision latch is off): the full
    /// parallel vector, exactly the pre-elision representation.
    Mixed(Vec<PropAttr>),
}

impl Default for PropAttrs {
    /// An empty column in the latched representation (`ObjMap` derives
    /// `Default`, so this must make the same choice `ObjMap::new` makes).
    fn default() -> Self {
        if attrs_elide_enabled() {
            PropAttrs::AllData { len: 0 }
        } else {
            PropAttrs::Mixed(Vec::new())
        }
    }
}

impl PropAttrs {
    const DEFAULT: PropAttr = PropAttr {
        writable: true,
        enumerable: true,
        configurable: true,
        accessor: false,
        setter: Value::UNDEFINED,
    };

    /// Is `a` bit-identical to the default data attribute? (`setter` compares
    /// by NaN-box bits; only `UNDEFINED` matches.)
    #[inline]
    fn is_default(a: &PropAttr) -> bool {
        a.writable && a.enumerable && a.configurable && !a.accessor && a.setter == Value::UNDEFINED
    }

    #[inline]
    fn len(&self) -> usize {
        match self {
            PropAttrs::AllData { len } => *len as usize,
            PropAttrs::Mixed(v) => v.len(),
        }
    }

    /// The slot's attributes; panics out of range in BOTH representations,
    /// preserving the parallel-vector indexing this replaced.
    #[inline]
    fn at(&self, i: usize) -> PropAttr {
        match self {
            PropAttrs::AllData { len } => {
                assert!(i < *len as usize, "attr index {i} out of range {len}");
                Self::DEFAULT
            }
            PropAttrs::Mixed(v) => v[i],
        }
    }

    #[inline]
    fn get(&self, i: usize) -> Option<PropAttr> {
        match self {
            PropAttrs::AllData { len } => (i < *len as usize).then_some(Self::DEFAULT),
            PropAttrs::Mixed(v) => v.get(i).copied(),
        }
    }

    /// Switch to the explicit vector (a no-op if already there) and return it.
    fn materialize(&mut self) -> &mut Vec<PropAttr> {
        if let PropAttrs::AllData { len } = *self {
            *self = PropAttrs::Mixed(vec![Self::DEFAULT; len as usize]);
        }
        match self {
            PropAttrs::Mixed(v) => v,
            PropAttrs::AllData { .. } => unreachable!(),
        }
    }

    #[inline]
    fn push(&mut self, a: PropAttr) {
        match self {
            PropAttrs::AllData { len } if Self::is_default(&a) => *len += 1,
            _ => self.materialize().push(a),
        }
    }

    #[inline]
    fn remove(&mut self, i: usize) -> PropAttr {
        match self {
            PropAttrs::AllData { len } => {
                assert!(i < *len as usize, "attr index {i} out of range {len}");
                *len -= 1;
                Self::DEFAULT
            }
            PropAttrs::Mixed(v) => v.remove(i),
        }
    }

    #[inline]
    fn reserve_exact(&mut self, n: usize) {
        if let PropAttrs::Mixed(v) = self {
            v.reserve_exact(n);
        }
    }

    #[inline]
    fn iter(&self) -> impl Iterator<Item = PropAttr> + '_ {
        (0..self.len()).map(move |i| self.at(i))
    }

    #[inline]
    fn capacity_bytes(&self) -> usize {
        match self {
            PropAttrs::AllData { .. } => 0,
            PropAttrs::Mixed(v) => vec_capacity_bytes(v),
        }
    }
}

/// One property's attributes — the ECMAScript property-descriptor flags plus an
/// accessor pair. A data property uses `writable` and the parallel `vals` entry;
/// an accessor (`accessor == true`) uses `vals[i]` as the getter and `setter`.
#[derive(Clone, Copy, Debug)]
pub struct PropAttr {
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
    pub accessor: bool,
    /// The setter function for an accessor property (`UNDEFINED` if none / data).
    pub setter: Value,
}

impl PropAttr {
    /// The default attributes for an ordinary created property (`obj.x = v`,
    /// object literals): a writable, enumerable, configurable data property.
    pub fn data() -> PropAttr {
        PropAttr {
            writable: true,
            enumerable: true,
            configurable: true,
            accessor: false,
            setter: Value::UNDEFINED,
        }
    }
}

impl ObjMap {
    pub(crate) fn resident_bytes(&self) -> usize {
        let mut n = self
            .keys
            .resident_bytes()
            .saturating_add(vec_capacity_bytes(&self.vals))
            .saturating_add(self.attrs.capacity_bytes());
        if let Some(index) = &self.index {
            n = n.saturating_add(index.resident_bytes());
        }
        if let Some(index) = &self.numeric_index {
            // Hash table control bytes are implementation-private. Two buckets'
            // worth per reported entry is a conservative stable approximation.
            n = n.saturating_add(
                index
                    .capacity()
                    .saturating_mul(std::mem::size_of::<(u32, u32)>() * 2),
            );
        }
        n
    }

    // ---- accessors ---------------------------------------------------------
    //
    // The three parallel `Vec`s are still public while the hidden-class
    // migration is in flight, but NEW code should go through these. They exist
    // so the layout can change underneath: a shape-based object stores its keys
    // and attributes in a SHARED descriptor rather than per-object vectors, and
    // every read site that names `m.keys[i]` directly would otherwise have to
    // change in the same commit that changes the layout.
    //
    // All of them are `#[inline]` and compile to exactly what the field access
    // compiled to, so converting a call site is a no-op you can land and measure
    // separately from the layout change itself.

    /// Number of own properties.
    #[inline]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// The property name at `i`. Panics out of range, exactly like the indexing
    /// it replaces — every caller has already established `i` from `pos()`.
    #[inline]
    pub fn key_at(&self, i: usize) -> &str {
        &self.keys[i]
    }

    /// The stored value at `i` — the data value, or the GETTER for an accessor.
    #[inline]
    pub fn val_at(&self, i: usize) -> Value {
        self.vals[i]
    }

    #[inline]
    pub fn attr_at(&self, i: usize) -> PropAttr {
        self.attrs.at(i)
    }

    #[inline]
    pub fn attr_get(&self, i: usize) -> Option<PropAttr> {
        self.attrs.get(i)
    }

    /// Mutable access to one slot's attributes. Prefer [`ObjMap::set_attr_at`]
    /// for redefinitions (it maintains `shape`); this is the raw parallel-vec
    /// write the direct field access used to be, for the construction and
    /// accessor-install paths whose shape discipline lives at the call site.
    #[inline]
    pub fn attr_mut(&mut self, i: usize) -> &mut PropAttr {
        let n = self.attrs.len();
        assert!(i < n, "attr index {i} out of range {n}");
        &mut self.attrs.materialize()[i]
    }

    /// Append one slot's attributes — the raw parallel-vec push. The caller
    /// owns the key/val pushes and any shape maintenance, exactly as with the
    /// direct field access this replaces. Part of the total accessor API: its
    /// callers are configuration-dependent (JIT emitters, tests), so the
    /// no-JIT sandbox build sees it unused; in the default build its callers
    /// are test code, so the lint sees none either. Kept: it is the ONLY
    /// public append, and the representation flip is meaningless without it.
    #[allow(dead_code)]
    #[inline]
    pub fn push_attr(&mut self, a: PropAttr) {
        self.attrs.push(a);
    }

    /// Every slot's attributes in order, by value (`PropAttr` is `Copy`).
    #[inline]
    pub fn attrs_iter(&self) -> impl Iterator<Item = PropAttr> + '_ {
        self.attrs.iter()
    }

    /// Number of attribute slots. Equals `len()` for a consistent map; the
    /// parallel vectors can disagree transiently mid-append, which is exactly
    /// what the callers that ask this are checking.
    #[cfg_attr(not(all(feature = "jit", target_arch = "x86_64")), allow(dead_code))]
    #[inline]
    pub fn attrs_len(&self) -> usize {
        self.attrs.len()
    }

    /// Might any slot hold an accessor (or otherwise deviate from default
    /// data attributes)? `false` is a proof of absence — the all-default
    /// representation cannot store a setter — which lets the GC skip the
    /// setter-tracing walk and enumeration skip per-slot `enumerable` checks.
    /// `true` only means "explicit attributes present", not "has accessors".
    #[inline]
    pub fn may_deviate_attrs(&self) -> bool {
        matches!(self.attrs, PropAttrs::Mixed(_))
    }

    /// The ADDRESS of slot `i`'s setter, for the accessor inline cache, which
    /// stores it and loads through it on later hits (guarded by the object
    /// version, which every key add bumps). Only meaningful for a slot that
    /// holds an accessor — callers check `attr_at(i).accessor` first.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub fn setter_ref(&self, i: usize) -> &Value {
        match &self.attrs {
            PropAttrs::Mixed(v) => &v[i].setter,
            // Only reachable for an accessor slot, which forces `Mixed`.
            PropAttrs::AllData { .. } => unreachable!("setter_ref on all-default attrs"),
        }
    }

    /// Overwrite the value in an EXISTING slot. Not a structural change: the key
    /// sequence is untouched, so no shape transition and no version bump.
    #[inline]
    pub fn set_val_at(&mut self, i: usize, v: Value) {
        self.vals[i] = v;
    }

    /// Overwrite an existing slot's attributes. Under shapes this becomes a
    /// transition, which is why it is a method rather than a field write.
    #[inline]
    pub fn set_attr_at(&mut self, i: usize, a: PropAttr) {
        let old = self.attrs.at(i);
        if old.writable != a.writable
            || old.enumerable != a.enumerable
            || old.configurable != a.configurable
            || old.accessor != a.accessor
        {
            self.shape_to_dict();
        }
        *self.attr_mut(i) = a;
    }

    /// `(name, value, attrs)` per own property, insertion order.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (&str, Value, PropAttr)> {
        (0..self.keys.len()).map(move |i| (&*self.keys[i], self.vals[i], self.attrs.at(i)))
    }

    /// This object's hidden class, or [`shape::DICT`] if it has none.
    #[inline]
    pub fn shape(&self) -> u32 {
        self.shape
    }

    /// Whether `shape()` may be used as a cache guard. `DICT` must never be
    /// compared for equality — two dictionary-mode objects share the sentinel
    /// while having nothing else in common — so this is the single place that
    /// question is answered.
    #[inline]
    pub fn shape_guardable(&self) -> bool {
        self.shape != crate::shape::DICT
    }

    /// Drop to dictionary mode: the layout stopped being a sequence of appends.
    #[inline]
    fn shape_to_dict(&mut self) {
        self.shape = crate::shape::DICT;
    }

    /// Check that this object's shape actually describes it, returning the first
    /// disagreement.
    ///
    /// A shape is a claim: "my keys are these, in this order, with these
    /// descriptor bits". Nothing native reads that claim TODAY, so a stale shape
    /// is invisible — every guard is receiver identity plus a version. The
    /// moment an emitted probe guards on a shape instead, a stale one is a hit
    /// on the wrong slot of the wrong object, silently. This is the check that
    /// makes the claim testable before anything depends on it.
    ///
    /// `DICT` is not checkable and is not meant to be: it is the sentinel for
    /// "no longer describable by appends", shared by every dictionary object,
    /// and `shape_guardable()` already refuses it.
    pub fn verify_shape(&self) -> Result<(), String> {
        if !self.shape_guardable() {
            return Ok(());
        }
        let claimed = crate::shape::describe(self.shape);
        if claimed.len() != self.keys.len() {
            return Err(format!(
                "shape {} claims {} properties, object holds {}",
                self.shape,
                claimed.len(),
                self.keys.len()
            ));
        }
        for (i, (key, bits)) in claimed.iter().enumerate() {
            if **key != *self.keys[i] {
                return Err(format!(
                    "shape {} slot {} claims key {:?}, object holds {:?}",
                    self.shape, i, key, self.keys[i]
                ));
            }
            let a = self.attrs.at(i);
            let actual =
                crate::shape::attr_bits(a.writable, a.enumerable, a.configurable, a.accessor);
            if *bits != actual {
                return Err(format!(
                    "shape {} slot {} ({:?}) claims attr bits {:#04b}, object has {:#04b}",
                    self.shape, i, key, bits, actual
                ));
            }
        }
        Ok(())
    }

    /// As `shape_pushed`, but called BEFORE the key is moved into the vector
    /// (so the length assertion is off by one and is skipped).
    #[inline]
    fn shape_pushed_owned(&mut self, key: &str, a: &PropAttr) {
        self.shape = crate::shape::add(
            self.shape,
            key,
            crate::shape::attr_bits(a.writable, a.enumerable, a.configurable, a.accessor),
        );
    }

    /// Extend the shape by the property just appended at the end of `keys`.
    #[inline]
    fn shape_pushed(&mut self, key: &str, a: &PropAttr) {
        self.shape = crate::shape::add(
            self.shape,
            key,
            crate::shape::attr_bits(a.writable, a.enumerable, a.configurable, a.accessor),
        );
        debug_assert!(
            self.shape == crate::shape::DICT
                || crate::shape::len(self.shape) as usize == self.keys.len(),
            "shape drifted from the key vector it summarises"
        );
    }

    /// Raw base of the value vector, for the JIT to bake into a compiled load.
    ///
    /// NAMED rather than inlined at the call sites so the ~7 places that depend
    /// on `vals` being a contiguous `Vec<Value>` — the codegen emits a scale-8
    /// indexed load through this — are greppable. A shape migration must keep
    /// `vals` exactly this shape; that constraint is why the values stay in a
    /// per-object vector while the KEYS move into the shared descriptor.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub fn vals_ptr(&self) -> *const Value {
        self.vals.as_ptr()
    }

    /// An empty map sized for `n` properties up front — one allocation per
    /// vector instead of the regrowth an object literal otherwise pays as it
    /// appends (a 6-key literal cost ~36ns/key at the tail against ~17ns
    /// steady-state, and that difference is the regrowth).
    pub fn with_capacity(n: usize) -> ObjMap {
        let mut m = ObjMap::new();
        if n > 0 {
            m.keys.reserve_exact(n);
            m.vals.reserve_exact(n);
            m.attrs.reserve_exact(n);
        }
        m
    }

    /// Empty object storage backed by a compiler-prepared immutable key list.
    /// The explicit mode seam lets tests compare both representations without
    /// racing the process-wide environment latch.
    pub(crate) fn with_static_key_plan_mode(plan: StaticKeyPlan, enabled: bool) -> ObjMap {
        let n = plan.len();
        if !enabled {
            return ObjMap::with_capacity(n);
        }
        let mut m = ObjMap::new();
        m.keys = PropKeys::planned(plan);
        if n > 0 {
            m.vals.reserve_exact(n);
            m.attrs.reserve_exact(n);
        }
        static_key_stats::object();
        m
    }

    /// Shipped constructor using the process-latched
    /// `ZIPP_NO_STATIC_KEY_PLANS` comparator.
    pub(crate) fn with_static_key_plan(plan: StaticKeyPlan) -> ObjMap {
        Self::with_static_key_plan_mode(plan, static_key_plans_enabled())
    }

    /// Fully-populated storage for a one-step `FinalizeObject` literal: every
    /// plan key visible, `vals` already complete, all default data attributes.
    ///
    /// `shape` MUST be the exact fold of `shape::add` over the plan's keys with
    /// data-attribute bits (the caller memoizes that fold per plan on its own
    /// thread — shape ids are thread-local). Passing the fold keeps this
    /// constructor equal in every observable respect to `with_static_key_plan`
    /// followed by one `push_static_data` per key, which is the equivalence the
    /// finalize tests pin.
    ///
    /// The caller guarantees `plan.runtime_valid()` and `vals.len() ==
    /// plan.len()`; both are debug-asserted here.
    pub(crate) fn finalized_from_plan(plan: StaticKeyPlan, vals: Vec<Value>, shape: u32) -> ObjMap {
        debug_assert!(plan.runtime_valid());
        debug_assert_eq!(vals.len(), plan.len());
        let n = plan.len();
        let has_element_key = plan.has_element_key();
        let mut m = ObjMap::new();
        m.keys = PropKeys::Planned {
            all: plan,
            visible_len: n,
        };
        m.vals = vals;
        m.attrs = PropAttrs::AllData { len: n as u32 };
        m.shape = shape;
        m.has_element_key = has_element_key;
        // The cold tails the per-append `index_appended` would have built: the
        // numeric side index for element-naming keys, and the key index past
        // the probe threshold. Both are rare for finalize-eligible literals.
        if has_element_key && sparse_num_index_enabled() {
            let mut numeric = Box::new(rustc_hash::FxHashMap::default());
            for (slot, key) in m.keys.as_ref().iter().enumerate() {
                if let Some(idx) = canonical_array_index_key(key) {
                    numeric.insert(idx, slot as u32);
                }
            }
            m.numeric_index = Some(numeric);
        }
        if n >= PROP_INDEX_THRESHOLD {
            m.index = Some(PropIndex::build(&m.keys));
        }
        static_key_stats::object();
        static_key_stats::bulk_appends(n);
        m
    }

    /// A map used as an engine-internal SIDE TABLE — an Array's or RegExp's
    /// named properties, a function's own properties — rather than as a JS
    /// object's own storage.
    ///
    /// It starts in dictionary mode and stays there, because it can never be the
    /// receiver of a shape-keyed guard (those only ever see `HeapObj::Object`)
    /// and maintaining a shape for it is pure cost. That cost is not
    /// hypothetical: a sparse array's side table is keyed by index STRINGS, so
    /// every element mints a fresh shape and every append misses the transition
    /// scan. `json-large` built 54,390 shapes with a fan-out of 313 that way,
    /// for +9% on the bench.
    pub fn new_side_table() -> ObjMap {
        let mut m = ObjMap::new();
        m.shape = crate::shape::DICT;
        m
    }

    /// `new_side_table` with pre-sized vectors — for the regex match-result
    /// entry, which appends exactly `index`/`input`/`groups` (+`indices` with
    /// /d) per match. That site used `with_capacity`, whose shape starts at
    /// the EMPTY root — so every match ran 3-4 REAL `shape::add` transitions
    /// (a TLS table probe each) for a map that can never serve a shape guard.
    /// Every other side table already starts DICT, where `push_data`'s shape
    /// maintenance is one branch; this closes the accidental exception.
    pub fn side_table_with_capacity(n: usize) -> ObjMap {
        let mut m = ObjMap::new_side_table();
        if n > 0 {
            m.keys.reserve_exact(n);
            m.vals.reserve_exact(n);
            m.attrs.reserve_exact(n);
        }
        m
    }

    pub fn new() -> ObjMap {
        ObjMap {
            keys: PropKeys::default(),
            vals: Vec::new(),
            attrs: if attrs_elide_enabled() {
                PropAttrs::AllData { len: 0 }
            } else {
                PropAttrs::Mixed(Vec::new())
            },
            class: None,
            extensible: true,
            is_ctor: false,
            is_raw_json: false,
            sealed: false,
            frozen: false,
            index: None,
            numeric_index: None,
            has_element_key: false,
            planned_append_failed: false,
            shape: crate::shape::EMPTY,
        }
    }

    /// Can this side table shadow, hide, or constrain an ELEMENT (or `length`)
    /// of the exotic object it belongs to? The precise form of the
    /// `arr_props.contains_key(idx)` test that the dense-array fast paths use —
    /// see [`ObjMap::has_element_key`]. Non-extensible/sealed/frozen counts
    /// because an in-place `items[i]` store on a frozen array must not silently
    /// succeed; `!extensible` subsumes both markers (`seal`/`freeze` clear it).
    ///
    /// Conservative direction is `true`: a `false` here licenses a dense path,
    /// so anything uncertain must answer `true`.
    #[inline]
    pub fn overlays_elements(&self) -> bool {
        self.has_element_key || !self.extensible
    }

    /// Whether any key here names an element — see [`ObjMap::has_element_key`].
    /// Unlike [`ObjMap::overlays_elements`] this ignores the integrity flags, for
    /// the one caller that is asking a pure key question (`array_index_override`).
    #[inline]
    pub fn has_element_key(&self) -> bool {
        self.has_element_key
    }

    /// `Object.isSealed`: not extensible and every own property non-configurable.
    pub fn is_sealed(&self) -> bool {
        !self.extensible && self.attrs.iter().all(|a| !a.configurable)
    }

    /// `Object.isFrozen`: sealed and every own data property non-writable.
    pub fn is_frozen(&self) -> bool {
        !self.extensible
            && self
                .attrs
                .iter()
                .all(|a| !a.configurable && (a.accessor || !a.writable))
    }

    /// `Object.seal`: clear extensibility and make every own property non-configurable.
    pub fn seal(&mut self) {
        self.shape_to_dict(); // every property's `configurable` changes
        self.extensible = false;
        self.sealed = true;
        for a in self.attrs.materialize() {
            a.configurable = false;
        }
    }

    /// `Object.freeze`: seal, and make every own data property non-writable too.
    pub fn freeze(&mut self) {
        self.shape_to_dict(); // every property's `configurable`/`writable` changes
        self.extensible = false;
        self.sealed = true;
        self.frozen = true;
        for a in self.attrs.materialize() {
            a.configurable = false;
            if !a.accessor {
                a.writable = false;
            }
        }
    }

    #[inline]
    pub fn pos(&self, key: &str) -> Option<usize> {
        match &self.index {
            Some(ix) => ix.find(&self.keys, key),
            None => self.keys.iter().position(|k| k == key),
        }
    }

    /// Whether `key` is the exact next entry in an intact compiler key plan.
    ///
    /// This is a PURE absence proof for the Tier-C append helper: a valid plan
    /// contains unique keys, and an intact planned map exposes exactly its
    /// already-built prefix. Therefore the next planned key cannot already be
    /// present and the helper may skip `pos()` before committing it. Every
    /// invariant is rechecked here so malformed internal state falls back to
    /// the ordinary lookup/deopt path rather than licensing an unchecked push.
    #[cfg(any(test, all(feature = "jit", target_arch = "x86_64")))]
    #[inline]
    pub(crate) fn planned_next_static_key(&self, key: &str) -> bool {
        if self.planned_append_failed {
            return false;
        }
        let PropKeys::Planned { all, visible_len } = &self.keys else {
            return false;
        };
        all.runtime_valid()
            && self.vals.len() == *visible_len
            && self.attrs.len() == *visible_len
            && all.keys().get(*visible_len).map(String::as_str) == Some(key)
    }

    /// Position of a canonical Array-index key. With the W25 side index on,
    /// absence of the table is also proof that this map has no numeric keys.
    /// The off-switch is the exact pre-wave spelling/hash lookup.
    #[inline]
    pub fn element_pos(&self, index: usize) -> Option<usize> {
        if sparse_num_index_enabled() {
            let index = u32::try_from(index).ok()?;
            if index == u32::MAX {
                return None;
            }
            return self
                .numeric_index
                .as_ref()?
                .get(&index)
                .copied()
                .map(|slot| slot as usize);
        }
        let mut buf = [0u8; 20];
        self.pos(index_key(&mut buf, index))
    }

    /// Maintain the index across the append of `keys`' LAST entry: insert it
    /// when the index exists, build the index when the map just reached the
    /// threshold. Every structural append (set/define) funnels through here.
    #[inline]
    fn index_appended(&mut self) {
        let slot = self.keys.len() - 1;
        if sparse_num_index_enabled() {
            if let Some(n) = canonical_array_index_key(&self.keys[slot]) {
                self.numeric_index
                    .get_or_insert_with(|| Box::new(rustc_hash::FxHashMap::default()))
                    .insert(n, slot as u32);
            }
        }
        if let Some(ix) = &mut self.index {
            ix.insert(&self.keys[slot], slot as u32);
        } else if self.keys.len() >= PROP_INDEX_THRESHOLD {
            self.index = Some(PropIndex::build(&self.keys));
        }
    }

    /// The raw stored value for `key` (a data value, or an accessor's getter).
    /// Callers that must honour accessors check `attrs[i].accessor` first.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.pos(key).map(|i| self.vals[i])
    }

    /// Set `key = val` as a DATA property. Returns `true` if a NEW key was
    /// appended (which may have reallocated `vals`), `false` if an existing slot
    /// was overwritten. New keys get default data attributes; existing keys keep
    /// their attributes (only the value changes). The JIT inline cache uses the
    /// return to bump the object's version on a key-add.
    pub fn set(&mut self, key: &str, val: Value) -> bool {
        self.planned_append_failed |= self.keys.is_planned();
        if let Some(i) = self.pos(key) {
            self.vals[i] = val;
            false
        } else {
            self.keys.push(key.to_string());
            self.vals.push(val);
            self.attrs.push(PropAttr::data());
            self.shape_pushed(key, &PropAttr::data());
            self.has_element_key |= key_names_element(key);
            self.index_appended();
            true
        }
    }

    /// [`ObjMap::set`] for a caller that already OWNS the key string: on the
    /// first-insertion path the `String` moves into `keys` instead of being cloned
    /// out of a `&str` and dropped.
    ///
    /// Same contract as `set` in every respect — data attributes for a new key,
    /// attributes preserved on overwrite, `true` iff a key was appended (so the
    /// caller still bumps the object's version, since an append can realloc
    /// `vals`). A duplicate key overwrites the existing slot IN PLACE, keeping its
    /// original insertion position, and the incoming `String` is dropped.
    ///
    /// `JSON.parse` is the motivating caller: it builds `Vec<(String, Value)>` and
    /// then handed each key to `set` as a `&str`, so every first insertion
    /// allocated a second copy of a string the parser had already allocated.
    pub fn set_owned(&mut self, key: String, val: Value) -> bool {
        self.planned_append_failed |= self.keys.is_planned();
        if let Some(i) = self.pos(&key) {
            self.vals[i] = val;
            false
        } else {
            self.push_data(key, val);
            true
        }
    }

    /// Define `key` with explicit attributes (`Object.defineProperty`, or a method
    /// with non-default enumerability). Overwrites any existing slot. Returns
    /// `true` if a new key was appended.
    pub fn define(&mut self, key: &str, val: Value, attr: PropAttr) -> bool {
        self.planned_append_failed |= self.keys.is_planned();
        if let Some(i) = self.pos(key) {
            // Redefining an EXISTING property's attributes is not an append, so
            // the transition tree cannot describe the result — unless nothing
            // shape-relevant actually changed, which is the common case
            // (`Object.defineProperty` re-stating the same flags, or only the
            // setter half of an accessor moving).
            let old = self.attrs.at(i);
            let changed = old.writable != attr.writable
                || old.enumerable != attr.enumerable
                || old.configurable != attr.configurable
                || old.accessor != attr.accessor;
            self.vals[i] = val;
            // Raw slot write (shape handled below); materializes only when the
            // incoming attributes actually deviate from the stored defaults.
            if !(matches!(self.attrs, PropAttrs::AllData { .. }) && PropAttrs::is_default(&attr)) {
                self.attrs.materialize()[i] = attr;
            }
            if changed {
                self.shape_to_dict();
            }
            false
        } else {
            self.keys.push(key.to_string());
            self.vals.push(val);
            self.attrs.push(attr);
            self.shape_pushed(key, &attr);
            self.has_element_key |= key_names_element(key);
            self.index_appended();
            true
        }
    }

    /// Append a NEW data property the caller has already proven absent (via
    /// `pos`); consumes the key, skipping `set`'s re-lookup and re-clone. The
    /// caller MUST bump the object's version (a key add reallocs `vals`).
    pub fn push_data(&mut self, key: String, val: Value) {
        self.planned_append_failed |= self.keys.is_planned();
        self.shape_pushed_owned(&key, &PropAttr::data());
        self.has_element_key |= key_names_element(&key);
        self.keys.push(key);
        self.vals.push(val);
        self.attrs.push(PropAttr::data());
        self.index_appended();
    }

    /// Append a compiler-proved static data property without allocating or
    /// cloning its name when it is the next key in this object's plan. Any
    /// mismatch (including malformed bytecode) falls back to the exact owned
    /// append path, materializing only the already-visible prefix.
    pub fn push_static_data(&mut self, key: &str, val: Value) {
        if !self.planned_append_failed && self.keys.advance_if_next(key) {
            self.vals.push(val);
            self.attrs.push(PropAttr::data());
            self.shape_pushed(key, &PropAttr::data());
            self.has_element_key |= key_names_element(key);
            self.index_appended();
            return;
        }
        if self.keys.is_planned() {
            self.planned_append_failed = true;
        }
        if self.planned_append_failed {
            self.define(key, val, PropAttr::data());
        } else {
            // Legacy NewObject + compiler-proved AppendDataProp: retain the
            // historical no-lookup push used by the OFF comparator.
            self.push_data(key.to_string(), val);
        }
    }

    /// Remove `key`'s own property; returns whether it existed. Shifts later
    /// slots, so the caller MUST bump the object's version (a JIT inline cache
    /// may have recorded a now-stale slot index for another key).
    pub fn remove(&mut self, key: &str) -> bool {
        match self.pos(key) {
            Some(i) => {
                self.remove_at(i);
                true
            }
            None => false,
        }
    }

    /// `remove` with the slot ALREADY resolved. Split out (W19 M2) so a caller
    /// that has just run `pos()` for its own reasons -- the ordinary-object
    /// delete fast path, which must read `attrs[i].configurable` first -- does
    /// not pay a second hash lookup to remove the same key. Pure refactor: the
    /// body below is `remove`'s former body verbatim, in the same order.
    ///
    /// `i` MUST be a live slot index (`i < self.keys.len()`).
    pub fn remove_at(&mut self, i: usize) {
        // A hole in the middle of the sequence: every later property's slot
        // shifts down, so no shape in the tree describes the result.
        self.shape_to_dict();
        self.planned_append_failed |= self.keys.is_planned();
        let key = self.keys.remove(i);
        self.vals.remove(i);
        self.attrs.remove(i);
        // Removing the last element-naming key must clear the bit, or a
        // `delete arr[0]` would leave every dense fast path shut off for the
        // life of the object. Recompute rather than count: deletion is rare
        // and a stale count is a silent wrong answer.
        if self.has_element_key && key_names_element(&key) {
            self.has_element_key = self.keys.iter().any(|k| key_names_element(k));
        }
        if let Some(numeric) = &mut self.numeric_index {
            if let Some(n) = canonical_array_index_key(&key) {
                numeric.remove(&n);
            }
            // `Vec::remove` shifted every later property down by one. Deletes
            // are cold; updating the compact u32 slots in place avoids hashing
            // every surviving decimal key again.
            for slot in numeric.values_mut() {
                if (*slot as usize) > i {
                    *slot -= 1;
                }
            }
        }
        if let Some(ix) = &mut self.index {
            if self.keys.len() < PROP_INDEX_THRESHOLD / 2 {
                self.index = None;
            } else {
                ix.remove(&key, i as u32);
            }
        }
    }
}

/// A flat (contiguous) JS string with cached metadata so `.length` and indexing
/// are O(1) for the common all-ASCII case. `bytes` holds WTF-8: well-formed
/// UTF-8 for the overwhelmingly common case, PLUS lone surrogates (which JS
/// strings can contain but UTF-8 prohibits) encoded as the 3-byte sequence
/// UTF-8 *would* use for their code point: `0xED 0xA0-0xBF 0x80-0xBF` (high
/// halves `0xED 0xA0-0xAF ..`, low halves `0xED 0xB0-0xBF ..`). The buffer is
/// kept CANONICAL — an encoded high surrogate is never immediately followed by
/// an encoded low surrogate (that pair is always stored as the astral scalar's
/// 4-byte encoding; see `wtf8_push`/`wtf8_push_cp`) — so byte equality remains
/// content equality. The fields are PRIVATE: every access funnels through the
/// accessors below, which decode WTF-8 (never `from_utf8_unchecked` over
/// surrogate bytes — that would be UB through `str`'s validity invariant).
/// `units` caches the length in UTF-16 CODE UNITS — the measure of every
/// JS-observable string position (`.length`, `charCodeAt`, `slice`, …); a lone
/// surrogate is 1 unit, an astral scalar 2. `ascii` flags the all-ASCII case,
/// where the i-th unit is the i-th byte — O(1) random access. `wellformed`
/// (no lone surrogates ⇔ the bytes are valid UTF-8) is computed once at
/// construction; only well-formed strings may be viewed as `&str`.
#[derive(Clone, Debug)]
pub struct JsStr {
    bytes: Vec<u8>,
    units: usize,
    ascii: bool,
    wellformed: bool,
}

/// UTF-16 code units contributed by one Unicode scalar: 1 for BMP, 2 for an
/// astral (supplementary-plane) scalar. This is THE unit/scalar switch — every
/// positional helper below counts through it.
#[inline]
pub fn char_units(c: char) -> usize {
    c.len_utf16()
}

/// UTF-16 code-unit length of a well-formed `&str`.
pub fn str_units(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.chars().map(char_units).sum()
    }
}

/// Unit position of char-boundary byte offset `b` in `s` (clamped to the end).
pub fn byte_to_units(s: &str, b: usize) -> usize {
    str_units(&s[..b.min(s.len())])
}

/// Resolve unit position `u` in `s` to byte offsets, clamped to the end:
/// `(floor, ceil)` are equal at a scalar boundary; a `u` that lands BETWEEN the
/// halves of a surrogate pair gives the enclosing astral scalar's (start, end).
pub fn unit_byte_bounds(s: &str, u: usize) -> (usize, usize) {
    if u == 0 {
        return (0, 0);
    }
    let mut units = 0usize;
    for (b, c) in s.char_indices() {
        if units == u {
            return (b, b);
        }
        let n = char_units(c);
        if units + n > u {
            // `u` addresses this scalar's trail half (only possible when n == 2).
            return (b, b + c.len_utf8());
        }
        units += n;
    }
    (s.len(), s.len())
}

/// Byte offset of unit position `u`, rounding a mid-pair position UP to the next
/// scalar boundary — exact for SEARCH-START positions (a well-formed needle can
/// never match starting at a trail unit). Anchored positions (`startsWith`/
/// `endsWith`/`lastIndexOf` caps) use `unit_byte_bounds` to detect the split.
pub fn unit_to_byte(s: &str, u: usize) -> usize {
    unit_byte_bounds(s, u).1
}

// ── WTF-8 primitives ──
// The byte-level helpers every accessor builds on. All of them treat the
// surrogate range exactly like any other 3-byte sequence; none of them ever
// construct a `&str` over the bytes.

/// UTF-16 code units contributed by code point `cp` (1 for BMP — including a
/// lone surrogate — 2 for an astral scalar).
#[inline]
pub fn cp_units(cp: u32) -> usize {
    if cp < 0x10000 {
        1
    } else {
        2
    }
}

/// The `off`-th UTF-16 unit of code point `cp` (`off` 0 = the code point
/// itself / the high surrogate; 1 = the low surrogate of an astral scalar).
#[inline]
fn unit_of_cp(cp: u32, off: usize) -> u16 {
    if cp < 0x10000 {
        cp as u16
    } else {
        let v = cp - 0x10000;
        if off == 0 {
            0xD800 | (v >> 10) as u16
        } else {
            0xDC00 | (v & 0x3FF) as u16
        }
    }
}

/// Decode the code point whose encoding starts at byte `i` of WTF-8 buffer `b`
/// (which must be a lead position of a valid sequence — the engine only builds
/// valid WTF-8). Returns `(code point, encoded byte length)`. A surrogate
/// decodes like any 3-byte sequence — the one place WTF-8 differs from UTF-8.
#[inline]
pub fn wtf8_decode(b: &[u8], i: usize) -> (u32, usize) {
    let b0 = b[i] as u32;
    if b0 < 0x80 {
        (b0, 1)
    } else if b0 < 0xE0 {
        (((b0 & 0x1F) << 6) | (b[i + 1] as u32 & 0x3F), 2)
    } else if b0 < 0xF0 {
        (
            (((b0 & 0x0F) << 12) | ((b[i + 1] as u32 & 0x3F) << 6)) | (b[i + 2] as u32 & 0x3F),
            3,
        )
    } else {
        (
            ((b0 & 0x07) << 18)
                | ((b[i + 1] as u32 & 0x3F) << 12)
                | ((b[i + 2] as u32 & 0x3F) << 6)
                | (b[i + 3] as u32 & 0x3F),
            4,
        )
    }
}

/// UTF-16 unit count of a WTF-8 buffer: one per lead byte, plus one extra per
/// 4-byte (astral) sequence. A 3-byte lone-surrogate encoding counts 1.
pub fn wtf8_units(b: &[u8]) -> usize {
    b.iter()
        .map(|&x| ((x & 0xC0) != 0x80) as usize + (x >= 0xF0) as usize)
        .sum()
}

/// Whether WTF-8 buffer `b` contains NO surrogate encodings — for engine-built
/// buffers this is exactly "the bytes are valid UTF-8".
pub fn wtf8_is_wellformed(b: &[u8]) -> bool {
    !b.windows(2)
        .any(|w| w[0] == 0xED && (0xA0..=0xBF).contains(&w[1]))
}

/// Raw WTF-8 encode of `cp` (a surrogate allowed) onto `out` — NO seam
/// canonicalization (use `wtf8_push_cp` when `cp` may be a low surrogate
/// completing a trailing high).
fn push_cp_raw(out: &mut Vec<u8>, cp: u32) {
    if cp < 0x80 {
        out.push(cp as u8);
    } else if cp < 0x800 {
        out.push(0xC0 | (cp >> 6) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    } else if cp < 0x10000 {
        out.push(0xE0 | (cp >> 12) as u8);
        out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    } else {
        out.push(0xF0 | (cp >> 18) as u8);
        out.push(0x80 | ((cp >> 12) & 0x3F) as u8);
        out.push(0x80 | ((cp >> 6) & 0x3F) as u8);
        out.push(0x80 | (cp & 0x3F) as u8);
    }
}

/// Push code point `cp` (a surrogate allowed) onto WTF-8 buffer `out`,
/// CANONICALIZING: a low surrogate that completes a trailing high surrogate
/// merges into the astral scalar's 4-byte encoding — exactly the JS rule that
/// `'\uD800' + '\uDC00'` is the 1-code-point string `'\u{10000}'`.
pub fn wtf8_push_cp(out: &mut Vec<u8>, cp: u32) {
    if (0xDC00..=0xDFFF).contains(&cp) {
        let n = out.len();
        if n >= 3 && out[n - 3] == 0xED && (0xA0..=0xAF).contains(&out[n - 2]) {
            let (hi, _) = wtf8_decode(out, n - 3);
            out.truncate(n - 3);
            push_cp_raw(out, 0x10000 + ((hi - 0xD800) << 10) + (cp - 0xDC00));
            return;
        }
    }
    push_cp_raw(out, cp);
}

/// Append WTF-8 `seg` onto WTF-8 `out`, canonicalizing the SEAM: a trailing
/// high surrogate in `out` followed by a leading low surrogate in `seg` merges
/// into the astral 4-byte encoding (unit count is unaffected: 1+1 halves = the
/// astral scalar's 2 units, so rope length math stays additive). The common
/// case bails on one byte compare (an ASCII tail can't end a surrogate).
pub fn wtf8_push(out: &mut Vec<u8>, seg: &[u8]) {
    let n = out.len();
    if n >= 3
        && seg.len() >= 3
        && *out.last().unwrap() >= 0x80
        && out[n - 3] == 0xED
        && (0xA0..=0xAF).contains(&out[n - 2])
        && seg[0] == 0xED
        && (0xB0..=0xBF).contains(&seg[1])
    {
        let (hi, _) = wtf8_decode(out, n - 3);
        let (lo, _) = wtf8_decode(seg, 0);
        out.truncate(n - 3);
        push_cp_raw(out, 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00));
        out.extend_from_slice(&seg[3..]);
        return;
    }
    out.extend_from_slice(seg);
}

/// Iterate the UTF-16 code units of a WTF-8 buffer: BMP code points (including
/// lone surrogates) yield their own value; an astral scalar yields its two
/// halves. This is the UTF-16 view every JS string comparison/order is defined
/// over.
pub fn wtf8_units_iter(b: &[u8]) -> impl Iterator<Item = u16> + '_ {
    let mut i = 0usize;
    let mut low: Option<u16> = None;
    std::iter::from_fn(move || {
        if let Some(u) = low.take() {
            return Some(u);
        }
        if i >= b.len() {
            return None;
        }
        let (cp, len) = wtf8_decode(b, i);
        i += len;
        if cp >= 0x10000 {
            low = Some(unit_of_cp(cp, 1));
            Some(unit_of_cp(cp, 0))
        } else {
            Some(cp as u16)
        }
    })
}

/// Iterate the code points of a WTF-8 buffer (a lone surrogate yields its
/// surrogate value 0xD800–0xDFFF — NOT a `char`, which can't hold it).
pub fn wtf8_code_points(b: &[u8]) -> impl Iterator<Item = u32> + '_ {
    let mut i = 0usize;
    std::iter::from_fn(move || {
        if i >= b.len() {
            return None;
        }
        let (cp, len) = wtf8_decode(b, i);
        i += len;
        Some(cp)
    })
}

/// Owned LOSSY `String` of a WTF-8 buffer: each lone-surrogate triple becomes
/// U+FFFD. Both encodings are 3 bytes, so byte offsets and unit positions in
/// the lossy form are IDENTICAL to the exact form — position math computed on
/// the lossy view remains valid for the WTF-8 original.
pub fn wtf8_to_lossy_string(b: &[u8]) -> String {
    wtf8_into_lossy_string(b.to_vec())
}

/// Decode oxc's lone-surrogate marker encoding into WTF-8 bytes. The parser
/// cooks a string/template literal containing lone-surrogate escapes (e.g.
/// `'\uD800'`) into text where each lone surrogate is the 5-char marker
/// `\u{FFFD}XXXX` (4 lowercase hex = the code unit) and a LITERAL U+FFFD is
/// `\u{FFFD}fffd`, setting `.lone_surrogates` on the AST node. Only flagged
/// literals are decoded (an unflagged string may contain genuine U+FFFD +
/// hex-looking text). Output is canonical WTF-8 via `wtf8_push_cp`.
pub fn decode_lone_surrogate_markers(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\u{FFFD}' {
            let rest = it.as_str();
            if rest.len() >= 4 && rest.as_bytes()[..4].iter().all(|b| b.is_ascii_hexdigit()) {
                let cu = u32::from_str_radix(&rest[..4], 16).unwrap();
                for _ in 0..4 {
                    it.next();
                }
                wtf8_push_cp(&mut out, cu);
                continue;
            }
            // Defensive: an unmarked U+FFFD (oxc escapes them all when the
            // flag is set) passes through literally.
        }
        wtf8_push_cp(&mut out, c as u32);
    }
    out
}

/// Inverse of [`decode_lone_surrogate_markers`]: encode WTF-8 bytes into the
/// oxc lone-surrogate MARKER form — each lone surrogate becomes the 5-char
/// marker `\u{FFFD}XXXX` (4 lowercase hex = the code unit) and a LITERAL
/// U+FFFD becomes `\u{FFFD}fffd` (so an unmarked U+FFFD can never be
/// mistaken for a marker). Used when the compiler recovers exact pattern
/// bytes for a regex literal whose lossy parse text contained U+FFFD: the
/// result feeds `add_string_const_wtf8`, and `resolve_const`'s decode
/// round-trips it back to these exact bytes.
pub fn encode_lone_surrogate_markers(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len() + 8);
    for cp in wtf8_code_points(b) {
        match char::from_u32(cp) {
            // A lone surrogate (no `char` exists for it) → marker.
            None => {
                out.push('\u{FFFD}');
                out.push_str(&format!("{cp:04x}"));
            }
            Some('\u{FFFD}') => out.push_str("\u{FFFD}fffd"),
            Some(c) => out.push(c),
        }
    }
    out
}

/// Consuming form of [`wtf8_to_lossy_string`] (patches the buffer in place).
pub fn wtf8_into_lossy_string(mut v: Vec<u8>) -> String {
    let mut i = 0;
    while i + 2 < v.len() {
        if v[i] == 0xED && (0xA0..=0xBF).contains(&v[i + 1]) {
            // U+FFFD's UTF-8 encoding, also 3 bytes.
            v[i] = 0xEF;
            v[i + 1] = 0xBF;
            v[i + 2] = 0xBD;
            i += 3;
        } else {
            i += 1;
        }
    }
    // Engine-built buffers are valid UTF-8 after the patch; degrade any
    // unexpected residue through the checked lossy path rather than UB.
    match String::from_utf8(v) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(&e.into_bytes()).into_owned(),
    }
}

impl JsStr {
    /// Construct from a (necessarily well-formed) Rust `String` — the common
    /// path for every string the engine builds out of `&str` material.
    pub fn new(bytes: String) -> JsStr {
        let ascii = bytes.is_ascii();
        let units = if ascii {
            bytes.len()
        } else {
            str_units(&bytes)
        };
        JsStr {
            bytes: bytes.into_bytes(),
            units,
            ascii,
            wellformed: true,
        }
    }

    /// Construct from WTF-8 bytes (the creation sites that can produce lone
    /// surrogates: literal marker decode, fromCharCode/fromCodePoint,
    /// JSON.parse, slicing, rope flattening). The buffer must be valid,
    /// CANONICAL WTF-8 — every producer in the engine builds it through
    /// `wtf8_push`/`wtf8_push_cp`/`slice_units`, which maintain that.
    /// W11 (B124): construct from bytes the CALLER proves are ASCII — skips
    /// `from_wtf8`'s linear rescan. The one caller class is a slice of a
    /// known-ascii subject (a slice of ascii is ascii by construction);
    /// regex-log-scan takes ~1.8M such slices per run.
    pub fn from_ascii(bytes: Vec<u8>) -> JsStr {
        debug_assert!(bytes.is_ascii(), "from_ascii: caller's ascii proof failed");
        JsStr {
            units: bytes.len(),
            bytes,
            ascii: true,
            wellformed: true,
        }
    }

    pub fn from_wtf8(bytes: Vec<u8>) -> JsStr {
        if bytes.is_ascii() {
            return JsStr {
                units: bytes.len(),
                bytes,
                ascii: true,
                wellformed: true,
            };
        }
        let wellformed = wtf8_is_wellformed(&bytes);
        debug_assert!(
            !wellformed || std::str::from_utf8(&bytes).is_ok(),
            "from_wtf8: surrogate-free buffer must be valid UTF-8"
        );
        debug_assert!(
            {
                // Canonical form: no encoded high surrogate immediately
                // followed by an encoded low surrogate.
                !bytes.windows(6).any(|w| {
                    w[0] == 0xED
                        && (0xA0..=0xAF).contains(&w[1])
                        && w[3] == 0xED
                        && (0xB0..=0xBF).contains(&w[4])
                })
            },
            "from_wtf8: non-canonical surrogate pair encoding"
        );
        let units = wtf8_units(&bytes);
        JsStr {
            bytes,
            units,
            ascii: false,
            wellformed,
        }
    }

    /// A 1-code-point string (`cp` may be a lone surrogate).
    pub fn from_code_point(cp: u32) -> JsStr {
        let mut v = Vec::with_capacity(4);
        push_cp_raw(&mut v, cp);
        JsStr::from_wtf8(v)
    }

    /// The content as `&str` — ONLY for well-formed strings (the type's
    /// validity invariant forbids surrogate bytes). Callers that can see a
    /// lone-surrogate string use `as_str_lossy` (observation paths: display,
    /// parsing, regex input, …) or the WTF-8 accessors (exact paths).
    /// Panics on a non-well-formed string — every call site must guarantee
    /// well-formedness (e.g. just constructed from `&str` material).
    #[allow(dead_code)]
    #[inline]
    pub fn as_str_wf(&self) -> &str {
        assert!(
            self.wellformed,
            "as_str_wf on a string containing lone surrogates"
        );
        // SAFETY: `wellformed` records that `bytes` holds no surrogate
        // encodings; the bytes otherwise originate from safe `String`s or the
        // engine's WTF-8 encoders, so they are valid UTF-8 (checked by
        // `from_wtf8`'s debug assertion).
        #[cfg(feature = "safe-sandbox")]
        return std::str::from_utf8(&self.bytes).expect("well-formed JsStr invariant");
        #[cfg(not(feature = "safe-sandbox"))]
        return unsafe { std::str::from_utf8_unchecked(&self.bytes) };
    }

    /// The content as `&str`, LOSSY for the lone-surrogate case (each lone
    /// surrogate reads as U+FFFD — same byte length, so positions computed on
    /// the lossy view remain valid for the exact bytes). Borrowed (free) for
    /// well-formed strings — the overwhelmingly common case.
    #[inline]
    pub fn as_str_lossy(&self) -> Cow<'_, str> {
        if self.wellformed {
            // SAFETY: as in `as_str_wf` — `wellformed` ⇒ valid UTF-8.
            #[cfg(feature = "safe-sandbox")]
            return Cow::Borrowed(
                std::str::from_utf8(&self.bytes).expect("well-formed JsStr invariant"),
            );
            #[cfg(not(feature = "safe-sandbox"))]
            return Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(&self.bytes) });
        } else {
            Cow::Owned(wtf8_to_lossy_string(&self.bytes))
        }
    }

    /// Owned lossy `String` (see `as_str_lossy`).
    pub fn to_lossy_string(&self) -> String {
        self.as_str_lossy().into_owned()
    }

    /// The raw WTF-8 bytes. NOT necessarily valid UTF-8 — never view them as
    /// `&str`; decode with the `wtf8_*` helpers.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Length in UTF-16 code units — the JS `.length`.
    #[inline]
    pub fn units(&self) -> usize {
        self.units
    }

    #[inline]
    pub fn is_ascii(&self) -> bool {
        self.ascii
    }

    /// No lone surrogates (`String.prototype.isWellFormed`) — O(1), computed
    /// at construction.
    #[inline]
    pub fn is_wellformed(&self) -> bool {
        self.wellformed
    }

    /// Reserve raw backing bytes for a caller that has already proved the
    /// appended pieces' exact byte lengths. Metadata is unchanged until the
    /// subsequent `push_*` calls. Used by the proven-linear right-pair append
    /// to make both leaves share one capacity growth.
    #[inline]
    pub(crate) fn reserve_bytes(&mut self, additional: usize) {
        self.bytes.reserve(additional);
    }

    /// Append one ASCII byte (the `s += digit` fast path), updating metadata.
    #[inline]
    pub fn push_ascii(&mut self, b: u8) {
        debug_assert!(b < 128);
        self.bytes.push(b);
        self.units += 1;
    }

    #[cfg(test)]
    pub(crate) fn byte_capacity(&self) -> usize {
        self.bytes.capacity()
    }

    /// Append a well-formed string, updating the cached metadata. No seam
    /// canonicalization is needed: a `&str` can never START with a low
    /// surrogate, so a trailing high surrogate in `self` stays lone.
    /// (Currently unreferenced — the append path goes through `push_wtf8` —
    /// kept as the natural `&str` entry of the accessor layer.)
    #[allow(dead_code)]
    pub fn push_str(&mut self, add: &str) {
        self.units += str_units(add);
        self.ascii &= add.is_ascii();
        self.bytes.extend_from_slice(add.as_bytes());
    }

    /// Append WTF-8 bytes (an exact `+=` of another string's content),
    /// canonicalizing the seam. Unit length stays additive across the merge.
    pub fn push_wtf8(&mut self, add: &[u8]) {
        let add_ascii = add.is_ascii();
        self.units += if add_ascii {
            add.len()
        } else {
            wtf8_units(add)
        };
        if self.wellformed && (add_ascii || wtf8_is_wellformed(add)) {
            // Surrogate-free on both sides: plain append, no seam possible.
            self.ascii &= add_ascii;
            self.bytes.extend_from_slice(add);
        } else {
            self.ascii = false;
            wtf8_push(&mut self.bytes, add);
            self.wellformed = wtf8_is_wellformed(&self.bytes);
        }
    }

    /// Locate unit position `i`: the code point containing it and `i`'s offset
    /// within that code point's units (0 = lead, 1 = the trail of an astral
    /// pair). O(1) for ASCII, O(i) otherwise.
    fn locate_unit(&self, i: usize) -> Option<(u32, usize)> {
        if self.ascii {
            return self.bytes.get(i).map(|&b| (b as u32, 0));
        }
        if i >= self.units {
            return None;
        }
        let mut pos = 0usize;
        let mut bi = 0usize;
        while bi < self.bytes.len() {
            let (cp, blen) = wtf8_decode(&self.bytes, bi);
            let n = cp_units(cp);
            if i < pos + n {
                return Some((cp, i - pos));
            }
            pos += n;
            bi += blen;
        }
        None
    }

    /// The UTF-16 code unit at unit position `i` (a lone surrogate's own
    /// value; a surrogate half for an astral scalar) — `charCodeAt` semantics.
    pub fn unit_at(&self, i: usize) -> Option<u16> {
        self.locate_unit(i).map(|(cp, off)| unit_of_cp(cp, off))
    }

    /// CodePointAt(unit position) per spec: the FULL code point when `i`
    /// addresses a lead unit, the trail surrogate's value mid-pair, and a lone
    /// surrogate's own value.
    pub fn code_point_at(&self, i: usize) -> Option<u32> {
        self.locate_unit(i).map(|(cp, off)| {
            if off == 0 {
                cp
            } else {
                unit_of_cp(cp, 1) as u32
            }
        })
    }

    /// Substring by UNIT positions `[a, b)`. A bound that splits a surrogate
    /// pair yields the REAL covered half (a 1-unit lone-surrogate string).
    /// The output of slicing a canonical buffer is canonical: a low half cut
    /// from one scalar can only be FOLLOWED by what followed that scalar, and
    /// a high half can only END the slice.
    pub fn slice_units(&self, a: usize, b: usize) -> JsStr {
        if self.ascii {
            let (a, b) = (a.min(self.bytes.len()), b.min(self.bytes.len()));
            return JsStr::from_wtf8(if a >= b {
                Vec::new()
            } else {
                self.bytes[a..b].to_vec()
            });
        }
        let mut out: Vec<u8> = Vec::new();
        if a < b {
            let (mut pos, mut bi) = (0usize, 0usize);
            while bi < self.bytes.len() && pos < b {
                let (cp, blen) = wtf8_decode(&self.bytes, bi);
                let n = cp_units(cp);
                if pos >= a && pos + n <= b {
                    out.extend_from_slice(&self.bytes[bi..bi + blen]);
                } else if n == 2 && pos >= a && pos < b {
                    // Window covers only the lead half.
                    push_cp_raw(&mut out, unit_of_cp(cp, 0) as u32);
                } else if n == 2 && pos + 1 >= a && pos + 1 < b {
                    // Window covers only the trail half.
                    push_cp_raw(&mut out, unit_of_cp(cp, 1) as u32);
                }
                pos += n;
                bi += blen;
            }
        }
        JsStr::from_wtf8(out)
    }

    /// Iterate the code points (for-of/spread semantics — one item per code
    /// point; a lone surrogate yields its 0xD800–0xDFFF value).
    pub fn code_points(&self) -> impl Iterator<Item = u32> + '_ {
        wtf8_code_points(&self.bytes)
    }

    /// Iterate the UTF-16 code units (split('') / string-spread semantics —
    /// an astral scalar contributes its two halves).
    pub fn units_iter(&self) -> impl Iterator<Item = u16> + '_ {
        wtf8_units_iter(&self.bytes)
    }

    /// One for-of step at unit position `pos`: the code point starting there
    /// and the position one CODE POINT later (units advance by 1 or 2). `None`
    /// once past the end.
    pub fn cp_step(&self, pos: usize) -> Option<(u32, usize)> {
        self.locate_unit(pos)
            .map(|(cp, off)| (cp, pos - off + cp_units(cp)))
    }
}

/// A generator's execution state. `Suspended(ip)` parks at the bytecode index of
/// the `Yield` that paused it (resume re-decodes that op to deliver the sent
/// value into its `dst`, then continues at `ip + 1`); `ip == 0` is the
/// not-yet-started state (the first `next()` runs from the top).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenState {
    Suspended(usize),
    Running,
    Completed,
}

/// An active `try` handler in a frame, innermost last. A `Catch` lands a thrown
/// value in `reg` and jumps to `target`. A `Finally` is visited on EVERY exit
/// from its protected region — throw, `return`, or normal completion — running
/// the finally block (at `target`) with a completion record deposited into
/// `kind_reg` (0 normal, 1 return, 2 throw) and `val_reg` (the return value /
/// thrown reason), which `EndFinally` then resumes.
#[derive(Clone, Copy, Debug)]
pub enum Handler {
    Catch {
        target: u32,
        reg: u16,
    },
    Finally {
        target: u32,
        kind_reg: u16,
        val_reg: u16,
    },
}

/// A Promise's settlement state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromiseState {
    Pending,
    Fulfilled,
    Rejected,
}

/// Which Promise combinator a `Combinator` is tracking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombKind {
    /// `Promise.all` — fulfil with all values, or reject on the first rejection.
    All,
    /// `Promise.allSettled` — fulfil with `{status, value|reason}` records.
    AllSettled,
    /// `Promise.race` — settle as the first input settles.
    Race,
    /// `Promise.any` — first fulfilment, or an AggregateError if all reject.
    Any,
    /// `Promise.allKeyed` — like `all`, but over an object's own enumerable keys;
    /// fulfils with a null-prototype object mapping each key to its value.
    AllKeyed,
    /// `Promise.allSettledKeyed` — like `allSettled` over an object's keys; fulfils
    /// with a null-prototype object mapping each key to its `{status, …}` record.
    AllSettledKeyed,
}

/// One SUBSCRIPTION to a pending promise: both handlers registered together.
///
/// When the promise settles, the handler matching the settlement runs as a
/// microtask and its outcome settles `dependent`. An `undefined` handler is a
/// pass-through (the value/reason forwards to `dependent` unchanged) -- that is
/// how the spec's Identity and Thrower defaults are represented here, so they
/// cost nothing rather than being real function objects.
///
/// The two handlers are ONE record because every site that registers them does
/// so in a pair: `.then(f)` supplies `f` and a pass-through rejection,
/// `.catch(g)` the reverse, and `await` a pair of async resumes. They used to
/// live in two parallel `Vec<Reaction>` on the promise, which made the common
/// single-subscriber case allocate two buffers to store two halves of this.
#[derive(Clone, Debug)]
pub struct ReactionPair {
    pub on_fulfilled: Value,
    pub on_rejected: Value,
    pub dependent: u32,
    /// A `.finally(cb)` reaction: run the handler (no args) for its side effect,
    /// then forward the ORIGINAL value/reason (a throw in it overrides).
    pub finally: bool,
    /// An `await` subscription: `dependent` is the suspended async ACTIVATION's
    /// heap index, resumed (value or thrown rejection) instead of running a
    /// callback.
    pub is_async: bool,
    /// Allocation-free intrinsic `Promise.all` fulfilment target. `dependent`
    /// is the `Combinator`, `on_fulfilled` is its non-negative element index
    /// encoded as an Int, and `on_rejected` remains the native result
    /// capability's reject function. Settlement still queues one FIFO job.
    pub is_combinator_all: bool,
}

/// A pending promise's subscriber list.
///
/// `One` is the case that matters and it holds the subscription INLINE, with no
/// heap allocation: a `.then` chain link, an `await`, and every `Promise.all`
/// element subscribe exactly once. `Many` appears only when the same promise is
/// subscribed to more than once, and then allocates once rather than twice.
///
/// Insertion order is the observable part -- settlement drains in registration
/// order onto a FIFO microtask queue, which is what fixes promise tick ordering
/// -- so `push` appends and `One -> Many` keeps the first record first.
#[derive(Clone, Debug, Default)]
pub enum Reactions {
    #[default]
    None,
    One(ReactionPair),
    Many(Vec<ReactionPair>),
}

impl Reactions {
    #[inline]
    pub fn push(&mut self, r: ReactionPair) {
        match self {
            Reactions::None => *self = Reactions::One(r),
            Reactions::One(_) => {
                let Reactions::One(first) = std::mem::take(self) else {
                    unreachable!("matched One")
                };
                *self = Reactions::Many(vec![first, r]);
            }
            Reactions::Many(v) => v.push(r),
        }
    }

    /// Borrowed view for tracing. `One` is a one-element slice via
    /// `slice::from_ref`, so the GC walks all three shapes with one loop.
    #[inline]
    pub fn as_slice(&self) -> &[ReactionPair] {
        match self {
            Reactions::None => &[],
            Reactions::One(r) => std::slice::from_ref(r),
            Reactions::Many(v) => v.as_slice(),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        matches!(self, Reactions::None)
    }
}

/// By-value drain for settlement. Deliberately NOT `into_vec()`: that would
/// allocate a `Vec` at settle time to replace the allocations this type exists
/// to remove, turning two allocations per subscription into one per settlement
/// instead of none.
pub enum ReactionsIter {
    Done,
    One(ReactionPair),
    Many(std::vec::IntoIter<ReactionPair>),
}

impl Iterator for ReactionsIter {
    type Item = ReactionPair;

    #[inline]
    fn next(&mut self) -> Option<ReactionPair> {
        match std::mem::replace(self, ReactionsIter::Done) {
            ReactionsIter::Done => None,
            ReactionsIter::One(r) => Some(r),
            ReactionsIter::Many(mut it) => {
                let next = it.next();
                *self = ReactionsIter::Many(it);
                next
            }
        }
    }
}

impl IntoIterator for Reactions {
    type Item = ReactionPair;
    type IntoIter = ReactionsIter;

    #[inline]
    fn into_iter(self) -> ReactionsIter {
        match self {
            Reactions::None => ReactionsIter::Done,
            Reactions::One(r) => ReactionsIter::One(r),
            Reactions::Many(v) => ReactionsIter::Many(v.into_iter()),
        }
    }
}

/// Boxed payload of a [`HeapObj::Class`] (see that variant's docs). Kept behind a
/// `Box` so the rarely-allocated class object — 8 fields incl. a `String`, three
/// `Vec`s and an `ObjMap` — does not inflate `size_of::<HeapObj>()` for the hot,
/// tiny variants (`Cons`/`Str`/`Array`/`Object`) that pay it on every alloc.
#[derive(Clone, Debug)]
pub struct ClassData {
    pub name: String,
    pub ctor: Option<u32>,
    /// Whether `ctor` is an explicit constructor (its body calls `super`
    /// itself) vs. a fields-only proto (the `new` path runs the parent ctor).
    pub has_explicit_ctor: bool,
    pub methods: Vec<(String, Value)>,
    /// `get x()` accessors, invoked with `this` = instance on property read.
    pub getters: Vec<(String, Value)>,
    /// `set x(v)` accessors, invoked with `this` = instance on property write.
    pub setters: Vec<(String, Value)>,
    /// Public instance prototype keys in SOURCE order (see `ClassDef::proto_order`)
    /// — the order `C.prototype`'s property map is built in, which the three
    /// kind-grouped lists above cannot express on their own.
    pub proto_order: Vec<String>,
    /// Static members — own properties of the class value (`C.method`,
    /// `C.field`). Methods start here; static fields are added by SetProp.
    pub statics: ObjMap,
    /// `static get`/`set` accessors, invoked with `this` = the class value on
    /// read/write of a static property.
    pub static_getters: Vec<(String, Value)>,
    pub static_setters: Vec<(String, Value)>,
    /// Heap index of the superclass value (`class C extends P`), for
    /// inherited method/getter lookup and `instanceof` up the chain.
    pub parent: Option<u32>,
    /// `class C extends null {}`: derived-class semantics (super required in
    /// an explicit ctor; implicit super throws) with a null prototype parent.
    pub extends_null: bool,
    /// Computed instance-field keys (`[expr] = v`), evaluated ONCE at class
    /// definition (in source order) and read per-instance by the `FieldInit` op
    /// during construction. Empty for classes with no computed instance fields.
    pub computed_field_keys: Vec<Value>,
    /// Exact `class … { … }` source text, for `Function.prototype.toString`.
    pub source: String,
    /// Upvalue cells captured by the constructor (incl. its field initializers)
    /// from the frame where the class was defined — supplied when `new` runs the
    /// ctor. Empty unless the class is nested in a function and its ctor/fields
    /// close over a local of that function.
    pub ctor_upvalues: Vec<u32>,
    /// Fields-initializer thunk for a DERIVED class with an explicit ctor: run
    /// by the SuperCtor ops on `this` right after `super()` completes (spec
    /// InitializeInstanceElements timing). `None` when the ctor layout carries
    /// entry inits (base/implicit classes) or there are no instance fields.
    pub field_thunk: Option<u32>,
    /// Upvalue cells for `field_thunk`, captured at MakeClass like
    /// `ctor_upvalues` (field initializers may close over enclosing locals).
    pub field_thunk_upvalues: Vec<u32>,
    /// A fresh per-EVALUATION private brand id, minted at MakeClass, giving each
    /// class evaluation a distinct private-name identity (so two classes that both
    /// declare `#m` don't collide). 0 = unbranded.
    pub private_brand: u64,
    /// The compile-time class id this value was materialized from — the same id
    /// `MakeClass` and the decorator ops carry. Lets running code identify WHICH
    /// evaluation of a class it belongs to (`class_values` only remembers the
    /// most recent one).
    pub class_id: u32,
    /// Live decoration state, allocated by `MakeClass` only when the class's
    /// `ClassDef` carries a decorator plan. `None` for every undecorated class
    /// (i.e. all of them today), so nothing on the hot class path pays for it.
    pub dec: Option<Box<DecState>>,
}

/// The per-EVALUATION decoration state of a decorated class: what the decorator
/// calls produced, read back by the field-initializer and extra-initializer ops.
///
/// Lives on `ClassData` rather than beside the compile-time `ClassDef` because
/// every entry here is a `Value` produced by USER code at class-definition time —
/// two evaluations of the same `class` source have different ones.
#[derive(Clone, Debug)]
pub struct DecState {
    /// Per decorated element (indexed by its position in
    /// `ClassDef::dec_plan.elements`): `elementRecord.[[Initializers]]`, i.e. the
    /// initializer chain the element's field / auto-accessor decorators returned.
    /// The spec PREPENDS each one, so the OUTERMOST decorator's initializer ends
    /// up first and runs first: `@a @b x = V` yields `b(a(V))`. Applied left to
    /// right to the field's initial value with `this` = the receiver. Empty for a
    /// method/getter/setter.
    pub field_inits: Vec<Vec<Value>>,
    /// Per decorated element: `elementRecord.[[ExtraInitializers]]` — the
    /// `addInitializer` callbacks of a FIELD or ACCESSOR element. These are
    /// per-element, not shared: InitializeFieldOrAccessor runs them with
    /// `this` = the receiver immediately AFTER that one element is defined, so a
    /// `@dec x` initializer sees `this.x` already set (and a later field not yet).
    pub elem_extra: Vec<Vec<Value>>,
    /// Per decorated element: the ToPropertyKey'd key, recorded when the element's
    /// ClassElementName is evaluated. A computed key is not knowable at compile
    /// time, and `context.name` must be the very Symbol/String the key evaluated to.
    pub keys: Vec<Value>,
    /// `instanceMethodExtraInitializers`: the `addInitializer` callbacks of
    /// INSTANCE METHOD / GETTER / SETTER elements only. Run with `this` = the new
    /// instance at the head of instance element initialization, before any field.
    pub instance_extra: Vec<Value>,
    /// `staticMethodExtraInitializers`: same, for STATIC method/getter/setter
    /// elements. Run with `this` = the (decorated) class after class decoration
    /// and before the static field initializers.
    pub static_extra: Vec<Value>,
    /// …and from the CLASS decorators, run with `this` = the (decorated) class
    /// after static elements — the last step of ClassDefinitionEvaluation.
    pub class_extra: Vec<Value>,
    /// The class's `[Symbol.metadata]` object, shared by every `context.metadata`
    /// of this evaluation (decorator-metadata proposal). Its [[Prototype]] is the
    /// superclass's own metadata object, so a subclass's decorators read through
    /// to the base class's metadata. `UNDEFINED` until MakeClass creates it.
    pub metadata: Value,
    /// The `decorationState.[[Finished]]` guard, as a GENERATION counter.
    ///
    /// The spec creates a FRESH `decorationState` per decorator call and finishes
    /// it the instant that decorator returns, so a context object stashed by one
    /// decorator is already closed when the NEXT one runs — not merely when the
    /// class is done. Each `addInitializer` closure captures the generation it was
    /// built at; the counter is bumped after every decorator call, so a stale
    /// context's `addInitializer` throws a TypeError exactly when the spec says.
    pub gen: u32,
    /// The value a class decorator returned in place of the class, or `UNDEFINED`
    /// when the class was not replaced. `LoadClassValue` resolves the class's own
    /// INNER binding through this: ClassDefinitionEvaluation performs
    /// `classEnv.InitializeBinding(classBinding, F)` AFTER `F` is set to the
    /// decorated value, so `class C { static who() { return C } }` under
    /// `@replace` must see the replacement, not the class it was handed.
    pub replacement: Value,
}

impl DecState {
    /// Sized for `n` decorated elements, so `field_inits`/`keys` can be indexed
    /// by element index without bounds juggling at every use site.
    pub fn new(n: usize) -> Self {
        DecState {
            field_inits: vec![Vec::new(); n],
            elem_extra: vec![Vec::new(); n],
            keys: vec![Value::UNDEFINED; n],
            instance_extra: Vec::new(),
            static_extra: Vec::new(),
            class_extra: Vec::new(),
            metadata: Value::UNDEFINED,
            gen: 0,
            replacement: Value::UNDEFINED,
        }
    }
}

/// Boxed payload of a [`HeapObj::AsyncState`] (see that variant's docs). Boxed for
/// the same reason as [`ClassData`]: it carries two `Vec`s and so is one of the
/// largest variants, but is allocated only when an `async` function suspends.
#[derive(Clone, Debug)]
pub struct AsyncStateData {
    pub func: u32,
    pub closure: u32,
    pub state: GenState,
    pub regs: Vec<Value>,
    pub result: u32,
    pub handlers: Vec<Handler>,
}

/// One pending request on an async generator (spec AsyncGeneratorRequest): the
/// completion kind a `.next()`/`.throw()`/`.return()` call wants delivered, its
/// argument, and the result promise that call returned. Requests are serviced
/// FIFO by `async_gen_service_queue`. GC: `arg` and `promise` are traced via the
/// owning [`AsyncGenState`]'s edge arm in `gc.rs`.
#[derive(Clone, Debug)]
pub struct AsyncGenRequest {
    /// 0 = next, 1 = throw, 2 = return.
    pub kind: u8,
    pub arg: Value,
    pub promise: u32,
}

/// Payload of [`HeapObj::AsyncGenerator`] (an `async function*` activation). Like
/// a generator (suspend/resume on `yield`) AND an async activation (suspend on
/// `await`), so it carries the saved window + handlers like both. `queue` holds
/// the pending `.next()`/`.return()`/`.throw()` requests awaiting the next
/// yield/return (FIFO) — each call returns a Promise.
#[derive(Clone, Debug)]
pub struct AsyncGenState {
    pub func: u32,
    pub closure: u32,
    pub state: GenState,
    pub regs: Vec<Value>,
    pub handlers: Vec<Handler>,
    /// Pending requests, FIFO. The argument must be stored (not just the promise)
    /// because a request can be QUEUED while the generator is awaiting/running, and
    /// the value must be delivered when the request is finally serviced.
    pub queue: Vec<AsyncGenRequest>,
    /// Spec state "awaiting-return": the front request is a `.return(v)` whose
    /// argument is being awaited (AsyncGeneratorAwaitReturn / the Await step of
    /// UnwrapYieldResumption). While set, new requests only enqueue; the await's
    /// settlement re-enters `drive_async_gen`, which routes it.
    pub awaiting_return: bool,
}

/// An ArrayBuffer's byte storage. A plain `ArrayBuffer` owns its bytes per-VM
/// (`Local`); a `SharedArrayBuffer` holds an `Arc` to process-shared memory
/// (`Shared`) so agents on other threads alias the SAME bytes — cloning the
/// heap object (or handing the buffer to a worker agent) clones the Arc, never
/// the memory, which is exactly SharedArrayBuffer semantics. `Deref<[u8]>` /
/// `DerefMut` make almost every byte access (`data.len()`, `&data[a..b]`,
/// `data[i] = x`, `copy_from_slice`) work unchanged on both variants.
#[derive(Clone, Debug)]
pub enum AbData {
    Local(Vec<u8>),
    Shared(std::sync::Arc<SharedMem>),
}

/// The process-shared byte store behind a `SharedArrayBuffer`. The backing
/// allocation is FIXED at construction (a growable SAB preallocates
/// `maxByteLength` bytes, zeroed); only the visible byte length moves, via an
/// atomic store, so `grow` never reallocates and raw pointers held by other
/// agent threads stay valid forever (the `Arc` keeps the allocation alive).
/// Storage is allocated as `u64` words so the base is 8-byte aligned: Atomics
/// element accesses cast interior pointers to `AtomicU8`..`AtomicU64`, and a
/// TypedArray's `byteOffset` is element-size aligned by construction.
pub struct SharedMem {
    #[cfg(not(feature = "safe-sandbox"))]
    buf: UnsafeCell<Box<[u64]>>,
    #[cfg(feature = "safe-sandbox")]
    buf: Box<[u8]>,
    /// Fixed capacity in bytes (== `maxByteLength`; == the initial length for a
    /// non-growable SAB).
    cap: usize,
    /// Current visible byte length (`grow` stores Release; readers load Acquire).
    len: AtomicUsize,
}

// SAFETY: `SharedMem` is the engine's model of JS shared memory, which is racy
// BY SPEC (ECMA-262 memory model): concurrent non-atomic accesses to a
// SharedArrayBuffer may tear, and that is a permitted outcome for non-atomic
// ops. The allocation itself is fixed (never moved/freed while an Arc holds
// it) and `len` only changes through an atomic. Atomic ops (Atomics.*) go
// through real atomic instructions on interior pointers, never through the
// plain slice views.
#[cfg(not(feature = "safe-sandbox"))]
unsafe impl Send for SharedMem {}
#[cfg(not(feature = "safe-sandbox"))]
unsafe impl Sync for SharedMem {}

impl SharedMem {
    /// Allocate `cap` bytes of zeroed shared storage with `len` initially
    /// visible (`len <= cap`; a non-growable SAB passes `len == cap`).
    pub fn try_new(len: usize, cap: usize) -> Result<SharedMem, std::collections::TryReserveError> {
        #[cfg(not(feature = "safe-sandbox"))]
        let words = cap.div_ceil(8).max(1);
        #[cfg(not(feature = "safe-sandbox"))]
        let buf = {
            let mut v = Vec::new();
            v.try_reserve_exact(words)?;
            v.resize(words, 0u64);
            v.into_boxed_slice()
        };
        #[cfg(feature = "safe-sandbox")]
        let buf = {
            let mut v = Vec::new();
            v.try_reserve_exact(cap.max(1))?;
            v.resize(cap.max(1), 0u8);
            v.into_boxed_slice()
        };
        Ok(SharedMem {
            #[cfg(not(feature = "safe-sandbox"))]
            buf: UnsafeCell::new(buf),
            #[cfg(feature = "safe-sandbox")]
            buf,
            cap,
            len: AtomicUsize::new(len.min(cap)),
        })
    }
    /// The current visible byte length.
    #[inline]
    pub fn byte_len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }
    /// Fixed capacity in bytes (`maxByteLength`).
    #[inline]
    pub fn capacity(&self) -> usize {
        self.cap
    }
    /// Move the visible length (clamped to capacity). `grow` only ever raises
    /// it; the shrink direction (engine-internal quirk paths only — spec SABs
    /// never shrink) zeroes the dropped tail so a later grow re-exposes zeroes,
    /// matching `Vec::resize` semantics on the Local variant.
    pub fn set_byte_len(&self, n: usize) {
        let n = n.min(self.cap);
        #[cfg(feature = "safe-sandbox")]
        {
            // SharedArrayBuffer is not exposed by the safe profile. Keep the
            // representation memory-safe if an internal path still constructs
            // one; no mutable alias or cross-thread byte access is available.
            self.len.store(n, Ordering::Release);
            return;
        }
        #[cfg(not(feature = "safe-sandbox"))]
        let old = self.len.swap(n, Ordering::AcqRel);
        #[cfg(not(feature = "safe-sandbox"))]
        if n < old {
            // SAFETY: n..old is within the fixed allocation; single-VM quirk
            // path (no concurrent agents reach a shrinking SAB).
            unsafe {
                std::ptr::write_bytes(self.base_ptr().add(n), 0, old - n);
            }
        }
    }
    /// Raw base pointer (8-byte aligned) — for the Atomics element accesses.
    #[cfg(not(feature = "safe-sandbox"))]
    #[inline]
    pub fn base_ptr(&self) -> *mut u8 {
        unsafe { (*self.buf.get()).as_mut_ptr() as *mut u8 }
    }
    #[inline]
    fn as_slice(&self) -> &[u8] {
        #[cfg(feature = "safe-sandbox")]
        return &self.buf[..self.byte_len()];
        #[cfg(not(feature = "safe-sandbox"))]
        {
            // SAFETY: the allocation is fixed and outlives `self`; see the
            // Send/Sync note for why cross-thread tearing is acceptable here.
            unsafe { std::slice::from_raw_parts(self.base_ptr(), self.byte_len()) }
        }
    }
}

impl std::fmt::Debug for SharedMem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedMem")
            .field("len", &self.byte_len())
            .field("cap", &self.cap)
            .finish_non_exhaustive()
    }
}

impl AbData {
    /// Current byte length (Shared: the visible length, not the capacity).
    #[inline]
    pub fn len(&self) -> usize {
        match self {
            AbData::Local(v) => v.len(),
            AbData::Shared(m) => m.byte_len(),
        }
    }

    /// Bytes reserved by the backing allocation (not merely the currently
    /// visible length of a growable buffer).
    #[inline]
    pub fn resident_bytes(&self) -> usize {
        match self {
            AbData::Local(v) => v.capacity(),
            AbData::Shared(m) => m.capacity(),
        }
    }
    #[inline]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The mutable `Vec` for STRUCTURAL mutations (detach-clear / resize /
    /// transfer) — `None` for a Shared buffer, whose allocation is fixed
    /// (callers either error first or route length changes via
    /// [`AbData::resize_bytes`]). Reserved for paths that must distinguish the
    /// variants; the current sites all go through `resize_bytes`.
    #[inline]
    #[allow(dead_code)]
    pub fn local_mut(&mut self) -> Option<&mut Vec<u8>> {
        match self {
            AbData::Local(v) => Some(v),
            AbData::Shared(_) => None,
        }
    }
    /// The shared store, when this is a SharedArrayBuffer's data.
    #[inline]
    pub fn shared(&self) -> Option<&std::sync::Arc<SharedMem>> {
        match self {
            AbData::Local(_) => None,
            AbData::Shared(m) => Some(m),
        }
    }
    /// Structural resize to `n` bytes: Local resizes the Vec (zero-filling
    /// growth); Shared stores the new visible length (the allocation is
    /// preallocated to `maxByteLength` — callers validate `n <= max`).
    pub fn resize_bytes(&mut self, n: usize) {
        match self {
            AbData::Local(v) => v.resize(n, 0u8),
            AbData::Shared(m) => m.set_byte_len(n),
        }
    }
}

impl From<Vec<u8>> for AbData {
    #[inline]
    fn from(v: Vec<u8>) -> AbData {
        AbData::Local(v)
    }
}

impl std::ops::Deref for AbData {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] {
        match self {
            AbData::Local(v) => v,
            AbData::Shared(m) => m.as_slice(),
        }
    }
}

impl std::ops::DerefMut for AbData {
    #[inline]
    fn deref_mut(&mut self) -> &mut [u8] {
        match self {
            AbData::Local(v) => v,
            // SAFETY (engine contract): JS SharedArrayBuffer memory is racy by
            // spec — tearing on concurrent non-atomic access is a permitted
            // outcome, so handing out a byte view of shared memory is sound
            // for the engine's usage. Within one VM the heap hands out only
            // one buffer borrow at a time; Atomics ops never use this path
            // (they use real atomic instructions on SharedMem directly).
            #[cfg(not(feature = "safe-sandbox"))]
            AbData::Shared(m) => unsafe {
                std::slice::from_raw_parts_mut(m.base_ptr(), m.byte_len())
            },
            #[cfg(feature = "safe-sandbox")]
            AbData::Shared(_) => {
                panic!("SharedArrayBuffer mutation is disabled by the safe-sandbox profile")
            }
        }
    }
}

/// Payload of `HeapObj::Combinator`, boxed out of the enum (see the variant).
#[derive(Clone, Debug)]
pub struct CombinatorData {
    pub kind: CombKind,
    pub results: Vec<Value>,
    pub remaining: u32,
    pub result: u32,
    pub settled: Vec<bool>,
    pub cap_resolve: Value,
    pub cap_reject: Value,
    pub keys: Vec<String>,
}

/// A heap-allocated object.
#[derive(Clone, Debug)]
pub enum HeapObj {
    /// An owned, contiguous JS string (with cached length / ASCII metadata).
    Str(JsStr),
    /// A lazily-concatenated string ("rope" / cons-string, as in V8). `left` and
    /// `right` are heap indices of string-like objects (flat `Str` or nested
    /// `Cons`); `len` is the total character count, so `.length` is O(1) without
    /// materializing. `+` builds one in O(1) instead of copying both operands;
    /// it is flattened to a contiguous `Str` in place on first content access
    /// (indexing, methods, comparison). JS strings are immutable here
    /// (`set_index` no-ops on them), so the structural sharing is sound.
    Cons { left: u32, right: u32, len: usize },
    /// A plain function: index into `Program::functions`. No captured state.
    Func(u32),
    /// A closure: a function id plus captured upvalue cells (indices of `Cell`
    /// heap objects). Captured variables are boxed into cells so mutation is
    /// shared between the closure and its defining scope. `this_val` is the
    /// lexically-captured `this` for an ARROW function (its proto has
    /// `lexical_this`); it is `UNDEFINED` and unused for ordinary closures.
    Closure {
        func: u32,
        upvalues: Vec<u32>,
        this_val: Value,
    },
    /// A boxed mutable variable cell (an upvalue's storage).
    Cell(Value),
    /// A sloppy direct eval's DYNAMIC variable environment for a FUNCTION
    /// context: name -> value bindings the eval's var/function declarations
    /// created in the caller activation (spec: the caller's varEnv). Reached
    /// via Frame.eval_scope / the closure_eval_scope stamps.
    EvalScope(std::collections::HashMap<String, Value>),
    /// A bound function (`fn.bind(thisArg, ...boundArgs)`): calling it invokes
    /// `target` with `this` fixed to `this` and `args` prepended to the call args.
    Bound {
        target: Value,
        this: Value,
        args: Vec<Value>,
    },
    /// A ShadowRealm WrappedFunction exotic: a fresh wrapper created each time
    /// a callable crosses the realm boundary. Calling it wraps the arguments,
    /// calls `target` with `this` = undefined, and wraps the result; any abrupt
    /// target completion surfaces as a caller-realm TypeError. `name`/`length`
    /// are the CopyNameAndLength snapshot taken at wrap time.
    Wrapped {
        target: Value,
        name: String,
        length: f64,
    },
    /// A built-in (native) function value, identified by a small id (see the
    /// `native` ids in vm.rs). Callable as a first-class value — this is what backs
    /// `Object.defineProperty`, `Array.isArray`, `Object.prototype.hasOwnProperty`,
    /// `Function.prototype.call`, etc. when accessed as values (not just called).
    Native(u16),
    /// A built-in function that CARRIES STATE: a native id plus the `Value`s it
    /// closes over (`Vm::call_native_closure` receives both).
    ///
    /// `Native` is a bare id, so every stateful builtin this engine has needed so
    /// far became its OWN `HeapObj` variant (`BoundResolver`, `CombinatorResolver`)
    /// — each costing ~18 arms across typeof / IsCallable / ToPrimitive /
    /// Object.prototype.toString / property access / GC trace / three call-dispatch
    /// fast paths. This is the general form: a new stateful builtin now costs one
    /// arm of `call_native_closure` and nothing else. Introduced for the decorator
    /// context's `addInitializer` and `access.{has,get,set}`, which are per-element
    /// closures over the class value, the element name and its kind.
    ///
    /// `name`/`length` are the CreateBuiltinFunction values (the spec names these
    /// closures — "addInitializer", "get", "set", "has"), since a state-carrying
    /// native has no entry in the static `native::static_name_length` table.
    NativeClosure {
        id: u16,
        state: Vec<Value>,
        name: &'static str,
        length: u8,
    },
    /// A dense array.
    Array(Vec<Value>),
    /// A plain object.
    Object(Box<ObjMap>),
    /// A JS Promise. `result` holds the fulfillment value / rejection reason
    /// (undefined while Pending); `reactions` are the subscriptions registered
    /// while Pending (drained as microtasks on settle). `handled` tracks whether
    /// a rejection handler was attached (for optional unhandled-rejection report).
    ///
    /// One list, not two: both handlers of a subscription are registered
    /// together and settlement picks the matching one, so splitting them across
    /// two `Vec`s bought nothing and cost two allocations per `.then`.
    Promise {
        state: PromiseState,
        result: Value,
        reactions: Reactions,
        handled: bool,
    },
    /// A native `resolve`/`reject` function bound to a promise — the pair handed
    /// to a `new Promise(executor)`. Calling it settles `promise`. `pair` is the
    /// CreateResolvingFunctions [[AlreadyResolved]] record id shared by the
    /// resolve+reject of one pair (0 = untracked): the FIRST call through either
    /// function consumes it and every later call is a spec no-op — even while the
    /// promise is still Pending because the first resolve deferred to a thenable
    /// job (see `Vm::resolver_pair_fire`).
    BoundResolver {
        promise: u32,
        is_reject: bool,
        pair: u32,
    },
    /// A `Date`: milliseconds since the Unix epoch (NaN = Invalid Date). The
    /// engine treats all component getters/setters as UTC (a documented
    /// simplification — node uses the host time zone for the non-UTC ones).
    Date(f64),
    /// Shared state for a Promise combinator (`all`/`allSettled`/`race`/`any`).
    /// `results` collects per-input outcomes (sized to the input count);
    /// `remaining` counts inputs still outstanding; `result` is the combinator's
    /// own promise (settled when the combinator's condition is met). `settled`
    /// is the per-index [[AlreadyCalled]] guard: a misbehaving thenable that calls
    /// a resolve/reject element more than once is ignored after the first.
    /// `cap_resolve`/`cap_reject` are the result capability's [[Resolve]]/[[Reject]]
    /// functions: the combinator settles its result THROUGH them (per spec), so a
    /// custom `this`-constructor's executor-provided functions are observably
    /// invoked. On the native path they are `BoundResolver`s bound to `result`, so
    /// calling them is identical to `self.resolve/reject(result, …)`.
    /// BOXED. Eight fields including three `Vec`s made this ~104 bytes, which —
    /// with `ObjMap` — set `HeapObj`'s size for EVERY heap slot. MEASURED: 64
    /// bytes of pure padding on `HeapObj` costs 7.9% across the suite.
    Combinator(Box<CombinatorData>),
    /// A native resolve/reject element for a combinator: performs one combinator
    /// step (`is_reject` selects fulfill vs reject when CALLED directly by a custom
    /// thenable; via the native reaction the kind comes from the reaction list).
    CombinatorResolver {
        combinator: u32,
        index: u32,
        is_reject: bool,
    },
    /// A suspended generator (`function*`). Owns a DETACHED register window (off
    /// the contiguous live `regs` Vec, so the JIT's pinned-capacity invariant
    /// holds while parked); `func`/`closure` re-create the frame on resume, and
    /// `state` carries the resume ip / completion. `handlers` preserves the
    /// frame's active `try` handlers across a yield, so `gen.throw(e)` resumes
    /// into an enclosing `try`/`catch` (and `gen.return(v)` can run `finally`).
    Generator {
        func: u32,
        closure: u32,
        state: GenState,
        regs: Vec<Value>,
        handlers: Vec<Handler>,
    },
    /// An `async function*` activation — see [`AsyncGenState`]. Its `.next()`
    /// returns a Promise; the body may both `yield` and `await`.
    AsyncGenerator(Box<AsyncGenState>),
    /// A suspended `async function` activation — like Generator (detached window
    /// resumed at each `await`) but it also owns its `result` Promise's heap index
    /// and PRESERVES `try` handlers across an await (so `try { await p } catch`
    /// works). `handlers` are (catch_target, catch_reg) pairs.
    AsyncState(Box<AsyncStateData>),
    /// A JS `Map`: insertion-ordered (key, value) entries with SameValueZero key
    /// equality. Parallel `keys`/`vals` Vecs (small Maps dominate; linear scan).
    Map { keys: Vec<Value>, vals: Vec<Value> },
    /// A JS `Set`: insertion-ordered unique values (SameValueZero equality).
    Set(Vec<Value>),
    /// A JS `WeakMap`: like `Map` but keys must be objects and there is no
    /// iteration/size (a distinct type so the [[WeakMapData]] brand check works —
    /// `WeakMap.prototype.set.call(aMap)` must throw). No GC, so refs stay strong.
    WeakMap { keys: Vec<Value>, vals: Vec<Value> },
    /// A JS `WeakSet`: like `Set` but values must be objects, no iteration/size.
    WeakSet(Vec<Value>),
    /// A JS `WeakRef`: a weak reference to an object. No GC, so `deref()` always
    /// returns the (still-live) target.
    WeakRef(Value),
    /// A JS `FinalizationRegistry`: holds a cleanup callback and the live
    /// unregister tokens. No GC, so cleanup never fires (spec-permitted); only
    /// `register`/`unregister` are observable. `tokens` tracks unregister tokens.
    FinalizationRegistry { cleanup: Value, tokens: Vec<Value> },
    /// A boxed primitive wrapper (`new String(x)`/`new Number(x)`/`new Boolean(x)`,
    /// or `Object(primitive)`). `kind` 0=String/1=Number/2=Boolean; `value` is the
    /// wrapped primitive ([[PrimitiveValue]]). `typeof` is "object"; valueOf returns
    /// the value; the kind's prototype provides the methods.
    Boxed { kind: u8, value: Value },
    /// A JS `RegExp`. `regex` is the compiled `regress` engine (ECMAScript regex);
    /// `source` is the pattern text, `flags` the JS flag string (`"gi"`); `last_index`
    /// is the writable `lastIndex` own data property — stored as a raw `Value` (not a
    /// coerced offset) so an assigned object survives until `exec`/the @@-methods
    /// apply ToLength, invoking its `valueOf` at the spec-mandated time.
    ///
    /// `ascii_twin` is the lazily-built BYTE-OPTIMIZED twin compile (regress
    /// `from_unicode_byteopt`) used by the ASCII-subject fast path, cached
    /// inline so a hot exec reads it with one `heap.get` instead of a per-exec
    /// side-table probe: `None` = not yet computed, `Some(None)` = compile
    /// failed (fall back to `regex`), `Some(Some(arc))` = the twin. Reset to
    /// `None` by `RegExp.prototype.compile`. Holds no `Value`s (an
    /// `Arc<regress::Regex>` is a pure compiled program), so GC need not trace it.
    RegExp {
        regex: std::sync::Arc<regress::Regex>,
        /// `Arc<str>`, not `String`, so that cloning a RegExp shares its text instead
        /// of copying it. `matchAll` clones a matcher per call — the iterator has to
        /// advance a `lastIndex` independently of the source regex, and that object is
        /// observable to a user `exec` as its receiver, so the clone cannot be elided —
        /// which meant two heap `String` allocations on a path measured at 480ns
        /// against node's 53. Sharing makes it two atomic increments.
        ///
        /// MEASURED: −2.0pp of `regex-log-scan` (the row went −0.9% with only the
        /// `flags` shortcut below, −2.9% with both; B70). Larger than it looks because
        /// `source`/`flags` are cloned on several regex paths, not just `matchAll`.
        ///
        /// NOT for size — that hypothesis was refuted. This variant's payload does drop
        /// 80 → 64 bytes, but `HeapObj` stays 80: `Generator` is 72 and something else
        /// still pins the ceiling, so `heap_obj_slot_stays_small` sees no change.
        source: std::sync::Arc<str>,
        flags: std::sync::Arc<str>,
        last_index: Value,
        ascii_twin: Option<Option<std::sync::Arc<regress::Regex>>>,
    },
    /// A JS `ArrayBuffer` — a raw byte buffer backing TypedArrays/DataViews.
    /// `detached` is set by transfer (we never detach via GC); `data` is the bytes
    /// ([`AbData`]: per-VM `Local` for ArrayBuffers, `Shared` for SharedArrayBuffers).
    ArrayBuffer { data: AbData, detached: bool },
    /// A JS TypedArray view (`Int8Array`, `Float64Array`, …). `kind` indexes the
    /// element type (see `vm::native::TA_KINDS`); `buffer` is the backing
    /// `ArrayBuffer`'s heap index; `byte_offset`/`length` (in elements) frame the view.
    TypedArray {
        buffer: u32,
        kind: u8,
        byte_offset: usize,
        length: usize,
    },
    /// A JS `DataView` over an ArrayBuffer (`buffer` heap index, byte window).
    DataView {
        buffer: u32,
        byte_offset: usize,
        byte_length: usize,
    },
    /// A JS `Proxy`: property/call operations route through `handler`'s traps (or
    /// fall through to `target`). `revoked` cuts it off (every op then throws).
    Proxy {
        target: Value,
        handler: Value,
        revoked: bool,
    },
    /// A `Temporal.*` value. `kind` selects the type (0=Duration, 1=PlainDate,
    /// 2=PlainTime, 3=PlainDateTime, …); `fields` holds its integer slots in a
    /// per-kind layout (Duration: y,mo,w,d,h,mi,s,ms,us,ns; PlainDate: isoY,isoM,isoD).
    Temporal { kind: u8, fields: Vec<i64> },
    /// An `Intl.*` instance. `kind` selects the service (0=NumberFormat,
    /// 1=DateTimeFormat, 2=Collator, 3=PluralRules, 4=ListFormat,
    /// 5=RelativeTimeFormat, 6=Segmenter, 7=Locale, 8=DisplayNames,
    /// 9=DurationFormat). `resolved` is the heap index of an Object holding the
    /// instance's resolved options (insertion-ordered, so resolvedOptions() can
    /// clone it directly); for Locale it also holds the parsed language/region/…
    /// subtags read back by the prototype getters. `typeof` is "object".
    Intl { kind: u8, resolved: u32 },
    /// A JS `BigInt` primitive in the FAST representation: any value that fits
    /// `i128` (virtually all real arithmetic). Compared by VALUE (`1n === 1n`),
    /// not identity; `typeof` is "bigint".
    BigInt(i128),
    /// A JS `BigInt` primitive OUTSIDE i128 range (arbitrary precision via
    /// `num_bigint`). CANONICAL-FORM INVARIANT: a `BigIntBig` NEVER holds an
    /// i128-representable value — `Vm::make_bigint_val`/`BigVal::from_num`
    /// (vm/bigint.rs) demote on construction, so equality/ordering between the
    /// two variants never has to cross-compare (a `BigInt` and a `BigIntBig`
    /// are always unequal). Holds no heap references (GC trace is a no-op).
    BigIntBig(Box<num_bigint::BigInt>),
    /// A JS `Symbol` primitive. Identity is the heap index (so `===` and use as a
    /// property key dedupe correctly). `desc` is the description (a string Value or
    /// UNDEFINED). `prop_key` is the internal string under which the symbol is
    /// stored as an object property — `"@@iterator"` etc. for the well-known
    /// symbols (matching the engine's existing iterator-key convention) and
    /// `"@@sym:N"` for user symbols. `typeof` is "symbol".
    Symbol { desc: Value, prop_key: String },
    /// A built-in iterator (Array/Map/Set `entries()`/`keys()`/`values()` and the
    /// default `@@iterator`). A snapshot of the values to yield plus a cursor;
    /// `proto` is its prototype heap index (%ArrayIteratorPrototype% etc., distinct
    /// per collection). `.next()` yields `items[index]` then advances. When `live`
    /// is `Some((coll, kind))` it is a LIVE Map/Set iterator that steps the backing
    /// collection `coll` at `index` (skipping tombstoned/HOLE slots) instead of the
    /// `items` snapshot, so a delete/add after the iterator is created is observed
    /// (`kind`: 0 = keys, 1 = values, 2 = entries `[k, v]`).
    Iterator {
        items: Vec<Value>,
        index: usize,
        proto: u32,
        live: Option<(u32, u8)>,
    },
    /// A lazy Iterator Helper (the result of `Iterator.prototype.{map,filter,
    /// take,drop,flatMap}`). `source` is the underlying iterator; `kind` selects
    /// the transform (0=map,1=filter,2=take,3=drop,4=flatMap); `arg` is the
    /// callback (map/filter/flatMap); `n` is the remaining count (take/drop);
    /// `idx` is the 0-based counter passed to callbacks; `done` marks exhaustion;
    /// `inner` is flatMap's current inner iterator (or UNDEFINED).
    IterHelper {
        source: Value,
        kind: u8,
        arg: Value,
        n: i64,
        idx: i64,
        done: bool,
        inner: Value,
        /// The source's `next` method, read ONCE at creation (GetIteratorDirect), so
        /// stepping calls the cached method rather than re-reading `source.next` each
        /// time. `UNDEFINED` when the source needs the generic step path (a generator,
        /// or a multi-source zip/concat helper).
        next: Value,
        /// `[[GeneratorState]] == "executing"` brand: set while a `.next()` step is in
        /// flight so that a callback re-entering `.next()`/`.return()` on the same
        /// helper is a TypeError (GeneratorValidate) rather than infinite recursion.
        running: bool,
    },
    /// A class value (`class C {…}`). Fields live in the boxed [`ClassData`]:
    /// `ctor` is the func id that runs instance field initializers then the user
    /// constructor (or `None`); `methods` maps each instance method name to its
    /// func id. `new C(args)` builds a plain object, links it to its class for
    /// method lookup, and runs the ctor with `this` = the new object.
    Class(Box<ClassData>),
}

impl HeapObj {
    /// Heap allocations owned directly by this slot. This deliberately uses
    /// capacities rather than lengths: shrinking a JS aggregate does not give
    /// its reserved address space back to the host allocator.
    fn resident_payload_bytes(&self) -> usize {
        match self {
            HeapObj::Str(s) => s.bytes.capacity(),
            HeapObj::Closure { upvalues, .. } => vec_capacity_bytes(upvalues),
            HeapObj::EvalScope(bindings) => bindings
                .capacity()
                .saturating_mul(std::mem::size_of::<(String, Value)>() * 2)
                .saturating_add(
                    bindings
                        .keys()
                        .fold(0usize, |n, key| n.saturating_add(key.capacity())),
                ),
            HeapObj::Bound { args, .. } => vec_capacity_bytes(args),
            HeapObj::Wrapped { name, .. } => name.capacity(),
            HeapObj::NativeClosure { state, .. } => vec_capacity_bytes(state),
            HeapObj::Array(items) => vec_capacity_bytes(items),
            HeapObj::Object(map) => {
                std::mem::size_of::<ObjMap>().saturating_add(map.resident_bytes())
            }
            HeapObj::Promise { reactions, .. } => match reactions {
                Reactions::Many(v) => vec_capacity_bytes(v),
                _ => 0,
            },
            HeapObj::Combinator(data) => {
                let mut n = std::mem::size_of::<CombinatorData>()
                    .saturating_add(vec_capacity_bytes(&data.results))
                    .saturating_add(vec_capacity_bytes(&data.settled))
                    .saturating_add(string_vec_payload(&data.keys));
                // Keep the fold visibly saturating when fields are extended.
                n = n.saturating_add(0);
                n
            }
            HeapObj::Generator { regs, handlers, .. } => {
                vec_capacity_bytes(regs).saturating_add(vec_capacity_bytes(handlers))
            }
            HeapObj::AsyncGenerator(state) => std::mem::size_of::<AsyncGenState>()
                .saturating_add(vec_capacity_bytes(&state.regs))
                .saturating_add(vec_capacity_bytes(&state.handlers))
                .saturating_add(vec_capacity_bytes(&state.queue)),
            HeapObj::AsyncState(state) => std::mem::size_of::<AsyncStateData>()
                .saturating_add(vec_capacity_bytes(&state.regs))
                .saturating_add(vec_capacity_bytes(&state.handlers)),
            HeapObj::Map { keys, vals } | HeapObj::WeakMap { keys, vals } => {
                vec_capacity_bytes(keys).saturating_add(vec_capacity_bytes(vals))
            }
            HeapObj::Set(values)
            | HeapObj::WeakSet(values)
            | HeapObj::FinalizationRegistry { tokens: values, .. } => vec_capacity_bytes(values),
            HeapObj::RegExp { source, flags, .. } => {
                // Compiled programs (including the ASCII twin) are measured
                // through Regex::resident_bytes and Arc-deduplicated by the
                // VM audit. Keep the observable source/flag text charged here.
                source.len().saturating_add(flags.len())
            }
            HeapObj::ArrayBuffer { data, .. } => data.resident_bytes(),
            HeapObj::Temporal { fields, .. } => vec_capacity_bytes(fields),
            HeapObj::BigIntBig(value) => (value.bits() as usize)
                .div_ceil(8)
                .saturating_add(std::mem::size_of::<num_bigint::BigInt>()),
            HeapObj::Symbol { prop_key, .. } => prop_key.capacity(),
            HeapObj::Iterator { items, .. } => vec_capacity_bytes(items),
            HeapObj::Class(class) => {
                let mut n = std::mem::size_of::<ClassData>()
                    .saturating_add(class.name.capacity())
                    .saturating_add(class.source.capacity())
                    .saturating_add(named_value_vec_payload(&class.methods))
                    .saturating_add(named_value_vec_payload(&class.getters))
                    .saturating_add(named_value_vec_payload(&class.setters))
                    .saturating_add(string_vec_payload(&class.proto_order))
                    .saturating_add(class.statics.resident_bytes())
                    .saturating_add(named_value_vec_payload(&class.static_getters))
                    .saturating_add(named_value_vec_payload(&class.static_setters))
                    .saturating_add(vec_capacity_bytes(&class.computed_field_keys))
                    .saturating_add(vec_capacity_bytes(&class.ctor_upvalues))
                    .saturating_add(vec_capacity_bytes(&class.field_thunk_upvalues));
                if let Some(dec) = &class.dec {
                    n = n
                        .saturating_add(std::mem::size_of::<DecState>())
                        .saturating_add(vec_capacity_bytes(&dec.field_inits))
                        .saturating_add(
                            dec.field_inits
                                .iter()
                                .fold(0usize, |n, v| n.saturating_add(vec_capacity_bytes(v))),
                        )
                        .saturating_add(vec_capacity_bytes(&dec.elem_extra))
                        .saturating_add(
                            dec.elem_extra
                                .iter()
                                .fold(0usize, |n, v| n.saturating_add(vec_capacity_bytes(v))),
                        )
                        .saturating_add(vec_capacity_bytes(&dec.keys))
                        .saturating_add(vec_capacity_bytes(&dec.instance_extra))
                        .saturating_add(vec_capacity_bytes(&dec.static_extra))
                        .saturating_add(vec_capacity_bytes(&dec.class_extra));
                }
                n
            }
            _ => 0,
        }
    }
}

/// Heap index of the interned empty string. The 128 single-ASCII-char strings
/// occupy indices `0..128`; the empty string is `128`.
pub const INTERN_EMPTY: u32 = 128;
/// First/last slots of the immutable two-digit decimal table (`"00".."99"`).
/// These primitive strings are returned by `Pad2Concat` without allocation.
pub const INTERN_PAD2_START: u32 = INTERN_EMPTY + 1;
pub const INTERN_PAD2_COUNT: u32 = 100;
pub const INTERN_PAD2_END: u32 = INTERN_PAD2_START + INTERN_PAD2_COUNT - 1;
/// Last immutable engine-interned slot. In-place string builders must accept
/// only indices strictly above this boundary; unlike the single-character
/// prefix, the pad2 table contains multi-character `Str`s.
pub const INTERN_PINNED_END: u32 = INTERN_PAD2_END;

pub struct Heap {
    objs: Vec<HeapObj>,
    /// Last reconciled live payload total and its monotonic peak. Per-slot
    /// charges let collection/reuse subtract the correct tracked size without
    /// confusing cumulative allocation churn with resident high-water.
    resident_payload_current: Cell<usize>,
    resident_payload_high_water: Cell<usize>,
    resident_payload_charged: Vec<Cell<usize>>,
    /// Whether the eager per-allocation payload charge runs. OFF for a
    /// trusted, un-instrumented run — the figure has no consumer there and the
    /// per-object sizing walk is a measurable tax on allocation-heavy code.
    /// The first `audit_resident_bytes` call (heap ceilings, `heap_bytes`
    /// reporting) flips it on and simultaneously backfills every live slot's
    /// charge, so reporting and enforcement never disagree once a consumer
    /// exists.
    payload_accounting: Cell<bool>,
    /// Per-object version, parallel to `objs` (one `u32` per heap object). Bumped
    /// whenever an object gains a NEW key (which may reallocate its `vals`). The
    /// JIT inline cache reads this (by heap index) to validate a cached
    /// `vals`-pointer: a matching version proves `vals` hasn't reallocated since
    /// the cache was filled. Allocated in lockstep with `objs` so indices align.
    versions: Vec<u32>,
    /// SHAPE MIRROR, parallel to `objs`: the object's hidden class as of the
    /// last settling event, or [`crate::shape::DICT`] (0). The emitted
    /// shape-way probes (B178) guard on THIS word — `mirror == way.shape`
    /// licenses a direct `vals[slot]` read with no helper call — so its
    /// maintenance discipline is the soundness argument:
    ///
    ///  * `alloc`/`replace` REFRESH it from the live map (a fresh object's
    ///    settled shape; for the append-built literal paths the captured
    ///    shape is EMPTY, which no way can carry — an empty map has no
    ///    property to fill — so the entry is stale-but-unmatchable while the
    ///    literal is under construction).
    ///  * `bump_version` INVALIDATES it to 0 rather than refreshing: every
    ///    reachable-object shape change bumps the version (the documented IC
    ///    contract), and 0 is fail-safe regardless of whether a caller bumps
    ///    before or after mutating — a refresh here would capture the WRONG
    ///    side at a bump-first call site, and a shape-way hit has no second
    ///    guard to catch it.
    ///  * the JIT miss helper REPAIRS it to the live shape when it resolves
    ///    own data on a guardable map (strictly after any mutation settled).
    ///  * `free_slot` clears it; non-`Object` slots hold 0 forever, so a
    ///    string/array receiver can never match a way.
    shape_mirror: Vec<u32>,
    /// `vals` base-pointer mirror, parallel to `objs` (0 for non-`Object`
    /// slots). Maintained by exactly the [`Heap::shape_mirror`] events; a
    /// shape-way hit dereferences it only AFTER the mirror-shape guard
    /// matched, which proves the entry was refreshed after the map's last
    /// version-bumping mutation (key adds are what reallocate `vals`).
    vals_ptr_mirror: Vec<u64>,
    /// Raw bases of the two mirrors, re-cached whenever the vectors grow. The
    /// emitted probes load these THROUGH the VM (`[rdi + offset]`) on every
    /// access, so growth during a native run (helper allocations) is safe —
    /// unlike the pinned `r13` versions base, nothing re-derives these.
    pub(crate) shape_mirror_raw: u64,
    pub(crate) vals_ptr_mirror_raw: u64,
    /// Free list of reclaimed slot indices (filled by the mark-sweep GC's sweep,
    /// drained by `alloc`). A reused slot is overwritten and its version bumped so
    /// any stale JIT inline-cache entry misses. Empty until the first collection.
    free: Vec<u32>,
    /// Number of live (allocated, non-free) slots — `objs.len()` minus the free
    /// list and the pinned built-in prefix bookkeeping. Used to decide when to GC.
    live: usize,
    /// `alloc` sets this once the live count passes `gc_threshold`; the interpreter
    /// dispatch loop polls it at a safe point and runs a collection.
    gc_requested: bool,
    /// Live-count at which the next collection is requested (grown adaptively after
    /// each GC to amortise; never below `GC_MIN_THRESHOLD`).
    gc_threshold: usize,
    /// B6 generational ORACLE (`ZIPP_GCSTATS=1` only; empty and never touched
    /// otherwise). Per-slot birth epoch, parallel to `objs`: a slot is "young"
    /// iff it was allocated since the last collection (`born[idx] == epoch`).
    /// Stats-only bookkeeping — never consulted by the collector's decisions.
    born: Vec<u32>,
    /// Current allocation epoch (bumped once per completed collection).
    epoch: u32,
    /// Allocations in the current epoch (== distinct young slots: the free list
    /// is only refilled at a GC, so no slot is handed out twice per epoch).
    allocs_epoch: u64,
    /// Latched `ZIPP_GCSTATS` presence (read once at construction) — the single
    /// gate on every oracle field above. `false` = the default build's behavior,
    /// bit for bit.
    oracle: bool,
    /// Stage-1 nursery alloc log (NURSERY_DESIGN.md §6, step 2): the slots
    /// allocated since the last collection, in allocation order — what a MINOR
    /// collection sweeps INSTEAD of walking `floor..len`. One `Vec` push per
    /// allocation (~1ns amortized against B104's 23-148ns alloc/free; the
    /// per-collection `clear` keeps capacity, so steady state never
    /// reallocates). Entries are distinct: the free list is refilled only at a
    /// collection, so no slot is handed out twice in one epoch. Never pushed
    /// unless the nursery is latched on.
    young: Vec<u32>,
    /// Latched ABSENCE of `ZIPP_NO_NURSERY` (read once at construction) — the
    /// single gate on the young log, the generation bytes, the write barrier
    /// and the minor/major decision. DEFAULT-ON since W9 (B122): with the
    /// 16k young budget and static pretenure, the one-binary 21-pair retrial
    /// measured net **−0.70% [−0.99, −0.48]** and its 5-row replication
    /// −2.15% [−3.07, −1.35] — regex −6.3/−7.1%, json −4.3/−4.4%, markdown
    /// −1.3/−2.1% both times, against two named sub-2% trades (async
    /// +1.1/+1.2%, map-set +1.8/+1.9%, §14's B113/B115 footing). The two
    /// prior refutations stand as history: stage 1's default was refuted at
    /// B120 (a minor still paid the full mark) and stage 3's at B121 (64k
    /// budget, no pretenure — markdown +6.2%); the budget sweep and the
    /// pretenured builders are what changed the economics. `ZIPP_NURSERY=1`
    /// is accepted as a no-op for compatibility with wave 7-8 scripts.
    /// `ZIPP_NO_NURSERY=1` is the pre-nursery collector exactly: every
    /// collection is a major, and `alloc` never touches `young`/`gen`.
    nursery: bool,
    /// Stage-3 generation byte per slot, parallel to `objs` (EMPTY unless the
    /// nursery is latched — every reader is gated on `self.nursery`). Low two
    /// bits are the state (`GEN_YOUNG`/`GEN_OLD`/`GEN_DIRTY`), `GEN_SCAN` is
    /// the sticky "call-free store target" bit (see [`Heap::register_scan_root`]).
    /// Kept in lockstep by `alloc`; a freed slot is stamped `GEN_OLD` so
    /// `gen == GEN_YOUNG ⇔ the slot is in the young log` is a real invariant.
    gen: Vec<u8>,
    /// Stage-3 remembered set: OLD holders that received a young store since
    /// the last collection (holder-grain — NURSERY_DESIGN.md §1's dirty-object
    /// form; deduped by the `GEN_DIRTY` state so each holder appears once).
    /// A minor re-traces every entry's full edge list; drained and reset to
    /// `GEN_OLD` by [`Heap::remset_reset`] at the end of each minor.
    remset: Vec<u32>,
    /// Stage-3 PERSISTENT trace roots: receivers a JIT cache can store into
    /// with NO helper call (a filled SetProp data way; a baked method-inline
    /// trivial-setter arm). Those stores can never run a barrier, so their
    /// targets' edges are re-scanned at EVERY minor instead — sound because
    /// the set of such receivers is exactly what the fill sites registered.
    /// Deduped by `GEN_SCAN`; an entry self-expires when its slot is recycled
    /// (`alloc` stamps `GEN_YOUNG`, clearing the bit; the minor prune drops it).
    scan_roots: Vec<u32>,
    /// Post-collection live count recorded by the last MAJOR. A major frees
    /// every unreachable slot, so this is the honest "true live" anchor the
    /// major schedule is measured against (a post-MINOR count includes floated
    /// old garbage, which must not compound the threshold — B120's named fix).
    live_at_major: usize,
    /// Occupied-slot count that latches the next collection into a MAJOR:
    /// the PRE-NURSERY schedule (`GC_GROWTH * live_at_major`, floored). A
    /// minor that ends with the heap still at or above this point has proven
    /// the growth is NOT young garbage — it is survivors plus floats (old
    /// garbage a young-only sweep can never reclaim) — so the next collection
    /// majors, i.e. floats are reclaimed within one young budget of where
    /// today's collector would have collected anyway. That bounds them
    /// without measuring them (a young-only trace cannot).
    major_at: usize,
    /// Latched by [`Heap::note_minor_done`] when the post-minor occupied
    /// count crossed `major_at`: the next collection runs as a major.
    major_due: bool,
    /// Minor collections since the last major (the scheduling backstop).
    minors_since_major: u32,
    /// Maximum consecutive minor collections before the scheduling backstop
    /// forces a major. `ZIPP_NURSERY_MAX_MINORS=<1..=4096>` overrides the
    /// default once at construction; malformed, zero, and larger values are
    /// ignored. `ZIPP_GC_STRESS` deliberately uses its smaller fixed cap.
    nursery_max_minors: u32,
    /// W9: allocations between minor collections. [`NURSERY_YOUNG_BUDGET`]
    /// unless `ZIPP_NURSERY_YOUNG_BUDGET=<n>` overrides it (latched once at
    /// construction; values below 1024 are ignored as certainly-wrong). Only
    /// read under `self.nursery`, so the default build is bit-identical.
    young_budget: usize,
    /// W9 static-pretenure depth (NURSERY_DESIGN.md §4): while non-zero,
    /// `alloc` stamps new slots OLD-clean and skips the young log — used by
    /// the builtin builders whose output is measured to survive minors
    /// wholesale (JSON.parse's tree, String.prototype.split's parts). A depth,
    /// not a bool: scopes nest. Stays 0 (and the mechanism inert) under
    /// `ZIPP_NO_PRETENURE=1` or when the nursery is dark.
    pretenure: u32,
    /// Latched absence of `ZIPP_NO_PRETENURE` — the escape hatch that keeps
    /// the nursery trial one-binary.
    pretenure_on: bool,
    /// W10: the minor mark vector, retained between minors (all-true at
    /// stash time; one O(young-log) pass re-derives the fresh build — see
    /// [`Heap::take_nonyoung_marks`]). Empty when invalid or taken.
    nonyoung_cache: Vec<bool>,
    /// Whether `nonyoung_cache` may be reused (false after a take, a major's
    /// gen rewrite, `young_reset`, or the `set_nursery` hook).
    nonyoung_cache_valid: bool,
    /// Latched absence of `ZIPP_NO_NONYOUNG_CACHE`.
    nonyoung_cache_on: bool,
    /// W10: whether the young budget is PINNED (an explicit
    /// `ZIPP_NURSERY_YOUNG_BUDGET` or `ZIPP_NO_NURSERY_ADAPT=1`) — the
    /// survival-adaptive controller then never moves it.
    budget_pinned: bool,
    /// W10 value-grain remembered set (B123): the YOUNG values stored into
    /// OLD-clean holders this epoch, recorded by `write_barrier_val` and
    /// marked DIRECTLY as minor roots — replacing the holder-grain full
    /// edge-list re-trace for every store site that knows its value (on
    /// regex-log-scan that re-trace was 59.3ms/run: 227,698 `jit_set_index`
    /// stores into two retained arrays). Deduped per value by `GEN_VLOG`;
    /// cleared (capacity kept) at every minor and major. The value-BLIND
    /// card form (`write_barrier`) and its callers keep holder-grain
    /// `remset` treatment, as do the `scan_roots`.
    vremset: Vec<u32>,
    /// Latched absence of `ZIPP_NO_VALGRAIN_REMSET`.
    valgrain: bool,
}

/// Stage-3 generation states (low two bits of a `Heap::gen` byte).
/// `GEN_YOUNG`: allocated since the last collection (in the young log).
const GEN_YOUNG: u8 = 0;
/// `GEN_OLD`: survived a collection, not in the remembered set.
const GEN_OLD: u8 = 1;
/// `GEN_DIRTY`: old AND in the remembered set (the dedup bit of the barrier).
const GEN_DIRTY: u8 = 2;
/// Mask selecting the state bits out of a `Heap::gen` byte.
const GEN_STATE: u8 = 0b11;
/// Sticky "registered in `Heap::scan_roots`" bit (call-free store target).
const GEN_SCAN: u8 = 0b100;
/// W10: "already recorded in `Heap::vremset` this epoch" — the value-grain
/// remembered set's dedup bit (a young VALUE stored into an old holder is
/// pushed at most once per epoch). Cleared wherever the slot's state is
/// wholesale restamped (`alloc`, `free_slot`, the promote arms).
const GEN_VLOG: u8 = 0b1000;

/// Smallest live-object count that triggers a collection — below this the heap is
/// trivially small and collecting would be pure overhead.
pub const GC_MIN_THRESHOLD: usize = 1 << 16;

/// Allocations between collections, as a multiple of the live count after the
/// last one. Collecting at `GC_GROWTH * live` means each collection traces `live`
/// objects per `(GC_GROWTH - 1) * live` allocations, so this is directly the
/// amortised tracing cost per allocation.
///
/// Was 2 — one full trace per allocation — which measured as 17-22% of three
/// benches' total wall time (json-large 122ms of 558, markdown-render 141 of 697,
/// regex-log-scan 324 of 1853). Those workloads keep a large live set (a parsed
/// document, 150k retained log lines) and allocate garbage against it, so the
/// same live objects were being retraced continuously.
///
/// 3, not more. Swept 2/3/4/6 by wall time: 3 is -1.4% overall (json-large -8.4%,
/// markdown-render -4.1%) while 4 is only -0.9% and 6 worse still, even though
/// both keep cutting GC time — past 3 the larger slot array costs more in cache
/// misses than the skipped tracing saves. That crossover is the same effect that
/// made disabling GC entirely SLOWER than leaving it on (a 3M-object run grew
/// `objs` to 240MB and went 35ns -> 64ns per `{}`).
///
/// The cost is peak slots: regex-log-scan goes 496k -> 804k (~40MB -> 64MB at 80
/// bytes each). The `objs.len() / 2` floor below is unchanged, so a heap that has
/// already grown still collects on the same schedule it did.
const GC_GROWTH: usize = 3;

/// Stage-3 nursery scheduling (NURSERY_DESIGN.md §2): allocations between
/// MINOR collections. A minor's cost is O(young live + roots + remset), not
/// O(heap), so it can afford to run far more often than a major — this is the
/// slot-recycling cadence §2 argues is the design's locality upside.
///
/// 16k, by measurement — the W9 sweep executed §2's own "swept empirically"
/// instruction and INVERTED its "larger budget spares churn rows" guess:
/// against the previous 64k default (nursery on both sides, six affected
/// rows), 16384 measured **−1.94% [−2.57, −0.34]** (markdown −6.1%,
/// polymorphic −4.2%, json −2.9%, regex −2.6%) while 131072 was +1.31% and
/// 262144 +3.64% — smaller-and-cheaper minors keep the recycled slot window
/// hot, the same cache-locality crossover the GC_GROWTH sweep above found for
/// majors. 8192 overshoots: async +7.6% (its reaction records die young but
/// not THAT young). async/map-set prefer fewer minors at every point — the
/// flip's named trades. `ZIPP_NURSERY_YOUNG_BUDGET=<n>` overrides for
/// sweeps. `GC_MIN_THRESHOLD` still gates the FIRST collection, so startup
/// is unchanged.
///
/// An unreachable OLD object is not freed by a minor — it FLOATS, occupying
/// its slot until a major. With the young-only trace the float mass can no
/// longer be measured exactly (old liveness is unknown at a minor), so the
/// stage-1 float census is replaced by `Heap::major_at`: the occupied count
/// grows through floats toward the PRE-nursery collection threshold, at which
/// point a major runs — i.e. majors happen no later than every collection
/// would have happened without the nursery, and the threshold is computed
/// from the last major's TRUE live count, never from a float-inflated
/// occupied count (B120's float-discount fix: churn rows stop compounding
/// the schedule off garbage they merely failed to sweep).
const NURSERY_YOUNG_BUDGET: usize = 1 << 14;

/// Default backstop: run a major after 64 consecutive minors even if the
/// occupied count never crosses `major_at`, so major-only hygiene (the
/// `brand_private_names` recompute, reclaiming table capacity) is never
/// deferred forever. `ZIPP_NURSERY_MAX_MINORS=<n>` is latched by each Heap so
/// the cadence can be measured rather than compiled in. Only 1..=4096 is
/// accepted: zero would suppress minors entirely, while an unbounded value
/// could defer hygiene for an effectively unlimited allocation span. W10 note:
/// at the adaptive cap (`NURSERY_BUDGET_MAX`) the default defers the hygiene
/// major to at most 64×128k allocations; `major_at` still bounds floats within
/// one (now larger) budget of the pre-nursery schedule.
const NURSERY_MAX_MINORS_DEFAULT: u32 = 64;
const NURSERY_MAX_MINORS_LIMIT: u32 = 4096;

#[inline]
fn parse_nursery_max_minors(raw: &str) -> Option<u32> {
    raw.parse::<u32>()
        .ok()
        .filter(|&value| (1..=NURSERY_MAX_MINORS_LIMIT).contains(&value))
}

/// W10: the survival-adaptive budget's ceiling (the floor is
/// [`NURSERY_YOUNG_BUDGET`]). 128k: the B122 sweep measured 131072 at +1.31%
/// SUITE-wide — but that was as a fixed budget for every row; the controller
/// reaches it only while young survival stays above ~25%, which on the
/// measured rows happens exactly where fewer minors pay
/// (async-promise-chain's chain-build phases: −3.3% at 64k in B121's trial).
const NURSERY_BUDGET_MAX: usize = 1 << 17;

/// Under `ZIPP_GC_STRESS=1` a collection runs at EVERY safe point; capping the
/// streak at 3 makes stress alternate minor,minor,minor,major so BOTH sweep
/// paths — and their interleavings over the same slots — are densely
/// exercised, instead of stress degenerating into minors alone.
const NURSERY_STRESS_MINORS: u32 = 3;

impl Default for Heap {
    fn default() -> Self {
        Heap::new()
    }
}

impl Heap {
    pub fn new() -> Heap {
        // Pre-intern the 128 single-ASCII-char strings (indices 0..127), the
        // empty string (128), and the fixed `"00".."99"` pad2 table
        // (INTERN_PAD2_START..=INTERN_PAD2_END). These are immutable and
        // ubiquitous; the final table lets Pad2Concat return a primitive
        // String without allocating. The entire prefix is OLD and pinned.
        let mut objs = Vec::with_capacity(256);
        let mut versions = Vec::with_capacity(256);
        for b in 0u8..128 {
            objs.push(HeapObj::Str(JsStr::new((b as char).to_string())));
            versions.push(0);
        }
        objs.push(HeapObj::Str(JsStr::new(String::new())));
        versions.push(0);
        for n in 0u8..100 {
            let bytes = vec![b'0' + n / 10, b'0' + n % 10];
            objs.push(HeapObj::Str(JsStr::from_ascii(bytes)));
            versions.push(0);
        }
        debug_assert_eq!(objs.len(), INTERN_PINNED_END as usize + 1);
        let live = objs.len();
        let resident_payload_charged: Vec<Cell<usize>> = objs
            .iter()
            .map(|obj| Cell::new(obj.resident_payload_bytes()))
            .collect();
        let resident_payload_high_water = resident_payload_charged
            .iter()
            .fold(0usize, |n, bytes| n.saturating_add(bytes.get()));
        let oracle = std::env::var_os("ZIPP_GCSTATS").is_some();
        let born = if oracle {
            vec![0; objs.len()]
        } else {
            Vec::new()
        };
        // Default-on since W9 (B122) — see the `nursery` field doc. The
        // ZIPP_NURSERY opt-in from waves 7-8 is subsumed (harmless if set).
        let nursery = std::env::var_os("ZIPP_NO_NURSERY").is_none();
        // The pre-interned prefix is pinned and immutable — OLD from birth.
        let gen = if nursery {
            vec![GEN_OLD; objs.len()]
        } else {
            Vec::new()
        };
        let budget_env = std::env::var("ZIPP_NURSERY_YOUNG_BUDGET")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&v| v >= 1024);
        let nursery_max_minors = std::env::var("ZIPP_NURSERY_MAX_MINORS")
            .ok()
            .as_deref()
            .and_then(parse_nursery_max_minors)
            .unwrap_or(NURSERY_MAX_MINORS_DEFAULT);
        // W10: an explicit budget (or the adapt kill switch) PINS it — the
        // survival controller below then never moves it.
        let budget_pinned =
            budget_env.is_some() || std::env::var_os("ZIPP_NO_NURSERY_ADAPT").is_some();
        let young_budget = budget_env.unwrap_or(NURSERY_YOUNG_BUDGET);
        let pretenure_on = std::env::var_os("ZIPP_NO_PRETENURE").is_none();
        let nonyoung_cache_on = std::env::var_os("ZIPP_NO_NONYOUNG_CACHE").is_none();
        let valgrain = std::env::var_os("ZIPP_NO_VALGRAIN_REMSET").is_none();
        let mut h = Heap {
            objs,
            resident_payload_current: Cell::new(resident_payload_high_water),
            resident_payload_high_water: Cell::new(resident_payload_high_water),
            resident_payload_charged,
            payload_accounting: Cell::new(false),
            shape_mirror: vec![crate::shape::DICT; versions.len()],
            vals_ptr_mirror: vec![0; versions.len()],
            shape_mirror_raw: 0,
            vals_ptr_mirror_raw: 0,
            versions,
            free: Vec::new(),
            live,
            gc_requested: false,
            // Keep the historical number of collectable allocations before
            // the first GC: the 100 new pad2 prefix slots are permanent OLD
            // objects, not young allocation pressure.
            gc_threshold: GC_MIN_THRESHOLD + INTERN_PAD2_COUNT as usize,
            born,
            epoch: 0,
            allocs_epoch: 0,
            oracle,
            young: Vec::new(),
            nursery,
            gen,
            remset: Vec::new(),
            scan_roots: Vec::new(),
            live_at_major: live,
            // Same offset as gc_threshold: a first minor's occupied count
            // includes these permanent slots, so preserve the old boundary in
            // terms of collectable survivors rather than total prefix size.
            major_at: GC_MIN_THRESHOLD + INTERN_PAD2_COUNT as usize,
            major_due: false,
            minors_since_major: 0,
            nursery_max_minors,
            young_budget,
            budget_pinned,
            pretenure: 0,
            pretenure_on,
            nonyoung_cache: Vec::new(),
            nonyoung_cache_valid: false,
            nonyoung_cache_on,
            vremset: Vec::new(),
            valgrain,
        };
        h.recache_mirror_raws();
        h
    }

    /// Re-cache the raw bases the emitted shape-way probes read through the
    /// VM. Called after any growth of the mirror vectors (and once at boot).
    #[inline]
    fn recache_mirror_raws(&mut self) {
        self.shape_mirror_raw = self.shape_mirror.as_ptr() as u64;
        self.vals_ptr_mirror_raw = self.vals_ptr_mirror.as_ptr() as u64;
    }

    /// Pin slot `idx`'s mirrors permanently unmatchable. For the receivers
    /// `ic_obj_ok` excludes from every property cache (the global object,
    /// %Array.prototype%, realm globals, module namespaces): their live
    /// semantics are layered OVER the ObjMap, the fill/repair path never
    /// touches them (the exclusion returns before it), and several are
    /// populated after allocation without version bumps — so their mirrors
    /// must never hold a guardable shape. The exclusion becomes an invariant
    /// here instead of an accident of shape non-collision (B178 review).
    #[inline]
    pub fn pin_mirror_dict(&mut self, idx: u32) {
        self.shape_mirror[idx as usize] = crate::shape::DICT;
        self.vals_ptr_mirror[idx as usize] = 0;
    }

    /// Refresh slot `idx`'s shape/vals mirrors from the live object — the
    /// settling events: allocation, wholesale replace, and the JIT miss
    /// helper's repair after resolving own data (see the field docs for why
    /// `bump_version` must NOT use this).
    #[inline]
    pub fn refresh_mirror(&mut self, idx: u32) {
        let (sh, vp) = match &self.objs[idx as usize] {
            HeapObj::Object(m) => (m.shape(), m.vals.as_ptr() as u64),
            _ => (crate::shape::DICT, 0),
        };
        self.shape_mirror[idx as usize] = sh;
        self.vals_ptr_mirror[idx as usize] = vp;
    }

    /// W9: enter a static-pretenure scope (NURSERY_DESIGN.md §4) — until the
    /// matching [`Heap::pretenure_end`], every allocation is stamped OLD-clean
    /// and skipped from the young log. Callers must pair begin/end on every
    /// path OUT of the scope including errors: a leaked depth silently turns
    /// the whole remaining run old-space. The wrong-guess cost is a float
    /// until the next major, never a copy, which is what licenses scoping
    /// whole builders. Inert unless the nursery is latched on (`alloc` only
    /// reads the depth under `self.nursery`).
    #[inline]
    pub fn pretenure_begin(&mut self) {
        if self.pretenure_on {
            self.pretenure += 1;
        }
    }

    /// W9: leave a static-pretenure scope. See [`Heap::pretenure_begin`].
    #[inline]
    pub fn pretenure_end(&mut self) {
        if self.pretenure_on {
            debug_assert!(self.pretenure > 0, "unbalanced pretenure_end");
            self.pretenure = self.pretenure.saturating_sub(1);
        }
    }

    #[inline]
    pub fn alloc(&mut self, obj: HeapObj) -> u32 {
        // Sizing every allocation is only worth paying once something reads
        // the figure; `audit_resident_bytes` turns accounting on and backfills
        // (see `payload_accounting`), so lazily-enabled totals stay exact.
        let payload = if self.payload_accounting.get() {
            let payload = obj.resident_payload_bytes();
            let current = self.resident_payload_current.get().saturating_add(payload);
            self.resident_payload_current.set(current);
            self.resident_payload_high_water
                .set(self.resident_payload_high_water.get().max(current));
            payload
        } else {
            0
        };
        self.live += 1;
        if self.live >= self.gc_threshold {
            self.gc_requested = true;
        }
        // Reuse a reclaimed slot when one is available (its version is bumped so a
        // stale inline-cache entry for the old occupant misses).
        if let Some(idx) = self.free.pop() {
            self.objs[idx as usize] = obj;
            debug_assert_eq!(self.resident_payload_charged[idx as usize].get(), 0);
            self.resident_payload_charged[idx as usize].set(payload);
            self.versions[idx as usize] = self.versions[idx as usize].wrapping_add(1);
            self.refresh_mirror(idx);
            if self.nursery {
                if self.pretenure == 0 {
                    self.young.push(idx);
                    // Clears every bit: a recycled slot sheds the dead occupant's
                    // remset/scan state (the scan_roots prune keys off this).
                    self.gen[idx as usize] = GEN_YOUNG;
                } else {
                    // W9 static pretenure: OLD-clean, not logged — the minor
                    // never sees it; `gen == GEN_YOUNG ⇔ in the young log`
                    // holds by both halves being false. Clears GEN_SCAN too,
                    // same as the young stamp.
                    self.gen[idx as usize] = GEN_OLD;
                }
            }
            if self.oracle {
                // A pretenured slot is stamped one epoch back so the oracle's
                // survival tables don't misread it as surviving young.
                self.born[idx as usize] = if self.pretenure == 0 {
                    self.epoch
                } else {
                    self.epoch.saturating_sub(1)
                };
                self.allocs_epoch += 1;
            }
            return idx;
        }
        let idx = self.objs.len() as u32;
        self.objs.push(obj);
        self.resident_payload_charged.push(Cell::new(payload));
        self.versions.push(0);
        self.shape_mirror.push(crate::shape::DICT);
        self.vals_ptr_mirror.push(0);
        self.recache_mirror_raws();
        self.refresh_mirror(idx);
        if self.nursery {
            if self.pretenure == 0 {
                self.young.push(idx);
                self.gen.push(GEN_YOUNG);
            } else {
                self.gen.push(GEN_OLD);
            }
        }
        if self.oracle {
            self.born.push(if self.pretenure == 0 {
                self.epoch
            } else {
                self.epoch.saturating_sub(1)
            });
            self.allocs_epoch += 1;
        }
        idx
    }

    /// Whether the B6 generational oracle is latched on (`ZIPP_GCSTATS=1`).
    #[inline]
    pub fn oracle_on(&self) -> bool {
        self.oracle
    }

    /// Oracle only: was slot `idx` allocated since the last collection?
    /// Callers must gate on [`Heap::oracle_on`] (`born` is empty otherwise).
    #[inline]
    pub fn oracle_young(&self, idx: u32) -> bool {
        self.born[idx as usize] == self.epoch
    }

    /// Oracle only: allocations in the current epoch (== young slot count).
    #[inline]
    pub fn oracle_allocs(&self) -> u64 {
        self.allocs_epoch
    }

    /// Oracle only: a collection completed — everything surviving is now old.
    #[inline]
    pub fn oracle_next_epoch(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.allocs_epoch = 0;
    }

    /// Whether the stage-1 nursery is latched on (`ZIPP_NO_NURSERY` unset).
    #[inline]
    pub fn nursery_on(&self) -> bool {
        self.nursery
    }

    /// Test-only: force the nursery latch (materialising the generation
    /// bytes), so the unit tests below hold in a suite run under
    /// `ZIPP_NO_NURSERY=1` too.
    #[cfg(test)]
    pub(crate) fn set_nursery(&mut self, on: bool) {
        self.nursery = on;
        if on && self.gen.len() != self.objs.len() {
            self.gen = vec![GEN_OLD; self.objs.len()];
        }
        self.invalidate_nonyoung_cache();
    }

    /// Whether the collection about to run may be a MINOR (young-only trace,
    /// sweep only the young log, unreachable OLD objects float). A MAJOR runs
    /// instead when: the nursery is off; the previous minor left the heap at
    /// or above `major_at` (growth the young sweep could not reclaim —
    /// survivors and floats); or the backstop streak runs out. Under
    /// `ZIPP_GC_STRESS` the streak cap drops to 3 so a collection at every
    /// safe point exercises both paths densely, not minors alone.
    #[inline]
    pub fn minor_due(&self, stress: bool) -> bool {
        let cap = if stress {
            NURSERY_STRESS_MINORS
        } else {
            self.nursery_max_minors
        };
        self.nursery && !self.major_due && self.minors_since_major < cap
    }

    /// The slots allocated since the last collection (the minor sweep set).
    #[inline]
    pub fn young_log(&self) -> &[u32] {
        &self.young
    }

    /// Promote everything logged so far and drop the log (called from
    /// `set_gc_floor`: the boot allocations are pinned forever, so they are
    /// OLD from the floor on and the first minor never walks them).
    pub fn young_reset(&mut self) {
        for &i in &self.young {
            self.gen[i as usize] = GEN_OLD;
        }
        self.young.clear();
        self.invalidate_nonyoung_cache();
    }

    /// MINOR sweep: walk ONLY the young log, freeing the unmarked entries —
    /// `free_slot` exactly as the major does (tombstone + version bump + free
    /// list), so every stale inline cache misses identically. Survivors are
    /// PROMOTED (`GEN_OLD`, keeping a sticky scan bit) — promotion is a
    /// bookkeeping byte, never a copy (NURSERY_DESIGN.md §0). `free_slot`
    /// appends each reclaimed slot to the persistent free list; the caller
    /// records that list's pre-sweep length and uses the appended suffix as its
    /// exact side-table prune set. The young log is then cleared (capacity
    /// kept), so no duplicate per-minor `Vec<u32>` is needed.
    ///
    /// No double-free is possible: a young slot is never already on the free
    /// list here, because the free list is refilled only by sweeps and a
    /// freed slot re-enters the log only when `alloc` hands it out again.
    pub fn sweep_young(&mut self, marks: &[bool]) -> usize {
        let mut log = std::mem::take(&mut self.young);
        let mut swept = 0;
        for &idx in &log {
            if !marks[idx as usize] {
                self.free_slot(idx);
                swept += 1;
            } else {
                // Promote: OLD, keeping only the sticky scan bit — a stale
                // GEN_VLOG must not survive promotion (W10).
                let g = &mut self.gen[idx as usize];
                *g = (*g & GEN_SCAN) | GEN_OLD;
            }
        }
        log.clear();
        self.young = log;
        swept
    }

    // ── stage-3 write barrier + remembered set ─────────────────────────────

    /// Holder-grain write barrier (the design doc's dirty-object/card form):
    /// `holder` was just stored into; if it is OLD and clean, remember it —
    /// the next minor re-traces its full edge list. One latched bool + one
    /// byte compare when the store is not the holder's first this epoch.
    /// Used where the stored value is not at hand (batch mutators, reg
    /// re-parks, `Heap::replace`); the value-tested form below is the hot-path
    /// spelling.
    #[inline]
    pub fn write_barrier(&mut self, holder: u32) {
        if self.nursery && self.gen[holder as usize] & GEN_STATE == GEN_OLD {
            self.dirty(holder);
        }
    }

    /// Value-tested write barrier. W10 (B123): VALUE-GRAIN — when `v` is a
    /// young heap value not yet recorded this epoch (`GEN_VLOG` clear, one
    /// masked compare tests young+unrecorded together) and `holder` is
    /// OLD-clean, record the VALUE itself in `vremset`; the minor marks it
    /// directly, and the holder's full edge-list re-trace stops existing for
    /// value-form stores. A `GEN_DIRTY` holder is deliberately SKIPPED — a
    /// card-dirtied holder gets a full re-trace anyway, so recording its
    /// values would be redundant work. The conservative trade: a recorded
    /// value later overwritten before the minor is still kept one epoch (a
    /// float, reclaimed on the unchanged `major_at` schedule); on the
    /// measured rows the added float is zero (the stores are retained
    /// appends). `ZIPP_NO_VALGRAIN_REMSET=1` restores the holder-grain body.
    #[inline]
    pub fn write_barrier_val(&mut self, holder: u32, v: Value) {
        if !self.nursery {
            return;
        }
        if self.valgrain {
            if v.is_heap()
                && self.gen[v.heap_index() as usize] & (GEN_STATE | GEN_VLOG) == GEN_YOUNG
                && self.gen[holder as usize] & GEN_STATE == GEN_OLD
            {
                self.gen[v.heap_index() as usize] |= GEN_VLOG;
                self.vremset.push(v.heap_index());
            }
        } else if self.gen[holder as usize] & GEN_STATE == GEN_OLD
            && v.is_heap()
            && self.gen[v.heap_index() as usize] & GEN_STATE == GEN_YOUNG
        {
            self.dirty(holder);
        }
    }

    #[inline]
    fn dirty(&mut self, holder: u32) {
        let g = &mut self.gen[holder as usize];
        *g = (*g & !GEN_STATE) | GEN_DIRTY;
        self.remset.push(holder);
    }

    /// Register `holder` as a PERSISTENT minor-trace root: a JIT cache was
    /// just filled (or a plan baked) that can store into it CALL-FREE, so no
    /// barrier will ever see those stores. Every minor re-scans its edges
    /// instead. Sticky until the slot is recycled (`alloc` clears the bit and
    /// the minor prune drops the entry); bounded by the number of distinct
    /// receivers ever filled into a Set-capable way, which the 8-way IC and
    /// the ≤8-arm method-inline plans keep small in practice.
    #[inline]
    #[cfg(any(test, all(feature = "jit", target_arch = "x86_64")))]
    pub fn register_scan_root(&mut self, holder: u32) {
        if self.nursery && self.gen[holder as usize] & GEN_SCAN == 0 {
            self.gen[holder as usize] |= GEN_SCAN;
            self.scan_roots.push(holder);
        }
    }

    /// The old holders a minor must re-trace: the remembered set (this
    /// epoch's barrier hits) plus the persistent scan roots, pruned of
    /// recycled slots. Returned as a snapshot so the caller can trace while
    /// borrowing the heap.
    pub fn dirty_for_trace(&mut self) -> Vec<u32> {
        let gen = &self.gen;
        self.scan_roots.retain(|&i| gen[i as usize] & GEN_SCAN != 0);
        let mut out = Vec::with_capacity(self.remset.len() + self.scan_roots.len());
        out.extend_from_slice(&self.remset);
        // A holder can be in both (dirtied while also registered): tracing an
        // edge list twice is harmless (the second scan pushes nothing new),
        // so no dedup pass is spent here.
        out.extend_from_slice(&self.scan_roots);
        out
    }

    /// End-of-minor: every remembered holder goes back to OLD-clean (its
    /// young referents were just promoted, so its edges are old→old now) and
    /// the set drains. The scan roots persist by design.
    pub fn remset_reset(&mut self) {
        let remset = std::mem::take(&mut self.remset);
        for &h in &remset {
            let g = &mut self.gen[h as usize];
            *g = (*g & !GEN_STATE) | GEN_OLD;
        }
        let mut remset = remset;
        remset.clear();
        self.remset = remset;
    }

    /// W10: the value-grain remembered set — young value indices recorded by
    /// `write_barrier_val` this epoch, for the minor to mark directly.
    #[inline]
    pub fn value_remset(&self) -> &[u32] {
        &self.vremset
    }

    /// W10 survival-adaptive young budget (B123): called by `gc_minor` after
    /// each sweep with the just-ended epoch's survivor and log counts,
    /// BEFORE `note_minor_done` reads the budget for the next threshold.
    /// Bang-bang with a 5×-wide dead band: survival above ~25% doubles the
    /// budget (minors are reclaiming little — the async chain-build shape,
    /// which measured −3.3% at a 64k budget while paying +7.6% at 8k), below
    /// ~5% halves it back toward the 16k floor (churn rows: json 0.2%
    /// post-pretenure, regex 3.4%, map-set 0.0% — all measured 16k-optimal).
    /// Monotone negative feedback (survival falls as the budget grows), so a
    /// stationary workload converges to a clamp bound or the dead band.
    /// Skipped when pinned (`ZIPP_NURSERY_YOUNG_BUDGET` /
    /// `ZIPP_NO_NURSERY_ADAPT=1`), on an empty log (an all-pretenured
    /// epoch), and under GC stress (the caller guards — epochs of ~1 alloc
    /// make the ratio noise).
    pub fn adapt_young_budget(&mut self, survivors: usize, log_len: usize) {
        if self.budget_pinned || log_len == 0 {
            return;
        }
        if survivors * 4 > log_len {
            self.young_budget = (self.young_budget * 2).min(NURSERY_BUDGET_MAX);
        } else if survivors * 20 < log_len {
            self.young_budget = (self.young_budget / 2).max(NURSERY_YOUNG_BUDGET);
        }
    }

    /// W10: the live young budget (for the GCSTATS report).
    #[inline]
    pub fn young_budget(&self) -> usize {
        self.young_budget
    }

    /// W10: drop the epoch's value records (capacity kept — steady state
    /// never reallocates, the young-log pattern). The recorded values were
    /// just promoted or swept, and a promoted slot's stale `GEN_VLOG` is
    /// stripped by the promote arms; a swept slot's by `free_slot`.
    pub fn vremset_reset(&mut self) {
        self.vremset.clear();
    }

    /// A minor needs `marks[i] == true` for every NON-YOUNG slot (old objects
    /// are boundary nodes presumed live; free tombstones are `GEN_OLD` by
    /// `free_slot`), so the shared root walk and the unchanged `trace_edges`
    /// push — and therefore trace — ONLY young objects.
    pub fn gen_nonyoung_marks(&self) -> Vec<bool> {
        self.gen
            .iter()
            .map(|&g| g & GEN_STATE != GEN_YOUNG)
            .collect()
    }

    /// W10: the minor's mark vector WITHOUT the O(heap) rebuild. Building
    /// `gen_nonyoung_marks` fresh at every minor measured 0.039ns/slot — at
    /// async-promise-chain's 1.37M slots × 463 minors that was 25.4ms/run,
    /// misattributed to "roots" (B123). A minor ends with the vector ALL TRUE
    /// (every young-log slot was either promoted — marked live by the trace —
    /// or freed and restored by `gc_minor`), which is exactly the next
    /// minor's starting point for every slot that is not in the NEW young
    /// log: an old slot stays old between minors, a recycled slot re-enters
    /// the log, a fresh push is either logged (young) or pretenured (old,
    /// covered by the `resize(.., true)`). So the stashed vector plus one
    /// O(young-log) clearing pass reproduces the fresh build. The dangerous
    /// direction — a stale TRUE on a young slot, whose referents the trace
    /// would then skip and sweep alive — is impossible by construction (every
    /// current log entry is cleared unconditionally); a stale FALSE on an old
    /// slot only over-traces. `gc_minor` re-verifies equivalence against the
    /// fresh build under `ZIPP_NURSERY_VERIFY=1`.
    /// `ZIPP_NO_NONYOUNG_CACHE=1` restores the per-minor rebuild.
    pub fn take_nonyoung_marks(&mut self) -> Vec<bool> {
        if !self.nonyoung_cache_on || !self.nonyoung_cache_valid {
            return self.gen_nonyoung_marks();
        }
        let mut m = std::mem::take(&mut self.nonyoung_cache);
        self.nonyoung_cache_valid = false;
        m.resize(self.objs.len(), true);
        for &i in &self.young {
            m[i as usize] = false;
        }
        m
    }

    /// W10: hand the minor's mark vector back for reuse. Called by `gc_minor`
    /// only after the freed-slot restore, when the vector is all-true again.
    pub fn stash_nonyoung_marks(&mut self, m: Vec<bool>) {
        if !self.nonyoung_cache_on {
            return;
        }
        self.nonyoung_cache = m;
        self.nonyoung_cache_valid = true;
    }

    /// W10: drop the cached mark vector — called wherever `gen` is rewritten
    /// outside the minor path (a major's wholesale promote, `young_reset`,
    /// the `set_nursery` test hook). The next minor rebuilds cold, exactly
    /// today's cost, once.
    fn invalidate_nonyoung_cache(&mut self) {
        self.nonyoung_cache_valid = false;
    }

    /// Total slot count (live + free + pinned). Sweeps iterate `0..len`.
    #[inline]
    pub fn len(&self) -> usize {
        self.objs.len()
    }

    /// O(1) conservative resident-byte high-water estimate. Initial payloads are
    /// charged at allocation and [`Self::audit_resident_bytes`] periodically
    /// incorporates capacity growth in existing objects. The payload peak is
    /// monotonic, while per-slot charges prevent freed/reused allocation churn
    /// from being mistaken for simultaneously resident memory.
    pub fn resident_bytes(&self) -> usize {
        vec_capacity_bytes(&self.objs)
            .saturating_add(vec_capacity_bytes(&self.resident_payload_charged))
            .saturating_add(vec_capacity_bytes(&self.versions))
            .saturating_add(vec_capacity_bytes(&self.free))
            .saturating_add(vec_capacity_bytes(&self.born))
            .saturating_add(vec_capacity_bytes(&self.young))
            .saturating_add(vec_capacity_bytes(&self.gen))
            .saturating_add(vec_capacity_bytes(&self.remset))
            .saturating_add(vec_capacity_bytes(&self.scan_roots))
            .saturating_add(self.nonyoung_cache.capacity().div_ceil(8))
            .saturating_add(vec_capacity_bytes(&self.vremset))
            .saturating_add(self.resident_payload_high_water.get())
    }

    /// Reconcile in-place payload growth into the current and peak caches. This
    /// is O(n) in heap slots and is therefore deliberately called much less
    /// often than the abort/step poll; new objects are charged eagerly once a
    /// first audit proves a consumer exists (see `payload_accounting`) — this
    /// full walk doubles as the backfill for allocations made before that.
    pub fn audit_resident_bytes(&self) -> usize {
        self.payload_accounting.set(true);
        debug_assert_eq!(self.objs.len(), self.resident_payload_charged.len());
        let mut payload = 0usize;
        for (obj, charged) in self.objs.iter().zip(&self.resident_payload_charged) {
            let bytes = obj.resident_payload_bytes();
            charged.set(bytes);
            payload = payload.saturating_add(bytes);
        }
        self.resident_payload_current.set(payload);
        self.resident_payload_high_water
            .set(self.resident_payload_high_water.get().max(payload));
        self.resident_bytes()
    }

    /// Whether the dispatch loop should run a collection (live count passed the
    /// adaptive threshold). Cleared by `note_gc_done`.
    #[inline]
    pub fn gc_requested(&self) -> bool {
        self.gc_requested
    }

    /// The currently-free slot indices (so the GC can protect them from a
    /// double-free without tracing them).
    #[inline]
    pub fn free_indices(&self) -> &[u32] {
        &self.free
    }

    /// Reclaim slot `idx`: drop its (possibly large) contents to a tiny tombstone
    /// and return the slot to the free list. The caller (GC sweep) guarantees no
    /// live reference remains. Never call on a pinned built-in slot.
    #[inline]
    pub fn free_slot(&mut self, idx: u32) {
        self.shape_mirror[idx as usize] = crate::shape::DICT;
        self.vals_ptr_mirror[idx as usize] = 0;
        let payload = self.resident_payload_charged[idx as usize].replace(0);
        self.resident_payload_current
            .set(self.resident_payload_current.get().saturating_sub(payload));
        self.objs[idx as usize] = HeapObj::Date(f64::NAN);
        self.versions[idx as usize] = self.versions[idx as usize].wrapping_add(1);
        if self.nursery {
            // Tombstones read OLD: `gen == GEN_YOUNG ⇔ in the young log`
            // stays exact, so a minor's boundary marks cover free slots too.
            self.gen[idx as usize] = GEN_OLD;
        }
        self.free.push(idx);
    }

    /// Record the post-sweep live count and grow the next threshold to ~2x it
    /// (amortising collection cost), clearing the request flag.
    ///
    /// The threshold is also floored at half the TOTAL slot count: a sweep
    /// walks `0..objs.len()` (tombstones included — the slot Vec never
    /// shrinks), so once a burst has grown the heap to N slots, collecting
    /// every `2*live` allocs on a small live set costs O(N) per O(live)
    /// allocations — quadratic for alloc-heavy, low-retention phases (e.g. a
    /// 3M-promise `Promise.all` workload). Requiring ~N/2 allocations between
    /// sweeps keeps the amortized cost O(1) per alloc; the reused slots come
    /// from the free list, so peak memory is unchanged.
    #[inline]
    pub fn note_gc_done(&mut self, live: usize) {
        self.live = live;
        // A MAJOR completed: nothing unreachable survived it, so `live` is
        // the TRUE live count — the anchor both schedules grow from. Floats
        // never enter this math (B120's float-discount fix): a minor reports
        // an occupied count, but the major threshold is only ever recomputed
        // here, from a count with zero floated garbage in it.
        self.major_at = (live.saturating_mul(GC_GROWTH))
            .max(GC_MIN_THRESHOLD)
            .max(self.objs.len() / 2);
        self.gc_threshold = if self.nursery {
            // Next collection: a minor, one young budget from now (whether it
            // stays a minor is `note_minor_done`'s post-sweep decision).
            live.saturating_add(self.young_budget)
        } else {
            self.major_at
        };
        self.gc_requested = false;
        self.live_at_major = live;
        self.minors_since_major = 0;
        self.major_due = false;
        if self.nursery {
            // Every survivor of a major is OLD (and the remembered set is
            // stale — its young referents were just promoted or swept).
            // Keeping only GEN_SCAN also strips any stale GEN_VLOG (W10) —
            // the value-grain dedup bit must not outlive its vremset entry.
            for g in &mut self.gen {
                *g = (*g & GEN_SCAN) | GEN_OLD;
            }
            self.remset.clear();
            self.vremset.clear();
        }
        self.young.clear();
        self.invalidate_nonyoung_cache();
    }

    /// [`Heap::note_gc_done`]'s MINOR twin. `live` is the post-sweep OCCUPIED
    /// slot count (reachable + floated). The young-only trace cannot measure
    /// floats, so the major decision is made from what the minor could NOT
    /// reclaim: an occupied count still at/above the pre-nursery collection
    /// point (`major_at`) is survivors + floats, and the next collection
    /// majors. Pure-churn heaps stay below it and run minors indefinitely
    /// (the backstop aside); float/survivor growth reaches a major within
    /// one young budget of where today's collector would have collected.
    #[inline]
    pub fn note_minor_done(&mut self, live: usize) {
        self.live = live;
        // Refresh the sweep-amortisation floor: the slot vector may have
        // grown since the last major, and a major sweep walks all of it.
        self.major_at = self.major_at.max(self.objs.len() / 2);
        if live >= self.major_at {
            self.major_due = true;
        }
        self.gc_threshold = live.saturating_add(self.young_budget);
        self.gc_requested = false;
        self.minors_since_major += 1;
        self.young.clear();
    }

    /// Overwrite the whole object at `idx`, bumping its version.
    ///
    /// A subclass constructor's `super()` allocates a plain object and then
    /// REPLACES it in place with the exotic one the base class produces (a Map, a
    /// Set, a Promise, a cloned Array). That drops the old `ObjMap` and frees its
    /// `vals` buffer, so any cache holding a `vals_ptr` for this slot is left
    /// pointing at freed memory — the version bump is what makes it miss.
    ///
    /// This has been safe in practice because the replaced object is always one
    /// allocated moments earlier inside the same `super()` call, which no cache
    /// has had the chance to see. That is an argument about timing, not an
    /// invariant, and it is not one a shape-keyed guard should have to make.
    #[inline]
    pub fn replace(&mut self, idx: u32, obj: HeapObj) {
        if self.payload_accounting.get() {
            let new_payload = obj.resident_payload_bytes();
            let old_payload = self.resident_payload_charged[idx as usize].replace(new_payload);
            let current = self
                .resident_payload_current
                .get()
                .saturating_sub(old_payload)
                .saturating_add(new_payload);
            self.resident_payload_current.set(current);
            self.resident_payload_high_water
                .set(self.resident_payload_high_water.get().max(current));
        }
        // Nursery barrier (NURSERY_DESIGN.md §1 case 8): the incoming object
        // may hold young references; if the SLOT is old, its whole edge list
        // is re-traced at the next minor (holder-grain — the value set is a
        // full HeapObj, not a single Value).
        self.write_barrier(idx);
        self.objs[idx as usize] = obj;
        self.versions[idx as usize] = self.versions[idx as usize].wrapping_add(1);
        self.refresh_mirror(idx);
    }

    /// Bump object `idx`'s version (call after a key-add reallocates its `vals`).
    ///
    /// The counter is `u32`. A false inline-cache hit would require it to wrap
    /// (2^32 key-adds to a SINGLE object); that is ~36 GB of keys on one object
    /// (OOM long before), and the cache is re-filled on every miss, so it is
    /// practically unreachable. A `u64` would remove even the theoretical edge.
    #[inline]
    pub fn bump_version(&mut self, idx: u32) {
        self.versions[idx as usize] = self.versions[idx as usize].wrapping_add(1);
        // Invalidate, don't refresh: callers bump on either side of the
        // mutation, and a refresh here would capture the wrong side at a
        // bump-first site — a shape-way hit has no second guard to catch it.
        // The next miss on the object repairs the mirror from the settled map.
        self.shape_mirror[idx as usize] = crate::shape::DICT;
        self.vals_ptr_mirror[idx as usize] = 0;
    }

    /// Base pointer of the parallel version array (for the JIT inline cache). The
    /// array does not reallocate during a native region run (a region never
    /// allocates a heap object), so this stays valid for the run.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub fn versions_ptr(&self) -> *const u32 {
        self.versions.as_ptr()
    }

    /// Current version of object `idx` (for filling an inline-cache entry).
    #[inline]
    pub fn version_of(&self, idx: u32) -> u32 {
        self.versions[idx as usize]
    }

    #[inline]
    pub fn get(&self, idx: u32) -> &HeapObj {
        &self.objs[idx as usize]
    }

    #[inline]
    pub fn get_mut(&mut self, idx: u32) -> &mut HeapObj {
        &mut self.objs[idx as usize]
    }

    /// Visit every compiled RegExp program held directly by a live heap slot.
    /// Both the authoritative Unicode program and its optional ASCII byte-opt
    /// twin participate. Callers deduplicate the shared `Arc` identities.
    pub(crate) fn visit_regexp_programs(
        &self,
        mut visit: impl FnMut(&std::sync::Arc<regress::Regex>),
    ) {
        for obj in &self.objs {
            if let HeapObj::RegExp {
                regex, ascii_twin, ..
            } = obj
            {
                visit(regex);
                if let Some(Some(twin)) = ascii_twin {
                    visit(twin);
                }
            }
        }
    }

    #[inline]
    pub fn alloc_str(&mut self, s: String) -> u32 {
        // Reuse the interned slot for the empty string and single-ASCII-char
        // strings instead of allocating (see `Heap::new`). Safe because strings
        // are immutable — nothing ever mutates a heap string in place.
        match s.len() {
            0 => return INTERN_EMPTY,
            1 => {
                let b = s.as_bytes()[0];
                if b < 128 {
                    return b as u32;
                }
            }
            _ => {}
        }
        self.alloc(HeapObj::Str(JsStr::new(s)))
    }

    /// `alloc_str` for an already-built `JsStr` (the WTF-8 creation sites):
    /// same interning of the empty / single-ASCII-char strings.
    pub fn alloc_js(&mut self, js: JsStr) -> u32 {
        let b = js.as_bytes();
        match b.len() {
            0 => return INTERN_EMPTY,
            1 if b[0] < 128 => return b[0] as u32,
            _ => {}
        }
        self.alloc(HeapObj::Str(js))
    }

    /// Allocate a rope node over two string-like children (O(1) concatenation).
    /// `len` is the children's combined length in the SAME measure as
    /// `JsStr::units` (UTF-16 code units) — `str_units` of both sides summed,
    /// which stays additive across concatenation.
    #[inline]
    pub fn alloc_cons(&mut self, left: u32, right: u32, len: usize) -> u32 {
        self.alloc(HeapObj::Cons { left, right, len })
    }

    /// Concatenate two string-likes into a fresh FLAT string. Used by `+` for
    /// SMALL results (a rope node + the inevitable flatten on first access
    /// costs two allocations and an objs write-back; a small copy is cheaper —
    /// same reasoning as V8's ConsString minimum length). `write_wtf8` appends
    /// leaf-by-leaf through `wtf8_push`, so a surrogate pair joining at the
    /// seam canonicalizes exactly as the rope's flatten would.
    pub fn alloc_concat_flat(&mut self, left: u32, right: u32, units: usize) -> u32 {
        // Both children flat and well-formed (the overwhelmingly common case):
        // plain byte concat — no seam canonicalization is possible (a
        // well-formed string cannot end/start mid-surrogate) and the cached
        // metadata composes, so neither scan nor `write_wtf8`'s walk runs.
        if let (HeapObj::Str(l), HeapObj::Str(r)) =
            (&self.objs[left as usize], &self.objs[right as usize])
        {
            if l.wellformed && r.wellformed {
                debug_assert_eq!(l.units + r.units, units);
                let mut bytes = Vec::with_capacity(l.bytes.len() + r.bytes.len());
                bytes.extend_from_slice(&l.bytes);
                bytes.extend_from_slice(&r.bytes);
                let ascii = l.ascii && r.ascii;
                return self.alloc(HeapObj::Str(JsStr {
                    bytes,
                    units,
                    ascii,
                    wellformed: true,
                }));
            }
        }
        let mut out = Vec::with_capacity(units * 3); // ≤ 3 WTF-8 bytes per UTF-16 unit
        self.write_wtf8(left, &mut out);
        self.write_wtf8(right, &mut out);
        self.alloc(HeapObj::Str(JsStr::from_wtf8(out)))
    }

    /// `left + tail` / `head + right` where the raw side is ASCII bytes (an
    /// int's decimal form): ONE flat allocation, the bytes written straight
    /// into the result buffer — no intermediate heap string for the number.
    /// The heap side must be FLAT (`Str`) — `None` falls back to the general
    /// path. An ASCII seam can never canonicalize (merging needs a surrogate
    /// half on EACH side), so plain byte concat is exact even when the string
    /// side holds lone surrogates.
    pub fn alloc_concat_str_ascii(&mut self, left: u32, tail: &[u8]) -> Option<u32> {
        debug_assert!(tail.is_ascii());
        let js = match &self.objs[left as usize] {
            HeapObj::Str(l) => {
                let mut bytes = Vec::with_capacity(l.bytes.len() + tail.len());
                bytes.extend_from_slice(&l.bytes);
                bytes.extend_from_slice(tail);
                JsStr {
                    bytes,
                    units: l.units + tail.len(),
                    ascii: l.ascii,
                    wellformed: l.wellformed,
                }
            }
            _ => return None,
        };
        Some(self.alloc(HeapObj::Str(js)))
    }

    /// See `alloc_concat_str_ascii` — the mirrored `head + right` order.
    pub fn alloc_concat_ascii_str(&mut self, head: &[u8], right: u32) -> Option<u32> {
        debug_assert!(head.is_ascii());
        let js = match &self.objs[right as usize] {
            HeapObj::Str(r) => {
                let mut bytes = Vec::with_capacity(head.len() + r.bytes.len());
                bytes.extend_from_slice(head);
                bytes.extend_from_slice(&r.bytes);
                JsStr {
                    bytes,
                    units: head.len() + r.units,
                    ascii: r.ascii,
                    wellformed: r.wellformed,
                }
            }
            _ => return None,
        };
        Some(self.alloc(HeapObj::Str(js)))
    }

    /// Is this heap object a string — flat `Str` or rope `Cons`?
    #[inline]
    pub fn is_str_like(&self, idx: u32) -> bool {
        matches!(self.get(idx), HeapObj::Str(_) | HeapObj::Cons { .. })
    }

    /// UTF-16 code-unit length of a string-like object (the JS `.length`) —
    /// O(1): a rope stores it; a flat `JsStr` caches it (computed once in
    /// `JsStr::new`). `None` if not a string.
    pub fn str_units(&self, idx: u32) -> Option<usize> {
        match self.get(idx) {
            HeapObj::Str(s) => Some(s.units()),
            HeapObj::Cons { len, .. } => Some(*len),
            _ => None,
        }
    }

    /// `Some(true)` if the string-like object is empty (O(1)); `None` if not a
    /// string. Reads the cached/stored length rather than scanning the bytes.
    #[inline]
    pub fn str_is_empty(&self, idx: u32) -> Option<bool> {
        match self.get(idx) {
            HeapObj::Str(s) => Some(s.units() == 0),
            HeapObj::Cons { len, .. } => Some(*len == 0),
            _ => None,
        }
    }

    /// Append the full WTF-8 content of a (possibly rope) string to `out`,
    /// canonicalizing each segment seam (a high surrogate ending one segment
    /// merges with a low surrogate opening the next — `wtf8_push`).
    /// Iterative, not recursive: a `s += x` loop builds a left-leaning rope that
    /// can be thousands of nodes deep, which would overflow the stack.
    pub fn write_wtf8(&self, idx: u32, out: &mut Vec<u8>) {
        // Explicit stack; push the right child then the left so the left is
        // popped (appended) first — preserving left-to-right concatenation.
        let mut stack = vec![idx];
        while let Some(n) = stack.pop() {
            match self.get(n) {
                HeapObj::Str(s) => wtf8_push(out, s.as_bytes()),
                HeapObj::Cons { left, right, .. } => {
                    stack.push(*right);
                    stack.push(*left);
                }
                _ => {}
            }
        }
    }

    /// Borrow a string-like as `&str` without allocating when it is already
    /// flat AND well-formed (the common case); materialize a rope / a
    /// lone-surrogate string into an owned `String` otherwise — LOSSY for lone
    /// surrogates (each reads as U+FFFD, byte-length preserving, so positions
    /// stay exchangeable with the exact bytes). Exact consumers use
    /// `str_wtf8_cow`. `None` if `idx` isn't a string.
    pub fn str_cow(&self, idx: u32) -> Option<Cow<'_, str>> {
        match self.get(idx) {
            HeapObj::Str(s) => Some(s.as_str_lossy()),
            HeapObj::Cons { len, .. } => {
                let mut out = Vec::with_capacity(*len);
                self.write_wtf8(idx, &mut out);
                Some(Cow::Owned(wtf8_into_lossy_string(out)))
            }
            _ => None,
        }
    }

    /// The EXACT (WTF-8) byte content of a string-like: borrowed when flat,
    /// materialized (with seam canonicalization) for a rope. `None` if not a
    /// string.
    pub fn str_wtf8_cow(&self, idx: u32) -> Option<Cow<'_, [u8]>> {
        match self.get(idx) {
            HeapObj::Str(s) => Some(Cow::Borrowed(s.as_bytes())),
            HeapObj::Cons { len, .. } => {
                let mut out = Vec::with_capacity(*len);
                self.write_wtf8(idx, &mut out);
                Some(Cow::Owned(out))
            }
            _ => None,
        }
    }

    /// Whether every flat leaf under a string-like object is well-formed —
    /// from the cached `JsStr` flags only, no flattening or byte scan. All
    /// leaves well-formed ⇒ the concatenation holds no surrogate bytes at
    /// all ⇒ well-formed. (The converse may not hold — a surrogate pair
    /// joining at a rope seam canonicalizes away on flatten — so a `false`
    /// here is only a conservative "may hold lone surrogates".)
    fn str_leaves_wellformed(&self, idx: u32) -> bool {
        match self.get(idx) {
            HeapObj::Str(s) => s.is_wellformed(),
            HeapObj::Cons { left, right, .. } => {
                self.str_leaves_wellformed(*left) && self.str_leaves_wellformed(*right)
            }
            _ => true,
        }
    }

    /// EXACT WTF-8 bytes of a string-like object, but ONLY when it is NOT
    /// well-formed (holds lone surrogates) — `None` for a well-formed string
    /// or a non-string. Rejection reads cached flags (O(1) flat, O(leaves)
    /// rope) — no flattening or byte scan on the well-formed path. The side
    /// channel for paths whose lossy `String` view would decay surrogates to
    /// U+FFFD (eval source capture, RegExp pattern/source exactness).
    pub fn str_exact_if_not_wellformed(&self, idx: u32) -> Option<Vec<u8>> {
        if !self.is_str_like(idx) || self.str_leaves_wellformed(idx) {
            return None;
        }
        // Flatten and re-check: a rope seam can canonicalize a high+low pair
        // into an astral scalar, leaving the WHOLE string well-formed even
        // though a leaf was not.
        let b = self.str_wtf8_cow(idx)?;
        (!wtf8_is_wellformed(&b)).then(|| b.into_owned())
    }

    /// Content equality of two string-like objects. Fast (no allocation) when
    /// both are already flat — the common case for a hot `a === b` comparison.
    /// Byte equality IS content equality: the WTF-8 buffers are canonical
    /// (`write_wtf8`/`wtf8_push` merge cross-segment surrogate pairs), so two
    /// equal unit sequences always have identical bytes.
    pub fn str_eq(&self, a: u32, b: u32) -> bool {
        match (self.get(a), self.get(b)) {
            (HeapObj::Str(x), HeapObj::Str(y)) => x.as_bytes() == y.as_bytes(),
            _ => {
                let (mut sa, mut sb) = (Vec::new(), Vec::new());
                self.write_wtf8(a, &mut sa);
                self.write_wtf8(b, &mut sb);
                sa == sb
            }
        }
    }

    /// Flatten the rope at `idx` into a contiguous `Str` in place. No-op if it is
    /// already flat (or not a string). The already-flat fast path is a single tag
    /// check, so this is cheap to call unconditionally before content access.
    #[inline]
    pub fn flatten(&mut self, idx: u32) {
        if matches!(self.objs[idx as usize], HeapObj::Cons { .. }) {
            self.flatten_cold(idx);
        }
    }

    #[cold]
    fn flatten_cold(&mut self, idx: u32) {
        let len = match &self.objs[idx as usize] {
            HeapObj::Cons { len, .. } => *len,
            _ => return,
        };
        let mut out = Vec::with_capacity(len);
        self.write_wtf8(idx, &mut out);
        self.objs[idx as usize] = HeapObj::Str(JsStr::from_wtf8(out));
    }

    /// Resolve a callable (plain function or closure) to its function id and
    /// upvalue list. Returns `None` for non-callables.
    #[inline]
    pub fn as_callable(&self, idx: u32) -> Option<(u32, &[u32])> {
        match self.get(idx) {
            HeapObj::Func(id) => Some((*id, &[])),
            HeapObj::Closure { func, upvalues, .. } => Some((*func, upvalues.as_slice())),
            _ => None,
        }
    }

    #[inline]
    pub fn cell_get(&self, idx: u32) -> Value {
        match self.get(idx) {
            HeapObj::Cell(v) => *v,
            _ => Value::UNDEFINED,
        }
    }

    #[inline]
    pub fn cell_set(&mut self, idx: u32, v: Value) {
        // Nursery barrier + B6 oracle: a captured-variable write is edge
        // source 3 of NURSERY_DESIGN.md §1 (CellSet/CellSetChecked/UpvalSet
        // all land here — the JIT's `jit_cell_set`/`jit_upval_set` too).
        self.write_barrier_val(idx, v);
        if self.oracle
            && v.is_heap()
            && !self.oracle_young(idx)
            && self.oracle_young(v.heap_index())
        {
            gcoracle::hit(gcoracle::CELL_SET);
        }
        if let HeapObj::Cell(slot) = self.get_mut(idx) {
            *slot = v;
        }
    }
}

/// B6 generational-ORACLE store counters (`ZIPP_GCSTATS=1` only): how many
/// stores at each helper chokepoint of NURSERY_DESIGN.md §1 would have been an
/// old→young edge — i.e. would have entered a remembered set had the nursery
/// existed. The tax NUMERATOR of the design study's kill rule; zero behavior.
pub(crate) mod gcoracle {
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(crate) const SET_PROP: usize = 0;
    pub(crate) const SET_INDEX: usize = 1;
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) const JIT_SET_PROP: usize = 2;
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    pub(crate) const JIT_SET_INDEX: usize = 3;
    pub(crate) const CELL_SET: usize = 4;
    pub(crate) const PROMISE_SETTLE: usize = 5;
    pub(crate) const PROMISE_REACT: usize = 6;
    pub(crate) const DEFINE_PROP: usize = 7;
    pub(crate) const COLL_INSERT: usize = 8;
    pub(crate) const CLOSURE_HOME: usize = 9;
    pub(crate) const CLOSURE_NEW_TARGET: usize = 10;

    const NAMES: [&str; 11] = [
        "set_prop",
        "set_index",
        "jit_set_prop",
        "jit_set_index",
        "cell_set",
        "promise_settle",
        "promise_react",
        "define_prop",
        "coll_insert",
        "closure_home",
        "closure_new_target",
    ];
    static COUNTS: [AtomicU64; 11] = [const { AtomicU64::new(0) }; 11];

    #[inline]
    pub(crate) fn hit(site: usize) {
        COUNTS[site].fetch_add(1, Ordering::Relaxed);
    }

    /// `(site name, would-be old→young store count)` per chokepoint.
    pub fn dump() -> Vec<(&'static str, u64)> {
        NAMES
            .iter()
            .zip(&COUNTS)
            .map(|(&n, c)| (n, c.load(Ordering::Relaxed)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn static_key_plan_keeps_hot_layout_sizes_exact() {
        assert_eq!(std::mem::size_of::<StaticKeyPlan>(), 8);
        assert_eq!(std::mem::size_of::<PropKeys>(), 24);
        assert_eq!(std::mem::size_of::<ObjMap>(), 112);
        assert_eq!(std::mem::size_of::<HeapObj>(), 80);
    }

    #[test]
    fn planned_keys_advance_clone_and_materialize_independently() {
        let plan = StaticKeyPlan::new(vec!["a".into(), "1".into(), "tail".into()]);
        let mut original = ObjMap::with_static_key_plan_mode(plan, true);
        assert!(original.keys.is_planned());
        assert!(original.keys.is_empty());

        original.push_static_data("a", Value::int(1));
        let mut clone = original.clone();
        clone.push_static_data("1", Value::int(2));
        assert_eq!(original.keys.as_ref(), &["a".to_string()]);
        assert_eq!(clone.keys.as_ref(), &["a".to_string(), "1".to_string()]);
        assert_eq!(clone.element_pos(1), Some(1));
        assert!(clone.has_element_key());
        clone.verify_shape().expect("planned prefix shape");

        // A mismatch must not publish the unbuilt `tail`; it materializes only
        // the visible prefix and performs the ordinary append.
        clone.push_static_data("other", Value::int(3));
        assert!(!clone.keys.is_planned());
        assert_eq!(
            clone.keys.as_ref(),
            &["a".to_string(), "1".to_string(), "other".to_string()]
        );
        assert_eq!(clone.pos("tail"), None);
        clone.verify_shape().expect("materialized mismatch shape");

        // Structural deletion also materializes, preserves order/indexes, and
        // leaves the independently-cloned original prefix untouched.
        assert!(clone.remove("1"));
        assert_eq!(clone.keys.as_ref(), &["a".to_string(), "other".to_string()]);
        assert_eq!(original.keys.as_ref(), &["a".to_string()]);
        assert!(original.keys.is_planned());
    }

    #[test]
    fn static_key_plan_off_seam_is_the_owned_capacity_path() {
        let plan = StaticKeyPlan::new(vec!["a".into(), "b".into()]);
        let mut map = ObjMap::with_static_key_plan_mode(plan, false);
        assert!(!map.keys.is_planned());
        assert!(!map.planned_next_static_key("a"));
        map.push_static_data("a", Value::int(1));
        map.push_static_data("b", Value::int(2));
        assert_eq!(map.keys.as_ref(), &["a".to_string(), "b".to_string()]);
        map.verify_shape().expect("owned comparator shape");
    }

    #[test]
    fn planned_next_probe_is_pure_and_fails_closed_on_mutation_or_drift() {
        let plan = StaticKeyPlan::new(vec!["a".into(), "b".into()]);
        let mut map = ObjMap::with_static_key_plan_mode(plan.clone(), true);
        assert!(map.planned_next_static_key("a"));
        assert!(!map.planned_next_static_key("b"));
        assert!(
            map.keys.is_empty(),
            "the absence proof must not advance the plan"
        );

        map.push_static_data("a", Value::int(1));
        assert!(map.planned_next_static_key("b"));
        assert!(!map.planned_next_static_key("a"));

        // Even a non-structural overwrite routes later appends through the
        // defensive path once a planned object has been mutated.
        assert!(!map.set("a", Value::int(9)));
        assert!(!map.planned_next_static_key("b"));

        let mut vals_drift = ObjMap::with_static_key_plan_mode(plan.clone(), true);
        vals_drift.vals.push(Value::UNDEFINED);
        assert!(!vals_drift.planned_next_static_key("a"));

        let mut attrs_drift = ObjMap::with_static_key_plan_mode(plan, true);
        attrs_drift.push_attr(PropAttr::data());
        assert!(!attrs_drift.planned_next_static_key("a"));

        let invalid = StaticKeyPlan::new(vec!["a".into(), "a".into()]);
        let invalid = ObjMap::with_static_key_plan_mode(invalid, true);
        assert!(!invalid.planned_next_static_key("a"));
    }

    #[test]
    fn malformed_planned_appends_never_create_duplicate_slots() {
        let plan = StaticKeyPlan::new(vec!["a".into(), "b".into()]);
        let mut duplicate = ObjMap::with_static_key_plan_mode(plan.clone(), true);
        assert!(duplicate.planned_next_static_key("a"));
        duplicate.push_static_data("a", Value::int(1));
        assert!(!duplicate.planned_next_static_key("a"));
        assert!(duplicate.planned_next_static_key("b"));
        duplicate.push_static_data("a", Value::int(9));
        assert!(!duplicate.planned_next_static_key("b"));
        duplicate.push_static_data("b", Value::int(2));
        assert_eq!(duplicate.keys.as_ref(), &["a".to_string(), "b".to_string()]);
        assert_eq!(duplicate.vals, &[Value::int(9), Value::int(2)]);
        duplicate.verify_shape().expect("duplicate-safe shape");

        let mut reordered = ObjMap::with_static_key_plan_mode(plan, true);
        assert!(!reordered.planned_next_static_key("b"));
        reordered.push_static_data("b", Value::int(2));
        assert!(!reordered.planned_next_static_key("a"));
        reordered.push_static_data("a", Value::int(1));
        reordered.push_static_data("b", Value::int(7));
        assert_eq!(reordered.keys.as_ref(), &["b".to_string(), "a".to_string()]);
        assert_eq!(reordered.vals, &[Value::int(7), Value::int(1)]);
        reordered.verify_shape().expect("reorder-safe shape");
    }

    #[test]
    fn duplicate_and_oversize_plan_metadata_is_cached_invalid() {
        assert!(!StaticKeyPlan::new(vec!["a".into(), "a".into()]).runtime_valid());
        assert!(!StaticKeyPlan::new(vec!["x".into(); 257]).runtime_valid());
        assert!(StaticKeyPlan::new(vec!["a".into(), "b".into()]).runtime_valid());
    }

    // ── W19 M1: PropIndex layout differential ──
    //
    // The split (`tags`/`slots`) and interleaved (`Vec<(tag, slot)>`) layouts
    // must be observationally identical. A desync between the two parallel
    // arrays is a HIT ON THE WRONG SLOT — a silent wrong answer, not a crash —
    // so the assertion that matters is that both layouts agree on `find` for
    // every key after every structural change, not merely that neither panics.
    //
    // `with_capacity_kind` is the seam: the shipped constructor reads a
    // process-wide latch, so only this seam can drive both representations in
    // one process.

    /// xorshift64* — deterministic, no dev-dependency.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// Both layouts, driven through the same random add/remove sequence, must
    /// answer every lookup identically and stay structurally sound throughout.
    #[test]
    fn propindex_split_matches_interleaved() {
        for seed in 1u64..40 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut keys: Vec<String> = Vec::new();
            let mut inter = PropIndex::with_capacity_kind(32, false);
            let mut split = PropIndex::with_capacity_kind(32, true);
            let mut next_id = 0u32;

            for step in 0..600 {
                // Bias toward growth early so the table crosses several grow()
                // boundaries, then toward deletion so the backward-shift
                // deletion and the renumber sweep both run hot.
                let add = keys.is_empty() || (step < 200 && rng.below(4) != 0) || rng.below(2) == 0;
                if add {
                    // Key spellings that collide in the low tag bits, share
                    // prefixes, and vary in length.
                    let k = match rng.below(4) {
                        0 => format!("prop_{next_id}"),
                        1 => format!("k{}", next_id % 97),
                        2 => format!("{}", next_id),
                        _ => format!("aVeryLongPropertyName_{next_id}"),
                    };
                    next_id += 1;
                    if keys.iter().any(|e| *e == k) {
                        continue;
                    }
                    keys.push(k);
                    let slot = (keys.len() - 1) as u32;
                    inter.insert(&keys[slot as usize], slot);
                    split.insert(&keys[slot as usize], slot);
                } else {
                    let i = rng.below(keys.len());
                    let k = keys.remove(i);
                    let tag = prop_tag(&k);
                    inter.remove_slot(tag, i as u32);
                    split.remove_slot(tag, i as u32);
                }

                assert_eq!(
                    inter.len(),
                    keys.len(),
                    "seed {seed} step {step}: inter len"
                );
                assert_eq!(
                    split.len(),
                    keys.len(),
                    "seed {seed} step {step}: split len"
                );
                assert_eq!(
                    inter.cap(),
                    split.cap(),
                    "seed {seed} step {step}: capacity diverged"
                );
                inter.verify(&keys);
                split.verify(&keys);
                for (want, k) in keys.iter().enumerate() {
                    assert_eq!(inter.find(&keys, k), Some(want));
                    assert_eq!(split.find(&keys, k), Some(want));
                }
                for miss in ["", "nope", "prop_999999", "0", "__proto__"] {
                    assert_eq!(
                        inter.find(&keys, miss),
                        split.find(&keys, miss),
                        "seed {seed} step {step}: layouts disagree on absent key {miss:?}"
                    );
                }
            }
        }
    }

    #[cfg(feature = "safe-sandbox")]
    #[test]
    fn safe_propindex_tree_ignores_precomputed_legacy_bucket_collisions() {
        // This 10k-key family shares the low eight bits of the former public
        // FNV/splitmix tag. Targeting all fourteen bits of its final 16k-bucket
        // table is the same offline operation at a higher trial count and made
        // construction quadratic. Exact-key B-tree ordering has no bucket for
        // either family to target.
        const COUNT: usize = 10_000;
        const MASK: u32 = 255;
        let mut keys = Vec::with_capacity(COUNT);
        let mut candidate = 0u64;
        while keys.len() < COUNT {
            let key = format!("attacker_{candidate:x}");
            if prop_tag(&key) & MASK == 0 {
                keys.push(key);
            }
            candidate += 1;
        }

        let index = PropIndex::build(&keys);
        assert!(matches!(&*index, PropIndex::Tree { .. }));
        for (slot, key) in keys.iter().enumerate() {
            assert_eq!(index.find(&keys, key), Some(slot));
        }

        // ObjMap::clone clones the index verbatim and keeps all exact mappings.
        let cloned = index.clone();
        for (slot, key) in keys.iter().enumerate() {
            assert_eq!(cloned.find(&keys, key), Some(slot));
        }
    }

    /// `remove_at` is `remove` with the slot pre-resolved (W19 M2 uses it to
    /// skip a second `pos()`). The two must leave byte-identical maps.
    #[test]
    fn objmap_remove_at_matches_remove() {
        for n in [3usize, 11, 12, 30, 60, 200] {
            for victim in [0usize, 1, n / 2, n - 1] {
                let mut a = ObjMap::new();
                let mut b = ObjMap::new();
                for i in 0..n {
                    a.set(&format!("prop_{i}"), Value::int(i as i32));
                    b.set(&format!("prop_{i}"), Value::int(i as i32));
                }
                let key = format!("prop_{victim}");
                let i = b.pos(&key).unwrap();
                assert!(a.remove(&key));
                b.remove_at(i);
                assert_eq!(a.keys, b.keys, "n={n} victim={victim}: keys diverged");
                assert_eq!(a.vals, b.vals, "n={n} victim={victim}: vals diverged");
                assert_map_consistent(&a);
                assert_map_consistent(&b);
                for j in 0..n {
                    let k = format!("prop_{j}");
                    assert_eq!(a.pos(&k), b.pos(&k), "n={n} victim={victim} key {k}");
                }
            }
        }
    }

    #[test]
    fn numeric_side_index_tracks_canonical_keys_and_shifted_slots() {
        assert_eq!(canonical_array_index_key("0"), Some(0));
        assert_eq!(canonical_array_index_key("4294967294"), Some(u32::MAX - 1));
        for named in ["", "00", "01", "-1", "1.0", "4294967295", "99999999999"] {
            assert_eq!(canonical_array_index_key(named), None, "{named:?}");
        }

        let mut m = ObjMap::new_side_table();
        m.set("named", Value::int(1));
        m.set("07", Value::int(2)); // named property, not index 7
        m.set("7", Value::int(3));
        m.set("123456", Value::int(4));
        assert_eq!(m.element_pos(7), m.pos("7"));
        assert_eq!(m.element_pos(123456), m.pos("123456"));
        assert_eq!(m.element_pos(8), None);

        // Removing a slot before both numeric entries shifts their authoritative
        // vector positions. Overwriting/descriptor changes leave positions fixed.
        assert!(m.remove("named"));
        assert_eq!(m.element_pos(7), m.pos("7"));
        assert_eq!(m.element_pos(123456), m.pos("123456"));
        m.set("7", Value::int(30));
        m.define("123456", Value::int(40), PropAttr::data());
        assert_eq!(m.vals[m.element_pos(7).unwrap()], Value::int(30));
        assert_eq!(m.vals[m.element_pos(123456).unwrap()], Value::int(40));
        assert!(m.remove("7"));
        assert_eq!(m.element_pos(7), None);
        assert_eq!(m.element_pos(123456), m.pos("123456"));
    }

    /// The renumber sweep after a delete must leave EVERY surviving key
    /// addressable at its new slot, at each capacity the row's objects pass
    /// through (the index is built at 12 keys and grows at 3/4 load: 32 -> 64 at
    /// 24 -> 128 at 48). Deleting front-first maximises the shift distance.
    #[test]
    fn objmap_delete_rebuild_cycle_keeps_every_key_addressable() {
        for n in [12usize, 24, 48, 60, 130, 400] {
            let mut m = ObjMap::new();
            for i in 0..n {
                m.set(&format!("prop_{i}"), Value::int(i as i32));
            }
            // Delete every other key front-first, then re-add them.
            for i in (0..n).step_by(2) {
                assert!(
                    m.remove(&format!("prop_{i}")),
                    "n={n}: prop_{i} not removed"
                );
                assert_map_consistent(&m);
            }
            for i in (0..n).step_by(2) {
                m.set(&format!("prop_{i}"), Value::int((i * 2) as i32));
            }
            assert_map_consistent(&m);
            assert_eq!(m.keys.len(), n, "n={n}: key count after rebuild");
            for i in 0..n {
                let want = if i % 2 == 0 { (i * 2) as i32 } else { i as i32 };
                assert_eq!(
                    m.get(&format!("prop_{i}")),
                    Some(Value::int(want)),
                    "n={n}: prop_{i} wrong after delete/rebuild"
                );
            }
        }
    }

    /// Every key the map claims to hold must be found by `pos()` at the slot
    /// that actually stores it, and absent keys must miss — checked through
    /// whatever lookup mode (index or linear) the map is currently in.
    fn assert_map_consistent(m: &ObjMap) {
        for (i, k) in m.keys.iter().enumerate() {
            assert_eq!(m.pos(k), Some(i), "key {k:?} not found at its slot");
        }
        assert_eq!(m.pos("missing-key-never-inserted"), None);
        assert_shape_agrees(m);
        assert_element_key_bit_agrees(m);
    }

    /// `has_element_key` is a cached summary of `keys`, and every dense-array
    /// fast path trusts it to be exact in the FALSE direction — a stale `false`
    /// licenses an in-place `items[i]` read/write past a defineProperty'd index
    /// override. Recompute it from scratch and compare.
    fn assert_element_key_bit_agrees(m: &ObjMap) {
        assert_eq!(
            m.has_element_key,
            m.keys.iter().any(|k| key_names_element(k)),
            "has_element_key disagrees with the key vector {:?}",
            m.keys
        );
    }

    /// THE invariant the whole hidden-class landing rests on: when a map claims
    /// a shape, that shape's key -> slot mapping must be exactly the map's own.
    ///
    /// This is what makes a shape-keyed cache guard sound. A guard matches on the
    /// shape id alone and then reads the slot it recorded at fill time, so a
    /// shape that disagreed with its object by even one position would read the
    /// wrong property and return a plausible wrong answer — the failure mode this
    /// engine has been bitten by twice.
    fn assert_shape_agrees(m: &ObjMap) {
        if !m.shape_guardable() {
            return; // dictionary mode promises nothing, and is never guarded on
        }
        assert_eq!(
            crate::shape::len(m.shape()) as usize,
            m.keys.len(),
            "shape length disagrees with the key vector"
        );
        for (i, k) in m.keys.iter().enumerate() {
            assert_eq!(
                crate::shape::slot_of(m.shape(), k),
                Some(i as u32),
                "shape puts {k:?} at a different slot than the map does"
            );
        }
    }

    /// `verify_shape` must catch the thing `assert_shape_agrees` cannot see.
    ///
    /// That helper checks key -> slot agreement, which is what a shape-keyed
    /// guard needs to read the RIGHT slot. It says nothing about descriptor
    /// bits — and those are part of a shape's identity, deliberately, because
    /// two objects whose `x` differs in enumerability do not have interchangeable
    /// layouts for a descriptor read. A raw `attrs[i] = a` therefore leaves an
    /// object claiming a shape that lies about it while every key still lands in
    /// the right place.
    ///
    /// That write existed: `eval_prog.rs` did it on `globalThis` while hoisting a
    /// redeclared `var`. It was harmless only because `ic_obj_ok` bans
    /// `global_this` from every cache — an accident of exclusion, not an
    /// invariant, and not one a native shape guard could rely on.
    #[test]
    fn a_raw_attribute_write_is_caught_by_verify_shape() {
        let mut m = ObjMap::new();
        m.set("a", Value::num(1.0));
        m.set("b", Value::num(2.0));
        assert!(m.shape_guardable());
        assert!(m.verify_shape().is_ok());

        // Every key is still at its own slot, so the key/slot check passes...
        m.attr_mut(1).enumerable = false;
        assert_shape_agrees(&m);
        // ...and the shape is now lying about slot 1's descriptor.
        let why = m
            .verify_shape()
            .expect_err("a changed attr bit must be caught");
        assert!(why.contains("attr bits"), "unexpected reason: {why}");
        assert!(why.contains("slot 1"), "reason should name the slot: {why}");
    }

    /// The same change through `set_attr_at` is sound: it drops the object to
    /// DICT, which is the honest answer — "this layout is no longer describable
    /// by a sequence of appends" — and `shape_guardable()` then refuses it.
    #[test]
    fn set_attr_at_drops_to_dict_instead_of_lying() {
        let mut m = ObjMap::new();
        m.set("a", Value::num(1.0));
        m.set("b", Value::num(2.0));
        let before = m.shape();

        let mut a = m.attr_at(1);
        a.enumerable = false;
        m.set_attr_at(1, a);

        assert_ne!(m.shape(), before);
        assert!(!m.shape_guardable(), "a descriptor change must leave DICT");
        assert!(
            m.verify_shape().is_ok(),
            "DICT claims nothing, so it cannot lie"
        );
    }

    /// A descriptor change that changes NOTHING must not cost the shape: writing
    /// back identical bits keeps the object guardable.
    #[test]
    fn rewriting_identical_attributes_keeps_the_shape() {
        let mut m = ObjMap::new();
        m.set("a", Value::num(1.0));
        let before = m.shape();
        let a = m.attr_at(0);
        m.set_attr_at(0, a);
        assert_eq!(m.shape(), before);
        assert!(m.shape_guardable());
        assert!(m.verify_shape().is_ok());
    }

    /// A value store is shape-NEUTRAL by definition — same keys, same descriptor
    /// bits, no reallocation. This is why the other 19 raw in-slot writes in the
    /// tree are not a hazard and were left alone.
    #[test]
    fn a_value_store_does_not_touch_the_shape() {
        let mut m = ObjMap::new();
        m.set("a", Value::num(1.0));
        m.set("b", Value::num(2.0));
        let before = m.shape();
        m.set_val_at(0, Value::num(99.0));
        assert_eq!(m.shape(), before);
        assert!(m.verify_shape().is_ok());
        assert_eq!(m.val_at(0).as_f64(), 99.0);
    }

    /// `seal` and `freeze` change every property's descriptor at once, so they
    /// must leave DICT rather than a shape that claims the old bits.
    #[test]
    fn seal_and_freeze_leave_a_shape_that_cannot_lie() {
        for op in [0u8, 1] {
            let mut m = ObjMap::new();
            m.set("a", Value::num(1.0));
            m.set("b", Value::num(2.0));
            if op == 0 {
                m.seal();
            } else {
                m.freeze();
            }
            assert!(!m.shape_guardable());
            assert!(m.verify_shape().is_ok());
        }
    }

    #[test]
    fn objects_built_the_same_way_share_a_shape() {
        // The premise: a guard on the shape of one of these matches all of them.
        let mk = || {
            let mut m = ObjMap::new();
            m.set("alpha", Value::num(1.0));
            m.set("beta", Value::num(2.0));
            m.set("gamma", Value::num(3.0));
            m
        };
        let (a, b) = (mk(), mk());
        assert!(a.shape_guardable());
        assert_eq!(a.shape(), b.shape());
        assert_map_consistent(&a);

        // Different ORDER is a different layout, so it must not share.
        let mut c = ObjMap::new();
        c.set("beta", Value::num(2.0));
        c.set("alpha", Value::num(1.0));
        assert_ne!(a.shape(), c.shape());
    }

    #[test]
    fn layout_changing_operations_drop_to_dictionary() {
        // Each of these makes the object's layout undescribable as a sequence of
        // appends, and each must therefore stop being guardable — a stale guard
        // here is a wrong-value read, not a slow one.
        let base = || {
            let mut m = ObjMap::new();
            m.set("a", Value::num(1.0));
            m.set("b", Value::num(2.0));
            m
        };

        let mut deleted = base();
        deleted.remove("a");
        assert!(!deleted.shape_guardable(), "delete shifts later slots");

        let mut resealed = base();
        resealed.seal();
        assert!(!resealed.shape_guardable(), "seal rewrites every attr");

        let mut frozen = base();
        frozen.freeze();
        assert!(!frozen.shape_guardable(), "freeze rewrites every attr");

        let mut redefined = base();
        redefined.define(
            "a",
            Value::num(9.0),
            PropAttr {
                writable: false,
                enumerable: true,
                configurable: true,
                accessor: false,
                setter: Value::UNDEFINED,
            },
        );
        assert!(!redefined.shape_guardable(), "attrs changed mid-sequence");

        // But re-stating the SAME attributes is not a layout change, and a plain
        // value overwrite certainly is not.
        let mut same = base();
        let sh = same.shape();
        same.define("a", Value::num(9.0), PropAttr::data());
        assert_eq!(same.shape(), sh, "identical attrs must not cost the shape");
        same.set("a", Value::num(10.0));
        assert_eq!(same.shape(), sh, "a value overwrite is not a layout change");
        assert_map_consistent(&same);
    }

    #[test]
    fn shape_survives_the_index_threshold_and_a_clone() {
        // Crossing PROP_INDEX_THRESHOLD changes the LOOKUP mode, not the layout.
        let mut m = ObjMap::new();
        for i in 0..(PROP_INDEX_THRESHOLD + 8) {
            m.set(&format!("k{i}"), Value::num(i as f64));
        }
        assert_map_consistent(&m);
        let c = m.clone();
        assert_eq!(
            c.shape(),
            m.shape(),
            "a clone has identical keys at identical slots"
        );
        assert_map_consistent(&c);
    }

    #[test]
    fn prop_index_survives_removals_in_every_order() {
        // 200 keys force a dense, collision-bearing table; three removal
        // orders exercise backward-shift across fresh holes (front-to-back),
        // shrinking tails (back-to-front), and scattered chains (stride).
        let n = 200usize;
        for order in 0..3 {
            let mut m = ObjMap::new();
            for i in 0..n {
                m.set(&format!("key_{i}"), Value::num(i as f64));
            }
            assert_map_consistent(&m);
            let victims: Vec<usize> = match order {
                0 => (0..n).collect(),
                1 => (0..n).rev().collect(),
                _ => (0..n).map(|i| (i * 7) % n).collect(),
            };
            for (removed, v) in victims.iter().enumerate() {
                assert!(m.remove(&format!("key_{v}")));
                assert!(!m.remove(&format!("key_{v}")), "double remove succeeded");
                assert_map_consistent(&m);
                assert_eq!(m.keys.len(), n - removed - 1);
            }
        }
    }

    #[test]
    fn prop_index_remove_then_readd_keeps_lookups_exact() {
        // The dictionary-churn shape: build past the threshold, delete every
        // other key, re-add them, and verify values land where pos() says.
        let mut m = ObjMap::new();
        for i in 0..60 {
            m.set(&format!("prop_{i}"), Value::num(i as f64));
        }
        for i in (0..60).step_by(2) {
            assert!(m.remove(&format!("prop_{i}")));
        }
        assert_map_consistent(&m);
        for i in (0..60).step_by(2) {
            m.set(&format!("prop_{i}"), Value::num((i * 100) as f64));
        }
        assert_map_consistent(&m);
        for i in 0..60 {
            let want = if i % 2 == 0 {
                (i * 100) as f64
            } else {
                i as f64
            };
            let got = m.get(&format!("prop_{i}")).expect("key vanished");
            assert_eq!(got.as_f64(), want, "prop_{i} wrong value");
        }
    }

    #[test]
    fn prop_index_drops_at_half_threshold_and_rebuilds() {
        let mut m = ObjMap::new();
        for i in 0..PROP_INDEX_THRESHOLD {
            m.set(&format!("k{i}"), Value::num(i as f64));
        }
        // Shrink through the hysteresis band down to empty; lookups must stay
        // exact across the index-drop boundary (THRESHOLD/2) and below.
        for i in (0..PROP_INDEX_THRESHOLD).rev() {
            assert!(m.remove(&format!("k{i}")));
            assert_map_consistent(&m);
        }
        assert_eq!(m.keys.len(), 0);
        // And grow straight back through the build boundary.
        for i in 0..PROP_INDEX_THRESHOLD + 4 {
            m.set(&format!("k{i}"), Value::num(i as f64));
            assert_map_consistent(&m);
        }
    }

    /// The stage-1 minor sweep's free invariants, pinned at the unit level:
    /// only UNMARKED YOUNG slots are freed; each freed slot's version is
    /// bumped (a stale inline cache for the dead occupant must miss on
    /// reuse); freed slots land on the free list and `alloc` hands them back
    /// out — re-entering the (cleared, capacity-kept) young log.
    #[test]
    fn a_minor_sweep_frees_unmarked_young_bumps_versions_and_recycles() {
        let mut h = Heap::new();
        h.set_nursery(true); // hold even in a suite run under ZIPP_NO_NURSERY=1
        let a = h.alloc(HeapObj::Str(JsStr::new("aa".into())));
        let b = h.alloc(HeapObj::Str(JsStr::new("bb".into())));
        let c = h.alloc(HeapObj::Str(JsStr::new("cc".into())));
        assert_eq!(h.young_log(), &[a, b, c]);
        let (va, vb, vc) = (h.version_of(a), h.version_of(b), h.version_of(c));

        // Mark everything except `b` (the pinned interned prefix included,
        // exactly as the collector's root pass would).
        let mut marks = vec![true; h.len()];
        marks[b as usize] = false;
        let free_before = h.free_indices().len();
        let swept = h.sweep_young(&marks);
        let freed = &h.free_indices()[free_before..];
        assert_eq!(
            (swept, freed),
            (1, &[b][..]),
            "only the unmarked young slot"
        );
        assert_eq!(h.free_indices(), &[b], "the freed slot is on the free list");
        assert_eq!(
            h.version_of(b),
            vb.wrapping_add(1),
            "free must bump the version"
        );
        assert_eq!(h.version_of(a), va, "a marked slot's version is untouched");
        assert_eq!(h.version_of(c), vc, "a marked slot's version is untouched");
        assert!(h.young_log().is_empty(), "survivors age out of the log");

        // Reuse: the recycled slot comes back from the free list, bumps the
        // version AGAIN, and is young in the new epoch.
        let d = h.alloc(HeapObj::Str(JsStr::new("dd".into())));
        assert_eq!(d, b, "the minor's freed slot is reused first");
        assert_eq!(h.version_of(d), vb.wrapping_add(2));
        assert_eq!(h.young_log(), &[d]);
        assert!(h.free_indices().is_empty());
    }

    /// The collector derives a minor's prune set from the free-list suffix
    /// appended by `sweep_young`. Pin both halves of that contract: an older
    /// free prefix is stable, while mixed live/dead young slots append exactly
    /// the dead slots in young-log order. Until the caller restores them, the
    /// same slots are exactly the false entries in the minor mark vector.
    #[test]
    fn a_minor_sweep_appends_an_exact_ordered_free_suffix() {
        let mut h = Heap::new();
        h.set_nursery(true);

        // Age one slot out of the young log, then make it an existing free-list
        // entry after constructing the next epoch. No allocation occurs after
        // this point, matching `gc_minor`'s capture-to-prune window.
        let prefix = h.alloc(HeapObj::Str(JsStr::new("prefix".into())));
        let promote = vec![true; h.len()];
        h.sweep_young(&promote);
        let live_a = h.alloc(HeapObj::Str(JsStr::new("live-a".into())));
        let dead_a = h.alloc(HeapObj::Str(JsStr::new("dead-a".into())));
        let live_b = h.alloc(HeapObj::Str(JsStr::new("live-b".into())));
        let dead_b = h.alloc(HeapObj::Str(JsStr::new("dead-b".into())));
        assert_eq!(h.young_log(), &[live_a, dead_a, live_b, dead_b]);
        h.free_slot(prefix);
        assert_eq!(h.free_indices(), &[prefix]);

        let mut marks = vec![true; h.len()];
        marks[dead_a as usize] = false;
        marks[dead_b as usize] = false;
        let free_before = h.free_indices().len();
        let swept = h.sweep_young(&marks);

        assert_eq!(swept, 2);
        assert_eq!(&h.free_indices()[..free_before], &[prefix]);
        assert_eq!(
            &h.free_indices()[free_before..],
            &[dead_a, dead_b],
            "the new suffix is exactly the dead young set, in sweep order"
        );
        let false_slots: Vec<u32> = marks
            .iter()
            .enumerate()
            .filter_map(|(i, &marked)| (!marked).then_some(i as u32))
            .collect();
        assert_eq!(false_slots, h.free_indices()[free_before..]);

        // This is the cache-restoration step `gc_minor` performs only after
        // every side table has consumed the false bits.
        for &i in &h.free_indices()[free_before..] {
            marks[i as usize] = true;
        }
        assert!(marks.iter().all(|&marked| marked));
    }

    /// The stage-3 barrier state machine, pinned at the unit level: only an
    /// OLD-CLEAN holder with a YOUNG heap value enters the remembered set;
    /// the DIRTY state dedups repeat stores; a minor's `remset_reset` returns
    /// holders to clean so the next epoch's first store re-remembers them.
    #[test]
    fn a_write_barrier_remembers_old_holders_of_young_values_exactly_once() {
        // W10 (B123): the value-tested form is VALUE-GRAIN — it records the
        // young VALUE (vremset, GEN_VLOG-deduped) and leaves the holder
        // clean; the value-BLIND card form still dirties the holder.
        let mut h = Heap::new();
        h.set_nursery(true); // hold even in a suite run under ZIPP_NO_NURSERY=1
        let holder = h.alloc(HeapObj::Cell(Value::UNDEFINED));
        // Promote: survive a "collection" (everything marked).
        let marks = vec![true; h.len()];
        let free_before = h.free_indices().len();
        h.sweep_young(&marks);
        assert!(h.free_indices()[free_before..].is_empty());

        let young = h.alloc(HeapObj::Str(JsStr::new("yy".into())));
        // young holder / young value: no record either way.
        h.write_barrier_val(young, Value::heap(young));
        assert_eq!(h.value_remset(), &[] as &[u32]);
        assert_eq!(h.dirty_for_trace(), Vec::<u32>::new());
        // old holder / non-heap value: nothing.
        h.write_barrier_val(holder, Value::int(7));
        assert_eq!(h.value_remset(), &[] as &[u32]);
        // old holder / OLD value (the interned prefix is old): nothing.
        h.write_barrier_val(holder, Value::heap(INTERN_EMPTY));
        assert_eq!(h.value_remset(), &[] as &[u32]);
        // old holder / young value: the VALUE is recorded — exactly once
        // across repeats (GEN_VLOG dedup), and the holder stays CLEAN.
        h.write_barrier_val(holder, Value::heap(young));
        h.write_barrier_val(holder, Value::heap(young));
        assert_eq!(h.value_remset(), &[young]);
        assert_eq!(h.dirty_for_trace(), Vec::<u32>::new());
        // The card form still dirties the holder (holder-grain).
        h.write_barrier(holder);
        assert_eq!(h.dirty_for_trace(), vec![holder]);
        // A DIRTY holder's further value stores are skipped (its full
        // re-trace covers them) — no new value records for a fresh young.
        let young1b = h.alloc(HeapObj::Str(JsStr::new("y2".into())));
        h.write_barrier_val(holder, Value::heap(young1b));
        assert_eq!(h.value_remset(), &[young]);

        // End of minor: both sets reset; the next epoch records afresh.
        let marks = vec![true; h.len()];
        h.sweep_young(&marks);
        h.remset_reset();
        h.vremset_reset();
        assert_eq!(h.dirty_for_trace(), Vec::<u32>::new());
        assert_eq!(h.value_remset(), &[] as &[u32]);
        let young2 = h.alloc(HeapObj::Str(JsStr::new("zz".into())));
        h.write_barrier_val(holder, Value::heap(young2));
        assert_eq!(h.value_remset(), &[young2]);

        // Stale-VLOG-after-major regression: run the major-side reset (gen
        // wholesale rewrite must strip GEN_VLOG), then a NEW young occupant
        // of the same slot must be recordable again.
        h.note_gc_done(h.len());
        assert_eq!(h.value_remset(), &[] as &[u32]);
        let young3 = h.alloc(HeapObj::Str(JsStr::new("ww".into())));
        h.write_barrier_val(holder, Value::heap(young3));
        assert_eq!(h.value_remset(), &[young3]);
    }

    /// Persistent scan roots (call-free JIT store targets): registered once,
    /// surviving `remset_reset`, deduped against the remembered set only by
    /// harmless double-tracing, and self-expiring when the slot is recycled.
    #[test]
    fn a_scan_root_persists_across_minors_and_expires_with_its_slot() {
        let mut h = Heap::new();
        h.set_nursery(true);
        let holder = h.alloc(HeapObj::Cell(Value::UNDEFINED));
        h.register_scan_root(holder);
        h.register_scan_root(holder); // sticky bit dedups
        assert_eq!(h.dirty_for_trace(), vec![holder]);
        h.remset_reset();
        assert_eq!(h.dirty_for_trace(), vec![holder], "scan roots persist");

        // Free the slot (as a sweep would) and let alloc recycle it: the
        // recycled occupant must NOT inherit the registration.
        h.free_slot(holder);
        let reused = h.alloc(HeapObj::Str(JsStr::new("rr".into())));
        assert_eq!(reused, holder, "the freed slot is reused first");
        assert_eq!(
            h.dirty_for_trace(),
            Vec::<u32>::new(),
            "registration expired"
        );
    }

    /// The stage-3 schedule: a minor that leaves the heap at/above the
    /// pre-nursery collection point (`major_at`) latches the next collection
    /// into a major; one that reclaims below it keeps minoring.
    #[test]
    fn a_minor_that_cannot_shrink_the_heap_latches_a_major() {
        let mut h = Heap::new();
        h.set_nursery(true);
        assert!(h.minor_due(false), "fresh heap: minors first");
        // Post-minor occupancy below major_at: keep minoring.
        h.note_minor_done(1000);
        assert!(h.minor_due(false));
        // Post-minor occupancy at/above major_at (the historical collectable
        // boundary plus the permanent pad2 prefix — no major has run yet):
        // the young sweep failed to shrink the heap, so the next collection
        // must be a major.
        h.note_minor_done(GC_MIN_THRESHOLD + INTERN_PAD2_COUNT as usize);
        assert!(!h.minor_due(false));
        // The major resets the anchor from its TRUE live count.
        h.note_gc_done(2000);
        assert!(h.minor_due(false));
    }

    #[test]
    fn nursery_max_minors_override_accepts_only_bounded_positive_decimals() {
        assert_eq!(parse_nursery_max_minors("1"), Some(1));
        assert_eq!(parse_nursery_max_minors("64"), Some(64));
        assert_eq!(
            parse_nursery_max_minors(&NURSERY_MAX_MINORS_LIMIT.to_string()),
            Some(NURSERY_MAX_MINORS_LIMIT)
        );
        for invalid in [
            "",
            "0",
            "-1",
            "1.5",
            " 64 ",
            "not-a-number",
            "4097",
            "4294967295",
            "4294967296",
        ] {
            assert_eq!(
                parse_nursery_max_minors(invalid),
                None,
                "unexpectedly accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn nursery_backstop_uses_its_latched_cap_and_stress_stays_denser() {
        let mut h = Heap::new();
        h.set_nursery(true);
        h.nursery_max_minors = 2;
        assert!(h.minor_due(false));
        h.note_minor_done(0);
        assert!(h.minor_due(false));
        h.note_minor_done(0);
        assert!(
            !h.minor_due(false),
            "the configured backstop must force a major"
        );

        h.note_gc_done(0);
        h.nursery_max_minors = NURSERY_MAX_MINORS_LIMIT;
        for _ in 0..NURSERY_STRESS_MINORS {
            assert!(h.minor_due(true));
            h.note_minor_done(0);
        }
        assert!(
            !h.minor_due(true),
            "GC stress must retain its fixed three-minor cadence"
        );
    }

    #[test]
    fn payload_peak_tracks_resident_reuse_not_cumulative_churn() {
        let mut h = Heap::new();
        // A first audit models the real consumer (ceilings/`heap_bytes`)
        // switching eager accounting on; before it, charges are deferred.
        h.audit_resident_bytes();
        let baseline = h.resident_payload_current.get();
        let first = h.alloc(HeapObj::Str(JsStr::new("x".repeat(1024))));
        let charge = h.resident_payload_charged[first as usize].get();
        assert!(charge > 0, "eager charging must be on after an audit");
        assert_eq!(h.resident_payload_current.get(), baseline + charge);
        let first_peak = h.resident_payload_high_water.get();

        h.free_slot(first);
        assert_eq!(h.resident_payload_current.get(), baseline);
        let reused = h.alloc(HeapObj::Str(JsStr::new("y".repeat(1024))));
        assert_eq!(reused, first);
        assert_eq!(h.resident_payload_current.get(), baseline + charge);
        assert_eq!(
            h.resident_payload_high_water.get(),
            first_peak,
            "reusing an equal-size slot must not double-charge lifetime churn"
        );

        h.audit_resident_bytes();
        assert_eq!(h.resident_payload_current.get(), baseline + charge);
    }

    #[test]
    fn shape_mirror_settles_at_alloc_invalidates_at_bump_repairs_on_demand() {
        let mut h = Heap::new();
        let mut m = ObjMap::new();
        m.push_data("a".to_string(), Value::num(1.0));
        m.push_data("b".to_string(), Value::num(2.0));
        let sh = m.shape();
        assert!(m.shape_guardable());
        let idx = h.alloc(HeapObj::Object(Box::new(m)));
        // Alloc is a settling event: mirror == live shape, vals base captured.
        assert_eq!(h.shape_mirror[idx as usize], sh);
        assert_ne!(h.vals_ptr_mirror[idx as usize], 0);

        // A version bump (every reachable-object shape change) INVALIDATES —
        // order-independence is the point, so no refresh here.
        h.bump_version(idx);
        assert_eq!(h.shape_mirror[idx as usize], crate::shape::DICT);
        assert_eq!(h.vals_ptr_mirror[idx as usize], 0);

        // The miss helper's repair re-settles from the live map.
        h.refresh_mirror(idx);
        assert_eq!(h.shape_mirror[idx as usize], sh);
        assert_ne!(h.vals_ptr_mirror[idx as usize], 0);

        // Reclaim clears; a recycled slot re-settles from the NEW occupant.
        h.free_slot(idx);
        assert_eq!(h.shape_mirror[idx as usize], crate::shape::DICT);
        assert_eq!(h.vals_ptr_mirror[idx as usize], 0);
        let again = h.alloc(HeapObj::Str(JsStr::new("s".into())));
        assert_eq!(again, idx, "free list must hand the slot back");
        assert_eq!(
            h.shape_mirror[idx as usize],
            crate::shape::DICT,
            "a non-Object occupant must never be matchable by a shape way"
        );
    }

    #[test]
    fn all_default_attrs_store_nothing() {
        let mut m = ObjMap::new();
        for k in ["a", "b", "c", "d"] {
            m.push_data(k.to_string(), Value::num(1.0));
        }
        assert!(
            !m.may_deviate_attrs(),
            "default-data pushes must keep the elided representation"
        );
        assert_eq!(m.attrs_len(), 4);
        assert!(m.attr_at(3).writable && !m.attr_at(3).accessor);
        // A deviating write materializes; the stored column then reports real bytes.
        let elided = m.resident_bytes();
        m.attr_mut(2).enumerable = false;
        assert!(m.may_deviate_attrs());
        assert!(m.resident_bytes() > elided, "materialized column must be charged");
        assert!(!m.attr_at(2).enumerable && m.attr_at(3).enumerable);
    }

    #[test]
    fn payload_accounting_defers_until_first_audit_then_backfills_exactly() {
        let mut h = Heap::new();
        assert!(!h.payload_accounting.get(), "trusted runs skip the charge");
        let idx = h.alloc(HeapObj::Str(JsStr::new("x".repeat(4096))));
        assert_eq!(
            h.resident_payload_charged[idx as usize].get(),
            0,
            "no consumer yet — the sizing walk must not run"
        );
        // First audit (a ceiling or `heap_bytes` read) backfills every live
        // slot, so the lazily-started total matches eager-from-birth exactly.
        h.audit_resident_bytes();
        assert!(h.payload_accounting.get());
        let charged = h.resident_payload_charged[idx as usize].get();
        assert!(charged >= 4096);
        let current = h.resident_payload_current.get();
        let again = h.alloc(HeapObj::Str(JsStr::new("y".repeat(2048))));
        assert_eq!(
            h.resident_payload_current.get(),
            current + h.resident_payload_charged[again as usize].get(),
            "post-audit allocations are charged eagerly again"
        );
    }
}
