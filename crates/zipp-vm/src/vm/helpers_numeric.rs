#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
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
        11 => out[..2].copy_from_slice(&crate::vm::helpers_num2::f64_to_f16_bits(f).to_le_bytes()),
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
/// getOwnPropertyNames, JSON): a symbol's internal key (`@@iterator`,
/// `@@sym:N`) — still reachable by getOwnPropertyDescriptor and surfaced by
/// getOwnPropertySymbols. Real private FIELDS live in the side table (never
/// own props), so a "#..." STRING key is an ordinary enumerable property.
///
/// Symbol keys are "@@" followed by anything but '@'; a guest STRING key
/// that itself begins with "@@" is stored one '@' longer (see
/// [`escape_guest_key`]), so it is never mistaken for one.
pub(crate) fn is_hidden_key(k: &str) -> bool {
    let b = k.as_bytes();
    b.len() >= 2 && b[0] == b'@' && b[1] == b'@' && b.get(2) != Some(&b'@')
}

/// Does guest string `s`, used as a property key, need the escape that keeps
/// it out of the symbol-key space (it begins with "@@")?
#[inline]
pub(crate) fn guest_key_needs_escape(s: &str) -> bool {
    s.as_bytes().starts_with(b"@@")
}

/// The internal property key for guest string `s` (ToPropertyKey of a
/// String): `s` itself, except that one beginning with "@@" — the spelling
/// of the engine's symbol keys (`@@iterator`, `@@sym:N`, `@@for:k`) — gains a
/// leading '@'. Without it `o["@@iterator"]` WAS `o[Symbol.iterator]`, and
/// such keys vanished from enumeration and JSON. [`guest_key_text`] undoes it.
#[inline]
pub(crate) fn escape_guest_key(s: String) -> String {
    if guest_key_needs_escape(&s) {
        let mut e = String::with_capacity(s.len() + 1);
        e.push('@');
        e.push_str(&s);
        e
    } else {
        s
    }
}

/// The guest-visible text of a (non-symbol) internal property key: the
/// inverse of [`escape_guest_key`].
#[inline]
pub(crate) fn guest_key_text(k: &str) -> &str {
    if k.as_bytes().starts_with(b"@@@") {
        &k[1..]
    } else {
        k
    }
}

pub(crate) fn len_value(n: usize) -> Value {
    if n <= i32::MAX as usize {
        Value::int(n as i32)
    } else {
        Value::num(n as f64)
    }
}

/// One `StrWhiteSpaceChar`: WhiteSpace ∪ LineTerminator. Unicode
/// `White_Space` — what `str::trim_start` used — differs in exactly two
/// characters: it omits U+FEFF (ZWNBSP), leaving
/// `parseInt("\u{FEFF}8675309")` at NaN, and it includes U+0085 (NEL), which
/// is not JS whitespace at all (`parseInt("\u{85}8")` must be NaN).
///
/// This is the ONE predicate for every trim in the engine: `String.prototype
/// .trim`/`trimStart`/`trimEnd`, StringToNumber, StringToBigInt and the
/// BigInt/string comparison. Each used to spell its own variant, so
/// `"\u{85}a".trim()` dropped the NEL, `Number("\u{85}1")` was 1 instead of
/// NaN, and `BigInt("\u{FEFF}1")` threw where the spec says 1n.
pub(crate) fn str_white_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{FEFF}'
}

