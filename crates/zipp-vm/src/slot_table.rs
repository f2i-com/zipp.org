//! [`SlotTable`] — a side table keyed by HEAP SLOT INDEX.
//!
//! Every `Vm` side table that hangs extra state off a heap object is keyed by the
//! object's `u32` slot, and each was an `FxHashMap<u32, _>`. That is the wrong
//! shape for this key. Slot indices are small, dense, and handed out in ASCENDING
//! order, so a paged direct index answers the same question with two dependent
//! loads and no hashing — and, more to the point, the GC's per-collection prune
//! becomes a walk over the LIVE entries instead of over the table's whole
//! capacity, which after a burst never shrinks.
//!
//! **Why this exists, measured.** `arr_props` is where a successful `exec` /
//! `matchAll` result parks its `index`/`input`/`groups`. On `regex-log-scan`,
//! ablating those four properties entirely is **−14.9%** of the row
//! ([−15.4, −13.7], 1882→1600ms) — the largest single measured item in
//! `docs/PERF_ROADMAP.md`. Ablating only the *table insert*, while still building the
//! `ObjMap` and dropping it, is **−12.2%** ([−12.5, −12.0]). So **79% of that
//! prize is the table, not the properties**: an `FxHashMap` that grows to
//! hundreds of thousands of entries inside one phase, is probed with a
//! effectively-random key, is walked whole for roots on every collection, and is
//! `retain`ed whole afterwards.
//!
//! **What it measured, and why the whole suite moved.** Swapping `arr_props`
//! alone: `regex-log-scan` **−10.6%** [−10.9, −10.3] and
//! `async-promise-chain` **−12.4%** [−12.9, −11.4], suite geomean **−3.63%**
//! [−3.94, −3.37], nothing regressed. The second row is the interesting one and
//! it is not about regexes: `ordinary_set_ok` probes this table on EVERY
//! ordinary property write, and `proto_chain_blocks_set` probes it again at
//! every prototype hop (up to eight). Both are guarded by `is_empty()` — so one
//! array named property, anywhere in the program, switched a hash probe per
//! write-plus-hop on for the rest of the process. Direct indexing makes that
//! guard almost irrelevant.
//!
//! **Now backs five tables**, the other four picked for being probed per
//! OPERATION rather than per object, with no `is_empty()` guard: `proto_of`
//! (every prototype-chain walk, and again at each of up to eight hops in
//! `proto_chain_blocks_set`), `arguments_objs` (`args_mapped_get`, on every
//! array element read), `array_js_len` (every array `.length`), and `fn_props`.
//! Together **−1.06%** [−1.68, −0.70] on top of the above. A table that is
//! `is_empty()`-guarded and empty in most programs — `module_namespaces`,
//! `deferred_ns_state`, `obj_realm` — is NOT worth converting: it is already
//! free, and it would pay for a paged index it never uses.
//!
//! The API is deliberately the subset of `HashMap`'s that the VM actually uses,
//! with identical signatures, so swapping a field's type is not a call-site
//! change and cannot alter behaviour.
//!
//! **Iteration order is NOT insertion order** and never was — `values()` walks
//! the dense array, whose order `remove`/`retain` perturb by swapping the last
//! entry into the hole. The `FxHashMap` it replaces had no order either. Nothing
//! observable may depend on it; the only iterations are the GC's root walk and
//! prune, which are order-free. Within one heap object the property order lives
//! in that object's `ObjMap`, which is untouched.

/// Slots per page. 1024 `u32`s is a 4 KiB page — one OS page of index for one
/// aligned run of a thousand heap slots, which is how a burst of same-shaped
/// allocations actually lands.
const PAGE_BITS: u32 = 10;
const PAGE_LEN: usize = 1 << PAGE_BITS;
const PAGE_MASK: u32 = (PAGE_LEN as u32) - 1;

/// "No entry for this slot". `u32::MAX` entries would need a 4-billion-entry
/// table to be reachable as a real position, which the heap cannot produce.
const EMPTY: u32 = u32::MAX;

/// One page of the slot→position index, plus a live count so the page can be
/// released the moment its last entry goes. Without the count a program that
/// touched one slot in each of ten thousand pages would retain 40 MiB of index
/// forever; with it, the index tracks live entries.
struct Page {
    pos: Box<[u32]>,
    used: u32,
}

