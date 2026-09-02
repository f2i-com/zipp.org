//! The truncation-aware Int add: an `Add`/`Sub`/`AddInt` whose result is
//! observed only through ToInt32 (`| 0`, `>>> 0`, `& mask`, any `Bitwise`
//! operand, or an arith chain into one) wraps in i32 on the boxed tiers instead
//! of branching to the f64 overflow path (`trunc_only_arith_ips`).
//!
//! The oracle is zipp against itself, as `jit_tier_fuzz` argues: every mode in
//! [`MODES`] — default, `ZIPP_NOJIT=1`, `ZIPP_JIT_THRESHOLD=1`,
//! `ZIPP_NO_NURSERY=1` — and the latch-off binary (`ZIPP_NO_INT32_TRUNC_ADD=1`)
//! must print byte-identical output; node is the secondary oracle. The
//! mechanism half checks that the default compile actually reports wrapped
//! sites (`ZIPP_JITLOG`) and that the latch pins them to zero.

const CHILD_ENV: &str = "ZIPP_INT32_TRUNC_ADD_CHILD";

/// Every truncation idiom over an overflowing pair, a chain, both `AddInt`
/// forms, the two exact-observer controls (`twice`, `both`), a consumer whose
/// other operand changes type mid-run (`poly` — the Bitwise bails, the
/// interpreter reads the wrapped Int through the same ToInt32), non-Int Add
/// operands (doubles, strings, null, objects, BigInt — the generic path), and
/// the calls-closures pipeline shape itself.
const PROGRAM: &str = r#"
"use strict";
function or0(a, b) { return (a + b) | 0; }
function sub0(a, b) { return (a - b) | 0; }
function ushr0(a, b) { return (a + b) >>> 0; }
function mask(a, b) { return (a + b) & 65535; }
function xorc(a, b, c) { return (a + b) ^ c; }
function shl(a, b) { return (a + b) << 3; }
function count(a, b) { return -1 >>> (a + b); }
function chain(a, b, c) { return (a + b + c) | 0; }
function addint(a) { return ((a | 0) + 1013904223) | 0; }
function subint(a) { return ((a | 0) - 7) | 0; }
function twice(a, b) { var s = a + b; return (s | 0) + s; }
function both(a, b) { return ((a + b) | 0) + (a + b); }
function poly(a, b, k) { return (a + b) | k; }
function step(v, salt) { v = Math.imul(v ^ salt, 1664525); return (v + 1013904223) | 0; }
function rotate(value, amount) { return (value << amount) | (value >>> (32 - amount)); }
function pipeline(seed) {
  let calls = 0;
  let offset = seed | 0;
  return (value, salt) => {
    calls = (calls + 1) | 0;
    if ((calls & 1023) === 0) { offset = (offset + seed + calls) | 0; }
    value = Math.imul(value ^ salt ^ offset, 1664525);
    return (rotate(value, (seed & 7) + 1) + 1013904223) | 0;
  };
}
var acc = 0;
var x = 0x13579bdf | 0;
for (var i = 0; i < 40000; i++) {
  x = (x + 0x9e3779b9) | 0;
  var a = x, b = Math.imul(x, 0x85ebca6b);
  acc = (acc + or0(a, b) + sub0(a, b) + ushr0(a, b) + mask(a, b) + xorc(a, b, i)) | 0;
  acc = (acc + shl(a, b) + count(a, b) + chain(a, b, i) + addint(a) + subint(b)) | 0;
  acc = (acc + twice(a, b) + both(a, b) + poly(a, b, 0) + step(a, i & 1023)) | 0;
}
console.log("warm", acc);
var pairs = [
  [2147483647, 1], [-2147483648, -1], [2147483647, 2147483647], [-2147483648, -2147483648],
  [-2147483648, 2147483647], [0, -2147483648], [-1, -2147483648], [1013904223, 1013904223],
  [2147483647, 0], [123456789, 987654321], [-123456789, -987654321], [1073741824, 1073741824],
  [-1073741824, -1073741825], [65535, 65535], [-65536, 1], [1, -1]
];
for (var k = 0; k < pairs.length; k++) {
  var p = pairs[k], pa = p[0], pb = p[1];
  console.log(pa, pb, or0(pa, pb), sub0(pa, pb), ushr0(pa, pb), mask(pa, pb), xorc(pa, pb, 7),
    shl(pa, pb), count(pa, pb), chain(pa, pb, pa), addint(pa), subint(pb), twice(pa, pb), both(pa, pb));
}
console.log("poly", poly(2147483647, 1, "0"), poly(2147483647, 1, 1.5),
  poly(2147483647, 1, { valueOf: function () { return 3; } }), poly(2147483647, 1, undefined),
  poly(2147483647, 1, NaN), poly(-2147483648, -1, "8"), poly(2147483647, 1, 0));
