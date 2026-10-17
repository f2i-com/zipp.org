//! Proxy traps whose invariant checks read an ordinary target's attributes
//! in place, a forwarding `set` trap whose `Reflect.set` with the proxy as
//! receiver goes straight to the target when the handler has neither a
//! `getOwnPropertyDescriptor` nor a `defineProperty` trap, and `in`,
//! `hasOwnProperty` and `Object.hasOwn` reading a string key where it lies,
//! on dictionaries large enough to be indexed and through deletes. The
//! expected lines are Node's.

const SOURCE: &str = r#"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) {
  try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); }
}

// A logging proxy: every trap forwards through Reflect with the receiver.
var counts = { get: 0, set: 0, has: 0 };
var seen = [];
var target = { a: 1, b: 2, total: 0 };
var obs = new Proxy(target, {
  get: function (t, k, r) { counts.get++; seen.push(typeof k + ":" + String(k)); return Reflect.get(t, k, r); },
  set: function (t, k, v, r) { counts.set++; return Reflect.set(t, k, v, r); },
  has: function (t, k) { counts.has++; seen.push("has " + typeof k + ":" + String(k)); return k in t; }
});
for (var i = 0; i < 50; i++) { obs.total = obs.total + obs.a * obs.b; if ("b" in obs) obs.b = obs.b + 1; }
obs.fresh = 7;
log("observable", JSON.stringify(target), counts.get, counts.set, counts.has, 1 in obs, "missing" in obs);
log("keys seen", seen.slice(0, 4).join(","), seen[seen.length - 1]);
var sym = Symbol("s");
obs[sym] = 3;
log("symbol", obs[sym], typeof Object.getOwnPropertySymbols(target)[0]);

// Reflect.set with a proxy receiver: each trap the handler has runs, in order.
var order = [];
var traced = new Proxy({ x: 1 }, {
  getOwnPropertyDescriptor: function (t, k) { order.push("gopd " + k); return Reflect.getOwnPropertyDescriptor(t, k); },
  defineProperty: function (t, k, d) { order.push("define " + k + " " + JSON.stringify(d)); return Reflect.defineProperty(t, k, d); }
});
log("traced", Reflect.set({}, "x", 5, traced), Reflect.set({}, "y", 6, traced), order.join(" | "));
// An inherited trap is a trap.
var inherited = new Proxy({ x: 1 }, Object.create({
  defineProperty: function (t, k, d) { order.push("inherited " + k); return Reflect.defineProperty(t, k, d); }
}));
order = [];
log("inherited", Reflect.set({}, "x", 9, inherited), order.join(" | "));
// A getter for a trap name runs.
var gets = 0;
var getterHandler = {};
Object.defineProperty(getterHandler, "defineProperty", { get: function () { gets++; return undefined; } });
var viaGetter = new Proxy({ x: 1 }, getterHandler);
log("trap getter", Reflect.set({}, "x", 2, viaGetter), gets, JSON.stringify(Object.getOwnPropertyDescriptor(viaGetter, "x")));
// No traps: the target decides.
var plainTarget = { w: 1 };
Object.defineProperty(plainTarget, "ro", { value: 1, writable: false, configurable: true });
Object.defineProperty(plainTarget, "acc", { get: function () { return 4; }, configurable: true });
var bare = new Proxy(plainTarget, {});
log("bare", Reflect.set({}, "w", 5, bare), Reflect.set({}, "ro", 5, bare), Reflect.set({}, "acc", 5, bare),
    Reflect.set({}, "nu", 8, bare), JSON.stringify(Object.getOwnPropertyDescriptor(plainTarget, "nu")), plainTarget.w, plainTarget.ro);
var nested = new Proxy(new Proxy({ deep: 1 }, {
  defineProperty: function (t, k, d) { order.push("inner " + k); return Reflect.defineProperty(t, k, d); }
}), {});
order = [];
log("nested", Reflect.set({}, "deep", 3, nested), order.join(" | "));
var arrTarget = new Proxy([1, 2, 3], {});
log("array target", Reflect.set({}, "length", 1, arrTarget), Reflect.set({}, "0", 9, arrTarget), JSON.stringify(Reflect.get(arrTarget, "length")));

