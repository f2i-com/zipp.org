//! Three fast paths against Node: a bound array-iterator `next` (a
//! loose-mode `for...of` as Babel compiles it) stepping the array without the
//! native call, iterator result objects allocated as object literals are, and
//! a method call site that learns each receiver layout reaching a way's
//! method. The probe covers growing, shrinking and exhausted arrays, keys and
//! entries, holes read through a prototype getter, array-likes and their
//! `length` getter, typed arrays, maps, sets, strings, `matchAll`, the wrong
//! receiver, binds of binds and with arguments, a `next` patched after the
//! bind, result objects' keys, attributes and mutation, and method sites over
//! many layouts whose own, inherited, prototype and class methods change. The
//! expected lines are Node's (v24).

const PROBE: &str = r###""use strict";
const out = [];
const log = (...a) => out.push(a.map(String).join(" "));
// A loose-mode for...of as Babel compiles it: the iterator's own `next`, bound.
function helper(r) {
  var t = r[Symbol.iterator];
  return (t = t.call(r)).next.bind(t);
}
function drain(next) {
  const got = [];
  for (let s; !(s = next()).done;) got.push(s.value);
  return got;
}
log("array", drain(helper([1, 2, 3])).join());
const grow = [1, 2];
const gn = helper(grow);
const seen = [];
for (let s; !(s = gn()).done;) {
  seen.push(s.value);
  if (grow.length < 5) grow.push(s.value * 10);
}
log("grow", seen.join());
const shrink = [1, 2, 3, 4];
const sn = helper(shrink);
sn();
shrink.length = 1;
log("shrink", JSON.stringify(sn()), JSON.stringify(sn()));
const lat = [1];
const ln = helper(lat);
ln();
ln();
lat.push(2);
log("latch", JSON.stringify(ln()));
const ki = [7, 8].keys();
const kn = ki.next.bind(ki);
log("keys", JSON.stringify(kn()), JSON.stringify(kn()), JSON.stringify(kn()));
const ei = [7, 8].entries();
const en = ei.next.bind(ei);
log("entries", JSON.stringify(en()), JSON.stringify(en()));
Object.defineProperty(Array.prototype, 1, { get() { return "proto" + this.length; }, configurable: true });
let holes = "";
for (let hn = helper([0, , 2]), s; !(s = hn()).done;) holes += s.value + ";";
delete Array.prototype[1];
log("hole", holes);
const al = { length: 2, 0: "a", 1: "b" };
const ai = Array.prototype.values.call(al);
log("array-like", drain(ai.next.bind(ai)).join());
const counted = { get length() { counted.reads++; return 2; }, reads: 0, 0: "x", 1: "y" };
const ci = Array.prototype.values.call(counted);
log("array-like getter", drain(ci.next.bind(ci)).join(), counted.reads);
log("typed", drain(helper(new Uint8Array([5, 6]))).join());
log("map", drain(helper(new Map([[1, 2]]))).map(String).join("|"));
log("set", drain(helper(new Set(["x", "y"]))).join());
log("string", drain(helper("h\u{1F600}")).length);
const mi = "a1b22".matchAll(/\d+/g);
log("matchAll", drain(mi.next.bind(mi)).map((m) => m[0] + "@" + m.index).join());
log("array after matchAll", drain(helper([9, 8])).join());
try {
  const v = [].values();
  v.next.bind({})();
} catch (e) {
  log("receiver", e.constructor.name);
}
try {
  const v = [].values();
  v.next.bind(1)();
} catch (e) {
  log("primitive receiver", e.constructor.name);
}
const b2 = [3].values();
const bb = b2.next.bind(b2).bind(null);
log("bind of bind", JSON.stringify(bb()), JSON.stringify(bb()));
const b3 = [4].values();
log("bound args", JSON.stringify(b3.next.bind(b3, "ignored")()));
const proto = Object.getPrototypeOf([][Symbol.iterator]());
const saved = proto.next;
const early = [5].values();
const earlyNext = early.next.bind(early);
proto.next = function () { return { value: "patched", done: true }; };
const late = [6].values();
log("patched", JSON.stringify(earlyNext()), JSON.stringify(late.next.bind(late)()));
proto.next = saved;
const r = helper([1])();
log("result", Object.keys(r).join(), JSON.stringify(Object.getOwnPropertyDescriptors(r)), Object.getPrototypeOf(r) === Object.prototype);
r.extra = 1;
delete r.value;
log("result mutated", JSON.stringify(r), Object.keys(r).join());
const r2 = helper([2])();
log("next result", JSON.stringify(r2), Object.keys(r2).join(), Object.isExtensible(r2));
let total = 0;
const kept = [];
for (let i = 0; i < 20000; i++) {
  const n = helper([i, i + 1]);
  let s;
  while (!(s = n()).done) total += s.value;
  if (i % 5000 === 0) kept.push(s);
}
log("loops", total, JSON.stringify(kept));
function* g() { yield 1; return 2; }
const gi = g();
log("generator", JSON.stringify(gi.next()), JSON.stringify(gi.next()), JSON.stringify(gi.next()));
// One call site, receivers of many layouts reaching the same inherited method.
function Base() {}
Base.prototype.who = function () { return "base:" + this.tag; };
function make(k) {
  const o = new Base();
  for (let j = 0; j < k; j++) o["f" + j] = j;
  o.tag = k;
  return o;
}
const objs = [];
for (let k = 0; k < 12; k++) objs.push(make(k));
function callAll(list) {
  const res = [];
  for (const o of list) res.push(o.who());
  return res.join(",");
}
log("layouts", callAll(objs), callAll(objs));
objs[3].who = function () { return "own3"; };
log("own override", callAll(objs));
Base.prototype.who = function () { return "new:" + this.tag; };
log("proto change", callAll(objs));
const other = Object.create({ who() { return "other"; } });
for (let j = 0; j < 2; j++) other["f" + j] = j;
other.tag = 2;
log("other proto", callAll([objs[2], other, objs[2], other]));
Object.setPrototypeOf(objs[5], { who() { return "reproto"; } });
log("reproto", callAll(objs));
class K { m() { return "K" + this.n; } }
class L extends K { m() { return "L" + super.m(); } }
const ks = [];
for (let k = 0; k < 10; k++) {
  const o = k & 1 ? new L() : new K();
  for (let j = 0; j < k; j++) o["p" + j] = j;
  o.n = k;
  ks.push(o);
}
function callK(list) { return list.map((o) => o.m()).join(","); }
log("class layouts", callK(ks), callK(ks));
K.prototype.m = function () { return "patched" + this.n; };
log("class patched", callK(ks));
const owners = [];
for (let k = 0; k < 10; k++) {
  const o = {};
  for (let j = 0; j < k; j++) o["q" + j] = j;
  o.f = function () { return "f" + k; };
  owners.push(o);
}
function callF(list) { return list.map((o) => o.f()).join(","); }
log("own layouts", callF(owners), callF(owners));
owners[4].f = function () { return "swapped"; };
log("own swapped", callF(owners));
delete owners[5].f;
try {
  callF(owners);
} catch (e) {
  log("own deleted", e.constructor.name);
}
console.log(out.join("\n"));
"###;

