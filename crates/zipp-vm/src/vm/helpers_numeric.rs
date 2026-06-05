#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

/// Encode `f` (already ToNumber'd) into a TypedArray element's little-endian
/// bytes per the element `kind` (JS ToInt8/ToUint8/clamp/… modular reduction;
/// Rust's `as` saturates, so reduce via `rem_euclid` first). BigInt kinds are
/// encoded by the caller.
pub(crate) fn ta_encode(kind: u8, f: f64) -> [u8; 8] {
    let mut out = [0u8; 8];
    match kind {
        0 | 1 => out[0] = to_uint_modular(f, 8) as u8,
        2 => out[0] = clamp_u8(f),
        3 | 4 => out[..2].copy_from_slice(&(to_uint_modular(f, 16) as u16).to_le_bytes()),
        5 | 6 => out[..4].copy_from_slice(&(to_uint_modular(f, 32) as u32).to_le_bytes()),
        7 => out[..4].copy_from_slice(&(f as f32).to_le_bytes()),
        8 => out.copy_from_slice(&f.to_le_bytes()),
        _ => {}
    }
    out
}

/// JS ToUintN modular reduction (the low `bits` bits of trunc(f)), NaN/±∞ → 0.
pub(crate) fn to_uint_modular(f: f64, bits: u32) -> u64 {
    if !f.is_finite() {
        return 0;
    }
    let m = 2f64.powi(bits as i32);
    f.trunc().rem_euclid(m) as u64
}

/// JS ToUint8Clamp: clamp to [0,255] with round-half-to-even.
pub(crate) fn clamp_u8(f: f64) -> u8 {
    if f.is_nan() || f <= 0.0 {
        return 0;
    }
    if f >= 255.0 {
        return 255;
    }
    let fl = f.floor();
    let diff = f - fl;
    let r = if diff < 0.5 {
        fl
    } else if diff > 0.5 {
        fl + 1.0
    } else if (fl as u64) % 2 == 0 {
        fl
    } else {
        fl + 1.0
    };
    r as u8
}

/// Convert a byte offset into `s` to a char offset (regress reports byte offsets;
/// our string indexing is char-based). Identity for ASCII.
pub(crate) fn byte_to_char(s: &str, byte: usize) -> usize {
    let b = byte.min(s.len());
    s[..b].chars().count()
}

/// Convert a char offset into `s` to a byte offset (for seeking regress).
pub(crate) fn char_to_byte(s: &str, ch: usize) -> usize {
    s.char_indices().nth(ch).map(|(b, _)| b).unwrap_or(s.len())
}

/// Format an i128 BigInt in the given radix (2..=36), lowercase digits.
pub(crate) fn bigint_to_radix(n: i128, radix: u32) -> String {
    if radix == 10 {
        return n.to_string();
    }
    if n == 0 {
        return "0".to_string();
    }
    let neg = n < 0;
    let mut m = (n as i128).unsigned_abs();
    let r = radix as u128;
    let mut digits = Vec::new();
    while m > 0 {
        let d = (m % r) as u32;
        digits.push(std::char::from_digit(d, radix).unwrap());
        m /= r;
    }
    if neg {
        digits.push('-');
    }
    digits.iter().rev().collect()
}

/// `BigInt.asUintN(bits, x)`: x mod 2^bits as a non-negative value (i128-limited).
pub(crate) fn bigint_as_uintn(bits: u32, x: i128) -> i128 {
    if bits == 0 {
        return 0;
    }
    if bits >= 127 {
        return x; // beyond the i128 representable mask — pass through (approx)
    }
    x & ((1i128 << bits) - 1)
}

/// `BigInt.asIntN(bits, x)`: x mod 2^bits as a signed bits-bit value.
pub(crate) fn bigint_as_intn(bits: u32, x: i128) -> i128 {
    if bits == 0 {
        return 0;
    }
    if bits >= 127 {
        return x;
    }
    let m = x & ((1i128 << bits) - 1);
    let half = 1i128 << (bits - 1);
    if m >= half {
        m - (1i128 << bits)
    } else {
        m
    }
}

#[inline]
/// A key hidden from STRING enumeration (for-in, Object.keys/values/entries,
/// getOwnPropertyNames, JSON): a private name (`#name`) or a symbol's internal
/// key (`@@iterator`, `@@sym:N`). Symbol keys are still reachable by
/// getOwnPropertyDescriptor and surfaced by getOwnPropertySymbols.
pub(crate) fn is_hidden_key(k: &str) -> bool {
    k.starts_with('#') || k.starts_with("@@")
}

pub(crate) fn len_value(n: usize) -> Value {
    if n <= i32::MAX as usize {
        Value::int(n as i32)
    } else {
        Value::num(n as f64)
    }
}

