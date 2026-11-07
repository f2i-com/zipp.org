//! `try ... finally` and for-of bodies with no handler pushed: every way out of
//! one, over arrays, iterators, sets and typed arrays. Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"3000 8997 9 2999 undefined
11 10t 11
151 102 100 105 -2147483548
1 TypeError TypeError
TypeError
5RangeError
SyntaxError SyntaxError
-2999 3001 broke out out EvalError
1 1 20 22 22
2999 2999
10 402 15 10
10 1 6 -1 9 x2 30 0 0
10 1 6 -1 9 x2 30 9000 102000
10 1 6 -1 9 x2 30 0 0
10 1 6 -1 9 x2 30 0 0
3
TypeError
TypeError 1
0ab 3.5 7
6 14 2
39 1000
8998
5999
number true"####;

const PROGRAM: &str = r####"// `try ... finally` and for-of bodies with no handler pushed: every way out.
const HOT = 3000;
var fin = 0, log = [];
function hot(f) { var r; for (var i = 0; i < HOT; i++) r = f(i); return r; }
function t() { throw new Error("t"); }
function noop() {}
// Falling out of the body, returning from it, and the value's register.
function a(x) { try { x = x + 1; } finally { fin++; } return x; }
function b(x) { try { if (x & 1) return x * 3; x = x + 1; } finally { fin++; } return x; }
function c(x) { var r = x; try { return r; } finally { r = -1; } }
function d(x) { try { return; } finally { fin++; } }
console.log(hot(a), hot(b), b(3), hot(c), hot(d));
// A throw and a bail from the body run the finally, once.
function e(x) { var n = 0; try { try { if (x % 7 === 0) t(); n = 1; } finally { n += 10; fin++; } } catch (m) { return n + m.message; } return n; }
console.log(hot(e), e(7), e(8));
function f(x) { var s = 0; try { s = (x + 1) | 0; } finally { s = s + 100; } return s; }
hot(f); console.log(f("5"), f(1.5), f(undefined), f({ valueOf() { return 4; } }), f(2147483647));
function g(o) { var s = 0; try { s = o.x.y; } catch (m) { return m.name; } finally { fin++; } return s; }
console.log(hot(() => g({ x: { y: 1 } })), g({}), g(null));
function h(o) { try { return o.x.y; } finally { fin++; } }
hot(() => h({ x: { y: 1 } })); try { h({}); } catch (m) { console.log(m.name); }
// A getter, a callee and a native throwing inside the body, seen by an outer try.
var ob = { get g() { throw new RangeError("g"); } };
function i2() { var r = 0; try { try { r = ob.g; } finally { r = 5; } } catch (m) { return r + m.name; } }
console.log(hot(i2));
function j(x) { try { try { JSON.parse(x % 500 === 499 ? "{" : "1"); } finally { fin++; } } catch (m) { return m.name; } return "ok"; }
console.log(hot(j), j(499));
// Replaced completions: a return, a break, a continue and a throw in the finally.
function k(x) { try { return x; } finally { if (x & 1) return -x; } }
function l(x) { for (var i = 0; i < 3; i++) { try { return x + i; } finally { if (i < 2) continue; } } return -1; }
function m2(x) { while (true) { try { return x; } finally { break; } } return "broke"; }
function n(x) { lbl: try { return x; } finally { if (x & 1) break lbl; } return "out"; }
function o2(x) { try { try { return x; } finally { if (x === HOT - 1) throw new EvalError("fin"); } } catch (m) { return m.name; } }
console.log(hot(k), hot(l), hot(m2), hot(n), n(1), hot(o2));
// Nested: only the inner bracket can be lazy, and the outer finally still runs.
function p(x) { var s = 0; try { try { if (x & 1) return s + 1; s = 2; } finally { s = 10; fin++; } s = s + 1; } finally { s = s * 2; log = [s]; } return s; }
console.log(hot(p), p(1), log.join(), p(2), log.join());
function q(x) { try { try { return x; } catch (m) { return -1; } } finally { fin++; } }
function r(x) { try { try { t(); } catch (m) { return x; } finally { fin++; } } finally { fin++; } }
console.log(hot(q), hot(r));
// A loop inside the body, a loop around it, and branches on both sides of the pop.
function s(n) { var v = 0; try { for (var i = 0; i < n; i++) v += i; } finally { fin++; } return v; }
function u(n) { var v = 0; for (var i = 0; i < n; i++) { try { if (i === 3) break; if (i % 2) continue; v += i; } finally { v += 100; } } return v; }
function w(n) { var v = 0; for (var i = 0; i < n; i++) { try { v += i; } finally { v += 1; } } return v; }
function x1(n) { var v = 0; for (var i = 0; i < n; i++) { try { if (i === n - 1) return v; v += i; } finally { v += 1; } } return -1; }
console.log(hot(() => s(5)), hot(() => u(6)), hot(() => w(5)), hot(() => x1(5)));
// For-of: exhausted, break, return, throw and continue; arrays and iterators.
var closed = 0, nexts = 0;
var it = { [Symbol.iterator]() { var i = 0; return { next() { nexts++; return { done: i > 4, value: i++ }; }, return() { closed++; return {}; } }; } };
function fa(a) { var v = 0; for (var e of a) v += e; return v; }
function fb(a, n) { var v = 0; for (var e of a) { if (e === n) break; v += e; } return v; }
function fc(a, n) { for (var e of a) { if (e === n) return e * 2; } return -1; }
function fd(a, n) { var v = 0; for (var e of a) { if (e === n) continue; v += e; } return v; }
function fe(a, n) { try { for (var e of a) { if (e === n) throw new Error("x" + e); } } catch (m) { return m.message; } return "none"; }
function ff(a) { var v = 0; for (var e of a) { for (var y of [1, 2]) v += e * y; } return v; }
for (var src of [[0, 1, 2, 3, 4], it, new Set([0, 1, 2, 3, 4]), new Uint8Array([0, 1, 2, 3, 4])]) {
  closed = 0; nexts = 0;
  console.log(hot(() => fa(src)), hot(() => fb(src, 2)), hot(() => fc(src, 3)), hot(() => fc(src, 9)), hot(() => fd(src, 1)), hot(() => fe(src, 2)), hot(() => ff(src)), closed, nexts);
}
function fg(a) { var s = 0; for (var v of a) { s += v.x.y; } return s; }
var good = [{ x: { y: 1 } }, { x: { y: 2 } }];
console.log(hot(() => fg(good)));
try { fg([{ x: { y: 1 } }, {}]); } catch (m) { console.log(m.name); }
closed = 0; var it2 = { [Symbol.iterator]() { return { next() { return { done: false, value: {} }; }, return() { closed++; return {}; } }; } };
try { fg(it2); } catch (m) { console.log(m.name, closed); }
function fh(a) { var v = 0; for (var e of a) { v = v + e; } return v; }
hot(() => fh([1, 2])); console.log(fh(["a", "b"]), fh([1.5, 2]), fh([{ valueOf() { return 7; } }]));
// Mutated while iterating, and destructuring in the head.
function fi(n) { var a = [1, 2, 3], c2 = 0; for (var e of a) { if (a.length < n) a.push(e); c2++; } return c2; }
function fj(a) { var v = 0; for (var [k2, w2] of a) v += k2 * w2; return v; }
console.log(hot(() => fi(6)), hot(() => fj([[1, 2], [3, 4]])), hot(() => fj(new Map([[1, 2]]))));
// Compiled through a deep stack, in a constructor, a method, a closure.
function rec(n) { try { if (n === 0) return 0; return 1 + rec(n - 1); } finally { fin++; } }
console.log(hot((i) => rec(i % 40)), rec(1000));
class K { constructor(v) { try { this.v = v; return; } finally { this.w = 1; } } get g() { try { return this.v; } finally { fin++; } } m() { for (var e of [this.v]) return e; } }
console.log(hot((i) => { var k2 = new K(i); return k2.v + k2.w + k2.g + k2.m(); }));
var cl = (function () { var n = 0; return function () { try { return ++n; } finally { n++; } }; })();
console.log(hot(cl));
console.log(typeof fin, fin > 0);
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
