//! Auto-possessified one-char greedy loops and the failed-run skip in the
//! regress fork (see crates/regress-fork/src/possessify.rs).
//!
//! Phase 1 marks a greedy one-char loop possessive when its class is disjoint
//! from the follow's first-set (backtracking into it is provably dead); Phase
//! 2 lets a failed attempt whose first atom is a possessive UNBOUNDED loop
//! resume the search after the maximal run. The verifier's counterexample —
//! bounded `/(\d{1,3})\./` on `"12345.6"`, where a real match starts INSIDE
//! the failed run — is pinned below; bounded loops must never get the skip.
//!
//! Every expectation was executed in node (v24) and matches byte-identically.
//! The whole file must also pass with `ZIPP_NO_RX_POSSESS=1` (the pass is
//! latched process-wide on first compile, so the off-mode runs in a child
//! process — see `zz_off_switch_agrees`).

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output
}

/// `JSON.stringify([index, ...groups])` for every match of `re` over `text`.
fn all_matches(re: &str, text: &str) -> String {
    let src = format!(
        r#"
        var re = {re};
        var text = {text};
        var out = [];
        var g = new RegExp(re.source, re.flags.indexOf("g") >= 0 ? re.flags : re.flags + "g");
        for (var m of text.matchAll(g)) out.push([m.index].concat(Array.from(m)));
        console.log(JSON.stringify(out));
        "#,
        re = re,
        text = text
    );
    run_ok(&src).join("\n")
}

