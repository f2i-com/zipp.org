//! 15 September 2026 regex audit, engine half (FORK.md B8-B11).
//!
//! - B8: a one-character loop whose search started on the trailing half of a
//!   surrogate pair backtracked past its own start (the surrogate-aware step
//!   jumps over a mid-pair `min`), so the equality test that ends the loop
//!   never fired and the next step reached `rs_unreachable!` — undefined
//!   behaviour in the unchecked build, a native crash from JavaScript.
//! - B9: `match_at_*` makes exactly one anchored attempt. It must agree with
//!   the unanchored search filtered to a start at the same offset, which is
//!   what the VM's sticky exec used to do at the cost of a whole-subject scan.
//! - B10: capture-group names are keyed by group id, so the groups inside a
//!   lookbehind (emitted in reverse) keep their own names.
//! - B11: `v` is a `[UnicodeMode]` grammar like `u`; `\b` in a `v` class is
//!   U+0008; a malformed `\x` in Annex B is the identity escape `x` followed
//!   by whatever came after it.

#![allow(clippy::uninlined_format_args)]

use regress::{Match, Regex};

fn ranges(m: &Match) -> (regress::Range, Vec<Option<regress::Range>>) {
    (m.range(), m.captures.clone())
}

#[cfg(feature = "utf16")]
fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// `"\u{1F600}ab"` and friends as code units, searched from index 1 — the
/// trailing half of the pair.
#[cfg(feature = "utf16")]
#[test]
fn loops_started_inside_a_surrogate_pair_backtrack_without_overshooting() {
    let emoji_ab = units("\u{1F600}ab");
    let emoji_abx = units("\u{1F600}abx");
    for pattern in [".*x", ".?x", "[^]*x", ".*?x", ".??x", "[^]*?x", "(?<=q.*)", "(?<=q.*?)"] {
        let re = Regex::with_flags(pattern, "u").unwrap();
        assert!(
            re.find_from_utf16(&emoji_ab, 1).next().is_none(),
            "find_from_utf16 /{pattern}/u from 1"
        );
        assert!(
            re.match_at_utf16(&emoji_ab, 1).is_none(),
            "match_at_utf16 /{pattern}/u at 1"
        );
    }
    // The successful shapes still match from the requested start (the VM, not
    // the engine, maps a mid-pair lastIndex back to the pair's start).
    let re = Regex::with_flags(".*x", "u").unwrap();
    assert_eq!(re.match_at_utf16(&emoji_abx, 1).map(|m| m.range()), Some(1..5));
    let re = Regex::with_flags(".*", "u").unwrap();
    assert_eq!(re.match_at_utf16(&emoji_ab, 1).map(|m| m.range()), Some(1..4));
    // The zero-iteration alternative at the start is still tried: the lone
    // trailing half is the continuation here.
    let re = Regex::with_flags("[^]*\\uDE00", "u").unwrap();
    assert_eq!(re.match_at_utf16(&emoji_ab, 1).map(|m| m.range()), Some(1..2));
}

fn assert_anchored_agrees(pattern: &str, flags: &str, text: &str) {
    let re = Regex::with_flags(pattern, flags).unwrap();
    for start in 0..=text.len() + 1 {
        let anchored = re.match_at_ascii(text, start).map(|m| ranges(&m));
        let filtered = re
            .find_from_ascii(text, start)
            .next()
            .filter(|m| m.start() == start)
            .map(|m| ranges(&m));
        assert_eq!(anchored, filtered, "/{pattern}/{flags} on {text:?} at {start}");
        #[cfg(feature = "utf16")]
        {
            let u = units(text);
            let anchored = re.match_at_ucs2(&u, start).map(|m| ranges(&m));
            let filtered = re
                .find_from_ucs2(&u, start)
                .next()
                .filter(|m| m.start() == start)
                .map(|m| ranges(&m));
            assert_eq!(anchored, filtered, "ucs2 /{pattern}/{flags} at {start}");
            let anchored = re.match_at_utf16(&u, start).map(|m| ranges(&m));
            let filtered = re
                .find_from_utf16(&u, start)
                .next()
                .filter(|m| m.start() == start)
                .map(|m| ranges(&m));
            assert_eq!(anchored, filtered, "utf16 /{pattern}/{flags} at {start}");
        }
    }
}

#[test]
fn match_at_is_the_start_filtered_search() {
    let text = "aab, foo abc,\nbar  ,x ABC";
    for (pattern, flags) in [
        ("a", ""),
        ("a*", ""),
        ("b*,", ""),
        ("\\s*,\\s*", ""),
        ("(?<=a)b", ""),
        ("(?<!a)b", ""),
        ("^a", ""),
        ("^b", "m"),
        ("$", "m"),
        ("\\bfoo", ""),
        ("(a|ab)(c|bcd)?", ""),
        ("x*", ""),
        ("abc", "i"),
        ("[a-c]+", ""),
        ("(?:)", ""),
        ("(\\w)\\1", ""),
        ("\\d+", ""),
    ] {
        assert_anchored_agrees(pattern, flags, text);
    }
}

