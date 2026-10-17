// recursion-shapes: recursion other than fib.
//
// Measures call/return cost across recursion shapes that the fib and
// stable-call benchmarks do not reach: two- and three-way mutual recursion at
// depths of several hundred frames, Ackermann (deep, irregular, argument-
// dependent depth), recursive tree build and multi-pass tree walks, backtracking
// (N-Queens) and divide-and-conquer (merge sort, wide shallow recursion),
// recursion with many parameters and default-parameter initialisers (called
// with 2, 4 and 7 arguments so the defaults run some of the time), and
// recursion through methods: object-literal methods, class methods, and a
// small polymorphic node hierarchy dispatching through prototypes.
//
// Why a likely weak spot: an interpreter's frame setup and teardown cost
// (argument adaptation, default initialisers, `this` binding, return through
// many frames) dominates here, and deep stacks stress any frame-pool or
// stack-growth logic that shallow benchmarks never touch.
//
// Dominant operations: function call/return, argument-count mismatch handling,
// default parameter evaluation, method lookup on `this`, small object
// allocation for tree nodes.
//
// Depth: QuickJS's default C stack holds roughly 1,500 frames of simple
// recursion (about 1,200 with seven default parameters), so the deepest
// recursion here is kept at about 700 frames.
"use strict";

const SIZE = 100;

let seed = 40503;
function rndInt(n) {
  seed = (Math.imul(seed, 1664525) + 1013904223) | 0;
  return ((seed >>> 0) % n) | 0;
}

// --- mutual recursion -------------------------------------------------------
function isEven(n) { return n === 0 ? true : isOdd(n - 1); }
function isOdd(n) { return n === 0 ? false : isEven(n - 1); }

// three-way mutual recursion carrying an accumulator
function stepA(n, acc) { return n <= 0 ? acc : stepB(n - 1, (acc + n) | 0); }
function stepB(n, acc) { return n <= 0 ? acc : stepC(n - 1, (acc ^ n) | 0); }
function stepC(n, acc) { return n <= 0 ? acc : stepA(n - 1, Math.imul(acc, 3) + 1 | 0); }

// --- Ackermann ---------------------------------------------------------------
let ackCalls = 0;
function ack(m, n) {
  ackCalls++;
  if (m === 0) return n + 1;
  if (n === 0) return ack(m - 1, 1);
  return ack(m - 1, ack(m, n - 1));
}

// --- tree build and walk -------------------------------------------------------
function build(depth, id) {
  if (depth === 0) return { id, left: null, right: null, w: id & 15 };
  return { id, left: build(depth - 1, id * 2 + 1), right: build(depth - 1, id * 2 + 2), w: (id * 7) & 15 };
}
function treeSum(t) { return t === null ? 0 : (t.w + treeSum(t.left) + treeSum(t.right)) | 0; }
function treeHeight(t) {
  if (t === null) return 0;
  const a = treeHeight(t.left), b = treeHeight(t.right);
  return (a > b ? a : b) + 1;
}
function pathHash(t, h) {
  if (t === null) return h;
  const x = (Math.imul(h, 31) + t.w) | 0;
  return (pathHash(t.left, x) ^ pathHash(t.right, x + 1)) | 0;
}
function mirror(t) {
  if (t === null) return null;
  return { id: t.id, left: mirror(t.right), right: mirror(t.left), w: t.w };
}
function sameShape(a, b) {
  if (a === null || b === null) return a === b;
  return a.w === b.w && sameShape(a.left, b.right) && sameShape(a.right, b.left);
}
// a degenerate (list-shaped) tree: deep rather than wide
function buildSpine(n) {
  let t = null;
  for (let i = 0; i < n; i++) t = { id: i, left: t, right: null, w: i & 7 };
  return t;
}

// --- backtracking and divide and conquer ----------------------------------------
function queens(n, row, cols, d1, d2) {
  if (row === n) return 1;
  let count = 0;
  for (let c = 0; c < n; c++) {
    const a = 1 << c, b = 1 << (row + c), d = 1 << (row - c + n - 1);
    if ((cols & a) || (d1 & b) || (d2 & d)) continue;
    count += queens(n, row + 1, cols | a, d1 | b, d2 | d);
  }
  return count;
}
function mergeSort(a, lo, hi, tmp) {
  if (hi - lo < 2) return 0;
  const mid = (lo + hi) >> 1;
  let inv = mergeSort(a, lo, mid, tmp) + mergeSort(a, mid, hi, tmp);
  let i = lo, j = mid, k = lo;
  while (i < mid && j < hi) {
    if (a[i] <= a[j]) tmp[k++] = a[i++];
    else { tmp[k++] = a[j++]; inv += mid - i; }
  }
  while (i < mid) tmp[k++] = a[i++];
  while (j < hi) tmp[k++] = a[j++];
  for (k = lo; k < hi; k++) a[k] = tmp[k];
  return inv;
}
function hanoi(n, from, to, via, moves) {
  if (n === 0) return 0;
  const a = hanoi(n - 1, from, via, to, moves);
  moves.sum = (moves.sum + from * 3 + to) | 0;
  return a + 1 + hanoi(n - 1, via, to, from, moves);
}

