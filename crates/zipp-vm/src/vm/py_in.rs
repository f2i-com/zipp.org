//! The Python runtime's membership test, native (`__zipp_py_in`).
//!
//! `__zipp_py_in(needle, container)`, called with the runtime's `R` as
//! `this`, answers `contains(container, needle)`
//! (`runtime/types.js`, what `x in c` asks) for the containers whose answer
//! needs nothing but plain values, and `undefined` for everything else, when
//! the emitter's code asks the runtime's `in` helper exactly as before:
//!
//! * a str in a str, both ASCII: `container.includes(needle)`;
//! * an exact list or tuple: `eq(item, needle)` item by item, first match
//!   wins; `needle` a plain value (int, float, bool, str, None) and every item
//!   looked at before the answer one too (an object item could run an
//!   `__eq__`, so the scan gives up there);
//! * an exact dict: its key test (`dictGet(d, needle) !== undefined`) for a
//!   plain needle, through the dict's own `map` (every key a str, or buckets
//!   keyed by `keyOf`, whose entries must hold plain keys);
//! * an exact set: its bucket test (`setHas`) likewise.
//!
//! `eq` on two plain values is `===`, then an int against a float or bool by
//! value (a float never equals another float that is not `===`, so a NaN
//! finds nothing), which is what [`Vm::py_prim_eq`] computes.

use super::*;
use crate::heap::HeapObj;
use crate::value::Value;
use std::sync::atomic::AtomicU16;

static HINT_CLS: AtomicU16 = AtomicU16::new(0);
static HINT_ITEMS: AtomicU16 = AtomicU16::new(1);
static HINT_MAP: AtomicU16 = AtomicU16::new(1);
static HINT_STR: AtomicU16 = AtomicU16::new(3);
static HINT_TLIST: AtomicU16 = AtomicU16::new(0);
static HINT_TTUPLE: AtomicU16 = AtomicU16::new(0);
static HINT_TDICT: AtomicU16 = AtomicU16::new(0);
static HINT_TSET: AtomicU16 = AtomicU16::new(0);

/// A plain value as `eq` sees it.
#[derive(Clone, Copy)]
enum Prim {
    Float(f64),
    Int(i128),
    Bool(bool),
    Str(u32),
    None,
}

/// `Number.MAX_SAFE_INTEGER`, the runtime's `SAFE`.
const SAFE: i128 = 9_007_199_254_740_991;

impl<'p> Vm<'p> {
    /// `v` as a plain value; `None` for an object (a big int beyond i128
    /// included, which keeps its own paths).
    fn py_prim(&mut self, v: Value) -> Option<Prim> {
        if v.is_number() {
            return Some(Prim::Float(v.as_f64()));
        }
        if v == Value::TRUE || v == Value::FALSE {
            return Some(Prim::Bool(v == Value::TRUE));
        }
        if v == Value::NULL {
            return Some(Prim::None);
        }
        if !v.is_heap() {
            return None;
        }
        let idx = v.heap_index();
        if self.heap.is_str_like(idx) {
            self.heap.flatten(idx);
            return matches!(self.heap.get(idx), HeapObj::Str(_)).then_some(Prim::Str(idx));
        }
        match self.heap.get(idx) {
            HeapObj::BigInt(n) => Some(Prim::Int(*n)),
            _ => None,
        }
    }

    /// The runtime's `eq(a, b)` for two plain values.
    fn py_prim_eq(&self, a: Prim, b: Prim) -> bool {
        match (a, b) {
            // `===`: a float only equals the same float (never a NaN).
            (Prim::Float(x), Prim::Float(y)) => x == y,
            (Prim::Int(x), Prim::Int(y)) => x == y,
            (Prim::Bool(x), Prim::Bool(y)) => x == y,
            (Prim::None, Prim::None) => true,
            (Prim::Str(x), Prim::Str(y)) => {
                x == y
                    || matches!((self.heap.get(x), self.heap.get(y)), (HeapObj::Str(s), HeapObj::Str(t)) if s.as_bytes() == t.as_bytes())
            }
            // An int against a float compares exactly; bools are 0 and 1.
            (Prim::Int(n), Prim::Float(f)) | (Prim::Float(f), Prim::Int(n)) => {
                super::bigint::BigVal::Small(n).cmp_f64(f) == Some(std::cmp::Ordering::Equal)
            }
            (Prim::Int(n), Prim::Bool(b)) | (Prim::Bool(b), Prim::Int(n)) => n == b as i128,
            (Prim::Float(f), Prim::Bool(b)) | (Prim::Bool(b), Prim::Float(f)) => f == b as i32 as f64,
            _ => false,
        }
    }

