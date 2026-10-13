"use strict";
// Literal search over ASCII text: a test() per line and a global count over
// one long string (the start-prefilter and the byte searcher).
function mulberry32(a) {
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(11);
var WORDS = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "theta", "lambda", "sigma", "omega", "kappa", "iota"];
var lines = [];
for (var i = 0; i < 4000; i++) {
  var s = "";
  for (var j = 0; j < 30; j++) s += WORDS[(rnd() * WORDS.length) | 0] + " ";
  if (i % 7 === 0) s += "needle-in-haystack ";
  lines.push(s);
}
var big = lines.join("\n");
var reLine = /needle-in-haystack/;
var reAll = /lambda sigma/g;
function w() {
  var n = 0;
  for (var r = 0; r < 4; r++) {
    for (var i = 0; i < lines.length; i++) if (reLine.test(lines[i])) n++;
    var m = big.match(reAll);
    n += m ? m.length : 0;
  }
  return n;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("literal-ascii " + best.toFixed(3) + " ms " + out);
