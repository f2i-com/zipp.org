//! How large a pattern may be, and what compiling it may cost.
//!
//! - Alternatives (issue #57). The checked profile (`prohibit-unsafe`) refused
//!   a disjunction of more than 64 alternatives, to keep a right spine away
//!   from the recursive IR walks; its alternation was already a balanced tree,
//!   so the limit protected nothing and keyword lists failed to load. The
//!   ordinary profile had no limit and built the right spine, which a 5,000
//!   word list made deep enough to overflow a 1 MiB stack, and whose
//!   early-fail pass was quadratic in its depth (682 ms for 20,000 words).
//! - Unrolled loops (issue #58). The optimizer unrolled every loop with a
//!   minimum of one to five, copying its body that many times, and copied an
//!   unrolled loop again with every copy of an enclosing body. The program grew
//!   by `min + 1` per level of nesting: `(?:…|)+` 20 deep took 2 s to compile,
//!   31 deep never finished, and past 65,535 copied loops the 16-bit loop ids
//!   overflowed (a panic with overflow checks, shared loop state without).

#![allow(clippy::uninlined_format_args)]

use regress::{CompileError, CompileLimits, Flags, Regex};
use std::time::{Duration, Instant};

fn words(n: usize) -> String {
    (0..n).map(|i| format!("w{i}")).collect::<Vec<_>>().join("|")
}

/// Run `f` on a thread with a 1 MiB stack, the smallest an embedder is likely
/// to give one, so an IR walk as deep as the alternation is long overflows.
fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(f)
        .expect("spawn")
        .join()
        .expect("compiling on a 1 MiB stack must not overflow it")
}

#[test]
fn long_alternations_compile_and_keep_their_order() {
    for n in [64, 65, 200, 5000] {
        let pattern = words(n);
        let re = Regex::new(&pattern).unwrap_or_else(|e| panic!("{n} alternatives: {e}"));
        assert_eq!(re.find("w5").map(|m| m.range()), Some(0..2), "{n}");
        // The leftmost alternative that matches wins, not the longest: `w4`
        // comes before `w49`, whatever shape the tree has.
        let last = format!("w{}", n - 1);
        assert_eq!(re.find(&last).map(|m| m.range()), Some(0..2), "{n}");
        assert_eq!(re.find(&format!("x{last}")).map(|m| m.range()), Some(1..3), "{n}");
        assert!(re.find("x5").is_none(), "{n}");
        // Inside a group, and quantified.
        let grouped = Regex::new(&format!("^(?:{pattern})+$")).unwrap();
        assert!(grouped.find(&format!("w0w1{last}")).is_some(), "{n}");
        assert!(grouped.find("w0x").is_none(), "{n}");
    }
}

#[test]
fn capture_groups_across_many_alternatives_keep_their_numbers() {
    let n = 300;
    let pattern = (0..n).map(|i| format!("(x{i}y)")).collect::<Vec<_>>().join("|");
    let re = Regex::new(&pattern).unwrap();
    let m = re.find("--x250y").unwrap();
    assert_eq!(m.range(), 2..7);
    assert_eq!(m.captures.len(), n);
    for (index, capture) in m.captures.iter().enumerate() {
        let want = (index == 250).then_some(2..7);
        assert_eq!(*capture, want, "group {}", index + 1);
    }
}

#[test]
fn alternation_depth_is_bounded_on_a_small_stack() {
    on_small_stack(|| {
        let re = Regex::new(&words(50_000)).unwrap();
        assert_eq!(re.find("w49999").map(|m| m.range()), Some(0..2));
        assert_eq!(re.find("-w7").map(|m| m.range()), Some(1..3));
        // A property of strings is one atom and 3,953 alternatives.
        let emoji = Regex::with_flags(r"^\p{RGI_Emoji}$", "v").unwrap();
        assert!(emoji.find("\u{1F600}").is_some());
        assert!(emoji.find("x").is_none());
    });
}

