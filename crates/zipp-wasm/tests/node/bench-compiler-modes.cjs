// node --no-liftoff bench-compiler-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
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
  ['register-classes', 'let sum=0;for(let i=0;i<s.length;i++){const c=s.charCodeAt(i);if(c>64&&c<123)sum+=c;}return sum;'],
  ['class-optional-operands', 'const x=s.charCodeAt(0);class C extends Array {value(){return x+this.length}}const c=new C(1,2,3);return c.value()+new Set([x]).size+new Number(x).valueOf();'],
  ['argument-windows', 'const x=s.charCodeAt(0);const valid=x>64;const f=(a,b,c)=>a+b+c;return f(x,Math.max(x,3,4),valid?2:0);'],
].map(([name, body]) => [name, Array.from({length:80}, (_, i) => `function f${i}(s){${body}}`).join('\n') + ';function w(){return f79("SampleText")}']);
const results = {
  node:process.version, v8:process.versions.v8, flags:process.execArgv,
  cpu:os.cpus()[0].model, packages:names,
  description:'Fresh engine, compile 80 functions, execute one, dispose. Register classes, optional operands and argument windows. Every result checked against Node. Ten warmup pairs, shared batches targeting 5 ms (cap 32), 31 alternating paired samples; includes host calls and checks.',
  cases:{},
};
for (const [name, source] of cases) {
  const expected = vm.runInNewContext(source+';w()');
  const warmup=[[],[]], samples=[[],[]];
  function call(i) {
    const e=new modules[i].Engine();
    try { e.initScript(source); assert.equal(e.callFunction('w', []), expected, name); }
    finally { e.dispose(); e.free(); }
  }
  for(let round=0;round<10;round++) for(const i of round%2?[1,0]:[0,1]) {
    const start=performance.now();call(i);warmup[i].push(performance.now()-start);
  }
  const iterations=Math.min(32,Math.max(1,Math.ceil(5/Math.min(...warmup.map(median)))));
  for(let round=0;round<31;round++) for(const i of round%2?[1,0]:[0,1]) {
    const start=performance.now();for(let j=0;j<iterations;j++)call(i);
    samples[i].push((performance.now()-start)/iterations);
  }
  const before=median(samples[0]),after=median(samples[1]);
  results.cases[name]={baseline_ms:before,candidate_ms:after,ratio:after/before,iterations,samples_ms:samples};
}
console.log(JSON.stringify(results,null,2));
