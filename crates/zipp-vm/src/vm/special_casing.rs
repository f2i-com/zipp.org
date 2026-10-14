//! The **language-sensitive** case mappings of Unicode's `SpecialCasing.txt`.
//!
//! `String.prototype.toLocale{Lower,Upper}Case` differ from their
//! locale-independent siblings in exactly one way: for three languages the UCD
//! defines conditional mappings that override the default ones. ECMA-402
//! §String.prototype.toLocaleLowerCase step 6 defines `availableLocales` here as
//! "the languages for which the Unicode Character Database contains language
//! sensitive case mappings" — that is the closed set `az`, `lt`, `tr`, and this
//! module is the whole of it.
//!
//! Source: `SpecialCasing.txt` §"Language-Sensitive Mappings" (Unicode 16.0.0,
//! <https://www.unicode.org/Public/16.0.0/ucd/SpecialCasing.txt>), transcribed
//! row for row:
//!
//! ```text
//! # Lithuanian
//! 0307; 0307;      ;      ; lt After_Soft_Dotted;   # COMBINING DOT ABOVE
//! 0049; 0069 0307; 0049;  0049; lt More_Above;      # LATIN CAPITAL LETTER I
//! 004A; 006A 0307; 004A;  004A; lt More_Above;      # LATIN CAPITAL LETTER J
//! 012E; 012F 0307; 012E;  012E; lt More_Above;      # LATIN CAPITAL LETTER I WITH OGONEK
//! 00CC; 0069 0307 0300; 00CC; 00CC; lt;             # LATIN CAPITAL LETTER I WITH GRAVE
//! 00CD; 0069 0307 0301; 00CD; 00CD; lt;             # LATIN CAPITAL LETTER I WITH ACUTE
//! 0128; 0069 0307 0303; 0128; 0128; lt;             # LATIN CAPITAL LETTER I WITH TILDE
//! # Turkish and Azeri
//! 0130; 0069; 0130; 0130; tr;                       # LATIN CAPITAL LETTER I WITH DOT ABOVE
//! 0130; 0069; 0130; 0130; az;                       # (same)
//! 0307;     ; 0307; 0307; tr After_I;               # COMBINING DOT ABOVE
//! 0307;     ; 0307; 0307; az After_I;               # (same)
//! 0049; 0131; 0049; 0049; tr Not_Before_Dot;        # LATIN CAPITAL LETTER I
//! 0049; 0131; 0049; 0049; az Not_Before_Dot;        # (same)
//! 0069; 0069; 0130; 0130; tr;                       # LATIN SMALL LETTER I
//! 0069; 0069; 0130; 0130; az;                       # (same)
//! ```
//!
//! The four conditions are the definitions from the same file's header, and all
//! of them are stated in terms of the Canonical_Combining_Class of the
//! surrounding characters, which `unorm` already provides — no
//! new table is needed for them. `After_Soft_Dotted` additionally needs the
//! `Soft_Dotted` property, which is [`SOFT_DOTTED`] below.

use crate::unorm::canonical_combining_class as ccc;

/// The `Soft_Dotted` code points, from `PropList.txt` (Unicode 16.0.0):
/// 34 ranges, 50 code points. Only `After_Soft_Dotted` (Lithuanian uppercasing)
/// reads this.
const SOFT_DOTTED: [(u32, u32); 34] = [
    (0x0069, 0x006A),
    (0x012F, 0x012F),
    (0x0249, 0x0249),
    (0x0268, 0x0268),
    (0x029D, 0x029D),
    (0x02B2, 0x02B2),
    (0x03F3, 0x03F3),
    (0x0456, 0x0456),
    (0x0458, 0x0458),
    (0x1D62, 0x1D62),
    (0x1D96, 0x1D96),
    (0x1DA4, 0x1DA4),
    (0x1DA8, 0x1DA8),
    (0x1E2D, 0x1E2D),
    (0x1ECB, 0x1ECB),
    (0x2071, 0x2071),
    (0x2148, 0x2149),
    (0x2C7C, 0x2C7C),
    (0x1D422, 0x1D423),
    (0x1D456, 0x1D457),
    (0x1D48A, 0x1D48B),
    (0x1D4BE, 0x1D4BF),
    (0x1D4F2, 0x1D4F3),
    (0x1D526, 0x1D527),
    (0x1D55A, 0x1D55B),
    (0x1D58E, 0x1D58F),
    (0x1D5C2, 0x1D5C3),
    (0x1D5F6, 0x1D5F7),
    (0x1D62A, 0x1D62B),
    (0x1D65E, 0x1D65F),
    (0x1D692, 0x1D693),
    (0x1DF1A, 0x1DF1A),
    (0x1E04C, 0x1E04D),
    (0x1E068, 0x1E068),
];

fn is_soft_dotted(c: char) -> bool {
    let u = c as u32;
    SOFT_DOTTED.iter().any(|&(a, b)| u >= a && u <= b)
}

/// BestAvailableLocale over the UCD's language-sensitive set. `None` means the
/// default (locale-independent) mapping applies — ECMA-402's "und".
pub(crate) fn special_casing_language(tag: &str) -> Option<&'static str> {
    // The tag is already canonical, so the language subtag is the prefix up to
    // the first "-" and is lowercase.
    let lang = tag.split('-').next().unwrap_or("");
    match lang {
        "tr" => Some("tr"),
        "az" => Some("az"),
        "lt" => Some("lt"),
        _ => None,
    }
}

/// `After_Soft_Dotted`/`After_I`: is there a `pred`-matching character before
/// byte offset `i` of `s`, with no intervening character of combining class 0
/// or 230?
fn after(s: &str, i: usize, pred: impl Fn(char) -> bool) -> bool {
    for c in s[..i].chars().rev() {
        if pred(c) {
            return true;
        }
        let k = ccc(c);
        if k == 0 || k == 230 {
            return false;
        }
    }
    false
}

/// `More_Above`: is the character ending at byte offset `next` of `s`
/// followed by a combining class 230 character with no intervening character
/// of combining class 0 or 230?
fn more_above(s: &str, next: usize) -> bool {
    for c in s[next..].chars() {
        let k = ccc(c);
        if k == 230 {
            return true;
        }
        if k == 0 {
            return false;
        }
    }
    false
}

/// `Before_Dot`: is the character ending at byte offset `next` of `s`
/// followed by U+0307, with only characters of combining class other than 0
/// and 230 in between?
fn before_dot(s: &str, next: usize) -> bool {
    for c in s[next..].chars() {
        if c == '\u{307}' {
            return true;
        }
        let k = ccc(c);
        if k == 0 || k == 230 {
            return false;
        }
    }
    false
}

