//! Focused semantic battery for the interpreter's pristine DataView getter
//! lane, including natural generic fallbacks and live resizable-buffer guards.

const SOURCE: &str = r#"
"use strict";
var buffer = new ArrayBuffer(32);
var bytes = new Uint8Array(buffer);
for (var i = 0; i < bytes.length; i++) bytes[i] = (i * 37 + 11) & 255;
var view = new DataView(buffer, 3, 20);

// Every direct numeric kind, both byte orders, and an unaligned view offset.
console.log([
  view.getInt8(0), view.getUint8(1),
  view.getInt16(1, true), view.getInt16(1, false),
  view.getUint16(2, true), view.getUint16(2, false),
  view.getInt32(3, true), view.getInt32(3, false),
  view.getUint32(4, true), view.getUint32(4, false),
  view.getFloat32(5, true), view.getFloat32(5, false),
  view.getFloat64(7, true), view.getFloat64(7, false)
].map(function (x) { return String(x); }).join("|"));

function caught(fn) {
  try { return "v:" + fn(); }
  catch (error) { return "e:" + error.constructor.name; }
}
console.log(caught(function () { return view.getUint32(16, true); }));
console.log(caught(function () { return view.getUint32(17, true); }));
console.log(caught(function () { return view.getUint16(-1, true); }));
console.log(view.getUint16(2.75, true)); // generic ToIndex fallback

// An own shadow permanently invalidates the receiver's birth-version proof.
// Overwriting the existing side-table slot does not bump the heap version, so
// both writes must still remain generic after the first structural bump.
view.getUint16 = function (offset, little) {
  return 8000 + offset + (little ? 1 : 0);
};
console.log(view.getUint16(2, false));
view.getUint16 = function (offset, little) {
  return 8100 + offset + (little ? 1 : 0);
};
console.log(view.getUint16(2, true));
delete view.getUint16;
console.log(view.getUint16(2, false));

var ordinaryProto = {
  getUint32: function (offset, little) {
    return 9000 + offset + (little ? 1 : 0);
  }
};
Object.setPrototypeOf(view, ordinaryProto);
console.log(view.getUint32(4, true));
Object.setPrototypeOf(view, DataView.prototype);
console.log(view.getUint32(4, true));

// Reflect.construct installs a non-default prototype after the DataView is
// allocated. Restoring the intrinsic prototype later must not resurrect the
// birth token. A derived `super()` takes a separate clone/replace path and must
// likewise retain its subclass lookup semantics.
var reflectBuffer = new ArrayBuffer(8);
new Uint8Array(reflectBuffer).set([1, 2, 3, 4, 5, 6, 7, 8]);
function AlternateView() {}
AlternateView.prototype = {
  getUint32: function (offset, little) { return 9100 + offset + (little ? 1 : 0); }
};
var reflected = Reflect.construct(DataView, [reflectBuffer], AlternateView);
console.log(reflected.getUint32(1, true));
Object.setPrototypeOf(reflected, DataView.prototype);
console.log(reflected.getUint32(1, true));

class DerivedView extends DataView {}
var derived = new DerivedView(reflectBuffer, 1, 6);
console.log(derived.getUint32(1, true));
DerivedView.prototype.getUint32 = function (offset, little) {
  return 9200 + offset + (little ? 1 : 0);
};
console.log(derived.getUint32(1, false));
delete DerivedView.prototype.getUint32;
console.log(derived.getUint32(1, false));

// Under ZIPP_GC_STRESS the dead buffer/view pair is reclaimed between loop
// iterations and heap slots are reused. Every fresh occupant must capture its
// own post-allocation version rather than inheriting an earlier view's token.
function churnDataViews(n) {
  var sum = 0;
  for (var i = 0; i < n; i++) {
    var churnBuffer = new ArrayBuffer(8);
    new Uint8Array(churnBuffer)[0] = i;
    var churnView = new DataView(churnBuffer);
    sum = (sum + churnView.getUint8(0)) | 0;
  }
  return sum;
}
console.log(churnDataViews(64));

