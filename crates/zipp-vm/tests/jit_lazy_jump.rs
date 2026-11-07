//! `break` and `continue` leaving lazy brackets are routed natively: into the
//! first lazy `finally` on the way with the jump completion the interpreter
//! deposits, on from its `EndFinally`, and over to the frame's routing once a
//! pushed bracket comes next. Every route, hot, against Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED_ROUTES: &str = r####"17033 399 843000
6:6:6 91 31530 34830 1 1 157
ret3 done broke2
6 fin7 3 0a2 NaN
18 3000 24000 18 912 6000 812
number true"####;

const ROUTES: &str = r####"// break/continue leaving lazy brackets: every route, hot, against Node.
"use strict";
const HOT = 3000;
var fin = 0, log = [];
function hot(f) { var r; for (var i = 0; i < HOT; i++) r = f(i); return r; }
function t(m) { throw new Error(m); }
// The bench's shapes.
function loopsThroughFinally(n, seedVal) {
  let acc = seedVal;
  for (let i = 0; i < n; i++) {
    for (let j = 0; j < 16; j++) {
      try { if (j === (i & 15)) continue; if (j > 12) break; acc = (acc + i * j) | 0; }
      finally { acc ^= j; fin++; }
    }
  }
  return acc;
}
function searchWithCleanup(rows, target) {
  let opened = 0, closed = 0, hit = -1;
  scan: for (let r = 0; r < rows.length; r++) {
    opened++;
    try {
      const row = rows[r];
      for (let c = 0; c < row.length; c++) {
        if (row[c] < 0) continue scan;
        if (row[c] === target) { hit = r * 100 + c; break scan; }
      }
    } finally { closed++; }
  }
  return (hit + opened * 7 + closed * 13) | 0;
}
const rows = []; for (let r = 0; r < 20; r++) { const row = []; for (let c = 0; c < 10; c++) row.push(((r * 31 + c * 7) % 23) - 2); rows.push(row); }
console.log(hot((i) => loopsThroughFinally(20, i)), hot((i) => searchWithCleanup(rows, i % 25)), fin);
// Two lazy finallys on one route, and the completion they deposit.
function two(n) { let a = 0, b = 0, s = 0; for (let i = 0; i < n; i++) { try { try { if (i === 5) break; if (i & 1) continue; s += i; } finally { a++; } } finally { b++; } } return s + ":" + a + ":" + b; }
// A lazy catch left by the jump, a catch inside a finally, a finally inside a catch.
function c1(n) { let s = 0; for (let i = 0; i < n; i++) { try { if (i % 3 === 0) continue; if (i === 17) break; s += i; } catch (e) { s = -1; } } return s; }
function c2(n) { let s = 0, f = 0; for (let i = 0; i < n; i++) { try { try { if (i % 4 === 1) continue; if (i === 13) break; s += i; } catch (e) { s = -1; } } finally { f++; } } return s * 100 + f; }
function c3(n) { let s = 0, f = 0; for (let i = 0; i < n; i++) { try { try { if (i % 5 === 2) continue; s += i; } finally { f++; } } catch (e) { s = -1; } } return s * 100 + f; }
// A labelled block, and a continue to a while loop's head.
function lb(x) { let s = 0; out: { try { if (x & 1) break out; s += 10; } finally { s += 1; } s += 100; } return s; }
function wl(n) { let i = 0, s = 0; while (i < n) { i++; try { if (i & 1) continue; s += i; } finally { s ^= 1; } } return s; }
console.log(hot(() => two(20)), hot(() => c1(30)), hot(() => c2(30)), hot(() => c3(30)), hot(lb), lb(1), hot(() => wl(25)));
// Overrides: a finally's return over a break, a break in a finally over a return.
function ov1(n) { for (let i = 0; i < n; i++) { try { if (i === 3) break; } finally { if (i === 3 && n > 10) return "ret" + i; } } return "done"; }
function ov2(n) { let i = 0; for (; i < n; i++) { try { if (i === 2) return "early"; } finally { if (i === 2) break; } } return "broke" + i; }
console.log(hot(() => ov1(20)), ov1(5), hot(() => ov2(9)));
// A throw inside a finally a jump entered; a guard bail in the body after warm-up.
function th(n, k) { let s = 0; for (let i = 0; i < n; i++) { try { if (i === 4) break; s += i; } finally { if (i === 4 && k) t("fin" + k); } } return s; }
function thc(k) { try { return th(10, k); } catch (e) { return e.message; } }
function gb(arr) { let s = 0; for (let i = 0; i < arr.length; i++) { try { if (arr[i] === null) continue; s = s + arr[i].v; } finally { fin++; } } return s; }
console.log(hot(() => thc(0)), thc(7), hot(() => gb([{ v: 1 }, null, { v: 2 }])), gb([{ v: "a" }, null, { v: 2 }]), gb([{ v: 1.5 }, {}, { v: 2 }]));
// for-of with continue and break: the iterator closes once per break.
let closes = 0, nexts = 0;
const it = { [Symbol.iterator]() { let i = 0; return { next() { nexts++; return { done: i > 9, value: i++ }; }, return() { closes++; return {}; } }; } };
function fo(src) { let s = 0; for (const v of src) { if (v === 3) continue; if (v === 7) break; s += v; } return s; }
function fo2(src) { let s = 0; for (const v of src) { try { if (v & 1) continue; if (v > 6) break; s += v; } finally { s += 100; } } return s; }
console.log(hot(() => fo(it)), closes, nexts, hot(() => fo([0, 1, 2, 3, 4, 5, 6, 7, 8])), hot(() => fo2(it)), closes, hot(() => fo2([1, 2, 3, 4, 5, 6, 7, 8, 9])));
console.log(typeof fin, fin > 0);
"####;