/// The language-sensitive lower/upper mapping of `s`, or `None` when `lang`
/// has none (so the caller keeps its default [`str_to_lowercase`]/[`str_to_uppercase`]).
///
/// Every character the conditional table does NOT name falls through to the
/// default mapping, so this is the full result, not a patch on top of one.
#[cfg(test)]
pub(crate) fn transform_case(s: &str, lang: &str, upper: bool) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    transform_case_each(s, lang, upper, &mut |c| out.push(c)).then_some(out)
}

/// [`transform_case`] one output character at a time, so a caller can size
/// the result in a first pass and build it into an admitted buffer in a
/// second (a case mapping can triple a string). Returns `false`, emitting
/// nothing, when `lang` has no language-sensitive mappings. The context
/// conditions read their neighbours from `s` itself, so no per-character copy
/// of the input is made.
pub(crate) fn transform_case_each(
    s: &str,
    lang: &str,
    upper: bool,
    emit: &mut impl FnMut(char),
) -> bool {
    if !matches!(lang, "tr" | "az" | "lt") {
        return false;
    }
    let turkic = lang == "tr" || lang == "az";
    for (i, c) in s.char_indices() {
        let next = i + c.len_utf8();
        let handled = if upper {
            match c {
                // tr/az: i uppercases to İ rather than I.
                'i' if turkic => {
                    emit('\u{130}');
                    true
                }
                // lt: the explicit dot a lowercase i carries is dropped again
                // when the letter it sits on becomes a capital.
                '\u{307}' if lang == "lt" && after(s, i, is_soft_dotted) => true,
                _ => false,
            }
        } else {
            match c {
                // tr/az: İ lowercases to plain i (its dot is inherent), and a
                // bare I loses its dot entirely unless one follows explicitly.
                '\u{130}' if turkic => {
                    emit('i');
                    true
                }
                '\u{307}' if turkic && after(s, i, |p| p == 'I') => true,
                'I' if turkic && !before_dot(s, next) => {
                    emit('\u{131}');
                    true
                }
                // lt: a capital I/J/Į that carries further accents above keeps
                // an explicit dot when it becomes lowercase, so the accents do
                // not sit where the dot belongs.
                'I' | 'J' | '\u{12E}' if lang == "lt" && more_above(s, next) => {
                    emit(match c {
                        'I' => 'i',
                        'J' => 'j',
                        _ => '\u{12F}',
                    });
                    emit('\u{307}');
                    true
                }
                // The three precomposed Lithuanian capitals decompose the same
                // way unconditionally.
                '\u{CC}' | '\u{CD}' | '\u{128}' if lang == "lt" => {
                    emit('i');
                    emit('\u{307}');
                    emit(match c {
                        '\u{CC}' => '\u{300}',
                        '\u{CD}' => '\u{301}',
                        _ => '\u{303}',
                    });
                    true
                }
                // Final_Sigma holds in these languages too.
                '\u{3A3}' => {
                    emit(lower_sigma(s, i));
                    true
                }
                _ => false,
            }
        };
        if !handled {
            // Not named by the conditional table: the default full mapping.
            if upper {
                to_upper(c).for_each(&mut *emit);
            } else {
                to_lower(c).for_each(&mut *emit);
            }
        }
    }
    true
}

// ---------------------------------------------------------------------------
// The default (locale-independent) mappings.
//
// `char::to_lowercase`/`to_uppercase` and `str::to_lowercase`/`to_uppercase`
// bring the standard library's conversion tables along: about 25 KB of
// `(char, u32)` pairs in every WebAssembly module. The same mappings are
// mostly runs of code points that move by one offset (A-Z by +32, the
// alternating Latin Extended pairs by one), so here they are runs and a few
// multi-character rows, generated from the standard library itself by
// `tools/gen_case_tables.py`. The tests compare every code point with the
// standard library, and `str_to_lowercase`'s final sigma with
// `str::to_lowercase` in every context a code point can make.

/// The first code point of a run in `CASE_LOWER`/`CASE_UPPER`.
const RUN_START: u32 = 0x1F_FFFF;
/// Run length minus one, at bit 21 (seven bits).
const RUN_LEN_SHIFT: u32 = 21;
/// The run covers every other code point.
const RUN_STRIDE2: u32 = 1 << 28;
/// A one-code-point entry whose value is a `CASE_MULTI` row, not an offset.
const RUN_MULTI: u32 = 1 << 29;

/// The default mapping of one character: one to three characters, like the
/// standard library's `ToLowercase`/`ToUppercase`.
#[derive(Clone, Debug)]
pub(crate) struct CaseMapping {
    chars: [char; 3],
    len: u8,
    at: u8,
}

impl CaseMapping {
    #[inline(always)]
    fn one(c: char) -> Self {
        CaseMapping {
            chars: [c, '\0', '\0'],
            len: 1,
            at: 0,
        }
    }
}

impl Iterator for CaseMapping {
    type Item = char;

    #[inline(always)]
    fn next(&mut self) -> Option<char> {
        if self.at >= self.len {
            return None;
        }
        let c = *self.chars.get(usize::from(self.at))?;
        self.at = self.at.wrapping_add(1);
        Some(c)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = usize::from(self.len.saturating_sub(self.at));
        (n, Some(n))
    }
}

/// What a code point maps to by a run table.
enum Mapped {
    /// Itself.
    Same,
    One(char),
    /// A `CASE_MULTI` row: two or three characters, zero-padded.
    Multi(&'static [u16; 3]),
}

/// The run of `table` covering `u`, if one does, and what it maps `u` to.
/// Written out, a branchless binary search over the runs' first code points
/// (the standard library's form), so that Lite, built for size, does not
/// leave each step a call.
#[inline(always)]
fn lookup(u: u32, table: &[(u32, i32)]) -> Mapped {
    let (mut base, mut size) = (0usize, table.len());
    while size > 1 {
        let half = size / 2;
        let mid = base + half;
        let Some(&(key, _)) = table.get(mid) else {
            break;
        };
        base = if key & RUN_START <= u { mid } else { base };
        size -= half;
    }
    // The last run starting at or before `u`, unless the first starts after.
    let Some(&(key, value)) = table.get(base).filter(|&&(key, _)| key & RUN_START <= u) else {
        return Mapped::Same;
    };
    let mut off = u.wrapping_sub(key & RUN_START);
    if key & RUN_STRIDE2 != 0 {
        if off & 1 != 0 {
            return Mapped::Same;
        }
        off >>= 1;
    }
    if off > (key >> RUN_LEN_SHIFT) & 0x7F {
        return Mapped::Same;
    }
    if key & RUN_MULTI != 0 {
        return match CASE_MULTI.get(value as usize) {
            Some(row) => Mapped::Multi(row),
            None => Mapped::Same,
        };
    }
    match char::from_u32(u.wrapping_add(value as u32)) {
        Some(m) => Mapped::One(m),
        None => Mapped::Same,
    }
}

/// The characters of a `CASE_MULTI` row.
#[inline(always)]
fn row_chars(row: &[u16; 3]) -> impl Iterator<Item = char> + '_ {
    row.iter()
        .take_while(|&&unit| unit != 0)
        .filter_map(|&unit| char::from_u32(u32::from(unit)))
}