    /// `keyOf(v)` for a plain value, as a Map key the bucket maps hold;
    /// `None` when the runtime would key it by something else (a None, a
    /// NaN, an int beyond 2^53: its sentinel or hash string).
    fn py_prim_key(&mut self, p: Prim) -> Option<Value> {
        match p {
            Prim::Str(idx) => {
                // A NUL-led str is keyed by an escaped copy.
                match self.heap.get(idx) {
                    HeapObj::Str(s) if s.as_bytes().first() != Some(&0) => Some(Value::heap(idx)),
                    _ => None,
                }
            }
            Prim::Int(n) if (-SAFE..=SAFE).contains(&n) => Some(Value::num(n as f64)),
            Prim::Bool(b) => Some(Value::num(b as i32 as f64)),
            Prim::Float(f) if !f.is_nan() && !(f.fract() == 0.0 && (f > SAFE as f64 || f < -(SAFE as f64))) => {
                Some(Value::num(f))
            }
            _ => None,
        }
    }

    /// `__zipp_py_in(needle, container)` with `R` as `this`: see the module
    /// comment.
    #[inline(never)]
    pub(crate) fn py_in(&mut self, needle: Value, container: Value, rt: Value) -> Value {
        match self.py_in_inner(needle, container, rt) {
            Some(b) => Value::bool(b),
            None => Value::UNDEFINED,
        }
    }

    fn py_in_inner(&mut self, needle: Value, container: Value, rt: Value) -> Option<bool> {
        if !container.is_heap() || !rt.is_heap() {
            return None;
        }
        let ci = container.heap_index();
        let p = self.py_prim(needle)?;
        if self.heap.is_str_like(ci) {
            let Prim::Str(ni) = p else {
                return None;
            };
            self.heap.flatten(ci);
            let (HeapObj::Str(hay), HeapObj::Str(n)) = (self.heap.get(ci), self.heap.get(ni)) else {
                return None;
            };
            if !hay.is_ascii() || !n.is_ascii() {
                return None;
            }
            let (h, n) = (hay.as_bytes(), n.as_bytes());
            let cost = 4 + (h.len() as u64 / 8);
            if !self.native_kernel_admits(cost, 0) {
                return None;
            }
            let found = n.is_empty() || h.windows(n.len()).any(|w| w == n);
            self.charge_steps(cost as i64);
            return Some(found);
        }
        let cls = self.py_rec_field_in(ci, &HINT_CLS, "cls")?;
        let r = rt.heap_index();
        let is = |vm: &Self, hint: &AtomicU16, key: &str| vm.py_rec_field_in(r, hint, key).is_some_and(|t| t.bits() == cls.bits());
        if is(self, &HINT_TLIST, "TLIST") || is(self, &HINT_TTUPLE, "TTUPLE") {
            let items = self.py_rec_field_in(ci, &HINT_ITEMS, "items")?;
            if !items.is_heap() {
                return None;
            }
            let n = match self.heap.get(items.heap_index()) {
                HeapObj::Array(a) => a.len(),
                _ => return None,
            };
            // Metered as the runtime's scan would be: one step per item.
            if !self.native_kernel_admits(4 + n as u64, 0) {
                return None;
            }
            let found = self.py_in_items(items, n, p)?;
            self.charge_steps(4 + n as i64);
            return Some(found);
        }
        if !self.native_kernel_admits(8, 0) {
            return None;
        }
        let found = self.py_in_keyed(ci, cls, rt, needle, p);
        if found.is_some() {
            self.charge_steps(8);
        }
        found
    }

