//! Tier O inlines callees whose branches the call's arguments decide
//! (default parameters, === undefined of a Number), and calls the rest,
//! against Node's output.
#![cfg(all(feature = "jit", target_arch = "x86_64"))]

const EXPECTED: &str = r####"21685694/13970026 224 x20NaN NaN 1 1 -1 s 13748700"####;

const PROGRAM: &str = r####"// Tier O inlines callees whose branches the call's arguments decide.
"use strict";
const out = [];
function rotate(value, amount = 5) { return (value << amount) | (value >>> (32 - amount)); }
function pick(a, b = 2, c = a * 3) { return a + b * 10 + c * 100; }
function isNil(x) { return x === undefined || x === null ? 1 : 0; }
function notNil(x) { return x !== undefined ? x : -1; }
function loop(n) {
  let h = 0x1234567, t = 0;
  for (let i = 0; i < n; i++) {
    h = rotate(h, (i & 7) + 1) ^ i;
    h = (h + rotate(h)) | 0;
    t += pick(i & 15) + pick(i & 3, i & 1) + pick(1, 2, i) + isNil(i) + notNil(i);
  }
  return [h, t];
}
let r = "";
for (let k = 0; k < 60; k++) r = loop(500).join("/");
out.push(r);
// The same callees given what they cannot fold: undefined, strings, null.
out.push(rotate(7, undefined), pick("x"), pick(1, undefined, "z"), isNil(null), isNil(undefined), notNil(undefined), notNil("s"));
function loop2(n) { let t = 0; for (let i = 0; i < n; i++) t += pick(i, i & 1 ? undefined : i) + notNil(i & 1 ? i : undefined); return t; }
let r2 = "";
for (let k = 0; k < 60; k++) r2 = loop2(300);
out.push(r2);
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
