// node --no-liftoff bench-sort-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
// Default Array and TypedArray sorting exercise the standard-library sorts;
// a numeric JS comparator exercises the VM's separate comparator-sort path.
'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const os = require('node:os');
const { performance } = require('node:perf_hooks');
const names = process.argv.slice(2);
if (names.length !== 2 || !process.execArgv.includes('--no-liftoff')) {
  throw Error('Use --no-liftoff and two package directories');
}
const modules = names.map(name => require(path.resolve(name, 'zipp_wasm.js')));
const median = a => [...a].sort((x, y) => x - y)[Math.floor(a.length / 2)];
const results = {
  node: process.version, v8: process.versions.v8, cpu: os.cpus()[0].model,
  flags: process.execArgv, packages: names,
  description: 'Node oracle checks the complete sorted sequence, signed zero and comparator stability. Sorting includes a fresh input copy. Five calibration pairs, ten warmups, 31 alternating paired samples; shared batch count targets 5 ms and is capped at 64. Setup, checks and budget renewal are outside timing.',
  cases: {},
};
let seed = 42;
function random() { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed % 10000; }
const cases = [];
for (const size of [32, 2048]) {
  const randomValues = Array.from({ length: size }, random);
  for (const [shape, values] of [
    ['random', randomValues],
    ['sorted', [...randomValues].sort((a, b) => a - b)],
    ['reverse', [...randomValues].sort((a, b) => b - a)],
    ['duplicates', randomValues.map(x => x % 4)],
  ]) {
    for (const mode of ['array-default', 'array-numeric', 'typed-default']) {
      const input = mode === 'array-default' && ['sorted', 'reverse'].includes(shape)
        ? [...values].sort() : values;
      if (mode === 'array-default' && shape === 'reverse') input.reverse();
      cases.push({ name: `${mode}-${shape}-${size}`, values: input, mode });
    }
  }
}
for (const { name, values, mode } of cases) {
  const typed = mode === 'typed-default';
  const compare = mode === 'array-numeric' ? '(a,b)=>a-b' : '';
  // Subtracting NaN is an inconsistent comparator: implementations may then
  // produce different orders. Default TypedArray sorting has defined NaN order.
  const edges = mode === 'array-numeric'
    ? '[-0,0,Infinity,-Infinity,-2,2]' : '[NaN,-0,0,Infinity,-Infinity,NaN,-2,2]';
  const source = `const input=${typed ? 'new Float64Array(' : ''}${JSON.stringify(values)}${typed ? ')' : ''};
    function sorted(){return input.slice().sort(${compare})}
    function probe(){return JSON.stringify(Array.from(sorted()))}
    function w(n){let sum=0;for(let j=0;j<n;j++){const a=sorted();sum+=a[0]+a[a.length-1]+a[a.length>>1]}return sum}
    function edges(){const a=${typed ? 'new Float64Array(' : ''}${edges}${typed ? ')' : ''};a.sort(${compare});return Array.from(a,x=>Number.isNaN(x)?'NaN':Object.is(x,-0)?'-0':String(x)).join(',')}
    function stable(){const a=[];for(let i=0;i<128;i++)a.push({key:i%4,id:i});a.sort((a,b)=>a.key-b.key);return a.map(x=>x.id).join(',')}`;
  const oracle = new Function(source + ';return {probe,w,edges,stable}')();
  const engines = modules.map(m => { const e = new m.Engine(); e.initScript(source); return e; });
  const samples = [[], []];
  let iterations = 1;
  let expected = oracle.w(iterations);
  function call(i) {
    assert.ok(engines[i].renewInstructionBudget());
    const start = performance.now();
    const value = engines[i].callFunction('w', [iterations]);
    const ms = performance.now() - start;
    assert.equal(value, expected, name);
    return ms / iterations;
  }
  try {
    for (const e of engines) {
      for (const probe of ['probe', 'edges', 'stable']) {
        assert.ok(e.renewInstructionBudget());
        assert.equal(e.callFunction(probe, []), oracle[probe](), `${name}: ${probe}`);
      }
    }
    const calibration = [[], []];
    for (let round = 0; round < 5; round++) {
      for (const i of round % 2 ? [1, 0] : [0, 1]) calibration[i].push(call(i));
    }
    iterations = Math.min(64, Math.max(1, Math.ceil(5 / Math.min(...calibration.map(median)))));
    expected = oracle.w(iterations);
    for (let round = -10; round < 31; round++) {
      for (const i of round % 2 ? [1, 0] : [0, 1]) {
        const ms = call(i);
        if (round >= 0) samples[i].push(ms);
      }
    }
  } finally {
    for (const e of engines) { e.dispose(); e.free(); }
  }
  const before = median(samples[0]), after = median(samples[1]);
  results.cases[name] = { baseline_ms: before, candidate_ms: after, ratio: after / before, iterations, samples_ms: samples };
}
console.log(JSON.stringify(results, null, 2));
