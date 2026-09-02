#![allow(unused_imports)]
use super::*;
use crate::bytecode::{Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PromiseState, PropAttr, ReactionPair, Reactions,
};
use crate::value::Value;

/// `ZIPP_NO_JSON_QUOTE_BULK=1` restores the per-code-point quoting loop
/// (`json_quote_cp` per char) in `json_quote_into`/`json_quote_wtf8_into`,
/// so the bulk-run path is A/B-able and bisectable on one binary.
#[inline]
fn json_quote_bulk_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static ON: AtomicU8 = AtomicU8::new(2);
    match ON.load(Ordering::Relaxed) {
        0 => false,
        1 => true,
        _ => {
            let v = std::env::var_os("ZIPP_NO_JSON_QUOTE_BULK").is_none() as u8;
            ON.store(v, Ordering::Relaxed);
            v == 1
        }
    }
}

/// `json_quote` appending into an existing buffer (the stringify fast path —
/// avoids a fresh String per quoted key/value).
pub(crate) fn json_quote_into(out: &mut String, s: &str) {
    out.reserve(s.len() + 2);
    out.push('"');
    if json_quote_bulk_enabled() {
        // Bulk-copy maximal clean runs: JSON escapes exactly `"`, `\` and
        // controls < 0x20 — all ASCII, and an ASCII byte never occurs inside
        // a multi-byte UTF-8 sequence, so a byte scan finds every escape and
        // every run boundary lands on a char boundary.
        let b = s.as_bytes();
        let (mut run, mut i) = (0usize, 0usize);
        while i < b.len() {
            let c = b[i];
            if c < 0x20 || c == b'"' || c == b'\\' {
                out.push_str(&s[run..i]);
                json_quote_cp(out, c as u32);
                run = i + 1;
            }
            i += 1;
        }
        out.push_str(&s[run..]);
    } else {
        // `ZIPP_NO_JSON_QUOTE_BULK=1`: the old per-code-point loop.
        for c in s.chars() {
            json_quote_cp(out, c as u32);
        }
    }
    out.push('"');
}

/// `json_quote_wtf8` appending into an existing buffer, carrying the caller's
/// ASCII knowledge. A flat `JsStr` records `ascii` at construction, and an
/// ASCII buffer is `&str` material, so it takes the `&str` quoter: same
/// escapes, same bytes out, but its clean runs are copied without
/// `json_quote_run`'s `from_utf8` re-validation and without the per-byte
/// lone-surrogate probe. `ascii == false` means "unknown" and keeps the WTF-8
/// scan; so does `ZIPP_NO_JSON_ASCII_UNCHECKED=1`.
pub(crate) fn json_quote_wtf8_into(out: &mut String, b: &[u8], ascii: bool) {
    if ascii && json_ascii_unchecked_enabled() {
        if let Some(s) = ascii_bytes_as_str(b) {
            json_quote_into(out, s);
            return;
        }
    }
    out.reserve(b.len() + 2);
    out.push('"');
    if json_quote_bulk_enabled() {
        // Same bulk-run scan over WTF-8. The only WTF-8 sequences that are
        // not valid UTF-8 are the 3-byte lone-surrogate encodings
        // `0xED 0xA0..=0xBF _`, so a run stops there and the surrogate keeps
        // its exact `\udXXX` escape via `json_quote_cp` — byte-for-byte the
        // per-code-point path. (`0xED` with a second byte below 0xA0 is
        // U+D000..U+D7FF — plain text, it stays in the run.)
        let (mut run, mut i) = (0usize, 0usize);
        while i < b.len() {
            let c = b[i];
            if c < 0x20 || c == b'"' || c == b'\\' {
                json_quote_run(out, &b[run..i]);
                json_quote_cp(out, c as u32);
                i += 1;
                run = i;
            } else if c == 0xED && b.get(i + 1).is_some_and(|&n| (0xA0..=0xBF).contains(&n)) {
                json_quote_run(out, &b[run..i]);
                let (cp, len) = crate::heap::wtf8_decode(b, i);
                json_quote_cp(out, cp);
                i += len;
                run = i;
            } else {
                i += 1;
            }
        }
        json_quote_run(out, &b[run..]);
    } else {
        // `ZIPP_NO_JSON_QUOTE_BULK=1`: the old per-code-point loop.
        for cp in crate::heap::wtf8_code_points(b) {
            json_quote_cp(out, cp);
        }
    }
    out.push('"');
}

