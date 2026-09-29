// node --no-liftoff bench-subclass-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
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
  ['plain-parent','function Parent(x){this.x=x;}class C extends Parent {}','s+=new C(i).x;'],
  ['array','class C extends Array {}','s+=new C(16).length;'],
  ['typedarray','class C extends Uint8Array {}','s+=new C(16).length;'],
  ['dataview','class C extends DataView {} const b=new ArrayBuffer(32);','s+=new C(b,2,16).byteLength;'],
  ['boolean','class C extends Boolean {}','s+=new C(1).valueOf();'],
  ['number','class C extends Number {}','s+=new C(i).valueOf();'],
  ['string','class C extends String {}','s+=new C("sample").length;'],
  ['date','class C extends Date {}','s+=new C(i).getTime();'],
  ['regexp','class C extends RegExp {}','s+=new C("a+").test("aaa");'],
  ['function','class C extends Function {}','s+=new C("x","return x+1")(i);'],
  ['generator','const G=Object.getPrototypeOf(function*(){}).constructor;class C extends G {}','s+=new C("x","yield x+1")(i).next().value;'],
].map(([name,setup,body])=>[name,`${setup};function w(){let s=0;for(let i=0;i<100;i++){${body}}return s}`]);
const results = {
  node:process.version, v8:process.versions.v8, flags:process.execArgv,
  cpu:os.cpus()[0].model, packages:names,
  description:'Intrinsic subclass allocation and use, 100 operations per call. Node oracle checks every result. Ten warmup pairs, shared batches targeting 5 ms (cap 64; dynamic Function/GeneratorFunction cap 2 to stay within lifetime quotas), 31 alternating pairs; includes host calls and checks.',
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
    // Dynamic-construction quotas are lifetime limits, not instruction budgets.
    // Each dynamic constructor retains two functions.
    // 100 calls * (10 warmups + 31 batches * 2) * 2 = 14,400 < 16,384.
    const cap = name === 'function' || name === 'generator' ? 2 : 64;
    const iterations=Math.min(cap,Math.max(1,Math.ceil(5/Math.min(...warmup.map(median)))));
    for(let round=0;round<31;round++) for(const i of round%2?[1,0]:[0,1]) {
      const start=performance.now();for(let j=0;j<iterations;j++)call(i);
      samples[i].push((performance.now()-start)/iterations);
    }
    const before=median(samples[0]),after=median(samples[1]);
    results.cases[name]={baseline_ms:before,candidate_ms:after,ratio:after/before,iterations,samples_ms:samples};
  } finally { for(const e of engines){e.dispose();e.free()} }
}
console.log(JSON.stringify(results,null,2));
