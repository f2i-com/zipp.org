"use strict";
// Backreferences: doubled words, and matched open/close tags with a lazy body.
function mulberry32(a) {
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(16);
var W = ["the", "cat", "sat", "on", "mat", "and", "dog", "ran", "far", "away"];
var text = "";
for (var i = 0; i < 30000; i++) {
  var x = W[(rnd() * W.length) | 0];
  text += x + " ";
  if (rnd() < 0.03) text += x + " ";
}
var TAGS = ["b", "i", "em", "span", "code"];
var html = "";
for (var i = 0; i < 1500; i++) {
  var t = TAGS[(rnd() * TAGS.length) | 0];
  html += "<" + t + " class=\"c" + i + "\">text " + i + "</" + t + "> plain ";
}
var reDup = /\b(\w+)\s+\1\b/g;
var reTag = /<([a-z]+)[^>]*>(.*?)<\/\1>/g;
function w() {
  var a = text.match(reDup).length;
  var b = 0, m;
  reTag.lastIndex = 0;
  while ((m = reTag.exec(html)) !== null) b += m[2].length;
  return a + ":" + b;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("backref " + best.toFixed(3) + " ms " + out);