console.log("dbl", or0(2147483647.5, 1), or0(2147483648, 2147483648), or0(1e21, 1), or0(-0, 0),
  or0(NaN, 1), or0(Infinity, -Infinity), sub0(9007199254740992, -1), ushr0(-1.5, 0.5),
  chain(2147483647, 0.5, 0.5), addint(2147483647.7), mask(4294967295.9, 0));
console.log("str", or0("7", 1), or0("x", 1), or0(true, 1), or0(null, 5), or0(undefined, 5),
  or0([3], 4), or0({}, 1));
var mixed = "none";
try { or0(1n, 2n); } catch (e) { mixed = e instanceof TypeError ? "TypeError" : "other"; }
console.log("bigint", mixed, String(1n + 2n));
acc = 0;
for (var j = 0; j < 20000; j++) { acc = (acc + or0(x, j) + poly(x, j, 0) + step(x, j)) | 0; }
console.log("rehot", acc);
var pipes = [];
for (var q = 0; q < 16; q++) { pipes.push(pipeline((q * 97 + 11) | 0)); }
var state = 0x13579bdf | 0, checksum = 0;
for (var r = 0; r < 60000; r++) {
  var fn = pipes[r & 15];
  state = fn(state, r & 1023);
  checksum = (checksum + (state & 65535)) | 0;
}
console.log("pipeline", state, checksum);
"#;

const MODES: &[(&str, &[(&str, &str)])] = &[
    ("default", &[]),
    ("interpreter", &[("ZIPP_NOJIT", "1")]),
    ("forced-jit", &[("ZIPP_JIT_THRESHOLD", "1")]),
    ("no-nursery", &[("ZIPP_NO_NURSERY", "1")]),
    ("latch-off", &[("ZIPP_NO_INT32_TRUNC_ADD", "1")]),
    (
        "latch-off-forced-jit",
        &[
            ("ZIPP_NO_INT32_TRUNC_ADD", "1"),
            ("ZIPP_JIT_THRESHOLD", "1"),
        ],
    ),
];

/// CHILD half: runs the program in-process under whatever mode the parent
/// chose and echoes each output line with an `OUT:` prefix.
#[test]
fn int32_trunc_add_program_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("program compiles");
    assert!(out.error.is_none(), "runtime error: {:?}", out.error);
    for line in out.output {
        println!("OUT:{line}");
    }
}

fn child(envs: &[(&str, &str)]) -> (Vec<String>, String) {
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["--exact", "int32_trunc_add_program_child", "--nocapture"])
        .env(CHILD_ENV, "1");
    for key in [
        "ZIPP_NOJIT",
        "ZIPP_JIT_THRESHOLD",
        "ZIPP_NO_NURSERY",
        "ZIPP_NO_INT32_TRUNC_ADD",
        "ZIPP_JITLOG",
        "ZIPP_GC_STRESS",
    ] {
        cmd.env_remove(key);
    }
    cmd.envs(envs.iter().copied());
    let out = cmd.output().expect("spawn mode child");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success() && !stdout.contains("running 0 tests"),
        "child {envs:?} failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    let lines = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("OUT:"))
        .map(str::to_owned)
        .collect();
    (lines, stderr)
}

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH (the secondary oracle)");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// PARENT half: every mode and the latch-off run print the same lines, and
/// they are node's lines.
#[test]
fn int32_trunc_add_modes_and_latch_agree() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let (reference, _) = child(MODES[0].1);
    assert!(
        reference.len() >= 20,
        "the program printed too little: {reference:?}"
    );
    for (mode, envs) in &MODES[1..] {
        let (lines, stderr) = child(envs);
        assert_eq!(lines, reference, "{mode} disagrees with default:\n{stderr}");
    }
    assert_eq!(node_output(PROGRAM), reference, "node disagrees with zipp");
}

/// MECHANISM: the default compile reports wrapped sites in the Tier-C body
/// and/or the MEM region; the latch reports none. (`ZIPP_JITLOG` is read at
/// each compile, so it is safe to set per child.)
#[cfg(feature = "jit")]
#[test]
fn int32_trunc_add_latch_controls_the_emission() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let (_, on) = child(&[("ZIPP_JITLOG", "1")]);
    assert!(
        on.contains("int32-trunc arith sites="),
        "the default compile reported no wrapped sites:\n{on}"
    );
    let (_, off) = child(&[("ZIPP_JITLOG", "1"), ("ZIPP_NO_INT32_TRUNC_ADD", "1")]);
    assert!(
        !off.contains("int32-trunc arith sites="),
        "the latch left wrapped sites in the emission:\n{off}"
    );
}
