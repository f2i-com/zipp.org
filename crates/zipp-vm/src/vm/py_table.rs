//! Python dict and set storage, native (stage S2b of the native-core plan):
//! a CPython-3.13-style compact ordered hash table of engine values.
//!
//! One non-generic [`PyTable`] serves dict (keys and values) and set (keys
//! only). Entries live in parallel columns (`hashes`, `keys`, `vals`) in
//! insertion order; `indices` maps hash slots to entry positions and is
//! absent while the table has at most [`SMALL`] entry slots (a linear scan
//! of `hashes` is then cheaper). A deleted entry keeps its slot as a
//! [`Value::HOLE`] key (never a user key) until the next compaction, which
//! happens when the entry columns or the index fill up.
//!
//! The table only ever compares stored hashes; key equality is the
//! caller's (`&mut dyn FnMut`, no monomorphization). Two front ends drive it:
//!
//! * the native one ([`native_hash`], [`native_eq`], [`native_lookup`],
//!   [`native_add`], the set algebra), for keys whose hash and equality
//!   need no guest code: str, int, float, bool, None, and tuples and
//!   frozensets of those. Each answers `None` where guest code would run;
//! * the split protocol, for any other key: the runtime computes the hash
//!   (`rt.hash(k)`), [`PyTable::probe`] reports an identity hit or the
//!   same-hash candidates, the runtime runs `eq()` on them in order and
//!   reads the winner with [`PyTable::at`], which refuses an index once
//!   [`PyTable::version`] moved (a guest `__eq__` mutated the table: the
//!   runtime restarts). An insert after a miss passes the probe's [`Hint`]
//!   back, refused likewise when stale. Natives never call guest code.
//!
//! Hashes equal the runtime's Python-visible `hash()` (`hashInt` and
//! friends in `runtime/types.js`) bit for bit, so a key hashed natively and
//! the same key hashed by the runtime land together.
//!
//! Order: a dict iterates in insertion order (a replaced value keeps its
//! place, a deleted-then-added key goes last). A set iterates in the order
//! the runtime's bucket Map gives today ([`PyTable::visible_order`]):
//! insertion order of *buckets*, where the runtime files two unequal keys
//! with the same `keyOf` in one bucket (every NaN; an int beyond 2^53 and
//! the int equal to its hash) and lists a later one right after the
//! bucket's earlier ones; then, when every element is an int in
//! `[0, table size)` (CPython's table size for that count), ascending.
//!
//! Work accounting: every operation adds its work units to a caller's
//! `steps` counter (one per probe step or comparison, [`STEPS_PER_ENTRY`]
//! per entry of bulk work such as a resize, copy, update or set algebra);
//! the caller charges them (`Vm::charge_steps`) and admits bulk work up
//! front with [`bulk_cost`] (`Vm::native_kernel_admits`). Tables hold at
//! most [`MAX_ITEMS`] entries (the runtime's `MAX_ITEMS`).

// Nothing references the module until the wiring stage (the heap variant
// and the runtime's native family); `pytable_size_probe` is a cfg for
// measuring the module's compiled size in the wasm build.
#![allow(dead_code, unexpected_cfgs)]

use super::BigVal;
use crate::heap::{wtf8_decode, Heap, HeapObj};
use crate::value::Value;
use num_bigint::BigInt as NumBig;
use std::cmp::Ordering;

/// Most entries a table holds (`rt.MAX_ITEMS`, `runtime/core.js`).
pub(crate) const MAX_ITEMS: usize = 1 << 24;
/// Steps charged per entry of bulk work (the `ORD_STEPS_PER_UNIT` pattern).
pub(crate) const STEPS_PER_ENTRY: u64 = 4;
/// Entry slots kept without an index.
pub(crate) const SMALL: usize = 8;
/// Smallest index.
const MIN_INDEX: usize = 16;
const EMPTY: u32 = u32::MAX;
const DUMMY: u32 = u32::MAX - 1;
const PERTURB_SHIFT: u32 = 5;
/// The Mersenne prime 2^61 - 1 of CPython's numeric hash.
const MODULUS: u64 = (1 << 61) - 1;
/// `hash(None)` (CPython 3.12+, and the runtime).
pub(crate) const NONE_HASH: i64 = 4_238_894_112;
const INF_HASH: i64 = 314_159;
/// `Number.MAX_SAFE_INTEGER`, the runtime's `SAFE`.
const SAFE: i128 = 9_007_199_254_740_991;
/// Deepest tuple/frozenset nesting handled natively.
const MAX_DEPTH: u32 = 32;
/// Largest set whose order the runtime checks for the small-int rule.
const SMALL_INT_RULE_MAX: usize = 50_000;
/// Largest frozenset hashed natively (its text sort is quadratic).
const MAX_FROZEN_TEXTS: usize = 1024;

/// Sort entry positions by `key`, ties by position (so, a stable sort by
/// `key` of an ascending list). Every sort in the module goes through this
/// one `[u64]::sort_unstable` instantiation: each extra monomorphized sort
/// costs kilobytes of wasm.
fn sort_positions(v: &mut [usize], key: &dyn Fn(usize) -> u32) {
    let mut packed: Vec<u64> = v.iter().map(|&i| (u64::from(key(i)) << 32) | i as u64).collect();
    packed.sort_unstable();
    for (d, p) in v.iter_mut().zip(packed) {
        *d = (p & 0xFFFF_FFFF) as usize;
    }
}

/// Steps to admit up front for bulk work over `n` entries.
pub(crate) fn bulk_cost(n: usize) -> u64 {
    (n as u64).saturating_mul(STEPS_PER_ENTRY)
}

/// A probe's promise: an insert may use it only while the table's version
/// is unchanged. (The free slot is re-found at insert; that walk makes no
/// key comparisons.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Hint {
    version: u32,
}

/// [`PyTable::lookup`]'s answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lookup {
    Found(usize),
    Absent(Hint),
}

/// [`PyTable::probe`]'s answer for a key needing guest equality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Probe {
    /// The key itself is stored here, and no other same-hash key precedes it.
    Hit(usize),
    /// Entries with the key's hash, in insertion order (the key itself may
    /// be among them): the runtime runs `eq()` on each in turn.
    Candidates(Vec<usize>, Hint),
    /// No entry has the key's hash.
    Miss(Hint),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InsertError {
    /// The table changed since the probe: probe again.
    Stale,
    /// [`MAX_ITEMS`] reached (the runtime raises MemoryError).
    Full,
}

