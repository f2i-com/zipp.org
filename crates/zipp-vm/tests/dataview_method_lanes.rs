//! DataView accessors called from compiled code's method fallback (`Vm::dataview_call_lanes`):
//! the numeric getters and setters and the BigInt accessors, with a replaced
//! prototype method, an own shadow, a subclass, another prototype, a fractional
//! or negative offset, an out-of-bounds offset, a BigInt setter given a Number
//! and a detached buffer. Run interpreted, at the default thresholds, with every
//! function compiled at once, under GC stress and with the loop-head entry off;
//! the expected lines are Node 24's.

const EXPECTED: &str = "sum 2663467
bigs 18416880325264776349309
sum 2664467
bigs 18416880325264776349309
sum 2666467
bigs 18416880325264776349309
replaced 1683467
restored 2680467
own-shadow 1686667
subclass 1996800
set-proto 24000
fraction 8403
negative RangeError
bounds RangeError
big-number TypeError
detach TypeError";

#[test]
fn dataview_method_lanes_child() {
    if std::env::var_os("ZIPP_DV_LANES_CHILD").is_none() {
        return;
    }
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    let lines: Vec<&str> = out.output.iter().flat_map(|l| l.lines()).collect();
    assert_eq!(lines, EXPECTED.lines().collect::<Vec<_>>());
}

#[test]
fn dataview_calls_match_the_interpreter() {
    if std::env::var_os("ZIPP_DV_LANES_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("default-thresholds", None),
        ("compile-at-once", Some(("ZIPP_JIT_THRESHOLD", "1"))),
        ("gc-stress", Some(("ZIPP_GC_STRESS", "1"))),
        ("osr-off", Some(("ZIPP_NO_TIERC_OSR", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "dataview_method_lanes_child", "--nocapture"])
            .env("ZIPP_DV_LANES_CHILD", "1")
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

const PROGRAM: &str = r####"// DataView accessors called from compiled code's method fallback: the
// numeric getters and setters and the BigInt accessors, with what must send
// a call elsewhere: a replaced prototype method, an own property shadowing
// one, a view given another prototype, a non-integral or negative offset, an
// out-of-bounds offset (RangeError), a detached buffer (TypeError), and a
// BigInt setter given a Number (TypeError).
"use strict";
const out = [];
function fill(dv, n, r) {
  for (let i = 0; i < n; i++) {
    dv.setUint32(i * 16, (i * 2654435761 + r) >>> 0, (i & 1) === 0);
    dv.setInt16(i * 16 + 4, i - 500, true);
    dv.setFloat64(i * 16 + 8, i / 3 + r);
  }
}
function sum(dv, n) {
  let t = 0;
  for (let i = 0; i < n; i++) {
    t += dv.getUint32(i * 16, (i & 1) === 0) % 1000;
    t += dv.getInt16(i * 16 + 4, true);
    t += Math.floor(dv.getFloat64(i * 16 + 8));
  }
  return t;
}
function bigs(dv, n) {
  let t = 0n;
  for (let i = 0; i < n; i++) dv.setBigInt64(i * 8, BigInt(i) * 7919n - 3n, true);
  for (let i = 0; i < n; i++) t += dv.getBigInt64(i * 8, true) ^ dv.getBigUint64(i * 8, false);
  return t;
}
function attempt(name, f) {
  try {
    out.push(name + " " + f());
  } catch (e) {
    out.push(name + " " + e.constructor.name);
  }
}
const N = 2000;
for (let round = 0; round < 3; round++) {
  const buf = new ArrayBuffer(N * 16);
  const dv = new DataView(buf);
  fill(dv, N, round);
  out.push("sum " + sum(dv, N));
  out.push("bigs " + bigs(new DataView(new ArrayBuffer(N * 8)), N));
}
const dv = new DataView(new ArrayBuffer(N * 16));
fill(dv, N, 9);
const orig = DataView.prototype.getInt16;
DataView.prototype.getInt16 = function (p, le) { return 1; };
attempt("replaced", () => sum(dv, N));
DataView.prototype.getInt16 = orig;
attempt("restored", () => sum(dv, N));
const shadow = new DataView(dv.buffer);
shadow.getUint32 = () => 2;
attempt("own-shadow", () => sum(shadow, N));
class MyView extends DataView {
  getFloat64(p) { return 0.5; }
}
attempt("subclass", () => sum(new MyView(dv.buffer), N));
const proto = new DataView(dv.buffer);
Object.setPrototypeOf(proto, { getUint32: () => 3, getInt16: () => 4, getFloat64: () => 5 });
attempt("set-proto", () => sum(proto, N));
attempt("fraction", () => { let t = 0; for (let i = 0; i < 100; i++) t += dv.getUint8(i + 0.5); return t; });
attempt("negative", () => { let t = 0; for (let i = 0; i < 100; i++) t += dv.getUint8(i - 50); return t; });
attempt("bounds", () => { let t = 0; for (let i = 0; i < N * 16 + 100; i += 7) t += dv.getUint8(i); return t; });
attempt("big-number", () => { const v = new DataView(new ArrayBuffer(64)); for (let i = 0; i < 100; i++) v.setBigInt64(0, i < 90 ? BigInt(i) : i); return v.getBigInt64(0); });
const tb = new ArrayBuffer(64);
const tv = new DataView(tb);
attempt("detach", () => { let t = 0; for (let i = 0; i < 100; i++) { if (i === 60) tb.transfer(); t += tv.getUint8(i % 8); } return t; });
console.log(out.join("\n"));
"####;
