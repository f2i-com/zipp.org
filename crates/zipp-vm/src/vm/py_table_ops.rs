//! The Python runtime's dict and set storage, native (`__zipp_py_table`).
//!
//! The runtime's dict and set records are `{cls, map, size}`: `map` is a
//! [`PyTable`] heap object, and `size` its length, which the runtime
//! (`runtime/types.js`, the only code touching the storage) writes back
//! after every change (the engine's `len()` and dict-iterator fast paths
//! read it). `__zipp_py_table(op, ...)`, bound only in a Python program:
//!
//! | op | arguments | answer |
//! |----|-----------|--------|
//! | `NEW` 0 | `set, T.tuple, T.frozenset` | a new empty table |
//! | `GET` 1 | `t, k` | the value (`true` in a set), or `undefined` |
//! | `SET` 2 | `t, k, v` | the new length, or `-1` when full ([`MAX_ITEMS`]) |
//! | `POP` 3 | `t, k` | the removed value (`true`), or `undefined` |
//! | `CLEAR` 4 | `t` | `0` (the table itself when it is not one) |
//! | `COPY` 5 | `t` | a compacted copy (`undefined`: over the budget) |
//! | `KEYS` 6 / `VALUES` 7 / `ENTRIES` 8 | `t` | an Array, in iteration order (`[k, v]` Arrays for `ENTRIES`) |
//! | `POPITEM` 9 | `t, first` | the last (first) entry `[k, v]`, removed, or `undefined` |
//! | `SETPOP` 10 | `t` | a set's first element, removed, or `undefined` |
//! | `PROBE` 11 | `t, h, k` | `p >= 0` (`k` itself is at `p`), `-1 - hint` (no entry has hash `h`) or `[hint, p, key, ...]` (the entries of hash `h`) |
//! | `VALAT` 12 | `t, p, key` | the value (`true`) at `p` |
//! | `SETAT` 13 | `t, p, key, v` | the length, with `v` stored at `p` |
//! | `DELAT` 14 | `t, p, key` | the value (`true`) at `p`, removed |
//! | `INSERT` 15 | `t, hint, h, k, v, mate` | the new length, or `-1` when full |
//! | `HASH` 16 | `k, T.tuple, T.frozenset` | `hash(k)`, or `undefined` for a key needing guest code |
//! | `UPDATE` 17 | `t, src` | `dict.update` / `set.update`: the new length, `-1` when full, or `-2 - n` after `n` of `src`'s entries (the runtime does the rest) |
//! | `EQ` 18 | `a, b` | `a == b`, or `undefined` |
//! | `SETOP` 19 | `a, b, op` | `a \| b`, `a & b`, `a - b`, `a ^ b` (`op` 0..3) as a new table, or `undefined` |
//! | `SUBSET` 20 | `a, b` | `a <= b`, or `undefined` |
//! | `FILL` 21 | `t, keys, values` | [`SET`](ops::SET) each pair (`values` `undefined` for a set): as `UPDATE` |
//! | `SIZE` 22 | `t` | the length |
//!
//! An op that needs guest code, whether for the key (a `__hash__` or
//! `__eq__` of the program's) or for a stored key it meets, answers the
//! table itself, which is never a Python value (`undefined` for the bulk
//! ops, which the runtime then does entry by entry). The runtime then takes
//! the split protocol: it computes the hash (`hashInt`), `PROBE` reports an
//! identity hit, the same-hash candidates or a miss, the runtime runs `eq()`
//! on the candidates in turn and reads, writes or removes the winner by
//! position (`VALAT`, `SETAT`, `DELAT`: refused, with the table as the
//! answer, once the slot no longer holds that key, when the runtime starts
//! over), or after a miss `INSERT`s with the probe's hint (refused once the
//! table changed since). Natives never call guest code. A storage that is
//! not a table (a live `__dict__` view keeps the instance's attribute Map)
//! is answered the same way.

use super::*;
use crate::heap::HeapObj;
use crate::value::Value;
use crate::vm::{Thrown, Vm};

