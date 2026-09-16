//! `Intl.Segmenter`'s segmentation, and the boundary search `%Segments%` needs.
//!
//! ## What this is NOT
//!
//! It is **not** UAX #29. The real algorithm is driven by the
//! `Grapheme_Cluster_Break`, `Word_Break` and `Sentence_Break` property tables
//! plus `emoji-data.txt`'s `Extended_Pictographic`, none of which this engine
//! ships; word breaking for Thai/Lao/Khmer/Japanese/Chinese additionally needs a
//! dictionary. What is implemented here is the part that follows from Unicode
//! properties zipp ALREADY has — general category, via `char::is_alphanumeric`
//! and `unicode_normalization`'s combining-mark predicate — and nothing is
//! invented to paper over the rest.
//!
//! Concretely:
//!
//! * **grapheme** — one Unicode scalar per segment, except that a combining mark
//!   (Mn/Mc/Me) joins the scalar before it (a subset of GB9/GB9a) and an LF
//!   joins a preceding CR (GB3). Correct for Latin/Greek/Cyrillic/Han/Kana and
//!   for any base+diacritic sequence. **Wrong** for Hangul jamo sequences
//!   (GB6-GB8), regional-indicator pairs (GB12/13), and ZWJ emoji sequences
//!   (GB11) — those need the break-property table.
//! * **word** — a maximal run of alphanumerics-and-marks is one word-like
//!   segment, plus the four INFIX rules WB6/WB7 (the apostrophe in "can't", the
//!   colon in "10:30") and WB11/WB12 (the full stop in "3.14"), whose character
//!   classes are the closed `MidLetter` / `MidNum` / `MidNumLet` sets below;
//!   every other scalar is its own segment. Correct for space- and
//!   punctuation-delimited scripts. **Wrong** for scripts that need a
//!   dictionary, and for the rest of the Word_Break table (ExtendNumLet,
//!   Regional_Indicator, WSegSpace).
//! * **sentence** — the whole string is one segment. There is no data-free way
//!   to tell a sentence-ending period from an abbreviation's, and a guess would
//!   be worse than the honest "one sentence".
//!
//! The failures this leaves are real failures and are meant to stay visible;
//! see `intl402/Segmenter/prototype/segment/containing/unbreakable-input.js`.
//!
//! ## Units
//!
//! Every offset here is a **UTF-16 code unit** position, because that is what
//! `%Segments.prototype%.containing` takes and what a Segment Data Object's
//! `index` reports. The `&str` passed in is the lossy view of the JS string
//! (each lone surrogate shown as U+FFFD), which has the SAME code-unit length,
//! so the positions are valid for slicing the exact string.

use crate::heap::char_units;

/// Conservative work for one boundary query. The current implementation
/// validates/counts units, materializes scalar values, walks them to derive all
/// boundaries, and (for word segments) may walk again for `isWordLike`.
pub(crate) fn segment_work_bound(byte_len: usize) -> u64 {
    (byte_len as u64).saturating_mul(8)
}

fn is_mark(c: char) -> bool {
    unicode_normalization::char::is_combining_mark(c)
}

/// A word-like scalar: what a `Word_Break` table would classify ALetter,
/// Numeric, Katakana or Hebrew_Letter, approximated by general category.
fn is_wordish(c: char) -> bool {
    c.is_alphanumeric() || is_mark(c)
}

/// Which of UAX #29's infix classes a scalar belongs to, if any:
/// `Letter` = `MidLetter` (WB6/WB7), `Num` = `MidNum` (WB11/WB12),
/// `Both` = `MidNumLetQ` = `MidNumLet` ∪ `Single_Quote`, which serves both.
///
/// Transcribed from `WordBreakProperty.txt` (Unicode 16.0.0,
/// <https://www.unicode.org/Public/16.0.0/ucd/auxiliary/WordBreakProperty.txt>):
/// MidLetter is 9 code points, MidNum 13, MidNumLet 7, Single_Quote 1. These
/// four classes are the entire data the infix rules need — every other operand
/// in WB6/7/11/12 is AHLetter or Numeric, which general category already gives.
#[derive(Clone, Copy, PartialEq)]
enum Mid {
    Letter,
    Num,
    Both,
}

