//! Getters on ES5-style prototype chains, read from fresh objects.
//!
//! A getter defined on a prototype object (`Object.defineProperty(
//! P.prototype, ...)`, what a class compiled to ES5 and every pre-class
//! library has) was cached by a compiled read per receiver OBJECT, so a
//! site that reads it from a new object each time called two runtime
//! helpers per read. The megamorphic stub now finds the getter by the
//! receiver's shape and first prototype (`es5_proto_memo_put_getter`,
//! `Es5ProtoMemo::getter`) and the site calls it natively.
//!
//! The program reads getters at one site from objects of three constructors
//! and several shapes; shadows them with own properties per object; has a
//! getter throw, and one return its receiver; uses strict, sloppy, arrow,
//! bound and native getters and a setter-only accessor; then replaces a
//! getter, turns one into a data property, deletes one, adds inherited data
//! and swaps a prototype while the sites are hot; and reads getters through
//! `Object.create`, a null prototype and `Object.prototype` itself.
//!
//! The expected lines are Node 24's.

const PROGRAM: &str = r#"
const out = [];
const log = (...a) => out.push(a.join(" "));
const N = 4000;
function Node0(tag, n) { this._tag = tag; this.n = n; }
Object.defineProperty(Node0.prototype, "tagName", { get: function () { return this._tag.toUpperCase(); }, configurable: true });
Object.defineProperty(Node0.prototype, "double", { get: function () { return this.n * 2; }, configurable: true });
Node0.prototype.kind = "node";
function Elem(tag, n) { Node0.call(this, tag, n); this.attrs = n & 1 ? { id: n } : null; }
Elem.prototype = Object.create(Node0.prototype, {
  constructor: { value: Elem },
  nodeName: { get: function () { return this.tagName; }, configurable: true },
  hasAttrs: { get: function () { return this.attrs !== null; }, configurable: true },
  writeOnly: { set: function (v) { this._w = v; }, configurable: true },
});
function Special(tag, n) { Elem.call(this, tag, n); this.extra = "x"; }
Special.prototype = Object.create(Elem.prototype, { constructor: { value: Special }, tagName: { get: function () { return "SPECIAL:" + this._tag; }, configurable: true } });
// One site, objects of three constructors and several shapes, all fresh.
function read(o) { return o.tagName + "/" + o.nodeName + "/" + o.double + "/" + o.hasAttrs + "/" + o.kind + "/" + o.writeOnly + "/" + o.missing; }
function mk(i) { return i % 3 === 0 ? new Node0("a" + (i & 3), i) : i % 3 === 1 ? new Elem("div", i) : new Special("s", i); }
function run(n) { let acc = 0, last = ""; for (let i = 0; i < n; i++) { const o = mk(i); last = read(o); acc += last.length; } return acc + "|" + last; }
log("mixed", run(N), read(mk(0)), read(mk(1)));
// Own property shadows, per object.
function shadowed(n) { let s = ""; for (let i = 0; i < n; i++) { const o = new Elem("p", i); if (i % 4 === 0) Object.defineProperty(o, "tagName", { value: "OWN" + (i & 7) }); if (i % 4 === 1) o.double = -1; if (i >= n - 4) s += o.tagName + "," + o.double + ";"; } return s; }
log("shadow", shadowed(N));
// A getter that throws, and one that sees its receiver.
function Thrower(i) { this.i = i; }
Object.defineProperty(Thrower.prototype, "risky", { get: function () { if (this.i % 1500 === 1499) throw new RangeError("risky" + this.i); return this.i; }, configurable: true });
Object.defineProperty(Thrower.prototype, "self", { get: function () { return this; }, configurable: true });
let sum = 0, caught = ""; for (let i = 0; i < N; i++) { const t = new Thrower(i); try { sum += t.risky; } catch (e) { caught += e.message + ";"; } if (t.self !== t) sum = NaN; }
log("throws", sum, caught);
// Strict and sloppy getters, an arrow, a bound function and a native as getters.
function Kinds() {}
Object.defineProperty(Kinds.prototype, "strictThis", { get: function () { "use strict"; return typeof this; }, configurable: true });
Object.defineProperty(Kinds.prototype, "sloppyThis", { get: function () { return this instanceof Kinds; }, configurable: true });
const outer = this;
Object.defineProperty(Kinds.prototype, "arrow", { get: () => typeof outer, configurable: true });
Object.defineProperty(Kinds.prototype, "bound", { get: function () { return this.tag; }.bind({ tag: "bound-this" }), configurable: true });
Object.defineProperty(Kinds.prototype, "native", { get: Date.now.bind(null) === undefined ? undefined : Object.getOwnPropertyDescriptor(Map.prototype, "size").get, configurable: true });
function kinds(n) { let s = ""; for (let i = 0; i < n; i++) { const k = new Kinds(); let nat; try { nat = k.native; } catch (e) { nat = e.constructor.name; } s = k.strictThis + "," + k.sloppyThis + "," + k.arrow + "," + k.bound + "," + nat; } return s; }
log("kinds", kinds(N));
// The prototype changes while the sites are hot.
log("before", run(600).split("|")[1]);
Object.defineProperty(Node0.prototype, "tagName", { get: function () { return "replaced:" + this._tag; }, configurable: true });
log("replaced", run(600).split("|")[1], read(mk(0)));
Object.defineProperty(Node0.prototype, "double", { value: 7, writable: true, configurable: true });
log("as data", run(600).split("|")[1]);
delete Elem.prototype.nodeName;
log("deleted", run(600).split("|")[1]);
Node0.prototype.nodeName = "inherited-data";
Elem.prototype.hasAttrs = undefined;
log("added", run(600).split("|")[1], read(mk(1)));
const swapped = new Elem("q", 5); Object.setPrototypeOf(swapped, { get tagName() { return "other-proto"; } });
log("swapped", swapped.tagName, read(new Elem("q", 5)));
// Object.create receivers, a null prototype, and a getter on Object.prototype itself.
const proto = { get area() { return this.w * this.h; }, get label() { return "box" + this.w; } };
function boxes(n) { let s = 0, l = ""; for (let i = 0; i < n; i++) { const b = Object.create(proto); b.w = i & 15; b.h = 2; if (i & 1) b.z = 1; s += b.area; l = b.label; } return s + ":" + l; }
log("create", boxes(N));
Object.defineProperty(Object.prototype, "viaObjectProto", { get: function () { return "op:" + typeof this; }, configurable: true });
function plain(n) { let s = ""; for (let i = 0; i < n; i++) { const o = { a: i }; const b = Object.create(null); b.k = i; s = o.viaObjectProto + "," + String(b.viaObjectProto) + "," + new Elem("e", i).viaObjectProto; } return s; }
log("object-proto", plain(N));
delete Object.prototype.viaObjectProto;
log("after delete", plain(50));
console.log(out.join("\n"));
"#;