/// The op codes (see the module comment).
pub(crate) mod ops {
    pub(crate) const NEW: i32 = 0;
    pub(crate) const GET: i32 = 1;
    pub(crate) const SET: i32 = 2;
    pub(crate) const POP: i32 = 3;
    pub(crate) const CLEAR: i32 = 4;
    pub(crate) const COPY: i32 = 5;
    pub(crate) const KEYS: i32 = 6;
    pub(crate) const VALUES: i32 = 7;
    pub(crate) const ENTRIES: i32 = 8;
    pub(crate) const POPITEM: i32 = 9;
    pub(crate) const SETPOP: i32 = 10;
    pub(crate) const PROBE: i32 = 11;
    pub(crate) const VALAT: i32 = 12;
    pub(crate) const SETAT: i32 = 13;
    pub(crate) const DELAT: i32 = 14;
    pub(crate) const INSERT: i32 = 15;
    pub(crate) const HASH: i32 = 16;
    pub(crate) const UPDATE: i32 = 17;
    pub(crate) const EQ: i32 = 18;
    pub(crate) const SETOP: i32 = 19;
    pub(crate) const SUBSET: i32 = 20;
    pub(crate) const FILL: i32 = 21;
    pub(crate) const SIZE: i32 = 22;
}

/// A count as a JS number.
fn count(n: usize) -> Value {
    match i32::try_from(n) {
        Ok(i) => Value::int(i),
        Err(_) => Value::num(n as f64),
    }
}

/// The table at `t`, when `t` is one.
fn table_at(heap: &Heap, t: Value) -> Option<&PyTable> {
    if !t.is_heap() {
        return None;
    }
    match heap.get(t.heap_index()) {
        HeapObj::PyTable(b) => Some(b),
        _ => None,
    }
}

/// A position argument.
fn pos_arg(v: Value) -> Option<usize> {
    if v.is_int() {
        return usize::try_from(v.as_int()).ok();
    }
    let f = v.as_f64();
    (v.is_number() && f >= 0.0 && f.fract() == 0.0 && f < 4_294_967_296.0).then_some(f as usize)
}

/// Where a bulk op stopped, as its answer.
fn stopped(r: Result<(), Stop>, len: usize) -> Value {
    match r {
        Ok(()) => count(len),
        Err(Stop::Full) => Value::int(-1),
        Err(Stop::Guest(n)) => Value::num(-2.0 - n as f64),
    }
}

impl<'p> Vm<'p> {
    /// `__zipp_py_table(op, ...)`: see the module comment.
    pub(crate) fn py_table(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        let op = arg(0);
        if !op.is_int() {
            return Ok(Value::UNDEFINED);
        }
        let t = arg(1);
        Ok(match op.as_int() {
            ops::GET => match self.py_table_get(t, arg(2)) {
                Some(Some(v)) => v,
                Some(None) => Value::UNDEFINED,
                None => t,
            },
            ops::SET => match self.py_table_set(t, arg(2), arg(3)) {
                Some(Ok(n)) => count(n),
                Some(Err(())) => Value::int(-1),
                None => t,
            },
            ops::POP => match self.py_table_pop(t, arg(2)) {
                Some(Some(v)) => v,
                Some(None) => Value::UNDEFINED,
                None => t,
            },
            ops::NEW => {
                let kinds = PyKinds {
                    tuple: arg(2),
                    frozenset: arg(3),
                };
                let table = PyTable::with_kinds(t == Value::TRUE, kinds);
                Value::heap(self.heap.alloc(HeapObj::PyTable(Box::new(table))))
            }
            ops::CLEAR => match table_at(&self.heap, t) {
                Some(_) => {
                    if let HeapObj::PyTable(b) = self.heap.get_mut(t.heap_index()) {
                        b.clear();
                    }
                    Value::int(0)
                }
                None => t,
            },
            ops::SIZE => table_at(&self.heap, t).map_or(Value::UNDEFINED, |b| count(b.len())),
            ops::COPY => self.pt_copy(t),
            ops::KEYS | ops::VALUES | ops::ENTRIES => self.pt_list(t, op.as_int()),
            ops::POPITEM => self.pt_popitem(t, arg(2) == Value::TRUE),
            ops::SETPOP => self.pt_setpop(t),
            ops::PROBE => self.pt_probe(t, arg(2), arg(3)).unwrap_or(t),
            ops::VALAT | ops::SETAT | ops::DELAT => self.pt_at(op.as_int(), t, arg(2), arg(3), arg(4)).unwrap_or(t),
            ops::INSERT => self.pt_insert(t, arg(2), arg(3), arg(4), arg(5), arg(6)).unwrap_or(t),
            ops::HASH => {
                let kinds = PyKinds {
                    tuple: arg(2),
                    frozenset: arg(3),
                };
                let mut steps = 0;
                let h = native_hash(&self.heap, &kinds, t, &mut steps);
                self.charge_steps(steps as i64);
                h.map_or(Value::UNDEFINED, |h| self.make_bigint(i128::from(h)))
            }
            ops::UPDATE => self.pt_update(t, arg(2)),
            ops::EQ | ops::SUBSET => self.pt_compare(op.as_int(), t, arg(2)),
            ops::SETOP => self.pt_setop(t, arg(2), arg(3)),
            ops::FILL => self.pt_fill(t, arg(2), arg(3)),
            _ => Value::UNDEFINED,
        })
    }

