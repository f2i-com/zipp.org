//! Operations a whole-function compile (Tier C) used to refuse, run hot.
//!
//! A body holding any of `Number(x)`/`parseInt(..)` (`GlobalFn`), an array
//! or object spread, a spread call, a regular expression literal, array
//! destructuring, or a private name (`this.#x`, `this.#x = v`, a private
//! field initializer) was never compiled whole: the compile was refused, or
//! the body bailed to the interpreter at its first private read on every
//! call. Each now runs through a helper that is the interpreter's own code
//! with the call protocol, and a private name resolves against the running
//! code's callee (a frame-free body's activation, not the top frame).
//!
//! Each program runs every shape thousands of times so the bodies compile,
//! then checks what can be seen: coercions that throw, a replaced global
//! `Number`, throwing iterators and getters, invalid patterns, `RegExp(re)`
//! returning `re`; two evaluations of one class (the same code, two
//! brands), a read of another class's private field, private accessors and
//! methods, statics, subclasses, a Proxy, and a frozen instance.
//!
//! Two more cover the routes those bodies call through: a method or a
//! `this.f(...)` call whose callee is an arrow (run over a fresh window with
//! its captured `this`) or a body that needs a frame (a `try`, a `for...of`,
//! a destructuring: its frame-backed native entry), with sloppy and strict
//! receivers, throws, `arguments`, and deep recursion; and an element read
//! hot across holes, a patched `Array.prototype`, mapped `arguments`, index
//! getters, frozen and re-prototyped arrays. The expected lines are Node 24's.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state
        .take_output()
        .iter()
        .flat_map(|chunk| chunk.lines().map(str::to_string).collect::<Vec<_>>())
        .collect()
}

const OPS: &str = r####""use strict";
const out = [];
const log = (s) => out.push(String(s));

// GlobalFn: Number/parseInt/parseFloat/isNaN/isFinite/Boolean with coercion,
// throwing valueOf, Symbol, BigInt.
function gf(x, r) {
  return [Number(x), parseInt(x, r), parseFloat(x), isNaN(x), isFinite(x), Boolean(x)].join("|");
}
let acc = 0;
for (let i = 0; i < 20000; i++) acc += Number(gf(String(i % 97), 10).split("|")[0]);
log(acc);
log(gf("0x1F", 16));
log(gf("  12.5e1xyz", 36));
log(gf({ valueOf() { return 7; }, toString() { return "8"; } }, undefined));
for (const bad of [Symbol("s"), { valueOf() { throw new Error("vo"); } }, 10n]) {
  try { log(gf(bad, 10)); } catch (e) { log(e.constructor.name + ":" + e.message); }
}
log(gf(null, 2) + gf(undefined, 2) + gf(true, 2) + gf([], 2) + gf([5], 2));

// A replaced global after the body is hot: the callee is no longer the
// intrinsic.
function usesNumber(s) { return Number(s) + 1; }
let n = 0;
for (let i = 0; i < 20000; i++) n += usesNumber("3");
log(n);
const savedNumber = globalThis.Number;
globalThis.Number = function (s) { return 100; };
log(usesNumber("3"));
globalThis.Number = savedNumber;
log(usesNumber("3"));

// ArrayAppend spread with iterables: arrays, strings, Sets, generators,
// a throwing iterator, holes.
function spreadAll(a, b) { return [0, ...a, 9, ...b]; }
let tot = 0;
for (let i = 0; i < 20000; i++) tot += spreadAll([1, 2], [3]).length;
log(tot);
log(JSON.stringify(spreadAll("ab", new Set([4, 4, 5]))));
log(JSON.stringify(spreadAll((function* () { yield 1; yield 2; })(), [, 7])));
try {
  spreadAll({ [Symbol.iterator]() { return { next() { throw new RangeError("it"); } }; } }, []);
} catch (e) { log(e.constructor.name + ":" + e.message); }
try { spreadAll(5, []); } catch (e) { log(e.constructor.name); }

