//! `Reflect.set(o, k, v, receiver)` writing a writable data property `o`
//! already has, with `receiver` either `o` or a trap-less-for-descriptors
//! Proxy over `o`, is a store into the slot; and a `has` trap's `false` for
//! a plain target is checked against the target's map. Every case either
//! shortcut must leave to the general path still behaves: a receiver proxy
//! with a defineProperty or getOwnPropertyDescriptor trap, a proxy over
//! another object, a primitive receiver, non-writable, accessor and absent
//! properties, frozen, sealed and non-extensible targets, symbol keys, a
//! class instance, and both `has` invariants. The expected lines are Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) { try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); } }
var t = { a: 1, b: 2 };
log("self receiver", Reflect.set(t, "a", 10), Reflect.set(t, "a", 11, t), t.a);
var traps = [];
var p = new Proxy(t, { set(tt, k, v, r) { traps.push("set " + k); return Reflect.set(tt, k, v, r); }, get(tt, k, r) { return Reflect.get(tt, k, r); } });
for (var i = 0; i < 10; i++) p.b = p.b + 1;
log("forwarding", t.b, traps.length);
var defined = [];
var pd = new Proxy(t, { defineProperty(tt, k, d) { defined.push(k + ":" + JSON.stringify(d)); return Reflect.defineProperty(tt, k, d); } });
log("defineProperty trap", Reflect.set(t, "a", 20, pd), t.a, defined.join());
var gopd = [];
var pg = new Proxy(t, { getOwnPropertyDescriptor(tt, k) { gopd.push(k); return Reflect.getOwnPropertyDescriptor(tt, k); } });
log("gopd trap", Reflect.set(t, "b", 30, pg), t.b, gopd.join());
var other = { a: "other" };
log("proxy over another", Reflect.set(t, "a", 40, new Proxy(other, {})), t.a, other.a);
log("primitive receiver", Reflect.set(t, "a", 50, 5), t.a);
Object.defineProperty(t, "ro", { value: 1, writable: false, configurable: true });
log("non-writable", Reflect.set(t, "ro", 2), Reflect.set(t, "ro", 2, new Proxy(t, {})), t.ro);
var seenThis = [];
Object.defineProperty(t, "acc", { set: function (v) { seenThis.push(this === t ? "t" : this === p ? "p" : "other"); }, configurable: true });
log("accessor", Reflect.set(t, "acc", 1), Reflect.set(t, "acc", 1, p), seenThis.join());
log("absent", Reflect.set(t, "fresh", 1, new Proxy(t, {})), t.fresh, Object.keys(t).join());
var fz = Object.freeze({ f: 1 });
log("frozen", Reflect.set(fz, "f", 2), Reflect.set(fz, "f", 2, new Proxy(fz, {})), fz.f);
var sl = Object.seal({ s: 1 });
log("sealed", Reflect.set(sl, "s", 2), sl.s, Reflect.set(sl, "new", 1), "new" in sl);
var ne = Object.preventExtensions({ n: 1 });
log("non-extensible", Reflect.set(ne, "n", 2, new Proxy(ne, {})), ne.n, Reflect.set(ne, "m", 1), "m" in ne);
var sym = Symbol("k"); var st = { [sym]: 1 };
log("symbol", Reflect.set(st, sym, 2, new Proxy(st, {})), st[sym]);
class K { constructor() { this.v = 1; } }
var k = new K();
log("class instance", Reflect.set(k, "v", 2, new Proxy(k, {})), k.v);
var obs = new Proxy({ total: 0, c: 1 }, {
  get(tt, key, r) { return Reflect.get(tt, key, r); },
  set(tt, key, v, r) { return Reflect.set(tt, key, v, r); },
  has(tt, key) { return key in tt; },
});
var hits = 0;
for (var i = 0; i < 40; i++) { obs.total = obs.total + i; if ("c" in obs) obs.c = obs.c + 1; if ("missing" in obs) hits++; }
log("observable", obs.total, obs.c, hits);
var nc = {}; Object.defineProperty(nc, "fixed", { value: 1, configurable: false });
attempt("has false non-configurable", function () { return "fixed" in new Proxy(nc, { has() { return false; } }); });
attempt("has false absent", function () { return "nope" in new Proxy(nc, { has() { return false; } }); });
attempt("has false configurable", function () { return "x" in new Proxy({ x: 1 }, { has() { return false; } }); });
attempt("has false non-extensible", function () { return "y" in new Proxy(Object.preventExtensions({ y: 1 }), { has() { return false; } }); });
attempt("has false non-extensible absent", function () { return "z" in new Proxy(Object.preventExtensions({ y: 1 }), { has() { return false; } }); });
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
fn reflect_set_forwarding_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
