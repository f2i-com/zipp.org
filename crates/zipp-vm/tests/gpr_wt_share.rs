//! B97 write-through home sharing on a GPR-only re-plan — default-on since W18,
//! `ZIPP_NO_GPR_WT_SHARE=1` restores the pre-W17 plans byte-for-byte.
//!
//! A register that is textually READ OUTSIDE the region used to pin a
//! whole-region ("permanent") home on every `region_int` plan, because the INT
//! tier passed `admit_wt_share=false`. On the flattened `parse-large-js` mix
//! loop that is fatal: the loop body lives at the TOP LEVEL, where the bytecode
//! compiler recycles registers across the script's phases, so the copies of
//! `ti` the three call sites load are each "read outside" — six of the eleven
//! planned homes were permanent, against a GPR pool of 8 (+2 when the guard
//! constants are inlined). The region ran on the xmm integer emitter instead,
//! paying three xmm↔gpr transfers for every `Bitwise` and `Math.imul` — nine
//! such ops per iteration.
//!
//! The licence B97 sharing needs is not "which tier" but "does THIS plan reach
//! only an emitter that implements the write-through". `region_int`'s
//! `share_homes` re-plans do: each hands its plan to `compile_region_int_gpr`
//! and to nothing else, and the xmm fallback below them keeps the ORIGINAL
//! distinct-homes plan. See `gpr_wt_share_enabled` for the whole contract.
//!
//! Every `gprwt_parity_*` case asserts byte-identical output against `node` —
//! the register allocator is the thing under test, so an intra-engine
//! comparison could not catch a shared miscompile. The `gprwt_mechanism_*`
//! cases read the plan back out of a child's `ZIPP_JITLOG`: a parity case that
//! quietly stopped reaching the GPR emitter would be testing nothing at all.
//!
//! W18: the mechanism is now DEFAULT-ON. W17 had to ship it dark, because
//! releasing the whole-region pins made a separate, pre-existing conditional-def
//! defect reachable on programs that had not reached it before (a local whose
//! only in-region def sits on a branch lost its entry load). That defect is
//! closed — `plan_region::region_liveness` now derives the region's true live-in
//! set and `shareable` asks it instead of `first_seen`, so an unfilled home is
//! no longer reachable at all; `tests/conditional_def_live_in.rs` is its gate.
//! The mechanism pins below therefore read the DEFAULT build for the ON side
//! and `ZIPP_NO_GPR_WT_SHARE=1` for the OFF side, which is what every other
//! mechanism in this suite does.

const PRELUDE: &str = r#""use strict";
var N = 30000;
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