// CallSpread: spread call, a throwing callee, a non-callable, `this`.
function sum3(a, b, c) { return a + b + c; }
function callSpread(f, xs) { return f(...xs); }
let cs = 0;
for (let i = 0; i < 20000; i++) cs += callSpread(sum3, [i, 1, 2]);
log(cs);
log(callSpread(Math.max, [3, 9, 2]));
log(callSpread(function () { return this; }, []));
try { callSpread(() => { throw new TypeError("cs"); }, [1]); } catch (e) { log(e.message); }
try { callSpread(42, [1]); } catch (e) { log(e.constructor.name); }

// NewRegExp: literals evaluated in a loop (a fresh object each time),
// lastIndex independence, `new RegExp` with dynamic and bad patterns,
// `RegExp(re)` returning re.
function lit(s) { const r = /a(b)?/g; r.lastIndex = 0; return r.test(s) + ":" + r.lastIndex; }
let lt = "";
for (let i = 0; i < 20000; i++) lt = lit("xxab");
log(lt);
function dyn(p, f) { return new RegExp(p, f); }
let dn = 0;
for (let i = 0; i < 5000; i++) dn += dyn("x" + (i % 3), "g").source.length;
log(dn);
try { dyn("(", ""); } catch (e) { log(e.constructor.name); }
try { dyn("a", "zz"); } catch (e) { log(e.constructor.name); }
const re = /q/y;
function call(p, f) { return RegExp(p, f); }
for (let i = 0; i < 5000; i++) call(re, undefined);
log(call(re, undefined) === re);
log(call(re, "g") === re);
log(String(dyn(re, "i")));
log(String(dyn({ toString() { return "t+"; } }, "")));

// ObjectSpread / InitDataProp: getters, throwing getters, symbols, order,
// null/undefined sources, overwriting.
function spreadObj(e, v) { return { ...e, left: v, kind: "k" }; }
let so = 0;
for (let i = 0; i < 20000; i++) so += spreadObj({ kind: "x", a: i }, 1).a;
log(so);
log(JSON.stringify(spreadObj({ get g() { return 5; }, left: 0, z: 1 }, 2)));
log(JSON.stringify(spreadObj(null, 3)) + JSON.stringify(spreadObj(undefined, 4)) + JSON.stringify(spreadObj("ab", 5)));
try { spreadObj({ get bad() { throw new Error("getter"); } }, 0); } catch (e) { log(e.message); }
const sym = Symbol("k");
log(spreadObj({ [sym]: 1 }, 0)[sym]);
log(Object.keys(spreadObj({ b: 1, a: 2, 1: 3 }, 0)).join(","));

console.log(out.join("\n"));
"####;

const CALLS: &str = r####"const out = [];
const log = (s) => out.push(String(s));
const N = 30000;
const err = (f) => { try { return "ok:" + f(); } catch (e) { return e.constructor.name + ":" + e.message; } };

// Arrows stored on objects and called as methods: `this` is the captured one.
function Maker() {
  this.f = 111;
  this.o = { f: 3, m: () => this.f, n() { return this.f; } };
}
const mk = new Maker();
let a1 = 0;
for (let i = 0; i < N; i++) a1 += mk.o.m() + mk.o.n();
log(a1);

// A comparator arrow in a field, called as `this.cmp(x, y)` from a method.
class Q {
  constructor(cmp) { this.cmp = cmp; this.items = []; }
  push(v) { this.items.push(v); let i = this.items.length - 1; while (i > 0 && this.cmp(this.items[i], this.items[i - 1]) < 0) { const t = this.items[i]; this.items[i] = this.items[i - 1]; this.items[i - 1] = t; i--; } }
}
const self = { k: 5 };
const q1 = new Q((a, b) => a - b);
const q2 = new Q(function (a, b) { return (b - a) * (this === undefined ? 1 : 2); });
for (let i = 0; i < 300; i++) { q1.push((i * 37) % 101); q2.push((i * 37) % 101); }
log(q1.items.slice(0, 5).join(",") + " " + q2.items.slice(0, 5).join(","));

// Sloppy functions see a boxed primitive / the global object; strict ones the
// value as it is, null included.
function sloppyThis() { return typeof this + (this === globalThis ? ":g" : ""); }
function strictThis() { "use strict"; return this === null ? "null" : typeof this; }
const holder = { s: sloppyThis, t: strictThis };
let st = "";
for (let i = 0; i < N; i++) st = holder.s() + holder.t() + sloppyThis.call(7) + strictThis.call(7) + strictThis.call(null) + sloppyThis.call(null);
log(st);
String.prototype.kind = function () { return typeof this; };
Number.prototype.kindS = function () { "use strict"; return typeof this; };
let ks = "";
for (let i = 0; i < N; i++) ks = "abc".kind() + (5).kindS();
log(ks);

