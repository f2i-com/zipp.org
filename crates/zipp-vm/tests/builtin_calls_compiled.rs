//! A builtin's call of a compiled function, frame-free.
//!
//! A builtin that calls a JavaScript function used to push a frame and enter
//! the interpreter loop, which found the callee's compiled body and ran it.
//! A callee whose body has a frame-free native entry is now run as a compiled
//! caller's cross-call runs it: over a fresh window at the top of the
//! register file, under an activation that names its closure
//! (`Vm::call_compiled_windowed`). `Map.prototype.forEach` and
//! `Set.prototype.forEach` keep one window for every element, as the array
//! builtins do.
//!
//! This program heats functions with behaviours every call must show fresh
//! (a local read before it is written, captured variables, every `this`,
//! `new.target`, `arguments`, default and rest parameters, throws) and calls
//! them from `Map`/`Set` `forEach`, `replace`, `Array.from`, typed-array
//! methods, the JSON reviver and replacer, `call`/`apply`/`Reflect.apply`/
//! `bind`, getters, setters and coercion hooks run by builtins, promise
//! reactions and `await`.
//!
//! The expected lines are Node 24's output for the same program.

use zipp_vm::embed;

fn run(source: &str) -> Vec<String> {
    let mut state = embed::compile_script(source).expect("source compiles");
    #[cfg(feature = "safe-sandbox")]
    state.set_limits(4_000_000_000, None);
    state.run_init().expect("source runs");
    state.take_output()
}

const PROGRAM: &str = r####"// Differential probe: builtins other than the array iteration methods calling compiled functions.
var out = [];
function log(label, v) { out.push(label + ": " + v); }
function heat(f) { for (var i = 0; i < 600; i++) f(); }

// Functions with behaviours a fresh activation must show on every call.
function stale(x) { var t; if (x > 5) t = x; return t === undefined ? -1 : t; }
function mkAdd(k) { return function (x) { var t; if (x > k) t = x + k; return t === undefined ? -k : t; }; }
var add3 = mkAdd(3);
function sloppyThis() { return this === globalThis ? "g" : typeof this; }
function strictThis() { "use strict"; return this === undefined ? "u" : this === null ? "n" : typeof this; }
function newT() { return new.target === undefined ? "call" : "new"; }
function argc() { return arguments.length; }
function dflt(a, b = 7) { return a + b; }
function rest(a, ...r) { return a + r.length; }

// 1. Map and Set forEach.
var m = new Map(); var st = new Set();
for (var i = 0; i < 40; i++) { m.set("k" + i, (i * 7) % 11); st.add((i * 5) % 13); }
function mapStale() { var r = []; m.forEach(function (v, k) { r.push(stale(v)); }); return r.slice(0, 10).join(","); }
heat(mapStale);
log("Map.forEach", mapStale());
function mapDirect() { var r = []; m.forEach(function (v) { var t; if (v > 5) t = v; r.push(t === undefined ? -1 : t); }); return r.slice(0, 10).join(","); }
heat(mapDirect);
log("Map.forEach locals", mapDirect());
function setSum(k) { var s = 0; st.forEach(function (v) { s += v + k; }); return s; }
heat(function () { setSum(1); });
log("Set.forEach capture", setSum(1) + " " + setSum(100));

// 2. String.prototype.replace with a function, and replaceAll.
function rep(s, k) { return s.replace(/[0-9]/g, function (d) { var t; if (d > "4") t = String(+d + k); return t === undefined ? "_" : t; }); }
heat(function () { rep("a1b7c3d9e5", 1); });
log("replace fn", rep("a1b7c3d9e5", 1) + " " + rep("0123456789", 10));

