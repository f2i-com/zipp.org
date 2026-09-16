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
//!
//! ── 07b400dc (2026-09-02), "Give booleans and global receivers their own
//! register classes" ── THE SHAPE B94 SPLITS MOVED. `Scopes::recv_expr` now
//! routes every receiver that is a plain global identifier into a RECV-class
//! register (`alloc_recv_reg`): provisional, above the ordinary stack, NEVER
//! reclaimed. A pinned global receiver therefore has exactly one definition,
//! `plan_region`'s `clean_global` rule admits it as an ordinary pin, and the
//! recycled-receiver shape these fixtures were written around cannot be
//! produced at all — not by rewriting the JS, because the recycling was the
//! allocator's, not the program's. That commit measured parse-large-js −38.9%
//! and typedarray-math −5.4%; parse-large-js is the very mix loop
//! `MULTI_SPLIT_BUDGET` was sized for, so the budget's motivating workload is
//! now served by not needing a split rather than by splitting four receivers.
//!
//! The mechanism is NOT dead: B94 still splits under the default lowering
//! wherever the receiver register is still recycled (the DataView route —
//! `split_recv_writethrough`'s `splitwt_mechanism_dv_oob_int_gpr` pins two live
//! splits), and the whole element/string arm remains the fallback the shipped
//! latch `ZIPP_NO_REG_CLASSES=1` restores. So the `msplit_mechanism_*` children
//! below run under that latch, where the shape exists and every count these
//! tests pin is exact; `msplit_mechanism_default_alloc_pins_receivers_without_splitting`
//! pins what the DEFAULT does instead, with counts of its own. Both halves are
//! falsifiable: the suite goes red if the split stops engaging under the latch,
//! and equally if the default stops pinning these receivers cleanly.

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
///
/// "Recycled" describes the allocation under `ZIPP_NO_REG_CLASSES=1`; since
/// 07b400dc (2026-09-02) the default gives each receiver its own never-reclaimed
/// RECV register and pins both cleanly. See the module header — this fixture is
/// checked against node in BOTH regimes by
/// `msplit_all_modes_answer_identically`.
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
///
/// That decline is the `ZIPP_NO_REG_CLASSES=1` behaviour. Under the default
/// since 07b400dc (2026-09-02) none of the five is recycled, so no budget is
/// spent and the region reaches INT-GPR with five hoisted pins —
/// `msplit_mechanism_default_alloc_pins_receivers_without_splitting` pins that
/// and `msplit_mechanism_budget_declines_past_four` pins this. See the header.
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
    let modes: [&[(&str, &str)]; 8] = [
        &[("ZIPP_NO_MULTI_SPLIT", "1")],
        &[("ZIPP_INT_SPLIT", "1")],
        &[("ZIPP_NO_GPR_SPLIT", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
        // 07b400dc's latch: the regime in which these fixtures still recycle a
        // global receiver's register, i.e. the one the `msplit_mechanism_*`
        // pins below measure. Without this row the B94 split plan would be
        // asserted about but never checked against node.
        &[("ZIPP_NO_REG_CLASSES", "1")],
        &[("ZIPP_NO_REG_CLASSES", "1"), ("ZIPP_INT_SPLIT", "1")],
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

/// The mechanism under study is the fused method-call lowering's split
/// receivers, and these fixtures keep their state in top-level `var`s (global
/// reads), which the strict default lowering (B280) captures rather than
/// fuses. The logged child therefore opts into the relaxed lowering; the
/// parity tests above still run under the default, so both lowerings stay
/// checked against node.
///
/// `legacy_alloc` selects the register-allocation regime, which is the axis
/// 07b400dc (2026-09-02) moved — see the module header. `true` sets the shipped
/// `ZIPP_NO_REG_CLASSES=1` latch, under which a global receiver's register is
/// still recycled and the B94 split is the plan; `false` is today's default,
/// where the receiver takes a never-reclaimed RECV register and is pinned
/// outright. `msplit_all_modes_answer_identically` runs the parity fixtures
/// under the latch too, so the split path stays checked against node.
fn jitlog_env(test_name: &str, env: &[(&str, &str)], legacy_alloc: bool) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(test_name)
        .arg("--exact")
        .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_JITDECLINE", "1")
        .env("ZIPP_RELAXED_CALL_ORDER", "1")
        .env_remove("ZIPP_STRICT_CALL_ORDER");
    if legacy_alloc {
        cmd.env("ZIPP_NO_REG_CLASSES", "1");
    } else {
        cmd.env_remove("ZIPP_NO_REG_CLASSES");
    }
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

/// The regime in which the recycled-receiver shape exists (see `jitlog_env`).
fn jitlog_of(test_name: &str, env: &[(&str, &str)]) -> String {
    jitlog_env(test_name, env, true)
}

/// Today's shipped lowering.
fn jitlog_default_alloc(test_name: &str, env: &[(&str, &str)]) -> String {
    jitlog_env(test_name, env, false)
}

fn splits(log: &str) -> usize {
    log.lines()
        .filter(|l| l.contains("B94 split receiver"))
        .count()
}

/// The span fn1's kernel compiled at, e.g. `"1,30"` from
/// `[jit] INT region fn1 [1,30] compiled`. Every count below is filtered to
/// fn1's own region: these fixtures also compile their script-scope build
/// loops, whose receivers would otherwise pad or mask the kernel's census.
fn fn1_span(log: &str) -> String {
    // Only the `... compiled` line, never `[jit] region fn1 [1] deopt at ip N`,
    // which also starts with `region fn1 [` but carries a start, not a span.
    let spans: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("] compiled"))
        .filter_map(|l| l.split("region fn1 [").nth(1)?.split(']').next())
        .collect();
    assert!(
        !spans.is_empty(),
        "no fn1 region compiled:\n{log}"
    );
    assert!(
        spans.windows(2).all(|w| w[0] == w[1]),
        "fn1 compiled more than one distinct region {spans:?}, so a single \
         span cannot filter its census:\n{log}"
    );
    spans[0].to_string()
}

/// One entry per `pinned receiver rN lg=[..]` line in fn1's region, holding
/// that pin's LoadGlobal count. A CLEAN pin has exactly ONE: the receiver
/// register is written once and never recycled, which is precisely what lets
/// `plan_region` exclude it without a B94 split.
fn fn1_pin_lg_counts(log: &str) -> Vec<usize> {
    let tag = format!("region [{}] pinned receiver r", fn1_span(log));
    log.lines()
        .filter_map(|l| l.split(&tag).nth(1))
        .map(|rest| {
            rest.split("lg=[")
                .nth(1)
                .and_then(|t| t.split(']').next())
                .map(|t| t.split(',').filter(|x| !x.trim().is_empty()).count())
                .unwrap_or(0)
        })
        .collect()
}

fn has_pinned_receiver_decline(log: &str) -> bool {
    log.lines()
        .any(|line| line.contains("pinned receiver r") && line.contains("not cleanly excludable"))
}

/// What the DEFAULT lowering does with these same fixtures since 07b400dc
/// (2026-09-02) — the other half of the pin, and the reason the tests below
/// may legitimately run under `ZIPP_NO_REG_CLASSES=1`.
///
/// Every receiver is now a CLEAN pin (one LoadGlobal, never recycled), so the
/// region needs no split at all, and the counts here are exact: the pin count
/// per fixture, one LoadGlobal each, all of them hoisted, ZERO splits, and no
/// `not cleanly excludable` decline anywhere. Two of the rows are outright
/// tier gains over the split era, which is why this is not a loosening:
///
///   * the FIVE-receiver fixture used to exceed `MULTI_SPLIT_BUDGET` and hit
///     the whole-region decline to MEM (`msplit_mechanism_budget_declines_past_four`
///     still pins that under the latch). It now compiles on INT-GPR with five
///     hoisted pins and SEVEN homes — the budget's ceiling is gone because the
///     thing it rationed is no longer spent.
///   * the two-receiver kernel plans 9 GPR homes here against 10 under the
///     latch: a split receiver costs a numeric home plus a boxed write-through
///     at every def, and a clean pin costs neither.
#[test]
fn msplit_mechanism_default_alloc_pins_receivers_without_splitting() {
    let cases: [(&str, &[(&str, &str)], usize); 4] = [
        ("msplit_parity_two_element_receivers", &[], 2),
        ("msplit_parity_string_and_element_receivers", &[], 2),
        ("msplit_parity_five_receivers_exceed_the_budget", &[], 5),
        // `starts` is the receiver of two distinct accesses here, and each
        // receiver SITE takes its own RECV register, so five pins cover the
        // four pinned objects (`guard-hoist pins=4/4` below).
        (
            "msplit_parity_four_receivers_with_a_string",
            &[("ZIPP_INT_SPLIT", "1"), ("ZIPP_NO_GPR_HOMES", "1")],
            5,
        ),
    ];
    for (name, env, pins) in cases {
        let log = jitlog_default_alloc(name, env);
        assert_eq!(
            splits(&log),
            0,
            "{name}: the default lowering should need no B94 split — a split \
             here means a global receiver's register is being recycled \
             again:\n{log}"
        );
        assert!(
            !has_pinned_receiver_decline(&log),
            "{name}: the default lowering should never reach the \
             not-cleanly-excludable decline:\n{log}"
        );
        let lg = fn1_pin_lg_counts(&log);
        assert_eq!(
            lg.len(),
            pins,
            "{name}: expected {pins} pinned receivers in fn1's region, got \
             {lg:?}\n{log}"
        );
        assert!(
            lg.iter().all(|&n| n == 1),
            "{name}: every pin must have exactly one LoadGlobal (that is what \
             makes it excludable without a split), got {lg:?}\n{log}"
        );
        assert!(
            log.contains("INT region fn1 ["),
            "{name}: the kernel is not on the integer tier:\n{log}"
        );
        assert!(
            !log.contains("MEM region fn1 ["),
            "{name}: the kernel dropped to the memory tier:\n{log}"
        );
    }

    // The two rows above that are tier/home gains, pinned as numbers so a
    // regression back to the split era cannot pass quietly.
    let five = jitlog_default_alloc("msplit_parity_five_receivers_exceed_the_budget", &[]);
    assert!(
        five.contains(&format!(
            "region [{}] guard-hoist pins=5/5",
            fn1_span(&five)
        )),
        "the five-receiver kernel must carry all five pins hoisted — it used \
         to exceed MULTI_SPLIT_BUDGET and decline to MEM:\n{five}"
    );
    assert!(
        five.contains("GPR homes engaged (7 homes, 0 lazy-sx)"),
        "the five-receiver kernel's home census moved:\n{five}"
    );
    let two = jitlog_default_alloc("msplit_parity_two_element_receivers", &[]);
    assert!(
        two.contains("GPR homes engaged (9 homes, 1 lazy-sx)"),
        "the two-receiver kernel plans 9 homes with clean pins and 10 with \
         split ones; this census moved:\n{two}"
    );
}

/// The mechanism itself: two receivers split in one region, the kernel on the
/// integer tier. With the switch off the SAME program must decline the plan
/// entirely (`plan_region=None`) and land on MEM — which is what makes this an
/// off-switch and not a no-op.
///
/// Runs under `ZIPP_NO_REG_CLASSES=1` (see `jitlog_env`): since 07b400dc
/// (2026-09-02) a global receiver takes a never-reclaimed RECV register, so the
/// recycled shape a split needs exists only under that latch. The default's own
/// census is pinned by
/// `msplit_mechanism_default_alloc_pins_receivers_without_splitting` above.
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
///
/// Under `ZIPP_NO_REG_CLASSES=1` (07b400dc, 2026-09-02 — see `jitlog_env`);
/// r10/r12 are that regime's numbering, and under the default this fixture's
/// two receivers are both clean RECV-register pins instead.
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
///
/// Under `ZIPP_NO_REG_CLASSES=1` (07b400dc, 2026-09-02 — see `jitlog_env`),
/// which is the only regime that still produces five RECYCLED receivers. Under
/// the default the same five are clean pins and the region reaches INT-GPR
/// instead, which
/// `msplit_mechanism_default_alloc_pins_receivers_without_splitting` pins.
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
///
/// Under `ZIPP_NO_REG_CLASSES=1` (07b400dc, 2026-09-02 — see `jitlog_env`);
/// r16 and the `[1,48]` span are that regime's layout. Under the default the
/// same span carries five clean pins and no split, pinned above.
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
