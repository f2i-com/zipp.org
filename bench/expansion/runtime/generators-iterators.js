// generators-iterators: the iteration protocol end to end.
//
// Measures generator functions (yield, yield* delegation with return values,
// two-way next(value), generators passed into other generators as lazy
// pipeline stages, a round-robin scheduler over suspended generators),
// hand-written iterables with [Symbol.iterator]() / next() / return(),
// for-of over Map / Set / arrays / strings (including astral code points),
// spread of iterables into arrays and calls, Array.from with and without a map
// function, Map/Set construction from iterables, array destructuring from
// iterators, and early exit (break, take-n, destructuring) that must run
// return() and generator finally blocks. Cleanup calls are counted into the
// checksum so the close path is verified, not just exercised.
//
// Why a likely weak spot: every step allocates or reuses an iterator result
// object and crosses a generator suspend/resume boundary; an interpreter
// without a fast path for built-in array/Map/Set iterators or for generator
// frame save/restore pays a full protocol call per element.
//
// Dominant operations: generator resume/suspend, iterator result object
// creation and `.done`/`.value` reads, Symbol.iterator lookup, closure calls.
"use strict";

const SIZE = 100;

let seed = 2463534242;
function rndInt(n) {
  seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5;
  return ((seed >>> 0) % n) | 0;
}

let finallyRuns = 0;   // generator finally blocks executed
let returnCalls = 0;   // custom iterator return() calls

// --- generator building blocks -------------------------------------------------
function* range(start, end, step) {
  try {
    for (let i = start; i < end; i += step) yield i;
  } finally {
    finallyRuns++;
  }
}
function* mapGen(it, f) { for (const x of it) yield f(x); }
function* filterGen(it, p) { for (const x of it) if (p(x)) yield x; }
function* take(it, n) {
  if (n <= 0) return;
  for (const x of it) { yield x; if (--n <= 0) return; }
}
function* chain(...its) { for (const it of its) yield* it; }
function* zip2(a, b) {
  const ia = a[Symbol.iterator](), ib = b[Symbol.iterator]();
  try {
    for (;;) {
      const x = ia.next(), y = ib.next();
      if (x.done || y.done) return;
      yield [x.value, y.value];
    }
  } finally {
    if (typeof ia.return === "function") ia.return();
    if (typeof ib.return === "function") ib.return();
  }
}
function pipeline(src, ...stages) {
  let it = src;
  for (const s of stages) it = s(it);
  return it;
}

// yield* with return values and values sent in through next(v)
function* accumulate(n) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    const got = yield i;
    s = (s + (got === undefined ? 1 : got)) | 0;
  }
  return s;
}
function* twoPhase(n) {
  const a = yield* accumulate(n);
  const b = yield* accumulate(n >> 1);
  return (a * 3 + b) | 0;
}

// recursive yield* over a tree (depth stays small: nested delegation is O(depth))
function makeTree(depth, v) {
  if (depth === 0) return null;
  return { v, left: makeTree(depth - 1, v * 2 + 1), right: makeTree(depth - 1, v * 2 + 2) };
}
function* inorder(t) {
  if (t === null) return 0;
  const l = yield* inorder(t.left);
  yield t.v;
  const r = yield* inorder(t.right);
  return l + r + 1;
}

// --- hand-written iterables ---------------------------------------------------------
class Ring {
  constructor(n, mul) { this.n = n; this.mul = mul; }
  [Symbol.iterator]() {
    let i = 0;
    const n = this.n, mul = this.mul;
    return {
      next() {
        if (i < n) { const v = Math.imul(i, mul) & 0xffff; i++; return { value: v, done: false }; }
        return { value: undefined, done: true };
      },
      return(v) { returnCalls++; return { value: v, done: true }; },
    };
  }
}
// an iterator that is its own iterable, with state on the instance
class Countdown {
  constructor(n) { this.n = n; }
  [Symbol.iterator]() { return this; }
  next() { return this.n > 0 ? { value: this.n--, done: false } : { value: undefined, done: true }; }
  return() { returnCalls++; this.n = 0; return { value: undefined, done: true }; }
}
const fibIterable = {
  [Symbol.iterator]() {
    let a = 0, b = 1;
    return { next() { const v = a; a = b; b = (a + v) | 0; return { value: v, done: false }; }, return() { returnCalls++; return { done: true }; } };
  },
};

// --- data for built-in iteration ----------------------------------------------
const words = [];
for (let i = 0; i < 200; i++) words.push("w" + (i * 37 % 211) + (i & 1 ? "x" : ""));
const text = words.join(" ") + String.fromCodePoint(0x1F600, 0x41, 0x1F4A9, 0xE9);

// --- driver -------------------------------------------------------------------------
let pipeHash = 0, protoHash = 0, builtinHash = 0, spreadHash = 0, schedHash = 0;

