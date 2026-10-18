//! Array indices and lengths at and past 2^32 must keep their exact values.
//! A generic array-like's ToLength runs to 2^53-1 and a relative index or a
//! `lastIndex` to 2^53, but a `usize` is 32 bits on wasm32 (every released
//! .wasm engine) and armv7: a cast before the clamp truncated 2^32 to 0, or
//! saturated 2^53-1 at 2^32-1, so `new ArrayBuffer(8).slice(2, 2 ** 32)` was
//! empty and `findLastIndex` on a 2^53-1 length began at index 2^32-2 (and
//! an offset + length sum wrapped: a DataView of 2^32-1 bytes over 8).
//!
//! This runs on a 64-bit host, where the answers were already right, so it
//! pins the semantics against Node; the 32-bit proof is a Test262 run of an
//! i686 build. Off in the safe-sandbox profile: its native-loop and buffer
//! caps turn several rows into the documented RangeErrors.
#![cfg(not(feature = "safe-sandbox"))]

const SOURCE: &str = r##"
var out = [];
var P32 = 2 ** 32, MAX = 2 ** 53 - 1;
function show(v) {
  if (Array.isArray(v)) return "[" + v.map(show).join(",") + "]";
  if (typeof v === "string") return JSON.stringify(v);
  if (v && typeof v === "object") {
    if (ArrayBuffer.isView(v)) return v.constructor.name + "(" + Array.from(v).join(",") + ")";
    return "{" + Object.keys(v).map(function (k) { return k + ":" + show(v[k]); }).join(",") + "}";
  }
  return String(v);
}
function t(label, f) {
  var r;
  try { r = show(f()); } catch (e) { r = "throws " + (e && e.constructor && e.constructor.name); }
  out.push(label + " => " + r);
}
// ArrayBuffer / SharedArrayBuffer slice: start and end clamp in 64 bits.
[ArrayBuffer, SharedArrayBuffer].forEach(function (C) {
  [P32 - 1, P32, P32 + 1, 2 ** 53].forEach(function (n) {
    t(C.name + " slice(2, " + n + ")", function () { return new C(8).slice(2, n).byteLength; });
    t(C.name + " slice(" + n + ", 7)", function () { return new C(8).slice(n, 7).byteLength; });
    t(C.name + " slice(-" + n + ", 3)", function () { return new C(8).slice(-n, 3).byteLength; });
  });
});
// Resizable buffers: a length past 2^32 is out of range, not 0.
[P32 - 1, P32, P32 + 1].forEach(function (n) {
  t("resize(" + n + ")", function () {
    var b = new ArrayBuffer(8, { maxByteLength: 16 });
    try { b.resize(n); } catch (e) { return [e.constructor.name, b.byteLength]; }
    return b.byteLength;
  });
  t("grow(" + n + ")", function () {
    var b = new SharedArrayBuffer(8, { maxByteLength: 16 });
    try { b.grow(n); } catch (e) { return [e.constructor.name, b.byteLength]; }
    return b.byteLength;
  });
});
// TypedArray relative indices and the set offset.
[P32 - 1, P32, P32 + 1].forEach(function (n) {
  t("TA slice(1, " + n + ")", function () { return new Int8Array([1, 2, 3]).slice(1, n); });
  t("TA slice(" + n + ")", function () { return new Int8Array([1, 2, 3]).slice(n); });
  t("TA subarray(" + n + ")", function () { return new Int8Array([1, 2, 3]).subarray(n).length; });
  t("TA fill(9, " + n + ")", function () { return new Int8Array([1, 2, 3]).fill(9, n); });
  t("TA copyWithin(0, 1, " + n + ")", function () { return new Int8Array([1, 2, 3]).copyWithin(0, 1, n); });
  t("TA at(" + n + ")", function () { return new Int8Array([1, 2, 3]).at(n); });
  t("TA at(-" + n + ")", function () { return new Int8Array([1, 2, 3]).at(-n); });
  t("TA set([1], " + n + ")", function () { return new Int8Array(4).set([1], n); });
  t("TA set([], " + n + ")", function () { return new Int8Array(4).set([], n); });
  t("TA set(TA, " + n + ")", function () { return new Int8Array(4).set(new Int8Array(1), n); });
});
t("TA set(detached, 2^32)", function () {
  var a = new Int8Array(4);
  return a.set([1], { valueOf: function () { a.buffer.transfer(); return P32; } });
});
// Views: a byteOffset of 2^32 is aligned, and offset + length must not wrap.
t("Float64Array(detached, 2^32)", function () {
  var b = new ArrayBuffer(8);
  b.transfer();
  return new Float64Array(b, P32);
});
t("Float64Array(buf, 2^32, length)", function () {
  var calls = 0;
  try {
    new Float64Array(new ArrayBuffer(8), P32, { valueOf: function () { calls++; return 0; } });
  } catch (e) { return [calls, e.constructor.name]; }
  return calls;
});
[P32 - 1, P32].forEach(function (n) {
  t("DataView(buf, 1, " + n + ")", function () { return new DataView(new ArrayBuffer(8), 1, n).byteLength; });
});
t("DataView(buf, 2^32)", function () { return new DataView(new ArrayBuffer(8), P32).byteLength; });
t("BigInt.asIntN(2^32, -1n)", function () { return BigInt.asIntN(P32, -1n); });
t("BigInt.asUintN(2^32, 5n)", function () { return BigInt.asUintN(P32, 5n); });
// Strings: positions and lengths past 2^32 are out of range.
[P32 - 1, P32, P32 + 1, 2 ** 53].forEach(function (n) {
  t("charAt(" + n + ")", function () { return "abc".charAt(n); });
  t("charCodeAt(" + n + ")", function () { return "abc".charCodeAt(n); });
  t("codePointAt(" + n + ")", function () { return "abc".codePointAt(n); });
  t("substr(1, " + n + ")", function () { return "abc".substr(1, n); });
  t("padStart(" + n + ")", function () { return "abc".padStart(n).length; });
  t("padEnd(" + n + ", '')", function () { return "abc".padEnd(n, ""); });
});
t("padStart fill runs first", function () {
  return "abc".padStart(P32, { toString: function () { throw new TypeError("fill"); } });
});
// Dense arrays: from/start/deleteCount clamp in 64 bits.
[P32 - 1, P32, P32 + 1].forEach(function (n) {
  t("includes(1, " + n + ")", function () { return [1, 2, 3].includes(1, n); });
  t("holey includes(1, " + n + ")", function () { return [1, , 3].includes(1, n); });
  t("at(" + n + ")", function () { return [1, 2, 3].at(n); });
  t("splice(" + n + ")", function () { var a = [1, 2, 3]; return [a.splice(n), a]; });
  t("splice(1, " + n + ")", function () { var a = [1, 2, 3]; return [a.splice(1, n), a]; });
  t("toSpliced(" + n + ", 0, 9)", function () { return [1, 2, 3].toSpliced(n, 0, 9); });
  t("toSpliced(1, " + n + ")", function () { return [1, 2, 3].toSpliced(1, n); });
});
// Array-likes near 2^53 and around 2^32, through the generic protocol.
var big = { length: MAX };
big[MAX - 2] = "a";
big[MAX - 1] = "b";
t("al slice near 2^53", function () { return Array.prototype.slice.call(big, MAX - 2, MAX); });
t("al slice(-2)", function () { return Array.prototype.slice.call(big, -2); });
t("al slice proxied", function () { return Array.prototype.slice.call(new Proxy(big, {}), MAX - 2); });
t("al slice around 2^32", function () {
  var o = { length: P32 + 2 };
  o[P32 - 1] = "a"; o[P32] = "b"; o[P32 + 1] = "c";
  return Array.prototype.slice.call(o, P32 - 1);
});
t("al findLast", function () { return Array.prototype.findLast.call(big, function (v) { return v !== undefined; }); });
t("al findLastIndex", function () { return Array.prototype.findLastIndex.call(big, function (v) { return v === "a"; }); });
[P32, P32 + 1, MAX].forEach(function (n) {
  t("al findLastIndex first index, length " + n, function () {
    var first;
    Array.prototype.findLastIndex.call({ length: n }, function (v, i) { first = i; return true; });
    return first;
  });
});
t("al indexOf", function () { var o = { length: Infinity }; o[MAX - 1] = "t"; return Array.prototype.indexOf.call(o, "t", MAX - 3); });
t("al lastIndexOf", function () { var o = { length: Infinity }; o[MAX - 2] = "t"; o[MAX - 1] = "u"; return Array.prototype.lastIndexOf.call(o, "t"); });
t("al includes", function () { var o = { length: Infinity }; o[MAX - 1] = "t"; return Array.prototype.includes.call(o, "t", MAX - 3); });
t("al copyWithin", function () {
  var o = { length: MAX }; o[MAX - 3] = "s";
  Array.prototype.copyWithin.call(o, MAX - 2, MAX - 3, MAX - 2);
  return o[MAX - 2];
});
t("al reduceRight", function () {
  var seen = [], o = { length: Infinity };
  o[MAX - 1] = 1; o[MAX - 2] = 2;
  try {
    Array.prototype.reduceRight.call(o, function (a, v, i) { seen.push(i); if (seen.length == 2) throw 0; return a; }, "");
  } catch (e) {}
  return seen;
});
[P32 - 1, P32, MAX].forEach(function (n) {
  t("al map species length " + n, function () {
    var got;
    var p = new Proxy([], { get: function (t, k) {
      if (k === "length") return n;
      var f = function (m) { got = m; throw 0; };
      f[Symbol.species] = f;
      return f;
    } });
    try { Array.prototype.map.call(p, function () {}); } catch (e) {}
    return got;
  });
});
t("al map species Infinity", function () {
  var got;
  var p = new Proxy([], { get: function (t, k) {
    if (k === "length") return Infinity;
    var f = function (m) { got = m; throw 0; };
    f[Symbol.species] = f;
    return f;
  } });
  try { Array.prototype.map.call(p, function () {}); } catch (e) {}
  return got;
});
[P32, 2 ** 53 + 4].forEach(function (n) {
  t("al splice species deleteCount " + n, function () {
    var got, a = [1, 2];
    a.constructor = {};
    a.constructor[Symbol.species] = function (m) { got = m; throw 0; };
    var p = new Proxy(a, { get: function (t, k, r) { return k === "length" ? 2 ** 53 + 2 : Reflect.get(t, k, r); } });
    try { Array.prototype.splice.call(p, 0, n); } catch (e) {}
    return got;
  });
});
// RegExp: ToLength(lastIndex), then AdvanceStringIndex, written back exactly.
[P32 - 1, P32, MAX, 2 ** 54].forEach(function (n) {
  function rx(flags) {
    var r = new RegExp(".", flags), called = false;
    r.exec = function () {
      if (called) return null;
      called = true;
      r.lastIndex = { valueOf: function () { return n; } };
      return { length: 1, 0: "", index: 0 };
    };
    return r;
  }
  t("replace lastIndex " + n, function () { var r = rx("g"); var s = r[Symbol.replace]("", ""); return [s, r.lastIndex]; });
  t("match lastIndex " + n, function () { var r = rx("g"); r[Symbol.match](""); return r.lastIndex; });
  t("matchAll lastIndex " + n, function () {
    var r = rx("gu");
    r.constructor = {};
    r.constructor[Symbol.species] = function () { return r; };
    r[Symbol.matchAll]("").next();
    return r.lastIndex;
  });
  t("matchAll matcher lastIndex " + n, function () {
    var m, r = /a/g;
    r.constructor = {};
    r.constructor[Symbol.species] = function (src, fl) { m = new RegExp(src, fl); return m; };
    r.lastIndex = n;
    r[Symbol.matchAll]("aaa");
    return m.lastIndex;
  });
});
console.log(out.join("\n"));
"##;

fn node_output(source: &str) -> Vec<String> {
    let out = std::process::Command::new("node")
        .arg("-e")
        .arg(source)
        .output()
        .expect("node on PATH");
    assert!(out.status.success(), "node failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("node output is UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn wide_indices_and_lengths_match_node() {
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    let lines: Vec<String> = out.output.join("\n").lines().map(str::to_owned).collect();
    let node = node_output(SOURCE);
    assert_eq!(lines.len(), node.len(), "row count differs");
    for (z, n) in lines.iter().zip(&node) {
        assert_eq!(z, n);
    }
}
