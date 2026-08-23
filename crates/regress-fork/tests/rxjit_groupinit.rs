//! PATCH (see rxjit.rs): the per-attempt capture-slot reset moved OUT of the
//! Rust attempt loop and INTO the compiled prologue (`ZIPP_NO_RX_GROUPINIT=1`
//! puts it back). The reset is what stops one attempt's capture writes from
//! being published by a LATER attempt, so this file pins the exact shapes
//! that would go wrong if the emitted stores were dropped, mis-sized, or
//! placed after a jump target the retry path re-enters through.
//!
//! Every case is asserted three ways: emitted-reset vs Rust-fill (the switch
//! matrix), and both against the plain interpreter. All of it lives in ONE
//! `#[test]` on purpose — the force flags are process-global, so two test
//! functions in this binary would race them.

#![cfg(target_arch = "x86_64")]

use regress::Regex;
use std::fmt::Write as _;

type Caps = Vec<Option<std::ops::Range<usize>>>;
type Stream = Vec<(std::ops::Range<usize>, Caps)>;

/// Canonical text of a match stream: "s,e[,gNs,gNe|-,-];..." — captures
/// included, because a stale-group defect is invisible in ranges alone.
fn render(stream: &Stream) -> String {
    let mut out = String::new();
    for (r, caps) in stream {
        if !out.is_empty() {
            out.push(';');
        }
        write!(out, "{},{}", r.start, r.end).unwrap();
        for c in caps {
            match c {
                Some(r) => write!(out, ",{},{}", r.start, r.end).unwrap(),
                None => out.push_str(",-,-"),
            }
        }
    }
    out
}

fn stream_iter(re: &Regex, text: &str, start: usize) -> Stream {
    re.find_from_ascii(text, start)
        .map(|m| (m.range(), m.captures.clone()))
        .collect()
}

/// The DRAINED stream: one `Session` spans every hit, so attempt N+1 starts
/// on the capture slots attempt N's successful match left behind. This is the
/// path the reset actually protects.
fn stream_drain(re: &Regex, text: &str, start: usize, cap: usize) -> Stream {
    let mut out: Stream = Vec::new();
    let mut from = start;
    loop {
        if re.scan_ascii(text, from, cap, &mut |r, caps| out.push((r, caps.to_vec()))) {
            return out;
        }
        let (last, _) = out.last().expect("a non-exhausted drain emitted at least one hit");
        from = if last.start == last.end { last.end + 1 } else { last.end };
    }
}

/// (pattern, subject). Each one makes a FAILED or EARLIER attempt write a
/// capture slot that a LATER attempt must not observe.
fn cases() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = Vec::new();
    let mut add = |p: &str, s: &str| v.push((p.into(), s.into()));

    // THE defect shape. Hit 1 takes the capturing branch and sets group 1;
    // hit 2 takes the bare branch, so group 1 must come back UNSET. Without
    // a reset, hit 2's attempt saves the STALE range in its backtrack entry
    // and restores it when the capturing branch fails.
    add(r"(?:(a)|b)", "ab");
    add(r"(?:(a)|b)", "aabbab");
    // Same, with the unset group in the middle and at the end.
    add(r"(?:(a)(x)?|b)", "axabaxb");
    add(r"(?:(a)|(b)|c)", "abcabc");
    // Optional trailing capture: set on some hits, unset on others.
    add(r"q(\d)?", "q1qq2qq3q");
    // A capture inside a quantified group: set on the last iteration only.
    add(r"(?:z(\d))+", "z1z2 z3 zz z9");
    // Failed attempts that write deep before failing, then a success that
    // enters none of those groups.
    add(r"(?:(a)(b)(c)(d)e|z)", "abcdz abcz abz az z");
    // The row's own shapes.
    add(r"([a-z]+)=(\d+)", "a=1 bb=22 ccc=333 x= =5 e=6 q");
    add(r"(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})", "1.2.3.4 x 10.20.30.40 9.9.9");
    add(r"//+(\w+)", "a//b/c//dd///e");
    // Empty matches interleaved with capturing ones (advance + reset).
    add(r"(a)|", "aba");
    add(r"(a*)", "aa b aaa");

    // Group-count sweep, including the unroll cap (64) and past it, where the
    // Rust fill must take over. Each pattern's second hit succeeds through a
    // branch that enters no group at all.
    for n in [0usize, 1, 2, 5, 16, 63, 64, 65, 70] {
        let nest = format!("{}x{}", "(".repeat(n), ")".repeat(n));
        v.push((format!("(?:{nest}|y)"), "xyxy".into()));
    }
    v
}

