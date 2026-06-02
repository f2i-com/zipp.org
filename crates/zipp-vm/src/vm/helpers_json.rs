#![allow(unused_imports)]
use super::*;
use crate::bytecode::{InstanceCtor, Instr, Program, UpvalSource};
use crate::heap::{
    AsyncGenState, AsyncStateData, ClassData, GenState, Handler, Heap, HeapObj, ObjMap,
    PropAttr, PromiseState, Reaction,
};
use crate::value::Value;

/// Quote a string as a JSON string literal (escaping per the JSON spec).
pub(crate) fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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

/// Parse a JSON string literal starting at the opening `"` (index `*i`), applying
/// escapes (incl. `\uXXXX` and surrogate pairs). Plain content is flushed as UTF-8
/// slices so multi-byte characters survive intact.
pub(crate) fn json_parse_string(src: &str, i: &mut usize) -> Result<String, Thrown> {
    let b = src.as_bytes();
    *i += 1; // opening quote
    let mut out = String::new();
    let mut run = *i;
    loop {
        match b.get(*i).copied() {
            None => return Err(Thrown("SyntaxError: Unterminated string in JSON".into())),
            Some(b'"') => {
                out.push_str(&src[run..*i]);
                *i += 1;
                return Ok(out);
            }
            Some(b'\\') => {
                out.push_str(&src[run..*i]); // flush the plain run before the escape
                *i += 1;
                match b.get(*i).copied() {
                    Some(b'"') => out.push('"'),
                    Some(b'\\') => out.push('\\'),
                    Some(b'/') => out.push('/'),
                    Some(b'n') => out.push('\n'),
                    Some(b'r') => out.push('\r'),
                    Some(b't') => out.push('\t'),
                    Some(b'b') => out.push('\u{0008}'),
                    Some(b'f') => out.push('\u{000c}'),
                    Some(b'u') => {
                        let cp = json_hex4(b, *i + 1)?;
                        *i += 4; // past the 4 hex (now at the last one)
                        let ch = if (0xD800..=0xDBFF).contains(&cp) {
                            // High surrogate: combine with a following \uXXXX low.
                            if b.get(*i + 1) == Some(&b'\\') && b.get(*i + 2) == Some(&b'u') {
                                let lo = json_hex4(b, *i + 3)?;
                                if (0xDC00..=0xDFFF).contains(&lo) {
                                    *i += 6;
                                    let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    char::from_u32(c).unwrap_or('\u{FFFD}')
                                } else {
                                    '\u{FFFD}'
                                }
                            } else {
                                '\u{FFFD}'
                            }
                        } else {
                            char::from_u32(cp).unwrap_or('\u{FFFD}')
                        };
                        out.push(ch);
                    }
                    _ => return Err(Thrown("SyntaxError: Invalid escape in JSON string".into())),
                }
                *i += 1;
                run = *i;
            }
            // A raw control character (< 0x20) is invalid in a JSON string — it
            // must be escaped (`\n`, `	`, …). (Matches the spec / node.)
            Some(c) if c < 0x20 => {
                return Err(Thrown("SyntaxError: Bad control character in string literal in JSON".into()));
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
    match std::str::from_utf8(&b[start..*i]).unwrap_or("").parse::<f64>() {
        Ok(n) => Ok(Value::num(n)),
        Err(_) => Err(err()),
    }
}

/// Wrap JSON `parts` in `open`/`close`, compact when `indent` is empty, else
/// one element per line indented `depth+1` deep with the closing bracket at `depth`.
pub(crate) fn wrap_json(parts: &[String], open: char, close: char, indent: &str, depth: usize) -> String {
    if indent.is_empty() {
        return format!("{}{}{}", open, parts.join(","), close);
    }
    let pad = indent.repeat(depth + 1);
    let pad_close = indent.repeat(depth);
    let sep = format!(",\n{pad}");
    format!("{open}\n{pad}{}\n{pad_close}{close}", parts.join(&sep))
}

