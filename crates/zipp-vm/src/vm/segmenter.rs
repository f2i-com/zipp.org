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
    let Some(kind) = chars.get(j).copied().and_then(mid_class) else {
        return false;
    };
    let (Some(&p), Some(&n)) = (chars.get(j.wrapping_sub(1)), chars.get(j + 1)) else {
        return false;
    };
    if j == 0 {
        return false;
    }
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

/// Whether the segment starting at `start` is "word-like" — the `isWordLike`
/// field of a Segment Data Object, present only at `granularity: "word"`.
pub(crate) fn is_word_like(s: &str, start: usize) -> bool {
    let mut pos = 0usize;
    for c in s.chars() {
        if pos == start {
            return is_wordish(c);
        }
        pos += char_units(c);
    }
    false
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

/// The boundary after `start`, for the iterator's forward walk.
pub(crate) fn segment_end(s: &str, granularity: &str, start: usize) -> usize {
    let len = crate::heap::str_units(s);
    segment_starts(s, granularity)
        .into_iter()
        .find(|&b| b > start)
        .unwrap_or(len)
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

    #[test]
    fn combining_marks_join_their_base() {
        // GB9: "a" + COMBINING ACUTE is one cluster, so there is no boundary at 1.
        assert_eq!(segment_starts("a\u{301}b", "grapheme"), vec![0, 2]);
        assert_eq!(segment_starts("\r\n", "grapheme"), vec![0]);
    }
}