#[test]
fn long_alternations_compile_in_linear_time() {
    // 20,000 alternatives took 682 ms to optimize in a release build when
    // the early-fail pass walked the subtree of every node; now ~10 ms.
    let pattern = words(20_000);
    let started = Instant::now();
    let re = Regex::new(&pattern).unwrap();
    let elapsed = started.elapsed();
    assert!(re.find("w19999").is_some());
    assert!(
        elapsed < Duration::from_secs(5),
        "20,000 alternatives took {elapsed:?}"
    );
}

/// `inner` wrapped `depth` times in `open`…`close`.
fn nested(inner: &str, depth: usize, open: &str, close: &str) -> String {
    let mut s = inner.to_string();
    for _ in 0..depth {
        s = format!("{open}{s}{close}");
    }
    s
}

/// Loops nested in loops, each with a minimum the optimizer unrolls.
const NESTED_FAMILIES: &[(&str, &str)] = &[
    ("(?:", "|)+"),
    ("(?:", ")+"),
    ("(?:", "){5}"),
    ("(?:", "|){2,}"),
    ("(?:", "|){1,3}"),
    ("(?:", "|)+?"),
    ("(?:", "|b){2,4}?"),
];

/// Compile on another thread, failing the test rather than hanging it when
/// the compile does not finish in `limit`.
fn compile_within(pattern: String, limit: Duration) -> (Regex, Duration) {
    let (send, receive) = std::sync::mpsc::channel();
    let shown = pattern.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        let re = Regex::new(&pattern).map(|re| (re, started.elapsed()));
        let _ = send.send(re);
    });
    match receive.recv_timeout(limit) {
        Ok(compiled) => compiled.unwrap_or_else(|e| panic!("/{shown}/: {e}")),
        Err(_) => panic!("/{shown}/ did not compile within {limit:?}"),
    }
}

#[test]
fn nested_unrolled_loops_compile_in_linear_size() {
    for &(open, close) in NESTED_FAMILIES {
        let size = |depth| {
            compile_within(nested("a", depth, open, close), Duration::from_secs(10))
                .0
                .resident_bytes()
        };
        let (shallow, deep) = (size(16), size(31));
        // Grown by (min + 1) per level, 15 more levels multiplied the size by
        // 2^15 or more; it now grows by the size of the pattern.
        assert!(
            deep <= 2 * shallow && deep < 256 * 1024,
            "{open}…{close}: {shallow} bytes 16 deep, {deep} bytes 31 deep"
        );
    }
}

#[test]
fn the_31_deep_pattern_from_quickjs_ng_1221_compiles_promptly() {
    // Release build: 2,033 ms 20 deep before, never finishing 31 deep; now
    // about 0.1 ms. The bound is for unoptimized test builds and a busy
    // machine, not a measurement.
    let (re, elapsed) =
        compile_within(nested("a", 31, "(?:", "|)+"), Duration::from_secs(20));
    assert!(
        elapsed < Duration::from_secs(1),
        "31 levels of (?:…|)+ took {elapsed:?}"
    );
    // Matching it is another matter: every engine, Node's included, takes
    // time exponential in the depth to backtrack through the levels.
    assert!(re.resident_bytes() < 64 * 1024, "{}", re.resident_bytes());
}

type Expected = Option<(usize, &'static [Option<&'static str>])>;