// Existing value stores preserve the hidden shape, so exact intrinsic Value
// bits are the second half of the proof. Exercise all eight direct getter
// names: every replacement must run, and restoring the exact function object
// must make the optimized answer valid again.
var protoView = new DataView(reflectBuffer);
var intrinsicI8 = DataView.prototype.getInt8;
DataView.prototype.getInt8 = function () { return 7001; };
console.log(protoView.getInt8(0));
DataView.prototype.getInt8 = intrinsicI8;
console.log(protoView.getInt8(0));

var intrinsicU8 = DataView.prototype.getUint8;
DataView.prototype.getUint8 = function () { return 7002; };
console.log(protoView.getUint8(0));
DataView.prototype.getUint8 = intrinsicU8;
console.log(protoView.getUint8(0));

var intrinsicI16 = DataView.prototype.getInt16;
DataView.prototype.getInt16 = function () { return 7003; };
console.log(protoView.getInt16(0, true));
DataView.prototype.getInt16 = intrinsicI16;
console.log(protoView.getInt16(0, true));

var intrinsicU16 = DataView.prototype.getUint16;
DataView.prototype.getUint16 = function () { return 7004; };
console.log(protoView.getUint16(0, true));
DataView.prototype.getUint16 = intrinsicU16;
console.log(protoView.getUint16(0, true));

var intrinsicI32 = DataView.prototype.getInt32;
DataView.prototype.getInt32 = function () { return 7005; };
console.log(protoView.getInt32(0, true));
DataView.prototype.getInt32 = intrinsicI32;
console.log(protoView.getInt32(0, true));

var intrinsicU32 = DataView.prototype.getUint32;
DataView.prototype.getUint32 = function () { return 7006; };
console.log(protoView.getUint32(0, true));
DataView.prototype.getUint32 = intrinsicU32;
console.log(protoView.getUint32(0, true));

var intrinsicF32 = DataView.prototype.getFloat32;
DataView.prototype.getFloat32 = function () { return 7007; };
console.log(protoView.getFloat32(0, true));
DataView.prototype.getFloat32 = intrinsicF32;
console.log(protoView.getFloat32(0, true));

var intrinsicF64 = DataView.prototype.getFloat64;
DataView.prototype.getFloat64 = function () { return 7008; };
console.log(protoView.getFloat64(0, true));
DataView.prototype.getFloat64 = intrinsicF64;
console.log(protoView.getFloat64(0, true));

// An unrelated append changes the complete prototype shape. Removing it drops
// the map permanently to dictionary mode; both states must fall back cleanly.
DataView.prototype.unrelatedProofControl = 1;
console.log(protoView.getUint32(0, true));
delete DataView.prototype.unrelatedProofControl;
console.log(protoView.getUint32(0, true));

// Descriptor mutation (including an accessor) must be observed. Restoring the
// original descriptor remains generic because dictionary mode is fail-closed.
var int16Descriptor = Object.getOwnPropertyDescriptor(DataView.prototype, "getInt16");
Object.defineProperty(DataView.prototype, "getInt16", {
  configurable: true,
  get: function () { return function () { return 7300; }; }
});
console.log("accessor-method|" + protoView.getInt16(0, true));
Object.defineProperty(DataView.prototype, "getInt16", int16Descriptor);
console.log(protoView.getInt16(0, true));

// A deleted then reinserted slot moves to the end of the ordinary property
// map. The cached-shape proof must decline, while generic resolution still calls
// the restored intrinsic native correctly.
delete DataView.prototype.getUint32;
DataView.prototype.getUint32 = intrinsicU32;
console.log(protoView.getUint32(0, true));

// Detachment is checked live after ToIndex and remains a TypeError.
buffer.transfer();
console.log(caught(function () { return view.getUint32(0, true); }));
"#;

