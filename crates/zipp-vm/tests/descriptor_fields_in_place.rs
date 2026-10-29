//! `Object.defineProperty`'s descriptor read in place, and `Reflect.set`
//! creating a property as a class field's definition does. Every case either
//! shortcut must leave to the general path still behaves: a field inherited
//! from a prototype (an ordinary one, `Object.prototype` given a `get`), a
//! getter for a field and the order its reads run in, a Proxy descriptor, an
//! array or function as the descriptor, the accessor-and-value refusal, a
//! non-callable getter; and for `Reflect.set`, a setter on the receiver's
//! prototype (never run), a receiver unlike the target, a non-extensible
//! receiver, an index key, `__proto__`. The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) { try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); } }
function show(o, k) { var d = Object.getOwnPropertyDescriptor(o, k); return d ? JSON.stringify(d) + (d.get ? " getter" : "") : "none"; }
var o = {};
Object.defineProperty(o, "plain", { value: 1, writable: true, enumerable: true, configurable: true });
Object.defineProperty(o, "partial", { value: 2 });
log("plain", show(o, "plain"), show(o, "partial"));
Object.defineProperty(o, "inherited", Object.create({ value: 3, writable: true, enumerable: true }));
log("inherited", show(o, "inherited"));
var reads = [];
Object.defineProperty(o, "getters", { get value() { reads.push("value"); return 4; }, get enumerable() { reads.push("enumerable"); return 1; }, configurable: 0 });
log("getters", show(o, "getters"), reads.join());
var traps = [];
var pd = new Proxy({ value: 5, writable: true }, { has(t, k) { traps.push("has " + k); return k in t; }, get(t, k) { traps.push("get " + k); return t[k]; } });
Object.defineProperty(o, "proxied", pd);
log("proxy descriptor", show(o, "proxied"), traps.join());
var arr = [9]; arr.value = 6;
Object.defineProperty(o, "array", arr);
var fn = function () {}; fn.value = 7; fn.enumerable = true;
Object.defineProperty(o, "function", fn);
log("array and function", show(o, "array"), show(o, "function"));
attempt("both", function () { return Object.defineProperty({}, "x", { value: 1, get: function () {} }); });
attempt("uncallable getter", function () { return Object.defineProperty({}, "x", { get: 1 }); });
attempt("uncallable setter", function () { return Object.defineProperty({}, "x", { set: {} }); });
var acc = {};
Object.defineProperty(acc, "a", { get: function () { return "got"; }, set: undefined, enumerable: true });
log("accessor", acc.a, show(acc, "a"));
Object.prototype.get = function () { return "polluted"; };
attempt("polluted get", function () { var t = {}; Object.defineProperty(t, "x", { enumerable: true }); return t.x; });
attempt("polluted get with value", function () { return Object.defineProperty({}, "x", { value: 1 }); });
delete Object.prototype.get;
Object.prototype.writable = true;
var w = {}; Object.defineProperty(w, "x", { value: 1 });
log("polluted writable", show(w, "x"));
delete Object.prototype.writable;
log("Reflect.defineProperty", Reflect.defineProperty({}, "x", { value: 1 }), Reflect.defineProperty(Object.freeze({}), "x", { value: 1 }));

var setterRan = [];
var proto = { set s(v) { setterRan.push(v); } };
var target = Object.create(proto);
log("setter on prototype", Reflect.set(target, "s", 1), setterRan.join(), show(target, "s"));
var recv = Object.create(proto);
log("other receiver", Reflect.set({}, "s", 2, recv), setterRan.join(), show(recv, "s"));
var fresh = {};
log("fresh", Reflect.set(fresh, "a", 1), Reflect.set(fresh, "b", 2), JSON.stringify(Object.keys(fresh)), show(fresh, "b"));
var ne = Object.preventExtensions({});
log("non-extensible", Reflect.set(ne, "a", 1), "a" in ne);
var idx = {};
log("index keys", Reflect.set(idx, "2", "two"), Reflect.set(idx, "1", "one"), Reflect.set(idx, "b", "b"), JSON.stringify(Object.keys(idx)));
var pp = {};
log("__proto__", Reflect.set(pp, "__proto__", null), Object.getPrototypeOf(pp) === null, Object.keys(pp).length);
var many = {};
for (var i = 0; i < 40; i++) Reflect.set(many, "k" + i, i);
log("many", Object.keys(many).length, many.k39, show(many, "k20"));
class C { constructor() { this.a = 1; } }
var c = new C();
log("class instance", Reflect.set(c, "b", 2), c.b, c instanceof C, JSON.stringify(Object.keys(c)));
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
fn descriptor_fields_in_place_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