/// Where a resumable bulk operation stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// At this position of the source (entries before it are applied): a
    /// key needs guest code, so the runtime continues from here.
    Guest(usize),
    Full,
}

/// A Python dict's or set's storage.
#[derive(Clone, Debug, Default)]
pub(crate) struct PyTable {
    /// Hash slot -> entry position, [`EMPTY`] or [`DUMMY`]; empty while the
    /// table has at most [`SMALL`] entry slots.
    indices: Vec<u32>,
    hashes: Vec<i64>,
    /// [`Value::HOLE`] for a deleted entry. Never ends in a hole.
    keys: Vec<Value>,
    /// Parallel to `keys` for a dict; empty for a set.
    vals: Vec<Value>,
    /// Set mode, once two unequal keys shared a runtime bucket: each entry's
    /// bucket stamp (see [`PyTable::visible_order`]). Empty otherwise, when
    /// every entry is its own bucket in entry order.
    stamps: Vec<u32>,
    next_stamp: u32,
    /// Index slots not [`EMPTY`].
    fill: u32,
    len: u32,
    /// Bumped by every insert, delete, clear and compaction (never by a
    /// value replacement).
    version: u32,
    /// Reserved for stage S3's hidden-class layouts (0 = none).
    pub(crate) layout: u32,
    set: bool,
}

impl PyTable {
    pub(crate) fn new_dict() -> PyTable {
        PyTable::default()
    }

    pub(crate) fn new_set() -> PyTable {
        PyTable {
            set: true,
            ..PyTable::default()
        }
    }

    pub(crate) fn is_set(&self) -> bool {
        self.set
    }

    pub(crate) fn len(&self) -> usize {
        self.len as usize
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn version(&self) -> u32 {
        self.version
    }

    /// Entry positions in use (live or deleted): the bound for iterating by
    /// position.
    pub(crate) fn slots(&self) -> usize {
        self.keys.len()
    }

    /// The live key at `i`.
    pub(crate) fn key_at(&self, i: usize) -> Option<Value> {
        self.keys.get(i).copied().filter(|k| !k.is_hole())
    }

    /// The value at live entry `i` (undefined for a set).
    pub(crate) fn value_at(&self, i: usize) -> Option<Value> {
        self.key_at(i)?;
        Some(self.vals.get(i).copied().unwrap_or(Value::UNDEFINED))
    }

    pub(crate) fn hash_at(&self, i: usize) -> Option<i64> {
        self.key_at(i)?;
        self.hashes.get(i).copied()
    }

    /// Key and value of live entry `i`, while the table is at `version`.
    pub(crate) fn at(&self, i: usize, version: u32) -> Option<(Value, Value)> {
        if version != self.version {
            return None;
        }
        Some((self.key_at(i)?, self.value_at(i)?))
    }

    /// The first live entry at or after position `from`.
    pub(crate) fn next_live(&self, from: usize) -> Option<usize> {
        (from..self.keys.len()).find(|&i| !self.keys[i].is_hole())
    }

    /// Live entries in insertion (dict) order: (position, key, value).
    pub(crate) fn entries(&self) -> impl Iterator<Item = (usize, Value, Value)> + '_ {
        self.keys.iter().enumerate().filter(|(_, k)| !k.is_hole()).map(move |(i, &k)| {
            (i, k, self.vals.get(i).copied().unwrap_or(Value::UNDEFINED))
        })
    }

    /// Bytes owned beyond the struct (for heap accounting).
    pub(crate) fn payload_bytes(&self) -> usize {
        std::mem::size_of::<PyTable>()
            + self.indices.capacity() * 4
            + self.hashes.capacity() * 8
            + self.keys.capacity() * 8
            + self.vals.capacity() * 8
            + self.stamps.capacity() * 4
    }

    /// Entry slots before the next compaction or growth.
    fn usable(&self) -> usize {
        if self.indices.is_empty() {
            SMALL
        } else {
            self.indices.len() * 2 / 3
        }
    }

    /// Walk `h`'s entries: `eq(position)` for each live entry with hash
    /// `h` (in index-probe order), until it answers `Some(true)` (found) or
    /// `None` (undecidable here). A miss yields the version hint.
    fn walk(&self, h: i64, eq: &mut dyn FnMut(usize) -> Option<bool>, steps: &mut u64) -> Option<Lookup> {
        let absent = Lookup::Absent(Hint {
            version: self.version,
        });
        if self.indices.is_empty() {
            for i in 0..self.keys.len() {
                *steps += 1;
                if self.hashes[i] == h && !self.keys[i].is_hole() && eq(i)? {
                    return Some(Lookup::Found(i));
                }
            }
            return Some(absent);
        }
        let mask = self.indices.len() - 1;
        let mut perturb = h as u64;
        let mut s = (h as u64 as usize) & mask;
        loop {
            *steps += 1;
            let ix = self.indices[s];
            if ix == EMPTY {
                return Some(absent);
            }
            if ix != DUMMY && self.hashes[ix as usize] == h && eq(ix as usize)? {
                return Some(Lookup::Found(ix as usize));
            }
            perturb >>= PERTURB_SHIFT;
            s = s.wrapping_mul(5).wrapping_add(perturb as usize).wrapping_add(1) & mask;
        }
    }

    /// The slot for a new entry of hash `h`: the first EMPTY or DUMMY one.
    fn free_slot(&self, h: i64) -> usize {
        let mask = self.indices.len() - 1;
        let mut perturb = h as u64;
        let mut s = (h as u64 as usize) & mask;
        while self.indices[s] < DUMMY {
            perturb >>= PERTURB_SHIFT;
            s = s.wrapping_mul(5).wrapping_add(perturb as usize).wrapping_add(1) & mask;
        }
        s
    }

