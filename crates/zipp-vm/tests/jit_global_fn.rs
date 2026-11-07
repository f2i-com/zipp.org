//! The bare global functions in compiled code: each one's values, coercion order
//! and throws, a replaced global, and a typed array the replacement moves. Node's
//! output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"0|0|0|0|false|true|false
-0|0|0|0|false|true|false
1.5|1.5|1|1.5|false|true|true
NaN|NaN|NaN|NaN|true|false|false
Infinity|Infinity|NaN|Infinity|false|false|true
0||NaN|NaN|false|true|false
12| 12 |12|12|false|true|true
NaN|12px|12|12|true|false|true
16|0x10|16|0|false|true|true
1000|1e3|1|1000|false|true|true
NaN|abc|NaN|NaN|true|false|true
0|null|NaN|NaN|false|true|false
NaN|undefined|NaN|NaN|true|false|false
1|true|NaN|NaN|false|true|true
0|false|NaN|NaN|false|true|false
0||NaN|NaN|false|true|true
5|5|5|5|false|true|true
NaN|1,2|1|1|true|false|true
NaN|[object Object]|NaN|NaN|true|false|true
10|10|10|10|TypeError|TypeError|true
1295 3 NaN NaN 10 -42
0  false
Symbol(x)
TypeError
TypeError
Symbol(y) true
7 9 9 9 false true true vsssvv
RangeError
EvalError
EvalError
EvalError
RangeError
RangeError
13018690.000000002:50
37492500
15000
303000
15000
6000
replaced
function
true
6000
502320
NaN
8002000
4.5
1,2,3 1,NaN,NaN a"####;

const PROGRAM: &str = r####"// The bare global functions in hot compiled code.
const HOT = 3000;
function hot(f) { var r; for (var i = 0; i < HOT; i++) r = f(i); return r; }
function num(x) { return Number(x); }
function str(x) { return String(x); }
function pi(x) { return parseInt(x); }
function pi2(x, r) { return parseInt(x, r); }
function pf(x) { return parseFloat(x); }
function nan(x) { return isNaN(x); }
function fin(x) { return isFinite(x); }
function boo(x) { return Boolean(x); }
function sym(x) { return typeof Symbol(x); }
hot((i) => [num(i), str(i), pi("" + i), pi2("7f", 16), pf(i + ".5"), nan(i), fin(i), boo(i), sym("a")]);
var vs = [0, -0, 1.5, NaN, Infinity, "", " 12 ", "12px", "0x10", "1e3", "abc", null, undefined, true, false, [], [5], [1, 2], {}, 10n];
for (var v of vs) {
  var row = [];
  for (var f of [num, str, pi, pf, nan, fin, boo]) {
    try { var r = f(v); row.push(Object.is(r, -0) ? "-0" : String(r)); } catch (e) { row.push(e.name); }
  }
  console.log(row.join("|"));
}
console.log(pi2("zz", 36), pi2("11", 2), pi2("9", 8), pi2("10", 37), pi2("10", 0), pi2(" -42", 10));
console.log(Number(), String(), Boolean());
try { String(Symbol("x")); console.log(String(Symbol("x"))); } catch (e) { console.log(e.name); }
try { Number(Symbol("x")); } catch (e) { console.log(e.name); }
try { num(Symbol("x")); } catch (e) { console.log(e.name); }
console.log(str(Symbol("y")), boo(Symbol("y")));
// Coercion is user code: order, a throw, and how many times.
var log = [];
var o = { valueOf() { log.push("v"); return 7; }, toString() { log.push("s"); return "9"; } };
console.log(num(o), str(o), pi(o), pf(o), nan(o), fin(o), boo(o), log.join(""));
var th = { valueOf() { throw new RangeError("vo"); }, toString() { throw new EvalError("ts"); } };
for (var f of [num, str, pi, pf, nan, fin]) { try { f(th); console.log("no throw"); } catch (e) { console.log(e.name); } }
// In a loop, compiled, with a throw from the argument every so often.
function lp(n) { var s = 0, b = 0; for (var i = 0; i < n; i++) { try { s += Number(i % 100 === 99 ? th : i); } catch (e) { b++; } s += parseInt("12" + (i % 10)) + parseFloat("1." + (i % 10)) + String(i).length + (isNaN(i) ? 1 : 0); } return s + ":" + b; }
console.log(lp(5000));
function lq(n) { var s = 0; for (var i = 0; i < n; i++) s += parseInt(String(i), 10) + Number(String(i * 2)); return s; }
console.log(lq(5000));
// Replaced globals: the plain call, a hot one, with arguments and `this`.
var realParseInt = parseInt;
function rp(n) { var s = 0; for (var i = 0; i < n; i++) s += parseInt("5"); return s; }
console.log(rp(3000));
parseInt = function (x) { "use strict"; return (this === undefined ? 100 : 200) + arguments.length; };
console.log(rp(3000));
parseInt = realParseInt;
console.log(rp(3000));
var realNumber = Number;
function rn(n) { var s = 0; for (var i = 0; i < n; i++) s += Number("2"); return s; }
console.log(rn(3000));
globalThis.Number = () => { throw new TypeError("replaced"); };
try { rn(10); } catch (e) { console.log(e.message); }
globalThis.Number = function* () {};
console.log(typeof Number("2").next);
globalThis.Number = async () => 1;
console.log(Number("2") instanceof Promise);
globalThis.Number = realNumber;
console.log(rn(3000));
// Strings pinned around a loop, and a typed array a replaced global detaches.
var ta = new Uint8Array(new ArrayBuffer(16));
var realIsNaN = isNaN;
function pin(n) { var s = 0; for (var i = 0; i < n; i++) { ta[i % 16] = i; s += ta[i % 16]; if (isNaN(i)) s = -1; } return s; }
console.log(pin(4000));
globalThis.isNaN = function () { ta = new Uint8Array(4); return false; };
console.log(pin(4000));
globalThis.isNaN = realIsNaN;
// A closure, a method and a generator calling them.
function* gn(n) { for (var i = 0; i < n; i++) yield Number(String(i)) + parseInt("1"); }
var sum = 0; for (var v of gn(4000)) sum += v; console.log(sum);
class C { m(x) { return parseFloat(x) + Number(this.k); } constructor() { this.k = "3"; } }
var c = new C(); console.log(hot(() => c.m("1.5")));
console.log([1, 2, 3].map(Number).join(), ["1", "2", "3"].map(parseInt).join(), ["a", "", 0].filter(Boolean).join());
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
