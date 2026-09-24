//! The Python runtime's str fast paths, native: `str % values`
//! (`__zipp_py_pct`) and some str methods (`__zipp_py_strm`, below).
//!
//! `rt.percentFormat` (`runtime/stdlib.js`) formats `str % values`; this
//! native produces the same text for the format strings programs use most,
//! and answers `undefined` for every other one, whereupon the runtime's own
//! code runs (and raises what it raises):
//!
//! `__zipp_py_pct(format, values, T.tuple)`: `values` is an exact tuple (its
//! items are the arguments) or a single argument that is not a runtime
//! record; every `%` in `format` starts `%%` or `%[-0]*[width](d|i|s)`; a
//! `d`/`i` argument is an int or a bool, an `s` argument a str, an int, a
//! bool, `None` or a float; every argument is used exactly once. The
//! conversions are the runtime's: `d` the decimal digits after the sign
//! (`-`), zero-padded after the sign by the `0` flag unless `-` is given,
//! left-aligned by `-`, else right-aligned; `s` the argument's str()
//! (`floatRepr` for a float), space-padded, left-aligned by `-`. Widths count
//! code points. Any str involved must be well-formed (a lone surrogate could
//! join its neighbour's when the runtime concatenates).

use super::*;
use crate::heap::{HeapObj, JsStr};
use crate::value::Value;
use super::py_ops::OrdKeyBox;

/// Largest result made here; longer ones take the runtime's path.
const MAX_OUT: usize = 1 << 24;
/// Instruction steps charged per byte of output.
const STEPS_PER_BYTE: u64 = 1;

impl<'p> Vm<'p> {
    /// A well-formed str's bytes (UTF-8), owned; `None` for anything else.
    fn py_pct_str(&mut self, v: Value) -> Option<Vec<u8>> {
        if !v.is_heap() || !self.heap.is_str_like(v.heap_index()) {
            return None;
        }
        let idx = v.heap_index();
        self.heap.flatten(idx);
        let HeapObj::Str(s) = self.heap.get(idx) else {
            return None;
        };
        let b = s.as_bytes();
        (s.is_ascii() || crate::heap::wtf8_is_wellformed(b)).then(|| b.to_vec())
    }

    /// An int argument (a bool as 0 or 1): its sign and decimal digits.
    fn py_pct_int(&self, v: Value) -> Option<(bool, String)> {
        if v == Value::TRUE {
            return Some((false, "1".into()));
        }
        if v == Value::FALSE {
            return Some((false, "0".into()));
        }
        if !v.is_heap() {
            return None;
        }
        match self.heap.get(v.heap_index()) {
            HeapObj::BigInt(n) => Some((*n < 0, n.unsigned_abs().to_string())),
            HeapObj::BigIntBig(b) => Some((b.sign() == num_bigint::Sign::Minus, b.magnitude().to_string())),
            _ => None,
        }
    }

    /// `__zipp_py_pct(format, values, T.tuple)`: see the module comment.
    pub(crate) fn py_pct(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        Ok(self.py_pct_format(args).unwrap_or(Value::UNDEFINED))
    }