    /// Detach the table at `idx` from its heap slot (an empty table stays
    /// there): an operation then reads the heap and writes the table. It
    /// goes back with [`Vm::pt_put`] before anything else can run.
    fn pt_take(&mut self, idx: u32) -> PyTable {
        match self.heap.get_mut(idx) {
            HeapObj::PyTable(b) => std::mem::take(&mut **b),
            _ => PyTable::default(),
        }
    }

    fn pt_put(&mut self, idx: u32, t: PyTable) {
        if let HeapObj::PyTable(b) = self.heap.get_mut(idx) {
            **b = t;
        }
    }

    /// A string key flat before it is stored (hashing and comparing a flat
    /// string are cheaper; every stored str is flat).
    fn pt_flat_key(&mut self, k: Value) {
        if k.is_heap() {
            self.heap.flatten(k.heap_index());
        }
    }

    /// `d[k]` / `k in s` of a table `storage` for a key of a native kind:
    /// `Some(Some(value))` (`true` for a set), `Some(None)` when absent,
    /// `None` when `storage` is not a table or guest code is needed. The
    /// fused dict ops (`PyDictLookup`, `PyGetItem`) call this with a dict
    /// record's `map`.
    #[inline(never)]
    pub(crate) fn py_table_get(&mut self, storage: Value, key: Value) -> Option<Option<Value>> {
        let mut steps = 0;
        let found = {
            let t = table_at(&self.heap, storage)?;
            let (at, _) = native_lookup(&self.heap, &t.kinds, t, key, &mut steps)?;
            at.map(|i| if t.is_set() { Value::TRUE } else { t.value_at(i).unwrap_or(Value::UNDEFINED) })
        };
        self.charge_steps(steps as i64);
        Some(found)
    }

    /// `d[k] = v` / `s.add(k)` of a table `storage` for a key of a native
    /// kind: `Some(Ok(new length))`, `Some(Err(()))` when the table is full
    /// (the runtime raises MemoryError), `None` when `storage` is not a
    /// table or guest code is needed (nothing changed). The caller writes
    /// the length into the record's `size`. The fused `PySetItem` calls
    /// this with a dict record's `map`.
    #[inline(never)]
    pub(crate) fn py_table_set(&mut self, storage: Value, key: Value, val: Value) -> Option<Result<usize, ()>> {
        table_at(&self.heap, storage)?;
        let idx = storage.heap_index();
        self.pt_flat_key(key);
        let mut t = self.pt_take(idx);
        let mut steps = 0;
        let kinds = t.kinds;
        let r = if t.is_set() {
            native_add(&self.heap, &kinds, &mut t, key, &mut steps).map(|_| ())
        } else {
            native_set_item(&self.heap, &kinds, &mut t, key, val, &mut steps).map(|_| ())
        };
        let len = t.len();
        self.pt_put(idx, t);
        self.charge_steps(steps as i64);
        match r {
            Ok(()) => {
                self.heap.write_barrier_val(idx, val);
                self.store_barrier(crate::heap::gcoracle::COLL_INSERT, idx, key);
                Some(Ok(len))
            }
            Err(Stop::Full) => Some(Err(())),
            Err(Stop::Guest(_)) => None,
        }
    }

