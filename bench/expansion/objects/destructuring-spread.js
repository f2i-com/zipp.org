// destructuring-spread: destructuring patterns, spread and rest, and
// higher-order array method chains.
//
// What it measures:
//   1. array destructuring with defaults, holes and rest
//      (const [a, b = 5, , ...rest] = arr) and swaps ([x, y] = [y, x]);
//   2. object destructuring with defaults, renaming, nesting and rest
//      (const { id, tags: [first] = [], ...others } = rec);
//   3. destructuring in parameters, including defaults that are evaluated per
//      call, and in for-of heads over Object.entries and Array#entries;
//   4. spread in calls with argument lists of varying length (f(...args) with
//      0 to 12 args), Math.max(...arr) over 100 elements, apply-style
//      forwarding through rest parameters, and `arguments` reads;
//   5. spread in array literals ([...a, x, ...b]) and in object literals;
//   6. map/filter/reduce/some/every/find/findIndex chains over 1,000-element
//      arrays with arrow callbacks.
// Why it is a likely weak spot: destructuring and spread compile to the
// iterator protocol (or to copying loops when the engine proves the array
// iterator is untouched); object rest must copy all remaining own
// enumerable keys; spread calls build argument vectors of dynamic length; and
// the array higher-order methods re-enter bytecode once per element. Engines
// that run the full protocol each time pay several calls per element.
// Dominant operations: array-iterator next() (or its fast path), argument
// vector construction for spread calls, object rest copying, callback calls
// from native map/filter/reduce.
//
// SIZE is the number of rounds; every round does the same fixed amount of work.

const SIZE = 100;

let seed = 8675309;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function mix(h, v) { return Math.imul(h ^ (v | 0), 16777619); }

function makeData() {
  const nums = [];
  for (let i = 0; i < 1000; i++) nums.push(((rnd() * 2000) | 0) - 1000);
  const tuples = [];
  for (let i = 0; i < 400; i++) {
    const len = (rnd() * 7) | 0;
    const t = [];
    for (let j = 0; j < len; j++) t.push((rnd() * 100) | 0);
    tuples.push(t);
  }
  const recs = [];
  for (let i = 0; i < 300; i++) {
    const r = { id: i, name: "n" + (i % 17), score: (rnd() * 100) | 0 };
    if (i % 3) r.tags = [i & 7, (i >> 3) & 7];
    if (i % 5 === 0) r.extra = i * 2;
    if (i % 4 === 1) r.pos = { x: i & 15, y: (i >> 4) & 15 };
    recs.push(r);
  }
  const argLists = [];
  for (let i = 0; i < 64; i++) {
    const n = (i * 5) % 13;
    const a = [];
    for (let j = 0; j < n; j++) a.push(i + j);
    argLists.push(a);
  }
  return { nums, tuples, recs, argLists };
}

function phaseArrayPatterns(D) {
  let s = 0;
  const T = D.tuples;
  for (let i = 0; i < T.length; i++) {
    const [a = -1, b = 5, , ...rest] = T[i];
    s = (s + a * 3 + b + rest.length) | 0;
    let [x, y] = [a, b];
    [x, y] = [y, x];
    s = (s + x - y) | 0;
    const [[p = 0, q = 0] = [], second] = [T[(i + 1) % T.length], i];
    s = (s + p + q + second) | 0;
  }
  return s;
}

function scoreOf({ score = 0, extra = 1 }) { return score * extra; }
function placed({ pos: { x, y } = { x: -1, y: -1 }, id }, scale = id & 3) { return (x + y * 16) * scale; }

function phaseObjectPatterns(D) {
  let s = 0;
  const R = D.recs;
  for (let i = 0; i < R.length; i++) {
    const rec = R[i];
    const { id, name: label, tags: [first, second = 9] = [], ...others } = rec;
    s = (s + id + label.length + (first === undefined ? 1 : first) + second) | 0;
    s = (s + Object.keys(others).length * 11) | 0;
    s = (s + scoreOf(rec) + placed(rec)) | 0;
  }
  let e = 0;
  for (const [k, v] of Object.entries(R[7])) e += k.length + (typeof v === "number" ? v : 1);
  for (const [i, v] of D.nums.entries()) if ((i & 63) === 0) e += v;
  return mix(s, e);
}

