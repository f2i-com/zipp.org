"use strict";
// real-world bench 10: sparse/holey array behavior. A virtual-length sparse
// array (length 50M, 40k populated by stride writes) with in/hasOwn probes
// and a for-in key walk; indexOf over a packed window; a packed 1M array with
// delete-punched holes iterated hole-aware; concat/slice over holey windows.
// Deterministic checksums.
//
// CALIBRATION NOTE (zipp): zipp's sparse-element storage is linear-scan —
// O(populated) per access and quadratic to populate: 100k stride writes into
// a length-50M array take 11.3s on zipp (node 5ms); ONE for-in walk over
// those 100k keys adds 26.8s (node 7ms); in/hasOwn probes cost ~18us each at
// 100k populated (node ~27ns). Packed arrays with delete-punched holes are
// FINE on zipp (1M-element hole-aware pass ~120ms). The sparse sections are
// therefore sized down (40k keys, one for-in rep) and the packed/holey
// sections carry the volume; node lands ~100ms — below the suite's usual
// 150-800ms band, accepted to keep zipp's run ~12s.
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
var holeCount = 0, holeSum = 0;
for (var rep = 0; rep < 8; rep++) {
  holeCount = 0; holeSum = 0;
  for (var i = 0; i < PACK; i++) {
    if (i in packed) holeSum = (holeSum + packed[i]) | 0;
    else holeCount++;
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
console.log("idxAcc=" + idxAcc + " holes=" + holeCount + " holeSum=" + holeSum +
  " idxAfter=" + idxAfter + " ccLen=" + cc.length + " ccHoles=" + ccHoles + " ccSum=" + ccSum);
