"use strict";
// A sticky (/y) tokenizer: one exec per token at lastIndex, the lexer shape.
function mulberry32(a) {
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(20);
var TOK = ["let", " ", "x", " = ", "42", ";", "\n", "foo", "(", "bar", ", ", "3.5", ")", " + ", "\"hi\"", "// c\n", "  ", "if", " { ", "}"];
var src = "";
for (var i = 0; i < 40000; i++) src += TOK[(rnd() * TOK.length) | 0];
var reWs = /[ \t\n]+|\/\/[^\n]*\n/y;
var reId = /[A-Za-z_$][\w$]*/y;
var reNum = /\d+(?:\.\d+)?/y;
var reStr = /"(?:[^"\\\n]|\\.)*"/y;
var rePunct = /[=;(),+{}]/y;
function w() {
  var pos = 0, n = 0, h = 0;
  var res = [reWs, reId, reNum, reStr, rePunct];
  while (pos < src.length) {
    var hit = false;
    for (var k = 0; k < res.length; k++) {
      var re = res[k];
      re.lastIndex = pos;
      var m = re.exec(src);
      if (m !== null) { pos = re.lastIndex; n++; h = (h + k * m[0].length) | 0; hit = true; break; }
    }
    if (!hit) pos++;
  }
  return n + ":" + h;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("sticky-tokenizer " + best.toFixed(3) + " ms " + out);
