//! W14: multi-receiver B94 live-range splitting (`ZIPP_NO_MULTI_SPLIT=1`).
//!
//! Two gates moved in `plan_region`, both in the recycled-pinned-receiver
//! branch:
//!
//!  * the non-DataView split budget was a hard ONE per region
//!    (`non_dv_split_used`). Nothing in either integer emitter is per-region
//!    about a split — `split_recvs` / `write_through` are register SETS, the
//!    write-through hook fires on every def of any member, and `flush_exit`
//!    skips all of them — so the budget was an untested-shape guard. It is now
//!    `MULTI_SPLIT_BUDGET` (4, the four recycled receivers of the
//!    parse-large-js mix loop).
//!  * the `pin_obj` match that decides whether a receiver's identity is
//!    readable from a GLOBAL slot only listed element ops and DataView `get*`.
//!    A recycled pinned flat-ASCII STRING receiver (`src.charCodeAt(i)`) could
//!    therefore never take the split path and declined the whole region with
//!    "pinned receiver reg not cleanly excludable" — even though `recv_use_at`
//!    forty lines below had always listed it and both emitters read a string
//!    pin's identity from the pin's source exactly like an element pin's.
//!
//! Every `msplit_parity_*` case asserts byte-identical output against
//! `node -e`. `msplit_all_modes_answer_identically` re-runs them under
//! `ZIPP_NO_MULTI_SPLIT=1` (the off-switch — a pure fallback),
//! `ZIPP_INT_SPLIT=1` (admits the multi-split plan to the XMM integer emitter),
//! `ZIPP_NO_GPR_SPLIT=1`, `ZIPP_JIT_THRESHOLD=1`, `ZIPP_GC_STRESS=1` and
//! `ZIPP_NOJIT=1`. The XMM mechanism pin additionally sets
//! `ZIPP_NO_GPR_HOMES=1`, ensuring the preferred GPR emitter cannot win first.
//!
//! `msplit_mechanism_*` reads the plan back out of a child's ZIPP_JITLOG. The
//! two-element case pins the budget extension with two B94 splits in one
//! region. The string case separately pins the `pin_obj` extension: its
//! recycled string receiver splits while its clean element receiver remains an
//! ordinary pin.

//! Pins x86-64 JIT mechanisms from the engine's logs and counters, which the interpreter-only profiles never emit; compiled only where that tier exists, like the other tier-pinning suites.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const PRELUDE: &str = r#""use strict";
var N = 20000;
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node v24 on PATH (expected values come from `node -e`)");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn assert_matches_node(src: &str) {
    let ours = run_ok(src);
    let node = node_output(src);
    assert_eq!(ours, node, "zipp != node for:\n{src}");
}

fn prog(body: &str) -> String {
    format!("{PRELUDE}{body}")
}