#[inline(always)]
fn map_char(c: char, table: &[(u32, i32)]) -> CaseMapping {
    match lookup(c as u32, table) {
        Mapped::Same => CaseMapping::one(c),
        Mapped::One(m) => CaseMapping::one(m),
        Mapped::Multi(row) => {
            let mut out = CaseMapping {
                chars: ['\0'; 3],
                len: 0,
                at: 0,
            };
            for (slot, m) in out.chars.iter_mut().zip(row_chars(row)) {
                *slot = m;
                out.len = out.len.wrapping_add(1);
            }
            out
        }
    }
}

/// Appends the default mapping of `c`, which is not ASCII, to `out`.
#[inline(always)]
fn push_mapped(out: &mut String, c: char, table: &[(u32, i32)]) {
    match lookup(c as u32, table) {
        Mapped::Same => out.push(c),
        Mapped::One(m) => out.push(m),
        Mapped::Multi(row) => {
            for m in row_chars(row) {
                out.push(m);
            }
        }
    }
}

/// `c.to_lowercase()`, without the standard library's tables.
#[inline]
pub(crate) fn to_lower(c: char) -> CaseMapping {
    if c.is_ascii() {
        return CaseMapping::one(c.to_ascii_lowercase());
    }
    map_char(c, &CASE_LOWER)
}

/// `c.to_uppercase()`, without the standard library's tables.
#[inline]
pub(crate) fn to_upper(c: char) -> CaseMapping {
    if c.is_ascii() {
        return CaseMapping::one(c.to_ascii_uppercase());
    }
    map_char(c, &CASE_UPPER)
}

/// The UTF-8 length of `c`'s default upper (`upper`) or lower mapping. The
/// contextual final sigma has the length of the ordinary one.
pub(crate) fn mapped_len(c: char, upper: bool) -> usize {
    if c.is_ascii() {
        return 1;
    }
    match lookup(c as u32, if upper { &CASE_UPPER } else { &CASE_LOWER }) {
        Mapped::Same => c.len_utf8(),
        Mapped::One(m) => m.len_utf8(),
        Mapped::Multi(row) => row_chars(row).map(char::len_utf8).sum(),
    }
}

/// `s.to_uppercase()`, without the standard library's tables.
pub(crate) fn str_to_uppercase(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c.to_ascii_uppercase());
        } else {
            push_mapped(&mut out, c, &CASE_UPPER);
        }
    }
    out
}

/// `s.to_lowercase()`, without the standard library's tables: the default
/// mapping, and the one context rule of the default mappings, `Final_Sigma`
/// (a capital sigma after a cased letter, not followed by one, ignoring
/// case-ignorable characters on both sides, becomes the final form).
pub(crate) fn str_to_lowercase(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.char_indices() {
        if c.is_ascii() {
            out.push(c.to_ascii_lowercase());
        } else if c == '\u{3A3}' {
            out.push(lower_sigma(s, i));
        } else {
            push_mapped(&mut out, c, &CASE_LOWER);
        }
    }
    out
}

/// The lowercase of the capital sigma at byte `i` of `s`: the final form
/// after a cased letter and not before one (case-ignorable characters
/// skipped on both sides), the ordinary one otherwise.
fn lower_sigma(s: &str, i: usize) -> char {
    let before = s.get(..i).unwrap_or_default();
    let after = s.get(i + '\u{3A3}'.len_utf8()..).unwrap_or_default();
    if cased_past_ignorables(before.chars().rev()) && !cased_past_ignorables(after.chars()) {
        '\u{3C2}'
    } else {
        '\u{3C3}'
    }
}

/// Whether the first character of `chars` that is not case-ignorable is cased.
fn cased_past_ignorables(chars: impl Iterator<Item = char>) -> bool {
    for c in chars {
        if !skip_search(c, &CASE_IGNORABLE_RUNS, &CASE_IGNORABLE_OFFSETS) {
            return skip_search(c, &CASED_RUNS, &CASED_OFFSETS);
        }
    }
    false
}

/// Membership in a property stored as `core::unicode::unicode_data` stores
/// its properties: run headers holding a code-point prefix sum (low 21 bits)
/// and an index into `offsets` (high 11 bits), and the lengths, in code
/// points, of the alternating ranges outside and inside the property.
fn skip_search(c: char, runs: &[u32], offsets: &[u8]) -> bool {
    let needle = c as u32;
    // The first run whose prefix sum is past `needle`.
    let (mut last, mut hi) = (0usize, runs.len());
    while last < hi {
        let mid = last + (hi - last) / 2;
        let Some(&h) = runs.get(mid) else {
            break;
        };
        if h & 0x1F_FFFF <= needle {
            last = mid + 1;
        } else {
            hi = mid;
        }
    }
    let Some(&header) = runs.get(last) else {
        return false;
    };
    let mut idx = (header >> 21) as usize;
    let end = runs
        .get(last + 1)
        .map_or(offsets.len(), |&h| (h >> 21) as usize);
    let prev = match last.checked_sub(1).and_then(|p| runs.get(p)) {
        Some(&h) => h & 0x1F_FFFF,
        None => 0,
    };
    let total = needle.wrapping_sub(prev);
    let mut sum = 0u32;
    for &offset in offsets.get(idx..end.saturating_sub(1)).unwrap_or_default() {
        sum += u32::from(offset);
        if sum > total {
            break;
        }
        idx += 1;
    }
    idx % 2 == 1
}

