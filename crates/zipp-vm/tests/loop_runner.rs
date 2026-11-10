//! The interpreter's loop runner (`vm/loop_tier.rs`) against Node: a loop's
//! instructions through handlers that do the number cases and hand anything
//! else back to the dispatch loop, so what a program prints must not depend on
//! which of them ran an instruction. Integer arithmetic at its edges (overflow
//! to doubles, -0, the remainder's sign, division); the bitwise operators on
//! every kind of operand; comparisons and equality as loop conditions and as
//! values; truthiness, loops that test at the bottom, `break` and `continue`;
//! a script's globals (counters, a slot never declared, a binding in its dead
//! zone, a constant); typed arrays of every kind with stores that wrap, round
//! and clamp, reads and writes past the end, a BigInt array, a buffer resized
//! under the loop; arrays with holes over a prototype's element, writes past
//! the end, frozen, with extra properties, with an accessor element, an
//! `arguments` object; properties whose shapes change in the loop, getters and
//! setters, inherited and read-only ones, `length` of every receiver; `push`
//! and `charCodeAt` on the receivers that are theirs and those that are not;
//! captured variables, a captured constant, a dead zone, a loop's cells;
//! `Math` methods on numbers and on what is not one, and after `Math`
//! changes; switches over integers and strings; conversions that run code in
//! a loop's arithmetic, compare and index; loops in a generator, under a
//! `try` that a throw leaves, in recursion, and long enough to spend the
//! runner's budget many times. In the interpreter with the runner entering
//! as it does by itself, from every loop's first back-edge
//! (`ZIPP_LOOP_RUNNER_EAGER`) with and without the lite loop and without
//! frames of its own (`ZIPP_NO_LOOP_FRAMES`), and switched off; by default;
//! and with every function compiled at once.

const SOURCE: &str = r##"var out = [];
function show(tag, v) { out.push(tag + " " + v); }
function name(f) { try { return String(f()); } catch (e) { return "throws " + e.constructor.name; } }
function z(x) { return x === 0 ? (1 / x > 0 ? "+0" : "-0") : String(x); }

// 1. integer arithmetic at its edges: overflow to doubles, -0, the remainder's sign, division
(function () {
  var a = 2147483640, b = -2147483640, m = 1, log = [];
  for (var i = 0; i < 40; i++) { a = a + 1; b = b - 1; m = m * 3; }
  log.push(a, b, m);
  var p = 0, q = 0;
  for (var j = -3; j < 4; j++) { p = j * 0; q = 0 * j; log.push(z(p), z(q), z(j % 2), z(-j % 3), z(j / 2), z(7 % j), z(j / 0)); }
  var n = -2147483647;
  for (var k = 0; k < 3; k++) { n = n - 1; log.push(n, -n, z(-(k - k))); }
  var c = 2147483647;
  for (var t = 0; t < 3; t++) { c++; log.push(c); }
  var d = 0.5;
  for (var u = 0; u < 5; u++) { d += 1; d = d * 2 - 0.25; }
  log.push(d);
  for (var w = 2147483645; w < 2147483650; w++) log.push(w | 0);
  for (var f = 0.5; f < 3; f += 0.5) log.push(f);
  show("1", log.join(","));
})();

// 2. bitwise operators on every kind of operand
(function () {
  var vals = [0, 1, -1, 5, 2147483647, -2147483648, 4294967295, 1.9, -1.9, NaN, Infinity, "7", "x", null, undefined, true, 1e21, -0];
  var log = [];
  for (var i = 0; i < vals.length; i++) {
    var v = vals[i];
    log.push((v | 0) + "/" + (v & 255) + "/" + (v ^ 1) + "/" + (v << 33) + "/" + (v >> 1) + "/" + (v >>> 28) + "/" + (v >>> 0) + "/" + (1 << v));
  }
  var s = 0x9E3779B9 | 0, acc = 0;
  for (var r = 0; r < 500; r++) { s ^= s << 13; s ^= s >>> 17; s ^= s << 5; acc = (acc + (s >>> 3) + (s & 255)) | 0; }
  log.push(s, acc);
  show("2", log.join(" "));
})();

