"use strict";
// real-world bench 4: Map/Set churn. String-key + integer-key inserts,
// interleaved hit/miss lookups, deletes of every 3rd, re-inserts, full
// iteration sums. Deterministic; prints final sizes + checksum.
//
// ROLE — this is a NO-REGRESSION SENTINEL, not an optimization target. It is the
// one row zipp already wins (0.897x node in the retained baseline), so its job is
// to fail loudly when an unrelated change costs it. The standing gate holds it to
// no worse than +2%. Attempts to widen the win have measured net zero (relaxing
// the Map method gates), which is the expected result for a row already ahead:
// spend the effort on the rows that are 2-3.6x instead.
// (Sized so node lands in the suite's 150-800ms compute band; zipp's
// 2026-06-12 Map/Set rework handles this scale fine — its previous
// linear-scan Map/Set was quadratic and could not.)
var N = 400000; // string keys AND integer keys each, for Map and for Set

var check = 0x811c9dc5 >>> 0;
function mix(x) { check = Math.imul(check ^ (x | 0), 16777619) >>> 0; }

// ---- Map ----
var m = new Map();
for (var i = 0; i < N; i++) {
  m.set("k" + i, i);
  m.set(i, i * 2 + 1);
}
// interleaved lookups: hits and misses
var hits = 0, misses = 0, lookSum = 0;
for (var i = 0; i < N; i++) {
  var v1 = m.get("k" + i);
  if (v1 !== undefined) { hits++; lookSum = (lookSum + v1) | 0; }
  var v2 = m.get(i);
  if (v2 !== undefined) { hits++; lookSum = (lookSum + v2) | 0; }
  if (m.has("x" + i)) hits++; else misses++;
  if (m.get(i + N) !== undefined) hits++; else misses++;
}
mix(hits); mix(misses); mix(lookSum);
// delete every 3rd of both key families
for (var i = 0; i < N; i += 3) {
  m.delete("k" + i);
  m.delete(i);
}
mix(m.size);
// re-insert half of the deleted with new values
for (var i = 0; i < N; i += 6) {
  m.set("k" + i, i + 7);
  m.set(i, i + 13);
}
// full iteration: sum number values + count string keys
var iterSum = 0, strKeys = 0;
for (var entry of m) {
  if (typeof entry[0] === "string") strKeys++;
  iterSum = (iterSum + entry[1]) | 0;
}
mix(iterSum); mix(strKeys);

// ---- Set ----
var s = new Set();
for (var i = 0; i < N; i++) {
  s.add("s" + i);
  s.add(i);
}
var sHits = 0, sMisses = 0;
for (var i = 0; i < N; i++) {
  if (s.has("s" + i)) sHits++; else sMisses++;
  if (s.has(i)) sHits++; else sMisses++;
  if (s.has("z" + i)) sHits++; else sMisses++;
  if (s.has(i + N)) sHits++; else sMisses++;
}
mix(sHits); mix(sMisses);
for (var i = 0; i < N; i += 3) {
  s.delete("s" + i);
  s.delete(i);
}
mix(s.size);
for (var i = 0; i < N; i += 6) {
  s.add("s" + i);
  s.add(i);
}
var sIterSum = 0, sStr = 0;
for (var v of s) {
  if (typeof v === "string") sStr++;
  else sIterSum = (sIterSum + v) | 0;
}
mix(sIterSum); mix(sStr);

console.log("mapSize=" + m.size + " setSize=" + s.size + " hits=" + hits + " misses=" + misses +
  " sHits=" + sHits + " sMisses=" + sMisses + " checksum=" + check);
