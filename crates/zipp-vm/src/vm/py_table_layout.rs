//! Python instance attribute storage in layout mode ("hidden classes"),
//! stage S3 of the native-core plan.
//!
//! An instance record is `{cls, dict}`; its `dict` is a
//! `HeapObj::PyAttrs { layout, vals }` while its keys are strs added one at
//! a time and never deleted. The keys then live once per VM, in the layout
//! tree ([`Layouts`], on the runtime registry `vm::py_rt`): a layout is a
//! node, its parent the layout one key shorter, the edge the key, so a
//! layout names an exact sequence of keys and `vals[i]` is the value of the
//! layout's `i`th key. Two instances with one layout have their attributes
//! at the same slots, which is what the attribute instructions' caches
//! (`vm::py_attr`) key on, and "this instance has no attribute `n`" is one
//! compare against a layout proven to lack `n`.
//!
//! Anything a layout cannot describe (a deleted key, a key that is not a
//! str, a tree past its bounds) turns the storage into a dict-mode
//! `HeapObj::PyTable` in place ([`Vm::py_attrs_materialize`]), for good;
//! the heap slot, and so the instance's identity and `obj.__dict__`, stay.
//!
//! The runtime's JavaScript treats an instance's `dict` as a `Map` (`get`,
//! `set`, `has`, `delete`, `size`, `forEach`, iteration): both storages
//! answer `Map.prototype`'s methods ([`Vm::py_store_map_method`]).
//!
//! ## The JIT contract (`Heap::hot_mirror`)
//!
//! A `PyAttrs` slot's mirror record is `shape = LAYOUT_BASE | layout`,
//! `fid = FID_MIRROR_NONE`, `vals = vals.as_ptr()`. Layout ids are below
//! `LAYOUT_BASE` and JS shape ids never reach it, so a mirror shape equal to
//! `LAYOUT_BASE | L` proves the slot holds attribute storage of layout `L`.
//! Every change that could move `vals` or change the layout (an append, a
//! `clear`, the turn into a `PyTable`) rewrites the mirror before the
//! instruction making it completes; a replaced value never moves `vals`.
//! An inline attribute read of a site cached as `(class, class ver, L,
//! slot)` is then:
//!
//! 1. guard the instance's mirror shape is the instance template's shape
//!    (`PyRt::inst_shape`), load `dict = inst.vals[1]` (its `cls` is
//!    `vals[0]`: guard its bits and the class stamp as the interpreter does);
//! 2. guard `mirror[dict].shape == LAYOUT_BASE | L`;
//! 3. load `*(mirror[dict].vals + slot * 8)`.
//!
//! A store at an existing slot is the same with a store (and the value's
//! write barrier). An append is not inlined: it may reallocate.

use super::{native_set_item, PyKinds, PyTable};
use crate::heap::HeapObj;
use crate::value::Value;
use crate::vm::{Thrown, Vm};

/// A mirror shape at or above this is a layout (see the module comment).
pub(crate) const LAYOUT_BASE: u32 = 0x4000_0000;
/// The layout of no keys.
pub(crate) const ROOT: u32 = 1;
/// Most layouts a VM makes; an instance past it turns into a dict.
const LAYOUT_MAX: usize = 1 << 14;
/// Most keys a layout holds.
const DEPTH_MAX: u32 = 64;
/// Longest key (bytes) a layout takes.
const KEY_MAX: usize = 128;

struct Node {
    parent: u32,
    /// The canonical key (a flat str, kept alive through the registry's
    /// `pins`).
    key: Value,
    len: u32,
    kids: Vec<(Value, u32)>,
}

/// The VM's layout tree (see the module comment).
pub(crate) struct Layouts {
    nodes: Vec<Node>,
    /// Per class (by bits: a hint only), the most attributes an instance
    /// grew to, for presizing the next instance's storage.
    presize: rustc_hash::FxHashMap<u64, u8>,
}

impl Default for Layouts {
    fn default() -> Layouts {
        let none = Node {
            parent: 0,
            key: Value::UNDEFINED,
            len: 0,
            kids: Vec::new(),
        };
        let root = Node {
            parent: 0,
            key: Value::UNDEFINED,
            len: 0,
            kids: Vec::new(),
        };
        Layouts {
            nodes: vec![none, root],
            presize: rustc_hash::FxHashMap::default(),
        }
    }
}

impl Layouts {
    /// The number of keys of layout `l`.
    pub(crate) fn len(&self, l: u32) -> usize {
        self.nodes.get(l as usize).map_or(0, |n| n.len as usize)
    }