    /// The slot holding entry `i` (of hash `h`).
    fn slot_of(&self, h: i64, i: usize) -> Option<usize> {
        let mask = self.indices.len() - 1;
        let mut perturb = h as u64;
        let mut s = (h as u64 as usize) & mask;
        loop {
            match self.indices[s] {
                EMPTY => return None,
                ix if ix as usize == i => return Some(s),
                _ => {}
            }
            perturb >>= PERTURB_SHIFT;
            s = s.wrapping_mul(5).wrapping_add(perturb as usize).wrapping_add(1) & mask;
        }
    }

    /// Find the key of hash `h` that `eq(stored_key)` accepts; `None` when
    /// `eq` could not decide (a stored key needs guest code).
    pub(crate) fn lookup(&self, h: i64, eq: &mut dyn FnMut(Value) -> Option<bool>, steps: &mut u64) -> Option<Lookup> {
        self.walk(h, &mut |i| eq(self.keys[i]), steps)
    }

    /// The split protocol's first step for a key needing guest equality.
    pub(crate) fn probe(&self, h: i64, key: Value, steps: &mut u64) -> Probe {
        let mut same: Vec<usize> = Vec::new();
        let hint = match self.walk(
            h,
            &mut |i| {
                same.push(i);
                Some(false)
            },
            steps,
        ) {
            Some(Lookup::Absent(hint)) => hint,
            _ => unreachable!("the collecting walk never finds"),
        };
        sort_positions(&mut same, &|_| 0);
        match same.first() {
            None => Probe::Miss(hint),
            Some(&i) if key.is_heap() && self.keys[i] == key => Probe::Hit(i),
            _ => Probe::Candidates(same, hint),
        }
    }

    /// Live entries with hash `h`, in insertion order.
    pub(crate) fn same_hash(&self, h: i64, steps: &mut u64) -> Vec<usize> {
        match self.probe(h, Value::UNDEFINED, steps) {
            Probe::Candidates(v, _) => v,
            _ => Vec::new(),
        }
    }

    /// Insert a key known to be absent (the probe's `hint` proves it, as
    /// long as the table has not changed since). `mate`, for a set, is an
    /// entry the runtime files in the key's bucket (see the module comment;
    /// the native front end finds it with [`same_bucket`]). Answers the new
    /// entry's position.
    pub(crate) fn insert(
        &mut self,
        hint: Hint,
        h: i64,
        key: Value,
        val: Value,
        mate: Option<usize>,
        steps: &mut u64,
    ) -> Result<usize, InsertError> {
        if hint.version != self.version {
            return Err(InsertError::Stale);
        }
        if self.len() >= MAX_ITEMS {
            return Err(InsertError::Full);
        }
        let mut mate = mate.filter(|&m| self.set && self.key_at(m).is_some());
        if self.keys.len() >= self.usable() || self.fill as usize >= self.usable() {
            mate = self.rebuild(mate, 1, steps);
        }
        let pos = self.keys.len();
        if !self.indices.is_empty() {
            let s = self.free_slot(h);
            if self.indices[s] == EMPTY {
                self.fill += 1;
            }
            self.indices[s] = pos as u32;
        }
        if let Some(m) = mate {
            if self.stamps.is_empty() {
                self.stamps = (0..pos as u32).collect();
                self.next_stamp = pos as u32;
            }
            self.stamps.push(self.stamps[m]);
        } else if !self.stamps.is_empty() {
            self.stamps.push(self.next_stamp);
            self.next_stamp += 1;
        }
        self.hashes.push(h);
        self.keys.push(key);
        if !self.set {
            self.vals.push(val);
        }
        self.len += 1;
        self.version = self.version.wrapping_add(1);
        Ok(pos)
    }

    /// Replace live entry `i`'s value (a dict's; keeps its place and key).
    pub(crate) fn set_value(&mut self, i: usize, val: Value) -> bool {
        if self.set || self.key_at(i).is_none() {
            return false;
        }
        self.vals[i] = val;
        true
    }

    /// Delete live entry `i`: its key and value.
    pub(crate) fn remove_at(&mut self, i: usize) -> Option<(Value, Value)> {
        let key = self.key_at(i)?;
        if !self.indices.is_empty() {
            let s = self.slot_of(self.hashes[i], i)?;
            self.indices[s] = DUMMY;
        }
        self.keys[i] = Value::HOLE;
        let val = if self.set {
            Value::UNDEFINED
        } else {
            std::mem::replace(&mut self.vals[i], Value::UNDEFINED)
        };
        self.len -= 1;
        self.version = self.version.wrapping_add(1);
        while self.keys.last().is_some_and(|k| k.is_hole()) {
            self.keys.pop();
            self.hashes.pop();
            self.vals.truncate(self.keys.len());
            self.stamps.truncate(self.keys.len());
        }
        Some((key, val))
    }

    /// `dict.popitem()`: the last entry.
    pub(crate) fn pop_last(&mut self) -> Option<(Value, Value)> {
        let i = self.keys.len().checked_sub(1)?;
        self.remove_at(i)
    }

    pub(crate) fn clear(&mut self) {
        let (set, version) = (self.set, self.version.wrapping_add(1));
        *self = PyTable {
            set,
            version,
            ..PyTable::default()
        };
    }

    /// Compact the entries (dropping holes) and size the index for `extra`
    /// more; answers `track`'s new position.
    fn rebuild(&mut self, track: Option<usize>, extra: usize, steps: &mut u64) -> Option<usize> {
        *steps += bulk_cost(self.keys.len());
        let mut tracked = None;
        let mut w = 0;
        for r in 0..self.keys.len() {
            if self.keys[r].is_hole() {
                continue;
            }
            if track == Some(r) {
                tracked = Some(w);
            }
            self.keys[w] = self.keys[r];
            self.hashes[w] = self.hashes[r];
            if !self.set {
                self.vals[w] = self.vals[r];
            }
            if !self.stamps.is_empty() {
                self.stamps[w] = self.stamps[r];
            }
            w += 1;
        }
        self.keys.truncate(w);
        self.hashes.truncate(w);
        self.vals.truncate(if self.set { 0 } else { w });
        self.stamps.truncate(if self.stamps.is_empty() { 0 } else { w });
        self.renumber_stamps();
        let want = w + extra;
        self.indices = Vec::new();
        self.fill = 0;
        if want > SMALL {
            let size = (want * 3).next_power_of_two().max(MIN_INDEX);
            self.indices = vec![EMPTY; size];
            for i in 0..w {
                let s = self.free_slot(self.hashes[i]);
                self.indices[s] = i as u32;
            }
            self.fill = w as u32;
        }
        self.version = self.version.wrapping_add(1);
        tracked
    }

