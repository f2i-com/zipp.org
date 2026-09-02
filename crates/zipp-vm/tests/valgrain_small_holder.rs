//! B273: the value-grain remembered set floats an OVERWRITTEN young value.
//!
//! Under value-grain recording (W10/B123) an old→young store records the
//! VALUE; the next minor marks every recorded value live whether or not its
//! slot was overwritten first. For the retained-append shape the design
//! measured (`survivors.push(x)`) that is exact, but for a fixed-size cache or
//! ring (`keep[i & 1023] = fresh`) every young object stored is promoted into
//! old space and reclaimed only by a major: 4M promotions, 15 majors, and 2.3x
//! the run time of the same loop without the store. Holders with at most
//! `ZIPP_VALGRAIN_SMALL_MAX` (default 4096) slots now take the holder-grain
//! barrier instead — one dirty entry per holder per epoch, re-traced exactly —
//! while large holders keep the value record.
//!
//! The assertions are collector shape (`gc_nursery_stats` counts update in
//! every mode) plus exact output; the children read the latch fresh.

const PROGRAM: &str = r#"
"use strict";
(function () {
  function make(v, k) { return { value: v, kind: k, left: v ^ 85, right: v + 3 }; }
  const n = 400000;
  let s = 0;
  const keep = new Array(1024);
  for (let i = 0; i < n; i++) { const o = make(i, i & 15); keep[i & 1023] = o; s = (s + o.value + o.left) | 0; }
  let live = 0;
  for (let i = 0; i < 1024; i++) live = (live + keep[i].value) | 0;
  console.log(s + ":" + live);
})();
"#;
// node v24.12.0
const EXPECTED: &str = "1085810048:409075200";

// A LARGE holder keeps the value grain: the retained-append shape must still
// survive minors and answer exactly (this is the case the value grain exists
// for; the small-holder rule must not touch it).
const LARGE: &str = r#"
"use strict";
(function () {
  const kept = [];
  let s = 0;
  for (let i = 0; i < 100000; i++) {
    const o = { v: i, w: i ^ 3 };
    kept.push(o);
    if (kept.length > 8192) kept.splice(0, 4096);
    s = (s + o.v) | 0;
  }
  let sum = 0;
  for (let i = 0; i < kept.length; i++) sum = (sum + kept[i].w) | 0;
  console.log(s + ":" + kept.length + ":" + sum);
})();
"#;
// node v24.12.0
const LARGE_EXPECTED: &str = "704982704:5792:562423472";

const CHILD_ENV: &str = "ZIPP_VALGRAIN_SMALL_CHILD";

fn run_and_report(source: &str, expected: &str) {
    let out = zipp_vm::run(source).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, vec![expected.to_string()]);
    let (minors, majors, _, _, swept_young, _, _, peak_slots) = zipp_vm::gc_nursery_stats();
    eprintln!("[nursery-test] minors {minors} majors {majors} swept_young {swept_young} peak_slots {peak_slots}");
}

#[test]
fn ring_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    run_and_report(PROGRAM, EXPECTED);
}

#[test]
fn large_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    run_and_report(LARGE, LARGE_EXPECTED);
}

/// `(minors, majors, swept_young, peak_slots)` reported by a child running
/// `test` under `envs`.
fn child(test: &str, envs: &[(&str, &str)]) -> (u64, u64, u64, u64) {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.args([test, "--exact", "--nocapture"]).env(CHILD_ENV, "1");
    for latch in [
        "ZIPP_VALGRAIN_SMALL_MAX",
        "ZIPP_NO_VALGRAIN_REMSET",
        "ZIPP_NO_NURSERY",
        "ZIPP_NOJIT",
        "ZIPP_GC_STRESS",
    ] {
        cmd.env_remove(latch);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn child");
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "{test} failed under {envs:?}:\n--- stdout ---\n{}\n--- stderr ---\n{stderr}",
        String::from_utf8_lossy(&out.stdout)
    );
    let line = stderr
        .lines()
        .find(|l| l.starts_with("[nursery-test]"))
        .unwrap_or_else(|| panic!("no nursery line in:\n{stderr}"));
    let nums: Vec<u64> = line
        .split_whitespace()
        .filter_map(|w| w.parse().ok())
        .collect();
    (nums[0], nums[1], nums[2], nums[3])
}

#[test]
fn an_overwritten_ring_no_longer_floats_into_majors() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let (minors, majors, swept_young, peak) = child("ring_child", &[]);
    assert!(minors >= 10, "the loop must run through real minors, saw {minors}");
    assert!(
        majors <= minors / 8 + 1,
        "small-holder barrier: the ring's overwritten values must die at their minor, saw minors {minors} majors {majors}"
    );
    // Pure value grain floats the ring's stores into old space: far fewer
    // young reclaims and a heap several times the size.
    let (_, _, swept_young_off, peak_off) = child("ring_child", &[("ZIPP_VALGRAIN_SMALL_MAX", "0")]);
    assert!(
        swept_young > 3 * swept_young_off && peak_off > 2 * peak,
        "pure value grain must show the float this rule removes: on swept_young {swept_young} peak {peak}, off swept_young {swept_young_off} peak {peak_off}"
    );
}

#[test]
fn large_holders_keep_the_value_grain_and_every_mode_agrees() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    for envs in [
        vec![],
        vec![("ZIPP_VALGRAIN_SMALL_MAX", "0")],
        vec![("ZIPP_VALGRAIN_SMALL_MAX", "1000000")],
        vec![("ZIPP_NO_VALGRAIN_REMSET", "1")],
        vec![("ZIPP_NO_NURSERY", "1")],
        vec![("ZIPP_NOJIT", "1")],
    ] {
        child("ring_child", &envs);
        child("large_child", &envs);
    }
}
