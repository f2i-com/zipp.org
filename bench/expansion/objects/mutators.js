// mutators: the Array.prototype mutators and copiers, holey arrays, mixed kinds.
//
// What it measures:
//   1. queue and deque traffic: push/shift and unshift/pop on 250-500-element
//      arrays (front removal and front insertion move every element);
//   2. splice inserting and deleting in the middle of a 500-element array,
//      including splice with several inserted items and splice's returned
//      array;
//   3. copying and searching builtins: slice, concat, reverse, fill, indexOf,
//      lastIndexOf, includes (with NaN), join of a small slice;
//   4. flat (depth 1 and 2) and flatMap over nested arrays;
//   5. Array.from with a map function, from an array-like and from an array;
//   6. arrays turned holey (delete a[i], length truncation and extension,
//      writes past the end) and then iterated with an index loop + `in`,
//      forEach (skips holes), for-of (sees undefined) and Object.keys;
//   7. arrays of mixed element kinds (small ints, doubles, strings, objects,
//      booleans, null) scanned with typeof dispatch, and an int array that
//      transitions to doubles and then to generic values.
// Why it is a likely weak spot: these builtins are where an engine's array
// representation shows: element moves on shift/unshift/splice, element-kind
// transitions, hole checks and prototype-chain lookups for missing elements,
// and allocation of result arrays. Interpreters that call generic
// [[Get]]/[[Set]] per element instead of a dense fast path lose badly.
// Dominant operations: shift/unshift/splice element moves, slice/concat
// copies, indexOf/includes scans, holey-array iteration.
//
// SIZE is the number of rounds; every round does the same fixed amount of work.

const SIZE = 100;

let seed = 99991;
function rnd() { seed = (Math.imul(seed, 1664525) + 1013904223) | 0; return (seed >>> 0) / 4294967296; }
function mix(h, v) { return Math.imul(h ^ (v | 0), 16777619); }

function phaseQueue(r) {
  const q = [];
  for (let i = 0; i < 300; i++) q.push(i);
  let s = 0;
  for (let i = 0; i < 1500; i++) {
    s = (s + q.shift()) | 0;
    q.push(i + r);
    if ((i & 7) === 0) q.push(-i);
  }
  const d = [];
  for (let i = 0; i < 250; i++) d.unshift(i ^ r);
  for (let i = 0; i < 400; i++) {
    s = (s + d.pop() * 3) | 0;
    d.unshift(i, i + 1);
    s = (s + d.shift()) | 0;
  }
  return mix(mix(s, q.length), d.length + d[0] + d[d.length - 1]);
}

function phaseSplice(r) {
  const a = [];
  for (let i = 0; i < 500; i++) a.push((i * 7 + r) & 4095);
  let s = 0;
  for (let i = 0; i < 300; i++) {
    const at = 150 + ((i * 37) % 250);
    const removed = a.splice(at, 3);
    s = (s + removed[0] + removed[2]) | 0;
    a.splice(at - 100, 0, i, i * 2, i * 3);
    if ((i & 3) === 0) {
      const rem = a.splice(at, 1, -1, -2);
      s = (s + rem[0]) | 0;
      a.splice(a.length - 50, 1);
    }
  }
  for (let i = 0; i < a.length; i += 11) s = mix(s, a[i]);
  return mix(s, a.length);
}

function phaseCopySearch(r) {
  const base = [];
  for (let i = 0; i < 500; i++) base.push((i * 13 + r) % 251);
  let s = 0;
  for (let k = 0; k < 20; k++) {
    const part = base.slice(k * 10, k * 10 + 200);
    const both = part.concat(base.slice(-50), [k, k + 1]);
    both.reverse();
    s = (s + both[0] + both[both.length - 1] + both.length) | 0;
    const filled = new Array(64).fill(k);
    filled.fill(-k, 10, 20);
    s = (s + filled[15] + filled[63]) | 0;
    s = (s + base.indexOf(k * 9 + 1) + base.lastIndexOf(k * 9 + 1) + both.indexOf(250)) | 0;
    if (base.includes(k * 17 % 300)) s++;
    if (base.indexOf(300) === -1 && !base.includes(301)) s += 2;
    s = mix(s, part.slice(0, 5).join("-").length);
  }
  const withNaN = [1, NaN, 3];
  s = (s + withNaN.indexOf(NaN) + (withNaN.includes(NaN) ? 10 : 0)) | 0;
  return s;
}

