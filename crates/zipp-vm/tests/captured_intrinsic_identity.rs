//! A captured callee (`recv.name(args)` lowered as GetProp then CallWithThis)
//! runs the array/string intrinsic lane only when the captured Value IS the
//! boot intrinsic of that name; what the prototype holds by then does not
//! matter, since the spec calls the captured Value. Covered against Node:
//! an override installed by an argument (the captured intrinsic still runs,
//! the next call sees the override), an unmemoised name (`map`), an intrinsic
//! of another name stored under this one, own shadows on the receiver, a
//! `charCodeAt` deleted mid-loop, an accessor method, restored intrinsics,
//! computed callees through `.call`, and a frozen receiver.

const SOURCE: &str = r#"
"use strict";
// Captured-callee intrinsic calls (`recv.name(args)` lowered as GetProp + CallWithThis) and the
// fused charCodeAt/push lanes, when the prototype slot changes around the call. Compared with Node.
var out = [];
var origCCA = String.prototype.charCodeAt, origPush = Array.prototype.push;
var origIdx = String.prototype.indexOf, origMap = Array.prototype.map, origUp = String.prototype.toUpperCase;
function hot(n, f) { var r; for (var i = 0; i < n; i++) r = f(i); return r; }
// 1. Override installed BY an argument: the captured intrinsic still runs.
var s = "hello";
out.push("argOverride=" + hot(3000, function (i) {
  return s.charCodeAt((i === 2999 ? (String.prototype.charCodeAt = function () { return -1; }, 1) : 1));
}));
out.push("after=" + s.charCodeAt(1));
String.prototype.charCodeAt = origCCA;
// 2. The same for push and an unmemoised name (map).
var a = [];
out.push("pushArg=" + hot(3000, function (i) {
  return a.push(i === 2999 ? (Array.prototype.push = function () { return "P"; }, i) : i);
}) + " len=" + a.length + " next=" + a.push(1));
Array.prototype.push = origPush;
var m = [1, 2, 3];
out.push("mapArg=" + hot(2000, function (i) {
  return m.map(i === 1999 ? (Array.prototype.map = function () { return "M"; }, function (x) { return x * 2; }) : function (x) { return x + i; }).join(",");
}) + " " + m.map(function (x) { return x; }));
Array.prototype.map = origMap;
// 3. An intrinsic of another name stored under this one.
String.prototype.charCodeAt = origIdx;
out.push("swapped=" + hot(2000, function () { return "abcabc".charCodeAt("c"); }));
String.prototype.charCodeAt = origCCA;
Array.prototype.push = Array.prototype.unshift;
var b = [9];
hot(10, function (i) { return b.push(i); });
out.push("swapPush=" + b.join(","));
Array.prototype.push = origPush;
// 4. Own shadows on the receiver.
var c = [];
c.push = function (x) { return "own" + x; };
out.push("ownPush=" + hot(2000, function (i) { return c.push(i); }) + " len=" + c.length);
var boxed = new String("xyz");
boxed.charCodeAt = function () { return 7; };
out.push("ownCCA=" + hot(2000, function () { return boxed.charCodeAt(0); }));
// 5. Deleted and accessor slots mid-loop.
var t = "q";
var res = [];
for (var i = 0; i < 3000; i++) {
  if (i === 1500) delete String.prototype.charCodeAt;
  try { res.push(t.charCodeAt(0)); } catch (e) { res.push(e.constructor.name); }
}
out.push("deleted=" + res[1499] + "," + res[1500] + "," + res[2999]);
String.prototype.charCodeAt = origCCA;
Object.defineProperty(String.prototype, "toUpperCase", { get: function () { return function () { return "G"; }; }, configurable: true });
out.push("accessor=" + hot(2000, function () { return "abc".toUpperCase(); }));
Object.defineProperty(String.prototype, "toUpperCase", { value: origUp, writable: true, configurable: true });
out.push("restored=" + "abc".toUpperCase() + "abc".charCodeAt(2) + [].push(1, 2));
// 6. Calls with a computed callee (captured lane) on many receivers, then frozen arrays.
var calls = 0;
for (var k = 0; k < 5000; k++) {
  var arr = [k];
  var f = arr.push;
  calls += f.call(arr, k, k);
  calls += "abcdef".indexOf("d", k % 3);
}
out.push("calls=" + calls);
var fz = Object.freeze([1]);
try { hot(2000, function (i) { return fz.push(i); }); } catch (e) { out.push("frozen " + e.constructor.name); }
for (var q = 0; q < out.length; q++) console.log(out[q]);
"#;

fn output_from_node(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
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

#[test]
fn captured_intrinsic_identity_worker() {
    if std::env::var_os("ZIPP_CAPTURED_INTRINSIC_IDENTITY_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    assert_eq!(out.output, output_from_node(SOURCE));
}

#[test]
fn captured_intrinsic_identity_matches_node() {
    if std::env::var_os("ZIPP_CAPTURED_INTRINSIC_IDENTITY_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit, gc_stress) in [
        ("interpreter", true, false),
        ("interpreter-gc-stress", true, true),
        ("default-tiers", false, false),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["captured_intrinsic_identity_worker", "--exact", "--nocapture"])
            .env("ZIPP_CAPTURED_INTRINSIC_IDENTITY_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS");
        if nojit {
            command.env("ZIPP_NOJIT", "1");
        }
        if gc_stress {
            command.env("ZIPP_GC_STRESS", "1");
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
