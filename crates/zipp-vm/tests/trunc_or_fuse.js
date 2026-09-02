"use strict";
// B267: in Tier C a truncation-only Add/AddInt followed by `LoadInt 0; Or`
// writes its wrapped Int straight into the Or's destination. Every case is a
// way the three-op shape can be entered with a non-Int operand (the f64 and
// concat paths must still run the real ToInt32) or observed after a deopt.
var out = [];
function log(tag, v) { out.push(tag + ":" + v); }

// 1. the closures shape: an int add that overflows on most iterations.
function rotate(v, k) { return ((v << k) | (v >>> (32 - k))) | 0; }
function step(value, salt) {
  value = (value ^ salt) | 0;
  return (rotate(value, 7) + 1013904223) | 0;
}
var v = 1;
for (var i = 0; i < 400000; i++) v = step(v, i & 255);
log("wrap", v);

// 2. both operand orders, Or dst equal to the Add dst, and AddInt form.
function orders(a, b) { var x = (a + b) | 0; var y = 0 | (a + b); var z = (a + 1) | 0; var w = 0 | (b + 2); return x + "," + y + "," + z + "," + w; }
var r2 = "";
for (var i = 0; i < 50000; i++) r2 = orders(2147483640 + (i & 15), i);
log("orders", r2);

// 3. double operands: the f64 path, then ToInt32 of the double sum.
function dbl(a, b) { return (a + b) | 0; }
var r3 = 0;
for (var i = 0; i < 50000; i++) { r3 = (r3 + dbl(1.5 * i, 0.25) + dbl(4294967296 + i, 0.5) + dbl(1e20, i) + dbl(-0.75, i)) | 0; }
log("double", r3 + "," + dbl(2.5, 3.5) + "," + dbl(4294967297, 0) + "," + dbl(NaN, 1) + "," + dbl(Infinity, 1) + "," + dbl(-2147483649, 0));

// 4. string / object / undefined operands: the concat and coercion paths.
function mixed(a, b) { return (a + b) | 0; }
var r4 = 0;
for (var i = 0; i < 40000; i++) { r4 = (r4 + mixed(i, "1") + mixed("7", i & 3) + mixed(i, undefined) + mixed(i, null) + mixed(i, true) + mixed(i, { valueOf: function () { return 3; } })) | 0; }
log("mixed", r4 + "," + mixed("12", "3") + "," + mixed("x", 1) + "," + mixed(1, undefined) + "," + mixed([1], 2));

// 5. a deopt right after the fused triple must see all three registers.
var trap = 0;
function afterFuse(a, b, o) { var t = (a + b) | 0; return t + o.k; }
var objs = [{ k: 1 }, { k: 2 }];
var r5 = 0;
for (var i = 0; i < 60000; i++) r5 = (r5 + afterFuse(i, 2147483647, objs[i & 1])) | 0;
Object.defineProperty(objs[0], "k", { get: function () { trap++; return 5; } });
for (var i = 0; i < 2000; i++) r5 = (r5 + afterFuse(i, 2147483647, objs[i & 1])) | 0;
log("deopt", r5 + "," + trap);

// 6. chains: `(a + b + c) | 0` and a loop-carried accumulator.
function chain(a, b, c) { return (a + b + c) | 0; }
var acc = 0;
for (var i = 0; i < 100000; i++) acc = chain(acc, 1013904223, i);
log("chain", acc);

console.log(out.join("|"));
