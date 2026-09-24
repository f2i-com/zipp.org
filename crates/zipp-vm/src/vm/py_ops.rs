//! The Python frontend's fused fast paths (`Instr::PyArith`, `PyAddImm`,
//! `PyCompare`, `PyJumpCompare`).
//!
//! Python's value model maps int to BigInt, float to Number, str to string
//! and bool to boolean. For the operand pairs handled here Python's result is
//! the JavaScript operator's result (or, for `//` and `%` on ints, the floor
//! form of it), so each helper either computes exactly that or answers
//! `None`, and the instruction then jumps to the emitter's slow path, which
//! asks the Python runtime. Nothing here coerces, calls out or observes
//! anything but the two primitive operands.

#![allow(unused_imports)]
use super::*;
use crate::bytecode::{PyArithOp, PyCmpOp};
use crate::heap::HeapObj;
use crate::value::Value;
use crate::vm::helpers_misc::BigOp;

/// Magnitudes of ints that convert to a float exactly (for comparisons).
const EXACT_F64: i128 = 1 << 53;

use std::sync::atomic::{AtomicU16, Ordering};

thread_local! {
    /// [`Vm::py_map_get_at`]'s per-site entry positions (hints only: every
    /// use re-checks the key at the position, so an entry left by another
    /// program or VM on this thread is merely a miss).
    static PY_GLOBAL_SITES: std::cell::RefCell<rustc_hash::FxHashMap<u64, u32>> =
        std::cell::RefCell::new(rustc_hash::FxHashMap::default());
}

/// Whether two flat heap strings hold the same text (anything else: false).
fn py_str_eq(heap: &crate::heap::Heap, a: Value, b: Value) -> bool {
    if !a.is_heap() || !b.is_heap() {
        return false;
    }
    match (heap.get(a.heap_index()), heap.get(b.heap_index())) {
        (HeapObj::Str(x), HeapObj::Str(y)) => x.as_bytes() == y.as_bytes(),
        _ => false,
    }
}
/// Slot hints for the runtime records' fields the attribute instructions
/// read (see `Vm::py_hinted_prop`): a class's `ga` / `sa` tables and an
/// instance's `dict`. Only hints: every use re-checks the key at the slot.
static HINT_DICT: AtomicU16 = AtomicU16::new(1);
static HINT_ISTYPE: AtomicU16 = AtomicU16::new(8);
static HINT_MRO: AtomicU16 = AtomicU16::new(5);
static HINT_ISTYPES: AtomicU16 = AtomicU16::new(0);
static HINT_TSTR: AtomicU16 = AtomicU16::new(0);
static HINT_GM: AtomicU16 = AtomicU16::new(13);
static HINT_GB: AtomicU16 = AtomicU16::new(14);
static HINT_TMODULE: AtomicU16 = AtomicU16::new(0);
static HINT_GLOBALS: AtomicU16 = AtomicU16::new(3);
static HINT_TLIST: AtomicU16 = AtomicU16::new(0);
static HINT_TTUPLE: AtomicU16 = AtomicU16::new(0);
static HINT_TDICT: AtomicU16 = AtomicU16::new(0);
static HINT_TSET: AtomicU16 = AtomicU16::new(0);
static HINT_ITEMS: AtomicU16 = AtomicU16::new(1);
static HINT_SIZE: AtomicU16 = AtomicU16::new(2);
#[cfg(feature = "python")]
static HINT_SEQTMPL: AtomicU16 = AtomicU16::new(0);
static HINT_EBASE: AtomicU16 = AtomicU16::new(0);
static HINT_EXCSTACK: AtomicU16 = AtomicU16::new(0);
static HINT_GV: AtomicU16 = AtomicU16::new(21);
static HINT_DCLS: AtomicU16 = AtomicU16::new(0);
static HINT_DMAP: AtomicU16 = AtomicU16::new(1);
static HINT_DSTR: AtomicU16 = AtomicU16::new(3);
/// A class's property getter (`gp`) and setter (`sp`) caches.
static PY_FN_TABLES: [(AtomicU16, &str); 2] = [(AtomicU16::new(15), "gp"), (AtomicU16::new(16), "sp")];
const HINT_GA: usize = 0;
const HINT_SA: usize = 1;
static PY_ATTR_TABLES: [(AtomicU16, &str); 2] = [(AtomicU16::new(11), "ga"), (AtomicU16::new(12), "sa")];

