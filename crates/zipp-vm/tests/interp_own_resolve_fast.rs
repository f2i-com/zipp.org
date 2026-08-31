//! Node-differential coverage for the interpreter-only authoritative-own
//! `GetProp` resolution in normal, GC-stress, and JIT-capable VMs.

use std::process::Command;

const SOURCE: &str = r#"
"use strict";

function readKey(o) { return o.key; }
var shapes = [
  { key: 1 },
  { a0: 0, key: 2 },
  { a1: 0, b1: 0, key: 3 },
  { a2: 0, b2: 0, c2: 0, key: 4 },
  { a3: 0, b3: 0, c3: 0, d3: 0, key: 5 },
  { a4: 0, b4: 0, c4: 0, d4: 0, e4: 0, key: 6 },
  { a5: 0, b5: 0, c5: 0, d5: 0, e5: 0, f5: 0, key: 7 },
  { a6: 0, b6: 0, c6: 0, d6: 0, e6: 0, f6: 0, g6: 0, key: 8 },
  { a7: 0, b7: 0, c7: 0, d7: 0, e7: 0, f7: 0, g7: 0, h7: 0, key: 9 },
  { a8: 0, b8: 0, c8: 0, d8: 0, e8: 0, f8: 0, g8: 0, h8: 0, i8: 0, key: 10 }
];
var shapeSum = 0;
for (var si = 0; si < 50000; si++) {
  shapeSum = (shapeSum + readKey(shapes[si % shapes.length])) | 0;
}
console.log("shapes", shapeSum);

// Deletion forces dictionary mode and moves/reinserts slots. `m.pos(key)` is
// the live authority after each mutation; no historical IC slot may win.
function readDict(o) { return o.target; }
var dict = { before: 1, target: 10, after: 2, tail: 3 };
readDict(dict);
delete dict.before;
var dictShifted = readDict(dict);
delete dict.target;
dict.target = 20;
var dictReinserted = readDict(dict);
delete dict.after;
dict.extra = 4;
console.log("dict", dictShifted, dictReinserted, readDict(dict));

// The live descriptor decides data versus accessor on every read.
function readFlip(o) { return o.flip; }
var flipHits = 0;
var flip = { flip: 2, pad: 0 };
readFlip(flip);
Object.defineProperty(flip, "flip", {
  configurable: true,
  get: function () { flipHits++; return 10 + flipHits; }
});
var flipA = readFlip(flip);
var flipB = readFlip(flip);
Object.defineProperty(flip, "flip", {
  configurable: true, writable: true, value: 30
});
console.log("flip", flipA, flipB, readFlip(flip), flipHits);

// User getter, absent getter, and a native getter that cannot be represented
// as GetAct::Accessor. The native case must fall through and still receive the
// original receiver as `this`.
function readUser(o) { return o.user; }
var userHits = 0;
var userWarm = { user: 1 };
readUser(userWarm);
var userObj = {};
Object.defineProperty(userObj, "user", {
  configurable: true,
  get: function () { userHits++; return 40 + userHits; }
});

function readNone(o) { return o.none; }
readNone({ none: 1 });
var noneObj = {};
Object.defineProperty(noneObj, "none", {
  configurable: true,
  set: function (value) {}
});

function readNative(o) { return o.nativeValue; }
readNative({ nativeValue: 1 });
var nativeObj = {};
var nativeGetter = Object.getOwnPropertyDescriptor(Object.prototype, "__proto__").get;
Object.defineProperty(nativeObj, "nativeValue", {
  configurable: true,
  get: nativeGetter
});
console.log("accessors", readUser(userObj), readUser(userObj), userHits,
            String(readNone(noneObj)),
            readNative(nativeObj) === Object.getPrototypeOf(nativeObj));

// An own property is authoritative over class and ordinary prototype members.
function readShadow(o) { return o.shadow; }
readShadow({ shadow: 1 });
var shadowHits = 0;
class Shadowed {
  get shadow() { shadowHits++; return 90; }
}
var classObj = new Shadowed();
Object.defineProperty(classObj, "shadow", {
  configurable: true, writable: true, value: 70
});
var shadowProto = {};
Object.defineProperty(shadowProto, "shadow", {
  configurable: true,
  get: function () { shadowHits++; return 91; }
});
var protoObj = Object.create(shadowProto);
Object.defineProperty(protoObj, "shadow", {
  configurable: true, writable: true, value: 71
});
console.log("shadow", readShadow(classObj), readShadow(protoObj), shadowHits);

// Proxies, Arrays/callables and other exotic receivers never enter the plain
// Object map lane. Their observable lookup behavior remains generic.
function readExotic(o) { return o.x; }
readExotic({ x: 0 });
var proxyHits = 0;
var proxy = new Proxy({ x: 4 }, {
  get: function (target, key, receiver) {
    if (key === "x") proxyHits++;
    return Reflect.get(target, key, receiver);
  }
});
var array = [];
array.x = 5;
function callable() {}
callable.x = 6;
var map = new Map([[1, 2]]);
function readSize(o) { return o.size; }
console.log("exotic", readExotic(proxy), proxyHits, readExotic(array),
            readExotic(callable), readSize(map));

// This two-shape static site becomes hot enough to compile in the default-JIT
// child. The shortcut must remain disabled there so both shapes are still
// installed as native-plan feedback.
function hotRead(o) { return o.value; }
var hotA = { value: 1 };
var hotB = { pad: 0, value: 2 };
var hotSum = 0;
for (var hi = 0; hi < 100000; hi++) {
  hotSum = (hotSum + hotRead((hi & 1) === 0 ? hotA : hotB)) | 0;
}
console.log("jit", hotSum);
"#;

fn node_output() -> Vec<String> {
    let out = Command::new("node")
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
fn interp_own_resolve_semantic_worker() {
    if std::env::var_os("ZIPP_INTERP_OWN_RESOLVE_WORKER").is_none() {
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
fn interp_own_resolve_default_gc_and_jit_match_node() {
    if std::env::var_os("ZIPP_INTERP_OWN_RESOLVE_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit, gc_stress) in [
        ("interp", true, false),
        ("interp-gc", true, true),
        ("default-jit", false, false),
    ] {
        let mut command = Command::new(&exe);
        command
            .args([
                "interp_own_resolve_semantic_worker",
                "--exact",
                "--nocapture",
            ])
            .env("ZIPP_INTERP_OWN_RESOLVE_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS");
        if nojit {
            command.env("ZIPP_NOJIT", "1");
        }
        if gc_stress {
            command.env("ZIPP_GC_STRESS", "1");
        }
        let out = command.output().expect("spawn semantic worker");
        assert!(
            out.status.success(),
            "{mode} diverged:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
