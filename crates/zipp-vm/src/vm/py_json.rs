//! The Python runtime's `json` fast paths, native (`__zipp_py_json`).
//!
//! `json.dumps` and `json.loads` are implemented in the runtime
//! (`runtime/stdlib.js`); this native does the same work for the documents
//! that need nothing but plain values, and answers `undefined` for anything
//! else, whereupon the runtime runs its own code, which then produces
//! exactly what it always did (every error included):
//!
//! * `__zipp_py_json(0, value, indent, sep, kv, flags, T.list, T.tuple,
//!   T.dict)`: the text `dumps` makes of `value` with those options
//!   (`indent` a str or `null`; `flags` bit 0 `sort_keys`, bit 1
//!   `ensure_ascii`, bit 2 `allow_nan`), when `value` is built only of
//!   None, bools, ints, floats, strs, exact lists and tuples, and exact
//!   dicts with only str keys, without a cycle. A non-finite float with
//!   `allow_nan` false, anything else (which `default=` or an error
//!   answers), or a nesting deeper than [`MAX_DEPTH`] gives `undefined`.
//! * `__zipp_py_json(1, text, list, dict)`: the value `loads` makes of
//!   `text` (no hooks), when `text` is well-formed JSON without the
//!   `NaN`/`Infinity` constants and every int fits 38 digits (the runtime
//!   takes the others); any error gives `undefined`. `list` and `dict` are
//!   an empty list and an empty dict the runtime made (`list([])`,
//!   `dict()`), never handed out: each list and dict made here is a copy
//!   of one of them (so it has exactly their layout) with its own items
//!   or Map.
//!
//! The encoder's pieces are the runtime's own: strings are quoted as
//! `JSON.stringify` quotes them (the engine's quoting routine) and then, for
//! `ensure_ascii`, every UTF-16 unit outside `\x20-\x7e` becomes `\uXXXX`;
//! floats are `rt.floatRepr` over the engine's shortest round-trip digits;
//! containers are wrapped as `wrap` wraps them. The decoder builds the
//! records `dict()` and `list()` build (a dict's storage a `PyTable`), with
//! `dictSet`'s semantics for a repeated key (first position, last value).

use super::*;
use crate::heap::{HeapObj, JsStr, ObjMap};
use crate::value::Value;

/// Deepest nesting either direction handles; deeper documents take the
/// runtime's path (and its errors).
const MAX_DEPTH: usize = 256;
/// Largest text either direction produces or accepts here.
const MAX_TEXT: usize = 1 << 26;
/// The runtime's sequence limit (`MAX_ITEMS` in `runtime/core.js`).
const MAX_ITEMS: usize = 1 << 24;
/// Instruction steps charged per byte of text and per value.
const STEPS_PER_BYTE: u64 = 1;
const STEPS_PER_VALUE: u64 = 8;

struct Enc {
    indent: Option<String>,
    sep: String,
    kv: String,
    sort_keys: bool,
    ensure_ascii: bool,
    allow_nan: bool,
    list: Value,
    tuple: Value,
    dict: Value,
    /// Containers being encoded (the runtime's circular-reference set).
    stack: Vec<u32>,
    values: u64,
}

/// Python's `repr(float)` of a finite `x` (`rt.floatRepr`).
pub(super) fn py_float_repr(out: &mut String, x: f64) {
    if x == 0.0 {
        out.push_str(if x.is_sign_negative() { "-0.0" } else { "0.0" });
        return;
    }
    if x < 0.0 {
        out.push('-');
    }
    let (digits, exp) = super::helpers_num2::shortest_digits(x.abs());
    if !(-4..16).contains(&exp) {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        let e = exp.unsigned_abs();
        if e < 10 {
            out.push('0');
        }
        out.push_str(&e.to_string());
        return;
    }
    if exp >= 0 {
        let whole = exp as usize + 1;
        if digits.len() <= whole {
            out.push_str(&digits);
            for _ in digits.len()..whole {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..whole]);
            out.push('.');
            out.push_str(&digits[whole..]);
        }
        return;
    }
    out.push_str("0.");
    for _ in 0..(-exp - 1) {
        out.push('0');
    }
    out.push_str(&digits);
}