    fn py_pct_format(&mut self, args: &[Value]) -> Option<Value> {
        let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        let fmt = self.py_pct_str(arg(0))?;
        let (values, tuple) = (arg(1), arg(2));
        let items: Vec<Value> = if values.is_heap() && matches!(self.heap.get(values.heap_index()), HeapObj::Object(_)) {
            // An exact tuple's items; any other record (a dict is a mapping,
            // another object's str() may run code) takes the runtime's path.
            let (cls, items) = self.py_seq_parts(values)?;
            if cls.bits() != tuple.bits() {
                return None;
            }
            if !items.is_heap() {
                return None;
            }
            match self.heap.get(items.heap_index()) {
                HeapObj::Array(v) => v.clone(),
                _ => return None,
            }
        } else {
            vec![values]
        };
        let mut out: Vec<u8> = Vec::with_capacity(fmt.len() + 16);
        let mut next = 0usize;
        let mut i = 0usize;
        while i < fmt.len() {
            let c = fmt[i];
            if c != b'%' {
                out.push(c);
                i += 1;
                continue;
            }
            i += 1;
            if fmt.get(i) == Some(&b'%') {
                out.push(b'%');
                i += 1;
                continue;
            }
            let (mut left, mut zero) = (false, false);
            while let Some(&f) = fmt.get(i) {
                match f {
                    b'-' => left = true,
                    b'0' => zero = true,
                    _ => break,
                }
                i += 1;
            }
            let mut width = 0usize;
            while let Some(&d) = fmt.get(i).filter(|d| d.is_ascii_digit()) {
                width = width.checked_mul(10)?.checked_add((d - b'0') as usize)?;
                i += 1;
            }
            if width > MAX_OUT {
                return None;
            }
            let conv = *fmt.get(i)?;
            i += 1;
            let v = *items.get(next)?;
            next += 1;
            match conv {
                b'd' | b'i' => {
                    let (neg, digits) = self.py_pct_int(v)?;
                    let sign: &[u8] = if neg { b"-" } else { b"" };
                    let n = sign.len() + digits.len();
                    let pad = width.saturating_sub(n);
                    if zero && !left {
                        out.extend_from_slice(sign);
                        out.resize(out.len() + pad, b'0');
                        out.extend_from_slice(digits.as_bytes());
                    } else if left {
                        out.extend_from_slice(sign);
                        out.extend_from_slice(digits.as_bytes());
                        out.resize(out.len() + pad, b' ');
                    } else {
                        out.resize(out.len() + pad, b' ');
                        out.extend_from_slice(sign);
                        out.extend_from_slice(digits.as_bytes());
                    }
                }
                b's' => {
                    let body: Vec<u8> = if v == Value::NULL {
                        b"None".to_vec()
                    } else if v == Value::TRUE {
                        b"True".to_vec()
                    } else if v == Value::FALSE {
                        b"False".to_vec()
                    } else if v.is_number() {
                        let x = v.as_f64();
                        let mut t = String::new();
                        if x.is_nan() {
                            t.push_str("nan");
                        } else if x.is_infinite() {
                            t.push_str(if x > 0.0 { "inf" } else { "-inf" });
                        } else {
                            super::py_json::py_float_repr(&mut t, x);
                        }
                        t.into_bytes()
                    } else if let Some((neg, digits)) = self.py_pct_int(v) {
                        let mut t = Vec::with_capacity(digits.len() + 1);
                        if neg {
                            t.push(b'-');
                        }
                        t.extend_from_slice(digits.as_bytes());
                        t
                    } else {
                        self.py_pct_str(v)?
                    };
                    let n = body.iter().filter(|&&b| b & 0xC0 != 0x80).count();
                    let pad = width.saturating_sub(n);
                    if left {
                        out.extend_from_slice(&body);
                        out.resize(out.len() + pad, b' ');
                    } else {
                        out.resize(out.len() + pad, b' ');
                        out.extend_from_slice(&body);
                    }
                }
                _ => return None,
            }
            if out.len() > MAX_OUT {
                return None;
            }
        }
        if next != items.len() {
            return None;
        }
        let cost = (out.len() as u64).saturating_mul(STEPS_PER_BYTE);
        if !self.native_kernel_admits(cost, out.len()) {
            return None;
        }
        self.charge_steps(i64::try_from(cost).unwrap_or(i64::MAX));
        Some(Value::heap(self.heap.alloc_js(JsStr::from_wtf8(out))))
    }
}

// ---- str methods (`__zipp_py_strm`) ----------------------------------------
//
// The runtime installs this native as the positional entry (`c<n>`) of some
// str methods, keeping the entry it replaces as `j<n>` on the same builtin
// record. Called as `builtin.c<n>(s, ...)`, it reads which method it is from
// the record's `strop` and answers exactly what `j<n>` answers for the plain
// cases below; for everything else it calls `j<n>` itself with the same
// arguments. Plain means: primitive strs with no lone surrogate (so byte
// positions of the UTF-8 form are code point positions), and for the
// case-mapping methods ASCII.
//
// * 1 / 2 / 3 `strip` / `lstrip` / `rstrip(s, chars)`: the code points of
//   `chars` cut from both / the left / the right end.
// * 4 `split(s)`: the runs between JavaScript whitespace, ASCII `s` only;
//   `split(s, sep)`, `sep` not empty: the pieces between the occurrences of
//   `sep`; a list made from the record's `ltmpl` (an empty list).
// * 5 `replace(s, old, new)`, `old` not empty: every occurrence replaced.
// * 6 `find(s, sub)`: the code-point index of the first occurrence, or -1.
// * 7 `join(sep, items)`: `items` an exact list or tuple (the record's
//   `ltmpl.cls` / `ttype`) of primitive strs, joined.
// * 8 / 9 `lower` / `upper(s)`: ASCII case mapping of an ASCII `s`.
// * 10 `hash(s)` of a str (any str): the runtime's `strHash`.
// * 11 `math.sqrt(x)` of a float not below zero: the IEEE square root.
// * 12 / 13 `heapq.heappush` / `heappop` (see below).
// * 14 / 15 `bisect.bisect_left` / `bisect_right(a, x)`, 16 / 17
//   `insort_left` / `insort_right(a, x)` (see below).
//
// Results longer than the runtime's text limit (`MAX_TEXT` UTF-16 units)
// take the `j<n>` path, which raises as it always did.

