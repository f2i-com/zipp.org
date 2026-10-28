//! A heap's methods calling its comparator field while the program moves from
//! one comparator (a fresh closure per round) to another and back, so the call
//! sites' caches name several functions and a recompiled method inlines the one
//! it calls last (`Vm::ic_call_latest`). Every comparison must still call the
//! comparator the heap holds. Run interpreted, at the default thresholds, with
//! every function compiled at once, under GC stress and with the latest-callee
//! inline off; the expected lines are Node 24's.

const EXPECTED: &str = "842974368
189096773
899193861
600084537
695996489
756270700
430963863
982544810
true
590779251";

#[test]
fn latest_callee_inline_child() {
    if std::env::var_os("ZIPP_LATEST_CALLEE_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn queues_call_the_comparator_they_hold() {
    if std::env::var_os("ZIPP_LATEST_CALLEE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("latest-off", Some(("ZIPP_NO_LATEST_LEAF", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "latest_callee_inline_child", "--nocapture"])
            .env("ZIPP_LATEST_CALLEE_CHILD", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS");
        if let Some((k, v)) = env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(
            out.status.success(),
            "{mode}:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const PROGRAM: &str = r####"// A method calling a comparator field: one comparator (a fresh closure per
// round, as dijkstra makes one per call), then another, then the first
// again, so the call site's cache names several functions and the compiled
// method inlines the one it sees last. Every comparison must still call the
// comparator the queue holds.
"use strict";
class Heap {
  constructor(cmp) { this.cmp = cmp; this.h = []; }
  push(v) {
    const h = this.h; h.push(v);
    let i = h.length - 1;
    while (i > 0) { const p = (i - 1) >> 1; if (this.cmp(h[i], h[p]) >= 0) break; const t = h[i]; h[i] = h[p]; h[p] = t; i = p; }
  }
  pop() {
    const h = this.h, top = h[0], last = h.pop();
    if (h.length > 0) {
      h[0] = last; let i = 0;
      for (;;) {
        const l = 2 * i + 1, r = l + 1; let m = i;
        if (l < h.length && this.cmp(h[l], h[m]) < 0) m = l;
        if (r < h.length && this.cmp(h[r], h[m]) < 0) m = r;
        if (m === i) break;
        const t = h[i]; h[i] = h[m]; h[m] = t; i = m;
      }
    }
    return top;
  }
}
function drain(q) { let s = 0, k = 0, prev = null, ok = true; while (q.h.length) { const v = q.pop(); if (prev !== null && q.cmp(prev, v) > 0) ok = false; prev = v; s = (s * 31 + (Array.isArray(v) ? v[1] : v) * (k++ % 7 + 1)) % 1000000007; } return s + (ok ? "" : "!"); }
const out = [];
for (let round = 0; round < 6; round++) {
  const q = new Heap((a, b) => a[1] - b[1]);
  for (let i = 0; i < 3000; i++) q.push([i, (i * 7919 + round) % 1009]);
  out.push(drain(q));
}
const desc = new Heap((a, b) => b - a);
for (let i = 0; i < 20000; i++) desc.push((i * 2654435761) % 100003);
out.push(drain(desc));
let calls = 0;
const counted = new Heap((a, b) => { calls++; return a - b; });
for (let i = 0; i < 5000; i++) counted.push((i * 48271) % 2003);
out.push(drain(counted), calls > 0);
const again = new Heap((a, b) => a[1] - b[1]);
for (let i = 0; i < 3000; i++) again.push([i, (i * 31) % 997]);
out.push(drain(again));
console.log(out.join("\n"));
"####;