for (let round = 0; round < SIZE; round++) {
  // lazy pipelines of generators passed into generators
  const scale = 3 + (round & 3);
  const p = pipeline(range(0, 4000, 1),
    it => mapGen(it, x => Math.imul(x, scale) ^ round),
    it => filterGen(it, x => (x & 3) !== 1),
    it => take(it, 2500));
  for (const v of p) pipeHash = (pipeHash + v) | 0;
  for (const [a, b] of zip2(range(0, 900, 3), new Ring(400, scale))) pipeHash = (pipeHash ^ (a + b)) | 0;
  for (const v of chain(range(0, 300, 2), new Countdown(200), [7, 8, 9], "abc")) {
    pipeHash = (pipeHash + (typeof v === "number" ? v : v.charCodeAt(0))) | 0;
  }

  // early break: generator finally + custom return()
  for (let k = 0; k < 40; k++) {
    for (const v of range(0, 1000, 1)) if (v > 20 + k) { pipeHash = (pipeHash + v) | 0; break; }
    for (const v of new Ring(1000, k + 1)) if (v > 1000) { pipeHash = (pipeHash + v) | 0; break; }
    for (const v of fibIterable) if (v > 100000 + k) { pipeHash = (pipeHash ^ v) | 0; break; }
  }

  // two-way communication and delegated return values
  for (let k = 0; k < 30; k++) {
    const g = twoPhase(40 + k);
    let r = g.next(), sent = 0;
    while (!r.done) { sent = (sent + r.value) & 7; r = g.next(sent); }
    protoHash = (protoHash + r.value) | 0;
  }
  const tree = makeTree(10, round & 7);
  const it = inorder(tree);
  let r = it.next();
  while (!r.done) { protoHash = (Math.imul(protoHash, 31) + r.value) | 0; r = it.next(); }
  protoHash = (protoHash + r.value) | 0;

  // built-in iterables: Map / Set / array / string
  const m = new Map();
  for (let i = 0; i < 600; i++) m.set(words[(i + round) % words.length] + (i & 15), i);
  for (const [k, v] of m) builtinHash = (builtinHash + k.length + v) | 0;
  for (const k of m.keys()) builtinHash = (builtinHash ^ k.charCodeAt(1)) | 0;
  for (const v of m.values()) builtinHash = (builtinHash + (v & 7)) | 0;
  const s = new Set(m.values());
  for (const v of s) if (v & 1) builtinHash = (builtinHash + v) | 0;
  for (const e of s.entries()) builtinHash = (builtinHash ^ e[0]) | 0;
  for (const w of words) builtinHash = (builtinHash + w.length) | 0;
  for (const [i, w] of words.entries()) if ((i & 31) === 0) builtinHash = (builtinHash + w.charCodeAt(0)) | 0;
  let cps = 0;
  for (const ch of text) { cps++; builtinHash = (builtinHash + ch.codePointAt(0)) | 0; }
  builtinHash = (builtinHash + cps) | 0;

  // spread, Array.from, constructors from iterables, destructuring
  const arr = [...range(0, 500, 1), ...new Ring(100, 5), ...s];
  spreadHash = (spreadHash + arr.length + arr[arr.length - 1] + Math.max(...new Ring(50, round + 1))) | 0;
  const doubled = Array.from(take(fibIterable, 40), x => x & 1023);
  const fromSet = Array.from(s);
  const fromLen = Array.from({ length: 64 }, (_, i) => i * round);
  spreadHash = (spreadHash + doubled[39] + fromSet.length + fromLen[63]) | 0;
  const m2 = new Map(zip2(range(0, 200, 1), words));
  const s2 = new Set(mapGen(m2.keys(), x => x & 31));
  spreadHash = (spreadHash + m2.size + s2.size) | 0;
  for (let k = 0; k < 100; k++) {
    const [a, b, c] = new Ring(10, k + 1);            // not exhausted: return() runs
    const [x, , y, ...rest] = range(k, k + 12, 1);    // rest exhausts the generator
    spreadHash = (spreadHash + a + b + c + x + y + rest.length) | 0;
  }

  // round-robin scheduler over suspended generators
  const tasks = [];
  for (let t = 0; t < 16; t++) tasks.push(take(mapGen(range(t, 4000, 16), x => x ^ t), 50 + rndInt(150)));
  let live = tasks.length, ticks = 0;
  while (live > 0) {
    for (let t = 0; t < tasks.length; t++) {
      const task = tasks[t];
      if (task === null) continue;
      const step = task.next();
      if (step.done) { tasks[t] = null; live--; continue; }
      ticks++;
      schedHash = (schedHash + step.value * (t + 1)) | 0;
    }
  }
  schedHash = (schedHash ^ ticks) | 0;
}

console.log("generators-iterators rounds=" + SIZE + " finally=" + finallyRuns + " returns=" + returnCalls);
console.log("pipe=" + pipeHash + " proto=" + protoHash + " builtin=" + builtinHash);
console.log("spread=" + spreadHash + " sched=" + schedHash);
