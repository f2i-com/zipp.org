//! The dense back-edge verdict must not change any answer.
//!
//! Every backward `Jump` used to pay `try_run_osr` (a `get_region` FxHashMap
//! probe) and, on miss, `record_region` (a `region_blacklist` FxHashSet probe)
//! — per iteration, forever, even for a loop the JIT permanently rejected.
//! `Jit::region_dead` now mirrors `region_blacklist` membership into a dense
//! per-func byte table, so a blacklisted loop pays two array reads instead
//! (the loop-region sibling of the FN_DEAD check at frame entry).
//!
//! The verdict is a pure memoization of an existing decision: it may change
//! HOW CHEAPLY a dead back-edge is skipped, never WHICH regions compile or
//! what any program prints. These tests run each program in the default mode
//! and under `ZIPP_NO_DENSE_BACKEDGE=1` (the old always-probe path) and assert
//! byte-identical output, plus agreement with node (v24) via `node -e`.

use std::sync::Mutex;

/// Serializes the `ZIPP_NO_DENSE_BACKEDGE` toggling: tests in this binary run
/// on parallel threads and the flag is process-global. (A race would not
/// change any ANSWER — the flag is perf-only — but keep the A/B runs clean.)
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

/// Run `src` with the dense verdict ON (default) and OFF
/// (`ZIPP_NO_DENSE_BACKEDGE=1` — read per-VM in `Jit::new`, so an in-process
/// toggle takes effect), assert the two agree, and return the output.
fn run_both_ways(src: &str) -> Vec<String> {
    let _g = ENV_LOCK.lock().unwrap();
    std::env::remove_var("ZIPP_NO_DENSE_BACKEDGE");
    let dense = run_ok(src);
    std::env::set_var("ZIPP_NO_DENSE_BACKEDGE", "1");
    let probed = run_ok(src);
    std::env::remove_var("ZIPP_NO_DENSE_BACKEDGE");
    assert_eq!(
        dense, probed,
        "dense back-edge verdict changed an answer for: {src}"
    );
    dense
}

/// The same program's output from `node -e` (node v24 expected on PATH), so
/// the expectations aren't hand-computed.
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

fn assert_matches_node_both_ways(src: &str) {
    let ours = run_both_ways(src);
    let node = node_output(src);
    assert_eq!(ours, node, "zipp != node for: {src}");
}

/// A hot integer loop: compiles (INT region) and OSR-enters, well past
/// `OSR_THRESHOLD` (8). The dense verdict must never engage here — the region
/// is COMPILED, not blacklisted — so this guards against a byte wrongly set
/// for a loop that tiers up.
#[test]
fn a_hot_compiled_loop_answers_the_same_both_ways() {
    assert_matches_node_both_ways(
        r#"
        var s = 0;
        for (var i = 0; i < 200000; i++) { s += i; }
        console.log(s);
        "#,
    );
}

/// A mixed program: a compiling loop, a permanently-blacklisted loop (a tiny
/// body dominated by a native-callee CallMethod — the call-mix gate declines
/// it, verified via ZIPP_JITLOG: "DECLINED (call-mix gate)"), a loop the
/// region compiler itself declines ("DECLINED (blacklisted)"), and a cold
/// loop that never reaches the threshold. Exercises COMPILED, both DEAD
/// entry points, and COUNTING side by side.
#[test]
fn a_mixed_program_answers_the_same_both_ways() {
    assert_matches_node_both_ways(
        r#"
        var s = 0;
        for (var i = 0; i < 100000; i++) { s += i; }

        var d = new Date(0);
        var t = 0;
        for (var j = 0; j < 100000; j++) { t += d.getTime(); }

        var last = "";
        for (var k = 0; k < 100000; k++) { last = JSON.stringify(k); }

        var c = 0;
        for (var m = 0; m < 4; m++) { c += m; }

        console.log(s + "," + t + "," + last + "," + c);
        "#,
    );
}

/// The blacklisted-loop shape on its own, long after the decline: every one
/// of its ~500k post-decline back-edges takes the dense fast path in default
/// mode and the two hash probes under ZIPP_NO_DENSE_BACKEDGE=1. The answers
/// must be identical, including the loop's own visible state afterwards.
#[test]
fn a_permanently_blacklisted_loop_answers_the_same_both_ways() {
    assert_matches_node_both_ways(
        r#"
        var out = "";
        for (var i = 0; i < 500000; i++) { out = String.fromCharCode(65 + (i % 26)); }
        console.log(out + "," + i);
        "#,
    );
}
