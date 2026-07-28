use regress::{Match, Regex, RegexFallbackReason, RegexPlan};

#[derive(Debug, PartialEq, Eq)]
struct MatchSnapshot {
    range: regress::Range,
    captures: Vec<Option<regress::Range>>,
    named: Vec<(String, Option<regress::Range>)>,
}

impl From<Match> for MatchSnapshot {
    fn from(found: Match) -> Self {
        let named = found
            .named_groups()
            .map(|(name, range)| (name.to_owned(), range))
            .collect();
        Self {
            range: found.range,
            captures: found.captures,
            named,
        }
    }
}

fn compare_ascii_byteopt(pattern: &str, flags: &str, input: &str, start: usize) {
    assert!(pattern.is_ascii() && input.is_ascii());
    let regex = Regex::from_unicode_byteopt(pattern.chars().map(u32::from), flags).unwrap();
    compare_ascii_compiled(&regex, pattern, flags, input, start);
}

fn compare_ascii_compiled(regex: &Regex, pattern: &str, flags: &str, input: &str, start: usize) {
    assert_eq!(
        regex.ascii_plan(),
        RegexPlan::LinearAscii,
        "/{pattern}/{flags} declined: {:?}",
        regex.ascii_fallback_reason()
    );
    let classical: Vec<_> = regex
        .find_from_ascii_with_plan(input, start, RegexPlan::Classical)
        .map(MatchSnapshot::from)
        .collect();
    let linear: Vec<_> = regex
        .find_from_ascii_with_plan(input, start, RegexPlan::LinearAscii)
        .map(MatchSnapshot::from)
        .collect();
    assert_eq!(
        linear, classical,
        "ASCII backend mismatch for /{pattern}/{flags} on {input:?} from {start}"
    );
}

fn expected_match(
    range: (usize, usize),
    captures: &[Option<(usize, usize)>],
    named: &[(&str, Option<(usize, usize)>)],
) -> MatchSnapshot {
    MatchSnapshot {
        range: range.0..range.1,
        captures: captures
            .iter()
            .map(|range| range.map(|(start, end)| start..end))
            .collect(),
        named: named
            .iter()
            .map(|(name, range)| ((*name).to_owned(), range.map(|(start, end)| start..end)))
            .collect(),
    }
}

#[test]
fn linear_matches_classical_on_regular_ascii_matrix() {
    let cases = [
        ("abc", "", "xxabcabczz"),
        ("a|ab", "", "zababa"),
        ("ab|a", "", "zababa"),
        ("a*", "", "baa"),
        ("a*?", "", "baa"),
        ("a+", "", "baaa"),
        ("a+?", "", "baaa"),
        ("a{1,3}", "", "baaaa"),
        ("(ab)", "", "zabx"),
        ("(?<word>[a-z]+)-(\\d+)", "", "xx-name-123-yy"),
        ("^foo$", "", "foo"),
        ("\\bcat\\b", "", "cat scatter cat"),
        (".+", "s", "a\nb"),
        ("[k]", "iu", "xKz"),
        ("(?:POST|PUT|DELETE)", "", "xDELETEz"),
        ("(?:ab)?", "", "zab"),
        ("[0-9:.]+", "", "12:34.56"),
    ];
    for (pattern, flags, input) in cases {
        compare_ascii_byteopt(pattern, flags, input, 0);
        compare_ascii_byteopt(pattern, flags, input, input.len().min(1));
    }
}

