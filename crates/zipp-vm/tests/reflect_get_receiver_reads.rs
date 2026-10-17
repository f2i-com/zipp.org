//! `Reflect.get(target, key, receiver)` with a receiver other than the target
//! (what a forwarding Proxy trap does) over objects and arrays, and the
//! get-trap invariants checked against array targets. The expected lines
//! are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) {
  try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); }
}
var recv = { who: "receiver" };
var obj = { a: 1, "@@iterator": "spelled", 7: "seven", get g() { return this.who; } };
Object.defineProperty(obj, "ro", { value: "fixed", writable: false, configurable: false });
log("object", Reflect.get(obj, "a", recv), Reflect.get(obj, "@@iterator", recv), Reflect.get(obj, "7", recv),
  Reflect.get(obj, 7, recv), Reflect.get(obj, "g", recv), Reflect.get(obj, "ro", recv), Reflect.get(obj, "toString", recv) === Object.prototype.toString,
  Reflect.get(obj, "missing", recv));
var proto = { inherited: "from proto", get pg() { return this.who; } };
var child = Object.create(proto);
child.own = "own";
log("chain", Reflect.get(child, "own", recv), Reflect.get(child, "inherited", recv), Reflect.get(child, "pg", recv));
var arr = [10, 20, , 40];
arr.length = 6;
log("array", Reflect.get(arr, "0", recv), Reflect.get(arr, "2", recv), Reflect.get(arr, "5", recv), Reflect.get(arr, "length", recv),
  Reflect.get(arr, "01", recv), Reflect.get(arr, "4294967295", recv), Reflect.get(arr, "push", recv) === Array.prototype.push);
Array.prototype[2] = "from Array.prototype";
log("hole reads the chain", Reflect.get(arr, "2", recv), Reflect.get([, ], "0", recv));
delete Array.prototype[2];
var extra = [1, 2, 3];
extra.label = "named";
Object.defineProperty(extra, "1", { get: function () { return "getter sees " + this.who; } });
log("array with own extras", Reflect.get(extra, "label", recv), Reflect.get(extra, "1", recv), Reflect.get(extra, "length", recv));
var sparse = [];
sparse[100000] = "far";
log("sparse", Reflect.get(sparse, "100000", recv), Reflect.get(sparse, "length", recv), Reflect.get(sparse, "5", recv));
function mapped(a, b) { a = "reassigned"; return [Reflect.get(arguments, "0", recv), Reflect.get(arguments, "length", recv), Reflect.get(arguments, "1", recv)].join(); }
function strict(a) { "use strict"; a = "reassigned"; return [Reflect.get(arguments, "0", recv), Reflect.get(arguments, "length", recv)].join(); }
log("arguments", mapped("x", "y"), strict("x"));
var traps = 0;
function forwarding(target) {
  return new Proxy(target, { get: function (t, k, r) { traps++; return Reflect.get(t, k, r); } });
}
var pa = forwarding([5, 6, 7]);
log("through a proxy", pa[0], pa[2], pa[3], pa.length, pa["1"], traps);
var po = forwarding({ x: 1, get self() { return this === po; } });
log("proxy receiver", po.x, po.self);
function lying(target, value) {
  return new Proxy(target, { get: function (t, k) { return k === "length" || k === "0" ? value : Reflect.get(t, k); } });
}
var fixedLen = [1, 2];
Object.defineProperty(fixedLen, "length", { writable: false });
attempt("non-writable length, lie", function () { return lying(fixedLen, 99).length; });
attempt("non-writable length, truth", function () { return lying(fixedLen, 2).length; });
attempt("writable length, lie", function () { return lying([1, 2], 99).length; });
attempt("frozen element, lie", function () { return lying(Object.freeze([1, 2]), 99)[0]; });
attempt("frozen element, truth", function () { return lying(Object.freeze([1, 2]), 1)[0]; });
var sealedEl = [1, 2];
Object.defineProperty(sealedEl, "0", { value: 1, writable: false, configurable: false });
attempt("non-configurable element, lie", function () { return lying(sealedEl, 99)[0]; });
attempt("plain element, lie", function () { return lying([1, 2], 99)[0]; });
function argsTarget() { return arguments; }
attempt("arguments length, lie", function () { return lying(argsTarget(1, 2), 99).length; });
var base = { data: "base data", get scaled() { return this.raw * 3; }, set onlySet(v) {} };
var mid = Object.create(base);
mid.midData = "mid data";
var leaf = Object.create(mid);
leaf.raw = 5;
var other = { raw: 7 };
log("chain getters", Reflect.get(base, "scaled", leaf), Reflect.get(base, "scaled", other), Reflect.get(leaf, "scaled", other),
  Reflect.get(leaf, "data", other), Reflect.get(leaf, "midData", other), String(Reflect.get(leaf, "onlySet", other)),
  String(Reflect.get(leaf, "nowhere", other)));
var bare = Object.create(null);
bare.x = "bare x";
var overBare = Object.create(bare);
log("null chains", Reflect.get(overBare, "x", other), String(Reflect.get(overBare, "toString", other)), String(Reflect.get(bare, "missing", other)));
class Shape { area() { return "area of " + this.raw; } get kind() { return "kind " + this.raw; } }
var viaClass = Object.create(Shape.prototype);
log("class prototype", typeof Reflect.get(viaClass, "area", other), Reflect.get(viaClass, "kind", other), Reflect.get(new Shape(), "kind", other));
var trapped = [];
var proxyProto = new Proxy({ fromProxy: "p" }, { get: function (t, k, r) { trapped.push(String(k)); return Reflect.get(t, k, r); } });
var overProxy = Object.create(proxyProto);
log("proxy in the chain", Reflect.get(overProxy, "fromProxy", other), String(Reflect.get(overProxy, "absent", other)), trapped.join());
var overArray = Object.create([1, 2, 3]);
log("array in the chain", Reflect.get(overArray, "1", other), Reflect.get(overArray, "length", other), typeof Reflect.get(overArray, "map", other));
var thrower = Object.create({ get boom() { throw new RangeError("from getter"); } });
attempt("throwing getter", function () { return Reflect.get(thrower, "boom", other); });
log("__proto__", Reflect.get(leaf, "__proto__", other) === Object.prototype, Reflect.get(leaf, "__proto__", leaf) === mid);
Object.defineProperty(Object.prototype, "everywhere", { get: function () { return "seen " + this.raw; }, configurable: true });
log("Object.prototype getter", Reflect.get(leaf, "everywhere", other), Reflect.get({}, "everywhere", leaf));
delete Object.prototype.everywhere;
log("after removal", String(Reflect.get(leaf, "everywhere", other)));
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
fn reflect_get_with_a_receiver_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