    /// The scan of a list's or tuple's `n` items for [`Vm::py_in`].
    fn py_in_items(&mut self, items: Value, n: usize, p: Prim) -> Option<bool> {
        {
            for i in 0..n {
                let item = match self.heap.get(items.heap_index()) {
                    HeapObj::Array(a) => *a.get(i)?,
                    _ => return None,
                };
                if item == Value::HOLE {
                    return None;
                }
                let q = self.py_prim(item)?;
                if self.py_prim_eq(q, p) {
                    return Some(true);
                }
            }
            Some(false)
        }
    }

    /// A dict's or set's key test for [`Vm::py_in`].
    fn py_in_keyed(&mut self, ci: u32, cls: Value, rt: Value, needle: Value, p: Prim) -> Option<bool> {
        let r = rt.heap_index();
        let is = |vm: &Self, hint: &AtomicU16, key: &str| vm.py_rec_field_in(r, hint, key).is_some_and(|t| t.bits() == cls.bits());
        let dict = is(self, &HINT_TDICT, "TDICT");
        if !dict && !is(self, &HINT_TSET, "TSET") {
            return None;
        }
        let map = self.py_rec_field_in(ci, &HINT_MAP, "map")?;
        if !map.is_heap() || !matches!(self.heap.get(map.heap_index()), HeapObj::Map { .. }) {
            return None;
        }
        if dict {
            let str_mode = self.py_rec_field_in(ci, &HINT_STR, "str")?;
            if str_mode == Value::TRUE {
                // Every key a str: a str is looked up, anything else (plain,
                // so hashable) is absent.
                return Some(match p {
                    Prim::Str(_) => self.coll_find(map.heap_index(), needle).is_some_and(|i| {
                        matches!(self.heap.get(map.heap_index()), HeapObj::Map { vals, .. } if vals.get(i).is_some_and(|v| !v.is_undefined()))
                    }),
                    _ => false,
                });
            }
            if str_mode != Value::FALSE {
                return None;
            }
        }
        let key = self.py_prim_key(p)?;
        let Some(i) = self.coll_find(map.heap_index(), key) else {
            return Some(false);
        };
        let bucket = match self.heap.get(map.heap_index()) {
            HeapObj::Map { vals, .. } => *vals.get(i)?,
            _ => return None,
        };
        if !bucket.is_heap() {
            return None;
        }
        let n = match self.heap.get(bucket.heap_index()) {
            HeapObj::Array(a) => a.len(),
            _ => return None,
        };
        for j in 0..n {
            // A dict's entry is `[key, value, seq]`; a set's is the value.
            let e = match self.heap.get(bucket.heap_index()) {
                HeapObj::Array(a) => *a.get(j)?,
                _ => return None,
            };
            let k = if dict {
                if !e.is_heap() {
                    return None;
                }
                match self.heap.get(e.heap_index()) {
                    HeapObj::Array(entry) => *entry.first()?,
                    _ => return None,
                }
            } else {
                e
            };
            if k == Value::HOLE {
                return None;
            }
            let q = self.py_prim(k)?;
            if self.py_prim_eq(q, p) {
                return Some(true);
            }
        }
        Some(false)
    }

    /// An own data property of the plain object `idx`, through `hint`.
    fn py_rec_field_in(&self, idx: u32, hint: &AtomicU16, key: &str) -> Option<Value> {
        use std::sync::atomic::Ordering;
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        if m.is_ctor {
            return None;
        }
        let h = hint.load(Ordering::Relaxed) as usize;
        let slot = if h < m.len() && m.key_at(h) == key {
            h
        } else {
            let slot = m.pos(key)?;
            if slot <= u16::MAX as usize {
                hint.store(slot as u16, Ordering::Relaxed);
            }
            slot
        };
        (!m.is_accessor_at(slot)).then(|| m.val_at(slot))
    }
}
