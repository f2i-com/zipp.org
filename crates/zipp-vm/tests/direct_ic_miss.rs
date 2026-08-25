//! Adaptive direct-miss property-site coverage.
//!
//! A sustained many-identity/same-shape site may recompile without its eight
//! identity-way probes, but it still enters the ordinary Rust miss helpers.
//! These fresh-process modes pin the site gate, its off switch, forced emission,
//! interpreter parity, and GC/write-barrier behaviour across live descriptor,
//! prototype and exotic changes.

#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const CHILD_ENV: &str = "ZIPP_DIRECT_IC_MISS_TEST_CHILD";
const CAP_CHILD_ENV: &str = "ZIPP_DIRECT_IC_MISS_CAP_CHILD";
const MARKER: &str = "[direct-ic-miss-test] ";
const CAP_MARKER: &str = "[direct-ic-miss-cap-test] ";

const SOURCE: &str = r#""use strict";
var gets = 0, sets = 0, pgets = 0, psets = 0, proxyGets = 0, proxySets = 0;

function read(o) { return o.x; }
function write(o, v) { o.x = v; }

var objects = [];
for (var i = 0; i < 128; i++) objects.push({ x: i, pad: i ^ 85 });
var sum = 0;
for (var round = 0; round < 80; round++) {
  for (var j = 0; j < objects.length; j++) {
    write(objects[j], (round + j) | 0);
    sum = (sum + read(objects[j])) | 0;
  }
}

// Delete + prototype data, then an own shadowing write.
var moved = objects[3];
delete moved.x;
Object.setPrototypeOf(moved, { x: 700 });
sum = (sum + read(moved)) | 0;
write(moved, 701);
sum = (sum + read(moved)) | 0;

// Data -> accessor -> non-writable data on a formerly hot receiver.
var changed = objects[5];
Object.defineProperty(changed, "x", {
  get: function () { gets++; return 50; },
  set: function (v) { sets += v; },
  configurable: true
});
sum = (sum + read(changed)) | 0;
write(changed, 9);
sum = (sum + read(changed)) | 0;
Object.defineProperty(changed, "x", {
  value: 11, writable: false, enumerable: true, configurable: true
});
var readonlyError = "";
try { write(changed, 12); } catch (e) { readonlyError = e.name; }
sum = (sum + read(changed)) | 0;
Object.defineProperty(changed, "x", {
  value: 11, writable: true, enumerable: true, configurable: true
});

// Prototype accessor: no own shape-slot shortcut may bypass it.
var proto = {};
Object.defineProperty(proto, "x", {
  get: function () { pgets++; return 33; },
  set: function (v) { psets += v * 2; },
  configurable: true
});
var inherited = { pad: 1 };
Object.setPrototypeOf(inherited, proto);
sum = (sum + read(inherited)) | 0;
write(inherited, 4);
sum = (sum + read(inherited)) | 0;

// Proxy and Array are exotic to the plain-object helper and must replay on the
// unchanged interpreter path, including observable traps.
var target = { x: 1 };
var proxy = new Proxy(target, {
  get: function (t, k) { proxyGets++; return t[k] + 100; },
  set: function (t, k, v) { proxySets++; t[k] = v + 1; return true; }
});
sum = (sum + read(proxy)) | 0;
write(proxy, 5);
sum = (sum + read(proxy)) | 0;
var array = [];
array.x = 8;
sum = (sum + read(array)) | 0;
write(array, 9);
sum = (sum + read(array)) | 0;

// A fresh shape and a non-extensible missing-property receiver.
var transitioned = { pad: 2 };
transitioned.x = 3;
sum = (sum + read(transitioned)) | 0;
write(transitioned, 4);
sum = (sum + read(transitioned)) | 0;
var sealed = { pad: 3 };
Object.preventExtensions(sealed);
sum = (sum + (read(sealed) === undefined ? 1 : 0)) | 0;
var sealedError = "";
try { write(sealed, 7); } catch (e) { sealedError = e.name; }

// Heap-valued stores through the direct helper must run the normal barrier on
// every write. Churn makes GC-stress/free-slot reuse exercise the live checks.
var keep = [];
for (var h = 0; h < 64; h++) {
  var payload = { id: h, nested: [h, h + 1] };
  write(objects[h], payload);
  keep.push(payload);
}
for (var c = 0; c < 512; c++) {
  var garbage = { id: c, nested: [c, c + 1, c + 2] };
  if ((c & 127) === 0) keep.push(garbage);
}
for (var q = 0; q < 64; q++) {
  var live = read(objects[q]);
  sum = (sum + live.id + live.nested[1]) | 0;
}

console.log([
  sum, gets, sets, pgets, psets, proxyGets, proxySets,
  readonlyError, sealedError, target.x, array.x, keep.length
].join("|"));
"#;

const EXPECTED: &str = "1060626|2|9|2|8|2|1|TypeError|TypeError|6|9|68";

