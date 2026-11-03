"use strict";
// Tail calls through every callee kind, each repeated so the call cache fills,
// and many functions so a lazy build defers these bodies too.
const out = [];
function log(k, v) { out.push(k + "=" + String(v)); }

function leaf(a, b) { return a + b; }
function t_leaf(a, b) { return leaf(a, b); }

function rec(n, acc) { if (n === 0) return acc; return rec(n - 1, acc + n); }

function fewer(a, b, c) { return [a, b, c].join("/"); }
function t_fewer(x) { return fewer(x); }
function more(a) { return a * 2; }
function t_more(x) { return more(x, x + 1, x + 2); }

// Arguments overlap: the callee's parameters are the caller's registers moved.
function swap(a, b) { return a + "," + b; }
function t_swap(a, b) { return swap(b, a); }
function shift(a, b, c, d) { return a + b * 10 + c * 100 + (d === undefined ? 1000 : d); }
function t_shift(a, b, c, d) { return shift(b, c, d); }

// A larger window than the caller's: locals must read undefined.
function big(a) { let p, q, r, s; const seen = [p, q, r, s, a]; let t; t = seen.length; return t + ":" + String(seen[0]); }
function t_big(a) { return big(a); }
// A smaller one: the frame shrinks.
function t_small(a) { let p = 1, q = 2, r = 3, s = 4, u = 5; return more(a + p + q + r + s + u); }

// Sloppy callee: `this` is the global object. Built by the Function constructor.
const sloppyThis = new Function("return this === globalThis;");
function t_sloppy() { return sloppyThis(); }
function strictThis() { return this === undefined; }
function t_strict() { return strictThis(); }

// Arrow callee: its captured `this`.
const obj = {
  v: 41,
  mk() { const f = (d) => this.v + d; function viaArrow(d) { return f(d); } return viaArrow; },
};
const viaArrow = obj.mk();

// Rest and arguments callees take the full path.
function restCallee(a, ...r) { return a + ":" + r.length; }
function t_rest(a, b, c) { return restCallee(a, b, c); }
function argsCallee() { return arguments.length + ":" + arguments[1]; }
function t_args(a, b) { return argsCallee(a, b); }
function argsLen() { return arguments.length; }
function t_argslen(a, b, c) { return argsLen(a, b, c); }

// Not plain: generator, async, native, bound, class.
function* gen(a) { yield a; yield a + 1; }
function t_gen(a) { return gen(a); }
async function asy(a) { return a + 1; }
function t_async(a) { return asy(a); }
const parse = parseInt;
function t_native(s) { return parse(s, 10); }
const bound = leaf.bind(null, 7);
function t_bound(a) { return bound(a); }
class K {}
const callK = K;
function t_class() { return callK(); }

// Constructing frame: the instance is the result unless the tail value is an object.
function Ctor(a) { this.a = a; return leaf(a, 1); }
function CtorObj(a) { this.a = a; return mkObj(a); }
function mkObj(a) { return { made: a }; }

// `new.target` and `arguments` of the frame that was reused.
function nt() { return new.target === undefined; }
function t_nt() { return nt(); }
function NTC() { this.r = nt(); return nt(); }

// Inside try and finally: not a tail call. The finally runs after the callee.
function inTry(a) { try { return thrower(a); } catch (e) { return "caught " + e; } }
function thrower(a) { throw a; }
function inFinally(log2) { try { return leafLog(log2); } finally { log2.push("f"); } }
function leafLog(l) { l.push("c"); return l.length; }

// Mutual recursion, through ternary, logical and sequence tails.
function even(n) { return n === 0 ? true : odd(n - 1); }
function odd(n) { return n === 0 ? false : even(n - 1); }
function logical(a) { return a && more(a); }
function nullish(a) { return a ?? more(4); }
function seq(a) { return (a++, more(a)); }

// A callee that changes at the site: cache ways, then a miss.
function one() { return 1; } function two() { return 2; } function three() { return 3; }
function four() { return 4; } function five() { return 5; } function six() { return 6; }
function pick(f) { return f(); }

// A closure's cells survive a tail call.
function counter() { let c = 0; return function inc() { c++; return get(); function get() { return c; } }; }

// A throw out of a tail callee keeps the caller's handler.
function t_throw() { return thrower("x"); }

// Deep enough to show stack use; Node, without tail calls, manages this too.
for (let i = 0; i < 3; i++) {
  log("leaf", t_leaf(i, 2));
  log("rec", rec(1000, 0));
  log("fewer", t_fewer(i));
  log("more", t_more(i));
  log("swap", t_swap(i, "b"));
  log("shift", t_shift(1, 2, 3, 4));
  log("big", t_big(i));
  log("small", t_small(i));
  log("sloppy", t_sloppy());
  log("strict", t_strict());
  log("arrow", viaArrow(i));
  log("rest", t_rest(i, 2, 3));
  log("args", t_args(i, 2));
  log("argslen", t_argslen(1, 2, 3));
  log("gen", [...t_gen(i)]);
  log("native", t_native("12px"));
  log("bound", t_bound(i));
  try { t_class(); } catch (e) { log("class", e.constructor.name); }
  log("ctor", new Ctor(i).a);
  log("ctorobj", new CtorObj(i).made);
  log("nt", t_nt());
  log("ntc", new NTC().r);
  log("intry", inTry(i));
  const l = [];
  log("infin", inFinally(l) + ":" + l.join(""));
  log("even", even(101));
  log("logical", logical(i));
  log("nullish", nullish(i === 1 ? null : i));
  log("seq", seq(i));
  log("pick", [one, two, three, four, five, six, one, two].map(pick).join(""));
  const inc = counter();
  log("cnt", inc() + inc() + inc());
  try { t_throw(); } catch (e) { log("throw", e); }
}
// Hot enough to compile, for the JIT lanes.
let sum = 0;
for (let i = 0; i < 3000; i++) sum += t_leaf(i, 1) + t_more(i) + even(3) + rec(3, i);
log("hot", sum);
t_async(1).then((v) => { log("async", v); console.log(out.join("\n")); });
