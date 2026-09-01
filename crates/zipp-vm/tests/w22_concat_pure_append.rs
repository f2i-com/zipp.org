//! W22 fused computed-write pure append: the JIT and interpreter share one
//! prebuilt-key OrdinarySet proof. A pure own hit/add returns a dedicated
//! sentinel so the memory emitter can retain r13/r14 and TypedArray snapshots;
//! every effectful or exotic case keeps the historical delegated path.

//! Pins x86-64 JIT mechanisms from the engine's logs and counters, which the interpreter-only profiles never emit; compiled only where that tier exists, like the other tier-pinning suites.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

use std::process::Command;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(
        out.error.is_none(),
        "unexpected runtime error: {:?}",
        out.error
    );
    out.output
}

fn node_output(src: &str) -> Vec<String> {
    let out = Command::new("node")
        .arg("-e")
        .arg(src)
        .output()
        .expect("node on PATH (expected values come from node -e)");
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

fn assert_matches_node(src: &str) {
    assert_eq!(run_ok(src), node_output(src), "zipp != node for:\n{src}");
}

/// One script pins the proof boundary and the no-refetch contract:
///
/// * a growing receiver has an already-cached `anchor` slot and a live
///   TypedArray snapshot across every append (the receiver `vals` Vec grows and
///   its version changes under the property IC);
/// * custom-prototype writable data shadows, while a prototype setter and a
///   non-writable property installed after warmup must take the delegated path;
/// * own accessors, non-extensible objects, and Proxy traps remain observable;
/// * a holder aged before young reference stores keeps those values through
///   later collections (the dedicated mode runner repeats this under GC stress).
#[test]
fn concat_pure_append_adversarial_node_parity() {
    assert_matches_node(ADVERSARIAL_SRC);
}

const ADVERSARIAL_SRC: &str = r#""use strict";
var out = [];

var ta = new Uint8Array(16);
var growing = { anchor: 7 };
function pinGrow(o, bytes, n) {
    var sum = 0, d0 = 1.5, d1 = 2.5, b0 = false, b1 = true;
    for (var i = 0; i < n; i++) {
        b0 = (i & 1) === 0;
        b1 = i < n;
        var before = o.anchor;
        bytes[i & 15] = (bytes[i & 15] + i) & 255;
        o["grow" + i] = i + before;
        sum = (sum + o.anchor + o["grow" + i] + bytes[i & 15]) | 0;
        d0 = d0 * 0.5 + (i & 7);
        d1 = d1 * 0.25 + (i & 3);
    }
    return sum + ":" + (d0 | 0) + ":" + (d1 | 0) + ":" + b0 + ":" + b1;
}
out.push(pinGrow(growing, ta, 600));

var protoHits = 0;
var proto = { p100: 9 };
var child = Object.create(proto);
function protoBatch(o, start, n) {
    for (var j = 0; j < n; j++) {
        var k = start + j;
        o["p" + k] = k;
    }
}
protoBatch(child, 100, 200);
Object.defineProperty(proto, "p300", {
    set: function (v) { protoHits = protoHits + v; }, configurable: true
});
Object.defineProperty(proto, "p301", {
    value: 77, writable: false, configurable: true
});
protoBatch(child, 300, 1);
var inheritedBlocked = false;
try { protoBatch(child, 301, 1); } catch (e) { inheritedBlocked = true; }
protoBatch(child, 302, 100);
out.push(protoHits + ":" + child.hasOwnProperty("p100") + ":" + child.p100 + ":" + inheritedBlocked);

var ownHits = 0;
var own = {};
Object.defineProperty(own, "o0", {
    set: function (v) { ownHits = ownHits + v + 1; }, configurable: true
});
Object.defineProperty(own, "o1", {
    value: 4, writable: false, configurable: true
});
function ownBatch(o, start, n) {
    for (var q = 0; q < n; q++) o["o" + (start + q)] = start + q;
}
ownBatch(own, 0, 1);
var ownBlocked = false;
try { ownBatch(own, 1, 1); } catch (e) { ownBlocked = true; }
ownBatch(own, 2, 120);
out.push(ownHits + ":" + ownBlocked + ":" + own.o119);

function addOne(o, i) { o["z" + i] = i; }
var blocked = 0;
var noext = {}; Object.preventExtensions(noext);
var frozen = {}; Object.freeze(frozen);
var sealed = {}; Object.seal(sealed);
try { addOne(noext, 1); } catch (e) { blocked++; }
try { addOne(frozen, 2); } catch (e) { blocked++; }
try { addOne(sealed, 3); } catch (e) { blocked++; }
out.push("blocked:" + blocked);

var proxyHits = 0, target = {};
var proxy = new Proxy(target, {
    set: function (t, k, v) { proxyHits++; t[k] = v; return true; }
});
function proxyBatch(o, n) {
    for (var i = 0; i < n; i++) o["x" + i] = i;
}
proxyBatch(proxy, 240);
out.push(proxyHits + ":" + target.x239);

var holder = { anchor: 1 }, ageSink = 0;
for (var a = 0; a < 700; a++) ageSink += ({ x: a }).x;
var refs = [];
for (var r = 0; r < 32; r++) refs.push({ id: r });
function storeRefs(o, values, n) {
    var s = 0;
    for (var i = 0; i < n; i++) {
        var v = values[i & 31];
        o["r" + i] = v;
        s = (s + o.anchor + o["r" + i].id) | 0;
    }
    return s;
}
var refSum = storeRefs(holder, refs, 320);
refs = null;
for (var g = 0; g < 900; g++) ageSink += ({ y: g }).y;
var retained = 0;
for (var h = 0; h < 320; h++) retained += holder["r" + h].id;
out.push(refSum + ":" + retained + ":" + ageSink);

var protoNamed = {};
protoNamed["__proto__" + 0] = 55;
out.push(protoNamed.hasOwnProperty("__proto__0") + ":" + protoNamed.__proto__0);

for (var z = 0; z < out.length; z++) console.log(out[z]);
"#;

const MECHANISM_SRC: &str = r#""use strict";
function kernel(rounds) {
    var total = 0;
    for (var outer = 0; outer < rounds; outer++) {
        var o = { anchor: outer };
        for (var i = 0; i < 72; i++) {
            o["k" + i] = i;
            total = (total + o.anchor) | 0;
        }
        for (var j = 0; j < 72; j++) {
            o["k" + j] = j + 1;
            total = (total + o["k" + j]) | 0;
        }
    }
    return total;
}
console.log(kernel(180));
"#;

#[test]
fn concat_pure_append_mechanism_child() {
    if std::env::var_os("ZIPP_CONCAT_PURE_CHILD").is_none() {
        return;
    }
    assert_matches_node(MECHANISM_SRC);
    let (hits, adds, slow) = zipp_vm::concat_set_stats();
    if std::env::var_os("ZIPP_NO_CONCAT_PURE_APPEND").is_some() {
        assert_eq!(adds, 0, "off switch unexpectedly served pure adds");
        assert!(
            hits > 1_000 && slow > 1_000,
            "off path did not engage: {hits}/{adds}/{slow}"
        );
    } else {
        assert!(
            hits > 1_000 && adds > 1_000,
            "pure path did not engage: {hits}/{adds}/{slow}"
        );
    }
}

/// Codegen reads the switch per process. Default must report both pure arms;
/// OFF must select the historical helper, force add=0, and still run the same
/// program byte-for-byte against Node.
#[test]
fn zz_concat_pure_append_counter_and_switch() {
    if std::env::var_os("ZIPP_CONCAT_PURE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    for off in [false, true] {
        let mut cmd = Command::new(&exe);
        cmd.args([
            "concat_pure_append_mechanism_child",
            "--exact",
            "--nocapture",
        ])
        .env("ZIPP_CONCAT_PURE_CHILD", "1")
        .env("ZIPP_ICSTATS", "1")
        .env_remove("ZIPP_NOJIT")
        .env_remove("ZIPP_NO_CONCAT_APPEND")
        .env_remove("ZIPP_NO_CONCAT_PURE_APPEND");
        if off {
            cmd.env("ZIPP_NO_CONCAT_PURE_APPEND", "1");
        }
        let got = cmd.output().expect("spawn concat mechanism child");
        let stdout = String::from_utf8_lossy(&got.stdout);
        assert!(
            got.status.success() && !stdout.contains("running 0 tests"),
            "mechanism child off={off} failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&got.stderr)
        );
    }
}

/// Semantic parity must survive the historical helper, pre-B86 deopt shape,
/// interpreter, early compilation, GC-at-every-safe-point, and old full GC.
#[test]
fn zz_concat_pure_append_modes_agree() {
    if std::env::var_os("ZIPP_CONCAT_MODE_CHILD").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test exe path");
    for (key, val) in [
        ("ZIPP_NO_CONCAT_PURE_APPEND", "1"),
        ("ZIPP_NO_CONCAT_APPEND", "1"),
        ("ZIPP_NOJIT", "1"),
        ("ZIPP_JIT_THRESHOLD", "1"),
        ("ZIPP_GC_STRESS", "1"),
        ("ZIPP_NO_NURSERY", "1"),
    ] {
        let got = Command::new(&exe)
            .args(["--skip", "zz_"])
            .env("ZIPP_CONCAT_MODE_CHILD", "1")
            .env(key, val)
            .output()
            .expect("spawn concat mode child");
        let stdout = String::from_utf8_lossy(&got.stdout);
        assert!(
            got.status.success() && !stdout.contains("running 0 tests"),
            "{key}={val} failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&got.stderr)
        );
    }
}
