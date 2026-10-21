//! `v`-mode class set expressions (FORK.md B12), checked against QuickJS's
//! `test_builtin.js` regexp block and the spec where V8 differs.
//!
//! - Under `vi` the operands are case folded before `&&` and `--` run
//!   (MaybeSimpleCaseFolding), and a complement is taken within the folded
//!   code points.
//! - A nested `[^…]` is complemented; it used to be parsed as `[…]`.
//! - MayContainStrings is syntactic and gates `[^…]`; one-code-point strings
//!   in a negated class are code points; `\q{}` is the empty string; strings
//!   are tried longest first.
//! - A lone `&` after the first operand no longer drops that operand.

#![allow(clippy::uninlined_format_args)]

use regress::Regex;

fn re(pattern: &str, flags: &str) -> Regex {
    Regex::with_flags(pattern, flags).unwrap_or_else(|e| panic!("/{pattern}/{flags}: {e:?}"))
}

/// Whether `pattern` matches the whole of `subject`.
fn whole(pattern: &str, flags: &str, subject: &str) -> bool {
    re(&format!("^(?:{pattern})$"), flags).find(subject).is_some()
}

fn ranges(pattern: &str, flags: &str, subject: &str) -> Vec<core::ops::Range<usize>> {
    re(pattern, flags).find_iter(subject).map(|m| m.range()).collect()
}

#[test]
fn nested_negated_classes_are_complemented() {
    assert!(whole("[[^b]]", "v", "a"));
    assert!(!whole("[[^b]]", "v", "b"));
    assert!(whole("[[^a]&&[a-c]]", "v", "b"));
    assert!(!whole("[[^a]&&[a-c]]", "v", "a"));
    assert!(whole("[^[^a]]", "v", "a"));
    assert!(!whole("[^[^a]]", "v", "b"));
}

#[test]
fn ignore_case_folds_operands_before_the_set_operators() {
    for s in ["b", "B"] {
        assert!(whole("[[a-c]&&B]", "vi", s), "{s}");
        assert!(!whole("[[a-c]--B]", "vi", s), "{s}");
    }
    for s in ["a", "A", "c", "C"] {
        assert!(!whole("[[a-c]&&B]", "vi", s), "{s}");
        assert!(whole("[[a-c]--B]", "vi", s), "{s}");
    }
    for s in ["k", "K", "\u{212A}"] {
        assert!(!whole("[\\w--k]", "vi", s), "{s}");
        assert!(whole("[K&&k]", "vi", s), "{s}");
    }
    assert!(whole("[\\w--k]", "vi", "s"));
    assert!(!whole("[\\u017F--s]", "vi", "\u{17F}"));
    assert!(whole("[\\u00DF&&\\u1E9E]", "vi", "\u{DF}"));
    // A complement inside a fold: `[^\P{Lower}]` is every cased letter.
    for s in ["a", "A", "\u{DF}", "\u{1E9E}"] {
        assert!(whole("[^\\P{Lower}]", "vi", s), "{s}");
    }
    assert!(!whole("[^\\P{Lower}]", "vi", "#"));
    assert!(!whole("[[^\\W]--s]", "vi", "S"));
    assert!(!whole("[[^a]]", "vi", "A"));
    assert!(whole("[[^a]]", "vi", "b"));
    // The four QuickJS cases, as `replace(…, "X")` ranges.
    assert_eq!(ranges("[^\\P{Lower}]", "vi", "aAbBcC#4"), [0..1, 1..2, 2..3, 3..4, 4..5, 5..6]);
    assert_eq!(ranges("[[a-c]&&B]", "vi", "aAbBcC"), [2..3, 3..4]);
    assert_eq!(ranges("[[a-c]--B]", "vi", "aAbBcC"), [0..1, 1..2, 4..5, 5..6]);
    assert_eq!(ranges("[\\q{BC|A}--a]", "vi", "abcAbC"), [1..3, 4..6]);
    // Without `i` nothing folds.
    assert!(whole("[\\w--K]", "v", "k"));
    assert!(!whole("[\\w--K]", "v", "K"));
}

#[test]
fn negated_classes_and_strings() {
    assert!(!whole("[^\\q{a}]", "v", "a"));
    assert!(whole("[^\\q{a}]", "v", "b"));
    assert!(!whole("[[^\\q{a}]]", "v", "a"));
    assert!(!whole("[^\\q{a|b}]", "vi", "B"));
    assert!(whole("[^\\q{a|b}]", "vi", "c"));
    assert!(!whole("[[^\\q{A}]&&a]", "vi", "a"));
    // MayContainStrings is syntactic: a subtraction asks its first operand,
    // an intersection all of them.
    for pattern in ["[^\\q{ab}]", "[^\\q{}]", "[^[\\q{ab}--\\q{ab}]]", "[^\\p{RGI_Emoji}]", "[[^\\q{a|bc}]]"] {
        assert!(Regex::with_flags(pattern, "v").is_err(), "/{pattern}/v must be a SyntaxError");
    }
    for pattern in ["[^[\\q{ab}&&a]]", "[^\\q{ab}&&a]", "[^a&&\\q{ab}]", "[^[a]--\\q{ab}]", "[^[\\p{RGI_Emoji}&&a]]"] {
        assert!(whole(pattern, "v", "b"), "/{pattern}/v matches b");
    }
}

#[test]
fn string_disjunctions_match_longest_first_and_keep_the_empty_string() {
    assert!(whole("[\\q{}]", "v", ""));
    assert!(whole("[\\q{a|}]", "v", ""));
    assert!(whole("[\\q{a|}]", "v", "a"));
    assert_eq!(ranges("[\\q{a|}]", "v", "b"), [0..0, 1..1]);
    assert_eq!(ranges("[\\q{a|ab}]", "v", "ab"), [0..2]);
    assert_eq!(ranges("[a\\q{abc|ab}]", "v", "abc"), [0..3]);
    assert_eq!(ranges("[\\q{ABC}]", "vi", "xabcx"), [1..4]);
    assert!(whole("[\\q{BC|A}]", "vi", "A"));
}

#[test]
fn a_lone_ampersand_keeps_the_first_operand() {
    for s in ["a", "&", "b"] {
        assert!(whole("[a&b]", "v", s), "{s}");
    }
    assert!(whole("[a&]", "v", "a"));
    assert!(whole("[\\q{a}&b]", "v", "a"));
    assert!(whole("[[a]&b]", "v", "a"));
    assert!(whole("[\\d&x]", "v", "5"));
    assert!(Regex::with_flags("[a&&&b]", "v").is_err());
}