function sum3(a, b, c) { return (a | 0) + (b | 0) * 2 + (c | 0) * 3; }
function countArgs() {
  let t = arguments.length;
  for (let i = 0; i < arguments.length; i++) t += arguments[i];
  return t;
}
function restSum(first, ...more) {
  let t = first === undefined ? 0 : first;
  for (let i = 0; i < more.length; i++) t += more[i] * (i + 1);
  return t;
}
function forward(...args) { return restSum(...args) + countArgs(...args, 1); }

function phaseSpreadCalls(D) {
  let s = 0;
  const L = D.argLists;
  for (let rep = 0; rep < 6; rep++) {
    for (let i = 0; i < L.length; i++) {
      const a = L[i];
      s = (s + sum3(...a) + countArgs(...a) + restSum(...a) + forward(...a, rep)) | 0;
      s = (s + Math.max(0, ...a) + Math.min(100, ...a)) | 0;
    }
  }
  for (let k = 0; k < 10; k++) {
    const chunk = D.nums.slice(k * 100, k * 100 + 100);
    s = (s + Math.max(...chunk) - Math.min(...chunk)) | 0;
  }
  return s;
}

function phaseSpreadLiterals(D) {
  let s = 0;
  const T = D.tuples;
  for (let i = 0; i < T.length; i++) {
    const a = T[i], b = T[(i * 7) % T.length];
    const c = [...a, i, ...b, ...a];
    s = (s + c.length + (c.length ? c[c.length - 1] : 0)) | 0;
    const o = { ...D.recs[i % D.recs.length], i, flag: true };
    s = (s + o.i + o.id + (o.flag ? 1 : 0)) | 0;
  }
  const big = [...D.nums, ...D.nums.slice(0, 100)];
  s = mix(s, big.length + big[1050]);
  return s;
}

function phaseChains(D, r) {
  const N = D.nums;
  let s = 0;
  for (let k = 0; k < 4; k++) {
    const t = N.map((x) => x * 3 + k)
      .filter((x) => (x & 3) !== 0)
      .map((x, i) => x ^ i)
      .reduce((acc, x) => (acc + x) | 0, r);
    s = (s + t) | 0;
    if (N.some((x) => x === 999 - k)) s++;
    if (N.every((x) => x >= -1000 && x < 1000)) s += 2;
    const f = N.find((x) => x > 900 + k * 10);
    const fi = N.findIndex((x) => x < -950 + k);
    s = (s + (f === undefined ? -1 : f) + fi) | 0;
    const pairs = D.recs.filter((rec) => rec.score > 50).map(({ id, score }) => ({ id, d: score - 50 }));
    s = (s + pairs.reduce((a, { id, d }) => a + id * d, 0)) | 0;
  }
  return s;
}

function main() {
  const rounds = SIZE;
  const D = makeData();
  const acc = [0, 0, 0, 0, 0];
  for (let r = 0; r < rounds; r++) {
    acc[0] = mix(acc[0], phaseArrayPatterns(D) + r);
    acc[1] = mix(acc[1], phaseObjectPatterns(D) + r);
    acc[2] = mix(acc[2], phaseSpreadCalls(D) + r);
    acc[3] = mix(acc[3], phaseSpreadLiterals(D) + r);
    acc[4] = mix(acc[4], phaseChains(D, r));
  }
  console.log("array-patterns", acc[0] >>> 0);
  console.log("object-patterns", acc[1] >>> 0);
  console.log("spread-calls", acc[2] >>> 0);
  console.log("spread-literals", acc[3] >>> 0);
  console.log("chains", acc[4] >>> 0);
}

main();
