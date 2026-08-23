// W19 M2 acceptance: `delete` over every receiver kind x every key spelling the
// `delete_prop` waterfall special-cases.
//
// The ordinary-object fast path added at the head of `Vm::delete_prop` is a
// conjunction of NEGATIVE guards; every case it rejects must still reach the
// untouched waterfall and answer exactly as before. This file is the oracle for
// that claim, and it is engine-agnostic — run it under node as well as under
// both settings of ZIPP_NO_DELETE_FASTPATH and diff the three outputs:
//
//   D=crates/zipp-vm/tests/w19_delete_differential.js
//   node $D                                          > a.txt
//   target/release/zipp.exe js $D                     > b.txt
//   ZIPP_NO_DELETE_FASTPATH=1 target/release/zipp.exe js $D > c.txt
//
// b.txt and c.txt must be BYTE-IDENTICAL (the mechanisms are perf work). a.txt
// differs only in the pre-existing zipp/node divergences the same command
// reproduces with every W19 latch off: the contents of globalThis, an
// `arguments` object listing `length` twice in getOwnPropertyNames, and class
// static-member ordering.
//
// It prints the delete's RETURN VALUE and the receiver's POST-STATE (own keys in
// order, their values, and the configurable/writable/enumerable bits), because a
// mis-guarded fast path is a silent wrong answer — a delete that reports true
// and removes nothing, removes the wrong slot, or skips a version bump — not a
// crash.

"use strict";

function show(v) {
  if (v === null) return "null";
  if (v === undefined) return "undefined";
  var t = typeof v;
  if (t === "string") return JSON.stringify(v);
  if (t === "number" || t === "boolean") return String(v);
  if (t === "symbol") return "sym";
  if (t === "function") return "fn";
  try { return "[" + Object.prototype.toString.call(v) + "]"; } catch (e) { return "?"; }
}

function state(o) {
  var out = [];
  var names;
  try { names = Object.getOwnPropertyNames(o); } catch (e) { return "names-threw:" + e.name; }
  for (var i = 0; i < names.length; i++) {
    var n = names[i];
    var d;
    try { d = Object.getOwnPropertyDescriptor(o, n); } catch (e) { out.push(n + "=<desc-threw>"); continue; }
    if (!d) { out.push(n + "=<no-desc>"); continue; }
    var bits = (d.enumerable ? "e" : "-") + (d.configurable ? "c" : "-") +
      ("writable" in d ? (d.writable ? "w" : "-") : "A");
    var val = ("value" in d) ? show(d.value) : "accessor";
    out.push(n + ":" + bits + "=" + val);
  }
  return "{" + out.join(",") + "}";
}

function attempt(label, mk, key, strictMode) {
  var o, res;
  try { o = mk(); } catch (e) { console.log(label + " | " + key + " | MAKE-THREW " + e.name); return; }
  var before = state(o);
  try {
    res = strictMode ? strictDelete(o, key) : sloppyDelete(o, key);
  } catch (e) {
    console.log(label + " | " + key + " | THREW " + e.name + " | post=" + state(o));
    return;
  }
  var after = state(o);
  console.log(label + " | " + key + " | " + res + " | " + (before === after ? "unchanged" : after));
}

function strictDelete(o, k) { "use strict"; return delete o[k]; }
var sloppyDelete = new Function("o", "k", "return delete o[k];");