#[test]
fn linear_matches_fixed_v8_fixture_corpus() {
    // These expected values were generated once with Node.js v24.12.0,
    // V8 13.6.233.17-node.37, using RegExp's `d` indices flag. This corpus is
    // deliberately independent of the classical regress executor.
    let cases = vec![
        (
            r"\x41\+",
            "",
            "zA+xA+",
            0,
            vec![
                expected_match((1, 3), &[], &[]),
                expected_match((4, 6), &[], &[]),
            ],
        ),
        (
            r"(?<key>[a-z]+)=(\d{2})",
            "",
            "x aa=12 bb=345 cc=09",
            0,
            vec![
                expected_match(
                    (2, 7),
                    &[Some((2, 4)), Some((5, 7))],
                    &[("key", Some((2, 4)))],
                ),
                expected_match(
                    (8, 13),
                    &[Some((8, 10)), Some((11, 13))],
                    &[("key", Some((8, 10)))],
                ),
                expected_match(
                    (15, 20),
                    &[Some((15, 17)), Some((18, 20))],
                    &[("key", Some((15, 17)))],
                ),
            ],
        ),
        (
            "ab",
            "",
            "ab--ab--ab",
            3,
            vec![
                expected_match((4, 6), &[], &[]),
                expected_match((8, 10), &[], &[]),
            ],
        ),
        (
            "[k]+",
            "iu",
            "xKKkz",
            0,
            vec![expected_match((1, 4), &[], &[])],
        ),
        (
            "a.b",
            "s",
            "xa\nbya\rb",
            0,
            vec![
                expected_match((1, 4), &[], &[]),
                expected_match((5, 8), &[], &[]),
            ],
        ),
        (
            r"[^\r\n]+",
            "m",
            "a\rbb\nc",
            0,
            vec![
                expected_match((0, 1), &[], &[]),
                expected_match((2, 4), &[], &[]),
                expected_match((5, 6), &[], &[]),
            ],
        ),
        (
            r"\bcat\b",
            "",
            "cat scatter cat",
            0,
            vec![
                expected_match((0, 3), &[], &[]),
                expected_match((12, 15), &[], &[]),
            ],
        ),
        (
            "(ab|a)(b?)",
            "",
            "ab a abb",
            0,
            vec![
                expected_match((0, 2), &[Some((0, 2)), Some((2, 2))], &[]),
                expected_match((3, 4), &[Some((3, 4)), Some((4, 4))], &[]),
                expected_match((5, 8), &[Some((5, 7)), Some((7, 8))], &[]),
            ],
        ),
        (
            r"\s+",
            "",
            "a \t\r\nb",
            0,
            vec![expected_match((1, 5), &[], &[])],
        ),
    ];

    for (pattern, flags, input, start, expected) in cases {
        assert!(pattern.is_ascii() && input.is_ascii());
        let regex = Regex::from_unicode_byteopt(pattern.chars().map(u32::from), flags).unwrap();
        assert_eq!(
            regex.ascii_plan(),
            RegexPlan::LinearAscii,
            "/{pattern}/{flags} declined: {:?}",
            regex.ascii_fallback_reason()
        );
        let actual: Vec<_> = regex
            .find_from_ascii_with_plan(input, start, RegexPlan::LinearAscii)
            .map(MatchSnapshot::from)
            .collect();
        assert_eq!(
            actual, expected,
            "V8 fixture mismatch for /{pattern}/{flags} on {input:?} from {start}"
        );
    }
}

#[test]
fn backend_specific_syntax_stays_classical() {
    let escape_cases = [
        r"\A",
        r"\z",
        r"\a",
        r"\U00000041",
        r"\<",
        r"\>",
        r"\b{start}",
        r"\x{41}",
    ];
    for pattern in escape_cases {
        let regex = Regex::new(pattern).unwrap();
        assert_eq!(regex.ascii_plan(), RegexPlan::Classical, "/{pattern}/");
        assert_eq!(
            regex.ascii_fallback_reason(),
            Some(RegexFallbackReason::UnsupportedEscape),
            "/{pattern}/"
        );
    }

    let class_cases = [r"[a&&b]", r"[a~~b]", r"[a[b]]", r"[[:alpha:]]"];
    for pattern in class_cases {
        let regex = Regex::new(pattern).unwrap();
        assert_eq!(regex.ascii_plan(), RegexPlan::Classical, "/{pattern}/");
        assert_eq!(
            regex.ascii_fallback_reason(),
            Some(RegexFallbackReason::UnsupportedClassSyntax),
            "/{pattern}/"
        );
    }
}