// 3. Array.from with a mapper, and a typed array's methods.
function fromMap(n, k) { return Array.from({ length: n }, function (_, i) { var t; if (i & 1) t = i * k; return t === undefined ? 0 : t; }); }
heat(function () { fromMap(8, 2); });
log("Array.from", fromMap(8, 2).join(",") + " " + fromMap(5, 10).join(","));
var ta = new Float64Array([5, 1, 9, 3, 7, 2, 8]);
function taMap(k) { return Array.prototype.join.call(ta.map(function (x) { var t; if (x > 4) t = x * k; return t === undefined ? -1 : t; }), ","); }
heat(function () { taMap(2); });
log("typed map", taMap(2) + " " + taMap(3));
function taSort(dir) { var c = ta.slice(); c.sort(function (a, b) { return dir * (a - b); }); return Array.prototype.join.call(c, ","); }
heat(function () { taSort(1); });
log("typed sort", taSort(1) + " " + taSort(-1));

// 4. JSON reviver and replacer.
function revive(text, k) { return JSON.stringify(JSON.parse(text, function (key, v) { var t; if (typeof v === "number" && v > 2) t = v + k; return t === undefined ? v : t; })); }
heat(function () { revive('{"a":1,"b":5,"c":[3,2]}', 10); });
log("JSON reviver", revive('{"a":1,"b":5,"c":[3,2]}', 10) + " " + revive("[9,1]", -9));
function replacer(o, k) { return JSON.stringify(o, function (key, v) { return typeof v === "number" ? v * k : v; }); }
heat(function () { replacer({ a: 1, b: [2, 3] }, 2); });
log("JSON replacer", replacer({ a: 1, b: [2, 3] }, 2) + " " + replacer({ z: 5 }, 0.5));

// 5. call / apply / Reflect.apply / bind with every kind of `this`.
function viaCall() {
  return [sloppyThis.call(undefined), sloppyThis.call(null), sloppyThis.call(5), sloppyThis.call("s"), sloppyThis.call({}),
    strictThis.call(undefined), strictThis.call(null), strictThis.call(5), strictThis.call("s"), strictThis.call({}),
    sloppyThis.apply(true, []), strictThis.apply(true, []), Reflect.apply(sloppyThis, 1n, []), Reflect.apply(strictThis, Symbol.iterator, [])].join("");
}
heat(viaCall);
log("this kinds", viaCall());
var boundS = strictThis.bind(42), boundL = sloppyThis.bind("x");
heat(function () { boundS(); boundL(); });
log("bound", boundS() + boundL() + [1].map(boundS)[0] + [1].map(boundL)[0]);
function viaArgs() { return [argc.call(null), argc.call(null, 1, 2, 3), argc.apply(null, [1, 2]), dflt.call(null, 1), dflt.call(null, 1, 2), dflt.apply(null, [1, undefined]), rest.call(null, 1, 2, 3), rest.apply(null, [5])].join(","); }
heat(viaArgs);
log("arguments", viaArgs());
function viaNew() { return newT.call(null) + "," + Reflect.apply(newT, null, []) + "," + (new newT() instanceof newT) + "," + [1].map(newT)[0] + "," + Reflect.construct(function () { return { v: new.target === undefined ? "call" : "new" }; }, []).v; }
heat(viaNew);
log("new.target", viaNew());

// 6. Getters, setters and coercion hooks run by builtins.
function mkObj(k) {
  var hits = 0;
  var o = { get g() { hits++; var t; if (hits & 1) t = k; return t === undefined ? -k : t; }, set s(v) { hits += v; }, valueOf: function () { return k + hits; }, toString: function () { return "o" + k; } };
  return { o: o, hits: function () { return hits; } };
}
function hooks(k) {
  var h = mkObj(k);
  var copy = Object.assign({}, h.o), js = JSON.stringify({ g: h.o.g });
  h.o.s = 2;
  return copy.g + "|" + js + "|" + (h.o + 1) + "|" + `${h.o}` + "|" + [h.o, h.o].join("-") + "|" + (h.o > 3) + "|" + h.hits();
}
heat(function () { hooks(4); });
log("hooks", hooks(4) + " # " + hooks(9));