/// JS `parseInt(s, radix)`: skip leading whitespace, an optional sign, an
/// optional `0x` prefix (radix 16), then digits in `radix` (default 10); stop at
/// the first invalid digit. `NaN` if no digits parse. `radix == 0` means "auto".
pub(crate) fn parse_int(s: &str, radix: i32) -> f64 {
    let b = s.trim_start().as_bytes();
    let mut i = 0;
    let mut sign = 1.0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        if b[i] == b'-' {
            sign = -1.0;
        }
        i += 1;
    }
    let mut radix = radix;
    if (radix == 16 || radix == 0)
        && i + 1 < b.len()
        && b[i] == b'0'
        && (b[i + 1] == b'x' || b[i + 1] == b'X')
    {
        i += 2;
        radix = 16;
    }
    if radix == 0 {
        radix = 10;
    }
    if !(2..=36).contains(&radix) {
        return f64::NAN;
    }
    let start = i;
    let mut val = 0.0;
    while i < b.len() {
        let d = match b[i] {
            c @ b'0'..=b'9' => (c - b'0') as i32,
            c @ b'a'..=b'z' => (c - b'a' + 10) as i32,
            c @ b'A'..=b'Z' => (c - b'A' + 10) as i32,
            _ => break,
        };
        if d >= radix {
            break;
        }
        val = val * radix as f64 + d as f64;
        i += 1;
    }
    if i == start {
        f64::NAN
    } else {
        sign * val
    }
}

/// JS `parseFloat(s)`: skip leading whitespace, then parse the longest leading
/// decimal-float prefix (sign, digits, `.`, exponent, or `Infinity`). `NaN` if
/// none.
pub(crate) fn parse_float(s: &str) -> f64 {
    let t = s.trim_start();
    let b = t.as_bytes();
    let mut end = 0;
    if end < b.len() && (b[end] == b'+' || b[end] == b'-') {
        end += 1;
    }
    if t[end..].starts_with("Infinity") {
        return if t.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let mut saw_digit = false;
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
        saw_digit = true;
    }
    if end < b.len() && b[end] == b'.' {
        end += 1;
        while end < b.len() && b[end].is_ascii_digit() {
            end += 1;
            saw_digit = true;
        }
    }
    if !saw_digit {
        return f64::NAN;
    }
    // Optional exponent — only consumed if it has at least one digit.
    if end < b.len() && (b[end] == b'e' || b[end] == b'E') {
        let mut e = end + 1;
        if e < b.len() && (b[e] == b'+' || b[e] == b'-') {
            e += 1;
        }
        let exp_start = e;
        while e < b.len() && b[e].is_ascii_digit() {
            e += 1;
        }
        if e > exp_start {
            end = e;
        }
    }
    t[..end].parse::<f64>().unwrap_or(f64::NAN)
}

/// A non-negative array index from a numeric key, coercing an integral double
/// the way JS does (`a[1.0]` is `a[1]`). `None` for a negative, non-integral, or
/// non-numeric key (those address no dense element → `undefined`). The JIT region
/// computes loop counters as f64, so `a[i]` arrives here with a double key.
#[inline]
/// Indices into `keys` in spec **OrdinaryOwnPropertyKeys** order: integer-index
/// keys (canonical array indices `0..2^32-1`, e.g. "0"/"7" but not "01"/"-1")
/// first in ascending numeric order, then every other key in its original
/// (insertion) order. Symbols/private keys keep their relative position among
/// "the rest"; callers filter hidden keys separately.
pub(crate) fn spec_key_order(keys: &[String]) -> Vec<usize> {
    let mut ints: Vec<(u32, usize)> = Vec::new();
    let mut rest: Vec<usize> = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        match k.parse::<u32>() {
            Ok(n) if n != u32::MAX && n.to_string() == *k => ints.push((n, i)),
            _ => rest.push(i),
        }
    }
    if ints.is_empty() {
        return rest; // common fast path: no integer keys → original order
    }
    ints.sort_unstable_by_key(|&(n, _)| n);
    ints.into_iter().map(|(_, i)| i).chain(rest).collect()
}

/// A canonical non-negative integer property key as a usize index — "0", "1",
/// "10" but NOT "00", "01", "-1", "1.5", "" or " 1" (no leading zeros / sign /
/// fraction / whitespace). Mirrors the array-index canonicality used by
/// `spec_key_order`. Used to index strings/arrays by a string key (`s["0"]`).
pub(crate) fn canonical_index_str(k: &str) -> Option<usize> {
    let i: usize = k.parse().ok()?;
    (i.to_string() == k).then_some(i)
}

pub(crate) fn array_index(key: Value) -> Option<usize> {
    if key.is_int() {
        let i = key.as_int();
        (i >= 0).then_some(i as usize)
    } else if key.is_double() {
        let d = key.as_f64();
        // Reject negatives, fractions, and absurdly large indices (≥ 2^32).
        if d >= 0.0 && d.fract() == 0.0 && d < 4_294_967_296.0 {
            Some(d as usize)
        } else {
            None
        }
    } else {
        None
    }
}

