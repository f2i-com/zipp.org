//! Differential harness for the regex JIT (rxjit.rs): every (pattern, flags,
//! subject) in the corpus is executed twice over the byte path — once with
//! the JIT forced OFF (the classical interpreter) and once with it forced ON
//! (compile on first attempt; patterns outside the subset decline and run
//! interpreted, which is itself part of the contract under test) — and the
//! complete match arrays, including every capture group and index, must be
//! byte-identical. When a `node` binary is on PATH, `matchAll` results for
//! the same corpus are compared as well.

#![cfg(target_arch = "x86_64")]

use regress::Regex;
use std::fmt::Write as _;

/// Deterministic xorshift so the fuzz corpus is reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T: Copy>(&mut self, v: &'a [T]) -> T {
        v[self.below(v.len())]
    }
}

/// Canonical encoding of all matches from `find_from_ascii` starting at
/// `start`: "s,e[,g1s,g1e|-,-...];..." — one segment per match.
fn run_all(re: &Regex, subject: &str, start: usize) -> String {
    let mut out = String::new();
    for m in re.find_from_ascii(subject, start) {
        if !out.is_empty() {
            out.push(';');
        }
        write!(out, "{},{}", m.range.start, m.range.end).unwrap();
        for c in &m.captures {
            match c {
                Some(r) => write!(out, ",{},{}", r.start, r.end).unwrap(),
                None => out.push_str(",-,-"),
            }
        }
    }
    out
}

fn compare_case(pattern: &str, flags: &str, subject: &str, start: usize) -> Option<String> {
    let compile = || Regex::with_flags(pattern, flags);
    let (Ok(interp_re), Ok(jit_re)) = (compile(), compile()) else {
        return None; // not a valid pattern; nothing to compare
    };
    regress::__rxjit_force(Some(false));
    let interp = run_all(&interp_re, subject, start);
    regress::__rxjit_force(Some(true));
    let jit = run_all(&jit_re, subject, start);
    regress::__rxjit_force(None);
    assert_eq!(
        interp, jit,
        "JIT/interpreter divergence on /{pattern}/{flags} over {subject:?} from {start}"
    );
    Some(interp)
}

