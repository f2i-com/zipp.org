// node --no-liftoff bench-json-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
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
const cases = [];
for (const count of [0,1,3,4,5,8,64,2048]) {
  const fields=Array.from({length:count},(_,i)=>`"k${i}":${i}`);
  for (const reviver of [false,true]) {
    const text='{'+fields.join(',')+'}';
    const source=`const text=${JSON.stringify(text)};function w(){const o=JSON.parse(text${reviver?',function(k,v,c){return v;}':''});return Object.keys(o).length+(${count}?o.k${Math.max(0,count-1)}:0)}`;
    cases.push([`${reviver?'reviver':'plain'}-${count}`,source]);
  }
}
const results = {
  node:process.version, v8:process.versions.v8, flags:process.execArgv,
  cpu:os.cpus()[0].model, packages:names,
  description:'JSON objects with 0-2048 members, plain and identity-reviver parse. Node oracle checks every result. Ten warmup pairs, shared batches targeting 5 ms (cap 64), 31 alternating pairs. Includes budget renewal, host calls and result checks.',
  cases:{},
};
for (const [name, source] of cases) {
  const expected=vm.runInNewContext(source+';w()');
  const engines = modules.map(m=>{const e=new m.Engine();e.initScript(source);return e});
  const warmup=[[],[]], samples=[[],[]];
  function call(i) {
    assert.ok(engines[i].renewInstructionBudget());
    assert.equal(engines[i].callFunction('w',[]),expected,name);
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
