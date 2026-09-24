//! Per-site slot hints for the Python frontend's attribute instructions
//! (`Instr::PyGetAttr`, `PySetAttr`, `PyMethod`).
//!
//! Those instructions answer from the runtime's per-class tables (`ga`, `sa`,
//! `gm`: plain objects keyed by attribute name) and an instance's `dict` (a
//! `Map`). Finding the name in each is a scan. A site remembers where it found
//! the name last time, in the class table and in the dict (instances of one
//! class usually hold their attributes in the same order), and tries those
//! positions first. A position is only a hint: it is used when the key stored
//! there is the site's name, which makes it that name's entry (an object's own
//! keys and a Map's keys are distinct), so a stale or foreign hint (another
//! program, another thread: the table is shared) is merely a miss, after which
//! the instruction's ordinary path runs and records what it found.

use super::*;
use crate::heap::HeapObj;
use crate::value::Value;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};

/// Hint words, direct-mapped by site: the class-table slot in the low half,
/// the dict entry position in the high half (`NONE` for neither).
const SITES: usize = 8192;
const NONE: u32 = u32::MAX;
static SITE_HINTS: [AtomicU32; SITES] = [const { AtomicU32::new(NONE) }; SITES];

/// The record fields these paths read, by the slot they usually have.
static HINT_CLS: AtomicU16 = AtomicU16::new(0);
static HINT_DICT: AtomicU16 = AtomicU16::new(1);
static HINT_GA: AtomicU16 = AtomicU16::new(11);
static HINT_SA: AtomicU16 = AtomicU16::new(12);

#[inline]
fn site(func_id: u32, ip: usize) -> &'static AtomicU32 {
    let h = (func_id as usize).wrapping_mul(0x9E37_79B9) ^ ip;
    &SITE_HINTS[(h ^ (h >> 13)) & (SITES - 1)]
}

/// The class-table half of a hint word.
#[inline]
fn table_slot(hint: u32) -> usize {
    (hint & 0xFFFF) as usize
}

/// The dict half of a hint word.
#[inline]
fn dict_pos(hint: u32) -> usize {
    (hint >> 16) as usize
}