// 3. comparisons and equality as loop conditions and as values
(function () {
  var vals = [0, -0, 1, 1.5, NaN, "1", "a", "ab", null, undefined, true, false, 10, "10", 9];
  var log = [];
  for (var i = 0; i < vals.length; i++) {
    var row = "";
    for (var j = 0; j < vals.length; j++) {
      var x = vals[i], y = vals[j];
      row += (x < y ? "l" : ".") + (x <= y ? "L" : ".") + (x > y ? "g" : ".") + (x >= y ? "G" : ".") + (x === y ? "e" : ".") + (x !== y ? "n" : ".") + " ";
    }
    log.push(row);
  }
  var c = 0;
  for (var k = 0; k < NaN; k++) c++;
  for (var k2 = 0; k2 < "5"; k2++) c++;
  for (var k3 = 0; k3 < 4.5; k3++) c++;
  for (var k4 = 0; k4 <= 3; k4++) c++;
  var lim = { valueOf: function () { c += 100; return 3; } };
  for (var k5 = 0; k5 < lim; k5++) c++;
  var s1 = "ab", s2 = "a";
  for (var k6 = 0; k6 < 3; k6++) { if (s1 === s2 + "b") c += 1000; if (s1 !== s2 + "b") c += 5000; }
  log.push(c);
  show("3", log.join("|"));
})();

// 4. truthiness of every kind of value, loops that test at the bottom, break and continue
(function () {
  var vals = [0, -0, NaN, 1, "", "0", "a", null, undefined, {}, [], true, false, 0n, 1n, 1e-320];
  var log = [], t = 0;
  for (var i = 0; i < vals.length; i++) { if (vals[i]) t++; if (!vals[i]) t += 100; log.push(!vals[i] ? "F" : "T"); }
  log.push(t);
  var n = 0, k = 10;
  do { n += k; k--; } while (k);
  log.push(n);
  var m = 0, q = 5;
  do { m++; q -= 1; } while (q > 0);
  log.push(m);
  var c = 0;
  outer: for (var a = 0; a < 20; a++) {
    for (var b = 0; b < 20; b++) {
      if (b === 3) continue;
      if (b > a) continue outer;
      if (a === 15) break outer;
      c += b;
    }
  }
  log.push(c);
  var w = 0, x = 0;
  while (true) { w++; if (w > 50) break; if (w & 1) continue; x += w; }
  log.push(x);
  show("4", log.join(","));
})();

// 5. a script's globals: counters, a slot never declared, a binding in its dead zone, a read-only global
var g1 = 0, g2 = 1.5, g3 = "s";
for (var gi = 0; gi < 300; gi++) { g1 = (g1 + gi) | 0; g2 = g2 + 0.5; g3 = gi & 1; }
show("5 counters", g1 + "," + g2 + "," + g3 + "," + gi);
for (var gj = 0; gj < 40; gj++) { undeclared = gj * 2; }
show("5 undeclared", undeclared);
show("5 dead zone", name(function () { for (var q = 0; q < 40; q++) { late = q; } return late; }));
let late = 7;
for (var gk = 0; gk < 40; gk++) { late = late + gk; }
show("5 let", late);
const fixed = 3;
show("5 const", name(function () { for (var q = 0; q < 40; q++) { fixed = q; } return fixed; }));
var gsum = 0;
for (var gl = 0; gl < 100; gl++) { gsum += gl; if (gl === 60) gl += 5; }
show("5 skip", gsum + "," + gl);
var gd = 2147483640;
for (var gm = 0; gm < 20; gm++) { gd++; }
show("5 overflow", gd);
var gstr = "";
for (var gn = 0; gn < 5; gn++) { gstr += gn; }
show("5 string", gstr);