function phaseFlat(r) {
  const nested = [];
  for (let i = 0; i < 120; i++) {
    if (i % 3 === 0) nested.push([i, [i + 1, i + 2]]);
    else if (i % 3 === 1) nested.push([i]);
    else nested.push(i);
  }
  let s = 0;
  for (let k = 0; k < 6; k++) {
    const f1 = nested.flat();
    const f2 = nested.flat(2);
    const fm = f2.flatMap((x, i) => (x & 1 ? [x, i] : (x % 4 === 0 ? [] : x * 2)));
    s = (s + f1.length * 3 + f2.length + fm.length + fm[fm.length - 1] + k + r) | 0;
  }
  return s;
}

function phaseFrom(r) {
  let s = 0;
  for (let k = 0; k < 10; k++) {
    const a = Array.from({ length: 200 }, (_, i) => i * k + r);
    const b = Array.from(a, (x) => x & 255);
    const c = Array.from({ length: 3, 0: "a", 1: "bc", 2: "def" }, (x) => x.length);
    s = (s + a[199] + b[150] + c[0] + c[1] + c[2] + b.length) | 0;
  }
  return s;
}

function phaseHoley(r) {
  let s = 0;
  for (let k = 0; k < 8; k++) {
    const a = [];
    for (let i = 0; i < 400; i++) a.push(i + k);
    for (let i = 0; i < 400; i += 7) delete a[i];
    a.length = 350;
    a[360] = 5;          // extends with a gap of holes
    a.length = 380;      // trailing holes
    let present = 0, sumIdx = 0;
    for (let i = 0; i < a.length; i++) {
      if (i in a) { present++; sumIdx = (sumIdx + a[i]) | 0; }
    }
    let fe = 0;
    a.forEach((v) => { fe = (fe + v) | 0; });
    let undef = 0;
    for (const v of a) if (v === undefined) undef++;
    const keys = Object.keys(a).length;
    const mapped = a.map((v) => v * 2);
    let mapPresent = 0;
    for (let i = 0; i < mapped.length; i++) if (i in mapped) mapPresent++;
    s = mix(s, present * 7 + sumIdx);
    s = mix(s, fe ^ (undef << 10) ^ (keys << 20));
    s = mix(s, mapPresent + a.indexOf(undefined) + (a.includes(undefined) ? 1 : 0));
  }
  return s + r;
}

function phaseMixed(r) {
  const objs = [{ w: 3 }, { w: 5 }, { w: 7 }];
  const m = [];
  for (let i = 0; i < 600; i++) {
    switch (i % 7) {
      case 0: m.push(i); break;
      case 1: m.push(i + 0.5); break;
      case 2: m.push("s" + (i & 15)); break;
      case 3: m.push(objs[i % 3]); break;
      case 4: m.push((i & 2) === 0); break;
      case 5: m.push(null); break;
      default: m.push(-i); break;
    }
  }
  let s = 0;
  for (let k = 0; k < 4; k++) {
    for (let i = 0; i < m.length; i++) {
      const v = m[i];
      switch (typeof v) {
        case "number": s = (s + ((v * 2) | 0)) | 0; break;
        case "string": s = (s + v.length) | 0; break;
        case "object": s = (s + (v === null ? 1 : v.w)) | 0; break;
        case "boolean": s = (s + (v ? 2 : 3)) | 0; break;
      }
    }
  }
  // element-kind transitions: ints -> doubles -> generic
  const t = [];
  for (let i = 0; i < 300; i++) t.push(i);
  for (let i = 0; i < 300; i += 3) t[i] = i / 4;
  let dsum = 0;
  for (let i = 0; i < t.length; i++) dsum += t[i];
  t[150] = "x";
  t.push(objs[0]);
  let gsum = 0;
  for (let i = 0; i < t.length; i++) if (typeof t[i] === "number") gsum += t[i];
  return mix(mix(s, dsum * 4), (gsum * 4) + r + t.indexOf("x"));
}

function main() {
  const rounds = SIZE;
  const acc = [0, 0, 0, 0, 0, 0, 0];
  for (let r = 0; r < rounds; r++) {
    acc[0] = mix(acc[0], phaseQueue(r));
    acc[1] = mix(acc[1], phaseSplice(r));
    acc[2] = mix(acc[2], phaseCopySearch(r));
    acc[3] = mix(acc[3], phaseFlat(r));
    acc[4] = mix(acc[4], phaseFrom(r));
    acc[5] = mix(acc[5], phaseHoley(r));
    acc[6] = mix(acc[6], phaseMixed(r));
  }
  console.log("queue", acc[0] >>> 0);
  console.log("splice", acc[1] >>> 0);
  console.log("copy", acc[2] >>> 0);
  console.log("flat", acc[3] >>> 0);
  console.log("from", acc[4] >>> 0);
  console.log("holey", acc[5] >>> 0);
  console.log("mixed", acc[6] >>> 0);
}

main();
