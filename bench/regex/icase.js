"use strict";
// Case-insensitive matching: a literal phrase, a class with a suffix, and a
// /iu literal with a non-ASCII fold.
function mulberry32(a) {
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(19);
var W = ["Hello", "WORLD", "running", "Jumping", "sing", "HeLLo World", "walked", "ThInKiNg", "straße", "STRASSE", "Σίσυφος"];
var text = "";
for (var i = 0; i < 40000; i++) text += W[(rnd() * W.length) | 0] + " ";
var re1 = /hello\s+world/gi;
var re2 = /\b[a-z]+ing\b/gi;
var re3 = /σίσυφοσ/giu;
function w() {
  return text.match(re1).length + ":" + text.match(re2).length + ":" + (text.match(re3) || []).length;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("icase " + best.toFixed(3) + " ms " + out);
