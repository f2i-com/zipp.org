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
        if let Some(n) = v.small_bigint_val() {
            return Some((n < 0, n.unsigned_abs().to_string()));
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

    pub(super) fn py_pct_format(&mut self, args: &[Value]) -> Option<Value> {
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
//   `chars` cut from both / the left / the right end; `strip` / `lstrip` /
//   `rstrip(s)`: Python's whitespace cut from them ([`py_space`]).
// * 4 `split(s)`: the runs between Python's whitespace, ASCII `s` only;
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
// * 18 / 19 `startswith` / `endswith(s, affix)`, `affix` a str (a tuple of
//   them, and the forms with bounds, are `j<n>`'s).
// * 20 `R.FJOIN.c<n>(...parts)`: an f-string's parts joined, each a str or
//   an int (its decimal digits, as `str(i)` makes them), the record's
//   `j<n>` (`out += part`, from "") in one allocation (see
//   [`Vm::py_fstr_join`]).
//
// Results longer than the runtime's text limit (`MAX_TEXT` UTF-16 units)
// take the `j<n>` path, which raises as it always did.

use super::py_rt::hint;

/// `rt.MAX_TEXT` (UTF-16 units).
const STRM_MAX_TEXT: usize = 1 << 26;

/// Python's whitespace among the ASCII characters: what `str.isspace` is
/// true for, which `strip()` and `split()` without an argument go by. It has
/// the four separators U+001C to U+001F, which JavaScript's `\s` and `trim`
/// lack. Beyond ASCII it is Unicode's White_Space ([`py_white`]): U+0085 is
/// in it and U+FEFF is not, the other way round from JavaScript's.
fn py_space(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | 0x1c..=0x1f | b' ')
}

/// Python's whitespace (see [`py_space`]).
fn py_white(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

impl<'p> Vm<'p> {
    /// An own data property of the plain object `idx`, at the slot `hint`
    /// remembers when that still holds `key`.
    fn py_strm_prop(&self, idx: u32, h: usize, key: &str) -> Option<Value> {
        self.py_hint_field(h, idx, key)
    }

    /// [`Vm::py_strm_prop`] of a builtin record, tried first at `at`: the
    /// slot the calling site remembers for this record (`usize::MAX`: none).
    /// The VM's hint `h` is one cell for every builtin record, and their
    /// slots for a key differ (the runtime's `install` appends `strop`,
    /// `ltmpl` and `ttype` after however many entries a record already
    /// had), so that hint alone misses at every call of a program that
    /// alternates between two str methods, and the key is found by name.
    #[inline]
    fn py_strm_prop_at(&self, idx: u32, at: usize, h: usize, key: &str) -> Option<Value> {
        if let HeapObj::Object(m) = self.heap.get(idx) {
            if !m.is_ctor && at < m.slot_len() && super::py_rt::key_eq(m.key_at(at), key) && !m.is_accessor_at(at) {
                return Some(m.val_at(at));
            }
        }
        self.py_hint_field(h, idx, key)
    }

    /// A primitive str with no lone surrogate, flattened in place: its heap
    /// slot and whether it is ASCII. The bytes are then read where they
    /// live ([`Vm::py_strm_bytes`]) once every operand has been flattened --
    /// each method used to copy its receiver and every str argument into a
    /// fresh buffer before doing any work.
    #[inline]
    fn py_strm_flat(&mut self, v: Value) -> Option<(u32, bool)> {
        if !v.is_heap() {
            return None;
        }
        let idx = v.heap_index();
        // Fast path: 99%+ of Python strings are already flat `HeapObj::Str`.
        if let HeapObj::Str(s) = self.heap.get(idx) {
            let ascii = s.is_ascii();
            return (ascii || s.is_wellformed()).then_some((idx, ascii));
        }
        if !self.heap.is_str_like(idx) {
            return None;
        }
        self.heap.flatten(idx);
        let HeapObj::Str(s) = self.heap.get(idx) else {
            return None;
        };
        let ascii = s.is_ascii();
        (ascii || s.is_wellformed()).then_some((idx, ascii))
    }

    /// The bytes of a str [`Vm::py_strm_flat`] admitted.
    fn py_strm_bytes(&self, idx: u32) -> &[u8] {
        match self.heap.get(idx) {
            HeapObj::Str(s) => s.as_bytes(),
            _ => &[],
        }
    }

    fn py_strm_new(&mut self, bytes: Vec<u8>) -> Value {
        Value::heap(self.heap.alloc_js(JsStr::from_wtf8(bytes)))
    }

    /// `s_idx`'s bytes `a..b` in a buffer from the heap's byte pool.
    fn py_strm_copy(&mut self, s_idx: u32, a: usize, b: usize) -> Vec<u8> {
        let mut buf = self.heap.take_byte_buf(b - a);
        buf.extend_from_slice(&self.py_strm_bytes(s_idx)[a..b]);
        buf
    }

    /// Slice `s_idx[a..b]` directly into an interned or 23-byte inline SSO string
    /// when ASCII and small, avoiding `byte_pool`/`mi_malloc`.
    #[inline]
    fn py_strm_slice_as(&mut self, s_idx: u32, a: usize, b: usize, ascii: bool) -> Value {
        let len = b - a;
        if ascii {
            if len == 0 {
                return Value::heap(crate::heap::INTERN_EMPTY);
            }
            if len == 1 {
                return Value::heap(self.py_strm_bytes(s_idx)[a] as u32);
            }
            if len <= crate::heap::STR_INLINE_CAP {
                let js = JsStr::from_ascii_slice(&self.py_strm_bytes(s_idx)[a..b]);
                return Value::heap(self.heap.alloc_str_settled(js));
            }
        }
        let bytes = self.py_strm_copy(s_idx, a, b);
        self.py_strm_new_as(bytes, ascii)
    }

    /// [`Vm::py_strm_new`] of bytes known to be ASCII when `ascii` (cut from
    /// or joined of ASCII strs only): made without the scan `from_wtf8`
    /// runs over every result to find that out again.
    #[inline]
    fn py_strm_new_as(&mut self, bytes: Vec<u8>, ascii: bool) -> Value {
        if ascii {
            match bytes.len() {
                0 => {
                    self.heap.return_byte_buf(bytes);
                    Value::heap(crate::heap::INTERN_EMPTY)
                }
                1 => {
                    let b = bytes[0] as u32;
                    self.heap.return_byte_buf(bytes);
                    Value::heap(b)
                }
                _ => Value::heap(self.heap.alloc_str_settled(JsStr::from_ascii(bytes))),
            }
        } else {
            self.py_strm_new(bytes)
        }
    }

    /// Ultra-thin 2-arg fast path for `startswith` (18), `endswith` (19), and
    /// `find` (6) called directly from `jit_py_strm_noalloc` without copying
    /// arguments or entering the 900-line `py_strm_fast_op` match.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn py_strm_affix_find_2(&mut self, op: i32, a0: Value, a1: Value) -> Option<Value> {
        let (s_idx, ascii) = self.py_strm_flat(a0)?;
        let (sub_idx, _) = self.py_strm_flat(a1)?;
        if op == 18 || op == 19 {
            let (found, compared) = {
                let s = self.py_strm_bytes(s_idx);
                let sub = self.py_strm_bytes(sub_idx);
                (
                    if op == 18 { s.starts_with(sub) } else { s.ends_with(sub) },
                    sub.len().min(s.len()),
                )
            };
            if self.instr_rec.is_some() {
                self.charge_steps(i64::try_from(compared).unwrap_or(i64::MAX));
            }
            return Some(Value::bool(found));
        }
        // op == 6 (`find`)
        let (found, scanned) = {
            let s = self.py_strm_bytes(s_idx);
            let sub = self.py_strm_bytes(sub_idx);
            let at = if sub.is_empty() {
                Some(0)
            } else if sub.len() == 1 {
                memchr::memchr(sub[0], s)
            } else if sub.len() > s.len() {
                None
            } else {
                memchr::memmem::find(s, sub)
            };
            let scanned = at.map_or(s.len(), |i| i + sub.len());
            (
                at.map(|i| if ascii { i } else { s[..i].iter().filter(|&&b| b & 0xC0 != 0x80).count() }),
                scanned,
            )
        };
        if self.instr_rec.is_some() {
            self.charge_steps(i64::try_from(scanned).unwrap_or(i64::MAX));
        }
        Some(match found {
            None => Value::small_bigint(-1)?,
            Some(cps) => Value::small_bigint(cps as i128)?,
        })
    }

    /// `__zipp_py_strm` fast path only (never runs guest code): returns `None`
    /// when `py_str_method_fallback` must handle the call.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn py_strm_try_fast_op(&mut self, this: Value, args: &[Value], at: usize, op: i32) -> Option<Value> {
        self.py_strm_fast_op(this, args, at, op)
    }

    /// `__zipp_py_strm` fallback when [`Vm::py_strm_try_fast_op`] declined.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[cold]
    #[inline(never)]
    pub(crate) fn py_str_method_fallback_entry(&mut self, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        self.py_str_method_fallback(this, args)
    }

    /// `__zipp_py_strm`, called as `builtin.c<n>(...args)`.
    pub(crate) fn py_str_method(&mut self, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        self.py_str_method_at(this, args, usize::MAX)
    }

    /// [`Vm::py_str_method`] from a call site that remembers the slot of
    /// the record's `strop` (`at`, `usize::MAX` for none; the runtime adds
    /// `ltmpl` and `ttype` with it, so they are looked for after it).
    pub(crate) fn py_str_method_at(&mut self, this: Value, args: &[Value], at: usize) -> Result<Value, Thrown> {
        if this.is_heap() {
            if let Some(op) = self.py_strm_prop_at(this.heap_index(), at, hint::STROP, "strop") {
                if op.is_int() {
                    let op_i = op.as_int();
                    if matches!(op_i, 52..=55) {
                        if let Some(v) = self.py_strm_throwing_op(this, args, op_i)? {
                            return Ok(v);
                        }
                        return self.py_str_method_fallback(this, args);
                    }
                    if let Some(v) = self.py_strm_fast_op(this, args, at, op_i) {
                        return Ok(v);
                    }
                    return self.py_str_method_fallback(this, args);
                }
            }
        }
        self.py_str_method_fallback(this, args)
    }

    #[cold]
    #[inline(never)]
    fn py_str_method_fallback(&mut self, this: Value, args: &[Value]) -> Result<Value, Thrown> {
        // The entry this one stands in for, with the same arguments.
        const FALLBACKS: [&str; 13] = ["j0", "j1", "j2", "j3", "j4", "j5", "j6", "j7", "j8", "j9", "j10", "j11", "j12"];
        let Some(&name) = FALLBACKS.get(args.len()) else {
            return Err(Thrown::from_static("TypeError: str method entry called with a wrong count"));
        };
        let HeapObj::Object(m) = (if this.is_heap() { self.heap.get(this.heap_index()) } else { return Err(Thrown::from_static("TypeError: str method entry called on a non-builtin")) }) else {
            return Err(Thrown::from_static("TypeError: str method entry called on a non-builtin"));
        };
        let Some(slot) = m.pos(name).filter(|&s| !m.attr_at(s).accessor) else {
            return Err(Thrown::from_static("TypeError: str method entry called on a non-builtin"));
        };
        let f = m.val_at(slot);
        self.call_value(f, this, args)
    }

    /// Native `str + str` for `PyArith(Add)`: well-formed primitive strings,
    /// returning `None` for lone surrogates or lengths exceeding `STRM_MAX_TEXT`.
    pub(super) fn py_str_concat(&mut self, a: Value, b: Value) -> Option<Value> {
        let (a_idx, a_ascii) = self.py_strm_flat(a)?;
        let (b_idx, b_ascii) = self.py_strm_flat(b)?;
        let (a_len, b_len) = (self.py_strm_bytes(a_idx).len(), self.py_strm_bytes(b_idx).len());
        if a_len == 0 {
            let units = match self.heap.get(b_idx) {
                HeapObj::Str(st) => st.units(),
                _ => return None,
            };
            return (units <= STRM_MAX_TEXT).then_some(b);
        }
        if b_len == 0 {
            let units = match self.heap.get(a_idx) {
                HeapObj::Str(st) => st.units(),
                _ => return None,
            };
            return (units <= STRM_MAX_TEXT).then_some(a);
        }
        let total = a_len.checked_add(b_len)?;
        let both_ascii = a_ascii && b_ascii;
        if both_ascii && total > STRM_MAX_TEXT {
            return None;
        }
        if !both_ascii && total > STRM_MAX_TEXT * 3 {
            return None;
        }
        let v = if both_ascii && total <= crate::heap::STR_INLINE_CAP {
            let mut stack = [0u8; 24];
            let ab = self.py_strm_bytes(a_idx);
            let bb = self.py_strm_bytes(b_idx);
            stack[..a_len].copy_from_slice(ab);
            stack[a_len..total].copy_from_slice(bb);
            Value::heap(
                self.heap
                    .alloc_str_settled(JsStr::from_ascii_slice(&stack[..total])),
            )
        } else {
            let mut out = self.heap.take_byte_buf(total);
            out.extend_from_slice(self.py_strm_bytes(a_idx));
            out.extend_from_slice(self.py_strm_bytes(b_idx));
            self.py_strm_new_as(out, both_ascii)
        };
        if !both_ascii {
            if let HeapObj::Str(st) = self.heap.get(v.heap_index()) {
                if st.units() > STRM_MAX_TEXT {
                    return None;
                }
            }
        }
        self.charge_steps(i64::try_from(total).unwrap_or(i64::MAX));
        Some(v)
    }

    /// Native `str * int/bool` for a well-formed primitive `str` and small repeat count.
    pub(super) fn py_str_repeat(&mut self, s: Value, n_val: Value) -> Option<Value> {
        let count_i = if let Some(k) = n_val.small_bigint_val() {
            k as i128
        } else if n_val.is_bool() {
            if n_val.as_bool() { 1 } else { 0 }
        } else if n_val.is_heap() {
            match self.heap.get(n_val.heap_index()) {
                HeapObj::BigInt(k) => *k,
                _ => return None,
            }
        } else {
            return None;
        };
        let (s_idx, is_ascii) = self.py_strm_flat(s)?;
        if count_i <= 0 {
            return Some(Value::heap(crate::heap::INTERN_EMPTY));
        }
        let slen = self.py_strm_bytes(s_idx).len();
        if slen == 0 {
            return Some(Value::heap(crate::heap::INTERN_EMPTY));
        }
        if count_i == 1 {
            let units = match self.heap.get(s_idx) {
                HeapObj::Str(st) => st.units(),
                _ => return None,
            };
            return (units <= STRM_MAX_TEXT).then_some(s);
        }
        let count = usize::try_from(count_i).ok()?;
        let total = slen.checked_mul(count)?;
        if total > 65536 {
            return None;
        }
        #[cfg(feature = "instrument")]
        if self.instr_rec.is_some() && total > 1024 {
            return None;
        }
        let v = if is_ascii && total <= crate::heap::STR_INLINE_CAP {
            let mut stack = [0u8; 24];
            let src = self.py_strm_bytes(s_idx);
            if slen == 1 {
                stack[..total].fill(src[0]);
            } else {
                for i in 0..count {
                    stack[i * slen..(i + 1) * slen].copy_from_slice(src);
                }
            }
            Value::heap(
                self.heap
                    .alloc_str_settled(JsStr::from_ascii_slice(&stack[..total])),
            )
        } else {
            let mut out = self.heap.take_byte_buf(total);
            {
                let src = self.py_strm_bytes(s_idx);
                if slen == 1 {
                    out.resize(total, src[0]);
                } else {
                    for _ in 0..count {
                        out.extend_from_slice(src);
                    }
                }
            }
            self.py_strm_new_as(out, is_ascii)
        };
        self.charge_steps(total as i64);
        Some(v)
    }

    /// Parse optional `start`/`end` slice bounds in `args[2..]` for an ASCII
    /// string of byte length `len`. Returns `None` to fall back to JS (non-int
    /// bound), `Some(None)` for an empty/out-of-range window (`lo > hi`), or
    /// `Some(Some((lo, hi)))` with `0 <= lo <= hi <= len`.
    fn py_strm_bounds(&self, args: &[Value], len: usize) -> Option<Option<(usize, usize)>> {
        let n = len as i128;
        let lo = match args.get(2).copied() {
            None => 0usize,
            Some(v) if v == Value::NULL || v.is_undefined() => 0usize,
            Some(v) => {
                let i = self.bigint_i128(v)?;
                if i < 0 {
                    (i + n).max(0) as usize
                } else if i > n {
                    return Some(None);
                } else {
                    i as usize
                }
            }
        };
        let hi = match args.get(3).copied() {
            None => len,
            Some(v) if v == Value::NULL || v.is_undefined() => len,
            Some(v) => {
                let i = self.bigint_i128(v)?;
                if i < 0 {
                    (i + n).max(0) as usize
                } else {
                    i.min(n) as usize
                }
            }
        };
        if lo > hi {
            Some(None)
        } else {
            Some(Some((lo, hi)))
        }
    }

    /// Parse optional `start`/`end` slice bounds in `args[2..]` for a well-formed
    /// ASCII or BMP string `s_idx`. Returns `None` to fall back to JS (non-int
    /// bound or astral string), `Some(None)` for an empty/out-of-range window
    /// (`lo > hi`), or `Some(Some((byte_lo, byte_hi, cp_lo)))`.
    fn py_strm_bmp_bounds(&self, s_idx: u32, args: &[Value]) -> Option<Option<(usize, usize, usize)>> {
        let HeapObj::Str(st) = self.heap.get(s_idx) else {
            return None;
        };
        if st.is_ascii() {
            return match self.py_strm_bounds(args, st.as_bytes().len())? {
                None => Some(None),
                Some((lo, hi)) => Some(Some((lo, hi, lo))),
            };
        }
        if !st.is_wellformed() || st.has_surrogate() {
            return None;
        }
        let cp_len = st.units();
        match self.py_strm_bounds(args, cp_len)? {
            None => Some(None),
            Some((cp_lo, cp_hi)) => {
                let (ba, bb) = st.bmp_byte_range(cp_lo, cp_hi);
                Some(Some((ba, bb, cp_lo)))
            }
        }
    }

    /// Test `startswith` (`starts == true`) or `endswith` against a single str
    /// or a tuple of strs over byte slice `win` of `s_idx`.
    fn py_strm_affix(
        &mut self,
        this: Value,
        at: usize,
        s_idx: u32,
        ba: usize,
        bb: usize,
        affix_arg: Value,
        starts: bool,
    ) -> Option<Value> {
        if let Some((affix_idx, _)) = self.py_strm_flat(affix_arg) {
            let (found, compared) = {
                let win = &self.py_strm_bytes(s_idx)[ba..bb];
                let affix = self.py_strm_bytes(affix_idx);
                (
                    if starts { win.starts_with(affix) } else { win.ends_with(affix) },
                    affix.len().min(win.len()),
                )
            };
            self.charge_steps(i64::try_from(compared).unwrap_or(i64::MAX));
            return Some(Value::bool(found));
        }
        if !affix_arg.is_heap() {
            return None;
        }
        let ttype = match self.py_rt.as_deref() {
            Some(p) if p.t_tuple.is_heap() => p.t_tuple,
            _ => self.py_strm_seq_types(this, at)?.1,
        };
        let (cls, arr) = self.py_seq_parts(affix_arg)?;
        if cls.bits() != ttype.bits() || !arr.is_heap() {
            return None;
        }
        let arr_idx = arr.heap_index();
        let mut elem_idxs = [0u32; 16];
        let count = match self.heap.get(arr_idx) {
            HeapObj::Array(a) if a.len() <= elem_idxs.len() => a.len(),
            _ => return None,
        };
        for i in 0..count {
            let el = match self.heap.get(arr_idx) {
                HeapObj::Array(a) => a[i],
                _ => return None,
            };
            let (ei, _) = self.py_strm_flat(el)?;
            elem_idxs[i] = ei;
        }
        let (found, compared) = {
            let win = &self.py_strm_bytes(s_idx)[ba..bb];
            let mut matched = false;
            let mut work = 0usize;
            for &ei in &elem_idxs[..count] {
                let affix = self.py_strm_bytes(ei);
                work += affix.len().min(win.len());
                if if starts { win.starts_with(affix) } else { win.ends_with(affix) } {
                    matched = true;
                    break;
                }
            }
            (matched, work)
        };
        self.charge_steps(i64::try_from(compared).unwrap_or(i64::MAX));
        Some(Value::bool(found))
    }

    fn py_strm_fast_op(&mut self, this: Value, args: &[Value], at: usize, op: i32) -> Option<Value> {
        match (op, args.len()) {
            (12, 2) => return self.py_heap_op(this, args, true, at),
            (14..=17, 2) => return self.py_bisect_op(this, args, op, at),
            (13, 1) => return self.py_heap_op(this, args, false, at),
            (20, _) => return self.py_fstr_join(args),
            // Direct `o[lo:hi]` / `o[lo:hi:step]` without allocating a slice record.
            (37, 3) | (37, 4) => return self.py_slice_get(args),
            // Native `re.Pattern.search` (38), `match` (39), `fullmatch` (40).
            (38..=40, 2) | (38 | 39, 3) => return self.py_re_exec(this, args, op),
            // Native `re.Pattern.sub` (41).
            (41, 3) | (41, 4) => return self.py_re_sub(args),
            // Native `re.Match.start` (42), `end` (43), `span` (44), `group` (45), `groups` (46).
            (42..=46, 1) | (42..=46, 2) => return self.py_re_match_op(args, op),
            // Native `typing.cast(typ, val) -> val` (47, strictly non-allocating).
            (47, 2) => return Some(args[1]),
            // Native module-level `re.sub` (48) with cached `str` pattern or compiled `Pattern`.
            (48, 3) => {
                let p = self.py_re_resolve_pat(this, args[0])?;
                return self.py_re_sub(&[p, args[1], args[2]]);
            }
            (48, 4) => {
                let p = self.py_re_resolve_pat(this, args[0])?;
                return self.py_re_sub(&[p, args[1], args[2], args[3]]);
            }
            // Native module-level `re.search` (49), `re.match` (50), `re.fullmatch` (51).
            (49..=51, 2) => {
                let p = self.py_re_resolve_pat(this, args[0])?;
                return self.py_re_exec(this, &[p, args[1]], op - 11);
            }
            // `ord(ch)` of a 1-code-point well-formed str.
            (34, 1) => {
                let (s_idx, ascii) = self.py_strm_flat(args[0])?;
                let cp = {
                    let b = self.py_strm_bytes(s_idx);
                    if ascii {
                        if b.len() != 1 {
                            return None;
                        }
                        b[0] as u32
                    } else {
                        let s = std::str::from_utf8(b).ok()?;
                        let mut it = s.chars();
                        let c = it.next()?;
                        if it.next().is_some() {
                            return None;
                        }
                        c as u32
                    }
                };
                return Some(self.make_bigint(cp as i128));
            }
            // `chr(n)` for a valid Unicode scalar `0..=0x10FFFF`.
            (35, 1) => {
                let n = self.bigint_i128(args[0])?;
                if !(0..=0x10FFFF).contains(&n) {
                    return None;
                }
                let cp = n as u32;
                if cp < 128 {
                    return Some(Value::heap(cp));
                }
                return Some(Value::heap(self.heap.alloc_js(JsStr::from_code_point(cp))));
            }
            // `hash(s)` of a str: `strHash` (`runtime/types.js`), computed
            // by the function the engine's own tables hash a str key with,
            // so the two cannot differ. It fits the int kept in the value.
            (10, 1) => {
                let v = args[0];
                if !v.is_heap() || !self.heap.is_str_like(v.heap_index()) {
                    return None;
                }
                let idx = v.heap_index();
                self.heap.flatten(idx);
                let cache = self.py_rt.as_deref().map(|p| &*p.str_hashes);
                let h = super::py_table::str_hash_value_cached(&self.heap, cache, idx)?;
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
        let (s_idx, ascii) = self.py_strm_flat(*args.first()?)?;
        let n = self.py_strm_bytes(s_idx).len() as u64;
        let out = match (op, args.len()) {
            // Whitespace cut from the ends: an ASCII str's by its bytes, any
            // other's by its code points. Also handles explicit `None` arg
            // (`s.strip(None)`).
            (1..=3, 1) | (1..=3, 2) if args.len() == 1 || args[1] == Value::NULL => {
                let trimmed = {
                    let s = self.py_strm_bytes(s_idx);
                    let (i, j) = if ascii {
                        let (mut i, mut j) = (0, s.len());
                        if op != 3 {
                            while i < j && py_space(s[i]) {
                                i += 1;
                            }
                        }
                        if op != 2 {
                            while j > i && py_space(s[j - 1]) {
                                j -= 1;
                            }
                        }
                        (i, j)
                    } else {
                        let text = std::str::from_utf8(s).ok()?;
                        let r = match op {
                            1 => text.trim_matches(py_white),
                            2 => text.trim_start_matches(py_white),
                            _ => text.trim_end_matches(py_white),
                        };
                        let i = r.as_ptr() as usize - text.as_ptr() as usize;
                        (i, i + r.len())
                    };
                    (j - i != s.len()).then_some((i, j))
                };
                match trimmed {
                    None => return Some(args[0]),
                    Some((i, j)) => self.py_strm_slice_as(s_idx, i, j, ascii),
                }
            }
            // Both strs are well formed, so equal bytes at an end are equal
            // code points there. Supports single str or tuple of strs.
            (18, 2) | (19, 2) => {
                let blen = self.py_strm_bytes(s_idx).len();
                return self.py_strm_affix(this, at, s_idx, 0, blen, args[1], op == 18);
            }
            (18, 3) | (18, 4) | (19, 3) | (19, 4) => {
                let Some((ba, bb, _)) = self.py_strm_bmp_bounds(s_idx, args)? else {
                    return Some(Value::FALSE);
                };
                return self.py_strm_affix(this, at, s_idx, ba, bb, args[1], op == 18);
            }
            (1..=3, 2) => {
                let (chars_idx, chars_ascii) = self.py_strm_flat(args[1])?;
                let trimmed = {
                    let s = self.py_strm_bytes(s_idx);
                    let cb = self.py_strm_bytes(chars_idx);
                    if cb.is_empty() || s.is_empty() {
                        None
                    } else if ascii && chars_ascii {
                        // 128-bit ASCII byte set on the stack: zero UTF-8
                        // validation and zero char decoding.
                        let mut bits = [0u64; 2];
                        for &b in cb {
                            bits[(b >> 6) as usize] |= 1u64 << (b & 63);
                        }
                        let has = |b: u8| (bits[(b >> 6) as usize] & (1u64 << (b & 63))) != 0;
                        let (mut i, mut j) = (0usize, s.len());
                        if op != 3 {
                            while i < j && has(s[i]) {
                                i += 1;
                            }
                        }
                        if op != 2 {
                            while j > i && has(s[j - 1]) {
                                j -= 1;
                            }
                        }
                        (j - i != s.len()).then_some((i, j))
                    } else {
                        let set = std::str::from_utf8(cb).ok()?;
                        let text = std::str::from_utf8(s).ok()?;
                        let cut = |c: char| set.contains(c);
                        let r = match op {
                            1 => text.trim_matches(cut),
                            2 => text.trim_start_matches(cut),
                            _ => text.trim_end_matches(cut),
                        };
                        let i = r.as_ptr() as usize - text.as_ptr() as usize;
                        (r.len() != text.len()).then_some((i, i + r.len()))
                    }
                };
                match trimmed {
                    None => return Some(args[0]),
                    Some((i, j)) => {
                        let out = self.py_strm_slice_as(s_idx, i, j, ascii);
                        if self.instr_rec.is_some() {
                            self.charge_steps(i64::try_from(n).unwrap_or(i64::MAX));
                        }
                        return Some(out);
                    }
                }
            }
            (4, 1) | (4, 2) | (4, 3) if args.len() == 1 || args[1] == Value::NULL => {
                if !ascii {
                    return None;
                }
                let maxsplit: Option<usize> = if args.len() == 3 {
                    let m = self.bigint_i128(args[2])?;
                    if m < 0 { None } else { Some(usize::try_from(m).unwrap_or(usize::MAX)) }
                } else {
                    None
                };
                const STACK_PARTS: usize = 16;
                let mut stack = [(0u32, 0u32); STACK_PARTS];
                let mut count = 0usize;
                let mut spill: Vec<(u32, u32)> = Vec::new();
                {
                    let s = self.py_strm_bytes(s_idx);
                    let mut i = 0;
                    let mut splits = 0usize;
                    while i < s.len() {
                        while i < s.len() && py_space(s[i]) {
                            i += 1;
                        }
                        if i >= s.len() {
                            break;
                        }
                        let start = i;
                        let end = if maxsplit.is_some_and(|m| splits >= m) {
                            i = s.len();
                            s.len()
                        } else {
                            while i < s.len() && !py_space(s[i]) {
                                i += 1;
                            }
                            splits += 1;
                            i
                        };
                        if spill.is_empty() && count < STACK_PARTS {
                            stack[count] = (start as u32, end as u32);
                            count += 1;
                        } else {
                            if spill.is_empty() {
                                spill.reserve(STACK_PARTS * 2);
                                spill.extend_from_slice(&stack[..count]);
                            }
                            spill.push((start as u32, end as u32));
                        }
                    }
                }
                let slice: &[(u32, u32)] = if spill.is_empty() { &stack[..count] } else { &spill };
                return self.py_strm_list(this, s_idx, slice, at, ascii);
            }
            (4, 2) | (4, 3) => {
                let (sep_idx, _) = self.py_strm_flat(args[1])?;
                let maxsplit: Option<usize> = if args.len() == 3 {
                    let m = self.bigint_i128(args[2])?;
                    if m < 0 { None } else { Some(usize::try_from(m).unwrap_or(usize::MAX)) }
                } else {
                    None
                };
                const STACK_PARTS: usize = 16;
                let mut stack = [(0u32, 0u32); STACK_PARTS];
                let mut count = 0usize;
                let mut spill: Vec<(u32, u32)> = Vec::new();
                let ok = {
                    let s = self.py_strm_bytes(s_idx);
                    let sep = self.py_strm_bytes(sep_idx);
                    if sep.is_empty() {
                        false
                    } else {
                        let mut push_part = |a: usize, b: usize| {
                            if spill.is_empty() && count < STACK_PARTS {
                                stack[count] = (a as u32, b as u32);
                                count += 1;
                            } else {
                                if spill.is_empty() {
                                    spill.reserve(STACK_PARTS * 2);
                                    spill.extend_from_slice(&stack[..count]);
                                }
                                spill.push((a as u32, b as u32));
                            }
                        };
                        let mut start = 0;
                        let mut splits = 0usize;
                        let limit = maxsplit.unwrap_or(usize::MAX);
                        if limit > 0 {
                            if sep.len() == 1 {
                                let needle = sep[0];
                                while splits < limit {
                                    let Some(rel) = memchr::memchr(needle, &s[start..]) else {
                                        break;
                                    };
                                    let at = start + rel;
                                    push_part(start, at);
                                    start = at + 1;
                                    splits += 1;
                                }
                            } else {
                                let finder = memchr::memmem::Finder::new(sep);
                                while splits < limit {
                                    let Some(rel) = finder.find(&s[start..]) else {
                                        break;
                                    };
                                    let at = start + rel;
                                    push_part(start, at);
                                    start = at + sep.len();
                                    splits += 1;
                                }
                            }
                        }
                        push_part(start, s.len());
                        true
                    }
                };
                if !ok {
                    return None;
                }
                let slice: &[(u32, u32)] = if spill.is_empty() { &stack[..count] } else { &spill };
                return self.py_strm_list(this, s_idx, slice, at, ascii);
            }
            (26, 1) | (26, 2) | (26, 3) if args.len() == 1 || args[1] == Value::NULL => {
                if !ascii {
                    return None;
                }
                let maxsplit: Option<usize> = if args.len() == 3 {
                    let m = self.bigint_i128(args[2])?;
                    if m < 0 { None } else { Some(usize::try_from(m).unwrap_or(usize::MAX)) }
                } else {
                    None
                };
                let mut parts: Vec<(u32, u32)> = Vec::new();
                {
                    let s = self.py_strm_bytes(s_idx);
                    let mut i = s.len();
                    let mut splits = 0usize;
                    while i > 0 {
                        while i > 0 && py_space(s[i - 1]) {
                            i -= 1;
                        }
                        if i == 0 {
                            break;
                        }
                        let end = i;
                        let start = if maxsplit.is_some_and(|m| splits >= m) {
                            i = 0;
                            0
                        } else {
                            while i > 0 && !py_space(s[i - 1]) {
                                i -= 1;
                            }
                            splits += 1;
                            i
                        };
                        parts.push((start as u32, end as u32));
                    }
                }
                parts.reverse();
                return self.py_strm_list(this, s_idx, &parts, at, ascii);
            }
            (26, 2) | (26, 3) => {
                let (sep_idx, _) = self.py_strm_flat(args[1])?;
                let maxsplit: Option<usize> = if args.len() == 3 {
                    let m = self.bigint_i128(args[2])?;
                    if m < 0 { None } else { Some(usize::try_from(m).unwrap_or(usize::MAX)) }
                } else {
                    None
                };
                let mut parts: Vec<(u32, u32)> = Vec::new();
                let ok = {
                    let s = self.py_strm_bytes(s_idx);
                    let sep = self.py_strm_bytes(sep_idx);
                    if sep.is_empty() {
                        false
                    } else {
                        let mut end = s.len();
                        let mut splits = 0usize;
                        let limit = maxsplit.unwrap_or(usize::MAX);
                        if limit > 0 {
                            if sep.len() == 1 {
                                let needle = sep[0];
                                while splits < limit {
                                    let Some(pos) = memchr::memrchr(needle, &s[..end]) else {
                                        break;
                                    };
                                    parts.push(((pos + 1) as u32, end as u32));
                                    end = pos;
                                    splits += 1;
                                }
                            } else {
                                let finder = memchr::memmem::FinderRev::new(sep);
                                while splits < limit {
                                    let Some(pos) = finder.rfind(&s[..end]) else {
                                        break;
                                    };
                                    parts.push(((pos + sep.len()) as u32, end as u32));
                                    end = pos;
                                    splits += 1;
                                }
                            }
                        }
                        parts.push((0, end as u32));
                        true
                    }
                };
                if !ok {
                    return None;
                }
                parts.reverse();
                return self.py_strm_list(this, s_idx, &parts, at, ascii);
            }
            (27, 1) | (27, 2) => {
                if !ascii {
                    return None;
                }
                let keepends = match args.get(1).copied() {
                    None => false,
                    Some(v) if v == Value::FALSE || v == Value::NULL => false,
                    Some(v) if v == Value::TRUE => true,
                    Some(v) => match self.bigint_i128(v) {
                        Some(0) => false,
                        Some(_) => true,
                        None => return None,
                    },
                };
                let mut parts: Vec<(u32, u32)> = Vec::new();
                {
                    let s = self.py_strm_bytes(s_idx);
                    let mut i = 0usize;
                    let len = s.len();
                    while i < len {
                        let start = i;
                        while i < len
                            && !matches!(
                                s[i],
                                b'\n' | b'\r' | 0x0b | 0x0c | 0x1c | 0x1d | 0x1e
                            )
                        {
                            i += 1;
                        }
                        let line_end = i;
                        if i < len {
                            if s[i] == b'\r' && i + 1 < len && s[i + 1] == b'\n' {
                                i += 2;
                            } else {
                                i += 1;
                            }
                        }
                        parts.push((start as u32, (if keepends { i } else { line_end }) as u32));
                    }
                }
                return self.py_strm_list(this, s_idx, &parts, at, true);
            }
            (5, 3) | (5, 4) => {
                let (old_idx, _) = self.py_strm_flat(args[1])?;
                let (new_idx, new_ascii) = self.py_strm_flat(args[2])?;
                let limit: usize = if args.len() == 4 {
                    let c = self.bigint_i128(args[3])?;
                    if c == 0 {
                        let units = match self.heap.get(s_idx) {
                            HeapObj::Str(st) => st.units(),
                            _ => return None,
                        };
                        return (units <= STRM_MAX_TEXT).then_some(args[0]);
                    }
                    if c < 0 { usize::MAX } else { usize::try_from(c).unwrap_or(usize::MAX) }
                } else {
                    usize::MAX
                };
                let mut out = self.heap.take_byte_buf(n as usize);
                let replaced = {
                    let s = self.py_strm_bytes(s_idx);
                    let old = self.py_strm_bytes(old_idx);
                    let new = self.py_strm_bytes(new_idx);
                    if old.is_empty() {
                        return None;
                    }
                    if old.len() == 1 {
                        let needle = old[0];
                        if let Some(first) = memchr::memchr(needle, s) {
                            if new.len() == 1 && limit == usize::MAX {
                                out.reserve(s.len());
                                out.extend_from_slice(s);
                                let repl = new[0];
                                out[first] = repl;
                                let mut pos = first + 1;
                                while let Some(rel) = memchr::memchr(needle, &out[pos..]) {
                                    pos += rel;
                                    out[pos] = repl;
                                    pos += 1;
                                }
                            } else {
                                out.reserve(s.len());
                                let mut last = 0;
                                let mut at = first;
                                let mut done = 0usize;
                                loop {
                                    out.extend_from_slice(&s[last..at]);
                                    out.extend_from_slice(new);
                                    last = at + 1;
                                    done += 1;
                                    if done >= limit {
                                        break;
                                    }
                                    match memchr::memchr(needle, &s[last..]) {
                                        Some(rel) => at = last + rel,
                                        None => break,
                                    }
                                }
                                out.extend_from_slice(&s[last..]);
                            }
                            true
                        } else {
                            false
                        }
                    } else {
                        let finder = memchr::memmem::Finder::new(old);
                        if let Some(first) = finder.find(s) {
                            out.reserve(s.len());
                            let mut last = 0;
                            let mut at = first;
                            let mut done = 0usize;
                            loop {
                                out.extend_from_slice(&s[last..at]);
                                out.extend_from_slice(new);
                                last = at + old.len();
                                done += 1;
                                if done >= limit {
                                    break;
                                }
                                match finder.find(&s[last..]) {
                                    Some(rel) => at = last + rel,
                                    None => break,
                                }
                            }
                            out.extend_from_slice(&s[last..]);
                            true
                        } else {
                            false
                        }
                    }
                };
                if !replaced {
                    self.heap.return_byte_buf(out);
                    // Still `rt.checkedText`'s limit, as the copy was checked.
                    let units = match self.heap.get(s_idx) {
                        HeapObj::Str(st) => st.units(),
                        _ => return None,
                    };
                    return (units <= STRM_MAX_TEXT).then_some(args[0]);
                }
                self.py_strm_new_as(out, ascii && new_ascii)
            }
            (6, 2) | (25, 2) => {
                let (sub_idx, _) = self.py_strm_flat(args[1])?;
                // The match position, as a count of code points before it.
                let (found, scanned) = {
                    let s = self.py_strm_bytes(s_idx);
                    let sub = self.py_strm_bytes(sub_idx);
                    let at = if sub.is_empty() {
                        Some(0)
                    } else if sub.len() == 1 {
                        memchr::memchr(sub[0], s)
                    } else if sub.len() > s.len() {
                        None
                    } else {
                        memchr::memmem::find(s, sub)
                    };
                    let scanned = at.map_or(s.len(), |i| i + sub.len());
                    (at.map(|i| if ascii { i } else { s[..i].iter().filter(|&&b| b & 0xC0 != 0x80).count() }), scanned)
                };
                self.charge_steps(i64::try_from(scanned).unwrap_or(i64::MAX));
                return match found {
                    None if op == 25 => None,
                    None => Some(self.make_bigint(-1)),
                    Some(cps) => Some(self.make_bigint(cps as i128)),
                };
            }
            (6, 3) | (6, 4) | (25, 3) | (25, 4) => {
                let (sub_idx, _) = self.py_strm_flat(args[1])?;
                let Some((ba, bb, cp_lo)) = self.py_strm_bmp_bounds(s_idx, args)? else {
                    return if op == 25 { None } else { Some(self.make_bigint(-1)) };
                };
                let (found, scanned) = {
                    let win = &self.py_strm_bytes(s_idx)[ba..bb];
                    let sub = self.py_strm_bytes(sub_idx);
                    let at = if sub.is_empty() {
                        Some(0)
                    } else if sub.len() == 1 {
                        memchr::memchr(sub[0], win)
                    } else if sub.len() > win.len() {
                        None
                    } else {
                        memchr::memmem::find(win, sub)
                    };
                    let scanned = at.map_or(win.len(), |i| i + sub.len());
                    (
                        at.map(|i| {
                            if ascii {
                                cp_lo + i
                            } else {
                                cp_lo + win[..i].iter().filter(|&&b| b & 0xC0 != 0x80).count()
                            }
                        }),
                        scanned,
                    )
                };
                self.charge_steps(i64::try_from(scanned).unwrap_or(i64::MAX));
                return match found {
                    None if op == 25 => None,
                    None => Some(self.make_bigint(-1)),
                    Some(pos) => Some(self.make_bigint(pos as i128)),
                };
            }
            (22, 2) | (24, 2) => {
                let (sub_idx, _) = self.py_strm_flat(args[1])?;
                let (found, scanned) = {
                    let s = self.py_strm_bytes(s_idx);
                    let sub = self.py_strm_bytes(sub_idx);
                    let at = if sub.is_empty() {
                        Some(s.len())
                    } else if sub.len() == 1 {
                        memchr::memrchr(sub[0], s)
                    } else if sub.len() > s.len() {
                        None
                    } else {
                        memchr::memmem::rfind(s, sub)
                    };
                    let scanned = at.map_or(s.len(), |i| s.len() - i);
                    (at.map(|i| if ascii { i } else { s[..i].iter().filter(|&&b| b & 0xC0 != 0x80).count() }), scanned)
                };
                self.charge_steps(i64::try_from(scanned).unwrap_or(i64::MAX));
                return match found {
                    None if op == 24 => None,
                    None => Some(self.make_bigint(-1)),
                    Some(cps) => Some(self.make_bigint(cps as i128)),
                };
            }
            (22, 3) | (22, 4) | (24, 3) | (24, 4) => {
                let (sub_idx, _) = self.py_strm_flat(args[1])?;
                let Some((ba, bb, cp_lo)) = self.py_strm_bmp_bounds(s_idx, args)? else {
                    return if op == 24 { None } else { Some(self.make_bigint(-1)) };
                };
                let (found, scanned) = {
                    let win = &self.py_strm_bytes(s_idx)[ba..bb];
                    let sub = self.py_strm_bytes(sub_idx);
                    let at = if sub.is_empty() {
                        Some(win.len())
                    } else if sub.len() == 1 {
                        memchr::memrchr(sub[0], win)
                    } else if sub.len() > win.len() {
                        None
                    } else {
                        memchr::memmem::rfind(win, sub)
                    };
                    let scanned = at.map_or(win.len(), |i| win.len() - i);
                    (
                        at.map(|i| {
                            if ascii {
                                cp_lo + i
                            } else {
                                cp_lo + win[..i].iter().filter(|&&b| b & 0xC0 != 0x80).count()
                            }
                        }),
                        scanned,
                    )
                };
                self.charge_steps(i64::try_from(scanned).unwrap_or(i64::MAX));
                return match found {
                    None if op == 24 => None,
                    None => Some(self.make_bigint(-1)),
                    Some(pos) => Some(self.make_bigint(pos as i128)),
                };
            }
            (23, 2) => {
                let (sub_idx, _) = self.py_strm_flat(args[1])?;
                let (cnt, scanned) = {
                    let s = self.py_strm_bytes(s_idx);
                    let sub = self.py_strm_bytes(sub_idx);
                    let c = if sub.is_empty() {
                        if ascii { s.len() + 1 } else { s.iter().filter(|&&b| b & 0xC0 != 0x80).count() + 1 }
                    } else if sub.len() == 1 {
                        memchr::memchr_iter(sub[0], s).count()
                    } else if sub.len() > s.len() {
                        0
                    } else {
                        memchr::memmem::find_iter(s, sub).count()
                    };
                    (c, s.len())
                };
                self.charge_steps(i64::try_from(scanned).unwrap_or(i64::MAX));
                return Some(self.make_bigint(cnt as i128));
            }
            (23, 3) | (23, 4) => {
                let (sub_idx, _) = self.py_strm_flat(args[1])?;
                let Some((ba, bb, _)) = self.py_strm_bmp_bounds(s_idx, args)? else {
                    return Some(self.make_bigint(0));
                };
                let (cnt, scanned) = {
                    let win = &self.py_strm_bytes(s_idx)[ba..bb];
                    let sub = self.py_strm_bytes(sub_idx);
                    let c = if sub.is_empty() {
                        if ascii {
                            win.len() + 1
                        } else {
                            win.iter().filter(|&&b| b & 0xC0 != 0x80).count() + 1
                        }
                    } else if sub.len() == 1 {
                        memchr::memchr_iter(sub[0], win).count()
                    } else if sub.len() > win.len() {
                        0
                    } else {
                        memchr::memmem::find_iter(win, sub).count()
                    };
                    (c, win.len())
                };
                self.charge_steps(i64::try_from(scanned).unwrap_or(i64::MAX));
                return Some(self.make_bigint(cnt as i128));
            }
            (36, 2..=9) => {
                // Direct `sep.join([e1, ..., ek])` (`R.SJOIN_DIRECT`): `args[0]`
                // is `sep` (`s_idx`), `args[1..]` are the string elements.
                let mut part_idxs = [0u32; 8];
                let count = args.len() - 1;
                let sep_len = self.py_strm_bytes(s_idx).len();
                let mut total = 0usize;
                let mut all_ascii = ascii;
                for i in 0..count {
                    let (pidx, pascii) = self.py_strm_flat(args[1 + i])?;
                    part_idxs[i] = pidx;
                    all_ascii &= pascii;
                    total += self.py_strm_bytes(pidx).len() + if i > 0 { sep_len } else { 0 };
                    if total > STRM_MAX_TEXT * 3 {
                        return None;
                    }
                }
                let mut out = self.heap.take_byte_buf(total);
                {
                    let sep = self.py_strm_bytes(s_idx);
                    for (i, &pidx) in part_idxs[..count].iter().enumerate() {
                        if i > 0 {
                            out.extend_from_slice(sep);
                        }
                        out.extend_from_slice(self.py_strm_bytes(pidx));
                    }
                }
                self.py_strm_new_as(out, all_ascii)
            }
            (7, 2) => {
                let items = args[1];
                if !items.is_heap() {
                    return None;
                }
                let (list_cls, ttype) = match self.py_rt.as_deref() {
                    Some(p) if p.t_list.is_heap() && p.t_tuple.is_heap() => (p.t_list, p.t_tuple),
                    _ => {
                        let (tmpl, ttype) = self.py_strm_seq_types(this, at)?;
                        let (list_cls, _) = self.py_seq_parts(tmpl)?;
                        (list_cls, ttype)
                    }
                };
                let (cls, arr) = self.py_seq_parts(items)?;
                if cls.bits() != list_cls.bits() && cls.bits() != ttype.bits() {
                    return None;
                }
                if !arr.is_heap() {
                    return None;
                }
                let arr_idx = arr.heap_index();
                let count = match self.heap.get(arr_idx) {
                    HeapObj::Array(parts) => parts.len(),
                    _ => return None,
                };
                // First every part is proved a str and flattened where it
                // lives, and the joined size is known; then one buffer takes
                // them all. (A flatten rewrites only that part's own slot.)
                let sep_len = self.py_strm_bytes(s_idx).len();
                let mut total = 0usize;
                let mut all_ascii = ascii;
                for i in 0..count {
                    let part = match self.heap.get(arr_idx) {
                        HeapObj::Array(parts) => *parts.get(i)?,
                        _ => return None,
                    };
                    let (part_idx, part_ascii) = self.py_strm_flat(part)?;
                    all_ascii &= part_ascii;
                    total += self.py_strm_bytes(part_idx).len() + if i > 0 { sep_len } else { 0 };
                    if total > STRM_MAX_TEXT * 3 {
                        return None;
                    }
                }
                let mut out = self.heap.take_byte_buf(total);
                {
                    let sep = self.py_strm_bytes(s_idx);
                    let HeapObj::Array(parts) = self.heap.get(arr_idx) else {
                        return None;
                    };
                    for (i, part) in parts.iter().enumerate() {
                        if i > 0 {
                            out.extend_from_slice(sep);
                        }
                        out.extend_from_slice(self.py_strm_bytes(part.heap_index()));
                    }
                }
                self.py_strm_new_as(out, all_ascii)
            }
            (8, 1) | (9, 1) => {
                if !ascii {
                    return None;
                }
                let lower = op == 8;
                let changes = {
                    let s = self.py_strm_bytes(s_idx);
                    if lower {
                        s.iter().any(u8::is_ascii_uppercase)
                    } else {
                        s.iter().any(u8::is_ascii_lowercase)
                    }
                };
                if !changes {
                    return Some(args[0]);
                }
                let len = n as usize;
                if len <= crate::heap::STR_INLINE_CAP {
                    let mut stack = [0u8; 24];
                    stack[..len].copy_from_slice(self.py_strm_bytes(s_idx));
                    if lower {
                        stack[..len].make_ascii_lowercase();
                    } else {
                        stack[..len].make_ascii_uppercase();
                    }
                    if len == 1 {
                        Value::heap(stack[0] as u32)
                    } else {
                        Value::heap(
                            self.heap
                                .alloc_str_settled(JsStr::from_ascii_slice(&stack[..len])),
                        )
                    }
                } else {
                    let mut bytes = self.py_strm_copy(s_idx, 0, len);
                    if lower {
                        bytes.make_ascii_lowercase();
                    } else {
                        bytes.make_ascii_uppercase();
                    }
                    self.py_strm_new_as(bytes, true)
                }
            }
            (21, 1) => {
                if !ascii {
                    return None;
                }
                if n == 0 {
                    return Some(args[0]);
                }
                let changes = {
                    let s = self.py_strm_bytes(s_idx);
                    s[0].is_ascii_lowercase() || s[1..].iter().any(u8::is_ascii_uppercase)
                };
                if !changes {
                    return Some(args[0]);
                }
                let mut bytes = self.py_strm_copy(s_idx, 0, n as usize);
                bytes[0].make_ascii_uppercase();
                bytes[1..].make_ascii_lowercase();
                self.py_strm_new_as(bytes, true)
            }
            // ASCII character-classification predicates (non-ASCII falls back
            // to the full Unicode regexes in `builtins.js`).
            (28..=33, 1) => {
                if !ascii {
                    return None;
                }
                let res = {
                    let b = self.py_strm_bytes(s_idx);
                    match op {
                        28 => !b.is_empty() && b.iter().all(|&c| py_space(c)),
                        29 => !b.is_empty() && b.iter().all(u8::is_ascii_digit),
                        30 => !b.is_empty() && b.iter().all(u8::is_ascii_alphabetic),
                        31 => !b.is_empty() && b.iter().all(u8::is_ascii_alphanumeric),
                        32 => {
                            let mut has_lower = false;
                            let mut ok = true;
                            for &c in b {
                                if c.is_ascii_uppercase() {
                                    ok = false;
                                    break;
                                }
                                if c.is_ascii_lowercase() {
                                    has_lower = true;
                                }
                            }
                            ok && has_lower
                        }
                        _ => {
                            let mut has_upper = false;
                            let mut ok = true;
                            for &c in b {
                                if c.is_ascii_lowercase() {
                                    ok = false;
                                    break;
                                }
                                if c.is_ascii_uppercase() {
                                    has_upper = true;
                                }
                            }
                            ok && has_upper
                        }
                    }
                };
                self.charge_steps(i64::try_from(n).unwrap_or(i64::MAX));
                return Some(Value::bool(res));
            }
            _ => return None,
        };
        // `rt.checkedText`: the runtime's text limit, in UTF-16 units.
        if let HeapObj::Str(st) = self.heap.get(out.heap_index()) {
            if st.units() > STRM_MAX_TEXT {
                return None;
            }
        }
        if self.instr_rec.is_some() {
            self.charge_steps(i64::try_from(n).unwrap_or(i64::MAX));
        }
        Some(out)
    }

    /// The builtin record `this`'s `ltmpl` (an empty list) and `ttype` (the
    /// tuple type), looked for right after its `strop` at `at`.
    #[inline]
    fn py_strm_seq_types(&self, this: Value, at: usize) -> Option<(Value, Value)> {
        let idx = this.heap_index();
        let tmpl = self.py_strm_prop_at(idx, at.saturating_add(1), hint::LTMPL, "ltmpl")?;
        let ttype = self.py_strm_prop_at(idx, at.saturating_add(2), hint::TTYPE, "ttype")?;
        Some((tmpl, ttype))
    }

    /// A list of the strs that are `s_idx`'s bytes at `parts` (pieces of an
    /// ASCII str when `ascii`), made from the record's `ltmpl` (an empty
    /// list).
    fn py_strm_list(&mut self, this: Value, s_idx: u32, parts: &[(u32, u32)], at: usize, ascii: bool) -> Option<Value> {
        if parts.len() > (1 << 24) {
            return None;
        }
        // Fast path: read the runtime's cached `(seq_tmpl, seq_shape, t_list)`
        // directly instead of probing `ltmpl` on the builtin record every call.
        let (tmpl, seq_shape, like) = match self.py_rt.as_deref() {
            Some(p)
                if p.seq_shape != crate::shape::DICT
                    && p.t_list.is_heap()
                    && ((p.seq_tmpl_hver != u32::MAX
                        && p.seq_tmpl.is_heap()
                        && self.heap.version_of(p.seq_tmpl.heap_index()) == p.seq_tmpl_hver)
                        || self.py_alloc_like_ok(p.seq_tmpl, p.seq_shape)) =>
            {
                (p.seq_tmpl, p.seq_shape, Some(p.t_list))
            }
            _ => {
                let tmpl = self.py_strm_prop_at(this.heap_index(), at.saturating_add(1), hint::LTMPL, "ltmpl")?;
                let seq_shape = self.py_rt.as_deref().map_or(crate::shape::DICT, |p| p.seq_shape);
                let like = if self.py_alloc_like_ok(tmpl, seq_shape) {
                    self.py_seq_parts(tmpl).map(|(cls, _)| cls)
                } else {
                    None
                };
                (tmpl, seq_shape, like)
            }
        };
        let copy = match like {
            Some(_) => None,
            None => Some(self.py_json_template(tmpl, &["cls", "items"])?),
        };
        let total: u64 = if self.instr_rec.is_some() {
            parts.iter().map(|&(a, b)| (b - a) as u64).sum()
        } else {
            0
        };
        let mut items = self.heap.take_arr_buf(parts.len());
        for &(a32, b32) in parts {
            let (a, b) = (a32 as usize, b32 as usize);
            let len = b - a;
            let v = if len == 0 {
                Value::heap(crate::heap::INTERN_EMPTY)
            } else if ascii && len == 1 {
                Value::heap(self.py_strm_bytes(s_idx)[a] as u32)
            } else {
                let bytes = self.py_strm_copy(s_idx, a, b);
                if ascii {
                    Value::heap(self.heap.alloc_str_settled(JsStr::from_ascii(bytes)))
                } else {
                    self.py_strm_new(bytes)
                }
            };
            items.push(v);
        }
        let arr_idx = self.heap.alloc_array_settled(items);
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        self.heap.repair_arr_mirror(arr_idx);
        let arr = Value::heap(arr_idx);
        let list = match (like, copy) {
            (Some(cls), _) => {
                if self.py_rt.as_deref().is_some_and(|p| p.seq_tmpl.bits() == tmpl.bits()) {
                    self.py_alloc_seq_fast(cls, arr)?
                } else {
                    self.py_alloc_like_ready(tmpl, seq_shape, &[cls, arr])?
                }
            }
            (None, Some((mut rec, slot))) => {
                rec.set_val_at(slot, arr);
                self.alloc_object_current_realm(rec)
            }
            (None, None) => return None,
        };
        if self.instr_rec.is_some() {
            self.charge_steps(i64::try_from(total).unwrap_or(i64::MAX));
        }
        Some(list)
    }

    /// Ultra-fast 2-part f-string join (`f"prefix{int}"`) called directly from
    /// `jit_py_strm_alloc` without entering the full `py_strm_fast_op` match.
    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
    #[inline]
    pub(crate) fn py_fstr_join2(&mut self, a0: Value, a1: Value) -> Option<Value> {
        if let Some(n) = a1.small_bigint_val() {
            let (idx, a_ascii) = self.py_strm_flat(a0)?;
            let slen = self.py_strm_bytes(idx).len();
            let total = slen + 20;
            if a_ascii && total <= STRM_MAX_TEXT {
                let mut out = self.heap.take_byte_buf(total);
                out.extend_from_slice(self.py_strm_bytes(idx));
                push_decimal_i64(&mut out, n);
                let len = out.len();
                let v = self.py_strm_new_as(out, true);
                if self.instr_rec.is_some() {
                    self.charge_steps(i64::try_from(len).unwrap_or(i64::MAX));
                }
                return Some(v);
            }
        }
        self.py_fstr_join(&[a0, a1])
    }

    /// `R.FJOIN.c<n>(...parts)` (op 20): what its `j<n>` answers (`out +=
    /// part` for each part, from "", then `rt.checkedText`) when every part
    /// is a str with no lone surrogate (two could pair up across a join) or
    /// an int of at most 128 bits (its decimal digits, which JavaScript's
    /// ToString of a BigInt and Python's `str` agree on): the text in one
    /// allocation, where the f-string's chain of `Add`s made the digits and
    /// then each joined text. `None` (the `j<n>` path) for anything else,
    /// and for a result past the runtime's text limit, which `j<n>` raises.
    fn py_fstr_join(&mut self, args: &[Value]) -> Option<Value> {
        // Fast single-pass path for 2-part f-strings like `f"item{i}"`.
        if let [a0, a1] = *args {
            if let Some(n) = a1.small_bigint_val() {
                let (idx, a_ascii) = self.py_strm_flat(a0)?;
                let slen = self.py_strm_bytes(idx).len();
                let total = slen + 20;
                if a_ascii && total <= STRM_MAX_TEXT {
                    let mut out = self.heap.take_byte_buf(total);
                    out.extend_from_slice(self.py_strm_bytes(idx));
                    push_decimal_i64(&mut out, n);
                    let len = out.len();
                    let v = self.py_strm_new_as(out, true);
                    if self.instr_rec.is_some() {
                        self.charge_steps(i64::try_from(len).unwrap_or(i64::MAX));
                    }
                    return Some(v);
                }
            }
        }
        // One str: the str itself (`"" + s`).
        if let [one] = *args {
            if one.is_heap() && self.heap.is_str_like(one.heap_index()) {
                let idx = one.heap_index();
                self.heap.flatten(idx);
                return match self.heap.get(idx) {
                    HeapObj::Str(s) if s.units() <= STRM_MAX_TEXT => Some(one),
                    _ => None,
                };
            }
        }
        // First every str part is flattened where it lives and the size is
        // known (an int's digits counted generously); then one buffer.
        let mut total = 0usize;
        let mut ascii = true;
        for &a in args {
            if a.small_bigint_val().is_some() {
                total += 20;
                continue;
            }
            if a.is_heap() && matches!(self.heap.get(a.heap_index()), HeapObj::BigInt(_)) {
                total += 40;
                continue;
            }
            let (idx, a_ascii) = self.py_strm_flat(a)?;
            ascii &= a_ascii;
            total += self.py_strm_bytes(idx).len();
            if total > STRM_MAX_TEXT * 3 {
                return None;
            }
        }
        let mut out = self.heap.take_byte_buf(total);
        for &a in args {
            if let Some(n) = a.small_bigint_val() {
                push_decimal(&mut out, n as i128);
                continue;
            }
            match self.heap.get(a.heap_index()) {
                HeapObj::BigInt(n) => push_decimal(&mut out, *n),
                HeapObj::Str(s) => out.extend_from_slice(s.as_bytes()),
                _ => return None,
            }
        }
        let n = out.len();
        if ascii && n > STRM_MAX_TEXT {
            return None;
        }
        let v = self.py_strm_new_as(out, ascii);
        if !ascii {
            if let HeapObj::Str(st) = self.heap.get(v.heap_index()) {
                if st.units() > STRM_MAX_TEXT {
                    return None;
                }
            }
        }
        self.charge_steps(i64::try_from(n).unwrap_or(i64::MAX));
        Some(v)
    }
}

/// Two-digit lookup table for fast decimal formatting (`00..99`).
const DEC_PAIRS: &[u8; 200] = b"00010203040506070809\
10111213141516171819\
20212223242526272829\
30313233343536373839\
40414243444546474849\
50515253545556575859\
60616263646566676869\
70717273747576777879\
80818283848586878889\
90919293949596979899";

/// Fast decimal formatting for 64-bit signed ints (covers every immediate
/// small BigInt without `u128` division).
#[inline]
fn push_decimal_i64(out: &mut Vec<u8>, n: i64) {
    let mut m = n.unsigned_abs();
    if m < 10 {
        if n < 0 {
            out.push(b'-');
        }
        out.push(b'0' + m as u8);
        return;
    }
    if m < 100 && n >= 0 {
        let p = (m as usize) * 2;
        out.extend_from_slice(&DEC_PAIRS[p..p + 2]);
        return;
    }
    let mut buf = [0u8; 24];
    let mut i = buf.len();
    while m >= 100 {
        let rem = (m % 100) as usize * 2;
        m /= 100;
        i -= 2;
        buf[i] = DEC_PAIRS[rem];
        buf[i + 1] = DEC_PAIRS[rem + 1];
    }
    if m >= 10 {
        let rem = (m as usize) * 2;
        i -= 2;
        buf[i] = DEC_PAIRS[rem];
        buf[i + 1] = DEC_PAIRS[rem + 1];
    } else {
        i -= 1;
        buf[i] = b'0' + m as u8;
    }
    if n < 0 {
        i -= 1;
        buf[i] = b'-';
    }
    out.extend_from_slice(&buf[i..]);
}

/// The decimal digits of `n` (a `-` first when negative), appended.
fn push_decimal(out: &mut Vec<u8>, n: i128) {
    if let Ok(n64) = i64::try_from(n) {
        push_decimal_i64(out, n64);
        return;
    }
    let mut buf = [0u8; 40];
    let mut i = buf.len();
    let mut m = n.unsigned_abs();
    // The digits beyond 64 bits in 128-bit steps, the rest in 64-bit ones.
    while m > u64::MAX as u128 {
        i -= 1;
        buf[i] = b'0' + (m % 10) as u8;
        m /= 10;
    }
    let mut m = m as u64;
    loop {
        i -= 1;
        buf[i] = b'0' + (m % 10) as u8;
        m /= 10;
        if m == 0 {
            break;
        }
    }
    if n < 0 {
        i -= 1;
        buf[i] = b'-';
    }
    out.extend_from_slice(&buf[i..]);
}

// ---- heapq (`__zipp_py_strm` ops 12 and 13) ------------------------------
//
// `heapq.heappush(heap, item)` (op 12) and `heapq.heappop(heap)` (op 13) on
// an exact list whose items (and `item`) the engine orders itself (ints,
// floats, strs, bools, None and tuples of those, as `__zipp_py_ord` op 2
// reads them): the runtime's sift, comparison for comparison (`lt` is
// Python's `<`), with the list changed only once every comparison has been
// answered. Each pair is compared where it lives (`Vm::py_ord_lt_values`),
// two tuples item by item up to the first pair that differs, which is all
// Python's `<` reads of them; no key is built. Anything else (a pair the
// engine does not order, an empty heap for a pop) takes the runtime's own
// entry (`j<n>`).
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
    fn py_seq_array(&self, this: Value, seq: Value, tuple_too: bool, at: usize) -> Option<(u32, Value)> {
        if !seq.is_heap() || !this.is_heap() {
            return None;
        }
        let (list_cls, ttype) = match self.py_rt.as_deref() {
            Some(p) if p.t_list.is_heap() && p.t_tuple.is_heap() => (p.t_list, p.t_tuple),
            _ => {
                let (tmpl, ttype) = self.py_strm_seq_types(this, at)?;
                let (list_cls, _) = self.py_seq_parts(tmpl)?;
                (list_cls, ttype)
            }
        };
        let (cls, arr) = self.py_seq_parts(seq)?;
        if cls.bits() != list_cls.bits() && !(tuple_too && cls.bits() == ttype.bits()) {
            return None;
        }
        if !arr.is_heap() || !matches!(self.heap.get(arr.heap_index()), HeapObj::Array(_)) {
            return None;
        }
        Some((arr.heap_index(), ttype))
    }

    /// `a[x] < a[y]` (`moving` standing for [`MOVING`]), when the two are
    /// ordered: compared where they live, as far as Python's `<` reads
    /// them (`Vm::py_ord_lt_values`).
    #[inline]
    fn py_heap_lt(&mut self, arr: u32, moving: Value, x: usize, y: usize, tuple: Value) -> Option<bool> {
        let a = if x == MOVING { moving } else { self.py_arr_at(arr, x)? };
        let b = if y == MOVING { moving } else { self.py_arr_at(arr, y)? };
        // Fast path 1: two immediate small ints (covers `bisect.insort` / int heaps).
        if let (Some(ia), Some(ib)) = (a.small_bigint_val(), b.small_bigint_val()) {
            if self.instr_rec.is_some() {
                self.charge_steps(8);
            }
            return Some(ia < ib);
        }
        // Fast path 2: two plain tuple records whose element 0 are immediate small ints
        // that differ (covers priority-queue tuples `(priority, ...)` such as k-way merge).
        if a.is_heap() && b.is_heap() {
            let seq_shape = self.py_rt.as_deref().map_or(crate::shape::DICT, |p| p.seq_shape);
            if seq_shape != crate::shape::DICT {
                let tbits = tuple.bits();
                let items_of = |v: Value| -> Option<u32> {
                    match self.heap.get(v.heap_index()) {
                        HeapObj::Object(m) if !m.is_ctor && m.shape() == seq_shape && m.val_at(0).bits() == tbits => {
                            let arr_v = m.val_at(1);
                            arr_v.is_heap().then(|| arr_v.heap_index())
                        }
                        _ => None,
                    }
                };
                if let (Some(ai), Some(bi)) = (items_of(a), items_of(b)) {
                    if let (HeapObj::Array(ax), HeapObj::Array(bx)) = (self.heap.get(ai), self.heap.get(bi)) {
                        if let (Some(&x0), Some(&y0)) = (ax.first(), bx.first()) {
                            if let (Some(ia), Some(ib)) = (x0.small_bigint_val(), y0.small_bigint_val()) {
                                if ia != ib {
                                    if self.instr_rec.is_some() {
                                        self.charge_steps(8);
                                    }
                                    return Some(ia < ib);
                                }
                                if let (Some(&x1), Some(&y1)) = (ax.get(1), bx.get(1)) {
                                    if let (Some(ia1), Some(ib1)) = (x1.small_bigint_val(), y1.small_bigint_val()) {
                                        if ia1 != ib1 {
                                            if self.instr_rec.is_some() {
                                                self.charge_steps(8);
                                            }
                                            return Some(ia1 < ib1);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        let lt = self.py_ord_lt_values(a, b, tuple)?;
        if self.instr_rec.is_some() {
            self.charge_steps(8);
        }
        Some(lt)
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
    fn py_heap_op(&mut self, this: Value, args: &[Value], push: bool, at: usize) -> Option<Value> {
        let (arr, tuple) = self.py_seq_array(this, *args.first()?, false, at)?;
        let n = match self.heap.get(arr) {
            HeapObj::Array(a) => a.len(),
            _ => return None,
        };
        if push {
            let item = *args.get(1)?;
            if n >= (1 << 24) {
                return None;
            }
            // `up`: the item rises past every parent it is below.
            let mut pos = n;
            let mut path = [0u32; 24];
            let mut plen = 0usize;
            while pos > 0 {
                let parent = (pos - 1) >> 1;
                if !self.py_heap_lt(arr, item, MOVING, parent, tuple)? {
                    break;
                }
                path[plen] = parent as u32;
                plen += 1;
                pos = parent;
            }
            self.py_arr_edit(arr, Some(item), |a| {
                a.push(item);
                let mut hole = n;
                for &parent in &path[..plen] {
                    let p = parent as usize;
                    a[hole] = a[p];
                    hole = p;
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
        let mut moves = [0u32; 24];
        let mut mlen = 0usize;
        let mut i = 0usize;
        if m > 0 {
            loop {
                let l = 2 * i + 1;
                let r = l + 1;
                let mut best = MOVING;
                if l < m && self.py_heap_lt(arr, last, l, best, tuple)? {
                    best = l;
                }
                if r < m && self.py_heap_lt(arr, last, r, best, tuple)? {
                    best = r;
                }
                if best == MOVING {
                    break;
                }
                moves[mlen] = best as u32;
                mlen += 1;
                i = best;
            }
        }
        self.py_arr_edit(arr, None, |a| {
            a.pop();
            if m > 0 {
                let mut hole = 0;
                for &child in &moves[..mlen] {
                    let c = child as usize;
                    a[hole] = a[c];
                    hole = c;
                }
                a[hole] = last;
            }
        })?;
        Some(top)
    }

    /// Ops 14 to 17; `None` sends the call to the runtime's entry.
    fn py_bisect_op(&mut self, this: Value, args: &[Value], op: i32, at: usize) -> Option<Value> {
        let insort = op >= 16;
        let right = op == 15 || op == 17;
        let (arr, tuple) = self.py_seq_array(this, args[0], !insort, at)?;
        let x = args[1];
        let n = match self.heap.get(arr) {
            HeapObj::Array(a) => a.len(),
            _ => return None,
        };
        let (mut lo, mut hi) = (0usize, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let go_left = if right {
                self.py_heap_lt(arr, x, MOVING, mid, tuple)?
            } else {
                !self.py_heap_lt(arr, x, mid, MOVING, tuple)?
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
            None => Err(Thrown::from_static("TypeError: iterator step on a non-iterator")),
        }
    }

    /// Native bulk drain for `drain(iter)` (`__zipp_py_iter.call(it, out)`):
    /// steps a `PY_ITER` iterator natively into the caller-supplied JS Array
    /// `out_val`.
    pub(crate) fn py_iter_drain(&mut self, this: Value, out_val: Value) -> Result<Value, Thrown> {
        if !out_val.is_heap() || !matches!(self.heap.get(out_val.heap_index()), HeapObj::Array(_)) {
            return Ok(Value::UNDEFINED);
        }
        let Some(stop) = self.py_rt.as_deref().map(|p| p.stop).filter(|v| !v.is_undefined()) else {
            return Ok(Value::UNDEFINED);
        };
        let out_idx = out_val.heap_index();
        loop {
            let v = self.py_iter_next(this)?;
            if v.bits() == stop.bits() {
                return Ok(out_val);
            }
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            let was_live = self.heap.arr_mirror_live(out_idx);
            if v.is_heap() {
                self.heap.write_barrier_val(out_idx, v);
            }
            if let HeapObj::Array(items) = self.heap.get_mut(out_idx) {
                items.push(v);
            }
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            if was_live {
                self.heap.repair_arr_mirror(out_idx);
            }
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
        let mut buf = self.heap.take_arr_buf(2);
        buf.push(a);
        buf.push(b);
        self.py_iter_tuple(rec, buf)
    }

    /// A tuple of `items` made from the record's template.
    fn py_iter_tuple(&mut self, rec: u32, items: Vec<Value>) -> Option<Value> {
        let tmpl = self.py_strm_prop(rec, hint::ITMPL, "tmpl")?;
        let ttype = self.py_strm_prop(rec, hint::ITTYPE, "ttype")?;
        if self.py_rt.as_deref().is_some_and(|p| p.seq_tmpl.bits() == tmpl.bits()) {
            let arr_idx = self.heap.alloc_array_settled(items);
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            self.heap.repair_arr_mirror(arr_idx);
            return self.py_alloc_seq_fast(ttype, Value::heap(arr_idx));
        }
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
        if kind == Value::int(5) {
            // `stringIter`: `a` is the primitive string, `i` is the UTF-16 code
            // unit index (which equals the UTF-8 byte index for ASCII). For
            // non-ASCII BMP strings, `b` caches the current UTF-8 byte offset so
            // stepping is O(1) while keeping `i` aligned with `strIterFallback`.
            if !a.is_heap() {
                return None;
            }
            let s_idx = a.heap_index();
            if !self.heap.is_str_like(s_idx) {
                return None;
            }
            self.heap.flatten(s_idx);
            let stop = self.py_rt.as_deref().map(|p| p.stop).filter(|v| !v.is_undefined())?;
            let (is_ascii, is_bmp, byte_len) = match self.heap.get(s_idx) {
                HeapObj::Str(st) if st.is_ascii() => (true, true, st.as_bytes().len()),
                HeapObj::Str(st) if st.is_wellformed() && !st.has_surrogate() => (false, true, st.as_bytes().len()),
                _ => return None,
            };
            if is_ascii {
                if pos >= byte_len {
                    return Some(stop);
                }
                let ch_byte = match self.heap.get(s_idx) {
                    HeapObj::Str(st) => st.as_bytes()[pos],
                    _ => return None,
                };
                let next_i = Value::int(i32::try_from(pos + 1).ok()?);
                self.py_iter_set(rec, hint::II, "i", next_i)?;
                return Some(Value::heap(ch_byte as u32));
            }
            if !is_bmp {
                return None;
            }
            let byte_off = if b == Value::NULL {
                if pos == 0 { 0usize } else { return None }
            } else if b.is_int() && b.as_int() >= 0 {
                b.as_int() as usize
            } else {
                return None;
            };
            if byte_off >= byte_len {
                return Some(stop);
            }
            let (cp, clen) = match self.heap.get(s_idx) {
                HeapObj::Str(st) => {
                    let bytes = st.as_bytes();
                    let first = bytes[byte_off];
                    if first < 128 {
                        (first as u32, 1usize)
                    } else {
                        let c = std::str::from_utf8(&bytes[byte_off..]).ok()?.chars().next()?;
                        (c as u32, c.len_utf8())
                    }
                }
                _ => return None,
            };
            let next_i = Value::int(i32::try_from(pos + 1).ok()?);
            let next_b = Value::int(i32::try_from(byte_off + clen).ok()?);
            let ch = if cp < 128 {
                Value::heap(cp)
            } else {
                Value::heap(self.heap.alloc_str_settled(JsStr::from_code_point(cp)))
            };
            self.py_iter_set(rec, hint::II, "i", next_i)?;
            self.py_iter_set(rec, hint::IB, "b", next_b)?;
            return Some(ch);
        }
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
            if !a.is_heap() {
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
            let next = self.bigint_i128(b)?.checked_add(1)?;
            let out = self.py_iter_pair(rec, b, v)?;
            let step = Value::int(i32::try_from(pos + 1).ok()?);
            let nb = self.make_bigint(next);
            self.py_iter_set(rec, hint::IB, "b", nb)?;
            self.py_iter_set(rec, hint::II, "i", step)?;
            return Some(out);
        }
        None
    }

    /// `__zipp_py_decode(items_or_u8, encoding, errors)`: fast native decode
    /// for UTF-8, ASCII, and Latin-1 byte buffers (`Uint8Array` zero-copy or
    /// `Array` of byte numbers). Returns `undefined` on invalid bytes or
    /// uncommon options so the runtime fallback raises the exact Python error.
    pub(crate) fn py_bytes_decode(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        let src = arg(0);
        if !src.is_heap() {
            return Ok(Value::UNDEFINED);
        }
        // Encoding & error mode: fast-path common exact ASCII spellings with zero allocation.
        let enc_kind: u8 = {
            if !arg(1).is_heap() || !self.heap.is_str_like(arg(1).heap_index()) {
                return Ok(Value::UNDEFINED);
            }
            let eidx = arg(1).heap_index();
            self.heap.flatten(eidx);
            let HeapObj::Str(es) = self.heap.get(eidx) else {
                return Ok(Value::UNDEFINED);
            };
            match es.as_bytes() {
                b"utf-8" | b"UTF-8" | b"utf8" | b"UTF8" | b"utf_8" => 0,
                b"ascii" | b"ASCII" => 1,
                b"latin-1" | b"latin1" | b"latin_1" | b"iso-8859-1" | b"ISO-8859-1" => 2,
                other => {
                    let Ok(s) = std::str::from_utf8(other) else {
                        return Ok(Value::UNDEFINED);
                    };
                    match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
                        "utf8" => 0,
                        "ascii" => 1,
                        "latin1" | "iso88591" => 2,
                        _ => return Ok(Value::UNDEFINED),
                    }
                }
            }
        };
        let err_kind: u8 = if arg(2) == Value::UNDEFINED {
            0
        } else {
            if !arg(2).is_heap() || !self.heap.is_str_like(arg(2).heap_index()) {
                return Ok(Value::UNDEFINED);
            }
            let midx = arg(2).heap_index();
            self.heap.flatten(midx);
            let HeapObj::Str(ms) = self.heap.get(midx) else {
                return Ok(Value::UNDEFINED);
            };
            match ms.as_bytes() {
                b"strict" => 0,
                b"replace" => 1,
                _ => return Ok(Value::UNDEFINED),
            }
        };
        let owned: Vec<u8>;
        let slice: &[u8] = match self.heap.get(src.heap_index()) {
            HeapObj::TypedArray {
                buffer,
                kind,
                byte_offset,
                length,
            } if *kind == 1 => {
                let off = *byte_offset;
                let len = *length;
                match self.heap.get(*buffer) {
                    HeapObj::ArrayBuffer { data, detached } if !*detached => {
                        match data.get(off..off + len) {
                            Some(s) => s,
                            None => return Ok(Value::UNDEFINED),
                        }
                    }
                    _ => return Ok(Value::UNDEFINED),
                }
            }
            HeapObj::Array(items) => {
                owned = items.iter().map(|v| v.as_f64() as u8).collect();
                &owned
            }
            _ => return Ok(Value::UNDEFINED),
        };
        let decoded: String = match (enc_kind, err_kind) {
            (0, 0) => match std::str::from_utf8(slice) {
                Ok(s) => s.to_owned(),
                Err(_) => return Ok(Value::UNDEFINED),
            },
            (0, 1) => String::from_utf8_lossy(slice).into_owned(),
            (1, 0) => {
                if slice.is_ascii() {
                    match std::str::from_utf8(slice) {
                        Ok(s) => s.to_owned(),
                        Err(_) => return Ok(Value::UNDEFINED),
                    }
                } else {
                    return Ok(Value::UNDEFINED);
                }
            }
            (2, _) => {
                if slice.is_ascii() {
                    match std::str::from_utf8(slice) {
                        Ok(s) => s.to_owned(),
                        Err(_) => return Ok(Value::UNDEFINED),
                    }
                } else {
                    let mut out = String::with_capacity(slice.len() + slice.len() / 4);
                    for &b in slice {
                        out.push(b as char);
                    }
                    out
                }
            }
            _ => return Ok(Value::UNDEFINED),
        };
        Ok(self.alloc_str(decoded))
    }

    /// `__zipp_py_re_findall(js_re, text, start_u16, tuple_tmpl)`: native
    /// `re.Pattern.findall` directly over `regress`, returning a JS `Array` of
    /// matched strings (0 or 1 capture group) or Python tuples (>=2 groups),
    /// or `undefined` to fall back to the JS scanner.
    pub(crate) fn py_re_findall(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        let re_val = arg(0);
        let text_val = arg(1);
        let start_val = arg(2);
        let tuple_tmpl = arg(3);
        if !re_val.is_heap() || !text_val.is_heap() || !self.heap.is_str_like(text_val.heap_index())
        {
            return Ok(Value::UNDEFINED);
        }
        let re_idx = re_val.heap_index();
        let unicode = match self.heap.get(re_idx) {
            HeapObj::RegExp(r) => r.flags.contains('u') || r.flags.contains('v'),
            _ => return Ok(Value::UNDEFINED),
        };
        let s_idx = text_val.heap_index();
        self.heap.flatten(s_idx);
        let start = if start_val.is_number() && start_val.as_f64() >= 0.0 {
            start_val.as_f64() as usize
        } else {
            0
        };
        let _gc = self.gc_lock_guard();
        let (is_ascii, ascii_units) = match self.heap.get(s_idx) {
            HeapObj::Str(js) => (js.is_ascii(), js.as_bytes().len()),
            _ => return Ok(Value::UNDEFINED),
        };
        let mut u16s: Vec<u16> = Vec::new();
        #[cfg(feature = "safe-sandbox")]
        let mut u16s_res = None;
        let matches: Vec<regress::Match> = if is_ascii {
            if start > ascii_units {
                return Ok(self.alloc_array_current_realm(Vec::new()));
            }
            self.ensure_regexp_ascii_twin_deferred(re_idx, ascii_units);
            #[cfg(feature = "safe-sandbox")]
            {
                let (limits, output_bytes) = self.instrument_regex_collection_limits();
                let subj = match self.heap.get(s_idx) {
                    HeapObj::Str(js) => js.as_str_wf(),
                    _ => "",
                };
                let regex = match self.heap.get(re_idx) {
                    HeapObj::RegExp(r) => match r.ascii_twin.get() {
                        Some(Some(twin)) => Some(&**twin),
                        _ => Some(&*r.regex),
                    },
                    _ => None,
                };
                let (collected, mut usage) = match regex {
                    Some(regex) => {
                        let mut iter = regex.find_from_ascii_with_limits(subj, start, limits);
                        let c = iter.try_collect_with_memory_limit(usize::MAX, output_bytes);
                        (c, iter.match_usage())
                    }
                    None => (Ok(Vec::new()), regress::MatchUsage::UNMETERED),
                };
                if let Err(err) = &collected {
                    usage.exhaustion.get_or_insert(*err);
                }
                self.instrument_regex_usage(usage)
                    .map_err(|m| Thrown(m.into()))?;
                collected.unwrap_or_default()
            }
            #[cfg(not(feature = "safe-sandbox"))]
            {
                let subj = match self.heap.get(s_idx) {
                    HeapObj::Str(js) => js.as_str_wf(),
                    _ => "",
                };
                match self.heap.get(re_idx) {
                    HeapObj::RegExp(r) => {
                        let regex = match r.ascii_twin.get() {
                            Some(Some(twin)) => &**twin,
                            _ => &*r.regex,
                        };
                        regex.find_from_ascii(subj, start).collect()
                    }
                    _ => Vec::new(),
                }
            }
        } else {
            #[cfg(feature = "safe-sandbox")]
            {
                let (units, res) = self.regex_subject_units_take(text_val)?;
                u16s = units;
                u16s_res = Some(res);
            }
            #[cfg(not(feature = "safe-sandbox"))]
            {
                u16s = self.regex_subject_units_take(text_val);
            }
            if start > u16s.len() {
                #[cfg(feature = "safe-sandbox")]
                self.regex_subject_units_put(text_val, u16s, u16s_res);
                #[cfg(not(feature = "safe-sandbox"))]
                self.regex_subject_units_put(text_val, u16s);
                return Ok(self.alloc_array_current_realm(Vec::new()));
            }
            #[cfg(feature = "safe-sandbox")]
            {
                let (limits, output_bytes) = self.instrument_regex_collection_limits();
                let (collected, mut usage) = match self.heap.get(re_idx) {
                    HeapObj::RegExp(r) => {
                        let regex = &r.core.regex;
                        if unicode {
                            let mut iter = regex.find_from_utf16_with_limits(&u16s, start, limits);
                            let c = iter.try_collect_with_memory_limit(usize::MAX, output_bytes);
                            (c, iter.match_usage())
                        } else {
                            let mut iter = regex.find_from_ucs2_with_limits(&u16s, start, limits);
                            let c = iter.try_collect_with_memory_limit(usize::MAX, output_bytes);
                            (c, iter.match_usage())
                        }
                    }
                    _ => (Ok(Vec::new()), regress::MatchUsage::UNMETERED),
                };
                if let Err(err) = &collected {
                    usage.exhaustion.get_or_insert(*err);
                }
                self.instrument_regex_usage(usage)
                    .map_err(|m| Thrown(m.into()))?;
                collected.unwrap_or_default()
            }
            #[cfg(not(feature = "safe-sandbox"))]
            {
                match self.heap.get(re_idx) {
                    HeapObj::RegExp(r) => {
                        let regex = &r.core.regex;
                        if unicode {
                            regex.find_from_utf16(&u16s, start).collect()
                        } else {
                            regex.find_from_ucs2(&u16s, start).collect()
                        }
                    }
                    _ => Vec::new(),
                }
            }
        };

        self.preflight_native_iteration_work(matches.len() as u64 + 1)?;
        if matches.is_empty() {
            return Ok(self.alloc_array_current_realm(Vec::new()));
        }
        let ngroups = matches[0].captures.len();
        let empty = self.alloc_str(String::new());
        let mut out: Vec<Value> = Vec::with_capacity(matches.len());
        if ngroups == 0 {
            for m in matches {
                let r = m.range();
                let v = if is_ascii {
                    self.ascii_slice_value(s_idx, r)
                } else {
                    self.units_value(&u16s[r])
                };
                out.push(v);
            }
        } else if ngroups == 1 {
            for m in matches {
                let v = match m.captures[0].clone() {
                    Some(r) => {
                        if is_ascii {
                            self.ascii_slice_value(s_idx, r)
                        } else {
                            self.units_value(&u16s[r])
                        }
                    }
                    None => empty,
                };
                out.push(v);
            }
        } else {
            let Some((tmpl_map, items_slot)) = self.py_json_template(tuple_tmpl, &["cls", "items"])
            else {
                return Ok(Value::UNDEFINED);
            };
            for m in matches {
                let mut tup_items = Vec::with_capacity(ngroups);
                for cap in &m.captures {
                    let v = match cap {
                        Some(r) => {
                            if is_ascii {
                                self.ascii_slice_value(s_idx, r.clone())
                            } else {
                                self.units_value(&u16s[r.clone()])
                            }
                        }
                        None => empty,
                    };
                    tup_items.push(v);
                }
                let arr = self.alloc_array_current_realm(tup_items);
                let mut obj = tmpl_map.clone();
                obj.set_val_at(items_slot, arr);
                out.push(self.alloc_object_current_realm(obj));
            }
        }
        if !is_ascii {
            #[cfg(feature = "safe-sandbox")]
            self.regex_subject_units_put(text_val, u16s, u16s_res);
            #[cfg(not(feature = "safe-sandbox"))]
            self.regex_subject_units_put(text_val, u16s);
        }
        Ok(self.alloc_array_current_realm(out))
    }

    /// Unwrap a primitive `str` or an unmodified `str` subclass (`pyval`) for
    /// native slice and `re` fast paths. Returns `(str_val, is_primitive)`.
    #[inline]
    fn py_unwrap_str_val(&self, v: Value) -> Option<(Value, bool)> {
        if !v.is_heap() {
            return None;
        }
        if self.heap.is_str_like(v.heap_index()) {
            return Some((v, true));
        }
        let cls = self.py_cls_of(v)?;
        if (self.py_sub_kind(cls) & super::py_rt::SUB_STR_GET) == 0 {
            return None;
        }
        let pyval = self.py_own_data(v.heap_index(), "pyval")?;
        if pyval.is_heap() && self.heap.is_str_like(pyval.heap_index()) {
            Some((pyval, false))
        } else {
            None
        }
    }

    /// `R.SLICEGET.c3(o, lo, hi)` / `.c4(o, lo, hi, step)` (op 37): direct
    /// slicing of `str`, unmodified `str` subclasses, `list`, and `tuple`
    /// without allocating a Python `slice` record.
    fn py_slice_get(&mut self, args: &[Value]) -> Option<Value> {
        let o = args[0];
        let lo_v = args[1];
        let hi_v = args[2];
        let step_v = args.get(3).copied().unwrap_or(Value::NULL);
        let step: i128 = if step_v == Value::NULL {
            1
        } else {
            let s = self.bigint_i128(step_v)?;
            if s != 1 && s != -1 {
                return None;
            }
            s
        };
        // 1. `str` or unmodified `str` subclass.
        if let Some((s_val, is_prim)) = self.py_unwrap_str_val(o) {
            let s_idx = s_val.heap_index();
            self.heap.flatten(s_idx);
            let (len, is_ascii) = match self.heap.get(s_idx) {
                HeapObj::Str(st) if st.is_ascii() => (st.as_bytes().len(), true),
                HeapObj::Str(st) if st.is_wellformed() && !st.has_surrogate() => (st.units(), false),
                _ => return None,
            };
            let n = len as i128;
            if step == 1 {
                let lo = if lo_v == Value::NULL {
                    0usize
                } else {
                    let i = self.bigint_i128(lo_v)?;
                    if i < 0 { (i + n).max(0) as usize } else { i.min(n) as usize }
                };
                let hi = if hi_v == Value::NULL {
                    len
                } else {
                    let i = self.bigint_i128(hi_v)?;
                    if i < 0 { (i + n).max(0) as usize } else { i.min(n) as usize }
                };
                if lo >= hi {
                    return Some(Value::heap(crate::heap::INTERN_EMPTY));
                }
                if lo == 0 && hi == len && is_prim {
                    return Some(o);
                }
                let slice_len = hi - lo;
                if slice_len > STRM_MAX_TEXT {
                    return None;
                }
                #[cfg(feature = "instrument")]
                if self.instr_rec.is_some() && slice_len > 1024 {
                    return None;
                }
                self.charge_steps(slice_len as i64);
                if is_ascii {
                    return Some(self.ascii_slice_value(s_idx, lo..hi));
                }
                let slice_js = {
                    let HeapObj::Str(st) = self.heap.get(s_idx) else {
                        return None;
                    };
                    let (ba, bb) = st.bmp_byte_range(lo, hi);
                    let b = &st.as_bytes()[ba..bb];
                    if b.len() == 1 && b[0] < 128 {
                        return Some(Value::heap(b[0] as u32));
                    }
                    JsStr::from_bmp_utf8_slice(b, slice_len)
                };
                return Some(Value::heap(self.heap.alloc_js(slice_js)));
            }
            // step == -1 (ASCII fast path)
            if !is_ascii {
                return None;
            }
            if len == 0 {
                return Some(Value::heap(crate::heap::INTERN_EMPTY));
            }
            let start = if lo_v == Value::NULL {
                n - 1
            } else {
                let i = self.bigint_i128(lo_v)?;
                if i < 0 { (i + n).max(-1) } else { i.min(n - 1) }
            };
            let stop = if hi_v == Value::NULL {
                -1
            } else {
                let i = self.bigint_i128(hi_v)?;
                if i < 0 { (i + n).max(-1) } else { i.min(n - 1) }
            };
            if start <= stop {
                return Some(Value::heap(crate::heap::INTERN_EMPTY));
            }
            let count = (start - stop) as usize;
            if count > STRM_MAX_TEXT {
                return None;
            }
            #[cfg(feature = "instrument")]
            if self.instr_rec.is_some() && count > 1024 {
                return None;
            }
            self.charge_steps(count as i64);
            let mut buf = self.heap.take_byte_buf(count);
            let src = &self.py_strm_bytes(s_idx)[(stop + 1) as usize..=(start as usize)];
            buf.extend(src.iter().rev().copied());
            return Some(self.py_strm_new_as(buf, true));
        }

        // 2. Exact `list` or `tuple`.
        let (t_list, t_tuple, seq_tmpl, seq_shape) = {
            let p = self.py_rt.as_deref()?;
            if p.seq_shape == crate::shape::DICT {
                return None;
            }
            (p.t_list, p.t_tuple, p.seq_tmpl, p.seq_shape)
        };
        let (cls, items) = self.py_seq_parts(o)?;
        if cls.bits() != t_list.bits() && cls.bits() != t_tuple.bits() {
            return None;
        }
        if !items.is_heap() {
            return None;
        }
        let items_idx = items.heap_index();
        let len = match self.heap.get(items_idx) {
            HeapObj::Array(a) => a.len(),
            _ => return None,
        };
        let n = len as i128;
        let out_vec: Vec<Value> = if step == 1 {
            let lo = if lo_v == Value::NULL {
                0usize
            } else {
                let i = self.bigint_i128(lo_v)?;
                if i < 0 { (i + n).max(0) as usize } else { i.min(n) as usize }
            };
            let hi = if hi_v == Value::NULL {
                len
            } else {
                let i = self.bigint_i128(hi_v)?;
                if i < 0 { (i + n).max(0) as usize } else { i.min(n) as usize }
            };
            if lo >= hi {
                Vec::new()
            } else {
                let count = hi - lo;
                #[cfg(feature = "instrument")]
                if self.instr_rec.is_some() && count > 1024 {
                    return None;
                }
                self.charge_steps(count as i64);
                let mut buf = self.heap.take_arr_buf(count);
                let ok = {
                    let HeapObj::Array(a) = self.heap.get(items_idx) else {
                        return None;
                    };
                    let slice = &a[lo..hi];
                    if slice.iter().any(|v| v.is_hole() || v.is_undefined()) {
                        false
                    } else {
                        buf.extend_from_slice(slice);
                        true
                    }
                };
                if !ok {
                    return None;
                }
                buf
            }
        } else {
            if len == 0 {
                Vec::new()
            } else {
                let start = if lo_v == Value::NULL {
                    n - 1
                } else {
                    let i = self.bigint_i128(lo_v)?;
                    if i < 0 { (i + n).max(-1) } else { i.min(n - 1) }
                };
                let stop = if hi_v == Value::NULL {
                    -1
                } else {
                    let i = self.bigint_i128(hi_v)?;
                    if i < 0 { (i + n).max(-1) } else { i.min(n - 1) }
                };
                if start <= stop {
                    Vec::new()
                } else {
                    let count = (start - stop) as usize;
                    #[cfg(feature = "instrument")]
                    if self.instr_rec.is_some() && count > 1024 {
                        return None;
                    }
                    self.charge_steps(count as i64);
                    let mut buf = self.heap.take_arr_buf(count);
                    let ok = {
                        let HeapObj::Array(a) = self.heap.get(items_idx) else {
                            return None;
                        };
                        let slice = &a[(stop + 1) as usize..=(start as usize)];
                        if slice.iter().any(|v| v.is_hole() || v.is_undefined()) {
                            false
                        } else {
                            buf.extend(slice.iter().rev().copied());
                            true
                        }
                    };
                    if !ok {
                        return None;
                    }
                    buf
                }
            }
        };
        if !self.py_alloc_like_ok(seq_tmpl, seq_shape) {
            return None;
        }
        let arr = self.alloc_array_current_realm(out_vec);
        self.py_alloc_like_ready(seq_tmpl, seq_shape, &[cls, arr])
    }

    /// Resolve the first argument of module-level `re.sub` / `re.search` /
    /// `re.match` / `re.fullmatch` (`ops 48..=51`): hits `this.strCache0` for
    /// a previously compiled `flags == 0` string pattern, or accepts an
    /// already-compiled `this.Pattern` object directly.
    fn py_re_resolve_pat(&mut self, this: Value, pat_arg: Value) -> Option<Value> {
        if !this.is_heap() || !pat_arg.is_heap() {
            return None;
        }
        let ti = this.heap_index();
        if self.heap.is_str_like(pat_arg.heap_index()) {
            self.heap.flatten(pat_arg.heap_index());
            let cache = self.py_own_data(ti, "strCache0")?;
            if !cache.is_heap() {
                return None;
            }
            return self.py_map_get(cache, pat_arg);
        }
        let pat_cls = self.py_own_data(ti, "Pattern")?;
        if pat_cls.is_heap() && self.py_cls_of(pat_arg).is_some_and(|c| c.bits() == pat_cls.bits()) {
            return Some(pat_arg);
        }
        None
    }

    /// Throwing / guest-reentrant `__zipp_py_strm` ops (`52..=54`):
    /// * `52`: `list()` / `list(x)` (fast copy for exact `list`/`tuple`, native
    ///   `py_gen_drain` for `PY_GEN` generators/iterators with GC rooting);
    /// * `53`: `next(it)` / `next(it, dflt)` (native `PY_GEN`/`PY_ITER` step,
    ///   invoking `this.stopExc(it)` on exhaustion to preserve `StopIteration.value`);
    /// * `54`: `generator.send(v)` (native `py_gen_send`, invoking `this.sendDone(g)`
    ///   on completion).
    ///
    /// Excluded from `PyStrmInline` (`py_jit_attr.rs`) because these ops may run
    /// guest code and throw; always dispatched via `jit_py_call` or `py_str_method_at`.
    pub(crate) fn py_strm_throwing_op(&mut self, this: Value, args: &[Value], op: i32) -> Result<Option<Value>, Thrown> {
        match op {
            52 | 55 => {
                // `52`: `list()` / `list(x)`; `55`: `tuple()` / `tuple(x)`
                let is_tuple = op == 55;
                let (t_list, t_tuple, stop) = match self.py_rt.as_deref() {
                    Some(p) if p.t_list.is_heap() && p.t_tuple.is_heap() && p.seq_shape != crate::shape::DICT => {
                        (p.t_list, p.t_tuple, p.stop)
                    }
                    _ => return Ok(None),
                };
                let target_cls = if is_tuple { t_tuple } else { t_list };
                if args.is_empty() {
                    let arr_idx = self.heap.alloc_array_settled(Vec::new());
                    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                    self.heap.repair_arr_mirror(arr_idx);
                    return Ok(self.py_alloc_seq_fast(target_cls, Value::heap(arr_idx)));
                }
                if args.len() != 1 {
                    return Ok(None);
                }
                let x = args[0];
                if !x.is_heap() {
                    return Ok(None);
                }
                // Exact `list` or `tuple`: `tuple(t)` returns `t` unchanged; otherwise clone array.
                if let Some((cls, items)) = self.py_seq_parts(x) {
                    if is_tuple && cls.bits() == t_tuple.bits() {
                        return Ok(Some(x));
                    }
                    if (cls.bits() == t_list.bits() || cls.bits() == t_tuple.bits())
                        && items.is_heap()
                        && (self.array_js_len.is_empty() || !self.array_js_len.contains_key(&items.heap_index()))
                    {
                        let len = match self.heap.get(items.heap_index()) {
                            HeapObj::Array(a) if a.len() <= (1 << 24) => a.len(),
                            _ => return Ok(None),
                        };
                        if self.instr_rec.is_some() && !self.native_kernel_admits(4 + len as u64, len.saturating_mul(8)) {
                            return Ok(None);
                        }
                        let mut cloned = self.heap.take_arr_buf(len);
                        match self.heap.get(items.heap_index()) {
                            HeapObj::Array(a) => cloned.extend_from_slice(a),
                            _ => return Ok(None),
                        }
                        if self.instr_rec.is_some() {
                            self.charge_steps(4 + len as i64);
                        }
                        let arr_idx = self.heap.alloc_array_settled(cloned);
                        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                        self.heap.repair_arr_mirror(arr_idx);
                        return Ok(self.py_alloc_seq_fast(target_cls, Value::heap(arr_idx)));
                    }
                }
                // Native generator / list_iterator (`PY_GEN`) or native iterator
                // (`PY_ITER`, no `wrapped`): drain natively with `arr_val` rooted
                // on `host_result_roots` across GC.
                if stop.is_undefined() {
                    return Ok(None);
                }
                let iter_kind: u8 = match self.heap.get(x.heap_index()) {
                    HeapObj::Object(m) if !m.is_ctor && m.pos("wrapped").is_none() => {
                        m.pos("next")
                            .filter(|&s| !m.is_accessor_at(s))
                            .map(|s| m.val_at(s))
                            .and_then(|nv| {
                                nv.is_heap().then(|| match self.heap.get(nv.heap_index()) {
                                    HeapObj::Native(id) if *id == super::native::PY_GEN => 1u8,
                                    HeapObj::Native(id) if *id == super::native::PY_ITER => 2u8,
                                    _ => 0u8,
                                })
                            })
                            .unwrap_or(0)
                    }
                    _ => 0,
                };
                if iter_kind == 0 {
                    return Ok(None);
                }
                let arr_idx = self.heap.alloc_array_settled(Vec::new());
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                self.heap.repair_arr_mirror(arr_idx);
                let arr_val = Value::heap(arr_idx);
                let roots_base = self.host_result_roots.len();
                self.host_result_roots.push(arr_val);
                let drain_res = if iter_kind == 1 {
                    self.py_gen_drain(x, arr_val)
                } else {
                    self.py_iter_drain(x, arr_val)
                };
                self.host_result_roots.truncate(roots_base);
                let out_arr = drain_res?;
                if out_arr.is_undefined() {
                    return Ok(None);
                }
                Ok(self.py_alloc_seq_fast(target_cls, out_arr))
            }
            53 => {
                // `next(it)` / `next(it, dflt)`
                if args.is_empty() || args.len() > 2 {
                    return Ok(None);
                }
                let it = args[0];
                if !it.is_heap() {
                    return Ok(None);
                }
                let Some(stop) = self.py_rt.as_deref().map(|p| p.stop).filter(|v| !v.is_undefined()) else {
                    return Ok(None);
                };
                let next_val = if let Some(v) = self.py_gen_next_fast(it) {
                    v
                } else {
                    let nat = match self.heap.get(it.heap_index()) {
                        HeapObj::Object(m) if !m.is_ctor && m.pos("wrapped").is_none() => {
                            m.pos("next")
                                .filter(|&s| !m.is_accessor_at(s))
                                .map(|s| m.val_at(s))
                                .and_then(|nv| {
                                    nv.is_heap().then(|| match self.heap.get(nv.heap_index()) {
                                        HeapObj::Native(id) if *id == super::native::PY_GEN => 1u8,
                                        HeapObj::Native(id) if *id == super::native::PY_ITER => 2u8,
                                        _ => 0u8,
                                    })
                                })
                                .unwrap_or(0)
                        }
                        _ => 0,
                    };
                    if nat == 1 {
                        self.py_gen_next(it)?
                    } else if nat == 2 {
                        self.py_iter_next(it)?
                    } else {
                        return Ok(None);
                    }
                };
                if next_val.bits() != stop.bits() {
                    return Ok(Some(next_val));
                }
                if args.len() == 2 {
                    return Ok(Some(args[1]));
                }
                let Some(stop_exc) = self.py_own_data(this.heap_index(), "stopExc") else {
                    return Err(Thrown::from_static("StopIteration"));
                };
                let _ = self.call_value(stop_exc, Value::UNDEFINED, &[it])?;
                Err(Thrown::from_static("StopIteration"))
            }
            54 => {
                // `generator.send(v)`
                if args.len() != 2 {
                    return Ok(None);
                }
                self.py_gen_send(args[0], args[1], this)
            }
            _ => Ok(None),
        }
    }

    /// Native `re.Pattern.search` (38), `match` (39), `fullmatch` (40) for
    /// non-bytes patterns on BMP/ASCII `str` (and unmodified `str` subclasses):
    /// bypasses `patRun` / `sticky` / `rt.hasSurrogate` / `execAt` / `mk`.
    fn py_re_exec(&mut self, this: Value, args: &[Value], op: i32) -> Option<Value> {
        #[cfg(feature = "instrument")]
        if self.instr_rec.is_some() {
            return None;
        }
        let p = args[0];
        let raw = args[1];
        if !p.is_heap() {
            return None;
        }
        let (s_val, _) = self.py_unwrap_str_val(raw)?;
        let s_idx = s_val.heap_index();
        self.heap.flatten(s_idx);
        let (s_ascii, n) = match self.heap.get(s_idx) {
            HeapObj::Str(st) if st.is_ascii() => (true, st.as_bytes().len()),
            HeapObj::Str(st) if st.is_wellformed() && !st.has_surrogate() => (false, st.units()),
            _ => return None,
        };
        let re_key = match op {
            38 => "re",
            39 => "sticky",
            _ => "full",
        };
        let re_val = {
            let HeapObj::Object(pm) = self.heap.get(p.heap_index()) else {
                return None;
            };
            if pm.is_ctor {
                return None;
            }
            let is_bytes_slot = pm.pos("isBytes").filter(|&s| !pm.is_accessor_at(s))?;
            if pm.val_at(is_bytes_slot) != Value::FALSE {
                return None;
            }
            let re_slot = pm.pos(re_key).filter(|&s| !pm.is_accessor_at(s))?;
            pm.val_at(re_slot)
        };
        if !re_val.is_heap() || !matches!(self.heap.get(re_val.heap_index()), HeapObj::RegExp(_)) {
            return None;
        }
        let pos = if let Some(&pos_v) = args.get(2) {
            let i = self.bigint_i128(pos_v)?;
            i.clamp(0, n as i128) as usize
        } else {
            0usize
        };
        let re_idx = re_val.heap_index();
        if s_ascii {
            if let Some(found) = self.regexp_exec_ascii_spans(re_idx, s_idx, pos) {
                let Some((ng, spans)) = found else {
                    return Some(Value::NULL);
                };
                let mtmpl = self.py_own_data(this.heap_index(), "mtmpl")?;
                if !mtmpl.is_heap() {
                    return None;
                }
                let (mtmpl_shape, cls_match) = match self.heap.get(mtmpl.heap_index()) {
                    HeapObj::Object(mm) if !mm.is_ctor && mm.slot_len() == 16 => {
                        (mm.shape(), mm.val_at(0))
                    }
                    _ => return None,
                };
                #[inline]
                fn pack_span(sp: Option<(usize, usize)>) -> Value {
                    match sp {
                        Some((a, b)) => Value::num((((a as u64) << 26) | (b as u64)) as f64),
                        None => Value::UNDEFINED,
                    }
                }
                return self.py_alloc_like(
                    mtmpl,
                    mtmpl_shape,
                    &[
                        cls_match,
                        Value::NULL,
                        s_val,
                        raw,
                        s_val,
                        p,
                        Value::num(pos as f64),
                        Value::num(n as f64),
                        Value::NULL,
                        Value::num(ng as f64),
                        pack_span(spans[0]),
                        pack_span(spans[1]),
                        pack_span(spans[2]),
                        pack_span(spans[3]),
                        pack_span(spans[4]),
                        pack_span(spans[5]),
                    ],
                );
            }
        }
        self.set_regexp_last_index(re_idx, pos as u64);
        let m = self.regexp_exec(re_idx, s_val).ok()?;
        if m == Value::NULL {
            return Some(Value::NULL);
        }
        let mtmpl = self.py_own_data(this.heap_index(), "mtmpl")?;
        if !mtmpl.is_heap() {
            return None;
        }
        let (mtmpl_shape, cls_match) = match self.heap.get(mtmpl.heap_index()) {
            HeapObj::Object(mm) if !mm.is_ctor && mm.slot_len() == 16 => (mm.shape(), mm.val_at(0)),
            _ => return None,
        };
        self.py_alloc_like(
            mtmpl,
            mtmpl_shape,
            &[
                cls_match,
                m,
                s_val,
                raw,
                s_val,
                p,
                Value::num(pos as f64),
                Value::num(n as f64),
                Value::NULL,
                Value::num(0.0),
                Value::UNDEFINED,
                Value::UNDEFINED,
                Value::UNDEFINED,
                Value::UNDEFINED,
                Value::UNDEFINED,
                Value::UNDEFINED,
            ],
        )
    }

    /// Native `re.Pattern.sub` (41) for non-bytes patterns with string
    /// replacements (literal or simple `\1`..`\99` / standard escapes) on
    /// well-formed BMP/ASCII `str` (and unmodified `str` subclasses).
    fn py_re_sub(&mut self, args: &[Value]) -> Option<Value> {
        #[cfg(feature = "safe-sandbox")]
        {
            let _ = args;
            return None;
        }
        #[cfg(not(feature = "safe-sandbox"))]
        {
            #[cfg(feature = "instrument")]
            if self.instr_rec.is_some() {
                return None;
            }
            let p = args[0];
            let repl = args[1];
            let raw = args[2];
            if !p.is_heap() {
                return None;
            }
            let count: usize = if let Some(&cv) = args.get(3) {
                let c = self.bigint_i128(cv)?;
                if c < 0 {
                    return None;
                }
                usize::try_from(c).ok()?
            } else {
                0
            };
            let (s_val, _) = self.py_unwrap_str_val(raw)?;
            let (repl_idx, repl_ascii) = self.py_strm_flat(repl)?;
            let s_idx = s_val.heap_index();
            self.heap.flatten(s_idx);
            let (s_ascii, s_len) = match self.heap.get(s_idx) {
                HeapObj::Str(st) if st.is_ascii() => (true, st.as_bytes().len()),
                HeapObj::Str(st) if st.is_wellformed() && !st.has_surrogate() => (false, st.units()),
                _ => return None,
            };
            let (re_val, ngroups) = {
                let HeapObj::Object(pm) = self.heap.get(p.heap_index()) else {
                    return None;
                };
                if pm.is_ctor {
                    return None;
                }
                let is_bytes_slot = pm.pos("isBytes").filter(|&s| !pm.is_accessor_at(s))?;
                if pm.val_at(is_bytes_slot) != Value::FALSE {
                    return None;
                }
                let re_slot = pm.pos("re").filter(|&s| !pm.is_accessor_at(s))?;
                let ng_slot = pm.pos("ngroups").filter(|&s| !pm.is_accessor_at(s))?;
                let ng_v = pm.val_at(ng_slot);
                if !ng_v.is_number() || ng_v.as_f64() < 0.0 {
                    return None;
                }
                (pm.val_at(re_slot), ng_v.as_f64() as usize)
            };
            if !re_val.is_heap() {
                return None;
            }
            let re_idx = re_val.heap_index();
            let unicode = match self.heap.get(re_idx) {
                HeapObj::RegExp(r) => r.flags.contains('u') || r.flags.contains('v'),
                _ => return None,
            };

            #[derive(Clone, Copy)]
            enum SubPart {
                Lit(usize, usize),
                Byte(u8),
                Group(usize),
            }
            let mut parts: Vec<SubPart> = Vec::new();
            let mut has_group = false;
            {
                let rb = self.py_strm_bytes(repl_idx);
                if !rb.contains(&b'\\') {
                    if !rb.is_empty() {
                        parts.push(SubPart::Lit(0, rb.len()));
                    }
                } else {
                    let mut i = 0usize;
                    let mut lit_start = 0usize;
                    while i < rb.len() {
                        if rb[i] != b'\\' {
                            i += 1;
                            continue;
                        }
                        if i > lit_start {
                            parts.push(SubPart::Lit(lit_start, i));
                        }
                        i += 1;
                        let d = *rb.get(i)?;
                        match d {
                            b'\\' => { parts.push(SubPart::Byte(b'\\')); i += 1; }
                            b'n' => { parts.push(SubPart::Byte(b'\n')); i += 1; }
                            b't' => { parts.push(SubPart::Byte(b'\t')); i += 1; }
                            b'r' => { parts.push(SubPart::Byte(b'\r')); i += 1; }
                            b'f' => { parts.push(SubPart::Byte(0x0c)); i += 1; }
                            b'v' => { parts.push(SubPart::Byte(0x0b)); i += 1; }
                            b'a' => { parts.push(SubPart::Byte(0x07)); i += 1; }
                            b'b' => { parts.push(SubPart::Byte(0x08)); i += 1; }
                            b'1'..=b'9' => {
                                // Octal escape (3 octal digits starting with 1..7) -> JS fallback.
                                if d <= b'7'
                                    && rb.get(i + 1).is_some_and(|&c| (b'0'..=b'7').contains(&c))
                                    && rb.get(i + 2).is_some_and(|&c| (b'0'..=b'7').contains(&c))
                                {
                                    return None;
                                }
                                let mut num = (d - b'0') as usize;
                                i += 1;
                                if let Some(&d2) = rb.get(i).filter(|&&c| c.is_ascii_digit()) {
                                    num = num * 10 + (d2 - b'0') as usize;
                                    i += 1;
                                }
                                if num == 0 || num > ngroups {
                                    return None;
                                }
                                has_group = true;
                                parts.push(SubPart::Group(num));
                            }
                            _ => return None,
                        }
                        lit_start = i;
                    }
                    if lit_start < rb.len() {
                        parts.push(SubPart::Lit(lit_start, rb.len()));
                    }
                }
            }

            if s_ascii {
                self.ensure_regexp_ascii_twin_deferred(re_idx, s_len);
                let _gc = self.gc_lock_guard();
                let matches: Vec<regress::Match> = {
                    let subj = match self.heap.get(s_idx) {
                        HeapObj::Str(js) => js.as_str_wf(),
                        _ => return None,
                    };
                    let HeapObj::RegExp(r) = self.heap.get(re_idx) else {
                        return None;
                    };
                    let regex = match r.ascii_twin.get() {
                        Some(Some(twin)) => &**twin,
                        _ => &*r.regex,
                    };
                    let mut it = regex.find_from_ascii(subj, 0);
                    let Some(first) = it.next() else {
                        return Some(s_val);
                    };
                    let mut ms = Vec::new();
                    ms.push(first);
                    if count != 1 {
                        for m in it {
                            ms.push(m);
                            if count > 0 && ms.len() >= count {
                                break;
                            }
                        }
                    }
                    ms
                };
                if has_group {
                    for m in &matches {
                        for &pt in &parts {
                            if let SubPart::Group(g) = pt {
                                if m.captures.get(g - 1).and_then(|c| c.as_ref()).is_none() {
                                    return None;
                                }
                            }
                        }
                    }
                }
                let both_ascii = repl_ascii;
                let mut out = self.heap.take_byte_buf(s_len + 32);
                {
                    let sb = self.py_strm_bytes(s_idx);
                    let rb = self.py_strm_bytes(repl_idx);
                    let mut last = 0usize;
                    for m in &matches {
                        let r = m.range();
                        out.extend_from_slice(&sb[last..r.start]);
                        for &pt in &parts {
                            match pt {
                                SubPart::Lit(a, b) => out.extend_from_slice(&rb[a..b]),
                                SubPart::Byte(b) => out.push(b),
                                SubPart::Group(g) => {
                                    if let Some(cr) = &m.captures[g - 1] {
                                        out.extend_from_slice(&sb[cr.start..cr.end]);
                                    }
                                }
                            }
                        }
                        last = r.end;
                        if out.len() > STRM_MAX_TEXT * 3 {
                            self.heap.return_byte_buf(out);
                            return None;
                        }
                    }
                    out.extend_from_slice(&sb[last..]);
                }
                if (both_ascii && out.len() > STRM_MAX_TEXT) || out.len() > STRM_MAX_TEXT * 3 {
                    self.heap.return_byte_buf(out);
                    return None;
                }
                let out_len = out.len();
                let res = self.py_strm_new_as(out, both_ascii);
                if !both_ascii {
                    if let HeapObj::Str(st) = self.heap.get(res.heap_index()) {
                        if st.units() > STRM_MAX_TEXT {
                            return None;
                        }
                    }
                }
                self.charge_steps(out_len as i64);
                return Some(res);
            }

            // Non-ASCII BMP path (`!has_surrogate()`): match over cached UTF-16 units.
            let _gc = self.gc_lock_guard();
            let u16s = self.regex_subject_units_take(s_val);
            let matches: Vec<regress::Match> = {
                let HeapObj::RegExp(r) = self.heap.get(re_idx) else {
                    self.regex_subject_units_put(s_val, u16s);
                    return None;
                };
                let regex = &r.core.regex;
                let mut ms = Vec::new();
                if unicode {
                    let mut it = regex.find_from_utf16(&u16s, 0);
                    if let Some(first) = it.next() {
                        ms.push(first);
                        if count != 1 {
                            for m in it {
                                ms.push(m);
                                if count > 0 && ms.len() >= count {
                                    break;
                                }
                            }
                        }
                    }
                } else {
                    let mut it = regex.find_from_ucs2(&u16s, 0);
                    if let Some(first) = it.next() {
                        ms.push(first);
                        if count != 1 {
                            for m in it {
                                ms.push(m);
                                if count > 0 && ms.len() >= count {
                                    break;
                                }
                            }
                        }
                    }
                }
                ms
            };
            if matches.is_empty() {
                self.regex_subject_units_put(s_val, u16s);
                return Some(s_val);
            }
            if has_group {
                for m in &matches {
                    for &pt in &parts {
                        if let SubPart::Group(g) = pt {
                            if m.captures.get(g - 1).and_then(|c| c.as_ref()).is_none() {
                                self.regex_subject_units_put(s_val, u16s);
                                return None;
                            }
                        }
                    }
                }
            }
            let repl_utf8 = match std::str::from_utf8(self.py_strm_bytes(repl_idx)) {
                Ok(s) => s.to_owned(),
                Err(_) => {
                    self.regex_subject_units_put(s_val, u16s);
                    return None;
                }
            };
            let mut out_u16: Vec<u16> = Vec::with_capacity(u16s.len() + 32);
            let mut last = 0usize;
            for m in &matches {
                let r = m.range();
                out_u16.extend_from_slice(&u16s[last..r.start]);
                for &pt in &parts {
                    match pt {
                        SubPart::Lit(a, b) => out_u16.extend(repl_utf8[a..b].encode_utf16()),
                        SubPart::Byte(b) => out_u16.push(b as u16),
                        SubPart::Group(g) => {
                            if let Some(cr) = &m.captures[g - 1] {
                                out_u16.extend_from_slice(&u16s[cr.start..cr.end]);
                            }
                        }
                    }
                }
                last = r.end;
                if out_u16.len() > STRM_MAX_TEXT {
                    self.regex_subject_units_put(s_val, u16s);
                    return None;
                }
            }
            out_u16.extend_from_slice(&u16s[last..]);
            self.regex_subject_units_put(s_val, u16s);
            if out_u16.len() > STRM_MAX_TEXT {
                return None;
            }
            let out_len = out_u16.len();
            let res = self.units_value(&out_u16);
            self.charge_steps(out_len as i64);
            Some(res)
        }
    }

    /// Native `re.Match` methods: `start` (42), `end` (43), `span` (44),
    /// `group` (45), `groups` (46) for BMP/ASCII non-bytes matches (`pairs == null`).
    /// Ops 42, 43 are strictly non-allocating (`noalloc` safe); op 45 allocates
    /// lazily on span-backed matches (`m == null`) and is routed via `jit_py_strm_alloc`.
    fn py_re_match_op(&mut self, args: &[Value], op: i32) -> Option<Value> {
        let m_obj = args[0];
        if !m_obj.is_heap() {
            return None;
        }
        let m_obj_idx = m_obj.heap_index();
        let (m_val, t_val, p_val, ng_lazy) = {
            let HeapObj::Object(mm) = self.heap.get(m_obj_idx) else {
                return None;
            };
            if mm.is_ctor || mm.slot_len() != 16 {
                return None;
            }
            if !super::py_rt::key_eq(mm.key_at(1), "m")
                || !super::py_rt::key_eq(mm.key_at(4), "t")
                || !super::py_rt::key_eq(mm.key_at(5), "p")
                || !super::py_rt::key_eq(mm.key_at(8), "pairs")
                || !super::py_rt::key_eq(mm.key_at(9), "ng")
                || !super::py_rt::key_eq(mm.key_at(10), "g0")
                || mm.is_accessor_at(1)
                || mm.is_accessor_at(4)
                || mm.is_accessor_at(5)
                || mm.is_accessor_at(8)
                || mm.is_accessor_at(9)
                || mm.is_accessor_at(10)
            {
                return None;
            }
            if mm.val_at(8) != Value::NULL {
                return None;
            }
            let m_val = mm.val_at(1);
            let t_val = mm.val_at(4);
            let p_val = mm.val_at(5);
            if !p_val.is_heap() {
                return None;
            }
            let HeapObj::Object(pm) = self.heap.get(p_val.heap_index()) else {
                return None;
            };
            if pm.is_ctor {
                return None;
            }
            let is_bytes_slot = pm.pos("isBytes").filter(|&s| !pm.is_accessor_at(s))?;
            if pm.val_at(is_bytes_slot) != Value::FALSE {
                return None;
            }
            let ng_v = mm.val_at(9);
            let ng_lazy = if m_val == Value::NULL && ng_v.is_number() {
                let f = ng_v.as_f64();
                if (1.0..=6.0).contains(&f) && f.fract() == 0.0 {
                    f as usize
                } else {
                    return None;
                }
            } else {
                0
            };
            (m_val, t_val, p_val, ng_lazy)
        };

        let resolve_group = |vm: &Vm, arg: Option<Value>| -> Option<usize> {
            let Some(a) = arg else {
                return Some(0);
            };
            if let Some(k) = a.small_bigint_val() {
                return if k < 0 { None } else { Some(k as usize) };
            }
            if a.is_heap() {
                let HeapObj::Str(key_s) = vm.heap.get(a.heap_index()) else {
                    return None;
                };
                let key_bytes = key_s.as_bytes();
                let names_v = {
                    let HeapObj::Object(pm) = vm.heap.get(p_val.heap_index()) else {
                        return None;
                    };
                    let ns = pm.pos("names").filter(|&s| !pm.is_accessor_at(s))?;
                    pm.val_at(ns)
                };
                if !names_v.is_heap() {
                    return None;
                }
                let HeapObj::Map { keys, vals } = vm.heap.get(names_v.heap_index()) else {
                    return None;
                };
                if keys.len() > 24 {
                    return None;
                }
                for (k, v) in keys.iter().zip(vals.iter()) {
                    if k.is_heap() {
                        if let HeapObj::Str(ks) = vm.heap.get(k.heap_index()) {
                            if ks.as_bytes() == key_bytes {
                                if v.is_number() && v.as_f64() >= 0.0 {
                                    return Some(v.as_f64() as usize);
                                }
                                return None;
                            }
                        }
                    }
                }
            }
            None
        };

        // Lazy ASCII span-backed Match (`m == null`, spans packed in slots `10..10+ng`).
        if ng_lazy > 0 {
            if !t_val.is_heap() {
                return None;
            }
            let t_idx = t_val.heap_index();
            let t_len = match self.heap.get(t_idx) {
                HeapObj::Str(st) if st.is_ascii() => st.as_bytes().len(),
                _ => return None,
            };
            #[inline]
            fn unpack_span(v: Value, limit: usize) -> Option<Option<(usize, usize)>> {
                if v.is_undefined() {
                    return Some(None);
                }
                if !v.is_number() {
                    return None;
                }
                let f = v.as_f64();
                if f < 0.0 || f.fract() != 0.0 {
                    return None;
                }
                let u = f as u64;
                let gs = (u >> 26) as usize;
                let ge = (u & 0x03ff_ffff) as usize;
                if gs > ge || ge > limit {
                    return None;
                }
                Some(Some((gs, ge)))
            }
            match op {
                42..=44 => {
                    let ki = resolve_group(self, args.get(1).copied())?;
                    if ki >= ng_lazy {
                        return None;
                    }
                    let pk = match self.heap.get(m_obj_idx) {
                        HeapObj::Object(mm) if !mm.is_accessor_at(10 + ki) => mm.val_at(10 + ki),
                        _ => return None,
                    };
                    let (start, end) = match unpack_span(pk, t_len)? {
                        Some((gs, ge)) => (gs as i128, ge as i128),
                        None => (-1i128, -1i128),
                    };
                    if op == 42 {
                        return Value::small_bigint(start);
                    }
                    if op == 43 {
                        return Value::small_bigint(end);
                    }
                    let s_v = Value::small_bigint(start)?;
                    let e_v = Value::small_bigint(end)?;
                    let (t_tuple, seq_tmpl, seq_shape) = {
                        let p = self.py_rt.as_deref()?;
                        if p.seq_shape == crate::shape::DICT {
                            return None;
                        }
                        (p.t_tuple, p.seq_tmpl, p.seq_shape)
                    };
                    if !self.py_alloc_like_ok(seq_tmpl, seq_shape) {
                        return None;
                    }
                    let mut buf = self.heap.take_arr_buf(2);
                    buf.push(s_v);
                    buf.push(e_v);
                    let arr = self.alloc_array_current_realm(buf);
                    return self.py_alloc_like_ready(seq_tmpl, seq_shape, &[t_tuple, arr]);
                }
                45 => {
                    let ki = resolve_group(self, args.get(1).copied())?;
                    if ki >= ng_lazy {
                        return None;
                    }
                    let pk = match self.heap.get(m_obj_idx) {
                        HeapObj::Object(mm) if !mm.is_accessor_at(10 + ki) => mm.val_at(10 + ki),
                        _ => return None,
                    };
                    let Some((gs, ge)) = unpack_span(pk, t_len)? else {
                        return Some(Value::NULL);
                    };
                    if gs == 0 && ge == t_len {
                        return Some(t_val);
                    }
                    return Some(self.py_strm_slice_as(t_idx, gs, ge, true));
                }
                46 => {
                    let default = args.get(1).copied().unwrap_or(Value::NULL);
                    let (t_tuple, seq_tmpl, seq_shape) = {
                        let p = self.py_rt.as_deref()?;
                        if p.seq_shape == crate::shape::DICT {
                            return None;
                        }
                        (p.t_tuple, p.seq_tmpl, p.seq_shape)
                    };
                    if !self.py_alloc_like_ok(seq_tmpl, seq_shape) {
                        return None;
                    }
                    let n_groups = ng_lazy - 1;
                    let mut spans: [Option<(usize, usize)>; 5] = [None; 5];
                    {
                        let HeapObj::Object(mm) = self.heap.get(m_obj_idx) else {
                            return None;
                        };
                        for (i, slot) in spans[..n_groups].iter_mut().enumerate() {
                            if mm.is_accessor_at(11 + i) {
                                return None;
                            }
                            *slot = unpack_span(mm.val_at(11 + i), t_len)?;
                        }
                    }
                    let _gc = self.gc_lock_guard();
                    let mut buf = self.heap.take_arr_buf(n_groups);
                    for &sp in &spans[..n_groups] {
                        match sp {
                            Some((gs, ge)) => {
                                let v = if gs == 0 && ge == t_len {
                                    t_val
                                } else {
                                    self.py_strm_slice_as(t_idx, gs, ge, true)
                                };
                                buf.push(v);
                            }
                            None => buf.push(default),
                        }
                    }
                    let arr = self.alloc_array_current_realm(buf);
                    return self.py_alloc_like_ready(seq_tmpl, seq_shape, &[t_tuple, arr]);
                }
                _ => return None,
            }
        }

        // Eager Array-backed Match fallback (`m` is a JS Array).
        if !m_val.is_heap() {
            return None;
        }
        let m_arr_idx = m_val.heap_index();
        if !matches!(self.heap.get(m_arr_idx), HeapObj::Array(_)) {
            return None;
        }

        match op {
            // `start()` / `start(0)`, `end()` / `end(0)`, `span()` / `span(0)`
            42..=44 => {
                if args.len() == 2 && args[1].small_bigint_val() != Some(0) {
                    return None;
                }
                let idx_v = if let Some(p) = self.regexp_result_props.get(&m_arr_idx) {
                    p.values[0]
                } else if let Some(props) = self.arr_props.get(&m_arr_idx) {
                    let s = props.pos("index").filter(|&s| !props.is_accessor_at(s))?;
                    props.val_at(s)
                } else {
                    return None;
                };
                if !idx_v.is_number() {
                    return None;
                }
                let start_f = idx_v.as_f64();
                if start_f < 0.0 || start_f.fract() != 0.0 || start_f > (1i64 << 45) as f64 {
                    return None;
                }
                let start = start_f as i128;
                if op == 42 {
                    return Value::small_bigint(start);
                }
                let m0 = match self.heap.get(m_arr_idx) {
                    HeapObj::Array(items) => *items.first()?,
                    _ => return None,
                };
                if !m0.is_heap() {
                    return None;
                }
                let m0_len = match self.heap.get(m0.heap_index()) {
                    HeapObj::Str(st) => {
                        if st.is_ascii() {
                            st.as_bytes().len() as i128
                        } else {
                            st.units() as i128
                        }
                    }
                    _ => return None,
                };
                let end = start + m0_len;
                if op == 43 {
                    return Value::small_bigint(end);
                }
                let s_v = Value::small_bigint(start)?;
                let e_v = Value::small_bigint(end)?;
                let (t_tuple, seq_tmpl, seq_shape) = {
                    let p = self.py_rt.as_deref()?;
                    if p.seq_shape == crate::shape::DICT {
                        return None;
                    }
                    (p.t_tuple, p.seq_tmpl, p.seq_shape)
                };
                if !self.py_alloc_like_ok(seq_tmpl, seq_shape) {
                    return None;
                }
                let mut buf = self.heap.take_arr_buf(2);
                buf.push(s_v);
                buf.push(e_v);
                let arr = self.alloc_array_current_realm(buf);
                self.py_alloc_like_ready(seq_tmpl, seq_shape, &[t_tuple, arr])
            }
            // `group()` / `group(i)` / `group(name)`
            45 => {
                let ki = resolve_group(self, args.get(1).copied())?;
                let v = match self.heap.get(m_arr_idx) {
                    HeapObj::Array(items) => *items.get(ki)?,
                    _ => return None,
                };
                if v.is_undefined() {
                    Some(Value::NULL)
                } else if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Str(_)) {
                    Some(v)
                } else {
                    None
                }
            }
            // `groups()` / `groups(default)`
            46 => {
                let default = args.get(1).copied().unwrap_or(Value::NULL);
                let (t_tuple, seq_tmpl, seq_shape) = {
                    let p = self.py_rt.as_deref()?;
                    if p.seq_shape == crate::shape::DICT {
                        return None;
                    }
                    (p.t_tuple, p.seq_tmpl, p.seq_shape)
                };
                if !self.py_alloc_like_ok(seq_tmpl, seq_shape) {
                    return None;
                }
                let n_groups = match self.heap.get(m_arr_idx) {
                    HeapObj::Array(items) if !items.is_empty() => items.len() - 1,
                    _ => return None,
                };
                let mut buf = self.heap.take_arr_buf(n_groups);
                {
                    let HeapObj::Array(items) = self.heap.get(m_arr_idx) else {
                        return None;
                    };
                    for &v in &items[1..] {
                        if v.is_undefined() {
                            buf.push(default);
                        } else if v.is_heap() && matches!(self.heap.get(v.heap_index()), HeapObj::Str(_)) {
                            buf.push(v);
                        } else {
                            return None;
                        }
                    }
                }
                let arr = self.alloc_array_current_realm(buf);
                self.py_alloc_like_ready(seq_tmpl, seq_shape, &[t_tuple, arr])
            }
            _ => None,
        }
    }

    /// `__zipp_py_write(op, to_stderr, payload)`: direct Python stdout/stderr
    /// emission that bypasses `inspect_line`'s 64 KiB truncation and preserves
    /// exact bytes (no appended newline) whenever a streaming host installs a
    /// raw output sink.
    ///
    /// - `op == 0`: write UTF-8 text to the raw sink if one is installed;
    ///   returns `true` if handled, `false` if the host has no raw sink (so
    ///   the runtime falls back to its line-buffered non-streaming path).
    /// - `op == 1`: emit one untruncated line on the non-streaming console path
    ///   (charged against `output_limit` when instrumented, never truncated at
    ///   64 KiB with `"..."`).
    /// - `op == 2`: write raw bytes (`Array`/`TypedArray` of `0..=255`) from
    ///   `sys.stdout.buffer.write` / `sys.stderr.buffer.write` to the raw sink
    ///   if one is installed; returns `true` if handled, `false` otherwise.
    /// - `op == 3`: flush the raw sink for the stream (passing `&[]`); returns
    ///   `true` if a raw sink is installed, `false` otherwise.
    pub(crate) fn py_write(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        let op = arg(0).as_f64() as i32;
        let to_stderr = self.truthy(arg(1));
        let stream = if to_stderr {
            crate::vm::ConsoleStream::Stderr
        } else {
            crate::vm::ConsoleStream::Stdout
        };
        match op {
            0 => {
                if self.raw_output_sink.is_none() {
                    return Ok(Value::bool(false));
                }
                let payload = arg(2);
                if payload.is_heap() && self.heap.is_str_like(payload.heap_index()) {
                    let idx = payload.heap_index();
                    self.heap.flatten(idx);
                    let byte_len = match self.heap.get(idx) {
                        HeapObj::Str(s) => s.as_bytes().len(),
                        _ => 0,
                    };
                    #[cfg(feature = "instrument")]
                    self.instrument_output_bytes(byte_len)
                        .map_err(|msg| Thrown(msg.into()))?;
                    if byte_len > 0 {
                        if let (HeapObj::Str(s), Some(sink)) =
                            (self.heap.get(idx), self.raw_output_sink.as_mut())
                        {
                            let text = s.as_str_lossy();
                            sink(stream, text.as_bytes());
                        }
                    }
                    return Ok(Value::bool(true));
                }
                let text = self.to_js_string(payload)?;
                #[cfg(feature = "instrument")]
                self.instrument_output_bytes(text.len())
                    .map_err(|msg| Thrown(msg.into()))?;
                if !text.is_empty() {
                    if let Some(sink) = self.raw_output_sink.as_mut() {
                        sink(stream, text.as_bytes());
                    }
                }
                Ok(Value::bool(true))
            }
            1 => {
                let payload = arg(2);
                let line = if payload.is_heap() && self.heap.is_str_like(payload.heap_index()) {
                    let idx = payload.heap_index();
                    self.heap.flatten(idx);
                    match self.heap.get(idx) {
                        HeapObj::Str(s) => s.to_lossy_string(),
                        _ => String::new(),
                    }
                } else {
                    self.to_js_string(payload)?
                };
                #[cfg(feature = "instrument")]
                self.instrument_output_line(&line)
                    .map_err(|msg| Thrown(msg.into()))?;
                self.push_console_line_raw(line, to_stderr);
                Ok(Value::UNDEFINED)
            }
            2 => {
                if self.raw_output_sink.is_none() {
                    return Ok(Value::bool(false));
                }
                let payload = arg(2);
                if !payload.is_heap() {
                    return Err(Thrown("TypeError: expected bytes array".into()));
                }
                let idx = payload.heap_index();
                enum Kind {
                    Array(usize),
                    Typed { buf: u32, off: usize, end: usize },
                }
                let kind = match self.heap.get(idx) {
                    HeapObj::Array(items) => Kind::Array(items.len()),
                    HeapObj::TypedArray {
                        buffer,
                        byte_offset,
                        length,
                        ..
                    } => {
                        let buf = *buffer;
                        let off = *byte_offset;
                        let len = *length;
                        let end = off.saturating_add(len);
                        match self.heap.get(buf) {
                            HeapObj::ArrayBuffer { data, detached } if !*detached => {
                                let clamped = end.min(data.len());
                                let start = off.min(clamped);
                                Kind::Typed {
                                    buf,
                                    off: start,
                                    end: clamped,
                                }
                            }
                            _ => return Err(Thrown("TypeError: detached buffer".into())),
                        }
                    }
                    _ => return Err(Thrown("TypeError: expected bytes array".into())),
                };
                match kind {
                    Kind::Array(n) => {
                        #[cfg(feature = "instrument")]
                        self.instrument_output_bytes(n)
                            .map_err(|msg| Thrown(msg.into()))?;
                        if n > 0 {
                            let bytes = match self.heap.get(idx) {
                                HeapObj::Array(items) => {
                                    let mut out = Vec::with_capacity(items.len());
                                    for &x in items {
                                        out.push(x.as_f64() as u8);
                                    }
                                    out
                                }
                                _ => Vec::new(),
                            };
                            if !bytes.is_empty() {
                                if let Some(sink) = self.raw_output_sink.as_mut() {
                                    sink(stream, &bytes);
                                }
                            }
                        }
                    }
                    Kind::Typed { buf, off, end } => {
                        #[cfg(feature = "instrument")]
                        self.instrument_output_bytes(end - off)
                            .map_err(|msg| Thrown(msg.into()))?;
                        if end > off {
                            if let (
                                HeapObj::ArrayBuffer { data, detached },
                                Some(sink),
                            ) = (self.heap.get(buf), self.raw_output_sink.as_mut())
                            {
                                if !*detached {
                                    if let Some(slice) = data.get(off..end) {
                                        sink(stream, slice);
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(Value::bool(true))
            }
            3 => {
                if let Some(sink) = self.raw_output_sink.as_mut() {
                    sink(stream, &[]);
                    Ok(Value::bool(true))
                } else {
                    Ok(Value::bool(false))
                }
            }
            _ => Ok(Value::UNDEFINED),
        }
    }

    /// Native fast-path `__zipp_py_textwrap(text, width, init_indent, sub_indent,
    /// flags, tabsize, max_lines, placeholder)` for Python's `textwrap.wrap` /
    /// `textwrap.fill` / `TextWrapper.wrap`. Returns a JS `Array` of wrapped
    /// line strings, or `undefined` to fall back to `stdlib.js` (e.g. invalid
    /// arguments, non-BMP astral characters, or metering decline).
    pub(crate) fn py_textwrap(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        #[inline]
        fn is_ascii_ws(b: u8) -> bool {
            b == b' ' || (9..=13).contains(&b)
        }
        #[inline]
        fn is_wrap_ws_char(c: char) -> bool {
            let u = c as u32;
            u == 32
                || (9..=13).contains(&u)
                || (u >= 160
                    && (u == 0x00A0
                        || u == 0xFEFF
                        || u == 0x1680
                        || (0x2000..=0x200A).contains(&u)
                        || u == 0x2028
                        || u == 0x2029
                        || u == 0x202F
                        || u == 0x205F
                        || u == 0x3000))
        }
        #[inline]
        fn to_isize_opt(v: Value) -> Option<isize> {
            if v.is_int() {
                Some(v.as_int() as isize)
            } else if v.is_number() {
                let f = v.as_f64();
                if f.is_finite() && f.fract() == 0.0 && f >= (i32::MIN as f64) && f <= (i32::MAX as f64) {
                    Some(f as isize)
                } else {
                    None
                }
            } else {
                None
            }
        }

        if args.len() < 8 {
            return Ok(Value::UNDEFINED);
        }
        let text_val = args[0];
        let Some(width) = to_isize_opt(args[1]) else {
            return Ok(Value::UNDEFINED);
        };
        if width <= 0 {
            return Ok(Value::UNDEFINED);
        }
        let init_val = args[2];
        let sub_val = args[3];
        let Some(flags) = to_isize_opt(args[4]) else {
            return Ok(Value::UNDEFINED);
        };
        let Some(tabsize) = to_isize_opt(args[5]) else {
            return Ok(Value::UNDEFINED);
        };
        let max_lines: Option<usize> = if args[6].is_null() {
            None
        } else {
            let Some(m) = to_isize_opt(args[6]) else {
                return Ok(Value::UNDEFINED);
            };
            if m <= 0 {
                return Ok(Value::UNDEFINED);
            }
            Some(m as usize)
        };
        let ph_val = args[7];

        if !text_val.is_heap() || !init_val.is_heap() || !sub_val.is_heap() || !ph_val.is_heap() {
            return Ok(Value::UNDEFINED);
        }
        let (ti, ii, si, pi) = (
            text_val.heap_index(),
            init_val.heap_index(),
            sub_val.heap_index(),
            ph_val.heap_index(),
        );
        if !self.heap.is_str_like(ti)
            || !self.heap.is_str_like(ii)
            || !self.heap.is_str_like(si)
            || !self.heap.is_str_like(pi)
        {
            return Ok(Value::UNDEFINED);
        }
        self.heap.flatten(ti);
        self.heap.flatten(ii);
        self.heap.flatten(si);
        self.heap.flatten(pi);

        let (all_ascii, text_len_bytes) = {
            let (
                HeapObj::Str(ts),
                HeapObj::Str(is),
                HeapObj::Str(ss),
                HeapObj::Str(ps),
            ) = (
                self.heap.get(ti),
                self.heap.get(ii),
                self.heap.get(si),
                self.heap.get(pi),
            )
            else {
                return Ok(Value::UNDEFINED);
            };
            if !ts.is_wellformed() || !is.is_wellformed() || !ss.is_wellformed() || !ps.is_wellformed() {
                return Ok(Value::UNDEFINED);
            }
            (
                ts.is_ascii() && is.is_ascii() && ss.is_ascii() && ps.is_ascii(),
                ts.as_bytes().len(),
            )
        };

        let cost = 8u64.saturating_add((text_len_bytes as u64).saturating_mul(2));
        if !self.native_kernel_admits(cost, text_len_bytes.saturating_add(512)) {
            return Ok(Value::UNDEFINED);
        }

        let expand_tabs = (flags & 1) != 0;
        let replace_ws = (flags & 2) != 0;
        let break_long = (flags & 4) != 0;
        let drop_ws = (flags & 8) != 0;
        let break_hyphens = (flags & 16) != 0;

        if all_ascii {
            #[inline]
            fn ascii_bytes_as_str(bytes: &[u8]) -> &str {
                debug_assert!(bytes.is_ascii());
                #[cfg(not(feature = "safe-sandbox"))]
                {
                    // SAFETY: all_ascii guarantees valid UTF-8.
                    unsafe { std::str::from_utf8_unchecked(bytes) }
                }
                #[cfg(feature = "safe-sandbox")]
                {
                    std::str::from_utf8(bytes).unwrap_or_default()
                }
            }
            // Borrow raw bytes into local buffers only if needed; we clone
            // owned `Vec<u8>` slices out before mutating `self.heap` to alloc
            // result strings.
            let (raw_text, init_b, sub_b, ph_b) = {
                let (
                    HeapObj::Str(ts),
                    HeapObj::Str(is),
                    HeapObj::Str(ss),
                    HeapObj::Str(ps),
                ) = (
                    self.heap.get(ti),
                    self.heap.get(ii),
                    self.heap.get(si),
                    self.heap.get(pi),
                )
                else {
                    return Ok(Value::UNDEFINED);
                };
                // Fast zero-copy single-line path: fits in width, no tabs/ws to
                // rewrite, empty initial_indent, and no trailing ws to drop.
                let tb = ts.as_bytes();
                let ib = is.as_bytes();
                let needs_expand = expand_tabs && memchr::memchr(b'\t', tb).is_some();
                let needs_replace = replace_ws && tb.iter().any(|&b| (9..=13).contains(&b));
                if !needs_expand && !needs_replace {
                    if tb.is_empty() {
                        let arr = self.heap.alloc_array_settled(Vec::new());
                        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                        self.heap.repair_arr_mirror(arr);
                        self.charge_steps(cost as i64);
                        return Ok(Value::heap(arr));
                    }
                    let avail0 = width - ib.len() as isize;
                    if (tb.len() as isize) <= avail0 {
                        let end = if drop_ws {
                            let mut e = tb.len();
                            while e > 0 && is_ascii_ws(tb[e - 1]) {
                                e -= 1;
                            }
                            e
                        } else {
                            tb.len()
                        };
                        if end == 0 {
                            let arr = self.heap.alloc_array_settled(Vec::new());
                            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                            self.heap.repair_arr_mirror(arr);
                            self.charge_steps(cost as i64);
                            return Ok(Value::heap(arr));
                        }
                        if ib.is_empty() && end == tb.len() {
                            let mut items = self.heap.take_arr_buf(1);
                            items.push(text_val);
                            let arr = self.heap.alloc_array_settled(items);
                            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                            self.heap.repair_arr_mirror(arr);
                            self.charge_steps(cost as i64);
                            return Ok(Value::heap(arr));
                        }
                        let mut one = Vec::with_capacity(ib.len() + end);
                        one.extend_from_slice(ib);
                        one.extend_from_slice(&tb[..end]);
                        let s_str = ascii_bytes_as_str(&one);
                        let s_idx = self.heap.alloc_str_copy(s_str);
                        let mut items = self.heap.take_arr_buf(1);
                        items.push(Value::heap(s_idx));
                        let arr = self.heap.alloc_array_settled(items);
                        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                        self.heap.repair_arr_mirror(arr);
                        self.charge_steps(cost as i64);
                        return Ok(Value::heap(arr));
                    }
                }
                (tb.to_vec(), ib.to_vec(), ss.as_bytes().to_vec(), ps.as_bytes().to_vec())
            };

            let owned: Vec<u8>;
            let s: &[u8] = {
                let mut cur: std::borrow::Cow<'_, [u8]> = std::borrow::Cow::Borrowed(&raw_text);
                if expand_tabs && memchr::memchr(b'\t', &cur).is_some() {
                    let mut out = Vec::with_capacity(cur.len() + 16);
                    let mut col = 0isize;
                    for &b in cur.iter() {
                        if b == b'\t' {
                            let pad = if tabsize > 0 { (tabsize - (col % tabsize)) as usize } else { 0 };
                            out.resize(out.len() + pad, b' ');
                            col += pad as isize;
                        } else {
                            out.push(b);
                            col = if b == b'\n' || b == b'\r' { 0 } else { col + 1 };
                        }
                    }
                    cur = std::borrow::Cow::Owned(out);
                }
                if replace_ws && cur.iter().any(|&b| (9..=13).contains(&b)) {
                    let mut out = cur.into_owned();
                    for b in &mut out {
                        if (9..=13).contains(b) {
                            *b = b' ';
                        }
                    }
                    cur = std::borrow::Cow::Owned(out);
                }
                owned = cur.into_owned();
                &owned
            };

            if s.is_empty() {
                let arr = self.heap.alloc_array_settled(Vec::new());
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                self.heap.repair_arr_mirror(arr);
                self.charge_steps(cost as i64);
                return Ok(Value::heap(arr));
            }
            let avail0 = width - init_b.len() as isize;
            if (s.len() as isize) <= avail0 {
                let end = if drop_ws {
                    let mut e = s.len();
                    while e > 0 && is_ascii_ws(s[e - 1]) {
                        e -= 1;
                    }
                    e
                } else {
                    s.len()
                };
                if end == 0 {
                    let arr = self.heap.alloc_array_settled(Vec::new());
                    #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                    self.heap.repair_arr_mirror(arr);
                    self.charge_steps(cost as i64);
                    return Ok(Value::heap(arr));
                }
                let mut one = Vec::with_capacity(init_b.len() + end);
                one.extend_from_slice(&init_b);
                one.extend_from_slice(&s[..end]);
                let s_str = ascii_bytes_as_str(&one);
                let s_idx = self.heap.alloc_str_copy(s_str);
                let mut items = self.heap.take_arr_buf(1);
                items.push(Value::heap(s_idx));
                let arr = self.heap.alloc_array_settled(items);
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                self.heap.repair_arr_mirror(arr);
                self.charge_steps(cost as i64);
                return Ok(Value::heap(arr));
            }

            let mut chunks: Vec<(&[u8], bool)> = Vec::with_capacity((s.len() / 4).max(8));
            let mut i = 0usize;
            let slen = s.len();
            while i < slen {
                let start = i;
                if is_ascii_ws(s[i]) {
                    i += 1;
                    while i < slen && is_ascii_ws(s[i]) {
                        i += 1;
                    }
                    chunks.push((&s[start..i], true));
                } else {
                    i += 1;
                    while i < slen && !is_ascii_ws(s[i]) {
                        i += 1;
                    }
                    if break_hyphens {
                        let mut seg = start;
                        for j in start..i {
                            if s[j] == b'-' && j + 1 < i && s[j + 1] != b'-' {
                                chunks.push((&s[seg..=j], false));
                                seg = j + 1;
                            }
                        }
                        if seg < i {
                            chunks.push((&s[seg..i], false));
                        }
                    } else {
                        chunks.push((&s[start..i], false));
                    }
                }
            }
            chunks.reverse();

            let mut lines: Vec<Value> = Vec::with_capacity((slen / (width as usize).max(1)).max(4));
            let mut cur_line: Vec<(&[u8], bool)> = Vec::with_capacity(16);
            let mut scratch: Vec<u8> = Vec::with_capacity((width as usize).min(256) + 16);
            while !chunks.is_empty() {
                cur_line.clear();
                let mut cur_len = 0isize;
                let indent: &[u8] = if lines.is_empty() { &init_b } else { &sub_b };
                let avail = width - indent.len() as isize;
                if drop_ws && !lines.is_empty() && chunks.last().is_some_and(|c| c.1) {
                    chunks.pop();
                }
                while let Some(&(top, top_ws)) = chunks.last() {
                    let l = top.len() as isize;
                    if cur_len + l <= avail {
                        chunks.pop();
                        cur_line.push((top, top_ws));
                        cur_len += l;
                    } else {
                        break;
                    }
                }
                if let Some(&(top, top_ws)) = chunks.last() {
                    if (top.len() as isize) > avail {
                        let space_left = avail - cur_len;
                        if break_long && space_left > 0 {
                            let k = space_left as usize;
                            cur_line.push((&top[..k], top_ws));
                            chunks.last_mut().unwrap().0 = &top[k..];
                        } else if cur_line.is_empty() {
                            if break_long && avail > 0 {
                                let k = avail as usize;
                                cur_line.push((&top[..k], top_ws));
                                chunks.last_mut().unwrap().0 = &top[k..];
                            } else {
                                cur_line.push(chunks.pop().unwrap());
                            }
                        }
                    }
                }
                if drop_ws && cur_line.last().is_some_and(|c| c.1) {
                    cur_line.pop();
                }
                if !cur_line.is_empty() {
                    let fits_max = match max_lines {
                        None => true,
                        Some(ml) => {
                            lines.len() + 1 < ml
                                || ((chunks.is_empty() || (drop_ws && chunks.len() == 1 && chunks[0].1))
                                    && cur_len <= avail)
                        }
                    };
                    if fits_max {
                        scratch.clear();
                        scratch.extend_from_slice(indent);
                        for &(seg, _) in &cur_line {
                            scratch.extend_from_slice(seg);
                        }
                        let s_str = ascii_bytes_as_str(&scratch);
                        let s_idx = self.heap.alloc_str_copy(s_str);
                        lines.push(Value::heap(s_idx));
                    } else {
                        let mut emitted = false;
                        while let Some(&(seg, seg_ws)) = cur_line.last() {
                            if !seg_ws && cur_len + (ph_b.len() as isize) <= avail {
                                scratch.clear();
                                scratch.extend_from_slice(indent);
                                for &(cseg, _) in &cur_line {
                                    scratch.extend_from_slice(cseg);
                                }
                                scratch.extend_from_slice(&ph_b);
                                let s_str = ascii_bytes_as_str(&scratch);
                                let s_idx = self.heap.alloc_str_copy(s_str);
                                lines.push(Value::heap(s_idx));
                                emitted = true;
                                break;
                            }
                            cur_len -= seg.len() as isize;
                            cur_line.pop();
                        }
                        if !emitted {
                            let mut pstart = 0usize;
                            while pstart < ph_b.len() && is_ascii_ws(ph_b[pstart]) {
                                pstart += 1;
                            }
                            scratch.clear();
                            scratch.extend_from_slice(indent);
                            scratch.extend_from_slice(&ph_b[pstart..]);
                            let s_str = ascii_bytes_as_str(&scratch);
                            let s_idx = self.heap.alloc_str_copy(s_str);
                            lines.push(Value::heap(s_idx));
                        }
                        break;
                    }
                }
            }
            let arr = self.heap.alloc_array_settled(lines);
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            self.heap.repair_arr_mirror(arr);
            self.charge_steps(cost as i64);
            return Ok(Value::heap(arr));
        }

        // General BMP UTF-8 path (any non-BMP astral char returns undefined -> JS fallback).
        let (raw_chars, init_chars, sub_chars, ph_chars) = {
            let (
                HeapObj::Str(ts),
                HeapObj::Str(is),
                HeapObj::Str(ss),
                HeapObj::Str(ps),
            ) = (
                self.heap.get(ti),
                self.heap.get(ii),
                self.heap.get(si),
                self.heap.get(pi),
            )
            else {
                return Ok(Value::UNDEFINED);
            };
            let to_bmp = |b: &[u8]| -> Option<Vec<char>> {
                let st = std::str::from_utf8(b).ok()?;
                let mut v = Vec::with_capacity(st.len());
                for ch in st.chars() {
                    if (ch as u32) > 0xFFFF {
                        return None;
                    }
                    v.push(ch);
                }
                Some(v)
            };
            let (Some(tc), Some(ic), Some(sc), Some(pc)) = (
                to_bmp(ts.as_bytes()),
                to_bmp(is.as_bytes()),
                to_bmp(ss.as_bytes()),
                to_bmp(ps.as_bytes()),
            ) else {
                return Ok(Value::UNDEFINED);
            };
            (tc, ic, sc, pc)
        };

        let mut chars = if expand_tabs && raw_chars.contains(&'\t') {
            let mut out = Vec::with_capacity(raw_chars.len() + 16);
            let mut col = 0isize;
            for ch in raw_chars {
                if ch == '\t' {
                    let pad = if tabsize > 0 { (tabsize - (col % tabsize)) as usize } else { 0 };
                    out.resize(out.len() + pad, ' ');
                    col += pad as isize;
                } else {
                    out.push(ch);
                    col = if ch == '\n' || ch == '\r' { 0 } else { col + 1 };
                }
            }
            out
        } else {
            raw_chars
        };
        if replace_ws {
            for ch in &mut chars {
                let u = *ch as u32;
                if (9..=13).contains(&u) {
                    *ch = ' ';
                }
            }
        }
        if chars.is_empty() {
            let arr = self.heap.alloc_array_settled(Vec::new());
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            self.heap.repair_arr_mirror(arr);
            self.charge_steps(cost as i64);
            return Ok(Value::heap(arr));
        }
        let avail0 = width - init_chars.len() as isize;
        if (chars.len() as isize) <= avail0 {
            let end = if drop_ws {
                let mut e = chars.len();
                while e > 0 && is_wrap_ws_char(chars[e - 1]) {
                    e -= 1;
                }
                e
            } else {
                chars.len()
            };
            if end == 0 {
                let arr = self.heap.alloc_array_settled(Vec::new());
                #[cfg(all(feature = "jit", target_arch = "x86_64"))]
                self.heap.repair_arr_mirror(arr);
                self.charge_steps(cost as i64);
                return Ok(Value::heap(arr));
            }
            let mut sbuf = String::with_capacity(init_chars.len() + end);
            for &ch in &init_chars {
                sbuf.push(ch);
            }
            for &ch in &chars[..end] {
                sbuf.push(ch);
            }
            let s_idx = self.heap.alloc_str_copy(&sbuf);
            let mut items = self.heap.take_arr_buf(1);
            items.push(Value::heap(s_idx));
            let arr = self.heap.alloc_array_settled(items);
            #[cfg(all(feature = "jit", target_arch = "x86_64"))]
            self.heap.repair_arr_mirror(arr);
            self.charge_steps(cost as i64);
            return Ok(Value::heap(arr));
        }

        let slen = chars.len();
        let mut chunks: Vec<(&[char], bool)> = Vec::with_capacity((slen / 4).max(8));
        let mut i = 0usize;
        while i < slen {
            let start = i;
            if is_wrap_ws_char(chars[i]) {
                i += 1;
                while i < slen && is_wrap_ws_char(chars[i]) {
                    i += 1;
                }
                chunks.push((&chars[start..i], true));
            } else {
                i += 1;
                while i < slen && !is_wrap_ws_char(chars[i]) {
                    i += 1;
                }
                if break_hyphens {
                    let mut seg = start;
                    for j in start..i {
                        if chars[j] == '-' && j + 1 < i && chars[j + 1] != '-' {
                            chunks.push((&chars[seg..=j], false));
                            seg = j + 1;
                        }
                    }
                    if seg < i {
                        chunks.push((&chars[seg..i], false));
                    }
                } else {
                    chunks.push((&chars[start..i], false));
                }
            }
        }
        chunks.reverse();

        let mut lines: Vec<Value> = Vec::with_capacity((slen / (width as usize).max(1)).max(4));
        let mut cur_line: Vec<(&[char], bool)> = Vec::with_capacity(16);
        let mut sbuf = String::with_capacity((width as usize).min(256) + 16);
        while !chunks.is_empty() {
            cur_line.clear();
            let mut cur_len = 0isize;
            let indent: &[char] = if lines.is_empty() { &init_chars } else { &sub_chars };
            let avail = width - indent.len() as isize;
            if drop_ws && !lines.is_empty() && chunks.last().is_some_and(|c| c.1) {
                chunks.pop();
            }
            while let Some(&(top, top_ws)) = chunks.last() {
                let l = top.len() as isize;
                if cur_len + l <= avail {
                    chunks.pop();
                    cur_line.push((top, top_ws));
                    cur_len += l;
                } else {
                    break;
                }
            }
            if let Some(&(top, top_ws)) = chunks.last() {
                if (top.len() as isize) > avail {
                    let space_left = avail - cur_len;
                    if break_long && space_left > 0 {
                        let k = space_left as usize;
                        cur_line.push((&top[..k], top_ws));
                        chunks.last_mut().unwrap().0 = &top[k..];
                    } else if cur_line.is_empty() {
                        if break_long && avail > 0 {
                            let k = avail as usize;
                            cur_line.push((&top[..k], top_ws));
                            chunks.last_mut().unwrap().0 = &top[k..];
                        } else {
                            cur_line.push(chunks.pop().unwrap());
                        }
                    }
                }
            }
            if drop_ws && cur_line.last().is_some_and(|c| c.1) {
                cur_line.pop();
            }
            if !cur_line.is_empty() {
                let fits_max = match max_lines {
                    None => true,
                    Some(ml) => {
                        lines.len() + 1 < ml
                            || ((chunks.is_empty() || (drop_ws && chunks.len() == 1 && chunks[0].1))
                                && cur_len <= avail)
                    }
                };
                if fits_max {
                    sbuf.clear();
                    for &ch in indent {
                        sbuf.push(ch);
                    }
                    for &(seg, _) in &cur_line {
                        for &ch in seg {
                            sbuf.push(ch);
                        }
                    }
                    let s_idx = self.heap.alloc_str_copy(&sbuf);
                    lines.push(Value::heap(s_idx));
                } else {
                    let mut emitted = false;
                    while let Some(&(seg, seg_ws)) = cur_line.last() {
                        if !seg_ws && cur_len + (ph_chars.len() as isize) <= avail {
                            sbuf.clear();
                            for &ch in indent {
                                sbuf.push(ch);
                            }
                            for &(cseg, _) in &cur_line {
                                for &ch in cseg {
                                    sbuf.push(ch);
                                }
                            }
                            for &ch in &ph_chars {
                                sbuf.push(ch);
                            }
                            let s_idx = self.heap.alloc_str_copy(&sbuf);
                            lines.push(Value::heap(s_idx));
                            emitted = true;
                            break;
                        }
                        cur_len -= seg.len() as isize;
                        cur_line.pop();
                    }
                    if !emitted {
                        let mut pstart = 0usize;
                        while pstart < ph_chars.len() && is_wrap_ws_char(ph_chars[pstart]) {
                            pstart += 1;
                        }
                        sbuf.clear();
                        for &ch in indent {
                            sbuf.push(ch);
                        }
                        for &ch in &ph_chars[pstart..] {
                            sbuf.push(ch);
                        }
                        let s_idx = self.heap.alloc_str_copy(&sbuf);
                        lines.push(Value::heap(s_idx));
                    }
                    break;
                }
            }
        }
        let arr = self.heap.alloc_array_settled(lines);
        #[cfg(all(feature = "jit", target_arch = "x86_64"))]
        self.heap.repair_arr_mirror(arr);
        self.charge_steps(cost as i64);
        Ok(Value::heap(arr))
    }
}
