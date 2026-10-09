//! Nested leaf inlining of a wrapper whose inner closure calls `Math.imul`
//! with arguments that are not order-transparent, the shape of every
//! `ri(n) { return (rnd() * n) | 0 }` over a mulberry32 generator.
//!
//! Under the strict default call order (B280) such a call captures its
//! callee first: `LoadGlobal Math; GetProp imul; <args>; MathOp { callee,
//! this_v }`. The nested splice (`splice_nested_leaf`) cannot carry a
//! `GetProp` into a flat body, so it declined the whole wrapper and the hot
//! loop made two real calls per iteration. A site that `captured_math_site`
//! proves is now flattened to the bare form, whose guard validates the live
//! `Math` global and the live `Math.imul` slot where the multiply runs.
//!
//! The probes pin what that rewrite must not change: a replacement installed
//! from outside is what later calls run; a replacement installed BY THE
//! ARGUMENT leaves the captured intrinsic running for that call (the one
//! place the relaxed diagnostic order answers differently, which is why this
//! file has no relaxed-order mode); a rebound `Math`, an accessor, a deleted
//! slot and a rotating inner closure are all observed exactly once per call.
//! Expected values come from `node` (v24) running the same program.

const PROBES: &str = r#""use strict";
var res = "";
function mulberry32(seed) {
  var a = seed | 0;
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
// 1 the wrapper over the closure, with and without a second argument
var rnd = mulberry32(0xC0FFEE);
function ri(n) { return (rnd() * n) | 0; }
function rj(n, k) { return ((rnd() * n) | 0) + k; }
var N = 24000, i, s = 0;
for (i = 0; i < N; i++) s = (s + ri(26)) | 0;
for (i = 0; i < N; i++) s = (s + rj(1000, i & 3)) | 0;
res += "a=" + s;
// 2 Math.imul replaced mid-loop from outside: later calls run the replacement
var origImul = Math.imul, over = 0;
s = 0;
for (i = 0; i < N; i++) {
  if (i === 16000) Math.imul = function (x, y) { over++; return origImul(x, y) ^ 1; };
  s = (s + ri(97)) | 0;
}
Math.imul = origImul;
res += ";b=" + s + "/" + over;
// 3 replaced BY THE ARGUMENT: that call still runs the captured intrinsic
function mk(seed) {
  var b = seed | 0;
  var f = function () { var t = Math.imul(b ^ 5, 3); return t | 0; };
  f.set = function (v) { b = v; };
  return f;
}
var g = mk(9), evil = 0, seen = "";
function w(n) { return (g() + n) | 0; }
s = 0;
for (i = 0; i < N; i++) {
  if (i === 20000) g.set({ valueOf: function () { Math.imul = function () { evil++; return 1000; }; return 7; } });
  var r = w(i & 1);
  if (i >= 19999 && i < 20003) seen += r + ",";
  s = (s + r) | 0;
}
Math.imul = origImul;
g.set(9);
res += ";c=" + s + "/" + evil + "/" + seen;
// 4 the Math global rebound mid-loop, then restored
var realMath = Math, fake = 0;
s = 0;
for (i = 0; i < N; i++) {
  if (i === 12000) Math = { imul: function (x, y) { fake++; return 42; } };
  if (i === 18000) Math = realMath;
  s = (s + w(1)) | 0;
}
res += ";d=" + s + "/" + fake;
// 5 Math.imul redefined as an accessor: every call performs the Get once
var gets = 0;
s = 0;
for (i = 0; i < N; i++) {
  if (i === 8000) Object.defineProperty(Math, "imul", { get: function () { gets++; return origImul; }, configurable: true });
  s = (s + ri(11)) | 0;
}
Object.defineProperty(Math, "imul", { value: origImul, writable: true, configurable: true, enumerable: false });
res += ";e=" + s + "/" + gets;
// 6 deleted, so the call throws a TypeError the wrapper's caller catches
var threw = 0;
s = 0;
for (i = 0; i < N; i++) {
  if (i === N - 10) delete Math.imul;
  try { s = (s + w(2)) | 0; } catch (e) { if (e instanceof TypeError) threw++; }
}
Math.imul = origImul;
res += ";f=" + s + "/" + threw;
// 7 the wrapper's inner closure rotates: the splice is guarded on its identity
var gens = [mulberry32(1), mulberry32(2)], cur = gens[0];
function rk(n) { return (cur() * n) | 0; }
s = 0;
for (i = 0; i < N; i++) { if (i === 12000) cur = gens[1]; if (i === 20000) cur = gens[0]; s = (s + rk(7)) | 0; }
res += ";g=" + s;
console.log(res);"#;

const EXPECTED: &str = "a=12325339;b=1155397/16000;c=4731006/3999/37,6,1001,1000,;d=924000/6000;e=119793/32000;f=911620/10;g=71656";

const SWITCHES: [&str; 5] = [
    "ZIPP_RELAXED_CALL_ORDER",
    "ZIPP_STRICT_CALL_ORDER",
    "ZIPP_NO_NESTED_CAPTURED_IMUL",
    "ZIPP_NOJIT",
    "ZIPP_JIT_THRESHOLD",
];

#[test]
fn nested_captured_imul_matches_node() {
    let out = zipp_vm::run(PROBES).expect("probes compile");
    assert!(out.error.is_none(), "probes threw: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn nested_captured_imul_probe_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (label, key) in [
        ("splice-off", "ZIPP_NO_NESTED_CAPTURED_IMUL"),
        ("interpreter", "ZIPP_NOJIT"),
        ("forced-jit", "ZIPP_JIT_THRESHOLD"),
        ("gc-stress", "ZIPP_GC_STRESS"),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("nested_captured_imul_matches_node").arg("--exact");
        for switch in SWITCHES {
            cmd.env_remove(switch);
        }
        let out = cmd.env(key, "1").output().expect("spawn the probe child");
        assert!(
            out.status.success(),
            "{label} child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[cfg(all(target_arch = "x86_64", feature = "jit"))]
#[test]
fn nested_captured_imul_is_spliced() {
    let exe = std::env::current_exe().expect("test exe path");
    let run = |off: bool| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("nested_captured_imul_matches_node")
            .arg("--exact")
            .arg("--nocapture");
        for switch in SWITCHES {
            cmd.env_remove(switch);
        }
        cmd.env("ZIPP_JITLOG", "1");
        if off {
            cmd.env("ZIPP_NO_NESTED_CAPTURED_IMUL", "1");
        }
        let out = cmd.output().expect("spawn the logging child");
        assert!(out.status.success(), "logging child failed");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    assert!(
        run(false).contains("NESTED-INLINE"),
        "no wrapper over a captured Math.imul closure was spliced"
    );
    // Every inner closure in the probes captures its callee, so with the
    // rewrite off nothing is left for the nested splice to take.
    assert!(
        !run(true).contains("NESTED-INLINE"),
        "ZIPP_NO_NESTED_CAPTURED_IMUL left a nested splice in place"
    );
}
