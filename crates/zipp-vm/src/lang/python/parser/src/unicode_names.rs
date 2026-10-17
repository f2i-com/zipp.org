//! `\N{...}` names: a word-coded table of the names programs commonly spell
//! (see `gen_unicode_names.py`), and the algorithmic CJK ideograph names.
//! Names compare case-insensitively, as in CPython.

#[path = "unicode_names_table.rs"]
mod table;

fn varint(data: &[u8], at: &mut usize) -> u32 {
    let mut value = 0u32;
    let mut shift = 0;
    loop {
        let byte = data[*at];
        *at += 1;
        value |= ((byte & 0x7F) as u32) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
        shift += 7;
    }
}

/// The character named `name`, if the table knows it.
pub fn lookup(name: &str) -> Option<char> {
    if let Some(hex) = name
        .get(..22)
        .filter(|prefix| prefix.eq_ignore_ascii_case("CJK UNIFIED IDEOGRAPH-"))
        .and_then(|_| name.get(22..))
    {
        // The name's own spelling: four digits, or five without a leading zero.
        if (hex.len() == 4 || (hex.len() == 5 && !hex.starts_with('0')))
            && hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            let code = u32::from_str_radix(hex, 16).ok()?;
            return table::CJK_UNIFIED
                .iter()
                .any(|(lo, hi)| (*lo..=*hi).contains(&code))
                .then(|| char::from_u32(code))
                .flatten();
        }
        return None;
    }
    // The name as word indices; a word the table lacks names nothing.
    let mut wanted = Vec::new();
    for word in name.split(' ') {
        let index = table::WORDS
            .split('\n')
            .position(|known| known.eq_ignore_ascii_case(word))?;
        wanted.push(index as u32);
    }
    let data = table::ENTRIES;
    let mut at = 0;
    let mut code = 0u32;
    while at < data.len() {
        code += varint(data, &mut at);
        let count = varint(data, &mut at) as usize;
        let mut matches = count == wanted.len();
        for i in 0..count {
            let word = varint(data, &mut at);
            matches = matches && wanted.get(i) == Some(&word);
        }
        if matches {
            return char::from_u32(code);
        }
    }
    None
}

/// Every name in the table with its character (for tests).
pub fn all() -> Vec<(String, char)> {
    let words: Vec<&str> = table::WORDS.split('\n').collect();
    let data = table::ENTRIES;
    let mut out = Vec::new();
    let mut at = 0;
    let mut code = 0u32;
    while at < data.len() {
        code += varint(data, &mut at);
        let count = varint(data, &mut at) as usize;
        let name: Vec<&str> = (0..count)
            .map(|_| words[varint(data, &mut at) as usize])
            .collect();
        out.push((name.join(" "), char::from_u32(code).unwrap()));
    }
    out
}