/// Diagnostic parity across a successful direct hit, a committed direct throw,
/// and a fractional-index decline that generic DataView dispatch completes.
/// The histogram is process-global, so this runs alone in a child process.
const BUILTIN_STATS_SOURCE: &str = r#"
"use strict";
function caught(fn) {
  try { return "v:" + fn(); }
  catch (error) { return "e:" + error.constructor.name; }
}
var buffer = new ArrayBuffer(8);
new Uint8Array(buffer).set([1, 2, 3, 4, 5, 6, 7, 8]);
var view = new DataView(buffer);
console.log(view.getUint8(0));
console.log(caught(function () { return view.getUint16(99, true); }));
console.log(view.getUint16(0.5, true));

var detachedBuffer = new ArrayBuffer(4);
var detachedView = new DataView(detachedBuffer);
detachedBuffer.transfer();
console.log(caught(function () { return detachedView.getUint32(0, true); }));
"#;

/// Auto-length DataViews store their construction-time length in the heap but
/// must use the backing buffer's LIVE remaining length for every operation.
/// This program crosses both sides of the bug: a shrink with the view offset
/// still valid, a shrink past the offset, and a grow beyond the initial size.
/// Fixed-length views are the control. The hot loops also make the same source
/// exercise the DataView pin and JIT getter helper in JIT-enabled child modes.
const TRACKING_SOURCE: &str = r#"
"use strict";
function caught(fn) {
  try { return "v:" + fn(); }
  catch (error) { return "e:" + error.constructor.name; }
}

var rab = new ArrayBuffer(24, { maxByteLength: 64 });
var bytes = new Uint8Array(rab);
for (var i = 0; i < bytes.length; i++) bytes[i] = (i * 17 + 3) & 255;
var tracking = new DataView(rab, 8);
var fixed = new DataView(rab, 8, 8);

function hotTracking(pos, n) {
  var sum = 0;
  for (var i = 0; i < n; i++) sum = (sum + tracking.getUint16(pos, true)) | 0;
  return sum;
}
function hotFixed(pos, n) {
  var sum = 0;
  for (var i = 0; i < n; i++) sum = (sum + fixed.getUint16(pos, true)) | 0;
  return sum;
}

console.log("initial|" + tracking.byteLength + "|" + fixed.byteLength + "|" +
            hotTracking(2, 5000) + "|" + hotFixed(2, 5000));

// The tracking view remains in-bounds and becomes six bytes long. The fixed
// eight-byte window no longer fits and is wholly out of bounds.
rab.resize(14);
console.log("shrink-valid|" + tracking.byteLength + "|" + tracking.byteOffset + "|" +
            caught(function () { return hotTracking(4, 5000); }) + "|" +
            caught(function () { return tracking.getUint16(5, true); }) + "|" +
            caught(function () { return fixed.getUint16(0, true); }));

// offset == buffer length is still an in-bounds, zero-length tracking view;
// an element access is therefore RangeError, not TypeError.
rab.resize(8);
console.log("zero|" + tracking.byteLength + "|" + tracking.byteOffset + "|" +
            caught(function () { return tracking.getUint8(0); }));

// Once the live buffer is shorter than byteOffset, both accessors and methods
// report an out-of-bounds view (TypeError).
rab.resize(7);
console.log("shrink-invalid|" +
            caught(function () { return tracking.byteLength; }) + "|" +
            caught(function () { return tracking.byteOffset; }) + "|" +
            caught(function () { return hotTracking(0, 1); }) + "|" +
            caught(function () { tracking.setUint8(0, 1); return "ok"; }));

// Regrowth revives both views. The auto-length view can address bytes beyond
// its construction-time 16-byte length; the fixed view stays eight bytes.
rab.resize(40);
tracking.setUint16(24, 0x3412, true);
tracking.setUint16(30, 0x7856, true);
console.log("grow|" + tracking.byteLength + "|" + fixed.byteLength + "|" +
            hotTracking(24, 5000) + "|" + hotFixed(0, 5000) + "|" +
            caught(function () { return tracking.getUint16(30, true); }) + "|" +
            caught(function () { return tracking.getUint16(31, true); }) + "|" +
            caught(function () { return fixed.getUint8(8); }));

