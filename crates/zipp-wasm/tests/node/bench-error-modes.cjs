// node --no-liftoff bench-error-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
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
  ['internal-range','','try{new Array(-1);}catch(e){s+=e.name.length;}'],
  ['internal-type','','try{Object.defineProperty(null,"x",{});}catch(e){s+=e.name.length;}'],
  ['internal-typedarray','','try{new Uint8Array(-1);}catch(e){s+=e.name.length;}'],
  ['internal-syntax','','try{JSON.parse("{");}catch(e){s+=e.name.length;}'],
  ['constructed-empty','','const e=new Error();s+=e.message.length+e.name.length;'],
  ['constructed-message','','const e=new TypeError("message");s+=e.message.length;'],
  ['constructed-cause','','const e=new Error("message",{cause:i});s+=e.cause;'],
  ['constructed-coerced','','const e=new Error({toString(){return "message";}});s+=e.message.length;'],
  ['constructed-subclass','class Custom extends Error {}','const e=new Custom("message",{cause:i});s+=e.cause;'],
].map(([name,setup,body])=>[name,`${setup};function w(){let s=0;for(let i=0;i<500;i++){${body}}return s}`]);
const results = {
  node:process.version, v8:process.versions.v8, flags:process.execArgv,
  cpu:os.cpus()[0].model, packages:names,
  description:'Internal errors and general Error constructors, including cause, message coercion and subclasses. 500 operations per call. Node oracle checks every result. Ten warmup pairs, shared batches targeting 5 ms (cap 64), 31 alternating pairs; includes host calls and checks.',
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
