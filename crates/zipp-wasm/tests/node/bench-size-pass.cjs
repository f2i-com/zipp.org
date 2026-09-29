// node --no-liftoff bench-size-pass.cjs BASELINE_NODE_DIR CANDIDATE_NODE_DIR
// Append a case name to measure it in a fresh process without earlier workloads.
// Force V8's optimizing WASM compiler: a tier-up halfway through sequential
// benchmarks can otherwise masquerade as a large code-size optimization win.
'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const os = require('node:os');
const { performance } = require('node:perf_hooks');
const { WORKLOADS } = require('./bench.cjs');
// Cover the operations behind native experiment switches, not just arithmetic.
const FOCUSED = [
  { name: 'recursive-calls', arg: 18, src: 'let calls=0;function fib(n){calls++;return n<2?n:fib(n-1)+fib(n-2)}function w(n){calls=0;const value=fib(n);return value+calls}' },
  { name: 'error-range', arg: 1000, src: 'function w(n){let s=0;for(let i=0;i<n;i++){try{new Array(-1)}catch(e){s+=e.name === "RangeError"}}return s}' },
  { name: 'error-type', arg: 1000, src: 'function w(n){let s=0;for(let i=0;i<n;i++){try{Object.defineProperty(null,"x",{})}catch(e){s+=e.name === "TypeError"}}return s}' },
  { name: 'error-typedarray', arg: 1000, src: 'function w(n){let s=0;for(let i=0;i<n;i++){try{new Uint8Array(-1)}catch(e){s+=e.name === "RangeError"}}return s}' },
  { name: 'property-delete', arg: 3000, src: 'function w(n){let s=0;for(let i=0;i<n;i++){const o={a:i,b:i+1,c:i+2};delete o.b;s+=o.a+o.c}return s}' },
  { name: 'enumeration', arg: 2000, src: 'function w(n){const o={a:1,b:2,c:3,d:4,e:5};let s=0;for(let i=0;i<n;i++){for(const k in o)s+=o[k]}return s}' },
  { name: 'string-append-index', arg: 15000, src: 'function w(n){const a="abcdefghijklmnop";let s="";for(let i=0;i<n;i++){s+=a[i%16];s+=":"+i}return s.length}' },
  { name: 'regexp-unicode-compile', arg: 100, src: String.raw`let serial=0;function w(n){let s=0;for(let i=0;i<n;i++)s+=new RegExp("\\p{Letter}+|"+(serial++),"u").test("abc");return s}` },
  { name: 'regexp-matchall', arg: 1000, src: 'function w(n){let s=0;for(let i=0;i<n;i++){for(const m of "k12;k34;k56".matchAll(/k(\\d+)/g))s+=Number(m[1])}return s}' },
  { name: 'promise-resolve', arg: 1000, src: 'function w(n){for(let i=0;i<n;i++)Promise.resolve(i).then(x=>x+1);return n}' },
];
const names = process.argv.slice(2, 4);
const onlyCase = process.argv[4] || null;
if (names.length !== 2 || process.argv.length > 5 || !process.execArgv.includes('--no-liftoff')) {
  throw new Error('node --no-liftoff bench-size-pass.cjs BASELINE_NODE_DIR CANDIDATE_NODE_DIR [CASE]');
}
const modules = names.map(name => require(path.resolve(name, 'zipp_wasm.js')));
const results = {
  node: process.version, v8: process.versions.v8, flags: process.execArgv,
  cpu: os.cpus()[0].model, packages: names,
  description: 'Optimizing WASM compiler; 10 warmup pairs, shared batches targeting >= 5 ms, and 31 alternating paired samples. Times are per call. Narrow screening, not a general performance claim.',
  only_case: onlyCase,
  cases: {},
};
function measure(name, calls) {
  const median = values => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];
  const warmup = [[], []];
  for (let round = 0; round < 10; round++) {
    for (const i of round % 2 ? [1, 0] : [0, 1]) {
      const start = performance.now();
      calls[i]();
      warmup[i].push(performance.now() - start);
    }
  }
  // Batch tiny calls (especially the memoized recursion workload) so clock,
  // host-call and scheduler noise do not dominate a few microseconds of work.
  // Both packages use the same count, chosen from the quicker warmup median.
  const iterations = Math.min(512, Math.max(1, Math.ceil(5 / Math.min(...warmup.map(median)))));
  const samples = [[], []];
  for (let round = 0; round < 31; round++) {
    for (const i of round % 2 ? [1, 0] : [0, 1]) {
      const start = performance.now();
      for (let j = 0; j < iterations; j++) calls[i]();
      samples[i].push((performance.now() - start) / iterations);
    }
  }
  const before = median(samples[0]), after = median(samples[1]);
  results.cases[name] = { baseline_ms: before, candidate_ms: after, ratio: after / before, iterations, samples_ms: samples };
}

for (const w of [...WORKLOADS, ...FOCUSED].filter(w => !onlyCase || w.name === onlyCase)) {
  const engines = modules.map(m => { const e = new m.Engine(); e.initScript(w.src); return e; });
  try {
    const expected = new Function(w.src + '; return w')()(w.arg);
    for (const e of engines) assert.equal(e.callFunction('w', [w.arg]), expected, w.name);
    measure(w.name, engines.map(e => () => {
      assert.ok(e.renewInstructionBudget());
      return e.callFunction('w', [w.arg]);
    }));
  } finally { for (const e of engines) { e.dispose(); e.free(); } }
}
for (const count of [0, 200]) {
  if (onlyCase && onlyCase !== `javascript_init_${count}_functions`) continue;
  const source = Array.from({ length: count }, (_, i) => `function f${i}(x){return x+${i}}`).join('\n');
  measure(`javascript_init_${count}_functions`, modules.map(m => () => {
    const e = new m.Engine();
    try { e.initScript(source); }
    finally { e.dispose(); e.free(); }
  }));
}
if (modules.every(m => JSON.parse(m.zippProfile()).languages.includes('python'))) {
  for (const count of [1, 200]) {
    if (onlyCase && onlyCase !== `python_init_${count}_functions`) continue;
    const source = Array.from({ length: count }, (_, i) => `def f${i}(x):\n    return x + ${i}\n`).join('') + 'print(f0(42))\n';
    measure(`python_init_${count}_functions`, modules.map(m => () => {
      const e = new m.Engine();
      try { e.initSource(source, 'python'); assert.deepEqual(e.takeOutput(), ['42']); }
      finally { e.dispose(); e.free(); }
    }));
  }
}
if (onlyCase && !results.cases[onlyCase]) throw Error(`Unknown or unavailable case: ${onlyCase}`);
const ratios = WORKLOADS.filter(w => results.cases[w.name]).map(w => results.cases[w.name].ratio);
results.js_geomean_elapsed_ratio = ratios.length ? Math.exp(ratios.reduce((sum, r) => sum + Math.log(r), 0) / ratios.length) : null;
console.log(JSON.stringify(results, null, 2));