/// `quote(s, ensureAscii)`: `JSON.stringify(s)`, then, for `ensure_ascii`,
/// each UTF-16 unit outside `\x20-\x7e` as `\uXXXX` (lowercase hex).
fn quote(out: &mut String, bytes: &[u8], ascii: bool, ensure_ascii: bool) {
    if !ensure_ascii || (ascii && !bytes.contains(&0x7f)) {
        super::helpers_json::json_quote_wtf8_into(out, bytes, ascii);
        return;
    }
    let mut q = String::new();
    super::helpers_json::json_quote_wtf8_into(&mut q, bytes, ascii);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for c in q.chars() {
        if (' '..='~').contains(&c) {
            out.push(c);
            continue;
        }
        let mut units = [0u16; 2];
        for &u in c.encode_utf16(&mut units).iter() {
            out.push_str("\\u");
            for sh in [12u32, 8, 4, 0] {
                out.push(HEX[((u as u32 >> sh) & 0xF) as usize] as char);
            }
        }
    }
}

impl<'p> Vm<'p> {
    /// `__zipp_py_json(op, ...)`: see the module comment.
    pub(crate) fn py_json(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        let op = arg(0);
        if op == Value::int(0) {
            return Ok(self.py_json_dumps(args).unwrap_or(Value::UNDEFINED));
        }
        if op == Value::int(1) {
            return Ok(self.py_json_loads(arg(1), arg(2), arg(3)).unwrap_or(Value::UNDEFINED));
        }
        Ok(Value::UNDEFINED)
    }

    /// A flat string's bytes, owned (`None` for a non-string).
    fn py_json_text(&mut self, v: Value) -> Option<String> {
        if !v.is_heap() || !self.heap.is_str_like(v.heap_index()) {
            return None;
        }
        let idx = v.heap_index();
        self.heap.flatten(idx);
        match self.heap.get(idx) {
            HeapObj::Str(s) => String::from_utf8(s.as_bytes().to_vec()).ok(),
            _ => None,
        }
    }

    fn py_json_dumps(&mut self, args: &[Value]) -> Option<Value> {
        let arg = |i: usize| args.get(i).copied().unwrap_or(Value::UNDEFINED);
        let indent = match arg(2) {
            v if v == Value::NULL => None,
            v => Some(self.py_json_text(v)?),
        };
        let sep = self.py_json_text(arg(3))?;
        let kv = self.py_json_text(arg(4))?;
        let flags = arg(5);
        if !flags.is_int() {
            return None;
        }
        let flags = flags.as_int();
        let mut enc = Enc {
            indent,
            sep,
            kv,
            sort_keys: flags & 1 != 0,
            ensure_ascii: flags & 2 != 0,
            allow_nan: flags & 4 != 0,
            list: arg(6),
            tuple: arg(7),
            dict: arg(8),
            stack: Vec::new(),
            values: 0,
        };
        if !self.native_kernel_admits(0, 0) {
            return None;
        }
        let mut out = String::new();
        self.py_json_encode(&mut enc, &mut out, arg(1), 0)?;
        let cost = (out.len() as u64)
            .saturating_mul(STEPS_PER_BYTE)
            .saturating_add(enc.values.saturating_mul(STEPS_PER_VALUE));
        if !self.native_kernel_admits(cost, out.len().saturating_mul(2)) {
            return None;
        }
        self.charge_steps(i64::try_from(cost).unwrap_or(i64::MAX));
        Some(Value::heap(self.heap.alloc_str(out)))
    }

    /// A template record (see the module comment): a plain object whose own
    /// data properties are exactly `keys`, in order; a copy of its map and
    /// the slot of `keys[1]`.
    pub(super) fn py_json_template(&self, v: Value, keys: &[&str]) -> Option<(ObjMap, usize)> {
        if !v.is_heap() {
            return None;
        }
        let HeapObj::Object(m) = self.heap.get(v.heap_index()) else {
            return None;
        };
        if m.is_ctor || !m.extensible || m.len() != keys.len() {
            return None;
        }
        for (i, k) in keys.iter().enumerate() {
            if m.key_at(i) != *k || m.attr_at(i).accessor {
                return None;
            }
        }
        Some(((**m).clone(), 1))
    }

    /// A plain record's own data property.
    pub(super) fn py_json_field(&self, idx: u32, key: &str) -> Option<Value> {
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
        Some(m.val_at(slot))
    }

