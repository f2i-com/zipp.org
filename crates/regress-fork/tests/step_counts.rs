//! The matcher's step counts for a fixed set of patterns and subjects, as the
//! checked profile meters them (`bounded-backtracking`). A change to the
//! matcher that changes what a match charges must update this table, so the
//! difference is visible in its diff and stated in its commit. Run with
//! `STEP_COUNTS_PRINT=1` to print the table as the code now counts it.

#![cfg(all(feature = "bounded-backtracking", feature = "utf16"))]

use regress::{MatchLimits, Regex};

/// How the engine runs a subject: the ASCII twin over one-byte text, or the
/// unicode program over UTF-16 code points (`u`/`v`) or code units.
#[derive(Clone, Copy, Debug)]
enum Mode {
    Ascii,
    Utf16,
    Ucs2,
}

/// (pattern, flags, subject, mode, steps over every match in the checked
/// profile (`prohibit-unsafe`, the WebAssembly modules), the same without
/// it, number of matches). The two differ where an alternation of three or
/// more is built as a balanced tree in the checked profile (`parse::make_alt`).
const CASES: &[(&str, &str, &str, Mode, u64, u64, usize)] = &[
    (r"needle", "", "hay hay needle hay needle", Mode::Ascii, 4, 4, 2),
    (r"[a-z]+\d", "", "abc1 de2 fgh", Mode::Ascii, 20, 20, 2),
    (r"(\d{1,3})\.(\d{1,3})", "", "ip 10.0.0.1 and 192.168.1.1", Mode::Ascii, 61, 61, 4),
    (r"\b(\w+)\s+\1\b", "", "the cat cat sat on on the mat", Mode::Ascii, 171, 171, 2),
    (r"<b>(.*?)</b>", "", "x <b>bold</b> y <b>more</b> z", Mode::Ascii, 49, 49, 2),
    (r"<(\w+)>.*?</\1>", "", "<i>a</i> <em>b</em>", Mode::Ascii, 39, 39, 2),
    (r"(?:foo|bar|baz)+qux", "", "foobarbazqux barqux fooqu", Mode::Ascii, 73, 70, 2),
    (r"(?<=\$)\d+(?:\.\d\d)?", "", "cost $12.50 or $7", Mode::Ascii, 25, 25, 2),
    (r"(?<!no )milk", "", "no milk, milk, oat milk", Mode::Ascii, 14, 14, 2),
    (r"(?<=(\d+)(\d+))$", "", "1053", Mode::Ascii, 99, 99, 1),
    (r"(?<=\b[a-z]{2,4}?)X", "", "ab X abcdX abcdeX", Mode::Ascii, 34, 34, 1),
    (r"(?<=(?:ab|c)+)d", "", "ababcd cd xd", Mode::Ascii, 46, 46, 2),
    (r"(?<=\1(a))b", "", "aab ab", Mode::Ascii, 22, 22, 1),
    (r"\d+(?=kg)", "", "5kg 10 lb 20kg", Mode::Ascii, 35, 35, 2),
    (r"hello\s+world", "i", "HELLO   World hello world", Mode::Ascii, 30, 30, 2),
    (r"^(?:\w+\s)+end$", "", "one two three end", Mode::Ascii, 38, 38, 1),
    (r"(a|aa)+$", "", "aaaaaaaaaa!", Mode::Ascii, 8091, 8091, 0),
    (r"\p{L}+", "u", "héllo wörld κόσμε", Mode::Utf16, 30, 30, 3),
    (r"(?<=\p{Lu})\p{Ll}+", "u", "Ωmega Δelta", Mode::Utf16, 36, 36, 2),
    (r"(?<!\p{L})\d+", "u", "a1 22 б3 444", Mode::Utf16, 55, 55, 2),
    (r"[\p{L}--[a-z]]+", "v", "abcΣΤΥdef", Mode::Utf16, 20, 20, 1),
    (r"(?<=é{1,3})x", "", "éx ééx eeex", Mode::Ucs2, 58, 58, 2),
    (r"(?<=(ab)+?)c", "i", "ABABc abc", Mode::Ucs2, 125, 125, 2),
    (r"σ+", "i", "ΣσςΣ", Mode::Ucs2, 9, 9, 1),
];

fn steps(pattern: &str, flags: &str, subject: &str, mode: Mode) -> (u64, usize) {
    let cps: Vec<u32> = pattern.chars().map(u32::from).collect();
    let limits = MatchLimits::SANDBOX;
    match mode {
        Mode::Ascii => {
            let re = Regex::from_unicode_byteopt(cps.iter().copied(), flags).unwrap();
            let mut it = re.find_from_ascii_with_limits(subject, 0, limits);
            let n = it.by_ref().count();
            (it.match_usage().steps, n)
        }
        Mode::Utf16 | Mode::Ucs2 => {
            let re = Regex::from_unicode(cps.iter().copied(), flags).unwrap();
            let units: Vec<u16> = subject.encode_utf16().collect();
            if let Mode::Utf16 = mode {
                let mut it = re.find_from_utf16_with_limits(&units, 0, limits);
                let n = it.by_ref().count();
                (it.match_usage().steps, n)
            } else {
                let mut it = re.find_from_ucs2_with_limits(&units, 0, limits);
                let n = it.by_ref().count();
                (it.match_usage().steps, n)
            }
        }
    }
}

#[test]
fn step_counts_are_as_recorded() {
    let print = std::env::var_os("STEP_COUNTS_PRINT").is_some();
    let mut wrong = Vec::new();
    for &(pattern, flags, subject, mode, checked, unchecked, matches) in CASES {
        let expected = if cfg!(feature = "prohibit-unsafe") { checked } else { unchecked };
        let (got, n) = steps(pattern, flags, subject, mode);
        if print {
            println!("    (r\"{pattern}\", \"{flags}\", \"{subject}\", Mode::{mode:?}, {got}, {n}),");
        }
        if (got, n) != (expected, matches) {
            wrong.push(format!(
                "{pattern:?} /{flags} {subject:?} {mode:?}: {got} steps, {n} matches (recorded {expected}, {matches})"
            ));
        }
    }
    assert!(wrong.is_empty(), "step counts changed:\n{}", wrong.join("\n"));
}