    /// Every canonical key the tree holds (the registry keeps them alive
    /// through its `pins`).
    pub(crate) fn keys(&self) -> impl Iterator<Item = Value> + '_ {
        self.nodes.iter().map(|n| n.key).filter(|k| k.is_heap())
    }

    pub(crate) fn presize(&self, cls: Value) -> usize {
        self.presize.get(&cls.bits()).map_or(0, |&n| n as usize)
    }

    pub(crate) fn note_size(&mut self, cls: Value, n: usize) {
        let n = n.min(DEPTH_MAX as usize) as u8;
        let e = self.presize.entry(cls.bits()).or_insert(0);
        if *e < n {
            *e = n;
        }
    }
}

/// Whether the heap strs `a` and `b` are equal (by bits, else content).
#[inline]
fn same_key(heap: &crate::heap::Heap, a: Value, b: Value) -> bool {
    a.bits() == b.bits() || (a.is_heap() && b.is_heap() && heap.str_eq(a.heap_index(), b.heap_index()))
}

impl<'p> Vm<'p> {
    fn py_layouts(&self) -> Option<&Layouts> {
        self.py_rt.as_deref().map(|p| &p.layouts)
    }

    /// A str key usable in a layout: a flat str short enough.
    fn py_layout_key(&mut self, k: Value) -> bool {
        if !k.is_heap() || !self.heap.is_str_like(k.heap_index()) {
            return false;
        }
        self.heap.flatten(k.heap_index());
        matches!(self.heap.get(k.heap_index()), HeapObj::Str(s) if s.as_bytes().len() <= KEY_MAX)
    }

    /// The slot of the key equal to `k` in layout `l`.
    pub(crate) fn py_layout_find(&self, l: u32, k: Value) -> Option<usize> {
        let t = self.py_layouts()?;
        if !k.is_heap() || !self.heap.is_str_like(k.heap_index()) {
            return None;
        }
        let mut n = l;
        while n > ROOT {
            let node = t.nodes.get(n as usize)?;
            if same_key(&self.heap, node.key, k) {
                return Some(node.len as usize - 1);
            }
            n = node.parent;
        }
        None
    }

    /// The keys of layout `l`, in order.
    pub(crate) fn py_layout_keys(&self, l: u32) -> Vec<Value> {
        let Some(t) = self.py_layouts() else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(t.len(l));
        let mut n = l;
        while n > ROOT {
            let Some(node) = t.nodes.get(n as usize) else {
                break;
            };
            out.push(node.key);
            n = node.parent;
        }
        out.reverse();
        out
    }

    /// The layout `l` plus the key `k` (absent from `l`), made if need be;
    /// `None` past the tree's bounds or for a key a layout does not take.
    pub(crate) fn py_layout_child(&mut self, l: u32, k: Value) -> Option<u32> {
        if !self.py_layout_key(k) {
            return None;
        }
        {
            let t = self.py_layouts()?;
            let node = t.nodes.get(l as usize)?;
            if let Some(&(_, c)) = node.kids.iter().find(|&&(key, _)| key.bits() == k.bits()) {
                return Some(c);
            }
            if let Some(&(_, c)) = node.kids.iter().find(|&&(key, _)| same_key(&self.heap, key, k)) {
                return Some(c);
            }
            if l == 0 || node.len >= DEPTH_MAX || t.nodes.len() >= LAYOUT_MAX {
                return None;
            }
        }
        // A new edge: `k` becomes the canonical key, kept alive by `pins`.
        let p = self.py_rt.as_deref()?;
        let pins = p.pins;
        if !pins.is_heap() || !matches!(self.heap.get(pins.heap_index()), HeapObj::Array(_)) {
            return None;
        }
        self.heap.write_barrier_val(pins.heap_index(), k);
        if let HeapObj::Array(a) = self.heap.get_mut(pins.heap_index()) {
            a.push(k);
        }
        let t = &mut self.py_rt.as_deref_mut()?.layouts;
        let id = t.nodes.len() as u32;
        let len = t.nodes[l as usize].len + 1;
        t.nodes.push(Node {
            parent: l,
            key: k,
            len,
            kids: Vec::new(),
        });
        t.nodes[l as usize].kids.push((k, id));
        Some(id)
    }

    /// New, empty attribute storage with room for `cap` values: layout
    /// mode once the runtime is bound, a dict-mode table before.
    pub(crate) fn py_attrs_new(&mut self, cap: usize) -> Value {
        let obj = if self.py_rt.is_some() {
            HeapObj::PyAttrs {
                layout: ROOT,
                vals: Vec::with_capacity(cap),
            }
        } else {
            HeapObj::PyTable(Box::new(PyTable::with_kinds(false, PyKinds::default())))
        };
        Value::heap(self.heap.alloc(obj))
    }

