//! A `return` through `finally` in compiled code: nested and replaced finallys,
//! break and continue out of one, catch, for-of closing, generators, constructors
//! and recursion. Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"8997 3 9 4 3003
9165 4 5 9007
2999 -8 9
3001 3
broke broke
out out 2
2999 5
3000 3001
1
undefined undefined undefined
3000 2
2999 2
5998 8
2999 3
fin2999
2999 2999,4
6 3000
3001
2
9:5:true
3000
2999
2999
TypeError
1
2999
2
49 1000
2999 string
number
async 1"####;

const PROGRAM: &str = r####"// `return` through `finally` in hot functions.
const HOT = 3000;
var fin = 0, log = [];
function hot(f) { var r; for (var i = 0; i < HOT; i++) r = f(i); return r; }
function a(x) { try { if (x & 1) return x * 3; if (x & 2) return x + 7; } finally { fin++; } return x; }
console.log(hot(a), a(1), a(2), a(4), fin);
function b(x) { var s = x; try { try { s = (s + 1) | 0; if (s & 4) return s; } finally { s ^= 0x55; fin++; } s = (s + 2) | 0; } finally { s = (s * 3) | 0; fin++; } return s; }
console.log(hot(b), b(3), b(4), fin);
// A return in `finally` replaces the pending one; a break cancels it.
function c(x) { try { return x; } finally { fin++; if ((x & 7) === 0) return -x; } }
console.log(hot(c), c(8), c(9));
function d(x) { for (var i = 0; i < 3; i++) { try { return x + i; } finally { if (i < 2) continue; } } return -1; }
console.log(hot(d), d(1));
function e(x) { while (true) { try { return x; } finally { break; } } return "broke"; }
console.log(hot(e), e(1));
function f(x) { lbl: try { return x; } finally { if (x & 1) break lbl; } return "out"; }
console.log(hot(f), f(1), f(2));
// The value is taken before the finally runs.
function g(x) { var s = x; try { return s; } finally { s = 99; } }
console.log(hot(g), g(5));
var o = { v: 1 };
function h() { try { return o.v; } finally { o.v++; } }
console.log(hot(h), o.v);
function i2() { var a = []; try { return a; } finally { a.push(1); } }
console.log(hot(i2).length);
// Undefined, in a void function, in a catch and in nested catch-finally.
function j(x) { try { if (x) return; fin++; } finally { log.length = 0; } }
console.log(hot(j), j(0), j(1));
function k(x) { try { throw x; } catch (e) { return e + 1; } finally { fin++; } }
console.log(hot(k), k(1));
function l(x) { try { try { return x; } catch (e) { return -1; } } finally { fin++; } }
console.log(hot(l), l(2));
function m(x) { try { try { throw x; } finally { fin++; } } catch (e) { return e * 2; } finally { fin++; } }
console.log(hot(m), m(4));
function n(x) { try { return x; } finally { try { throw 1; } catch (e) { fin += e; } } }
console.log(hot(n), n(3));
// A finally that throws replaces the return; one that calls and allocates.
function p(x) { try { return x; } finally { if (x === HOT - 1) throw new RangeError("fin" + x); } }
try { hot(p); } catch (e) { console.log(e.message); }
function q(x) { var c = 0; function bump() { c += 2; } try { return c + x; } finally { bump(); bump(); log = [x, c]; } }
console.log(hot(q), log.join());
// Returns from for-of, closing the iterator; through a user iterator; a generator.
var closed = 0;
var it = { [Symbol.iterator]() { var i = 0; return { next() { return { done: i > 5, value: i++ }; }, return() { closed++; return {}; } }; } };
function r(x) { for (var v of it) { if (v === x % 4) return v * 2; } return -1; }
console.log(hot(r), closed);
function s(x) { for (var v of [1, 2, 3]) { try { if (v === 2) return x + v; } finally { fin++; } } return 0; }
console.log(hot(s));
function* gn() { try { yield 1; return 2; } finally { fin++; } }
function t() { var g = gn(); g.next(); return g.next().value; }
console.log(hot(t));
function* gr() { try { yield 1; yield 2; } finally { yield 9; } }
function u() { var g = gr(); g.next(); var a = g.return(5), b = g.next(); return a.value + ":" + b.value + ":" + b.done; }
console.log(hot(u));
async function v(x) { try { return x; } finally { fin++; } }
v(1).then((x) => console.log("async", x));
// A constructor, a derived one, a getter, an arrow, `arguments`, a recursion.
function C(x) { try { this.x = x; return; } finally { this.y = 1; } }
console.log(hot((i) => new C(i).x + new C(i).y));
function C2(x) { try { return { z: x }; } finally { fin++; } }
console.log(hot((i) => new C2(i).z));
class B { constructor() { this.b = 1; } }
class D extends B { constructor(x) { try { super(); return; } finally { this.d = x; } } }
console.log(hot((i) => new D(i).d));
class E2 extends B { constructor(x) { super(); try { return x; } finally { fin++; } } }
try { new E2(1); } catch (e) { console.log(e.name); }
var ob = { get g() { try { return 1; } finally { fin++; } } };
console.log(hot(() => ob.g));
var ar = (x) => { try { return x; } finally { fin++; } };
console.log(hot(ar));
function w() { try { return arguments.length; } finally { arguments[0] = 1; } }
console.log(hot((i) => w(1, 2)));
function rec(n) { try { if (n === 0) return 0; return 1 + rec(n - 1); } finally { fin++; } }
console.log(hot((i) => rec(i % 50)), rec(1000));
// Stack trace depth at a throw after a routed return.
function st(x) { try { return x; } finally { if (x === HOT - 1) { var e = new Error("t"); log = [typeof e.stack]; } } }
console.log(hot(st), log.join());
console.log(typeof fin);
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