use super::py_rt::hint;

/// `rt.MAX_TEXT` (UTF-16 units).
const STRM_MAX_TEXT: usize = 1 << 26;

/// The runtime's str hash (`strHashOf` in `runtime/types.js`) over the
/// string's UTF-16 code units: two 32-bit shift-add lanes, mixed, combined
/// into a non-negative 53-bit value.
fn js_str_hash(units: impl Iterator<Item = u16>) -> u64 {
    let mut h1: i32 = 5381;
    let mut h2: i32 = 0x6a09e667;
    for c in units {
        let c = c as i32;
        h1 = h1.wrapping_shl(5).wrapping_add(h1).wrapping_add(c);
        // `((h2 << 7) - h2) ^ c`: the difference is exact in a double, and
        // `^` takes it modulo 2^32.
        let d = (h2.wrapping_shl(7) as i64) - (h2 as i64);
        h2 = (d as i32) ^ c;
    }
    h1 = (h1 ^ ((h1 as u32) >> 15) as i32).wrapping_mul(0x2c1b3c6d);
    h2 = (h2 ^ ((h2 as u32) >> 13) as i32).wrapping_mul(0x297a2d39);
    let hi = ((h2 as u32) >> 11) as u64;
    let lo = ((h1 ^ ((h1 as u32) >> 16) as i32) as u32) as u64;
    hi * 4_294_967_296 + lo
}

/// JavaScript's `\s` among the ASCII characters.
fn js_space(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | b' ')
}

impl<'p> Vm<'p> {
    /// An own data property of the plain object `idx`, at the slot `hint`
    /// remembers when that still holds `key`.
    fn py_strm_prop(&self, idx: u32, h: usize, key: &str) -> Option<Value> {
        self.py_hint_field(h, idx, key)
    }

    /// A primitive str with no lone surrogate: its bytes and whether it is
    /// ASCII.
    fn py_strm_str(&mut self, v: Value) -> Option<(Vec<u8>, bool)> {
        if !v.is_heap() || !self.heap.is_str_like(v.heap_index()) {
            return None;
        }
        let idx = v.heap_index();
        self.heap.flatten(idx);
        let HeapObj::Str(s) = self.heap.get(idx) else {
            return None;
        };
        let ascii = s.is_ascii();
        (ascii || s.is_wellformed()).then(|| (s.as_bytes().to_vec(), ascii))
    }

    fn py_strm_new(&mut self, bytes: Vec<u8>) -> Value {
        Value::heap(self.heap.alloc_js(JsStr::from_wtf8(bytes)))
    }

    /// `__zipp_py_strm`, called as `builtin.c<n>(...args)`.
    pub(crate) fn py_str_method(&mut self, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        if let Some(v) = self.py_strm_fast(this, args) {
            return Ok(v);
        }
        // The entry this one stands in for, with the same arguments.
        let name = match args.len() {
            1 => "j1",
            2 => "j2",
            3 => "j3",
            _ => return Err(Thrown("TypeError: str method entry called with a wrong count".into())),
        };
        let HeapObj::Object(m) = (if this.is_heap() { self.heap.get(this.heap_index()) } else { return Err(Thrown("TypeError: str method entry called on a non-builtin".into())) }) else {
            return Err(Thrown("TypeError: str method entry called on a non-builtin".into()));
        };
        let Some(slot) = m.pos(name).filter(|&s| !m.attr_at(s).accessor) else {
            return Err(Thrown("TypeError: str method entry called on a non-builtin".into()));
        };
        let f = m.val_at(slot);
        self.call_value(f, this, args)
    }

