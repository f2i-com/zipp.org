//! Built-in details the kangax compat-table checks, against Node.
//!
//! - %ArrayIteratorPrototype% and the Map, Set, String and RegExp String
//!   iterator prototypes have no own @@iterator; they inherit
//!   %Iterator.prototype%'s, and iterating them still works.
//! - Each String.prototype match/search/replace/replaceAll/split/matchAll
//!   reads its argument's @@-method once (and IsRegExp once): an argument
//!   without one is ToString'd for the default algorithm, and RegExpCreate
//!   never runs IsRegExp. `RegExp(p)` reads `@@match` once for both its
//!   `constructor` shortcut and the build. A RegExp whose @@match was
//!   removed becomes RegExpCreate(ToString(re)) instead of a TypeError.
//! - `SharedArrayBuffer[Symbol.species]` is the shared species getter.
//! - `__defineGetter__`/`__defineSetter__` box a primitive receiver.
//!
//! The expected lines are Node's (v24).

const PROBE: &str = r###""use strict";
const out = [];
const log = (...a) => out.push(a.map(String).join(" "));
const key = (k) => (typeof k === "symbol" ? k.description : k);

// Iterator prototypes inherit @@iterator from %Iterator.prototype%.
const iters = {
  array: [][Symbol.iterator](),
  map: new Map()[Symbol.iterator](),
  set: new Set().values(),
  string: ""[Symbol.iterator](),
  regexpString: "a".matchAll(/a/g),
};
for (const [name, it] of Object.entries(iters)) {
  const proto = Object.getPrototypeOf(it);
  log("iter", name, proto.hasOwnProperty(Symbol.iterator), Object.getPrototypeOf(proto) === Iterator.prototype,
    it[Symbol.iterator]() === it, Object.getOwnPropertySymbols(proto).map(key).join("|"));
}
log("iter-root", Iterator.prototype.hasOwnProperty(Symbol.iterator));
const m = new Map([[1, "a"], [2, "b"]]);
let acc = "";
for (const [k, v] of m.entries()) acc += k + v;
for (const v of new Set([3, 4]).values()) acc += v;
for (const v of [5, 6].values()) acc += v;
for (const c of "xy"[Symbol.iterator]()) acc += c;
log("for-of", acc, [..."ab".matchAll(/./g)].map((r) => r[0] + r.index).join(","));
log("matchAll-spread", Array.from("a1b2".matchAll(/\d/g), (r) => r[0]).join(""));

// Each internal Get on the argument happens once, in spec order.
function traced(target) {
  const gets = [];
  const p = new Proxy(target, { get(o, k) { gets.push(key(k)); return o[k]; } });
  return [p, gets];
}
function trace(label, make, run) {
  const [p, gets] = traced(make());
  let r;
  try { r = run(p); } catch (e) { r = e.constructor.name; }
  log("gets", label, gets.join(","), typeof r === "object" && r !== null ? JSON.stringify(r) : String(r));
}
const prim = () => { const o = {}; o[Symbol.toPrimitive] = () => "b"; return o; };
trace("RegExp()", () => { const re = { constructor: null, source: "x", flags: "g" }; re[Symbol.match] = true; return re; }, (p) => RegExp(p).toString());
trace("new RegExp()", () => { const re = { source: "x", flags: "i" }; re[Symbol.match] = true; return re; }, (p) => new RegExp(p).toString());
trace("RegExp(plain)", prim, (p) => RegExp(p).toString());
trace("match", prim, (p) => "abc".match(p));
trace("search", prim, (p) => "abc".search(p));
trace("replace", prim, (p) => "abc".replace(p, "X"));
trace("replaceAll", prim, (p) => "abcb".replaceAll(p, "X"));
trace("split", prim, (p) => "abcbd".split(p));
trace("split-limit", prim, (p) => "abcbd".split(p, { valueOf() { out.push("  limit coerced"); return 2; } }));
trace("matchAll", prim, (p) => [..."abcb".matchAll(p)].map((r) => r.index));
trace("String-obj match", prim, (p) => String.prototype.match.call(new String("abc"), p));
trace("String-obj replace", prim, (p) => String.prototype.replace.call(new String("abc"), p, "Y"));
const noMatch = /a/;
Object.defineProperty(noMatch, Symbol.match, { value: undefined });
log("deleted-@@match", JSON.stringify("x/a/".match(noMatch)), "x/a/".search(noMatch));

// SharedArrayBuffer[@@species]
const sp = Object.getOwnPropertyDescriptor(SharedArrayBuffer, Symbol.species);
log("sab-species", SharedArrayBuffer[Symbol.species] === SharedArrayBuffer, typeof sp.get, sp.get.name, sp.set, sp.configurable, sp.enumerable);

// __defineGetter__ / __defineSetter__ box a primitive receiver.
const k = "__accessors_probe__";
Object.prototype.__defineGetter__.call(1, k, function () { return 1; });
Object.prototype.__defineSetter__.call("s", k, function () {});
log("define-primitive", Number.prototype.hasOwnProperty(k), String.prototype.hasOwnProperty(k), (1)[k]);
for (const bad of [null, undefined]) {
  try { Object.prototype.__defineGetter__.call(bad, k, function () {}); log("define-nullish", "no throw"); } catch (e) { log("define-nullish", e.constructor.name); }
}
try { Object.prototype.__defineGetter__.call(1, k, 5); } catch (e) { log("define-noncallable", e.constructor.name); }
const o = {};
o.__defineGetter__("g", () => 7);
log("define-object", o.g, JSON.stringify(Object.getOwnPropertyDescriptor(o, "g").enumerable));
console.log(out.join("\n"));
"###;

const EXPECTED: &str = r###"iter array false true true Symbol.toStringTag
iter map false true true Symbol.toStringTag
iter set false true true Symbol.toStringTag
iter string false true true Symbol.toStringTag
iter regexpString false true true Symbol.toStringTag
iter-root true
for-of 1a2b3456xy a0,b1
matchAll-spread 12
gets RegExp() Symbol.match,constructor,source,flags /x/g
gets new RegExp() Symbol.match,source,flags /x/i
gets RegExp(plain) Symbol.match,Symbol.toPrimitive /b/
gets match Symbol.match,Symbol.toPrimitive ["b"]
gets search Symbol.search,Symbol.toPrimitive 1
gets replace Symbol.replace,Symbol.toPrimitive aXc
gets replaceAll Symbol.match,Symbol.replace,Symbol.toPrimitive aXcX
gets split Symbol.split,Symbol.toPrimitive ["a","c","d"]
  limit coerced
gets split-limit Symbol.split,Symbol.toPrimitive ["a","c"]
gets matchAll Symbol.match,Symbol.matchAll,Symbol.toPrimitive [1,3]
gets String-obj match Symbol.match,Symbol.toPrimitive ["b"]
gets String-obj replace Symbol.replace,Symbol.toPrimitive aYc
deleted-@@match ["/a/"] 2
sab-species true function get [Symbol.species] undefined true false
define-primitive false false undefined
define-nullish TypeError
define-nullish TypeError
define-noncallable TypeError
define-object 7 true"###;

fn run(src: &str) -> String {
    let src = src.to_string();
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(move || {
            let out = zipp_vm::run(&src).expect("the probe compiles");
            assert!(out.error.is_none(), "runtime error: {:?}", out.error);
            out.output.join("\n")
        })
        .expect("spawn the probe thread")
        .join()
        .expect("the probe thread finishes")
}

#[test]
fn compat_table_builtin_details_match_node() {
    assert_eq!(run(PROBE), EXPECTED);
}