/// JS `parseInt(s, radix)`: skip leading whitespace, an optional sign, an
/// optional `0x` prefix (radix 16), then digits in `radix` (default 10); stop at
/// the first invalid digit. `NaN` if no digits parse. `radix == 0` means "auto".
pub(crate) fn parse_int(s: &str, radix: i32) -> f64 {
    let b = s.trim_start_matches(str_white_space).as_bytes();
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
        return f64::NAN;
    }
    // The spec leaves the value implementation-approximated ONLY for radixes
    // other than 2, 4, 8, 10, 16 and 32 (and past a radix-10 literal's 20th
    // digit); for those six the result is 𝔽(mathInt), correctly rounded.
    // The f64 accumulation above is exact below 2^53 but rounds at every
    // step past it, so a wider run is recomputed exactly.
    let pow2 = (radix as u32).is_power_of_two();
    if pow2 && (i - start) as u64 * (radix as u32).trailing_zeros() as u64 > 53 {
        val = crate::parse::lexer::non_decimal_digits_to_f64(&b[start..i], radix as u32);
    } else if radix == 10 && i - start > 15 {
        // Every run of ASCII digits is a valid Rust float literal.
        val = std::str::from_utf8(&b[start..i])
            .ok()
            .and_then(|d| d.parse::<f64>().ok())
            .unwrap_or(val);
    }
    sign * val
}

/// JS `parseFloat(s)`: skip leading whitespace, then parse the longest leading
/// decimal-float prefix (sign, digits, `.`, exponent, or `Infinity`). `NaN` if
/// none.
pub(crate) fn parse_float(s: &str) -> f64 {
    let t = s.trim_start_matches(str_white_space);
    let b = t.as_bytes();
    let mut end = 0;
    if end < b.len() && (b[end] == b'+' || b[end] == b'-') {
        end += 1;
    }
    if t[end..].starts_with("Infinity") {
        return if t.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
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

/// The array-index value of a canonical integer key -- `"0"`, `"1"`, `"10"`,
/// but not `"00"`, `"01"`, `"-1"`, `"1.5"`, `""` or `" 1"`, and not `u32::MAX`
/// (which is not a valid array index).
///
/// Decided on the BYTES. The old spelling was `k.parse::<u32>()` followed by
/// `n.to_string() == *k`, which allocated a String for every numeric key just
/// to re-derive the text it already had -- once per key per enumeration.
#[inline]
pub(crate) fn canonical_u32_key(k: &str) -> Option<u32> {
    let b = k.as_bytes();
    if b.is_empty() || b.len() > 10 {
        return None;
    }
    // A leading zero is canonical only as the whole key.
    if b[0] == b'0' {
        return (b.len() == 1).then_some(0);
    }
    let mut n: u64 = 0;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        n = n * 10 + (c - b'0') as u64;
    }
    (n < u32::MAX as u64).then_some(n as u32)
}

/// `ZIPP_NO_ARRKEY_FAST=1` restores the allocating string-key paths on the
/// array element machinery: the `key_of` + `parse::<u32>` + `to_string`
/// canonicality re-derivation in `[[HasProperty]]`, and the `key_of` +
/// generic `get_prop` detour for a canonical numeric-string computed read.
/// This exists purely so the change is A/B-able and bisectable on one
/// binary.
#[inline]
pub(crate) fn arrkey_fast_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_ARRKEY_FAST").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// Indices into `keys` in spec **OrdinaryOwnPropertyKeys** order: integer-index
/// keys (canonical array indices `0..2^32-1`, e.g. "0"/"7" but not "01"/"-1")
/// first in ascending numeric order, then every other key in its original
/// (insertion) order. Symbols/private keys keep their relative position among
/// "the rest"; callers filter hidden keys separately.
pub(crate) fn spec_key_order(keys: &[String]) -> Vec<usize> {
    let mut ints: Vec<(u32, usize)> = Vec::new();
    let mut rest: Vec<usize> = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        match canonical_u32_key(k) {
            Some(n) => ints.push((n, i)),
            None => rest.push(i),
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
        // A spec array index is a canonical uint32 STRICTLY LESS THAN 2^32-1, so
        // 4294967295 (2^32-1) and anything ≥ 2^32 are ordinary string properties
        // (they do not extend `.length` and must not hit the dense-array limit).
        if d >= 0.0 && d.fract() == 0.0 && d < 4_294_967_295.0 {
            Some(d as usize)
        } else {
            None
        }
    } else {
        None
    }
}