    fn py_strm_fast(&mut self, this: Value, args: &[Value]) -> Option<Value> {
        if !this.is_heap() {
            return None;
        }
        let op = self.py_strm_prop(this.heap_index(), hint::STROP, "strop")?;
        if !op.is_int() {
            return None;
        }
        let op = op.as_int();
        match (op, args.len()) {
            (12, 2) => return self.py_heap_op(this, args, true),
            (14..=17, 2) => return self.py_bisect_op(this, args, op),
            (13, 1) => return self.py_heap_op(this, args, false),
            // `hash(s)` of a str: `strHash` (`runtime/types.js`).
            (10, 1) => {
                let v = args[0];
                if !v.is_heap() || !self.heap.is_str_like(v.heap_index()) {
                    return None;
                }
                let idx = v.heap_index();
                self.heap.flatten(idx);
                let HeapObj::Str(st) = self.heap.get(idx) else {
                    return None;
                };
                let h = js_str_hash(st.units_iter());
                return Some(self.make_bigint(h as i128));
            }
            // `math.sqrt(x)` of a float that is not below zero (a NaN too).
            (11, 1) => {
                let v = args[0];
                if !v.is_number() {
                    return None;
                }
                let x = v.as_f64();
                if x < 0.0 {
                    return None;
                }
                return Some(Value::num(x.sqrt()));
            }
            _ => {}
        }
        let (s, ascii) = self.py_strm_str(*args.first()?)?;
        let out = match (op, args.len()) {
            (1..=3, 2) => {
                let (chars, _) = self.py_strm_str(args[1])?;
                let set = String::from_utf8(chars).ok()?;
                let text = std::str::from_utf8(&s).ok()?;
                let cut = |c: char| set.contains(c);
                let r = match op {
                    1 => text.trim_matches(cut),
                    2 => text.trim_start_matches(cut),
                    _ => text.trim_end_matches(cut),
                };
                if r.len() == text.len() {
                    return Some(args[0]);
                }
                self.py_strm_new(r.as_bytes().to_vec())
            }
            (4, 1) => {
                if !ascii {
                    return None;
                }
                let parts: Vec<&[u8]> = s.split(|&b| js_space(b)).filter(|p| !p.is_empty()).collect();
                let parts: Vec<Vec<u8>> = parts.into_iter().map(|p| p.to_vec()).collect();
                return self.py_strm_list(this, parts);
            }
            (4, 2) => {
                let (sep, _) = self.py_strm_str(args[1])?;
                if sep.is_empty() {
                    return None;
                }
                let mut parts = Vec::new();
                let mut start = 0;
                let mut i = 0;
                while i + sep.len() <= s.len() {
                    if s[i..i + sep.len()] == sep[..] {
                        parts.push(s[start..i].to_vec());
                        i += sep.len();
                        start = i;
                    } else {
                        i += 1;
                    }
                }
                parts.push(s[start..].to_vec());
                return self.py_strm_list(this, parts);
            }
            (5, 3) => {
                let (old, _) = self.py_strm_str(args[1])?;
                let (new, _) = self.py_strm_str(args[2])?;
                if old.is_empty() {
                    return None;
                }
                let mut out = Vec::with_capacity(s.len());
                let mut i = 0;
                let mut last = 0;
                let mut changed = false;
                while i + old.len() <= s.len() {
                    if s[i..i + old.len()] == old[..] {
                        out.extend_from_slice(&s[last..i]);
                        out.extend_from_slice(&new);
                        i += old.len();
                        last = i;
                        changed = true;
                    } else {
                        i += 1;
                    }
                }
                if !changed {
                    // Still `rt.checkedText`'s limit, as the copy was checked.
                    let units = match self.heap.get(args[0].heap_index()) {
                        HeapObj::Str(st) => st.units(),
                        _ => return None,
                    };
                    return (units <= STRM_MAX_TEXT).then_some(args[0]);
                }
                out.extend_from_slice(&s[last..]);
                self.py_strm_new(out)
            }
            (6, 2) => {
                let (sub, _) = self.py_strm_str(args[1])?;
                let found = if sub.is_empty() {
                    Some(0)
                } else if sub.len() > s.len() {
                    None
                } else {
                    (0..=s.len() - sub.len()).find(|&i| s[i..i + sub.len()] == sub[..])
                };
                return Some(match found {
                    None => self.make_bigint(-1),
                    Some(i) => {
                        let cps = s[..i].iter().filter(|&&b| b & 0xC0 != 0x80).count();
                        self.make_bigint(cps as i128)
                    }
                });
            }
            (7, 2) => {
                let items = args[1];
                if !items.is_heap() {
                    return None;
                }
                let tmpl = self.py_strm_prop(this.heap_index(), hint::LTMPL, "ltmpl")?;
                let ttype = self.py_strm_prop(this.heap_index(), hint::TTYPE, "ttype")?;
                if !tmpl.is_heap() {
                    return None;
                }
                let list_cls = self.py_json_field(tmpl.heap_index(), "cls")?;
                let cls = self.py_json_field(items.heap_index(), "cls")?;
                if cls.bits() != list_cls.bits() && cls.bits() != ttype.bits() {
                    return None;
                }
                let arr = self.py_json_field(items.heap_index(), "items")?;
                if !arr.is_heap() {
                    return None;
                }
                let HeapObj::Array(parts) = self.heap.get(arr.heap_index()) else {
                    return None;
                };
                let parts = parts.clone();
                let mut out = Vec::new();
                for (i, p) in parts.into_iter().enumerate() {
                    let (b, _) = self.py_strm_str(p)?;
                    if i > 0 {
                        out.extend_from_slice(&s);
                    }
                    out.extend_from_slice(&b);
                    if out.len() > STRM_MAX_TEXT * 3 {
                        return None;
                    }
                }
                self.py_strm_new(out)
            }
            (8, 1) | (9, 1) => {
                if !ascii {
                    return None;
                }
                let lower = op == 8;
                let changes = s.iter().any(|b| if lower { b.is_ascii_uppercase() } else { b.is_ascii_lowercase() });
                if !changes {
                    return Some(args[0]);
                }
                let out: Vec<u8> = s.iter().map(|b| if lower { b.to_ascii_lowercase() } else { b.to_ascii_uppercase() }).collect();
                self.py_strm_new(out)
            }
            _ => return None,
        };
        // `rt.checkedText`: the runtime's text limit, in UTF-16 units.
        if let HeapObj::Str(st) = self.heap.get(out.heap_index()) {
            if st.units() > STRM_MAX_TEXT {
                return None;
            }
        }
        let n = s.len() as u64;
        self.charge_steps(i64::try_from(n).unwrap_or(i64::MAX));
        Some(out)
    }