const EXPECTED: &str = r###"array 1,2,3
grow 1,2,10,20,100
shrink {"done":true} {"done":true}
latch {"done":true}
keys {"value":0,"done":false} {"value":1,"done":false} {"done":true}
entries {"value":[0,7],"done":false} {"value":[1,8],"done":false}
hole 0;proto3;2;
array-like a,b
array-like getter x,y 3
typed 5,6
map 1,2
set x,y
string 2
matchAll 1@1,22@3
array after matchAll 9,8
receiver TypeError
primitive receiver TypeError
bind of bind {"value":3,"done":false} {"done":true}
bound args {"value":4,"done":false}
patched {"value":5,"done":false} {"value":"patched","done":true}
result value,done {"value":{"value":1,"writable":true,"enumerable":true,"configurable":true},"done":{"value":false,"writable":true,"enumerable":true,"configurable":true}} true
result mutated {"done":false,"extra":1} done,extra
next result {"value":2,"done":false} value,done true
loops 400000000 [{"done":true},{"done":true},{"done":true},{"done":true}]
generator {"value":1,"done":false} {"value":2,"done":true} {"done":true}
layouts base:0,base:1,base:2,base:3,base:4,base:5,base:6,base:7,base:8,base:9,base:10,base:11 base:0,base:1,base:2,base:3,base:4,base:5,base:6,base:7,base:8,base:9,base:10,base:11
own override base:0,base:1,base:2,own3,base:4,base:5,base:6,base:7,base:8,base:9,base:10,base:11
proto change new:0,new:1,new:2,own3,new:4,new:5,new:6,new:7,new:8,new:9,new:10,new:11
other proto new:2,other,new:2,other
reproto new:0,new:1,new:2,own3,new:4,reproto,new:6,new:7,new:8,new:9,new:10,new:11
class layouts K0,LK1,K2,LK3,K4,LK5,K6,LK7,K8,LK9 K0,LK1,K2,LK3,K4,LK5,K6,LK7,K8,LK9
class patched patched0,Lpatched1,patched2,Lpatched3,patched4,Lpatched5,patched6,Lpatched7,patched8,Lpatched9
own layouts f0,f1,f2,f3,f4,f5,f6,f7,f8,f9 f0,f1,f2,f3,f4,f5,f6,f7,f8,f9
own swapped f0,f1,f2,f3,swapped,f5,f6,f7,f8,f9
own deleted TypeError"###;

/// Run src on a 256 MiB stack, as the CLI does, and return its output.
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
fn iterator_steps_results_and_method_ways_match_node() {
    assert_eq!(run(PROBE), EXPECTED);
}
