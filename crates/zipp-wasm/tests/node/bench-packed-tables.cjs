// node bench-packed-tables.cjs <baseline-node-package-dir> <candidate-node-package-dir>
const path = require('node:path'), {performance} = require('node:perf_hooks');
const names = process.argv.slice(2);
if (names.length !== 2) throw Error('Pass baseline and candidate Node package directories');
const modules = names.map(n => require(path.resolve(n,'zipp_wasm.js')));
const workloads = {
  regex_compile: String.raw`let serial=0; function w(n) {let hits=0; for(let i=0;i<n;i++) hits+=new RegExp("\\p{Letter}+|"+(serial++),"u").test("abcdef"); return hits;}`,
  named_timezone: `function w(n) {let s=0; for(let i=0;i<n;i++) s+=Temporal.ZonedDateTime.from("2024-01-01T00:00[Australia/Sydney]").hour; return s;}`,
};
const result={description:'Warm Node WASM, 5 warmups, 21 alternating paired samples; 100 operations each. Narrow screening, not a full-engine performance claim.',cases:{}};
for(const [name,source] of Object.entries(workloads)) {
  const engines=modules.map(m=>{const e=new m.Engine();e.initScript(source);return e});
  const samples=[[],[]];
  try {
    for(let round=-5;round<21;round++) for(const i of (round%2?[1,0]:[0,1])) {
      const start=performance.now(); const value=engines[i].callFunction('w',[100]);
      const time=performance.now()-start;
      if(value!==(name==='regex_compile'?100:0))throw Error('wrong result: '+value);
      if(round>=0)samples[i].push(time);
    }
  } finally {for(const e of engines){e.dispose();e.free();}}
  const median=a=>[...a].sort((a,b)=>a-b)[Math.floor(a.length/2)];
  result.cases[name]={baseline_ms:median(samples[0]),candidate_ms:median(samples[1]),ratio:median(samples[1])/median(samples[0]),samples_ms:samples};
}
console.log(JSON.stringify(result,null,2));
