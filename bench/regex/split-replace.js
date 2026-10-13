"use strict";
// split with a pattern, replace with $n substitutions, and replace with a
// function.
function mulberry32(a) {
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(21);
var NAMES = ["ann", "bob", "cyd", "dee", "eve", "fay"];
var HOSTS = ["example", "mail", "corp", "uni"];
var lines = [];
for (var i = 0; i < 8000; i++) {
  var s = "";
  for (var j = 0; j < 6; j++) {
    s += NAMES[(rnd() * NAMES.length) | 0] + "@" + HOSTS[(rnd() * HOSTS.length) | 0] + ".com";
    s += rnd() < 0.5 ? " , " : ";";
  }
  lines.push(s);
}
var reSplit = /\s*[,;]\s*/;
var reMail = /(\w+)@(\w+)\.com/g;
var reWord = /\b[a-z]/g;
function up(c) { return c.toUpperCase(); }
function w() {
  var parts = 0, len = 0;
  for (var i = 0; i < lines.length; i++) {
    parts += lines[i].split(reSplit).length;
    len += lines[i].replace(reMail, "$2 at $1").length;
    len += lines[i].replace(reWord, up).charCodeAt(0);
  }
  return parts + ":" + len;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("split-replace " + best.toFixed(3) + " ms " + out);
