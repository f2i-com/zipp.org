//! The Python runtime's dict key of a tuple, native (`__zipp_py_tkey`).
//!
//! A dict holding a key other than a str files it under a bucket key
//! (`keyOf` in `runtime/types.js`); a tuple's is `"\0" + baseKey(tuple)`,
//! the text `"t(" + keyStr(item) + "," + ... + ")"`. This native builds that
//! text for a tuple of plain items, and answers `undefined` for any other
//! tuple, whereupon the runtime builds it itself:
//!
//! `__zipp_py_tkey(tuple, T.tuple)`, each item (as `keyStr` renders it)
//! a str (`"s" + str`), a bool (`"n1"` / `"n0"`), an int of at most 2^53-1
//! in magnitude (`"n" + digits`), a float (`"f"` for a NaN, else `"n"` +
//! the JavaScript text of the number, except an integral one beyond 2^53-1),
//! `None` (`"N"`) or such a tuple (its `baseKey` text), nested at most
//! [`MAX_DEPTH`] deep.

use super::*;
use crate::heap::{HeapObj, JsStr};
use crate::value::Value;

const MAX_DEPTH: u32 = 32;
/// `Number.MAX_SAFE_INTEGER`.
const SAFE: i128 = 9_007_199_254_740_991;

impl<'p> Vm<'p> {
    /// `__zipp_py_tkey(tuple, T.tuple)`: see the module comment.
    pub(crate) fn py_tkey(&mut self, args: &[Value]) -> Result<Value, Thrown> {
        let v = args.first().copied().unwrap_or(Value::UNDEFINED);
        let tuple = args.get(1).copied().unwrap_or(Value::UNDEFINED);
        let mut out = vec![0u8];
        if self.py_tkey_into(&mut out, v, tuple, 0).is_none() {
            return Ok(Value::UNDEFINED);
        }
        self.charge_steps(i64::try_from(out.len()).unwrap_or(i64::MAX));
        Ok(Value::heap(self.heap.alloc_js(JsStr::from_wtf8(out))))
    }

    /// `baseKey(v)` of a tuple record `v`, appended.
    fn py_tkey_into(&mut self, out: &mut Vec<u8>, v: Value, tuple: Value, depth: u32) -> Option<()> {
        if depth > MAX_DEPTH || !v.is_heap() {
            return None;
        }
        let items = {
            let HeapObj::Object(m) = self.heap.get(v.heap_index()) else {
                return None;
            };
            if m.is_ctor {
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
        };
        out.extend_from_slice(b"t(");
        for x in items {
            self.py_tkey_item(out, x, tuple, depth)?;
            out.push(b',');
            if out.len() > (1 << 24) {
                return None;
            }
        }
        out.push(b')');
        Some(())
    }

    /// `keyStr(x)`, appended.
    fn py_tkey_item(&mut self, out: &mut Vec<u8>, x: Value, tuple: Value, depth: u32) -> Option<()> {
        if x == Value::NULL {
            out.push(b'N');
            return Some(());
        }
        if x == Value::TRUE || x == Value::FALSE {
            out.extend_from_slice(if x == Value::TRUE { b"n1" } else { b"n0" });
            return Some(());
        }
        if x.is_number() {
            let f = x.as_f64();
            if f.is_nan() {
                out.push(b'f');
                return Some(());
            }
            if f.fract() == 0.0 && f.abs() > SAFE as f64 {
                return None;
            }
            let mut text = String::from("n");
            super::helpers_num2::fmt_f64_into(&mut text, f);
            out.extend_from_slice(text.as_bytes());
            return Some(());
        }
        if !x.is_heap() {
            return None;
        }
        let idx = x.heap_index();
        if self.heap.is_str_like(idx) {
            self.heap.flatten(idx);
            let HeapObj::Str(s) = self.heap.get(idx) else {
                return None;
            };
            out.push(b's');
            out.extend_from_slice(s.as_bytes());
            return Some(());
        }
        match self.heap.get(idx) {
            HeapObj::BigInt(n) => {
                if n.unsigned_abs() > SAFE as u128 {
                    return None;
                }
                out.push(b'n');
                out.extend_from_slice(n.to_string().as_bytes());
                Some(())
            }
            HeapObj::Object(_) => self.py_tkey_into(out, x, tuple, depth + 1),
            _ => None,
        }
    }
}
