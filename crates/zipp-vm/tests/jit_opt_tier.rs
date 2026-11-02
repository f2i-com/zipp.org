//! Tier O (the optimizing tier, `ZIPP_OPT=1`) against Node: Number code, branches, loops, own-property reads, strict equality, and deopts mid-run.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"6793104 24362.5 33 16.5 11 16 6 1 0 5 NaN NaN true true true true false true true false false false 0 true 2147483648 -1.5 -4 true false true true false false false true true true false false 0 255 12 0 40 41 7 0 4294967295 2147483648 0 0 2147483648 0.5 10 111 118 6801657.125 24362.5"####;

const PROGRAM: &str = r####""use strict";
// Tier O's first slice: Number code, branches, loops, own-property reads, and deopts mid-run.
function sumTo(n) { let s = 0; for (let i = 0; i < n; i++) { s = s + i * 0.5; } return s; }
function mix(a, b) { return (a * 3 + b) / 2 - (a | b) + (a >>> 1) + (b << 2); }
function cmpf(a, b) { return a < b ? 1 : (a === b ? 0 : -1); }
function dot(o, p) { return o.x * p.x + o.y * p.y; }
function isZero(x) { return !x; }
function neg(x) { return -x; }
function eqs(a, b) { return a === b; }
function nes(a, b) { return a !== b; }
function collatz(n) { let steps = 0; while (n !== 1) { n = (n & 1) ? 3 * n + 1 : n / 2; steps++; } return steps; }
function clampi(x) { if (x < 0) return 0; if (x > 255) return 255; return x | 0; }
function bits(a) { return ((a << 3) ^ (a >> 2)) & 0xffff; }
function ushr(a) { return a >>> 0; }
function inc(a) { let b = a; b++; return b; }
const out = [];
let acc = 0;
for (let k = 0; k < 200; k++) {
  acc += sumTo(k % 50) + mix(k, k + 1) + cmpf(k % 7, 3) + clampi(k * 3 - 100) + bits(k * 977) + collatz(k + 1);
}
out.push(acc);
const pts = [];
for (let k = 0; k < 20; k++) pts.push({ x: k * 0.25, y: k - 3 });
let d = 0;
for (let k = 0; k < 400; k++) d += dot(pts[k % 20], pts[(k * 7) % 20]);
out.push(d);
// Deopts: a string, objects with valueOf (each call counts), another shape, a missing field.
let calls = 0;
const vo = { valueOf() { calls++; return 4; } };
out.push(sumTo("12"), mix("3", 4), mix(vo, 2), mix(2, vo), calls, cmpf("a", "b"), cmpf("b", "b"));
out.push(dot({ y: 2, x: 3 }, { x: 1, y: 1 }), dot({ x: 1 }, { x: 2, y: 5 }), String(dot({}, { x: 1, y: 1 })));
out.push(isZero(0), isZero(-0), isZero(NaN), isZero(""), isZero("a"), isZero(null), isZero(undefined), isZero({}), isZero(3), isZero(0.5));
out.push(neg(0), Object.is(neg(0), -0), neg(-2147483648), neg(1.5), neg("4"));
out.push(eqs(1, 1.0), eqs(NaN, NaN), eqs(0, -0), eqs("ab", "a" + "b"), eqs({}, {}), eqs(null, undefined), eqs(true, 1), eqs(pts[1], pts[1]));
out.push(nes(1, 2), nes(NaN, NaN), nes("x", "x"), nes(-0, 0));
out.push(clampi(-5), clampi(300), clampi(12.7), clampi(NaN), clampi("40"));
out.push(bits(2 ** 32 + 5), bits(-1.5), bits(1e20), ushr(-1), ushr(2 ** 31), ushr(-0.5), ushr(NaN));
out.push(inc(2147483647), inc(-0.5), inc("9"), collatz(27), collatz(97));
for (let k = 0; k < 100; k++) acc += sumTo(10) + mix(k, 3) + dot(pts[k % 20], pts[3]);
out.push(acc, d);
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