fn mid_class(c: char) -> Option<Mid> {
    match c {
        // MidLetter
        '\u{3A}' | '\u{B7}' | '\u{387}' | '\u{55F}' | '\u{5F4}' | '\u{2027}' | '\u{FE13}'
        | '\u{FE55}' | '\u{FF1A}' => Some(Mid::Letter),
        // MidNum
        '\u{2C}' | '\u{3B}' | '\u{37E}' | '\u{589}' | '\u{60C}' | '\u{60D}' | '\u{66C}'
        | '\u{7F8}' | '\u{2044}' | '\u{FE50}' | '\u{FE54}' | '\u{FF0C}' | '\u{FF1B}' => {
            Some(Mid::Num)
        }
        // MidNumLet ∪ Single_Quote
        '\u{2E}' | '\u{2018}' | '\u{2019}' | '\u{2024}' | '\u{FE52}' | '\u{FF07}' | '\u{FF0E}'
        | '\u{27}' => Some(Mid::Both),
        _ => None,
    }
}

/// Does `chars[j]` bridge its two neighbours under WB6/7 or WB11/12 — i.e. is
/// it an infix character with a matching operand on each side? A bridged infix
/// takes no boundary on either side, so "3.14" and "can't" stay one segment.
fn bridges(chars: &[char], j: usize) -> bool {
    if j == 0 {
        return false;
    }
    match chars.get(j) {
        Some(&c) => bridges3(chars.get(j - 1).copied(), c, chars.get(j + 1).copied()),
        None => false,
    }
}

/// [`bridges`] for the infix candidate `c` between its neighbours `prev` and
/// `next` (absent at either end of the string).
fn bridges3(prev: Option<char>, c: char, next: Option<char>) -> bool {
    let Some(kind) = mid_class(c) else {
        return false;
    };
    let (Some(p), Some(n)) = (prev, next) else {
        return false;
    };
    let letters = p.is_alphabetic() && n.is_alphabetic();
    let numbers = p.is_numeric() && n.is_numeric();
    match kind {
        Mid::Letter => letters,
        Mid::Num => numbers,
        Mid::Both => letters || numbers,
    }
}

/// The segment starts, in UTF-16 code units, always beginning with 0 and
/// followed (implicitly) by the string's length. Empty for the empty string.
pub(crate) fn segment_starts(s: &str, granularity: &str) -> Vec<usize> {
    if s.is_empty() {
        return vec![];
    }
    if granularity == "sentence" {
        return vec![0];
    }
    let word = granularity == "word";
    // The infix rules need one scalar of lookahead on each side, so the word
    // walk indexes a materialized slice rather than streaming.
    let chars: Vec<char> = s.chars().collect();
    let mut out = vec![0usize];
    let mut pos = 0usize;
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 {
            let p = chars[i - 1];
            let joins = if word {
                // WB6/WB11 keep the boundary BEFORE a bridging infix closed;
                // WB7/WB12 keep the one after it closed.
                (is_wordish(c) && is_wordish(p)) || bridges(&chars, i) || bridges(&chars, i - 1)
            } else {
                is_mark(c) || (c == '\n' && p == '\r')
            };
            if !joins {
                out.push(pos);
            }
        }
        pos += char_units(c);
    }
    out
}

/// Whether a segment whose first code point is `first` is "word-like" — the
/// `isWordLike` field of a Segment Data Object, present only at
/// `granularity: "word"`. A word segment is word-like exactly when its first
/// scalar is, so the segment itself answers it; a lone surrogate reads as
/// U+FFFD, as in the lossy view the boundaries come from.
pub(crate) fn is_word_like_start(first: Option<u32>) -> bool {
    first.is_some_and(|cp| is_wordish(char::from_u32(cp).unwrap_or('\u{FFFD}')))
}

