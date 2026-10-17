//! `p.key` on a Proxy goes straight to its [[Get]], and the post-trap
//! invariant answers "none applies" for a target whose attributes are all
//! default. Every trap outcome must still match: a value, no trap (the
//! target's [[Get]] with the proxy as receiver), a null or non-callable
//! trap, a revoked proxy, both invariants (a non-configurable non-writable
//! data property and a getter-less non-configurable accessor), frozen and
//! sealed targets, one made non-configurable during the trap, a proxy as a
//! prototype, nested proxies, a throwing trap, an array target's `length`,
//! and `set` traps against the same invariants. The expected lines are
//! Node's.

const SOURCE: &str = r##"
var out = [];
var log = function () { out.push(Array.prototype.join.call(arguments, " ")); };
function attempt(label, f) { try { log(label, "ok", String(f())); } catch (e) { log(label, "threw", e.constructor.name); } }
var seen = [];
var p = new Proxy({ v: 1, w: 2 }, { get(t, k, r) { seen.push(k); return k === "v" ? 10 : Reflect.get(t, k, r); } });
var s = 0; for (var i = 0; i < 20; i++) s += p.v + p.w;
log("trap", s, seen.length, p.missing);
var target = { get g() { return this === fwd ? "receiver is proxy" : "other"; }, d: 5 };
var fwd = new Proxy(target, {});
log("no trap", fwd.d, fwd.g, fwd.nope);
log("null trap", new Proxy({ a: 1 }, { get: null }).a, new Proxy({ a: 2 }, { get: undefined }).a);
attempt("non-callable trap", function () { return new Proxy({ a: 1 }, { get: 5 }).a; });
var r = Proxy.revocable({ a: 1 }, {}); r.revoke();
attempt("revoked", function () { return r.proxy.a; });
var fixed = {}; Object.defineProperty(fixed, "k", { value: 1, writable: false, configurable: false });
attempt("invariant same", function () { return new Proxy(fixed, { get() { return 1; } }).k; });
attempt("invariant differs", function () { return new Proxy(fixed, { get() { return 2; } }).k; });
attempt("invariant other key", function () { return new Proxy(fixed, { get() { return 2; } }).other; });
var acc = {}; Object.defineProperty(acc, "a", { set: function () {}, configurable: false });
attempt("accessor undefined", function () { return new Proxy(acc, { get() { return undefined; } }).a; });
attempt("accessor value", function () { return new Proxy(acc, { get() { return 1; } }).a; });
var frozen = Object.freeze({ f: 1 });
attempt("frozen same", function () { return new Proxy(frozen, { get() { return 1; } }).f; });
attempt("frozen differs", function () { return new Proxy(frozen, { get() { return 9; } }).f; });
var sealed = Object.seal({ s: 1 });
attempt("sealed differs", function () { return new Proxy(sealed, { get() { return 9; } }).s; });
var late = { x: 1 };
attempt("made fixed in trap", function () { return new Proxy(late, { get(t, k) { Object.defineProperty(t, "x", { value: 1, writable: false, configurable: false }); return 2; } }).x; });
var proto = new Proxy({ inherited: "from proxy" }, { get(t, k, r) { return k === "who" ? (r === child ? "child receiver" : "other") : Reflect.get(t, k, r); } });
var child = Object.create(proto); child.own = 1;
log("as prototype", child.own, child.inherited, child.who);
var inner = new Proxy({ deep: 1 }, { get(t, k) { return "inner:" + String(k); } });
var outer = new Proxy(inner, { get(t, k, r) { return "outer(" + t[k] + ")"; } });
log("nested", outer.deep, new Proxy(inner, {}).deep);
attempt("throwing trap", function () { return new Proxy({}, { get() { throw new RangeError("no"); } }).x; });
var arrP = new Proxy([1, 2, 3], {});
log("array length", arrP.length, new Proxy([1, 2], { get(t, k) { return k === "length" ? 99 : t[k]; } }).length);
var sp = new Proxy({}, { set(t, k, v) { t[k] = v * 2; return true; } });
sp.a = 2; sp.b = 3;
log("set trap", sp.a, sp.b);
attempt("set invariant", function () { "use strict"; var q = new Proxy(fixed, { set() { return true; } }); q.k = 5; return "set"; });
attempt("set falsish strict", function () { "use strict"; var q = new Proxy({}, { set() { return false; } }); q.z = 1; return "set"; });
var count = 0; var hot = new Proxy({ n: 0 }, { get(t, k) { count++; return t[k]; }, set(t, k, v) { t[k] = v; return true; } });
for (var i = 0; i < 50; i++) hot.n = hot.n + 1;
log("hot", hot.n, count);
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
fn proxy_get_direct_matches_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    assert_eq!(lines, node_output(SOURCE));
}