    /// A list of these strs, made from the record's `ltmpl` (an empty list).
    fn py_strm_list(&mut self, this: Value, parts: Vec<Vec<u8>>) -> Option<Value> {
        if parts.len() > (1 << 24) {
            return None;
        }
        let tmpl = self.py_strm_prop(this.heap_index(), hint::LTMPL, "ltmpl")?;
        let (rec, slot) = self.py_json_template(tmpl, &["cls", "items"])?;
        let total: u64 = parts.iter().map(|p| p.len() as u64).sum();
        let items: Vec<Value> = parts.into_iter().map(|p| self.py_strm_new(p)).collect();
        let arr = Value::heap(self.heap.alloc(HeapObj::Array(items)));
        let mut rec = rec;
        rec.set_val_at(slot, arr);
        self.charge_steps(i64::try_from(total).unwrap_or(i64::MAX));
        Some(self.alloc_object_current_realm(rec))
    }
}

// ---- heapq (`__zipp_py_strm` ops 12 and 13) ------------------------------
//
// `heapq.heappush(heap, item)` (op 12) and `heapq.heappop(heap)` (op 13) on
// an exact list whose items (and `item`) the engine orders itself (ints,
// floats, strs, bools, None and tuples of those, as `__zipp_py_ord` op 2
// reads them): the runtime's sift, comparison for comparison (`lt` is
// Python's `<`), with the list changed only once every comparison has been
// answered. Anything else (an unordered pair, another item, an empty heap
// for a pop) takes the runtime's own entry (`j<n>`).
//
// ---- bisect (`__zipp_py_strm` ops 14 to 17) ------------------------------
//
// `bisect_left` / `bisect_right(a, x)` (14 / 15) on an exact list or tuple,
// and `insort_left` / `insort_right(a, x)` (16 / 17) on an exact list, with
// `x` and every item compared the engine orders itself: the runtime's
// binary search (`bis`), comparison for comparison (`right`: `x < a[mid]`,
// else `not a[mid] < x`), then, for an insort, the insertion. Anything else
// takes the runtime's entry (`j2`).

/// `usize::MAX` names the item being pushed / the element the sift moves.
const MOVING: usize = usize::MAX;

impl<'p> Vm<'p> {
    fn py_arr_at(&self, arr: u32, i: usize) -> Option<Value> {
        match self.heap.get(arr) {
            HeapObj::Array(a) => a.get(i).copied(),
            _ => None,
        }
    }