// 6. typed arrays of every kind: stores that wrap, round and clamp, reads and writes past the end
(function () {
  var kinds = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array];
  var vals = [0, 1, -1, 127, 128, 255, 256, 300.7, -300.7, 65535, 65536, 2147483647, 2147483648, 4294967295, 4294967296, 1.5, 2.5, -0, NaN, Infinity, 1e40, 0.1];
  var log = [];
  for (var k = 0; k < kinds.length; k++) {
    var ta = new kinds[k](vals.length);
    for (var i = 0; i < vals.length; i++) ta[i] = vals[i];
    var s = "";
    for (var j = 0; j < vals.length + 2; j++) s += z(ta[j]) + ";";
    ta[vals.length] = 5; ta[-1] = 6; ta[1.5] = 7;
    log.push(s + ta.length + ";" + ta[-1] + ";" + ta[1.5] + ";" + ta["1"]);
  }
  var f = new Float64Array(64), acc = 0;
  for (var a = 0; a < 2000; a++) { f[a & 63] = f[(a + 1) & 63] * 0.5 + a; acc += f[a & 63]; }
  log.push(acc);
  var big = new BigInt64Array(4);
  for (var b = 0; b < 40; b++) big[b & 3] = BigInt(b);
  log.push(big.join("."));
  var rab = new ArrayBuffer(8, { maxByteLength: 64 }), tr = new Uint8Array(rab), seen = [];
  for (var r = 0; r < 12; r++) { tr[r] = r + 1; seen.push(tr[r]); if (r === 5) rab.resize(16); if (r === 9) rab.resize(4); }
  log.push(seen.join("."));
  var u = new Uint8Array(8);
  for (var v = 0; v < 40; v++) { u[v & 7] = "9"; u[(v + 1) & 7] = true; u[(v + 2) & 7] = null; }
  log.push(u.join("."));
  show("6", log.join("|"));
})();

// 7. arrays: holes, the prototype behind them, writes past the end, frozen, extra properties, arguments
(function () {
  var log = [];
  var a = [1, 2, , 4, 5];
  Array.prototype[2] = "proto";
  var s = "";
  for (var i = 0; i < 8; i++) s += a[i] + ";";
  delete Array.prototype[2];
  for (var j = 0; j < 8; j++) s += a[j] + ";";
  log.push(s);
  var w = [0, 0, 0];
  for (var k = 0; k < 10; k++) w[k] = k * k;
  w[2] = "x"; w[-1] = "neg"; w[1.5] = "frac";
  log.push(w.join(".") + "/" + w.length + "/" + w[-1] + "/" + w[1.5]);
  var fz = Object.freeze([1, 2, 3]);
  for (var m = 0; m < 5; m++) fz[m] = 9;
  log.push(fz.join("."));
  var strict = (function () { "use strict"; return name(function () { for (var m2 = 0; m2 < 5; m2++) fz[m2] = 9; return "wrote"; }); })();
  log.push(strict);
  var meta = [10, 20, 30]; meta.tag = "t";
  var ms = 0;
  for (var n = 0; n < 60; n++) { ms += meta[n % 3]; meta[n % 3] = ms & 7; }
  log.push(ms + "/" + meta.join(".") + "/" + meta.tag);
  var acc = [5, 6, 7];
  Object.defineProperty(acc, 1, { get: function () { return "got"; }, set: function (v) { log.push("set" + v); } });
  for (var p = 0; p < 3; p++) { log.push(acc[p]); acc[p] = p; }
  function args() { var t = 0; for (var q = 0; q < arguments.length; q++) { t += arguments[q]; arguments[q] = 1; } return t + "," + arguments[0] + "," + arguments.length; }
  log.push(args(3, 4, 5));
  var grown = [];
  for (var g = 0; g < 100; g++) grown.push(g * 2);
  var gs = 0;
  for (var h = 0; h < grown.length; h++) gs += grown[h];
  log.push(gs + "/" + grown.length);
  var sparse = []; sparse[50] = 1;
  var sc = 0;
  for (var x = 0; x < sparse.length; x++) if (sparse[x] === undefined) sc++;
  log.push(sc);
  var objs = [{ v: 1 }, { v: 2 }], os = 0;
  for (var y = 0; y < 100; y++) { var o = objs[y & 1]; os += o.v; objs[y & 1] = objs[(y + 1) & 1]; }
  log.push(os);
  show("7", log.join("|"));
})();

