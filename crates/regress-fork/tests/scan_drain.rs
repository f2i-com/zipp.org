//! PATCH (see VENDORED.md): `Regex::scan_ascii` — the drained multi-match
//! scan behind the fused matchAll batch — must yield EXACTLY the
//! `find_from_ascii` match stream: same ranges, same capture ranges, in the
//! same order, for every start-predicate shape, including empty matches and
//! their advance. The drain is resumed across capped chunks exactly the way
//! the VM batch resumes it (from the last match end, one past it when empty).

use regress::{Flags, Regex};

type Caps = Vec<Option<std::ops::Range<usize>>>;
type Stream = Vec<(std::ops::Range<usize>, Caps)>;

fn stream_iter(re: &Regex, text: &str, start: usize) -> Stream {
    re.find_from_ascii(text, start)
        .map(|m| (m.range(), m.captures.clone()))
        .collect()
}

fn stream_drain(re: &Regex, text: &str, start: usize, cap: usize) -> Stream {
    let mut out: Stream = Vec::new();
    let mut from = start;
    loop {
        let before = out.len();
        let exhausted = re.scan_ascii(text, from, cap, &mut |r, caps| {
            out.push((r, caps.to_vec()));
        });
        if exhausted {
            return out;
        }
        assert_eq!(out.len() - before, cap, "a non-exhausted drain fills its cap");
        let (last, _) = out.last().expect("cap >= 1");
        from = if last.start == last.end { last.end + 1 } else { last.end };
    }
}

/// (pattern, flags, subject, start): every start-predicate shape, capture
/// mixes with unset groups, empty matches, and skip-hint-shaped patterns.
const CASES: &[(&str, &str, &str, usize)] = &[
    // ByteSet/Bracket prefixes + captures (the kv shape; skip-hint pattern).
    (r"([a-z]+)=(\d+)", "", "a=1 bb=22 ccc=333 dddd=4444 x= =5 e=6 q", 0),
    (r"([a-z]+)=(\d+)", "", "a=1 bb=22 ccc=333", 4),
    // Always-empty pattern: one match at every position.
    (r"(?:)", "", "abc", 0),
    // Mixed empty and non-empty matches.
    (r"a*", "", "xaaxa aaa", 0),
    (r"a*", "", "", 0),
    // Alternation leaving one group unset per match.
    (r"(a)|(b)", "", "abba x ab", 0),
    // Optional group unset on some matches.
    (r"ab(\d)?", "", "ab1 ab ab2 ab", 0),
    // ByteSeq literal prefix.
    (r"foobar(\d)", "", "xx foobar1 yy foobar2 foobar3zz foobar", 0),
    // Anchored (StartAnchored predicate): only start-adjacent matches.
    (r"^\d+", "", "123abc", 0),
    (r"^\d+", "", "abc123", 0),
    // Multiline anchor over several lines.
    (r"^(\w+):", "m", "one:1\ntwo:2\nthree:3", 0),
    // No match at all.
    (r"zzz", "", "aaaa", 0),
    // Start past the end of the subject.
    (r"a", "", "aaa", 7),
];

fn compile(pattern: &str, flags: &str) -> Regex {
    Regex::with_flags(pattern, Flags::from(flags)).expect("pattern compiles")
}

#[test]
fn drained_streams_equal_iterated_streams_for_every_cap() {
    for &(pattern, flags, text, start) in CASES {
        let re = compile(pattern, flags);
        let want = stream_iter(&re, text, start);
        for cap in [1usize, 2, 3, 16, 64] {
            let got = stream_drain(&re, text, start, cap);
            assert_eq!(
                got, want,
                "stream mismatch for /{pattern}/{flags} over {text:?} from {start} at cap {cap}"
            );
        }
    }
}

#[test]
fn exhausted_reports_end_of_subject_not_cap() {
    let re = compile(r"([a-z]+)=(\d+)", "");
    let text = "a=1 b=2 c=3";
    // Cap greater than the match count: one drain, exhausted.
    let mut n = 0usize;
    assert!(re.scan_ascii(text, 0, 16, &mut |_, _| n += 1));
    assert_eq!(n, 3);
    // Cap equal to the match count: the drain cannot know the subject is
    // done, so it reports non-exhausted and the resumed drain finds nothing.
    let mut hits: Vec<std::ops::Range<usize>> = Vec::new();
    assert!(!re.scan_ascii(text, 0, 3, &mut |r, _| hits.push(r)));
    assert_eq!(hits.len(), 3);
    let from = hits.last().unwrap().end;
    let mut extra = 0usize;
    assert!(re.scan_ascii(text, from, 3, &mut |_, _| extra += 1));
    assert_eq!(extra, 0);
}

#[test]
fn byteseq_optimized_twin_streams_match_too() {
    // `from_unicode_byteopt` is the compile the VM's ascii twin uses; the
    // drain must agree with the iterator on it as well.
    let re = Regex::from_unicode_byteopt("([a-z]+)-(\\d+)".chars().map(u32::from), Flags::default())
        .expect("pattern compiles");
    let text = "aq-0 bq-1 cq-2 dq-3 eq-4 fq-5 gq-6 hq-7 iq-8 jq-9 kq-10";
    let want = stream_iter(&re, text, 0);
    for cap in [1usize, 4, 16] {
        assert_eq!(stream_drain(&re, text, 0, cap), want);
    }
}
