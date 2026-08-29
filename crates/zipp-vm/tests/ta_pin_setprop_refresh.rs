//! A call-free property-store hit must invalidate method-licensed pin snapshots.

const PROBE: &str = r#"
"use strict";

const text = "ABCD";
const intrinsic = String.prototype.charCodeAt;
function replacement(index) {
  return 1000 + index;
}

function scan(count) {
  let sum = 0;
  for (let i = 0; i < count; i++) {
    // The loop restores the intrinsic before every back-edge, so OSR planning
    // legitimately publishes an ASCII/charCodeAt snapshot.  The next
    // iteration's warmed SetProp may then replace the slot without changing
    // String.prototype's layout/version.  A stale snapshot would return the
    // raw ASCII byte here instead of calling `replacement`.
    String.prototype.charCodeAt = replacement;
    sum = (sum + text.charCodeAt(i & 3)) | 0;
    String.prototype.charCodeAt = intrinsic;
  }
  return sum;
}

console.log(scan(20000));
String.prototype.charCodeAt = intrinsic;
"#;

// DataView.prototype is an ordinary object, so its warmed SetProp can execute
// call-free in the same native region as the pinned get. A stale snapshot would
// read the backing bytes instead of calling `replacementGet`.
const DATAVIEW_PROBE: &str = r#"
"use strict";

const bytes = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
const view = new DataView(bytes.buffer);
const intrinsicGet = DataView.prototype.getUint32;
function replacementGet(offset, littleEndian) {
  return 9000 + offset + (littleEndian ? 1 : 0);
}
const intrinsicSet = DataView.prototype.setUint8;
let setCalls = 0;
DataView.prototype.setUint8 = function replacementSet(offset, value) {
  setCalls++;
  return 77 + offset + value;
};
const setResult = view.setUint8(0, 5);
DataView.prototype.setUint8 = intrinsicSet;

function scan(count) {
  let sum = 0;
  for (let i = 0; i < count; i++) {
    DataView.prototype.getUint32 = replacementGet;
    sum = (sum + view.getUint32(0, true)) | 0;
    DataView.prototype.getUint32 = intrinsicGet;
  }
  return sum;
}

console.log(setResult + ":" + setCalls + ":" + bytes[0]);
console.log(scan(12000));
DataView.prototype.getUint32 = intrinsicGet;
DataView.prototype.setUint8 = intrinsicSet;
"#;

// The concat-store helper has a separate call-free pure-success sentinel.
// Keep the numeric suffix dynamic so this remains SetIndexConcat bytecode
// rather than folding to an ordinary SetProp.
const DATAVIEW_CONCAT_PROBE: &str = r#"
"use strict";

const bytes = new Uint8Array([1, 2, 3, 4]);
const view = new DataView(bytes.buffer);
const intrinsic = DataView.prototype.getUint32;
function replacement(offset, littleEndian) {
  return 12000 + offset + (littleEndian ? 1 : 0);
}

function scan(count, suffix) {
  let sum = 0;
  for (let i = 0; i < count; i++) {
    DataView.prototype["getUint" + suffix] = replacement;
    sum = (sum + view.getUint32(0, true)) | 0;
    DataView.prototype.getUint32 = intrinsic;
  }
  return sum;
}

console.log(scan(12000, 32));
DataView.prototype.getUint32 = intrinsic;
"#;

// Array.prototype is itself an exotic Array, so its SetProp currently takes
// the generic/deopt route rather than the call-free method-refetch join. Keep
// this separate as a differential for the sibling override semantics.
const ARRAY_PROBE: &str = r#"
"use strict";

const values = [];
const intrinsicPush = Array.prototype.push;
function replacementPush(value) {
  return 7000 + (value & 7);
}

function scan(count) {
  let sum = 0;
  for (let i = 0; i < count; i++) {
    Array.prototype.push = replacementPush;
    sum = (sum + values.push(i)) | 0;
    Array.prototype.push = intrinsicPush;
  }
  return sum + ":" + values.length;
}

