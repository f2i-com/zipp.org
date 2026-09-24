//! The Python frontend's fused fast paths (`Instr::PyArith`, `PyAddImm`,
//! `PyCompare`, `PyJumpCompare`, and the later rounds' `Py*`).
//!
//! Python's value model maps int to BigInt, float to Number, str to string
//! and bool to boolean. For the operand pairs handled here Python's result is
//! the JavaScript operator's result (or, for `//` and `%` on ints, the floor
//! form of it), so each helper either computes exactly that or answers
//! `None`, and the instruction then jumps to the emitter's slow path, which
//! asks the Python runtime. Nothing here coerces, calls out or observes
//! anything but the two primitive operands.
//!
//! The runtime's records and type objects are recognised through the VM's
//! registry of the runtime (`vm::py_rt`): the records' shapes, the class
//! record's field slots, and per-site caches.

use super::py_rt::{ic, PyIc, EXC_KEYS};
use super::*;
use crate::bytecode::{PyArithOp, PyCmpOp};
use crate::heap::HeapObj;
use crate::value::Value;
use crate::vm::helpers_misc::BigOp;

/// Magnitudes of ints that convert to a float exactly (for comparisons).
const EXACT_F64: i128 = 1 << 53;

/// A value as `__zipp_py_ord` orders it (Python's `<` and `==` on these are
/// side-effect free and total except for NaN).
enum OrdKey {
    Int(super::bigint::BigVal),
    Float(f64),
    /// A flattened heap string (its WTF-8 bytes order as its code points).
    Str(u32),
    None,
    /// A tuple's items (the caller passes tuples, nested ones included, as
    /// arrays of their items; never anything else as an array).
    Seq(Vec<OrdKey>),
}

/// An [`OrdKey`] for another module.
pub(super) struct OrdKeyBox(OrdKey);

/// Instruction steps charged per key and per comparison of a native sort.
const ORD_STEPS_PER_UNIT: u64 = 8;

/// One operand, classified.
#[derive(Clone, Copy)]
enum Num {
    /// A Number (a Python float).
    Float(f64),
    /// A fast-tier BigInt (a Python int).
    Int(i128),
    /// A slow-tier BigInt (a Python int beyond i128).
    BigInt,
    Other,
}

impl<'p> Vm<'p> {
    /// Execute one fused Python instruction at `ip`; the next ip. Kept out
    /// of line so the dispatch loop's own code (the JavaScript
    /// interpreter's) stays as it was around this one arm.
    #[inline(never)]
    pub(crate) fn py_step(&mut self, func_id: u32, base: usize, ip: usize, instr: &crate::bytecode::Instr) -> Result<usize, Thrown> {
        let r = self.py_step_run(func_id, base, ip, instr);
        if super::prof_py::on() {
            self.py_prof_step(func_id, ip, instr, &r);
        }
        r
    }

