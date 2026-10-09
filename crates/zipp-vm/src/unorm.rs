//! Unicode normalization (NFC, NFD, NFKC, NFKD), canonical combining classes
//! and the combining-mark test, over compact tables.
//!
//! The engine used the `unicode-normalization` crate for these. Its tables
//! are minimal perfect hashes: 111 KB in a 32-bit module, of which 42 KB was
//! left after Brotli, since a hashed table has no order for a compressor to
//! find. They were the largest block of compressed data in every WebAssembly
//! module. The same mappings are held here in code point order
//! (`unorm_tables.rs`, generated from that crate by the test at the end of
//! this file). A table's keys are a bitmap for each 256-code-point page that
//! has any, and a member's rank among them, counted from its page's bits,
//! is its index into the table's values: a lookup is a few loads and a
//! population count, as the hashed lookup was.
//!
//! The iterators follow that crate's: a decomposition keeps a buffer in which
//! each run of combining marks is put in canonical order before any of it is
//! read, and a recomposition folds a starter with what follows it until a
//! blocked character or the next starter. Every code point and a few hundred
//! thousand generated strings are checked against the crate, which stays a
//! development dependency for exactly that.

// Generated, and compared with what the generator writes: not reformatted.
#[rustfmt::skip]
#[path = "unorm_tables.rs"]
mod tables;

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

/// A set of code points in which each member has a rank, its position in
/// code point order.
struct Ranked {
    /// For each 256-code-point page up to the last with a member: 0, or one
    /// more than the page's place in `bits` and `below`.
    pages: &'static [u8],
    /// 32 bytes a page: a bit a code point, lowest first.
    bits: &'static [u8],
    /// The number of members before each 64 code points of each page.
    below: &'static [u16],
}

impl Ranked {
    /// The rank of `c`, if it is a member.
    #[inline]
    fn rank(&self, c: u32) -> Option<usize> {
        let slot = usize::from(*self.pages.get((c >> 8) as usize)?).checked_sub(1)?;
        // The 64 code points around `c`: their bits, and the members before them.
        let at = slot * 4 + ((c as usize & 255) >> 6);
        let bits = u64::from_le_bytes(self.bits[at * 8..at * 8 + 8].try_into().ok()?);
        let bit = c & 63;
        if bits >> bit & 1 == 0 {
            return None;
        }
        let before = (bits & ((1u64 << bit) - 1)).count_ones();
        Some(usize::from(self.below[at]) + before as usize)
    }
}

const CANONICAL: Ranked = Ranked {
    pages: tables::CANONICAL_PAGES,
    bits: tables::CANONICAL_BITS,
    below: tables::CANONICAL_BELOW,
};
const COMPATIBLE: Ranked = Ranked {
    pages: tables::COMPATIBLE_PAGES,
    bits: tables::COMPATIBLE_BITS,
    below: tables::COMPATIBLE_BELOW,
};
const CLASSED: Ranked = Ranked {
    pages: tables::CLASS_PAGES,
    bits: tables::CLASS_BITS,
    below: tables::CLASS_BELOW,
};
const MARKS: Ranked = Ranked {
    pages: tables::MARK_PAGES,
    bits: tables::MARK_BITS,
    below: tables::MARK_BELOW,
};
const FIRSTS: Ranked = Ranked {
    pages: tables::FIRST_PAGES,
    bits: tables::FIRST_BITS,
    below: tables::FIRST_BELOW,
};

/// The canonical combining class of `c` (0 for a starter).
#[inline]
pub(crate) fn canonical_combining_class(c: char) -> u8 {
    let c = c as u32;
    if c < 0x300 {
        return 0;
    }
    match CLASSED.rank(c) {
        Some(at) => tables::CLASS_VALUES[at],
        None => 0,
    }
}

/// Whether `c` has General_Category Mark (Mn, Mc or Me).
pub(crate) fn is_combining_mark(c: char) -> bool {
    let c = c as u32;
    if c < 0x300 {
        return false;
    }
    // The pages cover the planes with scripts; beyond them the marks are a
    // few runs (the variation selectors of plane 14).
    if (c >> 8) as usize >= tables::MARK_PAGES.len() {
        return tables::MARK_BEYOND
            .iter()
            .any(|&(first, last)| (first..=last).contains(&c));
    }
    MARKS.rank(c).is_some()
}

/// The text `c` maps to in a decomposition table.
#[inline]
fn mapped(
    keys: &Ranked,
    offsets: &'static [u16],
    text: &'static str,
    c: u32,
) -> Option<&'static str> {
    let at = keys.rank(c)?;
    Some(&text[usize::from(offsets[at])..usize::from(offsets[at + 1])])
}

