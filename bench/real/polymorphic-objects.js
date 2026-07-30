"use strict";
// real-world bench 9: megamorphic property access. The SAME property name
// ("val") lives at different slot positions across 8 distinct object layouts
// (incl. a proto-chain hit and an accessor-backed shape) read in a hot loop;
// plus dictionary-mode churn (add 60 props, delete half, re-add, read) and
// proto-chain walks. Deterministic sums.
//
// SCOPE NOTE — this row STOPS AT THE IC WAY COUNT. `shapes[i & 7]` means every
// site sees exactly eight receivers, and `JIT_IC_WAYS` is 8, so the ways always
// fill and never thrash. The identity-keyed native IC's 8->9 receiver cliff (B44:
// 5.67ns -> 11.67ns, where node goes 0.67 -> 0.33) is INVISIBLE here, and this row
// can therefore neither justify nor reject a shape-keyed IC.
//
// It also sums three unrelated mechanisms — megamorphic reads, dictionary churn,
// prototype walks — into one wall time, so a win in one can hide a loss in
// another. `class-prototype-hot` demonstrated exactly that failure: a 6x
// method-call loss sat behind an accessor win for months.
//
// Kept byte-identical anyway, because the retained headline is a geomean over this
// exact file. `polymorphic-objects-v2.js` is the split, past-the-cliff sibling;
// `property-ic-shapes.js` is the receiver-count acceptance benchmark.

// ---- 8 layouts, same property name at different positions ----
function mkAccessor(seed) {
  var o = { hidden: seed, pad: 0 };
  Object.defineProperty(o, "val", {
    get: function () { return this.hidden; },
    set: function (x) { this.hidden = x | 0; },
    enumerable: true, configurable: true
  });
  return o;
}
var shapes = [
  { val: 11, a: 1 },
  { a: 1, val: 22 },
  { a: 1, b: 2, val: 33 },
  { a: 1, b: 2, c: 3, d: 4, val: 44 },
  { x: 9, val: 55, y: 8, z: 7 },
  { p: 1, q: 2, r: 3, s: 4, t: 5, u: 6, val: 66 },
  (function () { var o = Object.create({ val: 77 }); o.own = 1; return o; })(),
  mkAccessor(88)
];

var READS = 8000000;
var mega = 0;
for (var i = 0; i < READS; i++) {
  mega = (mega + shapes[i & 7].val) | 0;
}
// mixed read+write pass over the same shapes (accessor setter included)
var WRITES = 2000000;
for (var i = 0; i < WRITES; i++) {
  var o = shapes[i & 7];
  o.val = (i & 255) + 1;
  mega = (mega + o.val) | 0;
}

// ---- dictionary-style objects: add 60 props, delete half, re-add, read ----
var DOBJS = 30000;
var dictSum = 0;
for (var d = 0; d < DOBJS; d++) {
  var obj = {};
  for (var p = 0; p < 60; p++) obj["prop_" + p] = p + (d & 7);
  for (var p = 0; p < 60; p += 2) delete obj["prop_" + p];
  for (var p = 0; p < 60; p += 2) obj["prop_" + p] = p * 2 + (d & 3);
  for (var p = 0; p < 60; p++) dictSum = (dictSum + obj["prop_" + p]) | 0;
  var keys = 0;
  for (var k in obj) keys++;
  dictSum = (dictSum + keys) | 0;
}

// ---- proto-chain property walks (depth 6, reads resolving at each level) ----
var p0 = { d0: 1 };
var p1 = Object.create(p0); p1.d1 = 2;
var p2 = Object.create(p1); p2.d2 = 3;
var p3 = Object.create(p2); p3.d3 = 4;
var p4 = Object.create(p3); p4.d4 = 5;
var p5 = Object.create(p4); p5.d5 = 6;
var WALKS = 4000000;
var protoSum = 0;
for (var i = 0; i < WALKS; i++) {
  protoSum = (protoSum + p5.d0 + p5.d2 + p5.d4 + p5.d5) | 0;
}

console.log("mega=" + mega + " dictSum=" + dictSum + " protoSum=" + protoSum +
  " accFinal=" + shapes[7].val + " protoShape=" + shapes[6].val);
