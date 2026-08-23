use regress::{Match, Regex};
use std::ops::Range;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    range: Range<usize>,
    captures: Vec<Option<Range<usize>>>,
}

impl From<Match> for Snapshot {
    fn from(value: Match) -> Self {
        Self {
            range: value.range,
            captures: value.captures,
        }
    }
}

fn byteopt(pattern: &str, flags: &str) -> Regex {
    Regex::from_unicode_byteopt(pattern.chars().map(u32::from), flags).unwrap()
}

fn ascii_matches(re: &Regex, text: &str, start: usize) -> Vec<Snapshot> {
    re.find_from_ascii(text, start)
        .map(Snapshot::from)
        .collect()
}

fn incumbent_matches(re: &Regex, text: &str, start: usize) -> Vec<Snapshot> {
    re.find_from(text, start).map(Snapshot::from).collect()
}

#[track_caller]
fn compare(pattern: &str, flags: &str, text: &str, start: usize) {
    let fast = byteopt(pattern, flags);
    let incumbent = Regex::with_flags(pattern, flags).unwrap();
    assert_eq!(
        ascii_matches(&fast, text, start),
        incumbent_matches(&incumbent, text, start),
        "pattern={pattern:?} flags={flags:?} text={text:?} start={start}"
    );
}

fn all_strings(alphabet: &[u8], max_len: usize) -> Vec<String> {
    fn append(out: &mut Vec<String>, current: &mut Vec<u8>, alphabet: &[u8], remaining: usize) {
        out.push(String::from_utf8(current.clone()).unwrap());
        if remaining == 0 {
            return;
        }
        for &byte in alphabet {
            current.push(byte);
            append(out, current, alphabet, remaining - 1);
            current.pop();
        }
    }

    let mut out = Vec::new();
    append(&mut out, &mut Vec::new(), alphabet, max_len);
    out
}

#[test]
fn plans_are_owned_only_by_the_ascii_byteopt_twin() {
    let required = byteopt(r"//+(\w+)", "");
    let run = byteopt(r"([a-z]+)=(\d+)", "");
    assert_eq!(required.__rx_suffix_start_kind(), Some("required-prefix"));
    assert_eq!(run.__rx_suffix_start_kind(), Some("run-literal"));

    // The ordinary program is the one used by UTF-8/UTF-16/UCS-2 matching.
    // It must never own a byte-element start plan.
    assert_eq!(
        Regex::new(r"//+(\w+)").unwrap().__rx_suffix_start_kind(),
        None
    );
    assert_eq!(
        Regex::new(r"([a-z]+)=(\d+)")
            .unwrap()
            .__rx_suffix_start_kind(),
        None
    );

    // Even the byteopt constructor fails closed for Unicode semantics,
    // case-folding, and non-ASCII pattern atoms.
    assert_eq!(
        byteopt(r"([a-z]+)=(\d+)", "u").__rx_suffix_start_kind(),
        None
    );
    assert_eq!(
        byteopt(r"([a-z]+)=(\d+)", "i").__rx_suffix_start_kind(),
        None
    );
    assert_eq!(byteopt(r"([é]+)=x", "").__rx_suffix_start_kind(), None);
    assert_eq!(byteopt(r"([a-z]+)=é", "").__rx_suffix_start_kind(), None);
}

#[cfg(feature = "utf16")]
#[test]
fn utf16_and_non_ascii_subjects_stay_on_the_incumbent_program() {
    let re = Regex::with_flags(r"([a-z]+)=(\d+)", "u").unwrap();
    assert_eq!(re.__rx_suffix_start_kind(), None);
    let input: Vec<u16> = "é aa=12 β bb=3".encode_utf16().collect();
    let got: Vec<Snapshot> = re.find_from_utf16(&input, 0).map(Snapshot::from).collect();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].range, 2..7);
    assert_eq!(got[1].range, 10..14);
}

#[test]
fn exhaustive_required_prefix_matches_the_incumbent() {
    let pattern = r"//+(a+)";
    assert_eq!(
        byteopt(pattern, "").__rx_suffix_start_kind(),
        Some("required-prefix")
    );
    for text in all_strings(b"/ax", 6) {
        for start in 0..=text.len() + 1 {
            compare(pattern, "", &text, start);
        }
    }
}

