//! Pinned dense-Array `HasProp` absent lane: semantics, invalidation and proof.
//!
//! The optimized answer is valid only for a non-negative Int index under the
//! live default-chain protector. These fixtures deliberately invalidate each
//! part of that statement and re-run in JIT, no-JIT, GC-stress and kill-switch
//! child processes so environment latches cannot make the matrix vacuous.

use std::process::Command;

const SOURCE: &str = r#"
    "use strict";

    // Direct hole/OOB lane: 410 punched holes plus three positive OOB indices.
    var a = new Array(2048);
    for (var f = 0; f < a.length; f++) a[f] = f + 1;
    for (var d = 0; d < a.length; d += 5) delete a[d];
    var present = 0, absent = 0;
    for (var rep = 0; rep < 4; rep++) {
      present = 0; absent = 0;
      for (var i = 0; i < a.length + 3; i++) {
        if (i in a) present++; else absent++;
      }
    }
    console.log("plain=" + present + ":" + absent);

    // Negative and non-canonical numeric spellings are named properties. They
    // do not invalidate the indexed protector and therefore catch an over-broad
    // snapshot proof especially well.
    Array.prototype[-1] = "NEG";
    Array.prototype["01"] = "NC";
    var neg = 0, computedNeg = 0, noncanon = 0;
    for (var n = 0; n < 1000; n++) {
      if (-1 in a) neg++;
      var negKey = n - n - 1;
      if (negKey in a) computedNeg++;
      if ("01" in a) noncanon++;
    }
    console.log("named=" + neg + ":" + computedNeg + ":" + noncanon);
    delete Array.prototype[-1];
    delete Array.prototype["01"];

    // An own accessor lives in arr_props. `in` observes its presence without
    // invoking it; the Array snapshot must decline rather than reading the hole.
    var gets = 0, overlay = [,];
    Object.defineProperty(overlay, "0", {
      get: function () { gets++; return 7; }, configurable: true
    });
    var overlayHits = 0;
    for (var o = 0; o < 1000; o++) if (0 in overlay) overlayHits++;
    console.log("overlay=" + overlayHits + ":" + gets);

    // Reassign the pinned global through a native-cross-call candidate. The
    // identity guard/refetch must not apply the old array's absence proof.
    var reassigned = [,], replacement = [9];
    function swapAt(x) { if (x === 100) reassigned = replacement; }
    var reassignedHits = 0;
    for (var r = 0; r < 1000; r++) {
      swapAt(r);
      if (0 in reassigned) reassignedHits++;
    }
    console.log("reassigned=" + reassignedHits);

    // Relink the receiver itself through a cross-called function. Unlike a
    // mutation to the intrinsic anchors below this does not trip the global
    // protector; the receiver's live default-chain check must independently
    // clear the snapshot proof.
    var relinked = [,], relinkProto = {0: 13};
    function relinkAt(x) {
      if (x === 100) Object.setPrototypeOf(relinked, relinkProto);
    }
    var relinkedHits = 0;
    for (var q = 0; q < 1000; q++) {
      relinkAt(q);
      if (0 in relinked) relinkedHits++;
    }
    console.log("relinked=" + relinkedHits);

    // Mutate the prototype from a cross-called function after the region and
    // snapshot cache are hot. The getter must not run for `in`, but its presence
    // must be visible on the very next probe (900 iterations including x=100).
    var inheritedGets = 0, inherited = [,];
    function installAt(x) {
      if (x === 100) Object.defineProperty(Array.prototype, "0", {
        get: function () { inheritedGets++; return 11; }, configurable: true
      });
    }
    var inheritedHits = 0;
    for (var p = 0; p < 1000; p++) {
      installAt(p);
      if (0 in inherited) inheritedHits++;
    }
    console.log("inherited=" + inheritedHits + ":" + inheritedGets);
    delete Array.prototype[0];

    // Virtual length and sparse side-table indices never publish a dense pin.
    var sparse = [];
    sparse.length = 1048577;
    sparse[1048576] = 1;
    var sparseHit = 0, sparseMiss = 0;
    for (var s = 0; s < 1000; s++) {
      if (1048576 in sparse) sparseHit++;
      if (1048575 in sparse) sparseMiss++;
    }
    console.log("sparse=" + sparseHit + ":" + sparseMiss + ":" + sparse.length);