    /// Stamps as ranks from 0 (order kept); dropped when every entry is its
    /// own bucket in entry order.
    fn renumber_stamps(&mut self) {
        if self.stamps.is_empty() {
            return;
        }
        if self.stamps.windows(2).all(|w| w[0] < w[1]) {
            self.stamps = Vec::new();
            self.next_stamp = 0;
            return;
        }
        let mut sorted: Vec<u64> = self.stamps.iter().map(|&s| u64::from(s)).collect();
        sorted.sort_unstable();
        sorted.dedup();
        for s in &mut self.stamps {
            *s = sorted.binary_search(&u64::from(*s)).unwrap_or(0) as u32;
        }
        self.next_stamp = sorted.len() as u32;
    }

    /// Live entry positions in the runtime's set iteration order (see the
    /// module comment); for a dict, insertion order.
    pub(crate) fn visible_order(&self, heap: &Heap) -> Vec<usize> {
        let mut order: Vec<usize> = self.entries().map(|(i, _, _)| i).collect();
        if !self.set {
            return order;
        }
        if !self.stamps.is_empty() {
            sort_positions(&mut order, &|i| self.stamps[i]);
        }
        let n = order.len();
        if n > 1 && n <= SMALL_INT_RULE_MAX {
            let bound = cpython_set_size(n) as i128;
            let small = order.iter().all(|&i| {
                let k = self.keys[i];
                k.is_heap() && matches!(heap.get(k.heap_index()), HeapObj::BigInt(v) if (0..bound).contains(v))
            });
            if small {
                // Such an int's hash is itself (and below 2^17).
                sort_positions(&mut order, &|i| self.hashes[i] as u32);
            }
        }
        order
    }

    /// `set.pop()`'s element: the first in [`PyTable::visible_order`].
    pub(crate) fn first_visible(&self, heap: &Heap) -> Option<usize> {
        if self.set {
            return self.visible_order(heap).first().copied();
        }
        self.next_live(0)
    }

    /// A compacted copy: a dict's entries in insertion order; a set's in
    /// [`PyTable::visible_order`] (`setFrom` iterates the source), buckets
    /// kept.
    pub(crate) fn copy(&self, heap: &Heap, steps: &mut u64) -> PyTable {
        let order = self.visible_order(heap);
        *steps += bulk_cost(order.len());
        self.copied_in(&order, steps)
    }

    /// A new table of the entries at `order`, in that order (stamps merged:
    /// entries sharing a bucket stay together).
    fn copied_in(&self, order: &[usize], steps: &mut u64) -> PyTable {
        let mut out = PyTable {
            set: self.set,
            ..PyTable::default()
        };
        out.hashes.reserve_exact(order.len());
        out.keys.reserve_exact(order.len());
        let mut prev: Option<u32> = None;
        let mut grouped = false;
        for &i in order {
            out.hashes.push(self.hashes[i]);
            out.keys.push(self.keys[i]);
            if !self.set {
                out.vals.push(self.vals[i]);
            }
            if !self.stamps.is_empty() {
                let s = self.stamps[i];
                grouped |= prev == Some(s);
                let next = match prev {
                    Some(p) if p == s => out.next_stamp - 1,
                    _ => {
                        out.next_stamp += 1;
                        out.next_stamp - 1
                    }
                };
                out.stamps.push(next);
                prev = Some(s);
            }
        }
        if !grouped {
            out.stamps = Vec::new();
            out.next_stamp = 0;
        }
        out.len = out.keys.len() as u32;
        out.rebuild(None, 0, steps);
        out
    }
}

/// CPython's set table size for `n` elements added one by one, as the
/// runtime's `setList` simulates it: from 8 slots, grown to the smallest
/// power of two (at least 8) above 4 * fill once fill * 5 >= (size - 1) * 3.
pub(crate) fn cpython_set_size(n: usize) -> usize {
    let (mut size, mut fill) = (8usize, 0usize);
    loop {
        let at = ((size - 1) * 3).div_ceil(5).max(fill + 1);
        if at > n {
            return size;
        }
        fill = at;
        size = 8;
        while size <= fill * 4 {
            size *= 2;
        }
    }
}

// ---- native keys: hash, equality, bucket ---------------------------------------------------------

/// The runtime's type objects the native front end recognizes (by
/// identity: a record's own data `cls`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PyKinds {
    pub(crate) tuple: Value,
    pub(crate) frozenset: Value,
}