    /// The length of a table `storage` (`None`: not a table). For the fused
    /// `PyLen`, which today reads the record's `size` instead.
    #[allow(dead_code)] // for the fused `PyLen` (see the report of stage S2)
    pub(crate) fn py_table_len(&self, storage: Value) -> Option<usize> {
        table_at(&self.heap, storage).map(PyTable::len)
    }

    /// `d.pop(k)` / `s.discard(k)` for a key of a native kind: as
    /// [`Vm::py_table_get`], the entry removed.
    pub(crate) fn py_table_pop(&mut self, storage: Value, key: Value) -> Option<Option<Value>> {
        let mut steps = 0;
        let at = {
            let t = table_at(&self.heap, storage)?;
            native_lookup(&self.heap, &t.kinds, t, key, &mut steps)?.0
        };
        self.charge_steps(steps as i64);
        let Some(i) = at else {
            return Some(None);
        };
        match self.heap.get_mut(storage.heap_index()) {
            HeapObj::PyTable(t) => {
                let set = t.is_set();
                let (_, v) = t.remove_at(i)?;
                Some(Some(if set { Value::TRUE } else { v }))
            }
            _ => None,
        }
    }

    fn pt_copy(&mut self, t: Value) -> Value {
        let Some(n) = table_at(&self.heap, t).map(PyTable::len) else {
            return Value::UNDEFINED;
        };
        if !self.native_kernel_admits(bulk_cost(n), 0) {
            return Value::UNDEFINED;
        }
        let mut steps = 0;
        let copy = match table_at(&self.heap, t) {
            Some(src) => src.copy(&self.heap, &mut steps),
            None => return Value::UNDEFINED,
        };
        self.charge_steps(steps as i64);
        Value::heap(self.heap.alloc(HeapObj::PyTable(Box::new(copy))))
    }

