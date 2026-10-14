// Runs of reaction jobs calling one compiled int-tier callback, which the
// drain runs over a shared register window: long runs, alternating
// callbacks, two chains interleaving, a bail past the entry inside the
// window, a RangeError from deep self-recursion, and a self-recursive
// callback whose global binding is replaced between two runs.
"use strict";
var log = [];
function L(s) { log.push(s); }

function addOne(x) { return x + 1; }
function dbl(x) { return x * 2; }
function sub3(x) { return x - 3; }
function down(n) { return n <= 0 ? 0 : down(n - 1) + 1; }
var rf = function rfImpl(n) { return n < 2 ? n : rf(n - 1) + rf(n - 2); };
var rfOrig = rf;

var steps = [];
function step(name, fn) { steps.push([name, fn]); }

step("long run", function () {
  var p = Promise.resolve(0);
  for (var i = 0; i < 20000; i++) p = p.then(addOne);
  return p.then(function (v) { L("long " + v); });
});
step("alternating", function () {
  var p = Promise.resolve(1);
  for (var i = 0; i < 3000; i++) p = p.then(i & 1 ? addOne : sub3);
  return p.then(function (v) { L("alternating " + v); });
});
step("two chains interleave", function () {
  var a = Promise.resolve(0), b = Promise.resolve(100);
  for (var i = 0; i < 2000; i++) { a = a.then(addOne); b = b.then(sub3); }
  return Promise.all([a, b]).then(function (v) { L("interleave " + v.join(",")); });
});
step("bail inside the window", function () {
  var p = Promise.resolve(3);
  for (var i = 0; i < 60; i++) p = p.then(dbl);
  return p.then(function (v) { L("bail " + v); });
});
// 100,000 frames deep: too slow under ZIPP_GC_STRESS=1, which skips it.
if (typeof SKIP_DEEP === "undefined") step("deep recursion throws", function () {
  var p = Promise.resolve(0);
  for (var i = 0; i < 50; i++) p = p.then(addOne);
  return p.then(function () { return 2000000; }).then(down).then(function (v) { L("deep no " + v); }, function (e) { L("deep " + e.name); });
});
step("rebound self-call", function () {
  var p = Promise.resolve(10);
  for (var i = 0; i < 200; i++) p = p.then(function () { return 10; }).then(rfOrig);
  return p.then(function (v) {
    L("rf before " + v);
    rf = function (n) { return 1000 + n; };
    var q = Promise.resolve(10);
    for (var j = 0; j < 200; j++) q = q.then(function () { return 10; }).then(rfOrig);
    return q;
  }).then(function (v) { L("rf after " + v); rf = rfOrig; });
});
step("same callback, rejection handler", function () {
  var p = Promise.reject(5);
  for (var i = 0; i < 100; i++) p = p.then(null, addOne).then(function (v) { throw v; });
  return p.then(null, function (v) { L("reject run " + v); });
});

var chain = Promise.resolve();
steps.forEach(function (s) {
  chain = chain.then(function () { L("-- " + s[0]); return s[1](); });
});
chain.then(function () { console.log(log.join("\n")); }, function (e) { console.log(log.join("\n") + "\nFAILED " + e); });
