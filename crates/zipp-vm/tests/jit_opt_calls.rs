//! Tier O calls (`ZIPP_OPT=1`) against Node: inlined plain, own-method and class-method callees behind their guards, call-outs, globals, throws through a call-out, recursion, objects held across allocating calls.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"1107931210.5 14 102.5 7.5 22 25521110 42x1NaN1 big 1950 1107952252.5 1107965264.5 1307674368000 1.5511210043330986e+25 51000"####;

const PROGRAM: &str = r####""use strict";
// Tier O with calls: inlined plain and method callees, call-outs, globals, throws, recursion, allocation.
var SCALE = 3;
function sq(x) { return x * x; }
function add3(a, b, c) { return a + b + c; }
function scaled(x) { return x * SCALE; }
function loopy(n) { let s = 0; for (let i = 0; i < n; i++) s += i; return s; }
function maybeThrow(x) { if (x > 1000) throw new RangeError("big " + x); return x + 1; }
class V {
  constructor(x, y, z) { this.x = x; this.y = y; this.z = z; }
  dot(o) { return this.x * o.x + this.y * o.y + this.z * o.z; }
  sum() { return this.x + this.y + this.z; }
}
const pt = { k: 2, m(a) { return a * this.k; } };
function useCalls(a, b) { return sq(a) + add3(a, b, 1) + scaled(b) + loopy(a & 7); }
function useMethods(v, w) { return v.dot(w) + v.sum() + pt.m(v.x); }
function safe(x) { try { return maybeThrow(x); } catch (e) { return -1; } }
function viaThrow(x) { return maybeThrow(x) * 2; }
function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); }
function holdAcross(o, n) { let t = 0; for (let i = 0; i < n; i++) { t += o.x + mk(i).x; } return t; }
function mk(i) { return { x: i & 3, pad: [i, i + 1] }; }
const out = [];
let acc = 0;
const vs = [];
for (let k = 0; k < 16; k++) vs.push(new V(k * 0.5, k - 4, 2));
for (let k = 0; k < 300; k++) acc += useCalls(k, k * 0.25) + useMethods(vs[k & 15], vs[(k * 3) & 15]) + fact(k % 12);
out.push(acc);
// Deopts inside inlined bodies: another shape, a replaced method, a changed global, a string.
out.push(useMethods(new V(1, 2, 3), { z: 1, y: 1, x: 1 }), useMethods({ x: 1, y: 2, z: 3, dot: V.prototype.dot, sum: () => 100 }, vs[1]));
pt.m = function (a) { return a - this.k; };
out.push(useMethods(vs[2], vs[3]));
SCALE = 0.5;
out.push(useCalls(3, 4), useCalls("5", 2), useCalls(2, "x"));
// Throws through a call-out.
let caught = "";
for (let k = 0; k < 40; k++) { try { acc += viaThrow(k * 50); } catch (e) { caught = e.message; } }
out.push(caught, acc);
for (let k = 0; k < 40; k++) acc += safe(k * 40);
out.push(acc, fact(15), fact(25));
// Objects held in registers across calls that allocate.
const keep = { x: 7 };
let h = 0;
for (let k = 0; k < 30; k++) h += holdAcross(keep, 200);
out.push(h);
console.log(out.join(" "));
"####;

#[test]
fn matches_node() {
    // Read once per process, before the first VM: this file's only test.
    std::env::set_var("ZIPP_OPT", "1");
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
