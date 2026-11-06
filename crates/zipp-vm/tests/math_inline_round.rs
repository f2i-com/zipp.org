//! `Math.round`, `fround` and `sign` emitted inline in compiled code
//! (`emit_math_unary_inline`) and `Math.clz32` counted in place
//! (`emit_math_clz32`), against signed zeros, NaN, infinities, the halves
//! `floor(x + 0.5)` gets wrong (the double below 0.5, odd values from 2^52
//! up), i32/u32 edges, huge, f32-overflowing and subnormal values, Ints, and
//! coercing arguments, which leave compiled code. Run interpreted, at the
//! default thresholds, with every function compiled at once, under GC stress
//! and with the helper call back; the expected line is Node 24's.

const EXPECTED: &str = "0 -0 1 -1 1 -0 0 -0 1 -1 2 -1 3 -2 4 -4 4503599627370496 -4503599627370495 4503599627370497 -4503599627370497 9007199254740991 -9007199254740991 2147483648 -2147483648 4294967295 4294967296 4294967301 2147483648 -2147483648 1e+21 -1e+21 9223372036854776000 -9223372036854776000 18446744073709556000 1e+300 -1e+300 0 -0 0 -0 3.4028235677973366e+38 0 0 0 NaN Infinity -Infinity|0 -0 1 -1 0.5 -0.5 0.5 -0.5 0.5 -0.5 1.5 -1.5 2.5 -2.5 3.700000047683716 -3.700000047683716 4503599627370496 -4503599627370496 4503599627370496 -4503599627370496 9007199254740992 -9007199254740992 2147483648 -2147483648 4294967296 4294967296 4294967296 2147483648 -2147483648 1.0000000200408773e+21 -1.0000000200408773e+21 9223372036854776000 -9223372036854776000 18446744073709552000 Infinity -Infinity 0 -0 0 -0 Infinity 1.401298464324817e-45 0 0.10000000149011612 NaN Infinity -Infinity|0 -0 1 -1 1 -1 1 -1 1 -1 1 -1 1 -1 1 -1 1 -1 1 -1 1 -1 1 -1 1 1 1 1 -1 1 -1 1 -1 1 1 -1 1 -1 1 -1 1 1 1 1 NaN 1 -1|32 32 31 0 32 32 32 32 32 32 31 0 30 0 30 0 0 31 31 0 0 31 1 0 0 32 29 0 0 0 2 32 32 19 32 32 32 32 32 32 32 32 32 32 32 32 32|4487|-Infinity|Infinity|-Infinity|-Infinity|63|-6|2.5|1|32|NaN|32|-1|0|602|TypeError|-608668496";

#[test]
fn math_round_child() {
    if std::env::var_os("ZIPP_MATH_ROUND_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, [EXPECTED]);
}

#[test]
fn compiled_round_fround_sign_clz32_match_node() {
    if std::env::var_os("ZIPP_MATH_ROUND_CHILD").is_some() {
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
        cmd.args(["--exact", "math_round_child", "--nocapture"])
            .env("ZIPP_MATH_ROUND_CHILD", "1")
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

const PROGRAM: &str = r####"// Math.round / fround / sign compiled inline and Math.clz32 counted in place,
// against signed zeros, NaN, infinities, halves (the double below 0.5, odd
// values from 2^52 up), i32/u32 edges, huge, f32-overflowing and subnormal
// values, Ints, and coercing arguments (which leave compiled code).
"use strict";
const out = [];
const show = (v) => (Object.is(v, -0) ? "-0" : String(v));
function ro(x) { return Math.round(x); }
function fr(x) { return Math.fround(x); }
function sg(x) { return Math.sign(x); }
function cz(x) { return Math.clz32(x); }
const xs = [0, -0, 1, -1, 0.5, -0.5, 0.49999999999999994, -0.49999999999999994, 0.5000000000000001,
  -0.5000000000000001, 1.5, -1.5, 2.5, -2.5, 3.7, -3.7, 4503599627370495.5, -4503599627370495.5, 4503599627370497,
  -4503599627370497, 9007199254740991, -9007199254740991, 2147483647.5, -2147483648.5, 4294967295, 4294967296,
  4294967301, 2 ** 31, -(2 ** 31), 1e21, -1e21, 2 ** 63, -(2 ** 63), 2 ** 64 + 2 ** 12, 1e300, -1e300, 1e-300,
  -1e-300, 5e-324, -5e-324, 3.4028235677973366e38, 1.401298464324817e-45, 7e-46, 0.1, NaN, Infinity, -Infinity];
const fs = [ro, fr, sg, cz];
for (let r = 0; r < 300; r++) for (const f of fs) for (const x of xs) f(x);
for (const f of fs) out.push(xs.map((x) => show(f(x))).join(" "));
// Results compared, used as keys and as Int operands.
let hits = 0; const a = [10, 11, 12, 13];
for (let k = 0; k < 4000; k++) { if (ro(k / 1000) === 2) hits++; hits += a[cz(k) - 19] === undefined ? 0 : 1; hits += sg(k - 2000) | 0; }
out.push(hits, 1 / ro(-0.4), 1 / ro(0.4), 1 / sg(-0), 1 / fr(-0), cz(0) + cz(-1) + cz(1));
// Coercing arguments.
let calls = 0; const o = { valueOf() { calls++; return -6.5; } };
for (let r = 0; r < 300; r++) { ro(o); cz(o); }
out.push(ro(o), fr("2.5"), sg(true), cz(null), ro(undefined), cz("x"), sg([-2]), cz(o), calls);
try { cz(1n); } catch (e) { out.push(e.constructor.name); }
// Loops that hash the results.
function hash(n) {
  let h = 0;
  for (let i = 0; i < n; i++) {
    const x = i * 0.37 - 900;
    h = (Math.imul(h, 31) + Math.round(x) + Math.clz32(i * 7919) + Math.sign(x) + (Math.fround(x) * 8 | 0)) | 0;
  }
  return h;
}
let t = 0; for (let r = 0; r < 50; r++) t = (t + hash(5000)) | 0;
out.push(t);
console.log(out.join("|"));
"####;