// 8. properties: shapes that change in the loop, getters and setters, the prototype, a read-only one
(function () {
  var log = [];
  var o = { a: 1, b: 2 };
  for (var i = 0; i < 100; i++) { o.a = o.a + o.b; if (i === 40) o.c = 5; if (i === 60) delete o.b; if (i === 70) o.b = 3; }
  log.push(o.a + "/" + o.b + "/" + o.c);
  var calls = 0;
  var acc = { get x() { calls++; return 7; }, set y(v) { calls += v; } };
  var s = 0;
  for (var j = 0; j < 50; j++) { s += acc.x; acc.y = 2; }
  log.push(s + "/" + calls);
  function P() { this.own = 1; }
  P.prototype.inherited = 10;
  var p = new P(), t = 0;
  for (var k = 0; k < 50; k++) { t += p.own + p.inherited; if (k === 25) P.prototype.inherited = 20; if (k === 40) p.inherited = 1; }
  log.push(t);
  var ro = Object.freeze({ v: 1 });
  for (var m = 0; m < 20; m++) ro.v = m;
  log.push(ro.v);
  var shapes = [{ v: 1 }, { w: 0, v: 2 }, { v: 3, z: 0 }, Object.create({ v: 4 })], sv = 0;
  for (var n = 0; n < 200; n++) sv += shapes[n & 3].v;
  log.push(sv);
  var nul = [{ v: 1 }, null], nv = 0;
  log.push(name(function () { for (var q = 0; q < 10; q++) nv += nul[q & 1].v; return nv; }));
  var lens = [[1, 2, 3], "four", new Uint8Array(5), { length: 6 }, function (a, b) {}, [], ""], ls = "";
  for (var r = 0; r < lens.length; r++) ls += lens[r].length;
  log.push(ls);
  var grow = [1], gl = 0;
  for (var u = 0; u < grow.length && u < 30; u++) { grow.push(u); gl += grow.length; }
  log.push(gl);
  show("8", log.join("|"));
})();

// 9. push and charCodeAt at their fast sites, on the receivers that are not theirs
(function () {
  var log = [];
  var arr = [], r = 0;
  for (var i = 0; i < 50; i++) r += arr.push(i);
  log.push(r + "/" + arr.length);
  class Stack extends Array { push(v) { return super.push(v * 2) + 100; } }
  var st = new Stack(), sr = 0;
  for (var j = 0; j < 20; j++) sr += st.push(j);
  log.push(sr + "/" + st[19]);
  var own = { n: 0, push: function (v) { this.n += v; return "p"; } }, os = "";
  for (var k = 0; k < 5; k++) os += own.push(k);
  log.push(os + own.n);
  var fz = Object.freeze([1]);
  log.push(name(function () { for (var q = 0; q < 3; q++) fz.push(q); return "pushed"; }));
  var s = "héllo wörld", cs = 0, cc = [];
  for (var m = 0; m < s.length + 2; m++) { var code = s.charCodeAt(m); if (code === code) cs += code; else cc.push(m); }
  log.push(cs + "/" + cc.join("."));
  var so = new String("abc"), ss = 0;
  for (var n = 0; n < 3; n++) ss += so.charCodeAt(n) + "abc".charCodeAt(n + 0.5) + "abc".charCodeAt("1");
  log.push(ss);
  var saved = Array.prototype.push;
  var pa = [], pl = [];
  for (var t = 0; t < 6; t++) { if (t === 3) Array.prototype.push = function () { return "patched"; }; pl.push.call(pl, 0); log.push(pa.push(t)); }
  Array.prototype.push = saved;
  show("9", log.join("|"));
})();

