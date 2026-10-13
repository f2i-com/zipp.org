"use strict";
// /u property escapes and /v set operations over multilingual text with
// astral characters.
function mulberry32(a) {
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(18);
var W = ["hello", "κόσμε", "мир", "世界", "café", "naïve", "𝐀𝐁", "42", "١٢٣", "😀", "Straße", "ÉCOLE"];
var text = "";
for (var i = 0; i < 30000; i++) text += W[(rnd() * W.length) | 0] + (rnd() < 0.1 ? ", " : " ");
var reL = /\p{L}+/gu;
var reGreek = /\p{Script=Greek}+/gu;
var reNd = /[\p{Nd}\p{Emoji_Presentation}]/gu;
var reV = /[\p{L}--[a-z]]+/gv;
function w() {
  return text.match(reL).length + ":" + text.match(reGreek).length + ":" + text.match(reNd).length + ":" + text.match(reV).length;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("unicode-props " + best.toFixed(3) + " ms " + out);
