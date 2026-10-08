//! The captured-method lane (`jit_captured_method`). Under the strict default
//! call order (B280) a member call whose argument is not order-transparent
//! lowers to `GetProp; <args>; CallWithThis`. The fused `CallMethod` form of
//! `m.get(k)`, `m.set(k, v)`, `s.has(v)`, `s.add(v)`, `m.delete(k)`,
//! `str.indexOf(t)`, `str.substring(a, b)`, `str.slice(a, b)` and
//! `hasOwn.call(o, k)` each run one dedicated intrinsic helper in the native
//! tiers; the captured form reached only the generic captured-callee call,
//! and its `f.call` read deopted until the loop was evicted. The lane serves
//! a captured callee through the same helpers when — and only when — it is
//! exactly the boot intrinsic a Get of that name on that receiver resolves to.
//!
//! These probes are HOT loops (the lane lives in the MEM region tier and
//! Tier C, not the interpreter) and pin the identity contract around the
//! proof: an override installed mid-loop is what later iterations call, an
//! override installed BY the argument leaves the captured intrinsic running
//! for that call, an own shadow, a replaced prototype, a subclass method and
//! another kind's intrinsic under the name all decline, and an Array receiver
//! under a string lane's name is never claimed. Expected values come from
//! `node` (v24) running the same program.

const PROBES: &str = r#""use strict";
var res = "";
// 1 Map / Set hot loops whose arguments are not order-transparent
var m = new Map(), s = new Set(), N = 3000, i, acc = 0;
for (i = 0; i < N; i++) { m.set("k" + i, i * 2); m.set(i, i + 1); s.add(i * 3); s.add("s" + i); }
for (i = 0; i < N; i++) {
  acc = (acc + m.get("k" + i) + m.get(i) + (m.has(i + N) ? 1 : 0) + (s.has(i * 3) ? 2 : 0) + (s.has("x" + i) ? 4 : 0)) | 0;
}
res += ";a=" + acc + "/" + m.size + "/" + s.size;
for (i = 0; i < N; i += 3) { m.delete("k" + i); s.delete(i * 3); }
res += ";b=" + m.size + "/" + s.size + "/" + m.delete(i + N) + "/" + s.delete("s" + 1);
// 2 an override installed mid-loop, BEFORE a capture: later iterations call it
var origGet = Map.prototype.get, hits = 0, over = 0;
for (i = 0; i < N; i++) {
  if (i === 2000) Map.prototype.get = function (k) { over++; return undefined; };
  if (m.get(i + 0) !== undefined) hits++;
}
Map.prototype.get = origGet;
res += ";c=" + hits + "/" + over;
// 3 an override installed BY THE ARGUMENT: that call still runs the captured intrinsic
function swapAt(k) { if (k === 2500) Map.prototype.get = function () { return "over"; }; return k; }
var seen = "", v;
for (i = 0; i < N; i++) { v = m.get(swapAt(i)); if (i >= 2498 && i < 2503) seen += v + ","; }
Map.prototype.get = origGet;
res += ";d=" + seen;
// 4 an own shadow appearing on the instance mid-loop
var m2 = new Map([[1, 10]]), sh = 0, t = 0;
for (i = 0; i < N; i++) { if (i === 1500) m2.get = function () { sh++; return 5; }; t += m2.get(i - i + 1); }
res += ";e=" + t + "/" + sh;
// 5 another kind's intrinsic under the name: Set.prototype.has on a Map is a TypeError
var m3 = new Map([[1, 1]]), errs = 0, oks = 0;
for (i = 0; i < N; i++) {
  if (i === 1500) m3.has = Set.prototype.has;
  try { if (m3.has(i - i + 1)) oks++; } catch (e) { if (e instanceof TypeError) errs++; }
}
res += ";f=" + oks + "/" + errs;
// 6 a replaced prototype and a subclass instance
var m4 = new Map([[1, 7]]), viaProto = 0;
for (i = 0; i < N; i++) { if (i === 1500) Object.setPrototypeOf(m4, { get: function () { viaProto++; return 1; } }); t += m4.get(i - i + 1); }
class MyMap extends Map { get(k) { return 100 + k; } }
var mm = new MyMap([[1, 1]]);
for (i = 0; i < N; i++) t += mm.get(i - i + 1);
res += ";g=" + t + "/" + viaProto;
// 7 string indexOf / substring / slice
var str = "hello world, hello zipp", cnt = 0, needle = "wor", from = 3, to = 9;
for (i = 0; i < N; i++) {
  cnt += str.indexOf(needle) + str.substring(from, to).length + str.slice(from).length
    + str.substring(to).length + str.slice(from, to + 1).length + str.slice(-from).length;
}
res += ";h=" + cnt + "/" + str.substring(to, from) + "/" + str.slice(to, from).length;
var origIdx = String.prototype.indexOf, c2 = 0;
for (i = 0; i < N; i++) { if (i === 2000) String.prototype.indexOf = function () { return 100; }; c2 += str.indexOf(needle); }
String.prototype.indexOf = origIdx;
res += ";i=" + c2;
var uni = "café ☃ snow", c4 = 0, un = "☃";
for (i = 0; i < N; i++) c4 += uni.indexOf(un) + uni.substring(from, to).length + uni.slice(from).length;
res += ";j=" + c4;
// 8 an Array receiver under the same names: no string lane may claim it
var arr = [5, 6, 7, 8], c3 = 0, want = 7;
for (i = 0; i < N; i++) c3 += arr.indexOf(want) + arr.slice(from - 2).length;
res += ";k=" + c3;
// 9 hasOwnProperty.call
var hasOwn = Object.prototype.hasOwnProperty, sp = [], own = 0;
for (i = 0; i < 400; i += 7) sp[i] = i;
for (i = 0; i < N; i++) { if (hasOwn.call(sp, i + 1)) own++; }
res += ";l=" + own;
var origCall = Function.prototype.call, own2 = 0, viaOver = 0;
for (i = 0; i < N; i++) {
  if (i === 2000) Function.prototype.call = function (self, k) { viaOver++; return false; };
  if (hasOwn.call(sp, i + 1)) own2++;
}
Function.prototype.call = origCall;
res += ";m=" + own2 + "/" + viaOver;
var own4 = 0, viaOwn = 0;
function myHas(k) { return k % 2 === 0; }
for (i = 0; i < N; i++) {
  if (i === 1500) myHas.call = function (self, k) { viaOwn++; return true; };
  if (myHas.call(sp, i + 1)) own4++;
}
res += ";n=" + own4 + "/" + viaOwn;
// 10 whole-function forms: arguments that are arithmetic over parameters
function probe(k) { return hasOwn.call(sp, k + 1); }
var own3 = 0; for (i = 0; i < N; i++) if (probe(i)) own3++;
function mapSum(map, n) { var r = 0; for (var q = 0; q < n; q++) { r = (r + map.get(q + 0)) | 0; if (map.has(q + n)) r++; } return r; }
function setFill(set, n) { for (var q = 0; q < n; q++) set.add(q * 2 + 1); return set.size; }
function fnCall(f, n) { var r = 0; for (var q = 0; q < n; q++) r += f.call(null, q + 1); return r; }
function strScan(text, n) { var r = 0; for (var q = 0; q < n; q++) r += text.indexOf(text[q % 5]) + text.slice(q % 7, 9).length; return r; }
res += ";o=" + own3 + "/" + mapSum(m, N) + "/" + setFill(new Set(), N) + "/" + fnCall(function (x) { return x * 2; }, N) + "/" + strScan(str, N);
console.log(res.slice(1));
"#;

