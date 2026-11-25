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

/// A value as [`OrdKey`] reads one, with no key built: what it is and where
/// it lives, for two values compared once (`Vm::py_ord_lt_values`). A sort
/// builds each key once and compares it many times; a heap operation
/// compares a few pairs, and a key per pair cost two allocations for every
/// tuple in it.
#[derive(Clone, Copy)]
enum OrdRef {
    Int(i128),
    /// A slow-tier int (beyond i128): its heap slot.
    Big(u32),
    Float(f64),
    /// A flattened heap string.
    Str(u32),
    None,
    /// A tuple record's `items` Array.
    Seq(u32),
}

/// Instruction steps charged per key and per comparison of a native sort.
const ORD_STEPS_PER_UNIT: u64 = 8;

/// The most items (columns) of a tuple key `Vm::py_ord_sort_columns`
/// sorts by columns, nested tuples' items counted.
const ORD_COLUMNS_MAX: usize = 6;

/// How deep a tuple key `Vm::py_ord_sort_columns` sorts by columns nests.
const ORD_COLUMNS_DEPTH: usize = 4;

env_off_switch! {
    /// Whether a native sort may compare its keys by columns
    /// (`Vm::py_ord_sort_columns`). On unless `ZIPP_NO_PY_SORT_COLUMNS` is set
    /// (the same-binary A/B). Read once per process.
    fn py_sort_columns_enabled() = "ZIPP_NO_PY_SORT_COLUMNS"
}

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
        if r.is_err() {
            self.py_step_failed(ip);
        }
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
                match self.py_class_of(o) {
                    Some(c) => {
                        self.set(base, dst, c);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyDictGet { dst, obj, key, absent, slow } => {
                let o = self.get(base, obj);
                match self.py_dict_get(func_id, ip, o, key, absent)? {
                    PyDictGot::Value(v) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    PyDictGot::Absent => ip + 1,
                    PyDictGot::Slow => slow as usize,
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
            Instr::PyGetItem { dst, o, k, seq, dict, slow, absent } => {
                let (ov, kv) = (self.get(base, o), self.get(base, k));
                let (sc, dc) = (self.get(base, seq), self.get(base, dict));
                match self.py_get_item(ov, kv, sc, dc)? {
                    Some(Some(v)) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    Some(None) => absent as usize,
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
        if r.is_err() {
            self.py_step_failed(ip);
        }
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
                let mut call = None;
                match self.py_attr_get(func_id, ip, o, key, &mut call) {
                    Some(v) => {
                        self.set(base, dst, v);
                        ip + 1
                    }
                    None => match call {
                        // A property: its getter's frame, as `PyCall` pushes
                        // a callee's (`usize::MAX`), the result for `dst`;
                        // a getter that only reads an attribute its own
                        // site's entry holds, without the frame.
                        Some(a) => match self.py_accessor_inline(&a, o, None) {
                            Some(v) => {
                                self.set(base, dst, v);
                                ip + 1
                            }
                            None => {
                                if self.py_gen_entry(a.entry) {
                                    if let Some((rfid, rclos, rentry, _, _)) = self.py_gen_real_of(a.f) {
                                        if let Some(res) = self.py_new_gen_direct(a.f, rfid, rclos, rentry, Some(o), base, 0, 0) {
                                            let g = res?;
                                            self.set(base, dst, g);
                                            return Ok(ip + 1);
                                        }
                                    }
                                }
                                self.py_push_accessor(a, base, obj, None, dst, ip)?;
                                usize::MAX
                            }
                        },
                        None => slow as usize,
                    },
                }
            }
            Instr::PySetAttr { obj, key, val, slow } => {
                let o = self.get(base, obj);
                let v = self.get(base, val);
                let mut call = None;
                if self.py_attr_set(func_id, ip, o, key, v, &mut call)? {
                    ip + 1
                } else {
                    match call {
                        // A property: its setter's frame, whose result is
                        // dropped; a setter that only stores an attribute
                        // its own site's entry holds, without the frame.
                        Some(a) => {
                            if self.py_accessor_inline(&a, o, Some(v)).is_some() {
                                ip + 1
                            } else {
                                self.py_push_accessor(a, base, obj, Some(v), super::ic::RET_DISCARD, ip)?;
                                usize::MAX
                            }
                        }
                        None => slow as usize,
                    }
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
                let nat = if nv.is_heap() {
                    match self.heap.get(nv.heap_index()) {
                        HeapObj::Native(id) if *id == super::native::PY_GEN => 1u8,
                        HeapObj::Native(id) if *id == super::native::PY_ITER => 2u8,
                        _ => 0u8,
                    }
                } else {
                    0u8
                };
                if nat == 0 {
                    return Ok(slow as usize);
                }
                let tv = self.get(base, this);
                let v = if nat == 1 {
                    self.py_gen_next(tv)?
                } else {
                    self.py_iter_next(tv)?
                };
                self.set(base, dst, v);
                ip + 1
            }
            Instr::PyMethod { dst, obj, rt, key, gb, slow } => {
                let (o, r) = (self.get(base, obj), self.get(base, rt));
                match self.py_method(func_id, ip, o, r, key.into(), gb.into()) {
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
                match self.py_len(vv, rv) {
                    Some(n) => {
                        let r = self.make_bigint(n as i128);
                        self.set(base, dst, r);
                        ip + 1
                    }
                    None => slow as usize,
                }
            }
            Instr::PyTruth { dst, v, rt, slow } => {
                let (vv, rv) = (self.get(base, v), self.get(base, rt));
                match self.py_truth(vv, rv) {
                    Some(b) => {
                        self.set(base, dst, Value::bool(b));
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
            Instr::PyRaise { e, rt, slow, catch } => return self.py_raise_step(base, ip, e, rt, slow, catch),
            Instr::PyCaught { dst, e, line, rt, slow } => {
                let (ev, lv, rv) = (self.get(base, e), self.get(base, line), self.get(base, rt));
                if !self.py_caught(ev, lv, rv) {
                    return Ok(slow as usize);
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
        if r.is_err() {
            self.py_step_failed(ip);
        }
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
    pub(super) fn py_call_entry(&mut self, func_id: u32, ip: usize, f: Value, name: u32) -> Option<Value> {
        let key: &str = self.func(func_id as usize).string_constants[name as usize].as_str();
        let e = self.py_entry_of(func_id, ip, f, key)?;
        self.py_callable_fn(e).then_some(e)
    }

    /// The record `f`'s own data property `key` when it is a heap value,
    /// through this site's cache entry.
    #[inline]
    fn py_entry_of(&mut self, func_id: u32, ip: usize, f: Value, key: &str) -> Option<Value> {
        if !f.is_heap() {
            return None;
        }
        let fi = f.heap_index();
        let e = self.py_ic(func_id, ip);
        if matches!(e.kind, ic::CALL_ENTRY | ic::CALL_PLAIN..=ic::CALL_REC | ic::CALL_EXC | ic::CALL_GEN) && e.a == f.bits() && self.heap.version_of(fi) == e.hver {
            if let HeapObj::Object(m) = self.heap.get(fi) {
                let pos = e.pos as usize;
                if pos < m.slot_len() && !m.is_accessor_at(pos) {
                    let v = m.val_at(pos);
                    if v.is_heap() {
                        return Some(v);
                    }
                }
            }
        }
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
        if !v.is_heap() {
            return None;
        }
        if let Ok(pos) = u32::try_from(slot) {
            let hver = self.heap.version_of(fi);
            self.py_ic_put(func_id, ip, PyIc { kind: ic::CALL_ENTRY, pos, hver, a: f.bits(), b: 0, c: 0, d: 0 });
        }
        Some(v)
    }

    /// [`Instr::PyCall`]: the callee's frame pushed (`usize::MAX`, the
    /// dispatch loop re-enters it), a call without a frame completed (`ip +
    /// 1`), or the slow edge.
    #[inline(never)]
    pub(crate) fn py_step_call(&mut self, func_id: u32, base: usize, ip: usize, instr: &crate::bytecode::Instr) -> Result<usize, Thrown> {
        let crate::bytecode::Instr::PyCall { dst, f, arg_base, argc, slow } = *instr else {
            return Ok(ip + 1);
        };
        let skip_leaf = self.py_rt.as_deref().is_some_and(|p| p.skip_leaf_once.replace(false));
        let fv = self.get(base, f);
        let r = match self.py_call_target(func_id, ip, fv, argc) {
            PyCallee::Lean(lean, kind) => {
                // Single-byte dispatch by IC classification: CALL_PLAIN skips
                // all fast-path checks and goes directly to frame push.
                let fast_val = match kind {
                    ic::CALL_PLAIN => None,
                    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                    ic::CALL_REC if argc >= 1 && argc <= 4 && argc == lean.params => {
                        let mut argv = [Value::UNDEFINED; 4];
                        let start = base + arg_base as usize;
                        let n = argc as usize;
                        argv[..n].copy_from_slice(&self.regs[start..start + n]);
                        self.py_rec_exec(lean.fid, fv, &argv[..n])
                    }
                    ic::CALL_LEAF if !skip_leaf && argc <= lean.params => {
                        self.py_leaf_exec_window(lean.fid, fv, base, arg_base, argc)
                    }
                    ic::CALL_STORE if argc == lean.params => {
                        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                        let stored = self.py_store_body(lean.fid).is_some_and(|stores| {
                            self.with_argv(base, arg_base, argc, |vm, argv| vm.py_store_call(lean.fid, stores, argv))
                        });
                        #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
                        let stored = false;
                        if stored {
                            Some(Value::NULL)
                        } else {
                            self.py_leaf_exec_window(lean.fid, fv, base, arg_base, argc)
                        }
                    }
                    _ => None,
                };
                if let Some(v) = fast_val {
                    self.set(base, dst, v);
                    Ok(ip + 1)
                } else {
                    self.py_push_frame(&lean, fv, base, arg_base, argc, dst, ip + 1).map(|()| {
                        if argc < lean.params {
                            self.py_fill_defaults(fv, lean.fid, argc as usize);
                        }
                        usize::MAX
                    })
                }
            }
            PyCallee::Plain(fid, closure, entry) => {
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                let rec_val = if argc >= 1 && argc <= 4 && !self.py_rec_is_no(fid) {
                    let mut argv = [Value::UNDEFINED; 4];
                    let start = base + arg_base as usize;
                    let n = argc as usize;
                    argv[..n].copy_from_slice(&self.regs[start..start + n]);
                    self.py_rec_exec(fid, fv, &argv[..n])
                } else {
                    None
                };
                #[cfg(not(all(feature = "jit", target_arch = "x86_64")))]
                let rec_val: Option<Value> = None;
                if let Some(v) = rec_val {
                    self.set(base, dst, v);
                    Ok(ip + 1)
                } else {
                    self.setup_call(fid, closure, fv, base, arg_base, argc, dst, ip + 1, entry).map(|()| {
                        if (argc as usize) < self.func(fid as usize).param_count as usize {
                            self.py_fill_defaults(fv, fid, argc as usize);
                        }
                        usize::MAX
                    })
                }
            }
            // An exception class's entry for the count (`R.EXCENTRY`):
            // `const e = makeExc(this, [args]); e.context = null; return e;`
            // run here, natively as `makeExc` itself runs (`py_make_exc`),
            // when that builds the record; the entry's frame otherwise.
            PyCallee::Exc(fid, closure, entry) => match self.py_new_exc(fv, base, arg_base, argc) {
                Some(e) => {
                    self.set(base, dst, e);
                    Ok(ip + 1)
                }
                None => self.setup_call(fid, closure, fv, base, arg_base, argc, dst, ip + 1, entry).map(|()| usize::MAX),
            },
            // A sync Python generator function (`GEN_ENTRY[n]` + `makeGenerator`):
            // allocate the engine generator directly (`alloc_generator`, which uses
            // native `GenStart`) and clone `R.GENTMPL` without pushing `GEN_ENTRY`
            // or `rt.makeGenerator` JS frames.
            PyCallee::Gen(real_fid, real_closure, real_entry, fallback) => {
                match self.py_new_gen_direct(fv, real_fid, real_closure, real_entry, None, base, arg_base, argc) {
                    Some(Ok(g)) => {
                        self.set(base, dst, g);
                        Ok(ip + 1)
                    }
                    Some(Err(t)) => Err(t),
                    None => self.with_argv(base, arg_base, argc, |vm, argv| vm.call_value(fallback, fv, argv)).map(|v| {
                        self.set(base, dst, v);
                        ip + 1
                    }),
                }
            }
            // What the slow edge's `CallWithThis` does with this entry. The
            // runtime's `__zipp_py_strm` (the entry of its native str
            // methods, `hash`, `heapq`, `bisect`, `math.sqrt`) is called
            // where it is: `call_value` reaches a native through its
            // cascade of rarer callees and `call_native`'s match, to run
            // this same function with these same arguments.
            PyCallee::Other(entry) => {
                let r = if self.py_strm_entry(entry) {
                    let at = self.py_strm_site(func_id, ip, fv);
                    self.with_argv(base, arg_base, argc, |vm, argv| vm.py_str_method_at(fv, argv, at))
                } else {
                    self.with_argv(base, arg_base, argc, |vm, argv| vm.call_value(entry, fv, argv))
                };
                r.map(|v| {
                    self.set(base, dst, v);
                    ip + 1
                })
            }
            PyCallee::Slow => match self.py_step_bound_method(fv, base, arg_base, argc, dst, ip)? {
                Some(next) => Ok(next),
                None => Ok(slow as usize),
            },
        };
        if r.is_err() {
            self.py_step_failed(ip);
        }
        if super::prof_py::on() {
            let shown = match r {
                Ok(usize::MAX) => Ok(ip + 1),
                Ok(n) => Ok(n),
                Err(_) => Err(Thrown(String::new())),
            };
            self.py_prof_step(func_id, ip, instr, &shown);
        }
        r
    }

    /// Direct frame push for a bound Python method (`{ cls: T.method, func, self }`)
    /// at a `PyCall` site (e.g. `self.out(data)` in `html2text`): unwraps
    /// `(func, self)` and calls `func`'s `c<argc + 1>` entry with `self`
    /// prepended, skipping `NewArray`/`R.callv`/`bindArgs`.
    fn py_step_bound_method(
        &mut self,
        fv: Value,
        caller_base: usize,
        arg_base: u16,
        argc: u16,
        dst: u16,
        ip: usize,
    ) -> Result<Option<usize>, Thrown> {
        const ENTRIES: [&str; 19] = [
            "c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11", "c12",
            "c13", "c14", "c15", "c16", "c17", "c18",
        ];
        if !fv.is_heap() || super::prof_py::on() {
            return Ok(None);
        }
        let Some(t_method) = self.py_rt.as_deref().map(|p| p.t_method) else {
            return Ok(None);
        };
        if !t_method.is_heap() {
            return Ok(None);
        }
        let (inner_f, bound_self) = match self.heap.get(fv.heap_index()) {
            HeapObj::Object(m) if !m.is_ctor && m.slot_len() >= 3 => {
                if m.val_at(0).bits() != t_method.bits()
                    || !super::py_rt::key_eq(m.key_at(0), "cls")
                    || !super::py_rt::key_eq(m.key_at(1), "func")
                    || !super::py_rt::key_eq(m.key_at(2), "self")
                    || m.is_accessor_at(0)
                    || m.is_accessor_at(1)
                    || m.is_accessor_at(2)
                {
                    return Ok(None);
                }
                (m.val_at(1), m.val_at(2))
            }
            _ => return Ok(None),
        };
        let total = (argc as usize) + 1;
        let Some(&entry_name) = ENTRIES.get(total) else {
            return Ok(None);
        };
        if !inner_f.is_heap() {
            return Ok(None);
        }
        let entry = match self.heap.get(inner_f.heap_index()) {
            HeapObj::Object(fm) if !fm.is_ctor => {
                let Some(s) = fm.pos(entry_name) else {
                    return Ok(None);
                };
                if fm.is_accessor_at(s) {
                    return Ok(None);
                }
                fm.val_at(s)
            }
            _ => return Ok(None),
        };
        if !entry.is_heap() {
            return Ok(None);
        }
        if self.py_gen_entry(entry) {
            if let Some((rfid, rclos, rentry, _, _)) = self.py_gen_real_of(inner_f) {
                if let Some(res) = self.py_new_gen_direct(inner_f, rfid, rclos, rentry, Some(bound_self), caller_base, arg_base, argc) {
                    let g = res?;
                    self.set(caller_base, dst, g);
                    return Ok(Some(ip + 1));
                }
            }
        }
        let (fid, closure) = match self.heap.get(entry.heap_index()) {
            HeapObj::Func(id) => (*id, super::NO_CLOSURE),
            HeapObj::Closure { func, .. } => (*func, entry.heap_index()),
            _ => return Ok(None),
        };
        let p = self.func(fid as usize);
        if p.is_generator
            || p.is_async
            || !p.is_strict
            || p.lexical_this
            || p.rest_reg.is_some()
            || p.arguments_reg.is_some()
        {
            return Ok(None);
        }
        let regs = (p.reg_count as usize).max(1);
        let params = p.param_count as usize;
        let new_base = self.regs.len();
        if self.frames.len() >= MAX_FRAMES || self.regs_would_overflow(new_base + regs) {
            return Ok(None);
        }
        self.regs.resize(new_base + regs, Value::UNDEFINED);
        self.regs[new_base] = inner_f;
        if params >= 1 {
            self.regs[new_base + 1] = bound_self;
            let args = caller_base + arg_base as usize;
            let n = (argc as usize).min(params - 1);
            for i in 0..n {
                let v = self.regs[args + i];
                self.regs[new_base + 2 + i] = v;
            }
        }
        let last = self.frames.len() - 1;
        self.frames[last].ip = ip + 1;
        let new_target = std::mem::replace(&mut self.pending_new_target, Value::UNDEFINED);
        self.frames.push(Frame {
            super_done: false,
            args_obj: u32::MAX,
            eval_scope: u32::MAX,
            arg_win: (caller_base + arg_base as usize) as u32,
            argc: total as u16,
            is_eval: false,
            construct_this: false,
            gen: u32::MAX,
            func: fid,
            base: new_base,
            ip: 0,
            ret_dst: dst,
            closure,
            handlers: Vec::new(),
            new_target,
            callee: entry,
        });
        if total < params {
            self.py_fill_defaults(inner_f, fid, total);
        }
        Ok(Some(usize::MAX))
    }

    /// Push the frame of a property's accessor for the `PyGetAttr` /
    /// `PySetAttr` at `ip` (the dispatch loop then enters it, as it enters a
    /// `PyCall`'s callee): the function record its `this`, the instance (in
    /// the caller's register `obj`) its first parameter, and for a setter
    /// `value` its second, which is in a register of its own and so is
    /// written into the new frame. The result goes to the caller's `dst`
    /// (`RET_DISCARD` for a setter's). Parameters beyond those take their
    /// defaults, as a `PyCall`'s do.
    fn py_push_accessor(&mut self, a: super::py_attr::PyAccessor, base: usize, obj: u16, value: Option<Value>, dst: u16, ip: usize) -> Result<(), Thrown> {
        self.setup_call(a.fid, a.closure, a.f, base, obj, 1, dst, ip + 1, a.entry)?;
        let mut argc = 1;
        if let Some(v) = value {
            if let Some(slot) = self.frames.last().map(|f| f.base + 2).filter(|&s| s < self.regs.len()) {
                self.regs[slot] = v;
            }
            argc = 2;
        }
        if argc < self.func(a.fid as usize).param_count as usize {
            self.py_fill_defaults(a.f, a.fid, argc);
        }
        Ok(())
    }

    /// Whether `entry` is the `__zipp_py_strm` native in a program with no
    /// other realm (`call_value` runs a native of a realm's copy of the
    /// builtins in that realm, and probes a realm's `eval` first: with
    /// neither table in use it calls `call_native` with nothing around it).
    #[inline]
    fn py_strm_entry(&self, entry: Value) -> bool {
        matches!(self.heap.get(entry.heap_index()), HeapObj::Native(id) if *id == super::native::PY_STRM)
            && self.realm_global_objs.is_empty()
            && self.realm_fns.is_empty()
    }

    /// The slot of the builtin record `f`'s `strop` for the [`Instr::PyCall`]
    /// at `ip`, kept in the site's entry (`d`, plus one; 0 for not looked up
    /// yet): the record a site calls is the same one call after call, where
    /// the VM's one hint for `strop` is shared by every builtin record and
    /// their slots differ. A hint, as that one is: its user checks the key.
    /// `usize::MAX` when there is none.
    #[inline]
    fn py_strm_site(&mut self, func_id: u32, ip: usize, f: Value) -> usize {
        let e = self.py_ic(func_id, ip);
        if e.kind != ic::CALL_ENTRY || e.a != f.bits() || self.heap.version_of(f.heap_index()) != e.hver {
            return usize::MAX;
        }
        if e.d != 0 {
            return (e.d - 1) as usize;
        }
        let Some((_, slot)) = self.py_hint_slot(super::py_rt::hint::STROP, f.heap_index(), "strop") else {
            return usize::MAX;
        };
        if let Ok(d) = u32::try_from(slot + 1) {
            self.py_ic_put(func_id, ip, PyIc { d, ..e });
        }
        // Settle hot mirror for inline `math.sqrt` (`strop == 11`) so the JIT's
        // inline `emit_py_sqrt_inline` hits on the very first compiled iteration.
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        {
            let fi = f.heap_index();
            if self.ic_obj_ok(fi) {
                if let HeapObj::Object(m) = self.heap.get(fi) {
                    if !m.is_ctor && m.shape() != crate::shape::DICT && slot < m.slot_len() && m.val_at(slot) == Value::int(11) {
                        self.heap.refresh_mirror(fi);
                    }
                }
            }
        }
        slot
    }

    /// Whether the cache entry of the [`Instr::PyCall`] at `ip` names a
    /// record whose entry for the count runs without pushing a frame: either a
    /// native (`e.c == 0`: builtin / str method), or a plain Python callee
    /// handled frame-lessly (`StoreBody`, `LeafBody`, or compiled `PyRec`).
    /// For the native tiers' call-bound gate (`codegen::set_py_native_calls`).
    #[cfg_attr(not(all(feature = "jit", target_arch = "x86_64")), allow(dead_code))]
    pub(crate) fn py_call_site_native(&self, func_id: u32, ip: usize) -> bool {
        let e = self.py_ic(func_id, ip);
        let f = Value::from_bits(e.a);
        if !matches!(e.kind, ic::CALL_ENTRY | ic::CALL_STORE | ic::CALL_LEAF | ic::CALL_REC) || !f.is_heap() {
            return false;
        }
        let fi = f.heap_index();
        if self.heap.version_of(fi) != e.hver {
            return false;
        }
        let HeapObj::Object(m) = self.heap.get(fi) else {
            return false;
        };
        let pos = e.pos as usize;
        if pos >= m.slot_len() || m.is_accessor_at(pos) {
            return false;
        }
        let entry = m.val_at(pos);
        if e.kind == ic::CALL_ENTRY {
            return e.c == 0 && entry.is_heap() && matches!(self.heap.get(entry.heap_index()), HeapObj::Native(_));
        }
        entry.bits() == e.b
    }

    /// The parameters a [`Instr::PyCall`] left unbound in the frame just
    /// pushed (`argc` of the code object `fid`'s), bound to `f`'s defaults
    /// as its prologue binds them (the last `len` positionals take
    /// `f.defaults`, a dense Array; that prologue then finds them bound).
    /// Anything else leaves them for the prologue.
    pub(super) fn py_fill_defaults(&mut self, f: Value, fid: u32, argc: usize) {
        let params = self.func(fid as usize).param_count as usize;
        let Some(d) = self.py_hint_field(super::py_rt::hint::DEFAULTS, f.heap_index(), "defaults") else {
            return;
        };
        if !d.is_heap() || self.array_js_len.contains_key(&d.heap_index()) {
            return;
        }
        let HeapObj::Array(items) = self.heap.get(d.heap_index()) else {
            return;
        };
        let Some(first) = params.checked_sub(items.len()) else {
            return;
        };
        if argc < first {
            return;
        }
        let base = match self.frames.last() {
            Some(fr) if fr.func == fid => fr.base,
            _ => return,
        };
        for i in argc..params {
            let v = items[i - first];
            if v == Value::HOLE || v.is_undefined() || base + 1 + i >= self.regs.len() {
                return;
            }
            self.regs[base + 1 + i] = v;
        }
    }

    /// What [`Instr::PyCall`] calls for `f` and `argc`: `f`'s own entry
    /// `c<argc>` (the one its slow edge's `PyCallEntry` finds), as a plain
    /// function to push a frame for or another callable (a builtin, a
    /// generator function) to call as its slow edge's `CallWithThis` does.
    ///
    /// A site whose entry is a plain Python code object keeps it in its
    /// cache entry (`b` the entry's bits, `c` its code object's id, register
    /// count and parameter count, `d` its closure): while the record's entry
    /// slot still holds those bits, the call is pushed from them
    /// ([`Vm::py_push_frame`]) with neither the entry nor its code object
    /// read. The slot is read on every call, as before, because the runtime
    /// rewrites a record's entries in place (new defaults) without changing
    /// its keys.
    fn py_call_target(&mut self, func_id: u32, ip: usize, f: Value, argc: u16) -> PyCallee {
        const ENTRIES: [&str; 19] = [
            "c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9", "c10", "c11", "c12",
            "c13", "c14", "c15", "c16", "c17", "c18",
        ];
        let cached = self.py_ic(func_id, ip);
        if cached.kind == ic::CALL_GEN && cached.c != 0 && cached.a == f.bits() {
            if let Some((rfid, rclos, rentry, gentry)) = self.py_gen_hit(&cached, f) {
                return PyCallee::Gen(rfid, rclos, rentry, gentry);
            }
            self.py_ic_put(func_id, ip, PyIc { kind: ic::CALL_ENTRY, b: 0, c: 0, d: 0, ..cached });
        } else if cached.kind == ic::CALL_EXC && cached.c != 0 && cached.a == f.bits() {
            if let Some(lean) = self.py_lean_hit(&cached, f) {
                return PyCallee::Exc(lean.fid, lean.closure, lean.entry);
            }
            self.py_ic_put(func_id, ip, PyIc { kind: ic::CALL_ENTRY, b: 0, c: 0, d: 0, ..cached });
        } else if matches!(cached.kind, ic::CALL_PLAIN..=ic::CALL_REC) && cached.c != 0 && cached.a == f.bits() {
            if let Some(lean) = self.py_lean_hit(&cached, f) {
                return PyCallee::Lean(lean, cached.kind);
            }
            // The entry changed: forget the callee (the rest of the entry
            // still names the record's slot).
            self.py_ic_put(func_id, ip, PyIc { kind: ic::CALL_ENTRY, b: 0, c: 0, d: 0, ..cached });
        }
        let Some(name) = ENTRIES.get(argc as usize) else {
            return PyCallee::Slow;
        };
        let Some(e) = self.py_entry_of(func_id, ip, f, name) else {
            return PyCallee::Slow;
        };
        let (fid, closure) = match self.heap.get(e.heap_index()) {
            HeapObj::Func(id) => (*id, super::NO_CLOSURE),
            HeapObj::Closure { func, .. } => (*func, e.heap_index()),
            _ if self.py_callable_fn(e) => return PyCallee::Other(e),
            _ => return PyCallee::Slow,
        };
        let p = self.func(fid as usize);
        if p.is_generator || p.is_async {
            return PyCallee::Other(e);
        }
        if self.py_gen_entry(e) {
            if let Some((rfid, rclos, rentry, rregs, rparams)) = self.py_gen_real_of(f) {
                let c = rfid as u64 | ((rregs as u64) << 32) | ((rparams as u64) << 48);
                let now = self.py_ic(func_id, ip);
                if matches!(now.kind, ic::CALL_ENTRY | ic::CALL_PLAIN..=ic::CALL_REC | ic::CALL_EXC | ic::CALL_GEN) && now.a == f.bits() {
                    self.py_ic_put(func_id, ip, PyIc { kind: ic::CALL_GEN, b: e.bits(), c, d: rclos, ..now });
                }
                return PyCallee::Gen(rfid, rclos, rentry, e);
            }
        }
        if self.py_exc_entry(e, argc) {
            let c = fid as u64 | ((p.reg_count.max(1) as u64) << 32) | ((p.param_count as u64) << 48);
            let now = self.py_ic(func_id, ip);
            if matches!(now.kind, ic::CALL_ENTRY | ic::CALL_PLAIN..=ic::CALL_REC | ic::CALL_EXC | ic::CALL_GEN) && now.a == f.bits() {
                self.py_ic_put(func_id, ip, PyIc { kind: ic::CALL_EXC, b: e.bits(), c, d: closure, ..now });
            }
            return PyCallee::Exc(fid, closure, e);
        }
        // What `setup_call` binds as it is given for this callee: strict,
        // so `this` as passed; no rest parameter; no `arguments`.
        if p.is_strict && !p.lexical_this && p.rest_reg.is_none() && p.arguments_reg.is_none() {
            let c = fid as u64 | ((p.reg_count.max(1) as u64) << 32) | ((p.param_count as u64) << 48);
            let now = self.py_ic(func_id, ip);
            if matches!(now.kind, ic::CALL_ENTRY | ic::CALL_PLAIN..=ic::CALL_REC) && now.a == f.bits() {
                let kind = self.py_classify_callee(fid);
                self.py_ic_put(func_id, ip, PyIc { kind, b: e.bits(), c, d: closure, ..now });
            }
        }
        PyCallee::Plain(fid, closure, e)
    }

    /// Classify a plain Python callee once at IC population time so hot call
    /// sites dispatch on a single `PyIc::kind` byte with zero redundant probes.
    fn py_classify_callee(&mut self, fid: u32) -> u8 {
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if self.py_store_body(fid).is_some() {
            return ic::CALL_STORE;
        }
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        if self.py_rec_prepare(fid) {
            return ic::CALL_REC;
        }
        if self.py_leaf_info(fid).is_some_and(|info| info.native_eligible) {
            return ic::CALL_LEAF;
        }
        ic::CALL_PLAIN
    }

    /// The slot of the builtin record `f`'s `strop` when the site's entry
    /// `e` names `f` with that slot recorded ([`Vm::py_strm_site`]: `d`, with
    /// no plain callee in `c`) and the record's entry for the count, read
    /// afresh at the entry's slot (the record's keys as they were), is the
    /// `__zipp_py_strm` native right now: what [`Vm::py_call_target`] would
    /// find through [`Vm::py_entry_of`] and the native's tests, without the
    /// entry's second read and the callee's kind tested by elimination. For
    /// compiled code's `jit_py_call` only: asked in the interpreter's step
    /// (`py_call_target`) it made every plain Python call 3 to 4% slower
    /// (call_simple, call_defaults), for 4% on str_methods there.
    #[cfg_attr(not(all(feature = "jit", target_arch = "x86_64")), allow(dead_code))]
    #[inline]
    pub(super) fn py_strm_hit(&self, e: &PyIc, f: Value) -> Option<(usize, i32)> {
        if e.kind != ic::CALL_ENTRY || e.c != 0 || e.d == 0 || e.a != f.bits() || !f.is_heap() {
            return None;
        }
        let fi = f.heap_index();
        if self.heap.version_of(fi) != e.hver {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(fi) else {
            return None;
        };
        let pos = e.pos as usize;
        if pos >= m.slot_len() || m.is_accessor_at(pos) {
            return None;
        }
        let entry = m.val_at(pos);
        if !entry.is_heap() || !self.py_strm_entry(entry) {
            return None;
        }
        let at = (e.d - 1) as usize;
        // `version_of(fi) == e.hver` guarantees `m`'s key layout and accessor
        // flags are unchanged since `py_strm_site` located `"strop"` at `at`.
        if at < m.slot_len() && !m.is_accessor_at(at) {
            let op = m.val_at(at);
            if op.is_int() {
                return Some((at, op.as_int()));
            }
        }
        None
    }

    /// Whether `e` is one of the runtime's sync generator positional wrappers
    /// (`R.GENENTRY[0..=18]`).
    #[inline]
    pub(super) fn py_gen_entry(&self, e: Value) -> bool {
        let Some(entries) = self.py_rt.as_deref().map(|p| p.gen_entries) else {
            return false;
        };
        if !entries.is_heap() {
            return false;
        }
        matches!(self.heap.get(entries.heap_index()), HeapObj::Array(items) if items.iter().any(|v| v.bits() == e.bits()))
    }

    /// Extract `(real_fid, real_closure, real_entry, real_regs, real_params)` from
    /// a sync Python generator function record `f` whose `f.wrap === R.MAKEGEN` and
    /// `f.real` is a strict non-async generator code object.
    #[inline]
    pub(super) fn py_gen_real_of(&self, f: Value) -> Option<(u32, u32, Value, u16, u16)> {
        let p = self.py_rt.as_deref()?;
        if p.gen_tmpl_shape == crate::shape::DICT || !p.make_gen.is_heap() || !f.is_heap() {
            return None;
        }
        let fi = f.heap_index();
        let wrap = self.py_hint_field(super::py_rt::hint::WRAP, fi, "wrap")?;
        if wrap.bits() != p.make_gen.bits() {
            return None;
        }
        let real = self.py_hint_field(super::py_rt::hint::REAL, fi, "real")?;
        if !real.is_heap() {
            return None;
        }
        let (fid, closure) = match self.heap.get(real.heap_index()) {
            HeapObj::Func(id) => (*id, super::NO_CLOSURE),
            HeapObj::Closure { func, .. } => (*func, real.heap_index()),
            _ => return None,
        };
        let proto = self.func(fid as usize);
        if !proto.is_generator
            || proto.is_async
            || !proto.is_strict
            || proto.lexical_this
            || proto.rest_reg.is_some()
            || proto.arguments_reg.is_some()
        {
            return None;
        }
        Some((fid, closure, real, proto.reg_count.max(1), proto.param_count))
    }

    /// Cached `ic::CALL_GEN` fast hit: verifies `f`'s entry slot still holds
    /// `e.b` (`GEN_ENTRY[n]`), `f.wrap === R.MAKEGEN`, and `f.real` still points
    /// to `(real_fid, real_closure)`.
    #[inline]
    pub(super) fn py_gen_hit(&self, e: &PyIc, f: Value) -> Option<(u32, u32, Value, Value)> {
        if !f.is_heap() {
            return None;
        }
        let fi = f.heap_index();
        if self.heap.version_of(fi) != e.hver {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(fi) else {
            return None;
        };
        let pos = e.pos as usize;
        if pos >= m.slot_len() || m.is_accessor_at(pos) || m.val_at(pos).bits() != e.b {
            return None;
        }
        let p = self.py_rt.as_deref()?;
        if p.gen_tmpl_shape == crate::shape::DICT || !p.make_gen.is_heap() {
            return None;
        }
        let wrap = self.py_hint_field(super::py_rt::hint::WRAP, fi, "wrap")?;
        if wrap.bits() != p.make_gen.bits() {
            return None;
        }
        let real = self.py_hint_field(super::py_rt::hint::REAL, fi, "real")?;
        if !real.is_heap() {
            return None;
        }
        let (rfid, rclos) = match self.heap.get(real.heap_index()) {
            HeapObj::Func(id) => (*id, super::NO_CLOSURE),
            HeapObj::Closure { func, .. } => (*func, real.heap_index()),
            _ => return None,
        };
        if rfid != e.c as u32 || rclos != e.d {
            return None;
        }
        Some((rfid, rclos, real, Value::from_bits(e.b)))
    }

    /// Direct native sync Python generator creation: calls `alloc_generator`
    /// (which uses native `GenStart`) and clones `R.GENTMPL` with `(js, name,
    /// qualname)` patched — zero `GEN_ENTRY` / `rt.makeGenerator` JS frames.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn py_new_gen_direct(
        &mut self,
        fv: Value,
        real_fid: u32,
        real_closure: u32,
        real_entry: Value,
        bound_self: Option<Value>,
        base: usize,
        arg_base: u16,
        argc: u16,
    ) -> Option<Result<Value, Thrown>> {
        let total = argc as usize + usize::from(bound_self.is_some());
        if total > 18 || super::prof_py::on() {
            return None;
        }
        let (gen_tmpl, gen_shape) = {
            let p = self.py_rt.as_deref()?;
            (p.gen_tmpl, p.gen_tmpl_shape)
        };
        if gen_shape == crate::shape::DICT || !self.py_alloc_like_ok(gen_tmpl, gen_shape) {
            return None;
        }
        let mut argv = [Value::UNDEFINED; 18];
        let mut idx = 0usize;
        if let Some(s) = bound_self {
            argv[0] = s;
            idx = 1;
        }
        let start = base + arg_base as usize;
        for i in 0..argc as usize {
            argv[idx + i] = self.regs[start + i];
        }
        self.pending_gen_callee = real_entry;
        let jsgen = match self.alloc_generator(real_fid, real_closure, fv, &argv[..total]) {
            Ok(v) => v,
            Err(t) => return Some(Err(t)),
        };
        // Re-check template and read `f.name` / `f.qualname` after `alloc_generator`
        // (the prologue may have allocated / run GC; `fv` and `gen_tmpl` remain rooted).
        let (gen_tmpl, gen_shape) = {
            let p = self.py_rt.as_deref()?;
            (p.gen_tmpl, p.gen_tmpl_shape)
        };
        if !self.py_alloc_like_ok(gen_tmpl, gen_shape) {
            return None;
        }
        let name = self.py_hint_field(super::py_rt::hint::NAME, fv.heap_index(), "name").unwrap_or(Value::UNDEFINED);
        let qualname = self.py_hint_field(super::py_rt::hint::QUALNAME, fv.heap_index(), "qualname").unwrap_or(Value::UNDEFINED);
        let mut vals = [Value::UNDEFINED; 14];
        match self.heap.get(gen_tmpl.heap_index()) {
            HeapObj::Object(m) if m.slot_len() == 14 => {
                for (i, slot) in vals.iter_mut().enumerate() {
                    *slot = m.val_at(i);
                }
            }
            _ => return None,
        }
        vals[1] = jsgen;
        vals[7] = name;
        vals[8] = qualname;
        self.py_alloc_like_ready(gen_tmpl, gen_shape, &vals).map(Ok)
    }

    /// Whether `e` is the runtime's positional entry of exception classes
    /// for `argc` arguments (`R.EXCENTRY[argc]`).
    #[inline]
    fn py_exc_entry(&self, e: Value, argc: u16) -> bool {
        let Some(entries) = self.py_rt.as_deref().map(|p| p.exc_entries) else {
            return false;
        };
        if !entries.is_heap() || self.array_js_len.contains_key(&entries.heap_index()) {
            return false;
        }
        matches!(self.heap.get(entries.heap_index()), HeapObj::Array(items) if items.get(argc as usize).is_some_and(|v| v.bits() == e.bits()))
    }

    /// An exception class `cls` called with the `argc` arguments in the
    /// caller's window at `arg_base`, as its positional entry does it: the
    /// record `makeExc` builds natively (`py_make_exc`, over an Array of the
    /// arguments) with `context` then `null`. `None` (nothing to undo: the
    /// Array is garbage) when that declines.
    fn py_new_exc(&mut self, cls: Value, base: usize, arg_base: u16, argc: u16) -> Option<Value> {
        let rt = self.py_rt.as_deref()?.r;
        let n = argc as usize;
        let mut args = self.heap.take_arr_buf(n);
        for i in 0..n {
            args.push(self.get(base, arg_base + i as u16));
        }
        let arr = Value::heap(self.heap.alloc_array_settled(args));
        let e = self.py_make_exc(cls, arr, rt)?;
        // `e.context = null` (the record `py_make_exc` builds: slot 4).
        if let HeapObj::Object(m) = self.heap.get_mut(e.heap_index()) {
            m.set_val_at(4, Value::NULL);
        }
        Some(e)
    }

    /// The plain callee a [`Instr::PyCall`] site's entry `e` records (see
    /// [`Vm::py_call_target`]) when the record `f` still holds it.
    #[inline]
    pub(super) fn py_lean_hit(&self, e: &PyIc, f: Value) -> Option<PyLean> {
        if !f.is_heap() {
            return None;
        }
        let fi = f.heap_index();
        if self.heap.version_of(fi) != e.hver {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(fi) else {
            return None;
        };
        let pos = e.pos as usize;
        if pos >= m.slot_len() || m.is_accessor_at(pos) || m.val_at(pos).bits() != e.b {
            return None;
        }
        Some(PyLean { fid: e.c as u32, regs: (e.c >> 32) as u16, params: (e.c >> 48) as u16, closure: e.d, entry: Value::from_bits(e.b) })
    }

    /// Push the frame of the plain Python callee `lean` for a `PyCall`:
    /// exactly what `setup_call` does for it (its code object is strict,
    /// with no rest parameter, no `arguments` and no lexical `this`, so
    /// `this` is bound as given and nothing else is built), from the numbers
    /// the site's entry keeps. The frame limit, a register file that would
    /// outgrow its pinned capacity, and a profiling run take `setup_call`
    /// itself.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn py_push_frame(&mut self, lean: &PyLean, this_val: Value, caller_base: usize, arg_base: u16, argc: u16, dst: u16, caller_ip_next: usize) -> Result<(), Thrown> {
        let new_base = self.regs.len();
        let regs = (lean.regs as usize).max(1);
        if self.frames.len() >= MAX_FRAMES || super::prof_py::on() || self.regs_would_overflow(new_base + regs) {
            return self.setup_call(lean.fid, lean.closure, this_val, caller_base, arg_base, argc, dst, caller_ip_next, lean.entry);
        }
        self.regs.resize(new_base + regs, Value::UNDEFINED);
        self.regs[new_base] = this_val;
        let args = caller_base + arg_base as usize;
        for i in 0..(argc as usize).min(lean.params as usize) {
            let v = self.regs[args + i];
            self.regs[new_base + 1 + i] = v;
        }
        let last = self.frames.len() - 1;
        self.frames[last].ip = caller_ip_next;
        let new_target = std::mem::replace(&mut self.pending_new_target, Value::UNDEFINED);
        self.frames.push(Frame {
            super_done: false,
            args_obj: u32::MAX,
            eval_scope: u32::MAX,
            arg_win: args as u32,
            argc,
            is_eval: false,
            construct_this: false,
            gen: u32::MAX,
            func: lean.fid,
            base: new_base,
            ip: 0,
            ret_dst: dst,
            closure: lean.closure,
            handlers: Vec::new(),
            new_target,
            callee: lean.entry,
        });
        Ok(())
    }

    /// A fused step raised (its own error, or one from code it ran): the
    /// frame's ip is the step's, for the unwinder (a Python frame's line,
    /// `vm::py_rt`).
    #[cold]
    #[inline(never)]
    pub(super) fn py_step_failed(&mut self, ip: usize) {
        if let Some(f) = self.frames.last_mut() {
            f.ip = ip;
        }
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

    /// What [`Vm::py_global`] answers at once for a site whose cache entry
    /// holds (its own first tests, with nothing hashed and nothing
    /// recorded): the module global at its entry's position, or the builtin
    /// while the globals have had no key appended. For the dispatch loop,
    /// which asks this first and takes the step when it declines. Out of
    /// line, as the three in `py_attr` are: inlined into the dispatch loop
    /// they measured slower than this one small call.
    #[inline(never)]
    pub(super) fn py_global_hit(&self, func_id: u32, ip: usize, g: Value, r: Value) -> Option<Value> {
        if !g.is_heap() {
            return None;
        }
        let gi = g.heap_index();
        let e = self.py_ic(func_id, ip);
        if e.a != g.bits() {
            // A function's record (lazy globals) stands for its globals.
            return self.py_global_hit_lazy(&e, g, r);
        }
        self.py_global_hit_at(&e, g, gi, r)
    }

    /// [`Vm::py_global_hit`] past its entry's globals test.
    #[inline(always)]
    fn py_global_hit_at(&self, e: &PyIc, g: Value, gi: u32, r: Value) -> Option<Value> {
        if self.heap.version_of(gi) != e.hver {
            return None;
        }
        if e.kind == ic::GLOBAL {
            return self.py_map_at_bits(g, e.pos, e.b, e.d);
        }
        if e.kind == ic::BUILTIN && matches!(self.heap.get(gi), HeapObj::Map { keys, .. } if keys.len() as u64 == e.c) {
            let b = self.py_rt_for(r)?.builtins;
            return self.py_map_at_bits(b, e.pos, e.b, e.d);
        }
        None
    }

    /// [`Vm::py_global_hit`] handed a function's record (lazy globals): the
    /// same answer for the record's globals.
    #[inline(never)]
    fn py_global_hit_lazy(&self, e: &PyIc, f: Value, r: Value) -> Option<Value> {
        let g = self.py_globals_of(f)?;
        if e.a != g.bits() {
            return None;
        }
        self.py_global_hit_at(e, g, g.heap_index(), r)
    }

    /// [`Instr::PyGlobal`]'s `globals` operand as the module's globals: a
    /// `Map` is itself; a plain record is a function's (the emitter's lazy
    /// globals: a function that only reads globals hands its own record,
    /// `this`, instead of reading its `globals` field on every call), whose
    /// own data `globals` is read here: at its slot for a record of a shape
    /// seen with it there (a shape fixes keys, order and kinds), else by
    /// name ([`Vm::py_globals_learn`]). `None` (the slow edge) for anything
    /// else. Out of line: a site handed the Map its entry names (every site
    /// outside such a function) never calls it.
    #[inline(never)]
    pub(super) fn py_globals_of(&self, g: Value) -> Option<Value> {
        if !g.is_heap() {
            return None;
        }
        match self.heap.get(g.heap_index()) {
            HeapObj::Map { .. } => Some(g),
            HeapObj::Object(m) => {
                if let Some(p) = self.py_rt.as_deref() {
                    let (shape, slot) = (m.shape(), p.func_globals_slot.get() as usize);
                    if shape != crate::shape::DICT && slot < m.slot_len() && p.func_shapes.get().contains(&shape) {
                        debug_assert!(
                            m.key_at(slot) == "globals" && !m.is_accessor_at(slot),
                            "a function record of a seen shape has no data `globals` at the seen slot"
                        );
                        let v = m.val_at(slot);
                        return v.is_heap().then_some(v);
                    }
                }
                self.py_globals_learn(g)
            }
            _ => None,
        }
    }

    /// [`Vm::py_globals_of`] of a function record of a shape not seen yet:
    /// its `globals` by name (through the VM's hint), the record's shape
    /// then noted with the slot (the shapes kept are those of one slot).
    #[cold]
    #[inline(never)]
    fn py_globals_learn(&self, g: Value) -> Option<Value> {
        let gi = g.heap_index();
        let (v, slot) = self.py_hint_slot(super::py_rt::hint::GLOBALS, gi, "globals")?;
        if !v.is_heap() {
            return None;
        }
        if let (HeapObj::Object(m), Some(p), Ok(slot)) = (self.heap.get(gi), self.py_rt.as_deref(), u16::try_from(slot)) {
            if m.shape_guardable() {
                let mut shapes = p.func_shapes.get();
                if p.func_globals_slot.get() != slot {
                    shapes = [crate::shape::DICT; 4];
                    p.func_globals_slot.set(slot);
                }
                // The first empty place, else the last place over again.
                let at = shapes.iter().position(|&s| s == crate::shape::DICT).unwrap_or(3);
                shapes[at] = m.shape();
                p.func_shapes.set(shapes);
            }
        }
        Some(v)
    }

    /// [`Instr::PyGlobal`]: a module global, else a builtin; `None` for the
    /// slow edge (see `vm::py_rt` for the site cache's entries).
    #[inline(never)]
    pub(super) fn py_global(&mut self, func_id: u32, ip: usize, g: Value, r: Value, key: u32) -> Option<Value> {
        if !g.is_heap() {
            return None;
        }
        let gi = g.heap_index();
        let e = self.py_ic(func_id, ip);
        if e.a == g.bits() && self.heap.version_of(gi) == e.hver {
            if e.kind == ic::GLOBAL {
                if let Some(v) = self.py_map_at_bits(g, e.pos, e.b, e.d) {
                    return Some(v);
                }
            } else if e.kind == ic::BUILTIN {
                // No key appended to the globals since the name was found
                // missing there: still missing.
                let absent = matches!(self.heap.get(gi), HeapObj::Map { keys, .. } if keys.len() as u64 == e.c);
                if absent {
                    if let Some(b) = self.py_rt_for(r).map(|p| p.builtins) {
                        if let Some(v) = self.py_map_at_bits(b, e.pos, e.b, e.d) {
                            return Some(v);
                        }
                    }
                }
            }
        }
        if !matches!(self.heap.get(gi), HeapObj::Map { .. }) {
            // A function's record (lazy globals): the same for its globals.
            let m = self.py_globals_of(g)?;
            if !matches!(self.heap.get(m.heap_index()), HeapObj::Map { .. }) {
                return None;
            }
            return self.py_global(func_id, ip, m, r, key);
        }
        let k = self.resolve_const_slot(func_id, key);
        // Module globals are asked for every builtin name they lack: through
        // the Map's hash index, not a scan of every global.
        self.coll_index_early(gi);
        let hver = self.heap.version_of(gi);
        match self.coll_find(gi, k) {
            Some(i) => {
                let (v, kv) = match self.heap.get(gi) {
                    HeapObj::Map { keys, vals } => (*vals.get(i)?, *keys.get(i)?),
                    _ => return None,
                };
                if v.is_undefined() {
                    return None;
                }
                let (kb, stamp) = (kv.bits(), self.py_key_stamp(func_id, key, kv));
                if let (Ok(pos), Some(kver)) = (u32::try_from(i), stamp) {
                    self.py_ic_put(func_id, ip, PyIc { kind: ic::GLOBAL, pos, hver, a: g.bits(), b: kb, c: 0, d: kver });
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
                let (v, kv) = match self.heap.get(bi) {
                    HeapObj::Map { keys, vals } => (*vals.get(i)?, *keys.get(i)?),
                    _ => return None,
                };
                if v.is_undefined() {
                    return None;
                }
                let (kb, stamp) = (kv.bits(), self.py_key_stamp(func_id, key, kv));
                if let (Ok(pos), Some(kver)) = (u32::try_from(i), stamp) {
                    self.py_ic_put(func_id, ip, PyIc { kind: ic::BUILTIN, pos, hver, a: g.bits(), b: kb, c: glen, d: kver });
                }
                Some(v)
            }
        }
    }

    /// The value of the `Map` `m`'s entry at `pos` when its key there has
    /// the bits `key` and stamp `kver` (see `py_map_at`), and its value is
    /// not `undefined`.
    #[inline]
    pub(super) fn py_map_at_bits(&self, m: Value, pos: u32, key: u64, kver: u32) -> Option<Value> {
        if !m.is_heap() {
            return None;
        }
        let HeapObj::Map { keys, vals } = self.heap.get(m.heap_index()) else {
            return None;
        };
        let pos = pos as usize;
        match (keys.get(pos), vals.get(pos)) {
            (Some(&k), Some(&v)) if k.bits() == key && !v.is_undefined() && (kver == super::py_attr::KEY_ROOTED || self.py_key_ver(k) == kver) => Some(v),
            _ => None,
        }
    }

    /// [`Instr::PyClassOf`]: the record `o`'s class when it is a plain
    /// record; `None` for the slow edge.
    #[inline]
    pub(super) fn py_class_of(&self, o: Value) -> Option<Value> {
        if let Some(c) = self.py_cls_of(o) {
            return self.py_plain_rec(c).then_some(c);
        }
        let p = self.py_rt.as_deref()?;
        let t = if o.is_null() {
            p.istypes[0]
        } else if o.is_bool() {
            p.istypes[1]
        } else if o.small_bigint_val().is_some() {
            p.istypes[2]
        } else if o.is_number() {
            p.istypes[3]
        } else if o.is_heap() {
            match self.heap.get(o.heap_index()) {
                HeapObj::Str(_) | HeapObj::Cons { .. } => p.istypes[4],
                HeapObj::BigInt(_) | HeapObj::BigIntBig(_) => p.istypes[2],
                _ => return None,
            }
        } else {
            return None;
        };
        self.py_plain_rec(t).then_some(t)
    }

    /// [`Instr::PyDictGet`] of the string constant `key` (of `func_id`) in
    /// the instance `o`'s attribute storage: what `map_method`'s `get`
    /// answers for it, asked of layout-mode storage directly (the step went
    /// through the method's name), sorted as the instruction's `absent`
    /// flag sorts it. Caches proved-absent layouts in `ic::DICT_GET_ABSENT`.
    pub(super) fn py_dict_get(&mut self, func_id: u32, ip: usize, o: Value, key: u32, absent: bool) -> Result<PyDictGot, Thrown> {
        let Some(map) = self.py_inst_map(o) else {
            return Ok(PyDictGot::Slow);
        };
        if let HeapObj::PyAttrs { layout, .. } = self.heap.get(map) {
            let layout = *layout;
            let e = self.py_ic(func_id, ip);
            if e.kind == ic::DICT_GET_ABSENT && e.d == layout {
                return Ok(if absent { PyDictGot::Absent } else { PyDictGot::Slow });
            }
            let k = self.resolve_const_slot(func_id, key);
            if let Some(slot) = self.py_layout_find(layout, k) {
                let v = self.py_attrs_at(Value::heap(map), layout, slot).unwrap_or(Value::UNDEFINED);
                return Ok(match (v.is_undefined(), absent) {
                    (false, false) => PyDictGot::Value(v),
                    (true, true) => PyDictGot::Absent,
                    _ => PyDictGot::Slow,
                });
            }
            self.py_ic_put(
                func_id,
                ip,
                PyIc { kind: ic::DICT_GET_ABSENT, pos: 0, hver: 0, a: 0, b: 0, c: 0, d: layout },
            );
            return Ok(if absent { PyDictGot::Absent } else { PyDictGot::Slow });
        }
        let k = self.resolve_const_slot(func_id, key);
        let v = self.map_method(map, "get", &[k])?.unwrap_or(Value::UNDEFINED);
        Ok(match (v.is_undefined(), absent) {
            (false, false) => PyDictGot::Value(v),
            (true, true) => PyDictGot::Absent,
            _ => PyDictGot::Slow,
        })
    }

    /// [`Instr::PyCaught`] for the exception `e` (the handler's `line`):
    /// whether it is an exception record, its `tbline` then set to `line`
    /// if it was -1; `false` (nothing done) for the slow edge.
    pub(super) fn py_caught(&mut self, e: Value, line: Value, rt: Value) -> bool {
        let Some(tb_slot) = self.py_exc_record(e, rt, false).map(|(_, tb)| tb) else {
            return false;
        };
        let idx = e.heap_index();
        let tb = match self.heap.get(idx) {
            HeapObj::Object(m) => m.val_at(tb_slot),
            _ => return false,
        };
        if tb.is_number() && tb.as_f64() == -1.0 {
            if line.is_heap() {
                self.heap.write_barrier_val(idx, line);
            }
            if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                m.set_val_at(tb_slot, line);
            }
        }
        true
    }

    /// [`Instr::PyExcPop`]: `Some` when it popped (see the instruction).
    #[inline(never)]
    pub(super) fn py_exc_pop(&mut self, rt: Value) -> Option<()> {
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
        if (!self.arr_props.is_empty() && self.arr_props.contains_key(&idx))
            || (!self.array_js_len.is_empty() && self.array_js_len.contains_key(&idx))
            || (!self.proto_of.is_empty() && self.proto_of.contains_key(&idx))
            || self.array_proto_has_index
            || !self.array_pop_is_intrinsic()
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
        let crate::heap::PropKeys::Planned { all, visible_len } = m.prop_keys() else {
            return false;
        };
        *visible_len == m.slot_len() && all.len() == m.slot_len() && (1..=crate::bytecode::FINALIZE_STAGE_SLOTS).contains(&m.slot_len())
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
        self.py_alloc_like_ready(tmpl, shape, vals)
    }

    /// [`Vm::py_alloc_like`] of a template its caller has just found
    /// [`Vm::py_alloc_like_ok`] for `shape` (nothing that could change the
    /// template has run since). The record is allocated from the template's
    /// own key plan, borrowed from the registry's copy of it (`PyRt::plans`,
    /// the same `Arc` by pointer) rather than cloned, which changed the
    /// plan's count twice per record.
    #[inline(never)]
    pub(super) fn py_alloc_like_ready(&mut self, tmpl: Value, shape: u32, vals: &[Value]) -> Option<Value> {
        let HeapObj::Object(m) = self.heap.get(tmpl.heap_index()) else {
            return None;
        };
        if m.slot_len() != vals.len() {
            return None;
        }
        let crate::heap::PropKeys::Planned { all, .. } = m.prop_keys() else {
            return None;
        };
        #[cfg(not(feature = "safe-sandbox"))]
        let idx = {
            let static_plan = self.py_rt.as_deref().and_then(|p| {
                if shape == p.seq_shape && vals.len() == super::py_rt::SEQ_KEYS.len() {
                    Some(super::py_rt::py_seq_static_plan())
                } else if shape == p.inst_shape && vals.len() == super::py_rt::INST_KEYS.len() {
                    Some(super::py_rt::py_inst_static_plan())
                } else if shape == p.dict_shape && vals.len() == super::py_rt::DICT_KEYS.len() {
                    Some(super::py_rt::py_dict_static_plan())
                } else if shape == p.exc_shape && vals.len() == super::py_rt::EXC_KEYS.len() {
                    Some(super::py_rt::py_exc_static_plan())
                } else if shape == p.slice_shape && vals.len() == super::py_rt::SLICE_KEYS.len() {
                    Some(super::py_rt::py_slice_static_plan())
                } else if shape == p.frame_shape && vals.len() == super::py_rt::FRAME_KEYS.len() {
                    Some(super::py_rt::py_frame_static_plan())
                } else if shape == p.gen_tmpl_shape && vals.len() == super::py_rt::GEN_KEYS.len() {
                    Some(super::py_rt::py_gen_static_plan())
                } else {
                    None
                }
            });
            if let Some(plan) = static_plan {
                self.heap.alloc_finalized_static(plan, vals, shape)
            } else {
                let kept = self.py_rt.as_deref().and_then(|p| p.plans.iter().position(|k| all.is(k)));
                match kept {
                    Some(i) => {
                        let p = self.py_rt.as_deref()?;
                        self.heap.alloc_finalized(&p.plans[i], vals, shape)
                    }
                    None => {
                        let plan = all.to_plan();
                        if let Some(p) = self.py_rt.as_deref_mut() {
                            if p.plans.len() < 8 {
                                p.plans.push(plan.clone());
                            }
                        }
                        self.heap.alloc_finalized(&plan, vals, shape)
                    }
                }
            }
        };
        #[cfg(feature = "safe-sandbox")]
        let idx = {
            let plan = all.to_plan();
            self.heap.alloc_finalized(&plan, vals, shape)
        };
        self.realm_born(idx, self.obj_proto);
        Some(Value::heap(idx))
    }

    /// Direct inline allocation of a sequence record `{cls, items}` using
    /// the `'static` sequence key plan (`alloc_finalized_static` -> ready pool),
    /// bypassing `py_alloc_like_ready`'s plan scan when `seq_tmpl` is unchanged.
    #[inline(always)]
    pub(super) fn py_alloc_seq_fast(&mut self, cls: Value, items: Value) -> Option<Value> {
        let (tmpl, shape, hver) = {
            let p = self.py_rt.as_deref()?;
            (p.seq_tmpl, p.seq_shape, p.seq_tmpl_hver)
        };
        if hver != u32::MAX && tmpl.is_heap() && self.heap.version_of(tmpl.heap_index()) == hver {
            let idx = self
                .heap
                .alloc_finalized_static(super::py_rt::py_seq_static_plan(), &[cls, items], shape);
            self.realm_born(idx, self.obj_proto);
            return Some(Value::heap(idx));
        }
        self.py_alloc_like(tmpl, shape, &[cls, items])
    }

    /// Direct inline allocation of an instance record `{cls, dict}` using
    /// the `'static` instance key plan (`alloc_finalized_static` -> ready pool).
    #[inline(always)]
    pub(super) fn py_alloc_inst_fast(&mut self, cls: Value, dict: Value) -> Option<Value> {
        let (tmpl, shape, hver) = {
            let p = self.py_rt.as_deref()?;
            (p.inst_tmpl, p.inst_shape, p.inst_tmpl_hver)
        };
        if hver != u32::MAX && tmpl.is_heap() && self.heap.version_of(tmpl.heap_index()) == hver {
            let idx = self
                .heap
                .alloc_finalized_static(super::py_rt::py_inst_static_plan(), &[cls, dict], shape);
            self.realm_born(idx, self.obj_proto);
            return Some(Value::heap(idx));
        }
        self.py_alloc_like(tmpl, shape, &[cls, dict])
    }

    /// Direct inline allocation of an exception record using the `'static`
    /// exception key plan (`alloc_finalized_static` -> ready pool).
    #[inline(always)]
    pub(super) fn py_alloc_exc_fast(&mut self, vals: &[Value; 10]) -> Option<Value> {
        let (tmpl, shape, hver) = {
            let p = self.py_rt.as_deref()?;
            (p.exc_tmpl, p.exc_shape, p.exc_tmpl_hver)
        };
        if hver != u32::MAX && tmpl.is_heap() && self.heap.version_of(tmpl.heap_index()) == hver {
            let idx = self
                .heap
                .alloc_finalized_static(super::py_rt::py_exc_static_plan(), vals, shape);
            self.realm_born(idx, self.obj_proto);
            return Some(Value::heap(idx));
        }
        self.py_alloc_like(tmpl, shape, vals)
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
        if !self.array_js_len.is_empty() && self.array_js_len.contains_key(&args.heap_index()) {
            return None;
        }
        let p = self.py_rt_for(rt)?;
        let (tmpl, exc_shape, exc_hver, seq_tmpl, seq_shape, seq_hver, ttuple, stack) = (
            p.exc_tmpl,
            p.exc_shape,
            p.exc_tmpl_hver,
            p.seq_tmpl,
            p.seq_shape,
            p.seq_tmpl_hver,
            p.t_tuple,
            p.excstack,
        );
        if !stack.is_heap() || (!self.array_js_len.is_empty() && self.array_js_len.contains_key(&stack.heap_index())) {
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
        let exc_ok = (exc_hver != u32::MAX && tmpl.is_heap() && self.heap.version_of(tmpl.heap_index()) == exc_hver)
            || self.py_alloc_like_ok(tmpl, exc_shape);
        let seq_ok = (seq_hver != u32::MAX && seq_tmpl.is_heap() && self.heap.version_of(seq_tmpl.heap_index()) == seq_hver)
            || self.py_alloc_like_ok(seq_tmpl, seq_shape);
        if !exc_ok || !seq_ok {
            return None;
        }
        let tuple = self.py_alloc_seq_fast(ttuple, args)?;
        let map = self.py_attrs_new(0);
        let vals = [cls, map, tuple, Value::NULL, context, Value::int(-1), Value::FALSE, Value::NULL, Value::int(0), Value::UNDEFINED];
        self.py_alloc_exc_fast(&vals)
    }

    /// The Array for [`Instr::PyUnpack`].
    pub(super) fn py_unpack(&self, v: Value, rt: Value, n: u32) -> Option<Value> {
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
    /// entry, `None` for the slow edge (the table's own lookup, `vm::py_table`).
    pub(super) fn py_dict_lookup(&mut self, d: Value, k: Value, rt: Value) -> Option<Option<Value>> {
        if !d.is_heap() {
            return None;
        }
        let tdict = self.py_rt_for(rt)?.t_dict;
        // A dict record of the template's shape: its `map` read with it.
        if let Some((_, map, _)) = self.py_dict_rec_fast(d, tdict) {
            return self.py_table_get(map, k);
        }
        let cls = self.py_cls_of(d)?;
        let sk = self.py_sub_kind(cls);
        if cls.bits() != tdict.bits()
            && (sk & (super::py_rt::SUB_DICT_GET | super::py_rt::SUB_DICT_GET_HIT)) == 0
        {
            return None;
        }
        let (_, map) = self.py_dict_field(d.heap_index(), 0)?;
        let res = self.py_table_get(map, k);
        if matches!(res, Some(None))
            && cls.bits() != tdict.bits()
            && (sk & super::py_rt::SUB_DICT_GET) == 0
        {
            return None;
        }
        res
    }

    /// Whether `t` is in class `c`'s `mro`, served from `PyRt::isinstance_cache`
    /// keyed by `(c, c_hver, c_ver, t, t_hver)`.
    #[inline]
    fn py_mro_contains_cached(&self, c: Value, t: Value, ty: super::py_rt::TySlots) -> Option<bool> {
        if c.bits() == t.bits() {
            return Some(true);
        }
        if !c.is_heap() || !t.is_heap() {
            return None;
        }
        let (ci, ti) = (c.heap_index(), t.heap_index());
        let (c_hver, t_hver) = (self.heap.version_of(ci), self.heap.version_of(ti));
        let c_ver = match self.heap.get(ci) {
            HeapObj::Object(m) if !m.is_ctor && ty.ver < m.slot_len() => Some(m.val_at(ty.ver).bits()),
            _ => None,
        };
        let slot = ((ci.wrapping_mul(31) ^ ti) as usize) & (super::py_rt::ISINST_CACHE_LEN - 1);
        if let (Some(cv), Some(p)) = (c_ver, self.py_rt.as_deref()) {
            let ce = p.isinstance_cache[slot].get();
            if ce.result != 0
                && ce.c == c.bits()
                && ce.c_hver == c_hver
                && ce.c_ver == cv
                && ce.t == t.bits()
                && ce.t_hver == t_hver
            {
                return Some(ce.result == 2);
            }
        }
        let mro = self.py_ty_field(ci, ty.mro, "mro")?;
        if !mro.is_heap() {
            return None;
        }
        let HeapObj::Array(items) = self.heap.get(mro.heap_index()) else {
            return None;
        };
        let found = items.iter().any(|x| x.bits() == t.bits());
        if let (Some(cv), Some(p)) = (c_ver, self.py_rt.as_deref()) {
            p.isinstance_cache[slot].set(super::py_rt::IsInstEntry {
                c: c.bits(),
                c_hver,
                c_ver: cv,
                t: t.bits(),
                t_hver,
                result: if found { 2 } else { 1 },
            });
        }
        Some(found)
    }

    /// For [`Instr::PyRaise`] / [`Instr::PyCaught`]: whether `e` is an
    /// exception record (own data `cls` a plain object whose own data `mro`
    /// Array holds `rt.EBASE`; for a raise, no own `isType` of `true`), and
    /// the slots of its own data `context` (a raise only) and `tbline`.
    pub(super) fn py_exc_record(&self, e: Value, rt: Value, raise: bool) -> Option<(usize, usize)> {
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
        debug_assert!(EXC_KEYS[4] == "context" && EXC_KEYS[5] == "tbline" && EXC_KEYS[7] == "tbfn" && EXC_KEYS[9] == "frames");
        if !self.py_plain_rec(cls) {
            return None;
        }
        self.py_mro_contains_cached(cls, ebase, ty)?.then_some((ctx_slot, tb_slot))
    }

    /// Whether the exception record at `idx` has a catch pending (its
    /// `tbfn`, see `Vm::py_note_catch`); `true` when that cannot be told.
    fn py_exc_caught(&self, idx: u32) -> bool {
        let exc_shape = self.py_rt.as_deref().map(|p| p.exc_shape);
        let HeapObj::Object(m) = self.heap.get(idx) else {
            return true;
        };
        let shape = m.shape();
        let slot = if shape != crate::shape::DICT && exc_shape == Some(shape) { Some(7) } else { m.pos("tbfn") };
        slot.is_none_or(|s| m.val_at(s) != Value::NULL)
    }

    /// [`Instr::PyRaise`] (see the instruction): its step, for the
    /// interpreter's `py_step_ext` and compiled code's `jit_py_raise`. The
    /// next ip (`slow`, or `catch` when the frame's own innermost handler is
    /// that catch handler), or the throw (pending).
    #[inline(never)]
    pub(super) fn py_raise_step(&mut self, base: usize, ip: usize, e: u16, rt: u16, slow: u32, catch: u32) -> Result<usize, Thrown> {
        let (ev, rv) = (self.get(base, e), self.get(base, rt));
        let Some((ctx_slot, tb_slot, cur)) = self.py_raise_plan(ev, rv) else {
            return Ok(slow as usize);
        };
        let idx = ev.heap_index();
        // Raising an exception some frame caught (its `tbfn` set):
        // `R.raise` first records that frame's traceback entry.
        if self.py_exc_caught(idx) {
            return Ok(slow as usize);
        }
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
        // The frame's own innermost handler is the catch handler the
        // instruction names: caught here, as `unwind_to_handler`
        // catches a throw in the frame that raised it (the handler
        // popped, the frame's line noted, the exception in the
        // handler's register), without leaving the dispatch loop.
        if let Some(&Handler::Catch { target, reg }) = self.frames[top].handlers.last() {
            if target == catch {
                self.frames[top].handlers.pop();
                if self.py_unwind_candidate(top) {
                    self.py_unwind_frame(top, true, ev, true);
                }
                let fbase = self.frames[top].base;
                self.regs[fbase + reg as usize] = ev;
                self.frames[top].ip = target as usize;
                return Ok(target as usize);
            }
        }
        self.pending_throw = Some(ev);
        return Err(Thrown(String::new()));
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
    pub(super) fn py_seq(&mut self, items: Value, rt: Value, tuple: bool) -> Option<Value> {
        if !items.is_heap() {
            return None;
        }
        let items_idx = items.heap_index();
        match self.heap.get(items_idx) {
            HeapObj::Array(a) if a.len() <= (1 << 24) => {}
            _ => return None,
        }
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        self.py_repair_seq_arr_mirror(items_idx);
        let p = self.py_rt_for(rt)?;
        let cls = if tuple { p.t_tuple } else { p.t_list };
        self.py_alloc_seq_fast(cls, items)
    }

    /// `len(v)` for [`Instr::PyLen`].
    pub(super) fn py_len(&mut self, v: Value, rt: Value) -> Option<usize> {
        if !v.is_heap() {
            return None;
        }
        if self.heap.is_str_like(v.heap_index()) {
            return self.py_str_len(v);
        }
        let p = self.py_rt_for(rt)?;
        let (tl, tt, td, ts, tfs, tb) = (
            p.t_list.bits(),
            p.t_tuple.bits(),
            p.t_dict.bits(),
            p.t_set.bits(),
            p.t_frozenset.bits(),
            p.istypes[5].bits(),
        );
        // A record of the sequence or dict template's shape, read once (its
        // class, then its items or its `size`, at their slots), as the
        // general code below reads it in three passes.
        let (seq_shape, dict_shape, size_slot) = (p.seq_shape, p.dict_shape, p.dict_slots[1]);
        if let HeapObj::Object(m) = self.heap.get(v.heap_index()) {
            let shape = m.shape();
            if !m.is_ctor && shape != crate::shape::DICT {
                let cls = m.val_at(0).bits();
                if shape == seq_shape && (cls == tl || cls == tt || cls == tb) {
                    let items = m.val_at(1);
                    if !items.is_heap() {
                        return None;
                    }
                    return match self.heap.get(items.heap_index()) {
                        HeapObj::Array(a) => Some(a.len()),
                        _ => None,
                    };
                }
                if shape == dict_shape && (cls == td || cls == ts || cls == tfs) {
                    let size = m.val_at(size_slot);
                    if !size.is_number() {
                        return None;
                    }
                    let n = size.as_f64();
                    return (n >= 0.0 && n.fract() == 0.0 && n < 9_007_199_254_740_992.0).then_some(n as usize);
                }
            }
        }
        let cls_v = self.py_cls_of(v)?;
        let cls = cls_v.bits();
        if cls == tl || cls == tt || cls == tb {
            let (_, items) = self.py_seq_parts(v)?;
            if !items.is_heap() {
                return None;
            }
            return match self.heap.get(items.heap_index()) {
                HeapObj::Array(a) => Some(a.len()),
                _ => None,
            };
        }
        if cls == td || cls == ts || cls == tfs {
            let (_, size) = self.py_dict_field(v.heap_index(), 1)?;
            if !size.is_number() {
                return None;
            }
            let n = size.as_f64();
            return (n >= 0.0 && n.fract() == 0.0 && n < 9_007_199_254_740_992.0).then_some(n as usize);
        }
        let sk = self.py_sub_kind(cls_v);
        if (sk & super::py_rt::SUB_DICT_LEN) != 0 {
            let (_, size) = self.py_dict_field(v.heap_index(), 1)?;
            if !size.is_number() {
                return None;
            }
            let n = size.as_f64();
            return (n >= 0.0 && n.fract() == 0.0 && n < 9_007_199_254_740_992.0).then_some(n as usize);
        }
        if (sk & super::py_rt::SUB_STR_LEN) != 0 {
            let pyval = self.py_own_data(v.heap_index(), "pyval")?;
            return self.py_str_len(pyval);
        }
        if (sk & (super::py_rt::SUB_LIST_LEN | super::py_rt::SUB_TUPLE_LEN)) != 0 {
            let (_, items) = self.py_seq_parts(v)?;
            if !items.is_heap() {
                return None;
            }
            return match self.heap.get(items.heap_index()) {
                HeapObj::Array(a) => Some(a.len()),
                _ => None,
            };
        }
        None
    }

    /// Python truthiness of `v` for [`Instr::PyTruth`], matching `types.js:truth`.
    /// Returns `None` when a custom `__bool__` or `__len__` must be invoked.
    pub(super) fn py_truth(&mut self, v: Value, rt: Value) -> Option<bool> {
        if v == Value::TRUE {
            return Some(true);
        }
        if v == Value::FALSE || v == Value::NULL || v.is_undefined() {
            return Some(false);
        }
        if let Some(n) = v.small_bigint_val() {
            return Some(n != 0);
        }
        if v.is_number() {
            return Some(v.as_f64() != 0.0);
        }
        if !v.is_heap() {
            return None;
        }
        let idx = v.heap_index();
        match self.heap.get(idx) {
            HeapObj::Str(s) => return Some(s.units() != 0),
            HeapObj::Cons { len, .. } => return Some(*len != 0),
            HeapObj::BigInt(n) => return Some(*n != 0),
            HeapObj::BigIntBig(b) => return Some(b.sign() != num_bigint::Sign::NoSign),
            _ => {}
        }
        let p = self.py_rt_for(rt)?;
        let (tl, tt, td, ts, tfs, tb) = (
            p.t_list.bits(),
            p.t_tuple.bits(),
            p.t_dict.bits(),
            p.t_set.bits(),
            p.t_frozenset.bits(),
            p.istypes[5].bits(),
        );
        let (seq_shape, dict_shape, inst_shape, size_slot, ty) =
            (p.seq_shape, p.dict_shape, p.inst_shape, p.dict_slots[1], p.ty);
        let mut is_plain_inst_shape = false;
        if let HeapObj::Object(m) = self.heap.get(idx) {
            let shape = m.shape();
            if !m.is_ctor && shape != crate::shape::DICT {
                let cls = m.val_at(0).bits();
                if shape == seq_shape && (cls == tl || cls == tt || cls == tb) {
                    let items = m.val_at(1);
                    if !items.is_heap() {
                        return None;
                    }
                    return match self.heap.get(items.heap_index()) {
                        HeapObj::Array(a) => Some(!a.is_empty()),
                        _ => None,
                    };
                }
                if shape == dict_shape && (cls == td || cls == ts || cls == tfs) {
                    let size = m.val_at(size_slot);
                    return size.is_number().then(|| size.as_f64() != 0.0);
                }
                if shape == inst_shape {
                    is_plain_inst_shape = true;
                }
            }
        }
        let cls_v = self.py_cls_of(v)?;
        let cls = cls_v.bits();
        if cls == tl || cls == tt || cls == tb {
            let (_, items) = self.py_seq_parts(v)?;
            if !items.is_heap() {
                return None;
            }
            return match self.heap.get(items.heap_index()) {
                HeapObj::Array(a) => Some(!a.is_empty()),
                _ => None,
            };
        }
        if cls == td || cls == ts || cls == tfs {
            let (_, size) = self.py_dict_field(idx, 1)?;
            return size.is_number().then(|| size.as_f64() != 0.0);
        }
        let ty = ty?;
        if !is_plain_inst_shape {
            let HeapObj::Object(m) = self.heap.get(idx) else {
                return None;
            };
            if m.is_ctor
                || m.pos("isType")
                    .is_some_and(|s| !m.is_accessor_at(s) && m.val_at(s) == Value::TRUE)
            {
                return None;
            }
        }
        if cls_v.is_heap() {
            let mut truth_eval: Option<(u8, Value)> = None;
            if let HeapObj::Object(cm) = self.heap.get(cls_v.heap_index()) {
                if !cm.is_ctor
                    && ty.truth_kind < cm.slot_len()
                    && ty.truth_ver < cm.slot_len()
                    && ty.ver < cm.slot_len()
                    && super::py_rt::key_eq(cm.key_at(ty.ver), "ver")
                    && super::py_rt::key_eq(cm.key_at(ty.truth_ver), "truthVer")
                    && super::py_rt::key_eq(cm.key_at(ty.truth_kind), "truthKind")
                {
                    let ver = cm.val_at(ty.ver);
                    let tver = cm.val_at(ty.truth_ver);
                    let tkind = cm.val_at(ty.truth_kind);
                    if ver.is_number() && tver.is_number() && ver.as_f64() == tver.as_f64() && tkind.is_number() {
                        let k = tkind.as_f64();
                        if k == 1.0 {
                            return Some(true);
                        }
                        if (k == 2.0 || k == 3.0)
                            && ty.truth_fn < cm.slot_len()
                            && super::py_rt::key_eq(cm.key_at(ty.truth_fn), "truthFn")
                        {
                            truth_eval = Some((if k == 2.0 { 2 } else { 3 }, cm.val_at(ty.truth_fn)));
                        }
                    }
                }
            }
            // Fast path for unmodified dict/str subclasses (`AttributeDict`, `NavigableString`).
            let sk = self.py_sub_kind(cls_v);
            if (sk & super::py_rt::SUB_DICT_LEN) != 0 {
                let (_, size) = self.py_dict_field(idx, 1)?;
                return size.is_number().then(|| size.as_f64() != 0.0);
            }
            if (sk & super::py_rt::SUB_STR_LEN) != 0 {
                let pyval = self.py_own_data(idx, "pyval")?;
                if pyval.is_heap() {
                    return match self.heap.get(pyval.heap_index()) {
                        HeapObj::Str(s) => Some(s.units() != 0),
                        HeapObj::Cons { len, .. } => Some(*len != 0),
                        _ => None,
                    };
                }
            }
            if (sk & (super::py_rt::SUB_LIST_LEN | super::py_rt::SUB_TUPLE_LEN)) != 0 {
                let (_, items) = self.py_seq_parts(v)?;
                if items.is_heap() {
                    if let HeapObj::Array(a) = self.heap.get(items.heap_index()) {
                        return Some(!a.is_empty());
                    }
                }
            }
            // Frameless leaf execution for pure Python `__bool__` / `__len__` (e.g. `Tag.__bool__`).
            if let Some((kind, tfn)) = truth_eval {
                if self.py_plain_rec(tfn)
                    && self.py_own_data(tfn.heap_index(), "unboxSelf") != Some(Value::TRUE)
                {
                    if let Some(c1) = self.py_own_data(tfn.heap_index(), "c1") {
                        if c1.is_heap() {
                            if let HeapObj::Func(fid) = *self.heap.get(c1.heap_index()) {
                                if let Some(info) = self.py_leaf_info(fid) {
                                    if info.params == 1 {
                                        if let Some(ret) = self.py_leaf_exec_pure_1arg(fid, tfn, info, v) {
                                            if kind == 2 {
                                                if ret == Value::TRUE {
                                                    return Some(true);
                                                }
                                                if ret == Value::FALSE {
                                                    return Some(false);
                                                }
                                            } else if let Some(n) = self.bigint_i128(ret) {
                                                if n >= 0 {
                                                    return Some(n != 0);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// The module global for [`Instr::PyModGet`].
    ///
    /// The site's entry names the globals Map and the name's entry in it,
    /// as `PyGlobal`'s does (the Map's keys as they were, the key there by
    /// its bits and stamp), so a hit reads that entry with no hashing.
    pub(super) fn py_mod_get(&mut self, func_id: u32, ip: usize, o: Value, rt: Value, key: u32) -> Option<Value> {
        let (tmodule, ty) = {
            let p = self.py_rt_for(rt)?;
            (p.t_module, p.ty)
        };
        let cls = self.py_cls_of(o)?;
        if tmodule.bits() != cls.bits() {
            // Class objects (`o.isType == true`, e.g. `str.__new__`, `Cls.method(...)`):
            // serve cached callables from `o.gt` via `ic::TYPE_CALL_ATTR`.
            if let Some(ty) = ty {
                if self.py_ty_field(o.heap_index(), ty.is_type, "isType") == Some(Value::TRUE) {
                    let e = self.py_ic_class(func_id, ip, o.bits());
                    if e.kind == ic::TYPE_CALL_ATTR && e.a == o.bits() && self.py_cls_stamp_ok(o, e.hver, e.c) {
                        return Some(Value::from_bits(e.b));
                    }
                    let name = self.py_const_name(func_id, key)?;
                    let (_, _, v) = self.py_table_entry(o, ty.gt, "gt", name)?;
                    if !v.is_undefined() && v != Value::HOLE {
                        if let Some((hver, ver)) = self.py_cls_stamp(o) {
                            self.py_ic_put(func_id, ip, PyIc { kind: ic::TYPE_CALL_ATTR, pos: 0, hver, a: o.bits(), b: v.bits(), c: ver, d: 0 });
                        }
                        return Some(v);
                    }
                    return None;
                }
                // Non-module user-class instances: fast check for cached `@classmethod`
                // (`cls.gc`, e.g. `self._make_match_rules(...)`) before `py_attr_get`.
                let e = self.py_ic_class(func_id, ip, cls.bits());
                if e.kind == ic::CLASS_METHOD && e.a == cls.bits() && self.py_cls_stamp_ok(cls, e.hver, e.c) {
                    let (_, dict) = self.py_inst_parts(o)?;
                    if e.d != 0 && self.py_attrs_of(dict).is_some_and(|(l, _)| l == e.d) {
                        return Some(Value::from_bits(e.b));
                    }
                    if dict.is_undefined() || dict.is_null() {
                        return Some(Value::from_bits(e.b));
                    }
                    let name = self.py_const_name(func_id, key)?;
                    let k = self.resolve_const_slot(func_id, key);
                    let (lacks, l) = self.py_store_lacks(dict, name, k, e.d)?;
                    self.py_note_lacks(func_id, ip, e, l);
                    if lacks {
                        return Some(Value::from_bits(e.b));
                    }
                }
            }
            // Own instance-dict callables (`self.out(...)`) and cached `@staticmethod`
            // / plain class attributes (`cls.gv`, e.g. `self.is_tag(el)`).
            let mut call = None;
            if let Some(v) = self.py_attr_get(func_id, ip, o, key, &mut call) {
                return Some(v);
            }
            // Learn `@classmethod` (`cls.gc`) when `py_attr_get` declined without a property.
            if call.is_none() {
                if let Some(ty) = ty {
                    let (_, dict) = self.py_inst_parts(o)?;
                    let name = self.py_const_name(func_id, key)?;
                    if let Some((_, _, cm)) = self.py_table_entry(cls, ty.gc, "gc", name) {
                        if self.py_plain_rec(cm) {
                            let no_dict = dict.is_undefined() || dict.is_null();
                            let (lacks, l) = if no_dict {
                                (true, 0)
                            } else {
                                let k = self.resolve_const_slot(func_id, key);
                                self.py_store_lacks(dict, name, k, 0)?
                            };
                            if lacks {
                                if let Some((hver, ver)) = self.py_cls_stamp(cls) {
                                    self.py_ic_put(func_id, ip, PyIc { kind: ic::CLASS_METHOD, pos: 0, hver, a: cls.bits(), b: cm.bits(), c: ver, d: l });
                                }
                                return Some(cm);
                            }
                        }
                    }
                }
            }
            return None;
        }
        let (g, gslot) = self.py_hint_slot(super::py_rt::hint::GLOBALS, o.heap_index(), "globals")?;
        if !g.is_heap() {
            return None;
        }
        if let Some(p) = self.py_rt.as_deref() {
            if p.mod_shape.get() == crate::shape::DICT {
                if let (Ok(gs16), HeapObj::Object(om)) = (u16::try_from(gslot), self.heap.get(o.heap_index())) {
                    if !om.is_ctor
                        && om.shape() != crate::shape::DICT
                        && om.slot_len() > gslot
                        && super::py_rt::key_eq(om.key_at(0), "cls")
                        && !om.is_accessor_at(0)
                    {
                        p.mod_shape.set(om.shape());
                        p.mod_globals_slot.set(gs16);
                    }
                }
            }
        }
        let gi = g.heap_index();
        let e = self.py_ic(func_id, ip);
        if e.kind == ic::MOD_GET && e.a == g.bits() && self.heap.version_of(gi) == e.hver {
            if let Some(v) = self.py_map_at_bits(g, e.pos, e.b, e.d) {
                return Some(v);
            }
        }
        let k = self.resolve_const_slot(func_id, key);
        let hver = self.heap.version_of(gi);
        let (i, v) = self.py_map_find(g, k)?;
        let kv = match self.heap.get(gi) {
            HeapObj::Map { keys, .. } => *keys.get(i)?,
            _ => return None,
        };
        if let (Ok(pos), Some(kver)) = (u32::try_from(i), self.py_key_stamp(func_id, key, kv)) {
            self.py_ic_put(func_id, ip, PyIc { kind: ic::MOD_GET, pos, hver, a: g.bits(), b: kv.bits(), c: 0, d: kver });
        }
        Some(v)
    }

    /// `isinstance(v, t)` for [`Instr::PyIsInstance`], including tuple RHS
    /// `isinstance(v, (A, B, ...))` over plain types.
    #[inline(never)]
    pub(super) fn py_isinstance(&mut self, func_id: u32, ip: usize, v: Value, t: Value, rt: Value) -> Option<bool> {
        if !t.is_heap() {
            return None;
        }
        let p = self.py_rt_for(rt)?;
        let (ty, istypes, t_tuple) = (p.ty?, p.istypes, p.t_tuple.bits());
        let ti = t.heap_index();
        let resolve_val_cls = |vm: &Self| -> Option<Value> {
            let prim = if v == Value::NULL {
                Some(0)
            } else if v == Value::TRUE || v == Value::FALSE {
                Some(1)
            } else if v.is_number() {
                Some(3)
            } else if v.is_small_bigint() {
                Some(2)
            } else if !v.is_heap() {
                return None;
            } else if vm.heap.is_str_like(v.heap_index()) {
                Some(4)
            } else if matches!(vm.heap.get(v.heap_index()), HeapObj::BigInt(_) | HeapObj::BigIntBig(_)) {
                Some(2)
            } else {
                None
            };
            match prim {
                Some(i) => Some(istypes[i]),
                None => {
                    let c = vm.py_cls_of(v)?;
                    vm.py_plain_rec(c).then_some(c)
                }
            }
        };
        // `t` a type: this site's entry, else its own `isType` (or a tuple of plain types).
        let e = self.py_ic(func_id, ip);
        if !(e.kind == ic::IS_TYPE && e.a == t.bits() && self.heap.version_of(ti) == e.hver) {
            if self.py_ty_field(ti, ty.is_type, "isType") != Some(Value::TRUE) {
                // Tuple of types: `isinstance(v, (A, B, ...))`.
                if self.py_cls_of(t)?.bits() != t_tuple {
                    return None;
                }
                let (_, items) = self.py_seq_parts(t)?;
                if !items.is_heap() {
                    return None;
                }
                let mut elems = [Value::UNDEFINED; 16];
                let count = match self.heap.get(items.heap_index()) {
                    HeapObj::Array(a) if a.len() <= elems.len() => {
                        for (dst, &el) in elems.iter_mut().zip(a.iter()) {
                            *dst = el;
                        }
                        a.len()
                    }
                    _ => return None,
                };
                let c = resolve_val_cls(self)?;
                let type_type = self.py_hint_field(super::py_rt::hint::HIT_CLS, istypes[0].heap_index(), "cls")?;
                // Validate every element before returning `false` (CPython checks left-to-right,
                // returning `true` on the first match and validating types up to that point).
                for &el in &elems[..count] {
                    if !el.is_heap() {
                        return None;
                    }
                    let ei = el.heap_index();
                    if self.py_ty_field(ei, ty.is_type, "isType") != Some(Value::TRUE) {
                        return None;
                    }
                    let el_cls = self.py_hint_field(super::py_rt::hint::HIT_CLS, ei, "cls")?;
                    if el_cls != type_type {
                        return None;
                    }
                    if c.bits() == el.bits() {
                        return Some(true);
                    }
                    if self.py_mro_contains_cached(c, el, ty)? {
                        if el.bits() != istypes[5].bits() {
                            return Some(true);
                        }
                    }
                }
                return Some(false);
            }
            // Custom metaclasses (e.g. ABCMeta) may override `__instancecheck__`.
            let t_cls = self.py_hint_field(super::py_rt::hint::HIT_CLS, ti, "cls")?;
            let type_type = self.py_hint_field(super::py_rt::hint::HIT_CLS, istypes[0].heap_index(), "cls")?;
            if t_cls != type_type {
                return None;
            }
            let hver = self.heap.version_of(ti);
            self.py_ic_put(func_id, ip, PyIc { kind: ic::IS_TYPE, pos: 0, hver, a: t.bits(), b: 0, c: 0, d: 0 });
        }
        let c = resolve_val_cls(self)?;
        if c.bits() == t.bits() {
            return Some(true);
        }
        if !self.py_mro_contains_cached(c, t, ty)? {
            return Some(false);
        }
        // `bytes` excludes bytearray, which the runtime models as a subclass.
        (t.bits() != istypes[5].bits()).then_some(true)
    }

    /// `s[k]` for [`Instr::PyStrItem`]: well-formed BMP (incl. ASCII) single
    /// index or step-1 slice, including unmodified `str` subclasses (`pyval`).
    fn py_str_item(&mut self, s: Value, k: Value) -> Option<Value> {
        if !s.is_heap() {
            return None;
        }
        let s = if self.heap.is_str_like(s.heap_index()) {
            s
        } else {
            let cls = self.py_cls_of(s)?;
            if (self.py_sub_kind(cls) & super::py_rt::SUB_STR_GET) == 0 {
                return None;
            }
            let pyval = self.py_own_data(s.heap_index(), "pyval")?;
            if !pyval.is_heap() || !self.heap.is_str_like(pyval.heap_index()) {
                return None;
            }
            pyval
        };
        let idx = s.heap_index();
        self.heap.flatten(idx);
        let (len, is_ascii) = {
            let HeapObj::Str(st) = self.heap.get(idx) else {
                return None;
            };
            if st.is_ascii() {
                (st.as_bytes().len(), true)
            } else if st.is_wellformed() && !st.has_surrogate() {
                (st.units(), false)
            } else {
                return None;
            }
        };
        let n = len as i128;
        if let Some(i) = self.bigint_i128(k) {
            let at = if i < 0 { i + n } else { i };
            if at < 0 || at >= n {
                return None;
            }
            let at = at as usize;
            if is_ascii {
                let HeapObj::Str(st) = self.heap.get(idx) else {
                    return None;
                };
                // A one-character ASCII str is the interned string at its byte.
                return Some(Value::heap(st.as_bytes()[at] as u32));
            }
            let char_js = {
                let HeapObj::Str(st) = self.heap.get(idx) else {
                    return None;
                };
                let (ba, bb) = st.bmp_byte_range(at, at + 1);
                let b = &st.as_bytes()[ba..bb];
                if b.len() == 1 && b[0] < 128 {
                    return Some(Value::heap(b[0] as u32));
                }
                crate::heap::JsStr::from_bmp_utf8_slice(b, 1)
            };
            return Some(Value::heap(self.heap.alloc_js(char_js)));
        }
        if !k.is_heap() {
            return None;
        }
        let (t_slice, slice_shape) = {
            let p = self.py_rt.as_deref()?;
            if p.slice_shape == crate::shape::DICT {
                return None;
            }
            (p.t_slice.bits(), p.slice_shape)
        };
        let (start_v, stop_v) = match self.heap.get(k.heap_index()) {
            HeapObj::Object(m)
                if !m.is_ctor
                    && m.shape() == slice_shape
                    && m.val_at(0).bits() == t_slice
                    && m.val_at(3) == Value::NULL =>
            {
                (m.val_at(1), m.val_at(2))
            }
            _ => return None,
        };
        let lo = if start_v == Value::NULL {
            0usize
        } else {
            let i = self.bigint_i128(start_v)?;
            if i < 0 {
                (i + n).max(0) as usize
            } else {
                i.min(n) as usize
            }
        };
        let hi = if stop_v == Value::NULL {
            len
        } else {
            let i = self.bigint_i128(stop_v)?;
            if i < 0 {
                (i + n).max(0) as usize
            } else {
                i.min(n) as usize
            }
        };
        if lo >= hi {
            return Some(Value::heap(crate::heap::INTERN_EMPTY));
        }
        let slice_len = hi - lo;
        if slice_len > 65536 {
            return None;
        }
        #[cfg(feature = "instrument")]
        if self.instr_rec.is_some() && slice_len > 1024 {
            return None;
        }
        self.charge_steps(slice_len as i64);
        if is_ascii {
            return Some(self.ascii_slice_value(idx, lo..hi));
        }
        let slice_js = {
            let HeapObj::Str(st) = self.heap.get(idx) else {
                return None;
            };
            let (ba, bb) = st.bmp_byte_range(lo, hi);
            let b = &st.as_bytes()[ba..bb];
            if b.len() == 1 && b[0] < 128 {
                return Some(Value::heap(b[0] as u32));
            }
            crate::heap::JsStr::from_bmp_utf8_slice(b, slice_len)
        };
        Some(Value::heap(self.heap.alloc_js(slice_js)))
    }

    /// `len(s)` for [`Instr::PyStrLen`]: the code points of a str with no
    /// lone surrogate (as the runtime's `strLen` counts them). O(1) for ASCII
    /// and well-formed BMP strings via `JsStr::has_surrogate()`.
    fn py_str_len(&mut self, s: Value) -> Option<usize> {
        if !s.is_heap() || !self.heap.is_str_like(s.heap_index()) {
            return None;
        }
        let idx = s.heap_index();
        self.heap.flatten(idx);
        let HeapObj::Str(st) = self.heap.get(idx) else {
            return None;
        };
        if st.is_ascii() {
            return Some(st.as_bytes().len());
        }
        if !st.is_wellformed() {
            return None;
        }
        if !st.has_surrogate() {
            return Some(st.units());
        }
        Some(st.as_bytes().iter().filter(|&&b| b & 0xC0 != 0x80).count())
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

    /// `o.dict` when it is a `Map` or a table (a Python instance's attribute
    /// storage, which answers `map_method` alike: `vm::py_table::layout`).
    pub(super) fn py_inst_map(&self, o: Value) -> Option<u32> {
        let (_, d) = self.py_inst_parts(o)?;
        (d.is_heap() && matches!(self.heap.get(d.heap_index()), HeapObj::Map { .. } | HeapObj::PyAttrs { .. } | HeapObj::PyTable(_)))
            .then(|| d.heap_index())
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
        Value::bool(s.has_surrogate())
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
        // Op 4: all-in-one native sort `PYORD(4, items, key, reverse, T.tuple)`.
        // Evaluates pure side-effect-free key lambdas (including zero-allocation
        // tuple-key lambdas like `lambda v: (v % 10, -v)` and `lambda kv: (-kv[1], kv[0])`),
        // sorts stably with CPython's exact `reverse` tie-breaking, and returns the
        // sorted `Array` directly without intermediate JS loops or permutation arrays.
        if op == Value::int(4) {
            return Ok(self.py_ord_sort_all(args).unwrap_or(Value::UNDEFINED));
        }
        // Op 5: O(N) top-k selection `PYORD(5, items, k, largest, T.tuple)` for
        // `heapq.nsmallest` / `heapq.nlargest` with `key=None` over small ints.
        if op == Value::int(5) {
            return Ok(self.py_ord_topk(args).unwrap_or(Value::UNDEFINED));
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
        if !self.py_ord_sort_columns(&keys, &mut order) {
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
        }
        self.charge_steps(i64::try_from(cost).unwrap_or(i64::MAX));
        let out: Vec<Value> = order.into_iter().map(|i| Value::int(i as i32)).collect();
        Ok(Value::heap(self.heap.alloc(HeapObj::Array(out))))
    }

    /// `__zipp_py_ord(5, items, k, largest)`: O(N) top-k selection for
    /// `heapq.nsmallest` / `heapq.nlargest` with `key=None` over small ints
    /// (`1 <= k <= 64`). Preserves exact CPython first-seen tie order and
    /// returns `None` (zero side effects) on any non-small-int element.
    fn py_ord_topk(&mut self, args: &[Value]) -> Option<Value> {
        let items_v = args.get(1).copied()?;
        let k_v = args.get(2).copied()?;
        let largest = args.get(3).copied() == Some(Value::int(1));
        if !k_v.is_int() {
            return None;
        }
        let k = usize::try_from(k_v.as_int()).ok()?;
        if k == 0 || k > 64 || !items_v.is_heap() {
            return None;
        }
        if !self.array_js_len.is_empty() && self.array_js_len.contains_key(&items_v.heap_index()) {
            return None;
        }
        let n = match self.heap.get(items_v.heap_index()) {
            HeapObj::Array(items) if items.len() > 1 => items.len(),
            _ => return None,
        };
        let cost = (n as u64).saturating_mul(ORD_STEPS_PER_UNIT);
        if !self.native_kernel_admits(cost, k.saturating_mul(64)) {
            return None;
        }
        let mut top = [(0i64, Value::UNDEFINED); 64];
        let mut len = 0usize;
        {
            let HeapObj::Array(items) = self.heap.get(items_v.heap_index()) else {
                return None;
            };
            if largest {
                for &v in items {
                    let x = v.small_bigint_val()?;
                    if len == k {
                        if x <= top[k - 1].0 {
                            continue;
                        }
                        let mut pos = k - 1;
                        while pos > 0 && x > top[pos - 1].0 {
                            top[pos] = top[pos - 1];
                            pos -= 1;
                        }
                        top[pos] = (x, v);
                    } else {
                        let mut pos = len;
                        while pos > 0 && x > top[pos - 1].0 {
                            top[pos] = top[pos - 1];
                            pos -= 1;
                        }
                        top[pos] = (x, v);
                        len += 1;
                    }
                }
            } else {
                for &v in items {
                    let x = v.small_bigint_val()?;
                    if len == k {
                        if x >= top[k - 1].0 {
                            continue;
                        }
                        let mut pos = k - 1;
                        while pos > 0 && x < top[pos - 1].0 {
                            top[pos] = top[pos - 1];
                            pos -= 1;
                        }
                        top[pos] = (x, v);
                    } else {
                        let mut pos = len;
                        while pos > 0 && x < top[pos - 1].0 {
                            top[pos] = top[pos - 1];
                            pos -= 1;
                        }
                        top[pos] = (x, v);
                        len += 1;
                    }
                }
            }
        }
        if self.instr_rec.is_some() {
            self.charge_steps(i64::try_from(cost).unwrap_or(i64::MAX));
        }
        let mut out = Vec::with_capacity(len);
        for &(_, v) in &top[..len] {
            out.push(v);
        }
        let arr_idx = self.heap.alloc_array_settled(out);
        self.realm_born(arr_idx, self.arr_proto);
        Some(Value::heap(arr_idx))
    }

    /// `__zipp_py_ord(4, items, key, reverse, T.tuple)`: all-in-one native sort
    /// for `sortItems` (`builtins.js`). Handles `key === null` and pure
    /// side-effect-free key lambdas (both plain leaf keys like `lambda r: r[0]`
    /// and zero-allocation tuple keys like `lambda v: (v % 10, -v)` or
    /// `lambda kv: (-kv[1], kv[0])`), returning the sorted `Array` directly or
    /// `None` (zero side effects) to fall back to `sortItems`.
    fn py_ord_sort_all(&mut self, args: &[Value]) -> Option<Value> {
        let items_v = args.get(1).copied()?;
        let key = args.get(2).copied()?;
        let reverse = args.get(3).copied() == Some(Value::int(1));
        let tuple = args.get(4).copied()?;
        if !items_v.is_heap() || (!self.array_js_len.is_empty() && self.array_js_len.contains_key(&items_v.heap_index())) {
            return None;
        }
        let mut values: Vec<Value> = match self.heap.get(items_v.heap_index()) {
            HeapObj::Array(items) if items.len() > 1 => items.clone(),
            _ => return None,
        };
        let n = values.len();
        let log = (usize::BITS - n.leading_zeros()) as u64;
        let extra = if key == Value::NULL { 0 } else { (n as u64).saturating_mul(16) };
        let cost = (n as u64)
            .saturating_mul(log + 1)
            .saturating_mul(ORD_STEPS_PER_UNIT)
            .saturating_add(extra);
        if !self.native_kernel_admits(cost, n.saturating_mul(64)) {
            return None;
        }
        if reverse {
            values.reverse();
        }
        let mut keys = Vec::with_capacity(n);
        if key == Value::NULL {
            for &v in &values {
                keys.push(self.py_ord_key_rec(v, 0, tuple, false)?);
            }
        } else {
            if super::prof_py::on() || !self.py_plain_rec(key) {
                return None;
            }
            let entry = self.py_own_data(key.heap_index(), "c1")?;
            if !entry.is_heap() {
                return None;
            }
            let fid = match self.heap.get(entry.heap_index()) {
                HeapObj::Func(id) => *id,
                HeapObj::Closure { func, .. } => *func,
                _ => return None,
            };
            if let Some(tk) = super::py_attr::py_leaf::py_leaf_tuple_key_scan(self.func(fid as usize)) {
                let mut elems = [Value::UNDEFINED; 4];
                for &v in &values {
                    let klen = self.py_leaf_exec_tuple_key(fid, key, tk, v, &mut elems)?;
                    let mut seq = Vec::with_capacity(klen);
                    for &ev in &elems[..klen] {
                        seq.push(self.py_ord_key_rec(ev, 0, tuple, false)?);
                    }
                    keys.push(OrdKey::Seq(seq));
                }
            } else {
                let info = self.py_leaf_info(fid)?;
                if !super::py_attr::py_leaf::py_leaf_is_pure_1arg(self.func(fid as usize), info) {
                    return None;
                }
                for &v in &values {
                    let kv = self.py_leaf_exec_pure_1arg(fid, key, info, v)?;
                    keys.push(self.py_ord_key_rec(kv, 0, tuple, false)?);
                }
            }
        }
        let mut order: Vec<usize> = (0..n).collect();
        if !self.py_ord_sort_columns(&keys, &mut order) {
            let undecided = std::cell::Cell::new(false);
            order.sort_by(|&i, &j| match self.py_ord_cmp(&keys[i], &keys[j]) {
                Some(o) => o,
                None => {
                    undecided.set(true);
                    std::cmp::Ordering::Equal
                }
            });
            if undecided.get() {
                return None;
            }
        }
        if self.instr_rec.is_some() {
            self.charge_steps(i64::try_from(cost).unwrap_or(i64::MAX));
        }
        let mut out = self.heap.take_arr_buf(n);
        if reverse {
            for i in order.into_iter().rev() {
                out.push(values[i]);
            }
        } else {
            for i in order {
                out.push(values[i]);
            }
        }
        Some(Value::heap(self.heap.alloc_array_settled(out)))
    }

    /// Sort `order` (the identity) by `keys` as [`Vm::py_ord_cmp`] orders
    /// them, stably, when every key is of one form whose order needs no
    /// case analysis per comparison: all ints of the fast tier, all floats
    /// with no NaN, all strs, or all tuples of one shape (the same lengths,
    /// nested the same way, at most [`ORD_COLUMNS_MAX`] items in all, at
    /// most [`ORD_COLUMNS_DEPTH`] deep) whose items at each place are all of
    /// one of those kinds. The keys are then read once into columns (the
    /// items in order, nested tuples' items in their place) and compared
    /// column by column, which is what `py_ord_cmp` answers for such pairs
    /// (two items of one kind compare as that kind, the first unequal pair
    /// decides, at whatever depth, and equal tuples of one shape are
    /// equal), without its match on two keys of every kind for every
    /// comparison. `false`, with `order` untouched, for any other set of
    /// keys.
    fn py_ord_sort_columns(&self, keys: &[OrdKey], order: &mut [usize]) -> bool {
        use super::bigint::BigVal;
        enum Col<'h> {
            Int(Vec<i128>),
            Float(Vec<f64>),
            Str(Vec<&'h [u8]>),
        }
        // A place in a key: the item indices from the top, with the length
        // the tuple at each step must have.
        type Path = Vec<(usize, usize)>;
        // The places of `k`'s items (`k` itself when it is no tuple), in
        // order; `None` past the bounds.
        fn places(k: &OrdKey, at: &mut Path, out: &mut Vec<Path>) -> Option<()> {
            match k {
                OrdKey::Seq(items) => {
                    if at.len() >= ORD_COLUMNS_DEPTH || items.is_empty() {
                        return None;
                    }
                    for (i, x) in items.iter().enumerate() {
                        at.push((i, items.len()));
                        places(x, at, out)?;
                        at.pop();
                    }
                }
                _ => {
                    if out.len() >= ORD_COLUMNS_MAX {
                        return None;
                    }
                    out.push(at.clone());
                }
            }
            Some(())
        }
        // `k`'s item at `path` when `k` has that shape there (a tuple of the
        // length at each step, and no tuple at the end).
        fn item_at<'k>(k: &'k OrdKey, path: &[(usize, usize)]) -> Option<&'k OrdKey> {
            let mut node = k;
            for &(i, len) in path {
                match node {
                    OrdKey::Seq(items) if items.len() == len => node = &items[i],
                    _ => return None,
                }
            }
            (!matches!(node, OrdKey::Seq(_))).then_some(node)
        }
        if !py_sort_columns_enabled() {
            return false;
        }
        let Some(first) = keys.first() else {
            return false;
        };
        let mut paths: Vec<Path> = Vec::new();
        if places(first, &mut Vec::new(), &mut paths).is_none() {
            return false;
        }
        let mut cols: Vec<Col<'_>> = Vec::with_capacity(paths.len());
        for path in &paths {
            let item = |k| item_at(k, path);
            let col = match item(first) {
                Some(OrdKey::Int(BigVal::Small(_))) => {
                    let mut v = Vec::with_capacity(keys.len());
                    for k in keys {
                        match item(k) {
                            Some(OrdKey::Int(BigVal::Small(x))) => v.push(*x),
                            _ => return false,
                        }
                    }
                    Col::Int(v)
                }
                Some(OrdKey::Float(_)) => {
                    let mut v = Vec::with_capacity(keys.len());
                    for k in keys {
                        match item(k) {
                            Some(OrdKey::Float(x)) if !x.is_nan() => v.push(*x),
                            _ => return false,
                        }
                    }
                    Col::Float(v)
                }
                Some(OrdKey::Str(_)) => {
                    let mut v = Vec::with_capacity(keys.len());
                    for k in keys {
                        match item(k) {
                            Some(OrdKey::Str(s)) => match self.heap.get(*s) {
                                HeapObj::Str(st) => v.push(st.as_bytes()),
                                _ => return false,
                            },
                            _ => return false,
                        }
                    }
                    Col::Str(v)
                }
                _ => return false,
            };
            cols.push(col);
        }
        order.sort_by(|&i, &j| {
            for col in &cols {
                let o = match col {
                    Col::Int(v) => v[i].cmp(&v[j]),
                    Col::Float(v) => v[i].partial_cmp(&v[j]).unwrap_or(std::cmp::Ordering::Equal),
                    Col::Str(v) => v[i].cmp(v[j]),
                };
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            std::cmp::Ordering::Equal
        });
        true
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
        if let Some(n) = v.small_bigint_val() {
            return Some(OrdKey::Int(BigVal::Small(n as i128)));
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
            HeapObj::BigIntBig(b) => Some(OrdKey::Int(BigVal::Big(num_bigint::BigInt::clone(b)))),
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
        // The Array holding the items.
        let arr = match self.heap.get(idx) {
            HeapObj::Array(_) if top => idx,
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
                items.heap_index()
            }
            _ => return self.py_ord_key(v, depth),
        };
        let n = match self.heap.get(arr) {
            HeapObj::Array(items) => items.len(),
            _ => return None,
        };
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            // Read where it lives, each time: building a key allocates no
            // heap object and runs no code (it may flatten a str, in that
            // str's own slot), so the Array is as it was.
            let item = match self.heap.get(arr) {
                HeapObj::Array(items) => *items.get(i)?,
                _ => return None,
            };
            out.push(self.py_ord_key_rec(item, depth + 1, tuple, false)?);
        }
        Some(OrdKey::Seq(out))
    }

    /// What `v` is as [`Vm::py_ord_key_rec`] reads one below the top level
    /// (a tuple is a record whose class is `tuple`, nested at most 8 deep),
    /// without its items; `None` for a value it gives no key.
    fn py_ord_ref(&mut self, v: Value, depth: u32, tuple: Value) -> Option<OrdRef> {
        if v.is_number() {
            return Some(OrdRef::Float(v.as_f64()));
        }
        if v == Value::TRUE {
            return Some(OrdRef::Int(1));
        }
        if v == Value::FALSE {
            return Some(OrdRef::Int(0));
        }
        if v == Value::NULL {
            return Some(OrdRef::None);
        }
        if let Some(n) = v.small_bigint_val() {
            return Some(OrdRef::Int(n as i128));
        }
        if !v.is_heap() {
            return None;
        }
        let idx = v.heap_index();
        match self.heap.get(idx) {
            HeapObj::Array(_) => return None,
            HeapObj::Object(m) if !m.is_ctor => {
                if depth > 8 {
                    return None;
                }
                let (cls, items) = self.py_seq_parts(v)?;
                if cls.bits() != tuple.bits() || !items.is_heap() {
                    return None;
                }
                let arr = items.heap_index();
                return matches!(self.heap.get(arr), HeapObj::Array(_)).then_some(OrdRef::Seq(arr));
            }
            HeapObj::BigInt(n) => return Some(OrdRef::Int(*n)),
            HeapObj::BigIntBig(_) => return Some(OrdRef::Big(idx)),
            _ => {}
        }
        if self.heap.is_str_like(idx) {
            self.heap.flatten(idx);
            return matches!(self.heap.get(idx), HeapObj::Str(_)).then_some(OrdRef::Str(idx));
        }
        None
    }

    /// Python's `a < b` on two values as op 2 orders them (ints, floats,
    /// strs, bools, None and tuple records of those), when they are
    /// ordered: what [`Vm::py_ord_cmp`] answers for their two keys, read
    /// from the values where they live. A tuple's items are compared in
    /// turn and only as far as the first pair that differs, as Python's
    /// own `<` compares them, so an item past that pair is never read (and
    /// one this cannot order there does not make the answer `None`).
    pub(super) fn py_ord_lt_values(&mut self, a: Value, b: Value, tuple: Value) -> Option<bool> {
        let lt = self.py_ord_lt_in_place(a, b, tuple);
        // Whenever both keys can be built, comparing them answers the same.
        #[cfg(debug_assertions)]
        if let (Some(ka), Some(kb)) = (self.py_ord_key_rec(a, 0, tuple, false), self.py_ord_key_rec(b, 0, tuple, false)) {
            let by_key = self.py_ord_cmp(&ka, &kb).map(|o| o == std::cmp::Ordering::Less);
            debug_assert!(by_key == lt, "in-place `<` {lt:?} differs from the keys' {by_key:?}");
        }
        lt
    }

    fn py_ord_lt_in_place(&mut self, a: Value, b: Value, tuple: Value) -> Option<bool> {
        let x = self.py_ord_ref(a, 0, tuple)?;
        let y = self.py_ord_ref(b, 0, tuple)?;
        self.py_ord_cmp_refs(x, y, tuple, 0).map(|o| o == std::cmp::Ordering::Less)
    }

    /// [`Vm::py_ord_cmp`], arm for arm, on two values read where they live
    /// (`depth`: how deep the two are nested in tuples).
    fn py_ord_cmp_refs(&mut self, x: OrdRef, y: OrdRef, tuple: Value, depth: u32) -> Option<std::cmp::Ordering> {
        use num_bigint::Sign::Minus;
        use std::cmp::Ordering;
        match (x, y) {
            (OrdRef::Int(a), OrdRef::Int(b)) => Some(a.cmp(&b)),
            // A slow-tier value is beyond i128: its sign decides.
            (OrdRef::Int(_), OrdRef::Big(b)) => match self.heap.get(b) {
                HeapObj::BigIntBig(q) => Some(if q.sign() == Minus { Ordering::Greater } else { Ordering::Less }),
                _ => None,
            },
            (OrdRef::Big(a), OrdRef::Int(_)) => match self.heap.get(a) {
                HeapObj::BigIntBig(p) => Some(if p.sign() == Minus { Ordering::Less } else { Ordering::Greater }),
                _ => None,
            },
            (OrdRef::Big(a), OrdRef::Big(b)) => match (self.heap.get(a), self.heap.get(b)) {
                (HeapObj::BigIntBig(p), HeapObj::BigIntBig(q)) => Some(p.cmp(q)),
                _ => None,
            },
            (OrdRef::Int(a), OrdRef::Float(f)) => crate::vm::coerce::cmp_i128_f64(a, f),
            (OrdRef::Float(f), OrdRef::Int(a)) => crate::vm::coerce::cmp_i128_f64(a, f).map(Ordering::reverse),
            (OrdRef::Big(a), OrdRef::Float(f)) => match self.heap.get(a) {
                HeapObj::BigIntBig(p) => super::bigint::cmp_big_f64(p, f),
                _ => None,
            },
            (OrdRef::Float(f), OrdRef::Big(a)) => match self.heap.get(a) {
                HeapObj::BigIntBig(p) => super::bigint::cmp_big_f64(p, f).map(Ordering::reverse),
                _ => None,
            },
            (OrdRef::Float(a), OrdRef::Float(b)) => a.partial_cmp(&b),
            (OrdRef::Str(a), OrdRef::Str(b)) => match (self.heap.get(a), self.heap.get(b)) {
                (HeapObj::Str(p), HeapObj::Str(q)) => Some(p.as_bytes().cmp(q.as_bytes())),
                _ => None,
            },
            (OrdRef::Seq(a), OrdRef::Seq(b)) => {
                let mut i = 0usize;
                loop {
                    // The two items at `i`, read afresh: comparing the pair
                    // before may have flattened a str (in its own slot), and
                    // nothing here moves or resizes an Array.
                    let (xv, yv) = match (self.heap.get(a), self.heap.get(b)) {
                        (HeapObj::Array(xs), HeapObj::Array(ys)) => match (xs.get(i), ys.get(i)) {
                            (Some(&xv), Some(&yv)) => (xv, yv),
                            // Equal as far as the shorter goes.
                            _ => return Some(xs.len().cmp(&ys.len())),
                        },
                        _ => return None,
                    };
                    let ex = self.py_ord_ref(xv, depth + 1, tuple)?;
                    let ey = self.py_ord_ref(yv, depth + 1, tuple)?;
                    // The first unequal pair decides, by `<`. None equals
                    // None (and orders against nothing: a pair of them at
                    // the top level is not ordered); a tuple never equals a
                    // number or a str, nor a str a number, and `<` between
                    // them is the general protocol's.
                    let o = match (ex, ey) {
                        (OrdRef::None, OrdRef::None) => Ordering::Equal,
                        _ => self.py_ord_cmp_refs(ex, ey, tuple, depth + 1)?,
                    };
                    if o != Ordering::Equal {
                        return Some(o);
                    }
                    i += 1;
                }
            }
            _ => None,
        }
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

    /// Repair `arr_mirror[items]` for a plain dense Array backing a Python
    /// list or tuple so compiled inline paths hit on subsequent iterations.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(super) fn py_repair_seq_arr_mirror(&mut self, items: u32) {
        if !self.heap.arr_mirror_live(items)
            && (self.arr_props.is_empty() || !self.arr_props.contains_key(&items))
            && (self.arguments_objs.is_empty() || !self.arguments_objs.contains_key(&items))
            && (self.array_js_len.is_empty() || !self.array_js_len.contains_key(&items))
        {
            self.heap.repair_arr_mirror(items);
        }
    }

    /// For [`Instr::PyGetItem`]: `o.items` and the resolved index when `o` is
    /// a list or tuple record of `seq_shape` and `k` is an in-bounds immediate
    /// int (positive or negative).
    #[inline(always)]
    fn py_seq_get_fast(&self, o: Value, k: Value, seq: Value) -> Option<(u32, usize)> {
        let n = k.small_bigint_val()?;
        if !o.is_heap() {
            return None;
        }
        let p = self.py_rt.as_deref()?;
        let shape = p.seq_shape;
        let HeapObj::Object(m) = self.heap.get(o.heap_index()) else {
            return None;
        };
        if m.is_ctor || shape == crate::shape::DICT || m.shape() != shape {
            return None;
        }
        let cls = m.val_at(0).bits();
        if cls != seq.bits() && cls != p.t_tuple.bits() {
            return None;
        }
        let items = m.val_at(1);
        if !items.is_heap() {
            return None;
        }
        let HeapObj::Array(vals) = self.heap.get(items.heap_index()) else {
            return None;
        };
        let len = vals.len();
        let i = if n >= 0 {
            usize::try_from(n).ok().filter(|&i| i < len)?
        } else {
            let adj = (len as i64).checked_add(n)?;
            usize::try_from(adj).ok().filter(|&i| i < len)?
        };
        Some((items.heap_index(), i))
    }

    /// For [`Instr::PySetItem`]: `o.items` and the index when `o` is a record
    /// of the sequence template's shape whose `cls` is `seq` (mutable list
    /// only, never tuple) and `k` an immediate int within its items.
    #[inline(always)]
    fn py_seq_index_fast(&self, o: Value, k: Value, seq: Value) -> Option<(u32, usize)> {
        let n = k.small_bigint_val()?;
        if !o.is_heap() {
            return None;
        }
        let shape = self.py_rt.as_deref()?.seq_shape;
        let HeapObj::Object(m) = self.heap.get(o.heap_index()) else {
            return None;
        };
        if m.is_ctor || shape == crate::shape::DICT || m.shape() != shape || m.val_at(0).bits() != seq.bits() {
            return None;
        }
        let items = m.val_at(1);
        if !items.is_heap() {
            return None;
        }
        let HeapObj::Array(vals) = self.heap.get(items.heap_index()) else {
            return None;
        };
        let len = vals.len();
        let i = if n >= 0 {
            usize::try_from(n).ok().filter(|&i| i < len)?
        } else {
            let adj = (len as i64).checked_add(n)?;
            usize::try_from(adj).ok().filter(|&i| i < len)?
        };
        Some((items.heap_index(), i))
    }

    /// For [`Instr::PyGetItem`] / [`Instr::PySetItem`]: a `dict` record of
    /// the dict template's shape, its `map` and the slot of its `size`, read
    /// in one pass (`py_item_kind` and `py_dict_field` read the record once
    /// each). `None` for anything else, which those then decide.
    #[inline(always)]
    fn py_dict_rec_fast(&self, o: Value, dict: Value) -> Option<(u32, Value, usize)> {
        if !o.is_heap() {
            return None;
        }
        let p = self.py_rt.as_deref()?;
        let (shape, slots) = (p.dict_shape, p.dict_slots);
        let HeapObj::Object(m) = self.heap.get(o.heap_index()) else {
            return None;
        };
        if m.is_ctor || shape == crate::shape::DICT || m.shape() != shape || m.val_at(0).bits() != dict.bits() {
            return None;
        }
        Some((o.heap_index(), m.val_at(slots[0]), slots[1]))
    }

    /// `o.items` and a valid index (including negative indices) into it for `k`.
    fn py_seq_slot(&self, o: Value, k: Value) -> Option<(u32, usize)> {
        let (_, items) = self.py_seq_parts(o)?;
        if !items.is_heap() {
            return None;
        }
        let n = self.bigint_i128(k)?;
        let HeapObj::Array(vals) = self.heap.get(items.heap_index()) else {
            return None;
        };
        let len = vals.len();
        let i = if n >= 0 {
            usize::try_from(n).ok().filter(|&i| i < len)?
        } else {
            let adj = (len as i128).checked_add(n)?;
            usize::try_from(adj).ok().filter(|&i| i < len)?
        };
        Some((items.heap_index(), i))
    }

    /// Non-allocating single-character ASCII string subscript `s[i]` (returns
    /// pre-interned slot `0..128` without touching the heap allocator, keeping
    /// `jit_py_get_item` strictly non-allocating).
    #[inline]
    fn py_str_ascii_char_noalloc(&mut self, s: Value, k: Value) -> Option<Value> {
        let i = self.bigint_i128(k)?;
        let idx = s.heap_index();
        self.heap.flatten(idx);
        let HeapObj::Str(st) = self.heap.get(idx) else {
            return None;
        };
        if !st.is_ascii() {
            return None;
        }
        let b = st.as_bytes();
        let n = b.len() as i128;
        let at = if i < 0 { i + n } else { i };
        if at < 0 || at >= n {
            return None;
        }
        Some(Value::heap(b[at as usize] as u32))
    }

    /// [`Instr::PyGetItem`]: `Some(Some(value))`, `Some(None)` for a key
    /// the dict's table finds missing, `None` for the slow edge.
    /// Guaranteed never to allocate on the VM heap (`jit_py_get_item` skips `refetch`).
    pub(super) fn py_get_item(&mut self, o: Value, k: Value, seq: Value, dict: Value) -> Result<Option<Option<Value>>, Thrown> {
        // The commonest case first, in one pass: a list or tuple record of
        // the sequence template's shape and a fast-tier int within its items.
        if let Some((items, i)) = self.py_seq_get_fast(o, k, seq) {
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            self.py_repair_seq_arr_mirror(items);
            let HeapObj::Array(vals) = self.heap.get(items) else {
                return Ok(None);
            };
            let v = vals[i];
            return Ok((v != Value::HOLE && !v.is_undefined()).then_some(Some(v)));
        }
        // A dict record of the template's shape: its `map`, read with it.
        if let Some((_, map, _)) = self.py_dict_rec_fast(o, dict) {
            return Ok(self.py_table_get(map, k));
        }
        // Non-allocating single-char ASCII primitive `str[i]` (slices and
        // non-ASCII strings fall through to `PyStrItem`).
        if o.is_heap() && self.heap.is_str_like(o.heap_index()) {
            return Ok(self.py_str_ascii_char_noalloc(o, k).map(Some));
        }
        if let Some((idx, is_seq)) = self.py_item_kind(o, seq, dict) {
            if is_seq {
                let Some((items, i)) = self.py_seq_slot(o, k) else {
                    return Ok(None);
                };
                let HeapObj::Array(vals) = self.heap.get(items) else {
                    return Ok(None);
                };
                let v = vals[i];
                return Ok((v != Value::HOLE && !v.is_undefined()).then_some(Some(v)));
            }
            let Some((_, map)) = self.py_dict_field(idx, 0) else {
                return Ok(None);
            };
            return Ok(self.py_table_get(map, k));
        }
        // Exact non-`seq_shape` tuple, or unmodified `dict`/`str`/`list`/`tuple`
        // subclasses (`AttributeDict`, `Counter`, `defaultdict`, `NavigableString`,
        // `AttributeValueList`, `NamedTuple`).
        if let Some(cls) = self.py_cls_of(o) {
            let sk = self.py_sub_kind(cls);
            if (sk & (super::py_rt::SUB_LIST_GET | super::py_rt::SUB_TUPLE_GET)) != 0 {
                let Some((items, i)) = self.py_seq_slot(o, k) else {
                    return Ok(None);
                };
                let HeapObj::Array(vals) = self.heap.get(items) else {
                    return Ok(None);
                };
                let v = vals[i];
                return Ok((v != Value::HOLE && !v.is_undefined()).then_some(Some(v)));
            }
            if (sk & (super::py_rt::SUB_DICT_GET | super::py_rt::SUB_DICT_GET_HIT)) != 0 {
                let Some((_, map)) = self.py_dict_field(o.heap_index(), 0) else {
                    return Ok(None);
                };
                let res = self.py_table_get(map, k);
                if matches!(res, Some(None)) && (sk & super::py_rt::SUB_DICT_GET) == 0 {
                    return Ok(None);
                }
                return Ok(res);
            }
            if (sk & super::py_rt::SUB_STR_GET) != 0 {
                if let Some(pyval) = self.py_own_data(o.heap_index(), "pyval") {
                    if pyval.is_heap() && self.heap.is_str_like(pyval.heap_index()) {
                        return Ok(self.py_str_ascii_char_noalloc(pyval, k).map(Some));
                    }
                }
            }
        }
        // Non-allocating lookup for memoized typing subscripts (`typing._Alias[T]`,
        // `ResultSet[Tag]`, `list[int]`, etc.) populated in JS as `o._subCache`.
        if o.is_heap() {
            if let Some(cache) = self.py_own_data(o.heap_index(), "_subCache") {
                if cache.is_heap() {
                    if let Some(v) = self.py_map_get(cache, k) {
                        return Ok(Some(Some(v)));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Fast non-allocating `d[k] = v` for exact `dict` records of `dict_shape`:
    /// never allocates on the VM heap and never invokes guest code, so compiled
    /// code (`jit_py_set_item_dict`) skips `maybe_gc()` and `refetch(ops)`.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(super) fn py_set_item_dict_fast(&mut self, o: Value, k: Value, v: Value, dict: Value) -> bool {
        let Some((idx, map, size_slot)) = self.py_dict_rec_fast(o, dict) else {
            return false;
        };
        let Some(Ok((n, inserted))) = self.py_table_set_ex(map, k, v) else {
            return false;
        };
        if inserted {
            if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                m.set_val_at(size_slot, Value::num(n as f64));
            }
        }
        true
    }

    pub(super) fn py_set_item(&mut self, o: Value, k: Value, v: Value, seq: Value, dict: Value) -> Result<bool, Thrown> {
        // As `py_get_item`: the commonest cases' records first (a dict
        // record's `map` and `size` slot read with it).
        let fast = self.py_seq_index_fast(o, k, seq);
        let dict_fast = if fast.is_none() { self.py_dict_rec_fast(o, dict) } else { None };
        let (idx, is_seq) = match (fast, dict_fast) {
            (Some((items, _)), _) => (items, true),
            (None, Some((rec, _, _))) => (rec, false),
            (None, None) => match self.py_item_kind(o, seq, dict) {
                Some(kind) => kind,
                None => {
                    let Some(cls) = self.py_cls_of(o) else {
                        return Ok(false);
                    };
                    let sk = self.py_sub_kind(cls);
                    if (sk & super::py_rt::SUB_LIST_SET) != 0 {
                        (o.heap_index(), true)
                    } else if (sk & super::py_rt::SUB_DICT_SET) != 0 {
                        (o.heap_index(), false)
                    } else {
                        return Ok(false);
                    }
                }
            },
        };
        if is_seq {
            let Some((items, i)) = fast.or_else(|| self.py_seq_slot(o, k)) else {
                return Ok(false);
            };
            // An element present in a plain Array (no side table: nothing
            // frozen, overridden or mapped to an `arguments` object): stored
            // in place without clearing `arr_mirror`.
            let plain = (self.arr_props.is_empty() || !self.arr_props.contains_key(&items))
                && (self.arguments_objs.is_empty() || !self.arguments_objs.contains_key(&items))
                && matches!(self.heap.get(items), HeapObj::Array(vals) if vals.get(i).is_some_and(|x| !x.is_hole()));
            if plain {
                if v.is_heap() {
                    self.store_barrier_v(crate::heap::gcoracle::SET_INDEX, Value::heap(items), v);
                }
                if !self.heap.array_overwrite_present(items, i, v) {
                    if let HeapObj::Array(vals) = self.heap.get_mut(items) {
                        vals[i] = v;
                    }
                }
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                self.py_repair_seq_arr_mirror(items);
                return Ok(true);
            }
            self.set_index(Value::heap(items), Value::int(i as i32), v, true)?;
            return Ok(true);
        }
        let (map, size_slot) = match dict_fast {
            Some((_, map, size_slot)) => (map, size_slot),
            None => match (self.py_dict_field(idx, 0), self.py_dict_field(idx, 1)) {
                (Some((_, map)), Some((size_slot, _))) => (map, size_slot),
                _ => return Ok(false),
            },
        };
        // A key needing guest code, or a full table: the runtime's path.
        let Some(Ok((n, inserted))) = self.py_table_set_ex(map, k, v) else {
            return Ok(false);
        };
        if inserted {
            if let HeapObj::Object(m) = self.heap.get_mut(idx) {
                m.set_val_at(size_slot, Value::num(n as f64));
            }
        }
        Ok(true)
    }

    #[inline]
    fn py_num(&self, v: Value) -> Num {
        if v.is_number() {
            return Num::Float(v.as_f64());
        }
        if let Some(n) = v.small_bigint_val() {
            return Num::Int(n as i128);
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
    #[inline(always)] // one copy in the step, one in its JIT helper (`lang/python/jit/py_jit.rs`)
    pub(crate) fn py_arith(&mut self, op: PyArithOp, va: Value, vb: Value) -> Result<Option<Value>, Thrown> {
        // `str + str`: native concatenation for well-formed primitive strs.
        if op == PyArithOp::Add
            && va.is_heap()
            && vb.is_heap()
            && self.heap.is_str_like(va.heap_index())
            && self.heap.is_str_like(vb.heap_index())
        {
            return Ok(self.py_str_concat(va, vb));
        }
        // `str * int/bool` and `int/bool * str`: native string repetition.
        if op == PyArithOp::Mul {
            if va.is_heap() && self.heap.is_str_like(va.heap_index()) {
                return Ok(self.py_str_repeat(va, vb));
            }
            if vb.is_heap() && self.heap.is_str_like(vb.heap_index()) {
                return Ok(self.py_str_repeat(vb, va));
            }
        }
        // `str % values`: the runtime's `R.mod` asks `__zipp_py_pct` first
        // (`py_pct_format`, which answers only what the runtime's
        // `percentFormat` answers, and declines anything else); asked here,
        // its call is not made.
        if op == PyArithOp::Mod && va.is_heap() && self.heap.is_str_like(va.heap_index()) {
            let tuple = match self.py_rt.as_deref() {
                Some(p) => p.t_tuple,
                None => return Ok(None),
            };
            return Ok(self.py_pct_format(&[va, vb, tuple]));
        }
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
        // Two small (immediate) ints: i64 arithmetic (value.rs).
        if let (Some(x), Some(y)) = (va.small_bigint_val(), vb.small_bigint_val()) {
            if let Some(r) = py_small_arith(op, x, y) {
                return Ok(Some(self.make_bigint(r as i128)));
            }
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
    #[inline(always)] // one copy in the step, one in its JIT helper (`lang/python/jit/py_jit.rs`)
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
    #[inline(always)] // one copy in the step, one in its JIT helper (`lang/python/jit/py_jit.rs`)
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
                let va_str = va.is_heap() && self.heap.is_str_like(va.heap_index());
                let vb_str = vb.is_heap() && self.heap.is_str_like(vb.heap_index());
                if va_str && vb_str {
                    let eq = self.values_strict_eq(va, vb);
                    Ok(Some(if op == PyCmpOp::Eq { eq } else { !eq }))
                } else if va.is_null() && vb.is_null() {
                    Ok(Some(op == PyCmpOp::Eq))
                } else if va.is_bool() && vb.is_bool() {
                    let eq = va.bits() == vb.bits();
                    Ok(Some(if op == PyCmpOp::Eq { eq } else { !eq }))
                } else if (va_str || va.is_null() || va.is_bool())
                    && (vb_str || vb.is_null() || vb.is_bool())
                {
                    // Distinct primitive types (`str` vs `bool`/`None`, `bool` vs `None`): never equal.
                    Ok(Some(op == PyCmpOp::Ne))
                } else if let Some(eq) = self.py_deep_eq(va, vb) {
                    // Two exact lists or dicts (`vm::py_table`).
                    Ok(Some(if op == PyCmpOp::Eq { eq } else { !eq }))
                } else {
                    Ok(None)
                }
            }
            (Num::Int(x), Num::Other) | (Num::Other, Num::Int(x)) if matches!(op, PyCmpOp::Eq | PyCmpOp::Ne) => {
                let other = if matches!(self.py_num(va), Num::Other) { va } else { vb };
                if other.is_bool() {
                    let eq = x == if other.as_bool() { 1 } else { 0 };
                    Ok(Some(if op == PyCmpOp::Eq { eq } else { !eq }))
                } else if other.is_null() || (other.is_heap() && self.heap.is_str_like(other.heap_index())) {
                    Ok(Some(op == PyCmpOp::Ne))
                } else {
                    Ok(None)
                }
            }
            (Num::Float(x), Num::Other) | (Num::Other, Num::Float(x)) if matches!(op, PyCmpOp::Eq | PyCmpOp::Ne) => {
                let other = if matches!(self.py_num(va), Num::Other) { va } else { vb };
                if other.is_bool() {
                    let eq = x == if other.as_bool() { 1.0 } else { 0.0 };
                    Ok(Some(if op == PyCmpOp::Eq { eq } else { !eq }))
                } else if other.is_null() || (other.is_heap() && self.heap.is_str_like(other.heap_index())) {
                    Ok(Some(op == PyCmpOp::Ne))
                } else {
                    Ok(None)
                }
            }
            (Num::BigInt, Num::Other) | (Num::Other, Num::BigInt) if matches!(op, PyCmpOp::Eq | PyCmpOp::Ne) => {
                let other = if matches!(self.py_num(va), Num::Other) { va } else { vb };
                if other.is_bool() || other.is_null() || (other.is_heap() && self.heap.is_str_like(other.heap_index())) {
                    Ok(Some(op == PyCmpOp::Ne))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }
}

/// `x <op> y` for two small (immediate, 47-bit) ints as `py_arith` computes
/// it for ints, in i64 (none of these can overflow i64 except a product,
/// which is checked); `None` for true division, a zero divisor and an
/// overflowing product, which take `py_arith`'s general path.
#[inline(always)]
pub(super) fn py_small_arith(op: PyArithOp, x: i64, y: i64) -> Option<i64> {
    Some(match op {
        PyArithOp::Add => x + y,
        PyArithOp::Sub => x - y,
        PyArithOp::Mul => x.checked_mul(y)?,
        PyArithOp::BitAnd => x & y,
        PyArithOp::BitOr => x | y,
        PyArithOp::BitXor => x ^ y,
        PyArithOp::FloorDiv | PyArithOp::Mod if y != 0 => {
            let (q, r) = (x / y, x % y);
            let adjust = r != 0 && ((r < 0) != (y < 0));
            match op {
                PyArithOp::FloorDiv => q - adjust as i64,
                _ => r + if adjust { y } else { 0 },
            }
        }
        _ => return None,
    })
}

// ---- the interpreter's inline forms -------------------------------------
//
// The dispatch loop settles the three commonest operand shapes of the
// arithmetic and comparison instructions itself, without the call into
// `Vm::py_step`: two small (immediate) ints whose result is one too. Each
// answers exactly what the step's own helper answers for those operands
// (`py_arith`, `py_add_imm`, `py_compare`), and `None` for everything else,
// which then takes the step.

/// [`Vm::py_arith`] for two small ints with a small result.
#[inline(always)]
pub(super) fn py_small_int_arith(op: PyArithOp, va: Value, vb: Value) -> Option<Value> {
    let (x, y) = (va.small_bigint_val()?, vb.small_bigint_val()?);
    Value::small_bigint(py_small_arith(op, x, y)? as i128)
}

/// [`Vm::py_add_imm`] for a small int with a small result.
#[inline(always)]
pub(super) fn py_small_int_add_imm(va: Value, imm: i32) -> Option<Value> {
    Value::small_bigint(va.small_bigint_val()? as i128 + imm as i128)
}

/// [`Vm::py_compare`] for two small ints.
#[inline(always)]
pub(super) fn py_small_int_compare(op: PyCmpOp, va: Value, vb: Value) -> Option<bool> {
    let (x, y) = (va.small_bigint_val()?, vb.small_bigint_val()?);
    Some(order_holds(op, x.cmp(&y)))
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

/// [`Vm::py_dict_get`]'s answer: the value for `dst`, the fall-through with
/// `dst` untouched (an absent key the instruction expects absent), or the
/// slow edge.
pub(super) enum PyDictGot {
    Value(Value),
    Absent,
    Slow,
}

/// [`Vm::py_call_target`]'s answer.
enum PyCallee {
    /// A plain Python code object the site's entry records, plus its classified `ic::CALL_*` kind.
    Lean(PyLean, u8),
    /// A plain function: its id, closure and value.
    Plain(u32, u32, Value),
    /// An exception class's positional entry (`R.EXCENTRY`), as `Plain`.
    Exc(u32, u32, Value),
    /// A sync Python generator function (`R.GENENTRY` + `R.MAKEGEN`):
    /// `(real_fid, real_closure, real_entry, fallback_gen_entry)`.
    Gen(u32, u32, Value, Value),
    /// Any other callable entry.
    Other(Value),
    /// No entry to call: the slow edge.
    Slow,
}

/// A `PyCall` site's plain Python callee, from its cache entry (see
/// [`Vm::py_call_target`]): the code object's id, its closure, register
/// count and parameter count, and the entry value.
pub(super) struct PyLean {
    pub(super) fid: u32,
    pub(super) closure: u32,
    pub(super) regs: u16,
    pub(super) params: u16,
    pub(super) entry: Value,
}