// ToIndex and value conversion may resize the buffer. Generic get/set paths
// must derive the tracking length only after the relevant coercion.
rab.resize(24);
new Uint8Array(rab)[8] = 91;
var coerciveIndex = { valueOf: function () { rab.resize(12); return 0; } };
console.log("coerce-get|" + caught(function () { return tracking.getUint8(coerciveIndex); }) +
            "|" + tracking.byteLength);

rab.resize(24);
var coerciveValue = { valueOf: function () { rab.resize(10); return 171; } };
console.log("coerce-set-valid|" +
            caught(function () { tracking.setUint8(1, coerciveValue); return tracking.getUint8(1); }) +
            "|" + tracking.byteLength);

rab.resize(24);
var invalidatingValue = { valueOf: function () { rab.resize(7); return 1; } };
console.log("coerce-set-invalid|" +
            caught(function () { tracking.setUint8(0, invalidatingValue); return "ok"; }));
"#;

/// Child-realm method identity and a foreign newTarget. The Zipp spelling uses
/// the test262-compatible realm host; Node's `vm` module below is its oracle.
const REALM_SOURCE: &str = r#"
"use strict";
var child = $262.createRealm().global;
child.eval(`
  var b = new ArrayBuffer(8);
  new Uint8Array(b).set([1, 2, 3, 4, 5, 6, 7, 8]);
  var v = new DataView(b);
  DataView.prototype.getUint32 = function (offset, little) {
    return 9400 + offset + (little ? 1 : 0);
  };
  this.childResult = v.getUint32(2, true);
  this.ForeignView = function ForeignView() {};
`);
console.log(child.childResult);

var mainBuffer = new ArrayBuffer(8);
new Uint8Array(mainBuffer).set([1, 2, 3, 4, 5, 6, 7, 8]);
child.ForeignView.prototype = DataView.prototype;
var foreignNewTarget = Reflect.construct(DataView, [mainBuffer], child.ForeignView);
console.log(foreignNewTarget.getUint32(0, true));
"#;

const NODE_REALM_SOURCE: &str = r#"
"use strict";
const vm = require("node:vm");
const context = vm.createContext({});
const childResult = vm.runInContext(`
  var b = new ArrayBuffer(8);
  new Uint8Array(b).set([1, 2, 3, 4, 5, 6, 7, 8]);
  var v = new DataView(b);
  DataView.prototype.getUint32 = function (offset, little) {
    return 9400 + offset + (little ? 1 : 0);
  };
  this.ForeignView = function ForeignView() {};
  v.getUint32(2, true);
`, context);
console.log(childResult);