fn corpus() -> Vec<(String, String, String)> {
    let mut cases: Vec<(String, String, String)> = Vec::new();
    let mut add = |p: &str, f: &str, s: &str| cases.push((p.into(), f.into(), s.into()));

    // The bench row's real shapes (bench/real/regex-log-scan.js).
    let log = "2026-07-01T10:11:12Z 10.42.7.19 GET /api/v1/items?id=99 status=200 bytes=5124 ua=\"curl/8.4\" ref=- lvl=info";
    add(r"(\d+)\.(\d+)\.(\d+)\.(\d+)", "g", log);
    add(r"(\w+)=(\S+)", "g", log);
    add(r"status=(\d{3})", "g", log);
    add(r"\b(GET|POST|PUT|DELETE)\b", "g", log);
    add(r"^\S+ \S+ (\w+) ([^ ]+)", "gm", "a b GET /x\nc d POST /y");
    add(r#""([^"]*)""#, "g", log);
    add(r"lvl=(info|warn|error)", "g", log);

    // Edge cases: empty subject, empty matches, match at end, no match,
    // overlapping alternations, nested/optional captures, lazy vs greedy.
    add("a*", "g", "");
    add("a*", "g", "aaabaa");
    add("", "g", "abc");
    add("a", "g", "");
    add("b$", "g", "aab");
    add("^a", "g", "ba");
    add("(a)(b)?", "g", "ab a");
    add("(a|ab)(c|bcd)", "g", "abcd");
    add("a+?b", "g", "aaab aab");
    add("a+b", "g", "aaab aab");
    add("a??", "g", "aa");
    add("(z)?(a)", "g", "a");
    add("[a-c]+[0-9]{2,3}", "g", "abc12 b999 c1");
    add("x|", "g", "axb");
    add(r"\bword\b", "g", "a word, wordy word");
    add(r"\B\d", "g", "a1 22 3b");
    add("a{2,}", "g", "a aa aaaa");
    add("a{0,2}", "g", "aaa");
    add("(ab?)*c?", "g", "ababac abc");
    add(".", "g", "a\nb");
    add(".", "gs", "a\nb");
    add("^.", "gm", "ab\ncd\r\nef");
    add(".$", "gm", "ab\ncd\r\nef");
    add("AbC", "gi", "abc ABC aBc");
    add("[a-z]+", "gi", "MiXeD case");
    add("(a+)(a*)(a?)", "g", "aaaa");
    add("q(?:u)(x)?", "g", "quiz qu");
    add("[^a]+", "g", "abba");
    // Unsupported subset (declines; both sides interpreted -> equal).
    add("(a)\\1", "g", "aa ab");
    add("a(?=b)", "g", "ab ac");
    add("(?<=a)b", "g", "ab cb");
    add("(ab)+", "g", "ababab");
    add(r"A", "g", "A");

    // Deterministic fuzz over the supported subset.
    let mut rng = Rng(0x5eed_2026_0803);
    let atoms = [
        "a", "b", "c", "0", r"\d", r"\w", r"\s", ".", "[a-c]", "[^a-c]", "[b0-9-]", "ab", "abc",
        "a|b", "(a)", "(b|cd)", r"(\d)", "(?:ab|a)", "[abx]", "=",
    ];
    let quants = ["", "", "", "*", "+", "?", "*?", "+?", "??", "{1,3}", "{2}", "{0,2}?"];
    let flagsets = ["g", "g", "g", "gi", "gm", "gs"];
    let alphabet: Vec<char> = "aabbccdd0123 =-.x\n".chars().collect();
    for _ in 0..400 {
        let mut pat = String::new();
        if rng.below(8) == 0 {
            pat.push('^');
        }
        for _ in 0..(1 + rng.below(4)) {
            pat.push_str(rng.pick(&atoms));
            pat.push_str(rng.pick(&quants));
        }
        if rng.below(8) == 0 {
            pat.push('$');
        }
        let mut subj = String::new();
        for _ in 0..rng.below(28) {
            subj.push(rng.pick(&alphabet));
        }
        cases.push((pat, rng.pick(&flagsets).to_string(), subj));
    }
    cases
}

#[test]
fn rxjit_differential_vs_interpreter() {
    let cases = corpus();
    let mut compared = 0usize;
    for (p, f, s) in &cases {
        // Whole-subject scan plus a couple of nonzero start offsets
        // (the sticky/lastIndex entry points use find_from_ascii(_, start)).
        for start in [0usize, 1, s.len() / 2] {
            if start <= s.len() && compare_case(p, f, s, start).is_some() {
                compared += 1;
            }
        }
    }
    // Force-on must have actually compiled a good chunk of the corpus.
    let (compiled, declined_unsup, declined_lim, native, _interp, bails) =
        regress::rx_jit_stats();
    println!(
        "differential: {compared} runs; jit compiled {compiled}, declined {declined_unsup}+{declined_lim}, native attempts {native}, bails {bails}"
    );
    assert!(compared > 800, "corpus unexpectedly small: {compared}");
    assert!(compiled > 100, "JIT compiled too few corpus patterns: {compiled}");
    assert_eq!(bails, 0, "native code should not hit the backtrack cap here");
}

/// The scan-session path (rxjit.rs `with_session`) hoists the TLS borrow and
/// context build out of the advance loop; its match streams — ranges and
/// capture groups — must be identical to the legacy per-attempt wrapper's.
#[test]
fn rx_session_differential_vs_legacy() {
    let cases = corpus();
    let sessions_before = regress::rx_session_stats();
    let mut compared = 0usize;
    for (p, f, s) in &cases {
        for start in [0usize, 1, s.len() / 2] {
            if start > s.len() {
                continue;
            }
            let compile = || Regex::with_flags(p.as_str(), f.as_str());
            let (Ok(legacy_re), Ok(session_re)) = (compile(), compile()) else {
                continue;
            };
            regress::__rxjit_force(Some(true));
            regress::__rx_scansession_force(Some(false));
            let legacy = run_all(&legacy_re, s, start);
            regress::__rx_scansession_force(Some(true));
            // Twice: the second drain is compiled from its first attempt, so
            // every next_match call of it runs inside a session.
            let session_cold = run_all(&session_re, s, start);
            let session_warm = run_all(&session_re, s, start);
            regress::__rx_scansession_force(None);
            regress::__rxjit_force(None);
            assert_eq!(
                legacy, session_cold,
                "session/legacy divergence (cold) on /{p}/{f} over {s:?} from {start}"
            );
            assert_eq!(
                legacy, session_warm,
                "session/legacy divergence (warm) on /{p}/{f} over {s:?} from {start}"
            );
            compared += 1;
        }
    }
    let sessions = regress::rx_session_stats() - sessions_before;
    println!("session differential: {compared} cases; {sessions} sessions opened");
    assert!(compared > 300, "corpus unexpectedly small: {compared}");
    assert!(sessions > 100, "session path did not engage: {sessions}");
}

/// The backtrack buffer can grow mid-session (the native -2 return); the
/// session must recompute its context pointers rather than keep entering the
/// stale buffer. A fresh thread gets the small initial buffer, so the first
/// deep attempt is guaranteed to grow it inside an active session.
#[test]
fn rx_session_bt_grow() {
    // ~1100 GreedyLoop1Char pushes per attempt at position 0 — past the
    // 1024-entry initial buffer.
    let pattern = "a?".repeat(1200);
    let subject = "a".repeat(1100);
    regress::__rxjit_force(Some(true));
    regress::__rx_scansession_force(Some(false));
    let legacy = run_all(&Regex::with_flags(&pattern, "g").unwrap(), &subject, 0);
    regress::__rxjit_force(None);
    regress::__rx_scansession_force(None);
    let session = std::thread::spawn(move || {
        regress::__rxjit_force(Some(true));
        regress::__rx_scansession_force(Some(true));
        let r = run_all(&Regex::with_flags(&pattern, "g").unwrap(), &subject, 0);
        regress::__rxjit_force(None);
        regress::__rx_scansession_force(None);
        r
    })
    .join()
    .unwrap();
    assert_eq!(legacy, session, "session diverged across a mid-scan bt grow");
    // Deterministic when run filtered to this test alone (no force races):
    // both instances compile, and the spawned drain runs inside sessions.
    let (compiled, _, _, native, _, bails) = regress::rx_jit_stats();
    println!(
        "bt-grow: compiled {compiled}, native attempts {native}, bails {bails}, sessions {}",
        regress::rx_session_stats()
    );
    assert_eq!(bails, 0, "the grow-and-retry loop must not bail here");
}

/// The session entry probes read-only (rxjit.rs `acquire_if_compiled`): a
/// scan that attempts zero positions must not advance the compile threshold,
/// while real attempts still tick it — and the match streams with the gate on
/// must be identical to the entry-tick path's. One test function on purpose:
/// the acqgate force flag is process-global and phases must not race it.
#[test]
fn rx_acqgate_threshold_and_streams() {
    // Phase A: a compile-eligible pattern whose prefix search finds no
    // candidate. Zero attempts per scan, so under the gate the slot must
    // stay cold no matter how many scans run. Read-only probing makes this
    // deterministic even while other tests toggle the jit/session forces.
    regress::__rx_acqgate_force(Some(true));
    regress::__rx_scansession_force(Some(true));
    let re = Regex::with_flags(r"x\d\d", "g").unwrap();
    let blank = "a".repeat(256);
    let sessions_before = regress::rx_session_stats();
    for _ in 0..200 {
        assert_eq!(run_all(&re, &blank, 0), "");
    }
    let gated_sessions = regress::rx_session_stats() - sessions_before;
    assert!(
        !re.__rxjit_is_compiled(),
        "zero-attempt scans advanced the compile threshold through the gate"
    );

    // Gate-off contrast (the ZIPP_NO_RX_ACQGATE semantics): the same scans
    // tick the counter once per ENTRY, so a regex that never attempts still
    // compiles — and then opens a session per zero-attempt scan. Loop with
    // slack: concurrent tests may transiently force the jit or session off.
    regress::__rx_acqgate_force(Some(false));
    let ungated = Regex::with_flags(r"x\d\d", "g").unwrap();
    let sessions_before = regress::rx_session_stats();
    for _ in 0..10_000 {
        assert_eq!(run_all(&ungated, &blank, 0), "");
        if ungated.__rxjit_is_compiled() {
            break;
        }
    }
    assert!(
        ungated.__rxjit_is_compiled(),
        "gate off no longer reproduces the per-entry counter tick"
    );
    // Once compiled, every further zero-attempt scan opens a session.
    for _ in 0..20 {
        assert_eq!(run_all(&ungated, &blank, 0), "");
    }
    let ungated_sessions = regress::rx_session_stats() - sessions_before;
    // Deterministic when run filtered to this test alone (no force races):
    // the ungated twin compiles on entry ticks alone and sessions its
    // zero-attempt scans; the gated one opens none.
    println!(
        "acqgate zero-attempt sessions: gated {gated_sessions}, ungated {ungated_sessions}"
    );
    regress::__rx_acqgate_force(Some(true));

    // Phase B: the SAME instance compiles once scans elsewhere actually
    // attempt (one failing attempt per scan at the 'x'). Loop past the
    // threshold with slack: concurrent tests may force the jit off for
    // brief windows, skipping some counter ticks.
    let attempting = format!("{}x1a", "a".repeat(8));
    for _ in 0..10_000 {
        assert_eq!(run_all(&re, &attempting, 0), "");
        if re.__rxjit_is_compiled() {
            break;
        }
    }
    assert!(
        re.__rxjit_is_compiled(),
        "attempting scans no longer cross the compile threshold under the gate"
    );

    // Phase C: gate on/off byte-identity of match streams over the corpus,
    // cold (compiles mid-stream) and warm (all-session) alike.
    regress::__rxjit_force(Some(true));
    let mut compared = 0usize;
    for (p, f, s) in &corpus() {
        let compile = || Regex::with_flags(p.as_str(), f.as_str());
        let (Ok(off_re), Ok(on_re)) = (compile(), compile()) else {
            continue;
        };
        regress::__rx_acqgate_force(Some(false));
        let off_cold = run_all(&off_re, s, 0);
        let off_warm = run_all(&off_re, s, 0);
        regress::__rx_acqgate_force(Some(true));
        let on_cold = run_all(&on_re, s, 0);
        let on_warm = run_all(&on_re, s, 0);
        assert_eq!(
            off_cold, on_cold,
            "acqgate divergence (cold) on /{p}/{f} over {s:?}"
        );
        assert_eq!(
            off_warm, on_warm,
            "acqgate divergence (warm) on /{p}/{f} over {s:?}"
        );
        compared += 1;
    }
    regress::__rx_acqgate_force(None);
    regress::__rx_scansession_force(None);
    regress::__rxjit_force(None);
    println!("acqgate differential: {compared} cases");
    assert!(compared > 300, "corpus unexpectedly small: {compared}");
}

/// Compare against node's matchAll for the same corpus where flags allow.
#[test]
fn rxjit_differential_vs_node() {
    let cases = corpus();
    let hex = |s: &str| s.bytes().fold(String::new(), |mut a, b| {
        write!(a, "{b:02x}").unwrap();
        a
    });
    // One input line per case; node prints the same canonical encoding as
    // run_all, or "ERR" for patterns it rejects.
    let mut input = String::new();
    for (p, f, s) in &cases {
        input.push_str(&format!("{}\t{}\t{}\n", hex(p), hex(f), hex(s)));
    }
    let script = r#"
const fs = require('fs');
const unhex = h => Buffer.from(h, 'hex').toString('latin1');
const lines = fs.readFileSync(0, 'utf8').split('\n').filter(l => l.length);
for (const line of lines) {
  const [p, f, s] = line.split('\t').map(unhex);
  let out;
  try {
    const re = new RegExp(p, f.includes('d') ? f : f + 'd');
    const segs = [];
    for (const m of s.matchAll(re)) {
      let seg = m.index + ',' + (m.index + m[0].length);
      for (let g = 1; g < m.length; g++) {
        const ind = m.indices && m.indices[g];
        seg += ind ? (',' + ind[0] + ',' + ind[1]) : ',-,-';
      }
      segs.push(seg);
    }
    out = segs.join(';');
  } catch (e) { out = 'ERR'; }
  console.log(out);
}
"#;
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let child = Command::new("node")
        .arg("-e")
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("node not on PATH; skipping node differential");
        return;
    };
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "node run failed");
    let node_lines: Vec<&str> = std::str::from_utf8(&out.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(node_lines.len(), cases.len(), "node output line count");
    let mut compared = 0usize;
    for ((p, f, s), node) in cases.iter().zip(node_lines) {
        if node == "ERR" || !s.is_ascii() {
            continue;
        }
        // Skip subjects node treats differently only via non-ASCII; corpus is
        // ASCII already. Compare the interpreter (== JIT per the other test).
        regress::__rxjit_force(Some(true));
        let ours = match Regex::with_flags(p, f.as_str()) {
            Ok(re) => run_all(&re, s, 0),
            Err(_) => continue,
        };
        regress::__rxjit_force(None);
        assert_eq!(&ours, node, "node divergence on /{p}/{f} over {s:?}");
        compared += 1;
    }
    println!("node differential: {compared}/{} cases compared", cases.len());
    assert!(compared > 300, "too few node comparisons: {compared}");
}
