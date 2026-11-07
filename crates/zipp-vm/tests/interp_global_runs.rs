//! A script's global loops run as the interpreter's global runs
//! (`overlay_global_runs`: a global counter's loop head, its step and
//! back-edge, a global's step, an accumulator's `| 0` store, a masked
//! global; `ZIPP_NO_GLOBAL_RUNS` leaves them to the pair forms): Int
//! counters and accumulators, a double counter and bound, a counter past
//! 2^31, a string counter, negative and fractional globals masked and
//! shifted, an accessor on the global object behind every read and store, a
//! `valueOf` that runs and one that throws inside an accumulator's `| 0`,
//! a function stepping a global counter (the lite loop), a do-while and a
//! top-level `let`. The expected lines are Node's (v24), with the runs,
//! without them and in the interpreter.

const SOURCE: &str = r##"
var out = [];
// A script's counters and accumulators: every run shape.
var s = 0, cnt = 0, m = 0;
for (var i = 0; i < 3000; i++) {
  var o = i & 255;
  s = (s + o + (i >> 2)) | 0;
  m = (m ^ (i * 7)) | 0;
  cnt++;
}
out.push("ints " + s + " " + m + " " + cnt + " " + i + " " + o);
// A double counter and bound, a counter past 2^31, a string counter.
var d, dd = 0;
for (d = 0.5; d < 40; d++) dd = (dd + d * 2) | 0;
var big = 2147483640;
for (var j = 0; j < 20; j++) big++;
var sc = "5";
for (var k = 0; k < 3; k++) sc++;
out.push("kinds " + d + " " + dd + " " + big + " " + j + " " + sc + " " + typeof sc);
// A negative and a fractional global masked and shifted.
var neg = -12345.75, sh = 0;
for (var q = 0; q < 10; q++) { sh = (sh + (neg & 255) + (neg >> 3) + (neg >>> 28)) | 0; neg -= 1.5; }
out.push("masks " + sh + " " + neg);
// An accessor on the global object takes every store and read.
var log = [];
Object.defineProperty(globalThis, "acc", {
  get() { log.push("g"); return this._a | 0; },
  set(v) { log.push("s" + v); this._a = v * 2; },
  configurable: true,
});
for (var w = 0; w < 3; w++) acc++;
out.push("acc " + acc + " " + log.join(""));
// A value whose valueOf runs, and one that throws, in an accumulator's `| 0`.
var calls = 0, t = 0;
var obj = { valueOf() { calls++; return 3; } };
for (var u = 0; u < 4; u++) t = (obj | 0) + t | 0;
var bad = { valueOf() { throw new Error("boom " + calls); } };
try { for (var v = 0; v < 2; v++) t = (bad | 0); } catch (e) { out.push("threw " + e.message + " " + v + " " + t); }
out.push("valueOf " + t + " " + calls);
// A function stepping a global counter (the lite loop runs it).
var hits = 0;
function hit(n) { hits++; return n + hits; }
var hs = 0;
for (var z = 0; z < 5000; z++) hs = (hs + hit(z)) | 0;
out.push("hits " + hits + " " + hs);
// A do-while over a global, and a top-level `let` accumulator.
var dw = 0;
do { dw++; } while (dw < 7);
let L = 0;
for (let k2 = 0; k2 < 50; k2++) L = (L + k2) | 0;
out.push("dw " + dw + " L " + L);
console.log(out.join("\n"));
"##;

const EXPECTED: &str = "ints 1499376 17480 3000 3000 183\nkinds 40.5 1600 2147483660 20 8 number\nmasks -13375 -12360.75\nacc 14 gs1gs3gs7g\nthrew boom 4 0 12\nvalueOf 12 4\nhits 5000 25000000\ndw 7 L 1225";

#[test]
fn interp_global_runs_worker() {
    if std::env::var_os("ZIPP_GLOBAL_RUNS_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn interp_global_runs_match_node() {
    if std::env::var_os("ZIPP_GLOBAL_RUNS_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, vars) in [
        ("default", &[][..]),
        ("interpreter", &["ZIPP_NOJIT"][..]),
        ("interpreter-runs-off", &["ZIPP_NOJIT", "ZIPP_NO_GLOBAL_RUNS"][..]),
        ("interpreter-lite-off", &["ZIPP_NOJIT", "ZIPP_NO_LITE"][..]),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["interp_global_runs_worker", "--exact", "--nocapture"])
            .env("ZIPP_GLOBAL_RUNS_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_GLOBAL_RUNS")
            .env_remove("ZIPP_NO_LITE");
        for v in vars {
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