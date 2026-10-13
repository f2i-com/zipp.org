"use strict";
// Backtracking-heavy but bounded matching: nested quantifiers that fail late,
// a lazy dot-star between anchors, and an email validator over many strings.
function mulberry32(a) {
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
var rnd = mulberry32(22);
var subjects = [];
for (var i = 0; i < 3000; i++) {
  var s = "";
  var n = 4 + ((rnd() * 8) | 0);
  for (var j = 0; j < n; j++) s += "word" + j + (rnd() < 0.8 ? " " : "-");
  subjects.push(s + (rnd() < 0.5 ? "end" : "!"));
}
var mails = [];
for (var i = 0; i < 6000; i++) mails.push((rnd() < 0.8 ? "user" + i + "@host" + (i % 13) + ".org" : "bad user" + i + "@@x"));
var reNest = /^(?:\w+\s)+end$/;
var reLazy = /^word0.*?word3.*?end$/;
var reMail = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
function w() {
  var a = 0, b = 0, c = 0;
  for (var i = 0; i < subjects.length; i++) {
    if (reNest.test(subjects[i])) a++;
    if (reLazy.test(subjects[i])) b++;
  }
  for (var i = 0; i < mails.length; i++) if (reMail.test(mails[i])) c++;
  return a + ":" + b + ":" + c;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("backtrack " + best.toFixed(3) + " ms " + out);
