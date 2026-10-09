//! Unicode normalization (NFC, NFD, NFKC, NFKD), canonical combining classes
//! and the combining-mark test, over compact tables.
//!
//! The engine used the `unicode-normalization` crate for these. Its tables
//! are minimal perfect hashes: 111 KB in a 32-bit module, of which 42 KB was
//! left after Brotli, since a hashed table has no order for a compressor to
//! find. They were the largest block of compressed data in every WebAssembly
//! module. The same mappings are held here as sorted keys over UTF-8 text
//! (`unorm_tables.rs`, generated from that crate by the test at the end of
//! this file) and found by binary search.
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

/// Entry `i` of a table of three-byte little-endian code points.
#[inline]
fn code_point(table: &[u8], i: usize) -> u32 {
    u32::from(table[i * 3]) | u32::from(table[i * 3 + 1]) << 8 | u32::from(table[i * 3 + 2]) << 16
}

/// The number of entries of the sorted three-byte table `keys` that are at
/// most `c`. `index[p]` is the number of entries below page `p` (256 code
/// points) for the BMP's pages and one past them, so a search stays inside
/// one page, and a page with no entry, as most are, is not searched.
fn rank(keys: &[u8], index: &[u16], c: u32) -> usize {
    let page = (c >> 8) as usize;
    let (mut low, mut high) = if page < 256 {
        (usize::from(index[page]), usize::from(index[page + 1]))
    } else {
        (usize::from(index[256]), keys.len() / 3)
    };
    while low < high {
        let middle = low + (high - low) / 2;
        if code_point(keys, middle) <= c {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

/// Whether the 256 code points around `c` hold any entry of the table that
/// `pages` (a bit a page) was made from.
#[inline]
fn in_page(pages: &[u8], c: u32) -> bool {
    let page = (c >> 8) as usize;
    pages
        .get(page >> 3)
        .is_some_and(|bits| bits >> (page & 7) & 1 != 0)
}

/// Whether `c` is in one of the runs `starts[i] ..= starts[i] + lengths[i]`,
/// and which.
fn run_of(starts: &[u8], index: &[u16], lengths: &[u8], c: u32) -> Option<usize> {
    // A run may reach into `c`'s page from the one before.
    let at = rank(starts, index, c).checked_sub(1)?;
    (c - code_point(starts, at) <= u32::from(lengths[at])).then_some(at)
}

/// The text `c` maps to in a decomposition table.
fn mapped(
    keys: &'static [u8],
    index: &'static [u16],
    offsets: &'static [u16],
    text: &'static str,
    c: u32,
) -> Option<&'static str> {
    let at = rank(keys, index, c).checked_sub(1)?;
    if code_point(keys, at) != c {
        return None;
    }
    Some(&text[usize::from(offsets[at])..usize::from(offsets[at + 1])])
}

/// The canonical combining class of `c` (0 for a starter).
pub(crate) fn canonical_combining_class(c: char) -> u8 {
    let c = c as u32;
    if c < 0x300 {
        return 0;
    }
    match run_of(
        tables::CLASS_STARTS,
        tables::CLASS_INDEX,
        tables::CLASS_LENGTHS,
        c,
    ) {
        Some(at) => tables::CLASS_VALUES[at],
        None => 0,
    }
}

/// Whether `c` has General_Category Mark (Mn, Mc or Me).
pub(crate) fn is_combining_mark(c: char) -> bool {
    let c = c as u32;
    c >= 0x300
        && run_of(
            tables::MARK_STARTS,
            tables::MARK_INDEX,
            tables::MARK_LENGTHS,
            c,
        )
        .is_some()
}

/// The full decomposition of `c`, canonical or compatibility, a character at
/// a time. A character with none is given back as it is.
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
                tables::COMPATIBLE_KEYS,
                tables::COMPATIBLE_INDEX,
                tables::COMPATIBLE_OFFSETS,
                tables::COMPATIBLE_TEXT,
                code,
            )
        })
        .flatten()
        .or_else(|| {
            mapped(
                tables::CANONICAL_KEYS,
                tables::CANONICAL_INDEX,
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
    // Few characters are ever the second of a pair.
    if !in_page(tables::SECOND_PAGES, b) {
        return None;
    }
    if a < 0x10000 && b < 0x10000 {
        let pairs = tables::COMPOSITION_PAIRS;
        let key = a << 16 | b;
        let (mut low, mut high) = (0, pairs.len() / 3);
        while low < high {
            let middle = low + (high - low) / 2;
            let entry = u32::from(pairs[middle * 3]) << 16 | u32::from(pairs[middle * 3 + 1]);
            match entry.cmp(&key) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => {
                    return char::from_u32(u32::from(pairs[middle * 3 + 2]))
                }
            }
        }
        return None;
    }
    tables::COMPOSITION_ASTRAL
        .iter()
        .find(|entry| entry.0 == a && entry.1 == b)
        .and_then(|entry| char::from_u32(entry.2))
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
}

impl Decompositions<'_> {
    /// The next character with its canonical combining class.
    fn next_classed(&mut self) -> Option<(u8, char)> {
        while self.ready == 0 {
            let Some(c) = self.source.next() else {
                if self.buffer.is_empty() {
                    return None;
                }
                self.buffer.sort_by_key(|entry| entry.0);
                self.ready = self.buffer.len();
                break;
            };
            let (buffer, ready) = (&mut self.buffer, &mut self.ready);
            decompose(c, self.compatible, |d| {
                let class = canonical_combining_class(d);
                if class == 0 {
                    // A starter closes the run before it. The sort is stable,
                    // so marks of one class keep their order.
                    buffer[*ready..].sort_by_key(|entry| entry.0);
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
            self.buffer.drain(..self.ready);
            self.next = 0;
            self.ready = 0;
        }
        Some(entry)
    }
}

impl Iterator for Decompositions<'_> {
    type Item = char;

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

    fn three(out: &mut Vec<u8>, c: u32) {
        out.extend_from_slice(&c.to_le_bytes()[..3]);
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

    /// One bit for each 256 code points, set where any of `members` is.
    fn pages(members: impl Iterator<Item = u32>) -> Vec<u8> {
        let mut bits = Vec::new();
        for c in members {
            let page = (c >> 8) as usize;
            if bits.len() <= page >> 3 {
                bits.resize((page >> 3) + 1, 0u8);
            }
            bits[page >> 3] |= 1 << (page & 7);
        }
        bits
    }

    /// For each page of the BMP and one past them, how many of the sorted
    /// `members` are below it.
    fn page_index(members: &[u32]) -> Vec<u16> {
        (0..=256u32)
            .map(|page| {
                let below = members.iter().filter(|&&c| c < page << 8).count();
                u16::try_from(below).expect("the table fits a 16-bit index")
            })
            .collect()
    }

    fn three_byte_values(table: &[u8]) -> Vec<u32> {
        (0..table.len() / 3).map(|i| code_point(table, i)).collect()
    }

    /// A decomposition table: sorted keys, the text each maps to, and where.
    fn decomposition_table(prefix: &str, entries: &[(char, String)]) -> String {
        let (mut keys, mut offsets, mut text) = (Vec::new(), Vec::new(), String::new());
        for (c, mapping) in entries {
            three(&mut keys, *c as u32);
            offsets.push(u16::try_from(text.len()).expect("the text fits 16-bit offsets"));
            text.push_str(mapping);
        }
        offsets.push(u16::try_from(text.len()).expect("the text fits 16-bit offsets"));
        numbers(
            &format!("{prefix}_INDEX"),
            "u16",
            &page_index(&three_byte_values(&keys)),
        ) + &bytes(&format!("{prefix}_KEYS"), &keys)
            + &numbers(&format!("{prefix}_OFFSETS"), "u16", &offsets)
            + &string(&format!("{prefix}_TEXT"), &text)
    }

    /// Runs of consecutive code points with one value, each at most 256 long.
    fn runs(values: impl Iterator<Item = (char, u8)>) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (mut starts, mut lengths, mut kinds) = (Vec::new(), Vec::new(), Vec::new());
        let mut open: Option<(u32, u32, u8)> = None;
        for (c, value) in values {
            let c = c as u32;
            match open {
                Some((start, end, kind)) if end + 1 == c && kind == value && c - start <= 255 => {
                    open = Some((start, c, kind));
                }
                _ => {
                    if let Some((start, end, kind)) = open {
                        three(&mut starts, start);
                        lengths.push((end - start) as u8);
                        kinds.push(kind);
                    }
                    open = Some((c, c, value));
                }
            }
        }
        if let Some((start, end, kind)) = open {
            three(&mut starts, start);
            lengths.push((end - start) as u8);
            kinds.push(kind);
        }
        (starts, lengths, kinds)
    }

    /// Every pair the crate composes, as (first, second, composite).
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
        let mut bmp: Vec<u16> = Vec::new();
        let mut astral: Vec<String> = Vec::new();
        for &(a, b, c) in &pairs {
            if a < 0x10000 && b < 0x10000 {
                bmp.extend([
                    a as u16,
                    b as u16,
                    u16::try_from(c).expect("a BMP pair composes in the BMP"),
                ]);
            } else {
                astral.push(format!("({a:#x}, {b:#x}, {c:#x})"));
            }
        }
        let (class_starts, class_lengths, class_values) = runs(
            chars()
                .map(|c| (c, unicode_normalization::char::canonical_combining_class(c)))
                .filter(|&(_, class)| class != 0),
        );
        let (mark_starts, mark_lengths, _) = runs(
            chars()
                .filter(|&c| unicode_normalization::char::is_combining_mark(c))
                .map(|c| (c, 0)),
        );
        let version = unicode_normalization::UNICODE_VERSION;
        let mut text = format!(
            "// Generated from the unicode-normalization crate (Unicode {}.{}.{}) by the\n\
             // test `unorm::tests::tables_are_generated_from_the_crate`; do not edit.\n\
             // ZIPP_REGENERATE_UNORM=1 cargo test -p zipp-vm --lib unorm::tests\n\n",
            version.0, version.1, version.2
        );
        text += "// Canonical decompositions, fully decomposed: sorted three-byte keys, and\n// each key's text from its offset to the next.\n";
        text += &decomposition_table("CANONICAL", &canonical_entries);
        text += "\n// Compatibility decompositions that differ from the canonical one.\n";
        text += &decomposition_table("COMPATIBLE", &compatible_entries);
        text += "\n// The pages that hold the second character of any composing pair.\n";
        text += &numbers(
            "SECOND_PAGES",
            "u8",
            &pages(pairs.iter().map(|pair| pair.1)),
        );
        text += "\n// Primary composites in the BMP as (first, second, composite), sorted.\n";
        text += &numbers("COMPOSITION_PAIRS", "u16", &bmp);
        text += "\n// And beyond it.\n";
        text += &format!(
            "pub(super) static COMPOSITION_ASTRAL: &[(u32, u32, u32)] = &[\n{}];\n",
            astral
                .iter()
                .map(|entry| format!("    {entry},\n"))
                .collect::<String>()
        );
        text += "\n// Runs of one non-zero canonical combining class: three-byte starts, each\n// run's length less one, and its class.\n";
        text += &numbers(
            "CLASS_INDEX",
            "u16",
            &page_index(&three_byte_values(&class_starts)),
        );
        text += &bytes("CLASS_STARTS", &class_starts);
        text += &numbers("CLASS_LENGTHS", "u8", &class_lengths);
        text += &numbers("CLASS_VALUES", "u8", &class_values);
        text += "\n// Runs of combining marks (General_Category M).\n";
        text += &numbers(
            "MARK_INDEX",
            "u16",
            &page_index(&three_byte_values(&mark_starts)),
        );
        text += &bytes("MARK_STARTS", &mark_starts);
        text += &numbers("MARK_LENGTHS", "u8", &mark_lengths);
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