// BEGIN GENERATED by tools/gen_case_tables.py: do not edit by hand.
// Default case mappings of Rust 1.92.0 (Unicode 17.0.0), and the `Cased` and
// `Case_Ignorable` properties of the regex crate's tables (see the generator).
#[rustfmt::skip]
static CASE_LOWER: [(u32, i32); 186] = [
    (0x3200041, 32), (0x2C000C0, 32), (0xC000D8, 32), (0x12E00100, 1), (0x20000130, 0), (0x10400132, 1),
    (0x10E00139, 1), (0x12C0014A, 1), (0x178, -121), (0x10400179, 1), (0x181, 210), (0x10200182, 1),
    (0x186, 206), (0x187, 1), (0x200189, 205), (0x18B, 1), (0x18E, 79), (0x18F, 202),
    (0x190, 203), (0x191, 1), (0x193, 205), (0x194, 207), (0x196, 211), (0x197, 209),
    (0x198, 1), (0x19C, 211), (0x19D, 213), (0x19F, 214), (0x104001A0, 1), (0x1A6, 218),
    (0x1A7, 1), (0x1A9, 218), (0x1AC, 1), (0x1AE, 218), (0x1AF, 1), (0x2001B1, 217),
    (0x102001B3, 1), (0x1B7, 219), (0x1B8, 1), (0x1BC, 1), (0x1C4, 2), (0x1C5, 1),
    (0x1C7, 2), (0x1C8, 1), (0x1CA, 2), (0x110001CB, 1), (0x110001DE, 1), (0x1F1, 2),
    (0x102001F2, 1), (0x1F6, -97), (0x1F7, -56), (0x126001F8, 1), (0x220, -130), (0x11000222, 1),
    (0x23A, 10795), (0x23B, 1), (0x23D, -163), (0x23E, 10792), (0x241, 1), (0x243, -195),
    (0x244, 69), (0x245, 71), (0x10800246, 1), (0x10200370, 1), (0x376, 1), (0x37F, 116),
    (0x386, 38), (0x400388, 37), (0x38C, 64), (0x20038E, 63), (0x2000391, 32), (0x10003A3, 32),
    (0x3CF, 8), (0x116003D8, 1), (0x3F4, -60), (0x3F7, 1), (0x3F9, -7), (0x3FA, 1),
    (0x4003FD, -130), (0x1E00400, 80), (0x3E00410, 32), (0x12000460, 1), (0x1340048A, 1), (0x4C0, 15),
    (0x10C004C1, 1), (0x15E004D0, 1), (0x4A00531, 48), (0x4A010A0, 7264), (0x10C7, 7264), (0x10CD, 7264),
    (0x9E013A0, 38864), (0xA013F0, 8), (0x1C89, 1), (0x5401C90, -3008), (0x401CBD, -3008), (0x19401E00, 1),
    (0x1E9E, -7615), (0x15E01EA0, 1), (0xE01F08, -8), (0xA01F18, -8), (0xE01F28, -8), (0xE01F38, -8),
    (0xA01F48, -8), (0x10601F59, -8), (0xE01F68, -8), (0xE01F88, -8), (0xE01F98, -8), (0xE01FA8, -8),
    (0x201FB8, -8), (0x201FBA, -74), (0x1FBC, -9), (0x601FC8, -86), (0x1FCC, -9), (0x201FD8, -8),
    (0x201FDA, -100), (0x201FE8, -8), (0x201FEA, -112), (0x1FEC, -7), (0x201FF8, -128), (0x201FFA, -126),
    (0x1FFC, -9), (0x2126, -7517), (0x212A, -8383), (0x212B, -8262), (0x2132, 28), (0x1E02160, 16),
    (0x2183, 1), (0x32024B6, 26), (0x5E02C00, 48), (0x2C60, 1), (0x2C62, -10743), (0x2C63, -3814),
    (0x2C64, -10727), (0x10402C67, 1), (0x2C6D, -10780), (0x2C6E, -10749), (0x2C6F, -10783), (0x2C70, -10782),
    (0x2C72, 1), (0x2C75, 1), (0x202C7E, -10815), (0x16202C80, 1), (0x10202CEB, 1), (0x2CF2, 1),
    (0x12C0A640, 1), (0x11A0A680, 1), (0x10C0A722, 1), (0x13C0A732, 1), (0x1020A779, 1), (0xA77D, -35332),
    (0x1080A77E, 1), (0xA78B, 1), (0xA78D, -42280), (0x1020A790, 1), (0x1120A796, 1), (0xA7AA, -42308),
    (0xA7AB, -42319), (0xA7AC, -42315), (0xA7AD, -42305), (0xA7AE, -42308), (0xA7B0, -42258), (0xA7B1, -42282),
    (0xA7B2, -42261), (0xA7B3, 928), (0x10E0A7B4, 1), (0xA7C4, -48), (0xA7C5, -42307), (0xA7C6, -35384),
    (0x1020A7C7, 1), (0xA7CB, -42343), (0x10E0A7CC, 1), (0xA7DC, -42561), (0xA7F5, 1), (0x320FF21, 32),
    (0x4E10400, 40), (0x46104B0, 40), (0x1410570, 39), (0x1C1057C, 39), (0xC1058C, 39), (0x210594, 39),
    (0x6410C80, 64), (0x2A10D50, 32), (0x3E118A0, 32), (0x3E16E40, 32), (0x3016EA0, 27), (0x421E900, 34),
];
#[rustfmt::skip]
static CASE_UPPER: [(u32, i32); 303] = [
    (0x3200061, -32), (0xB5, 743), (0x200000DF, 1), (0x2C000E0, -32), (0xC000F8, -32), (0xFF, 121),
    (0x12E00101, -1), (0x131, -232), (0x10400133, -1), (0x10E0013A, -1), (0x20000149, 2), (0x12C0014B, -1),
    (0x1040017A, -1), (0x17F, -300), (0x180, 195), (0x10200183, -1), (0x188, -1), (0x18C, -1),
    (0x192, -1), (0x195, 97), (0x199, -1), (0x19A, 163), (0x19B, 42561), (0x19E, 130),
    (0x104001A1, -1), (0x1A8, -1), (0x1AD, -1), (0x1B0, -1), (0x102001B4, -1), (0x1B9, -1),
    (0x1BD, -1), (0x1BF, 56), (0x1C5, -1), (0x1C6, -2), (0x1C8, -1), (0x1C9, -2),
    (0x1CB, -1), (0x1CC, -2), (0x10E001CE, -1), (0x1DD, -79), (0x110001DF, -1), (0x200001F0, 3),
    (0x1F2, -1), (0x1F3, -2), (0x1F5, -1), (0x126001F9, -1), (0x11000223, -1), (0x23C, -1),
    (0x20023F, 10815), (0x242, -1), (0x10800247, -1), (0x250, 10783), (0x251, 10780), (0x252, 10782),
    (0x253, -210), (0x254, -206), (0x200256, -205), (0x259, -202), (0x25B, -203), (0x25C, 42319),
    (0x260, -205), (0x261, 42315), (0x263, -207), (0x264, 42343), (0x265, 42280), (0x266, 42308),
    (0x268, -209), (0x269, -211), (0x26A, 42308), (0x26B, 10743), (0x26C, 42305), (0x26F, -211),
    (0x271, 10749), (0x272, -213), (0x275, -214), (0x27D, 10727), (0x280, -218), (0x282, 42307),
    (0x283, -218), (0x287, 42282), (0x288, -218), (0x289, -69), (0x20028A, -217), (0x28C, -71),
    (0x292, -219), (0x29D, 42261), (0x29E, 42258), (0x345, 84), (0x10200371, -1), (0x377, -1),
    (0x40037B, 130), (0x20000390, 4), (0x3AC, -38), (0x4003AD, -37), (0x200003B0, 5), (0x20003B1, -32),
    (0x3C2, -31), (0x10003C3, -32), (0x3CC, -64), (0x2003CD, -63), (0x3D0, -62), (0x3D1, -57),
    (0x3D5, -47), (0x3D6, -54), (0x3D7, -8), (0x116003D9, -1), (0x3F0, -86), (0x3F1, -80),
    (0x3F2, 7), (0x3F3, -116), (0x3F5, -96), (0x3F8, -1), (0x3FB, -1), (0x3E00430, -32),
    (0x1E00450, -80), (0x12000461, -1), (0x1340048B, -1), (0x10C004C2, -1), (0x4CF, -15), (0x15E004D1, -1),
    (0x4A00561, -48), (0x20000587, 6), (0x54010D0, 3008), (0x4010FD, 3008), (0xA013F8, -8), (0x1C80, -6254),
    (0x1C81, -6253), (0x1C82, -6244), (0x201C83, -6242), (0x1C85, -6243), (0x1C86, -6236), (0x1C87, -6181),
    (0x1C88, 35266), (0x1C8A, -1), (0x1D79, 35332), (0x1D7D, 3814), (0x1D8E, 35384), (0x19401E01, -1),
    (0x20001E96, 7), (0x20001E97, 8), (0x20001E98, 9), (0x20001E99, 10), (0x20001E9A, 11), (0x1E9B, -59),
    (0x15E01EA1, -1), (0xE01F00, 8), (0xA01F10, 8), (0xE01F20, 8), (0xE01F30, 8), (0xA01F40, 8),
    (0x20001F50, 12), (0x1F51, 8), (0x20001F52, 13), (0x1F53, 8), (0x20001F54, 14), (0x1F55, 8),
    (0x20001F56, 15), (0x1F57, 8), (0xE01F60, 8), (0x201F70, 74), (0x601F72, 86), (0x201F76, 100),
    (0x201F78, 128), (0x201F7A, 112), (0x201F7C, 126), (0x20001F80, 16), (0x20001F81, 17), (0x20001F82, 18),
    (0x20001F83, 19), (0x20001F84, 20), (0x20001F85, 21), (0x20001F86, 22), (0x20001F87, 23), (0x20001F88, 16),
    (0x20001F89, 17), (0x20001F8A, 18), (0x20001F8B, 19), (0x20001F8C, 20), (0x20001F8D, 21), (0x20001F8E, 22),
    (0x20001F8F, 23), (0x20001F90, 24), (0x20001F91, 25), (0x20001F92, 26), (0x20001F93, 27), (0x20001F94, 28),
    (0x20001F95, 29), (0x20001F96, 30), (0x20001F97, 31), (0x20001F98, 24), (0x20001F99, 25), (0x20001F9A, 26),
    (0x20001F9B, 27), (0x20001F9C, 28), (0x20001F9D, 29), (0x20001F9E, 30), (0x20001F9F, 31), (0x20001FA0, 32),
    (0x20001FA1, 33), (0x20001FA2, 34), (0x20001FA3, 35), (0x20001FA4, 36), (0x20001FA5, 37), (0x20001FA6, 38),
    (0x20001FA7, 39), (0x20001FA8, 32), (0x20001FA9, 33), (0x20001FAA, 34), (0x20001FAB, 35), (0x20001FAC, 36),
    (0x20001FAD, 37), (0x20001FAE, 38), (0x20001FAF, 39), (0x201FB0, 8), (0x20001FB2, 40), (0x20001FB3, 41),
    (0x20001FB4, 42), (0x20001FB6, 43), (0x20001FB7, 44), (0x20001FBC, 41), (0x1FBE, -7205), (0x20001FC2, 45),
    (0x20001FC3, 46), (0x20001FC4, 47), (0x20001FC6, 48), (0x20001FC7, 49), (0x20001FCC, 46), (0x201FD0, 8),
    (0x20001FD2, 50), (0x20001FD3, 4), (0x20001FD6, 51), (0x20001FD7, 52), (0x201FE0, 8), (0x20001FE2, 53),
    (0x20001FE3, 5), (0x20001FE4, 54), (0x1FE5, 7), (0x20001FE6, 55), (0x20001FE7, 56), (0x20001FF2, 57),
    (0x20001FF3, 58), (0x20001FF4, 59), (0x20001FF6, 60), (0x20001FF7, 61), (0x20001FFC, 58), (0x214E, -28),
    (0x1E02170, -16), (0x2184, -1), (0x32024D0, -26), (0x5E02C30, -48), (0x2C61, -1), (0x2C65, -10795),
    (0x2C66, -10792), (0x10402C68, -1), (0x2C73, -1), (0x2C76, -1), (0x16202C81, -1), (0x10202CEC, -1),
    (0x2CF3, -1), (0x4A02D00, -7264), (0x2D27, -7264), (0x2D2D, -7264), (0x12C0A641, -1), (0x11A0A681, -1),
    (0x10C0A723, -1), (0x13C0A733, -1), (0x1020A77A, -1), (0x1080A77F, -1), (0xA78C, -1), (0x1020A791, -1),
    (0xA794, 48), (0x1120A797, -1), (0x10E0A7B5, -1), (0x1020A7C8, -1), (0x10E0A7CD, -1), (0xA7F6, -1),
    (0xAB53, -928), (0x9E0AB70, -38864), (0x2000FB00, 62), (0x2000FB01, 63), (0x2000FB02, 64), (0x2000FB03, 65),
    (0x2000FB04, 66), (0x2000FB05, 67), (0x2000FB06, 67), (0x2000FB13, 68), (0x2000FB14, 69), (0x2000FB15, 70),
    (0x2000FB16, 71), (0x2000FB17, 72), (0x320FF41, -32), (0x4E10428, -40), (0x46104D8, -40), (0x1410597, -39),
    (0x1C105A3, -39), (0xC105B3, -39), (0x2105BB, -39), (0x6410CC0, -64), (0x2A10D70, -32), (0x3E118C0, -32),
    (0x3E16E60, -32), (0x3016EBB, -27), (0x421E922, -34),
];
#[rustfmt::skip]
static CASE_MULTI: [[u16; 3]; 73] = [
    [0x69, 0x307, 0x0], [0x53, 0x53, 0x0], [0x2BC, 0x4E, 0x0], [0x4A, 0x30C, 0x0],
    [0x399, 0x308, 0x301], [0x3A5, 0x308, 0x301], [0x535, 0x552, 0x0], [0x48, 0x331, 0x0],
    [0x54, 0x308, 0x0], [0x57, 0x30A, 0x0], [0x59, 0x30A, 0x0], [0x41, 0x2BE, 0x0],
    [0x3A5, 0x313, 0x0], [0x3A5, 0x313, 0x300], [0x3A5, 0x313, 0x301], [0x3A5, 0x313, 0x342],
    [0x1F08, 0x399, 0x0], [0x1F09, 0x399, 0x0], [0x1F0A, 0x399, 0x0], [0x1F0B, 0x399, 0x0],
    [0x1F0C, 0x399, 0x0], [0x1F0D, 0x399, 0x0], [0x1F0E, 0x399, 0x0], [0x1F0F, 0x399, 0x0],
    [0x1F28, 0x399, 0x0], [0x1F29, 0x399, 0x0], [0x1F2A, 0x399, 0x0], [0x1F2B, 0x399, 0x0],
    [0x1F2C, 0x399, 0x0], [0x1F2D, 0x399, 0x0], [0x1F2E, 0x399, 0x0], [0x1F2F, 0x399, 0x0],
    [0x1F68, 0x399, 0x0], [0x1F69, 0x399, 0x0], [0x1F6A, 0x399, 0x0], [0x1F6B, 0x399, 0x0],
    [0x1F6C, 0x399, 0x0], [0x1F6D, 0x399, 0x0], [0x1F6E, 0x399, 0x0], [0x1F6F, 0x399, 0x0],
    [0x1FBA, 0x399, 0x0], [0x391, 0x399, 0x0], [0x386, 0x399, 0x0], [0x391, 0x342, 0x0],
    [0x391, 0x342, 0x399], [0x1FCA, 0x399, 0x0], [0x397, 0x399, 0x0], [0x389, 0x399, 0x0],
    [0x397, 0x342, 0x0], [0x397, 0x342, 0x399], [0x399, 0x308, 0x300], [0x399, 0x342, 0x0],
    [0x399, 0x308, 0x342], [0x3A5, 0x308, 0x300], [0x3A1, 0x313, 0x0], [0x3A5, 0x342, 0x0],
    [0x3A5, 0x308, 0x342], [0x1FFA, 0x399, 0x0], [0x3A9, 0x399, 0x0], [0x38F, 0x399, 0x0],
    [0x3A9, 0x342, 0x0], [0x3A9, 0x342, 0x399], [0x46, 0x46, 0x0], [0x46, 0x49, 0x0],
    [0x46, 0x4C, 0x0], [0x46, 0x46, 0x49], [0x46, 0x46, 0x4C], [0x53, 0x54, 0x0],
    [0x544, 0x546, 0x0], [0x544, 0x535, 0x0], [0x544, 0x53B, 0x0], [0x54E, 0x546, 0x0],
    [0x544, 0x53D, 0x0],
];
#[rustfmt::skip]
static CASED_RUNS: [u32; 22] = [
    0x10A0, 0x6E013A0, 0x8201C80, 0x8A01F16, 0x9C024B6, 0x13202C00, 0x1360A640, 0x14E0AB30,
    0x16A0FB00, 0x1760FF21, 0x17E10400, 0x18610780, 0x1B210C80, 0x1C2118A0, 0x1D216E40, 0x1D61D400,
    0x1E21D6A6, 0x22C1DF00, 0x25A1E030, 0x2661E900, 0x26A1F130, 0x26F2F18A,
];
#[rustfmt::skip]
static CASED_OFFSETS: [u8; 317] = [
    65, 26, 6, 26, 47, 1, 10, 1, 4, 1, 5, 23, 1, 31, 1, 195, 1, 4, 4, 208,
    2, 35, 7, 2, 30, 5, 96, 1, 42, 4, 2, 2, 2, 4, 1, 1, 6, 1, 1, 3,
    1, 1, 1, 20, 1, 83, 1, 139, 8, 166, 1, 38, 9, 41, 0, 38, 1, 1, 5, 1,
    2, 43, 1, 4, 0, 86, 2, 6, 0, 11, 5, 43, 2, 3, 64, 192, 64, 0, 2, 6,
    2, 38, 2, 6, 2, 8, 1, 1, 1, 1, 1, 1, 1, 31, 2, 53, 1, 7, 1, 1,
    3, 3, 1, 7, 3, 4, 2, 6, 4, 13, 5, 3, 1, 7, 116, 1, 13, 1, 16, 13,
    101, 1, 4, 1, 2, 10, 1, 1, 3, 5, 6, 1, 1, 1, 1, 1, 1, 4, 1, 6,
    4, 1, 2, 4, 5, 5, 4, 1, 17, 32, 3, 2, 0, 52, 0, 229, 6, 4, 3, 2,
    12, 38, 1, 1, 5, 1, 0, 46, 18, 30, 132, 102, 3, 4, 1, 77, 20, 6, 1, 3,
    0, 43, 1, 14, 6, 80, 0, 7, 12, 5, 0, 26, 6, 26, 0, 80, 96, 36, 4, 36,
    116, 11, 1, 15, 1, 7, 1, 2, 1, 11, 1, 15, 1, 7, 1, 2, 0, 1, 2, 3,
    1, 42, 1, 9, 0, 51, 13, 51, 93, 22, 10, 22, 0, 64, 0, 64, 32, 25, 2, 25,
    0, 85, 1, 71, 1, 2, 2, 1, 2, 2, 2, 4, 1, 12, 1, 1, 1, 7, 1, 65,
    1, 4, 2, 8, 1, 7, 1, 28, 1, 4, 1, 5, 1, 1, 3, 7, 1, 0, 2, 25,
    1, 25, 1, 31, 1, 25, 1, 31, 1, 25, 1, 31, 1, 25, 1, 31, 1, 25, 1, 8,
    0, 10, 1, 20, 6, 6, 0, 62, 0, 68, 0, 26, 6, 26, 6, 26, 0,
];
#[rustfmt::skip]
static CASE_IGNORABLE_RUNS: [u32; 36] = [
    0x2B0, 0x2A0135D, 0x22201712, 0x22601FBD, 0x30602C7C, 0x34A03005, 0x3620A015, 0x37A0A4F8,
    0x37E0A60C, 0x3820FB1E, 0x4420FE00, 0x44A101FD, 0x48610780, 0x49210A01, 0x49E10D24, 0x4B610EAB,
    0x4C61182F, 0x5C21193B, 0x5CA11EF3, 0x64E13430, 0x6661611E, 0x66E16AF0, 0x67616D40, 0x68216F4F,
    0x68A1AFF0, 0x69E1BC9D, 0x6AA1CF00, 0x6B21D167, 0x6BA1DA00, 0x6CE1E000, 0x6E61E2AE, 0x7061E4EB,
    0x70E1E8D0, 0x72A1F3FB, 0x732E0001, 0x737F01F0,
];
#[rustfmt::skip]
static CASE_IGNORABLE_OFFSETS: [u8; 929] = [
    39, 1, 6, 1, 11, 1, 35, 1, 1, 1, 71, 1, 4, 1, 1, 1, 4, 1, 2, 2,
    0, 192, 4, 2, 4, 1, 9, 2, 1, 1, 251, 7, 207, 1, 5, 1, 49, 45, 1, 1,
    1, 2, 1, 2, 1, 1, 44, 1, 11, 6, 10, 11, 1, 1, 35, 1, 10, 21, 16, 1,
    101, 8, 1, 10, 1, 4, 33, 1, 1, 1, 30, 27, 91, 11, 58, 11, 4, 1, 2, 1,
    24, 24, 43, 3, 44, 1, 7, 2, 5, 9, 41, 58, 55, 1, 1, 1, 4, 8, 4, 1,
    3, 7, 10, 2, 13, 1, 15, 1, 58, 1, 4, 4, 8, 1, 20, 2, 26, 1, 2, 2,
    57, 1, 4, 2, 4, 2, 2, 3, 3, 1, 30, 2, 3, 1, 11, 2, 57, 1, 4, 5,
    1, 2, 4, 1, 20, 2, 22, 6, 1, 1, 58, 1, 2, 1, 1, 4, 8, 1, 7, 2,
    11, 2, 30, 1, 61, 1, 12, 1, 50, 1, 3, 1, 55, 1, 1, 3, 5, 3, 1, 4,
    7, 2, 11, 2, 29, 1, 58, 1, 2, 1, 6, 1, 5, 2, 20, 2, 28, 2, 57, 2,
    4, 4, 8, 1, 20, 2, 29, 1, 72, 1, 7, 3, 1, 1, 90, 1, 2, 7, 11, 9,
    98, 1, 2, 9, 9, 1, 1, 7, 73, 2, 27, 1, 1, 1, 1, 1, 55, 14, 1, 5,
    1, 2, 5, 11, 1, 36, 9, 1, 102, 4, 1, 6, 1, 2, 2, 2, 25, 2, 4, 3,
    16, 4, 13, 1, 2, 2, 6, 1, 15, 1, 94, 1, 0, 3, 0, 3, 29, 2, 30, 2,
    30, 2, 64, 2, 1, 7, 8, 1, 2, 11, 3, 1, 5, 1, 45, 5, 51, 1, 65, 2,
    34, 1, 118, 3, 4, 2, 9, 1, 6, 3, 219, 2, 2, 1, 58, 1, 1, 7, 1, 1,
    1, 1, 2, 8, 6, 10, 2, 1, 39, 1, 8, 46, 2, 12, 20, 4, 48, 1, 1, 5,
    1, 1, 5, 1, 40, 9, 12, 2, 32, 4, 2, 2, 1, 3, 56, 1, 1, 2, 3, 1,
    1, 3, 58, 8, 2, 2, 64, 6, 82, 3, 1, 13, 1, 7, 4, 1, 6, 1, 3, 2,
    50, 63, 13, 1, 34, 101, 0, 1, 1, 3, 11, 3, 13, 3, 13, 3, 13, 2, 12, 5,
    8, 2, 10, 1, 2, 1, 2, 5, 49, 5, 1, 10, 1, 1, 13, 1, 16, 13, 51, 33,
    0, 2, 113, 3, 125, 1, 15, 1, 96, 32, 47, 1, 0, 1, 36, 4, 3, 5, 5, 1,
    93, 6, 93, 3, 0, 1, 0, 6, 0, 1, 98, 4, 1, 10, 1, 1, 28, 4, 80, 2,
    14, 34, 78, 1, 23, 3, 102, 4, 3, 2, 8, 1, 3, 1, 4, 1, 25, 2, 5, 1,
    151, 2, 26, 18, 13, 1, 38, 8, 25, 11, 46, 3, 48, 1, 2, 4, 2, 2, 17, 1,
    21, 2, 66, 6, 2, 2, 2, 2, 12, 1, 8, 1, 35, 1, 11, 1, 51, 1, 1, 3,
    2, 2, 5, 2, 1, 1, 27, 1, 14, 2, 5, 2, 1, 1, 100, 5, 9, 3, 121, 1,
    2, 1, 4, 1, 0, 1, 147, 17, 0, 16, 3, 1, 12, 16, 34, 1, 2, 1, 169, 1,
    7, 1, 6, 1, 11, 1, 35, 1, 1, 1, 47, 1, 45, 2, 67, 1, 21, 3, 0, 1,
    226, 1, 149, 5, 0, 6, 1, 42, 1, 9, 0, 3, 1, 2, 5, 4, 40, 3, 4, 1,
    165, 2, 0, 4, 38, 1, 26, 5, 1, 1, 0, 2, 24, 1, 52, 6, 70, 11, 49, 4,
    123, 1, 54, 15, 41, 1, 2, 2, 10, 3, 49, 4, 2, 2, 2, 1, 4, 1, 10, 1,
    50, 3, 36, 5, 1, 8, 62, 1, 12, 2, 52, 9, 10, 4, 2, 1, 95, 3, 2, 1,
    1, 2, 6, 1, 2, 1, 157, 1, 3, 8, 21, 2, 57, 2, 3, 1, 37, 7, 3, 5,
    70, 6, 13, 1, 1, 1, 1, 1, 14, 2, 85, 8, 2, 3, 1, 1, 23, 1, 84, 6,
    1, 1, 4, 2, 1, 2, 238, 4, 6, 2, 1, 2, 27, 2, 85, 8, 2, 1, 1, 2,
    106, 1, 1, 1, 2, 6, 1, 1, 101, 1, 1, 1, 2, 4, 1, 5, 0, 9, 1, 2,
    0, 2, 1, 1, 4, 1, 144, 4, 2, 2, 4, 1, 32, 10, 40, 6, 2, 4, 8, 1,
    9, 6, 2, 3, 46, 13, 1, 2, 198, 1, 1, 3, 1, 1, 201, 7, 1, 6, 1, 1,
    82, 22, 2, 7, 1, 2, 1, 2, 122, 6, 3, 1, 1, 2, 1, 7, 1, 1, 72, 2,
    3, 1, 1, 1, 65, 1, 0, 2, 11, 2, 52, 5, 5, 1, 1, 1, 23, 1, 0, 17,
    6, 15, 0, 12, 3, 3, 0, 5, 59, 7, 9, 4, 0, 3, 40, 2, 0, 1, 63, 17,
    64, 2, 1, 2, 13, 2, 0, 4, 1, 7, 1, 2, 0, 2, 1, 4, 0, 46, 2, 23,
    0, 3, 9, 16, 2, 7, 30, 4, 148, 3, 0, 55, 4, 50, 8, 1, 14, 1, 22, 5,
    1, 15, 0, 7, 1, 17, 2, 7, 1, 2, 1, 5, 5, 62, 33, 1, 160, 14, 0, 1,
    61, 4, 0, 5, 254, 2, 243, 1, 2, 1, 7, 2, 5, 1, 9, 1, 0, 7, 109, 8,
    0, 5, 0, 1, 30, 96, 128, 240, 0,
];
// END GENERATED by tools/gen_case_tables.py