"#;

const EXPECTED: &[&str] = &[
    "plain=1638:413",
    "named=1000:1000:1000",
    "overlay=1000:0",
    "reassigned=900",
    "relinked=900",
    "inherited=900:0",
    "sparse=1000:0:1048577",
];

fn run_zipp() -> Vec<String> {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    out.output
}

fn node_output() -> Vec<String> {
    let out = Command::new("node")
        .args(["-e", SOURCE])
        .output()
        .expect("node is available");
    assert!(
        out.status.success(),
        "node failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect()
}

#[test]
fn hasprop_pin_absent_child() {
    let out = run_zipp();
    assert_eq!(out, EXPECTED);
    if std::env::var_os("ZIPP_HASPROP_ABSENT_PROBE").is_some() {
        println!(
            "HASPROP_PIN_ABSENT_HITS={}",
            zipp_vm::hasprop_pin_absent_stats()
        );
    }
}

fn run_child(env: &[(&str, &str)]) -> std::process::Output {
    let exe = std::env::current_exe().expect("test executable path");
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", "hasprop_pin_absent_child", "--nocapture"])
        .env("ZIPP_HASPROP_ABSENT_MODE_CHILD", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_JIT_THRESHOLD")
        .env_remove("ZIPP_GC_STRESS")
        .env_remove("ZIPP_NO_HASPROP_PIN_ABSENT")
        .env_remove("ZIPP_ICSTATS")
        .env_remove("ZIPP_HASPROP_ABSENT_PROBE")
        .env_remove("ZIPP_JITLOG");
    for &(key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("mode child runs")
}

#[test]
fn semantics_match_node_and_all_execution_modes() {
    let direct = run_zipp();
    assert_eq!(direct, EXPECTED);
    assert_eq!(direct, node_output());
    if std::env::var_os("ZIPP_HASPROP_ABSENT_MODE_CHILD").is_some() {
        return;
    }
    for env in [
        vec![("ZIPP_JIT_THRESHOLD", "1")],
        vec![("ZIPP_NO_HASPROP_PIN_ABSENT", "1")],
        vec![("ZIPP_NOJIT", "1")],
        vec![("ZIPP_JIT_THRESHOLD", "1"), ("ZIPP_GC_STRESS", "1")],
        vec![("ZIPP_NO_HASPROP_PIN_ABSENT", "1"), ("ZIPP_GC_STRESS", "1")],
    ] {
        let child = run_child(&env);
        assert!(
            child.status.success(),
            "mode {env:?} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn probe_hits(disabled: bool) -> (u64, String) {
    let mut env = vec![
        ("ZIPP_HASPROP_ABSENT_PROBE", "1"),
        ("ZIPP_ICSTATS", "1"),
        ("ZIPP_JITLOG", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
    ];
    if disabled {
        env.push(("ZIPP_NO_HASPROP_PIN_ABSENT", "1"));
    }
    let child = run_child(&env);
    assert!(
        child.status.success(),
        "probe disabled={disabled} failed:\n{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    let stdout = String::from_utf8_lossy(&child.stdout);
    let hits = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("HASPROP_PIN_ABSENT_HITS="))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("missing counter in:\n{stdout}"));
    (hits, String::from_utf8_lossy(&child.stderr).into_owned())
}

#[test]
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
fn mechanism_counter_and_kill_switch_prove_the_native_absent_path() {
    let (enabled, enabled_log) = probe_hits(false);
    let (disabled, disabled_log) = probe_hits(true);
    assert!(
        enabled > 1_000,
        "native absent lane did not serve the hole scan: {enabled}\n{enabled_log}"
    );
    assert_eq!(disabled, 0, "kill switch still counted native answers");
    assert!(enabled_log.contains("MEM pinned HasProp absent lane emitted"));
    assert!(!disabled_log.contains("MEM pinned HasProp absent lane emitted"));
}