// 10. closures: captured variables read and written in loops, a constant, a dead zone, cells of a loop
(function () {
  var log = [];
  var c = 0, d = 1.5;
  function bump() { for (var i = 0; i < 100; i++) { c = (c + i) | 0; d = d + c; } return c; }
  bump(); bump();
  log.push(c + "/" + d);
  const k = 5;
  function clobber() { for (var i = 0; i < 5; i++) { k = i; } }
  log.push(name(clobber));
  function early() { for (var i = 0; i < 5; i++) { v = i; } return v; }
  log.push(name(early));
  let v = 9;
  log.push(early());
  var fns = [];
  for (let i = 0; i < 5; i++) { let j = i * 2; fns.push(function () { j += i; return j; }); }
  var fs = 0;
  for (var q = 0; q < 20; q++) fs += fns[q % 5]();
  log.push(fs);
  var self = function me() { for (var i = 0; i < 3; i++) { me = 1; } return typeof me; };
  log.push(self());
  function counter() { var n = 0; return function () { for (var i = 0; i < 10; i++) n++; return n; }; }
  var ct = counter(); ct(); ct();
  log.push(ct());
  show("10", log.join("|"));
})();

// 11. Math methods on numbers and on what is not one, and after Math changes
(function () {
  var log = [];
  var vals = [0, -0, 1.5, -1.5, 2147483647, -2147483648, NaN, Infinity, "4", null, undefined, 1e300];
  for (var i = 0; i < vals.length; i++) {
    var v = vals[i];
    log.push(z(Math.floor(v)) + "/" + z(Math.abs(v)) + "/" + z(Math.imul(v, 3)) + "/" + z(Math.min(v, 1)) + "/" + z(Math.max(v, 1)) + "/" + z(Math.sqrt(v)) + "/" + z(Math.round(v)) + "/" + z(Math.trunc(v)));
  }
  var h = 0;
  for (var j = 0; j < 500; j++) h = Math.imul(h ^ j, 2654435761) | 0;
  log.push(h);
  var saved = Math.floor, f = 0;
  for (var k = 0; k < 10; k++) { if (k === 5) Math.floor = function () { return 100; }; f += Math.floor(k / 2); }
  Math.floor = saved;
  log.push(f);
  show("11", log.join("|"));
})();

// 12. switches over integers and strings inside loops
(function () {
  var log = [];
  var keys = [0, 1, 2, 3, 4, 5, 6, 7, 8, 1.0, 2.5, "3", NaN, -0, null, undefined, true];
  var t = 0;
  for (var i = 0; i < keys.length * 3; i++) {
    switch (keys[i % keys.length]) {
      case 0: t += 1; break;
      case 1: t += 10; break;
      case 2: t += 100;
      case 3: t += 1000; break;
      case 4: t += 2; break;
      case 5: t += 3; break;
      case 6: t += 4; break;
      case 7: t += 5; break;
      default: t += 7;
    }
  }
  log.push(t);
  var names = ["add", "sub", "mul", "div", "mod", "neg", "x", "add", 3, null, "ad" + "d"], u = 0;
  for (var j = 0; j < names.length * 3; j++) {
    switch (names[j % names.length]) {
      case "add": u += 1; break;
      case "sub": u += 10; break;
      case "mul": u += 100; break;
      case "div": u += 1000; break;
      case "mod": u += 10000; break;
      case "neg": u += 100000; break;
      default: u += 3;
    }
  }
  log.push(u);
  show("12", log.join("|"));
})();

// 13. values with conversions that run code, in a loop's arithmetic, compare and index
(function () {
  var log = [], n = 0;
  var obj = { valueOf: function () { n++; return n; }, toString: function () { return "str"; } };
  var s = 0;
  for (var i = 0; i < 10; i++) { s += obj * 2 + (obj | 0) + (obj < 100 ? 1 : 0); }
  log.push(s + "/" + n);
  var arr = [5, 6, 7], key = { toString: function () { n += 100; return "1"; } }, ks = 0;
  for (var j = 0; j < 5; j++) { ks += arr[key]; arr[key] = j; }
  log.push(ks + "/" + n + "/" + arr.join("."));
  var d = new Date(0), ds = 0;
  for (var k = 0; k < 3; k++) ds += d - k;
  log.push(ds);
  var sym = Symbol("s");
  log.push(name(function () { var t = 0; for (var q = 0; q < 3; q++) t += sym; return t; }));
  var bi = 0n;
  for (var m = 0; m < 50; m++) { bi = bi + 2n; }
  log.push(bi + "/" + name(function () { var t = 0; for (var q = 0; q < 3; q++) t += 1n; return t; }));
  var str = "";
  for (var p = 0; p < 20; p++) { str = str + p; if (str.length > 12) str = str.slice(6); }
  log.push(str);
  show("13", log.join("|"));
})();