// Callees whose bodies need a frame: try/finally, catch, for-of, destructuring.
function withFinally(x) { try { if (x < 0) throw new RangeError("neg"); return x * 2; } finally { withFinally.count++; } }
withFinally.count = 0;
function retFromFinally(x) { try { return x; } finally { if (x === 7) return -1; } }
function forOfSum(xs) { let s = 0; for (const [a, b] of xs) s += a * b; return s; }
const obj = { wf: withFinally, rf: retFromFinally, fs: forOfSum, sw(a) { [a[0], a[1]] = [a[1], a[0]]; return a; } };
let fsum = 0;
const pairs = [[1, 2], [3, 4]];
for (let i = 0; i < N; i++) fsum += obj.wf(i) + obj.rf(i % 10) + obj.fs(pairs) + obj.sw([i, 1])[0];
log(fsum + " " + withFinally.count + " " + err(() => obj.wf(-1)) + " " + withFinally.count);
// A throw out of a frame-backed callee inside a caller's own try.
function caller(o, n) { let c = 0; for (let i = 0; i < n; i++) { try { c += o.wf(i % 5 === 4 ? -1 : i); } catch (e) { c -= 1000; } } return c; }
for (let i = 0; i < 50; i++) caller(obj, 200);
log(caller(obj, 1000));

// `arguments`, rest, `new.target`, default parameters through method calls.
const ops = {
  args() { return arguments.length + ":" + Array.prototype.join.call(arguments, "-"); },
  rest(...xs) { return xs.length; },
  nt() { return new.target === undefined; },
  def(a, b = a + 1) { return a + b; },
};
let r = "";
for (let i = 0; i < N; i++) r = ops.args(1, 2, 3) + " " + ops.rest(1, 2) + " " + ops.nt() + " " + ops.def(i);
log(r);

// Deep recursion through methods whose bodies need frames: the RangeError.
const deep = { d(n) { try { return n === 0 ? 0 : 1 + this.d(n - 1); } finally { } } };
for (let i = 0; i < 20; i++) deep.d(100);
log(deep.d(3000) + " " + err(() => deep.d(1e6)).split(":")[0]);

// A method replaced after it is hot; a getter-defined method; a Proxy receiver.
const box = { v: 1, get() { return this.v; } };
let gsum = 0;
for (let i = 0; i < N; i++) gsum += box.get();
box.get = () => 100;
gsum += box.get();
Object.defineProperty(box, "acc", { get() { return () => this.v + 1; } });
gsum += box.acc();
const px = new Proxy({ v: 9, get() { return this.v; } }, {});
gsum += px.get();
log(gsum);

console.log(out.join("\n"));
"####;

const INDEX: &str = r####"const out = [];
const log = (s) => out.push(String(s));
const N = 20000;

// One hot reader, many receivers.
function sum3(a) { let s = ""; for (let i = 0; i < 3; i++) s += String(a[i]) + ","; return s; }
const plain = [1, 2, 3];
for (let i = 0; i < N; i++) sum3(plain);
log(sum3(plain));

// A hole read while Array.prototype has that index.
const holey = [1, , 3];
log(sum3(holey));
Array.prototype[1] = "proto";
log(sum3(holey) + " " + sum3([7, , 9]));
delete Array.prototype[1];
log(sum3(holey));

// A mapped arguments object after a parameter is reassigned.
function mapped(a, b, c) { b = "re"; return sum3(arguments); }
function unmapped(a, b, c) { "use strict"; b = "re"; return sum3(arguments); }
let m = "";
for (let i = 0; i < 2000; i++) m = mapped(1, 2, 3) + unmapped(1, 2, 3);
log(m);

// A getter on an index, a non-writable index, a frozen array.
const withGetter = [1, 2, 3];
let calls = 0;
Object.defineProperty(withGetter, 1, { get() { calls++; return "g"; } });
log(sum3(withGetter) + calls);
const frozen = Object.freeze([4, 5, 6]);
log(sum3(frozen));

