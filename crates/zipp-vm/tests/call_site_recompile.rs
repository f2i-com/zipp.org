//! Compiled call sites whose callee changes with the program's phase, so their
//! fallback runs until the body is recompiled with an arm per callee
//! (`Jit::note_call_fallback`, a `CallWithThis` lane per distinct callee):
//! `this.cmp(a, b)` with three comparators in turn and the first again, a plain
//! call of a closure made anew per round, then a function of another arity and
//! a native at that site, and two classes' methods through one captured call.
//! Run interpreted, at the default thresholds, with every function compiled at
//! once, under GC stress, with the recompile off and with the polymorphic lane
//! off; the expected line is Node 24's. A second test checks that the default
//! run does recompile.

const EXPECTED: &str = "678882618,205362106,379644286,394210059,2549865,9400,989403,255000,360000,450000";

#[test]
fn call_site_recompile_child() {
    if std::env::var_os("ZIPP_CALL_SITE_RECOMPILE_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn phase_changing_call_sites_match_node() {
    if std::env::var_os("ZIPP_CALL_SITE_RECOMPILE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("recompile-off", Some(("ZIPP_NO_CALL_FALLBACK_RECOMPILE", "1"))),
        ("poly-off", Some(("ZIPP_NO_CWT_POLY", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "call_site_recompile_child", "--nocapture"])
            .env("ZIPP_CALL_SITE_RECOMPILE_CHILD", "1")
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

/// At the default thresholds the queue's push and pop bodies are evicted
/// for their comparator site's fallback and recompiled; with the switch
/// off they never are.
#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn a_missing_call_site_recompiles_its_body() {
    if std::env::var_os("ZIPP_CALL_SITE_RECOMPILE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let log = |off: bool| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "call_site_recompile_child", "--nocapture"])
            .env("ZIPP_CALL_SITE_RECOMPILE_CHILD", "1")
            .env("ZIPP_JITLOG", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_NO_CALL_FALLBACK_RECOMPILE");
        if off {
            cmd.env("ZIPP_NO_CALL_FALLBACK_RECOMPILE", "1");
        }
        let out = cmd.output().expect("re-run test binary");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    assert!(log(false).contains("EVICTED (call fallback"), "no call-fallback recompile");
    assert!(!log(true).contains("EVICTED (call fallback"), "the switch did not stop it");
}

const PROGRAM: &str = r####"// A compiled call site whose callee changes with the program's phase, so
// its fallback runs until the body is recompiled with an arm per callee:
// `this.cmp(a, b)` (CallWithThis) with three comparators in turn, a plain
// call of a closure made anew per round, a callee replaced by a function
// with another arity, and receivers of two classes. Every phase's result
// must be exact before, across and after the recompile.
const out = [];
class PQ {
  constructor(cmp) { this.cmp = cmp; this.h = []; }
  push(v) { const h = this.h; h.push(v); let i = h.length - 1; while (i > 0) { const p = (i - 1) >> 1; if (this.cmp(h[i], h[p]) >= 0) break; const t = h[i]; h[i] = h[p]; h[p] = t; i = p; } }
  pop() { const h = this.h; const top = h[0]; const last = h.pop(); if (h.length > 0) { h[0] = last; let i = 0; for (;;) { const l = 2 * i + 1, r = l + 1; let m = i; if (l < h.length && this.cmp(h[l], h[m]) < 0) m = l; if (r < h.length && this.cmp(h[r], h[m]) < 0) m = r; if (m === i) break; const t = h[i]; h[i] = h[m]; h[m] = t; i = m; } } return top; }
}
function drain(cmp, n, seed) {
  const q = new PQ(cmp); let s = seed;
  for (let i = 0; i < n; i++) { s = (s * 1103515245 + 12345) % 2147483648; q.push(s % 100003); }
  let acc = 0, k = 0;
  while (q.h.length > 0) acc = (acc * 31 + q.pop() * (++k)) % 1000000007;
  return acc;
}
out.push(drain((a, b) => a - b, 20000, 1));
out.push(drain((a, b) => b - a, 20000, 2));
out.push(drain((a, b) => (a % 97) - (b % 97) || a - b, 20000, 3));
out.push(drain((a, b) => a - b, 5000, 4));
// A plain call of a closure made anew per round, then a function of
// another arity and a native at the same site.
function rounds(mk, n) { let t = 0; const f = mk(); for (let i = 0; i < n; i++) t = (t + f(i)) % 1000003; return t; }
let r6 = 0; for (let r = 0; r < 6; r++) { let s = r; r6 += rounds(() => (x) => (s = (s * 7 + x) % 9973), 20000); }
out.push(r6, rounds(() => function (x, y) { return (y === undefined ? 1 : 2) + x; }, 20000), rounds(() => Math.abs, 20000));
// Two classes' methods through one captured call.
class A { f(x) { return x + 1; } } class B { f(x) { return x * 2; } }
function each(objs, n) { let t = 0; for (let i = 0; i < n; i++) { const o = objs[i & 1]; t = (t + o.f(i & 15)) | 0; } return t; }
out.push(each([new A(), new A()], 30000), each([new A(), new B()], 30000), each([new B(), new B()], 30000));
console.log(out.join(","));
"####;