pub(crate) struct SlotTable<V> {
    /// Paged slot→dense-position index. `pages[slot >> PAGE_BITS]` is `None`
    /// until some slot in that page gets an entry.
    pages: Vec<Option<Page>>,
    /// Dense entry values, no holes.
    vals: Vec<V>,
    /// The owning slot of each dense entry, parallel to `vals`.
    keys: Vec<u32>,
}

impl<V> Default for SlotTable<V> {
    fn default() -> Self {
        SlotTable {
            pages: Vec::new(),
            vals: Vec::new(),
            keys: Vec::new(),
        }
    }
}

impl<V> SlotTable<V> {
    /// Reserved bytes owned directly by the table (including allocated index
    /// pages, but excluding allocations owned inside each `V`).
    pub(crate) fn resident_bytes(&self) -> usize {
        let pages = self
            .pages
            .iter()
            .filter_map(Option::as_ref)
            .fold(0usize, |n, page| {
                n.saturating_add(page.pos.len().saturating_mul(std::mem::size_of::<u32>()))
            });
        self.pages
            .capacity()
            .saturating_mul(std::mem::size_of::<Option<Page>>())
            .saturating_add(
                self.vals
                    .capacity()
                    .saturating_mul(std::mem::size_of::<V>()),
            )
            .saturating_add(
                self.keys
                    .capacity()
                    .saturating_mul(std::mem::size_of::<u32>()),
            )
            .saturating_add(pages)
    }

    #[inline]
    fn pos(&self, k: u32) -> Option<usize> {
        let page = self.pages.get((k >> PAGE_BITS) as usize)?.as_ref()?;
        let p = page.pos[(k & PAGE_MASK) as usize];
        if p == EMPTY {
            None
        } else {
            Some(p as usize)
        }
    }

    /// Point `k` at dense position `p`, allocating the page if needed. `k` must
    /// not already have an entry — callers either just pushed it or are fixing up
    /// the entry that `swap_remove` moved (whose page slot is already counted).
    fn set_pos(&mut self, k: u32, p: u32) {
        let pi = (k >> PAGE_BITS) as usize;
        if pi >= self.pages.len() {
            self.pages.resize_with(pi + 1, || None);
        }
        let page = self.pages[pi].get_or_insert_with(|| Page {
            pos: vec![EMPTY; PAGE_LEN].into_boxed_slice(),
            used: 0,
        });
        let cell = &mut page.pos[(k & PAGE_MASK) as usize];
        if *cell == EMPTY {
            page.used += 1;
        }
        *cell = p;
    }

    fn clear_pos(&mut self, k: u32) {
        let pi = (k >> PAGE_BITS) as usize;
        let Some(slot) = self.pages.get_mut(pi) else {
            return;
        };
        let Some(page) = slot.as_mut() else { return };
        let cell = &mut page.pos[(k & PAGE_MASK) as usize];
        if *cell != EMPTY {
            *cell = EMPTY;
            page.used -= 1;
            if page.used == 0 {
                *slot = None;
            }
        }
    }

    fn push_new(&mut self, k: u32, v: V) -> usize {
        let p = self.vals.len();
        self.vals.push(v);
        self.keys.push(k);
        self.set_pos(k, p as u32);
        p
    }

    /// Drop the dense entry at `p` and repair the index for whatever `swap_remove`
    /// moved into its place. The caller has already cleared `p`'s own key.
    fn unlink(&mut self, p: usize) -> V {
        let v = self.vals.swap_remove(p);
        self.keys.swap_remove(p);
        if p < self.keys.len() {
            let moved = self.keys[p];
            self.set_pos(moved, p as u32);
        }
        v
    }

    #[inline]
    pub(crate) fn get(&self, k: &u32) -> Option<&V> {
        self.pos(*k).map(|p| &self.vals[p])
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, k: &u32) -> Option<&mut V> {
        match self.pos(*k) {
            Some(p) => Some(&mut self.vals[p]),
            None => None,
        }
    }

    /// [`Self::contains_key`] for a probe on a hot path that almost always
    /// misses: forced inline, where a size-first build leaves the hinted one
    /// as a call (6% of a loop of `push` in Lite, for a table no array of the
    /// loop is in).
    #[inline(always)]
    pub(crate) fn has(&self, k: u32) -> bool {
        match self.pages.get((k >> PAGE_BITS) as usize) {
            Some(Some(page)) => page.pos[(k & PAGE_MASK) as usize] != EMPTY,
            _ => false,
        }
    }

    #[inline]
    pub(crate) fn contains_key(&self, k: &u32) -> bool {
        self.pos(*k).is_some()
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.vals.is_empty()
    }