/// Loops whose body can match empty, which the unroll budget now leaves
/// rolled at depth: the minimum's iterations may be empty and later ones may
/// not (ES2025 22.2.2.3.1 RepeatMatcher, step 2.b). Every expectation is
/// Node 24's `exec`.
const NODE_CASES: &[(&str, &str, Expected)] = &[
    (r"(?:a|)+", "aaab", Some((0, &[Some("aaa")]))),
    (r"(?:a|)+?", "aaab", Some((0, &[Some("a")]))),
    (r"(?:a|)+b", "aaab", Some((0, &[Some("aaab")]))),
    (r"(?:a|)+?b", "aaab", Some((0, &[Some("aaab")]))),
    (r"(?:(?:a|)+|)+b", "aaab", Some((0, &[Some("aaab")]))),
    (r"(?:(?:a|)+?|)+?b", "aaab", Some((0, &[Some("aaab")]))),
    (r"(?:(?:a|)+|)+b", "xb", Some((1, &[Some("b")]))),
    (r"(?:(a)|b|)+", "abab", Some((0, &[Some("abab"), None]))),
    (r"(?:(a)|b|)+c", "abac", Some((0, &[Some("abac"), Some("a")]))),
    (r"(?:(a)|b|)+?c", "abac", Some((0, &[Some("abac"), Some("a")]))),
    (r"((?:a|)+)+c", "aac", Some((0, &[Some("aac"), Some("aa")]))),
    (r"(?:(a|)+)+b", "aab", Some((0, &[Some("aab"), Some("a")]))),
    (r"(?:(a)|\1b|)+c", "aabc", Some((0, &[Some("aabc"), None]))),
    (r"(?:x(a)|\1|)+y", "xaaxay", Some((3, &[Some("xay"), Some("a")]))),
    (r"(?<n>a|)+\k<n>b", "aab", Some((0, &[Some("aab"), Some("a")]))),
    (r"(?:a|){2,}b", "ab", Some((0, &[Some("ab")]))),
    (r"(?:a|){2,}?b", "aab", Some((0, &[Some("aab")]))),
    (r"(?:(?:a|){2}){3}b", "aaaab", Some((0, &[Some("aaaab")]))),
    (r"(?:(?:a|b?){1,3}){2}c", "abbc", Some((0, &[Some("abbc")]))),
    (r"(?:(?:(?:(?:a|)+|)+|)+|)+c", "aac", Some((0, &[Some("aac")]))),
    (r"(?:(?:ab|a|)+){2}b", "abab", Some((0, &[Some("abab")]))),
    (r"(?:a{2}|){3}", "aaaaa", Some((0, &[Some("aaaa")]))),
    (r"(?:(?=a)|a)+", "aa", Some((0, &[Some("aa")]))),
    (r"(?:a|(?=b))+b", "aab", Some((0, &[Some("aab")]))),
    (r"(?:(?:a?)?)+n", "aan", Some((0, &[Some("aan")]))),
];

fn captures<'t>(m: &regress::Match, text: &'t str) -> Vec<Option<&'t str>> {
    std::iter::once(Some(&text[m.range()]))
        .chain(m.captures.iter().map(|c| c.clone().map(|r| &text[r])))
        .collect()
}

#[test]
fn loops_that_match_empty_agree_with_node() {
    // Optimized (unrolled where the budget allows) and unoptimized (every
    // loop rolled), which is what the budget leaves of a deep nesting.
    let rolled = Flags {
        no_opt: true,
        ..Flags::default()
    };
    for &(pattern, subject, ref expected) in NODE_CASES {
        for flags in [Flags::default(), rolled] {
            let re = Regex::with_flags(pattern, flags).unwrap();
            let got = re.find(subject).map(|m| (m.start(), captures(&m, subject)));
            let got = got.as_ref().map(|(start, groups)| (*start, groups.as_slice()));
            assert_eq!(got, *expected, "/{pattern}/ {flags:?} on {subject:?}");
        }
    }
}

/// Every match and capture of `re` in `text`, as text.
fn all_matches(re: &Regex, text: &str) -> Vec<(usize, Vec<Option<String>>)> {
    re.find_iter(text)
        .map(|m| {
            let groups = captures(&m, text).into_iter().map(|g| g.map(String::from)).collect();
            (m.start(), groups)
        })
        .collect()
}

