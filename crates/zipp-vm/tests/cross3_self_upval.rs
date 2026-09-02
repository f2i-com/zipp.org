//! B270: a Tier-C body reaches ITSELF through a captured cell (a function
//! declaration inside an IIFE or module scope calling itself) or a global.
//!
//! Two gaps kept such a site off the native lanes for the life of the program:
//! the plan for a body's own recursive site always found its cross entry
//! missing (the plan builds before the compile installs it, and the pending
//! retry deliberately skips self sites), and a site whose IC was still EMPTY at
//! compile time — the second recursive call, never executed during the first
//! descent — got no cross plan at all. `fib(32)` written inside an IIFE ran 5x
//! slower than the same function declared at top level (245 ms against 47 ms,
//! Node 77 ms). The planner now resolves an empty-IC callee from the callee
//! register's `UpvalGet`/`LoadGlobal` definition in the live exemplar frame and
//! bakes a CROSS3 arm against the entry the compile is about to install, with
//! the mask generation `set_cross_entry` will leave.
//!
//! Each program runs in a CHILD process (this test binary re-invoked on one of
//! the `*_child` tests below) under `ZIPP_JITLOG=1`, so the parent can assert
//! the lanes were planned, not merely that the answer is right, and so every
//! process-latched switch is read fresh. Expected values are node-oracled
//! (v24.12.0).
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const STRICT_UPVAL: &str = r#"
"use strict";
(function main() {
  function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
  console.log(fib(22));
})();
"#;

// The cell is REASSIGNED after the lanes are hot: every later recursive call
// resolves to the arrow, so the baked fid guard must miss and the call must
// take the generic route with the live callee. 300 * 50 + (1 + 1000) = 16001.
const REASSIGNED_CELL: &str = r#"
"use strict";
(function main() {
  let f = function (n) { return n <= 0 ? 0 : 1 + f(n - 1); };
  const g = f;
  let acc = 0;
  for (let i = 0; i < 300; i++) acc += g(50);
  f = (n) => 1000;
  acc += g(5);
  console.log(acc);
})();
"#;

// Sloppy self-recursion: the emitted lane admits only strict or arrow callees,
// so this must stay correct through the helper route (the site still gains a
// plan from its definition).
const SLOPPY_UPVAL: &str = r#"
(function main() {
  function depth(n) { return n === 0 ? 0 : 1 + depth(n - 1); }
  var total = 0;
  for (var i = 0; i < 200; i++) total += depth(40);
  console.log(total);
})();
"#;

const CHILD_ENV: &str = "ZIPP_CROSS3_SELF_CHILD";

fn run_program(source: &str, expected: &str) {
    let out = zipp_vm::run(source).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, vec![expected.to_string()]);
}

#[test]
fn strict_upval_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    run_program(STRICT_UPVAL, "17711");
}

#[test]
fn reassigned_cell_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    run_program(REASSIGNED_CELL, "16001");
}

#[test]
fn sloppy_upval_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    run_program(SLOPPY_UPVAL, "8000");
}

const LATCHES: [&str; 6] = [
    "ZIPP_NO_CROSS3_SELF",
    "ZIPP_NO_CROSS_DEF_CALLEE",
    "ZIPP_NOJIT",
    "ZIPP_JIT_THRESHOLD",
    "ZIPP_NO_NURSERY",
    "ZIPP_NO_CROSS3",
];

/// Run one `*_child` test in a fresh process with `ZIPP_JITLOG=1` plus `envs`;
/// returns the child's stderr (the JIT log) after asserting it passed.
fn child(test: &str, envs: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.args([test, "--exact", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("ZIPP_JITLOG", "1")
        // The latched-off and interpreter modes run the recursion through the
        // framed route; in an unoptimized test binary each interpreter level
        // is a large Rust frame, so give the child's test thread real room.
        .env("RUST_MIN_STACK", "268435456");
    for latch in LATCHES {
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
    stderr
}

#[test]
fn strict_upvalue_recursion_gets_a_self_arm_on_both_sites() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let log = child("strict_upval_child", &[]);
    assert!(
        log.contains("callee resolved from its definition"),
        "the empty-IC second site must be resolved from its UpvalGet:\n{log}"
    );
    let self_arms = log
        .lines()
        .filter(|l| l.contains("CROSS3 self arm against the entry this compile installs"))
        .count();
    assert!(
        self_arms >= 2,
        "both recursive sites must bake a self arm, saw {self_arms}:\n{log}"
    );
}

#[test]
fn latches_restore_the_declines_with_the_same_answer() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let log = child(
        "strict_upval_child",
        &[
            ("ZIPP_NO_CROSS3_SELF", "1"),
            ("ZIPP_NO_CROSS_DEF_CALLEE", "1"),
        ],
    );
    assert!(
        !log.contains("self arm") && !log.contains("resolved from its definition"),
        "latches must disable both mechanisms:\n{log}"
    );
    assert!(
        log.contains("CROSS3 decline: no entry yet for callee"),
        "with the self arm off the site declines as before:\n{log}"
    );
}

#[test]
fn guard_misses_and_sloppy_bodies_stay_correct_in_every_mode() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    for envs in [
        vec![],
        vec![("ZIPP_NO_CROSS3_SELF", "1")],
        vec![("ZIPP_NO_CROSS_DEF_CALLEE", "1")],
        vec![("ZIPP_NO_CROSS3", "1")],
        vec![("ZIPP_NOJIT", "1")],
        vec![("ZIPP_JIT_THRESHOLD", "1")],
        vec![("ZIPP_NO_NURSERY", "1")],
    ] {
        child("strict_upval_child", &envs);
        child("reassigned_cell_child", &envs);
        child("sloppy_upval_child", &envs);
    }
}
