"use strict";
// Lookbehind and lookahead: prices after a dollar sign, words not preceded by
// "no ", and numbers followed by a unit.
function mulberry32(a) {
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(17);
var text = "";
for (var i = 0; i < 25000; i++) {
  var r = rnd();
  if (r < 0.2) text += "$" + ((rnd() * 1000) | 0) + "." + ((rnd() * 90 + 10) | 0) + " ";
  else if (r < 0.35) text += ((rnd() * 100) | 0) + "kg ";
  else if (r < 0.45) text += "no milk ";
  else text += "milk " + ((rnd() * 50) | 0) + " ";
}
var rePrice = /(?<=\$)\d+(?:\.\d\d)?/g;
var reNeg = /(?<!no )milk/g;
var reAhead = /\d+(?=kg)/g;
function w() {
  return text.match(rePrice).length + ":" + text.match(reNeg).length + ":" + text.match(reAhead).length;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("lookbehind " + best.toFixed(3) + " ms " + out);
