//! The Python frontend's attribute instructions (`Instr::PyGetAttr`,
//! `PySetAttr`, `PyMethod`, `PyClassAttr`, `PyAttrFn`, `PyNew`) and the
//! runtime's `smfind` fast path.
//!
//! Those instructions answer from the runtime's per-class tables (`ga`, `sa`,
//! `gm`, `gv`, `gp`, `sp`: plain objects keyed by attribute name) and an
//! instance's `dict`: layout-mode attribute storage (`vm::py_table::layout`,
//! where a site caches the class, its stamp, the layout and the slot, and a
//! layout proves a name absent), a dict-mode table, or a `Map` (an object
//! made by other code). A site's ordinary path finds the name in each
//! and records what it found in the site's cache entry (`vm::py_rt`), keyed
//! by the class and its version stamp; a later hit checks the stamp and the
//! dict entry at the recorded position instead (see `vm::py_rt`).

use super::py_rt::{hint, ic, PyIc};
use super::*;
use crate::heap::HeapObj;
use crate::value::Value;


/// A `PyIc` key stamp (`d` of an `ATTR_GET` / `ATTR_SET` / `GLOBAL` /
/// `BUILTIN` entry): the key is the site's cached constant, never freed.
pub(super) const KEY_ROOTED: u32 = u32::MAX;
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

    /// The value of the `Map` `d`'s entry at `pos` when its key there is
    /// still the entry's: the bits `key` and the stamp `kver` (the key's
    /// slot version when the entry was made, or [`KEY_ROOTED`]: see
    /// [`Vm::py_key_stamp`]), and its value is not `undefined`. The bits alone do not identify the key: a dict's key
    /// string need not be the site's rooted constant (a `setattr` name, or
    /// a constant loaded past the constant cache's bound, is a fresh
    /// string), so it can die with its dict and its slot be reused by
    /// another name's key at the same position of another instance's dict.
    #[inline]
    fn py_map_at(&self, d: Value, pos: u32, key: u64, kver: u32) -> Option<Value> {
        if !d.is_heap() {
            return None;
        }
        let HeapObj::Map { keys, vals } = self.heap.get(d.heap_index()) else {
            return None;
        };
        let pos = pos as usize;
        match (keys.get(pos), vals.get(pos)) {
            (Some(&k), Some(&v)) if k.bits() == key && !v.is_undefined() && (kver == KEY_ROOTED || self.py_key_ver(k) == kver) => Some(v),
            _ => None,
        }
    }

    /// The slot version of a heap key (0 for an immediate): with its bits,
    /// the key's identity for an entry that outlives it.
    #[inline]
    pub(super) fn py_key_ver(&self, k: Value) -> u32 {
        if k.is_heap() {
            self.heap.version_of(k.heap_index())
        } else {
            0
        }
    }

    /// The key stamp an entry records for the dict key `k` found for the
    /// site's constant `cidx`: [`KEY_ROOTED`] when `k` is that constant's
    /// cached representation (alive for the VM's life, so a hit needs no
    /// check), else `k`'s slot version; `None` (do not cache) when that
    /// version is the sentinel.
    pub(super) fn py_key_stamp(&self, func_id: u32, cidx: u32, k: Value) -> Option<u32> {
        if self.const_slot_cached(func_id, cidx).is_some_and(|c| c.bits() == k.bits()) {
            return Some(KEY_ROOTED);
        }
        let v = self.py_key_ver(k);
        (v != KEY_ROOTED).then_some(v)
    }

    /// A class table (`slot` one of the class record's [`TySlots`]) and the
    /// own data property `name` in it, with its slot; `cls` a plain record.
    fn py_table_entry(&self, cls: Value, slot: usize, table_key: &str, name: &str) -> Option<(Value, usize, Value)> {
        if !self.py_plain_rec(cls) {
            return None;
        }
        let t = self.py_ty_field(cls.heap_index(), slot, table_key)?;
        if !t.is_heap() {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(t.heap_index()) else {
            return None;
        };
        let s = m.pos(name)?;
        if m.is_accessor_at(s) {
            return None;
        }
        Some((t, s, m.val_at(s)))
    }

    /// The value of the class table `table` (whose bits the class's field
    /// must still be) at `pos`; `cls` has an entry's stamp.
    #[inline]
    fn py_table_at(&self, cls: Value, slot: usize, table: u64, pos: u32) -> Option<Value> {
        let HeapObj::Object(c) = self.heap.get(cls.heap_index()) else {
            return None;
        };
        // The class's keys are as when the entry was made (its stamp): the
        // table field is at its slot.
        if slot >= c.len() {
            return None;
        }
        let t = c.val_at(slot);
        if t.bits() != table || !t.is_heap() {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(t.heap_index()) else {
            return None;
        };
        let pos = pos as usize;
        (pos < m.len() && !m.is_accessor_at(pos)).then(|| m.val_at(pos))
    }

    /// Whether this site's [`ic::NOT_PLAIN`] entry `e` still proves the
    /// class table at `slot` holds no `true` for the site's name.
    #[inline]
    fn py_not_plain(&self, e: &PyIc, cls: Value, slot: usize) -> bool {
        if e.kind != ic::NOT_PLAIN || cls.bits() != e.a || !cls.is_heap() || !self.py_cls_stamp_ok(cls, e.hver, e.c) {
            return false;
        }
        let HeapObj::Object(c) = self.heap.get(cls.heap_index()) else {
            return false;
        };
        if slot >= c.len() {
            return false;
        }
        let t = c.val_at(slot);
        if t.bits() != e.b || !t.is_heap() {
            return false;
        }
        let HeapObj::Object(m) = self.heap.get(t.heap_index()) else {
            return false;
        };
        if e.pos == u32::MAX {
            return m.len() == e.d as usize;
        }
        let pos = e.pos as usize;
        pos < m.len() && !m.is_accessor_at(pos) && m.val_at(pos) != Value::TRUE
    }

    /// Record that the class table at `slot` holds no `true` for `name`.
    fn py_note_not_plain(&mut self, func_id: u32, ip: usize, cls: Value, slot: usize, key: &str, name: &str) {
        let Some((hver, ver)) = self.py_cls_stamp(cls) else {
            return;
        };
        let Some(t) = self.py_ty_field(cls.heap_index(), slot, key) else {
            return;
        };
        if !t.is_heap() {
            return;
        }
        let HeapObj::Object(m) = self.heap.get(t.heap_index()) else {
            return;
        };
        let (pos, d) = match m.pos(name) {
            Some(s) if m.is_accessor_at(s) => return,
            Some(s) => match u32::try_from(s) {
                Ok(s) if s != u32::MAX => (s, 0),
                _ => return,
            },
            None => match u32::try_from(m.len()) {
                Ok(n) => (u32::MAX, n),
                Err(_) => return,
            },
        };
        self.py_ic_put(func_id, ip, PyIc { kind: ic::NOT_PLAIN, pos, hver, a: cls.bits(), b: t.bits(), c: ver, d });
    }

    /// The class entry of `PyIc` kind `kind` for `cls`, when this site has
    /// one and the class still has its stamp.
    #[inline]
    fn py_cls_ic(&self, func_id: u32, ip: usize, kind: u8, cls: Value) -> Option<PyIc> {
        let e = self.py_ic(func_id, ip);
        (e.kind == kind && cls.bits() == e.a && cls.is_heap() && self.py_cls_stamp_ok(cls, e.hver, e.c)).then_some(e)
    }

    /// [`Instr::PyGetAttr`]: `obj.name` from an instance's own dict when its
    /// class's `ga[name]` is `true`; `None` for the slow edge.
    #[inline(never)]
    pub(super) fn py_attr_get(&mut self, func_id: u32, ip: usize, o: Value, key: u32) -> Option<Value> {
        let (cls, dict) = self.py_inst_parts(o)?;
        let ty = self.py_rt.as_deref()?.ty?;
        let e = self.py_ic(func_id, ip);
        if cls.bits() == e.a && cls.is_heap() {
            match e.kind {
                // The instance's layout puts the name at `pos`.
                ic::ATTR_GET_L => {
                    if let Some(v) = self.py_attrs_at(dict, e.d, e.pos as usize) {
                        if self.py_cls_stamp_ok(cls, e.hver, e.c) {
                            return Some(v);
                        }
                    }
                }
                // The instance's layout lacks the name: the class's plain
                // attribute (what `PyClassAttr` would answer next).
                ic::GET_CLASS_L => {
                    if self.py_attrs_of(dict).is_some_and(|(l, _)| l == e.d) && self.py_cls_stamp_ok(cls, e.hver, e.c) {
                        if let Some(v) = self.py_table_at(cls, ty.gv, e.b, e.pos) {
                            if !v.is_undefined() && v != Value::HOLE {
                                return Some(v);
                            }
                        }
                    }
                }
                ic::ATTR_GET => {
                    if self.py_cls_stamp_ok(cls, e.hver, e.c) {
                        if let Some(v) = self.py_map_at(dict, e.pos, e.b, e.d) {
                            return Some(v);
                        }
                    }
                }
                _ => {}
            }
        }
        if self.py_not_plain(&e, cls, ty.ga) {
            return None;
        }
        let name = self.py_const_name(func_id, key)?;
        let flag = self.py_table_entry(cls, ty.ga, "ga", name).map(|(_, _, f)| f);
        if flag != Some(Value::TRUE) {
            self.py_note_not_plain(func_id, ip, cls, ty.ga, "ga", name);
            return None;
        }
        if !dict.is_heap() {
            return None;
        }
        let k = self.resolve_const_slot(func_id, key);
        if let Some((l, _)) = self.py_attrs_of(dict) {
            if let Some(slot) = self.py_layout_find(l, k) {
                let v = self.py_attrs_at(dict, l, slot)?;
                self.py_attr_note_l(func_id, ip, ic::ATTR_GET_L, cls, l, slot, 0);
                return Some(v);
            }
            // Not the instance's: the class's plain attribute, if it has one.
            let (t, s, v) = self.py_table_entry(cls, ty.gv, "gv", name)?;
            if v.is_undefined() || v == Value::HOLE {
                return None;
            }
            if let (Some((hver, ver)), Ok(pos)) = (self.py_cls_stamp(cls), u32::try_from(s)) {
                self.py_ic_put(func_id, ip, PyIc { kind: ic::GET_CLASS_L, pos, hver, a: cls.bits(), b: t.bits(), c: ver, d: l });
            }
            return Some(v);
        }
        if matches!(self.heap.get(dict.heap_index()), HeapObj::PyTable(_)) {
            return self.py_table_get(dict, k)?;
        }
        let (pos, v) = self.py_map_find(dict, k)?;
        self.py_attr_note(func_id, ip, ic::ATTR_GET, cls, dict, pos, key);
        Some(v)
    }

    /// Record a layout-mode entry for a `ga` / `sa` site: the name at
    /// `slot` of layout `l` (`to`: the layout an append makes).
    fn py_attr_note_l(&mut self, func_id: u32, ip: usize, kind: u8, cls: Value, l: u32, slot: usize, to: u32) {
        let (Some((hver, ver)), Ok(pos)) = (self.py_cls_stamp(cls), u32::try_from(slot)) else {
            return;
        };
        self.py_ic_put(func_id, ip, PyIc { kind, pos, hver, a: cls.bits(), b: u64::from(to), c: ver, d: l });
    }

    /// Record an instance-dict entry found at `pos` for a `ga` / `sa` site.
    fn py_attr_note(&mut self, func_id: u32, ip: usize, kind: u8, cls: Value, dict: Value, pos: usize, cidx: u32) {
        let Some((hver, ver)) = self.py_cls_stamp(cls) else {
            return;
        };
        let key = match self.heap.get(dict.heap_index()) {
            HeapObj::Map { keys, .. } => match keys.get(pos) {
                Some(&k) => k,
                None => return,
            },
            _ => return,
        };
        let Ok(pos) = u32::try_from(pos) else {
            return;
        };
        let Some(kver) = self.py_key_stamp(func_id, cidx, key) else {
            return;
        };
        self.py_ic_put(func_id, ip, PyIc { kind, pos, hver, a: cls.bits(), b: key.bits(), c: ver, d: kver });
    }

    /// [`Instr::PySetAttr`]: `obj.name = v` as a store into an instance's
    /// own dict when its class's `sa[name]` is `true`; `false` for the slow
    /// edge.
    #[inline(never)]
    pub(super) fn py_attr_set(&mut self, func_id: u32, ip: usize, o: Value, key: u32, v: Value) -> Result<bool, Thrown> {
        let Some((cls, dict)) = self.py_inst_parts(o) else {
            return Ok(false);
        };
        if !dict.is_heap() {
            return Ok(false);
        }
        let di = dict.heap_index();
        let e = self.py_ic(func_id, ip);
        // Layout mode: a value replaced at its slot, or appended.
        if cls.bits() == e.a && cls.is_heap() {
            match e.kind {
                ic::ATTR_SET_L => {
                    if self.py_attrs_of(dict).is_some_and(|(l, _)| l == e.d) && self.py_cls_stamp_ok(cls, e.hver, e.c) {
                        self.py_attrs_put(di, e.pos as usize, v);
                        return Ok(true);
                    }
                }
                ic::ATTR_APPEND_L => {
                    if self.py_attrs_of(dict) == Some((e.d, e.pos as usize)) && self.py_cls_stamp_ok(cls, e.hver, e.c) {
                        self.py_attrs_push(di, e.b as u32, v);
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
        let Some(ty) = self.py_rt.as_deref().and_then(|p| p.ty) else {
            return Ok(false);
        };
        if self.py_not_plain(&e, cls, ty.sa) {
            return Ok(false);
        }
        match self.heap.get(di) {
            HeapObj::PyAttrs { .. } | HeapObj::PyTable(_) => return self.py_attr_set_store(func_id, ip, cls, dict, key, v),
            HeapObj::Map { .. } => {}
            _ => return Ok(false),
        }
        if (e.kind == ic::ATTR_SET || e.kind == ic::ATTR_APPEND) && cls.bits() == e.a && self.py_cls_stamp_ok(cls, e.hver, e.c) {
            if e.kind == ic::ATTR_SET && self.py_map_at(dict, e.pos, e.b, e.d).is_some() {
                // The entry's value replaced in place, as `Map.prototype.set`
                // replaces it.
                self.heap.write_barrier_val(di, v);
                if let HeapObj::Map { vals, .. } = self.heap.get_mut(di) {
                    vals[e.pos as usize] = v;
                }
                return Ok(true);
            }
            if e.kind == ic::ATTR_APPEND {
                // A fresh instance's store of the name (its dict has exactly
                // the entries it had when the entry was made, none of them
                // the name): appended, as `Map.prototype.set` appends it.
                let k = self.resolve_const_slot(func_id, key);
                if k.bits() == e.b && self.py_map_lacks(di, e.pos as usize, k) {
                    self.heap.write_barrier_val(di, v);
                    if let HeapObj::Map { keys, vals } = self.heap.get_mut(di) {
                        keys.push(k);
                        vals.push(v);
                    }
                    self.coll_index_insert(di, k, e.pos as usize);
                    return Ok(true);
                }
            }
        }
        let Some(name) = self.py_const_name(func_id, key) else {
            return Ok(false);
        };
        match self.py_table_entry(cls, ty.sa, "sa", name) {
            Some((_, _, flag)) if flag == Value::TRUE => {}
            _ => {
                self.py_note_not_plain(func_id, ip, cls, ty.sa, "sa", name);
                return Ok(false);
            }
        }
        let k = self.resolve_const_slot(func_id, key);
        let before = match self.heap.get(di) {
            HeapObj::Map { keys, .. } => keys.len(),
            _ => return Ok(false),
        };
        let absent = self.py_map_lacks(di, before, k);
        self.map_method(di, "set", &[k, v])?;
        // Where the entry is now (replaced in place, or appended).
        let (appended, pos) = match self.heap.get(di) {
            HeapObj::Map { keys, .. } if keys.len() == before + 1 && keys.last().is_some_and(|l| l.bits() == k.bits()) => (true, Some(before)),
            _ => (false, self.py_map_find(dict, k).map(|(p, _)| p)),
        };
        if appended && absent {
            if let (Some((hver, ver)), Ok(pos)) = (self.py_cls_stamp(cls), u32::try_from(before)) {
                self.py_ic_put(func_id, ip, PyIc { kind: ic::ATTR_APPEND, pos, hver, a: cls.bits(), b: k.bits(), c: ver, d: 0 });
            }
        } else if let Some(pos) = pos {
            self.py_attr_note(func_id, ip, ic::ATTR_SET, cls, dict, pos, key);
        }
        Ok(true)
    }

    /// [`Vm::py_attr_set`] into layout-mode or table storage, past the
    /// site's cache.
    fn py_attr_set_store(&mut self, func_id: u32, ip: usize, cls: Value, dict: Value, key: u32, v: Value) -> Result<bool, Thrown> {
        let Some(ty) = self.py_rt.as_deref().and_then(|p| p.ty) else {
            return Ok(false);
        };
        let Some(name) = self.py_const_name(func_id, key) else {
            return Ok(false);
        };
        match self.py_table_entry(cls, ty.sa, "sa", name) {
            Some((_, _, flag)) if flag == Value::TRUE => {}
            _ => {
                self.py_note_not_plain(func_id, ip, cls, ty.sa, "sa", name);
                return Ok(false);
            }
        }
        let k = self.resolve_const_slot(func_id, key);
        let di = dict.heap_index();
        if let Some((l, n)) = self.py_attrs_of(dict) {
            if let Some(slot) = self.py_layout_find(l, k) {
                self.py_attrs_put(di, slot, v);
                self.py_attr_note_l(func_id, ip, ic::ATTR_SET_L, cls, l, slot, 0);
                return Ok(true);
            }
            if let Some(to) = self.py_layout_child(l, k) {
                self.py_attrs_push(di, to, v);
                self.py_attr_note_l(func_id, ip, ic::ATTR_APPEND_L, cls, l, n, to);
                if let Some(p) = self.py_rt.as_deref_mut() {
                    p.layouts.note_size(cls, n + 1);
                }
                return Ok(true);
            }
            self.py_attrs_materialize(dict);
        }
        Ok(matches!(self.py_table_set(dict, k, v), Some(Ok(_))))
    }

    /// Whether instance storage `d` lacks the name `name` (`k` its
    /// constant), `known` a layout already proven to lack it: the answer and
    /// the layout it proves lacking (0: none). `None` when it cannot tell.
    fn py_store_lacks(&mut self, d: Value, name: &str, k: Value, known: u32) -> Option<(bool, u32)> {
        if let Some((l, _)) = self.py_attrs_of(d) {
            if l == known {
                return Some((true, l));
            }
            let lacks = self.py_layout_find(l, k).is_none();
            return Some((lacks, if lacks { l } else { 0 }));
        }
        if d.is_heap() && matches!(self.heap.get(d.heap_index()), HeapObj::PyTable(_)) {
            return Some((self.py_table_get(d, k)?.is_none(), 0));
        }
        self.py_dict_lacks(d, name, k).map(|b| (b, 0))
    }

    /// Record the layout `l` proven to lack a site's name in its entry `e`.
    fn py_note_lacks(&mut self, func_id: u32, ip: usize, mut e: PyIc, l: u32) {
        if l != 0 && l != e.d {
            e.d = l;
            self.py_ic_put(func_id, ip, e);
        }
    }

    /// Whether the `Map` `idx` has exactly `len` entries (holes included),
    /// none a live key equal to the str `k`, and few enough that it has no
    /// hash index (a scan answers).
    fn py_map_lacks(&self, idx: u32, len: usize, k: Value) -> bool {
        let HeapObj::Map { keys, vals } = self.heap.get(idx) else {
            return false;
        };
        if keys.len() != len || vals.len() != len || len >= 8 || self.collection_index.contains_key(&idx) {
            return false;
        }
        let HeapObj::Str(name) = self.heap.get(k.heap_index()) else {
            return false;
        };
        let name = name.as_bytes();
        keys.iter().all(|&key| {
            if key.bits() == k.bits() {
                return false;
            }
            if key == Value::HOLE || !key.is_heap() {
                return true;
            }
            match self.heap.get(key.heap_index()) {
                HeapObj::Str(s) => s.as_bytes() != name,
                // A string of another representation might equal it.
                _ => !self.heap.is_str_like(key.heap_index()),
            }
        })
    }

    /// For a `Map` without a hash index whose keys are all flat strs or
    /// non-strs: whether it lacks a live entry for the str `name` (`k` its
    /// interned copy) with a value other than `undefined`
    /// (`Map.get(name) === undefined`). `None` when the scan cannot tell (an
    /// index, a string of another form).
    #[inline(never)]
    pub(super) fn py_map_lacks_name(&self, idx: u32, name: &str, k: Value) -> Option<bool> {
        let HeapObj::Map { keys, vals } = self.heap.get(idx) else {
            return None;
        };
        if keys.len() >= 16 || self.collection_index.contains_key(&idx) {
            return None;
        }
        for (i, &key) in keys.iter().enumerate() {
            if key.bits() == k.bits() {
                return Some(vals.get(i).is_none_or(|v| v.is_undefined()));
            }
            if key == Value::HOLE || !key.is_heap() {
                continue;
            }
            match self.heap.get(key.heap_index()) {
                HeapObj::Str(s) => {
                    if s.as_bytes() == name.as_bytes() {
                        return Some(vals.get(i).is_none_or(|v| v.is_undefined()));
                    }
                }
                _ if self.heap.is_str_like(key.heap_index()) => return None,
                _ => {}
            }
        }
        Some(true)
    }

    /// Whether an instance's dict (`d`, a Map) holds no attribute `name`
    /// (`k` its constant) that would shadow a class-level answer; `None`
    /// when `d` is not a Map.
    fn py_dict_lacks(&mut self, d: Value, name: &str, k: Value) -> Option<bool> {
        if !d.is_heap() || !matches!(self.heap.get(d.heap_index()), HeapObj::Map { .. }) {
            return None;
        }
        if let Some(lacks) = self.py_map_lacks_name(d.heap_index(), name, k) {
            return Some(lacks);
        }
        Some(self.py_map_get(d, k).is_none())
    }

    /// The method for [`Instr::PyMethod`]: a user class's plain Python
    /// function (`gm`) the instance dict does not shadow, or a builtin
    /// method of a str or a dict-less builtin container (`gb["name#n"]`).
    #[inline(never)]
    pub(super) fn py_method(&mut self, func_id: u32, ip: usize, o: Value, rt: Value, key: u32, gb: u32) -> Option<Value> {
        if !o.is_heap() {
            return None;
        }
        let p = self.py_rt_for(rt)?;
        let (ty, t_str, t_module) = (p.ty?, p.t_str, p.t_module);
        let (cls, dict) = if self.heap.is_str_like(o.heap_index()) {
            (t_str, Value::UNDEFINED)
        } else {
            self.py_inst_parts(o)?
        };
        if !cls.is_heap() {
            return None;
        }
        let e = self.py_ic(func_id, ip);
        if e.a == cls.bits() && self.py_cls_stamp_ok(cls, e.hver, e.c) {
            if e.kind == ic::METHOD {
                if let Some(f) = self.py_table_at(cls, ty.gm, e.b, e.pos) {
                    if self.py_plain_rec(f) {
                        if e.d != 0 && self.py_attrs_of(dict).is_some_and(|(l, _)| l == e.d) {
                            return Some(f);
                        }
                        let name = self.py_const_name(func_id, key)?;
                        let k = self.resolve_const_slot(func_id, key);
                        let (lacks, l) = self.py_store_lacks(dict, name, k, e.d)?;
                        self.py_note_lacks(func_id, ip, e, l);
                        return lacks.then_some(f);
                    }
                }
            } else if e.kind == ic::BUILTIN_METHOD && dict.is_undefined() {
                if let Some(b) = self.py_table_at(cls, ty.gb, e.b, e.pos) {
                    if self.py_plain_rec(b) {
                        return Some(b);
                    }
                }
            }
        }
        // A module's functions are its globals (`PyModGet`).
        if cls.bits() == t_module.bits() || !self.py_plain_rec(cls) {
            return None;
        }
        let stamp = self.py_cls_stamp(cls);
        if !dict.is_undefined() {
            // A user class's function, unless the instance dict shadows it.
            let name = self.py_const_name(func_id, key)?;
            if let Some((t, s, m)) = self.py_table_entry(cls, ty.gm, "gm", name) {
                if self.py_plain_rec(m) {
                    let k = self.resolve_const_slot(func_id, key);
                    let (lacks, l) = self.py_store_lacks(dict, name, k, 0)?;
                    if let (Some((hver, ver)), Ok(pos)) = (stamp, u32::try_from(s)) {
                        self.py_ic_put(func_id, ip, PyIc { kind: ic::METHOD, pos, hver, a: cls.bits(), b: t.bits(), c: ver, d: l });
                    }
                    return lacks.then_some(m);
                }
            }
        }
        let gb_name: &str = self.func(func_id as usize).string_constants[gb as usize].as_str();
        let (t, s, b) = self.py_table_entry(cls, ty.gb, "gb", gb_name)?;
        if !self.py_plain_rec(b) {
            return None;
        }
        if dict.is_undefined() {
            if let (Some((hver, ver)), Ok(pos)) = (stamp, u32::try_from(s)) {
                self.py_ic_put(func_id, ip, PyIc { kind: ic::BUILTIN_METHOD, pos, hver, a: cls.bits(), b: t.bits(), c: ver, d: 0 });
            }
        }
        Some(b)
    }

    /// The value for [`Instr::PyClassAttr`]: a plain class attribute
    /// (`gv[name]`) of an instance whose dict lacks the name (and whose
    /// class's `ga[name]` is `true`).
    #[inline(never)]
    pub(super) fn py_class_attr(&mut self, func_id: u32, ip: usize, o: Value, key: u32) -> Option<Value> {
        let (cls, dict) = self.py_inst_parts(o)?;
        if !cls.is_heap() || !dict.is_heap() {
            return None;
        }
        let ty = self.py_rt.as_deref()?.ty?;
        let e = self.py_ic(func_id, ip);
        if self.py_not_plain(&e, cls, ty.ga) {
            return None;
        }
        let name = self.py_const_name(func_id, key)?;
        let k = self.resolve_const_slot(func_id, key);
        if e.kind == ic::CLASS_ATTR && cls.bits() == e.a && self.py_cls_stamp_ok(cls, e.hver, e.c) {
            if let Some(v) = self.py_table_at(cls, ty.gv, e.b, e.pos) {
                if !v.is_undefined() && v != Value::HOLE {
                    let (lacks, l) = self.py_store_lacks(dict, name, k, e.d)?;
                    self.py_note_lacks(func_id, ip, e, l);
                    if lacks {
                        return Some(v);
                    }
                }
            }
        }
        let flag = self.py_table_entry(cls, ty.ga, "ga", name).map(|(_, _, f)| f);
        if flag != Some(Value::TRUE) {
            self.py_note_not_plain(func_id, ip, cls, ty.ga, "ga", name);
            return None;
        }
        let (lacks, l) = self.py_store_lacks(dict, name, k, 0)?;
        if !lacks {
            return None;
        }
        let (t, s, v) = self.py_table_entry(cls, ty.gv, "gv", name)?;
        if v.is_undefined() || v == Value::HOLE {
            return None;
        }
        if let (Some((hver, ver)), Ok(pos)) = (self.py_cls_stamp(cls), u32::try_from(s)) {
            self.py_ic_put(func_id, ip, PyIc { kind: ic::CLASS_ATTR, pos, hver, a: cls.bits(), b: t.bits(), c: ver, d: l });
        }
        Some(v)
    }

    /// The accessor for [`Instr::PyAttrFn`]: a property's Python getter
    /// (`gp[name]`) or setter (`sp[name]`), when the class's `ga` / `sa`
    /// does not say `true` for the name.
    #[inline(never)]
    pub(super) fn py_attr_fn(&mut self, func_id: u32, ip: usize, o: Value, key: u32, set: bool) -> Option<Value> {
        let cls = self.py_cls_of(o)?;
        if !cls.is_heap() {
            return None;
        }
        let ty = self.py_rt.as_deref()?.ty?;
        let (fn_slot, fn_key, plain_slot, plain_key) = if set { (ty.sp, "sp", ty.sa, "sa") } else { (ty.gp, "gp", ty.ga, "ga") };
        if let Some(e) = self.py_cls_ic(func_id, ip, ic::ATTR_FN, cls) {
            if let Some(f) = self.py_table_at(cls, fn_slot, e.b, e.pos) {
                if self.py_plain_rec(f) {
                    return Some(f);
                }
            }
        }
        if !self.py_plain_rec(cls) {
            return None;
        }
        let name = self.py_const_name(func_id, key)?;
        let plain = self.py_ty_field(cls.heap_index(), plain_slot, plain_key)?;
        if !self.py_plain_rec(plain) || self.py_own_data(plain.heap_index(), name) == Some(Value::TRUE) {
            return None;
        }
        let (t, s, f) = self.py_table_entry(cls, fn_slot, fn_key, name)?;
        if !self.py_plain_rec(f) {
            return None;
        }
        if let (Some((hver, ver)), Ok(pos)) = (self.py_cls_stamp(cls), u32::try_from(s)) {
            self.py_ic_put(func_id, ip, PyIc { kind: ic::ATTR_FN, pos, hver, a: cls.bits(), b: t.bits(), c: ver, d: 0 });
        }
        Some(f)
    }
}

/// The positional entry names a class and its `__init__` carry (`c<n>`).
const ENTRY_NAMES: [&str; 8] = ["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7"];

impl<'p> Vm<'p> {
    /// An own data property `name` of the plain object `idx`, tried at the
    /// slot `hint` first; the slot it is found at.
    #[inline]
    fn py_own_hinted(&self, idx: u32, name: &str, hint: usize) -> Option<(Value, usize)> {
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        let slot = if hint < m.len() && super::py_rt::key_eq(m.key_at(hint), name) { hint } else { m.pos(name)? };
        (!m.is_accessor_at(slot)).then(|| (m.val_at(slot), slot))
    }

    /// [`Instr::PyNew`]: the new instance, the `__init__` entry to call (or
    /// `null`) and its `this`; `None` for the slow edge.
    #[inline(never)]
    pub(super) fn py_new(&mut self, func_id: u32, ip: usize, cls: Value, rt: Value, n: usize) -> Option<(Value, Value, Value)> {
        let (&entry_name, &init_name) = (ENTRY_NAMES.get(n)?, ENTRY_NAMES.get(n + 1)?);
        if !cls.is_heap() || !self.py_plain_rec(cls) {
            return None;
        }
        let p = self.py_rt_for(rt)?;
        let (ctors, tmpl, inst_shape) = (p.ctors, p.inst_tmpl, p.inst_shape);
        let ci = cls.heap_index();
        // The class's entry for the count is the runtime's plain one.
        let want = match ctors.is_heap().then(|| self.heap.get(ctors.heap_index())) {
            Some(HeapObj::Array(items)) => *items.get(n)?,
            _ => return None,
        };
        // Where this site found the two entries last time (hints only).
        let e = self.py_ic(func_id, ip);
        let (entry_hint, init_hint) = if e.kind == ic::NEW { (e.pos as usize, e.hver as usize) } else { (0, 0) };
        let (have, entry_slot) = self.py_own_hinted(ci, entry_name, entry_hint)?;
        if !want.is_heap() || have.bits() != want.bits() {
            return None;
        }
        let init = self.py_hint_field(hint::CTOR_INIT, ci, "ctorInit")?;
        let (entry, init_slot) = if init == Value::NULL {
            if n != 0 {
                return None;
            }
            (Value::NULL, init_hint)
        } else {
            if !self.py_plain_rec(init) {
                return None;
            }
            let (e, slot) = self.py_own_hinted(init.heap_index(), init_name, init_hint)?;
            if !self.py_callable_fn(e) {
                return None;
            }
            (e, slot)
        };
        // `{ cls: this, dict: <attribute storage> }`, as the entry's literal
        // makes it, the storage presized for the class's instances.
        if inst_shape == crate::shape::DICT || !self.py_alloc_like_ok(tmpl, inst_shape) {
            return None;
        }
        let cap = self.py_rt.as_deref().map_or(0, |p| p.layouts.presize(cls));
        let map = self.py_attrs_new(cap);
        let obj = self.py_alloc_like(tmpl, inst_shape, &[cls, map])?;
        if (entry_slot, init_slot) != (entry_hint, init_hint) {
            if let (Ok(pos), Ok(hver)) = (u32::try_from(entry_slot), u32::try_from(init_slot)) {
                self.py_ic_put(func_id, ip, PyIc { kind: ic::NEW, pos, hver, ..PyIc::default() });
            }
        }
        Some((obj, entry, init))
    }

    /// `__zipp_py_smfind(cls, self, name)` with the runtime's `R` as `this`:
    /// the runtime's `smfind` fast path, natively. When `self` is a plain
    /// object that is not a type, its own data `cls` a plain object whose own
    /// data `gs` table holds for `name` (a str) a plain record `{cls, f}`
    /// with `cls` being `cls`: `R.mself = true` (an own data property `R`
    /// already has) and `f`. `undefined` for anything else, when the runtime's
    /// `smfind` answers exactly as before.
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

    fn py_smfind_hit(&mut self, cls: Value, this_self: Value, name: Value, rt: Value) -> Option<(Value, usize)> {
        if !this_self.is_heap() || !name.is_heap() {
            return None;
        }
        let slot = self.py_rt_for(rt)?.mself_slot?;
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
        let t = self.py_cls_of(this_self)?;
        if !t.is_heap() {
            return None;
        }
        let gs = self.py_hint_field(hint::GS, t.heap_index(), "gs")?;
        if !gs.is_heap() || !self.heap.is_str_like(name.heap_index()) {
            return None;
        }
        self.heap.flatten(name.heap_index());
        let hit = {
            let HeapObj::Str(s) = self.heap.get(name.heap_index()) else {
                return None;
            };
            let key = std::str::from_utf8(s.as_bytes()).ok()?;
            self.py_own_data(gs.heap_index(), key)?
        };
        if !hit.is_heap() {
            return None;
        }
        let hit_cls = self.py_hint_field(hint::HIT_CLS, hit.heap_index(), "cls")?;
        if hit_cls.bits() != cls.bits() {
            return None;
        }
        let f = self.py_hint_field(hint::HIT_F, hit.heap_index(), "f")?;
        // `R.mself = true` needs the slot `R` has for it (still a writable
        // data property named so).
        let HeapObj::Object(r) = self.heap.get(rt.heap_index()) else {
            return None;
        };
        if slot >= r.len() || r.key_at(slot) != "mself" {
            return None;
        }
        let a = r.attr_at(slot);
        if a.accessor || !a.writable {
            return None;
        }
        Some((f, slot))
    }
}
