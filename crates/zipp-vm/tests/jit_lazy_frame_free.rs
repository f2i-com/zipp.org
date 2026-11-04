//! Handler bodies whose brackets are all lazy run frame-free: through the
//! emitted cross-call lanes and the generic helper, a throw or a bail from a
//! bracket's body finds the handlers its exit stub deferred on the frame it
//! materializes, and a reused window keeps a catch from reading a stale
//! register. Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"167951485 1241 a5:82;in;a2:96;a5:165;in;a2:193;a5:248;in;a2:290;a5:331;in;a undefined:undefined;undefinedundefined;undefined:undefined;undefined:undefined;u 27495 30000
10000 NaN 7 NaN
9998 6 cTypeError 5002 4999,3,2
17743614 g1:70;g1:141;g1:212;g1:283;g1:354;g1:425 657
bottom 501 501
bottom 501 501
bottom 501 501
400401
400401
400401
number true"####;

const PROGRAM: &str = r####"// Handler bodies with every bracket lazy run frame-free: through the emitted
// lanes and the generic helper, with throws, bails and window reuse.
"use strict";
var fin = 0, log = [];
function t(m) { throw new Error(m); }
// Lane callees (one argument, called with one).
function a1(x) { try { if (x & 1) return x * 3; } finally { fin++; } return x; }
function a2(x) { try { if (x % 97 === 96) t("a2:" + x); return x + 1; } catch (e) { return e.message; } }
function a3(x) { let s = x; try { try { s = (s + 1) | 0; if (s % 89 === 0) t("in"); if (s & 4) return s; } finally { s ^= 0x55; fin++; } s = (s + 2) | 0; } finally { s = (s * 3) | 0; fin++; } return s; }
function a4(x) { try { return x; } finally { fin++; if ((x & 7) === 0) return -x; } }
// A throw out of the lazy finally into the caller's catch.
function a5(x) { try { if (x % 83 === 82) t("a5:" + x); return x; } finally { fin++; } }
// The may-read-before-write mask: a hoisted var assigned in the try, read in
// the catch after a throw before the assignment, in a reused window.
function zf(x) { try { if (x < 0) throw 0; var v = x * 2; return v; } catch (e) { return typeof v + ":" + v; } }
function zg(x) { var w; try { w = x.k; var u = w + 1; return u; } catch (e) { return String(u) + String(w); } }
function drive(n) {
  var h = 0, s = "", z = "";
  for (var i = 0; i < n; i++) {
    h = (h + a1(i) + a4(i)) | 0;
    var r = a2(i); if (typeof r === "string") s += r + ";"; else h = (h + r) | 0;
    try { h = (h + a3(i)) | 0; } catch (e) { s += e.message + ";"; }
    try { h = (h + a5(i)) | 0; } catch (e) { s += e.message + ";"; }
    var q = zf(i % 7 === 6 ? -1 : i); if (typeof q === "string") z += q + ";"; else h = (h + q) | 0;
    var g = zg(i % 11 === 10 ? null : { k: i }); if (typeof g === "string") z += g + ";"; else h = (h + g) | 0;
  }
  return h + " " + s.length + " " + s.slice(0, 60) + " " + z.slice(0, 80) + " " + z.length;
}
console.log(drive(6000), fin);
// Guard bails mid-body after warm-up: a different type.
function b1(o) { var s = 0; try { s = o.a + o.b; } finally { s = s * 2; fin++; } return s; }
function b2(o) { try { try { return o.a * o.b; } finally { log.push(o.a); } } catch (e) { return "c" + e.name; } }
function driveB(n, f, mk) { var r; for (var i = 0; i < n; i++) r = f(mk(i)); return r; }
console.log(driveB(5000, b1, (i) => ({ a: i, b: 1 })), b1({ a: "x", b: 2 }), b1({ a: 1.5, b: 2 }), b1({ b: 1 }));
log = [];
console.log(driveB(5000, b2, (i) => ({ a: i, b: 2 })), b2({ a: "3", b: 2 }), b2({ a: 2n, b: 2 }), log.length, log.slice(-3).join());
// The generic helper: an argument count the lanes do not take.
function g1(x, y) { try { if (y === undefined && x % 71 === 70) t("g1:" + x); return x; } finally { fin++; } }
function driveG(n) { var s = 0, m = ""; for (var i = 0; i < n; i++) { try { s = (s + g1(i)) | 0; } catch (e) { m += e.message + ";"; } } return s + " " + m.slice(0, 40) + " " + m.length; }
console.log(driveG(6000));
// A throw from 500 calls deep: every entered call's finally runs once.
var entered = 0, left = 0;
function deep(n) { entered++; try { if (n === 500) t("bottom"); return deep(n + 1) + 1; } finally { left++; } }
for (var k = 0; k < 3; k++) { entered = 0; left = 0; try { deep(0); } catch (e) { console.log(e.message, entered, left); } }
function deepC(n) { try { if (n === 400) t("c"); return deepC(n + 1) + 1; } catch (e) { return n * 1000 + (e.message === "c" ? 1 : 0); } }
for (var k = 0; k < 3; k++) console.log(deepC(0));
console.log(typeof fin, fin > 0);
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