    #[allow(dead_code)]
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.vals.len()
    }

    pub(crate) fn insert(&mut self, k: u32, v: V) -> Option<V> {
        match self.pos(k) {
            Some(p) => Some(std::mem::replace(&mut self.vals[p], v)),
            None => {
                self.push_new(k, v);
                None
            }
        }
    }

    pub(crate) fn remove(&mut self, k: &u32) -> Option<V> {
        let p = self.pos(*k)?;
        self.clear_pos(*k);
        Some(self.unlink(p))
    }

    #[inline]
    pub(crate) fn entry(&mut self, k: u32) -> Entry<'_, V> {
        let pos = self.pos(k);
        Entry {
            table: self,
            key: k,
            pos,
        }
    }

    /// Dense walk of the live values. Order is unspecified (see the module note).
    #[inline]
    pub(crate) fn values(&self) -> std::slice::Iter<'_, V> {
        self.vals.iter()
    }

    #[allow(dead_code)]
    pub(crate) fn iter(&self) -> impl Iterator<Item = (u32, &V)> {
        self.keys.iter().copied().zip(self.vals.iter())
    }

    /// `HashMap::retain`'s signature, over the dense array. The GC's prune is the
    /// only caller, and this is the half of the win the ablation measured: a walk
    /// of the live entries rather than of a capacity that a burst inflated and
    /// nothing ever shrinks.
    pub(crate) fn retain<F: FnMut(&u32, &mut V) -> bool>(&mut self, mut f: F) {
        let mut i = 0;
        while i < self.vals.len() {
            if f(&self.keys[i], &mut self.vals[i]) {
                i += 1;
            } else {
                let k = self.keys[i];
                self.clear_pos(k);
                self.unlink(i);
            }
        }
    }
}

/// `HashMap::entry`'s shape, restricted to the two forms the VM uses:
/// `.entry(idx).or_insert_with(ObjMap::new_side_table)` and one `.or_default()`.
pub(crate) struct Entry<'a, V> {
    table: &'a mut SlotTable<V>,
    key: u32,
    pos: Option<usize>,
}

impl<'a, V> Entry<'a, V> {
    #[inline]
    pub(crate) fn or_insert_with<F: FnOnce() -> V>(self, f: F) -> &'a mut V {
        let p = match self.pos {
            Some(p) => p,
            None => {
                let v = f();
                self.table.push_new(self.key, v)
            }
        };
        &mut self.table.vals[p]
    }
}

impl<'a, V: Default> Entry<'a, V> {
    #[inline]
    pub(crate) fn or_default(self) -> &'a mut V {
        self.or_insert_with(V::default)
    }
}

/// A set of heap slots: one bit a slot.
///
/// For a side table that only records membership. Slots are small and dense,
/// so the set costs an eighth of a byte for each slot up to the highest it
/// has held, a test is one load, and nothing is hashed. Unlike a
/// [`SlotTable`] it has no index pages, so its first entry costs a few
/// hundred bytes rather than a page. The surface is the subset of
/// `HashSet<u32>`'s that the VM uses.
#[derive(Default)]
pub(crate) struct SlotSet {
    words: Vec<u64>,
    len: usize,
}

impl SlotSet {
    /// Reserved bytes owned by the set.
    pub(crate) fn resident_bytes(&self) -> usize {
        self.words
            .capacity()
            .saturating_mul(std::mem::size_of::<u64>())
    }