const EXPECTED_MORE: &str = r####"0:12:6 174 1803 48013 11105015
123 i1m23z 123 a..c.. 7
3 tb5 10 0sssss NaN 8
number true"####;

const MORE: &str = r####"// More break/continue routes through lazy and pushed brackets, hot, against Node.
"use strict";
const HOT = 3000;
var fin = 0;
function hot(f) { var r; for (var i = 0; i < HOT; i++) r = f(i); return r; }
function t(m) { throw new Error(m); }
// A lazy finally first, then a pushed one (its body holds a loop): the route
// hands over to the frame's routing.
function mixed(n) {
  let a = 0, b = 0, s = 0;
  outer: for (let i = 0; i < n; i++) {
    try {
      for (let j = 0; j < 4; j++) {
        try { if (i === 3 && j === 2) break outer; if (j === 1) continue outer; s += i * j; } finally { a++; }
      }
    } finally { b++; }
  }
  return s + ":" + a + ":" + b;
}
// Out of a switch inside a try, and a labelled continue over two loops.
function sw(n) { let s = 0; for (let i = 0; i < n; i++) { try { switch (i % 4) { case 0: continue; case 1: s += 1; break; case 2: if (i > 9) break; s += 2; break; default: s += 3; } s += 10; } finally { fin++; } } return s; }
function lc(n) { let s = 0; o: for (let i = 0; i < n; i++) { for (let j = 0; j < 3; j++) { try { if (j === i % 3) continue o; s += j; } finally { s += 100; } } } return s; }
// From a catch block and from a finally block, inside an outer try.
function fc(n) { let s = 0, f = 0; for (let i = 0; i < n; i++) { try { try { if (i % 3 === 0) t("x"); s += i; } catch (e) { if (i > 10) break; continue; } } finally { f++; } } return s * 1000 + f; }
function ff(n) { let s = 0, f = 0; for (let i = 0; i < n; i++) { try { try { s += i; } finally { if (i % 4 === 3) continue; if (i > 13) break; } s += 1000; } finally { f++; } } return s * 1000 + f; }
// Three deep, a labelled break out of two of them.
function deep3(x) { let s = ""; a: { try { try { try { if (x & 1) break a; s += "i"; } finally { s += "1"; } s += "m"; } finally { s += "2"; } } finally { s += "3"; } s += "z"; } return s; }
// for-in, and a while(true) left only by break.
function fi(o) { let s = ""; for (const k in o) { try { if (k === "b") continue; if (k === "d") break; s += k; } finally { s += "."; } } return s; }
function wt(n) { let i = 0; while (true) { try { i++; if (i >= n) break; } finally { fin++; } } return i; }
// A throw from the finally a break entered, caught by the caller; a guard bail in the routed finally.
function tb(k) { let s = 0; for (let i = 0; i < 8; i++) { try { if (i === 3) break; s += i; } finally { if (i === 3 && k) t("tb" + k); } } return s; }
function tbc(k) { try { return tb(k); } catch (e) { return e.message; } }
function gbf(o) { let s = 0; for (let i = 0; i < 6; i++) { try { if (i === 2) continue; if (i === 4) break; } finally { s = s + o.v; } } return s; }
// A generator's brackets stay pushed: the same routes through it.
function* gen(n) { for (let i = 0; i < n; i++) { try { if (i === 2) continue; if (i === 5) break; yield i; } finally { fin++; } } }
function gsum(n) { let s = 0; for (const v of gen(n)) s += v; return s; }
console.log(hot(() => mixed(6)), hot(() => sw(20)), hot(() => lc(9)), hot(() => fc(20)), hot(() => ff(20)));
console.log(hot(deep3), deep3(0), deep3(1), hot(() => fi({ a: 1, b: 2, c: 3, d: 4, e: 5 })), hot(() => wt(7)));
console.log(hot(() => tbc(0)), tbc(5), hot(() => gbf({ v: 2 })), gbf({ v: "s" }), gbf({}), hot(() => gsum(9)));
console.log(typeof fin, fin > 0);
"####;

fn matches(program: &str, expected: &str) {
    let out = zipp_vm::run(program).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), expected);
}

#[test]
fn routes_match_node() {
    matches(ROUTES, EXPECTED_ROUTES);
}

#[test]
fn more_routes_match_node() {
    matches(MORE, EXPECTED_MORE);
}
