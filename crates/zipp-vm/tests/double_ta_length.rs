//! `ta.length` on the DOUBLE register tier. A loop such as
//! `for (k = 0; k < f64.length; k++) s += f64[k]` is double-typed, so it is
//! never an INTEGER-tier candidate, and the double tier had no arm for the
//! length read: the planner typed the receiver's load as a number, the
//! emitter declined, and the loop ran on the memory tier with a helper call
//! per iteration for its bound (4096 elements x 2000 passes: 253 ms, against
//! 9 ms for the same loop over a Uint8Array and 5 ms in node).
//!
//! The tier now reads the length from the pin snapshot the INTEGER tier
//! already uses. The snapshot is taken only under the live pristine-getter
//! proof, so these probes change everything that proof depends on BETWEEN
//! entries -- the receiver global re-pointed to shorter, longer, differently
//! typed and non-typed arrays, an own shadow, a subclass getter, a re-pointed
//! prototype, a replaced base getter, a detached buffer, a resized
//! length-tracking view -- and check the answers against `node` (v24).

const PROBES: &str = r#""use strict";
var res = "";
var i, t;
// 1 the canonical double-typed loop, receiver in a register and in a global
function sum(ta) { var s = 0; for (var k = 0; k < ta.length; k++) s += ta[k]; return s; }
var f64 = new Float64Array(64), f32 = new Float32Array(48);
for (i = 0; i < 64; i++) f64[i] = i * 0.5;
for (i = 0; i < 48; i++) f32[i] = i * 0.25;
t = 0; for (i = 0; i < 400; i++) t += sum(f64) + sum(f32);
res += ";a=" + t;
var g = f64;
function gsum() { var s = 0; for (var k = 0; k < g.length; k++) s += g[k]; return s; }
t = 0; for (i = 0; i < 400; i++) t += gsum();
// 2 the global is re-pointed to a shorter, a longer and a differently typed array between entries
g = new Float64Array(8).fill(1.5); t += gsum();
g = new Float64Array(200).fill(0.25); t += gsum();
g = new Float32Array(20).fill(0.5); t += gsum();
g = [0.5, 1.5, 2.5]; t += gsum();
res += ";b=" + t;
// 3 a receiver whose `length` is not the built-in getter
var shadow = new Float64Array(16).fill(2); Object.defineProperty(shadow, "length", { value: 4 });
class Short extends Float64Array { get length() { return 3; } }
var short = new Short(16).fill(3);
var reproto = new Float64Array(16).fill(1); Object.setPrototypeOf(reproto, { length: 2 });
res += ";c=" + sum(shadow) + "/" + sum(short) + "/" + sum(reproto);
// 4 a detached buffer reads as length 0
var det = new Float64Array(32).fill(1);
t = sum(det); det.buffer.transfer(); t += sum(det);
res += ";d=" + t;
// 5 the base getter is replaced, then restored
var base = Object.getPrototypeOf(Float64Array.prototype);
var desc = Object.getOwnPropertyDescriptor(base, "length");
Object.defineProperty(base, "length", { get: function () { return 5; }, configurable: true });
g = f64; t = sum(f64) + gsum();
Object.defineProperty(base, "length", desc);
t += sum(f64) + gsum();
res += ";e=" + t;
// 6 a length-tracking view over a resizable buffer, resized between entries
var rab = new ArrayBuffer(64, { maxByteLength: 4096 }), track = new Float64Array(rab);
for (i = 0; i < track.length; i++) track[i] = i + 0.5;
t = 0; for (i = 0; i < 300; i++) t += sum(track);
rab.resize(2048); t += sum(track) * 1000 + track.length;
rab.resize(16); t += sum(track) * 100000 + track.length;
res += ";f=" + t;
// 7 the length used as a value in double arithmetic, and NaN / Infinity / -0 elements
var odd = new Float64Array([1.5, NaN, Infinity, -0, 2.5]);
function mix(ta) { var n = 0, s = 0; for (var k = 0; k < ta.length; k++) { s += ta.length * 0.5 + k / ta.length; if (ta[k] === ta[k]) n += ta[k] > 1e300 ? 0 : ta[k]; } return s + n; }
t = 0; for (i = 0; i < 400; i++) t += mix(odd) + mix(f32);
res += ";g=" + t;
// 8 a zero-length view and a view with a byte offset
var none = new Float64Array(0), off = new Float64Array(f64.buffer, 16, 10);
t = 0; for (i = 0; i < 400; i++) t += sum(none) + sum(off);
res += ";h=" + t;
console.log(res.slice(1));
"#;

const EXPECTED: &str = "a=516000;b=403276.5;c=8/9/2;d=32;e=2026;f=241858;g=590400;h=13000";

#[test]
fn double_loop_lengths_match_node() {
    let out = zipp_vm::run(PROBES).expect("probes compile");
    assert!(out.error.is_none(), "probes threw: {:?}", out.error);
    assert_eq!(out.output.join("
"), EXPECTED);
}

#[test]
fn double_loop_length_probe_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (label, key) in [
        ("arm-off", "ZIPP_NO_DOUBLE_TA_LENGTH"),
        ("interpreter", "ZIPP_NOJIT"),
        ("forced-jit", "ZIPP_JIT_THRESHOLD"),
        ("gc-stress", "ZIPP_GC_STRESS"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("double_loop_lengths_match_node")
            .arg("--exact")
            .env_remove("ZIPP_NO_DOUBLE_TA_LENGTH")
            .env(key, "1")
            .output()
            .expect("spawn the probe child");
        assert!(
            out.status.success(),
            "{label} child failed:
{}
{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// One loop, whose only bound is `a.length`, so the tier it lands on is the
/// mechanism under test.
#[test]
fn canonical_double_loop() {
    let out = zipp_vm::run(
        "var f = new Float64Array(4096); for (var q = 0; q < 4096; q += 1024) f[q] = 0.5;
         function s(a) { var t = 0; for (var k = 0; k < a.length; k++) t += a[k]; return t; }
         var r = 0; r += s(f); r += s(f); r += s(f); console.log(r);",
    )
    .expect("compiles");
    assert!(out.error.is_none(), "threw: {:?}", out.error);
    assert_eq!(out.output.join("
"), "6");
}

/// The length-bounded double loop compiles on the DOUBLE tier, and the
/// off-switch sends it back to the memory tier.
#[cfg(all(target_arch = "x86_64", feature = "jit"))]
#[test]
fn length_bounded_double_loop_stays_on_the_double_tier() {
    let exe = std::env::current_exe().expect("test exe path");
    let log = |off: bool| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("canonical_double_loop")
            .arg("--exact")
            .arg("--nocapture")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env_remove("ZIPP_NO_DOUBLE_TA_LENGTH")
            .env("ZIPP_JITLOG", "1");
        if off {
            cmd.env("ZIPP_NO_DOUBLE_TA_LENGTH", "1");
        }
        let out = cmd.output().expect("spawn the logging child");
        assert!(out.status.success(), "logging child failed");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let on = log(false);
    assert!(
        on.contains("DOUBLE region fn1") && !on.contains("MEM region fn1"),
        "the length-bounded loop left the DOUBLE tier:
{on}"
    );
    let off = log(true);
    assert!(
        off.contains("MEM region fn1") && !off.contains("DOUBLE region fn1"),
        "ZIPP_NO_DOUBLE_TA_LENGTH did not restore the memory tier:
{off}"
    );
}