    #[inline]
    pub(crate) fn contains(&self, k: &u32) -> bool {
        self.words
            .get((*k >> 6) as usize)
            .is_some_and(|word| word >> (*k & 63) & 1 != 0)
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[allow(dead_code)] // `HashSet`-compatible surface; read by the tests.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Add `k`; whether it was absent.
    pub(crate) fn insert(&mut self, k: u32) -> bool {
        let at = (k >> 6) as usize;
        if at >= self.words.len() {
            self.words.resize(at + 1, 0);
        }
        let bit = 1u64 << (k & 63);
        let added = self.words[at] & bit == 0;
        self.words[at] |= bit;
        self.len += usize::from(added);
        added
    }

    /// Remove `k`; whether it was present.
    pub(crate) fn remove(&mut self, k: &u32) -> bool {
        let bit = 1u64 << (*k & 63);
        match self.words.get_mut((*k >> 6) as usize) {
            Some(word) if *word & bit != 0 => {
                *word &= !bit;
                self.len -= 1;
                true
            }
            _ => false,
        }
    }

    /// Keep the slots `keep` answers true for.
    pub(crate) fn retain<F: FnMut(&u32) -> bool>(&mut self, mut keep: F) {
        if self.len == 0 {
            return;
        }
        for (at, word) in self.words.iter_mut().enumerate() {
            let mut left = *word;
            while left != 0 {
                let low = left.trailing_zeros();
                left &= left - 1;
                if !keep(&((at as u32) << 6 | low)) {
                    *word &= !(1u64 << low);
                    self.len -= 1;
                }
            }
        }
    }
}

/// `Vm::proto_of`: an object's explicit [[Prototype]], for the objects that
/// have one (`Object.create(p)`, `new F()`, `setPrototypeOf`), with a dense
/// copy of each entry's link beside it.
///
/// The property caches ask on every inherited read whether the receiver has
/// an entry, and which object it names (`Vm::ic_chain_ok`), and the answer is
/// almost always "none". The table's paged index answered in three dependent
/// loads for "none" and five for an entry; the link copy answers both in two.
/// It is a copy only: the table stays the authority for the value itself,
/// and every way an entry comes or goes runs through `insert` or the
/// collector's two prunes (`SlotKeyed`), which keep it in step. The table
/// has no `remove` and hands out no `&mut` to a value: nothing else changes
/// an entry. An inherited-read loop over `Object.create(p)` objects ran 5%
/// faster with the check gone altogether.
///
/// Four bytes for each slot up to the highest that ever had an entry.
#[derive(Default)]
pub(crate) struct ProtoTable {
    table: SlotTable<crate::value::Value>,
    /// Per slot: [`ProtoTable::NONE`], [`ProtoTable::NON_HEAP`] (an entry
    /// that is not a heap object: `null`), or the heap index plus
    /// [`ProtoTable::HEAP_BASE`].
    link: Vec<u32>,
}

impl ProtoTable {
    pub(crate) const NONE: u32 = 0;
    pub(crate) const NON_HEAP: u32 = 1;
    pub(crate) const HEAP_BASE: u32 = 2;

    #[inline]
    fn encode(v: crate::value::Value) -> u32 {
        if v.is_heap() {
            v.heap_index() + Self::HEAP_BASE
        } else {
            Self::NON_HEAP
        }
    }

    /// Slot `k`'s link: `NONE`, `NON_HEAP`, or `HEAP_BASE` plus the index.
    #[inline(always)]
    pub(crate) fn link(&self, k: u32) -> u32 {
        self.link.get(k as usize).copied().unwrap_or(Self::NONE)
    }

    pub(crate) fn resident_bytes(&self) -> usize {
        self.table.resident_bytes().saturating_add(
            self.link
                .capacity()
                .saturating_mul(std::mem::size_of::<u32>()),
        )
    }

    #[inline]
    pub(crate) fn get(&self, k: &u32) -> Option<&crate::value::Value> {
        if self.link(*k) == Self::NONE {
            return None;
        }
        self.table.get(k)
    }

    /// [`Self::get`] by value: a heap prototype, the case nearly every entry
    /// is, is the link's index itself, so only a `null` one reads the table.
    #[inline]
    pub(crate) fn get_copied(&self, k: u32) -> Option<crate::value::Value> {
        match self.link(k) {
            Self::NONE => None,
            Self::NON_HEAP => self.table.get(&k).copied(),
            l => Some(crate::value::Value::heap(l - Self::HEAP_BASE)),
        }
    }

    #[inline(always)]
    pub(crate) fn has(&self, k: u32) -> bool {
        self.link(k) != Self::NONE
    }

    #[inline]
    pub(crate) fn contains_key(&self, k: &u32) -> bool {
        self.has(*k)
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    pub(crate) fn insert(&mut self, k: u32, v: crate::value::Value) -> Option<crate::value::Value> {
        let at = k as usize;
        if at >= self.link.len() {
            self.link.resize(at + 1, Self::NONE);
        }
        self.link[at] = Self::encode(v);
        self.table.insert(k, v)
    }

    /// Dense walk of the live values. Order is unspecified.
    #[inline]
    pub(crate) fn values(&self) -> std::slice::Iter<'_, crate::value::Value> {
        self.table.values()
    }

    /// The table's `retain`, clearing the link of each entry it drops.
    pub(crate) fn retain<F: FnMut(&u32) -> bool>(&mut self, mut keep: F) {
        let link = &mut self.link;
        self.table.retain(|k, _| {
            let kept = keep(k);
            if !kept {
                if let Some(l) = link.get_mut(*k as usize) {
                    *l = Self::NONE;
                }
            }
            kept
        });
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.table.len()
    }

