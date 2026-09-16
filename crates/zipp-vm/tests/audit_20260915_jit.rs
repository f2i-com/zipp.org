//! JIT-only wrong answers from the 15 September 2026 review (track E-jit).
//! Every probe below printed the node answer under `ZIPP_NOJIT=1` and a
//! different one once a tier compiled it; each is now pinned to node (v24) in
//! a clean child process under the default thresholds, the interpreter, and
//! forced compilation (`ZIPP_JIT_THRESHOLD=1`).
//!
//! * R073 `nested_leaf` — a wrapper spliced with its inner leaf re-ran the
//!   inner's global write when a later wrapper op bailed (an LCG seed advanced
//!   once per bail).
//! * R076 `async_callbacks` — map/filter/reduce kernels and Tier A (sort
//!   comparators, native callbacks) ran async/generator callbacks as plain
//!   functions and handed back the raw completion value.
//! * R077 `kernel_mod` — the fused kernels computed `%` as `a - trunc(a/b)*b`
//!   (lost bits on large quotients, NaN for `x % Infinity`, +0 for -0).
//! * R078 `callback_this` — native callback windows skipped sloppy-mode `this`
//!   binding (no global object, no primitive wrapper).
//! * R082 `hoisted_length` — the MEM tier's hoisted `g.length` went stale when
//!   a getter, `valueOf` or `toString` run by an admitted op grew the array.
//! * R083/R084 `continue_length` — both length hoists published the length on
//!   a `while` loop whose `continue` skipped the read before region entry.
//! * R085 `dead_reads` — a discarded pinned dense-Array read was dead-code
//!   eliminated, so an inherited getter or Proxy trap never ran.
//! * R087 `captured_callee` — a captured `Math.imul`/`charCodeAt` callee or
//!   receiver in a recycled register was dead-code eliminated; a guard exit
//!   between the lookup and the call then ran a stale value.
//! * R088 `negative_zero_mul` — `0 * -5` is -0, which no i64 home can hold:
//!   the interpreter, both INT tiers and Tier A all produced +0 while the
//!   DOUBLE/MEM tiers produced -0, so the answer depended on the tier.
//! * R089 `frozen_store*` — `Object.freeze` did not dirty the Array snapshot
//!   epoch, so an inline dense store kept writing through a frozen array.
//! * R159 `private_brand*` — a hot `o.#m()` site looked `#m` up as a plain
//!   property and called another class's same-named private method (and could
//!   not call a private field or getter holding a function at all).

const CHILD_ENV: &str = "ZIPP_AUDIT_20260915_JIT_CHILD";

struct Probe {
    name: &'static str,
    src: &'static str,
    expected: &'static [&'static str],
}