/// Append one clean (escape-free) run of WTF-8 bytes. The bulk scanner only
/// hands this valid UTF-8 (lone-surrogate triples are routed to the escape
/// path), so the `Err` arm is a defensive exact fallback, not a reachable
/// path for canonical buffers.
fn json_quote_run(out: &mut String, seg: &[u8]) {
    match std::str::from_utf8(seg) {
        Ok(s) => out.push_str(s),
        Err(_) => {
            for cp in crate::heap::wtf8_code_points(seg) {
                json_quote_cp(out, cp);
            }
        }
    }
}

/// One code point of JSON string-literal quoting. `cp` may be a lone
/// surrogate (escaped — it has no UTF-8 form).
fn json_quote_cp(out: &mut String, cp: u32) {
    match cp {
        0x22 => out.push_str("\\\""),
        0x5C => out.push_str("\\\\"),
        0x0A => out.push_str("\\n"),
        0x0D => out.push_str("\\r"),
        0x09 => out.push_str("\\t"),
        0x08 => out.push_str("\\b"),
        0x0C => out.push_str("\\f"),
        c if c < 0x20 => json_quote_u_escape(out, c),
        c if (0xD800..=0xDFFF).contains(&c) => json_quote_u_escape(out, c),
        c => out.push(char::from_u32(c).unwrap_or('\u{FFFD}')),
    }
}

/// `\uXXXX` with four LOWERCASE hex digits — exactly what the
/// `format!("\\u{c:04x}")` it replaces printed, minus the String each
/// escape allocated.
fn json_quote_u_escape(out: &mut String, c: u32) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push_str("\\u");
    for sh in [12u32, 8, 4, 0] {
        out.push(HEX[((c >> sh) & 0xF) as usize] as char);
    }
}

pub(crate) fn json_skip_ws(b: &[u8], i: &mut usize) {
    while matches!(b.get(*i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        *i += 1;
    }
}

/// Match a literal `word` (true/false/null) at `*i`, advancing past it.
pub(crate) fn json_expect(b: &[u8], i: &mut usize, word: &str) -> Result<(), Thrown> {
    if b[*i..].starts_with(word.as_bytes()) {
        *i += word.len();
        Ok(())
    } else {
        Err(Thrown("SyntaxError: Unexpected token in JSON".into()))
    }
}

/// Read exactly 4 hex digits at `pos` as a code unit.
pub(crate) fn json_hex4(b: &[u8], pos: usize) -> Result<u32, Thrown> {
    if pos + 4 > b.len() {
        return Err(Thrown("SyntaxError: Bad unicode escape in JSON".into()));
    }
    let mut v = 0u32;
    for k in 0..4 {
        let d = match b[pos + k] {
            c @ b'0'..=b'9' => (c - b'0') as u32,
            c @ b'a'..=b'f' => (c - b'a' + 10) as u32,
            c @ b'A'..=b'F' => (c - b'A' + 10) as u32,
            _ => return Err(Thrown("SyntaxError: Bad unicode escape in JSON".into())),
        };
        v = v * 16 + d;
    }
    Ok(v)
}

/// B233 latch: `ZIPP_NO_JSON_PLAIN_KEY=1` reads every member name through the
/// general string parser again.
pub(crate) fn json_plain_key_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_JSON_PLAIN_KEY").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// Latch for the parser-proved ASCII views: `ZIPP_NO_JSON_ASCII_UNCHECKED=1`
/// re-validates every escape-free member name, every number token and every
/// ASCII string value through `core::str::from_utf8`, exactly as before.
///
/// Those three sites were the whole `from_utf8` share of json-large (3.4% of
/// PC samples): ~440,000 member names and ~225,000 number tokens per run on
/// the parse side, and every string value on the stringify side — all bytes
/// the scanner in front of the call had already looked at one by one.
pub(crate) fn json_ascii_unchecked_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_JSON_ASCII_UNCHECKED").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// View bytes the CALLER has proved ASCII as `&str` without a second scan.
/// ASCII is valid UTF-8 by construction, so the unchecked view is sound. The
/// memory-safe sandbox profile keeps the checked conversion; its `None` (a
/// failed proof, which cannot happen) sends the caller back to the exact old
/// path rather than panicking.
pub(crate) fn ascii_bytes_as_str(bytes: &[u8]) -> Option<&str> {
    debug_assert!(
        bytes.is_ascii(),
        "ascii_bytes_as_str: caller's ascii proof failed"
    );
    #[cfg(feature = "safe-sandbox")]
    return std::str::from_utf8(bytes).ok();
    // SAFETY: every byte is < 0x80 (the caller's proof, checked above in
    // debug builds), and a pure-ASCII byte sequence is valid UTF-8.
    #[cfg(not(feature = "safe-sandbox"))]
    return Some(unsafe { std::str::from_utf8_unchecked(bytes) });
}

/// `ZIPP_NO_JSON_INT_FAST=1` sends every number token through
/// `str::parse::<f64>` again (see `json_int_token_fast`).
pub(crate) fn json_int_fast_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static STATE: AtomicU8 = AtomicU8::new(0);
    match STATE.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = std::env::var_os("ZIPP_NO_JSON_INT_FAST").is_none();
            STATE.store(if on { 1 } else { 2 }, Ordering::Relaxed);
            on
        }
    }
}

