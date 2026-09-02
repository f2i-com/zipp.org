//! B272: a Tier-C body that carries handler ops (`try`, and the iterator-close
//! bracket every `for...of` compiles to) receives a FRAME-BACKED cross entry.
//!
//! Such bodies were statically denied a cross entry, because the emitted
//! `push_finally`/`pop_finally` helpers write the ACTIVE frame's handler stack
//! and a frame-free activation has none. Every call to such a function from
//! compiled code therefore took the framed interpreter route (`setup_call` →
//! `run_loop` → `dispatch_body` → `try_run_jit`): reactish-reconcile's `diff`
//! paid it ~130k times per run. The generic cross-call helper now pushes the
//! callee `Frame` first — the exact record its bail path materializes — enters
//! natively frame-backed, pops it on a clean return, and finishes a bail or a
//! throw over that frame the way the framed route does. Emitted lanes never
//! see such an entry (the native-visible slot stays null).
//!
//! The program below exercises every completion shape through the helper:
//! normal for...of completion, recursion, `break` (the bracket's normal exit),
//! a throw unwinding THROUGH the callee's frame to the caller's `try`, and a
//! throw caught by a `try` INSIDE the callee. Expected value is node-oracled
//! (v24.12.0). Each program runs in a child process so the latch and the
//! execution modes are read fresh.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const PROGRAM: &str = r#"
"use strict";
(function main() {
  function sumKeys(o) { let s = 0; for (const k of Object.keys(o)) s += o[k]; return s; }
  function count(node) { let n = 1; for (const c of node.kids) n += count(c); return n; }
  function firstBad(list) { for (const v of list) { if (v < 0) throw new RangeError("neg " + v); } return list.length; }
  function safeFirstBad(list) { try { return firstBad(list); } catch (e) { return -e.message.length; } }
  function firstOver(list, lim) { let idx = -1; for (const v of list) { idx++; if (v > lim) break; } return idx; }
  const tree = { kids: [{ kids: [{ kids: [] }, { kids: [] }] }, { kids: [{ kids: [] }] }] };
  let acc = 0;
  for (let i = 0; i < 3000; i++) {
    const o = { a: i, b: i & 7, c: 2 };
    acc = (acc + sumKeys(o)) | 0;
    acc = (acc + count(tree)) | 0;
    let caught = 0;
    try { acc = (acc + firstBad([1, 2, i & 3 ? 3 : -i])) | 0; } catch (e) { caught = e.message.length; }
    acc = (acc + caught + safeFirstBad([5, i & 1 ? -2 : 6, 7])) | 0;
    acc = (acc + firstOver([1, 4, 9, 16], i & 15)) | 0;
  }
  console.log(acc);
})();
"#;
const EXPECTED: &str = "4548088";

// Deep recursion through a handler body: the frame-backed entries must count
// toward MAX_FRAMES like any other frame, so the overflow is the ordinary
// catchable RangeError, never a native stack fault.
const DEEP: &str = r#"
"use strict";
(function main() {
  function down(n) { for (const _ of [0]) { return n === 0 ? 0 : 1 + down(n - 1); } }
  let ok = down(500);
  let overflow = "none";
  try { down(1e7); } catch (e) { overflow = e instanceof RangeError ? "RangeError" : "other"; }
  console.log(ok + ":" + overflow);
})();
"#;
const DEEP_EXPECTED: &str = "500:RangeError";

const CHILD_ENV: &str = "ZIPP_CROSS_FRAMED_CHILD";

fn run_program(source: &str, expected: &str) {
    let out = zipp_vm::run(source).expect("source compiles");
    assert!(out.error.is_none(), "{:?}", out.error);
    assert_eq!(out.output, vec![expected.to_string()]);
    // The CLI prints this line for `ZIPP_ICSTATS=1`; an in-process run has to
    // print it itself for the parent's `fills` parser.
    let (fast, full) = zipp_vm::cross_fill_stats();
    eprintln!("[ic] cross-call window fills  fast {fast}  full {full}");
}

#[test]
fn program_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    run_program(PROGRAM, EXPECTED);
}

#[test]
fn deep_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    run_program(DEEP, DEEP_EXPECTED);
}

const LATCHES: [&str; 5] = [
    "ZIPP_NO_CROSS_FRAMED_ENTRY",
    "ZIPP_NOJIT",
    "ZIPP_JIT_THRESHOLD",
    "ZIPP_NO_NURSERY",
    "ZIPP_NO_CROSS3",
];

/// Run one `*_child` test in a fresh process with `ZIPP_ICSTATS=1` plus `envs`;
/// returns the child's stderr after asserting it passed.
fn child(test: &str, envs: &[(&str, &str)]) -> String {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.args([test, "--exact", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("ZIPP_ICSTATS", "1")
        // The latched-off and interpreter modes run through the framed route;
        // in an unoptimized test binary each interpreter level is a large
        // Rust frame, so give the child's test thread real room.
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

/// The `[ic] cross-call window fills  fast N  full M` line's two counts.
fn fills(log: &str) -> (u64, u64) {
    let line = log
        .lines()
        .find(|l| l.contains("cross-call window fills"))
        .unwrap_or_else(|| panic!("no cross-call fill line in:\n{log}"));
    let nums: Vec<u64> = line
        .split_whitespace()
        .filter_map(|w| w.parse().ok())
        .collect();
    (nums[0], nums[1])
}

#[test]
fn handler_bodies_are_entered_natively_frame_backed() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let log = child("program_child", &[]);
    let (fast, full) = fills(&log);
    assert!(
        fast + full >= 10_000,
        "the five handler bodies must be cross-entered thousands of times, saw fast={fast} full={full}:\n{log}"
    );
}

#[test]
fn the_latch_restores_the_framed_route_with_the_same_answer() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let log = child("program_child", &[("ZIPP_NO_CROSS_FRAMED_ENTRY", "1")]);
    let (fast, full) = fills(&log);
    assert_eq!(
        (fast, full),
        (0, 0),
        "with the latch off no handler body may hold a cross entry:\n{log}"
    );
}

#[test]
fn every_mode_agrees_and_deep_recursion_overflows_catchably() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    for envs in [
        vec![],
        vec![("ZIPP_NO_CROSS_FRAMED_ENTRY", "1")],
        vec![("ZIPP_NO_CROSS3", "1")],
        vec![("ZIPP_NOJIT", "1")],
        vec![("ZIPP_JIT_THRESHOLD", "1")],
        vec![("ZIPP_NO_NURSERY", "1")],
    ] {
        child("program_child", &envs);
        child("deep_child", &envs);
    }
}