#[cfg(test)]
mod tests {
    use super::*;

    /// The re-encoded tables against the form they replace: every code
    /// point's mapping, entry by entry, with the standard library's.
    #[test]
    fn default_mappings_match_the_standard_library() {
        for u in 0..=0x10_FFFFu32 {
            let Some(c) = char::from_u32(u) else { continue };
            assert!(to_lower(c).eq(c.to_lowercase()), "to_lower U+{u:04X}");
            assert!(to_upper(c).eq(c.to_uppercase()), "to_upper U+{u:04X}");
            assert_eq!(to_lower(c).size_hint().0, c.to_lowercase().count());
            let width = |m: &mut dyn Iterator<Item = char>| m.map(char::len_utf8).sum::<usize>();
            assert_eq!(
                mapped_len(c, false),
                width(&mut c.to_lowercase()),
                "lower width U+{u:04X}"
            );
            assert_eq!(
                mapped_len(c, true),
                width(&mut c.to_uppercase()),
                "upper width U+{u:04X}"
            );
        }
    }

    /// Next to a capital sigma a code point counts as cased, case-ignorable
    /// or neither; these four contexts tell the three apart on both sides,
    /// so the final-sigma rule (and the `Cased`/`Case_Ignorable` tables
    /// behind it) agree with `str::to_lowercase` for every code point.
    #[test]
    fn final_sigma_matches_the_standard_library() {
        let mut s = String::new();
        for u in 0..=0x10_FFFFu32 {
            let Some(c) = char::from_u32(u) else { continue };
            for (before, after) in [
                ("", "\u{3A3}"),
                ("A", "\u{3A3}"),
                ("A\u{3A3}", ""),
                ("\u{3A3}", ""),
            ] {
                s.clear();
                s.push_str(before);
                s.push(c);
                s.push_str(after);
                assert_eq!(
                    str_to_lowercase(&s),
                    s.to_lowercase(),
                    "U+{u:04X} in {before:?}..{after:?}"
                );
                assert_eq!(
                    str_to_uppercase(&s),
                    s.to_uppercase(),
                    "U+{u:04X} in {before:?}..{after:?}"
                );
            }
        }
    }