/// The segment containing code-unit index `n`: `(start, end)`, or `None` when
/// `n` is outside `[0, len)`. `end` is the next boundary (or the length).
pub(crate) fn segment_at(s: &str, granularity: &str, n: i64) -> Option<(usize, usize)> {
    let len = crate::heap::str_units(s);
    if n < 0 || n >= len as i64 {
        return None;
    }
    let n = n as usize;
    let starts = segment_starts(s, granularity);
    let i = match starts.binary_search(&n) {
        Ok(i) => i,
        // `n` may land inside a segment — or between the halves of a surrogate
        // pair, which is still "inside" the scalar's segment.
        Err(0) => return None,
        Err(i) => i - 1,
    };
    let end = starts.get(i + 1).copied().unwrap_or(len);
    Some((starts[i], end))
}

/// The boundary after `start`, for the iterator's forward walk: `start` is a
/// boundary at byte offset `at` of the WTF-8 `bytes`, and `len` is their
/// length in code units. It decodes forward from `at` only as far as the next
/// boundary, with the one scalar of lookbehind that WB7/WB12 need, so a
/// `for…of` over the segments is linear; re-deriving every boundary from 0 on
/// each step made it quadratic. A lone surrogate reads as U+FFFD, as it does
/// in the lossy view [`segment_starts`] walks.
pub(crate) fn next_boundary(
    bytes: &[u8],
    granularity: &str,
    start: usize,
    at: usize,
    len: usize,
) -> usize {
    if granularity == "sentence" {
        // The whole string is the one segment.
        return len;
    }
    let word = granularity == "word";
    let scalar = |cp: u32| char::from_u32(cp).unwrap_or('\u{FFFD}');
    // The scalar before `start`, if any: `bridges(start)` reads it.
    let before = (at > 0).then(|| {
        let mut b = at - 1;
        while b > 0 && bytes[b] & 0xC0 == 0x80 {
            b -= 1;
        }
        scalar(crate::heap::wtf8_decode(bytes, b).0)
    });
    let mut rest = crate::heap::wtf8_code_points(&bytes[at..])
        .map(scalar)
        .peekable();
    let Some(first) = rest.next() else {
        return len;
    };
    // At each candidate boundary `i`: `p1` is chars[i-1], `p2` chars[i-2].
    let (mut p2, mut p1) = (before, first);
    let mut pos = start + char_units(first);
    while let Some(c) = rest.next() {
        let joins = if word {
            // Exactly `segment_starts`' test: WB6/WB11 look at `bridges(i)`,
            // WB7/WB12 at `bridges(i - 1)`.
            (is_wordish(c) && is_wordish(p1))
                || bridges3(Some(p1), c, rest.peek().copied())
                || bridges3(p2, p1, Some(c))
        } else {
            is_mark(c) || (c == '\n' && p1 == '\r')
        };
        if !joins {
            return pos;
        }
        pos += char_units(c);
        p2 = Some(p1);
        p1 = c;
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_exact() {
        assert_eq!(segment_starts("ABCD", "grapheme"), vec![0, 1, 2, 3]);
        assert_eq!(segment_starts("a c", "word"), vec![0, 1, 2]);
        assert_eq!(segment_starts("a c", "sentence"), vec![0]);
        assert_eq!(segment_starts("", "grapheme"), Vec::<usize>::new());
        // "[object Object]" — the bracket is its own segment, the run is one word.
        assert_eq!(segment_starts("[object", "word"), vec![0, 1]);
    }

    #[test]
    fn infix_rules_keep_one_word() {
        // WB11/WB12: Numeric MidNumLet Numeric.
        assert_eq!(segment_starts("1.23", "word"), vec![0]);
        // WB6/WB7: AHLetter MidNumLetQ AHLetter.
        assert_eq!(segment_starts("can't", "word"), vec![0]);
        // WB6/WB7 with MidLetter.
        assert_eq!(segment_starts("a:b", "word"), vec![0]);
        // A trailing infix does NOT bridge — the sentence-final full stop is its
        // own segment.
        assert_eq!(segment_starts("hi.", "word"), vec![0, 2]);
        // MidNum joins numbers only; between letters it stays a break.
        assert_eq!(segment_starts("a,b", "word"), vec![0, 1, 2]);
        assert_eq!(segment_starts("1,2", "word"), vec![0]);
    }

    #[test]
    fn astral_scalars_count_two_units() {
        // A supplementary-plane scalar is ONE segment but TWO code units, so the
        // next segment starts at 2 (`index` is a UTF-16 offset).
        assert_eq!(segment_starts("\u{1F600}b", "grapheme"), vec![0, 2]);
        assert_eq!(segment_at("\u{1F600}b", "grapheme", 1), Some((0, 2)));
        assert_eq!(segment_at("\u{1F600}b", "grapheme", 2), Some((2, 3)));
        assert_eq!(segment_at("\u{1F600}b", "grapheme", 3), None);
        assert_eq!(segment_at("abc", "grapheme", -1), None);
    }

    /// Every boundary the iterator reaches by streaming `next_boundary` from
    /// one boundary to the next, in code units.
    fn walk(bytes: &[u8], granularity: &str) -> Vec<usize> {
        let len = crate::heap::wtf8_units(bytes);
        let mut out = Vec::new();
        let (mut start, mut at) = (0usize, 0usize);
        while start < len {
            out.push(start);
            let end = next_boundary(bytes, granularity, start, at, len);
            assert!(end > start, "no progress at {start}");
            // Advance the byte offset over the units just consumed.
            let mut units = start;
            while units < end {
                let (cp, blen) = crate::heap::wtf8_decode(bytes, at);
                units += if cp >= 0x10000 { 2 } else { 1 };
                at += blen;
            }
            start = end;
        }
        out
    }

    #[test]
    fn streaming_boundaries_match_the_full_walk() {
        let samples = [
            "",
            "a",
            "ABCD",
            "a c",
            "[object Object]",
            "can't won't 3.14 1,2 a,b a:b hi. 'x' .5 5.",
            "a\u{301}b\r\n\r\u{1F600}x\u{308}\u{308} y",
            "\u{2019}a\u{2019}b\u{2019} 1\u{FF0C}2 \u{1F600}\u{1F600}",
            "abcde fghi ".repeat(40).as_str(),
            "..a.b..1.2..",
            "x'y'z",
        ]
        .map(String::from);
        for s in &samples {
            for g in ["grapheme", "word", "sentence"] {
                assert_eq!(walk(s.as_bytes(), g), segment_starts(s, g), "{g}: {s:?}");
            }
        }
        // A lone surrogate (WTF-8 ED A0 80) walks like the U+FFFD the lossy
        // view shows in its place.
        let wtf8 = [b'a', 0xED, 0xA0, 0x80, b'b', b' ', 0xED, 0xB0, 0x80];
        for g in ["grapheme", "word"] {
            let lossy = String::from_utf8_lossy(&wtf8).replace("\u{FFFD}\u{FFFD}\u{FFFD}", "\u{FFFD}");
            assert_eq!(walk(&wtf8, g), segment_starts(&lossy, g), "{g}");
        }
    }

    #[test]
    fn combining_marks_join_their_base() {
        // GB9: "a" + COMBINING ACUTE is one cluster, so there is no boundary at 1.
        assert_eq!(segment_starts("a\u{301}b", "grapheme"), vec![0, 2]);
        assert_eq!(segment_starts("\r\n", "grapheme"), vec![0]);
    }
}
