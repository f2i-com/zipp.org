// await-sustained: async functions awaiting async functions, at scale.
//
// Measures async/await as application code uses it, beyond the promise-burst
// and then-chain benchmarks: async call chains 20-50 deep (each level awaits
// the next), fan-out of many chains joined with Promise.all, an async tree
// that awaits Promise.all of its children at every node,
// Promise.allSettled over mixed fulfilments and rejections (Error objects
// and plain values), Promise.race among promises that settle after
// different numbers of ticks, await inside counted loops, for-await over
// arrays of promises, user thenables (sync and deferred resolution, plus
// rejection), and rejections caught with try/await/catch/finally. Everything
// runs on the microtask queue and the result is printed from the final
// continuation; no timers are used.
//
// Why a likely weak spot: each await suspends and resumes a frame and
// queues at least one job; thenables and returning a promise from an async
// function add NewPromiseResolveThenableJob ticks. The cost is spread over
// promise allocation, reaction records, job queue handling and frame
// save/restore, all paths that synchronous benchmarks never touch.
//
// Output: line 2 holds order-independent sums; line 3 holds a hash of the
// order in which continuations ran, which depends on the spec's exact tick
// counts (await, thenable adoption, async return of a promise).
"use strict";

const SIZE = 800;

let seed = 99991;
function rndInt(n) {
  seed = (Math.imul(seed, 1664525) + 1013904223) | 0;
  return (((seed >>> 8) & 0xffffff) % n) | 0;
}

let order = 0;          // hash of continuation order (spec-defined scheduling)
function trace(x) { order = (Math.imul(order, 31) + x) | 0; }
let cleanups = 0, caught = 0, thenCalls = 0;

async function leaf(x) { return (x * 3) | 0; }                 // settles without awaiting
async function leafAwait(x) { await null; trace(x & 7); return (x + 1) | 0; }

// --- depth: each level awaits the next --------------------------------------------
async function chain(d, x) {
  if (d === 0) return leafAwait(x);                             // returns a promise: adopted
  const v = await chain(d - 1, (x + d) | 0);
  return (v ^ d) | 0;
}

// --- breadth: fan out and join -----------------------------------------------------------
async function fanOut(width, x) {
  const ps = [];
  for (let i = 0; i < width; i++) ps.push(chain(i & 7, (x + i) | 0));
  const vs = await Promise.all(ps);
  let s = 0;
  for (let i = 0; i < vs.length; i++) s = (s + vs[i] * (i + 1)) | 0;
  return s;
}
async function asyncTree(depth, x) {
  if (depth === 0) { await null; trace(x & 3); return x & 255; }
  const [a, b, c] = await Promise.all([asyncTree(depth - 1, x * 3 + 1), asyncTree(depth - 1, x * 3 + 2), leaf(x)]);
  return (a + b + (c & 15)) | 0;
}

// --- settle / race -------------------------------------------------------------------------
async function maybeFail(i) {
  await null;
  if (i % 10 === 0) throw new Error("fail-" + i);
  if (i % 10 === 5) return Promise.reject(i);                   // rejection by value
  return i * 2;
}
async function settleBatch(n, base) {
  const ps = [];
  for (let i = 0; i < n; i++) ps.push(maybeFail(base + i));
  const rs = await Promise.allSettled(ps);
  let s = 0;
  for (const r of rs) {
    if (r.status === "fulfilled") s = (s + r.value) | 0;
    else if (typeof r.reason === "number") s = (s - r.reason) | 0;
    else s = (s + r.reason.message.length * 100) | 0;
  }
  return s;
}
async function ticks(n, v) {
  for (let i = 0; i < n; i++) await null;
  trace(v);
  return v;
}
async function raceRound(r) {
  const k = r % 5;
  const a = await Promise.race([ticks(3 + k, 1), ticks(1 + k, 2), ticks(2 + k, 3), ticks(5, 4)]);
  let b;
  try {
    b = await Promise.race([ticks(4, 10), Promise.reject(20), ticks(2, 30)]);
  } catch (e) {
    b = e;
  }
  return (a * 100 + b) | 0;
}