const PROBES: &[Probe] = &[
    Probe {
        name: "nested_leaf",
        src: r##"
"use strict";
var seed = 1;
function next() { var v = (seed * 48271) % 2147483647; seed = v; return v; }
function scaled(x) { return next() * x; }
var acc = 0;
for (var i = 0; i < 60000; i++) {
  var x = (i % 5000 === 4999) ? "3" : 3;
  acc = (acc + scaled(x)) % 1000003;
}
console.log("seed", seed, "acc", acc);
var steps = 0, probe = 1;
while (probe !== seed && steps < 70000) { probe = (probe * 48271) % 2147483647; steps++; }
console.log("steps", steps);
var gets = 0;
class Sized { get size() { gets++; return 7; } }
function pick(o) { return next() % o.size; }
var plain = { size: 7 }, odd = new Sized();
seed = 1;
var hist = 0;
for (var j = 0; j < 60000; j++) hist = (hist + pick(j % 1000 === 999 ? odd : plain)) % 1000003;
console.log("seed", seed, "hist", hist, "getter", gets);
var cnt = 0;
function bump() { var v = cnt + 1; cnt = v; return v; }
function twice(y) { return bump() * y; }
var t = 0;
for (var k = 0; k < 40000; k++) t = (t + twice(k % 625 === 624 ? "2" : 2)) % 1000003;
console.log("cnt", cnt, "t", t);
// An upvalue write is buffered to the end of the flat body, so this splice
// stays admitted and a bail after it re-runs nothing twice.
function mk() { let c = 0; return function up() { c = c + 1; return c; }; }
var up = mk();
function twiceUp(y) { return up() * y; }
var u = 0;
for (var m = 0; m < 40000; m++) u = (u + twiceUp(m % 625 === 624 ? "2" : 2)) % 1000003;
console.log("u", u, "next", up());
"##,
        expected: &[
            "seed 182525745 acc 787440",
            "steps 60000",
            "seed 182525745 hist 179832 getter 60",
            "cnt 40000 t 35200",
            "u 35200 next 40001",
        ],
    },
    Probe {
        name: "async_callbacks",
        src: r##"
console.log([1, 2, 3].filter(async x => x < 2));
console.log(typeof [1, 2, 3].map(async x => x + 1)[0]);
const a = [3, 1, 2]; a.sort(async (x, y) => x - y); console.log(a.join());
console.log(Object.prototype.toString.call([1, 2, 3].reduce(async (s, x) => s + x, 0)));
const b = []; for (let i = 0; i < 2000; i++) b.push(i);
console.log(b.filter(async x => x > 1000).length);
console.log(Object.prototype.toString.call(b.reduce(async (s, x) => s + x, 0)));
console.log(b.map(async x => x * 2).every(p => p instanceof Promise));
function* gen(x) { return x + 1; }
console.log(Object.prototype.toString.call([1, 2].map(gen)[0]));
async function named(x, y) { return x - y; }
const s = [3, 1, 2]; s.sort(named); console.log(s.join());
console.log([1, 2, 3].map(async function (x) { return x * 2; }).map(p => typeof p).join());
Promise.all([1, 2].map(async x => x + 1)).then(v => console.log("all", v.join()));
"##,
        expected: &[
            "[ 1, 2, 3 ]",
            "object",
            "3,1,2",
            "[object Promise]",
            "2000",
            "[object Promise]",
            "true",
            "[object Generator]",
            "3,1,2",
            "object,object,object",
            "all 2,3",
        ],
    },
    Probe {
        name: "kernel_mod",
        src: r##"
const show = v => Object.is(v, -0) ? "-0" : String(v);
console.log([1e17, 2 ** 60, 123456789012345680].map(x => x % 3).join());
console.log([-4, -6, -0].map(x => x % 2).map(show).join());
console.log([10, 3, Infinity].reduce((a, e) => a % e));
console.log([5.5, 1.1].reduce((a, e) => a % e));
console.log([5, -4, 7.5].map(x => x % (1 / 0)).join());
console.log([-6, 6, -9].map(x => 1 / (x % 3)).join());
console.log([1, 2, 3, 4, 5, 6].filter(x => x % 3 === 0).join());
console.log([7, -7, 2147483647, -2147483648, 0].map(x => x % 10).map(show).join());
console.log([-2147483648, 5, -5].map(x => x % -1).map(show).join());
console.log([2 ** 53, -(2 ** 62), 1e300, -1e300].map(x => x % 7).join());
console.log([NaN, Infinity, -Infinity, 4].map(x => x % 2).join());
console.log([4, -4].map(x => x % NaN).join(), [4, -4].map(x => x % -Infinity).map(show).join());
console.log([1.5, 2.25, -3.75].map(x => x % 1).map(show).join());
console.log([12, 13, 14].map((x, i) => x % (i + 2)).join());
const big = []; for (let i = 0; i < 5000; i++) big.push(i * 1000003 - 2500000000);
console.log(big.map(x => x % 97).reduce((a, e) => a + e, 0));
console.log(big.filter(x => x % 7 === 0).length, big.reduce((a, e) => (a + e) % 1000, 0));
console.log([5.5, -5.5, 1e300, -1e300, -0.5, 7.25].map(x => x % 2.5).map(show).join());
console.log([1e308, 2 ** 1000 + 2 ** 960, 5e-324, -5e-324].map(x => x % 3).map(show).join());
console.log([1, -1, 1e-300].map(x => x % 5e-324).map(show).join(), [7, -7, 0].map(x => x % 0).join());
console.log([3.75, -3.75].map((x, i) => x % (i + 0.5)).map(show).join());
const fr = []; for (let i = 0; i < 3000; i++) fr.push(i * 0.37 - 555.5);
console.log(fr.map(x => x % 1.25).reduce((a, e) => a + e, 0).toFixed(9), fr.filter(x => x % 2 < -1).length);
// A kernel's own `x * 2` stores doubles; exactly-i32 ones take the integer
// divide, and -0, `% -1` and out-of-range values still come out right.
const dd = [-2, 2, 3.5, -3.5, 1073741823.5, -1073741824, 1500000000, -0, 0.25].map(x => x * 2);
console.log(dd.map(x => x % 3).map(show).join(), dd.map(x => x % -2).map(show).join());
console.log(dd.map(x => x % -1).map(show).join(), dd.map((x, i) => x % (i * 2 - 3)).map(show).join());
const odd = []; for (let i = 0; i < 4000; i++) odd.push(i - 2000.5);
const tw = odd.map(x => x * 2);
console.log(tw.map(x => x % 7).reduce((a, e) => a + e, 0), tw.filter(x => x % 4 === -1).length, tw.map((x, i) => x % (i * 0.5 + 1)).reduce((a, e) => a + e, 0));
"##,
        expected: &[
            "1,1,2",
            "-0,-0,-0",
            "1",
            "1.0999999999999996",
            "5,-4,7.5",
            "-Infinity,Infinity,-Infinity",
            "3,6",
            "7,-7,7,-8,0",
            "-0,0,-0",
            "4,-4,1,-1",
            "NaN,NaN,NaN,0",
            "NaN,NaN 4,-4",
            "0.5,0.25,-0.75",
            "0,1,2",
            "-25",
            "714 500",
            "0.5,-0.5,0,-0,-0.5,2.25",
            "2,2,5e-324,-5e-324",
            "0,-0,0 NaN,NaN,NaN",
            "0.25,-0.75",
            "-2.500000000 743",
            "-1,1,1,-1,1,-2,0,-0,0.5 -0,0,1,-1,1,-0,0,-0,0.5",
            "-0,0,0,-0,0,-0,0,-0,0.5 -1,0,0,-1,2,-2,3,-0,0.5",
            "-6 1001 1317626",
        ],
    },
    Probe {
        name: "callback_this",
        src: r##"
console.log([1, 2].map(function (x) { return typeof this; }).join());
console.log([1, 2].map(function (x) { return typeof this; }, 5).join());
console.log([1, 2].map(function (x) { return this === undefined; }).join());
console.log([1, 2].map(function (x) { return this; }).map(v => v === globalThis).join());
console.log([1, 2].filter(function (x) { return typeof this === "object"; }).join());
var seen = []; [2, 1].sort(function (a, b) { seen.push(typeof this); return a - b; }); console.log(seen[0]);
console.log([1, 2].reduce(function (a, x) { return a + typeof this; }, ""));
console.log([1, 2].map(function (x) { "use strict"; return typeof this; }).join());
console.log([1, 2].map(function (x) { "use strict"; return this; }, 7).join());
var o = {
  f() { return [1, 2].map(x => this === o).join(); },
  g() { return [1, 2, 3].filter(x => this === o).length; },
  h() { var s = [3, 1, 2]; s.sort((a, b) => (this === o ? a - b : b - a)); return s.join(); }
};
console.log(o.f(), o.g(), o.h());
console.log([1, 2].map(function (x) { return this == 5; }, 5).join());
var w = [1, 2, 3].map(function () { return this; }, "s");
console.log(w[0] !== w[1], typeof w[0], w[0] instanceof String, String(w[2]));
var sloppySeen = new Set(), strictSeen = new Set();
(function () { [3, 1, 2].sort((a, b) => { sloppySeen.add(this === globalThis); return a - b; }); }).call(undefined);
(function () { "use strict"; [3, 1, 2].sort((a, b) => { strictSeen.add(this === undefined); return a - b; }); }).call(undefined);
console.log([...sloppySeen].join(), [...strictSeen].join());
"##,
        expected: &[
            "object,object",
            "object,object",
            "false,false",
            "true,true",
            "1,2",
            "object",
            "objectobject",
            "undefined,undefined",
            "7,7",
            "true,true 3 1,2,3",
            "true,true",
            "true object true s",
            "true true",
        ],
    },
    Probe {
        name: "hoisted_length",
        src: r##"
var arr = [];
for (var k = 0; k < 10; k++) arr.push(k);
var o = { get v() { if (arr.length < 5000) arr.push(1); return 1; } };
var s = 0;
for (var i = 0; i < arr.length; i++) { s = s + o.v; }
console.log(s, arr.length);
var arr2 = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
var o2 = { valueOf() { if (arr2.length < 5000) arr2.push(1); return 1; } };
var s2 = 0;
for (var i = 0; i < arr2.length; i++) { s2 = s2 + o2; }
console.log(s2, arr2.length);
var arr3 = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
var o3 = { toString() { if (arr3.length < 5000) arr3.push(1); return "x"; } };
var s3 = "";
for (var i = 0; i < arr3.length; i++) { s3 = "a" + o3 + i; }
console.log(s3, arr3.length);
var str = "abc";
var o5 = { get v() { if (str.length < 3000) str = str + "d"; return 1; } };
var s5 = 0;
for (var i = 0; i < str.length; i++) { s5 = s5 + o5.v; }
console.log(s5, str.length);
class K { get kids() { if (queue.length < 5000) queue.push(1); return 0; } }
var nodes = [new K()];
var queue = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];
var s7 = 0;
for (var i = 0; i < queue.length; i++) { s7 = s7 + nodes[0].kids; }
console.log(s7, queue.length);
var arr8 = []; for (var k = 0; k < 5000; k++) arr8.push(1);
var o8 = { get v() { if (arr8.length > 2500) arr8.pop(); return 1; } };
var s8 = 0, miss = 0;
for (var i = 0; i < arr8.length; i++) { s8 = s8 + o8.v; if (arr8[i] === undefined) miss = miss + 1; }
console.log(s8, arr8.length, miss);
var arrE = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
var oE = { toString() { if (arrE.length < 3000) arrE.push(1); return "x"; } };
var sE = "";
for (var i = 0; i < arrE.length; i++) { sE = (i < 10 ? "0" : "") + oE; }
console.log(sE, arrE.length);
var text = "abcdefghij", n = 0;
for (var d = 0; d < 5000; d++) { for (var i = 0; i < text.length; i++) { n = (n + text.charCodeAt(i)) | 0; } }
console.log(n);
"##,
        expected: &[
            "5000 5000",
            "5000 5000",
            "ax4999 5000",
            "3000 3000",
            "0 5000",
            "2500 2500 0",
            "x 3000",
            "5075000",
        ],
    },
    Probe {
        name: "continue_length",
        src: r##"
var arr = [1, 2, 3];
function f(n) {
  var t = -1, i = 0, z;
  while (i < n) { i = i + 1; z = null; if (i > 0) continue; t = arr.length; }
  return t;
}
console.log(f(100000), f(100000));
var table = [10, 20, 30, 40];
function findSlot(keys, n, needle) {
  var size = -1, i = 0;
  while (i < n) { var k = keys[i]; i = i + 1; if (k !== needle) continue; size = table.length; }
  return size;
}
var keys = []; for (var j = 0; j < 5000; j++) keys.push("k" + j);
console.log(findSlot(keys, keys.length, "k42"), findSlot(keys, keys.length, "missing"), findSlot(keys, keys.length, "missing"));
var gs = "hello";
function v2(n) { var t = -1, i = 0; while (i < n) { i = i + 1; if (i > 0) continue; t = gs.length; } return t; }
console.log(v2(100000), v2(100000));
function forc(n) { var t = -1; for (var i = 0; i < n; i++) { if (i > -1) continue; t = arr.length; } return t; }
function dowc(n) { var t = -1, i = 0; do { i = i + 1; if (i > 0) continue; t = arr.length; } while (i < n); return t; }
function brk(n) { var t = -1, i = 0; while (i < n) { i = i + 1; if (i > n + 5) break; t = arr.length; } return t; }
console.log(forc(100000), dowc(100000), brk(100000), brk(0));
var str = "hello world";
function g(n) {
  var t = -1, i = 0, h = 0;
  while (i < n) { h = (h + str.charCodeAt(i & 7)) | 0; i = i + 1; if (i > 0) continue; t = str.length; }
  return t + ":" + h;
}
console.log(g(100000), g(100000));
function g2(n) { var t = -1, i = 0, h = 0; while (i < n) { i = i + 1; h = (h + i) | 0; if (h !== -12345) continue; t = str.length; } return t; }
console.log(g2(100000), g2(100000));
function g3(n) { var t = -1, i = 0, h = 0; while (i < n) { i = i + 1; h = (h + i) | 0; t = str.length; } return t + ":" + h; }
console.log(g3(100000), g3(0), g3(1));
"##,
        expected: &[
            "-1 -1",
            "4 -1 -1",
            "-1 -1",
            "-1 -1 3 -1",
            "-1:9925000 -1:9925000",
            "-1 -1",
            "11:705082704 -1:0 11:1",
        ],
    },
    Probe {
        name: "dead_reads",
        src: r##"
var hits = 0;
Object.defineProperty(Array.prototype, 9, { get() { hits++; return 7; }, configurable: true });
function f(a) { var s = 0; for (var i = 0; i < 100000; i++) { a[i & 15]; s = (s + i) | 0; } return s; }
console.log(f([1, 2, 3, 4, 5, 6, 7, 8]), hits);
hits = 0;
function g(a) { var s = 0.5; for (var i = 0; i < 100000; i++) { var t = a[i & 15]; s = s + 1.5; } return s; }
console.log(g([1.5, 2.5, 3.5, 4.5, 5.5, 6.5, 7.5, 8.5]), hits);
delete Array.prototype[9];
var trapped = 0;
var objProto = Object.getPrototypeOf(Array.prototype);
Object.setPrototypeOf(Array.prototype, new Proxy(objProto, {
  get(t, k, r) { if (k === "12") trapped++; return Reflect.get(t, k, r); }
}));
function h(a) { var s = 0; for (var i = 0; i < 100000; i++) { a[i & 15]; s = (s + i) | 0; } return s; }
var hr = h([1, 2, 3, 4, 5, 6, 7, 8]);
Object.setPrototypeOf(Array.prototype, objProto);
console.log(hr, trapped);
var ta = new Int32Array(16);
function tv(a) { var s = 0; for (var i = 0; i < 100000; i++) { a[i & 31]; s = (s + i) | 0; } return s; }
console.log(tv(ta));
"##,
        expected: &[
            "704982704 6250",
            "150000.5 6250",
            "704982704 6250",
            "704982704",
        ],
    },
    Probe {
        name: "captured_callee",
        src: r##"
var ta = new Int32Array(5000);
for (var q = 0; q < 5000; q++) ta[q] = (q % 97) + 1;
function f(t) {
  var h = 0;
  for (var i = 0; i < t.length; i++) { h = Math.imul(-t[i], 7) ^ h; }
  return h;
}
console.log(f(ta));
ta[2500] = 0;
try { console.log(f(ta)); } catch (e) { console.log("threw", e.message); }
function w(t) { var h = 0, i = 0; while (i < t.length) { h = Math.imul(-t[i], 7) ^ h; i = i + 1; } return h; }
try { console.log(w(ta)); } catch (e) { console.log("threw", e.message); }
var idx = new Int32Array(1000);
for (var q = 0; q < 1000; q++) idx[q] = (q * 7) % 26;
var s = "abcdefghijklmnopqrstuvwxyz";
function g(n) { var h = 0; for (var i = 0; i < n; i++) { h = (h + s.charCodeAt(idx[i])) | 0; } return h; }
console.log(g(1000)); console.log(g(1001));
function g7(n) { var h = 0; for (var i = 700; i < n; i++) { h = (h + s.charCodeAt(idx[i])) | 0; } return h; }
console.log(g7(1000)); console.log(g7(1001));
var pa = []; for (var q = 0; q < 5000; q++) pa.push(q % 13);
function a8(t) { var h = 0; for (var i = 0; i < t.length; i++) { h = Math.imul(t[i] % 5, 7) + h | 0; } return h; }
console.log(a8(pa)); pa[2500] = 1.5;
try { console.log(a8(pa)); } catch (e) { console.log("threw", e.message); }
function s9(n, d) { var h = 0; for (var i = 0; i < n; i++) { h = (h + s.charCodeAt((i * 3) % d)) | 0; } return h; }
console.log(s9(3000, 26)); console.log(s9(3000, 0));
"##,
        expected: &[
            "192", "-724", "-724", "109500", "109597", "32856", "32953", "61915", "61894",
            "328484", "291000",
        ],
    },
    Probe {
        // ECMAScript `*` gives -0 for a zero product with one negative
        // operand. An i64 home cannot hold -0, so every integer multiply
        // (interpreter, INT xmm, INT-GPR, Tier A) must yield the double;
        // `mul_shift`'s positive power-of-two constant and `Math.imul` never
        // can. The three settings must agree with each other AND with node.
        name: "negative_zero_mul",
        src: r##"
var show = function (v) { return Object.is(v, -0) ? "-0" : String(v); };
console.log(Object.is(0 * -5, -0), 1 / (0 * -5));
function f(a, b) { return a * b; }
console.log(Object.is(f(0, -5), -0), Object.is(f(-5, 0), -0), 1 / (0 * -5), 1 / (-0 * 5));
console.log(show(0 * -5), show(-5 * 0), show(-0 * 5), show(5 * -0), show(0 * 5), show(-0 * -5), show(-4 * -0));
var z = 0, neg = -5, pos = 5, nz = -0;
console.log(show(z * neg), show(neg * z), show(nz * pos), show(z * pos), show(neg * neg), show(z * z));
var arr = [0, -5, 5, -1];
console.log(show(arr[0] * arr[1]), show(arr[1] * arr[0]), show(arr[0] * arr[2]), show(false * -5), show(true * -5));
// Tier A: a hot small int function, then the -0 shapes.
function m(a, b) { return a * b; }
for (var i = 0; i < 40000; i++) { m(3, 4); }
console.log(show(m(0, -5)), show(m(-5, 0)), show(m(0, 5)), show(m(-3, -4)), show(m(2147483647, 2147483647)));
// INT region (GPR homes): the loop's last product decides the sign.
function run(neg) { var r = 1, s = 0; for (var i = 0; i < 100000; i++) { r = s * neg; s = s & 0; } return 1 / r; }
console.log(run(-3), run(3), run(0));
// Only SOME iterations are -0 (a bail per zero product, then re-entry).
function mixed(k) { var c = 0; for (var i = 0; i < 100000; i++) { var p = (i & 1) * k; if (1 / p < 0) c++; } return c; }
console.log(mixed(-3), mixed(3), mixed(-1));
// DOUBLE region: `+ 0.5` keeps the body off the INT tiers.
function dbl(k) { var c = 0; for (var i = 0; i < 100000; i++) { var p = (i & 1) * k; var q = p + 0.5; if (q === 0.5 && 1 / p < 0) c++; } return c; }
console.log(dbl(-3), dbl(3));
// Constant multipliers: positive (`mul_shift` / no check emitted), 0 and -1.
function byConst(n) {
  var a = 0, b = 0, c = 0, d = 0, last0 = 1, lastN = 1, lastZ = 1;
  for (var i = 0; i < n; i++) {
    var v = (i & 1) - 1;
    last0 = v * 4; lastN = v * -1; lastZ = v * 0;
    a = a + last0; b = b + lastN; c = c + lastZ; d = d + v * 3;
  }
  return [a, b, c, d, show(last0), show(lastN), show(lastZ)].join(" ");
}
console.log(byConst(100000));
// A signed counter crossing zero inside a hot int loop.
function cross(n) { var hits = 0, k = -1; for (var i = 0; i < n; i++) { var p = (i - 50000) * k; if (1 / p === -Infinity) hits++; } return hits; }
console.log(cross(100000));
// Math.imul is a 32-bit signed product: never -0, so it keeps its bare imul.
console.log(show(Math.imul(0, -5)), show(Math.imul(-5, 0)), show(Math.imul(-1, -1)));
function hot(t) { var h = 0; for (var i = 0; i < t.length; i++) { h = Math.imul(t[i], -1) | 0; } return show(h); }
var ta = new Int32Array(2000); console.log(hot(ta));
console.log(2147483647 * 2147483647, (-2147483648) * 2, show((-2147483648) * 0));
console.log([0, -5, 5].map(function (x) { return x * -1; }).map(show).join());
console.log([0, 1, 2].map(function (x) { return (x - 1) * 0; }).map(show).join());
console.log(show([0, -5].reduce(function (s, x) { return s * x; }, 1)));
function comp(n) { var v = 0, last = 1; for (var i = 0; i < n; i++) { v = i & 0; last = v; last *= -7; } return show(last); }
console.log(comp(100000));
// A PRODUCT is a new source of -0 in this engine (it used to narrow to Int 0),
// and -0 is the key "0" everywhere. Every Int-fast-lane consumer must agree.
var kk = 0 * -1;
var ka = [7, 8]; ka[kk] = 9; console.log(ka[0], ka.length, ka[kk], Object.keys(ka).join());
var kt = new Int32Array(2); kt[kk] = 5; console.log(kt[0]);
var ko = {}; ko[kk] = 1; console.log(Object.keys(ko).join(), ko[0]);
var km = new Map([[0, "z"]]); console.log(km.get(kk), new Set([kk, 0]).size);
console.log(String(kk), JSON.stringify([kk]), kk.toFixed(1), [kk].join(), kk === 0, kk < 1);
switch (kk) { case 0: console.log("case0"); }
console.log(show(Math.sign(kk)), show(Math.abs(kk)), show(Math.min(kk, 0)), show(Math.max(kk, -0)));
console.log(show(kk), show(-kk), show(kk + 0), show(kk - 0), show(kk * 1), show(kk / 1), show(kk % 5), kk | 0, kk >>> 0);
// The same keys once the producing loop is hot on an int tier.
function idx(n) { var arr = [1, 2, 3], s = 0; for (var i = 0; i < n; i++) { var key = (i & 0) * -3; arr[key] = i; s = (s + arr[key]) | 0; } return s + " " + arr.join() + " " + arr.length; }
function tak(n) { var t = new Int32Array(4), s = 0; for (var i = 0; i < n; i++) { var key = (i & 0) * -3; t[key] = i & 255; s = (s + t[key]) | 0; } return s + " " + t.join(); }
function objk(n) { var ob = {}, s = 0; for (var i = 0; i < n; i++) { var key = (i & 0) * -3; ob[key] = i; s = (s + ob[key]) | 0; } return s + " " + Object.keys(ob).join(); }
console.log(idx(100000)); console.log(tak(100000)); console.log(objk(100000));
"##,
        expected: &[
            "true -Infinity",
            "true true -Infinity -Infinity",
            "-0 -0 -0 -0 0 0 0",
            "-0 -0 -0 0 25 0",
            "-0 -0 0 -0 -5",
            "-0 -0 0 12 4611686014132420600",
            "-Infinity Infinity Infinity",
            "100000 0 100000",
            "50000 0",
            "-200000 50000 0 -150000 0 -0 0",
            "1",
            "0 0 1",
            "0",
            "4611686014132420600 -4294967296 -0",
            "-0,5,-5",
            "-0,0,0",
            "-0",
            "-0",
            "9 2 9 0,1",
            "5",
            "0 1",
            "z 1",
            "0 [0] 0.0 0 true true",
            "case0",
            "-0 0 -0 -0",
            "-0 0 0 -0 -0 -0 -0 0 0",
            "704982704 99999,2,3 3",
            "12742320 159,0,0,0",
            "704982704 0",
        ],
    },
    Probe {
        name: "frozen_store",
        src: r##"
"use strict";
const fmt = (a) => a.join(',');
for (let k = 0; k < 20000; k++) fmt([k]);
function run(n) {
  const a = [1, 2, 3, 4, 5, 6, 7, 8];
  let err = 'none';
  try {
    for (let i = 0; i < n; i++) {
      if (i === 20000) Object.freeze(a);
      a[i & 7] = i;
    }
  } catch (e) { err = e.constructor.name; }
  return err + ' ' + Object.isFrozen(a) + ' ' + fmt(a);
}
console.log(run(30000));
function viaHelper(n) {
  const a = [1, 2, 3, 4, 5, 6, 7, 8];
  const g = (x) => { Object.freeze(x); };
  let err = 'none';
  try { for (let i = 0; i < n; i++) { if (i === 15000) g(a); a[i & 3] = i; } } catch (e) { err = e.constructor.name; }
  return err + ' ' + fmt(a);
}
console.log(viaHelper(30000));
"##,
        expected: &[
            "TypeError true 19992,19993,19994,19995,19996,19997,19998,19999",
            "TypeError 14996,14997,14998,14999,5,6,7,8",
        ],
    },
    Probe {
        name: "frozen_store_sloppy",
        src: r##"
const fmt = (a) => a.join(',');
for (let k = 0; k < 20000; k++) fmt([k]);
function run(n) {
  const a = [1, 2, 3, 4, 5, 6, 7, 8];
  for (let i = 0; i < n; i++) {
    if (i === 20000) Object.freeze(a);
    a[i & 7] = i;
  }
  return Object.isFrozen(a) + ' ' + fmt(a);
}
console.log(run(30000));
"##,
        expected: &["true 19992,19993,19994,19995,19996,19997,19998,19999"],
    },
    Probe {
        name: "private_brand",
        src: r##"
class X { #secret() { return 'X-secret'; } }
class Y { #secret() { return 'Y'; } static call(o) { return o.#secret(); } }
const y = new Y();
for (let i = 0; i < 100; i++) Y.call(y);
try { console.log(Y.call(new X())); } catch (e) { console.log('threw', e.constructor.name); }
try { console.log(Y.call(Object.create(y))); } catch (e) { console.log('threw', e.constructor.name); }
try { console.log(Y.call({})); } catch (e) { console.log('threw', e.constructor.name); }
console.log(Y.call(y));
function mk(n) { return class { #m() { return 'm-' + n; } static call(o) { return o.#m(); } }; }
const A = mk('A'), B = mk('B'); const a = new A(), bb = new B();
for (let i = 0; i < 100; i++) B.call(bb);
try { console.log(B.call(a)); } catch (e) { console.log('threw', e.constructor.name); }
console.log(B.call(bb), A.call(a));
class W { #p() { return 'Wp'; } }
class Z { #p() { return 'Zp'; } run(o) { return o.#p(); } }
const z = new Z(); for (let i = 0; i < 100; i++) z.run(z);
try { console.log(z.run(new W())); } catch (e) { console.log('threw', e.constructor.name); }
console.log(z.run(z));
class Acc {
  #n = 0;
  #add(x) { this.#n = this.#n + x; return this.#n; }
  run(k) { let s = 0; for (let i = 0; i < k; i++) s = this.#add(i & 3); return s; }
}
console.log(new Acc().run(20000));
"##,
        expected: &[
            "threw TypeError",
            "threw TypeError",
            "threw TypeError",
            "Y",
            "threw TypeError",
            "m-B m-A",
            "threw TypeError",
            "Zp",
            "30000",
        ],
    },
    Probe {
        // The `#m` site inside a method the caller's loop cross-calls (a
        // Tier-C body, frame-free): the brand chain is that body's, a foreign
        // receiver throws without replaying the call (`cnt` counts entries),
        // and a private field or getter holding a function is called too.
        name: "private_brand_cross",
        src: r##"
"use strict";
let cnt = 0;
class P {
  #m(x) { return x + 1; }
  run(o, x) { cnt++; return o.#m(x); }
  static srun(o, x) { cnt++; return o.#m(x); }
}
class Q { #m(x) { return x * 100; } }
const p = new P(), q = new Q();
function outer(recv, o, n) { let s = 0; for (let i = 0; i < n; i++) s = (s + recv.run(o, i)) | 0; return s; }
function outerS(o, n) { let s = 0; for (let i = 0; i < n; i++) s = (s + P.srun(o, i)) | 0; return s; }
console.log(outer(p, p, 50000), cnt);
try { console.log(outer(p, q, 10)); } catch (e) { console.log("threw", e.constructor.name, cnt); }
console.log(outerS(p, 50000), cnt);
try { console.log(outerS(q, 10)); } catch (e) { console.log("threw", e.constructor.name, cnt); }
const mixed = [p, p, p, Object.create(p)];
let caught = 0;
for (let i = 0; i < 40000; i++) { try { outer(p, mixed[i & 3], 1); } catch (e) { caught++; } }
console.log(caught, cnt);
class G {
  #f = (x) => x * 3;
  get #g() { return (x) => x - 1; }
  a(o, x) { return o.#f(x); }
  b(o, x) { return o.#g(x); }
}
const g = new G();
let t = 0;
for (let i = 0; i < 30000; i++) t = (t + g.a(g, i) + g.b(g, i)) | 0;
console.log(t);
try { g.a({}, 1); } catch (e) { console.log("threw", e.constructor.name); }
try { g.b(p, 1); } catch (e) { console.log("threw", e.constructor.name); }
"##,
        expected: &[
            "1250025000 50000",
            "threw TypeError 50001",
            "1250025000 100001",
            "threw TypeError 100002",
            "10000 140002",
            "1799910000",
            "threw TypeError",
            "threw TypeError",
        ],
    },
];

/// Runs every probe in THIS process and reports each mismatch by name. Only
/// the mode driver below runs it, with the mode's environment already set.
#[test]
fn audit_jit_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let mut failures = Vec::new();
    for probe in PROBES {
        match zipp_vm::run(probe.src) {
            Err(e) => failures.push(format!("{}: compile error {e}", probe.name)),
            Ok(out) if out.error.is_some() => failures.push(format!(
                "{}: runtime error {:?} after {:?}",
                probe.name, out.error, out.output
            )),
            Ok(out) if out.output != probe.expected => failures.push(format!(
                "{}:\n  expected {:?}\n  got      {:?}",
                probe.name, probe.expected, out.output
            )),
            Ok(_) => {}
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Each tier decision (thresholds, `ZIPP_NOJIT`) is read once per process, so
/// every mode runs the probes in a fresh child of this test binary.
#[test]
fn audit_jit_modes_match_node() {
    if std::env::var_os(CHILD_ENV).is_some() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    for (mode, env) in [
        ("default", None),
        ("interpreter", Some(("ZIPP_NOJIT", "1"))),
        ("forced-jit", Some(("ZIPP_JIT_THRESHOLD", "1"))),
    ] {
        let mut cmd = std::process::Command::new(&exe);
        cmd.args(["--exact", "audit_jit_child", "--nocapture"])
            // An inherited setting must not decide what a mode tests.
            .env_remove("ZIPP_NOJIT")
            .env_remove("ZIPP_JIT_THRESHOLD")
            .env(CHILD_ENV, "1");
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let out = cmd.output().expect("spawn mode child");
        assert!(
            out.status.success(),
            "{mode} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
