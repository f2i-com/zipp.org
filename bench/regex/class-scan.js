"use strict";
// Character classes: every identifier and every number in code-like text, by
// a global match and an exec loop.
function mulberry32(a) {
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(13);
var TOK = ["var", "x1", "=", "foo_bar", "(", ")", ";", "123", "0x1F", "+", "obj.prop", "[", "]", "{", "}", "if", "return", "3.14", "_tmp$", "\"str\""];
var src = "";
for (var i = 0; i < 60000; i++) src += TOK[(rnd() * TOK.length) | 0] + (rnd() < 0.1 ? "\n" : " ");
var reId = /[A-Za-z_$][A-Za-z0-9_$]*/g;
var reNum = /[0-9]+(?:\.[0-9]+)?/g;
function w() {
  var m = src.match(reId);
  var n = m.length, len = 0;
  reNum.lastIndex = 0;
  var r;
  while ((r = reNum.exec(src)) !== null) len += r[0].length;
  return n + ":" + len;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("class-scan " + best.toFixed(3) + " ms " + out);