/// The full decomposition of `c`, canonical or compatibility, a character at
/// a time. A character with none is given back as it is.
#[inline]
fn decompose(c: char, compatible: bool, mut emit: impl FnMut(char)) {
    let code = c as u32;
    if code < 0xC0 && !(compatible && code >= 0xA0) {
        emit(c);
        return;
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&code) {
        let index = code - S_BASE;
        emit(char::from_u32(L_BASE + index / N_COUNT).unwrap_or(c));
        emit(char::from_u32(V_BASE + index % N_COUNT / T_COUNT).unwrap_or(c));
        if index % T_COUNT != 0 {
            emit(char::from_u32(T_BASE + index % T_COUNT).unwrap_or(c));
        }
        return;
    }
    let text = compatible
        .then(|| {
            mapped(
                &COMPATIBLE,
                tables::COMPATIBLE_OFFSETS,
                tables::COMPATIBLE_TEXT,
                code,
            )
        })
        .flatten()
        .or_else(|| {
            mapped(
                &CANONICAL,
                tables::CANONICAL_OFFSETS,
                tables::CANONICAL_TEXT,
                code,
            )
        });
    match text {
        Some(text) => text.chars().for_each(emit),
        None => emit(c),
    }
}

/// The primary composite of `a` followed by `b`, if there is one.
#[inline]
fn compose(a: char, b: char) -> Option<char> {
    let (a, b) = (a as u32, b as u32);
    // Hangul: a leading and a vowel jamo, then that syllable and a trailing one.
    if (L_BASE..L_BASE + L_COUNT).contains(&a) && (V_BASE..V_BASE + V_COUNT).contains(&b) {
        return char::from_u32(S_BASE + ((a - L_BASE) * V_COUNT + (b - V_BASE)) * T_COUNT);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&a)
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&b)
        && (a - S_BASE) % T_COUNT == 0
    {
        return char::from_u32(a + (b - T_BASE));
    }
    // Every second character is a mark or a vowel sign, none below U+0300:
    // a letter after a letter is answered here.
    if b < 0x300 {
        return None;
    }
    if a >= 0x10000 || b >= 0x10000 {
        return tables::COMPOSITION_BEYOND
            .iter()
            .find(|entry| entry.0 == a && entry.1 == b)
            .and_then(|entry| char::from_u32(entry.2));
    }
    // The pairs of one first character are together, a few each, in the
    // order of their seconds.
    let first = FIRSTS.rank(a)?;
    let pairs =
        usize::from(tables::FIRST_PAIRS[first])..usize::from(tables::FIRST_PAIRS[first + 1]);
    tables::COMPOSITION_PAIRS[pairs.start * 2..pairs.end * 2]
        .chunks_exact(2)
        .find(|pair| u32::from(pair[0]) == b)
        .and_then(|pair| char::from_u32(u32::from(pair[1])))
}

/// The characters of a string in NFD or NFKD.
#[derive(Clone)]
pub(crate) struct Decompositions<'a> {
    source: std::str::Chars<'a>,
    compatible: bool,
    /// Decomposed characters with their combining classes. Those before
    /// `ready` are in canonical order; the rest are a run of combining
    /// marks still open at the end of what has been read.
    buffer: Vec<(u8, char)>,
    next: usize,
    ready: usize,
}

/// Put a run of marks in canonical order. The sort is stable, so marks of
/// one class keep their order; most runs are of one mark or none.
#[inline]
fn order(run: &mut [(u8, char)]) {
    if run.len() > 1 {
        run.sort_by_key(|entry| entry.0);
    }
}

impl<'a> Decompositions<'a> {
    fn new(s: &'a str, compatible: bool) -> Self {
        Decompositions {
            source: s.chars(),
            compatible,
            buffer: Vec::new(),
            next: 0,
            ready: 0,
        }
    }