/// A native key, classified.
enum Kind<'h> {
    Float(f64),
    Int(i128),
    Big(&'h NumBig),
    Str(u32),
    None,
    Tuple(&'h [Value]),
    /// A frozenset's storage.
    Frozen(Value),
}

/// An own data property of the plain object `idx`.
fn rec_field(heap: &Heap, idx: u32, key: &str) -> Option<Value> {
    let HeapObj::Object(m) = heap.get(idx) else {
        return None;
    };
    if m.is_ctor {
        return None;
    }
    let s = m.pos(key)?;
    (!m.is_accessor_at(s)).then(|| m.val_at(s))
}

fn classify<'h>(heap: &'h Heap, kinds: &PyKinds, v: Value) -> Option<Kind<'h>> {
    if v.is_number() {
        return Some(Kind::Float(v.as_f64()));
    }
    if v.is_bool() {
        return Some(Kind::Int(v.as_bool() as i128));
    }
    if v.is_null() {
        return Some(Kind::None);
    }
    if !v.is_heap() {
        return None;
    }
    let idx = v.heap_index();
    match heap.get(idx) {
        HeapObj::Str(_) | HeapObj::Cons { .. } => Some(Kind::Str(idx)),
        HeapObj::BigInt(n) => Some(Kind::Int(*n)),
        HeapObj::BigIntBig(b) => Some(Kind::Big(b)),
        HeapObj::Object(_) => {
            let cls = rec_field(heap, idx, "cls")?;
            if cls == kinds.tuple {
                let items = rec_field(heap, idx, "items")?;
                if !items.is_heap() {
                    return None;
                }
                match heap.get(items.heap_index()) {
                    HeapObj::Array(a) if !a.iter().any(|x| x.is_hole()) => Some(Kind::Tuple(a)),
                    _ => None,
                }
            } else if cls == kinds.frozenset {
                Some(Kind::Frozen(rec_field(heap, idx, "map")?))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// A frozenset's elements, from its storage: today the runtime's bucket
/// Map (`Map<keyOf, Array<element>>`). The wiring stage adds the PyTable
/// storage arm.
fn members(heap: &Heap, storage: Value) -> Option<Vec<Value>> {
    if !storage.is_heap() {
        return None;
    }
    let HeapObj::Map { vals, .. } = heap.get(storage.heap_index()) else {
        return None;
    };
    let mut out = Vec::new();
    for b in vals.iter().filter(|b| !b.is_hole()) {
        if !b.is_heap() {
            return None;
        }
        let HeapObj::Array(items) = heap.get(b.heap_index()) else {
            return None;
        };
        out.extend(items.iter().copied().filter(|x| !x.is_hole()));
    }
    Some(out)
}

/// `-1` is reserved: it becomes `-2`.
fn fix(h: i64) -> i64 {
    if h == -1 {
        -2
    } else {
        h
    }
}

/// `hashBigInt` of an i128: the value modulo 2^61 - 1, sign kept.
pub(crate) fn hash_i128(n: i128) -> i64 {
    if n.unsigned_abs() < MODULUS as u128 {
        // Every int an i64 holds but the largest: no 128-bit division (a
        // libcall on wasm32).
        return fix(n as i64);
    }
    let m = (n.unsigned_abs() % MODULUS as u128) as i64;
    fix(if n < 0 { -m } else { m })
}

/// `hashBigInt` of an int beyond i128.
pub(crate) fn hash_big(b: &NumBig) -> i64 {
    let (sign, digits) = b.to_u64_digits();
    // 2^64 = 8 * 2^61 = 8 (mod 2^61 - 1).
    let mut r: u128 = 0;
    for &d in digits.iter().rev() {
        r = (r * 8 + d as u128) % MODULUS as u128;
    }
    let m = r as i64;
    fix(if sign == num_bigint::Sign::Minus { -m } else { m })
}

/// `hashFloat`: CPython's, with NaN hashing 0 (the runtime's rule).
pub(crate) fn hash_f64(x: f64) -> i64 {
    if x.is_nan() || x == 0.0 {
        return 0;
    }
    if x.is_infinite() {
        return if x > 0.0 { INF_HASH } else { -INF_HASH };
    }
    let bits = x.to_bits();
    let e = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1 << 52) - 1);
    let (mant, exp) = if e == 0 { (frac, -1074) } else { (frac | (1 << 52), e - 1075) };
    // x = mant * 2^exp, and 2^61 = 1 (mod 2^61 - 1).
    let m = (((mant as u128) << exp.rem_euclid(61)) % MODULUS as u128) as i64;
    fix(if x < 0.0 { -m } else { m })
}

/// The runtime's `strHashOf` over UTF-16 code units: two 32-bit lanes,
/// mixed into a non-negative 53-bit value.
pub(crate) fn hash_str_units(units: impl Iterator<Item = u16>) -> i64 {
    let (mut h1, mut h2): (i32, i32) = (5381, 0x6a09_e667);
    for c in units {
        let c = c as i32;
        h1 = h1.wrapping_shl(5).wrapping_add(h1).wrapping_add(c);
        h2 = h2.wrapping_shl(7).wrapping_sub(h2) ^ c;
    }
    h1 = (h1 ^ ((h1 as u32) >> 15) as i32).wrapping_mul(0x2c1b_3c6d);
    h2 = (h2 ^ ((h2 as u32) >> 13) as i32).wrapping_mul(0x297a_2d39);
    let hi = ((h2 as u32) >> 11) as i64;
    let lo = (h1 ^ ((h1 as u32) >> 16) as i32) as u32 as i64;
    hi * 4_294_967_296 + lo
}

/// The UTF-16 code units of WTF-8 text.
fn units_of(bytes: &[u8]) -> impl Iterator<Item = u16> + '_ {
    let mut i = 0;
    let mut low: Option<u16> = None;
    std::iter::from_fn(move || {
        if let Some(u) = low.take() {
            return Some(u);
        }
        if i >= bytes.len() {
            return None;
        }
        let (cp, n) = wtf8_decode(bytes, i);
        i += n;
        if cp < 0x10000 {
            return Some(cp as u16);
        }
        let v = cp - 0x10000;
        low = Some(0xDC00 | (v & 0x3FF) as u16);
        Some(0xD800 | (v >> 10) as u16)
    })
}

/// `strHash` of the string at `idx`.
fn hash_str(heap: &Heap, idx: u32, steps: &mut u64) -> Option<i64> {
    let bytes = heap.str_wtf8_cow(idx)?;
    *steps += 1 + bytes.len() as u64 / 16;
    Some(hash_str_units(units_of(&bytes)))
}

/// CPython's tuple hash (xxHash-based), over the items' hashes.
fn hash_tuple(lanes: &[i64]) -> i64 {
    const P1: u64 = 11_400_714_785_074_694_791;
    const P2: u64 = 14_029_467_366_897_019_727;
    const P5: u64 = 2_870_177_450_012_600_261;
    let mut acc = P5;
    for &lane in lanes {
        acc = acc.wrapping_add((lane as u64).wrapping_mul(P2));
        acc = acc.rotate_left(31);
        acc = acc.wrapping_mul(P1);
    }
    acc = acc.wrapping_add(lanes.len() as u64 ^ (P5 ^ 3_527_539));
    if acc == u64::MAX {
        1_546_275_796
    } else {
        acc as i64
    }
}

/// Python's `hash(v)` as the runtime computes it, for a key needing no
/// guest code; `None` for any other value (the runtime hashes it).
pub(crate) fn native_hash(heap: &Heap, kinds: &PyKinds, v: Value, steps: &mut u64) -> Option<i64> {
    hash_in(heap, kinds, v, 0, steps)
}

