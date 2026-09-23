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
        if op == Value::int(0) {
            let b = args.get(2).copied().unwrap_or(Value::UNDEFINED);
            let (Some(x), Some(y)) = (self.py_ord_key(a, 0), self.py_ord_key(b, 0)) else {
                return Ok(Value::UNDEFINED);
            };
            return Ok(match self.py_ord_cmp(&x, &y) {
                Some(o) => Value::int(o as i32),
                None => Value::UNDEFINED,
            });
        }
        if op != Value::int(1) || !a.is_heap() {
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
            match self.py_ord_key(v, 0) {
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
