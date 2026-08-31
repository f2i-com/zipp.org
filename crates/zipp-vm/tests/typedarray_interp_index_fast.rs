//! Pure-interpreter semantic coverage for the direct numeric-index TypedArray
//! lane, including coercive/BigInt fallbacks and live resizable-buffer guards.

const SOURCE: &str = r#"
"use strict";

function show(value) {
  if (typeof value === "number") {
    if (value !== value) return "NaN";
    if (value === Infinity) return "Infinity";
    if (value === -Infinity) return "-Infinity";
    if (value === 0 && 1 / value === -Infinity) return "-0";
  }
  return String(value);
}
function caught(fn) {
  try { return "v:" + show(fn()); }
  catch (error) { return "e:" + error.constructor.name; }
}

var ctors = [
  Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array,
  Int32Array, Uint32Array, Float32Array, Float64Array
];
if (typeof Float16Array === "function") ctors.push(Float16Array);
var inputs = [-Infinity, -300.5, -1, -0, 0, 0.5, 1.5, 254.5, 255.5, 1e20, Infinity, NaN];
for (var c = 0; c < ctors.length; c++) {
  var ta = new ctors[c](inputs.length);
  for (var i = 0; i < inputs.length; i++) ta[i] = inputs[i];
  var got = [];
  for (var i = 0; i < ta.length; i++) got.push(show(ta[i]));
  console.log(ctors[c].name + ":" + got.join(","));
}

// Numeric -0 becomes property key "0". Fraction/NaN/infinity and the STRING
// "-0" are canonical-numeric invalid indices and are absorbed by the exotic
// object (undefined/no-op), never inherited from a custom prototype.
var protoHits = 0;
var custom = Object.create(Uint8Array.prototype);
Object.defineProperty(custom, "0", { get: function () { protoHits++; return 91; } });
Object.defineProperty(custom, "99", { get: function () { protoHits++; return 92; } });
var edge = new Uint8Array([7, 8]);
Object.setPrototypeOf(edge, custom);
console.log([
  show(edge[-0]), show(edge[0.5]), show(edge[NaN]), show(edge[Infinity]),
  show(edge["-0"]), show(edge[99]), protoHits
].join("|"));
edge[0.5] = 13; edge[NaN] = 14; edge[Infinity] = 15; edge["-0"] = 16; edge[99] = 17;
console.log(show(edge[0]) + "|" + show(edge[1]) + "|" + protoHits);

// Heap-valued stores stay generic: ToNumber runs exactly once even when the
// index is invalid, and can resize the backing buffer before the live recheck.
var calls = 0;
var objectValue = { valueOf: function () { calls++; return 42.75; } };
edge[1] = objectValue;
edge[999] = objectValue;
console.log(show(edge[1]) + "|" + calls);

var rab = new ArrayBuffer(16, { maxByteLength: 32 });
var fixed = new Uint16Array(rab, 4, 4);
var tracking = new Uint16Array(rab, 4);
fixed[0] = 513; tracking[5] = 1027;
console.log(fixed.length + "|" + tracking.length + "|" + fixed[0] + "|" + tracking[5]);
rab.resize(6);
fixed[0] = 9; tracking[0] = 10;
console.log(show(fixed[0]) + "|" + tracking.length + "|" + show(tracking[0]));
rab.resize(20);
tracking[6] = 2057;
console.log(show(fixed[0]) + "|" + tracking.length + "|" + tracking[6]);

var detachedBuffer = new ArrayBuffer(8);
var detached = new Float64Array(detachedBuffer);
detached[0] = 3.5;
detachedBuffer.transfer();
detached[0] = 8.5;
console.log(show(detached[0]) + "|" + detached.length);

// BigInt element kinds are intentionally excluded from the numeric lane.
var big = new BigInt64Array(2);
big[0] = -3n;
console.log(String(big[0]) + "|" + caught(function () { big[1] = 4; return big[1]; }));
"#;

fn node_output() -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(SOURCE)
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
fn typedarray_interp_index_semantic_worker() {
    if std::env::var_os("ZIPP_TA_INTERP_INDEX_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    assert_eq!(out.output, node_output());
}

#[test]
fn typedarray_interp_index_fast_and_gc_match_node() {
    if std::env::var_os("ZIPP_TA_INTERP_INDEX_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, gc_stress) in [("fast", false), ("fast-gc-stress", true)] {
        let mut command = std::process::Command::new(&exe);
        command
            .args([
                "typedarray_interp_index_semantic_worker",
                "--exact",
                "--nocapture",
            ])
            .env("ZIPP_TA_INTERP_INDEX_WORKER", "1")
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