    fn py_json_encode(&mut self, enc: &mut Enc, out: &mut String, v: Value, level: usize) -> Option<()> {
        if out.len() > MAX_TEXT || level > MAX_DEPTH {
            return None;
        }
        enc.values += 1;
        if v == Value::NULL {
            out.push_str("null");
            return Some(());
        }
        if v == Value::TRUE {
            out.push_str("true");
            return Some(());
        }
        if v == Value::FALSE {
            out.push_str("false");
            return Some(());
        }
        if v.is_number() {
            let x = v.as_f64();
            if x.is_finite() {
                py_float_repr(out, x);
            } else if enc.allow_nan {
                out.push_str(if x.is_nan() {
                    "NaN"
                } else if x > 0.0 {
                    "Infinity"
                } else {
                    "-Infinity"
                });
            } else {
                return None;
            }
            return Some(());
        }
        if let Some(n) = v.small_bigint_val() {
            out.push_str(&n.to_string());
            return Some(());
        }
        if !v.is_heap() {
            return None;
        }
        let idx = v.heap_index();
        if self.heap.is_str_like(idx) {
            self.heap.flatten(idx);
            let HeapObj::Str(s) = self.heap.get(idx) else {
                return None;
            };
            quote(out, s.as_bytes(), s.is_ascii(), enc.ensure_ascii);
            return Some(());
        }
        match self.heap.get(idx) {
            HeapObj::BigInt(n) => {
                out.push_str(&n.to_string());
                return Some(());
            }
            HeapObj::BigIntBig(b) => {
                out.push_str(&b.to_string());
                return Some(());
            }
            HeapObj::Object(_) => {}
            _ => return None,
        }
        let cls = self.py_json_field(idx, "cls")?;
        if cls.bits() == enc.list.bits() || cls.bits() == enc.tuple.bits() {
            let items = self.py_json_field(idx, "items")?;
            if !items.is_heap() {
                return None;
            }
            let HeapObj::Array(items) = self.heap.get(items.heap_index()) else {
                return None;
            };
            if items.is_empty() {
                out.push_str("[]");
                return Some(());
            }
            let items = items.clone();
            if enc.stack.contains(&idx) {
                return None;
            }
            enc.stack.push(idx);
            out.push('[');
            for (i, item) in items.into_iter().enumerate() {
                if item == Value::HOLE || item.is_undefined() {
                    return None;
                }
                self.py_json_gap(enc, out, i, level);
                self.py_json_encode(enc, out, item, level + 1)?;
            }
            self.py_json_close(enc, out, level);
            out.push(']');
            enc.stack.pop();
            return Some(());
        }
        if cls.bits() != enc.dict.bits() {
            return None;
        }
        let map = self.py_json_field(idx, "map")?;
        let mut entries: Vec<(Value, Value)> = match map.is_heap().then(|| self.heap.get(map.heap_index())) {
            Some(HeapObj::PyTable(t)) => t.entries().map(|(_, k, v)| (k, v)).collect(),
            _ => return None,
        };
        if entries.is_empty() {
            out.push_str("{}");
            return Some(());
        }
        for &(k, _) in &entries {
            if !k.is_heap() || !self.heap.is_str_like(k.heap_index()) {
                return None;
            }
            self.heap.flatten(k.heap_index());
            if !matches!(self.heap.get(k.heap_index()), HeapObj::Str(_)) {
                return None;
            }
        }
        if enc.stack.contains(&idx) {
            return None;
        }
        if enc.sort_keys {
            // Code-point order: the order of the WTF-8 bytes.
            let heap = &self.heap;
            let bytes = |k: Value| match heap.get(k.heap_index()) {
                HeapObj::Str(s) => s.as_bytes(),
                _ => &[],
            };
            entries.sort_by(|a, b| bytes(a.0).cmp(bytes(b.0)));
        }
        enc.stack.push(idx);
        out.push('{');
        for (i, (k, val)) in entries.into_iter().enumerate() {
            self.py_json_gap(enc, out, i, level);
            let HeapObj::Str(s) = self.heap.get(k.heap_index()) else {
                return None;
            };
            quote(out, s.as_bytes(), s.is_ascii(), enc.ensure_ascii);
            out.push_str(&enc.kv);
            self.py_json_encode(enc, out, val, level + 1)?;
        }
        self.py_json_close(enc, out, level);
        out.push('}');
        enc.stack.pop();
        Some(())
    }

