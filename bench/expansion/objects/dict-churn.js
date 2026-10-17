// dict-churn: plain objects used as hash maps, with keys added and deleted.
//
// What it measures: objects that never settle into a stable shape.
//   1. build a 2,100-key dictionary: 1,500 string keys in a shuffled order,
//      plus 600 integer-like keys (300 stored through numeric keys, 300
//      through "200123"-style strings);
//   2. probe it with `in`, hasOwnProperty.call, Object.hasOwn and plain reads,
//      half hits and half misses;
//   3. delete half of the keys and re-add a quarter of them (holes in the
//      property table, re-insertion order);
//   4. Object.keys / Object.values / Object.entries over the churned object,
//      hashing key order (spec-defined: integer keys ascending, then strings
//      in insertion order);
//   5. for-in over the object, deleting the visited key during some loops and
//      adding keys between loops;
//   6. Object.assign and object spread {...a, x} merging small and medium
//      dictionaries, then counting keys.
// Why it is a likely weak spot: engines optimise objects as hidden-class
// records; past a few dozen keys or after deletes they must switch to a
// dictionary representation, and the enumeration caches, key-order rules for
// integer-like keys, and the copying builtins (assign, spread, entries) all
// take generic paths.
// Dominant operations: keyed property store/load/delete with string keys,
// `in` checks, own-key enumeration and the arrays it allocates.
//
// SIZE is the number of rounds; each round rebuilds the same-sized dictionaries.

const SIZE = 100;

let seed = 2463534242;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function mix(h, v) { return Math.imul(h ^ (v | 0), 16777619); }
function strHash(h, s) {
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h;
}

const NSTR = 1500;
const NINT = 300;

function makeKeys() {
  const syll = ["ka", "lo", "mi", "zu", "re", "pa", "ti", "no", "sha", "ve", "qu", "dor"];
  const strKeys = [];
  for (let i = 0; i < NSTR; i++) {
    let s = "";
    let n = i;
    do { s += syll[n % 12]; n = (n / 12) | 0; } while (n > 0);
    strKeys.push(s + "_" + (i & 7));
  }
  const missKeys = [];
  for (let i = 0; i < NSTR; i++) missKeys.push(strKeys[i] + "x");
  const intKeys = [];
  const intStrKeys = [];
  for (let i = 0; i < NINT; i++) {
    const k = 1 + ((i * 7919) % 100003);
    intKeys.push(k);
    intStrKeys.push(String(k + 200000));
  }
  const prefixes = [];
  for (let i = 0; i < 64; i++) prefixes.push("n" + i);
  return { strKeys, missKeys, intKeys, intStrKeys, prefixes };
}

function shuffled(n) {
  const p = [];
  for (let i = 0; i < n; i++) p.push(i);
  for (let i = n - 1; i > 0; i--) {
    const j = (rnd() * (i + 1)) | 0;
    const t = p[i]; p[i] = p[j]; p[j] = t;
  }
  return p;
}

function round(K, perm, r, acc) {
  const { strKeys, missKeys, intKeys, intStrKeys } = K;
  const hasOwn = Object.prototype.hasOwnProperty;
  const d = {};
  // 1. build
  for (let i = 0; i < NSTR; i++) {
    const j = perm[(i + r * 37) % NSTR];
    d[strKeys[j]] = j + r;
  }
  for (let i = 0; i < NINT; i++) {
    d[intKeys[i]] = i;
    d[intStrKeys[i]] = -i;
  }
  // 2. probe
  let hits = 0, sum = 0;
  for (let i = 0; i < NSTR; i++) {
    const k = strKeys[i];
    if (k in d) hits++;
    if (missKeys[i] in d) hits += 1000;
    if (hasOwn.call(d, missKeys[(i * 3) % NSTR])) hits += 1000;
    if (Object.hasOwn(d, k)) sum += d[k];
  }
  for (let i = 0; i < NINT; i++) {
    if (hasOwn.call(d, intKeys[i])) sum += d[intKeys[i]] * 3;
    if (intStrKeys[i] in d) sum += d[intStrKeys[i]];
    if ((intKeys[i] + 1) in d) hits += 7;
  }
  // 3. delete half, re-add a quarter
  let deleted = 0;
  for (let i = 0; i < NSTR; i++) {
    if (((i * 2654435761) >>> 0) % 2 === (r & 1)) {
      if (delete d[strKeys[i]]) deleted++;
    }
  }
  for (let i = 0; i < NINT; i += 2) delete d[intKeys[i]];
  for (let i = 0; i < NSTR; i += 4) d[strKeys[i]] = i * 2;
  // 4. enumeration with order hashed
  const keys = Object.keys(d);
  let h = 0;
  for (let i = 0; i < keys.length; i += 7) h = strHash(h, keys[i]);
  const vals = Object.values(d);
  let vsum = 0;
  for (let i = 0; i < vals.length; i++) vsum = (vsum + vals[i]) | 0;
  const ents = Object.entries(d);
  for (let i = 0; i < ents.length; i += 5) h = mix(h, ents[i][0].length * 131 + ents[i][1]);
  // 5. for-in: plain, then deleting the visited key, then after adding keys
  let fsum = 0, fcount = 0;
  for (const k in d) { fsum = (fsum + d[k]) | 0; fcount++; }
  for (const k in d) {
    if (k.length > 7) { delete d[k]; fcount += 2; }
  }
  const suffix = "_" + r;
  for (let i = 0; i < 200; i++) d[K.prefixes[i & 63] + suffix] = i;
  for (const k in d) { fsum = (fsum ^ (d[k] * 7)) | 0; fcount++; }
  // 6. assign and spread
  const small = { a: r, b: 2, c: 3, d: 4 };
  const merged = Object.assign({}, small, { e: 5, b: 20 });
  let msum = 0;
  for (let i = 0; i < 40; i++) {
    const s = { ...small, x: i, b: i * 2 };
    const t = { ...s, ...merged, y: s.x };
    msum += t.x + t.b + t.y + Object.keys(t).length;
  }
  const big = Object.assign({}, d);
  const bigger = { ...big, extra: 1, [strKeys[r % NSTR]]: -5 };
  msum += Object.keys(bigger).length + Object.keys(big).length;
  acc[0] = mix(acc[0], hits);
  acc[1] = (acc[1] + sum) | 0;
  acc[2] = mix(acc[2], deleted);
  acc[3] = mix(acc[3], h ^ vsum);
  acc[4] = mix(acc[4], fsum + fcount);
  acc[5] = (acc[5] + msum) | 0;
  acc[6] += keys.length;
}

function main() {
  const rounds = SIZE;
  const K = makeKeys();
  const perm = shuffled(NSTR);
  const acc = [0, 0, 0, 0, 0, 0, 0];
  for (let r = 0; r < rounds; r++) round(K, perm, r, acc);
  console.log("probe", acc[0] >>> 0, acc[1] >>> 0);
  console.log("delete", acc[2] >>> 0);
  console.log("enum", acc[3] >>> 0, acc[6]);
  console.log("forin", acc[4] >>> 0);
  console.log("assign", acc[5] >>> 0);
}

main();
