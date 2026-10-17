// old-to-young: fresh objects stored into a large long-lived structure.
//
// Measures the cost of a big, long-lived object graph (up to 100,000 records,
// each with a child object, a 4-slot history array and a string tag, plus
// 512 long-lived bucket arrays) that is continuously mutated with freshly
// allocated objects, arrays and strings: property stores and array-element
// stores of young values into old containers, occasional replacement of
// whole records in the old table, interleaved with short-lived garbage that
// never escapes. Periodic partial scans read back through the old-to-young
// edges, and a final full scan folds the whole structure into the checksum.
//
// Why a likely weak spot: a generational collector must track every store of
// a young pointer into an old object (write barrier + remembered set) and
// rescan those edges on each minor collection; with a large old space and
// many such stores per collection, barrier overhead, remembered-set growth or
// promotion of soon-dead objects can dominate. A refcounting engine
// (QuickJS) instead frees each overwritten value immediately, and pays at
// cycle-collection time by scanning the whole live heap. The graph here is
// acyclic, so QuickJS's cycle collector scans it but finds nothing to free.
//
// Dominant operations: small object/array allocation, property and element
// stores into old objects, string concatenation, GC (minor collections or
// refcount frees plus periodic whole-heap cycle scans).
"use strict";

const SIZE = 100;

const EPOCHS = SIZE;
const WRITES_PER_EPOCH = 1500;
const N = Math.min(100000, SIZE * 1000);   // long-lived records
const BUCKETS = 512, BUCKET_LEN = 64;

let seed = 0x2545f491;
function rndInt(n) {
  seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5;
  return ((seed >>> 0) % n) | 0;
}

function makeRecord(id, epoch) {
  return {
    id,
    epoch,
    child: { v: id & 1023, gen: epoch },
    history: [null, null, null, null],
    tag: "r" + id,
    hits: 0,
  };
}

// long-lived structure, built once
const table = new Array(N);
for (let i = 0; i < N; i++) table[i] = makeRecord(i, 0);
const buckets = new Array(BUCKETS);
for (let b = 0; b < BUCKETS; b++) {
  const arr = new Array(BUCKET_LEN);
  for (let j = 0; j < BUCKET_LEN; j++) arr[j] = { k: b * BUCKET_LEN + j, payload: null };
  buckets[b] = arr;
}

let checksum = 0, replaced = 0, garbage = 0;

for (let epoch = 1; epoch <= EPOCHS; epoch++) {
  for (let w = 0; w < WRITES_PER_EPOCH; w++) {
    const i = rndInt(N);
    const rec = table[i];
    const r = w & 15;

    // young object into an old object's property
    rec.child = { v: (rec.child.v + w) & 1023, gen: epoch };
    rec.hits++;

    // young object into an old array's element
    rec.history[w & 3] = { at: epoch, w, ref: rec.child };

    // young array into a long-lived bucket entry
    const slot = buckets[(i + w) & (BUCKETS - 1)][w & (BUCKET_LEN - 1)];
    slot.payload = [i, w, epoch];

    if (r === 0) rec.tag = "t" + (i & 255) + ":" + epoch;               // young string into old object
    if (r === 7 && (w & 127) === 7) {                                    // replace a whole record
      table[i] = makeRecord(i, epoch);
      replaced++;
    }

    // short-lived garbage that never escapes
    const tmp = { a: i, b: w, c: [r, r + 1] };
    const s = "g" + (tmp.a & 63);
    garbage = (garbage + tmp.c[1] + s.length) | 0;
  }

  // partial scan through the old-to-young edges
  const start = rndInt(N);
  for (let k = 0; k < 300; k++) {
    const rec = table[(start + k * 37) % N];
    const h = rec.history[k & 3];
    checksum = (checksum + rec.child.v + (h === null ? 0 : h.w + h.ref.v) + rec.tag.length) | 0;
  }
}

// full scan of the long-lived structure
let liveHistory = 0, hitSum = 0;
for (let i = 0; i < N; i++) {
  const rec = table[i];
  hitSum = (hitSum + rec.hits) | 0;
  checksum = (Math.imul(checksum, 31) + rec.child.v + rec.child.gen + rec.tag.length + rec.epoch) | 0;
  for (let j = 0; j < 4; j++) {
    const h = rec.history[j];
    if (h !== null) { liveHistory++; checksum = (checksum ^ (h.at + h.w + h.ref.gen)) | 0; }
  }
}
let payloads = 0;
for (let b = 0; b < BUCKETS; b++) {
  const arr = buckets[b];
  for (let j = 0; j < BUCKET_LEN; j++) {
    const p = arr[j].payload;
    if (p !== null) { payloads++; checksum = (checksum + p[0] + p[1] + p[2]) | 0; }
  }
}

console.log("old-to-young epochs=" + EPOCHS + " records=" + N + " writes=" + EPOCHS * WRITES_PER_EPOCH);
console.log("replaced=" + replaced + " liveHistory=" + liveHistory + " payloads=" + payloads + " hits=" + hitSum);
console.log("checksum=" + checksum + " garbage=" + garbage);
