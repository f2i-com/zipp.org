//! `.length` answered from the internal slot (`Vm::length_fast`,
//! `Vm::ta_length_fast`). A string's, an Array's and a TypedArray's length is
//! not a property the shape-keyed inline cache can hold, so the interpreter's
//! `GetProp` used to miss the cache and take the generic member walk on every
//! read, and the TypedArray proof cost two side-table probes and two
//! prototype key lookups each time. Both now answer directly; the TypedArray
//! prototype proof is memoized under the two prototype versions.
//!
//! The probes pin what the shortcut must not change: an arguments object's
//! writable `length`, sparse and shrunk Arrays, String objects and ropes,
//! and for a TypedArray every way the inherited getter can stop being the
//! answer -- an own shadow, a re-pointed instance or kind prototype, a
//! subclass getter, an own `length` on the kind prototype, a replaced base
//! getter -- each also switched MID-LOOP so a warm memo has to notice, plus
//! resizable, length-tracking and detached buffers. Expected values come
//! from `node` (v24) running the same program.

const PROBES: &str = r#""use strict";
var res = "";
var N = 3000, i, t;
// 1 strings: flat, rope, non-ASCII, astral, a String object
var s = "hello", rope = "", uni = "café ☃", astral = "a😀b";
for (i = 0; i < 40; i++) rope += "ab";
var so = new String("boxed");
t = 0; for (i = 0; i < N; i++) t += s.length + rope.length + uni.length + astral.length + so.length;
res += ";s=" + t;
// 2 arrays: dense, growing, sparse, shrunk through `length`, a subclass
var a = [1, 2, 3], grow = [], sp = [];
sp[100000] = 1;
t = 0; for (i = 0; i < N; i++) { grow.push(i); t += a.length + grow.length + sp.length; }
sp.length = 7; a.length = 1;
class Sub extends Array {}
var sub = new Sub(); sub.push(1, 2, 3, 4);
res += ";a=" + t + "/" + sp.length + "/" + a.length + "/" + sub.length;
// 3 an arguments object's length is an ordinary writable property
function args() { var n = arguments.length; arguments.length = 9; return n * 100 + arguments.length; }
t = 0; for (i = 0; i < N; i++) t += args(1, 2, 3);
res += ";g=" + t;
// 4 typed arrays: every way the inherited getter can stop being the answer
var u8 = new Uint8Array(16), f64 = new Float64Array(5);
t = 0; for (i = 0; i < N; i++) t += u8.length + f64.length;
res += ";t=" + t;
var own = new Uint8Array(4); Object.defineProperty(own, "length", { value: 99 });
var reproto = new Uint8Array(4); Object.setPrototypeOf(reproto, { length: 77 });
var nul = new Uint8Array(4); Object.setPrototypeOf(nul, null);
class MyU8 extends Uint8Array { get length() { return 55; } }
var mine = new MyU8(4);
class PlainSub extends Uint8Array {}
var plain = new PlainSub(6);
t = 0; for (i = 0; i < N; i++) t += own.length + reproto.length + mine.length + plain.length;
res += ";o=" + t + "/" + nul.length;
// 5 the kind prototype gains an own `length` mid-loop, then loses it
var seen = "";
for (i = 0; i < N; i++) {
  if (i === 1000) Object.defineProperty(Uint8Array.prototype, "length", { get: function () { return 42; }, configurable: true });
  if (i === 2000) delete Uint8Array.prototype.length;
  t = u8.length + f64.length;
  if (i === 999 || i === 1000 || i === 1999 || i === 2000) seen += t + ",";
}
res += ";k=" + seen;
// 6 the base getter is replaced mid-loop: defineProperty, then restored
var base = Object.getPrototypeOf(Uint8Array.prototype);
var desc = Object.getOwnPropertyDescriptor(base, "length");
seen = "";
for (i = 0; i < N; i++) {
  if (i === 1000) Object.defineProperty(base, "length", { get: function () { return 7; }, configurable: true });
  if (i === 2000) Object.defineProperty(base, "length", desc);
  t = u8.length + f64.length;
  if (i === 999 || i === 1000 || i === 1999 || i === 2000) seen += t + ",";
}
res += ";b=" + seen;
// 7 the kind prototype is re-pointed mid-loop, then restored
seen = "";
for (i = 0; i < N; i++) {
  if (i === 1000) Object.setPrototypeOf(Float64Array.prototype, { length: 3 });
  if (i === 2000) Object.setPrototypeOf(Float64Array.prototype, base);
  t = u8.length + f64.length;
  if (i === 999 || i === 1000 || i === 1999 || i === 2000) seen += t + ",";
}
res += ";r=" + seen;
// 8 an own shadow added to an instance mid-loop
var late = new Int32Array(8); seen = "";
for (i = 0; i < N; i++) {
  if (i === 1500) Object.defineProperty(late, "length", { value: 123, configurable: true });
  t = late.length;
  if (i === 1499 || i === 1500) seen += t + ",";
}
res += ";l=" + seen;
// 9 resizable and detached buffers
var rab = new ArrayBuffer(8, { maxByteLength: 64 });
var tracking = new Uint8Array(rab), fixed = new Uint8Array(rab, 0, 8);
seen = tracking.length + "," + fixed.length + ",";
rab.resize(32); seen += tracking.length + "," + fixed.length + ",";
rab.resize(4); seen += tracking.length + "," + fixed.length + ",";
var gone = new Uint8Array(8); gone.buffer.transfer();
t = 0; for (i = 0; i < N; i++) t += gone.length + tracking.length;
res += ";d=" + seen + t;
// 10 an object that merely has a `length` field, null and undefined receivers
var o = { length: 12 }, p = Object.create({ length: 13 });
t = 0; for (i = 0; i < N; i++) t += o.length + p.length;
var errs = 0;
try { t += null.length; } catch (e) { errs += e instanceof TypeError ? 1 : 0; }
try { t += (void 0).length; } catch (e) { errs += e instanceof TypeError ? 10 : 0; }
res += ";x=" + t + "/" + errs + "/" + (5).length + "/" + true.length + "/" + (function (q, w) {}).length;
console.log(res.slice(1));
"#;

const EXPECTED: &str =
    "s=300000;a=304513500/7/1/4;g=927000;t=63000;o=711000/undefined;k=21,47,47,21,;b=21,14,14,21,;r=21,19,19,21,;l=8,123,;d=8,8,32,8,4,0,12000;x=75000/11/undefined/undefined/2";

#[test]
fn length_reads_match_node() {
    let out = zipp_vm::run(PROBES).expect("probes compile");
    assert!(out.error.is_none(), "probes threw: {:?}", out.error);
    assert_eq!(out.output.join("
"), EXPECTED);
}

/// The same answers from the interpreter alone, with every function compiled
/// on first entry, and with a collection at every safepoint.
#[test]
fn length_reads_probe_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (label, key) in [
        ("interpreter", "ZIPP_NOJIT"),
        ("forced-jit", "ZIPP_JIT_THRESHOLD"),
        ("gc-stress", "ZIPP_GC_STRESS"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("length_reads_match_node")
            .arg("--exact")
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
