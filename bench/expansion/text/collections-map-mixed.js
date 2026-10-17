// collections/map-mixed: Map and Set keyed by a mix of value types: small
// ints, negative and large integers, non-integral doubles, strings, plain
// objects, arrays, symbols, booleans, null/undefined, NaN, and both zeros
// (SameValueZero: -0 and +0 are one key, NaN is one key). get/set/has/delete
// run at three live sizes in the same run: small (~8 keys), medium (~1k) and
// large (~SIZE keys, 100k at the medium size), followed by iteration with
// for-of over entries/keys/values and forEach, a delete-heavy churn phase
// (insert, delete 90%, iterate past the holes, re-insert, delete during
// forEach), and a WeakMap with object keys.
// Why a likely weak spot: one hash table must hash and compare every key kind
// (number normalisation, string hashing, object identity), deletions leave
// tombstones that iteration must skip while keeping insertion order, and the
// large tier stresses resizing and cache misses; the small tier stresses the
// per-call overhead of the Map/Set built-ins.
// Dominant operations: Map.prototype.get/set/has/delete, Set add/has/delete,
// Map/Set iterators, forEach callbacks, WeakMap get/set/has.
const SIZE = 100000;

let seed = 40503;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function rint(n) { return Math.floor(rnd() * n); }
function mix(h, n) { return Math.imul(h ^ (n | 0), 16777619) >>> 0; }

const SYMS = [Symbol("a"), Symbol("b"), Symbol.iterator];
const SPECIAL = [NaN, -0, 0, true, false, null, undefined, SYMS[0], SYMS[1], SYMS[2], 0 / 0, -Infinity, Infinity];
function makeKey(i) {
  switch (i % 8) {
    case 0: return i;                         // small int
    case 1: return i + 0.5;                   // double
    case 2: return "k" + i;                   // string
    case 3: return { id: i };                 // object
    case 4: return -i * 1000003;              // negative / large int
    case 5: return "s" + (i * 2654435761 % 1000003) + "_" + (i & 15); // longer string
    case 6: return i % 3 === 0 ? [i] : (i * 1e-3); // array or small double
    default: return SPECIAL[i % SPECIAL.length];
  }
}
function keyCode(k) {
  switch (typeof k) {
    case "number":
      if (k !== k) return 7;
      if (k === Infinity) return 11;
      if (k === -Infinity) return 13;
      if (Object.is(k, -0)) return 99991; // never expected: Map keys normalise -0
      return Number.isInteger(k) ? (k | 0) : Math.floor(k * 4);
    case "string": return k.length * 131 + k.charCodeAt(1);
    case "object": return k === null ? 17 : (Array.isArray(k) ? k[0] * 3 : k.id * 5);
    case "boolean": return k ? 19 : 23;
    case "undefined": return 29;
    case "symbol": return 31 + k.toString().length;
    default: return 37;
  }
}
function makeKeys(n) {
  const ks = [];
  for (let i = 0; i < n; i++) ks.push(makeKey(i));
  return ks;
}

// ---- A. get/set/has/delete churn at three live sizes -------------------------
function churn(keys, ops) {
  const m = new Map();
  const s = new Set();
  let h = 0, hits = 0;
  const n = keys.length;
  for (let i = 0; i < n; i++) { m.set(keys[i], -i); s.add(keys[i]); } // prefill: every key live
  for (let i = 0; i < ops; i++) {
    const k = keys[rint(n)];
    const r = i & 7;
    if (r < 3) { m.set(k, i); s.add(k); }
    else if (r < 6) {
      const v = m.get(k);
      if (v !== undefined) { hits++; h = mix(h, v); }
      if (s.has(k)) hits++;
    } else if (r === 6) {
      if (m.delete(k)) hits += 3;
      s.delete(k);
    } else if (m.has(k)) hits += 5;
  }
  for (const [k, v] of m) h = mix(h, keyCode(k) + v);
  for (const k of s) h = mix(h, keyCode(k));
  let fe = 0;
  m.forEach(function (v, k, map) { fe = (fe + v + (map === m ? 1 : 0)) | 0; });
  s.forEach(function (v, k) { if (v === k) fe++; });
  let kv = 0;
  for (const k of m.keys()) kv = (kv + keyCode(k)) | 0;
  for (const v of m.values()) kv = (kv + v) | 0;
  return [m.size, s.size, hits, h, fe, kv];
}
const OPS = SIZE * 3;
const small = churn(makeKeys(12), OPS);
const medium = churn(makeKeys(1500), OPS);
const large = churn(makeKeys(Math.max(16, Math.ceil(SIZE * 1.5))), OPS);

// ---- B. delete-heavy churn and iteration past deletions ---------------------
const dm = new Map();
const ds = new Set();
const N = Math.max(64, SIZE >> 1);
for (let i = 0; i < N; i++) { const k = (i & 1) ? i : "d" + i; dm.set(k, i); ds.add(k); }
for (let i = 0; i < N; i++) if (i % 10 !== 0) { const k = (i & 1) ? i : "d" + i; dm.delete(k); ds.delete(k); }
let dh = 0;
for (const [k, v] of dm.entries()) dh = mix(dh, v + (typeof k === "string" ? 1 : 0));
for (const v of ds.values()) dh = mix(dh, typeof v === "number" ? v : v.length);
for (let i = 0; i < N; i += 3) dm.set(i, -i);
let visited = 0, added = 0;
dm.forEach(function (v, k) {
  visited++;
  if (typeof k === "number" && k % 9 === 0) dm.delete(k);   // delete the current entry
  if (added < 1000 && (visited & 63) === 0) { dm.set("late" + added, added); added++; } // visited later in this walk
});
let lateSeen = 0;
for (const k of dm.keys()) if (typeof k === "string" && k.charCodeAt(0) === 108) lateSeen++;
const dsize = dm.size;
dm.clear();

// ---- C. WeakMap with object keys -------------------------------------------
const wm = new WeakMap();
const objs = [];
const NW = Math.max(16, SIZE >> 2);
for (let i = 0; i < NW; i++) {
  const o = (i % 3 === 0) ? [i] : (i % 3 === 1) ? { w: i } : function () { return i; };
  objs.push(o);
  wm.set(o, i * 2);
}
let wh = 0, wHits = 0;
for (let r = 0; r < 4; r++) {
  for (let i = 0; i < NW; i++) {
    const o = objs[(i * 7 + r) % NW];
    const v = wm.get(o);
    if (v !== undefined) { wh = (wh + v) | 0; wHits++; }
    if (r === 1 && (i & 3) === 0) wm.delete(o);
    if (r === 2 && (i & 7) === 0) wm.set(o, i);
    if (wm.has(o)) wHits++;
  }
}
if (wm.get({}) === undefined) wHits += 7;

console.log("small " + small.join(" "));
console.log("medium " + medium.join(" "));
console.log("large " + large.join(" "));
console.log("delete " + dh + " size " + dsize + " visited " + visited + " added " + added + " late " + lateSeen);
console.log("weak " + wh + " hits " + wHits);
