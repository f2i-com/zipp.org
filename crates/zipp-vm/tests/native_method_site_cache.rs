//! A `CallMethod` site whose receiver is an ordinary object holding the
//! method as its own native (`Reflect.get`, `JSON.stringify`) calls that
//! native through the site's cache. A replacement of the method, an
//! accessor in its place, its deletion and re-addition, other
//! native-holding objects at the same site, `toString` and its kin, a
//! native with a receiver check, and Proxy traps forwarding through
//! `Reflect` must all behave as the full Get-then-call does. Expected
//! output is node's.

#[test]
fn own_native_method_calls_honour_replacement() {
    let src = r###""use strict";
const out = [];
const t = (f) => { try { return String(f()); } catch (e) { return e.constructor.name; } };
// a namespace object's own native method, warmed then replaced
const o = { a: 1, b: 2 };
let s = 0;
const step = () => Reflect.get(o, "a") + Reflect.get(o, "b");
for (let i = 0; i < 3000; i++) s += step();
out.push("1 " + s);
const origGet = Reflect.get;
Reflect.get = function (tt, k) { return "patched:" + k; };
out.push("2 " + step());
Reflect.get = origGet;
out.push("3 " + step());
Object.defineProperty(Reflect, "get", { get() { return () => "acc"; }, configurable: true });
out.push("4 " + step());
Object.defineProperty(Reflect, "get", { value: origGet, writable: true, configurable: true });
out.push("5 " + step());
delete Reflect.get;
out.push("6 " + t(step));
Reflect.get = origGet;
out.push("7 " + step());
// same name on a different native-holding object at one site
const ns = [JSON, { stringify: (v) => "own:" + v }, Object.freeze({ stringify: JSON.stringify })];
let js = [];
for (let i = 0; i < 30; i++) js.push(ns[i % 3].stringify({ i: i % 3 }));
out.push("8 " + js.slice(0, 3).join("|"));
// toString and kin keep the dispatcher's path
const tobj = { toString: Object.prototype.toString, valueOf: Object.prototype.valueOf };
out.push("9 " + tobj.toString() + " " + (tobj.valueOf() === tobj));
// console.log-like natives with a receiver check: Map.prototype.get on a non-Map
const fake = { get: Map.prototype.get };
out.push("10 " + t(() => fake.get(1)));
// a revoked proxy and a trap-less proxy through a computed key
const { proxy: rp, revoke } = Proxy.revocable({ x: 1 }, {});
const kx = "x";
out.push("12 " + rp[kx]);
revoke();
out.push("13 " + t(() => rp[kx]));
// invariant check still runs for a computed string key
const frozen = Object.freeze({ fixed: 1 });
const liar = new Proxy(frozen, { get() { return 2; } });
const kf = "fix" + "ed";
out.push("14 " + t(() => liar[kf]));
console.log(out.join("\n"));
"###;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), r###"1 9000
2 patched:apatched:b
3 3
4 accacc
5 3
6 TypeError
7 3
8 {"i":0}|own:[object Object]|{"i":2}
9 [object Object] true
10 TypeError
12 1
13 TypeError
14 TypeError"###);
}