    /// The next character with its canonical combining class.
    #[inline]
    fn next_classed(&mut self) -> Option<(u8, char)> {
        while self.ready == 0 {
            let Some(c) = self.source.next() else {
                if self.buffer.is_empty() {
                    return None;
                }
                order(&mut self.buffer);
                self.ready = self.buffer.len();
                break;
            };
            let (buffer, ready) = (&mut self.buffer, &mut self.ready);
            decompose(c, self.compatible, |d| {
                let class = canonical_combining_class(d);
                if class == 0 {
                    // A starter closes the run before it.
                    order(&mut buffer[*ready..]);
                    buffer.push((0, d));
                    *ready = buffer.len();
                } else {
                    buffer.push((class, d));
                }
            });
        }
        let entry = self.buffer[self.next];
        self.next += 1;
        if self.next == self.ready {
            // Keep only the open run.
            if self.ready == self.buffer.len() {
                self.buffer.clear();
            } else {
                self.buffer.drain(..self.ready);
            }
            self.next = 0;
            self.ready = 0;
        }
        Some(entry)
    }
}

impl Iterator for Decompositions<'_> {
    type Item = char;

    #[inline]
    fn next(&mut self) -> Option<char> {
        self.next_classed().map(|entry| entry.1)
    }
}

#[derive(Clone)]
enum Recomposing {
    Composing,
    /// Giving out the marks left after a starter that has been given out.
    Purging(usize),
    /// The source has ended: the marks, then the last starter.
    Finished(usize),
}

/// The characters of a string in NFC or NFKC.
#[derive(Clone)]
pub(crate) struct Recompositions<'a> {
    source: Decompositions<'a>,
    state: Recomposing,
    /// Marks after `starter` that did not combine with it.
    marks: Vec<char>,
    starter: Option<char>,
    /// The class of the last mark kept, which blocks a later mark of a class
    /// no higher from combining with `starter`.
    last_class: Option<u8>,
}

impl<'a> Recompositions<'a> {
    fn new(s: &'a str, compatible: bool) -> Self {
        Recompositions {
            source: Decompositions::new(s, compatible),
            state: Recomposing::Composing,
            marks: Vec::new(),
            starter: None,
            last_class: None,
        }
    }
}

impl Iterator for Recompositions<'_> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        loop {
            match self.state {
                Recomposing::Composing => {
                    while let Some((class, c)) = self.source.next_classed() {
                        let Some(starter) = self.starter else {
                            if class != 0 {
                                return Some(c);
                            }
                            self.starter = Some(c);
                            continue;
                        };
                        match self.last_class {
                            None => match compose(starter, c) {
                                Some(composed) => self.starter = Some(composed),
                                None if class == 0 => {
                                    self.starter = Some(c);
                                    return Some(starter);
                                }
                                None => {
                                    self.marks.push(c);
                                    self.last_class = Some(class);
                                }
                            },
                            Some(last) if last >= class => {
                                if class == 0 {
                                    self.starter = Some(c);
                                    self.last_class = None;
                                    self.state = Recomposing::Purging(0);
                                    return Some(starter);
                                }
                                self.marks.push(c);
                                self.last_class = Some(class);
                            }
                            Some(_) => match compose(starter, c) {
                                Some(composed) => self.starter = Some(composed),
                                None => {
                                    self.marks.push(c);
                                    self.last_class = Some(class);
                                }
                            },
                        }
                    }
                    self.state = Recomposing::Finished(0);
                    if self.starter.is_some() {
                        return self.starter.take();
                    }
                }
                Recomposing::Purging(next) => match self.marks.get(next) {
                    Some(&mark) => {
                        self.state = Recomposing::Purging(next + 1);
                        return Some(mark);
                    }
                    None => {
                        self.marks.clear();
                        self.state = Recomposing::Composing;
                    }
                },
                Recomposing::Finished(next) => match self.marks.get(next) {
                    Some(&mark) => {
                        self.state = Recomposing::Finished(next + 1);
                        return Some(mark);
                    }
                    None => {
                        self.marks.clear();
                        return self.starter.take();
                    }
                },
            }
        }
    }
}

/// `s` in Normalization Form C.
pub(crate) fn nfc(s: &str) -> Recompositions<'_> {
    Recompositions::new(s, false)
}

/// `s` in Normalization Form D.
pub(crate) fn nfd(s: &str) -> Decompositions<'_> {
    Decompositions::new(s, false)
}

/// `s` in Normalization Form KC.
pub(crate) fn nfkc(s: &str) -> Recompositions<'_> {
    Recompositions::new(s, true)
}

