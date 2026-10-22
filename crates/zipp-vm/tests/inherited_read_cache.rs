//! An inherited read whose cache way is the site's first is answered by
//! the own hit through the receiver's mirror, and the receiver's explicit
//! prototype is read from `proto_of`'s link copy. Every way the answer can
//! change after a warm loop: the prototype replaced (`setPrototypeOf`,
//! `__proto__`, `Reflect.setPrototypeOf`, to `null` and back), the holder
//! written and its key deleted, `Object.prototype` taking the key, an own
//! property shadowing it; a plain object, an `Object.create` one, a null-
//! prototype one and a class instance of one shape at one site; a
//! constructor's prototype replaced; and short-lived objects with explicit
//! prototypes collected while others take their slots. The expected lines
//! are Node's (v24), in the JIT, the interpreter and the interpreter under
//! GC stress.

const SOURCE: &str = r###"
"use strict";
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function read(o) { return o.c; }
function readK(o) { return o.k; }

// An object made by Object.create, read through its prototype in a loop,
// then each way its answer can change.
var p = { c: 1 }, q = { c: 2 };
var o = Object.create(p);
o.a = 0;
var s = 0;
for (var i = 0; i < 100; i++) s += read(o);
log("warm", s, read(o));
Object.setPrototypeOf(o, q);
log("reproto", read(o));
q.c = 3;
log("holder write", read(o));
delete q.c;
log("holder delete", read(o));
Object.prototype.c = "objproto";
log("from Object.prototype", read(o));
delete Object.prototype.c;
q.c = 4;
o.c = "own";
log("own shadows", read(o));
delete o.c;
log("unshadowed", read(o));
o.__proto__ = p;
log("__proto__", read(o));
Reflect.setPrototypeOf(o, null);
log("null proto", read(o));
Reflect.setPrototypeOf(o, p);
log("back", read(o));

// Same shape, different chains at one site: a plain object, an
// Object.create one, a null-prototype one and a class instance.
class K { constructor() { this.a = 0; } get c() { return "getter"; } }
var plain = { a: 0 }, made = Object.create({ c: "made" }), bare = Object.create(null), inst = new K();
made.a = 0; bare.a = 0;
var seen = [];
for (var j = 0; j < 40; j++) seen.push(read([plain, made, bare, inst][j % 4]));
log("mixed", seen.slice(-4).join(","));

// Constructor-function instances, then the constructor's prototype replaced.
function F() { this.x = 1; }
F.prototype.k = "first";
var f1 = new F();
for (var j = 0; j < 30; j++) readK(f1);
F.prototype = { k: "second" };
var f2 = new F();
log("ctor protos", readK(f1), readK(f2));
for (var j = 0; j < 30; j++) readK(f2);
log("ctor protos again", readK(f1), readK(f2));

// Many short-lived objects with explicit prototypes, collected while others
// take their slots: a reused slot must not keep its dead object's link.
var protos = [{ c: "p0" }, { c: "p1" }, { c: "p2" }];
var acc = 0, last = "";
for (var round = 0; round < 200; round++) {
  var batch = [];
  for (var k = 0; k < 50; k++) {
    var b = (k % 3 === 0) ? { a: 0 } : Object.create(protos[(round + k) % 3]);
    b.a = k;
    batch.push(b);
  }
  for (var k = 0; k < 50; k++) { var v = read(batch[k]); if (v !== undefined) { acc += v.length; last = v; } }
  var junk = [];
  for (var k = 0; k < 400; k++) junk.push({ k: k });
}
log("churn", acc, last);
console.log(out.join("\n"));
"###;

const EXPECTED: &str = "warm 100 1\nreproto 2\nholder write 3\nholder delete \nfrom Object.prototype objproto\nown shadows own\nunshadowed 4\n__proto__ 1\nnull proto \nback 1\nmixed ,made,,getter\nctor protos first second\nctor protos again first second\nchurn 13200 p2";

#[test]
fn inherited_read_cache_worker() {
    if std::env::var_os("ZIPP_INHERITED_READ_CACHE_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn inherited_read_cache_matches_node() {
    if std::env::var_os("ZIPP_INHERITED_READ_CACHE_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit, gc) in [("default", false, false), ("interpreter", true, false), ("interpreter-gc-stress", true, true)] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["inherited_read_cache_worker", "--exact", "--nocapture"])
            .env("ZIPP_INHERITED_READ_CACHE_WORKER", "1")
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_GC_STRESS");
        if nojit {
            command.env("ZIPP_NOJIT", "1");
        }
        if gc {
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
