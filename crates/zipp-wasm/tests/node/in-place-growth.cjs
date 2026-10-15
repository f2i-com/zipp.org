// Memory that grows in place has to reach the collector, and the ceiling.
//
// An array is born empty and pushed, or index-stored, to a megabyte; an
// object gathers twenty thousand keys; a Map fills. None of that allocates an
// object, so neither the count-based schedule nor the size-at-birth one
// (large-alloc-gc.cjs) saw it. A function that builds one 80,000-element array
// and returns ended its engine on the 512th call ("script exceeded its memory
// budget", with a megabyte live), and a loop that stores at index 131,071 of a
// fresh array reached the end of the linear memory inside one call: a trap,
// after which no engine of the instance could be made.
//
// The module's allocator now reports what it has outstanding and the
// collector's poll reads it.
//
// A store more than 1,024 holes past an array's end goes to its sparse side
// table rather than growing it (0.0.25), so `a[131071]=1` on an empty array
// no longer makes a megabyte. The index stores below step by 1,024, each
// within the dense gap, and grow the same megabyte in place.
const { Engine } = require("./pkg/zipp_wasm.js");

let pass = 0;
let fail = 0;
function check(label, ok, detail) {
  if (ok) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}${detail ? " — " + detail : ""}`); }
}

const MIB = 1024 * 1024;

function run(source, calls, name, args) {
  const e = new Engine();
  let value;
  let failure = "";
  let heapBytes = 0;
  try {
    const r = e.initScript(source);
    if (r && r.error) throw new Error("initScript: " + r.error);
    e.setInstructionBudget(2000000000);
    for (let i = 0; i < calls; i++) {
      e.renewInstructionBudget();
      value = e.callFunction(name, args);
    }
    heapBytes = e.resourceUsage().heapBytes;
  } catch (err) {
    failure = String(err && err.message ? err.message : err);
  }
  try { e.dispose(); e.free(); } catch {}
  return { value, failure, heapBytes };
}

// ---- garbage: nothing here is live for longer than an iteration ----------
const GARBAGE = [
  ["an 80,000-element array pushed and dropped, 700 calls",
    "function w(n){const a=[];for(let i=0;i<n;i++)a.push(i);let s=0;for(let i=0;i<a.length;i++)s+=a[i];return s}",
    700, [80000], 3199960000],
  ["a megabyte index store on a fresh array, 2,000 times in one call",
    "function w(){let s=0;for(let i=0;i<2000;i++){const a=[];for(let j=1023;j<131072;j+=1024)a[j]=1;s+=a.length}return s}",
    1, [], 262144000],
  ["a megabyte length set and fill, 2,000 times in one call",
    "function w(){let s=0;for(let i=0;i<2000;i++){const a=[];a.length=131072;a.fill(0);s+=a.length}return s}",
    1, [], 262144000],
  ["a megabyte array pushed, 600 times in one call",
    "function w(){let s=0;for(let i=0;i<600;i++){const a=[];for(let j=0;j<131072;j++)a.push(j);s+=a.length}return s}",
    1, [], 78643200],
  ["an object given 20,000 keys, 300 times in one call",
    "function w(){let s=0;for(let i=0;i<300;i++){const o={};for(let j=0;j<20000;j++)o['k'+j]=j;s+=o.k7}return s}",
    1, [], 2100],
  ["a Map given 50,000 numbers, 300 times in one call",
    "function w(){let s=0;for(let i=0;i<300;i++){const o=new Map();for(let j=0;j<50000;j++)o.set(j,j);s+=o.size}return s}",
    1, [], 15000000],
  ["a Set given 50,000 numbers, 300 times in one call",
    "function w(){let s=0;for(let i=0;i<300;i++){const o=new Set();for(let j=0;j<50000;j++)o.add(j);s+=o.size}return s}",
    1, [], 15000000],
  ["a Map given 250 numbers, 40,000 times in one call",
    "function w(){let s=0;for(let i=0;i<40000;i++){const o=new Map();for(let j=0;j<250;j++)o.set(j,j);s+=o.size}return s}",
    1, [], 10000000],
];
for (const [label, source, calls, args, expected] of GARBAGE) {
  const r = run(source, calls, "w", args);
  check(label, r.failure === "" && r.value === expected && r.heapBytes < 128 * MIB,
    r.failure || `value ${r.value}, heap figure ${(r.heapBytes / MIB).toFixed(1)} MiB`);
}

// ---- live growth: the ceiling's error, not the linear memory's trap ------
const LIVE = [
  ["600 live megabyte arrays, each made and index-stored",
    "var keep=[];function grow(n){for(let i=0;i<n;i++){const a=[];for(let j=1023;j<131072;j+=1024)a[j]=1;keep.push(a)}return keep.length}",
    "grow", [600]],
  // Nothing is allocated while these grow: only the poll at the loop's
  // back-edge can see them.
  ["600 live megabyte arrays that existed before they grew",
    "var arrs=[];for(let k=0;k<600;k++)arrs.push([]);function fill(n){for(let i=0;i<n;i++){const a=arrs[i];for(let j=1023;j<131072;j+=1024)a[j]=1}return n}",
    "fill", [600]],
];
for (const [label, source, name, args] of LIVE) {
  const r = run(source, 1, name, args);
  check(`${label} meet the memory budget`, /memory budget/.test(r.failure), r.failure || `returned ${r.value}`);
  // The instance is intact: a trap would have left every later engine dead.
  const after = run("function ok(){return 6*7}", 1, "ok", []);
  check(`an engine made after ${label.split(",")[0]} runs`, after.failure === "" && after.value === 42, after.failure);
}

console.log(`\n${pass} passed, ${fail} failed`);
if (fail) process.exit(1);