// A RegExp match array; an array with a replaced prototype and a hole.
const match = /(a)(b)?/.exec("xa");
log(sum3(match));
const reproto = [1, , 3];
Object.setPrototypeOf(reproto, { 1: "custom" });
log(sum3(reproto));
const viaProxyProto = [1, , 3];
Object.setPrototypeOf(viaProxyProto, new Proxy([], { get(t, k, r) { return k === "1" ? "trap" : Reflect.get(t, k, r); } }));
log(sum3(viaProxyProto));

// An array that grows and shrinks under the hot reader.
const grow = [];
let gs = "";
for (let i = 0; i < 2000; i++) { grow.push(i); if (grow.length > 2) grow.length = 2; gs = sum3(grow); }
log(gs);

// Typed arrays and strings through the same site.
log(sum3(new Float64Array([0.5, 1.5, 2.5])) + sum3("xyz") + sum3(new Uint8Array([255, 256, 257])));

console.log(out.join("\n"));
"####;

const PRIVATE: &str = r####""use strict";
const out = [];
const log = (s) => out.push(String(s));
const N = 20000;
const err = (f) => { try { return "ok:" + f(); } catch (e) { return e.constructor.name; } };

// Two evaluations of one class body: the same functions, different brands.
function makeClass(tag) {
  return class {
    #v = 0;
    #log = [];
    static #count = 0;
    constructor(v) { this.#v = v; this.constructor.bumpCount(); }
    static bumpCount() { return ++this.#count; }
    static count() { return this.#count; }
    get v() { return this.#v; }
    set v(x) { this.#v = x; }
    add(n) { this.#v += n; this.#log.push(n); return this.#v; }
    peek(other) { return other.#v; }
    poke(other, x) { other.#v = x; return x; }
    static has(o) { return #v in o; }
    tag() { return tag + this.#log.length; }
  };
}
const A = makeClass("A"), B = makeClass("B");
const a = new A(1), b = new B(2);
let s = 0;
for (let i = 0; i < N; i++) { s += a.add(1); s += b.add(2); s += a.peek(a); s += b.peek(b); }
log(s + " " + a.v + " " + b.v + " " + a.tag() + " " + b.tag());
log(err(() => a.peek(b)) + " " + err(() => b.peek(a)) + " " + err(() => a.poke(b, 5)));
log(A.has(a) + " " + A.has(b) + " " + B.has(b) + " " + A.count() + " " + B.count());
// A hot reader that then sees a foreign instance.
function readAll(xs) { let t = 0; for (const x of xs) t += a.peek(x); return t; }
const as = Array.from({ length: 50 }, (_, i) => new A(i));
for (let i = 0; i < 400; i++) readAll(as);
log(readAll(as) + " " + err(() => readAll([...as, b])) + " " + err(() => readAll([{}])));
log(err(() => a.peek(new Proxy(a, {}))));

// Private accessors and methods.
class Acc {
  #x = 1;
  get #dbl() { return this.#x * 2; }
  set #dbl(v) { this.#x = v / 2; }
  get #ro() { return 7; }
  #m() { return this.#x + 1; }
  run(i) { this.#dbl = i * 2; return this.#dbl + this.#m() + this.#ro; }
  breakRo() { this.#ro = 1; }
  breakM() { this.#m = 1; }
}
const acc = new Acc();
let t = 0;
for (let i = 0; i < N; i++) t += acc.run(i);
log(t + " " + err(() => acc.breakRo()) + " " + err(() => acc.breakM()));

// Subclasses: base private fields on a derived instance; a derived class's
// own field with the same spelling is a different name.
class Base { #id; constructor(id) { this.#id = id; } id() { return this.#id; } setId(v) { this.#id = v; } }
class Derived extends Base { #id = "d"; constructor(id) { super(id); } own() { return this.#id; } }
let u = 0;
for (let i = 0; i < N; i++) { const d = new Derived(i); d.setId(d.id() + 1); u += d.id(); }
const d0 = new Derived(9);
log(u + " " + d0.id() + " " + d0.own());

// A non-extensible `this` cannot gain a private field; an existing one stays
// writable on a frozen object.
class Frozen { #a = 1; bump() { this.#a++; return this.#a; } }
const fz = Object.freeze(new Frozen());
let fv = 0;
for (let i = 0; i < N; i++) fv = fz.bump();
log(fv);
class Stamp extends class { constructor(o) { return o; } } { #tag = "t"; static get(o) { return o.#tag; } }
log(Stamp.get(new Stamp({})));

// Counters stored in private fields read and written in a loop that
// compiles, including through a compound assignment and an update.
class Ctr { #n = 0; #m = 0; tick() { this.#n++; this.#m += this.#n; return this.#m; } }
const ctr = new Ctr();
let last = 0;
for (let i = 0; i < 100000; i++) last = ctr.tick();
log(last);

console.log(out.join("\n"));
"####;

#[test]
fn global_fns_spreads_regexps_and_destructuring_match_node() {
    assert_eq!(
        run(OPS),
        [
            r#"959289"#,
            r#"31|31|0|false|true|true"#,
            r#"NaN|38|125|true|false|true"#,
            r#"7|8|8|false|true|true"#,
            r#"TypeError:Cannot convert a Symbol value to a number"#,
            r#"Error:vo"#,
            r#"TypeError:Cannot convert a BigInt value to a number"#,
            r#"0|NaN|NaN|false|true|falseNaN|NaN|NaN|true|false|false1|NaN|NaN|false|true|true0|NaN|NaN|false|true|true5|NaN|5|false|true|true"#,
            r#"80000"#,
            r#"101"#,
            r#"4"#,
            r#"100000"#,
            r#"[0,"a","b",9,4,5]"#,
            r#"[0,1,2,9,null,7]"#,
            r#"RangeError:it"#,
            r#"TypeError"#,
            r#"200050000"#,
            r#"9"#,
            r#"undefined"#,
            r#"cs"#,
            r#"TypeError"#,
            r#"true:4"#,
            r#"10000"#,
            r#"SyntaxError"#,
            r#"SyntaxError"#,
            r#"true"#,
            r#"false"#,
            r#"/q/i"#,
            r#"/t+/"#,
            r#"199990000"#,
            r#"{"g":5,"left":2,"z":1,"kind":"k"}"#,
            r#"{"left":3,"kind":"k"}{"left":4,"kind":"k"}{"0":"a","1":"b","left":5,"kind":"k"}"#,
            r#"getter"#,
            r#"1"#,
            r#"1,b,a,left,kind"#,
        ]
    );
}

#[test]
fn private_names_on_hot_paths_match_node() {
    assert_eq!(
        run(PRIVATE),
        [
            r#"1200180000 20001 40002 A20000 B20000"#,
            r#"TypeError TypeError TypeError"#,
            r#"true false true 1 1"#,
            r#"1225 TypeError TypeError"#,
            r#"TypeError"#,
            r#"600130000 TypeError TypeError"#,
            r#"200010000 9 d"#,
            r#"20001"#,
            r#"t"#,
            r#"5000050000"#,
        ]
    );
}

#[test]
fn calls_through_the_generic_helper_match_node() {
    assert_eq!(
        run(CALLS),
        [
            r#"3420000"#,
            r#"0,0,0,1,1 100,100,100,99,99"#,
            r#"objectobjectobjectnumbernullobject:g"#,
            r#"objectnumber"#,
            r#"900531000 30000 RangeError:neg 30001"#,
            r#"598400"#,
            r#"3:1-2-3 2 true 59999"#,
            r#"3000 RangeError"#,
            r#"30111"#,
        ]
    );
}

#[test]
fn element_reads_on_hot_paths_match_node() {
    assert_eq!(
        run(INDEX),
        [
            r#"1,2,3,"#,
            r#"1,undefined,3,"#,
            r#"1,proto,3, 7,proto,9,"#,
            r#"1,undefined,3,"#,
            r#"1,re,3,1,2,3,"#,
            r#"1,g,3,1"#,
            r#"4,5,6,"#,
            r#"a,a,undefined,"#,
            r#"1,custom,3,"#,
            r#"1,trap,3,"#,
            r#"0,1,undefined,"#,
            r#"0.5,1.5,2.5,x,y,z,255,0,1,"#,
        ]
    );
}
