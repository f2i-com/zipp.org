// Jobs queued across many minor collections, holding the only references
// to their callbacks, arguments and dependents, then drained: a minor skips
// the jobs queued before the last collection, whose Values are old by then.
"use strict";
var log = [];
function L(s) { log.push(s); }

var total = 0;
// 1. Fresh objects and closures reachable only from queued reaction jobs.
for (var i = 0; i < 60000; i++) {
  var o = { v: i, pad: [i, i + 1] };
  Promise.resolve(o).then(function (x) { total += x.v + x.pad[1]; });
}
// 2. An old job's argument mutated afterwards to hold a young object.
var holders = [];
for (var j = 0; j < 2000; j++) {
  var h = { k: j };
  holders.push(h);
  Promise.resolve(h).then(function (x) { total += x.later.n; });
}
for (var a = 0; a < 20000; a++) { var junk = { a: a, b: [a] }; }
for (var m = 0; m < holders.length; m++) holders[m].later = { n: m * 2 };
holders = null;
// 3. Thenable jobs and async resumes queued between collections.
async function f(x) { var y = await x; return y + 1; }
for (var k = 0; k < 20000; k++) {
  f({ then: function (r) { r(1); } }).then(function (v) { total += v; });
  var junk2 = [k, { k: k }];
}
// 4. Promise.all over promises queued long before it settles.
var ps = [];
for (var q = 0; q < 5000; q++) ps.push(Promise.resolve({ q: q }).then(function (x) { return x.q; }));
Promise.all(ps).then(function (v) {
  var s = 0;
  for (var z = 0; z < v.length; z++) s += v[z];
  L("all " + s);
});
Promise.resolve().then(function () {}).then(function () {}).then(function () {}).then(function () {}).then(function () {
  L("total " + total);
  console.log(log.join("\n"));
});