fn hash_in(heap: &Heap, kinds: &PyKinds, v: Value, depth: u32, steps: &mut u64) -> Option<i64> {
    *steps += 1;
    Some(match classify(heap, kinds, v)? {
        Kind::Float(x) => hash_f64(x),
        Kind::Int(n) => hash_i128(n),
        Kind::Big(b) => hash_big(b),
        Kind::Str(idx) => hash_str(heap, idx, steps)?,
        Kind::None => NONE_HASH,
        Kind::Tuple(items) => {
            if depth >= MAX_DEPTH {
                return None;
            }
            let mut lanes = Vec::with_capacity(items.len());
            for &x in items {
                lanes.push(hash_in(heap, kinds, x, depth + 1, steps)?);
            }
            hash_tuple(&lanes)
        }
        Kind::Frozen(_) => {
            let mut text = Vec::new();
            key_text(heap, kinds, v, depth, &mut text, steps)?;
            hash_str_units(text.into_iter())
        }
    })
}

/// The decimal digits of `n`, as UTF-16 units.
fn push_ascii(out: &mut Vec<u16>, s: &str) {
    out.extend(s.bytes().map(u16::from));
}

/// The runtime's `keyStr(v)` (the text of `v`'s bucket key, which composes
/// tuple and frozenset keys and hashes), as UTF-16 units.
fn key_text(heap: &Heap, kinds: &PyKinds, v: Value, depth: u32, out: &mut Vec<u16>, steps: &mut u64) -> Option<()> {
    *steps += 1;
    if out.len() > (1 << 24) || depth > MAX_DEPTH {
        return None;
    }
    match classify(heap, kinds, v)? {
        Kind::Float(x) if x.is_nan() => out.push(u16::from(b'f')),
        Kind::Float(x) if x.is_finite() && x.fract() == 0.0 && x.abs() > SAFE as f64 => {
            push_ascii(out, "n");
            push_ascii(out, &hash_f64(x).to_string());
        }
        Kind::Float(x) => {
            let mut s = String::from("n");
            super::helpers_num2::fmt_f64_into(&mut s, x);
            push_ascii(out, &s);
        }
        Kind::Int(n) => {
            push_ascii(out, "n");
            let shown = if (-SAFE..=SAFE).contains(&n) { n as i64 } else { hash_i128(n) };
            push_ascii(out, &shown.to_string());
        }
        Kind::Big(b) => {
            push_ascii(out, "n");
            push_ascii(out, &hash_big(b).to_string());
        }
        Kind::Str(idx) => {
            let bytes = heap.str_wtf8_cow(idx)?;
            *steps += bytes.len() as u64 / 16;
            out.push(u16::from(b's'));
            out.extend(units_of(&bytes));
        }
        Kind::None => out.push(u16::from(b'N')),
        Kind::Tuple(items) => {
            push_ascii(out, "t(");
            for &x in items {
                key_text(heap, kinds, x, depth + 1, out, steps)?;
                out.push(u16::from(b','));
            }
            out.push(u16::from(b')'));
        }
        Kind::Frozen(storage) => {
            let members = members(heap, storage)?;
            if members.len() > MAX_FROZEN_TEXTS {
                return None;
            }
            let mut texts = Vec::new();
            for x in members {
                let mut t = Vec::new();
                key_text(heap, kinds, x, depth + 1, &mut t, steps)?;
                texts.push(t);
            }
            // JS's default sort: by UTF-16 code units. Insertion sort keeps
            // the module to one sort instantiation (see `sort_positions`).
            for i in 1..texts.len() {
                let mut j = i;
                while j > 0 && texts[j - 1] > texts[j] {
                    texts.swap(j - 1, j);
                    j -= 1;
                    *steps += 1;
                }
            }
            *steps += bulk_cost(texts.len());
            push_ascii(out, "F{");
            for (i, t) in texts.iter().enumerate() {
                if i > 0 {
                    out.push(u16::from(b','));
                }
                out.extend_from_slice(t);
            }
            out.push(u16::from(b'}'));
        }
    }
    Some(())
}

/// Whether the runtime files `a` and `b` (native keys) in one bucket: their
/// `keyOf` keys agree, which is their `keyStr` texts agreeing.
pub(crate) fn same_bucket(heap: &Heap, kinds: &PyKinds, a: Value, b: Value, steps: &mut u64) -> Option<bool> {
    let (mut x, mut y) = (Vec::new(), Vec::new());
    key_text(heap, kinds, a, 0, &mut x, steps)?;
    key_text(heap, kinds, b, 0, &mut y, steps)?;
    Some(x == y)
}

/// The runtime's container equality (`eq`: identity first, then `==`) of
/// two native keys; `None` when either needs guest code. A NaN never
/// equals anything, itself included (unboxed floats have no identity: the
/// runtime's documented rule).
pub(crate) fn native_eq(heap: &Heap, kinds: &PyKinds, a: Value, b: Value, steps: &mut u64) -> Option<bool> {
    eq_in(heap, kinds, a, b, 0, steps)
}

fn eq_in(heap: &Heap, kinds: &PyKinds, a: Value, b: Value, depth: u32, steps: &mut u64) -> Option<bool> {
    *steps += 1;
    if a == b && !a.is_number() {
        return Some(true);
    }
    if depth > MAX_DEPTH {
        return None;
    }
    let (x, y) = (classify(heap, kinds, a)?, classify(heap, kinds, b)?);
    Some(match (x, y) {
        (Kind::Float(p), Kind::Float(q)) => p == q,
        (Kind::Int(p), Kind::Int(q)) => p == q,
        (Kind::Int(n), Kind::Float(f)) | (Kind::Float(f), Kind::Int(n)) => {
            BigVal::Small(n).cmp_f64(f) == Some(Ordering::Equal)
        }
        (Kind::Big(p), Kind::Big(q)) => p == q,
        (Kind::Big(p), Kind::Float(f)) | (Kind::Float(f), Kind::Big(p)) => {
            super::bigint::cmp_big_f64(p, f) == Some(Ordering::Equal)
        }
        (Kind::Str(p), Kind::Str(q)) => heap.str_eq(p, q),
        (Kind::None, Kind::None) => true,
        (Kind::Tuple(p), Kind::Tuple(q)) => {
            if p.len() != q.len() {
                return Some(false);
            }
            for (&s, &t) in p.iter().zip(q) {
                if !eq_in(heap, kinds, s, t, depth + 1, steps)? {
                    return Some(false);
                }
            }
            true
        }
        (Kind::Frozen(p), Kind::Frozen(q)) => {
            let (p, q) = (members(heap, p)?, members(heap, q)?);
            if p.len() != q.len() {
                return Some(false);
            }
            let mut t = PyTable::new_set();
            for &e in &q {
                native_add(heap, kinds, &mut t, e, steps).ok()?;
            }
            for &e in &p {
                if native_lookup(heap, kinds, &t, e, steps)?.0.is_none() {
                    return Some(false);
                }
            }
            true
        }
        _ => false,
    })
}