    /// Drop `k`'s entry, if any (the collector's prune of a freed slot).
    pub(crate) fn forget(&mut self, k: u32) {
        if self.link(k) != Self::NONE {
            self.link[k as usize] = Self::NONE;
            self.table.remove(&k);
        }
    }
}

#[cfg(test)]
mod proto_table_tests {
    use super::ProtoTable;
    use crate::value::Value;

    #[test]
    fn the_link_copy_follows_every_change() {
        let mut t = ProtoTable::default();
        assert!(t.is_empty() && !t.has(5) && t.get(&5).is_none());
        assert_eq!(t.link(4_000_000_000), ProtoTable::NONE);
        assert!(t.insert(5, Value::heap(9)).is_none());
        assert!(t.insert(70_000, Value::NULL).is_none());
        assert_eq!(t.link(5), 9 + ProtoTable::HEAP_BASE);
        assert_eq!(t.link(70_000), ProtoTable::NON_HEAP);
        assert_eq!(t.get(&5), Some(&Value::heap(9)));
        assert_eq!(t.get_copied(5), Some(Value::heap(9)));
        assert_eq!(t.get_copied(70_000), Some(Value::NULL));
        assert_eq!(t.get_copied(6), None);
        assert_eq!(t.insert(5, Value::heap(0)), Some(Value::heap(9)));
        assert_eq!(t.get_copied(5), Some(Value::heap(0)));
        assert_eq!(t.link(5), ProtoTable::HEAP_BASE);
        assert!(t.has(5) && t.contains_key(&70_000) && !t.has(6));
        t.retain(|&k| k != 5);
        assert!(!t.has(5) && t.get(&5).is_none() && t.has(70_000));
        t.forget(70_000);
        t.forget(70_000);
        assert!(t.is_empty() && !t.has(70_000) && t.len() == 0);
    }
}

#[cfg(test)]
mod slot_set_tests {
    use super::SlotSet;
    use std::collections::HashSet;

    #[test]
    fn a_slot_set_is_a_set() {
        let mut set = SlotSet::default();
        assert!(set.is_empty());
        assert!(!set.contains(&0));
        assert!(!set.contains(&4_000_000_000));
        assert!(!set.remove(&7));
        assert!(set.insert(7));
        assert!(!set.insert(7));
        assert!(set.insert(0));
        assert!(set.insert(63));
        assert!(set.insert(64));
        assert!(set.insert(100_000));
        assert_eq!(set.len(), 5);
        for k in [0, 7, 63, 64, 100_000] {
            assert!(set.contains(&k), "{k}");
        }
        for k in [1, 6, 8, 62, 65, 99_999, 100_001] {
            assert!(!set.contains(&k), "{k}");
        }
        assert!(set.remove(&63));
        assert!(!set.remove(&63));
        assert!(!set.contains(&63));
        set.retain(|&k| k != 0 && k != 100_000);
        assert_eq!(set.len(), 2);
        assert!(set.contains(&7) && set.contains(&64));
        set.retain(|_| false);
        assert!(set.is_empty());
        assert!(!set.contains(&7));
    }

