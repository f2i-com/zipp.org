//! `Math.sqrt`, `abs`, `floor`, `ceil` and `trunc` emitted inline in compiled code
//! (`emit_math_unary_inline`) against every class of input: signed zeros,
//! NaN, infinities, halves, i32 edges, huge and subnormal values, Ints, and
//! coercing arguments, which leave compiled code; results compared, used as
//! keys and summed in a loop. Run interpreted, at the default thresholds, with
//! every function compiled at once, under GC stress and with the helper call
//! back; the expected line is Node 24's.

const EXPECTED: &str = "0 -0 1 NaN 1.4142135623730951 2 3 1.5 0.7071067811865476 NaN 1.224744871391589 NaN 1.5811388300841898 NaN 0.7071067811865475 NaN 46340.950001051984 46340.95001184158 NaN NaN 46340.95000644678 NaN 67108864 94906265.62425156 1e+150 NaN 2.2227587494850775e-162 NaN 9.999999999999986e-156 NaN Infinity NaN 1.7320508075688772 NaN|0 0 1 1 2 4 9 2.25 0.5 0.5 1.5 1.5 2.5 2.5 0.49999999999999994 0.49999999999999994 2147483647 2147483648 2147483648 2147483649 2147483647.5 2147483648.5 4503599627370496 9007199254740992 1e+300 1e+300 5e-324 5e-324 1e-310 NaN Infinity Infinity 3 7|0 -0 1 -1 2 4 9 2 0 -1 1 -2 2 -3 0 -1 2147483647 2147483648 -2147483648 -2147483649 2147483647 -2147483649 4503599627370496 9007199254740992 1e+300 -1e+300 0 -1 0 NaN Infinity -Infinity 3 -7|0 -0 1 -1 2 4 9 3 1 -0 2 -1 3 -2 1 -0 2147483647 2147483648 -2147483648 -2147483649 2147483648 -2147483648 4503599627370496 9007199254740992 1e+300 -1e+300 1 -0 1 NaN Infinity -Infinity 3 -7|0 -0 1 -1 2 4 9 2 0 -0 1 -1 2 -2 0 -0 2147483647 2147483648 -2147483648 -2147483649 2147483647 -2147483648 4503599627370496 9007199254740992 1e+300 -1e+300 0 -0 0 NaN Infinity -Infinity 3 -7|1000|true|true|true|-1|-Infinity|-Infinity|Infinity|-7|3|1|0|NaN|NaN|2|301|TypeError|751581.3674340382";

#[test]
fn math_inline_child() {
    if std::env::var_os("ZIPP_MATH_INLINE_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn compiled_math_ops_match_node() {
    if std::env::var_os("ZIPP_MATH_INLINE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("helper", Some(("ZIPP_NO_MATH_INLINE", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "math_inline_child", "--nocapture"])
            .env("ZIPP_MATH_INLINE_CHILD", "1")
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

const PROGRAM: &str = r####"// Math.sqrt / abs / floor / ceil / trunc compiled inline, against every
// class of input: signed zeros, NaN, infinities, halves, i32 edges, huge
// and subnormal values, Ints, and coercing arguments (which leave
// compiled code).
"use strict";
const out = [];
const show = (v) => (Object.is(v, -0) ? "-0" : String(v));
function sq(x) { return Math.sqrt(x); }
function ab(x) { return Math.abs(x); }
function fl(x) { return Math.floor(x); }
function ce(x) { return Math.ceil(x); }
function tr(x) { return Math.trunc(x); }
const xs = [0, -0, 1, -1, 2, 4, 9, 2.25, 0.5, -0.5, 1.5, -1.5, 2.5, -2.5, 0.49999999999999994,
  -0.49999999999999994, 2147483647, 2147483648, -2147483648, -2147483649, 2147483647.5, -2147483648.5,
  4503599627370496.5, 9007199254740993, 1e300, -1e300, 5e-324, -5e-324, 1e-310, NaN, Infinity, -Infinity, 3, -7];
const fs = [sq, ab, fl, ce, tr];
for (let r = 0; r < 300; r++) for (const f of fs) for (const x of xs) f(x);
for (const f of fs) out.push(xs.map((x) => show(f(x))).join(" "));
// Results compared and used as keys.
let hits = 0; const a = [10, 11, 12, 13];
for (let k = 0; k < 4000; k++) { if (fl(k / 1000) === 2) hits++; hits += a[tr(k / 1000)] === 10 + tr(k / 1000) ? 0 : 1000; }
out.push(hits, sq(16) === 4, ab(-3) === 3, ce(2.1) === 3, 1 / fl(-0.5), 1 / ce(-0.5), 1 / tr(-0.9), 1 / ab(-0));
// Coercing arguments.
let calls = 0; const o = { valueOf() { calls++; return -6.5; } };
for (let r = 0; r < 300; r++) fl(o);
out.push(fl(o), ce("2.5"), tr(true), sq(null), ab(undefined), sq("x"), ab([-2]), calls);
try { ab(1n); } catch (e) { out.push(e.constructor.name); }
// A loop that sums square roots.
function norm(v) { let s = 0; for (let i = 0; i < v.length; i++) s += Math.sqrt(v[i] * v[i] + 1); return s; }
const v = []; for (let i = 0; i < 100; i++) v.push(i - 50);
let t = 0; for (let r = 0; r < 300; r++) t += norm(v);
out.push(t);
console.log(out.join("|"));
"####;
