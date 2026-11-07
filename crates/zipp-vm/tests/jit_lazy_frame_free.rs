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

const EXPECTED_JUMPS: &str = r####"1348552 bct50;bct50;bct50;bct50;bct50;bct50;bct5 35539 78600
4999 0x1 NaN 1004
number true"####;

const JUMPS: &str = r####"// Returns in a catch's body under a finally; frame-free bodies with break/continue.
"use strict";
var fin = 0;
function t(m) { throw new Error(m); }
// The bench's error chain: try { return } catch { throw } finally.
class ParseError extends Error { constructor(message, pos) { super(message); this.name = "ParseError"; this.pos = pos; } }
function level3(x) { if ((x % 41) === 0) throw new ParseError("bad token " + (x & 15), x); return x & 255; }
function level2(x) { try { return level3(x) + 1; } catch (e) { if (e instanceof ParseError && (e.pos & 1)) { e.pos += 1; throw e; } return -1; } }
function level1(x) { try { return level2(x); } catch (e) { throw new Error("wrapped: " + e.message, { cause: e }); } finally { fin++; } }
function topLevel(x) { try { return level1(x); } catch (e) { return (e.message.length + e.cause.pos + (e.cause instanceof ParseError ? 1000 : 0)) | 0; } }
// A catch's own return under a finally, and under two.
function r1(x) { try { if (x & 1) return "odd"; t("even" + x); } catch (e) { return e.message.length; } finally { fin++; } }
function r2(x) { try { try { return x * 2; } catch (e) { return -1; } } finally { fin++; if (x === 7) return "fin7"; } }
function r3(x) { try { try { t("r3"); } catch (e) { if (x & 2) return "c" + x; } finally { fin++; } return "after" + x; } finally { fin++; } }
// Frame-free bodies with break/continue, called from a compiled loop.
function bc(n) { let s = 0; for (let i = 0; i < n; i++) { try { if (i % 3 === 0) continue; if (i === 11) break; s += i; } finally { fin++; } } return s; }
function bct(n) { let s = 0; for (let i = 0; i < n; i++) { try { if (i === n - 2) t("bct" + n); if (i & 1) continue; s += i; } finally { s += 100; } } return s; }
function drive(n) {
  let h = 0, m = "";
  for (let i = 0; i < n; i++) {
    h = (h + topLevel(i) + bc(i % 20)) | 0;
    const a = r1(i); h = (h + (typeof a === "number" ? a : a.length)) | 0;
    const b = r2(i % 9); h = (h + (typeof b === "number" ? b : 99)) | 0;
    h = (h + r3(i).length) | 0;
    try { h = (h + bct(i % 13 === 12 ? 6 : 50)) | 0; } catch (e) { m += e.message + ";"; }
  }
  return h + " " + m.slice(0, 40) + " " + m.length;
}
console.log(drive(6000), fin);
// A guard bail inside the frame-free break/continue body.
function bg(arr) { let s = 0; for (let i = 0; i < arr.length; i++) { try { if (arr[i] == null) continue; s = s + arr[i].v; if (s > 1000) break; } finally { fin++; } } return s; }
function driveG(n, mk) { let r; for (let i = 0; i < n; i++) r = bg(mk(i)); return r; }
console.log(driveG(5000, (i) => [{ v: i }, null, { v: 1 }]), bg([{ v: "x" }, null, { v: 1 }]), bg([{ v: 1.5 }, {}, { v: 2 }]), bg([{ v: 999 }, { v: 5 }, { v: 1 }]));
console.log(typeof fin, fin > 0);
"####;

/// A catch's return under a `finally`, and `break`/`continue` in a body that
/// runs frame-free.
#[test]
fn catch_returns_and_jumps_match_node() {
    let out = zipp_vm::run(JUMPS).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED_JUMPS);
}

const EXPECTED_COMBINED: &str = r####"-2999 -4 1112 1107 1112 1001
5 RangeError RangeError 2 3003 3,2,1,1
catch fin catch try 10224 tf2999
2999 c:TypeError c:TypeError s
number true"####;

const COMBINED: &str = r####"// try ... catch ... finally in one statement: the finally pops twice, once out
// of the try and once out of the catch, and the catch block runs under it.
"use strict";
const HOT = 3000;
var fin = 0, log = [];
function hot(f) { var r; for (var i = 0; i < HOT; i++) r = f(i); return r; }
function t(m) { throw new Error(m); }
function k1(x) { try { if (x % 5 === 4) t("k1"); return x; } catch (e) { return -x; } finally { fin++; } }
function k2(x) { let s = 0; try { if (x & 1) t("odd"); s = 1; } catch (e) { s = 2; try { if (x % 3 === 0) t("in"); s += 10; } catch (e2) { s += 5; } finally { s += 100; } } finally { s += 1000; } return s; }
function k3(x) { try { t("a" + x); } catch (e) { if (x % 7 === 6) throw new RangeError("again" + x); return e.message.length; } finally { log.push(x & 3); } }
function k3c(x) { try { return k3(x); } catch (e) { return e.name; } }
function k4(x) { try { if (x & 2) t("q"); return "try"; } catch (e) { return "catch"; } finally { if (x % 11 === 10) return "fin"; } }
function k5(n) { let s = 0; for (let i = 0; i < n; i++) { try { if (i % 4 === 0) t("e"); if (i === 9) break; s += i; } catch (e) { if (i > 6) continue; s += 100; } finally { s += 1000; } } return s; }
function k6(x) { let s = ""; try { s += "t"; } catch (e) { s += "c"; } finally { s += "f"; } return s + x; }
function k7(o) { try { return o.a.b; } catch (e) { return "c:" + e.name; } finally { fin++; } }
console.log(hot(k1), k1(4), hot(k2), k2(3), k2(5), k2(6));
log = [];
console.log(hot(k3c), k3c(6), k3c(13), k3c(1), log.length, log.slice(-4).join());
console.log(hot(k4), k4(10), k4(2), k4(1), hot(() => k5(20)), hot(k6));
console.log(hot((i) => k7({ a: { b: i } })), k7({}), k7(null), k7({ a: { b: "s" } }));
console.log(typeof fin, fin > 0);
"####;

/// `try ... catch ... finally` in one statement: the catch block runs under
/// the `finally`, which pops once out of the `try` and again out of the catch.
#[test]
fn combined_try_catch_finally_matches_node() {
    let out = zipp_vm::run(COMBINED).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED_COMBINED);
}
