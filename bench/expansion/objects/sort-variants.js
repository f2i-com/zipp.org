// sort-variants: Array.prototype.sort across input shapes and comparators.
//
// What it measures: the sort implementation itself, not just the comparator
// call.
//   1. default sort (no comparator) of integers, which compares their string
//      forms (ToString on every comparison or a precomputed key per element);
//   2. default sort of strings, and of a mix of short and long strings with
//      shared prefixes;
//   3. numeric sort with (a, b) => a - b on ints and on doubles;
//   4. objects sorted by a numeric key and by a string key (three-way
//      comparator), then by a two-field comparator;
//   5. stability: 1,000 records with only 8 distinct keys; after sorting by
//      key, records with equal keys must stay in original-index order (the
//      violation count must be 0);
//   6. adversarial input orders: already sorted, reverse sorted, all equal,
//      many duplicates, sawtooth, sorted-with-a-few-swaps (galloping and run
//      detection in TimSort-like sorts; worst cases for naive quicksorts);
//   7. binary search (lower bound) over every sorted result.
// toSorted is not used (ES2023); arrays are copied with slice() before sorting.
// Why it is a likely weak spot: an interpreter's sort is a native loop that
// calls back into bytecode for every comparison; the cost of that re-entry,
// of default-sort string conversion, and of the merge strategy on presorted
// runs varies enormously between engines.
// Dominant operations: comparator calls from native sort, string comparison,
// number-to-string conversion in default sort, element moves.
//
// SIZE is the number of rounds; every round sorts the same-sized arrays.

const SIZE = 200;

let seed = 31337;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function mix(h, v) { return Math.imul(h ^ (v | 0), 16777619); }
function strHash(h, s) {
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h;
}

const N = 1000;

function lowerBound(a, x, cmp) {
  let lo = 0, hi = a.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    if (cmp(a[mid], x) < 0) lo = mid + 1; else hi = mid;
  }
  return lo;
}
const numCmp = (a, b) => a - b;
function strCmp(a, b) { return a < b ? -1 : a > b ? 1 : 0; }

// Fold a sorted array into a hash, and count order violations.
function checkNums(a, h) {
  let bad = 0;
  for (let i = 1; i < a.length; i++) if (a[i - 1] > a[i]) bad++;
  for (let i = 0; i < a.length; i += 37) h = mix(h, a[i] * 8);
  return [h, bad];
}

function makeInputs() {
  const ints = [], doubles = [], strs = [], words = [];
  for (let i = 0; i < N; i++) {
    ints.push(((rnd() * 200000) | 0) - 100000);
    doubles.push((rnd() - 0.5) * 1e6);
  }
  const pre = ["alpha", "alp", "beta", "be", "gamma", "g", "delta", "del"];
  for (let i = 0; i < N; i++) {
    let s = pre[(rnd() * 8) | 0];
    const extra = (rnd() * 6) | 0;
    for (let j = 0; j < extra; j++) s += String.fromCharCode(97 + ((rnd() * 26) | 0));
    strs.push(s);
    words.push(String.fromCharCode(65 + ((rnd() * 26) | 0)) + ((rnd() * 1000) | 0));
  }
  const recs = [];
  for (let i = 0; i < N; i++) {
    recs.push({ idx: i, key: (rnd() * 8) | 0, score: ((rnd() * 1000) | 0) / 4, name: words[i] });
  }
  // order patterns over ints
  const sorted = ints.slice().sort(numCmp);
  const patterns = {
    sorted: sorted,
    reversed: sorted.slice().reverse(),
    equal: new Array(N).fill(42),
    dups: ints.map((x) => x & 7),
    saw: ints.map((_, i) => (i % 50) * 3 - (i % 7)),
    nearly: (function () {
      const a = sorted.slice();
      for (let i = 0; i < 10; i++) {
        const p = (rnd() * N) | 0, q = (rnd() * N) | 0;
        const t = a[p]; a[p] = a[q]; a[q] = t;
      }
      return a;
    })(),
  };
  return { ints, doubles, strs, words, recs, patterns };
}

