//! JS `%` on Numbers: two integral doubles under 2^53 take the exact integer
//! remainder (`js_rem`), everything else the IEEE fmod: signs, -0, zero
//! divisors, NaN, infinities, fractions, past 2^53 and coerced operands, cold
//! and hot (the interpreter's loops and compiled code). Node's output.
const EXPECTED: &str = r####"2507 0 0 0 0 0 0 0 0 0 0 0 0 0 NaN NaN 0 0 0 0 0 NaN -0 -0 -0 -0 -0 -0 -0 -0 -0 -0 -0 -0 -0 NaN NaN -0 -0 -0 -0 -0 NaN 0 0 1 1 1 1 1 1 1 1 1 1 1 NaN NaN 0 1 1 1 1 NaN -0 -0 -1 -1 -1 -1 -1 -1 -1 -1 -1 -1 -1 NaN NaN -0 -1 -1 -1 -1 NaN 0 0 1 1 1 1 0 7 7 7 7 7 7 NaN NaN 0 2 2 7 7 NaN -0 -0 -1 -1 -1 -1 -0 -7 -7 -7 -7 -7 -7 NaN NaN -0 -2 -2 -7 -7 NaN 0 0 1 1 1 1 1 7 2147483647 2147483647 2147483647 2147483647 2147483647 NaN NaN 0 2 2 2147483647 2147483647 NaN -0 -0 -0 -0 -2 -2 -2 -8 -0 -0 -2147483648 -2147483648 -2147483648 NaN NaN -0 -0.5 -0.5 -2147483648 -2147483648 NaN 0 0 0 0 2 2 2 8 0 0 2147483648 2147483648 2147483648 NaN NaN 0 0.5 0.5 2147483648 2147483648 NaN 0 0 1 1 1 1 0 3 5 5 2147483653 2147483653 2147483653 NaN NaN 0 0.5 0.5 2147483653 2147483653 NaN -0 -0 -1 -1 -1 -1 -0 -3 -5 -5 -2147483653 -2147483653 -2147483653 NaN NaN -0 -0.5 -0.5 -2147483653 -2147483653 NaN 0 0 1 1 0 0 3 5 2147483647 2147483647 4294967295 4294967295 4294967295 NaN NaN 0 0 0 4294967295 4294967295 NaN 0 0 1 1 1 1 5 5 7 7 7 12884901895 12884901895 NaN NaN 0 0 0 12884901895 12884901895 NaN 0 0 1 1 1 1 3 1 2147483647 2147483647 4294967295 0 9007199254740991 NaN NaN 0 1 1 9007199254740991 9007199254740991 NaN -0 -0 -1 -1 -1 -1 -3 -1 -2147483647 -2147483647 -4294967295 -0 -9007199254740991 NaN NaN -0 -1 -1 -9007199254740991 -9007199254740991 NaN 0 0 0 0 2 2 4 2 0 0 0 1 0 NaN NaN 0 2 2 9007199254740992 9007199254740992 NaN 0 0 0 0 1 1 6 4 2 2 2 3 2 NaN NaN 0 1.5 1.5 9007199254740994 9007199254740994 NaN -0 -0 -0 -0 -2 -2 -4 -2 -0 -0 -0 -1 -0 NaN NaN -0 -2 -2 -9007199254740992 -9007199254740992 NaN 0 0 0 0 0 0 1 0 0 0 0 3630557584031773 0 NaN NaN 0 0 0 1e+300 1e+300 NaN -0 -0 -0 -0 -0 -0 -1 -0 -0 -0 -0 -3630557584031773 -0 NaN NaN -0 -0 -0 -1e+300 -1e+300 NaN 0.5 0.5 0.5 0.5 0.5 0.5 0.5 0.5 0.5 0.5 0.5 0.5 0.5 NaN NaN 0 0.5 0.5 0.5 0.5 NaN -0.5 -0.5 -0.5 -0.5 -0.5 -0.5 -0.5 -0.5 -0.5 -0.5 -0.5 -0.5 -0.5 NaN NaN -0 -0.5 -0.5 -0.5 -0.5 NaN 0.5 0.5 1.5 1.5 2.5 2.5 5.5 5.5 5.5 5.5 5.5 5.5 5.5 NaN NaN 0 0.5 0.5 5.5 5.5 NaN -0.5 -0.5 -1.5 -1.5 -2.5 -2.5 -5.5 -5.5 -5.5 -5.5 -5.5 -5.5 -5.5 NaN NaN -0 -0.5 -0.5 -5.5 -5.5 NaN 1e-310 1e-310 1e-310 1e-310 1e-310 1e-310 1e-310 1e-310 1e-310 1e-310 1e-310 1e-310 1e-310 NaN NaN 1e-310 1e-310 1e-310 1e-310 1e-310 NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN NaN
true
97559483
-0 -0 0 NaN 0 -0
2 1 0 0 NaN -0
-13"####;

const PROGRAM: &str = r####"// JS `%` on Numbers: integral doubles, signs, -0, zero divisors, NaN,
// infinities, fractions, past 2^53, coerced operands; cold and hot.
"use strict";
const T = 2 ** 31, B = 2 ** 53;
const xs = [0, -0, 1, -1, 7, -7, 2147483647, -2147483648, T, T + 5, -T - 5, 4294967295, 4294967296 * 3 + 7,
  B - 1, -(B - 1), B, B + 2, -B, 1e300, -1e300, 0.5, -0.5, 5.5, -5.5, 1e-310, Infinity, -Infinity, NaN];
const ys = [1, -1, 2, -2, 3, -3, 7, 10, T, -T, 4294967296, B - 1, B, 0, -0, 0.5, 2.5, -2.5, Infinity, -Infinity, NaN];
function fmt(v) { return Object.is(v, -0) ? "-0" : String(v); }
function table(f) { const out = []; for (const x of xs) for (const y of ys) out.push(fmt(f(x, y))); return out.join(" "); }
const rem = (x, y) => x % y;
let cold = table(rem);
console.log(cold.length, cold);
// Hot: the same table many times (compiled code and the interpreter's loops).
let hot = "";
for (let i = 0; i < 300; i++) hot = table(rem);
console.log(hot === cold);
// The bench's shape: a uint32 past 2^31 modulo a small int.
let seed = 7, acc = 0;
for (let i = 0; i < 200000; i++) { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; acc = (acc + ((seed >>> 0) % 977)) | 0; }
console.log(acc);
// Int-tagged edge cases and coerced operands.
console.log(fmt(-2147483648 % -1), fmt(-6 % 3), fmt(6 % -3), fmt(5 % 0), fmt(0 % 5), fmt(-0 % 5));
console.log("17" % 5, true % 2, null % 3, fmt([] % 4), "x" % 2, fmt({ valueOf() { return -12; } } % 4));
let m = 0;
for (let i = -5000; i < 5000; i++) m = (m + (i % 7) + ((i * 2654435761) % 13)) | 0;
console.log(m);
"####;

#[test]
fn matches_node() {
    let out = zipp_vm::run(PROGRAM).expect("compiles");
    assert_eq!(out.error, None, "{:?}", out.errput);
    assert_eq!(out.output.join("\n"), EXPECTED);
}