// Invariants the target's own attributes impose on the traps.
var frozen = Object.freeze({ k: 1 });
attempt("get lies about frozen", function () { return new Proxy(frozen, { get: function () { return 2; } }).k; });
attempt("get tells frozen truth", function () { return new Proxy(frozen, { get: function () { return 1; } }).k; });
attempt("set claims frozen", function () { var p = new Proxy(frozen, { set: function () { return true; } }); p.k = 5; return frozen.k; });
attempt("set same frozen value", function () { var p = new Proxy(frozen, { set: function () { return true; } }); p.k = 1; return "same"; });
var noSetter = {};
Object.defineProperty(noSetter, "g", { get: function () { return 1; }, configurable: false });
attempt("set claims accessor", function () { var p = new Proxy(noSetter, { set: function () { return true; } }); p.g = 5; return "no"; });
attempt("get accessor without getter", function () {
  var t = {}; Object.defineProperty(t, "s", { set: function () {}, configurable: false });
  return new Proxy(t, { get: function () { return 3; } }).s;
});
attempt("configurable lie allowed", function () {
  var t = {}; Object.defineProperty(t, "c", { value: 1, writable: false, configurable: true });
  return new Proxy(t, { get: function () { return 9; } }).c;
});
attempt("has hides non-configurable", function () {
  var t = {}; Object.defineProperty(t, "n", { value: 1, configurable: false });
  return "n" in new Proxy(t, { has: function () { return false; } });
});
attempt("has hides on sealed", function () {
  return "q" in new Proxy(Object.preventExtensions({ q: 1 }), { has: function () { return false; } });
});

// Key probes on an indexed dictionary, through deletes, with concatenated keys.
var d = {};
var names = [];
for (var i = 0; i < 300; i++) { names.push("k" + (i * 7919 % 1000)); d[names[i]] = i; }
for (var i = 0; i < 40; i++) d[String(1000 + i * 3)] = -i;
for (var i = 0; i < 300; i += 3) delete d[names[i]];
for (var i = 0; i < 40; i += 4) delete d[String(1000 + i * 3)];
var hop = Object.prototype.hasOwnProperty;
var row = [];
for (var i = 0; i < 300; i += 13) {
  var rope = "k" + String(i * 7919 % 1000);
  row.push([rope in d, hop.call(d, rope), Object.hasOwn(d, rope), d[rope], (rope + "z") in d].join("/"));
}
log("dict", row.join(" "));
log("dict ints", 1003 in d, "1003" in d, hop.call(d, 1012), Object.hasOwn(d, "1012"), Object.hasOwn(d, 1012), d[1015], Object.keys(d).length);
var proto = { inherited: 1 };
var child = Object.create(proto);
child.own = 2;
var bare2 = Object.create(null);
bare2.x = 1;
log("chains", "inherited" in child, "own" in child, "toString" in child, "toString" in bare2, "x" in bare2,
    hop.call(child, "inherited"), Object.hasOwn(child, "own"), "@@iterator" in child);
class C { constructor() { this.f = 1; } m() {} }
var c = new C();
log("class", "f" in c, "m" in c, "constructor" in c, "nope" in c, hop.call(c, "m"), Object.hasOwn(c, "f"));
var arr = [1, , 3];
log("array", 0 in arr, 1 in arr, "length" in arr, Object.hasOwn(arr, 1), Object.hasOwn(arr, "2"), hop.call(arr, "length"));
attempt("hasOwn null", function () { return Object.hasOwn(null, "x"); });
attempt("hasOwn key coercion", function () { return Object.hasOwn({ ab: 1 }, { toString: function () { return "ab"; } }); });
console.log(out.join("\n"));
"#;

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
fn proxy_traps_and_key_probes_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