/// The oracle. Fed on STDIN rather than `-e`: node evaluates both in global
/// SCRIPT scope, which is what zipp's own top level is — and top level is
/// exactly where this mechanism lives.
fn node_output(src: &str) -> Vec<String> {
    use std::io::Write;
    let mut child = std::process::Command::new("node")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("node v24 on PATH (expected values come from node)");
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(src.as_bytes())
        .expect("write to node");
    let out = child.wait_with_output().expect("node exits");
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

/// THE target shape: the `parse-large-js` mix loop as the benchmark actually
/// writes it — at TOP LEVEL, so `ti` and `h` are globals and the temps that
/// hold them are recycled by the enclosing script.
#[test]
fn gprwt_parity_top_level_mix_loop() {
    assert_matches_node(&prog(
        r#"var kinds = [], starts = [], ends = [];
var src = "";
for (var i = 0; i < 64; i++) src += "abcdefghijklmnopqrstuvwxyz0123456789 ";
for (var i = 0; i < N; i++) { kinds.push(i % 13); starts.push(i % 2000); ends.push((i % 2000) + 3); }
var h = 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
for (var r = 0; r < 5; r++) {
  h = 2166136261;
  for (var ti = 0; ti < kinds.length; ti++) {
    mix(kinds[ti]); mix(ends[ti] - starts[ti]); mix(src.charCodeAt(starts[ti]));
  }
}
console.log("mix h=" + h);
"#,
    ));
}

/// The hazard the write-through exists for: a shared home is flushed at EVERY
/// exit, so a register whose value still matters afterwards would come back
/// holding an unrelated temp. Here the same top-level loop is followed by a
/// second phase that reads the very registers the loop body recycled — and the
/// loop is re-entered by an OUTER loop, so the region is OSR-entered, exited
/// and re-entered many times with the second phase running in between.
#[test]
fn gprwt_parity_recycled_temps_read_after_the_loop() {
    assert_matches_node(&prog(
        r#"var kinds = [], starts = [], ends = [];
var src = "";
for (var i = 0; i < 64; i++) src += "abcdefghijklmnopqrstuvwxyz0123456789 ";
for (var i = 0; i < N; i++) { kinds.push(i % 13); starts.push(i % 2000); ends.push((i % 2000) + 3); }
var h = 0, tail = 0, k = 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
for (var r = 0; r < 6; r++) {
  h = 2166136261;
  for (var ti = 0; ti < kinds.length; ti++) {
    mix(kinds[ti]); mix(ends[ti] - starts[ti]); mix(src.charCodeAt(starts[ti]));
  }
  // A second phase over the SAME temps the loop body used, so a stale flush
  // into any of their slots would be read here rather than being harmlessly
  // overwritten first.
  for (k = 0; k + 2 < kinds.length; k += 977) {
    tail = (tail + kinds[k] + (ends[k + 1] - starts[k + 1]) + src.charCodeAt(starts[k + 2])) | 0;
  }
}
console.log("tail h=" + h + " tail=" + tail + " k=" + k);
"#,
    ));
}

/// The mechanism fixture is intentionally a pure-result sibling of the real
/// parse-style helper above. Returning `h` and computing the third input from
/// the loop counter keep the flattened body entirely numeric and isolate the
/// write-through home allocator from the pinned-string/replayable-prefix plan.
#[test]
fn gprwt_parity_flattenable_pure_mix_loop() {
    assert_matches_node(&prog(
        r#"var kinds = [], starts = [], ends = [];
var src = "";
for (var i = 0; i < 64; i++) src += "abcdefghijklmnopqrstuvwxyz0123456789 ";
for (var i = 0; i < N; i++) { kinds.push(i % 13); starts.push(i % 2000); ends.push((i % 2000) + 3); }
var h = 0, tail = 0, k = 0;
function mixPure(a, x) { return Math.imul(a ^ x, 16777619) >>> 0; }
for (var r = 0; r < 5; r++) {
  h = 2166136261;
  for (var ti = 0; ti < kinds.length; ti++) {
    h = mixPure(h, kinds[ti]); h = mixPure(h, ends[ti] - starts[ti]); h = mixPure(h, ti);
  }
}
for (k = 0; k + 2 < kinds.length; k += 977) {
  tail = (tail + kinds[k] + (ends[k + 1] - starts[k + 1]) + k) | 0;
}
console.log("pure mix h=" + h + " tail=" + tail + " k=" + k);
"#,
    ));
}

/// Dedicated mechanism fixture: its three compact numeric callees flatten
/// into a region that still fits the physical XMM planner, then overflows the
/// smaller GPR pool. With glob-range narrowing disabled, only B97's proven
/// write-through sharing makes the GPR-only retry fit.
#[test]
fn gprwt_fixture_flattened_b97_probe() {
    assert_matches_node(&prog(
        r#"var xs = [];
for (var i = 0; i < N; i++) xs.push(i % 13);
var h = 0, tail = 0, k = 0;
function step(a, x) { return (((a ^ x) + 17) | 0); }
for (var r = 0; r < 5; r++) {
  h = 2166136261;
  for (var ti = 0; ti < xs.length; ti++) {
    h = step(h, xs[ti]); h = step(h, ti); h = step(h, ti & 7);
  }
}
for (k = 0; k + 2 < xs.length; k += 977) tail = (tail + xs[k]) | 0;
console.log("b97 h=" + h + " tail=" + tail + " k=" + k);
"#,
    ));
}

/// The same flattenable shape followed by reads of recycled top-level temps.
/// This is the exit-flush hazard B97's write-through sharing must preserve.
#[test]
fn gprwt_parity_flattenable_recycled_temps_read_after_the_loop() {
    assert_matches_node(&prog(
        r#"var kinds = [], starts = [], ends = [];
var src = "";
for (var i = 0; i < 64; i++) src += "abcdefghijklmnopqrstuvwxyz0123456789 ";
for (var i = 0; i < N; i++) { kinds.push(i % 13); starts.push(i % 2000); ends.push((i % 2000) + 3); }
var h = 0, tail = 0, k = 0;
function mixPure(a, x) { return Math.imul(a ^ x, 16777619) >>> 0; }
for (var r = 0; r < 6; r++) {
  h = 2166136261;
  for (var ti = 0; ti < kinds.length; ti++) {
    h = mixPure(h, kinds[ti]); h = mixPure(h, ends[ti] - starts[ti]); h = mixPure(h, ti);
  }
  for (k = 0; k + 2 < kinds.length; k += 977) {
    tail = (tail + kinds[k] + (ends[k + 1] - starts[k + 1]) + k) | 0;
  }
}
console.log("pure tail h=" + h + " tail=" + tail + " k=" + k);
"#,
    ));
}

/// ZERO body iterations after the region compiles. The region is OSR-entered
/// on the back edge of the LAST trip, runs no body at all, and flushes anyway
/// — the exact shape that made blanket home unification a silent wrong answer
/// (`for (i=0;i<8;i++) s = i;` returned 8 instead of 7). Every trip count here
/// is short, so most entries flush without a single def.
#[test]
fn gprwt_parity_zero_body_iterations_on_entry() {
    assert_matches_node(&prog(
        r#"var kinds = [], starts = [], ends = [];
var src = "";
for (var i = 0; i < 64; i++) src += "abcdefghijklmnopqrstuvwxyz0123456789 ";
for (var i = 0; i < 40; i++) { kinds.push(i % 13); starts.push(i % 37); ends.push((i % 37) + 3); }
var h = 0, n = 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
for (var r = 0; r < N; r++) {
  n = (r % 5);
  for (var ti = 0; ti < n; ti++) {
    mix(kinds[ti]); mix(ends[ti] - starts[ti]); mix(src.charCodeAt(starts[ti]));
  }
}
console.log("zero h=" + h + " n=" + n + " ti=" + ti);
"#,
    ));
}

/// A mid-body DEOPT: the pinned dense-Int array is mutated to hold a double
/// part-way through, so the region bails at a recorded ip with the interpreter
/// resuming inside the flattened span. The flush that precedes it writes every
/// shared home; the write-through is what keeps the recycled temps' slots
/// truthful across it.
#[test]
fn gprwt_parity_deopt_midway_through_the_body() {
    assert_matches_node(&prog(
        r#"var kinds = [], starts = [], ends = [];
var src = "";
for (var i = 0; i < 64; i++) src += "abcdefghijklmnopqrstuvwxyz0123456789 ";
for (var i = 0; i < N; i++) { kinds.push(i % 13); starts.push(i % 2000); ends.push((i % 2000) + 3); }
var h = 0, acc = 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
for (var r = 0; r < 4; r++) {
  h = 2166136261;
  for (var ti = 0; ti < kinds.length; ti++) {
    mix(kinds[ti]); mix(ends[ti] - starts[ti]); mix(src.charCodeAt(starts[ti]));
  }
  acc = (acc + h) | 0;
  ends[(N >> 1) + r] = 0.5;   // the pin's element type changes under the region
}
console.log("deopt h=" + h + " acc=" + acc);
"#,
    ));
}

/// The spliced callee's global is READ between the calls, so the home the
/// shared plan gives it has to hold the current value at every point, not just
/// at the exits.
#[test]
fn gprwt_parity_stored_global_read_between_the_calls() {
    assert_matches_node(&prog(
        r#"var kinds = [], starts = [], ends = [];
var src = "";
for (var i = 0; i < 64; i++) src += "abcdefghijklmnopqrstuvwxyz0123456789 ";
for (var i = 0; i < N; i++) { kinds.push(i % 13); starts.push(i % 2000); ends.push((i % 2000) + 3); }
var h = 0, seen = 0;
function mix(x) { h = Math.imul(h ^ x, 16777619) >>> 0; }
for (var r = 0; r < 5; r++) {
  h = 2166136261;
  for (var ti = 0; ti < kinds.length; ti++) {
    mix(kinds[ti]);
    seen = (seen + (h & 15)) | 0;
    mix(ends[ti] - starts[ti]);
    mix(src.charCodeAt(starts[ti]) ^ (h & 7));
  }
}
console.log("seen h=" + h + " seen=" + seen);
"#,
    ));
}

/// B97's mechanism is isolated with glob-range narrowing disabled. The default
/// benchmark-style fixtures above remain parity coverage only: their rewritten
/// regions no longer claim to engage this allocator path.
#[test]
fn gprwt_mechanism_no_glob_range_rewritten_probe_reaches_gpr() {
    const NAME: &str = "gprwt_fixture_flattened_b97_probe";
    let on = jitlog_of(NAME, &[("ZIPP_NO_GLOB_RANGE", "1")]);
    let on_span = rewritten_splice_retry_span(&on);
    assert!(
        on.contains(&format!(
            "INT-GPR nest retry [{on_span}]: shared-home re-plan"
        )) && on.contains(&format!("INT region [{on_span}] GPR homes engaged")),
        "the no-glob-range rewritten region did not engage B97:\n{on}"
    );

    let off = jitlog_of(
        NAME,
        &[("ZIPP_NO_GLOB_RANGE", "1"), ("ZIPP_NO_GPR_WT_SHARE", "1")],
    );
    let off_span = rewritten_splice_retry_span(&off);
    assert_eq!(on_span, off_span, "ON/OFF selected different rewrites");
    assert!(
        gpr_declined_home_counts(&off, &off_span).len() >= 2
            && !off.contains(&format!("INT region [{off_span}] GPR homes engaged"))
            && off.contains(&format!("INT region [{off_span}] guard-hoist")),
        "the B97 off-switch did not overflow and fall back to XMM:\n{off}"
    );
}

/// The re-plan that engages is the SHARED-HOME one, and it engages because the
/// permanent-home pins were released — not because something else shrank the
/// body. Pin both halves: the first attempt overflows, the retry fits.
#[test]
fn gprwt_mechanism_the_shared_home_replan_is_what_fits() {
    const NAME: &str = "gprwt_fixture_flattened_b97_probe";
    let on = jitlog_of(NAME, &[("ZIPP_NO_GLOB_RANGE", "1")]);
    let span = rewritten_splice_retry_span(&on);
    let initial = *gpr_declined_home_counts(&on, &span)
        .first()
        .expect("the distinct-home attempt must overflow");
    let shared = gpr_engaged_home_count(&on, &span)
        .expect("the shared-home retry must engage the rewritten region");
    assert!(
        shared < initial,
        "B97 did not reduce rewritten-region homes ({initial} -> {shared}):\n{on}"
    );

    let off = jitlog_of(
        NAME,
        &[("ZIPP_NO_GLOB_RANGE", "1"), ("ZIPP_NO_GPR_WT_SHARE", "1")],
    );
    let off_span = rewritten_splice_retry_span(&off);
    let off_declines = gpr_declined_home_counts(&off, &off_span);
    assert!(
        off_span == span
            && off_declines.len() >= 2
            && off_declines.iter().all(|&homes| homes >= initial)
            && gpr_engaged_home_count(&off, &off_span).is_none(),
        "disabled B97 unexpectedly narrowed or engaged the rewrite:\n{off}"
    );
}

/// Every case above must answer identically in every mode.
#[test]
fn gprwt_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    let modes: [&[(&str, &str)]; 8] = [
        &[("ZIPP_NO_GPR_WT_SHARE", "1")],
        &[("ZIPP_NO_INT_SPLICE", "1")],
        &[("ZIPP_NO_GPR_HOMES", "1")],
        &[("ZIPP_NO_GLOB_RANGE", "1")],
        &[("ZIPP_NO_WT_SHARE", "1")],
        &[("ZIPP_JIT_THRESHOLD", "1")],
        &[("ZIPP_GC_STRESS", "1")],
        &[("ZIPP_NOJIT", "1")],
    ];
    for mode in modes {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("gprwt_parity_");
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
            "the gprwt_parity_ filter matched nothing under {mode:?}:\n{stdout}"
        );
    }
}