/// The verifier's counterexample: a BOUNDED quantifier whose possessive
/// attempt at 0 fails at '4', while a real match ("345.") starts at offset 2
/// — inside the run. Phase 1 may possessify it; Phase 2 must not skip it.
#[test]
fn bounded_quantifier_matches_inside_failed_run() {
    assert_eq!(
        all_matches(r"/(\d{1,3})\./", r#""12345.6""#),
        r#"[[2,"345.","345"]]"#
    );
    // Longer runs: every window ending at the dot still found.
    assert_eq!(
        all_matches(r"/(\d{1,3})\./", r#""9876543.a1234567.b""#),
        r#"[[4,"543.","543"],[13,"567.","567"]]"#
    );
    // Exactly-bounded {2,3} variant.
    assert_eq!(
        all_matches(r"/(\d{2,3})!/", r#""12345! 1! 12!""#),
        r#"[[2,"345!","345"],[10,"12!","12"]]"#
    );
}

/// The bench's unbounded key=value shape: hits, misses, partial runs.
#[test]
fn unbounded_kv_hits_and_misses() {
    assert_eq!(
        all_matches(r"/([a-z]+)=(\d+)/", r#""key=123 plainword abc= =5 a1b=2 z=0""#),
        r#"[[0,"key=123","key","123"],[28,"b=2","b","2"],[32,"z=0","z","0"]]"#
    );
    assert_eq!(all_matches(r"/([a-z]+)=(\d+)/", r#""no equals here at all""#), "[]");
    // Run ends exactly at string end (skip hint == end of input).
    assert_eq!(all_matches(r"/([a-z]+)=(\d+)/", r#""trailingword""#), "[]");
    assert_eq!(all_matches(r"/([a-z]+)=(\d+)/", r#""x=1trailingword""#), r#"[[0,"x=1","x","1"]]"#);
    // Empty string.
    assert_eq!(all_matches(r"/([a-z]+)=(\d+)/", r#""""#), "[]");
}

/// min > 1 unbounded quantifiers: the skip is still exact — a start with
/// fewer than min class chars left fails the loop outright, and any other
/// start reaches the same failing follow position.
#[test]
fn min_greater_than_one_unbounded() {
    assert_eq!(
        all_matches(r"/(\d{2,})!/", r#""1! 12! 12345 999!""#),
        r#"[[3,"12!","12"],[13,"999!","999"]]"#
    );
    assert_eq!(all_matches(r"/([a-z]{3,})=/", r#""ab=cd= xyz=""#), r#"[[7,"xyz=","xyz"]]"#);
}

/// Overlapping classes: 'e' is inside [a-z], so /([a-z]+)e/ must NOT be
/// possessified — backtracking is load-bearing here.
#[test]
fn overlapping_follow_still_backtracks() {
    assert_eq!(
        all_matches(r"/([a-z]+)e/", r#""tree bee x""#),
        r#"[[0,"tree","tre"],[5,"bee","be"]]"#
    );
    // \w includes digits: /(\w+)(\d)/ needs backtracking.
    assert_eq!(all_matches(r"/(\w+)(\d)/", r#""abc123""#), r#"[[0,"abc123","abc12","3"]]"#);
}

/// Patterns with lookaround or backreferences are excluded from the pass
/// entirely; answers must be unchanged.
#[test]
fn lookaround_and_backref_excluded() {
    assert_eq!(
        all_matches(r"/([a-z]+)(?==)/", r#""aa=1 bbb=2""#),
        r#"[[0,"aa","aa"],[5,"bbb","bbb"]]"#
    );
    assert_eq!(
        all_matches(r"/([a-z]+)\1=/", r#""abab= aa=""#),
        r#"[[0,"abab=","ab"],[6,"aa=","a"]]"#
    );
    assert_eq!(
        all_matches(r"/(?<=\s)([a-z]+)=(\d+)/", r#""a=1 b=2""#),
        r#"[[4,"b=2","b","2"]]"#
    );
}

/// Unicode mode and non-ASCII text around the runs (exercises the UTF-8 and
/// UTF-16 input paths; the skip hint is a position, not a byte offset).
#[test]
fn unicode_flag_and_nonascii_text() {
    assert_eq!(
        all_matches(r"/([a-z]+)=(\d+)/u", r#""héllo wörld aa=42 é=1""#),
        r#"[[12,"aa=42","aa","42"]]"#
    );
    assert_eq!(
        all_matches(r"/(\d{1,3})\./u", r#""é12345.6é""#),
        r#"[[3,"345.","345"]]"#
    );
    // Astral characters inside the scanned region.
    assert_eq!(
        all_matches(r"/([a-z]+)=(\d+)/u", "\"\u{1F600}abc\u{1F600}k=7\""),
        r#"[[7,"k=7","k","7"]]"#
    );
}

/// Sticky and global lastIndex behaviour is unchanged by the skip (the skip
/// only ever crosses positions proven matchless).
#[test]
fn sticky_and_global_last_index() {
    let out = run_ok(
        r#"
        var y = /([a-z]+)=(\d+)/y;
        y.lastIndex = 0;
        console.log(y.test("aa=1 bb=2"), y.lastIndex);
        y.lastIndex = 3;
        console.log(y.test("aa=1 bb=2"), y.lastIndex);
        y.lastIndex = 5;
        console.log(y.test("aa=1 bb=2"), y.lastIndex);
        var g = /([a-z]+)=(\d+)/g;
        var s = "wwww aa=1 xxxx bb=22 yyyy";
        var m, idx = [];
        while ((m = g.exec(s)) !== null) idx.push(m.index + ":" + m[0] + ":" + g.lastIndex);
        console.log(idx.join(" "));
        "#,
    );
    assert_eq!(out, ["true 4", "false 0", "true 9", "5:aa=1:9 15:bb=22:20"]);
}

/// First atom possessive with the follow at Goal: /([a-z]+)/ — the loop's
/// backtrack entry is dead because nothing follows the match.
#[test]
fn follow_is_goal() {
    assert_eq!(
        all_matches(r"/([a-z]+)/", r#""ab1cd""#),
        r#"[[0,"ab","ab"],[3,"cd","cd"]]"#
    );
    assert_eq!(all_matches(r"/[a-z]+/", r#""zzz""#), r#"[[0,"zzz"]]"#);
}

/// Full match-set equality against node over a generated corpus: ~200
/// deterministic pseudo-random lines x the bench's patterns, comparing
/// String(m)+index for every match. Requires node on PATH.
const CORPUS_JS: &str = r#"
    function rng(seed) {
        var s = seed >>> 0;
        return function () {
            s = (s + 0x6D2B79F5) >>> 0;
            var t = s;
            t = Math.imul(t ^ (t >>> 15), t | 1);
            t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
            return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
        };
    }
    var r = rng(1234567);
    var words = ["error", "warn", "info", "user", "sess", "ip", "GET", "POST",
                 "trace", "abc", "de", "f", "keyvaluepairs", "x"];
    function line() {
        var n = 3 + Math.floor(r() * 12), parts = [];
        for (var i = 0; i < n; i++) {
            var t = r();
            if (t < 0.25) {
                parts.push(words[Math.floor(r() * words.length)] + "=" + Math.floor(r() * 100000));
            } else if (t < 0.45) {
                parts.push(Math.floor(r() * 300) + "." + Math.floor(r() * 300) + "." +
                           Math.floor(r() * 99999) + "." + Math.floor(r() * 300));
            } else if (t < 0.6) {
                parts.push(words[Math.floor(r() * words.length)]);
            } else if (t < 0.75) {
                parts.push("" + Math.floor(r() * 10 ** (1 + Math.floor(r() * 9))));
            } else if (t < 0.85) {
                parts.push(words[Math.floor(r() * words.length)] + "=");
            } else {
                parts.push("=" + Math.floor(r() * 1000));
            }
        }
        return parts.join(r() < 0.8 ? " " : "");
    }
    var res = [/(\d{1,3})\./g, /([a-z]+)=(\d+)/g, /([a-z]+)=/g, /(\d{2,})!/g, /(\w+)$/g];
    var acc = [];
    for (var i = 0; i < 200; i++) {
        var s = line();
        for (var j = 0; j < res.length; j++) {
            for (var m of s.matchAll(res[j])) acc.push(j + "|" + m.index + "|" + String(m));
        }
    }
    console.log(acc.length + ";" + acc.join(";"));
"#;

#[test]
fn corpus_matches_node() {
    let ours = run_ok(CORPUS_JS).join("\n");
    let node = std::process::Command::new("node")
        .arg("-e")
        .arg(CORPUS_JS)
        .output()
        .expect("node on PATH");
    assert!(node.status.success(), "node failed: {}", String::from_utf8_lossy(&node.stderr));
    let theirs = String::from_utf8_lossy(&node.stdout);
    assert_eq!(ours.trim(), theirs.trim(), "corpus match sets diverge from node");
}

/// The whole file again with `ZIPP_NO_RX_POSSESS=1`. The switch latches into
/// a process-wide cache on the first regex compile, so the off-mode needs a
/// fresh process: re-run this test binary as a child with the flag set.
#[test]
fn zz_off_switch_agrees() {
    if std::env::var_os("ZIPP_RX_POSSESS_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let out = std::process::Command::new(exe)
        .args(["--skip", "zz_off_switch_agrees"])
        .env("ZIPP_NO_RX_POSSESS", "1")
        .env("ZIPP_RX_POSSESS_CHILD", "1")
        .output()
        .expect("re-run test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !stdout.contains(" 0 passed"),
        "off switch (ZIPP_NO_RX_POSSESS=1) diverges:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );
}