#[test]
fn ascii_line_terminators_match_ecmascript() {
    let eligible_cases = [
        (r"a.b", "", "a\rb"),
        (r"a.b", "", "a\nb"),
        (r"a.b", "s", "a\rb"),
        (r"a.b", "m", "a\rb"),
        (r"a$", "", "a\r"),
        (r"a$", "", "a\n"),
        (r"^a$", "", "a"),
    ];
    for (pattern, flags, input) in eligible_cases {
        compare_ascii_byteopt(pattern, flags, input, 0);
    }

    let multiline_anchor_cases = [r"^b", r"a$", r"^\n", r"\r$"];
    for pattern in multiline_anchor_cases {
        let regex = Regex::with_flags(pattern, "m").unwrap();
        assert_eq!(regex.ascii_plan(), RegexPlan::Classical, "/{pattern}/m");
        assert_eq!(
            regex.ascii_fallback_reason(),
            Some(RegexFallbackReason::MultilineAnchor),
            "/{pattern}/m"
        );
    }
}

#[test]
fn linear_matches_classical_on_benchmark_ascii_patterns() {
    let line = concat!(
        "2026-03-14T12:34:56.789Z [ERROR] 192.168.12.3 POST ",
        "/api//users status=204 bytes=1234 ms=17 ua=\"curl/8.5.0\""
    );
    let cases = [
        (r"\[ERROR\]", ""),
        (r"(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})", ""),
        (r"//+(\w+)", ""),
        (r"([a-z]+)=(\d+)", ""),
        (
            r#"^2026-\d\d-\d\dT[0-9:.]+Z \[\w+\] \S+ (?:POST|PUT|DELETE) .*"$"#,
            "",
        ),
    ];
    for (pattern, flags) in cases {
        compare_ascii_byteopt(pattern, flags, line, 0);
        compare_ascii_byteopt(pattern, flags, line, 1);
    }
}

#[test]
fn unsupported_features_have_stable_fallback_reasons() {
    let cases = [
        (r"(a)\1", RegexFallbackReason::Backreference),
        (r"(?<=x)y", RegexFallbackReason::Lookaround),
        (r"(?=ab)ab", RegexFallbackReason::Lookaround),
        (r"\p{Letter}", RegexFallbackReason::UnsupportedEscape),
        (r"((a)?b)*", RegexFallbackReason::CaptureRepetitionSemantics),
        (r"(?:(a)b)*", RegexFallbackReason::CaptureResetSemantics),
        (r"(a?)*", RegexFallbackReason::CaptureRepetitionSemantics),
        (r"(ab)+", RegexFallbackReason::CaptureRepetitionSemantics),
    ];
    for (pattern, reason) in cases {
        let regex = Regex::new(pattern).unwrap();
        assert_eq!(regex.ascii_plan(), RegexPlan::Classical);
        assert_eq!(regex.ascii_fallback_reason(), Some(reason), "/{pattern}/");
    }
}

#[test]
fn non_ascii_and_unicode_sets_stay_classical() {
    let non_ascii = Regex::new("é+").unwrap();
    assert_eq!(
        non_ascii.ascii_fallback_reason(),
        Some(RegexFallbackReason::NonAsciiPattern)
    );
    let unicode_sets = Regex::with_flags("[a-z]", "v").unwrap();
    assert_eq!(
        unicode_sets.ascii_fallback_reason(),
        Some(RegexFallbackReason::UnicodeSets)
    );
}

#[test]
fn auto_profitability_filter_keeps_existing_fast_paths() {
    let cases = [
        ("", false),
        ("a", false),
        ("POST", false),
        ("[0-9:.]+", false),
        ("^short", false),
        ("this-is-a-long-unanchored-literal-pattern", false),
        (r"\[ERROR\]", false),
        (r"(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})", true),
        (r"//+(\w+)", false),
        (r"([a-z]+)=(\d+)", true),
        (
            r#"^2026-\d\d-\d\dT[0-9:.]+Z \[\w+\] \S+ (?:POST|PUT|DELETE) .*"$"#,
            true,
        ),
    ];
    for (pattern, expected) in cases {
        let regex = Regex::from_unicode_byteopt(pattern.chars().map(u32::from), "").unwrap();
        assert_eq!(
            regex.ascii_auto_eligible(),
            expected,
            "unexpected auto decision for /{pattern}/"
        );
    }
}