#[test]
fn rx_groupinit_prologue_reset() {
    let cases = cases();
    assert!(cases.len() > 15, "corpus unexpectedly small");

    // --- 1. Interpreter reference, then both JIT arms, on every case. ---
    for (p, s) in &cases {
        for start in [0usize, 1] {
            if start > s.len() {
                continue;
            }
            let mk = || Regex::with_flags(p.as_str(), "g").expect("valid pattern");

            regress::__rxjit_force(Some(false));
            let interp_iter = render(&stream_iter(&mk(), s, start));
            let interp_drain = render(&stream_drain(&mk(), s, start, 3));

            // Emitted reset (the mechanism).
            regress::__rxjit_force(Some(true));
            regress::__rx_groupinit_force(Some(true));
            let on_iter = render(&stream_iter(&mk(), s, start));
            let on_drain1 = render(&stream_drain(&mk(), s, start, 1));
            let on_drain3 = render(&stream_drain(&mk(), s, start, 3));
            let on_drain_all = render(&stream_drain(&mk(), s, start, 4096));

            // Rust fill (the off-switch).
            regress::__rx_groupinit_force(Some(false));
            let off_iter = render(&stream_iter(&mk(), s, start));
            let off_drain_all = render(&stream_drain(&mk(), s, start, 4096));

            // The LEGACY per-attempt wrapper (`run_attempt`, no Session):
            // it re-establishes the scratch LENGTH but must leave the value
            // fill to the prologue exactly as the session path does.
            regress::__rx_scansession_force(Some(false));
            regress::__rx_groupinit_force(Some(true));
            let legacy_on = render(&stream_iter(&mk(), s, start));
            regress::__rx_groupinit_force(Some(false));
            let legacy_off = render(&stream_iter(&mk(), s, start));
            regress::__rx_scansession_force(None);

            regress::__rx_groupinit_force(None);
            regress::__rxjit_force(None);

            let ctx = format!("/{p}/g over {s:?} from {start}");
            assert_eq!(interp_iter, on_iter, "iter: jit/interp divergence, {ctx}");
            assert_eq!(interp_iter, off_iter, "iter: off-switch divergence, {ctx}");
            assert_eq!(interp_drain, on_drain3, "drain: jit/interp divergence, {ctx}");
            assert_eq!(on_drain_all, off_drain_all, "drain: switch divergence, {ctx}");
            // A drained stream must equal the one-match-at-a-time stream: the
            // iter path opens a FRESH session per match (so `Session::new`
            // resets its slots), the drain shares one across hits. Only the
            // per-attempt reset makes the two agree.
            assert_eq!(on_iter, on_drain_all, "drain != iter under the emitted reset, {ctx}");
            assert_eq!(on_drain1, on_drain_all, "cap-1 resume diverged, {ctx}");
            assert_eq!(interp_iter, legacy_on, "run_attempt: emitted-reset divergence, {ctx}");
            assert_eq!(interp_iter, legacy_off, "run_attempt: off-switch divergence, {ctx}");
        }
    }

    // --- 2. The -2 backtrack-overflow RETRY re-enters the compiled entry, so
    // the reset must sit at the true entry label: a retry has to re-clear the
    // slots the aborted run dirtied. ~1100 pushes per attempt blows the
    // 1024-entry initial buffer on a fresh thread. ---
    let pattern = format!("{}(x)?y", "a?".repeat(1200));
    let subject = format!("{}y", "a".repeat(1100));
    let grow = |on: bool| {
        let p = pattern.clone();
        let s = subject.clone();
        std::thread::spawn(move || {
            regress::__rxjit_force(Some(true));
            regress::__rx_groupinit_force(Some(on));
            let re = Regex::with_flags(&p, "g").unwrap();
            let out = render(&stream_drain(&re, &s, 0, 4096));
            regress::__rx_groupinit_force(None);
            regress::__rxjit_force(None);
            out
        })
        .join()
        .unwrap()
    };
    let grow_on = grow(true);
    let grow_off = grow(false);
    regress::__rxjit_force(Some(false));
    let grow_interp = render(&stream_drain(
        &Regex::with_flags(&pattern, "g").unwrap(),
        &subject,
        0,
        4096,
    ));
    regress::__rxjit_force(None);
    assert_eq!(grow_on, grow_off, "bt-grow retry diverged across the switch");
    assert_eq!(grow_on, grow_interp, "bt-grow retry diverged from the interpreter");

    // --- 3. The thread-local scratch is SHARED between regexes. A regex with
    // the same group count must not read the previous one's leftovers. ---
    let interleave = |jit: bool| {
        regress::__rxjit_force(Some(jit));
        regress::__rx_groupinit_force(Some(jit));
        let a = Regex::with_flags(r"(\d)(\d)", "g").unwrap();
        let b = Regex::with_flags(r"(?:(p)(q)|r)", "g").unwrap();
        let mut out = String::new();
        for _ in 0..8 {
            out.push_str(&render(&stream_drain(&a, "12 34 56", 0, 4096)));
            out.push('|');
            out.push_str(&render(&stream_drain(&b, "pq r pq r", 0, 4096)));
            out.push('|');
        }
        regress::__rx_groupinit_force(None);
        regress::__rxjit_force(None);
        out
    };
    assert_eq!(
        interleave(true),
        interleave(false),
        "the shared thread-local scratch leaked capture slots between regexes"
    );

    // --- 4. The mechanism must actually have engaged: without a compiled
    // regex the assertions above prove nothing. ---
    regress::__rxjit_force(Some(true));
    regress::__rx_groupinit_force(Some(true));
    let engaged = Regex::with_flags(r"(?:(a)|b)", "g").unwrap();
    let _ = stream_drain(&engaged, "ab", 0, 4096);
    let compiled = engaged.__rxjit_is_compiled();
    regress::__rx_groupinit_force(None);
    regress::__rxjit_force(None);
    assert!(compiled, "the forced JIT did not compile: the corpus ran interpreted");
}
