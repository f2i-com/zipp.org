// node --no-liftoff bench-promise-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const os = require('node:os');
const vm = require('node:vm');
const {performance} = require('node:perf_hooks');
const names = process.argv.slice(2);
if (names.length !== 2 || !process.execArgv.includes('--no-liftoff')) throw Error('Use --no-liftoff and two package directories');
const modules = names.map(name => require(path.resolve(name, 'zipp_wasm.js')));
const median = a => [...a].sort((x, y) => x - y)[Math.floor(a.length / 2)];
const cases = [
  ['settled-then', '', 'for(let i=0;i<n;i++)Promise.resolve(i).then(x=>{total+=x})'],
  ['pending-chain', '', 'let resolve;let p=new Promise(r=>{resolve=r});for(let i=0;i<n;i++)p=p.then(x=>x+1);p.then(x=>{total=x});resolve(0)'],
  ['await-loop', 'async function work(n){for(let i=0;i<n;i++)total+=await Promise.resolve(i)}', 'work(n)'],
  ['catch', '', 'for(let i=0;i<n;i++)Promise.reject(i).catch(x=>{total+=x})'],
  ['finally', '', 'for(let i=0;i<n;i++)Promise.resolve(i).finally(()=>{total++})'],
  ['subclass', 'class P extends Promise {}', 'for(let i=0;i<n;i++)P.resolve(i).then(x=>{total+=x})'],
  ['patched-then', 'const original=Promise.prototype.then;Promise.prototype.then=function(f,r){total++;return original.call(this,f,r)}', 'for(let i=0;i<n;i++)Promise.resolve(i).then(x=>{total+=x})'],
  ['await-primitive', 'async function work(n){for(let i=0;i<n;i++)total+=await i}', 'work(n)'],
];
const results = {
  node:process.version, v8:process.versions.v8, flags:process.execArgv,
  cpu:os.cpus()[0].model, packages:names,
  description:'Eight Promise shapes, 512 operations per call. Node oracle checks the drained result after every call. Ten warmup pairs, shared batches targeting 5 ms (cap 64), 31 alternating pairs. Timing includes budget renewal, host calls and result checks.',
  cases:{},
};
for (const [name, setup, body] of cases) {
  const source = `let total=0;${setup};function w(n){total=0;${body}}function probe(){return total}`;
  const context = vm.createContext({}, {microtaskMode:'afterEvaluate'});
  vm.runInContext(source + ';w(512)', context);
  const expected = vm.runInContext('probe()', context);
  const engines = modules.map(m=>{const e=new m.Engine();e.initScript(source);return e});
  const warmup=[[],[]], samples=[[],[]];
  function call(i) {
    assert.ok(engines[i].renewInstructionBudget());
    engines[i].callFunction('w',[512]);
    assert.equal(engines[i].callFunction('probe',[]),expected,name);
  }
  try {
    for(let round=0;round<10;round++) for(const i of round%2?[1,0]:[0,1]) {
      const start=performance.now();call(i);warmup[i].push(performance.now()-start);
    }
    const iterations=Math.min(64,Math.max(1,Math.ceil(5/Math.min(...warmup.map(median)))));
    for(let round=0;round<31;round++) for(const i of round%2?[1,0]:[0,1]) {
      const start=performance.now();for(let j=0;j<iterations;j++)call(i);
      samples[i].push((performance.now()-start)/iterations);
    }
    const before=median(samples[0]),after=median(samples[1]);
    results.cases[name]={baseline_ms:before,candidate_ms:after,ratio:after/before,iterations,samples_ms:samples};
  } finally { for(const e of engines){e.dispose();e.free()} }
}
console.log(JSON.stringify(results,null,2));