#[test]
fn exhaustive_run_literal_matches_the_incumbent() {
    let pattern = r"([ab]+)=([01]+)";
    assert_eq!(
        byteopt(pattern, "").__rx_suffix_start_kind(),
        Some("run-literal")
    );
    for text in all_strings(b"ab=01", 5) {
        for start in 0..=text.len() + 1 {
            compare(pattern, "", &text, start);
        }
    }
}

#[test]
fn finite_bounds_overlap_and_failed_delimiters_are_exact() {
    for (pattern, text) in [
        (r"([ab]{2,3})=x", "a=xaab=xaaa=xaaaa=x"),
        (r"([ab]+)==x", "ab===x z ab==x ab====x aa==x"),
        (r"([ab]+)=([01]{2,3})", "ab=0 aa=01 b=012 a=0123"),
        (r"([ab]+):xy", "ab:x ab:xy a::xy ba:xy"),
    ] {
        assert_eq!(
            byteopt(pattern, "").__rx_suffix_start_kind(),
            Some("run-literal"),
            "pattern={pattern}"
        );
        for start in 0..=text.len() + 1 {
            compare(pattern, "", text, start);
        }
    }
}

#[test]
fn cap_ambiguity_restarts_incumbent_without_skipping() {
    let pattern = r"([ab]+)=x";
    let mut text = "a".repeat(96);
    text.push_str("=x tail bb=x");
    let fast = byteopt(pattern, "");
    let incumbent = Regex::new(pattern).unwrap();
    assert_eq!(fast.__rx_suffix_start_kind(), Some("run-literal"));
    for start in [0, 1, 17, 31, 32, 33, 64, 95, 96, text.len()] {
        assert_eq!(
            ascii_matches(&fast, &text, start),
            incumbent_matches(&incumbent, &text, start),
            "start={start}"
        );
    }
    assert_eq!(ascii_matches(&fast, &text, 0)[0].range, 0..98);
}

#[test]
fn sticky_exact_start_filter_remains_observable() {
    let pattern = r"([ab]+)=([01]+)";
    let fast = byteopt(pattern, "");
    let incumbent = Regex::new(pattern).unwrap();
    let text = "x aa=10 bb=11";
    for start in 0..=text.len() {
        let fast_sticky = fast
            .find_from_ascii(text, start)
            .next()
            .filter(|m| m.start() == start)
            .map(Snapshot::from);
        let slow_sticky = incumbent
            .find_from(text, start)
            .next()
            .filter(|m| m.start() == start)
            .map(Snapshot::from);
        assert_eq!(fast_sticky, slow_sticky, "sticky start={start}");
    }
}

#[test]
fn scan_drain_uses_the_same_candidate_order() {
    let pattern = r"([a-z]+)=(\d+)";
    let text = "status=200 bytes=13 bad=x ms=9 status=404";
    let re = byteopt(pattern, "");
    let expected = ascii_matches(&re, text, 0);
    let mut drained = Vec::new();
    let exhausted = re.scan_ascii(text, 0, usize::MAX, &mut |range, captures| {
        drained.push(Snapshot {
            range,
            captures: captures.to_vec(),
        });
    });
    assert!(exhausted);
    assert_eq!(drained, expected);
}

#[test]
fn unsupported_empty_backref_and_structural_cases_fall_back() {
    for pattern in [
        "",
        r"([ab]+)=\1",
        r"([ab]+)=(?:x|y)",
        r"([ab]+)=(?=x)",
        r"([ab]+)=x$",
        r"((?:a|b)+)=x",
        r"([ab]*)=x",
    ] {
        let re = byteopt(pattern, "");
        assert_eq!(
            re.__rx_suffix_start_kind(),
            None,
            "unexpected plan for {pattern}"
        );
        compare(pattern, "", "ab=x abab=y aa=aa x", 0);
    }
}

fn spawn_exact(test: &str, marker: &str, envs: &[(&str, &str)]) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.arg("--exact").arg(test).arg("--nocapture");
    for name in [
        "ZIPP_NO_RX_SUFFIX_START",
        "ZIPP_NO_RX_SUFFIX_RUNLITERAL",
        "ZIPP_NO_RX_SUFFIX_REQUIRED_PREFIX",
        "ZIPP_RXSTATS",
    ] {
        command.env_remove(name);
    }
    command.env("ZIPP_SUFFIX_START_TEST_CHILD", marker);
    for &(name, value) in envs {
        command.env(name, value);
    }
    let status = command.status().expect("spawn focused child test");
    assert!(status.success(), "child {test} ({marker}) failed: {status}");
}