/// Look up a native key: (its position if present, its hash); `None`
/// when the key or a same-hash stored key needs guest code.
pub(crate) fn native_lookup(
    heap: &Heap,
    kinds: &PyKinds,
    t: &PyTable,
    key: Value,
    steps: &mut u64,
) -> Option<(Option<usize>, i64)> {
    let h = native_hash(heap, kinds, key, steps)?;
    let mut work = 0;
    let found = t.lookup(h, &mut |k| native_eq(heap, kinds, k, key, &mut work), steps);
    *steps += work;
    match found? {
        Lookup::Found(i) => Some((Some(i), h)),
        Lookup::Absent(_) => Some((None, h)),
    }
}

/// Look up `key` of hash `h` natively, and insert it (with `val`) when
/// absent: `Ok((position, inserted))`.
fn add_hashed(
    heap: &Heap,
    kinds: &PyKinds,
    t: &mut PyTable,
    h: i64,
    key: Value,
    val: Value,
    steps: &mut u64,
) -> Result<(usize, bool), Stop> {
    let mut work = 0;
    let mut same: Vec<usize> = Vec::new();
    let found = t.walk(
        h,
        &mut |i| {
            let r = native_eq(heap, kinds, t.keys[i], key, &mut work);
            if r == Some(false) {
                same.push(i);
            }
            r
        },
        steps,
    );
    *steps += work;
    let hint = match found.ok_or(Stop::Guest(0))? {
        Lookup::Found(i) => return Ok((i, false)),
        Lookup::Absent(hint) => hint,
    };
    let mut mate = None;
    if t.set {
        // Any member of the bucket will do: they share its stamp.
        for &i in &same {
            let Some(k) = t.key_at(i) else { continue };
            if same_bucket(heap, kinds, k, key, steps).ok_or(Stop::Guest(0))? {
                mate = Some(i);
                break;
            }
        }
    }
    match t.insert(hint, h, key, val, mate, steps) {
        Ok(i) => Ok((i, true)),
        Err(InsertError::Full) => Err(Stop::Full),
        Err(InsertError::Stale) => unreachable!("nothing ran between the lookup and the insert"),
    }
}

/// `setAdd` / a dict's key insert for a native key: `Ok((position,
/// inserted))`; a present key is left as it is (the caller replaces a
/// dict's value with [`PyTable::set_value`]). `Stop::Guest(0)` when guest
/// code is needed.
pub(crate) fn native_add(heap: &Heap, kinds: &PyKinds, t: &mut PyTable, key: Value, steps: &mut u64) -> Result<(usize, bool), Stop> {
    let h = native_hash(heap, kinds, key, steps).ok_or(Stop::Guest(0))?;
    add_hashed(heap, kinds, t, h, key, Value::UNDEFINED, steps)
}

/// `dictSet` for a native key.
pub(crate) fn native_set_item(
    heap: &Heap,
    kinds: &PyKinds,
    t: &mut PyTable,
    key: Value,
    val: Value,
    steps: &mut u64,
) -> Result<usize, Stop> {
    let h = native_hash(heap, kinds, key, steps).ok_or(Stop::Guest(0))?;
    let (i, inserted) = add_hashed(heap, kinds, t, h, key, val, steps)?;
    if !inserted {
        t.set_value(i, val);
    }
    Ok(i)
}

// ---- bulk operations ----------------------------------------------------------------------------------

/// `dict.update(src)` / `R.dictmerge` into `dst` (detached from the heap):
/// each of `src`'s entries in order, set natively. Stops before the first
/// entry needing guest code (`Stop::Guest(n)`: `n` entries applied; the
/// runtime continues from there with the same result).
pub(crate) fn update_from(heap: &Heap, kinds: &PyKinds, dst: &mut PyTable, src: &PyTable, steps: &mut u64) -> Result<(), Stop> {
    *steps += bulk_cost(src.len());
    for (n, (i, k, v)) in src.entries().enumerate() {
        let (at, inserted) = match add_hashed(heap, kinds, dst, src.hashes[i], k, v, steps) {
            Ok(r) => r,
            Err(Stop::Guest(_)) => return Err(Stop::Guest(n)),
            Err(e) => return Err(e),
        };
        if !inserted {
            dst.set_value(at, v);
        }
    }
    Ok(())
}

/// `dictEq`: same size, and every key of `a` in `b` with an equal value.
pub(crate) fn dict_eq(heap: &Heap, kinds: &PyKinds, a: &PyTable, b: &PyTable, steps: &mut u64) -> Option<bool> {
    if a.len() != b.len() {
        return Some(false);
    }
    *steps += bulk_cost(a.len());
    for (i, k, v) in a.entries() {
        let mut work = 0;
        let found = b.lookup(a.hashes[i], &mut |s| native_eq(heap, kinds, s, k, &mut work), steps)?;
        *steps += work;
        let Lookup::Found(j) = found else {
            return Some(false);
        };
        if !native_eq(heap, kinds, v, b.vals[j], steps)? {
            return Some(false);
        }
    }
    Some(true)
}

/// Whether `a`'s element at position `i` is in `b`.
fn contains_entry(heap: &Heap, kinds: &PyKinds, a: &PyTable, i: usize, b: &PyTable, steps: &mut u64) -> Option<bool> {
    let k = a.keys[i];
    let mut work = 0;
    let found = b.lookup(a.hashes[i], &mut |s| native_eq(heap, kinds, s, k, &mut work), steps);
    *steps += work;
    Some(matches!(found?, Lookup::Found(_)))
}

