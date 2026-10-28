//! `a % b` in compiled code: Int operands with a dividend of at least 0 and a
//! positive divisor take the Int lane (`emit_int_mod_fast`), in a loop, a
//! method body and a leaf call (both inliner copies); every other pair (a
//! negative dividend's -0, `% -1`, `% 0`, fractions, doubles past i32, NaN
//! and Infinity) the double path or the interpreter. Run interpreted, at the
//! default thresholds, with every function compiled at once, under GC stress
//! and with the lane off; the expected lines are Node 24's.

const EXPECTED: &str = "90000
0
199990000
-3 -0 -2 2 0 -0 0 -0 1 -2 -0 NaN NaN NaN 1.5 2 1 2 -2 NaN NaN NaN 3
224995
90000
0
199990000
-3 -0 -2 2 0 -0 0 -0 1 -2 -0 NaN NaN NaN 1.5 2 1 2 -2 NaN NaN NaN 3
224995
90000
0
199990000
-3 -0 -2 2 0 -0 0 -0 1 -2 -0 NaN NaN NaN 1.5 2 1 2 -2 NaN NaN NaN 3
224995";

#[test]
fn int_mod_lane_child() {
    if std::env::var_os("ZIPP_INT_MOD_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn compiled_mod_matches_the_double_path() {
    if std::env::var_os("ZIPP_INT_MOD_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("lane-off", Some(("ZIPP_NO_INT_MOD", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "int_mod_lane_child", "--nocapture"])
            .env("ZIPP_INT_MOD_CHILD", "1")
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

const PROGRAM: &str = r####"// `%` in compiled code: Int operands take the Int lane, everything else the
// double path; each line is a sum or a list Node prints identically.
"use strict";
const out = [];
function negZero(x) { return Object.is(x, -0) ? "-0" : String(x); }
// The lane's own case: a non-negative dividend, a positive divisor.
function modLoop(n, d) { let s = 0; for (let k = 0; k < n; k++) s += k % d; return s; }
// Cases the lane leaves to the double path.
function modPair(a, b) { return a % b; }
// A method body and a leaf call, so both inliner copies run.
class M { m(k) { return k % 10; } }
function leaf(k) { return k % 7; }
function viaInline(n) { const o = new M(); let s = 0; for (let k = 0; k < n; k++) s += o.m(k) + leaf(k); return s; }
for (let round = 0; round < 3; round++) {
  out.push(modLoop(20000, 10), modLoop(20000, 1), modLoop(20000, 0x7fffffff));
  const pairs = [[-4, 2], [-5, 3], [5, -3], [7, -1], [-7, -1], [0, 5], [-0, 5], [0x7fffffff, 7], [-0x80000000, 3],
    [-0x80000000, -1], [9, 0], [-9, 0], [0, 0], [5.5, 2], [7, 2.5], [Math.floor(9.7), 4], [2 ** 40, 7], [-(2 ** 40), 7],
    [NaN, 3], [3, NaN], [Infinity, 3], [3, Infinity], [-3, Infinity]];
  const r = [];
  for (let i = 0; i < 2000; i++) {
    const [a, b] = pairs[i % pairs.length];
    if (i >= 2000 - pairs.length) r.push(negZero(modPair(a, b)));
  }
  out.push(r.join(" "));
  out.push(viaInline(30000));
}
console.log(out.join("\n"));
"####;
