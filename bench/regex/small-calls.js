"use strict";
// Many calls on short subjects, where per-call setup outweighs the matching:
// matchAll over a short string (the module screen's regexp-matchall), test()
// on short tokens, and exec with captures.
var reNum = /^-?\d+(?:\.\d+)?$/;
var reKey = /^([a-z_]+)=(.*)$/;
var TOK = ["12", "-3.5", "abc", "x=1", "4.0", "key_a=val", "7", "q"];
function w() {
  var s = 0;
  for (var i = 0; i < 6000; i++) {
    for (var m of "k12;k34;k56".matchAll(/k(\d+)/g)) s += Number(m[1]);
  }
  for (var i = 0; i < 40000; i++) {
    var t = TOK[i & 7];
    if (reNum.test(t)) s++;
    var m2 = reKey.exec(t);
    if (m2 !== null) s += m2[1].length + m2[2].length;
  }
  return s;
}
// The minimum of seven runs, or of as many as fit in about three seconds.
var best = 1e9, out, spent = 0;
for (var k = 0; k < 7 && spent < 3000; k++) { var t0 = performance.now(); out = w(); var dt = performance.now() - t0; spent += dt; if (dt < best) best = dt; }
console.log("small-calls " + best.toFixed(3) + " ms " + out);