fn parse_span_after<'a>(line: &'a str, marker: &str) -> Option<(&'a str, usize, usize)> {
    let span = line.split_once(marker)?.1.split_once(']')?.0;
    let (start, end) = span.split_once(',')?;
    Some((span, start.parse().ok()?, end.parse().ok()?))
}

fn rewritten_splice_retry_span(log: &str) -> String {
    let (splice_at, splice_line) = log
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains("[jit] INT splice ["))
        .expect("the fixture's source region must flatten");
    let (_, _, source_end) = parse_span_after(splice_line, "INT splice [")
        .expect("the source splice log must carry a span");
    let ops = splice_line
        .rsplit_once(", ")
        .and_then(|(_, tail)| tail.strip_suffix(" ops"))
        .and_then(|n| n.parse::<usize>().ok())
        .expect("the source splice log must carry its rewritten op count");
    log.lines()
        .skip(splice_at + 1)
        .filter_map(|line| parse_span_after(line, "INT-GPR nest retry ["))
        .find(|(_, start, end)| *start > source_end && end - start + 1 == ops)
        .map(|(span, _, _)| span.to_string())
        .expect("a GPR retry whose span is exactly the rewritten splice")
}

fn gpr_declined_home_counts(log: &str, span: &str) -> Vec<usize> {
    let marker = format!("INT-GPR decline [{span}]: ");
    log.lines()
        .filter_map(|line| line.split_once(&marker).map(|(_, tail)| tail))
        .filter_map(|tail| tail.split_once(" homes >").map(|(n, _)| n))
        .filter_map(|n| n.parse().ok())
        .collect()
}

fn gpr_engaged_home_count(log: &str, span: &str) -> Option<usize> {
    let marker = format!("INT region [{span}] GPR homes engaged (");
    log.lines()
        .find_map(|line| line.split_once(&marker).map(|(_, tail)| tail))
        .and_then(|tail| tail.split_once(" homes").map(|(n, _)| n))
        .and_then(|n| n.parse().ok())
}

fn jitlog_of(test_name: &str, env: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test exe path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(test_name)
        .arg("--exact")
        .arg("--nocapture") // libtest swallows a PASSING child's stderr otherwise
        .env("ZIPP_JITLOG", "1");
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