#[test]
fn linear_matches_classical_exhaustively_on_small_regular_programs() {
    let patterns = [
        "",
        "a",
        "ab",
        "a|b",
        "a|ab",
        "ab|a",
        "(a|ab)",
        "(ab|a)",
        "a?",
        "a??",
        "a*",
        "a*?",
        "a+",
        "a+?",
        "a{0,2}",
        "a{0,2}?",
        "a{1,3}",
        "[ab]+",
        "[^ab]*",
        ".+",
        "^a",
        "a$",
        "^a$",
        "\\ba\\b",
        "(?:ab|b)?",
        "(?<x>a)b",
        "([a-z]+)=(\\d+)",
    ];

    fn inputs(prefix: &mut String, depth: usize, out: &mut Vec<String>) {
        out.push(prefix.clone());
        if depth == 4 {
            return;
        }
        for ch in ['a', 'b', '1', '=', '\n'] {
            prefix.push(ch);
            inputs(prefix, depth + 1, out);
            prefix.pop();
        }
    }

    let mut corpus = Vec::new();
    inputs(&mut String::new(), 0, &mut corpus);
    for pattern in patterns {
        let regex = Regex::from_unicode_byteopt(pattern.chars().map(u32::from), "").unwrap();
        for input in &corpus {
            for start in 0..=input.len() {
                compare_ascii_compiled(&regex, pattern, "", input, start);
            }
        }
    }
}

#[test]
#[ignore = "manual performance experiment; run with --release --ignored --nocapture"]
fn benchmark_linear_against_classical_by_pattern_shape() {
    use std::hint::black_box;
    use std::time::Instant;

    let line = concat!(
        "2026-03-14T12:34:56.789Z [ERROR] 192.168.12.3 POST ",
        "/api//users status=204 bytes=1234 ms=17 ua=\"curl/8.5.0\""
    );
    let cases = [
        ("", false),
        ("a", false),
        ("POST", false),
        ("[0-9:.]+", false),
        ("a|b", false),
        ("^2026", false),
        (r"\[ERROR\]", false),
        (r"(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})", false),
        (r"//+(\w+)", true),
        (r"([a-z]+)=(\d+)", true),
        (
            r#"^2026-\d\d-\d\dT[0-9:.]+Z \[\w+\] \S+ (?:POST|PUT|DELETE) .*"$"#,
            false,
        ),
    ];

    for (pattern, global) in cases {
        let regex = Regex::from_unicode_byteopt(pattern.chars().map(u32::from), "").unwrap();
        let repetitions = 100_000;
        let run_classical = || {
            let start = Instant::now();
            let mut checksum = 0usize;
            for _ in 0..repetitions {
                let matches = regex.find_from_ascii_with_plan(line, 0, RegexPlan::Classical);
                for found in matches.take(if global { usize::MAX } else { 1 }) {
                    checksum = checksum.wrapping_add(found.range.end);
                }
            }
            black_box(checksum);
            start.elapsed()
        };
        let run_linear = || {
            let start = Instant::now();
            let mut checksum = 0usize;
            for _ in 0..repetitions {
                for found in regex
                    .find_from_ascii_with_plan(line, 0, RegexPlan::LinearAscii)
                    .take(if global { usize::MAX } else { 1 })
                {
                    checksum = checksum.wrapping_add(found.range.end);
                }
            }
            black_box(checksum);
            start.elapsed()
        };
        let mut classical = Vec::new();
        let mut linear = Vec::new();
        for round in 0..7 {
            if round % 2 == 0 {
                classical.push(run_classical().as_secs_f64());
                linear.push(run_linear().as_secs_f64());
            } else {
                linear.push(run_linear().as_secs_f64());
                classical.push(run_classical().as_secs_f64());
            }
        }
        classical.sort_by(f64::total_cmp);
        linear.sort_by(f64::total_cmp);
        let classical_median = classical[classical.len() / 2];
        let linear_median = linear[linear.len() / 2];
        eprintln!(
            "/{pattern}/ classical={:.1}ns linear={:.1}ns ratio={:.3}",
            classical_median * 1e9 / repetitions as f64,
            linear_median * 1e9 / repetitions as f64,
            linear_median / classical_median,
        );
    }
}