    /// `(layout, number of values)` of layout-mode storage `d`.
    #[inline]
    pub(crate) fn py_attrs_of(&self, d: Value) -> Option<(u32, usize)> {
        if !d.is_heap() {
            return None;
        }
        match self.heap.get(d.heap_index()) {
            HeapObj::PyAttrs { layout, vals } => Some((*layout, vals.len())),
            _ => None,
        }
    }

    /// The value at `slot` of layout-mode storage `d` of layout `l`.
    #[inline]
    pub(crate) fn py_attrs_at(&self, d: Value, l: u32, slot: usize) -> Option<Value> {
        match self.heap.get(d.heap_index()) {
            HeapObj::PyAttrs { layout, vals } if *layout == l => vals.get(slot).copied(),
            _ => None,
        }
    }

    /// `d[k]` of layout-mode storage: `Some(None)` when absent.
    pub(crate) fn py_attrs_get(&self, d: Value, k: Value) -> Option<Option<Value>> {
        let (l, _) = self.py_attrs_of(d)?;
        Some(self.py_layout_find(l, k).and_then(|s| self.py_attrs_at(d, l, s)))
    }

    /// Store `v` at `slot` of `idx` (layout-mode storage).
    #[inline]
    pub(crate) fn py_attrs_put(&mut self, idx: u32, slot: usize, v: Value) {
        self.heap.write_barrier_val(idx, v);
        if let HeapObj::PyAttrs { vals, .. } = self.heap.get_mut(idx) {
            if let Some(x) = vals.get_mut(slot) {
                *x = v;
            }
        }
    }

    /// Append `v` to `idx` (layout-mode storage of `from`, `slot` values)
    /// under the layout `to`, and republish its mirror.
    #[inline]
    pub(crate) fn py_attrs_push(&mut self, idx: u32, to: u32, v: Value) {
        self.heap.write_barrier_val(idx, v);
        if let HeapObj::PyAttrs { layout, vals } = self.heap.get_mut(idx) {
            vals.push(v);
            *layout = to;
        }
        self.heap.refresh_mirror(idx);
    }

    /// `d[k] = v` in layout mode: replaced, or appended through the layout
    /// tree. `false` (nothing done) when a layout cannot hold `k`.
    pub(crate) fn py_attrs_set(&mut self, d: Value, k: Value, v: Value) -> bool {
        let Some((l, _)) = self.py_attrs_of(d) else {
            return false;
        };
        if let Some(s) = self.py_layout_find(l, k) {
            self.py_attrs_put(d.heap_index(), s, v);
            return true;
        }
        match self.py_layout_child(l, k) {
            Some(to) => {
                self.py_attrs_push(d.heap_index(), to, v);
                true
            }
            None => false,
        }
    }

    /// Turn layout-mode storage `d` into a dict-mode table in place (its
    /// entries in order); a table (or anything else) is left as it is.
    pub(crate) fn py_attrs_materialize(&mut self, d: Value) {
        let Some((l, _)) = self.py_attrs_of(d) else {
            return;
        };
        let keys = self.py_layout_keys(l);
        let idx = d.heap_index();
        let vals = match self.heap.get_mut(idx) {
            HeapObj::PyAttrs { vals, .. } => std::mem::take(vals),
            _ => return,
        };
        let kinds = PyKinds::default();
        let mut t = PyTable::with_kinds(false, kinds);
        let mut steps = 0;
        for (&k, &v) in keys.iter().zip(vals.iter()) {
            // Distinct strs: never refused.
            let _ = native_set_item(&self.heap, &kinds, &mut t, k, v, &mut steps);
        }
        self.charge_steps(steps as i64);
        *self.heap.get_mut(idx) = HeapObj::PyTable(Box::new(t));
        self.heap.write_barrier(idx);
        self.heap.refresh_mirror(idx);
    }

    /// The entries of layout-mode storage `d`, in order.
    pub(crate) fn py_attrs_entries(&self, d: Value) -> Vec<(Value, Value)> {
        let Some((l, _)) = self.py_attrs_of(d) else {
            return Vec::new();
        };
        let keys = self.py_layout_keys(l);
        match self.heap.get(d.heap_index()) {
            HeapObj::PyAttrs { vals, .. } => keys.into_iter().zip(vals.iter().copied()).collect(),
            _ => Vec::new(),
        }
    }

    /// The number of entries of attribute or table storage `idx`.
    pub(crate) fn py_store_len(&self, idx: u32) -> Option<usize> {
        match self.heap.get(idx) {
            HeapObj::PyAttrs { vals, .. } => Some(vals.len()),
            HeapObj::PyTable(t) => Some(t.len()),
            _ => None,
        }
    }