const CAP_SOURCE: &str = r#""use strict";
function access(which, o, v) {
  switch (which) {
    case 0: o.a = v; return o.a;
    case 1: o.b = v; return o.b;
    case 2: o.c = v; return o.c;
    case 3: o.d = v; return o.d;
    case 4: o.e = v; return o.e;
    default: o.f = v; return o.f;
  }
}
var objects = [];
for (var i = 0; i < 64; i++) {
  objects.push({ a: 0, b: 0, c: 0, d: 0, e: 0, f: 0, pad: i });
}
var sum = 0;
for (var phase = 0; phase < 6; phase++) {
  for (var n = 0; n < 512; n++) {
    sum = (sum + access(phase, objects[n & 63], n)) | 0;
  }
}
console.log(sum);
"#;

fn child_run() {
    let outcome = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(
        outcome.error.is_none(),
        "unexpected runtime error: {:?}",
        outcome.error
    );
    assert_eq!(outcome.output, [EXPECTED]);
    eprintln!("{MARKER}{}", outcome.output[0]);
}

fn run_mode(env: &[(&str, &str)]) -> std::process::Output {
    let mut command = std::process::Command::new(std::env::current_exe().expect("test exe"));
    command
        .arg("--exact")
        .arg("direct_ic_miss_preserves_live_property_semantics")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_NO_FIELD_WRITE_STREAM", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_DIRECT_IC_MISS")
        .env_remove("ZIPP_FORCE_DIRECT_IC_MISS")
        .env_remove("ZIPP_GC_STRESS")
        .env_remove("ZIPP_SHAPE_VERIFY");
    for &(key, value) in env {
        command.env(key, value);
    }
    command.output().expect("spawn fresh test process")
}

fn assert_ok(label: &str, output: &std::process::Output) -> String {
    assert!(
        output.status.success(),
        "{label} child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains(MARKER), "{label}: missing output marker");
    stderr
}

#[test]
fn direct_ic_miss_preserves_live_property_semantics() {
    if std::env::var_os(CHILD_ENV).is_some() {
        child_run();
        return;
    }

    let default = assert_ok("default", &run_mode(&[]));
    assert!(
        default.contains("direct-miss site gate"),
        "adaptive mode never proved and parked a thrashing site:\n{default}"
    );
    assert!(
        default.contains("direct_miss=1") || default.contains("direct_miss=2"),
        "recompile never emitted direct form:\n{default}"
    );

    let off = assert_ok("off", &run_mode(&[("ZIPP_NO_DIRECT_IC_MISS", "1")]));
    assert!(
        !off.contains("direct-miss site gate") && !off.contains("direct_miss=1"),
        "off switch still emitted/flipped direct form:\n{off}"
    );

    let forced = assert_ok("forced", &run_mode(&[("ZIPP_FORCE_DIRECT_IC_MISS", "1")]));
    assert!(
        forced.contains("direct_miss=1"),
        "forced compile did not omit property probes:\n{forced}"
    );

    let nojit = assert_ok("nojit", &run_mode(&[("ZIPP_NOJIT", "1")]));
    assert!(
        !nojit.contains("direct_miss="),
        "NOJIT unexpectedly emitted native property sites:\n{nojit}"
    );

    let gc = assert_ok(
        "forced-gc",
        &run_mode(&[
            ("ZIPP_FORCE_DIRECT_IC_MISS", "1"),
            ("ZIPP_GC_STRESS", "1"),
            ("ZIPP_SHAPE_VERIFY", "1"),
        ]),
    );
    assert!(
        gc.contains("direct_miss=1"),
        "GC-stress run did not exercise forced direct stores:\n{gc}"
    );
}

#[test]
fn adaptive_recompiles_are_batched_and_resource_bounded() {
    if std::env::var_os(CAP_CHILD_ENV).is_some() {
        let outcome = zipp_vm::run(CAP_SOURCE).expect("cap source compiles");
        assert!(
            outcome.error.is_none(),
            "unexpected cap runtime error: {:?}",
            outcome.error
        );
        assert_eq!(outcome.output, ["784896"]);
        eprintln!("{CAP_MARKER}{}", outcome.output[0]);
        return;
    }

    let mut command = std::process::Command::new(std::env::current_exe().expect("test exe"));
    let output = command
        .arg("--exact")
        .arg("adaptive_recompiles_are_batched_and_resource_bounded")
        .arg("--nocapture")
        .env(CAP_CHILD_ENV, "1")
        .env("ZIPP_JITLOG", "1")
        .env("ZIPP_NO_FIELD_WRITE_STREAM", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_DIRECT_IC_MISS")
        .env_remove("ZIPP_FORCE_DIRECT_IC_MISS")
        .output()
        .expect("spawn cap child");
    assert!(
        output.status.success(),
        "cap child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(CAP_MARKER), "missing cap output marker");
    let events = stderr.matches("direct-miss site gate").count();
    assert_eq!(
        events, 4,
        "six staggered property pairs must hit the deterministic four-event cap:\n{stderr}"
    );
    assert!(
        stderr.contains("batched 2"),
        "already-proven read/write siblings were not marked together:\n{stderr}"
    );
}