console.log(scan(12000));
Array.prototype.push = intrinsicPush;
"#;

const FOREIGN_DATAVIEW_METHOD: &str = r#"
"use strict";
const foreign = $262.createRealm().global;
const intrinsic = DataView.prototype.getUint32;
DataView.prototype.getUint32 = foreign.DataView.prototype.getUint32;
const view = new DataView(new ArrayBuffer(1));
try {
  view.getUint32(0);
  console.log("no-throw");
} catch (error) {
  console.log([
    error.constructor === foreign.RangeError,
    error.constructor === RangeError
  ].join("|"));
}
DataView.prototype.getUint32 = intrinsic;
"#;

fn zipp_output(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn node_output(src: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(src)
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
fn call_free_setprop_refreshes_string_method_pin() {
    assert_eq!(zipp_output(PROBE), node_output(PROBE));
}

#[test]
fn call_free_setprop_refreshes_dataview_method_pin() {
    assert_eq!(zipp_output(DATAVIEW_PROBE), node_output(DATAVIEW_PROBE));
}

#[test]
fn concat_set_pure_refreshes_dataview_method_pin() {
    let bytecode = zipp_vm::compile_to_text(DATAVIEW_CONCAT_PROBE, false)
        .expect("computed-key probe compiles");
    assert!(
        bytecode.contains("SetIndexConcat"),
        "computed-key adversary stopped exercising SetIndexConcat:\n{bytecode}"
    );
    assert_eq!(
        zipp_output(DATAVIEW_CONCAT_PROBE),
        node_output(DATAVIEW_CONCAT_PROBE)
    );
}

#[test]
fn array_push_override_remains_exact() {
    assert_eq!(zipp_output(ARRAY_PROBE), node_output(ARRAY_PROBE));
}

#[test]
fn foreign_dataview_method_keeps_its_callee_realm() {
    assert_eq!(run_ok_foreign(), vec!["true|false"]);
}

fn run_ok_foreign() -> Vec<String> {
    zipp_output(FOREIGN_DATAVIEW_METHOD)
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn call_free_setprop_refresh_mechanism_child() {
    match std::env::var("ZIPP_SETPROP_PIN_CHILD").as_deref() {
        Ok("string") => assert_eq!(zipp_output(PROBE), node_output(PROBE)),
        Ok("dataview") => assert_eq!(zipp_output(DATAVIEW_PROBE), node_output(DATAVIEW_PROBE)),
        Ok("dataview-concat") => {
            assert_eq!(
                zipp_output(DATAVIEW_CONCAT_PROBE),
                node_output(DATAVIEW_CONCAT_PROBE)
            );
            let (hits, adds, slow) = zipp_vm::concat_set_stats();
            assert!(
                hits > 0,
                "computed method replacement missed CONCAT_SET_PURE: {hits}/{adds}/{slow}"
            );
        }
        _ => {}
    }
}

#[cfg(all(feature = "jit", target_arch = "x86_64"))]
#[test]
fn call_free_setprop_refresh_engages_pinned_mem_region() {
    let exe = std::env::current_exe().expect("test executable");
    for family in ["string", "dataview", "dataview-concat"] {
        let out = std::process::Command::new(&exe)
            .args([
                "call_free_setprop_refresh_mechanism_child",
                "--exact",
                "--nocapture",
            ])
            .env("ZIPP_SETPROP_PIN_CHILD", family)
            .env("ZIPP_JITLOG", "1")
            .env("ZIPP_ICSTATS", "1")
            .env("ZIPP_JIT_THRESHOLD", "1")
            .env_remove("ZIPP_NOJIT")
            .output()
            .expect("spawn mechanism child");
        assert!(
            out.status.success(),
            "{family} mechanism child failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.lines().any(|line| {
                line.contains("[pin] fn")
                    && line.contains("built pins=1")
                    && line.contains("access=[")
            }) && stderr.contains("[jit] MEM region"),
            "{family} probe did not exercise a pinned MEM region:\n{stderr}"
        );
    }
}