    /// Before the `i`th part of a container at `level`: `wrap`'s opening
    /// pad (for the first) or its separator.
    fn py_json_gap(&self, enc: &Enc, out: &mut String, i: usize, level: usize) {
        if i > 0 {
            out.push_str(&enc.sep);
        }
        if let Some(indent) = &enc.indent {
            out.push('\n');
            for _ in 0..=level {
                out.push_str(indent);
            }
        }
    }

    /// `wrap`'s closing pad.
    fn py_json_close(&self, enc: &Enc, out: &mut String, level: usize) {
        if let Some(indent) = &enc.indent {
            out.push('\n');
            for _ in 0..level {
                out.push_str(indent);
            }
        }
    }

    fn py_json_loads(&mut self, text: Value, list: Value, dict: Value) -> Option<Value> {
        if !text.is_heap() || !self.heap.is_str_like(text.heap_index()) {
            return None;
        }
        let idx = text.heap_index();
        self.heap.flatten(idx);
        let src: Vec<u8> = match self.heap.get(idx) {
            HeapObj::Str(s) => s.as_bytes().to_vec(),
            _ => return None,
        };
        if src.len() > MAX_TEXT {
            return None;
        }
        let cost = (src.len() as u64).saturating_mul(STEPS_PER_BYTE + STEPS_PER_VALUE);
        if !self.native_kernel_admits(cost, src.len().saturating_mul(8)) {
            return None;
        }
        let (list_tmpl, list_items) = self.py_json_template(list, &["cls", "items"])?;
        let (dict_tmpl, dict_map) = self.py_json_template(dict, &["cls", "map", "size"])?;
        let kinds = match dict_tmpl.val_at(dict_map) {
            t if t.is_heap() => match self.heap.get(t.heap_index()) {
                HeapObj::PyTable(t) => t.kinds,
                _ => return None,
            },
            _ => return None,
        };
        let mut p = Parser {
            src: &src,
            i: 0,
            list: list_tmpl,
            list_items,
            dict: dict_tmpl,
            dict_map,
            kinds,
            keys: std::collections::HashMap::new(),
            values: 0,
        };
        let v = p.value(self, 0)?;
        p.ws();
        if p.i != src.len() {
            return None;
        }
        let charge = (src.len() as u64)
            .saturating_mul(STEPS_PER_BYTE)
            .saturating_add(p.values.saturating_mul(STEPS_PER_VALUE));
        self.charge_steps(i64::try_from(charge).unwrap_or(i64::MAX));
        Some(v)
    }
}

struct Parser<'s> {
    src: &'s [u8],
    i: usize,
    /// The template records and the slot of their `items` / `map` (a
    /// dict's `size` is the next slot).
    list: ObjMap,
    list_items: usize,
    dict: ObjMap,
    dict_map: usize,
    /// The template dict's table kinds, for every table made here.
    kinds: super::py_table::PyKinds,
    /// Short keys already made in this document, shared (strings are
    /// immutable, and a str's identity is not observable).
    keys: std::collections::HashMap<&'s [u8], Value>,
    values: u64,
}

impl<'s> Parser<'s> {
    fn ws(&mut self) {
        super::helpers_json::json_skip_ws(self.src, &mut self.i);
    }

    fn value(&mut self, vm: &mut Vm<'_>, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.values += 1;
        self.ws();
        let c = *self.src.get(self.i)?;
        match c {
            b'{' => self.object(vm, depth),
            b'[' => self.array(vm, depth),
            b'"' => self.string(vm, false),
            b't' => self.word(b"true", Value::TRUE),
            b'f' => self.word(b"false", Value::FALSE),
            b'n' => self.word(b"null", Value::NULL),
            b'-' | b'0'..=b'9' => self.number(vm),
            _ => None,
        }
    }

    fn word(&mut self, w: &[u8], v: Value) -> Option<Value> {
        if self.src[self.i..].starts_with(w) {
            self.i += w.len();
            Some(v)
        } else {
            None
        }
    }