#[test]
fn match_at_makes_one_attempt() {
    let re = Regex::new("b*,").unwrap();
    let text = format!("{},", "a".repeat(4096));
    // Every start but the last fails immediately: the attempt at `start`
    // sees `a`, and there is no scan forward to the comma.
    for start in 0..4096 {
        assert!(re.match_at_ascii(&text, start).is_none());
    }
    assert_eq!(re.match_at_ascii(&text, 4096).map(|m| m.range()), Some(4096..4097));
    assert!(re.match_at_ascii(&text, 4098).is_none(), "past the end");
}

#[test]
fn named_groups_inside_lookbehind_keep_their_ids() {
    let text = "12.50USD";
    let re = Regex::new(r"(?<=(?<int>\d+)\.(?<frac>\d+))USD").unwrap();
    let m = re.find(text).unwrap();
    assert_eq!(m.named_group("int"), Some(0..2));
    assert_eq!(m.named_group("frac"), Some(3..5));
    let names: Vec<_> = m.named_groups().map(|(n, r)| (n, r)).collect();
    assert_eq!(names, vec![("int", Some(0..2)), ("frac", Some(3..5))]);

    let re = Regex::new(r"(?<=(?<a>p)(?<b>q)(?<c>r))").unwrap();
    let m = re.find("pqr").unwrap();
    let names: Vec<_> = m.named_groups().collect();
    assert_eq!(
        names,
        vec![("a", Some(0..1)), ("b", Some(1..2)), ("c", Some(2..3))]
    );
    // Unnamed groups interleaved, and a group after the lookbehind.
    let re = Regex::new(r"(?<=(x)(?<a>a)(y)(?<b>b))(?<c>c)").unwrap();
    let m = re.find("xaybc").unwrap();
    assert_eq!(m.named_group("a"), Some(1..2));
    assert_eq!(m.named_group("b"), Some(3..4));
    assert_eq!(m.named_group("c"), Some(4..5));
    assert_eq!(m.group(1), Some(0..1));
    assert_eq!(m.group(3), Some(2..3));
}

#[test]
fn unicode_sets_mode_is_a_unicode_mode_grammar() {
    for pattern in [
        "a{", "q{3", "(?=a)+", "(?!a)*", "a}", "a]", "{", "}", "]", "x{1,", "\\c", "\\c1", "\\x1",
        "\\xZZ", "\\u12", "\\u", "\\1", "\\01", "\\00", "\\a", "\\z", "\\-", "\\k", "\\k<a>", "\\8",
        "[\\u12]", "[\\a]", "[\\1]",
    ] {
        for flags in ["u", "v"] {
            assert!(
                Regex::with_flags(pattern, flags).is_err(),
                "/{pattern}/{flags} must be a SyntaxError"
            );
        }
    }
    for pattern in [
        "a{1}", "a{1,2}", "(?:a){2}", "\\/", "\\^", "\\$", "[\\-]", "[\\&]", "[\\q{abc}]", "\\p{L}",
        "(?<n>a)\\k<n>", "(a)\\1", "\\0", "\\cA",
    ] {
        assert!(Regex::with_flags(pattern, "v").is_ok(), "/{pattern}/v is valid");
    }
    // The same text stays valid Annex B without a unicode flag.
    for pattern in ["a{", "a}", "a]", "\\c", "\\x1", "\\a", "\\8", "(?=a)+"] {
        assert!(Regex::new(pattern).is_ok(), "/{pattern}/ is Annex B");
    }
}

#[test]
fn class_set_backspace_escape() {
    let re = Regex::with_flags("[\\b]", "v").unwrap();
    assert!(re.find("\u{8}").is_some());
    assert!(re.find("b").is_none());
    let re = Regex::with_flags("[\\b-\\n]", "v").unwrap();
    assert!(re.find("\t").is_some());
    let re = Regex::with_flags("[\\b]", "u").unwrap();
    assert!(re.find("\u{8}").is_some());
    assert!(re.find("b").is_none());
}

#[test]
fn annex_b_malformed_hex_escape_rereads_the_following_text() {
    let find = |p: &str, t: &str| Regex::new(p).unwrap().find(t).map(|m| m.range());
    assert_eq!(find("\\x1", "x1"), Some(0..2));
    assert_eq!(find("a\\xZ", "axZ"), Some(0..3));
    assert_eq!(find("\\x", "x"), Some(0..1));
    assert_eq!(find("[\\x1]", "1"), Some(0..1));
    assert_eq!(find("[\\x1]", "x"), Some(0..1));
    assert_eq!(find("\\x41", "A"), Some(0..1));
}
