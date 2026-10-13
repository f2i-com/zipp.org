"use strict";
// Alternation of words between word boundaries, counted over log-like text.
function mulberry32(a) {
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(14);
var W = ["request", "served", "error", "cache", "warning", "miss", "fatal", "hit", "panic", "user", "timeout", "retry", "errors", "ok"];
var text = "";
for (var i = 0; i < 50000; i++) text += W[(rnd() * W.length) | 0] + (i % 12 === 11 ? "\n" : " ");
var re = /\b(?:error|warning|fatal|panic|timeout)\b/g;
var re2 = /(?:cache (?:hit|miss)|user (?:request|retry))/g;
function w() {
  var a = text.match(re).length;
  var b = text.match(re2).length;
  return a + ":" + b;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("alternation " + best.toFixed(3) + " ms " + out);