    /// A JSON number, as the runtime's `NUMBER` pattern reads it: an int
    /// (no fraction, no exponent) is an exact int, anything else a float.
    fn number(&mut self, vm: &mut Vm<'_>) -> Option<Value> {
        let b = self.src;
        let start = self.i;
        let mut i = self.i;
        if b.get(i) == Some(&b'-') {
            i += 1;
        }
        match b.get(i) {
            Some(b'0') => i += 1,
            Some(c) if c.is_ascii_digit() => {
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
            }
            _ => return None,
        }
        let mut float = false;
        if b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
            float = true;
            i += 1;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
        if matches!(b.get(i), Some(b'e' | b'E')) {
            let mut j = i + 1;
            if matches!(b.get(j), Some(b'+' | b'-')) {
                j += 1;
            }
            if b.get(j).is_some_and(u8::is_ascii_digit) {
                float = true;
                i = j;
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
            }
        }
        self.i = i;
        let tok = std::str::from_utf8(&b[start..i]).ok()?;
        if float {
            return tok.parse::<f64>().ok().map(Value::num);
        }
        // At most 38 digits always fits an i128.
        if tok.trim_start_matches('-').len() > 38 {
            return None;
        }
        let n: i128 = tok.parse().ok()?;
        Some(vm.make_bigint(n))
    }

    fn string(&mut self, vm: &mut Vm<'_>, key: bool) -> Option<Value> {
        let start = self.i + 1;
        // A key without escapes: shared by every occurrence in the document.
        if key {
            let mut j = start;
            while let Some(&c) = self.src.get(j) {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                j += 1;
            }
            if self.src.get(j) == Some(&b'"') && j - start <= 64 {
                let k = &self.src[start..j];
                self.i = j + 1;
                if let Some(&v) = self.keys.get(k) {
                    return Some(v);
                }
                let v = Value::heap(vm.heap.alloc_js(JsStr::from_wtf8(k.to_vec())));
                self.keys.insert(k, v);
                return Some(v);
            }
        }
        let s = super::helpers_json::json_parse_string(self.src, &mut self.i).ok()?;
        Some(Value::heap(vm.heap.alloc_js(s)))
    }

    fn array(&mut self, vm: &mut Vm<'_>, depth: usize) -> Option<Value> {
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.src.get(self.i) == Some(&b']') {
            self.i += 1;
        } else {
            loop {
                items.push(self.value(vm, depth + 1)?);
                self.ws();
                match self.src.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b']') => {
                        self.i += 1;
                        break;
                    }
                    _ => return None,
                }
            }
        }
        if items.len() > MAX_ITEMS {
            return None;
        }
        let arr = Value::heap(vm.heap.alloc(HeapObj::Array(items)));
        let mut rec = self.list.clone();
        rec.set_val_at(self.list_items, arr);
        Some(vm.alloc_object_current_realm(rec))
    }

    fn object(&mut self, vm: &mut Vm<'_>, depth: usize) -> Option<Value> {
        self.i += 1;
        let mut table = super::py_table::PyTable::with_kinds(false, self.kinds);
        let mut steps = 0;
        self.ws();
        if self.src.get(self.i) == Some(&b'}') {
            self.i += 1;
        } else {
            loop {
                self.ws();
                if self.src.get(self.i) != Some(&b'"') {
                    return None;
                }
                let k = self.string(vm, true)?;
                self.ws();
                if self.src.get(self.i) != Some(&b':') {
                    return None;
                }
                self.i += 1;
                let v = self.value(vm, depth + 1)?;
                // `dictSet`: a repeated key keeps its first position and
                // takes the last value.
                let kinds = self.kinds;
                super::py_table::native_set_item(&vm.heap, &kinds, &mut table, k, v, &mut steps).ok()?;
                self.ws();
                match self.src.get(self.i) {
                    Some(b',') => self.i += 1,
                    Some(b'}') => {
                        self.i += 1;
                        break;
                    }
                    _ => return None,
                }
            }
        }
        let size = table.len();
        self.values += steps / super::py_table::STEPS_PER_ENTRY;
        let map = Value::heap(vm.heap.alloc(HeapObj::PyTable(Box::new(table))));
        let mut rec = self.dict.clone();
        rec.set_val_at(self.dict_map, map);
        rec.set_val_at(self.dict_map + 1, Value::num(size as f64));
        Some(vm.alloc_object_current_realm(rec))
    }
}
