// node --no-liftoff bench-size-pass.cjs BASELINE_NODE_DIR CANDIDATE_NODE_DIR
// Force V8's optimizing WASM compiler: a tier-up halfway through sequential
// benchmarks can otherwise masquerade as a large code-size optimization win.
'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const os = require('node:os');
const { performance } = require('node:perf_hooks');
const { WORKLOADS } = require('./bench.cjs');
const names = process.argv.slice(2);
if (names.length !== 2 || !process.execArgv.includes('--no-liftoff')) {
  throw new Error('node --no-liftoff bench-size-pass.cjs BASELINE_NODE_DIR CANDIDATE_NODE_DIR');
}
const modules = names.map(name => require(path.resolve(name, 'zipp_wasm.js')));
const results = {
  node: process.version, v8: process.versions.v8, flags: process.execArgv,
  cpu: os.cpus()[0].model, packages: names,
  description: 'Optimizing WASM compiler; 10 warmups and 31 alternating paired samples. Narrow screening, not a general performance claim.',
  cases: {},
};
function measure(name, calls) {
  const samples = [[], []];
  for (let round = -10; round < 31; round++) {
    for (const i of round % 2 ? [1, 0] : [0, 1]) {
      const start = performance.now();
      calls[i]();
      if (round >= 0) samples[i].push(performance.now() - start);
    }
  }
  const median = values => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];
  const before = median(samples[0]), after = median(samples[1]);
  results.cases[name] = { baseline_ms: before, candidate_ms: after, ratio: after / before, samples_ms: samples };
}
for (const w of WORKLOADS) {
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
if (modules.every(m => JSON.parse(m.zippProfile()).languages.includes('python'))) {
  for (const count of [1, 200]) {
    const source = Array.from({ length: count }, (_, i) => `def f${i}(x):\n    return x + ${i}\n`).join('') + 'print(f0(42))\n';
    measure(`python_init_${count}_functions`, modules.map(m => () => {
      const e = new m.Engine();
      try { e.initSource(source, 'python'); assert.deepEqual(e.takeOutput(), ['42']); }
      finally { e.dispose(); e.free(); }
    }));
  }
}
const ratios = WORKLOADS.map(w => results.cases[w.name].ratio);
results.js_geomean_elapsed_ratio = Math.exp(ratios.reduce((sum, r) => sum + Math.log(r), 0) / ratios.length);
console.log(JSON.stringify(results, null, 2));
