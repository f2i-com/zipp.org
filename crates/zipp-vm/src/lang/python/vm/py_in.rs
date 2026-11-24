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
//! * an exact dict or set: its key test (`dictGet(d, needle) !== undefined`,
//!   `setHas`) for a plain needle, through its table (`vm::py_table`).
//!
//! `eq` on two plain values is `===`, then an int against a float or bool by
//! value (a float never equals another float that is not `===`, so a NaN
//! finds nothing), which is what [`Vm::py_prim_eq`] computes.

use super::*;
use crate::heap::HeapObj;
use crate::value::Value;
use super::py_rt::hint;


/// A plain value as `eq` sees it.
#[derive(Clone, Copy)]
enum Prim {
    Float(f64),
    Int(i128),
    Bool(bool),
    Str(u32),
    None,
}

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
        if let Some(n) = v.small_bigint_val() {
            return Some(Prim::Int(n as i128));
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
        if self.heap.is_str_like(ci) {
            let p = self.py_prim(needle)?;
            let Prim::Str(ni) = p else {
                return None;
            };
            self.heap.flatten(ci);
            let (HeapObj::Str(hay), HeapObj::Str(n)) = (self.heap.get(ci), self.heap.get(ni)) else {
                return None;
            };
            if !hay.is_wellformed() || !n.is_wellformed() {
                return None;
            }
            let (h, n) = (hay.as_bytes(), n.as_bytes());
            // The engine's own substring search (`String.prototype.includes`),
            // linear in both lengths, so the charge covers the work.
            let cost = 4 + (h.len() as u64 + n.len() as u64) / 8;
            if !self.native_kernel_admits(cost, 0) {
                return None;
            }
            let found = if n.len() == 1 {
                memchr::memchr(n[0], h).is_some()
            } else {
                Self::find_bytes(h, n).is_some()
            };
            self.charge_steps(cost as i64);
            return Some(found);
        }
        let (tl, tt, td, ts, tfs, seq_shape, dict_shape, map_slot) = {
            let p = self.py_rt_for(rt)?;
            (
                p.t_list.bits(),
                p.t_tuple.bits(),
                p.t_dict.bits(),
                p.t_set.bits(),
                p.t_frozenset.bits(),
                p.seq_shape,
                p.dict_shape,
                p.dict_slots[0],
            )
        };
        // Fast single-pass shape dispatch for template-shaped sequences and dicts/sets.
        let mut fast_items: Option<Value> = None;
        let mut fast_map: Option<Value> = None;
        let cls_bits = if let HeapObj::Object(m) = self.heap.get(ci) {
            let shape = m.shape();
            if !m.is_ctor && shape != crate::shape::DICT {
                if shape == seq_shape {
                    let cb = m.val_at(0).bits();
                    if cb == tl || cb == tt {
                        fast_items = Some(m.val_at(1));
                    }
                    cb
                } else if shape == dict_shape {
                    let cb = m.val_at(0).bits();
                    if cb == td || cb == ts || cb == tfs {
                        fast_map = Some(m.val_at(map_slot));
                    }
                    cb
                } else {
                    self.py_cls_of(Value::heap(ci))?.bits()
                }
            } else {
                self.py_cls_of(Value::heap(ci))?.bits()
            }
        } else {
            return None;
        };
        // Exact dict, set, or frozenset: `py_table_get` natively handles all
        // native keys (including tuples, `NavigableString`, `NamedTuple`, types).
        if cls_bits == td || cls_bits == ts || cls_bits == tfs {
            if !self.native_kernel_admits(8, 0) {
                return None;
            }
            let map = match fast_map {
                Some(m) => m,
                None => self.py_hint_field(hint::MAP, ci, "map")?,
            };
            let found = Some(self.py_table_get(map, needle)?.is_some());
            self.charge_steps(8);
            return found;
        }
        if cls_bits == tl || cls_bits == tt {
            let p = self.py_prim(needle)?;
            let items = match fast_items {
                Some(v) => v,
                None => self.py_seq_parts(Value::heap(ci))?.1,
            };
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
        None
    }

    /// The scan of a list's or tuple's `n` items for [`Vm::py_in`].
    fn py_in_items(&mut self, items: Value, n: usize, p: Prim) -> Option<bool> {
        let arr_idx = items.heap_index();
        // Fast immutable single-borrow scan for string needles against flat
        // strings/primitives (the dominant `tag in ("a", "b", ...)` workload):
        // checks Value identity, then byte length, then bytes with no heap
        // re-indexing per element. Falls back on Cons ropes or object items.
        if let Prim::Str(ni) = p {
            let needle_bits = Value::heap(ni).bits();
            let mut fallback_at = None;
            if let (HeapObj::Array(vals), HeapObj::Str(ns)) =
                (self.heap.get(arr_idx), self.heap.get(ni))
            {
                let nb = ns.as_bytes();
                let nlen = nb.len();
                for (i, &item) in vals.iter().take(n).enumerate() {
                    if item.bits() == needle_bits {
                        return Some(true);
                    }
                    if item == Value::HOLE {
                        return None;
                    }
                    if item.is_heap() {
                        match self.heap.get(item.heap_index()) {
                            HeapObj::Str(s) => {
                                let sb = s.as_bytes();
                                if sb.len() == nlen && sb == nb {
                                    return Some(true);
                                }
                            }
                            HeapObj::BigInt(_) => {}
                            HeapObj::Cons { .. } => {
                                fallback_at = Some(i);
                                break;
                            }
                            _ => return None,
                        }
                    }
                }
                if fallback_at.is_none() {
                    return Some(false);
                }
            }
            if let Some(start) = fallback_at {
                for i in start..n {
                    let item = match self.heap.get(arr_idx) {
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
                return Some(false);
            }
        }
        for i in 0..n {
            let item = match self.heap.get(arr_idx) {
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