    /// The Array of an exact list (or, with `tuple_too`, tuple) `seq`, and
    /// the tuple type (for the keys).
    fn py_seq_array(&self, this: Value, seq: Value, tuple_too: bool) -> Option<(u32, Value)> {
        if !seq.is_heap() || !this.is_heap() {
            return None;
        }
        let tmpl = self.py_strm_prop(this.heap_index(), hint::LTMPL, "ltmpl")?;
        let ttype = self.py_strm_prop(this.heap_index(), hint::TTYPE, "ttype")?;
        if !tmpl.is_heap() {
            return None;
        }
        let list_cls = self.py_json_field(tmpl.heap_index(), "cls")?;
        let cls = self.py_json_field(seq.heap_index(), "cls")?;
        if cls.bits() != list_cls.bits() && !(tuple_too && cls.bits() == ttype.bits()) {
            return None;
        }
        let arr = self.py_json_field(seq.heap_index(), "items")?;
        if !arr.is_heap() || !matches!(self.heap.get(arr.heap_index()), HeapObj::Array(_)) {
            return None;
        }
        Some((arr.heap_index(), ttype))
    }

    /// The key of `a[i]` (of `moving` for [`MOVING`]), made once.
    fn py_heap_key(&mut self, keys: &mut Vec<(usize, OrdKeyBox)>, arr: u32, moving: Value, i: usize, tuple: Value) -> Option<usize> {
        if let Some(pos) = keys.iter().position(|(j, _)| *j == i) {
            return Some(pos);
        }
        let v = if i == MOVING { moving } else { self.py_arr_at(arr, i)? };
        let k = self.py_ord_key_boxed(v, tuple)?;
        keys.push((i, k));
        Some(keys.len() - 1)
    }

    /// `a[x] < a[y]`, when the two are ordered.
    fn py_heap_lt(&mut self, keys: &mut Vec<(usize, OrdKeyBox)>, arr: u32, moving: Value, x: usize, y: usize, tuple: Value) -> Option<bool> {
        let kx = self.py_heap_key(keys, arr, moving, x, tuple)?;
        let ky = self.py_heap_key(keys, arr, moving, y, tuple)?;
        self.charge_steps(8);
        self.py_ord_lt_boxed(&keys[kx].1, &keys[ky].1)
    }

    /// The Array changed in place (moves within it need no barrier; a new
    /// element does), then its version bumped.
    fn py_arr_edit(&mut self, arr: u32, new_value: Option<Value>, edit: impl FnOnce(&mut Vec<Value>)) -> Option<()> {
        if let Some(v) = new_value.filter(|v| v.is_heap()) {
            self.heap.write_barrier_val(arr, v);
        }
        match self.heap.get_mut(arr) {
            HeapObj::Array(a) => edit(a),
            _ => return None,
        }
        self.heap.bump_version(arr);
        Some(())
    }

    /// Ops 12 / 13; `None` sends the call to the runtime's entry.
    fn py_heap_op(&mut self, this: Value, args: &[Value], push: bool) -> Option<Value> {
        let (arr, tuple) = self.py_seq_array(this, *args.first()?, false)?;
        let n = match self.heap.get(arr) {
            HeapObj::Array(a) => a.len(),
            _ => return None,
        };
        let mut keys: Vec<(usize, OrdKeyBox)> = Vec::new();
        if push {
            let item = *args.get(1)?;
            if n >= (1 << 24) {
                return None;
            }
            // `up`: the item rises past every parent it is below.
            let mut pos = n;
            let mut path = Vec::new();
            while pos > 0 {
                let parent = (pos - 1) >> 1;
                if !self.py_heap_lt(&mut keys, arr, item, MOVING, parent, tuple)? {
                    break;
                }
                path.push(parent);
                pos = parent;
            }
            self.py_arr_edit(arr, Some(item), |a| {
                a.push(item);
                let mut hole = n;
                for &parent in &path {
                    a[hole] = a[parent];
                    hole = parent;
                }
                a[hole] = item;
            })?;
            return Some(Value::NULL);
        }
        if n == 0 {
            return None;
        }
        let top = self.py_arr_at(arr, 0)?;
        let last = self.py_arr_at(arr, n - 1)?;
        let m = n - 1;
        // `down` from the root with `last` there: a hole sift, as
        // `lt(h[l], h[m])` then `lt(h[r], h[m])` compare it.
        let mut moves = Vec::new();
        let mut i = 0usize;
        if m > 0 {
            loop {
                let l = 2 * i + 1;
                let r = l + 1;
                let mut best = MOVING;
                if l < m && self.py_heap_lt(&mut keys, arr, last, l, best, tuple)? {
                    best = l;
                }
                if r < m && self.py_heap_lt(&mut keys, arr, last, r, best, tuple)? {
                    best = r;
                }
                if best == MOVING {
                    break;
                }
                moves.push(best);
                i = best;
            }
        }
        self.py_arr_edit(arr, None, |a| {
            a.pop();
            if m > 0 {
                let mut hole = 0;
                for &child in &moves {
                    a[hole] = a[child];
                    hole = child;
                }
                a[hole] = last;
            }
        })?;
        Some(top)
    }