impl<'p> Vm<'p> {
    /// The name a string constant spells (`key` a constant-pool index).
    pub(super) fn py_const_name(&self, func_id: u32, key: u32) -> Option<&'p str> {
        let func = self.func(func_id as usize);
        let raw = *func.constants.get(key as usize)?;
        if !raw.is_heap() || raw.heap_index() & crate::vm::helpers_misc::STRING_CONST_BIT == 0 {
            return None;
        }
        func.string_constants
            .get((raw.heap_index() & !crate::vm::helpers_misc::STRING_CONST_BIT) as usize)
            .map(|s| s.as_str())
    }

    /// An own data property of the plain object `idx` at `slot`, when the
    /// key there is `name`.
    #[inline]
    fn py_own_at(&self, idx: u32, slot: usize, name: &str) -> Option<Value> {
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        (slot < m.len() && m.key_at(slot) == name && !m.is_accessor_at(slot)).then(|| m.val_at(slot))
    }

    /// An own data property of the record `idx` (a plain object), through
    /// the slot `hint` remembers for `key`, else found and remembered.
    #[inline]
    fn py_rec_field(&self, idx: u32, hint: &AtomicU16, key: &str) -> Option<Value> {
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

    /// The value of the entry of the `Map` `d` at `pos`, when that entry's
    /// key is the str `name` (and its value is not `undefined`).
    #[inline]
    fn py_map_entry_at(&self, d: Value, pos: usize, name: &str) -> Option<Value> {
        if !d.is_heap() {
            return None;
        }
        let HeapObj::Map { keys, vals } = self.heap.get(d.heap_index()) else {
            return None;
        };
        let (Some(&k), Some(&v)) = (keys.get(pos), vals.get(pos)) else {
            return None;
        };
        if !k.is_heap() || v.is_undefined() {
            return None;
        }
        match self.heap.get(k.heap_index()) {
            HeapObj::Str(s) if s.as_bytes() == name.as_bytes() => Some(v),
            _ => None,
        }
    }

    /// The instance `o` (a plain object), its class (a plain object) and
    /// that class's table `table` (`ga` / `sa`), when the table holds `true`
    /// for `name` at the hinted slot; then `o`'s own `dict`.
    #[inline]
    fn py_attr_hinted(&self, o: Value, name: &str, table: &AtomicU16, table_key: &str, hint: u32) -> Option<Value> {
        if !o.is_heap() {
            return None;
        }
        let oi = o.heap_index();
        let cls = self.py_rec_field(oi, &HINT_CLS, "cls")?;
        if !cls.is_heap() {
            return None;
        }
        let t = self.py_rec_field(cls.heap_index(), table, table_key)?;
        if !t.is_heap() || self.py_own_at(t.heap_index(), table_slot(hint), name)? != Value::TRUE {
            return None;
        }
        self.py_rec_field(oi, &HINT_DICT, "dict")
    }

    /// [`Instr::PyGetAttr`] answered from this site's hints (see the module
    /// comment); `None` leaves the instruction to its ordinary path.
    #[inline(never)]
    pub(super) fn py_attr_get_fast(&self, func_id: u32, ip: usize, o: Value, key: u32) -> Option<Value> {
        let hint = site(func_id, ip).load(Ordering::Relaxed);
        if hint == NONE {
            return None;
        }
        let name = self.py_const_name(func_id, key)?;
        let d = self.py_attr_hinted(o, name, &HINT_GA, "ga", hint)?;
        self.py_map_entry_at(d, dict_pos(hint), name)
    }

    /// [`Instr::PySetAttr`] for a name the instance dict already holds at
    /// this site's hinted position: the entry's value replaced in place, as
    /// `Map.prototype.set` replaces it. `false` leaves the instruction to its
    /// ordinary path.
    #[inline(never)]
    pub(super) fn py_attr_set_fast(&mut self, func_id: u32, ip: usize, o: Value, key: u32, v: Value) -> bool {
        let hint = site(func_id, ip).load(Ordering::Relaxed);
        if hint == NONE {
            return false;
        }
        let Some(name) = self.py_const_name(func_id, key) else {
            return false;
        };
        let Some(d) = self.py_attr_hinted(o, name, &HINT_SA, "sa", hint) else {
            return false;
        };
        let pos = dict_pos(hint);
        if self.py_map_entry_at(d, pos, name).is_some() {
            let di = d.heap_index();
            self.heap.write_barrier_val(di, v);
            if let HeapObj::Map { vals, .. } = self.heap.get_mut(di) {
                vals[pos] = v;
            }
            return true;
        }
        // A fresh instance's first store of the name (the dict has exactly
        // the entries before it, as when the hint was taken): appended, as
        // `Map.prototype.set` appends a key it does not hold.
        if !d.is_heap() || !self.py_map_lacks(d.heap_index(), pos, name) {
            return false;
        }
        let k = self.resolve_const_slot(func_id, key);
        if !k.is_heap() || !matches!(self.heap.get(k.heap_index()), HeapObj::Str(s) if s.as_bytes() == name.as_bytes()) {
            return false;
        }
        let di = d.heap_index();
        self.heap.write_barrier_val(di, v);
        if let HeapObj::Map { keys, vals } = self.heap.get_mut(di) {
            keys.push(k);
            vals.push(v);
        }
        self.coll_index_insert(di, k, pos);
        true
    }

    /// For a `Map` without a hash index whose keys are all flat strs or
    /// non-strs: whether it lacks a live entry for the str `name` with a
    /// value other than `undefined` (`Map.get(name) === undefined`). `None`
    /// when the scan cannot tell (an index, a string of another form).
    #[inline(never)]
    pub(super) fn py_map_lacks_name(&self, idx: u32, name: &str) -> Option<bool> {
        let HeapObj::Map { keys, vals } = self.heap.get(idx) else {
            return None;
        };
        if keys.len() >= 16 || self.collection_index.contains_key(&idx) {
            return None;
        }
        for (i, &k) in keys.iter().enumerate() {
            if k == Value::HOLE || !k.is_heap() {
                continue;
            }
            match self.heap.get(k.heap_index()) {
                HeapObj::Str(s) => {
                    if s.as_bytes() == name.as_bytes() {
                        return Some(vals.get(i).is_none_or(|v| v.is_undefined()));
                    }
                }
                _ if self.heap.is_str_like(k.heap_index()) => return None,
                _ => {}
            }
        }
        Some(true)
    }

    /// Whether the `Map` `idx` has exactly `len` entries (tombstones
    /// included), none of them a live key that is the str `name`, and few
    /// enough that it has no hash index.
    fn py_map_lacks(&self, idx: u32, len: usize, name: &str) -> bool {
        let HeapObj::Map { keys, vals } = self.heap.get(idx) else {
            return false;
        };
        if keys.len() != len || vals.len() != len || len >= 8 || self.collection_index.contains_key(&idx) {
            return false;
        }
        keys.iter().all(|&k| {
            if k == Value::HOLE || !k.is_heap() {
                return true;
            }
            match self.heap.get(k.heap_index()) {
                HeapObj::Str(s) => s.as_bytes() != name.as_bytes(),
                // A string of another representation might equal it.
                _ => !self.heap.is_str_like(k.heap_index()),
            }
        })
    }

    /// Record what an attribute site's ordinary path found: the class-table
    /// slot and, when known, the dict entry position.
    pub(super) fn py_attr_note(&self, func_id: u32, ip: usize, slot: usize, pos: Option<usize>) {
        let (Ok(slot), Ok(pos)) = (u16::try_from(slot), u16::try_from(pos.unwrap_or(0xFFFF))) else {
            return;
        };
        site(func_id, ip).store(slot as u32 | (pos as u32) << 16, Ordering::Relaxed);
    }

    /// The own data property `name` of the class table `t` (a plain
    /// object), tried first at the slot this site remembers; a slot found by
    /// the scan is remembered.
    #[inline(never)]
    pub(super) fn py_table_own(&self, func_id: u32, ip: usize, t: u32, name: &str) -> Option<Value> {
        let hint = site(func_id, ip).load(Ordering::Relaxed);
        if hint != NONE {
            if let Some(v) = self.py_own_at(t, table_slot(hint), name) {
                return Some(v);
            }
        }
        let HeapObj::Object(m) = self.heap.get(t) else {
            return None;
        };
        let slot = m.pos(name)?;
        if m.is_accessor_at(slot) {
            return None;
        }
        let v = m.val_at(slot);
        self.py_attr_note(func_id, ip, slot, None);
        Some(v)
    }
}