/// A plain integer token — `-?digits`, at most 15 digits, no fraction and no
/// exponent — read directly. Below 10^15 every integer is exactly
/// representable, so `u64 as f64` IS the correctly rounded value
/// `parse::<f64>` returns; `-0` stays the negative zero `parse` gives (and
/// `Value::num` keeps it a double). Anything else → `None`, the general
/// parser. The caller has already validated the token against the JSON
/// number grammar, so a leading zero can only be the lone `0`.
fn json_int_token_fast(tok: &[u8]) -> Option<f64> {
    if !json_int_fast_enabled() {
        return None;
    }
    let (neg, digits) = match tok.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, tok),
    };
    if digits.is_empty() || digits.len() > 15 {
        return None;
    }
    let mut n: u64 = 0;
    for &d in digits {
        if !d.is_ascii_digit() {
            return None;
        }
        n = n * 10 + (d - b'0') as u64;
    }
    let f = n as f64;
    Some(if neg { -f } else { f })
}

/// B233: read a member name that needs no decoding, borrowing it from the
/// source instead of allocating it.
///
/// `Some(name)` means the bytes between the quotes held no escape and no
/// control character and are valid UTF-8 — so the name IS those bytes, and
/// `*i` is left just past the closing quote exactly where `json_parse_string`
/// would leave it. `None` consumes nothing: `*i` is untouched and the caller
/// re-reads the name with the general parser, which is where every escape,
/// lone-surrogate and error rule continues to live. WTF-8-encoded lone
/// surrogates fail the UTF-8 check and take that path too.
pub(crate) fn json_scan_plain_key<'a>(src: &'a [u8], i: &mut usize) -> Option<&'a str> {
    debug_assert_eq!(src.get(*i), Some(&b'"'));
    let start = *i + 1;
    let mut j = start;
    // The scan already looks at every byte; remembering whether one was
    // >= 0x80 makes the UTF-8 question free for the ASCII name (the common
    // one), so the `from_utf8` pass below is only paid for a non-ASCII name.
    let mut ascii = true;
    loop {
        match *src.get(j)? {
            b'"' => break,
            b'\\' => return None,
            c if c < 0x20 => return None,
            c => {
                ascii &= c < 0x80;
                j += 1;
            }
        }
    }
    let raw = &src[start..j];
    let name = match (ascii && json_ascii_unchecked_enabled())
        .then(|| ascii_bytes_as_str(raw))
        .flatten()
    {
        Some(s) => s,
        None => std::str::from_utf8(raw).ok()?,
    };
    *i = j + 1;
    Some(name)
}