    /// Ops 14 to 17; `None` sends the call to the runtime's entry.
    fn py_bisect_op(&mut self, this: Value, args: &[Value], op: i32) -> Option<Value> {
        let insort = op >= 16;
        let right = op == 15 || op == 17;
        let (arr, tuple) = self.py_seq_array(this, args[0], !insort)?;
        let x = args[1];
        let n = match self.heap.get(arr) {
            HeapObj::Array(a) => a.len(),
            _ => return None,
        };
        let mut keys: Vec<(usize, OrdKeyBox)> = Vec::new();
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let go_left = if right {
                self.py_heap_lt(&mut keys, arr, x, MOVING, mid, tuple)?
            } else {
                !self.py_heap_lt(&mut keys, arr, x, mid, MOVING, tuple)?
            };
            if go_left {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        if !insort {
            return Some(self.make_bigint(lo as i128));
        }
        if n >= (1 << 24) {
            return None;
        }
        self.py_arr_edit(arr, Some(x), |a| a.insert(lo, x))?;
        Some(Value::NULL)
    }
}

// ---- iterator steps (`__zipp_py_iter`) ------------------------------------
//
// The `next` member of two kinds of runtime iterator records, which keep
// their state in their own fields (`{cls, next, jnext, kind, a, i, b, size,
// pick, tmpl, ttype}`) and carry their JavaScript step as `jnext`:
//
// * kind 1, a dict view's iterator: `a` the entries snapshot (an Array of
//   `[key, value]` Arrays), `i` the position, `b` the dict record, `size`
//   the size it had; `pick` 0 / 1 / 2 for keys / values / items (a tuple).
// * kind 2, `enumerate` over an exact list or tuple: `a` the sequence
//   record (its live `items` Array), `i` the position, `b` the count (an
//   int).
// * kind 3, a dict's key iterator: `a` the keys snapshot (an Array), `i`,
//   `b` and `size` as for kind 1.
// * kind 4, `zip` over exact lists and tuples: `a` an Array of the
//   sequence records (their live `items`), `i` the common position; each
//   step a tuple of the items at `i`.
//
// The native step does exactly what `jnext` does for the common step (an
// item there, the dict unchanged) and calls `jnext` for every other one
// (the end, a changed dict, a record of another shape), so errors and the
// end are the runtime's own.


impl<'p> Vm<'p> {
    /// `__zipp_py_iter`, called as `it.next()`.
    pub(crate) fn py_iter_next(&mut self, this: Value) -> Result<Value, Thrown> {
        if let Some(v) = self.py_iter_fast(this) {
            return Ok(v);
        }
        let jnext = if this.is_heap() { self.py_strm_prop(this.heap_index(), hint::JNEXT, "jnext") } else { None };
        match jnext {
            Some(f) => self.call_value(f, this, &[]),
            None => Err(Thrown("TypeError: iterator step on a non-iterator".into())),
        }
    }

    /// Write the record's own data slot for `key` (found through `hint`).
    fn py_iter_set(&mut self, rec: u32, h: usize, key: &str, v: Value) -> Option<()> {
        let (_, slot) = self.py_hint_slot(h, rec, key)?;
        if v.is_heap() {
            self.heap.write_barrier_val(rec, v);
        }
        if let HeapObj::Object(m) = self.heap.get_mut(rec) {
            m.set_val_at(slot, v);
        }
        Some(())
    }

    /// A two-item tuple made from the record's template.
    fn py_iter_pair(&mut self, rec: u32, a: Value, b: Value) -> Option<Value> {
        self.py_iter_tuple(rec, vec![a, b])
    }

    /// A tuple of `items` made from the record's template.
    fn py_iter_tuple(&mut self, rec: u32, items: Vec<Value>) -> Option<Value> {
        let tmpl = self.py_strm_prop(rec, hint::ITMPL, "tmpl")?;
        let ttype = self.py_strm_prop(rec, hint::ITTYPE, "ttype")?;
        let (mut obj, slot) = self.py_json_template(tmpl, &["cls", "items"])?;
        let items = Value::heap(self.heap.alloc(HeapObj::Array(items)));
        obj.set_val_at(0, ttype);
        obj.set_val_at(slot, items);
        Some(self.alloc_object_current_realm(obj))
    }