/// Set algebra, as the runtime's `setBinop` builds a new set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetOp {
    Union,
    Intersection,
    Difference,
    SymmetricDifference,
}

/// `a op b` for two sets (or frozensets) of native keys, element order as
/// the runtime's: `|` copies `a` then adds `b`'s; `&` walks the smaller
/// operand (the right one on a tie) keeping its elements; `-` keeps `a`'s;
/// `^` is `a - b` then `b - a`, each operand walked in its visible order.
/// `None` when guest code is needed.
pub(crate) fn set_op(heap: &Heap, kinds: &PyKinds, op: SetOp, a: &PyTable, b: &PyTable, steps: &mut u64) -> Option<PyTable> {
    *steps += bulk_cost(a.len() + b.len());
    let mut out = match op {
        SetOp::Union => a.copy(heap, steps),
        _ => PyTable::new_set(),
    };
    let keep = |out: &mut PyTable, src: &PyTable, other: &PyTable, want: bool, steps: &mut u64| -> Option<()> {
        for i in src.visible_order(heap) {
            if contains_entry(heap, kinds, src, i, other, steps)? == want {
                add_hashed(heap, kinds, out, src.hashes[i], src.keys[i], Value::UNDEFINED, steps).ok()?;
            }
        }
        Some(())
    };
    match op {
        SetOp::Union => {
            for i in b.visible_order(heap) {
                add_hashed(heap, kinds, &mut out, b.hashes[i], b.keys[i], Value::UNDEFINED, steps).ok()?;
            }
        }
        SetOp::Intersection => {
            let (walk, probe) = if b.len() > a.len() { (a, b) } else { (b, a) };
            keep(&mut out, walk, probe, true, steps)?;
        }
        SetOp::Difference => keep(&mut out, a, b, false, steps)?,
        SetOp::SymmetricDifference => {
            keep(&mut out, a, b, false, steps)?;
            keep(&mut out, b, a, false, steps)?;
        }
    }
    Some(out)
}

/// `a |= b` / `set.update(b)` into `a` (detached from the heap), resumable
/// like [`update_from`]: `Stop::Guest(n)` after `n` of `b`'s elements (in
/// its visible order) were added.
pub(crate) fn union_into(heap: &Heap, kinds: &PyKinds, a: &mut PyTable, b: &PyTable, steps: &mut u64) -> Result<(), Stop> {
    *steps += bulk_cost(b.len());
    for (n, i) in b.visible_order(heap).into_iter().enumerate() {
        match add_hashed(heap, kinds, a, b.hashes[i], b.keys[i], Value::UNDEFINED, steps) {
            Ok(_) => {}
            Err(Stop::Guest(_)) => return Err(Stop::Guest(n)),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// `a <= b`: every element of `a` is in `b`.
pub(crate) fn is_subset(heap: &Heap, kinds: &PyKinds, a: &PyTable, b: &PyTable, steps: &mut u64) -> Option<bool> {
    if a.len() > b.len() {
        return Some(false);
    }
    *steps += bulk_cost(a.len());
    for (i, _, _) in a.entries() {
        if !contains_entry(heap, kinds, a, i, b, steps)? {
            return Some(false);
        }
    }
    Some(true)
}

/// `setEq`.
pub(crate) fn set_eq(heap: &Heap, kinds: &PyKinds, a: &PyTable, b: &PyTable, steps: &mut u64) -> Option<bool> {
    if a.len() != b.len() {
        return Some(false);
    }
    is_subset(heap, kinds, a, b, steps)
}

/// Reaches every entry point, for measuring the module's compiled size
/// (built with `RUSTFLAGS="--cfg pytable_size_probe"` and called from a
/// temporary export, so that link-time optimization keeps the module).
#[cfg(pytable_size_probe)]
pub(crate) fn size_probe(x: u64) -> u64 {
    let heap = Heap::new();
    let kinds = PyKinds {
        tuple: Value::from_bits(x),
        frozenset: Value::from_bits(x ^ 1),
    };
    let mut steps = 0;
    let (mut a, mut b) = (PyTable::new_set(), PyTable::new_dict());
    let v = Value::from_bits(x.rotate_left(7));
    let _ = native_add(&heap, &kinds, &mut a, v, &mut steps);
    let _ = native_set_item(&heap, &kinds, &mut b, v, v, &mut steps);
    let c = b.clone();
    let _ = update_from(&heap, &kinds, &mut b, &c, &mut steps);
    let a2 = a.clone();
    let _ = union_into(&heap, &kinds, &mut a, &a2, &mut steps);
    let mut acc = steps;
    for op in [SetOp::Union, SetOp::Intersection, SetOp::Difference, SetOp::SymmetricDifference] {
        acc += set_op(&heap, &kinds, op, &a, &a, &mut steps).map_or(0, |t| t.len() as u64);
    }
    acc += set_eq(&heap, &kinds, &a, &a, &mut steps).unwrap_or(false) as u64;
    acc += dict_eq(&heap, &kinds, &b, &c, &mut steps).unwrap_or(false) as u64;
    acc += native_hash(&heap, &kinds, v, &mut steps).unwrap_or(0) as u64;
    acc += same_bucket(&heap, &kinds, v, v, &mut steps).unwrap_or(false) as u64;
    match a.probe(x as i64, v, &mut steps) {
        Probe::Hit(i) => acc += i as u64,
        Probe::Candidates(c, h) => {
            acc += c.len() as u64;
            acc += a.insert(h, x as i64, v, v, c.first().copied(), &mut steps).unwrap_or(0) as u64;
        }
        Probe::Miss(h) => acc += a.insert(h, x as i64, v, v, None, &mut steps).unwrap_or(0) as u64,
    }
    acc += a.first_visible(&heap).unwrap_or(0) as u64;
    acc += a.at(0, x as u32).map_or(0, |(k, _)| k.bits());
    acc += b.pop_last().map_or(0, |(k, _)| k.bits());
    acc += a.remove_at(x as usize).map_or(0, |(k, _)| k.bits());
    acc += b.copy(&heap, &mut steps).payload_bytes() as u64;
    b.clear();
    acc + steps + b.version() as u64
}

#[cfg(test)]
#[path = "py_table_tests.rs"]
mod tests;
