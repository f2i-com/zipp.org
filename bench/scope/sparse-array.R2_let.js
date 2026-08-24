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
let hasOwn = Object.prototype.hasOwnProperty;

// ---- virtual-length sparse array: stride writes ----
let SPLEN = 50000000;
let STRIDE = 1250;
let sp = [];
sp.length = SPLEN;
let writes = 0;
for (let i = 0; i < SPLEN; i += STRIDE) { sp[i] = (i % 1000) + 1; writes++; }

// in / hasOwnProperty probes (mix of hits and misses)
let inHits = 0, ownHits = 0;
for (let i = 0; i < 1900000; i += 14) {
  if (i in sp) inHits++;
  if (hasOwn.call(sp, i + 1)) ownHits++;
}

// for-in key walk: count keys, fold key values
let keyCount = 0, keyFold = 0;
for (let k in sp) {
  keyCount++;
  keyFold = (keyFold + (+k) + sp[k]) % 1000000007;
}

// ---- indexOf over a packed window ----
let PACK = 1000000;
let packed = new Array(PACK);
for (let i = 0; i < PACK; i++) packed[i] = (i * 7) % 1009;
let idxAcc = 0;
for (let t = 0; t < 2000; t++) {
  idxAcc = (idxAcc + packed.indexOf(t * 3 % 1009, (t * 487) % 900000)) | 0;
}

// ---- delete-punch holes in the packed array, then hole-aware iteration ----
for (let i = 0; i < PACK; i += 5) delete packed[i];
let holeCount = 0, holeSum = 0;
for (let rep = 0; rep < 8; rep++) {
  holeCount = 0; holeSum = 0;
  for (let i = 0; i < PACK; i++) {
    if (i in packed) holeSum = (holeSum + packed[i]) | 0;
    else holeCount++;
  }
}
// indexOf must skip holes
let idxAfter = (packed.indexOf(packed[1], 0) + packed.indexOf(7, 0)) | 0;

// ---- concat / slice over holey windows (holes preserved) ----
let sl1 = packed.slice(100000, 350000);
let sl2 = packed.slice(600000, 700000);
let cc = sl1.concat(sl2, [1, , 3]);
let ccHoles = 0, ccSum = 0;
for (let rep = 0; rep < 4; rep++) {
  ccHoles = 0; ccSum = 0;
  for (let i = 0; i < cc.length; i++) {
    if (i in cc) ccSum = (ccSum + cc[i]) | 0;
    else ccHoles++;
  }
}

console.log("writes=" + writes + " inHits=" + inHits + " ownHits=" + ownHits +
  " keys=" + keyCount + " keyFold=" + keyFold);
console.log("idxAcc=" + idxAcc + " holes=" + holeCount + " holeSum=" + holeSum +
  " idxAfter=" + idxAfter + " ccLen=" + cc.length + " ccHoles=" + ccHoles + " ccSum=" + ccSum);