/// Parse a JSON string literal starting at the opening `"` (index `*i`),
/// applying escapes. The result is WTF-8 (a `JsStr`): each `\uXXXX` escape
/// pushes its CODE UNIT through `wtf8_push_cp`, which combines a high+low
/// escape pair into the astral scalar and keeps an unpaired surrogate as a
/// real lone surrogate — an unpaired high followed by a non-low escape does
/// NOT consume that next escape (each unit is pushed independently). Plain
/// content is flushed as byte slices so multi-byte characters survive intact.
pub(crate) fn json_parse_string(src: &[u8], i: &mut usize) -> Result<crate::heap::JsStr, Thrown> {
    let b = src;
    *i += 1; // opening quote
    let mut out: Vec<u8> = Vec::new();
    let mut run = *i;
    loop {
        match b.get(*i).copied() {
            None => return Err(Thrown("SyntaxError: Unterminated string in JSON".into())),
            Some(b'"') => {
                out.extend_from_slice(&b[run..*i]);
                *i += 1;
                return Ok(crate::heap::JsStr::from_wtf8(out));
            }
            Some(b'\\') => {
                out.extend_from_slice(&b[run..*i]); // flush the plain run before the escape
                *i += 1;
                match b.get(*i).copied() {
                    Some(b'"') => out.push(b'"'),
                    Some(b'\\') => out.push(b'\\'),
                    Some(b'/') => out.push(b'/'),
                    Some(b'n') => out.push(b'\n'),
                    Some(b'r') => out.push(b'\r'),
                    Some(b't') => out.push(b'\t'),
                    Some(b'b') => out.push(0x08),
                    Some(b'f') => out.push(0x0C),
                    Some(b'u') => {
                        let cu = json_hex4(b, *i + 1)?;
                        *i += 4; // past the 4 hex (now at the last one)
                        crate::heap::wtf8_push_cp(&mut out, cu);
                    }
                    _ => return Err(Thrown("SyntaxError: Invalid escape in JSON string".into())),
                }
                *i += 1;
                run = *i;
            }
            // A raw control character (< 0x20) is invalid in a JSON string — it
            // must be escaped (`\n`, `	`, …). (Matches the spec / node.)
            Some(c) if c < 0x20 => {
                return Err(Thrown(
                    "SyntaxError: Bad control character in string literal in JSON".into(),
                ));
            }
            Some(_) => *i += 1, // plain byte (ASCII or UTF-8 continuation) — sliced later
        }
    }
}

/// Parse a JSON number token at `*i`.
pub(crate) fn json_parse_number(b: &[u8], i: &mut usize) -> Result<Value, Thrown> {
    let start = *i;
    let err = || Thrown("SyntaxError: Invalid number in JSON".into());
    // ECMA-404 number grammar (stricter than Rust's f64 parser):
    //   number = [ '-' ] int [ frac ] [ exp ]
    //   int    = '0' | [1-9] digit*        (no leading zeros)
    //   frac   = '.' digit+                (at least one digit)
    //   exp    = ('e'|'E') ['+'|'-'] digit+
    if b.get(*i) == Some(&b'-') {
        *i += 1;
    }
    match b.get(*i) {
        Some(b'0') => *i += 1, // a leading 0 must stand alone (no further digits)
        Some(c) if c.is_ascii_digit() => {
            *i += 1;
            while matches!(b.get(*i), Some(d) if d.is_ascii_digit()) {
                *i += 1;
            }
        }
        _ => return Err(err()), // missing integer part ("-", ".5", "e1", …)
    }
    if b.get(*i) == Some(&b'.') {
        *i += 1;
        if !matches!(b.get(*i), Some(c) if c.is_ascii_digit()) {
            return Err(err()); // "1." — fraction needs a digit
        }
        while matches!(b.get(*i), Some(c) if c.is_ascii_digit()) {
            *i += 1;
        }
    }
    if matches!(b.get(*i), Some(b'e' | b'E')) {
        *i += 1;
        if matches!(b.get(*i), Some(b'+' | b'-')) {
            *i += 1;
        }
        if !matches!(b.get(*i), Some(c) if c.is_ascii_digit()) {
            return Err(err()); // "1e" — exponent needs a digit
        }
        while matches!(b.get(*i), Some(c) if c.is_ascii_digit()) {
            *i += 1;
        }
    }
    // Every byte the grammar above accepted is ASCII (`-`, digits, `.`, `e`,
    // `E`, `+`), so the token is `&str` material without a UTF-8 pass.
    let tok = &b[start..*i];
    if let Some(n) = json_int_token_fast(tok) {
        return Ok(Value::num(n));
    }
    let text = match json_ascii_unchecked_enabled()
        .then(|| ascii_bytes_as_str(tok))
        .flatten()
    {
        Some(s) => s,
        None => std::str::from_utf8(tok).unwrap_or(""),
    };
    match text.parse::<f64>() {
        Ok(n) => Ok(Value::num(n)),
        Err(_) => Err(err()),
    }
}