function round(I, r, acc) {
  let h = 0, bad = 0, found = 0;
  // 1. default sort of numbers (string order)
  const d1 = I.ints.slice(0, 400).sort();
  for (let i = 1; i < d1.length; i++) if (String(d1[i - 1]) > String(d1[i])) bad++;
  for (let i = 0; i < d1.length; i += 13) h = mix(h, d1[i]);
  // 2. default sort of strings
  const s1 = I.strs.slice().sort();
  for (let i = 1; i < s1.length; i++) if (s1[i - 1] > s1[i]) bad++;
  for (let i = 0; i < s1.length; i += 41) h = strHash(h, s1[i]);
  const s2 = I.words.slice().sort();
  h = strHash(h, s2[r % N]);
  // 3. numeric with comparator: ints and doubles
  const n1 = I.ints.slice().sort(numCmp);
  let c = checkNums(n1, h); h = c[0]; bad += c[1];
  const n2 = I.doubles.slice().sort((a, b) => a - b);
  c = checkNums(n2, h); h = c[0]; bad += c[1];
  const n3 = I.ints.slice().sort((a, b) => b - a);
  h = mix(h, n3[0] - n3[N - 1]);
  // 7a. binary search over the numeric results
  for (let i = 0; i < 200; i++) {
    const x = I.ints[(i * 7 + r) % N];
    const p = lowerBound(n1, x, numCmp);
    if (n1[p] === x) found++;
    const y = n2[(i * 5) % N];
    if (n2[lowerBound(n2, y, numCmp)] === y) found++;
  }
  // 4. objects by numeric key, by string key, by two fields
  const o1 = I.recs.slice().sort((a, b) => a.score - b.score);
  for (let i = 1; i < N; i++) if (o1[i - 1].score > o1[i].score) bad++;
  const o2 = I.recs.slice().sort((a, b) => strCmp(a.name, b.name));
  for (let i = 1; i < N; i++) if (o2[i - 1].name > o2[i].name) bad++;
  const o3 = I.recs.slice().sort((a, b) => a.key - b.key || b.score - a.score || a.idx - b.idx);
  for (let i = 0; i < N; i += 29) h = mix(h, o3[i].idx);
  for (let i = 0; i < 100; i++) {
    const nm = I.words[(i * 11 + r) % N];
    const p = lowerBound(o2, nm, (rec, x) => strCmp(rec.name, x));
    if (o2[p].name === nm) found++;
  }
  // 5. stability with 8 distinct keys
  const st = I.recs.slice().sort((a, b) => a.key - b.key);
  let unstable = 0;
  for (let i = 1; i < N; i++) {
    if (st[i - 1].key === st[i].key && st[i - 1].idx > st[i].idx) unstable++;
    if (st[i - 1].key > st[i].key) bad++;
  }
  h = mix(h, st[r % N].idx);
  // 6. adversarial orders
  const P = I.patterns;
  const names = ["sorted", "reversed", "equal", "dups", "saw", "nearly"];
  for (let k = 0; k < names.length; k++) {
    const a = P[names[k]].slice().sort(numCmp);
    c = checkNums(a, h); h = c[0]; bad += c[1];
    const x = a[(r * 13) % N];
    if (a[lowerBound(a, x, numCmp)] === x) found++;
  }
  acc[0] = mix(acc[0], h);
  acc[1] += bad;
  acc[2] += unstable;
  acc[3] += found;
}

function main() {
  const rounds = SIZE;
  const I = makeInputs();
  const acc = [0, 0, 0, 0];
  for (let r = 0; r < rounds; r++) round(I, r, acc);
  console.log("hash", acc[0] >>> 0);
  console.log("violations", acc[1], "unstable", acc[2]);
  console.log("found", acc[3]);
}

main();
