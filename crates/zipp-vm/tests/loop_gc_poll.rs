//! A compiled loop's back-edge is a GC safe point when a collection is
//! requested (`jit_loop_gc_poll`; `ZIPP_NO_LOOP_GC_POLL` leaves it a bare
//! jump): a top-level loop region whose `exec` results, capture strings and
//! match arrays are allocated by a native with no poll, and a function's
//! loop (a Tier C body) whose `slice` results are, collect while they run
//! instead of growing the heap by every object they make. The expected lines
//! are Node's (v24), with the poll, without it and in the interpreter; with
//! the poll and in the interpreter the nursery's peak stays far below the
//! ~800K slots the loops reached without it.

const SOURCE: &str = r##"
"use strict";
const out = [];
let record = "";
for (let i = 0; i < 200; i++) record += "k" + i.toString(36) + "=" + (i * 7919 % 100000) + (i % 5 === 4 ? "; " : ", ");
const KV = /(\w+)=(\d+)/g;
let n = 0, sum = 0, h = 0;
for (let r = 0; r < 1000; r++) {
  KV.lastIndex = r % 7;
  let m;
  while ((m = KV.exec(record)) !== null) {
    n++;
    sum = (sum + (+m[2]) + m.index) | 0;
    h = Math.imul(h ^ (m[1].length + KV.lastIndex), 16777619) >>> 0;
  }
}
out.push("top " + n + " " + sum + " " + h);
function slices(s, k) {
  let acc = 0;
  for (let i = 0; i < k; i++) {
    const piece = s.slice(i % 50, (i % 50) + 20);
    acc = (acc + piece.length + piece.charCodeAt(3)) | 0;
  }
  return acc;
}
out.push("fn " + slices(record, 200000));
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "top 199286 1507492551 2622003607\nfn 15276000";

/// Peak heap slots the collector saw, with the poll or in the interpreter.
const PEAK_BOUND: u64 = 300_000;

#[test]
fn loop_gc_poll_worker() {
    if std::env::var_os("ZIPP_LOOP_GC_POLL_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
    if std::env::var_os("ZIPP_NO_LOOP_GC_POLL").is_none() {
        let peak = zipp_vm::gc_nursery_stats().7;
        assert!(peak < PEAK_BOUND, "the loops' heap grew to {peak} slots");
    }
}

#[test]
fn loop_gc_poll_matches_node() {
    if std::env::var_os("ZIPP_LOOP_GC_POLL_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, var) in [
        ("default", None),
        ("poll-off", Some("ZIPP_NO_LOOP_GC_POLL")),
        ("interpreter", Some("ZIPP_NOJIT")),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["loop_gc_poll_worker", "--exact", "--nocapture"])
            .env("ZIPP_LOOP_GC_POLL_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_LOOP_GC_POLL")
            .env_remove("ZIPP_GC_STRESS");
        if let Some(v) = var {
            command.env(v, "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