// --- many arguments and default parameters -----------------------------------
function blend(depth, a, b, c = (a ^ b) & 255, d = (c * 3 + 1) | 0, e = depth & 7, f = a + e) {
  if (depth <= 0) return (a + b + c + d + e + f) | 0;
  switch (depth % 3) {
    case 0: return (blend(depth - 1, b, a + 1) + c) | 0;                        // 3 args: c..f defaulted
    case 1: return (blend(depth - 1, a, b ^ c, d, e) ^ f) | 0;                  // 5 args: f defaulted
    default: return (blend(depth - 1, f & 1023, d & 1023, c, d, e, f) + 1) | 0;  // all 7 args
  }
}
function restSum(depth, first, ...rest) {
  if (depth === 0) return first + rest.length;
  return (first + restSum(depth - 1, rest.length, first, depth, ...rest.slice(0, 3))) | 0;
}

// --- method recursion -----------------------------------------------------------
const counter = {
  hits: 0,
  down(n) { this.hits++; return n === 0 ? 0 : this.down(n - 1) + (n & 3); },
  collatz(n, steps) { return n === 1 ? steps : this.collatz((n & 1) ? 3 * n + 1 : n >> 1, steps + 1); },
};

class Leaf {
  constructor(v) { this.v = v; }
  total() { return this.v; }
  count() { return 1; }
  depthOf() { return 1; }
}
class Pair {
  constructor(a, b, v) { this.a = a; this.b = b; this.v = v; }
  total() { return (this.v + this.a.total() + this.b.total()) | 0; }
  count() { return 1 + this.a.count() + this.b.count(); }
  depthOf() { const x = this.a.depthOf(), y = this.b.depthOf(); return 1 + (x > y ? x : y); }
}
class Chain {
  constructor(next, v) { this.next = next; this.v = v; }
  total() { return (this.v * 2 + this.next.total()) | 0; }
  count() { return 1 + this.next.count(); }
  depthOf() { return 1 + this.next.depthOf(); }
}
function buildNodes(depth) {
  if (depth <= 0) return new Leaf(rndInt(100));
  const r = rndInt(4);
  if (r === 0) return new Chain(buildNodes(depth - 1), rndInt(50));
  return new Pair(buildNodes(depth - 1), buildNodes(depth - 1 - (r & 1)), rndInt(50));
}

// --- driver -------------------------------------------------------------------------
let mutualHash = 0, ackHash = 0, treeHash = 0, btHash = 0, argHash = 0, methodHash = 0;
const sortBuf = new Array(2048), sortTmp = new Array(2048);
const spine = buildSpine(600);

for (let round = 0; round < SIZE; round++) {
  // mutual recursion, depth 200..700
  for (let k = 0; k < 40; k++) {
    const n = 200 + rndInt(500);
    mutualHash = (Math.imul(mutualHash, 3) + (isEven(n) ? n : -n)) | 0;
    mutualHash = (mutualHash + stepA(n, k)) | 0;
  }

  // Ackermann: ack(2, n) is shallow-and-long, ack(3, n) is deep and irregular
  ackHash = (ackHash + ack(2, 50 + rndInt(100)) + ack(3, 3 + (round % 3))) | 0;

  // tree build and four walks
  const t = build(9 + (round & 1), round);
  const m = mirror(t);
  treeHash = (treeHash + treeSum(t) + treeHeight(m) + pathHash(t, round)) | 0;
  if (sameShape(t, m)) treeHash = (treeHash + 1) | 0;
  treeHash = (treeHash ^ treeSum(spine) ^ treeHeight(spine)) | 0;

  // backtracking and divide and conquer
  btHash = (btHash + queens(6 + (round % 3), 0, 0, 0, 0)) | 0;
  for (let i = 0; i < sortBuf.length; i++) sortBuf[i] = rndInt(100000);
  btHash = (btHash + mergeSort(sortBuf, 0, sortBuf.length, sortTmp) + sortBuf[round & 2047]) | 0;
  const moves = { sum: 0 };
  btHash = (btHash + hanoi(10, 1, 3, 2, moves) + moves.sum) | 0;

  // argument shapes
  for (let k = 0; k < 20; k++) {
    argHash = (argHash + blend(100 + rndInt(200), k, round)) | 0;
    argHash = (argHash ^ restSum(50 + rndInt(100), k, 1, 2, 3)) | 0;
  }

  // method recursion
  for (let k = 0; k < 10; k++) {
    methodHash = (methodHash + counter.down(100 + rndInt(500)) + counter.collatz(27 + k + round, 0)) | 0;
  }
  const nodes = buildNodes(10);
  methodHash = (methodHash + nodes.total() + nodes.count() * 7 + nodes.depthOf()) | 0;
}

console.log("recursion-shapes rounds=" + SIZE + " ackCalls=" + ackCalls + " hits=" + counter.hits);
console.log("mutual=" + mutualHash + " ack=" + ackHash + " tree=" + treeHash);
console.log("backtrack=" + btHash + " args=" + argHash + " method=" + methodHash);