#[test]
fn unrolled_loops_match_like_rolled_ones() {
    // The optimizer unrolls what the budget allows, the unoptimized program
    // nothing. Both must find the same matches: fully unrolled shallow
    // nestings, and 8 levels of `(?:…|)+`, where the budget runs out
    // part-way, so the inner levels are unrolled and the outer ones not.
    let unoptimized = Flags {
        no_opt: true,
        ..Flags::default()
    };
    // Kept short and shallow: a failing attempt backtracks through every way
    // of spreading the subject over the levels.
    let subjects = ["", "c", "ac", "aac", "bac", "abc", "xc", "a"];
    let mut cases = Vec::new();
    for &(open, close) in NESTED_FAMILIES {
        for depth in 1..=3 {
            for (inner, tail) in [("a", "c"), ("(a)", "c"), ("a", "$"), ("(?:ab|a)", "c")] {
                cases.push((format!("{}{tail}", nested(inner, depth, open, close)), &subjects[..]));
            }
        }
    }
    // 7 levels unroll in full, doubling at each; the eighth does not.
    let size = |depth| Regex::new(&nested("a", depth, "(?:", "|)+")).unwrap().resident_bytes();
    assert!(2 * size(8) < 3 * size(7), "8 levels were unrolled in full");
    cases.push((format!("{}c", nested("a", 8, "(?:", "|)+")), &["c", "ac"][..]));
    for (pattern, subjects) in cases {
        let optimized = Regex::new(&pattern).unwrap();
        let rolled = Regex::with_flags(&pattern, unoptimized).unwrap();
        for &subject in subjects {
            assert_eq!(
                all_matches(&optimized, subject),
                all_matches(&rolled, subject),
                "/{pattern}/ on {subject:?}"
            );
        }
    }
}

#[test]
fn copied_loops_never_run_out_of_loop_ids() {
    // 65,000 loops in the source, then a nesting whose unrolling would copy
    // more than the 535 loop ids left: they overflowed the emitter's 16-bit
    // counter, a panic with overflow checks and two loops sharing one id's
    // state without. The source loops sit behind an `x` the subjects lack.
    let pattern = format!("(?:x{})?{}$", "(?:ab)?".repeat(65_000), nested("cd", 12, "(?:", ")+"));
    let re = Regex::new(&pattern).unwrap();
    // The metered profile's default ceilings do not hold 65,000 loops' state.
    #[cfg(feature = "bounded-backtracking")]
    let find = |text| {
        re.find_from_with_limits(text, 0, regress::MatchLimits::UNLIMITED)
            .next()
            .map(|m| m.range())
    };
    #[cfg(not(feature = "bounded-backtracking"))]
    let find = |text| re.find(text).map(|m| m.range());
    assert_eq!(find("abcdcd"), Some(2..6));
    assert_eq!(find("cdcdcdx"), None);
}

#[test]
fn the_program_limit_is_checked_while_emitting() {
    let patterns = [
        r"(?:a|b|c)+".repeat(500),
        r"\p{L}x".repeat(40),
        r"(?<=(?:ab)+)(c)".repeat(100) + r"(?:(?<n>a)|(?<n>b))\k<n>",
        nested("a", 31, "(?:", "|)+"),
    ];
    for pattern in &patterns {
        let flags = if pattern.contains("\\p") { "u" } else { "" };
        let cps = || pattern.chars().map(u32::from);
        let size = Regex::with_flags(pattern, flags).unwrap().resident_bytes();
        // The emitter counts a lower bound of what the finished program
        // reports, so a limit the finished program meets is never refused.
        let exact = CompileLimits {
            max_program_bytes: size,
        };
        let re = Regex::from_unicode_with_limits(cps(), flags, exact)
            .unwrap_or_else(|e| panic!("/{pattern}/ within its own size {size}: {e}"));
        assert_eq!(re.resident_bytes(), size);
        let small = CompileLimits {
            max_program_bytes: size / 4,
        };
        assert!(
            matches!(
                Regex::from_unicode_with_limits(cps(), flags, small),
                Err(CompileError::ProgramTooLarge)
            ),
            "/{pattern}/ must exceed {} bytes",
            size / 4
        );
        assert!(matches!(
            Regex::from_unicode_byteopt_with_limits(cps(), flags, small),
            Err(CompileError::ProgramTooLarge)
        ));
    }
    // A syntax error is still one, whatever the limit.
    assert!(matches!(
        Regex::from_unicode_with_limits("a)".chars().map(u32::from), "", CompileLimits::UNLIMITED),
        Err(CompileError::Syntax(_))
    ));
}
