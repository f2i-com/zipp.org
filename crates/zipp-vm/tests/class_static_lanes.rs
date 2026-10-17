//! `C.key` reads a class's static data property (its own, or one inherited
//! through unmodified `extends` links) and `C.key = v` writes an own
//! writable one directly, before the general member walk. Every case the
//! lanes must leave to that walk (static accessors at any level, an
//! explicit `setPrototypeOf`, a non-class parent, the synthesized `name` /
//! `length` / `prototype`, non-writable and frozen statics, writes that
//! create a subclass's own property, `caller` / `arguments`, deletions)
//! must still give Node's answers, as must the hot loops.

const SOURCE: &str = r##"
"use strict";
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function catchName(f) { try { return f(); } catch (e) { return e.constructor.name; } }

class A { static count = 0; static kind = "a"; static #serial = 0; static bump() { return ++A.#serial; } static [Symbol.for("s")] = "sym"; }
class B extends A { static kind = "b"; static launched = 0; }
class C extends B {}
for (var i = 0; i < 3000; i++) { A.count++; B.launched++; C.launched; }
log("counts", A.count, B.launched, C.launched, A.bump(), A.bump());
log("reads", A.kind, B.kind, C.kind, C.count, B[Symbol.for("s")], C.bump === A.bump, typeof C.bump);
log("synth", A.name, B.name, C.name, A.length, typeof C.prototype, C.prototype instanceof B);

// A write through a subclass creates its own property.
C.count = 7;
log("shadow", A.count, C.count, Object.hasOwn(C, "count"), Object.hasOwn(B, "count"));
delete C.count;
log("unshadow", C.count, Object.hasOwn(C, "count"));

// Static accessors on the chain.
class G { static #v = 1; static get v() { return "get:" + this.name + ":" + G.#v; } static set v(x) { G.#v = x; } }
class H extends G {}
H.v = 5;
log("accessor", G.v, H.v);
Object.defineProperty(A, "kind", { get: function () { return "defined:" + this.name; }, configurable: true });
log("defined-accessor", A.kind, B.kind, C.kind);
Object.defineProperty(A, "kind", { value: "a2", writable: true, configurable: true });
log("redefined", A.kind, C.kind);

// Non-writable and frozen statics.
Object.defineProperty(B, "fixed", { value: 1, writable: false, configurable: true });
log("non-writable", catchName(function () { B.fixed = 2; }), B.fixed, C.fixed);
class F { static x = 1; }
Object.freeze(F);
log("frozen", catchName(function () { F.x = 2; }), F.x, catchName(function () { F.y = 1; }), F.y,
  JSON.stringify(Object.getOwnPropertyDescriptor(F, "x")), Object.isFrozen(F));
class S { static x = 1; static m() { return 1; } }
Object.seal(S);
log("sealed", catchName(function () { S.x = 2; }), S.x, catchName(function () { S.y = 1; }), S.y,
  JSON.stringify(Object.getOwnPropertyDescriptor(S, "x")), JSON.stringify(Object.getOwnPropertyDescriptor(S, "m")),
  Object.isSealed(S), Object.isFrozen(S), catchName(function () { return delete S.x; }));
class NE { static x = 1; }
Object.preventExtensions(NE);
log("no-extend", catchName(function () { NE.x = 2; }), NE.x, catchName(function () { NE.y = 1; }), NE.y, Object.isExtensible(NE));
class FM { static m() { return 1; } }
Object.freeze(FM);
log("frozen-method", catchName(function () { FM.m = null; }), typeof FM.m, JSON.stringify(Object.getOwnPropertyDescriptor(FM, "m")));

// An explicit prototype and a non-class parent.
class P {} ; P.count = 100; P.kind = "p";
Object.setPrototypeOf(C, P);
log("set-proto", C.count, C.kind, C.launched);
Object.setPrototypeOf(C, B);
log("restored", C.count, C.kind);
function Fn() {} Fn.stat = "fn";
class FromFn extends Fn {}
log("fn-parent", FromFn.stat, FromFn.name);
log("restricted", catchName(function () { A.caller = 1; }), catchName(function () { A.arguments = 1; }));

// Static fields that read and write statics while initializing.
class Init { static a = 1; static b = Init.a + 1; static { Init.a = Init.b * 10; } static c = Init.a++; }
log("init", Init.a, Init.b, Init.c);

// A hot construct loop reading new.target statics, as class-features does.
class Base { static made = 0; constructor() { this.kind = new.target.kind; Base.made++; } }
class K1 extends Base { static kind = "k1"; }
class K2 extends Base { static kind = "k2"; }
var kinds = "";
for (var j = 0; j < 2000; j++) { var o = (j & 1) ? new K1() : new K2(); if (j < 4) kinds += o.kind; }
log("ctor-statics", Base.made, kinds, Object.hasOwn(K1, "made"));
// Static method calls, inherited and as values.
class M {
  static twice(x) { return 2 * x + (this === M ? 1 : this === N ? 2 : 0); }
  static arrow = () => typeof this;
  static *gen() { yield this.name; }
  static async later() { return 1; }
  static Inner = class {};
}
class N extends M {}
var calls = 0;
for (var c = 0; c < 2000; c++) calls = (calls + M.twice(c & 7) + N.twice(1)) | 0;
log("static-calls", calls, M.arrow(), N.arrow(), [...M.gen()].join(), [...N.gen()].join(), typeof M.later(),
  catchName(function () { return M.Inner(); }), catchName(function () { return M.nope(); }));

// Function own properties.
function counter() { return counter.calls; }
counter.calls = 0;
for (var d = 0; d < 3000; d++) counter.calls++;
var util = { };
function fmt(x) { return (this === fmt ? "self:" : "other:") + x; }
fmt.inner = function (x) { return (this === fmt ? "fn-this:" : "?") + x; };
fmt.bound = fmt.inner.bind(null);
log("fn-props", counter(), counter.calls, fmt.inner(3), fmt.bound(4), typeof Math.max.extra);
Math.max.extra = 9;
log("native-prop", Math.max.extra, Math.max(1, 2));
delete Math.max.extra;
Object.defineProperty(counter, "fixed", { value: 1, writable: false, configurable: true });
Object.defineProperty(counter, "acc", { get: function () { return "acc:" + (this === counter); }, configurable: true });
var setLog = [];
Object.defineProperty(counter, "both", { get: function () { return "both"; }, set: function (v) { setLog.push(v + ":" + (this === counter)); }, configurable: true });
log("fn-descriptors", catchName(function () { counter.fixed = 2; }), counter.fixed, counter.acc, catchName(function () { counter.acc = 1; }), counter.acc, catchName(function () { counter.both = 5; }), counter.both, setLog.join());
Object.freeze(counter);
log("fn-frozen", catchName(function () { counter.calls = 0; }), counter.calls, catchName(function () { counter.fresh = 1; }));
var arrowFn = () => 1; arrowFn.tag = "arrow";
var bf = counter.bind(null); bf.tag = "bound";
log("fn-kinds", arrowFn.tag, bf.tag, delete bf.tag, bf.tag);
console.log(out.join("\n"));
"##;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn class_static_lanes_worker() {
    if std::env::var_os("ZIPP_CLASS_STATIC_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}

#[test]
fn class_static_lanes_match_node() {
    if std::env::var_os("ZIPP_CLASS_STATIC_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    for (mode, nojit, gc) in [("default", false, false), ("interpreter", true, false), ("interpreter-gc-stress", true, true)] {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["class_static_lanes_worker", "--exact", "--nocapture"])
            .env("ZIPP_CLASS_STATIC_WORKER", "1")
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
