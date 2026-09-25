//! Number literal values, read from the literal's text when needed (the
//! lexer has already checked the syntax).

/// A float literal's (or an imaginary literal's part's) value.
pub fn float(text: &str) -> f64 {
    if !text.contains('_') {
        return text.parse().unwrap_or(f64::NAN);
    }
    let digits: String = text.chars().filter(|&c| c != '_').collect();
    digits.parse().unwrap_or(f64::NAN)
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