var mainBuffer = new ArrayBuffer(8);
new Uint8Array(mainBuffer).set([1, 2, 3, 4, 5, 6, 7, 8]);
var ForeignView = context.ForeignView;
ForeignView.prototype = DataView.prototype;
var foreignNewTarget = Reflect.construct(DataView, [mainBuffer], ForeignView);
console.log(foreignNewTarget.getUint32(0, true));
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
fn dataview_interp_get_fast_semantic_worker() {
    if std::env::var_os("ZIPP_DV_INTERP_GET_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    // A failed pristine proof must enter an accessor-aware OrdinaryGet. The
    // getter returns the method; CallMethod must then invoke that returned
    // callable rather than expose the raw getter or returned function object.
    assert!(
        out.output.iter().any(|line| line == "accessor-method|7300"),
        "DataView prototype accessor method was not called: {:?}",
        out.output
    );
    assert_eq!(out.output, output_from_node(SOURCE));

    let realm = zipp_vm::run(REALM_SOURCE).expect("realm source compiles");
    assert!(
        realm.error.is_none(),
        "unexpected realm runtime error: {:?}",
        realm.error
    );
    assert_eq!(realm.output, output_from_node(NODE_REALM_SOURCE));
}

#[test]
fn dataview_interp_get_and_gc_match_node() {
    if std::env::var_os("ZIPP_DV_INTERP_GET_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, gc_stress) in [
        ("pristine-proof", false),
        ("pristine-proof-gc-stress", true),
    ] {
        let mut command = std::process::Command::new(&exe);
        command
            .args([
                "dataview_interp_get_fast_semantic_worker",
                "--exact",
                "--nocapture",
            ])
            .env("ZIPP_DV_INTERP_GET_WORKER", "1")
            .env("ZIPP_NOJIT", "1")
            .env_remove("ZIPP_GC_STRESS");
        if gc_stress {
            command.env("ZIPP_GC_STRESS", "1");
        }
        let out = command.output().expect("spawn semantic worker");
        assert!(
            out.status.success(),
            "{mode} interpreter mode diverged:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn dataview_interp_get_builtin_stats_worker() {
    if std::env::var_os("ZIPP_DV_BUILTIN_STATS_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(BUILTIN_STATS_SOURCE).expect("stats source compiles");
    assert!(
        out.error.is_none(),
        "unexpected stats error: {:?}",
        out.error
    );
    assert_eq!(out.output, output_from_node(BUILTIN_STATS_SOURCE));

    let stats = zipp_vm::builtin_stats();
    let calls = |name: &str| {
        stats
            .iter()
            .filter(|(kind, candidate, _)| *kind == "dataview" && candidate == name)
            .map(|(_, _, calls)| *calls)
            .sum::<u64>()
    };
    assert_eq!(calls("getUint8"), 1, "successful direct call counted once");
    assert_eq!(
        calls("getUint16"),
        2,
        "direct RangeError and generic fractional fallback counted once each"
    );
    assert_eq!(
        calls("getUint32"),
        1,
        "committed detached-buffer throw counted once"
    );
    let total: u64 = stats
        .iter()
        .filter(|(kind, _, _)| *kind == "dataview")
        .map(|(_, _, calls)| *calls)
        .sum();
    assert_eq!(
        total, 4,
        "no direct decline may double-count downstream dispatch"
    );
}

#[test]
fn dataview_interp_get_builtin_stats_are_exact() {
    if std::env::var_os("ZIPP_DV_BUILTIN_STATS_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    let out = std::process::Command::new(&exe)
        .args([
            "dataview_interp_get_builtin_stats_worker",
            "--exact",
            "--nocapture",
        ])
        .env("ZIPP_DV_BUILTIN_STATS_WORKER", "1")
        .env("ZIPP_BUILTINSTATS", "1")
        .env("ZIPP_NOJIT", "1")
        .output()
        .expect("spawn builtin-stats worker");
    assert!(
        out.status.success(),
        "DataView builtin stats diverged:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn dataview_tracking_live_length_worker() {
    if std::env::var_os("ZIPP_DV_TRACKING_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(TRACKING_SOURCE).expect("tracking source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );

    let node = std::process::Command::new("node")
        .arg("-e")
        .arg(TRACKING_SOURCE)
        .output()
        .expect("node on PATH");
    assert!(
        node.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&node.stderr)
    );
    let expected: Vec<String> = String::from_utf8(node.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(out.output, expected);
}

#[test]
fn dataview_tracking_live_length_matches_node_in_all_tiers() {
    if std::env::var_os("ZIPP_DV_TRACKING_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    let modes: &[(&str, &[(&str, &str)])] = &[
        ("interpreter", &[("ZIPP_NOJIT", "1")]),
        (
            "interpreter-gc-stress",
            &[("ZIPP_NOJIT", "1"), ("ZIPP_GC_STRESS", "1")],
        ),
        ("jit-default", &[]),
        ("jit-threshold-1", &[("ZIPP_JIT_THRESHOLD", "1")]),
    ];

    for &(mode, env) in modes {
        let mut command = std::process::Command::new(&exe);
        command
            .args([
                "dataview_tracking_live_length_worker",
                "--exact",
                "--nocapture",
            ])
            .env("ZIPP_DV_TRACKING_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS")
            .env_remove("ZIPP_JIT_THRESHOLD");
        for &(key, value) in env {
            command.env(key, value);
        }
        let out = command.output().expect("spawn tracking worker");
        assert!(
            out.status.success(),
            "{mode} mode diverged:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