    #[test]
    fn strings_match_the_standard_library() {
        let samples = [
            "",
            "\u{3A3}",
            "\u{3A3}\u{3A3}",
            "A\u{3A3}",
            "A\u{3A3}\u{3A3}",
            "\u{391}\u{3A3} \u{3A3}\u{391}\u{3A3}.",
            "A\u{3A3}'s \u{3A3}\u{308}x \u{3A3}\u{308}",
            "\u{1F88}\u{3A3}\u{301}\u{345} \u{130}\u{1E9E}\u{FB00}\u{149}",
            "Stra\u{DF}e \u{130}stanbul \u{17F}\u{212A}\u{2126} \u{10400}\u{1E900}",
            "\u{41F}\u{420}\u{418}\u{412}\u{415}\u{422} \u{4E16}\u{754C} \u{1F600}",
        ];
        for s in samples {
            assert_eq!(str_to_lowercase(s), s.to_lowercase(), "{s:?}");
            assert_eq!(str_to_uppercase(s), s.to_uppercase(), "{s:?}");
        }
    }

    #[test]
    fn turkish_dotted_and_dotless_i() {
        assert_eq!(transform_case("\u{130}", "tr", false).unwrap(), "i");
        assert_eq!(transform_case("I\u{307}", "tr", false).unwrap(), "i");
        // A class-220 mark may intervene between the I and its dot (After_I).
        assert_eq!(
            transform_case("I\u{323}\u{307}", "tr", false).unwrap(),
            "i\u{323}"
        );
        // A class-230 mark breaks both After_I and Not_Before_Dot.
        assert_eq!(
            transform_case("I\u{300}\u{307}", "tr", false).unwrap(),
            "\u{131}\u{300}\u{307}"
        );
        assert_eq!(transform_case("I", "tr", false).unwrap(), "\u{131}");
        assert_eq!(transform_case("i", "tr", true).unwrap(), "\u{130}");
        assert_eq!(transform_case("\u{131}", "tr", true).unwrap(), "I");
    }

