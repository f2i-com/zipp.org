//! An int literal's value in decimal, for error messages (any size).

/// `text` is the literal (with its `0x`/`0o`/`0b` prefix unless `radix` is 10).
pub(crate) fn int_decimal(text: &str, radix: u32) -> String {
    let digits = if radix == 10 { text } else { &text[2..] };
    // Little-endian limbs of 10^9.
    let mut limbs: Vec<u32> = vec![0];
    for c in digits.chars() {
        let Some(d) = c.to_digit(radix) else { continue };
        let mut carry = d as u64;
        for limb in &mut limbs {
            let v = *limb as u64 * radix as u64 + carry;
            *limb = (v % 1_000_000_000) as u32;
            carry = v / 1_000_000_000;
        }
        if carry > 0 {
            limbs.push(carry as u32);
        }
    }
    let mut out = limbs.last().unwrap().to_string();
    for limb in limbs.iter().rev().skip(1) {
        out.push_str(&format!("{limb:09}"));
    }
    out
}