// 14. loops in generators, in a `try` with a throw from inside, in recursion, and long ones
(function () {
  var log = [];
  function* gen() { var t = 0; for (var i = 0; i < 100; i++) { t += i; if (i % 40 === 39) yield t; } for (var j = 0; j < 100; j++) t ^= j; return t; }
  var g = gen(), r, gs = [];
  while (!(r = g.next()).done) gs.push(r.value);
  log.push(gs.join(".") + "." + r.value);
  var caught = 0, fin = 0;
  for (var a = 0; a < 30; a++) {
    try { for (var b = 0; b < 10; b++) { if (b === 5 && (a & 3) === 0) throw new Error("x"); caught += b; } }
    catch (e) { caught -= 100; }
    finally { fin++; }
  }
  log.push(caught + "/" + fin);
  function rec(n) { var t = 0; for (var i = 0; i < 50; i++) t = (t + i * n) | 0; return n === 0 ? t : t + rec(n - 1); }
  log.push(rec(30));
  var big = 0;
  for (var c = 0; c < 300000; c++) { big = (big + (c ^ (c >>> 3))) | 0; }
  log.push(big);
  var junk = [], jl = 0;
  for (var d = 0; d < 20000; d++) { junk[d & 1023] = "s" + d; jl = (jl + junk[d & 1023].length) | 0; }
  log.push(jl);
  var nested = 0;
  for (var x = 0; x < 200; x++) for (var y = 0; y < 200; y++) nested = (nested + (x ^ y)) | 0;
  log.push(nested);
  show("14", log.join("|"));
})();

console.log(out.join("\n"));
"##;

