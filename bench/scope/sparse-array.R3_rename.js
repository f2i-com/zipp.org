"use strict";
// real-world bench 10: sparse/holey array behavior. A virtual-length sparse
// array (length 50M, 40k populated by stride writes) with in/hasOwn probes
// and a for-in key walk; indexOf over a packed window; a packed 1M array with
// delete-punched holes iterated hole-aware; concat/slice over holey windows.
// Deterministic checksums.
//
// CALIBRATION NOTE (zipp) — RE-MEASURED 2026-07-25, the original premise is
// GONE. Sparse-element storage is no longer linear-scan: per-access cost is now
// flat as the populated count scales (60ns/probe at 2k through 100k keys), and
// populating is linear, not quadratic — 100k stride writes into a length-50M
// array take 14ms (was quoted as 11.3s; node 13ms). The one section still
// behind is the for-in key walk at ~700ns/key vs node's ~180ns.
//
// The sparse sections below are therefore sized for a problem that no longer
// exists (40k keys, one for-in rep), so this bench now mostly measures its
// packed/holey sections. RESIZING IT IS DELIBERATELY NOT DONE HERE: it would
// silently invalidate every historical ratio in PERF_ROADMAP. Re-size it in a
// commit that does nothing else and re-baselines the table.
var hasOwn = Object.prototype.hasOwnProperty;

// ---- virtual-length sparse array: stride writes ----
var SPLEN = 50000000;
var STRIDE = 1250;
var sp = [];
sp.length = SPLEN;
var writes = 0;
for (var i = 0; i < SPLEN; i += STRIDE) { sp[i] = (i % 1000) + 1; writes++; }

// in / hasOwnProperty probes (mix of hits and misses)
var inHits = 0, ownHits = 0;
for (var i = 0; i < 1900000; i += 14) {
  if (i in sp) inHits++;
  if (hasOwn.call(sp, i + 1)) ownHits++;
}

// for-in key walk: count keys, fold key values
var keyCount = 0, keyFold = 0;
for (var k in sp) {
  keyCount++;
  keyFold = (keyFold + (+k) + sp[k]) % 1000000007;
}

// ---- indexOf over a packed window ----
var PACK = 1000000;
var packed = new Array(PACK);
for (var i = 0; i < PACK; i++) packed[i] = (i * 7) % 1009;
var idxAcc = 0;
for (var t = 0; t < 2000; t++) {
  idxAcc = (idxAcc + packed.indexOf(t * 3 % 1009, (t * 487) % 900000)) | 0;
}

// ---- delete-punch holes in the packed array, then hole-aware iteration ----
for (var i = 0; i < PACK; i += 5) delete packed[i];
var nHoles = 0, sumOfHoleFree = 0;
for (var rep = 0; rep < 8; rep++) {
  nHoles = 0; sumOfHoleFree = 0;
  for (var i = 0; i < PACK; i++) {
    if (i in packed) sumOfHoleFree = (sumOfHoleFree + packed[i]) | 0;
    else nHoles++;
  }
}
// indexOf must skip holes
var idxAfter = (packed.indexOf(packed[1], 0) + packed.indexOf(7, 0)) | 0;

// ---- concat / slice over holey windows (holes preserved) ----
var sl1 = packed.slice(100000, 350000);
var sl2 = packed.slice(600000, 700000);
var cc = sl1.concat(sl2, [1, , 3]);
var ccHoles = 0, ccSum = 0;
for (var rep = 0; rep < 4; rep++) {
  ccHoles = 0; ccSum = 0;
  for (var i = 0; i < cc.length; i++) {
    if (i in cc) ccSum = (ccSum + cc[i]) | 0;
    else ccHoles++;
  }
}

console.log("writes=" + writes + " inHits=" + inHits + " ownHits=" + ownHits +
  " keys=" + keyCount + " keyFold=" + keyFold);
console.log("idxAcc=" + idxAcc + " holes=" + nHoles + " sumOfHoleFree=" + sumOfHoleFree +
  " idxAfter=" + idxAfter + " ccLen=" + cc.length + " ccHoles=" + ccHoles + " ccSum=" + ccSum);