// 7. Promise reactions: chains of compiled callbacks, capturing and not, with throws.
var pending = 3;
// A hot callback the host runs as a job, with no caller frame under it,
// that calls a cold function, a native and a method late in its life.
var hotCalls = 0, coldSeen = [];
function cold(x) { coldSeen.push(x); return x * 2 + 1; }
function hot(x) {
  hotCalls++;
  if (hotCalls === 900) return cold(x) + Math.max(x, 3) + [x, 1].indexOf(1);
  if (hotCalls === 950) return cold(-x) + String(x).length;
  return x + 1;
}
(function () {
  var p = Promise.resolve(0);
  for (var i = 0; i < 1000; i++) p = p.then(hot);
  p.then(function (v) { log("host job calls out", v + " " + hotCalls + " " + coldSeen.join(",")); done(); });
}());
function done() { if (--pending === 0) console.log(out.join("\n")); }
function chain(k) {
  var p = Promise.resolve(1), seen = [];
  for (var i = 0; i < 300; i++) p = p.then(add3).then(function (x) { var t; if (x > 100) t = 1; return t === undefined ? (x + k) % 37 : t; });
  return p.then(function (x) { seen.push(x); throw new RangeError("r" + x); })
    .catch(function (e) { seen.push(e.message); return strictThis(); })
    .then(function (u) { seen.push(u); return stale(9); })
    .finally(function () { seen.push("fin"); })
    .then(function (v) { seen.push(v); return seen.join(","); });
}
chain(1).then(function (a) { log("promise chain", a); return chain(5); }).then(function (b) { log("promise chain 2", b); done(); });
async function awaits(k) { var s = 0; for (var i = 0; i < 400; i++) { s += await add3(i % 9) + k; } return s; }
awaits(0).then(function (a) { return awaits(2).then(function (b) { log("async", a + " " + b); done(); }); });
"####;

#[test]
fn builtins_calling_compiled_functions_match_node() {
    let expected: &[&str] = &[
        r####"Map.forEach: -1,7,-1,10,6,-1,9,-1,-1,8"####,
        r####"Map.forEach locals: -1,7,-1,10,6,-1,9,-1,-1,8"####,
        r####"Set.forEach capture: 91 1378"####,
        r####"replace fn: a_b8c_d10e6 _____1516171819"####,
        r####"Array.from: 0,2,0,6,0,10,0,14 0,10,0,30,0"####,
        r####"typed map: 10,-1,18,-1,14,-1,16 15,-1,27,-1,21,-1,24"####,
        r####"typed sort: 1,2,3,5,7,8,9 9,8,7,5,3,2,1"####,
        r####"JSON reviver: {"a":1,"b":15,"c":[13,2]} [0,1]"####,
        r####"JSON replacer: {"a":2,"b":[4,6]} {"z":2.5}"####,
        r####"this kinds: ggobjectobjectobjectunnumberstringobjectobjectbooleanobjectsymbol"####,
        r####"bound: numberobjectnumberobject"####,
        r####"arguments: 0,3,2,8,3,8,3,5"####,
        r####"new.target: call,call,true,call,new"####,
        r####"hooks: 4|{"g":-4}|9|o4|o4-o4|true|4 # 9|{"g":-9}|14|o9|o9-o9|true|4"####,
        r####"promise chain: -2,r-2,u,fin,9"####,
        r####"async: 1440 2240"####,
        r####"host job calls out: -5441 1000 899,-2748"####,
        r####"promise chain 2: 2,r2,u,fin,9"####,
    ];
    let out = run(PROGRAM);
    let got: Vec<&str> = out.iter().flat_map(|chunk| chunk.lines()).collect();
    assert_eq!(got.len(), expected.len(), "{got:#?}");
    for (got, want) in got.iter().zip(expected) {
        assert_eq!(got, want);
    }
}