/// TWO recycled dense-Array receivers in one region — the shape the old budget
/// of one declined outright ("pinned receiver reg not cleanly excludable"),
/// dropping a 10-home fnv1a-over-two-arrays loop from the INT-GPR tier to MEM.
#[test]
fn msplit_parity_two_element_receivers() {
    assert_matches_node(&prog(
        r#"var a = [], b = [];
for (var i = 0; i < N; i++) { a.push(i % 13); b.push(i % 97); }
var h = 0;
function f(n) {
  for (var ti = 0; ti < n; ti++) {
    h = (h ^ a[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
    h = (h ^ b[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
  }
}
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("two h=" + h);
"#,
    ));
}

/// A recycled pinned-STRING receiver next to a cleanly excludable element
/// receiver — the `pin_obj` half. `src` is a global read only through
/// `charCodeAt`, and the bytecode allocator recycles its register as an
/// arithmetic temp. `starts` remains an ordinary pinned receiver.
#[test]
fn msplit_parity_string_and_element_receivers() {
    assert_matches_node(&prog(
        r#"var starts = [];
var src = "";
for (var i = 0; i < 64; i++) src += "abcdefghijklmnopqrstuvwxyz0123456789 ";
for (var i = 0; i < N; i++) starts.push(i % 2000);
var h = 0;
function f(n) {
  for (var ti = 0; ti < n; ti++) {
    h = (h ^ src.charCodeAt(starts[ti])) | 0; h = Math.imul(h, 16777619) >>> 0;
  }
}
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("strrecv h=" + h);
"#,
    ));
}

/// THREE recycled element receivers.
#[test]
fn msplit_parity_three_element_receivers() {
    assert_matches_node(&prog(
        r#"var a = [], b = [], c = [];
for (var i = 0; i < N; i++) { a.push(i % 13); b.push(i % 97); c.push(i % 31); }
var h = 0;
function f(n) {
  for (var ti = 0; ti < n; ti++) {
    h = (h ^ a[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
    h = (h ^ b[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
    h = (h ^ c[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
  }
}
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("three h=" + h);
"#,
    ));
}

/// FOUR pinned receivers, three recycled and one clean, across three element
/// sources and one string — the parse-large-js mix loop's receiver set.
#[test]
fn msplit_parity_four_receivers_with_a_string() {
    assert_matches_node(&prog(
        r#"var kinds = [], starts = [], ends = [];
var src = "";
for (var i = 0; i < 64; i++) src += "abcdefghijklmnopqrstuvwxyz0123456789 ";
for (var i = 0; i < N; i++) { kinds.push(i % 13); starts.push(i % 2000); ends.push((i % 2000) + 3); }
var h = 0;
function f(n) {
  for (var ti = 0; ti < n; ti++) {
    h = (h ^ kinds[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
    h = (h ^ (ends[ti] - starts[ti])) | 0; h = Math.imul(h, 16777619) >>> 0;
    h = (h ^ src.charCodeAt(starts[ti])) | 0; h = Math.imul(h, 16777619) >>> 0;
  }
}
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("four h=" + h);
"#,
    ));
}

/// FIVE recycled receivers — one past `MULTI_SPLIT_BUDGET`. The budget must
/// DECLINE the region to the memory tier, not silently plan a receiver it will
/// not write through (the whole-region veto is the same one B94 shipped with).
#[test]
fn msplit_parity_five_receivers_exceed_the_budget() {
    assert_matches_node(&prog(
        r#"var a = [], b = [], c = [], d = [], e = [];
for (var i = 0; i < N; i++) { a.push(i % 13); b.push(i % 97); c.push(i % 31); d.push(i % 7); e.push(i % 5); }
var h = 0;
function f(n) {
  for (var ti = 0; ti < n; ti++) {
    h = (h ^ a[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
    h = (h ^ b[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
    h = (h ^ c[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
    h = (h ^ d[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
    h = (h ^ e[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
  }
}
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("five h=" + h);
"#,
    ));
}

/// A deopt forced mid-loop on EACH split receiver in turn (a double element in
/// one array, a string element in the other), so `flush_exit` runs with two
/// write-through registers live and the interpreter has to find both their
/// memory slots current.
#[test]
fn msplit_parity_deopt_on_a_non_int_element() {
    assert_matches_node(&prog(
        r#"var a = [], b = [];
for (var i = 0; i < N; i++) { a.push(i % 13); b.push(i % 97); }
a[N - 7] = 2.5;
b[N - 11] = "x";
var h = 0;
function f(n) {
  for (var ti = 0; ti < n; ti++) {
    h = (h ^ a[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
    h = (h ^ b[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
  }
}
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("deopt h=" + h);
"#,
    ));
}

/// The index leaving range on one of two split receivers (`b[ti + 5]` runs off
/// the end): the bounds guard deopts every iteration near the tail.
#[test]
fn msplit_parity_index_out_of_range() {
    assert_matches_node(&prog(
        r#"var a = [], b = [];
for (var i = 0; i < N; i++) { a.push(i % 13); b.push(i % 97); }
var h = 0;
function f(n) {
  for (var ti = 0; ti < n; ti++) {
    h = (h ^ a[ti]) | 0; h = Math.imul(h, 16777619) >>> 0;
    h = (h ^ b[ti + 5]) | 0; h = Math.imul(h, 16777619) >>> 0;
  }
}
for (var r = 0; r < 5; r++) { h = 2166136261; f(N); }
console.log("oob h=" + h);
"#,
    ));
}

/// The split STRING receiver reassigned between OSR entries: ASCII → non-ASCII
/// (the pin snapshot declines, so the entry guard must bail) → a shorter ASCII
/// string (charCodeAt runs off the end and returns NaN, which no i64 home can
/// hold, so every tail iteration deopts).
#[test]
fn msplit_parity_string_receiver_swapped_between_entries() {
    assert_matches_node(&prog(
        r#"var starts = [];
var src = "abcdefghijklmnopqrstuvwxyz0123456789 ";
for (var i = 0; i < N; i++) starts.push(i % 30);
var h = 0;
function f(n) {
  for (var ti = 0; ti < n; ti++) {
    h = (h ^ src.charCodeAt(starts[ti])) | 0; h = Math.imul(h, 16777619) >>> 0;
  }
}
var out = [];
for (var r = 0; r < 4; r++) { h = 2166136261; f(N); out.push(h); }
src = "héllo wörld ☃ non-ascii";
for (var r = 0; r < 4; r++) { h = 2166136261; f(N); out.push(h); }
src = "short";
for (var r = 0; r < 4; r++) { h = 2166136261; f(N); out.push(h); }
console.log("strswap " + out.join(","));
"#,
    ));
}

/// A split ARRAY receiver rebound between entries — replaced by a shorter
/// array, then by one carrying a double, then by a non-array. Each rebinding
/// must be caught by the pin's identity/validity guard rather than read
/// through the stale snapshot the split's memory slot points at.
#[test]
fn msplit_parity_array_receiver_swapped_between_entries() {
    assert_matches_node(&prog(
        r#"var a = [], b = [];
for (var i = 0; i < N; i++) { a.push(i % 13); b.push(i % 97); }
var h = 0;
function f(n) {
  for (var ti = 0; ti < n; ti++) {
    h = (h ^ a[ti % a.length]) | 0; h = Math.imul(h, 16777619) >>> 0;
    h = (h ^ b[ti % b.length]) | 0; h = Math.imul(h, 16777619) >>> 0;
  }
}
var out = [];
for (var r = 0; r < 4; r++) { h = 2166136261; f(N); out.push(h); }
b = b.slice(0, 64);
for (var r = 0; r < 4; r++) { h = 2166136261; f(N); out.push(h); }
b[7] = 0.5;
for (var r = 0; r < 4; r++) { h = 2166136261; f(N); out.push(h); }
console.log("arrswap " + out.join(","));
"#,
    ));
}

/// Every case above must answer identically in every mode.
#[test]
fn msplit_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 6] = [
        &[("ZIPP_NO_MULTI_SPLIT", "1")],
        &[("ZIPP_INT_SPLIT", "1")],
        &[("ZIPP_NO_GPR_SPLIT", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("msplit_parity_");
        for (key, val) in mode {
            cmd.env(key, val);
        }
        let out = cmd.output().expect("spawn the test binary");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{mode:?} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the msplit_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}

fn jitlog_of(test_name: &str, env: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(test_name)
        .arg("--exact")
        .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JITDECLINE", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn the test binary");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "{test_name} child failed:\n{}\n{stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    stderr
}

fn splits(log: &str) -> usize {
    log.lines()
        .filter(|l| l.contains("B94 split receiver"))
        .count()
}

fn has_pinned_receiver_decline(log: &str) -> bool {
    log.lines()
        .any(|line| line.contains("pinned receiver r") && line.contains("not cleanly excludable"))
}

/// The mechanism itself: two receivers split in one region, the kernel on the
/// integer tier. With the switch off the SAME program must decline the plan
/// entirely (`plan_region=None`) and land on MEM — which is what makes this an
/// off-switch and not a no-op.
#[test]
fn msplit_mechanism_two_receivers_split_and_reach_the_int_tier() {
    let name = "msplit_parity_two_element_receivers";
    let on = jitlog_of(name, &[]);
    assert!(
        splits(&on) >= 2,
        "{name}: fewer than two B94 split receivers — the multi-split did \
         not engage:\n{on}"
    );
    assert!(
        on.contains("INT region fn1 ["),
        "{name}: the kernel is not on the integer tier:\n{on}"
    );

    let off = jitlog_of(name, &[("ZIPP_NO_MULTI_SPLIT", "1")]);
    assert!(
        splits(&off) <= 1,
        "{name}: ZIPP_NO_MULTI_SPLIT=1 still split more than one \
         receiver:\n{off}"
    );
    assert!(
        off.contains("plan_region=None"),
        "{name}: with the switch off the region should decline the plan; \
         if it no longer does, this pin has stopped measuring the \
         switch:\n{off}"
    );
}

/// The pinned-STRING receiver specifically. Both pins (the string and the
/// index array) are hoisted in the compiled region. The fixture's bytecode
/// deliberately recycles `src` in r10, which must split, while `starts` in r12
/// has one `LoadGlobal` and remains an ordinary pinned receiver.
///
/// The `pinned receiver reg not cleanly excludable` line is still expected in
/// the ON log: the FIRST plan attempt runs with `admit_split=false` (the B94
/// xmm refutation keeps `int_split_enabled` off), declines, and only then does
/// the W8 GPR-split retry plan the splits. The off-switch is pinned by the
/// tier, not by that message.
#[test]
fn msplit_mechanism_string_receiver_gate() {
    let name = "msplit_parity_string_and_element_receivers";
    let off = jitlog_of(name, &[("ZIPP_NO_MULTI_SPLIT", "1")]);
    assert!(
        has_pinned_receiver_decline(&off),
        "expected the pre-W14 decline with the switch off:\n{off}"
    );
    assert!(
        off.contains("MEM region fn1 ["),
        "with the switch off the string-receiver kernel must fall to MEM:\n{off}"
    );
    let on = jitlog_of(name, &[]);
    assert!(
        on.contains("guard-hoist pins=2/2"),
        "the compiled region should carry both the STRING and the ARRAY pin — \
         without the string pin this is not the arm being tested:\n{on}"
    );
    assert_eq!(
        splits(&on),
        1,
        "the recycled string receiver should be the fixture's sole split:\n{on}"
    );
    assert!(
        on.contains("B94 split receiver r10") && on.contains("pinned receiver r12"),
        "the string receiver must split while the element receiver remains a clean pin:\n{on}"
    );
    assert!(
        on.contains("INT region fn1 ["),
        "with the switch on the string-receiver region must reach the integer tier:\n{on}"
    );
}

/// The budget is a real ceiling: five recycled receivers must reach the
/// whole-region decline, not a partial plan.
#[test]
fn msplit_mechanism_budget_declines_past_four() {
    let log = jitlog_of("msplit_parity_five_receivers_exceed_the_budget", &[]);
    assert!(
        has_pinned_receiver_decline(&log),
        "five receivers should hit the whole-region decline:\n{log}"
    );
    assert!(
        log.contains("MEM region fn1 ["),
        "the over-budget kernel should land on the memory tier:\n{log}"
    );
}

/// The XMM integer emitter must carry every recycled receiver too; its
/// write-through / `flush_exit` implementation is separate from the GPR one.
/// `ZIPP_NO_GPR_HOMES=1` is load-bearing: otherwise the preferred GPR retry
/// wins before the XMM body is emitted. The current fn1 bytecode has three
/// recycled receivers and one clean pin.
#[test]
fn msplit_mechanism_xmm_emitter_carries_three_splits_and_a_clean_pin() {
    let log = jitlog_of(
        "msplit_parity_four_receivers_with_a_string",
        &[("ZIPP_INT_SPLIT", "1"), ("ZIPP_NO_GPR_HOMES", "1")],
    );
    let fn1_xmm_splits = log
        .lines()
        .filter(|line| line.contains("INT region [1,48] B94 split receiver"))
        .count();
    assert_eq!(
        fn1_xmm_splits, 3,
        "expected exactly three fn1 B94 splits on the XMM emitter:\n{log}"
    );
    assert!(
        log.contains("INT region [1,48] pinned receiver r16")
            && log.contains("INT region [1,48] guard-hoist pins=4/4"),
        "the fourth fn1 receiver must remain a clean, hoisted pin:\n{log}"
    );
    assert!(
        !log.contains("INT-GPR region [1,48]"),
        "the mechanism pin must execute the XMM emitter, not a GPR retry:\n{log}"
    );
    assert!(
        log.contains("INT region fn1 [1,48] compiled"),
        "the four-receiver fn1 kernel is not on the XMM integer tier:\n{log}"
    );
}