const EXPECTED: &str = "a=13504500/6000/6000;b=5000/5000/false/true;c=2000/1000;d=2499,2500,2501,over,over,;e=22500/1500;f=1500/1500;g=337500/1500;h=168000/lo wor/0;i=112000;j=57000;k=15000;l=57;m=57/1000;n=2250/1500;o=57/4501500/3000/9003000/23406";

#[test]
fn captured_method_lane_matches_node() {
    let out = zipp_vm::run(PROBES).expect("probes compile");
    assert!(out.error.is_none(), "probes threw: {:?}", out.error);
    assert_eq!(out.output.join("
"), EXPECTED);
}

/// The contract must not depend on the tier or on the lane itself: the same
/// answers with the lane off, the JIT off, every function compiled on first
/// entry, the pre-B280 fused lowering, and a collection at every safepoint.
#[test]
fn captured_method_lane_probe_modes() {
    let exe = std::env::current_exe().expect("test exe path");
    for (label, key) in [
        ("lane-off", "ZIPP_NO_CAPTURED_METHOD_LANE"),
        ("interpreter", "ZIPP_NOJIT"),
        ("forced-jit", "ZIPP_JIT_THRESHOLD"),
        ("relaxed-order", "ZIPP_RELAXED_CALL_ORDER"),
        ("gc-stress", "ZIPP_GC_STRESS"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("captured_method_lane_matches_node")
            .arg("--exact")
            .env_remove("ZIPP_RELAXED_CALL_ORDER")
            .env_remove("ZIPP_STRICT_CALL_ORDER")
            .env_remove("ZIPP_NO_CAPTURED_METHOD_LANE")
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

/// The lane is actually emitted for these sites on x86-64 (a probe that only
/// ever ran the generic call would pass the two tests above vacuously), and
/// its off-switch removes it.
#[cfg(all(target_arch = "x86_64", feature = "jit"))]
#[test]
fn captured_method_lane_is_emitted() {
    let exe = std::env::current_exe().expect("test exe path");
    let run = |off: bool| {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("captured_method_lane_matches_node")
            .arg("--exact")
            .arg("--nocapture")
            .env_remove("ZIPP_RELAXED_CALL_ORDER")
            .env_remove("ZIPP_STRICT_CALL_ORDER")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_NO_CAPTURED_METHOD_LANE")
            .env("ZIPP_JITLOG", "1");
        if off {
            cmd.env("ZIPP_NO_CAPTURED_METHOD_LANE", "1");
        }
        let out = cmd.output().expect("spawn the logging child");
        assert!(out.status.success(), "logging child failed");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let log = run(false);
    for name in ["get", "has", "set", "add", "delete", "indexOf", "substring", "slice", "call"] {
        assert!(
            log.contains(&format!("captured method lane `{name}` emitted")),
            "no captured `{name}` lane was emitted"
        );
    }
    assert!(
        !run(true).contains("captured method lane"),
        "ZIPP_NO_CAPTURED_METHOD_LANE left a lane in place"
    );
}