/// The positional entry names a class and its `__init__` carry (`c<n>`).
#[cfg_attr(not(feature = "python"), allow(dead_code))]
const ENTRY_NAMES: [&str; 8] = ["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7"];
#[cfg_attr(not(feature = "python"), allow(dead_code))]
static HINT_CTORINIT: AtomicU16 = AtomicU16::new(0);
#[cfg_attr(not(feature = "python"), allow(dead_code))]
static HINT_CTORS: AtomicU16 = AtomicU16::new(0);
#[cfg_attr(not(feature = "python"), allow(dead_code))]
static HINT_INSTTMPL: AtomicU16 = AtomicU16::new(0);

impl<'p> Vm<'p> {
    /// An own data property `name` of the plain object `idx`, tried at the
    /// slot `hint` (a site hint half) first; the slot it is found at.
    #[inline]
    #[cfg_attr(not(feature = "python"), allow(dead_code))]
    fn py_own_hinted(&self, idx: u32, name: &str, hint: usize) -> Option<(Value, usize)> {
        if let Some(v) = self.py_own_at(idx, hint, name) {
            return Some((v, hint));
        }
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        let slot = m.pos(name)?;
        (!m.is_accessor_at(slot)).then(|| (m.val_at(slot), slot))
    }

    /// [`Instr::PyNew`]: the new instance, the `__init__` entry to call (or
    /// `null`) and its `this`; `None` for the slow edge.
    #[cfg(feature = "python")]
    #[inline(never)]
    pub(super) fn py_new(&mut self, func_id: u32, ip: usize, cls: Value, rt: Value, n: usize) -> Option<(Value, Value, Value)> {
        let (&entry_name, &init_name) = (ENTRY_NAMES.get(n)?, ENTRY_NAMES.get(n + 1)?);
        if !cls.is_heap() || !rt.is_heap() {
            return None;
        }
        let ci = cls.heap_index();
        match self.heap.get(ci) {
            HeapObj::Object(m) if !m.is_ctor => {}
            _ => return None,
        }
        // The class's entry for the count is the runtime's plain one.
        let ctors = self.py_rec_field(rt.heap_index(), &HINT_CTORS, "CTORS")?;
        let want = match (ctors.is_heap(), ctors.is_heap().then(|| self.heap.get(ctors.heap_index()))) {
            (true, Some(HeapObj::Array(items))) => *items.get(n)?,
            _ => return None,
        };
        let site = site(func_id, ip);
        let hint = site.load(Ordering::Relaxed);
        let (have, entry_slot) = self.py_own_hinted(ci, entry_name, table_slot(hint))?;
        if !want.is_heap() || have.bits() != want.bits() {
            return None;
        }
        let init = self.py_rec_field(ci, &HINT_CTORINIT, "ctorInit")?;
        let (entry, init_slot) = if init == Value::NULL {
            if n != 0 {
                return None;
            }
            (Value::NULL, dict_pos(hint))
        } else {
            if !init.is_heap() || !matches!(self.heap.get(init.heap_index()), HeapObj::Object(m) if !m.is_ctor) {
                return None;
            }
            let (e, slot) = self.py_own_hinted(init.heap_index(), init_name, dict_pos(hint))?;
            if self.type_of(e) != "function" {
                return None;
            }
            (e, slot)
        };
        // `{ cls: this, dict: new Map() }`, as the entry's literal makes it.
        let tmpl = self.py_rec_field(rt.heap_index(), &HINT_INSTTMPL, "INSTTMPL")?;
        if !self.py_alloc_like_ok(tmpl, &["cls", "dict"], super::py_ops::TMPL_INST) {
            return None;
        }
        let map = self.heap.alloc(HeapObj::Map { keys: Vec::new(), vals: Vec::new() });
        self.adopt_native_result_realm(map, self.map_proto);
        let obj = self.py_alloc_like(tmpl, &["cls", "dict"], super::py_ops::TMPL_INST, &[cls, Value::heap(map)])?;
        if let (Ok(a), Ok(b)) = (u16::try_from(entry_slot), u16::try_from(init_slot)) {
            site.store(a as u32 | (b as u32) << 16, Ordering::Relaxed);
        }
        Some((obj, entry, init))
    }
}