// --- await in loops ----------------------------------------------------------------------------
async function sumLoop(n, x) {
  let s = 0;
  for (let i = 0; i < n; i++) s = (s + (await leaf(i + x))) | 0;
  return s;
}
async function forAwaitSum(n, x) {
  const arr = [];
  for (let i = 0; i < n; i++) arr.push(i & 1 ? leaf(i + x) : i + x);     // mix of promises and plain values
  let s = 0;
  for await (const v of arr) s = (s + v) | 0;
  return s;
}

// --- thenables -------------------------------------------------------------------------------------
function syncThenable(v) {
  return { then(resolve, reject) { thenCalls++; if (v < 0) reject(v); else resolve(v * 2); } };
}
function deferredThenable(v) {
  return { then(resolve) { thenCalls++; Promise.resolve(v).then(x => resolve(x + 1)); } };
}
async function thenableRound(n, x) {
  let s = 0;
  for (let i = 0; i < n; i++) {
    s = (s + (await syncThenable(i + x))) | 0;
    s = (s ^ (await deferredThenable(i))) | 0;
    try {
      s = (s + (await syncThenable(-1 - i))) | 0;
    } catch (e) {
      s = (s + e) | 0;
    }
  }
  const all = await Promise.all([syncThenable(x), deferredThenable(x), Promise.resolve(syncThenable(1))]);
  return (s + all[0] + all[1] + all[2]) | 0;
}

// --- rejections caught with try/await/catch -------------------------------------------------
async function mayReject(i) {
  if ((i & 7) === 3) { await null; throw { code: i & 255 }; }
  if ((i & 15) === 9) throw new RangeError("bad-" + i);                 // rejects before any await
  await null;
  return i;
}
async function guarded(i) {
  try {
    return await mayReject(i);
  } catch (e) {
    caught++;
    return e instanceof RangeError ? -e.message.length : -e.code;
  } finally {
    cleanups++;
  }
}
async function guardedBatch(n, x) {
  let s = 0;
  for (let i = 0; i < n; i++) s = (s + (await guarded(i + x))) | 0;          // sequential
  const vs = await Promise.all(Array.from({ length: n }, (_, i) => guarded(i * 3 + x)));  // concurrent
  for (const v of vs) s = (s ^ v) | 0;
  return s;
}

// --- driver ----------------------------------------------------------------------------------------
const sums = [0, 0, 0, 0, 0, 0, 0];
async function round(r) {
  sums[0] = (sums[0] + (await chain(20 + rndInt(30), r))) | 0;
  sums[1] = (sums[1] + (await fanOut(48, r)) + (await asyncTree(4, r & 63))) | 0;
  sums[2] = (sums[2] + (await settleBatch(40, r * 40))) | 0;
  sums[3] = (sums[3] + (await raceRound(r))) | 0;
  sums[4] = (sums[4] + (await sumLoop(150, r)) + (await forAwaitSum(100, r))) | 0;
  sums[5] = (sums[5] + (await thenableRound(30, r))) | 0;
  sums[6] = (sums[6] + (await guardedBatch(40, r))) | 0;
  trace(r);
}

async function main() {
  // a few independent rounds in flight at once, then joined
  for (let r = 0; r < SIZE; r += 4) {
    const batch = [];
    for (let k = r; k < r + 4 && k < SIZE; k++) batch.push(round(k));
    await Promise.all(batch);
  }
}

main().then(
  () => {
    console.log("await-sustained rounds=" + SIZE + " caught=" + caught + " cleanups=" + cleanups + " thenCalls=" + thenCalls);
    console.log("sums=" + sums.join(","));
    console.log("order=" + order);
  },
  e => console.log("FAILED " + (e && e.message))
);
