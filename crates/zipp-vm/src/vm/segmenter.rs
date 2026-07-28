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
//!   segment; every other scalar is its own segment. Correct for space- and
//!   punctuation-delimited scripts. **Wrong** for WB6/WB7 (the apostrophe in
//!   "can't", the full stop in "3.14") and for scripts that need a dictionary.
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

fn is_mark(c: char) -> bool {
    unicode_normalization::char::is_combining_mark(c)
}

/// A word-like scalar: what a `Word_Break` table would classify ALetter,
/// Numeric, Katakana or Hebrew_Letter, approximated by general category.
fn is_wordish(c: char) -> bool {
    c.is_alphanumeric() || is_mark(c)
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
    let mut out = vec![0usize];
    let mut pos = 0usize;
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if pos > 0 {
            let p = prev.unwrap();
            let joins = if word {
                is_wordish(c) && is_wordish(p)
            } else {
                is_mark(c) || (c == '\n' && p == '\r')
            };
            if !joins {
                out.push(pos);
            }
        }
        pos += char_units(c);
        prev = Some(c);
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
