//! The split member-call form of a DataView getter call: `dv.getUint32(n(4))`,
//! whose argument is not order-transparent, reads the method first and calls
//! the captured value (GetProp + CallWithThis). Both halves have DataView lanes
//! (`dataview_proto_read`, `dataview_captured_get`); this battery pins them to
//! Node's answers through every way the read or the callee can change, in the
//! interpreter, under GC stress, and with the default tiers.

const SOURCE: &str = r#"
"use strict";
// Every call below passes its offset through `n(...)`, a call, so the strict
// lowering captures the method before the arguments (GetProp + CallWithThis).
function n(x) { return x; }
var buffer = new ArrayBuffer(32);
var bytes = new Uint8Array(buffer);
for (var i = 0; i < bytes.length; i++) bytes[i] = (i * 37 + 11) & 255;
var view = new DataView(buffer, 3, 20);

console.log([
  view.getInt8(n(0)), view.getUint8(n(1)),
  view.getInt16(n(1), true), view.getInt16(n(1), false),
  view.getUint16(n(2), true), view.getUint16(n(2)),
  view.getInt32(n(3), true), view.getInt32(n(3), false),
  view.getUint32(n(4), true), view.getUint32(n(4), false),
  view.getFloat32(n(5), true), view.getFloat32(n(5), false),
  view.getFloat64(n(7), true), view.getFloat64(n(7), false),
  view.getUint32(n(4), n(1)), view.getUint32(n(4), n(""))
].map(function (x) { return String(x); }).join("|"));

function caught(fn) {
  try { return "v:" + fn(); }
  catch (error) { return "e:" + error.constructor.name; }
}
console.log(caught(function () { return view.getUint32(n(16), true); }));
console.log(caught(function () { return view.getUint32(n(17), true); }));
console.log(caught(function () { return view.getUint16(n(-1), true); }));
console.log(caught(function () { return view.getUint16(n(2.75), true); }));
console.log(caught(function () { return view.getUint16(n("2"), true); }));
console.log(caught(function () { return view.getUint16(n({ valueOf: function () { return 3; } }), true); }));
console.log(caught(function () { return view.getUint16(); }));

// The method is read before the arguments run: an argument that replaces it
// does not change which function this call makes.
var intrinsicU32 = DataView.prototype.getUint32;
function swapIn(x) {
  DataView.prototype.getUint32 = function (offset) { return 9000 + offset; };
  return x;
}
console.log(view.getUint32(swapIn(4), true));
console.log(view.getUint32(n(4), true));
function swapBack(x) {
  DataView.prototype.getUint32 = intrinsicU32;
  return x;
}
console.log(view.getUint32(swapBack(4), true));
console.log(view.getUint32(n(4), true));

// Own shadows, deletion, and a re-pointed prototype.
view.getUint16 = function (offset) { return 8000 + offset; };
console.log(view.getUint16(n(2), false));
delete view.getUint16;
console.log(view.getUint16(n(2), false));
Object.setPrototypeOf(view, { getUint32: function (offset) { return 8100 + offset; } });
console.log(view.getUint32(n(4), true));
Object.setPrototypeOf(view, DataView.prototype);
console.log(view.getUint32(n(4), true));

// An accessor on the prototype runs, and the method it returns is called.
var intrinsicI8 = DataView.prototype.getInt8;
Object.defineProperty(DataView.prototype, "getInt8", {
  get: function () { return function (offset) { return "accessor-method|" + (7300 + offset); }; },
  configurable: true
});
var fresh = new DataView(buffer);
console.log(fresh.getInt8(n(0)));
Object.defineProperty(DataView.prototype, "getInt8", {
  value: intrinsicI8, writable: true, enumerable: false, configurable: true
});
console.log(fresh.getInt8(n(0)));

// A getter borrowed by another receiver acts on that receiver.
var other = new DataView(new ArrayBuffer(8));
new Uint8Array(other.buffer).set([9, 8, 7, 6, 5, 4, 3, 2]);
other.read = intrinsicU32;
console.log(other.read(n(0), true), other.read(n(4), false));
var plain = { read: intrinsicU32 };
console.log(caught(function () { return plain.read(n(0), true); }));
var typed = new Uint8Array(8);
typed.read = intrinsicU32;
console.log(caught(function () { return typed.read(n(0), true); }));

// Subclass instances and Reflect.construct views take the generic Get, then
// still call the intrinsic they found.
class DerivedView extends DataView {}
var derived = new DerivedView(other.buffer, 1, 6);
console.log(derived.getUint32(n(1), true));
DerivedView.prototype.getUint32 = function (offset) { return 9200 + offset; };
console.log(derived.getUint32(n(1), false));
delete DerivedView.prototype.getUint32;
console.log(derived.getUint32(n(1), false));

// A detached buffer.
var doomed = new ArrayBuffer(8);
var doomedView = new DataView(doomed);
console.log(doomedView.getUint8(n(0)));
doomed.transfer();
console.log(caught(function () { return doomedView.getUint8(n(0)); }));

// Strict equality keeps a call fused: these take the fused form's lane.
function fusedSum(dv, len) {
  var s = 0;
  for (var o = 0; o + 4 <= len; o += 4) {
    var le = (o >> 2) & 1;
    s = (s + dv.getUint32(o, le === 1) + dv.getUint16(o, le !== 1)) | 0;
  }
  return s;
}
console.log(fusedSum(other, 8));

// Under GC stress the dead views are reclaimed and their slots reused.
function churn(count) {
  var sum = 0;
  for (var i = 0; i < count; i++) {
    var b = new ArrayBuffer(8);
    new Uint8Array(b)[0] = i;
    var v = new DataView(b);
    sum = (sum + v.getUint8(n(0))) | 0;
  }
  return sum;
}
console.log(churn(64));

// A top-level loop over globals: every call here is split.
var dv = new DataView(bytes.buffer, 0, 32);
var bsum = 0;
for (var r = 0; r < 3; r++) {
  for (var o = 0; o < 28; o += 4) {
    var le = (o >> 2) & 1;
    var v = dv.getUint32(o, le === 1);
    bsum = (bsum + (v >>> 24) + (v & 255) + dv.getUint16(o, le === 0) + dv.getInt8(o + 2)) | 0;
  }
}
console.log(bsum);
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
fn dataview_split_call_worker() {
    if std::env::var_os("ZIPP_DV_SPLIT_WORKER").is_none() {
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
fn dataview_split_call_matches_node() {
    if std::env::var_os("ZIPP_DV_SPLIT_WORKER").is_some() {
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
            .args(["dataview_split_call_worker", "--exact", "--nocapture"])
            .env("ZIPP_DV_SPLIT_WORKER", "1")
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