    fn py_iter_fast(&mut self, this: Value) -> Option<Value> {
        if !this.is_heap() {
            return None;
        }
        let rec = this.heap_index();
        let kind = self.py_strm_prop(rec, hint::IKIND, "kind")?;
        let i = self.py_strm_prop(rec, hint::II, "i")?;
        if !i.is_int() || i.as_int() < 0 {
            return None;
        }
        let pos = i.as_int() as usize;
        let a = self.py_strm_prop(rec, hint::IA, "a")?;
        let b = self.py_strm_prop(rec, hint::IB, "b")?;
        if kind == Value::int(3) {
            let size = self.py_strm_prop(rec, hint::ISIZE, "size")?;
            if !b.is_heap() || !a.is_heap() {
                return None;
            }
            let now = self.py_strm_prop(b.heap_index(), hint::ISIZE_DICT, "size")?;
            if !size.is_number() || !now.is_number() || size.as_f64() != now.as_f64() {
                return None;
            }
            let k = match self.heap.get(a.heap_index()) {
                HeapObj::Array(keys) => *keys.get(pos)?,
                _ => return None,
            };
            if k == Value::HOLE {
                return None;
            }
            self.py_iter_set(rec, hint::II, "i", Value::int(i32::try_from(pos + 1).ok()?))?;
            return Some(k);
        }
        if kind == Value::int(4) {
            if !a.is_heap() {
                return None;
            }
            let srcs = match self.heap.get(a.heap_index()) {
                HeapObj::Array(s) => s.clone(),
                _ => return None,
            };
            let mut out = Vec::with_capacity(srcs.len());
            for src in srcs {
                if !src.is_heap() {
                    return None;
                }
                let items = self.py_strm_prop(src.heap_index(), hint::IITEMS, "items")?;
                if !items.is_heap() {
                    return None;
                }
                match self.heap.get(items.heap_index()) {
                    HeapObj::Array(it) => out.push(*it.get(pos)?),
                    _ => return None,
                }
            }
            let step = Value::int(i32::try_from(pos + 1).ok()?);
            let t = self.py_iter_tuple(rec, out)?;
            self.py_iter_set(rec, hint::II, "i", step)?;
            return Some(t);
        }
        if kind == Value::int(1) {
            // `if (d.size !== this.size) fail(...)`: the dict is `b`.
            let size = self.py_strm_prop(rec, hint::ISIZE, "size")?;
            if !b.is_heap() {
                return None;
            }
            let now = self.py_strm_prop(b.heap_index(), hint::ISIZE_DICT, "size")?;
            if !size.is_number() || !now.is_number() || size.as_f64() != now.as_f64() {
                return None;
            }
            if !a.is_heap() {
                return None;
            }
            let entry = match self.heap.get(a.heap_index()) {
                HeapObj::Array(e) => *e.get(pos)?,
                _ => return None,
            };
            if !entry.is_heap() {
                return None;
            }
            let (k, v) = match self.heap.get(entry.heap_index()) {
                HeapObj::Array(kv) => (*kv.first()?, *kv.get(1)?),
                _ => return None,
            };
            let pick = self.py_strm_prop(rec, hint::IPICK, "pick")?;
            let out = if pick == Value::int(0) {
                k
            } else if pick == Value::int(1) {
                v
            } else if pick == Value::int(2) {
                self.py_iter_pair(rec, k, v)?
            } else {
                return None;
            };
            self.py_iter_set(rec, hint::II, "i", Value::int(i32::try_from(pos + 1).ok()?))?;
            return Some(out);
        }
        if kind == Value::int(2) {
            // `const items = this.a.items; if (this.i < items.length) ...`
            if !a.is_heap() || !b.is_heap() {
                return None;
            }
            let items = self.py_strm_prop(a.heap_index(), hint::IITEMS, "items")?;
            if !items.is_heap() {
                return None;
            }
            let v = match self.heap.get(items.heap_index()) {
                HeapObj::Array(it) => *it.get(pos)?,
                _ => return None,
            };
            let HeapObj::BigInt(c) = self.heap.get(b.heap_index()) else {
                return None;
            };
            let next = c.checked_add(1)?;
            let out = self.py_iter_pair(rec, b, v)?;
            let step = Value::int(i32::try_from(pos + 1).ok()?);
            let nb = self.make_bigint(next);
            self.py_iter_set(rec, hint::IB, "b", nb)?;
            self.py_iter_set(rec, hint::II, "i", step)?;
            return Some(out);
        }
        None
    }
}