fn run_ok(src: &str) -> Vec<String> {
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    out.output.iter().flat_map(|l| l.lines().map(str::to_string)).collect()
}

#[test]
fn es5_proto_getters_answers_as_node() {
    assert_eq!(
        run_ok(PROGRAM),
        [
            "mixed 198114|A3/undefined/7998/undefined/node/undefined/undefined A0/undefined/0/undefined/node/undefined/undefined DIV/DIV/2/true/node/undefined/undefined",
            "shadow OWN4,7992;P,7994;P,7996;P,7998;",
            "throws 7993502 risky1499;risky2999;",
            "kinds object,true,object,bound-this,TypeError",
            "before SPECIAL:s/SPECIAL:s/1198/true/node/undefined/undefined",
            "replaced SPECIAL:s/SPECIAL:s/1198/true/node/undefined/undefined replaced:a0/undefined/0/undefined/node/undefined/undefined",
            "as data SPECIAL:s/SPECIAL:s/7/true/node/undefined/undefined",
            "deleted SPECIAL:s/undefined/7/true/node/undefined/undefined",
            "added SPECIAL:s/inherited-data/7/true/node/undefined/undefined replaced:div/inherited-data/7/true/node/undefined/undefined",
            "swapped other-proto replaced:q/inherited-data/7/true/node/undefined/undefined",
            "create 60000:box15",
            "object-proto op:object,undefined,op:object",
            "after delete undefined,undefined,undefined",
        ]
    );
}

/// The same answers with the JIT compiling at once, with a collection at
/// every safe point, and with no native code.
#[test]
fn es5_proto_getters_all_modes_answer_identically() {
    let exe = std::env::current_exe().expect("test exe path");
    for (name, key, value) in [
        ("eager", "ZIPP_JIT_THRESHOLD", "1"),
        ("stress", "ZIPP_GC_STRESS", "1"),
        ("nojit", "ZIPP_NOJIT", "1"),
    ] {
        let out = std::process::Command::new(&exe)
            .arg("es5_proto_getters_answers")
            .arg("--nocapture")
            .env(key, value)
            .output()
            .expect("spawn mode child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{name} mode failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !stdout.contains("running 0 tests"),
            "the filter matched nothing in {name} mode"
        );
    }
}
