//! Number literal values, read from the literal's text when needed (the
//! lexer has already checked the syntax).

/// A float literal's (or an imaginary literal's part's) value.
///
/// Read by `zipp_numparse`, which returns what `str::parse::<f64>` returns
/// without linking core's parser and its power-of-five table into the
/// WebAssembly modules (the engine reads its numbers with it too).
pub fn float(text: &str) -> f64 {
    if !text.contains('_') {
        return zipp_numparse::parse_f64(text).unwrap_or(f64::NAN);
    }
    let digits: Vec<u8> = text.bytes().filter(|&c| c != b'_').collect();
    zipp_numparse::parse_f64_bytes(&digits).unwrap_or(f64::NAN)
}

/// An int literal's value when it fits in a `u64` (`text` as written,
/// prefix and underscores included).
pub fn small_int(text: &str, radix: u8) -> Option<u64> {
    let digits = if radix == 10 { text } else { &text[2..] };
    let mut value: u64 = 0;
    for c in digits.bytes() {
        if c == b'_' {
            continue;
        }
        let digit = (c as char).to_digit(radix as u32)? as u64;
        value = value.checked_mul(radix as u64)?.checked_add(digit)?;
    }
    Some(value)
}