    /// `KEYS`, `VALUES` or `ENTRIES`: an Array snapshot.
    fn pt_list(&mut self, t: Value, op: i32) -> Value {
        let Some(tb) = table_at(&self.heap, t) else {
            return t;
        };
        let order = tb.visible_order(&self.heap);
        let pairs: Vec<(Value, Value)> = order
            .iter()
            .map(|&i| (tb.key_at(i).unwrap_or(Value::UNDEFINED), tb.value_at(i).unwrap_or(Value::UNDEFINED)))
            .collect();
        self.charge_steps(bulk_cost(pairs.len()) as i64);
        let items: Vec<Value> = match op {
            ops::KEYS => pairs.into_iter().map(|(k, _)| k).collect(),
            ops::VALUES => pairs.into_iter().map(|(_, v)| v).collect(),
            _ => pairs
                .into_iter()
                .map(|(k, v)| Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))))
                .collect(),
        };
        Value::heap(self.heap.alloc(HeapObj::Array(items)))
    }

    fn pt_popitem(&mut self, t: Value, first: bool) -> Value {
        if table_at(&self.heap, t).is_none() {
            return t;
        }
        let entry = match self.heap.get_mut(t.heap_index()) {
            HeapObj::PyTable(b) => match if first { b.next_live(0) } else { b.slots().checked_sub(1) } {
                Some(i) => b.remove_at(i),
                None => None,
            },
            _ => None,
        };
        match entry {
            Some((k, v)) => Value::heap(self.heap.alloc(HeapObj::Array(vec![k, v]))),
            None => Value::UNDEFINED,
        }
    }

    fn pt_setpop(&mut self, t: Value) -> Value {
        let Some(at) = table_at(&self.heap, t).map(|b| b.first_visible(&self.heap)) else {
            return t;
        };
        let Some(i) = at else {
            return Value::UNDEFINED;
        };
        match self.heap.get_mut(t.heap_index()) {
            HeapObj::PyTable(b) => b.remove_at(i).map_or(Value::UNDEFINED, |(k, _)| k),
            _ => Value::UNDEFINED,
        }
    }

    /// A hash from the runtime: an int (a BigInt, or a Number).
    fn pt_hash_arg(&self, h: Value) -> Option<i64> {
        if h.is_int() {
            return Some(i64::from(h.as_int()));
        }
        if h.is_number() {
            let f = h.as_f64();
            return (f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0).then_some(f as i64);
        }
        if let Some(n) = h.small_bigint_val() {
            return Some(n);
        }
        if !h.is_heap() {
            return None;
        }
        match self.heap.get(h.heap_index()) {
            HeapObj::BigInt(n) => i64::try_from(*n).ok(),
            _ => None,
        }
    }

    fn pt_probe(&mut self, t: Value, h: Value, key: Value) -> Option<Value> {
        let h = self.pt_hash_arg(h)?;
        let mut steps = 0;
        let (probe, keys) = {
            let tb = table_at(&self.heap, t)?;
            let p = tb.probe(h, key, &mut steps);
            let keys: Vec<Value> = match &p {
                Probe::Candidates(c, _) => c.iter().map(|&i| tb.key_at(i).unwrap_or(Value::UNDEFINED)).collect(),
                _ => Vec::new(),
            };
            (p, keys)
        };
        self.charge_steps(steps as i64);
        Some(match probe {
            Probe::Hit(i) => count(i),
            Probe::Miss(hint) => Value::num(-1.0 - f64::from(hint.version())),
            Probe::Candidates(c, hint) => {
                let mut out = Vec::with_capacity(1 + 2 * c.len());
                out.push(Value::num(f64::from(hint.version())));
                for (i, k) in c.into_iter().zip(keys) {
                    out.push(count(i));
                    out.push(k);
                }
                Value::heap(self.heap.alloc(HeapObj::Array(out)))
            }
        })
    }

    /// `VALAT`, `SETAT` or `DELAT` at position `p`, while it holds `key`.
    fn pt_at(&mut self, op: i32, t: Value, p: Value, key: Value, val: Value) -> Option<Value> {
        let p = pos_arg(p)?;
        let (set, len) = {
            let tb = table_at(&self.heap, t)?;
            if tb.key_at(p)?.bits() != key.bits() {
                return None;
            }
            (tb.is_set(), tb.len())
        };
        let idx = t.heap_index();
        let HeapObj::PyTable(tb) = self.heap.get_mut(idx) else {
            return None;
        };
        match op {
            ops::VALAT => Some(if set { Value::TRUE } else { tb.value_at(p)? }),
            ops::SETAT => {
                tb.set_value(p, val);
                self.heap.write_barrier_val(idx, val);
                Some(count(len))
            }
            _ => {
                let (_, v) = tb.remove_at(p)?;
                Some(if set { Value::TRUE } else { v })
            }
        }
    }

    fn pt_insert(&mut self, t: Value, hint: Value, h: Value, key: Value, val: Value, mate: Value) -> Option<Value> {
        table_at(&self.heap, t)?;
        let version = u32::try_from(pos_arg(hint)?).ok()?;
        let h = self.pt_hash_arg(h)?;
        let mate = pos_arg(mate);
        self.pt_flat_key(key);
        let idx = t.heap_index();
        let mut steps = 0;
        let r = match self.heap.get_mut(idx) {
            HeapObj::PyTable(tb) => tb.insert(Hint::at(version), h, key, val, mate, &mut steps),
            _ => return None,
        };
        self.charge_steps(steps as i64);
        match r {
            Ok(_) => {
                self.heap.write_barrier_val(idx, val);
                self.store_barrier(crate::heap::gcoracle::COLL_INSERT, idx, key);
                table_at(&self.heap, t).map(|b| count(b.len()))
            }
            Err(InsertError::Full) => Some(Value::int(-1)),
            Err(InsertError::Stale) => None,
        }
    }

    /// `UPDATE`: `src`'s entries into `t` (both tables of one kind).
    fn pt_update(&mut self, t: Value, src: Value) -> Value {
        let (Some(a), Some(b)) = (table_at(&self.heap, t), table_at(&self.heap, src)) else {
            return Value::num(-2.0);
        };
        if a.is_set() != b.is_set() || !self.native_kernel_admits(bulk_cost(b.len()), 0) {
            return Value::num(-2.0);
        }
        let idx = t.heap_index();
        let mut dst = self.pt_take(idx);
        let mut steps = 0;
        let kinds = dst.kinds;
        // `src` is `t` itself: the detached table leaves an empty one in its
        // slot, and updating a table with itself changes nothing.
        let r = match table_at(&self.heap, src) {
            Some(s) if dst.is_set() => union_into(&self.heap, &kinds, &mut dst, s, &mut steps),
            Some(s) => update_from(&self.heap, &kinds, &mut dst, s, &mut steps),
            None => Ok(()),
        };
        let len = dst.len();
        self.pt_put(idx, dst);
        self.heap.write_barrier(idx);
        self.charge_steps(steps as i64);
        stopped(r, len)
    }

    /// `EQ` or `SUBSET` of two tables of one kind.
    fn pt_compare(&mut self, op: i32, a: Value, b: Value) -> Value {
        let (Some(x), Some(y)) = (table_at(&self.heap, a), table_at(&self.heap, b)) else {
            return Value::UNDEFINED;
        };
        if x.is_set() != y.is_set() || !self.native_kernel_admits(bulk_cost(x.len()), 0) {
            return Value::UNDEFINED;
        }
        let mut steps = 0;
        let kinds = x.kinds;
        let r = match (op, x.is_set()) {
            (ops::EQ, false) => dict_eq(&self.heap, &kinds, x, y, &mut steps),
            (ops::EQ, true) => set_eq(&self.heap, &kinds, x, y, &mut steps),
            (_, true) => is_subset(&self.heap, &kinds, x, y, &mut steps),
            _ => None,
        };
        self.charge_steps(steps as i64);
        r.map_or(Value::UNDEFINED, Value::bool)
    }

    fn pt_setop(&mut self, a: Value, b: Value, op: Value) -> Value {
        let op = match op {
            v if v == Value::int(0) => SetOp::Union,
            v if v == Value::int(1) => SetOp::Intersection,
            v if v == Value::int(2) => SetOp::Difference,
            v if v == Value::int(3) => SetOp::SymmetricDifference,
            _ => return Value::UNDEFINED,
        };
        let (Some(x), Some(y)) = (table_at(&self.heap, a), table_at(&self.heap, b)) else {
            return Value::UNDEFINED;
        };
        if !x.is_set() || !y.is_set() || !self.native_kernel_admits(bulk_cost(x.len() + y.len()), 0) {
            return Value::UNDEFINED;
        }
        let mut steps = 0;
        let out = set_op(&self.heap, &x.kinds, op, x, y, &mut steps);
        self.charge_steps(steps as i64);
        match out {
            Some(t) => Value::heap(self.heap.alloc(HeapObj::PyTable(Box::new(t)))),
            None => Value::UNDEFINED,
        }
    }

    /// `FILL`: each `keys[i]` set to `values[i]` (a set: added).
    fn pt_fill(&mut self, t: Value, keys: Value, values: Value) -> Value {
        let array = |heap: &Heap, v: Value| -> Option<Vec<Value>> {
            match v.is_heap().then(|| heap.get(v.heap_index())) {
                Some(HeapObj::Array(a)) => Some(a.clone()),
                _ => None,
            }
        };
        let Some(ks) = array(&self.heap, keys) else {
            return Value::num(-2.0);
        };
        let vs = array(&self.heap, values).unwrap_or_default();
        if table_at(&self.heap, t).is_none() || !self.native_kernel_admits(bulk_cost(ks.len()), 0) {
            return Value::num(-2.0);
        }
        for &k in &ks {
            self.pt_flat_key(k);
        }
        let idx = t.heap_index();
        let mut tb = self.pt_take(idx);
        let kinds = tb.kinds;
        let mut steps = 0;
        let mut r = Ok(());
        for (n, &k) in ks.iter().enumerate() {
            let v = vs.get(n).copied().unwrap_or(Value::UNDEFINED);
            let one = if k.is_hole() || v.is_hole() {
                Err(Stop::Guest(0))
            } else if tb.is_set() {
                native_add(&self.heap, &kinds, &mut tb, k, &mut steps).map(|_| ())
            } else {
                native_set_item(&self.heap, &kinds, &mut tb, k, v, &mut steps).map(|_| ())
            };
            if let Err(e) = one {
                r = Err(match e {
                    Stop::Guest(_) => Stop::Guest(n),
                    Stop::Full => Stop::Full,
                });
                break;
            }
        }
        let len = tb.len();
        self.pt_put(idx, tb);
        self.heap.write_barrier(idx);
        self.charge_steps(steps as i64);
        stopped(r, len)
    }
}
