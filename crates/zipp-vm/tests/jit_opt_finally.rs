//! Tier O: try/finally and for-of exits (break, continue, return and throw
//! through finally, an iterator's return(), labeled breaks), against Node's
//! output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"145159 seven 21|3|0f1ff3ff 74 2000 200 [[0],[2],[3,1],[1,2,3],[5,5]] true 2400"####;

const PROGRAM: &str = r####"// Tier O with handler ops: for-of (its iterator-closing finally), break,
// continue, return and throw out of it, user iterators whose return() must
// run, try/catch/finally, run from compiled callers, sort and forEach.
const out = [];
const log = [];
function sumOf(xs) { let s = 0; for (const x of xs) s += x; return s; }
function firstOver(xs, k) { for (const x of xs) { if (x > k) return x; } return -1; }
function skipOdd(xs) { let s = 0; for (const x of xs) { if (x & 1) continue; s += x; } return s; }
function breakAt(xs, k) { let s = 0; for (const x of xs) { if (x === k) break; s += x; } return s; }
function throwIn(xs) { let s = 0; for (const x of xs) { if (x === 7) throw new Error("seven " + s); s += x; } return s; }
function catchIt(xs) { try { return throwIn(xs); } catch (e) { return e.message; } }
function finallyOrder(n) { let r = ""; for (let i = 0; i < n; i++) { try { if (i === 2) continue; if (i === 4) break; r += i; } finally { r += "f"; } } return r; }
function overrideReturn(x) { try { return x * 2; } finally { if (x > 5) log.push("fin" + x); } }
function nested(xs) { let s = 0; outer: for (const a of xs) { for (const b of xs) { if (b > a) continue outer; if (a + b > 12) break outer; s += a * b; } } return s; }
class Counter {
  constructor(n) { this.n = n; this.closed = 0; }
  [Symbol.iterator]() { let i = 0; const self = this; return { next() { return i < self.n ? { value: i++, done: false } : { value: undefined, done: true }; }, return() { self.closed++; return { done: true }; } }; }
}
function userBreak(c, k) { let s = 0; for (const x of c) { if (x === k) break; s += x; } return s; }
const xs = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
let acc = 0, cs = "";
for (let r = 0; r < 300; r++) {
  acc += sumOf(xs) + firstOver(xs, r % 12) + skipOdd(xs) + breakAt(xs, (r % 10) + 1) + nested(xs) + overrideReturn(r % 8);
  cs = catchIt(xs) + "|" + catchIt([1, 2]) + "|" + finallyOrder(6);
}
out.push(acc, cs, log.length);
const c = new Counter(10);
let ub = 0;
for (let r = 0; r < 200; r++) ub += userBreak(c, 5);
out.push(ub, c.closed);
// From native callers: a comparator and a forEach callback that iterate.
const rows = [[3, 1], [1, 2, 3], [2], [5, 5], [0]];
let cmpCalls = 0;
for (let r = 0; r < 100; r++) rows.slice().sort((a, b) => { cmpCalls++; return sumOf(a) - sumOf(b) || breakAt(a, 9) - breakAt(b, 9); });
const sorted = rows.slice().sort((a, b) => sumOf(a) - sumOf(b));
let fe = 0;
for (let r = 0; r < 200; r++) rows.forEach((row) => { for (const v of row) { if (v === 5) break; fe += v; } });
out.push(JSON.stringify(sorted), cmpCalls > 0, fe);
console.log(out.join(" "));
"####;

#[test]
fn matches_node() {
    // Read once per process, before the first VM: this file's only test.
    std::env::set_var("ZIPP_OPT", "1");
    std::env::set_var("ZIPP_NO_OPT_TIER_UP", "1");
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
