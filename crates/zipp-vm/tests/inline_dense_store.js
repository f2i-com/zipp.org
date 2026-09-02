"use strict";
// B264: the inline pinned dense-Array store lane. Every loop is hot enough
// for a MEM region with the array pinned; each case is one way the lane must
// route to the helper or deopt, and the answer must match node exactly.
var out = [];
function log(tag, v) { out.push(tag + ":" + v); }

// 1. hole-fill into `new Array(n)` (licensed fill), then overwrite.
var holes = new Array(4096);
var acc = 0;
for (var r = 0; r < 40; r++) {
  for (var i = 0; i < 4096; i++) holes[i] = (i * 7 + r) | 0;
  for (var i = 0; i < 4096; i++) acc = (acc + holes[i]) | 0;
}
log("holes", acc + "," + holes.length + "," + (2 in holes));

// 2. append at i == len (helper path) mixed with in-range stores.
var grow = [];
for (var r = 0; r < 30; r++) {
  for (var i = 0; i < 2000; i++) {
    if (i < grow.length) grow[i] = grow[i] + 1; else grow[i] = i;
  }
}
var gs = 0;
for (var i = 0; i < grow.length; i++) gs = (gs + grow[i]) | 0;
log("grow", grow.length + "," + gs);

// 3. OLD holder across minors: a retained array receives young objects; the
//    barrier must record them (or the lane must go to the helper).
var retained = new Array(512);
for (var i = 0; i < 512; i++) retained[i] = { v: i };
var junk = null;
for (var r = 0; r < 200; r++) {
  for (var i = 0; i < 512; i++) retained[i] = { v: retained[i].v + 1, r: r };
  var pile = [];
  for (var k = 0; k < 400; k++) pile.push({ k: k, s: "s" + k });
  junk = pile;
}
var rs = 0;
for (var i = 0; i < 512; i++) rs = (rs + retained[i].v + retained[i].r) | 0;
log("retained", rs + "," + junk.length);

// 4. non-writable length + hole fill: the store is silently ignored in sloppy
//    mode but THROWS in strict mode (this file is strict).
var fixed = new Array(64);
Object.defineProperty(fixed, "length", { writable: false });
var threw = 0;
for (var r = 0; r < 300; r++) {
  for (var i = 0; i < 64; i++) {
    try { fixed[i] = i; } catch (e) { threw++; }
  }
}
log("fixed", threw + "," + fixed.length + "," + (0 in fixed));

// 5. non-writable length, PRESENT elements: in-range stores are fine.
var fixed2 = [1, 2, 3, 4, 5, 6, 7, 8];
Object.defineProperty(fixed2, "length", { writable: false });
var f2 = 0;
for (var r = 0; r < 4000; r++) for (var i = 0; i < 8; i++) { fixed2[i] = r + i; f2 = (f2 + fixed2[i]) | 0; }
log("fixed2", f2 + "," + fixed2.length);

// 6. an indexed setter on Array.prototype fires for hole fills, not for
//    present-element stores.
var setterHits = 0;
Object.defineProperty(Array.prototype, "3", {
  set: function (v) { setterHits++; },
  get: function () { return "proto3"; },
  configurable: true
});
var withHoles = new Array(8);
var dense = [0, 0, 0, 0, 0, 0, 0, 0];
for (var r = 0; r < 500; r++) {
  for (var i = 0; i < 8; i++) { withHoles[i] = i; dense[i] = i + r; }
  if (r % 2 === 0) withHoles = new Array(8);
}
log("setter", setterHits + "," + withHoles[3] + "," + dense[3]);
delete Array.prototype[3];

// 7. setPrototypeOf(arr, {5: 'x'}) then hole-fill/read.
var protoObj = { 5: "x" };
var sp = new Array(16);
Object.setPrototypeOf(sp, protoObj);
var spv = "";
for (var r = 0; r < 300; r++) {
  sp = new Array(16);
  Object.setPrototypeOf(sp, protoObj);
  for (var i = 0; i < 16; i++) if (i !== 5) sp[i] = i;
  spv = sp[5] + "," + sp[4];
}
log("setproto", spv + "," + sp.length);

// 8. fractional / negative / string keys on a pinned array.
var keyed = new Array(32);
for (var i = 0; i < 32; i++) keyed[i] = 0;
for (var r = 0; r < 500; r++) {
  for (var i = 0; i < 32; i++) {
    keyed[i] = i;
    keyed[i + 0.5] = "half";
    keyed[-1] = "neg";
    keyed["k" + (i & 3)] = i;
  }
}
log("keys", keyed.length + "," + keyed[1.5] + "," + keyed[-1] + "," + keyed.k3 + "," + Object.keys(keyed).length);

// 9. store of a double, a string, undefined and null into a young array.
var mixed = new Array(1024);
var ms = 0;
for (var r = 0; r < 60; r++) {
  for (var i = 0; i < 1024; i++) {
    mixed[i] = (i & 3) === 0 ? i * 1.5 : (i & 3) === 1 ? "s" : (i & 3) === 2 ? undefined : null;
  }
  for (var i = 0; i < 1024; i++) ms = (ms + (typeof mixed[i] === "number" ? mixed[i] : mixed[i] === "s" ? 1 : mixed[i] === undefined ? 2 : 3)) | 0;
}
log("mixed", ms);

console.log(out.join("|"));