const EXPECTED: &str = "1 2147483680,-2147483680,12157665459056929000,-0,-0,-1,+0,-1.5,1,-Infinity,-0,-0,-0,2,-1,1,-Infinity,-0,-0,-1,1,-0.5,+0,-Infinity,+0,+0,+0,-0,+0,NaN,NaN,+0,+0,1,-1,0.5,+0,Infinity,+0,+0,+0,-2,1,1,Infinity,+0,+0,1,-0,1.5,1,Infinity,-2147483648,2147483648,-0,-2147483649,2147483649,-0,-2147483650,2147483650,-0,2147483648,2147483649,2147483650,70.25,2147483645,2147483646,2147483647,-2147483648,-2147483647,0.5,1,1.5,2,2.5\n2 0/0/1/0/0/0/0/1 1/1/0/2/0/0/1/2 -1/255/-2/-2/-1/15/4294967295/-2147483648 5/5/4/10/2/0/5/32 2147483647/255/2147483646/-2/1073741823/7/2147483647/-2147483648 -2147483648/0/-2147483647/0/-1073741824/8/2147483648/1 -1/255/-2/-2/-1/15/4294967295/-2147483648 1/1/0/2/0/0/1/2 -1/255/-2/-2/-1/15/4294967295/-2147483648 0/0/1/0/0/0/0/1 0/0/1/0/0/0/0/1 7/7/6/14/3/0/7/128 0/0/1/0/0/0/0/1 0/0/1/0/0/0/0/1 0/0/1/0/0/0/0/1 1/1/0/2/0/0/1/2 -559939584/0/-559939583/-1119879168/-279969792/13/3735027712/1 0/0/1/0/0/0/0/1 -580680600 -103409484\n3 .L.Ge. .L.Ge. lL...n lL...n .....n lL...n .....n .....n .L.G.n .....n lL...n .L.G.n lL...n lL...n lL...n |.L.Ge. .L.Ge. lL...n lL...n .....n lL...n .....n .....n .L.G.n .....n lL...n .L.G.n lL...n lL...n lL...n |..gG.n ..gG.n .L.Ge. lL...n .....n .L.G.n .....n .....n ..gG.n .....n .L.G.n ..gG.n lL...n lL...n lL...n |..gG.n ..gG.n ..gG.n .L.Ge. .....n ..gG.n .....n .....n ..gG.n .....n ..gG.n ..gG.n lL...n lL...n lL...n |.....n .....n .....n .....n .....n .....n .....n .....n .....n .....n .....n .....n .....n .....n .....n |..gG.n ..gG.n .L.G.n lL...n .....n .L.Ge. lL...n lL...n ..gG.n .....n .L.G.n ..gG.n lL...n lL...n lL...n |.....n .....n .....n .....n .....n ..gG.n .L.Ge. lL...n .....n .....n .....n .....n .....n ..gG.n .....n |.....n .....n .....n .....n .....n ..gG.n ..gG.n .L.Ge. .....n .....n .....n .....n .....n ..gG.n .....n |.L.G.n .L.G.n lL...n lL...n .....n lL...n .....n .....n .L.Ge. .....n lL...n .L.G.n lL...n lL...n lL...n |.....n .....n .....n .....n .....n .....n .....n .....n .....n ....e. .....n .....n .....n .....n .....n |..gG.n ..gG.n .L.G.n lL...n .....n .L.G.n .....n .....n ..gG.n .....n .L.Ge. ..gG.n lL...n lL...n lL...n |.L.G.n .L.G.n lL...n lL...n .....n lL...n .....n .....n .L.G.n .....n lL...n .L.Ge. lL...n lL...n lL...n |..gG.n ..gG.n ..gG.n ..gG.n .....n ..gG.n .....n .....n ..gG.n .....n ..gG.n ..gG.n .L.Ge. .L.G.n ..gG.n |..gG.n ..gG.n ..gG.n ..gG.n .....n ..gG.n lL...n lL...n ..gG.n .....n ..gG.n ..gG.n .L.G.n .L.Ge. ..gG.n |..gG.n ..gG.n ..gG.n ..gG.n .....n ..gG.n .....n .....n ..gG.n .....n ..gG.n ..gG.n lL...n lL...n .L.Ge. |3417\n4 F,F,F,T,F,T,T,F,F,T,T,T,F,F,T,T,808,55,5,524,650\n5 counters 44850,151.5,1,300\n5 undeclared 78\n5 dead zone throws ReferenceError\n5 let 787\n5 const throws TypeError\n5 skip 4635,100\n5 overflow 2147483660\n5 string 01234\n6 +0;1;-1;127;-128;-1;+0;44;-44;-1;+0;-1;+0;-1;+0;1;2;+0;+0;+0;+0;+0;undefined;undefined;22;undefined;undefined;1|+0;1;255;127;128;255;+0;44;212;255;+0;255;+0;255;+0;1;2;+0;+0;+0;+0;+0;undefined;undefined;22;undefined;undefined;1|+0;1;+0;127;128;255;255;255;+0;255;255;255;255;255;255;2;2;+0;+0;255;255;+0;undefined;undefined;22;undefined;undefined;1|+0;1;-1;127;128;255;256;300;-300;-1;+0;-1;+0;-1;+0;1;2;+0;+0;+0;+0;+0;undefined;undefined;22;undefined;undefined;1|+0;1;65535;127;128;255;256;300;65236;65535;+0;65535;+0;65535;+0;1;2;+0;+0;+0;+0;+0;undefined;undefined;22;undefined;undefined;1|+0;1;-1;127;128;255;256;300;-300;65535;65536;2147483647;-2147483648;-1;+0;1;2;+0;+0;+0;+0;+0;undefined;undefined;22;undefined;undefined;1|+0;1;4294967295;127;128;255;256;300;4294966996;65535;65536;2147483647;2147483648;4294967295;+0;1;2;+0;+0;+0;+0;+0;undefined;undefined;22;undefined;undefined;1|+0;1;-1;127;128;255;256;300.70001220703125;-300.70001220703125;65535;65536;2147483648;2147483648;4294967296;4294967296;1.5;2.5;-0;NaN;Infinity;Infinity;0.10000000149011612;undefined;undefined;22;undefined;undefined;1|+0;1;-1;127;128;255;256;300.7;-300.7;65535;65536;2147483647;2147483648;4294967295;4294967296;1.5;2.5;-0;NaN;Infinity;1e+40;0.1;undefined;undefined;22;undefined;undefined;1|3757969.9999966803|36.37.38.39|1.2.3.4.5.6.7.8.9.10..|1.0.9.9.9.9.9.9\n7 1;2;proto;4;5;undefined;undefined;undefined;1;2;undefined;4;5;undefined;undefined;undefined;|0.1.x.9.16.25.36.49.64.81/10/neg/frac|1.2.3|throws TypeError|254/6.4.6/t|5|got|set1|7|12,1,3|9900/100|50|199\n8 NaN/3/5|350/150|619|1|500|throws TypeError|3456200|495\n9 1275/50|2210/38|ppppp10|throws TypeError|1383/11.12|882|1|2|3\n10 9900/828301.5|throws TypeError|throws ReferenceError|4|180|function|30\n11 +0/+0/+0/+0/1/+0/+0/+0|-0/+0/+0/-0/1/-0/-0/-0|1/1.5/3/1/1.5/1.224744871391589/2/1|-2/1.5/-3/-1.5/1/NaN/-1/-1|2147483647/2147483647/2147483645/1/2147483647/46340.950001051984/2147483647/2147483647|-2147483648/2147483648/-2147483648/-2147483648/1/NaN/-2147483648/-2147483648|NaN/NaN/+0/NaN/NaN/NaN/NaN/NaN|Infinity/Infinity/+0/1/Infinity/Infinity/Infinity/Infinity|4/4/12/1/4/2/4/4|+0/+0/+0/+0/1/+0/+0/+0|NaN/NaN/+0/NaN/NaN/NaN/NaN/NaN|1e+300/1e+300/+0/1/1e+300/1e+150/1e+300/1e+300|1291466784|504\n12 6555|333366\n13 455/30|12/1030/5.4.7|-3|throws TypeError|100/throws TypeError|141516171819\n14 780.3160.4950|270/30|569625|-1325038576|108890|4797152";