// ---- receivers ----
var receivers = [
  ["plain-small", function () { return { a: 1, b: 2, prop_0: 3 }; }],
  ["plain-60", function () {
    var o = {};
    for (var p = 0; p < 60; p++) o["prop_" + p] = p;
    return o;
  }],
  ["plain-60-halfdeleted", function () {
    var o = {};
    for (var p = 0; p < 60; p++) o["prop_" + p] = p;
    for (var p = 0; p < 60; p += 2) delete o["prop_" + p];
    for (var p = 0; p < 60; p += 2) o["prop_" + p] = p * 2;
    return o;
  }],
  ["plain-nonconfigurable", function () {
    var o = { keep: 1 };
    Object.defineProperty(o, "fixed", { value: 9, configurable: false, enumerable: true, writable: true });
    Object.defineProperty(o, "prop_0", { value: 8, configurable: false, enumerable: true, writable: false });
    return o;
  }],
  ["plain-accessor", function () {
    var o = { plain: 1 };
    Object.defineProperty(o, "acc", { get: function () { return 1; }, configurable: true, enumerable: true });
    Object.defineProperty(o, "accFixed", { get: function () { return 2; }, configurable: false, enumerable: true });
    return o;
  }],
  ["plain-frozen", function () { return Object.freeze({ a: 1, prop_0: 2 }); }],
  ["plain-sealed", function () { return Object.seal({ a: 1, prop_0: 2 }); }],
  ["plain-nonextensible", function () { var o = { a: 1, prop_0: 2 }; Object.preventExtensions(o); return o; }],
  ["plain-index-keys", function () { var o = {}; o[0] = "z"; o[1] = "y"; o["05"] = "x"; o["+5"] = "w"; o["4294967295"] = "v"; o.a = 1; return o; }],
  ["plain-proto-chain", function () { var p = { inherited: 1, prop_0: 99 }; var o = Object.create(p); o.own = 2; return o; }],
  ["null-proto", function () { var o = Object.create(null); o.a = 1; o.prop_0 = 2; return o; }],
  ["array-dense", function () { return [10, 20, 30]; }],
  ["array-holey", function () { var a = [1, 2, 3, 4]; delete a[1]; a.named = 7; return a; }],
  ["array-frozen", function () { return Object.freeze([1, 2]); }],
  ["arguments", function () { return (function (a, b) { return arguments; })(1, 2); }],
  ["typedarray", function () { var t = new Int32Array(3); t[0] = 5; return t; }],
  ["boxed-string", function () { var s = new String("ab"); s.extra = 1; return s; }],
  ["regexp", function () { var r = /x/g; r.extra = 1; return r; }],
  ["function", function () { function f(a) { return a; } f.extra = 1; return f; }],
  ["arrow", function () { var f = function () { return 1; }; f.extra = 1; return f; }],
  ["class-ctor", function () { var C = class { static s() { return 1; } static get g() { return 2; } }; C.extra = 1; return C; }],
  ["class-instance", function () { var C = class { constructor() { this.f = 1; this.prop_0 = 2; } }; return new C(); }],
  ["date", function () { var d = new Date(0); d.extra = 1; return d; }],
  ["map", function () { var m = new Map(); m.extra = 1; return m; }],
  ["globalThis", function () { return globalThis; }],
  ["proxy-passthru", function () { return new Proxy({ a: 1, prop_0: 2 }, {}); }],
  ["proxy-deny", function () {
    return new Proxy({ a: 1, prop_0: 2 }, { deleteProperty: function () { return false; } });
  }],
  ["proxy-logging", function () {
    var t = { a: 1, prop_0: 2 };
    return new Proxy(t, {
      deleteProperty: function (tt, k) { trace.push("trap:" + String(k)); return Reflect.deleteProperty(tt, k); }
    });
  }],
  ["boxed-number", function () { var n = new Number(3); n.extra = 1; return n; }]
];

// ---- keys: every spelling the waterfall reasons about, plus ordinary ones ----
var keys = [
  "a", "own", "extra", "keep", "missing", "prop_0", "prop_1", "prop_30", "prop_59",
  "aVeryLongPropertyNameThatIsNotSpecial",
  "__proto__", "length", "name", "prototype", "caller", "arguments", "lastIndex",
  "index", "input", "groups", "indices", "constructor", "toString",
  "0", "1", "2", "05", "+5", "-1", " 5", "4294967295", "4294967294", "1e2",
  "", "#x", "@@iterator", "$dollar", "_under", "fixed", "acc", "accFixed",
  "s", "g", "f", "named", "inherited", "undefined", "NaN", "Infinity"
];

var trace = [];
console.log("=== strict ===");
for (var r = 0; r < receivers.length; r++) {
  for (var k = 0; k < keys.length; k++) {
    attempt(receivers[r][0], receivers[r][1], keys[k], true);
  }
}
console.log("=== sloppy ===");
for (var r2 = 0; r2 < receivers.length; r2++) {
  for (var k2 = 0; k2 < keys.length; k2++) {
    attempt(receivers[r2][0], receivers[r2][1], keys[k2], false);
  }
}
console.log("=== proxy trap trace === " + trace.length);

// ---- randomised add/delete/read/enumerate sequences over one map ----
// Drives the PropIndex through the grow boundaries and the renumber sweep with
// interleaved reads, so a wrong-slot hit shows up as a wrong VALUE, not a crash.
var seed = 12345;
function rnd(n) { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed % n; }
console.log("=== churn ===");
for (var trial = 0; trial < 12; trial++) {
  var o = {};
  var mirror = Object.create(null);
  var live = [];
  for (var step = 0; step < 900; step++) {
    var op = rnd(10);
    if (op < 5 || live.length === 0) {
      var nk = "key_" + rnd(500);
      o[nk] = step;
      if (!(nk in mirror)) live.push(nk);
      mirror[nk] = step;
    } else if (op < 8) {
      var vi = rnd(live.length);
      var vk = live[vi];
      var got = delete o[vk];
      if (got !== true) console.log("BAD delete result " + got + " for " + vk);
      live.splice(vi, 1);
      delete mirror[vk];
    } else {
      var ri = rnd(live.length);
      var rk = live[ri];
      if (o[rk] !== mirror[rk]) {
        console.log("WRONG VALUE trial=" + trial + " step=" + step + " key=" + rk +
          " got=" + o[rk] + " want=" + mirror[rk]);
      }
    }
  }
  var n = 0, bad = 0;
  for (var kk in o) { n++; if (o[kk] !== mirror[kk]) bad++; }
  var names2 = Object.keys(o);
  console.log("trial " + trial + " forin=" + n + " keys=" + names2.length +
    " live=" + live.length + " mismatched=" + bad);
}