/// `s` in Normalization Form KD.
pub(crate) fn nfkd(s: &str) -> Decompositions<'_> {
    Decompositions::new(s, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;
    use unicode_normalization::UnicodeNormalization;

    fn chars() -> impl Iterator<Item = char> {
        (0..=0x10FFFFu32).filter_map(char::from_u32)
    }

    fn hangul(c: char) -> bool {
        (S_BASE..S_BASE + S_COUNT).contains(&(c as u32))
    }

    fn canonical(c: char) -> String {
        let mut out = String::new();
        unicode_normalization::char::decompose_canonical(c, |d| out.push(d));
        out
    }

    fn compatible(c: char) -> String {
        let mut out = String::new();
        unicode_normalization::char::decompose_compatible(c, |d| out.push(d));
        out
    }

    fn bytes(name: &str, data: &[u8]) -> String {
        let mut text = format!("pub(super) static {name}: &[u8] = b\"\\\n");
        for line in data.chunks(24) {
            text.push_str("    ");
            for byte in line {
                write!(text, "\\x{byte:02x}").unwrap();
            }
            text.push_str("\\\n");
        }
        text.push_str("\";\n");
        text
    }

    fn numbers<T: std::fmt::Display>(name: &str, kind: &str, data: &[T]) -> String {
        let mut text = format!("pub(super) static {name}: &[{kind}] = &[\n");
        for line in data.chunks(12) {
            let row: Vec<String> = line.iter().map(T::to_string).collect();
            writeln!(text, "    {},", row.join(", ")).unwrap();
        }
        text.push_str("];\n");
        text
    }

    fn string(name: &str, data: &str) -> String {
        let mut text = format!("pub(super) static {name}: &str = \"\\\n");
        let all: Vec<char> = data.chars().collect();
        for line in all.chunks(12) {
            text.push_str("    ");
            for c in line {
                write!(text, "\\u{{{:x}}}", *c as u32).unwrap();
            }
            text.push_str("\\\n");
        }
        text.push_str("\";\n");
        text
    }

    /// The three tables of a [`Ranked`] over the sorted `members`.
    fn ranked(prefix: &str, members: &[u32]) -> String {
        let last = members.last().map_or(0, |&c| (c >> 8) as usize + 1);
        let (mut pages, mut bits) = (vec![0u8; last], Vec::new());
        for &c in members {
            let page = (c >> 8) as usize;
            if pages[page] == 0 {
                bits.extend([0u8; 32]);
                pages[page] =
                    u8::try_from(bits.len() / 32).expect("at most 255 pages have members");
            }
            let slot = usize::from(pages[page]) - 1;
            bits[slot * 32 + ((c as usize & 255) >> 3)] |= 1 << (c & 7);
        }
        let (mut below, mut count) = (Vec::new(), 0u32);
        for word in bits.chunks_exact(8) {
            below.push(u16::try_from(count).expect("ranks fit 16 bits"));
            count += word.iter().map(|byte| byte.count_ones()).sum::<u32>();
        }
        numbers(&format!("{prefix}_PAGES"), "u8", &pages)
            + &bytes(&format!("{prefix}_BITS"), &bits)
            + &numbers(&format!("{prefix}_BELOW"), "u16", &below)
    }

    /// A decomposition table: its keys, the text each maps to, and where.
    fn decomposition_table(prefix: &str, entries: &[(char, String)]) -> String {
        let keys: Vec<u32> = entries.iter().map(|entry| entry.0 as u32).collect();
        let (mut offsets, mut text) = (Vec::new(), String::new());
        for (_, mapping) in entries {
            offsets.push(u16::try_from(text.len()).expect("the text fits 16-bit offsets"));
            text.push_str(mapping);
        }
        offsets.push(u16::try_from(text.len()).expect("the text fits 16-bit offsets"));
        ranked(prefix, &keys)
            + &numbers(&format!("{prefix}_OFFSETS"), "u16", &offsets)
            + &string(&format!("{prefix}_TEXT"), &text)
    }

    /// Every pair the crate composes, as (first, second, composite), sorted.
    fn composition_pairs() -> Vec<(u32, u32, u32)> {
        // A composing pair is a composite's own two-part decomposition, so
        // its first part is a composite or the first character of some
        // decomposition, and its second is a composite, a character of some
        // decomposition or a combining mark.
        let mut firsts = std::collections::BTreeSet::new();
        let mut seconds = std::collections::BTreeSet::new();
        for c in chars().filter(|&c| !hangul(c)) {
            let full = canonical(c);
            if full.chars().ne(std::iter::once(c)) {
                firsts.insert(c);
                seconds.insert(c);
                firsts.extend(full.chars().next());
                seconds.extend(full.chars());
            }
            if unicode_normalization::char::canonical_combining_class(c) != 0 {
                seconds.insert(c);
            }
        }
        let mut pairs = Vec::new();
        for &a in &firsts {
            for &b in &seconds {
                if let Some(c) = unicode_normalization::char::compose(a, b) {
                    if !hangul(c) {
                        pairs.push((a as u32, b as u32, c as u32));
                    }
                }
            }
        }
        pairs
    }

    fn generated() -> String {
        let canonical_entries: Vec<(char, String)> = chars()
            .filter(|&c| !hangul(c))
            .map(|c| (c, canonical(c)))
            .filter(|(c, full)| full.chars().ne(std::iter::once(*c)))
            .collect();
        // Only where the compatibility decomposition differs: the canonical
        // table answers for the rest.
        let compatible_entries: Vec<(char, String)> = chars()
            .filter(|&c| !hangul(c))
            .map(|c| (c, compatible(c)))
            .filter(|(c, full)| *full != canonical(*c))
            .collect();
        let pairs = composition_pairs();
        assert!(
            pairs.iter().all(|pair| pair.1 >= 0x300),
            "`compose` takes no second character to be below U+0300"
        );
        let (mut firsts, mut starts, mut bmp, mut beyond) =
            (Vec::new(), Vec::new(), Vec::<u16>::new(), Vec::new());
        for &(a, b, c) in &pairs {
            if a < 0x10000 && b < 0x10000 {
                if firsts.last() != Some(&a) {
                    firsts.push(a);
                    starts.push(u16::try_from(bmp.len() / 2).expect("pairs fit 16 bits"));
                }
                bmp.extend([
                    b as u16,
                    u16::try_from(c).expect("a BMP pair composes in the BMP"),
                ]);
            } else {
                beyond.push(format!("({a:#x}, {b:#x}, {c:#x})"));
            }
        }
        starts.push(u16::try_from(bmp.len() / 2).expect("pairs fit 16 bits"));
        let classed: Vec<(u32, u8)> = chars()
            .map(|c| {
                (
                    c as u32,
                    unicode_normalization::char::canonical_combining_class(c),
                )
            })
            .filter(|&(_, class)| class != 0)
            .collect();
        // Marks up to the supplementary planes that hold scripts; the few
        // runs beyond (plane 14) are listed.
        let marks: Vec<u32> = chars()
            .filter(|&c| unicode_normalization::char::is_combining_mark(c))
            .map(|c| c as u32)
            .collect();
        let near: Vec<u32> = marks.iter().copied().filter(|&c| c < 0x20000).collect();
        let mut far: Vec<(u32, u32)> = Vec::new();
        for &c in marks.iter().filter(|&&c| c >= 0x20000) {
            match far.last_mut() {
                Some(run) if run.1 + 1 == c => run.1 = c,
                _ => far.push((c, c)),
            }
        }
        let version = unicode_normalization::UNICODE_VERSION;
        let mut text = format!(
            "// Generated from the unicode-normalization crate (Unicode {}.{}.{}) by the\n\
             // test `unorm::tests::tables_are_generated_from_the_crate`; do not edit.\n\
             // ZIPP_REGENERATE_UNORM=1 cargo test -p zipp-vm --lib unorm::tests\n\
             //\n\
             // A set of code points is three tables: for each 256-code-point page, 0 or\n\
             // one more than its place in the next two; 32 bytes of membership bits a\n\
             // page; and the number of members before each 64 code points of each page.\n\n",
            version.0, version.1, version.2
        );
        text += "// Canonical decompositions, fully decomposed: the characters that have one,\n// and each one's text from its offset to the next.\n";
        text += &decomposition_table("CANONICAL", &canonical_entries);
        text += "\n// Compatibility decompositions that differ from the canonical one.\n";
        text += &decomposition_table("COMPATIBLE", &compatible_entries);
        text += "\n// Primary composites in the BMP: the first characters of the pairs, where\n// each one's pairs start, and the pairs as (second, composite).\n";
        text += &ranked("FIRST", &firsts);
        text += &numbers("FIRST_PAIRS", "u16", &starts);
        text += &numbers("COMPOSITION_PAIRS", "u16", &bmp);
        text += "\n// And beyond it, as (first, second, composite).\n";
        text += &format!(
            "pub(super) static COMPOSITION_BEYOND: &[(u32, u32, u32)] = &[\n{}];\n",
            beyond
                .iter()
                .map(|entry| format!("    {entry},\n"))
                .collect::<String>()
        );
        text += "\n// The characters with a non-zero canonical combining class, and the classes.\n";
        text += &ranked(
            "CLASS",
            &classed.iter().map(|entry| entry.0).collect::<Vec<_>>(),
        );
        text += &numbers(
            "CLASS_VALUES",
            "u8",
            &classed.iter().map(|entry| entry.1).collect::<Vec<_>>(),
        );
        text += "\n// Combining marks (General_Category M) below U+20000, and the runs beyond.\n";
        text += &ranked("MARK", &near);
        text += &format!(
            "pub(super) static MARK_BEYOND: &[(u32, u32)] = &[\n{}];\n",
            far.iter()
                .map(|run| format!("    ({:#x}, {:#x}),\n", run.0, run.1))
                .collect::<String>()
        );
        text
    }

    #[test]
    fn tables_are_generated_from_the_crate() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/unorm_tables.rs");
        let expected = generated();
        if std::env::var_os("ZIPP_REGENERATE_UNORM").is_some() {
            std::fs::write(&path, &expected).unwrap();
        }
        let current = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\r\n", "\n");
        assert!(
            current == expected,
            "src/unorm_tables.rs is out of date: see its first lines"
        );
    }

    #[test]
    fn every_code_point_agrees_with_the_crate() {
        let mut buffer = [0u8; 4];
        for c in chars() {
            assert_eq!(
                canonical_combining_class(c),
                unicode_normalization::char::canonical_combining_class(c),
                "class of {c:?}"
            );
            assert_eq!(
                is_combining_mark(c),
                unicode_normalization::char::is_combining_mark(c),
                "mark {c:?}"
            );
            let s: &str = c.encode_utf8(&mut buffer);
            assert!(nfd(s).eq(s.nfd()), "NFD of {c:?}");
            assert!(nfkd(s).eq(s.nfkd()), "NFKD of {c:?}");
            assert!(nfc(s).eq(s.nfc()), "NFC of {c:?}");
            assert!(nfkc(s).eq(s.nfkc()), "NFKC of {c:?}");
        }
    }

    #[test]
    fn every_composing_pair_agrees_with_the_crate() {
        // The crate's answer for every pair of characters that take part in
        // any composition, composing or not.
        let pairs = composition_pairs();
        let firsts: std::collections::BTreeSet<u32> = pairs.iter().map(|p| p.0).collect();
        let seconds: std::collections::BTreeSet<u32> = pairs.iter().map(|p| p.1).collect();
        let jamo = [
            0x1100, 0x1112, 0x1161, 0x1175, 0x11A7, 0x11A8, 0x11C2, 0xAC00, 0xAC01, 0xD7A3, 0xC8E0,
            0x61, 0x1F600,
        ];
        for &a in firsts.iter().chain(&jamo) {
            for &b in seconds.iter().chain(&jamo) {
                let (a, b) = (char::from_u32(a).unwrap(), char::from_u32(b).unwrap());
                assert_eq!(
                    compose(a, b),
                    unicode_normalization::char::compose(a, b),
                    "{a:?} {b:?}"
                );
            }
        }
    }

    #[test]
    fn generated_strings_agree_with_the_crate() {
        // Characters that take part in normalization, so that runs of marks
        // are reordered, blocked and composed, among a few that do not.
        let mut pool: Vec<char> = chars()
            .filter(|&c| {
                !hangul(c)
                    && (unicode_normalization::char::canonical_combining_class(c) != 0
                        || canonical(c).chars().ne(std::iter::once(c))
                        || compatible(c).chars().ne(std::iter::once(c)))
            })
            .collect();
        pool.extend(
            "aeiounAEIOU \u{1100}\u{1161}\u{11A8}\u{AC00}\u{AC01}\u{D7A3}\u{3099}\u{1F600}".chars(),
        );
        let mut seed = 0x5a17_2026_u64;
        let mut random = move |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % below as u64) as usize
        };
        let marks: Vec<char> = pool
            .iter()
            .copied()
            .filter(|&c| unicode_normalization::char::canonical_combining_class(c) != 0)
            .collect();
        let mut s = String::new();
        for _ in 0..300_000 {
            s.clear();
            for _ in 0..1 + random(9) {
                // Half the time a mark, so that most strings hold a run of them.
                s.push(if random(2) == 0 {
                    marks[random(marks.len())]
                } else {
                    pool[random(pool.len())]
                });
            }
            assert!(nfd(&s).eq(s.nfd()), "NFD of {s:?}");
            assert!(nfkd(&s).eq(s.nfkd()), "NFKD of {s:?}");
            assert!(nfc(&s).eq(s.nfc()), "NFC of {s:?}");
            assert!(nfkc(&s).eq(s.nfkc()), "NFKC of {s:?}");
        }
    }
}
