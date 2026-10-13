"use strict";
// Capture groups: a timestamp split into six groups, exec per line, and named
// groups read back from the result.
function mulberry32(a) {
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(15);
function p2(n) { return n < 10 ? "0" + n : "" + n; }
var lines = [];
for (var i = 0; i < 20000; i++) {
  lines.push("evt " + i + " at 20" + p2((rnd() * 30) | 0) + "-" + p2(1 + ((rnd() * 12) | 0)) + "-" + p2(1 + ((rnd() * 28) | 0)) +
    "T" + p2((rnd() * 24) | 0) + ":" + p2((rnd() * 60) | 0) + ":" + p2((rnd() * 60) | 0) + " user=u" + ((rnd() * 1000) | 0));
}
var re = /(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})/;
var reNamed = /user=(?<name>[a-z]+)(?<id>\d+)/;
function w() {
  var s = 0;
  for (var i = 0; i < lines.length; i++) {
    var m = re.exec(lines[i]);
    s = (s + (+m[1]) + (+m[2]) + (+m[3]) + (+m[4]) + (+m[5]) + (+m[6])) | 0;
    var g = reNamed.exec(lines[i]).groups;
    s = (s + g.name.length + (+g.id)) | 0;
  }
  return s;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("captures " + best.toFixed(3) + " ms " + out);
