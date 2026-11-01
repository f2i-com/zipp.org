//! A `try` in compiled code pushes its handler only on the way out of its body.
//! Every way out of one, hot enough to compile: a throw here and from a callee,
//! a native, a getter and a coercion, a bail on each operand type, rethrows,
//! `finally`, loops, deep recursion and overflow. Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"3999
b3999
1
SyntaxError
1
SyntaxError
51 2 0 5 -2147483648
1.5 3 0 Infinity
fine TypeError TypeError
20
4000
re3999
1225
380
1225
129
4000
3999
3
4000
12
bottom399
1 RangeError
TypeError
4000
0,1,2,c2,3
3999 none
2 3
async 5"####;

const PROGRAM: &str = r####"// Every way out of a `try` in a hot function.
const HOT = 4000;
var log = [];
function hot(f) { var r; for (var i = 0; i < HOT; i++) r = f(i); return r; }
// Plain, and a throw at the end of a hot run.
function a(i) { var s = 0; try { s = (s + i) | 0; } catch (e) { s = -1; } return s; }
console.log(hot(a));
function b(i) { try { if (i === HOT - 1) throw new RangeError("b" + i); return_(); } catch (e) { return e.message; } return "none"; }
function return_() {}
console.log(hot(b));
// A throw from a callee: compiled, native, a getter, a coercion.
function t() { throw new Error("from t"); }
function c(i) { var r = 0; try { if (i % 500 === 0) t(); r = 1; } catch (e) { r = e.message; } return r; }
console.log(hot(c));
function d(i) { try { return_(i); JSON.parse(i === HOT - 1 ? "{" : "1"); } catch (e) { return e.name; } return "ok"; }
console.log(hot(d));
var o = { get g() { if (o.n++ % 700 === 699) throw new TypeError("g"); return 1; }, n: 0 };
function e1(i) { var r = 0; try { r = o.g; } catch (e) { r = e.message; } return r; }
console.log(hot(e1));
var vo = { valueOf() { throw new SyntaxError("vo"); } };
function f(x) { var r; try { r = 1 + x; } catch (e) { r = e.name; } return r; }
console.log(hot((i) => f(i % 1000 === 999 ? vo : i)));
// The body bails: a string, a double, undefined, an object with valueOf.
function g(x) { var s = 0; try { s = (x + 1) | 0; } catch (e) { s = -1; } return s; }
hot(g);
console.log(g("5"), g(1.5), g(undefined), g({ valueOf() { return 4; } }), g(2147483647));
function g2(x) { var s = 0; try { s = x * 2; s = s - x; } catch (e) { s = -1; } return s; }
hot(g2);
console.log(g2(1.5), g2("3"), g2(null), g2(1e308));
try { g2(1n); } catch (e) { console.log(e.name, "escaped"); }
function g3(x) { try { return_(); x.y.z; } catch (e) { return e.name; } return "fine"; }
console.log(hot((i) => g3({ y: {} })), g3({}), g3(null));
// A throw leaves the body after a partial write, and the catch sees it.
function h(i) { var s = 1; try { s = 2; if (i >= 0) throw 0; s = 3; } catch (e) { s = s * 10; } return s; }
console.log(hot(h));
// The catch rethrows; an outer try catches, in this frame and the caller's.
function j(i) { try { try { if (i % 3 === 0) throw i; return_(); } catch (e) { throw e + 1; } } catch (e) { return e; } return -1; }
console.log(hot(j));
function k(i) { try { t(); } catch (e) { throw new Error("re" + i); } }
function l(i) { try { k(i); } catch (e) { return e.message; } }
console.log(hot(l));
// In loops, with break and continue, and a loop in the body.
function m(n) { var s = 0; for (var i = 0; i < n; i++) { try { s = (s + i) | 0; } catch (e) { s = 0; } } return s; }
console.log(hot(() => m(50)));
function p(n) { var s = 0; for (var i = 0; i < n; i++) { try { if (i === 40) break; if (i % 2) continue; s += i; } catch (e) {} } return s; }
console.log(hot(() => p(50)));
function q(n) { var s = 0; try { for (var i = 0; i < n; i++) { s += i; if (s === -1) throw 1; } } catch (e) { s = -2; } return s; }
console.log(hot(() => q(50)));
function r(n) { var s = 0; for (var i = 0; i < n; i++) { try { if (i === n - 1) t(); s += 1; } catch (e) { s += 100; } } return s; }
console.log(hot(() => r(30)));
// Return, finally, nested.
function u(i) { try { return i + 1; } catch (e) { return -1; } }
console.log(hot(u));
function v(i) { try { return_(); } finally { log.length = 0; } return i; }
console.log(hot(v));
function w(i) { try { try { return_(); } catch (e) { return 1; } if (i < 0) throw 1; } catch (e) { return 2; } return 3; }
console.log(hot(w));
function x(i) { try { return_(); } catch (e) { return 1; } finally { i = i + 1; } return i; }
console.log(hot(x));
function y(i) { var s = 0; try { s = 1; } catch { s = 2; } try { s = s + 1; if (i === HOT - 1) throw 0; } catch { s = s + 10; } return s; }
console.log(hot(y));
// Deep: a throw from far below, into a lazy try at the bottom of the stack.
function deep(n) { if (n === 0) throw new Error("bottom"); return deep(n - 1) + 1; }
function z(i) { try { return deep(i % 400); } catch (e) { return e.message + (i % 400); } }
console.log(hot(z));
function over(n) { return over(n + 1) + 1; }
function aa() { try { over(0); } catch (e) { return e.name; } }
console.log(hot((i) => (i % 1000 === 0 ? aa() : 1)), aa());
// Stack traces and the line of a throw.
function bb(i) { try { if (i === HOT - 1) { var n = null; n.x; } } catch (e) { return e.name; } return "s"; }
console.log(hot(bb));
// A closure in the body, a generator around it, async.
function cc(i) { var f = null; try { f = () => i + 1; if (i % 1000 === 7) t(); } catch (e) { return f ? f() : -1; } return f(); }
console.log(hot(cc));
function* gen(n) { for (var i = 0; i < n; i++) { try { yield i; if (i === 2) throw 1; } catch (e) { yield "c" + i; } } }
console.log([...gen(4)].join());
async function dd(i) { try { await null; if (i) throw 5; } catch (e) { return e; } return 0; }
dd(1).then((v) => console.log("async", v));
// `this`, arguments, a class constructor.
class K { constructor(v) { try { this.v = v.x; } catch (e) { this.v = "none"; } } }
console.log(hot((i) => new K(i % 100 === 0 ? null : { x: i }).v), new K(null).v);
function ee() { try { return_(); if (arguments.length === 3) throw arguments[2]; } catch (e) { return e; } return arguments.length; }
console.log(hot((i) => ee(1, 2)), ee(1, 2, 3));
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