    #[test]
    fn lithuanian_explicit_dot() {
        assert_eq!(
            transform_case("I\u{300}", "lt", false).unwrap(),
            "i\u{307}\u{300}"
        );
        assert_eq!(
            transform_case("\u{12E}\u{300}", "lt", false).unwrap(),
            "\u{12F}\u{307}\u{300}"
        );
        // No accent above: the ordinary mapping, no explicit dot.
        assert_eq!(transform_case("I", "lt", false).unwrap(), "i");
        // Uppercasing strips the dot the lowercase letter carried.
        assert_eq!(transform_case("i\u{307}", "lt", true).unwrap(), "I");
        assert_eq!(
            transform_case("i\u{323}\u{307}", "lt", true).unwrap(),
            "I\u{323}"
        );
        // Not after a Soft_Dotted letter: the dot stays.
        assert_eq!(transform_case("I\u{307}", "lt", true).unwrap(), "I\u{307}");
    }

    /// The language mappings keep the default `Final_Sigma` rule: a Greek
    /// string lowercases in tr, az and lt as it does by default.
    #[test]
    fn final_sigma_in_the_language_mappings() {
        for s in [
            "\u{391}\u{3A3}",
            "\u{3A3}",
            "\u{391}\u{3A3}\u{391}",
            "\u{391}\u{3A3} \u{3A3}\u{391}\u{3A3}.",
            "A\u{3A3}'s \u{3A3}\u{308}x \u{3A3}\u{308}",
        ] {
            for lang in ["tr", "az", "lt"] {
                assert_eq!(transform_case(s, lang, false).unwrap(), s.to_lowercase(), "{s:?} {lang}");
            }
        }
        // Cased context is read from the source, so a Turkish I counts.
        assert_eq!(transform_case("I\u{3A3}", "tr", false).unwrap(), "\u{131}\u{3C2}");
    }

    #[test]
    fn other_languages_are_untouched() {
        assert!(transform_case("I", "en", false).is_none());
        assert_eq!(special_casing_language("tr-TR"), Some("tr"));
        assert_eq!(special_casing_language("en-US"), None);
    }
}