#[cfg_attr(not(feature = "python"), allow(dead_code))]
static HINT_GS: AtomicU16 = AtomicU16::new(0);
#[cfg_attr(not(feature = "python"), allow(dead_code))]
static HINT_MSELF: AtomicU16 = AtomicU16::new(0);
#[cfg_attr(not(feature = "python"), allow(dead_code))]
static HINT_HIT_CLS: AtomicU16 = AtomicU16::new(0);
#[cfg_attr(not(feature = "python"), allow(dead_code))]
static HINT_HIT_F: AtomicU16 = AtomicU16::new(1);

impl<'p> Vm<'p> {
    /// `__zipp_py_smfind(cls, self, name)` with the runtime's `R` as `this`:
    /// the runtime's `smfind` fast path, natively. When `self` is a plain
    /// object that is not a type, its own data `cls` a plain object whose own
    /// data `gs` table holds for `name` (a str) a plain record `{cls, f}`
    /// with `cls` being `cls`: `R.mself = true` (an own data property `R`
    /// already has) and `f`. `undefined` for anything else, when the runtime's
    /// `smfind` answers exactly as before.
    #[cfg(feature = "python")]
    #[inline(never)]
    pub(crate) fn py_smfind(&mut self, cls: Value, this_self: Value, name: Value, rt: Value) -> Value {
        match self.py_smfind_hit(cls, this_self, name, rt) {
            Some((f, slot)) => {
                if let HeapObj::Object(m) = self.heap.get_mut(rt.heap_index()) {
                    m.set_val_at(slot, Value::TRUE);
                }
                f
            }
            None => Value::UNDEFINED,
        }
    }

    #[cfg(feature = "python")]
    fn py_smfind_hit(&mut self, cls: Value, this_self: Value, name: Value, rt: Value) -> Option<(Value, usize)> {
        if !this_self.is_heap() || !rt.is_heap() || !name.is_heap() {
            return None;
        }
        let si = this_self.heap_index();
        // `self.isType !== true`: a record without an own `isType` of true
        // (a type's is its own data property).
        match self.heap.get(si) {
            HeapObj::Object(m) if !m.is_ctor => {
                if m.pos("isType").is_some_and(|s| m.is_accessor_at(s) || m.val_at(s) == Value::TRUE) {
                    return None;
                }
            }
            _ => return None,
        }
        let t = self.py_rec_field(si, &HINT_CLS, "cls")?;
        if !t.is_heap() {
            return None;
        }
        let gs = self.py_rec_field(t.heap_index(), &HINT_GS, "gs")?;
        if !gs.is_heap() || !self.heap.is_str_like(name.heap_index()) {
            return None;
        }
        self.heap.flatten(name.heap_index());
        let hit = {
            let HeapObj::Str(s) = self.heap.get(name.heap_index()) else {
                return None;
            };
            let key = std::str::from_utf8(s.as_bytes()).ok()?;
            let HeapObj::Object(m) = self.heap.get(gs.heap_index()) else {
                return None;
            };
            let slot = m.pos(key)?;
            if m.is_accessor_at(slot) {
                return None;
            }
            m.val_at(slot)
        };
        if !hit.is_heap() {
            return None;
        }
        let hit_cls = self.py_rec_field(hit.heap_index(), &HINT_HIT_CLS, "cls")?;
        if hit_cls.bits() != cls.bits() {
            return None;
        }
        let f = self.py_rec_field(hit.heap_index(), &HINT_HIT_F, "f")?;
        // `R.mself = true` needs the slot `R` already has for it.
        let HeapObj::Object(r) = self.heap.get(rt.heap_index()) else {
            return None;
        };
        let h = HINT_MSELF.load(Ordering::Relaxed) as usize;
        let slot = if h < r.len() && r.key_at(h) == "mself" {
            h
        } else {
            let s = r.pos("mself")?;
            if let Ok(s16) = u16::try_from(s) {
                HINT_MSELF.store(s16, Ordering::Relaxed);
            }
            s
        };
        let a = r.attr_at(slot);
        if a.accessor || !a.writable {
            return None;
        }
        Some((f, slot))
    }
}
