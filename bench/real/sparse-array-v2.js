"use strict";
// M0.3 benchmark 3 — a representation-honest replacement for `sparse-array.js`.
//
// The original row is retained unchanged, and its own header now says its
// sparse-storage calibration is OBSOLETE: it mixes virtual length, holes, index
// probes and packed/holey operations into one number, over one gap size. Nothing
// representation-specific should be promoted or rejected from it, because it
// cannot say WHICH representation moved.
//
// This file separates the two axes that actually decide the representation —
// GAP SIZE and LOGICAL LENGTH — and separates the operations that behave
// differently per representation. Phases:
//
//   packed          — no holes at all. The floor; every fast path applies.
//   holey           — one hole per 4 elements, dense storage. Hole-aware reads.
//   gapCurve/<g>    — one element every `g` indices, same ELEMENT COUNT each
//                     time, so `g` varies the logical length while the work
//                     stays constant. This is the curve the original row has a
//                     single point on.
//   in/forIn/read/write/slice/concat — one line each, because they hit
//                     different code: `in` is HasProperty (a prototype-chain
//                     walk when the index is absent), `for-in` builds a key
//                     snapshot, `slice`/`concat` allocate and copy.
//
// The `in` and hole-read phases are also the ones gated on the indexed-prototype
// PROTECTOR (`array_proto_has_index`): while it is valid they answer absence
// directly, and once anything defines an integer key on — or re-prototypes —
// Array.prototype/Object.prototype they must walk the real chain. The last phase
// exercises exactly that transition, so a future hole/OOB fast path cannot be
// promoted on protector-valid numbers alone.

var out = [];
var READS = 2000000;

// ── phase: packed ────────────────────────────────────────────────────────────
var packed = new Array(4096);
for (var i = 0; i < 4096; i++) packed[i] = i;
var packedSum = 0;
for (var r0 = 0; r0 < READS; r0++) packedSum = (packedSum + packed[r0 & 4095]) | 0;
out.push("packed=" + packedSum);

// ── phase: holey (dense storage, holes punched) ───────────────────────────────
var holey = new Array(4096);
for (var h = 0; h < 4096; h++) if (h % 4 !== 0) holey[h] = h;
var holeySum = 0, holeyUndef = 0;
for (var r1 = 0; r1 < READS; r1++) {
  var v1 = holey[r1 & 4095];
  if (v1 === undefined) holeyUndef++;
  else holeySum = (holeySum + v1) | 0;
}
out.push("holey=" + holeySum + "/" + holeyUndef);

// ── phases: the GAP CURVE ────────────────────────────────────────────────────
// Constant element count (512), gap `g` ⇒ logical length 512*g. So the phases
// differ only in how sparse the same data is, which is the variable the storage
// choice turns on. 4096 crosses MAX_DENSE_ARRAY_LEN territory for the larger
// gaps and stays inside it for the smaller ones.
var GAPS = [1, 2, 8, 64, 1024, 65536];
var ELEMS = 512;
for (var gi = 0; gi < GAPS.length; gi++) {
  var g = GAPS[gi];
  var a = [];
  for (var e = 0; e < ELEMS; e++) a[e * g] = e;
  // Read only the PRESENT indices, so the phases do equal work.
  var s = 0;
  var perElem = Math.max(1, (READS / ELEMS) | 0);
  for (var pass = 0; pass < perElem; pass++) {
    for (var e2 = 0; e2 < ELEMS; e2++) s = (s + a[e2 * g]) | 0;
  }
  out.push("gapCurve/" + g + "=" + s + "/" + a.length);
}

// ── phase: `in` (HasProperty) on present, hole and out-of-range indices ───────
var probe = [];
for (var p = 0; p < 512; p++) probe[p * 8] = p;
var inPresent = 0, inHole = 0, inOob = 0;
var PROBES = 400000;
for (var q = 0; q < PROBES; q++) {
  if ((q % 512) * 8 in probe) inPresent++;
  if ((q % 512) * 8 + 1 in probe) inHole++;   // a hole below length
  if (1000000 + (q % 512) in probe) inOob++;  // past length entirely
}
out.push("in=" + inPresent + "/" + inHole + "/" + inOob);

// ── phase: for-in over a sparse array ────────────────────────────────────────
var FORIN = 2000;
var forInKeys = 0;
for (var f = 0; f < FORIN; f++) {
  for (var kk in probe) forInKeys++;
}
out.push("forIn=" + forInKeys);

// ── phase: writes that CREATE indices ────────────────────────────────────────
// Separated from reads because a create is OrdinarySet: it consults the
// prototype chain when the indexed-proto protector is invalid, and it may grow
// or convert the storage.
var WRITES = 200000;
var wtarget = [];
for (var w = 0; w < WRITES; w++) wtarget[(w % 512) * 16] = w;
var wsum = 0;
for (var w2 = 0; w2 < 512; w2++) wsum = (wsum + wtarget[w2 * 16]) | 0;
out.push("createWrites=" + wsum + "/" + wtarget.length);

// ── phases: slice / concat (allocate + copy) ─────────────────────────────────
// The two iteration counts are DELIBERATELY different, and the reason is a
// finding rather than a tuning convenience: `concat` on this 4089-length sparse
// array costs zipp about 300 µs per call. At `slice`'s 20,000 iterations it was
// 6.0 s of a 6.9 s run — one phase drowning every other, so the file would have
// reported "sparse arrays are 22x node" when what it measured was one builtin.
//
// Sized down to 1,000 instead of deleted: an unmeasured phase is how the original
// row's blind spots happened. The asymmetry is stated here so nobody reads the
// two numbers as comparable throughputs — and `concat`'s per-call cost is itself
// worth an entry, since `slice(0, 64)` over the same receiver is ~1000x cheaper.
var SLICES = 20000;
var CONCATS = 1000;
var sliceLen = 0, concatLen = 0;
for (var c = 0; c < SLICES; c++) sliceLen = (sliceLen + probe.slice(0, 64).length) | 0;
for (var c2 = 0; c2 < CONCATS; c2++) concatLen = (concatLen + probe.concat([1, 2]).length) | 0;
out.push("slice=" + sliceLen + "/" + SLICES + " concat=" + concatLen + "/" + CONCATS);

// ── phase: the SAME hole/OOB reads with the protector INVALIDATED ────────────
// `Object.setPrototypeOf(Array.prototype, …)` and an integer key DEFINED on
// Array.prototype are the two mutations that invalidate the indexed-proto
// protector. Either one means an absent index may now be INHERITED, so every
// hole and out-of-range read has to walk the chain. Running the identical loop
// on both sides of that line is what prices the fast path honestly — the row
// above measures the fast path, this one measures its absence.
//
// Order matters: this phase must come LAST, because the protector is sticky for
// the life of the VM by design.
Object.defineProperty(Array.prototype, "5", { value: "P5", configurable: true });
var invalidSum = 0, invalidInherited = 0;
for (var r3 = 0; r3 < PROBES; r3++) {
  var v3 = probe[(r3 % 512) * 8 + 1];        // a hole
  if (v3 !== undefined) invalidInherited++;
  var v4 = probe[5];                          // index 5 is a hole ⇒ inherits "P5"
  if (v4 === "P5") invalidSum++;
}
out.push("protectorInvalid=" + invalidSum + "/" + invalidInherited);
delete Array.prototype[5];

console.log(out.join(" "));
