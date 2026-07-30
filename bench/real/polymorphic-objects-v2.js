"use strict";
// M0.3 benchmark 2 — the sibling `polymorphic-objects.js` cannot be.
//
// The historical row is kept BYTE-IDENTICAL so its measurements stay comparable,
// which also freezes its blind spot: it reads `shapes[i & 7].val`, so every site
// sees exactly eight receivers — the inline-cache way count — and the row can
// neither justify nor reject the shape-keyed IC that M3 proposes. It also mixes
// megamorphic reads, dictionary churn and prototype walks into ONE number, so a
// win in any of them can hide a loss in another (the same aggregation that let
// `class-prototype-hot` hide a 6x method-call loss behind an accessor win).
//
// This file splits them and adds the cases past the cliff. Phases:
//
//   sameLayoutManyInstances  — ONE shape, 1024 identities. The case the current
//                              row omits entirely, and the one a shape-keyed IC
//                              should collapse to monomorphic speed.
//   layouts8 / 9 / 16        — the way count, one past it, and twice it. 8 vs 9
//                              is the cliff; 9 vs 16 says whether it deepens.
//   dynamicChurn             — add / delete / re-add, i.e. DICT transitions.
//   protoWalk                — depth-5 inherited data read, isolated.
//   enumerate                — for-in / Object.keys, isolated: these are key
//                              enumeration, not property access, and they were
//                              never separable in the original row.
//
// Each phase prints its own checksum so a change that alters a value is visible
// per phase rather than as one summed integer.

var ITERS = 3000000;
var out = [];

// ── phase: one layout, MANY instances ────────────────────────────────────────
function mkSame(n) {
  var a = new Array(n);
  for (var i = 0; i < n; i++) a[i] = { a: 1, b: 2, val: i };
  return a;
}
function readSame(objs, n) {
  var sum = 0, k = 0;
  for (var i = 0; i < ITERS; i++) {
    sum = (sum + objs[k].val) | 0;
    k++;
    if (k === n) k = 0;
  }
  return sum;
}
out.push("sameLayout/1024=" + readSame(mkSame(1024), 1024));

// ── phases: 8 / 9 / 16 distinct LAYOUTS ──────────────────────────────────────
// `val` sits at a different slot in each layout, so this is genuine layout
// polymorphism rather than receiver-identity polymorphism. Built by an explicit
// list per count so the layouts are the same objects the original row uses for
// its first eight, making 8-layout numbers directly comparable to it.
function mkLayouts(n) {
  var a = new Array(n);
  for (var i = 0; i < n; i++) {
    var o = {};
    // i leading pad keys ⇒ `val` at slot i ⇒ n distinct shapes.
    for (var p = 0; p < i; p++) o["p" + p] = p;
    o.val = i + 1;
    a[i] = o;
  }
  return a;
}
function readLayouts(objs, n) {
  var sum = 0, k = 0;
  for (var i = 0; i < ITERS; i++) {
    sum = (sum + objs[k].val) | 0;
    k++;
    if (k === n) k = 0;
  }
  return sum;
}
var LAYOUT_COUNTS = [8, 9, 16];
for (var li = 0; li < LAYOUT_COUNTS.length; li++) {
  var ln = LAYOUT_COUNTS[li];
  out.push("layouts/" + ln + "=" + readLayouts(mkLayouts(ln), ln));
}

// ── phase: dynamic add / delete / re-add (DICT churn) ────────────────────────
// Isolated from the reads above: a delete sends the object to `shape::DICT`
// permanently, so mixing this into a read phase measures dictionary lookup and
// calls it megamorphic caching.
var CHURN = 4000;
var churnSum = 0;
for (var c = 0; c < CHURN; c++) {
  var d = {};
  for (var k1 = 0; k1 < 30; k1++) d["k" + k1] = k1;
  for (var k2 = 0; k2 < 30; k2 += 2) delete d["k" + k2];
  for (var k3 = 0; k3 < 30; k3 += 2) d["k" + k3] = k3 + 1;
  for (var k4 = 0; k4 < 30; k4++) churnSum = (churnSum + d["k" + k4]) | 0;
}
out.push("dynamicChurn=" + churnSum);

// ── phase: depth-5 prototype data read ───────────────────────────────────────
var q0 = { d0: 1 };
var q1 = Object.create(q0); q1.d1 = 2;
var q2 = Object.create(q1); q2.d2 = 3;
var q3 = Object.create(q2); q3.d3 = 4;
var q4 = Object.create(q3); q4.d4 = 5;
var q5 = Object.create(q4); q5.d5 = 6;
var protoSum = 0;
for (var i5 = 0; i5 < ITERS; i5++) {
  protoSum = (protoSum + q5.d0 + q5.d2 + q5.d4) | 0;
}
out.push("protoWalk=" + protoSum);

// ── phase: enumeration ───────────────────────────────────────────────────────
// Key enumeration, not property access. `for-in` also walks the prototype chain
// for enumerable inherited keys, so it and `Object.keys` are separate lines.
var ENUM = 200000;
var forInCount = 0, keysCount = 0;
var esrc = { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6, g: 7, h: 8 };
for (var e1 = 0; e1 < ENUM; e1++) {
  for (var kk in esrc) forInCount = (forInCount + esrc[kk]) | 0;
}
for (var e2 = 0; e2 < ENUM; e2++) {
  keysCount = (keysCount + Object.keys(esrc).length) | 0;
}
out.push("forIn=" + forInCount + " objectKeys=" + keysCount);

console.log(out.join(" "));