    #[test]
    fn a_slot_set_agrees_with_a_hash_set_and_stays_small() {
        let mut set = SlotSet::default();
        let mut model = HashSet::new();
        let mut state = 0x9E37_79B9u32;
        for round in 0..20_000u32 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let k = (state >> 8) % 5_000;
            match round % 3 {
                0 | 1 => assert_eq!(set.insert(k), model.insert(k)),
                _ => assert_eq!(set.remove(&k), model.remove(&k)),
            }
            if round % 997 == 0 {
                set.retain(|k| k % 7 != 3);
                model.retain(|k| k % 7 != 3);
            }
            assert_eq!(set.len(), model.len());
        }
        for k in 0..5_100 {
            assert_eq!(set.contains(&k), model.contains(&k), "{k}");
        }
        // A bit a slot: five thousand slots are well under a kilobyte, where
        // one page of a paged index is four.
        assert!(set.resident_bytes() <= 1024, "{}", set.resident_bytes());
        let mut one = SlotSet::default();
        one.insert(3_000);
        assert!(one.resident_bytes() <= 1024, "{}", one.resident_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn basic_get_insert_remove() {
        let mut t: SlotTable<u32> = SlotTable::default();
        assert!(t.is_empty());
        assert_eq!(t.get(&0), None);
        assert_eq!(t.get(&999_999), None);
        assert_eq!(t.insert(5, 50), None);
        assert_eq!(t.insert(5, 51), Some(50));
        assert_eq!(t.get(&5), Some(&51));
        assert!(t.contains_key(&5));
        assert!(!t.contains_key(&6));
        assert_eq!(t.len(), 1);
        assert_eq!(t.remove(&5), Some(51));
        assert_eq!(t.remove(&5), None);
        assert!(t.is_empty());
    }

    #[test]
    fn entry_or_insert_with() {
        let mut t: SlotTable<Vec<u32>> = SlotTable::default();
        t.entry(3).or_insert_with(Vec::new).push(1);
        t.entry(3).or_insert_with(Vec::new).push(2);
        assert_eq!(t.get(&3), Some(&vec![1, 2]));
        assert_eq!(t.len(), 1);
    }

    /// A page must be released when its last entry goes, or a program that
    /// touches one slot per page retains the whole index forever.
    #[test]
    fn pages_are_released_when_empty() {
        let mut t: SlotTable<u32> = SlotTable::default();
        for i in 0..64u32 {
            t.insert(i * PAGE_LEN as u32, i);
        }
        assert_eq!(t.pages.iter().filter(|p| p.is_some()).count(), 64);
        for i in 0..64u32 {
            t.remove(&(i * PAGE_LEN as u32));
        }
        assert_eq!(t.pages.iter().filter(|p| p.is_some()).count(), 0);
        assert!(t.is_empty());
        // and it still works afterwards
        t.insert(7, 7);
        assert_eq!(t.get(&7), Some(&7));
    }

    #[test]
    fn retain_matches_hashmap() {
        let mut t: SlotTable<u32> = SlotTable::default();
        let mut h: HashMap<u32, u32> = HashMap::new();
        for i in 0..5000u32 {
            let k = (i * 7919) % 20000;
            t.insert(k, i);
            h.insert(k, i);
        }
        t.retain(|&k, _| k % 3 == 0);
        h.retain(|&k, _| k % 3 == 0);
        assert_eq!(t.len(), h.len());
        for (k, v) in &h {
            assert_eq!(t.get(k), Some(v), "key {k}");
        }
        for k in 0..20000u32 {
            assert_eq!(t.contains_key(&k), h.contains_key(&k), "key {k}");
        }
    }

    /// The real gate: a deterministic pseudo-random op stream applied to both a
    /// `SlotTable` and a `HashMap`, compared after every operation. Anything the
    /// swap-remove index repair gets wrong shows up here.
    #[test]
    fn differential_against_hashmap() {
        let mut t: SlotTable<u64> = SlotTable::default();
        let mut h: HashMap<u32, u64> = HashMap::new();
        let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for step in 0..40_000u64 {
            let r = next();
            // Keys clustered like heap slots: mostly a low dense run, sometimes far away.
            let k = if r % 8 == 0 {
                (r >> 8) as u32 % 100_000
            } else {
                (r >> 8) as u32 % 300
            };
            match r % 5 {
                0 | 1 => {
                    assert_eq!(t.insert(k, step), h.insert(k, step), "insert {k} @{step}");
                }
                2 => {
                    assert_eq!(t.remove(&k), h.remove(&k), "remove {k} @{step}");
                }
                3 => {
                    let a = *t.entry(k).or_insert_with(|| step);
                    let b = *h.entry(k).or_insert(step);
                    assert_eq!(a, b, "entry {k} @{step}");
                }
                _ => {
                    assert_eq!(t.get(&k), h.get(&k), "get {k} @{step}");
                    if let (Some(a), Some(b)) = (t.get_mut(&k), h.get_mut(&k)) {
                        *a += 1;
                        *b += 1;
                    }
                }
            }
            assert_eq!(t.len(), h.len(), "len @{step}");
            if step % 1000 == 0 {
                let keep = (next() % 4) as u64;
                t.retain(|_, v| *v % 4 != keep);
                h.retain(|_, v| *v % 4 != keep);
                assert_eq!(t.len(), h.len(), "retain len @{step}");
            }
        }
        // Final full comparison, both directions.
        for (k, v) in &h {
            assert_eq!(t.get(k), Some(v), "final key {k}");
        }
        let mut seen: Vec<(u32, u64)> = t.iter().map(|(k, v)| (k, *v)).collect();
        seen.sort_unstable();
        let mut want: Vec<(u32, u64)> = h.iter().map(|(k, v)| (*k, *v)).collect();
        want.sort_unstable();
        assert_eq!(seen, want);
        assert_eq!(t.values().count(), h.len());
    }
}
