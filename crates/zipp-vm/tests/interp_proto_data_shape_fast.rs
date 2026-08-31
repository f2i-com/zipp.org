//! Node-differential coverage for the interpreter-only receiver-shape proof on
//! first-way inherited-data `GetProp` entries and its same-binary ablation.

use std::process::Command;

const SOURCE: &str = r#"
"use strict";

// A stable deep chain is the intended lane: the receiver shape proves the
// static key is still absent, while every proto hop remains version-guarded.
function readDeep(o) { return o.deep; }
var deep0 = { deep: 3 };
var deep1 = Object.create(deep0); deep1.d1 = 1;
var deep2 = Object.create(deep1); deep2.d2 = 2;
var deep3 = Object.create(deep2); deep3.d3 = 3;
var deep4 = Object.create(deep3); deep4.d4 = 4;
var deep5 = Object.create(deep4); deep5.pad = 5;
var deepSum = 0;
for (var di = 0; di < 50000; di++) deepSum = (deepSum + readDeep(deep5)) | 0;
console.log("deep", deepSum);

// Identical own layouts can have different explicit prototypes. The receiver
// shape may match, but FIRST_HEAP must still reject the wrong first link.
function shaped(proto) { var o = Object.create(proto); o.pad = 1; return o; }
function readSame(o) { return o.same; }
var sameA = shaped({ same: 11 });
var sameB = shaped({ same: 22 });
var sameSum = 0;
for (var si = 0; si < 20000; si++) {
  sameSum = (sameSum + readSame((si & 1) === 0 ? sameA : sameB)) | 0;
}
console.log("same-shape-proto", sameSum);

// Own shadows change the receiver shape; deletion/reinsertion also exercises
// dictionary mode, which is deliberately excluded from the shortcut.
function readShadow(o) { return o.shadow; }
var shadowProto = { shadow: 5 };
var shadow = shaped(shadowProto);
var shadowInherited = readShadow(shadow);
shadow.shadow = 8;
var shadowOwn = readShadow(shadow);
delete shadow.shadow;
var shadowAfterDelete = readShadow(shadow);
Object.defineProperty(shadow, "shadow", {
  configurable: true, enumerable: true, writable: true, value: 9
});
console.log("shadow", shadowInherited, shadowOwn, shadowAfterDelete, readShadow(shadow));

// Relinking the receiver does not need to change its own shape, so the live
// first-link guard remains essential.
function readLink(o) { return o.linkValue; }
var linkA = { linkValue: 30 };
var linkB = { linkValue: 31 };
var linked = shaped(linkA);
var linkBefore = readLink(linked);
Object.setPrototypeOf(linked, linkB);
var linkAfter = readLink(linked);
Object.setPrototypeOf(linked, null);
console.log("relink", linkBefore, linkAfter, String(readLink(linked)));

// Holder value/descriptor changes must be observed live. Data-to-accessor,
// getter-only, native and throwing getters all leave the data-only lane.
function readFlip(o) { return o.flip; }
var flipHolder = { flip: 2 };
var flipObject = shaped(flipHolder);
var flipInitial = readFlip(flipObject);
flipHolder.flip = 4;
var flipOverwrite = readFlip(flipObject);
var flipHits = 0;
Object.defineProperty(flipHolder, "flip", {
  configurable: true,
  get: function () { flipHits++; return 10 + flipHits; }
});
var flipGetterA = readFlip(flipObject);
var flipGetterB = readFlip(flipObject);
Object.defineProperty(flipHolder, "flip", {
  configurable: true, enumerable: true, writable: true, value: 20
});
var flipDataAgain = readFlip(flipObject);
delete flipHolder.flip;
flipHolder.flip = 21;
console.log("flip", flipInitial, flipOverwrite, flipGetterA, flipGetterB,
            flipDataAgain, readFlip(flipObject), flipHits);

function readNone(o) { return o.none; }
var noneHolder = { none: 1 };
var noneObject = shaped(noneHolder);
readNone(noneObject);
Object.defineProperty(noneHolder, "none", {
  configurable: true,
  set: function (value) {}
});