    #[inline(always)]
    fn py_step_run(&mut self, func_id: u32, base: usize, ip: usize, instr: &crate::bytecode::Instr) -> Result<usize, Thrown> {
        use crate::bytecode::Instr;
        Ok(match *instr {
            Instr::PyClassOf { dst, obj, slow } => {
                let o = self.get(base, obj);
                match self.py_cls_of(o) {
                    Some(c) if self.py_plain_rec(c) => {
                        self.set(base, dst, c);
                        ip + 1
                    }
                    _ => slow as usize,
                }
            }
            Instr::PyDictGet { dst, obj, key, absent, slow } => {
                let o = self.get(base, obj);
                let Some(map) = self.py_inst_map(o) else {
                    return Ok(slow as usize);
                };
                let k = self.resolve_const_slot(func_id, key);
                let v = self.map_method(map, "get", &[k])?.unwrap_or(Value::UNDEFINED);
                match (v.is_undefined(), absent) {
                    (false, false) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    (true, true) => ip + 1,
                    _ => slow as usize,
                }
            }
            Instr::PyCallEntry { dst, f, name, slow } => {
                let fv = self.get(base, f);
                match self.py_call_entry(func_id, ip, fv, name) {
                    Some(e) => {
                        self.set(base, dst, e);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyGetItem { dst, o, k, seq, dict, slow } => {
                let (ov, kv) = (self.get(base, o), self.get(base, k));
                let (sc, dc) = (self.get(base, seq), self.get(base, dict));
                match self.py_get_item(ov, kv, sc, dc)? {
                    Some(v) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PySetItem { o, k, v, seq, dict, slow } => {
                let (ov, kv, vv) = (self.get(base, o), self.get(base, k), self.get(base, v));
                let (sc, dc) = (self.get(base, seq), self.get(base, dict));
                if self.py_set_item(ov, kv, vv, sc, dc)? {
                    ip + 1
                } else {
                    slow as usize
                }
            }
            Instr::PyDictSet { obj, key, val, slow } => {
                let o = self.get(base, obj);
                let Some(map) = self.py_inst_map(o) else {
                    return Ok(slow as usize);
                };
                let k = self.resolve_const_slot(func_id, key);
                let v = self.get(base, val);
                self.map_method(map, "set", &[k, v])?;
                ip + 1
            }
            Instr::PyArith { op, dst, a, b, slow } => {
                let va = self.get(base, a);
                let vb = self.get(base, b);
                match self.py_arith(op, va, vb)? {
                    Some(r) => {
                        self.set(base, dst, r);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyAddImm { dst, a, imm, slow } => {
                let va = self.get(base, a);
                match self.py_add_imm(va, imm)? {
                    Some(r) => {
                        self.set(base, dst, r);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyCompare { op, dst, a, b, slow } => {
                let va = self.get(base, a);
                let vb = self.get(base, b);
                match self.py_compare(op, va, vb)? {
                    Some(r) => {
                        self.set(base, dst, Value::bool(r));
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyJumpCompare { op, a, b, when, target, slow } => {
                let va = self.get(base, a);
                let vb = self.get(base, b);
                match self.py_compare(op, va, vb)? {
                    Some(r) if r == when => {
                        let t = target as usize;
                        // A loop back-edge polls the GC, as `Jump` does.
                        if t < ip {
                            self.maybe_gc();
                        }
                        t
                    }
                    Some(_) => ip + 1,
                    None => slow as usize,
                }
            }
            _ => ip + 1,
        })
    }

    /// [`Vm::py_step`] for the round-2 fused instructions, in a function of
    /// their own so the round-1 arms' code is exactly as it was.
    #[inline(never)]
    pub(crate) fn py_step_ext(&mut self, func_id: u32, base: usize, ip: usize, instr: &crate::bytecode::Instr) -> Result<usize, Thrown> {
        let r = self.py_step_ext_run(func_id, base, ip, instr);
        if super::prof_py::on() {
            self.py_prof_step(func_id, ip, instr, &r);
        }
        r
    }

    #[inline(always)]
    fn py_step_ext_run(&mut self, func_id: u32, base: usize, ip: usize, instr: &crate::bytecode::Instr) -> Result<usize, Thrown> {
        use crate::bytecode::Instr;
        Ok(match *instr {
            Instr::PyGlobal { dst, globals, rt, key, slow } => {
                let (g, r) = (self.get(base, globals), self.get(base, rt));
                match self.py_global(func_id, ip, g, r, key) {
                    Some(v) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyStrItem { dst, s, k, slow } => {
                let (sv, kv) = (self.get(base, s), self.get(base, k));
                match self.py_str_item(sv, kv) {
                    Some(v) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyStrLen { dst, s, slow } => {
                let sv = self.get(base, s);
                match self.py_str_len(sv) {
                    Some(n) => {
                        let v = self.make_bigint(n as i128);
                        self.set(base, dst, v);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyGetAttr { dst, obj, key, slow } => {
                let o = self.get(base, obj);
                match self.py_attr_get(func_id, ip, o, key) {
                    Some(v) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PySetAttr { obj, key, val, slow } => {
                let o = self.get(base, obj);
                let v = self.get(base, val);
                if self.py_attr_set(func_id, ip, o, key, v)? {
                    ip + 1
                } else {
                    slow as usize
                }
            }
            Instr::PyIsInstance { dst, v, t, rt, slow } => {
                let (vv, tv, rv) = (self.get(base, v), self.get(base, t), self.get(base, rt));
                match self.py_isinstance(func_id, ip, vv, tv, rv) {
                    Some(r) => {
                        self.set(base, dst, Value::bool(r));
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyGenNext { dst, next, this, slow } => {
                let nv = self.get(base, next);
                let native = nv.is_heap()
                    && matches!(self.heap.get(nv.heap_index()), HeapObj::Native(id) if *id == super::native::PY_GEN);
                if !native {
                    return Ok(slow as usize);
                }
                let tv = self.get(base, this);
                let v = self.py_gen_next(tv)?;
                self.set(base, dst, v);
                ip + 1
            }
            Instr::PyMethod { dst, obj, rt, key, gb, slow } => {
                let (o, r) = (self.get(base, obj), self.get(base, rt));
                match self.py_method(func_id, ip, o, r, key, gb) {
                    Some(f) => {
                        self.set(base, dst, f);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyModGet { dst, obj, rt, key, slow } => {
                let (o, r) = (self.get(base, obj), self.get(base, rt));
                match self.py_mod_get(func_id, o, r, key) {
                    Some(v) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyLen { dst, v, rt, slow } => {
                let (vv, rv) = (self.get(base, v), self.get(base, rt));
                match self.py_len(vv, rv) {
                    Some(n) => {
                        let r = self.make_bigint(n as i128);
                        self.set(base, dst, r);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyAttrFn { dst, obj, key, set, slow } => {
                let o = self.get(base, obj);
                match self.py_attr_fn(func_id, ip, o, key, set) {
                    Some(f) => {
                        self.set(base, dst, f);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PySeq { dst, items, rt, tuple, slow } => {
                let (iv, rv) = (self.get(base, items), self.get(base, rt));
                match self.py_seq(iv, rv, tuple) {
                    Some(v) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyRaise { e, rt, slow } => {
                let (ev, rv) = (self.get(base, e), self.get(base, rt));
                let Some((ctx_slot, tb_slot, cur)) = self.py_raise_plan(ev, rv) else {
                    return Ok(slow as usize);
                };
                let idx = ev.heap_index();
                if let Some(cur) = cur {
                    if cur.bits() != ev.bits() {
                        if cur.is_heap() {
                            self.heap.write_barrier_val(idx, cur);
                        }
                        if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                            m.set_val_at(ctx_slot, cur);
                        }
                    }
                }
                if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                    m.set_val_at(tb_slot, Value::int(-1));
                }
                // As `Throw`: the frame's ip kept coherent, the value thrown.
                // The message is left empty: `run_loop` builds it from the
                // pending value, which nothing changes before then, when the
                // throw leaves the loop uncaught (a caught one never reads it).
                let top = self.frames.len() - 1;
                self.frames[top].ip = ip;
                self.pending_throw = Some(ev);
                return Err(Thrown(String::new()));
            }
            Instr::PyCaught { dst, e, line, rt, slow } => {
                let (ev, rv) = (self.get(base, e), self.get(base, rt));
                let Some(tb_slot) = self.py_exc_record(ev, rv, false).map(|(_, tb)| tb) else {
                    return Ok(slow as usize);
                };
                let idx = ev.heap_index();
                let tb = match self.heap.get(idx) {
                    HeapObj::Object(m) => m.val_at(tb_slot),
                    _ => return Ok(slow as usize),
                };
                if tb.is_number() && tb.as_f64() == -1.0 {
                    let lv = self.get(base, line);
                    if lv.is_heap() {
                        self.heap.write_barrier_val(idx, lv);
                    }
                    if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                        m.set_val_at(tb_slot, lv);
                    }
                }
                self.set(base, dst, ev);
                ip + 1
            }
            Instr::PyClassAttr { dst, obj, key, slow } => {
                let o = self.get(base, obj);
                match self.py_class_attr(func_id, ip, o, key) {
                    Some(v) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyDictLookup { dst, d, k, rt, absent, slow } => {
                let (dv, kv, rv) = (self.get(base, d), self.get(base, k), self.get(base, rt));
                match self.py_dict_lookup(dv, kv, rv) {
                    Some(Some(v)) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    Some(None) => absent as usize,
                    None => slow as usize,
                }
            }
            Instr::PyUnpack { dst, v, rt, n, slow } => {
                let (vv, rv) = (self.get(base, v), self.get(base, rt));
                match self.py_unpack(vv, rv, n) {
                    Some(items) => {
                        self.set(base, dst, items);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            _ => ip + 1,
        })
    }

    /// [`Vm::py_step`] for the round-3 fused instructions, in a function of
    /// their own so the earlier rounds' arms' code is exactly as it was.
    #[inline(never)]
    pub(crate) fn py_step_ext2(&mut self, func_id: u32, base: usize, ip: usize, instr: &crate::bytecode::Instr) -> Result<usize, Thrown> {
        let r = self.py_step_ext2_run(func_id, base, ip, instr);
        if super::prof_py::on() {
            self.py_prof_step(func_id, ip, instr, &r);
        }
        r
    }

    #[inline(always)]
    fn py_step_ext2_run(&mut self, func_id: u32, base: usize, ip: usize, instr: &crate::bytecode::Instr) -> Result<usize, Thrown> {
        use crate::bytecode::Instr;
        Ok(match *instr {
            Instr::PyMakeExc { dst, cls, args, rt, slow } => {
                let (c, a, r) = (self.get(base, cls), self.get(base, args), self.get(base, rt));
                match self.py_make_exc(c, a, r) {
                    Some(e) => {
                        self.set(base, dst, e);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyNew { dst, entry, this_f, cls, rt, n, slow } => {
                let (c, r) = (self.get(base, cls), self.get(base, rt));
                match self.py_new(func_id, ip, c, r, n as usize) {
                    Some((obj, e, t)) => {
                        self.set(base, dst, obj);
                        self.set(base, entry, e);
                        self.set(base, this_f, t);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyExcPop { rt, slow } => {
                let r = self.get(base, rt);
                if self.py_exc_pop(r).is_some() {
                    ip + 1
                } else {
                    slow as usize
                }
            }
            _ => ip + 1,
        })
    }

    /// [`Instr::PyCallEntry`]: the function record `f`'s own data entry
    /// `name` (`c<n>`) when it is callable. This site's entry names the
    /// record and its heap version (so its keys are as they were) and the
    /// entry's slot; the value there is read afresh.
    #[inline(never)]
    fn py_call_entry(&mut self, func_id: u32, ip: usize, f: Value, name: u32) -> Option<Value> {
        if !f.is_heap() {
            return None;
        }
        let fi = f.heap_index();
        let e = self.py_ic(func_id, ip);
        if e.kind == ic::CALL_ENTRY && e.a == f.bits() && self.heap.version_of(fi) == e.hver {
            if let HeapObj::Object(m) = self.heap.get(fi) {
                let pos = e.pos as usize;
                if pos < m.len() && !m.is_accessor_at(pos) {
                    let v = m.val_at(pos);
                    if self.py_callable_fn(v) {
                        return Some(v);
                    }
                }
            }
        }
        let key: &str = self.func(func_id as usize).string_constants[name as usize].as_str();
        // `func` borrows the program, not the VM (see `Vm::func`).
        let (v, slot) = match self.heap.get(fi) {
            HeapObj::Object(m) if !m.is_ctor => {
                let s = m.pos(key)?;
                if m.is_accessor_at(s) {
                    return None;
                }
                (m.val_at(s), s)
            }
            _ => return None,
        };
        if !self.py_callable_fn(v) {
            return None;
        }
        if let Ok(pos) = u32::try_from(slot) {
            let hver = self.heap.version_of(fi);
            self.py_ic_put(func_id, ip, PyIc { kind: ic::CALL_ENTRY, pos, hver, a: f.bits(), b: 0, c: 0, d: 0 });
        }
        Some(v)
    }

    /// Whether `e` is callable (`typeof e === "function"`).
    #[inline]
    pub(super) fn py_callable_fn(&self, e: Value) -> bool {
        if !e.is_heap() {
            return false;
        }
        match self.heap.get(e.heap_index()) {
            HeapObj::Closure { .. } | HeapObj::Func(_) | HeapObj::Bound { .. } | HeapObj::Native(_) | HeapObj::NativeClosure { .. } => true,
            HeapObj::Object(m) if !m.is_ctor => false,
            HeapObj::Str(_) | HeapObj::Cons { .. } | HeapObj::Array(_) | HeapObj::Map { .. } | HeapObj::BigInt(_) => false,
            _ => self.type_of(e) == "function",
        }
    }

    /// [`Instr::PyGlobal`]: a module global, else a builtin; `None` for the
    /// slow edge (see `vm::py_rt` for the site cache's entries).
    #[inline(never)]
    fn py_global(&mut self, func_id: u32, ip: usize, g: Value, r: Value, key: u32) -> Option<Value> {
        if !g.is_heap() {
            return None;
        }
        let gi = g.heap_index();
        let e = self.py_ic(func_id, ip);
        if e.a == g.bits() && self.heap.version_of(gi) == e.hver {
            if e.kind == ic::GLOBAL {
                if let Some(v) = self.py_map_at_bits(g, e.pos, e.b) {
                    return Some(v);
                }
            } else if e.kind == ic::BUILTIN {
                // No key appended to the globals since the name was found
                // missing there: still missing.
                let absent = matches!(self.heap.get(gi), HeapObj::Map { keys, .. } if keys.len() as u64 == e.c);
                if absent {
                    if let Some(b) = self.py_rt_for(r).map(|p| p.builtins) {
                        if let Some(v) = self.py_map_at_bits(b, e.pos, e.b) {
                            return Some(v);
                        }
                    }
                }
            }
        }
        if !matches!(self.heap.get(gi), HeapObj::Map { .. }) {
            return None;
        }
        let k = self.resolve_const_slot(func_id, key);
        // Module globals are asked for every builtin name they lack: through
        // the Map's hash index, not a scan of every global.
        self.coll_index_early(gi);
        let hver = self.heap.version_of(gi);
        match self.coll_find(gi, k) {
            Some(i) => {
                let (v, kb) = match self.heap.get(gi) {
                    HeapObj::Map { keys, vals } => (*vals.get(i)?, keys.get(i)?.bits()),
                    _ => return None,
                };
                if v.is_undefined() {
                    return None;
                }
                if let Ok(pos) = u32::try_from(i) {
                    self.py_ic_put(func_id, ip, PyIc { kind: ic::GLOBAL, pos, hver, a: g.bits(), b: kb, c: 0, d: 0 });
                }
                Some(v)
            }
            None => {
                let glen = match self.heap.get(gi) {
                    HeapObj::Map { keys, .. } => keys.len() as u64,
                    _ => return None,
                };
                let b = self.py_rt_for(r)?.builtins;
                if !b.is_heap() || !matches!(self.heap.get(b.heap_index()), HeapObj::Map { .. }) {
                    return None;
                }
                let bi = b.heap_index();
                let i = self.coll_find(bi, k)?;
                let (v, kb) = match self.heap.get(bi) {
                    HeapObj::Map { keys, vals } => (*vals.get(i)?, keys.get(i)?.bits()),
                    _ => return None,
                };
                if v.is_undefined() {
                    return None;
                }
                if let Ok(pos) = u32::try_from(i) {
                    self.py_ic_put(func_id, ip, PyIc { kind: ic::BUILTIN, pos, hver, a: g.bits(), b: kb, c: glen, d: 0 });
                }
                Some(v)
            }
        }
    }

    /// The value of the `Map` `m`'s entry at `pos` when its key there has
    /// the bits `key` (and its value is not `undefined`).
    #[inline]
    fn py_map_at_bits(&self, m: Value, pos: u32, key: u64) -> Option<Value> {
        if !m.is_heap() {
            return None;
        }
        let HeapObj::Map { keys, vals } = self.heap.get(m.heap_index()) else {
            return None;
        };
        let pos = pos as usize;
        match (keys.get(pos), vals.get(pos)) {
            (Some(k), Some(&v)) if k.bits() == key && !v.is_undefined() => Some(v),
            _ => None,
        }
    }

    /// [`Instr::PyExcPop`]: `Some` when it popped (see the instruction).
    #[inline(never)]
    fn py_exc_pop(&mut self, rt: Value) -> Option<()> {
        let stack = self.py_rt_for(rt)?.excstack;
        if !stack.is_heap() {
            return None;
        }
        let idx = stack.heap_index();
        // As `array_method`'s own `pop` path requires: no side-table
        // properties, no virtual length, the default prototype (so an
        // inherited index cannot matter), and a present last element.
        match self.heap.get(idx) {
            HeapObj::Array(items) if items.last().is_none_or(|v| !v.is_hole()) => {}
            _ => return None,
        }
        if self.arr_props.contains_key(&idx)
            || self.array_js_len.contains_key(&idx)
            || self.proto_of.contains_key(&idx)
            || self.array_proto_has_index
            || !self.array_method_is_intrinsic("pop")
        {
            return None;
        }
        if let HeapObj::Array(items) = self.heap.get_mut(idx) {
            items.pop();
        }
        // As that path does: a pop can remove a for-in snapshot's last index.
        self.heap.bump_version(idx);
        Some(())
    }

    /// Whether [`Vm::py_alloc_like`] can copy `tmpl`'s literal plan: an
    /// ordinary extensible object of the (guardable) shape `shape` whose keys
    /// are still its literal's plan.
    pub(super) fn py_alloc_like_ok(&self, tmpl: Value, shape: u32) -> bool {
        if !tmpl.is_heap() || shape == crate::shape::DICT {
            return false;
        }
        let HeapObj::Object(m) = self.heap.get(tmpl.heap_index()) else {
            return false;
        };
        if m.is_ctor || !m.extensible || m.sealed || m.frozen || m.is_raw_json || m.class.is_some() || m.shape() != shape {
            return false;
        }
        let crate::heap::PropKeys::Planned { all, visible_len } = &m.keys else {
            return false;
        };
        *visible_len == m.len() && all.len() == m.len() && (1..=crate::bytecode::FINALIZE_STAGE_SLOTS).contains(&m.len())
    }

    /// A new plain record laid out like the literal-born record `tmpl`
    /// (which [`Vm::py_alloc_like_ok`] accepts for `shape`: its static key
    /// plan and its shape, whose keys are all plain data properties, as
    /// `vm::py_rt` checked of the template), holding `vals`, allocated as
    /// that literal allocates one.
    #[inline(never)]
    pub(super) fn py_alloc_like(&mut self, tmpl: Value, shape: u32, vals: &[Value]) -> Option<Value> {
        if !self.py_alloc_like_ok(tmpl, shape) {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(tmpl.heap_index()) else {
            return None;
        };
        if m.len() != vals.len() {
            return None;
        }
        let crate::heap::PropKeys::Planned { all, .. } = &m.keys else {
            return None;
        };
        let plan = all.clone();
        let idx = self.heap.alloc_finalized(&plan, vals, shape);
        self.realm_born(idx, self.obj_proto);
        Some(Value::heap(idx))
    }

    /// The runtime's `makeExc(cls, args)` for [`Instr::PyMakeExc`] and the
    /// native `__zipp_py_exc`: the record its literal builds (see the
    /// instruction), or `None` when an input is not of the expected shape.
    #[inline(never)]
    pub(crate) fn py_make_exc(&mut self, cls: Value, args: Value, rt: Value) -> Option<Value> {
        if !self.py_plain_rec(cls) || !args.is_heap() {
            return None;
        }
        // `sequence`'s limit reads `items.length`: a dense Array's own.
        match self.heap.get(args.heap_index()) {
            HeapObj::Array(a) if a.len() <= (1 << 24) => {}
            _ => return None,
        }
        if self.array_js_len.contains_key(&args.heap_index()) {
            return None;
        }
        let p = self.py_rt_for(rt)?;
        let (tmpl, exc_shape, seq_tmpl, seq_shape, ttuple, stack) = (p.exc_tmpl, p.exc_shape, p.seq_tmpl, p.seq_shape, p.t_tuple, p.excstack);
        if !stack.is_heap() || self.array_js_len.contains_key(&stack.heap_index()) {
            return None;
        }
        // `excStack.length ? excStack[excStack.length - 1] : null`.
        let context = match self.heap.get(stack.heap_index()) {
            HeapObj::Array(items) => match items.last() {
                None => Value::NULL,
                Some(&v) if v != Value::HOLE && !v.is_undefined() => v,
                Some(_) => return None,
            },
            _ => return None,
        };
        // Both templates are checked before anything is allocated.
        if !self.py_alloc_like_ok(tmpl, exc_shape) || !self.py_alloc_like_ok(seq_tmpl, seq_shape) {
            return None;
        }
        let tuple = self.py_alloc_like(seq_tmpl, seq_shape, &[ttuple, args])?;
        let map = self.heap.alloc(HeapObj::Map { keys: Vec::new(), vals: Vec::new() });
        self.adopt_native_result_realm(map, self.map_proto);
        let vals = [cls, Value::heap(map), tuple, Value::NULL, context, Value::int(-1), Value::FALSE];
        self.py_alloc_like(tmpl, exc_shape, &vals)
    }

    /// The Array for [`Instr::PyUnpack`].
    fn py_unpack(&self, v: Value, rt: Value, n: u32) -> Option<Value> {
        let p = self.py_rt_for(rt)?;
        let (tl, tt) = (p.t_list.bits(), p.t_tuple.bits());
        let (cls, items) = self.py_seq_parts(v)?;
        if cls.bits() != tt && cls.bits() != tl {
            return None;
        }
        if !items.is_heap() {
            return None;
        }
        match self.heap.get(items.heap_index()) {
            HeapObj::Array(a) if a.len() == n as usize => Some(items),
            _ => None,
        }
    }

    /// [`Instr::PyDictLookup`]: `Some(Some(value))`, `Some(None)` for no
    /// entry, `None` for the slow edge.
    fn py_dict_lookup(&mut self, d: Value, k: Value, rt: Value) -> Option<Option<Value>> {
        if !d.is_heap() || !k.is_heap() {
            return None;
        }
        let tdict = self.py_rt_for(rt)?.t_dict;
        if self.py_cls_of(d)?.bits() != tdict.bits() {
            return None;
        }
        let di = d.heap_index();
        let (_, map) = self.py_dict_field(di, 0)?;
        let (_, str_mode) = self.py_dict_field(di, 2)?;
        if !map.is_heap() || !matches!(self.heap.get(map.heap_index()), HeapObj::Map { .. }) {
            return None;
        }
        let ki = k.heap_index();
        if str_mode == Value::TRUE {
            if !self.heap.is_str_like(ki) {
                return None;
            }
            return Some(self.py_map_get(map, k));
        }
        if str_mode != Value::FALSE {
            return None;
        }
        let HeapObj::BigInt(n) = self.heap.get(ki) else {
            return None;
        };
        let n = *n;
        if n.unsigned_abs() > 9_007_199_254_740_991 {
            return None;
        }
        let Some(bucket) = self.py_map_get(map, Value::num(n as f64)) else {
            return Some(None);
        };
        if !bucket.is_heap() {
            return None;
        }
        let HeapObj::Array(b) = self.heap.get(bucket.heap_index()) else {
            return None;
        };
        let [entry] = b.as_slice() else {
            return None;
        };
        if !entry.is_heap() {
            return None;
        }
        let HeapObj::Array(e) = self.heap.get(entry.heap_index()) else {
            return None;
        };
        let (Some(&stored), Some(&value)) = (e.first(), e.get(1)) else {
            return None;
        };
        if !stored.is_heap() || !matches!(self.heap.get(stored.heap_index()), HeapObj::BigInt(s) if *s == n) {
            return None;
        }
        (value != Value::HOLE).then_some(Some(value))
    }

    /// For [`Instr::PyRaise`] / [`Instr::PyCaught`]: whether `e` is an
    /// exception record (own data `cls` a plain object whose own data `mro`
    /// Array holds `rt.EBASE`; for a raise, no own `isType` of `true`), and
    /// the slots of its own data `context` (a raise only) and `tbline`.
    fn py_exc_record(&self, e: Value, rt: Value, raise: bool) -> Option<(usize, usize)> {
        if !e.is_heap() {
            return None;
        }
        let p = self.py_rt_for(rt)?;
        let (ebase, exc_shape, ty) = (p.ebase, p.exc_shape, p.ty?);
        let HeapObj::Object(m) = self.heap.get(e.heap_index()) else {
            return None;
        };
        if m.is_ctor {
            return None;
        }
        // A record with the layout `makeExc` builds (its shape): the slots
        // are known.
        let shape = m.shape();
        let (cls, ctx_slot, tb_slot) = if shape != crate::shape::DICT && shape == exc_shape {
            (m.val_at(0), 4, 5)
        } else {
            let own = |key: &str| m.pos(key).filter(|&s| !m.attr_at(s).accessor);
            if raise && own("isType").is_some_and(|s| m.val_at(s) == Value::TRUE) {
                return None;
            }
            let cls = m.val_at(own("cls")?);
            let tb_slot = own("tbline")?;
            let ctx_slot = if raise { own("context")? } else { 0 };
            (cls, ctx_slot, tb_slot)
        };
        debug_assert!(EXC_KEYS[4] == "context" && EXC_KEYS[5] == "tbline");
        if !self.py_plain_rec(cls) {
            return None;
        }
        let mro = self.py_ty_field(cls.heap_index(), ty.mro, "mro")?;
        if !mro.is_heap() {
            return None;
        }
        let HeapObj::Array(items) = self.heap.get(mro.heap_index()) else {
            return None;
        };
        items.iter().any(|x| x.bits() == ebase.bits()).then_some((ctx_slot, tb_slot))
    }

    /// For [`Instr::PyRaise`]: the slots to write and the current exception
    /// (`None` when `rt.EXCSTACK` is empty).
    fn py_raise_plan(&self, e: Value, rt: Value) -> Option<(usize, usize, Option<Value>)> {
        let (ctx, tb) = self.py_exc_record(e, rt, true)?;
        let stack = self.py_rt_for(rt)?.excstack;
        if !stack.is_heap() {
            return None;
        }
        let HeapObj::Array(items) = self.heap.get(stack.heap_index()) else {
            return None;
        };
        Some((ctx, tb, items.last().copied()))
    }

    /// The record for [`Instr::PySeq`].
    fn py_seq(&mut self, items: Value, rt: Value, tuple: bool) -> Option<Value> {
        if !items.is_heap() {
            return None;
        }
        match self.heap.get(items.heap_index()) {
            HeapObj::Array(a) if a.len() <= (1 << 24) => {}
            _ => return None,
        }
        let p = self.py_rt_for(rt)?;
        let cls = if tuple { p.t_tuple } else { p.t_list };
        let (tmpl, shape) = (p.seq_tmpl, p.seq_shape);
        self.py_alloc_like(tmpl, shape, &[cls, items])
    }

    /// `len(v)` for [`Instr::PyLen`].
    fn py_len(&mut self, v: Value, rt: Value) -> Option<usize> {
        if !v.is_heap() {
            return None;
        }
        if self.heap.is_str_like(v.heap_index()) {
            return self.py_str_len(v);
        }
        let p = self.py_rt_for(rt)?;
        let (tl, tt, td, ts) = (p.t_list.bits(), p.t_tuple.bits(), p.t_dict.bits(), p.t_set.bits());
        let cls = self.py_cls_of(v)?.bits();
        if cls == tl || cls == tt {
            let (_, items) = self.py_seq_parts(v)?;
            if !items.is_heap() {
                return None;
            }
            return match self.heap.get(items.heap_index()) {
                HeapObj::Array(a) => Some(a.len()),
                _ => None,
            };
        }
        if cls == td || cls == ts {
            let (_, size) = self.py_dict_field(v.heap_index(), 1)?;
            if !size.is_number() {
                return None;
            }
            let n = size.as_f64();
            return (n >= 0.0 && n.fract() == 0.0 && n < 9_007_199_254_740_992.0).then_some(n as usize);
        }
        None
    }

    /// The module global for [`Instr::PyModGet`].
    fn py_mod_get(&mut self, func_id: u32, o: Value, rt: Value, key: u32) -> Option<Value> {
        let tmodule = self.py_rt_for(rt)?.t_module;
        let cls = self.py_cls_of(o)?;
        if tmodule.bits() != cls.bits() {
            return None;
        }
        let g = self.py_hint_field(super::py_rt::hint::GLOBALS, o.heap_index(), "globals")?;
        let k = self.resolve_const_slot(func_id, key);
        self.py_map_get(g, k)
    }

    /// `isinstance(v, t)` for [`Instr::PyIsInstance`].
    #[inline(never)]
    fn py_isinstance(&mut self, func_id: u32, ip: usize, v: Value, t: Value, rt: Value) -> Option<bool> {
        if !t.is_heap() {
            return None;
        }
        let p = self.py_rt_for(rt)?;
        let (ty, istypes) = (p.ty?, p.istypes);
        let ti = t.heap_index();
        // `t` a type: this site's entry, else its own `isType`.
        let e = self.py_ic(func_id, ip);
        if !(e.kind == ic::IS_TYPE && e.a == t.bits() && self.heap.version_of(ti) == e.hver) {
            if self.py_ty_field(ti, ty.is_type, "isType")? != Value::TRUE {
                return None;
            }
            let hver = self.heap.version_of(ti);
            self.py_ic_put(func_id, ip, PyIc { kind: ic::IS_TYPE, pos: 0, hver, a: t.bits(), b: 0, c: 0, d: 0 });
        }
        let prim = if v == Value::NULL {
            Some(0)
        } else if v == Value::TRUE || v == Value::FALSE {
            Some(1)
        } else if v.is_number() {
            Some(3)
        } else if !v.is_heap() {
            return None;
        } else if self.heap.is_str_like(v.heap_index()) {
            Some(4)
        } else if matches!(self.heap.get(v.heap_index()), HeapObj::BigInt(_) | HeapObj::BigIntBig(_)) {
            Some(2)
        } else {
            None
        };
        let c = match prim {
            Some(i) => istypes[i],
            None => {
                let c = self.py_cls_of(v)?;
                if !self.py_plain_rec(c) {
                    return None;
                }
                c
            }
        };
        if c.bits() == t.bits() {
            return Some(true);
        }
        if !c.is_heap() {
            return None;
        }
        let mro = self.py_ty_field(c.heap_index(), ty.mro, "mro")?;
        if !mro.is_heap() {
            return None;
        }
        let HeapObj::Array(items) = self.heap.get(mro.heap_index()) else {
            return None;
        };
        if !items.iter().any(|x| x.bits() == t.bits()) {
            return Some(false);
        }
        // `bytes` excludes bytearray, which the runtime models as a subclass.
        (t.bits() != istypes[5].bits()).then_some(true)
    }

    /// `s[k]` for [`Instr::PyStrItem`].
    fn py_str_item(&mut self, s: Value, k: Value) -> Option<Value> {
        if !s.is_heap() || !k.is_heap() || !self.heap.is_str_like(s.heap_index()) {
            return None;
        }
        let HeapObj::BigInt(i) = self.heap.get(k.heap_index()) else {
            return None;
        };
        let i = *i;
        let idx = s.heap_index();
        self.heap.flatten(idx);
        let HeapObj::Str(st) = self.heap.get(idx) else {
            return None;
        };
        if !st.is_ascii() {
            return None;
        }
        let bytes = st.as_bytes();
        let n = bytes.len() as i128;
        let at = if i < 0 { i + n } else { i };
        if at < 0 || at >= n {
            return None;
        }
        // A one-character ASCII str is the interned string at its byte.
        Some(Value::heap(bytes[at as usize] as u32))
    }

    /// `len(s)` for [`Instr::PyStrLen`]: the code points of a str with no
    /// lone surrogate (as the runtime's `strLen` counts them).
    fn py_str_len(&mut self, s: Value) -> Option<usize> {
        if !s.is_heap() || !self.heap.is_str_like(s.heap_index()) {
            return None;
        }
        let idx = s.heap_index();
        self.heap.flatten(idx);
        let HeapObj::Str(st) = self.heap.get(idx) else {
            return None;
        };
        let bytes = st.as_bytes();
        if st.is_ascii() {
            return Some(bytes.len());
        }
        if !crate::heap::wtf8_is_wellformed(bytes) {
            return None;
        }
        Some(bytes.iter().filter(|&&b| b & 0xC0 != 0x80).count())
    }

    /// [`Vm::py_map_get`] with the entry's position.
    pub(super) fn py_map_find(&mut self, m: Value, k: Value) -> Option<(usize, Value)> {
        if !m.is_heap() {
            return None;
        }
        let idx = m.heap_index();
        if !matches!(self.heap.get(idx), HeapObj::Map { .. }) {
            return None;
        }
        let i = self.coll_find(idx, k)?;
        match self.heap.get(idx) {
            HeapObj::Map { vals, .. } => vals.get(i).copied().filter(|v| !v.is_undefined()).map(|v| (i, v)),
            _ => None,
        }
    }

    /// `m.get(k)` for a `Map` `m` holding `k` (never `undefined`: a Python
    /// value is never `undefined`, and an `undefined` entry answers `None`
    /// like a missing one); `None` for anything else.
    #[inline]
    pub(super) fn py_map_get(&mut self, m: Value, k: Value) -> Option<Value> {
        self.py_map_find(m, k).map(|(_, v)| v)
    }

    /// `o.dict` when it is a `Map` (a Python instance's attribute storage).
    fn py_inst_map(&self, o: Value) -> Option<u32> {
        let (_, d) = self.py_inst_parts(o)?;
        (d.is_heap() && matches!(self.heap.get(d.heap_index()), HeapObj::Map { .. })).then(|| d.heap_index())
    }

    /// `__zipp_py_str(s)`: whether `s` holds a UTF-16 surrogate unit (an
    /// astral character or a lone surrogate). An ASCII string answers from
    /// its flag; any other string scans its bytes (WTF-8: a surrogate pair
    /// is a four-byte sequence, a lone surrogate `ED A0..BF ..`). A
    /// non-string answers `true`, which sends every caller to its general
    /// code-point path.
    pub(crate) fn py_str_has_surrogate(&mut self, v: Value) -> Value {
        if !v.is_heap() || !self.heap.is_str_like(v.heap_index()) {
            return Value::bool(true);
        }
        let idx = v.heap_index();
        self.heap.flatten(idx);
        let HeapObj::Str(s) = self.heap.get(idx) else {
            return Value::bool(true);
        };
        if s.is_ascii() {
            return Value::bool(false);
        }
        let bytes = s.as_bytes();
        let found = bytes
            .iter()
            .enumerate()
            .any(|(i, &b)| b >= 0xF0 || (b == 0xED && bytes.get(i + 1).is_some_and(|&n| n >= 0xA0)));
        Value::bool(found)
    }

    /// `__zipp_py_ord(0, a, b)`: -1, 0 or 1 as `a` orders against `b`;
    /// `__zipp_py_ord(1, keys)`: a stable sort of `keys` as the permutation
    /// of indices that sorts it. `undefined` when a key is not a plain value
    /// (an array stands for a tuple's items, at the top level only), when a
    /// pair is not ordered (a str against a number, None against anything,
    /// a NaN), or when the budget cannot cover the sort.
    pub(crate) fn py_ord(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let op = args.first().copied().unwrap_or(Value::UNDEFINED);
        let a = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        // Ops 2 and 3 are ops 1 and 0 with tuples as the runtime's tuple
        // records (`tuple` the tuple type, the last argument), read here
        // instead of converted by the caller first.
        let records = op == Value::int(2) || op == Value::int(3);
        let tuple = if op == Value::int(2) {
            args.get(2).copied()
        } else if op == Value::int(3) {
            args.get(3).copied()
        } else {
            None
        };
        if op == Value::int(0) || op == Value::int(3) {
            let b = args.get(2).copied().unwrap_or(Value::UNDEFINED);
            let (x, y) = if records {
                let Some(t) = tuple else {
                    return Ok(Value::UNDEFINED);
                };
                (self.py_ord_key_rec(a, 0, t, true), self.py_ord_key_rec(b, 0, t, true))
            } else {
                (self.py_ord_key(a, 0), self.py_ord_key(b, 0))
            };
            let (Some(x), Some(y)) = (x, y) else {
                return Ok(Value::UNDEFINED);
            };
            return Ok(match self.py_ord_cmp(&x, &y) {
                Some(o) => Value::int(o as i32),
                None => Value::UNDEFINED,
            });
        }
        if (op != Value::int(1) && op != Value::int(2)) || !a.is_heap() {
            return Ok(Value::UNDEFINED);
        }
        let values: Vec<Value> = match self.heap.get(a.heap_index()) {
            HeapObj::Array(items) => items.clone(),
            _ => return Ok(Value::UNDEFINED),
        };
        let n = values.len();
        let log = (usize::BITS - n.leading_zeros()) as u64;
        let cost = (n as u64).saturating_mul(log + 1).saturating_mul(ORD_STEPS_PER_UNIT);
        if !self.native_kernel_admits(cost, n.saturating_mul(64)) {
            return Ok(Value::UNDEFINED);
        }
        let mut keys = Vec::with_capacity(n);
        for v in values {
            let key = match tuple {
                Some(t) => self.py_ord_key_rec(v, 0, t, false),
                None if records => None,
                None => self.py_ord_key(v, 0),
            };
            match key {
                Some(k) => keys.push(k),
                None => return Ok(Value::UNDEFINED),
            }
        }
        let mut order: Vec<usize> = (0..n).collect();
        let undecided = std::cell::Cell::new(false);
        order.sort_by(|&i, &j| match self.py_ord_cmp(&keys[i], &keys[j]) {
            Some(o) => o,
            None => {
                undecided.set(true);
                std::cmp::Ordering::Equal
            }
        });
        if undecided.get() {
            return Ok(Value::UNDEFINED);
        }
        self.charge_steps(i64::try_from(cost).unwrap_or(i64::MAX));
        let out: Vec<Value> = order.into_iter().map(|i| Value::int(i as i32)).collect();
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
    }

    fn py_ord_key(&mut self, v: Value, depth: u32) -> Option<OrdKey> {
        use super::bigint::BigVal;
        if v.is_number() {
            return Some(OrdKey::Float(v.as_f64()));
        }
        if v == Value::TRUE {
            return Some(OrdKey::Int(BigVal::Small(1)));
        }
        if v == Value::FALSE {
            return Some(OrdKey::Int(BigVal::Small(0)));
        }
        if v == Value::NULL {
            return Some(OrdKey::None);
        }
        if !v.is_heap() {
            return None;
        }
        let idx = v.heap_index();
        if self.heap.is_str_like(idx) {
            self.heap.flatten(idx);
            return matches!(self.heap.get(idx), HeapObj::Str(_)).then_some(OrdKey::Str(idx));
        }
        match self.heap.get(idx) {
            HeapObj::BigInt(n) => Some(OrdKey::Int(BigVal::Small(*n))),
            HeapObj::BigIntBig(b) => Some(OrdKey::Int(BigVal::Big((**b).clone()))),
            HeapObj::Array(items) if depth < 16 => {
                let items = items.clone();
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.py_ord_key(item, depth + 1)?);
                }
                Some(OrdKey::Seq(out))
            }
            _ => None,
        }
    }

    /// [`Vm::py_ord_key`] for ops 2 and 3: a tuple is a runtime tuple
    /// record (an own data `cls` that is `tuple`, its items the own data
    /// `items` Array), taken at nesting depth 8 at most, as the runtime's
    /// `ordForm` takes one; with `top`, a bare Array stands for the items
    /// of a list or tuple at the top level (op 3), and never below it.
    fn py_ord_key_rec(&mut self, v: Value, depth: u32, tuple: Value, top: bool) -> Option<OrdKey> {
        if !v.is_heap() {
            return self.py_ord_key(v, depth);
        }
        let idx = v.heap_index();
        let items = match self.heap.get(idx) {
            HeapObj::Array(items) if top => items.clone(),
            HeapObj::Array(_) => return None,
            HeapObj::Object(m) if !m.is_ctor => {
                if depth > 8 {
                    return None;
                }
                let (cls, items) = self.py_seq_parts(v)?;
                if cls.bits() != tuple.bits() {
                    return None;
                }
                if !items.is_heap() {
                    return None;
                }
                match self.heap.get(items.heap_index()) {
                    HeapObj::Array(items) => items.clone(),
                    _ => return None,
                }
            }
            _ => return self.py_ord_key(v, depth),
        };
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            out.push(self.py_ord_key_rec(item, depth + 1, tuple, false)?);
        }
        Some(OrdKey::Seq(out))
    }

    /// A value's key as op 2 reads it, for `vm::py_str`'s heap operations.
    pub(super) fn py_ord_key_boxed(&mut self, v: Value, tuple: Value) -> Option<OrdKeyBox> {
        self.py_ord_key_rec(v, 0, tuple, false).map(OrdKeyBox)
    }

    /// Python's `a < b` on two keys, when they are ordered.
    pub(super) fn py_ord_lt_boxed(&self, a: &OrdKeyBox, b: &OrdKeyBox) -> Option<bool> {
        self.py_ord_cmp(&a.0, &b.0).map(|o| o == std::cmp::Ordering::Less)
    }

    /// Python's `<` as an ordering (`None`: not ordered, or a NaN).
    fn py_ord_cmp(&self, a: &OrdKey, b: &OrdKey) -> Option<std::cmp::Ordering> {
        use super::bigint::BigVal;
        use std::cmp::Ordering;
        match (a, b) {
            (OrdKey::Int(x), OrdKey::Int(y)) => Some(match (x, y) {
                (BigVal::Small(x), BigVal::Small(y)) => x.cmp(y),
                // A slow-tier value is beyond i128: its sign decides.
                (BigVal::Small(_), BigVal::Big(y)) => {
                    if y.sign() == num_bigint::Sign::Minus {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                }
                (BigVal::Big(x), BigVal::Small(_)) => {
                    if x.sign() == num_bigint::Sign::Minus {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                }
                (BigVal::Big(x), BigVal::Big(y)) => x.cmp(y),
            }),
            (OrdKey::Int(x), OrdKey::Float(f)) => x.cmp_f64(*f),
            (OrdKey::Float(f), OrdKey::Int(x)) => x.cmp_f64(*f).map(Ordering::reverse),
            (OrdKey::Float(x), OrdKey::Float(y)) => x.partial_cmp(y),
            (OrdKey::Str(x), OrdKey::Str(y)) => {
                let (HeapObj::Str(sx), HeapObj::Str(sy)) = (self.heap.get(*x), self.heap.get(*y)) else {
                    return None;
                };
                Some(sx.as_bytes().cmp(sy.as_bytes()))
            }
            (OrdKey::Seq(xs), OrdKey::Seq(ys)) => {
                for (x, y) in xs.iter().zip(ys.iter()) {
                    // The first unequal pair decides, by `<`.
                    let equal = match (x, y) {
                        (OrdKey::None, OrdKey::None) => true,
                        (OrdKey::None, _) | (_, OrdKey::None) => false,
                        // A tuple never equals a number or a str.
                        (OrdKey::Seq(_), OrdKey::Seq(_)) => match self.py_ord_cmp(x, y) {
                            Some(o) => o == Ordering::Equal,
                            None => return None,
                        },
                        (OrdKey::Seq(_), _) | (_, OrdKey::Seq(_)) => false,
                        _ => match self.py_ord_cmp(x, y) {
                            Some(o) => o == Ordering::Equal,
                            // A NaN, or a str against a number: `==` is
                            // false and `<` is the general protocol's.
                            None => return None,
                        },
                    };
                    if !equal {
                        return self.py_ord_cmp(x, y);
                    }
                }
                Some(xs.len().cmp(&ys.len()))
            }
            _ => None,
        }
    }

    /// The record's class and which of the two expected ones it is.
    fn py_item_kind(&self, o: Value, seq: Value, dict: Value) -> Option<(u32, bool)> {
        let cls = self.py_cls_of(o)?;
        if cls.bits() == seq.bits() {
            Some((o.heap_index(), true))
        } else if cls.bits() == dict.bits() {
            Some((o.heap_index(), false))
        } else {
            None
        }
    }

    /// `o.items` and a valid index into it for `k`.
    fn py_seq_slot(&self, o: Value, k: Value) -> Option<(u32, usize)> {
        let (_, items) = self.py_seq_parts(o)?;
        if !items.is_heap() || !k.is_heap() {
            return None;
        }
        let HeapObj::BigInt(n) = self.heap.get(k.heap_index()) else {
            return None;
        };
        let HeapObj::Array(vals) = self.heap.get(items.heap_index()) else {
            return None;
        };
        let i = usize::try_from(*n).ok().filter(|&i| i < vals.len())?;
        Some((items.heap_index(), i))
    }

    /// `o.map` of a str-keyed dict record, for a str `k`.
    fn py_str_map(&self, idx: u32, k: Value) -> Option<u32> {
        if !k.is_heap() || !self.heap.is_str_like(k.heap_index()) {
            return None;
        }
        let (_, flag) = self.py_dict_field(idx, 2)?;
        if flag != Value::TRUE {
            return None;
        }
        let (_, map) = self.py_dict_field(idx, 0)?;
        (map.is_heap() && matches!(self.heap.get(map.heap_index()), HeapObj::Map { .. })).then(|| map.heap_index())
    }

    fn py_get_item(&mut self, o: Value, k: Value, seq: Value, dict: Value) -> Result<Option<Value>, Thrown> {
        let Some((idx, is_seq)) = self.py_item_kind(o, seq, dict) else {
            return Ok(None);
        };
        if is_seq {
            let Some((items, i)) = self.py_seq_slot(o, k) else {
                return Ok(None);
            };
            let HeapObj::Array(vals) = self.heap.get(items) else {
                return Ok(None);
            };
            let v = vals[i];
            return Ok((v != Value::HOLE && !v.is_undefined()).then_some(v));
        }
        let Some(map) = self.py_str_map(idx, k) else {
            return Ok(None);
        };
        let v = self.map_method(map, "get", &[k])?.unwrap_or(Value::UNDEFINED);
        Ok((!v.is_undefined()).then_some(v))
    }

    fn py_set_item(&mut self, o: Value, k: Value, v: Value, seq: Value, dict: Value) -> Result<bool, Thrown> {
        let Some((idx, is_seq)) = self.py_item_kind(o, seq, dict) else {
            return Ok(false);
        };
        if is_seq {
            let Some((items, i)) = self.py_seq_slot(o, k) else {
                return Ok(false);
            };
            self.set_index(Value::heap(items), Value::int(i as i32), v, true)?;
            return Ok(true);
        }
        let Some(map) = self.py_str_map(idx, k) else {
            return Ok(false);
        };
        let Some((size_slot, _)) = self.py_dict_field(idx, 1) else {
            return Ok(false);
        };
        self.map_method(map, "set", &[k, v])?;
        let n = self.coll_live_len(map);
        if let HeapObj::Object(m) = self.heap.get_mut(idx) {
            m.set_val_at(size_slot, Value::num(n as f64));
        }
        Ok(true)
    }

    #[inline]
    fn py_num(&self, v: Value) -> Num {
        if v.is_number() {
            return Num::Float(v.as_f64());
        }
        if v.is_heap() {
            match self.heap.get(v.heap_index()) {
                HeapObj::BigInt(n) => return Num::Int(*n),
                HeapObj::BigIntBig(_) => return Num::BigInt,
                _ => {}
            }
        }
        Num::Other
    }

    /// `a <op> b` for [`Instr::PyArith`]; `None` sends the instruction to
    /// its slow path.
    #[inline(always)] // one copy in the step, one in its JIT helper (`engine/py_jit.rs`)
    pub(crate) fn py_arith(&mut self, op: PyArithOp, va: Value, vb: Value) -> Result<Option<Value>, Thrown> {
        // Two floats: exactly the VM's own Number arithmetic (the int-tagged
        // fast forms included), as the inline code this replaces used.
        if va.is_number() && vb.is_number() {
            return Ok(match op {
                PyArithOp::Add => Some(self.add_values(va, vb)?),
                PyArithOp::Sub => Some(if va.is_int() && vb.is_int() {
                    match va.as_int().checked_sub(vb.as_int()) {
                        Some(v) => Value::int(v),
                        None => Value::num(va.as_int() as f64 - vb.as_int() as f64),
                    }
                } else {
                    Value::num(va.as_f64() - vb.as_f64())
                }),
                PyArithOp::Mul => Some(if va.is_int() && vb.is_int() {
                    let (ia, ib) = (va.as_int(), vb.as_int());
                    match ia.checked_mul(ib) {
                        // A zero product with a negative operand is -0.0.
                        Some(v) if v != 0 || (ia | ib) >= 0 => Value::int(v),
                        _ => Value::num(ia as f64 * ib as f64),
                    }
                } else {
                    Value::num(va.as_f64() * vb.as_f64())
                }),
                PyArithOp::TrueDiv => {
                    let y = vb.as_f64();
                    if y == 0.0 {
                        None
                    } else {
                        Some(Value::num(va.as_f64() / y))
                    }
                }
                _ => None,
            });
        }
        match (self.py_num(va), self.py_num(vb)) {
            (Num::Int(x), Num::Int(y)) => Ok(match op {
                PyArithOp::Add | PyArithOp::Sub | PyArithOp::Mul => {
                    let r = match op {
                        PyArithOp::Add => x.checked_add(y),
                        PyArithOp::Sub => x.checked_sub(y),
                        _ => x.checked_mul(y),
                    };
                    match r {
                        Some(r) => Some(self.make_bigint(r)),
                        None => Some(self.numeric_binop(big_op(op), va, vb)?),
                    }
                }
                PyArithOp::BitAnd => Some(self.make_bigint(x & y)),
                PyArithOp::BitOr => Some(self.make_bigint(x | y)),
                PyArithOp::BitXor => Some(self.make_bigint(x ^ y)),
                PyArithOp::TrueDiv => {
                    if y == 0 || x.unsigned_abs() >= EXACT_F64 as u128 || y.unsigned_abs() >= EXACT_F64 as u128 {
                        None
                    } else {
                        Some(Value::num(x as f64 / y as f64))
                    }
                }
                PyArithOp::FloorDiv => {
                    if y == 0 {
                        return Ok(None);
                    }
                    match (x.checked_div(y), x.checked_rem(y)) {
                        (Some(q), Some(r)) => {
                            let q = if r != 0 && ((r < 0) != (y < 0)) { q - 1 } else { q };
                            Some(self.make_bigint(q))
                        }
                        _ => None,
                    }
                }
                PyArithOp::Mod => {
                    if y == 0 {
                        return Ok(None);
                    }
                    match x.checked_rem(y) {
                        Some(r) => {
                            let r = if r != 0 && ((r < 0) != (y < 0)) { r + y } else { r };
                            Some(self.make_bigint(r))
                        }
                        None => None,
                    }
                }
            }),
            // Ints of either tier: the BigInt operators (no floor division
            // or true division here).
            (Num::Int(_) | Num::BigInt, Num::Int(_) | Num::BigInt) => Ok(match op {
                PyArithOp::Add
                | PyArithOp::Sub
                | PyArithOp::Mul
                | PyArithOp::BitAnd
                | PyArithOp::BitOr
                | PyArithOp::BitXor => Some(self.numeric_binop(big_op(op), va, vb)?),
                _ => None,
            }),
            // An int and a float: the int as a float (exact rounding, as
            // `float(int)`), then the float operation.
            (Num::Int(x), Num::Float(y)) => Ok(mixed_float(op, x as f64, y)),
            (Num::Float(x), Num::Int(y)) => Ok(mixed_float(op, x, y as f64)),
            _ => Ok(None),
        }
    }

    /// `a + imm` for [`Instr::PyAddImm`].
    #[inline(always)] // one copy in the step, one in its JIT helper (`engine/py_jit.rs`)
    pub(crate) fn py_add_imm(&mut self, va: Value, imm: i32) -> Result<Option<Value>, Thrown> {
        if va.is_int() {
            return Ok(Some(match va.as_int().checked_add(imm) {
                Some(v) => Value::int(v),
                None => Value::num(va.as_int() as f64 + imm as f64),
            }));
        }
        if va.is_number() {
            return Ok(Some(Value::num(va.as_f64() + imm as f64)));
        }
        match self.py_num(va) {
            Num::Int(x) => Ok(Some(match x.checked_add(imm as i128) {
                Some(r) => self.make_bigint(r),
                None => {
                    let b = self.make_bigint(imm as i128);
                    self.numeric_binop(BigOp::Add, va, b)?
                }
            })),
            Num::BigInt => {
                let b = self.make_bigint(imm as i128);
                Ok(Some(self.numeric_binop(BigOp::Add, va, b)?))
            }
            _ => Ok(None),
        }
    }

    /// `a <op> b` for [`Instr::PyCompare`] / [`Instr::PyJumpCompare`].
    #[inline(always)] // one copy in the step, one in its JIT helper (`engine/py_jit.rs`)
    pub(crate) fn py_compare(&mut self, op: PyCmpOp, va: Value, vb: Value) -> Result<Option<bool>, Thrown> {
        if va.is_int() && vb.is_int() {
            return Ok(Some(order_holds(op, va.as_int().cmp(&vb.as_int()))));
        }
        match (self.py_num(va), self.py_num(vb)) {
            (Num::Float(x), Num::Float(y)) => Ok(Some(float_holds(op, x, y))),
            (Num::Int(x), Num::Int(y)) => Ok(Some(order_holds(op, x.cmp(&y)))),
            (Num::Int(_) | Num::BigInt, Num::Int(_) | Num::BigInt) => {
                // A slow-tier BigInt is beyond i128: the BigInt comparisons.
                let lt = match op {
                    PyCmpOp::Eq => return Ok(Some(self.values_strict_eq(va, vb))),
                    PyCmpOp::Ne => return Ok(Some(!self.values_strict_eq(va, vb))),
                    PyCmpOp::Lt => self.cmp_lt_values(va, vb, true)?,
                    PyCmpOp::Gt => self.cmp_lt_values(vb, va, false)?,
                    PyCmpOp::Le => !self.cmp_lt_values(vb, va, false)?,
                    PyCmpOp::Ge => !self.cmp_lt_values(va, vb, true)?,
                };
                Ok(Some(lt))
            }
            (Num::Int(x), Num::Float(y)) if x.unsigned_abs() <= EXACT_F64 as u128 => {
                Ok(Some(float_holds(op, x as f64, y)))
            }
            (Num::Float(x), Num::Int(y)) if y.unsigned_abs() <= EXACT_F64 as u128 => {
                Ok(Some(float_holds(op, x, y as f64)))
            }
            (Num::Other, Num::Other) if matches!(op, PyCmpOp::Eq | PyCmpOp::Ne) => {
                if va.is_heap()
                    && vb.is_heap()
                    && self.heap.is_str_like(va.heap_index())
                    && self.heap.is_str_like(vb.heap_index())
                {
                    let eq = self.values_strict_eq(va, vb);
                    Ok(Some(if op == PyCmpOp::Eq { eq } else { !eq }))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }
}

fn big_op(op: PyArithOp) -> BigOp {
    match op {
        PyArithOp::Add => BigOp::Add,
        PyArithOp::Sub => BigOp::Sub,
        PyArithOp::Mul => BigOp::Mul,
        PyArithOp::BitAnd => BigOp::And,
        PyArithOp::BitOr => BigOp::Or,
        PyArithOp::BitXor => BigOp::Xor,
        PyArithOp::TrueDiv => BigOp::Div,
        PyArithOp::FloorDiv => BigOp::Div,
        PyArithOp::Mod => BigOp::Mod,
    }
}

/// The float operation on an int converted to float and a float.
fn mixed_float(op: PyArithOp, x: f64, y: f64) -> Option<Value> {
    match op {
        PyArithOp::Add => Some(Value::num(x + y)),
        PyArithOp::Sub => Some(Value::num(x - y)),
        PyArithOp::Mul => Some(Value::num(x * y)),
        PyArithOp::TrueDiv if y != 0.0 => Some(Value::num(x / y)),
        _ => None,
    }
}

fn order_holds(op: PyCmpOp, o: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::*;
    match op {
        PyCmpOp::Lt => o == Less,
        PyCmpOp::Le => o != Greater,
        PyCmpOp::Gt => o == Greater,
        PyCmpOp::Ge => o != Less,
        PyCmpOp::Eq => o == Equal,
        PyCmpOp::Ne => o != Equal,
    }
}

/// IEEE comparison: every ordering and `==` is false with a NaN, `!=` true.
fn float_holds(op: PyCmpOp, x: f64, y: f64) -> bool {
    match op {
        PyCmpOp::Lt => x < y,
        PyCmpOp::Le => x <= y,
        PyCmpOp::Gt => x > y,
        PyCmpOp::Ge => x >= y,
        PyCmpOp::Eq => x == y,
        PyCmpOp::Ne => x != y,
    }
}
