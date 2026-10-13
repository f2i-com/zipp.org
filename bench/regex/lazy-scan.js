"use strict";
// A lazy dot-star between delimiters over a subject with no line terminator:
// each match's (.*?) must stop at the first closing tag, not look at the rest
// of the subject. Sized so the start build fits the module's budget.
var html = "";
var TAGS = ["b", "i", "em", "span", "code"];
for (var i = 0; i < 2500; i++) {
  var t = TAGS[i % TAGS.length];
  html += "<" + t + ">item " + i + "</" + t + "> and ";
}
var reB = /<b>(.*?)<\/b>/g;
var reAny = /<(\w+)>.*?<\/\1>/g;
function w() {
  var n = 0, m;
  reB.lastIndex = 0;
  while ((m = reB.exec(html)) !== null) n += m[1].length;
  reAny.lastIndex = 0;
  while ((m = reAny.exec(html)) !== null) n++;
  return n;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("lazy-scan " + best.toFixed(3) + " ms " + out);
