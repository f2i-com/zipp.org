"use strict";
// M0.3 benchmark 1 — the RECEIVER-COUNT CLIFF the existing suite cannot see.
//
// `codegen::IcEntry` is IDENTITY keyed: a way holds `obj_bits`, and there are
// `JIT_IC_WAYS` = 8 of them per site. A site that sees NINE different objects
// therefore misses on every access even when all nine have exactly the SAME
// shape and the property sits at the same slot. B44 measured the step:
//
//     receivers        1      8      9     16   1024
//     zipp          4.67   5.67  11.67  11.67  11.67   ns/access
//     node          1.67   0.67   0.33   0.33   0.67   ns/access
//
// `polymorphic-objects.js` uses exactly EIGHT layouts — `i & 7` — so it sits
// precisely at the last way and never crosses the cliff. It can neither justify
// nor reject a shape-keyed IC, which is why this file exists: it is the
// acceptance benchmark for milestone M3, and it must be committed BEFORE the
// implementation it judges.
//
// METHOD NOTES, both of which are easy to get wrong:
//
//  * The wrap is an explicit counter, never `i & (n - 1)`. Nine and 1024 are not
//    both powers of two, and a mask would silently benchmark 8 and 1024
//    receivers while the phase name claimed 9 and 1024.
//  * Every phase reads a DISTINCT property name so the phases cannot share an
//    inline-cache site. Sharing one name would let phase N arrive at a site
//    phase N-1 had already driven megamorphic, and the per-phase numbers would
//    measure the order they ran in.
//
// Read the phases against each other, not in isolation: 1 vs 8 is the way-fill
// cost, 8 vs 9 is the cliff, 9 vs 16 vs 1024 is whether anything degrades
// FURTHER past it, and sameShape vs diffShape separates "many receivers" from
// "many layouts" — a shape-keyed IC should collapse the former and keep the
// latter.

var ACCESSES = 4000000;

// A checksum per phase, printed as one line. Any change to a fast path that
// alters a value shows up here rather than in a timing.
var out = [];

// ── build `n` receivers that all share ONE shape ──────────────────────────────
// Same key inserted in the same order ⇒ one shape, one slot index, n identities.
function sameShape(n, prop) {
  var a = new Array(n);
  for (var i = 0; i < n; i++) {
    var o = {};
    o.pad0 = i;
    o.pad1 = i + 1;
    o[prop] = i;
    a[i] = o;
  }
  return a;
}

// ── build `n` receivers with n DISTINCT shapes ────────────────────────────────
// `prop` lands at a different slot in each, so no single (shape → slot) answer
// exists. This is the control: a shape-keyed IC must NOT improve it.
function diffShapes(n, prop) {
  var a = new Array(n);
  for (var i = 0; i < n; i++) {
    var o = {};
    for (var p = 0; p < (i % 12); p++) o["f" + p] = p;
    o[prop] = i;
    a[i] = o;
  }
  return a;
}

// ── dictionary-mode control ──────────────────────────────────────────────────
// A delete forces `shape::DICT`, and DICT == 0 == an empty IC way. Two dict
// objects must never match each other's cached shape, so this phase is the one
// that catches a shape guard using a zero-initialised id as "valid".
function dictShape(n, prop) {
  var a = new Array(n);
  for (var i = 0; i < n; i++) {
    var o = { pad0: i, pad1: i + 1, doomed: i };
    o[prop] = i;
    delete o.doomed;
    a[i] = o;
  }
  return a;
}

// ── phases ───────────────────────────────────────────────────────────────────
// The hot loops are separate named functions so each gets its own inline-cache
// sites and its own JIT region, exactly as a real call site would.

function readLoop(objs, n) {
  var sum = 0, k = 0;
  for (var i = 0; i < ACCESSES; i++) {
    sum = (sum + objs[k].rd) | 0;
    // Explicit wrap — NOT `i & (n - 1)`. See the header.
    k++;
    if (k === n) k = 0;
  }
  return sum;
}

// An EXISTING-slot store. Deliberately not an append: an append is a shape
// transition plus a possible `vals` realloc, which is a different mechanism and
// would drown the probe cost this phase is measuring.
function writeLoop(objs, n) {
  var k = 0;
  for (var i = 0; i < ACCESSES; i++) {
    objs[k].wr = i;
    k++;
    if (k === n) k = 0;
  }
  var sum = 0;
  for (var j = 0; j < n; j++) sum = (sum + objs[j].wr) | 0;
  return sum;
}

function readDiff(objs, n) {
  var sum = 0, k = 0;
  for (var i = 0; i < ACCESSES; i++) {
    sum = (sum + objs[k].dv) | 0;
    k++;
    if (k === n) k = 0;
  }
  return sum;
}

function readDict(objs, n) {
  var sum = 0, k = 0;
  for (var i = 0; i < ACCESSES; i++) {
    sum = (sum + objs[k].dc) | 0;
    k++;
    if (k === n) k = 0;
  }
  return sum;
}

var COUNTS = [1, 2, 4, 8, 9, 16, 1024];

// ── same shape, GetProp ──
for (var ci = 0; ci < COUNTS.length; ci++) {
  var n = COUNTS[ci];
  out.push("read/same/" + n + "=" + readLoop(sameShape(n, "rd"), n));
}

// ── same shape, existing-slot SetProp ──
for (var ci2 = 0; ci2 < COUNTS.length; ci2++) {
  var n2 = COUNTS[ci2];
  var objs2 = sameShape(n2, "pad2");
  for (var q = 0; q < n2; q++) objs2[q].wr = 0;
  out.push("write/same/" + n2 + "=" + writeLoop(objs2, n2));
}

// ── distinct shapes, GetProp (the control) ──
for (var ci3 = 0; ci3 < COUNTS.length; ci3++) {
  var n3 = COUNTS[ci3];
  out.push("read/diff/" + n3 + "=" + readDiff(diffShapes(n3, "dv"), n3));
}

// ── dictionary mode, GetProp (the DICT==0 trap) ──
for (var ci4 = 0; ci4 < COUNTS.length; ci4++) {
  var n4 = COUNTS[ci4];
  out.push("read/dict/" + n4 + "=" + readDict(dictShape(n4, "dc"), n4));
}

// ── prototype-chain control ──
// The property is INHERITED, so the answer depends on more than the receiver's
// own layout: a shape-keyed own-property entry must decline here and leave the
// existing identity/hop-version path in charge.
var protoBase = { pr: 7 };
function readProto(objs, n) {
  var sum = 0, k = 0;
  for (var i = 0; i < ACCESSES; i++) {
    sum = (sum + objs[k].pr) | 0;
    k++;
    if (k === n) k = 0;
  }
  return sum;
}
for (var ci5 = 0; ci5 < COUNTS.length; ci5++) {
  var n5 = COUNTS[ci5];
  var objs5 = new Array(n5);
  for (var m = 0; m < n5; m++) {
    var po = Object.create(protoBase);
    po.own0 = m;
    po.own1 = m + 1;
    objs5[m] = po;
  }
  out.push("read/proto/" + n5 + "=" + readProto(objs5, n5));
}

console.log(out.join(" "));
