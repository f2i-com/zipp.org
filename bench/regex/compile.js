"use strict";
// Compile time: a 60-word alternation (the sandbox allows 64 alternatives), a
// /u pattern with property escapes, a /v class and a /gi pattern, each made
// unique per iteration so no cache can serve it.
var words = [];
for (var i = 0; i < 60; i++) words.push("w" + i.toString(36) + "x" + (i * 7919 % 1000));
var alt = words.join("|");
var serial = 0;
function w() {
  var s = 0;
  for (var i = 0; i < 200; i++) {
    var re = new RegExp("\\b(?:" + alt + "|z" + (serial++) + ")\\b");
    s += re.test("say w1x7919 now") ? 1 : 0;
  }
  for (var i = 0; i < 150; i++) {
    var re2 = new RegExp("[\\p{L}\\p{N}]+|\\p{Script=Han}" + (serial++), "u");
    s += re2.test("abc") ? 1 : 0;
    var re3 = new RegExp("[\\p{L}--\\p{Lu}]+" + (serial++), "v");
    s += re3.test("abc") ? 1 : 0;
    var re4 = new RegExp("(\\d+)-(\\w+)\\s*(?:x|y|z)?" + (serial++), "gi");
    s += re4.test("1-a") ? 1 : 0;
  }
  return s;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("compile " + best.toFixed(3) + " ms " + out);
