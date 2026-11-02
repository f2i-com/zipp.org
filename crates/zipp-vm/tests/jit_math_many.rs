//! `Math.min`, `max` and `hypot` past two arguments, in compiled code, against Node.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"sum3 1921875
hyp 1948179.8076530558
mm 6
arity 3,4,5,6,7,8,9,1,3,-Infinity,Infinity,0
zeros true,true,true,true,NaN,NaN,NaN,NaN,Infinity,Infinity,-Infinity,Infinity,0
scale 1.4142135623730952e+300,5.0990195135927843e-200
before 2
string 7
null 2
undefined NaN
true 2
object 3
1,2,3
order 4
5,4,6
TypeError
TypeError
live 100897
restored 1
alias 2
destructured 5.830951894845301"####;

const PROGRAM: &str = r####"// Math.min/max/hypot of three or more arguments, in a hot loop, in each compiled form.
function hot(f, n) { let r = 0; for (let i = 0; i < n; i++) r = f(i); return r; }
function show(label, v) { console.log(label, Object.is(v, -0) ? "-0" : String(v)); }

// A loop that holds one such call compiles.
function sum3(n) {
  let s = 0;
  for (let i = 0; i < n; i++) s += Math.max(i & 7, 3, (i >> 2) & 15) + Math.min(i & 3, 2.5, 9);
  return s;
}
show("sum3", sum3(200000));

function hyp(n) {
  let s = 0;
  for (let i = 0; i < n; i++) s += Math.hypot(i & 15, 3, 4.5);
  return s;
}
show("hyp", hyp(200000));

// A body called often enough to compile as a whole.
function mm(a, b, c, d) { return Math.max(a, b, c, d) - Math.min(a, b, c, d); }
show("mm", hot(i => mm(i & 3, 1.5, -2, i % 5), 100000));

// Every arity up to the limit and one past it.
function arity(i) {
  const a = i & 1;
  return [Math.max(a, 2, 3), Math.max(a, 2, 3, 4), Math.max(a, 2, 3, 4, 5), Math.max(a, 2, 3, 4, 5, 6),
    Math.max(a, 2, 3, 4, 5, 6, 7), Math.max(a, 2, 3, 4, 5, 6, 7, 8), Math.max(a, 2, 3, 4, 5, 6, 7, 8, 9),
    Math.min(a, 2, 3, 4, 5, 6, 7, 8, 9, 10), Math.hypot(a, 1, 1, 1, 1, 1, 1, 1, 1), Math.max(), Math.min(), Math.hypot()].join();
}
show("arity", hot(arity, 50000));

// Signed zero, NaN and the infinities, whatever the order.
function zeros(i) {
  const z = i < 0 ? 1 : 0, m = -z;
  const nan = i < 0 ? 1 : NaN, inf = i < 0 ? 1 : Infinity;
  return [Object.is(Math.max(-0, m, -0), -0), Object.is(Math.max(-0, z, -0), 0), Object.is(Math.min(0, m, 0), -0),
    Object.is(Math.min(0, z, 0), 0), Math.max(1, nan, 3), Math.min(nan, 2, 3), Math.max(1, 2, nan), Math.hypot(1, nan, 3),
    Math.hypot(nan, inf, 1), Math.hypot(-inf, 0, nan), Math.min(inf, 5, -inf), Math.max(inf, 5, -inf), Math.hypot(0, -0, m)].join();
}
show("zeros", hot(zeros, 50000));

// Large and small magnitudes keep the scaled sum.
function scale(i) { const k = i & 1; return [Math.hypot(1e300, 1e300, k), Math.hypot(3e-200, 4e-200, k * 1e-200)].join(); }
show("scale", hot(scale, 50000));

// Non-numbers leave compiled code, and coerce left to right exactly once each.
const log = [];
const obj = n => ({ valueOf() { log.push(n); return n; } });
function coerce(i, x) { return Math.max(i & 1, x, 2); }
let last = 0;
for (let i = 0; i < 50000; i++) last = coerce(i, 1);
show("before", last);
show("string", coerce(1, "7"));
show("null", coerce(0, null));
show("undefined", coerce(0, undefined));
show("true", coerce(0, true));
show("object", Math.max(obj(1), obj(2), obj(3)));
console.log(log.join());
log.length = 0;
function order(a, b, c) { return Math.min(a, b, c); }
for (let i = 0; i < 50000; i++) order(1, 2, 3);
show("order", order(obj(5), obj(4), obj(6)));
console.log(log.join());
try { order(1, Symbol(), obj(1)); } catch (e) { console.log(e.name); }
try { order(1n, 2, 3); } catch (e) { console.log(e.name); }

// A replaced function is honoured in a loop already compiled.
function live(n) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    if (i === n - 3) Math.max = (...a) => a.length * 100;
    s += Math.max(i & 1, 1, 0);
  }
  return s;
}
const realMax = Math.max;
show("live", live(100000));
Math.max = realMax;
show("restored", hot(i => Math.max(i & 1, 1, 0), 100000));

// `this` free, and through an alias.
const m = Math.min;
show("alias", hot(i => m(i & 3, 2, 5), 100000));
const { hypot } = Math;
show("destructured", hot(i => hypot(i & 3, 3, 4), 100000));
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