#[test]
fn named_kill_switches_are_independent() {
    spawn_exact(
        "kill_switch_child",
        "master",
        &[("ZIPP_NO_RX_SUFFIX_START", "1")],
    );
    spawn_exact(
        "kill_switch_child",
        "required",
        &[("ZIPP_NO_RX_SUFFIX_REQUIRED_PREFIX", "1")],
    );
    spawn_exact(
        "kill_switch_child",
        "run",
        &[("ZIPP_NO_RX_SUFFIX_RUNLITERAL", "1")],
    );
}

#[test]
fn kill_switch_child() {
    let Ok(mode) = std::env::var("ZIPP_SUFFIX_START_TEST_CHILD") else {
        return;
    };
    let required = byteopt(r"//+(\w+)", "").__rx_suffix_start_kind();
    let run = byteopt(r"([a-z]+)=(\d+)", "").__rx_suffix_start_kind();
    match mode.as_str() {
        "master" => assert_eq!((required, run), (None, None)),
        "required" => assert_eq!((required, run), (None, Some("run-literal"))),
        "run" => assert_eq!((required, run), (Some("required-prefix"), None)),
        other => panic!("unexpected child mode {other}"),
    }
}

#[test]
fn mechanism_counters_prove_candidate_and_cap_paths() {
    spawn_exact("mechanism_counter_child", "stats", &[("ZIPP_RXSTATS", "1")]);
}

#[test]
fn mechanism_counter_child() {
    if std::env::var("ZIPP_SUFFIX_START_TEST_CHILD").as_deref() != Ok("stats") {
        return;
    }
    let before = regress::rx_suffix_start_stats();
    let short = byteopt(r"([ab]+)=x", "");
    assert_eq!(short.find_ascii("zz ab=x").unwrap().range(), 3..7);
    let middle = regress::rx_suffix_start_stats();
    assert!(middle.0 > before.0, "literal-hit counter did not move");
    assert!(middle.1 > before.1, "candidate counter did not move");

    let mut long = "a".repeat(96);
    long.push_str("=x");
    assert_eq!(short.find_ascii(&long).unwrap().range(), 0..98);
    let after = regress::rx_suffix_start_stats();
    assert!(after.2 > middle.2, "cap-fallback counter did not move");
}

#[cfg(all(feature = "rx-jit", target_arch = "x86_64"))]
#[test]
fn forced_rxjit_scan_session_uses_prefiltered_candidates() {
    spawn_exact("forced_rxjit_child", "jit", &[("ZIPP_RXSTATS", "1")]);
}

#[cfg(all(feature = "rx-jit", target_arch = "x86_64"))]
#[test]
fn forced_rxjit_child() {
    if std::env::var("ZIPP_SUFFIX_START_TEST_CHILD").as_deref() != Ok("jit") {
        return;
    }
    regress::__rxjit_force(Some(true));
    regress::__rx_scansession_force(Some(true));
    regress::__rx_acqgate_force(Some(false));
    let jit_before = regress::rx_jit_stats();

    let pattern = r"([ab]+)=([01]+)";
    let text = "x a=x aa=01 ab=10 bad=x b=11";
    let re = byteopt(pattern, "");
    let incumbent = Regex::new(pattern).unwrap();
    let expected = incumbent_matches(&incumbent, text, 0);
    let mut got = Vec::new();
    assert!(re.scan_ascii(text, 0, usize::MAX, &mut |range, captures| {
        got.push(Snapshot {
            range,
            captures: captures.to_vec(),
        });
    }));
    assert_eq!(got, expected);
    assert!(
        re.__rxjit_is_compiled(),
        "forced native plan did not compile"
    );
    let jit_after = regress::rx_jit_stats();
    assert!(
        jit_after.3 > jit_before.3,
        "forced native candidate attempts did not execute"
    );

    regress::__rx_acqgate_force(None);
    regress::__rx_scansession_force(None);
    regress::__rxjit_force(None);
}