/// A value as `__zipp_py_ord` orders it (Python's `<` and `==` on these are
/// side-effect free and total except for NaN).
#[cfg(feature = "python")]
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
#[cfg(feature = "python")]
pub(super) struct OrdKeyBox(OrdKey);

/// Instruction steps charged per key and per comparison of a native sort.
#[cfg(feature = "python")]
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
        use crate::bytecode::Instr;
        Ok(match *instr {
            Instr::PyClassOf { dst, obj, slow } => {
                let o = self.get(base, obj);
                match self.py_record_prop(func_id, ip, o, "cls") {
                    Some(c) if c.is_heap() && self.py_is_plain_object(c) => {
                        self.set(base, dst, c);
                        ip + 1
                    }
                    _ => slow as usize,
                }
            }
            Instr::PyDictGet { dst, obj, key, absent, slow } => {
                let o = self.get(base, obj);
                let Some(map) = self.py_record_map(func_id, ip, o) else {
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
                let func = self.func(func_id as usize);
                let key: &str = func.string_constants[name as usize].as_str();
                // `func` borrows the program, not the VM (see `Vm::func`).
                match self.py_record_prop(func_id, ip, fv, key) {
                    Some(e) if self.type_of(e) == "function" => {
                        self.set(base, dst, e);
                        ip + 1
                    }
                    _ => slow as usize,
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
                let Some(map) = self.py_record_map(func_id, ip, o) else {
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
        use crate::bytecode::Instr;
        Ok(match *instr {
            Instr::PyGlobal { dst, globals, rt, key, slow } => {
                let k = self.resolve_const_slot(func_id, key);
                let g = self.get(base, globals);
                if let Some(v) = self.py_map_get_at(g, k, func_id, ip, false) {
                    self.set(base, dst, v);
                    return Ok(ip + 1);
                }
                let r = self.get(base, rt);
                match self.py_record_prop(func_id, ip, r, "BUILTINS") {
                    Some(b) => match self.py_map_get_at(b, k, func_id, ip, true) {
                        Some(v) => {
                            self.set(base, dst, v);
                            ip + 1
                        }
                        None => slow as usize,
                    },
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
                let Some(map) = self.py_attr_plain(func_id, ip, o, key, HINT_GA) else {
                    return Ok(slow as usize);
                };
                let k = self.resolve_const_slot(func_id, key);
                match self.py_map_get(Value::heap(map), k) {
                    Some(v) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PySetAttr { obj, key, val, slow } => {
                let o = self.get(base, obj);
                let Some(map) = self.py_attr_plain(func_id, ip, o, key, HINT_SA) else {
                    return Ok(slow as usize);
                };
                let k = self.resolve_const_slot(func_id, key);
                let v = self.get(base, val);
                self.map_method(map, "set", &[k, v])?;
                ip + 1
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
            #[cfg(feature = "python")]
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
                match self.py_mod_get(func_id, ip, o, r, key) {
                    Some(v) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyLen { dst, v, rt, slow } => {
                let (vv, rv) = (self.get(base, v), self.get(base, rt));
                match self.py_len(func_id, ip, vv, rv) {
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
            #[cfg(feature = "python")]
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
                let top = self.frames.len() - 1;
                self.frames[top].ip = ip;
                self.pending_throw = Some(ev);
                return Err(Thrown(self.throw_message(ev)));
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

    /// The Array for [`Instr::PyUnpack`].
    fn py_unpack(&self, v: Value, rt: Value, n: u32) -> Option<Value> {
        if !v.is_heap() || !rt.is_heap() {
            return None;
        }
        let vi = v.heap_index();
        let cls = self.py_hinted_prop(vi, &HINT_DCLS, "cls")?;
        let r = rt.heap_index();
        let seq = self.py_hinted_prop(r, &HINT_TTUPLE, "TTUPLE")?.bits() == cls.bits()
            || self.py_hinted_prop(r, &HINT_TLIST, "TLIST")?.bits() == cls.bits();
        if !seq {
            return None;
        }
        let items = self.py_hinted_prop(vi, &HINT_ITEMS, "items")?;
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
        if !d.is_heap() || !rt.is_heap() || !k.is_heap() {
            return None;
        }
        let tdict = self.py_hinted_prop(rt.heap_index(), &HINT_TDICT, "TDICT")?;
        let di = d.heap_index();
        if self.py_hinted_prop(di, &HINT_DCLS, "cls")?.bits() != tdict.bits() {
            return None;
        }
        let map = self.py_hinted_prop(di, &HINT_DMAP, "map")?;
        let str_mode = self.py_hinted_prop(di, &HINT_DSTR, "str")?;
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

    /// The value for [`Instr::PyClassAttr`].
    fn py_class_attr(&mut self, func_id: u32, ip: usize, o: Value, key: u32) -> Option<Value> {
        let map = self.py_attr_plain(func_id, ip, o, key, HINT_GA)?;
        let k = self.resolve_const_slot(func_id, key);
        if self.py_map_get(Value::heap(map), k).is_some() {
            return None;
        }
        let cls = self.py_record_prop(func_id, ip, o, "cls")?;
        let gv = self.py_hinted_prop(cls.heap_index(), &HINT_GV, "gv")?;
        if !self.py_plain(gv) {
            return None;
        }
        let func = self.func(func_id as usize);
        let raw = func.constants[key as usize];
        let name: &str = func.string_constants
            [(raw.heap_index() & !crate::vm::helpers_misc::STRING_CONST_BIT) as usize]
            .as_str();
        let v = self.py_json_like_own(gv.heap_index(), name)?;
        (!v.is_undefined() && v != Value::HOLE).then_some(v)
    }

    /// For [`Instr::PyRaise`] / [`Instr::PyCaught`]: whether `e` is an
    /// exception record (own data `cls` a plain object whose own data `mro`
    /// Array holds `rt.EBASE`; for a raise, no own `isType` of `true`), and
    /// the slots of its own data `context` (a raise only) and `tbline`.
    fn py_exc_record(&self, e: Value, rt: Value, raise: bool) -> Option<(usize, usize)> {
        if !e.is_heap() || !rt.is_heap() {
            return None;
        }
        let ebase = self.py_hinted_prop(rt.heap_index(), &HINT_EBASE, "EBASE")?;
        let HeapObj::Object(m) = self.heap.get(e.heap_index()) else {
            return None;
        };
        if m.is_ctor {
            return None;
        }
        let own = |key: &str| m.pos(key).filter(|&s| !m.attr_at(s).accessor);
        if raise && own("isType").is_some_and(|s| m.val_at(s) == Value::TRUE) {
            return None;
        }
        let cls = m.val_at(own("cls")?);
        let tb_slot = own("tbline")?;
        let ctx_slot = if raise { own("context")? } else { 0 };
        if !cls.is_heap() || !self.py_is_plain_object(cls) {
            return None;
        }
        let mro = self.py_hinted_prop(cls.heap_index(), &HINT_MRO, "mro")?;
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
        let stack = self.py_hinted_prop(rt.heap_index(), &HINT_EXCSTACK, "EXCSTACK")?;
        if !stack.is_heap() {
            return None;
        }
        let HeapObj::Array(items) = self.heap.get(stack.heap_index()) else {
            return None;
        };
        Some((ctx, tb, items.last().copied()))
    }

    /// The record for [`Instr::PySeq`].
    #[cfg(feature = "python")]
    fn py_seq(&mut self, items: Value, rt: Value, tuple: bool) -> Option<Value> {
        if !items.is_heap() || !rt.is_heap() {
            return None;
        }
        match self.heap.get(items.heap_index()) {
            HeapObj::Array(a) if a.len() <= (1 << 24) => {}
            _ => return None,
        }
        let r = rt.heap_index();
        let cls = if tuple {
            self.py_hinted_prop(r, &HINT_TTUPLE, "TTUPLE")?
        } else {
            self.py_hinted_prop(r, &HINT_TLIST, "TLIST")?
        };
        let tmpl = self.py_hinted_prop(r, &HINT_SEQTMPL, "SEQTMPL")?;
        let (mut rec, slot) = self.py_json_template(tmpl, &["cls", "items"])?;
        rec.set_val_at(0, cls);
        rec.set_val_at(slot, items);
        Some(self.alloc_object_current_realm(rec))
    }

    /// `len(v)` for [`Instr::PyLen`].
    fn py_len(&mut self, func_id: u32, ip: usize, v: Value, rt: Value) -> Option<usize> {
        if !v.is_heap() || !rt.is_heap() {
            return None;
        }
        if self.heap.is_str_like(v.heap_index()) {
            return self.py_str_len(v);
        }
        let cls = self.py_record_prop(func_id, ip, v, "cls")?;
        let r = rt.heap_index();
        let is = |vm: &Self, hint: &AtomicU16, key: &str| vm.py_hinted_prop(r, hint, key).is_some_and(|t| t.bits() == cls.bits());
        if is(self, &HINT_TLIST, "TLIST") || is(self, &HINT_TTUPLE, "TTUPLE") {
            let items = self.py_hinted_prop(v.heap_index(), &HINT_ITEMS, "items")?;
            if !items.is_heap() {
                return None;
            }
            return match self.heap.get(items.heap_index()) {
                HeapObj::Array(a) => Some(a.len()),
                _ => None,
            };
        }
        if is(self, &HINT_TDICT, "TDICT") || is(self, &HINT_TSET, "TSET") {
            let size = self.py_hinted_prop(v.heap_index(), &HINT_SIZE, "size")?;
            if !size.is_number() {
                return None;
            }
            let n = size.as_f64();
            return (n >= 0.0 && n.fract() == 0.0 && n < 9_007_199_254_740_992.0).then_some(n as usize);
        }
        None
    }

    /// The accessor for [`Instr::PyAttrFn`].
    fn py_attr_fn(&mut self, func_id: u32, ip: usize, o: Value, key: u32, set: bool) -> Option<Value> {
        let cls = self.py_record_prop(func_id, ip, o, "cls")?;
        if !cls.is_heap() || !self.py_is_plain_object(cls) {
            return None;
        }
        let func = self.func(func_id as usize);
        let raw = func.constants[key as usize];
        if !raw.is_heap() || raw.heap_index() & crate::vm::helpers_misc::STRING_CONST_BIT == 0 {
            return None;
        }
        let name: &str = func.string_constants
            [(raw.heap_index() & !crate::vm::helpers_misc::STRING_CONST_BIT) as usize]
            .as_str();
        let (plain_hint, plain_key) = &PY_ATTR_TABLES[if set { HINT_SA } else { HINT_GA }];
        let plain = self.py_hinted_prop(cls.heap_index(), plain_hint, plain_key)?;
        if !self.py_plain(plain) || self.py_json_like_own(plain.heap_index(), name) == Some(Value::TRUE) {
            return None;
        }
        let (fn_hint, fn_key) = &PY_FN_TABLES[set as usize];
        let table = self.py_hinted_prop(cls.heap_index(), fn_hint, fn_key)?;
        if !self.py_plain(table) {
            return None;
        }
        let f = self.py_json_like_own(table.heap_index(), name)?;
        self.py_plain(f).then_some(f)
    }

    /// The module global for [`Instr::PyModGet`].
    fn py_mod_get(&mut self, func_id: u32, ip: usize, o: Value, rt: Value, key: u32) -> Option<Value> {
        let cls = self.py_record_prop(func_id, ip, o, "cls")?;
        if !rt.is_heap() || !cls.is_heap() {
            return None;
        }
        let tmodule = self.py_hinted_prop(rt.heap_index(), &HINT_TMODULE, "TMODULE")?;
        if tmodule.bits() != cls.bits() {
            return None;
        }
        let g = self.py_hinted_prop(o.heap_index(), &HINT_GLOBALS, "globals")?;
        let k = self.resolve_const_slot(func_id, key);
        self.py_map_get(g, k)
    }

    /// A plain object (a runtime record: `typeof` "object", not exotic).
    fn py_plain(&self, v: Value) -> bool {
        v.is_heap() && self.py_is_plain_object(v)
    }

    /// The method for [`Instr::PyMethod`].
    fn py_method(&mut self, func_id: u32, ip: usize, o: Value, rt: Value, key: u32, gb: u32) -> Option<Value> {
        if !o.is_heap() {
            return None;
        }
        let func = self.func(func_id as usize);
        let gb_name: &str = func.string_constants[gb as usize].as_str();
        let cls = if self.heap.is_str_like(o.heap_index()) {
            if !rt.is_heap() {
                return None;
            }
            let t = self.py_hinted_prop(rt.heap_index(), &HINT_TSTR, "TSTR")?;
            if !self.py_plain(t) {
                return None;
            }
            t
        } else {
            let cls = self.py_record_prop(func_id, ip, o, "cls")?;
            if !self.py_plain(cls) || !rt.is_heap() {
                return None;
            }
            // A module's functions are its globals (`PyModGet`).
            let tmodule = self.py_hinted_prop(rt.heap_index(), &HINT_TMODULE, "TMODULE")?;
            if tmodule.bits() == cls.bits() {
                return None;
            }
            // A user class's function, unless the instance dict shadows it.
            let gm = self.py_hinted_prop(cls.heap_index(), &HINT_GM, "gm")?;
            if !self.py_plain(gm) {
                return None;
            }
            let raw = func.constants[key as usize];
            if !raw.is_heap() || raw.heap_index() & crate::vm::helpers_misc::STRING_CONST_BIT == 0 {
                return None;
            }
            let name: &str = func.string_constants
                [(raw.heap_index() & !crate::vm::helpers_misc::STRING_CONST_BIT) as usize]
                .as_str();
            if let Some(m) = self.py_json_like_own(gm.heap_index(), name) {
                if self.py_plain(m) {
                    let d = self.py_hinted_prop(o.heap_index(), &HINT_DICT, "dict")?;
                    let k = self.resolve_const_slot(func_id, key);
                    if !d.is_heap() || !matches!(self.heap.get(d.heap_index()), HeapObj::Map { .. }) {
                        return None;
                    }
                    return match self.py_map_get(d, k) {
                        None => Some(m),
                        Some(_) => None,
                    };
                }
            }
            cls
        };
        let table = self.py_hinted_prop(cls.heap_index(), &HINT_GB, "gb")?;
        if !self.py_plain(table) {
            return None;
        }
        let b = self.py_json_like_own(table.heap_index(), gb_name)?;
        self.py_plain(b).then_some(b)
    }

    /// An own data property of the plain object `idx` (a linear lookup).
    fn py_json_like_own(&self, idx: u32, key: &str) -> Option<Value> {
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        let slot = m.pos(key)?;
        if m.attr_at(slot).accessor {
            return None;
        }
        Some(m.val_at(slot))
    }

    /// `isinstance(v, t)` for [`Instr::PyIsInstance`].
    fn py_isinstance(&mut self, func_id: u32, ip: usize, v: Value, t: Value, rt: Value) -> Option<bool> {
        if !t.is_heap() || self.py_hinted_prop(t.heap_index(), &HINT_ISTYPE, "isType")? != Value::TRUE {
            return None;
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
        if !rt.is_heap() {
            return None;
        }
        let types = self.py_hinted_prop(rt.heap_index(), &HINT_ISTYPES, "ISTYPES")?;
        if !types.is_heap() {
            return None;
        }
        let (c, bytes) = {
            let HeapObj::Array(items) = self.heap.get(types.heap_index()) else {
                return None;
            };
            let bytes = *items.get(5)?;
            match prim {
                Some(i) => (*items.get(i)?, bytes),
                None => (Value::UNDEFINED, bytes),
            }
        };
        let c = if prim.is_some() {
            c
        } else {
            let c = self.py_record_prop(func_id, ip, v, "cls")?;
            if !c.is_heap() || !self.py_is_plain_object(c) {
                return None;
            }
            c
        };
        if c.bits() == t.bits() {
            return Some(true);
        }
        if !c.is_heap() {
            return None;
        }
        let mro = self.py_hinted_prop(c.heap_index(), &HINT_MRO, "mro")?;
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
        (t.bits() != bytes.bits()).then_some(true)
    }

    /// An own data property of the plain object `idx`, found at the slot
    /// `hint` remembers for `key` when it is still there (records made by
    /// one runtime literal share their layout), else by a lookup that
    /// updates the hint.
    fn py_hinted_prop(&self, idx: u32, hint: &AtomicU16, key: &str) -> Option<Value> {
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
        if m.attr_at(slot).accessor {
            return None;
        }
        Some(m.val_at(slot))
    }

    /// For [`Instr::PyGetAttr`] / [`Instr::PySetAttr`]: `obj.dict` (its heap
    /// index) when `obj.cls[table][key]` is `true` (`table` the class's
    /// `ga` or `sa`) and `obj.dict` is a `Map`.
    fn py_attr_plain(&mut self, func_id: u32, ip: usize, o: Value, key: u32, table: usize) -> Option<u32> {
        let cls = self.py_record_prop(func_id, ip, o, "cls")?;
        if !cls.is_heap() || !self.py_is_plain_object(cls) {
            return None;
        }
        let (hint, table_key) = &PY_ATTR_TABLES[table];
        let t = self.py_hinted_prop(cls.heap_index(), hint, table_key)?;
        if !t.is_heap() {
            return None;
        }
        let func = self.func(func_id as usize);
        let raw = func.constants[key as usize];
        if !raw.is_heap() || raw.heap_index() & crate::vm::helpers_misc::STRING_CONST_BIT == 0 {
            return None;
        }
        let name: &str = func.string_constants
            [(raw.heap_index() & !crate::vm::helpers_misc::STRING_CONST_BIT) as usize]
            .as_str();
        let HeapObj::Object(m) = self.heap.get(t.heap_index()) else {
            return None;
        };
        let slot = m.pos(name)?;
        if m.attr_at(slot).accessor || m.val_at(slot) != Value::TRUE {
            return None;
        }
        let d = self.py_hinted_prop(o.heap_index(), &HINT_DICT, "dict")?;
        (d.is_heap() && matches!(self.heap.get(d.heap_index()), HeapObj::Map { .. })).then(|| d.heap_index())
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

    /// [`Vm::py_map_get`] trying first the entry position this site found
    /// last time (`builtins`: its second lookup). A position is only a hint:
    /// it is used when the entry there still has a key equal to `k`, which
    /// makes it `k`'s entry (a Map's keys are distinct).
    fn py_map_get_at(&mut self, m: Value, k: Value, func_id: u32, ip: usize, builtins: bool) -> Option<Value> {
        if !m.is_heap() {
            return None;
        }
        let site = ((func_id as u64) << 33) | ((ip as u64) << 1) | builtins as u64;
        let hint = PY_GLOBAL_SITES.with(|c| c.borrow().get(&site).copied());
        let idx = m.heap_index();
        if let Some(pos) = hint {
            if let HeapObj::Map { keys, vals } = self.heap.get(idx) {
                if let (Some(&stored), Some(&v)) = (keys.get(pos as usize), vals.get(pos as usize)) {
                    if (stored.bits() == k.bits() || py_str_eq(&self.heap, stored, k)) && !v.is_undefined() {
                        return Some(v);
                    }
                }
            }
        }
        if !matches!(self.heap.get(idx), HeapObj::Map { .. }) {
            return None;
        }
        let i = self.coll_find(idx, k)?;
        let v = match self.heap.get(idx) {
            HeapObj::Map { vals, .. } => vals.get(i).copied().filter(|v| !v.is_undefined())?,
            _ => return None,
        };
        if let Ok(pos) = u32::try_from(i) {
            PY_GLOBAL_SITES.with(|c| {
                let mut c = c.borrow_mut();
                if c.len() >= 1 << 16 {
                    c.clear();
                }
                c.insert(site, pos);
            });
        }
        Some(v)
    }

    /// `m.get(k)` for a `Map` `m` holding `k` (never `undefined`: a Python
    /// value is never `undefined`, and an `undefined` entry answers `None`
    /// like a missing one); `None` for anything else.
    #[inline]
    fn py_map_get(&mut self, m: Value, k: Value) -> Option<Value> {
        if !m.is_heap() {
            return None;
        }
        let idx = m.heap_index();
        if !matches!(self.heap.get(idx), HeapObj::Map { .. }) {
            return None;
        }
        let i = self.coll_find(idx, k)?;
        match self.heap.get(idx) {
            HeapObj::Map { vals, .. } => vals.get(i).copied().filter(|v| !v.is_undefined()),
            _ => None,
        }
    }

    /// Whether `v` is a plain object (a runtime record: not a function,
    /// array, Map, proxy or other exotic object).
    #[inline]
    fn py_is_plain_object(&self, v: Value) -> bool {
        matches!(self.heap.get(v.heap_index()), HeapObj::Object(m) if !m.is_ctor)
    }

    /// An own data property of a plain-object record, read through this
    /// site's inline cache (filled on a miss); `None` for anything else,
    /// getters included.
    fn py_record_prop(&mut self, func_id: u32, ip: usize, o: Value, key: &str) -> Option<Value> {
        if !o.is_heap() || !self.py_is_plain_object(o) {
            return None;
        }
        match self.ic_get_prop(func_id, ip, o, key) {
            GetAct::Value(v) => Some(v),
            GetAct::Accessor { .. } => None,
            GetAct::None => {
                // The cache declined: the record's own data property, if it
                // has one (never a getter).
                let HeapObj::Object(m) = self.heap.get(o.heap_index()) else {
                    return None;
                };
                let slot = m.pos(key)?;
                if m.attr_at(slot).accessor {
                    return None;
                }
                Some(m.val_at(slot))
            }
        }
    }

    /// `o.dict` when it is a `Map` (a Python instance's attribute storage).
    fn py_record_map(&mut self, func_id: u32, ip: usize, o: Value) -> Option<u32> {
        let d = self.py_record_prop(func_id, ip, o, "dict")?;
        if d.is_heap() && matches!(self.heap.get(d.heap_index()), HeapObj::Map { .. }) {
            Some(d.heap_index())
        } else {
            None
        }
    }

    /// `__zipp_py_str(s)`: whether `s` holds a UTF-16 surrogate unit (an
    /// astral character or a lone surrogate). An ASCII string answers from
    /// its flag; any other string scans its bytes (WTF-8: a surrogate pair
    /// is a four-byte sequence, a lone surrogate `ED A0..BF ..`). A
    /// non-string answers `true`, which sends every caller to its general
    /// code-point path.
    #[cfg(feature = "python")]
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
    #[cfg(feature = "python")]
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

    #[cfg(feature = "python")]
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
    #[cfg(feature = "python")]
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
                let cls = m.pos("cls").filter(|&s| !m.attr_at(s).accessor).map(|s| m.val_at(s))?;
                if cls.bits() != tuple.bits() {
                    return None;
                }
                let items = m.pos("items").filter(|&s| !m.attr_at(s).accessor).map(|s| m.val_at(s))?;
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
    #[cfg(feature = "python")]
    pub(super) fn py_ord_key_boxed(&mut self, v: Value, tuple: Value) -> Option<OrdKeyBox> {
        self.py_ord_key_rec(v, 0, tuple, false).map(OrdKeyBox)
    }

    /// Python's `a < b` on two keys, when they are ordered.
    #[cfg(feature = "python")]
    pub(super) fn py_ord_lt_boxed(&self, a: &OrdKeyBox, b: &OrdKeyBox) -> Option<bool> {
        self.py_ord_cmp(&a.0, &b.0).map(|o| o == std::cmp::Ordering::Less)
    }

    /// Python's `<` as an ordering (`None`: not ordered, or a NaN).
    #[cfg(feature = "python")]
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

    /// A plain-object record's own data property (a small record: a scan
    /// of its keys), never a getter.
    fn py_field(&self, idx: u32, key: &str) -> Option<(usize, Value)> {
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return None;
        };
        if m.is_ctor {
            return None;
        }
        let slot = m.pos(key)?;
        if m.attr_at(slot).accessor {
            return None;
        }
        Some((slot, m.val_at(slot)))
    }

    /// The record's class and which of the two expected ones it is.
    fn py_item_kind(&self, o: Value, seq: Value, dict: Value) -> Option<(u32, bool)> {
        if !o.is_heap() {
            return None;
        }
        let idx = o.heap_index();
        let (_, cls) = self.py_field(idx, "cls")?;
        if cls.bits() == seq.bits() {
            Some((idx, true))
        } else if cls.bits() == dict.bits() {
            Some((idx, false))
        } else {
            None
        }
    }

    /// `o.items` and a valid index into it for `k`.
    fn py_seq_slot(&self, idx: u32, k: Value) -> Option<(u32, usize)> {
        let (_, items) = self.py_field(idx, "items")?;
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
        let (_, flag) = self.py_field(idx, "str")?;
        if flag != Value::TRUE {
            return None;
        }
        let (_, map) = self.py_field(idx, "map")?;
        (map.is_heap() && matches!(self.heap.get(map.heap_index()), HeapObj::Map { .. })).then(|| map.heap_index())
    }

    fn py_get_item(&mut self, o: Value, k: Value, seq: Value, dict: Value) -> Result<Option<Value>, Thrown> {
        let Some((idx, is_seq)) = self.py_item_kind(o, seq, dict) else {
            return Ok(None);
        };
        if is_seq {
            let Some((items, i)) = self.py_seq_slot(idx, k) else {
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
            let Some((items, i)) = self.py_seq_slot(idx, k) else {
                return Ok(false);
            };
            self.set_index(Value::heap(items), Value::int(i as i32), v, true)?;
            return Ok(true);
        }
        let Some(map) = self.py_str_map(idx, k) else {
            return Ok(false);
        };
        let Some((size_slot, _)) = self.py_field(idx, "size") else {
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