function readNative(o) { return o.nativeValue; }
var nativeHolder = { nativeValue: 1 };
var nativeObject = shaped(nativeHolder);
readNative(nativeObject);
var nativeGetter = Object.getOwnPropertyDescriptor(Object.prototype, "__proto__").get;
Object.defineProperty(nativeHolder, "nativeValue", {
  configurable: true,
  get: nativeGetter
});

function readBoom(o) { return o.boom; }
var boomHolder = { boom: 1 };
var boomObject = shaped(boomHolder);
readBoom(boomObject);
Object.defineProperty(boomHolder, "boom", {
  configurable: true,
  get: function () { throw new Error("getter-boom"); }
});
var boomThrown = false;
try { readBoom(boomObject); } catch (e) { boomThrown = e instanceof Error; }
console.log("accessors", String(readNone(noneObject)),
            readNative(nativeObject) === Object.getPrototypeOf(nativeObject),
            boomThrown);

// FIRST_DEFAULT is safe only while the receiver has no explicit proto entry
// and Object.prototype's guarded holder remains live. A same-slot overwrite
// must return the new value even if it does not alter layout.
function readDefault(o) { return o.defaultHot; }
Object.defineProperty(Object.prototype, "defaultHot", {
  configurable: true, enumerable: false, writable: true, value: 40
});
var defaultObject = { pad: 1 };
var defaultSum = 0;
for (var oi = 0; oi < 10000; oi++) defaultSum += readDefault(defaultObject);
Object.prototype.defaultHot = 41;
var defaultAfterWrite = readDefault(defaultObject);
delete Object.prototype.defaultHot;
console.log("default", defaultSum, defaultAfterWrite, String(readDefault(defaultObject)));

// DICT, class, Proxy and non-Object heap receivers must retain generic/class
// semantics and observable traps.
function readDict(o) { return o.dictValue; }
var dictProto = { dictValue: 50 };
var dict = shaped(dictProto);
for (var xi = 0; xi < 40; xi++) dict["x" + xi] = xi;
for (var xi = 0; xi < 40; xi += 2) delete dict["x" + xi];

class ClassReceiver {}
ClassReceiver.prototype.classValue = 60;
function readClass(o) { return o.classValue; }
var classObject = new ClassReceiver();

function readExotic(o) { return o.exoticValue; }
var exoticProto = { exoticValue: 70 };
var exoticPlain = shaped(exoticProto);
readExotic(exoticPlain);
var proxyHits = 0;
var proxy = new Proxy(exoticPlain, {
  get: function (target, key, receiver) {
    if (key === "exoticValue") proxyHits++;
    return Reflect.get(target, key, receiver);
  }
});
var array = [];
Object.setPrototypeOf(array, exoticProto);
function callable() {}
Object.setPrototypeOf(callable, exoticProto);
var map = new Map([[1, 2]]);
Object.setPrototypeOf(map, exoticProto);
console.log("excluded", readDict(dict), readClass(classObject),
            readExotic(proxy), proxyHits, readExotic(array),
            readExotic(callable), readExotic(map));

// Hot enough for the default-JIT child. The interpreter shortcut must remain
// disabled there so the ordinary feedback/install path is identical with both
// gate settings.
function hotRead(o) { return o.hot; }
var hotA = shaped({ hot: 80 });
var hotB = shaped({ hot: 81 });
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
fn interp_proto_data_shape_semantic_worker() {
    if std::env::var_os("ZIPP_INTERP_PROTO_DATA_SHAPE_WORKER").is_none() {
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
fn interp_proto_data_shape_interpreter_gc_and_jit_match_node() {
    if std::env::var_os("ZIPP_INTERP_PROTO_DATA_SHAPE_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit, gc_stress) in [
        ("interpreter", true, false),
        ("interpreter-gc", true, true),
        ("default-jit", false, false),
    ] {
        let mut command = Command::new(&exe);
        command
            .args([
                "interp_proto_data_shape_semantic_worker",
                "--exact",
                "--nocapture",
            ])
            .env("ZIPP_INTERP_PROTO_DATA_SHAPE_WORKER", "1")
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