/// Every switch a mode below may set, cleared before each run.
const SWITCHES: [&str; 6] = [
    "ZIPP_NOJIT",
    "ZIPP_JIT_THRESHOLD",
    "ZIPP_LOOP_RUNNER_EAGER",
    "ZIPP_NO_LOOP_RUNNER",
    "ZIPP_NO_LOOP_FRAMES",
    "ZIPP_NO_LITE",
];

#[test]
fn loop_runner_worker() {
    if std::env::var_os("ZIPP_LOOP_RUNNER_WORKER").is_none() {
        return;
    }
    let out = zipp_vm::run(SOURCE).expect("source compiles");
    assert!(out.error.is_none(), "unexpected runtime error: {:?}", out.error);
    assert_eq!(out.output.join("\n"), EXPECTED);
}

#[test]
fn loop_runner_match_node() {
    if std::env::var_os("ZIPP_LOOP_RUNNER_WORKER").is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test executable");
    let modes: [(&str, &[&str]); 7] = [
        ("default", &[]),
        ("interpreter", &["ZIPP_NOJIT"]),
        ("interpreter, runner from the first back-edge", &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER"]),
        (
            "interpreter, runner from the first back-edge, no lite loop",
            &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER", "ZIPP_NO_LITE"],
        ),
        (
            "interpreter, runner from the first back-edge, no frame of its own",
            &["ZIPP_NOJIT", "ZIPP_LOOP_RUNNER_EAGER", "ZIPP_NO_LOOP_FRAMES"],
        ),
        ("interpreter, no runner", &["ZIPP_NOJIT", "ZIPP_NO_LOOP_RUNNER"]),
        ("compile-at-once", &["ZIPP_JIT_THRESHOLD"]),
    ];
    for (mode, vars) in modes {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["loop_runner_worker", "--exact", "--nocapture"])
            .env("ZIPP_LOOP_RUNNER_WORKER", "1");
        for v in SWITCHES {
            command.env_remove(v);
        }
        for v in vars {
            command.env(v, "1");
        }
        let out = command.output().expect("spawn the worker");
        assert!(
            out.status.success(),
            "{mode} diverged from node:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
