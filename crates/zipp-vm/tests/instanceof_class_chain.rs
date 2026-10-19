//! `instanceof` against a class answers a class instance from its class
//! chain, and remembers per class that no `Symbol.hasInstance` of its own
//! can intercept the test. Each shortcut must give way when the
//! prototype chain or the statics change:
//! - a re-prototyped intermediate class prototype or instance;
//! - a `Reflect.construct` instance with a foreign newTarget;
//! - built-in, function and null parents;
//! - class prototypes themselves;
//! - two evaluations of one class expression;
//! - a static `Symbol.hasInstance` method, getter or field, inherited or
//!   added to a warmed parent later, then deleted;
//! - a frozen prototype.
//!
//! Expected output is node's.

#[test]
fn instanceof_follows_the_live_class_chain() {
    let src = r###""use strict";
const out = [];
const t = (f) => { try { return String(f()); } catch (e) { return e.constructor.name; } };
class A {}
class B extends A {}
class C extends B {}
class X {}
const c = new C(), b = new B(), x = new X();
const row = (v) => [A, B, C, X].map((K) => (v instanceof K ? 1 : 0)).join("");
let s = "";
for (let i = 0; i < 2000; i++) s = row(c) + row(b) + row(x);
out.push("1 " + s);
// class prototypes and Object.create
out.push("2 " + row(C.prototype) + " " + row(B.prototype) + " " + row(Object.create(C.prototype)) + " " + row(A.prototype));
// re-prototype an intermediate level, then an instance
Object.setPrototypeOf(B.prototype, X.prototype);
for (let i = 0; i < 2000; i++) s = row(c) + row(b) + row(new C());
out.push("3 " + s);
Object.setPrototypeOf(B.prototype, A.prototype);
out.push("4 " + row(c) + row(b));
const c2 = new C();
Object.setPrototypeOf(c2, X.prototype);
out.push("5 " + row(c2) + " " + row(new C()));
// Reflect.construct with a foreign newTarget
const r = Reflect.construct(A, [], X);
const r2 = Reflect.construct(C, [], X);
out.push("6 " + row(r) + " " + row(r2));
// built-in, function and null parents
class E extends Error {}
function F() {}
class G extends F {}
class N extends null { constructor() { return Object.create(N.prototype); } }
const e = new E("m"), g = new G(), n = new N();
out.push("7 " + [e instanceof E, e instanceof Error, e instanceof A, g instanceof G, g instanceof F, g instanceof A, n instanceof N, n instanceof A].map(Number).join(""));
// two evaluations of one class expression
const mk = () => class K extends A {};
const K1 = mk(), K2 = mk();
const k1 = new K1();
out.push("8 " + [k1 instanceof K1, k1 instanceof K2, k1 instanceof A].map(Number).join(""));
// an inherited static @@hasInstance, and one added later to a parent
class H { static [Symbol.hasInstance](v) { return v === 42; } }
class H2 extends H {}
out.push("9 " + [42 instanceof H2, new H2() instanceof H2, new H() instanceof H].map(Number).join(""));
Object.defineProperty(A, Symbol.hasInstance, { value: (v) => v === "yes", configurable: true });
out.push("10 " + [c instanceof C, c instanceof A, "yes" instanceof C].map(Number).join(""));
delete A[Symbol.hasInstance];
out.push("11 " + row(c));
// a static getter, a static field, and one added to a warmed parent's parent
class SG { static get [Symbol.hasInstance]() { return (v) => v === 1; } }
class SF { static [Symbol.hasInstance] = (v) => v === 2; }
class SG2 extends SG {}
out.push("10b " + [1 instanceof SG2, 2 instanceof SF, new SF() instanceof SF].map(Number).join(""));
class W {}
class W2 extends W {}
class W3 extends W2 {}
const w3 = new W3();
let ws = 0;
for (let i = 0; i < 3000; i++) ws += w3 instanceof W3 ? 1 : 0;
Object.defineProperty(W, Symbol.hasInstance, { value: () => false, configurable: true });
out.push("10c " + ws + " " + [w3 instanceof W3, w3 instanceof W2, w3 instanceof W].map(Number).join(""));
W2.extra = 1;
delete W[Symbol.hasInstance];
out.push("10d " + [w3 instanceof W3, w3 instanceof W].map(Number).join(""));
// a frozen prototype keeps the chain
Object.freeze(B.prototype);
out.push("12 " + row(c) + row(new C()));
// a non-object right side, and a primitive left side
out.push("13 " + t(() => c instanceof c) + " " + t(() => 5 instanceof C));
console.log(out.join("\n"));
"###;
    let out = zipp_vm::run(src).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), r###"1 111011000001
2 1100 1000 1110 0000
3 011101010111
4 11101100
5 0001 1110
6 0001 0001
7 11011010
8 101
9 100
10 001
11 1110
10b 110
10c 3000 000
10d 11
12 11101110
13 TypeError false"###);
}