    /// The entries of attribute or table storage `idx`, in order.
    fn py_store_entries(&self, idx: u32) -> Vec<(Value, Value)> {
        match self.heap.get(idx) {
            HeapObj::PyAttrs { .. } => self.py_attrs_entries(Value::heap(idx)),
            HeapObj::PyTable(t) => t.entries().map(|(_, k, v)| (k, v)).collect(),
            _ => Vec::new(),
        }
    }

    /// The first entry of attribute or table storage `idx` at or after
    /// position `from`: its position, key and value.
    fn py_store_entry_from(&self, idx: u32, from: usize) -> Option<(usize, Value, Value)> {
        match self.heap.get(idx) {
            HeapObj::PyAttrs { layout, vals } => {
                let v = *vals.get(from)?;
                let k = *self.py_layout_keys(*layout).get(from)?;
                Some((from, k, v))
            }
            HeapObj::PyTable(t) => {
                let at = t.next_live(from)?;
                Some((at, t.key_at(at)?, t.value_at(at)?))
            }
            _ => None,
        }
    }

    /// `Map.prototype.<name>` called on attribute or table storage (the
    /// runtime's JavaScript reads an instance's `dict` as a `Map`). Keys are
    /// Python keys: the runtime passes strs. A key needing guest code is a
    /// TypeError here (the runtime never passes one).
    pub(crate) fn py_store_map_method(&mut self, idx: u32, name: &str, args: &[Value]) -> Result<Option<Value>, Thrown> {
        let recv = Value::heap(idx);
        let a0 = args.first().copied().unwrap_or(Value::UNDEFINED);
        let attrs = matches!(self.heap.get(idx), HeapObj::PyAttrs { .. });
        let lookup = |vm: &mut Self, k: Value| -> Option<Value> {
            if attrs {
                vm.py_attrs_get(recv, k).flatten()
            } else {
                vm.py_table_get(recv, k).flatten()
            }
        };
        match name {
            "get" => Ok(Some(lookup(self, a0).unwrap_or(Value::UNDEFINED))),
            "has" => Ok(Some(Value::bool(lookup(self, a0).is_some()))),
            "set" => {
                let v = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                if attrs {
                    if self.py_attrs_set(recv, a0, v) {
                        return Ok(Some(recv));
                    }
                    self.py_attrs_materialize(recv);
                }
                match self.py_table_set(recv, a0, v) {
                    Some(Ok(_)) => Ok(Some(recv)),
                    Some(Err(())) => Err(Thrown("MemoryError: dict limit exceeded".into())),
                    None => Err(Thrown("TypeError: unsupported attribute storage key".into())),
                }
            }
            "delete" => {
                if lookup(self, a0).is_none() {
                    return Ok(Some(Value::FALSE));
                }
                self.py_attrs_materialize(recv);
                Ok(Some(Value::bool(self.py_table_pop(recv, a0).flatten().is_some())))
            }
            "clear" => {
                match self.heap.get_mut(idx) {
                    HeapObj::PyAttrs { layout, vals } => {
                        *layout = ROOT;
                        vals.clear();
                    }
                    HeapObj::PyTable(t) => t.clear(),
                    _ => {}
                }
                self.heap.refresh_mirror(idx);
                Ok(Some(Value::UNDEFINED))
            }
            "forEach" => {
                if !self.is_callable(a0) {
                    return Err(Thrown("TypeError: Map.prototype.forEach callback is not a function".into()));
                }
                let this_arg = args.get(1).copied().unwrap_or(Value::UNDEFINED);
                // Live, as `Map.prototype.forEach` walks: each step reads the
                // entry at the next position now (a callback may change the
                // storage, and nothing read earlier is held across a call).
                let mut i = 0;
                let mut steps = 0u64;
                while let Some((at, k, v)) = self.py_store_entry_from(idx, i) {
                    i = at + 1;
                    steps += 1;
                    self.preflight_native_iteration_work(steps)?;
                    self.call_value(a0, this_arg, &[v, k, recv])?;
                }
                Ok(Some(Value::UNDEFINED))
            }
            "keys" | "values" | "entries" => {
                let entries = self.py_store_entries(idx);
                let items: Vec<Value> = match name {
                    "keys" => entries.into_iter().map(|(k, _)| k).collect(),
                    "values" => entries.into_iter().map(|(_, v)| v).collect(),
                    _ => entries
                        .into_iter()
                        .map(|(k, v)| self.alloc_array_current_realm(vec![k, v]))
                        .collect(),
                };
                let proto = self.native_home(self.map_iter_proto);
                let it = self.heap.alloc(HeapObj::Iterator {
                    items,
                    index: 0,
                    proto,
                    live: None,
                });
                Ok(Some(Value::heap(it)))
            }
            _ => Ok(None),
        }
    }
}
