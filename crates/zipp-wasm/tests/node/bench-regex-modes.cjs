// node --no-liftoff bench-regex-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
// Exercise the ASCII, code-unit and code-point executors separately. Compare
// actual match/capture offsets with Node before timing matching-only calls.
'use strict';
const assert = require('node:assert/strict');
const path = require('node:path');
const {performance} = require('node:perf_hooks');
const os = require('node:os');
const names = process.argv.slice(2);
if (names.length !== 2 || !process.execArgv.includes('--no-liftoff')) throw Error('Use --no-liftoff and two package directories');
const modules = names.map(n => require(path.resolve(n, 'zipp_wasm.js')));
const cases = [
  ['ascii-short', '[a-z]+', '', 'abc'],
  ['ucs2-short', '[αβ]+', '', 'αβ'],
  ['unicode-short', '[😀-🙏]+', 'u', '😀'],
  ['ascii-empty-loop', 'a*', '', 'bbb'],
  ['ucs2-empty-loop', 'α*', '', 'βββ'],
  ['unicode-empty-loop', '😀*', 'u', 'βββ'],
  ['zero-max-loop', 'a{0}', '', 'aaa'],
  ['ascii-lazy-loop', 'a*?b', '', 'aaaaaaaaab'],
  ['ucs2-lazy-loop', 'α*?β', '', 'αααααααααβ'],
  ['unicode-lazy-loop', '😀*?β', 'u', '😀😀😀😀😀β'],
  ['ascii-class', '[a-z]+', 'g', 'abc'.repeat(150)],
  ['ucs2-bmp-class', '[А-Я]+', 'g', 'ЖФЮ'.repeat(150)],
  ['ucs2-bmp-any', '.+', '', 'αβγ'.repeat(150)],
  ['ucs2-surrogate-units', '[\\ud800-\\udfff]+', '', '😀😃😄'.repeat(75)],
  ['unicode-bmp-class', '[А-Я]+', 'gu', 'ЖФЮ'.repeat(150)],
  ['unicode-astral-class', '[😀-🙏]+', 'gu', '😀😃😄'.repeat(75)],
  ['unicode-astral-any', '.+', 'u', '😀😃😄'.repeat(75)],
  ['ucs2-lookbehind', '(?<=(α+))β', '', 'α'.repeat(100) + 'β'],
  ['unicode-lookbehind', '(?<=(😀+))β', 'u', '😀'.repeat(100) + 'β'],
  ['ucs2-backreference', '(α+)β\\1', '', 'α'.repeat(100) + 'β' + 'α'.repeat(100)],
  ['unicode-backreference', '(😀+)β\\1', 'u', '😀'.repeat(100) + 'β' + '😀'.repeat(100)],
  ['unicode-casefold', '[sk]+', 'iu', 'ſK'.repeat(150)],
  ['unicode-property', '\\p{Script=Greek}+', 'u', 'αβγ'.repeat(150)],
  ['ucs2-alternation', '^(?:α|αβ)+γ$', '', 'αβ'.repeat(30) + 'γ'],
  ['unicode-alternation', '^(?:😀|😀β)+γ$', 'u', '😀β'.repeat(30) + 'γ'],
];
const results = {node:process.version, v8:process.versions.v8, cpu:os.cpus()[0].model, flags:process.execArgv, packages:names, description:'10 warmups and 31 alternating pairs; shared match count targets at least 5 ms (300 minimum, 6000 maximum); matching only, with precompiled patterns. All initial matches and capture offsets checked against Node.',cases:{}};
for (const [name, pattern, flags, input] of cases) {
  const source = `const r=new RegExp(${JSON.stringify(pattern)},${JSON.stringify(flags)});const t=${JSON.stringify(input)};
    function probe(){r.lastIndex=0;const m=r.exec(t);return JSON.stringify(m?[m.index,...m]:null)}
    function w(n){let total=0;for(let i=0;i<n;i++){r.lastIndex=0;const m=r.exec(t);if(m)total+=m.index+m[0].length+(m[1]?m[1].length:0)}return total}`;
  const oracle = new Function(source + '; return {probe,w}')();
  let iterations = 300;
  let expected = oracle.w(iterations);
  const median = a=>[...a].sort((a,b)=>a-b)[Math.floor(a.length/2)];
  const engines = modules.map(m => {const e = new m.Engine();e.initScript(source);return e;});
  const samples = [[],[]];
  try {
    for(const e of engines){assert.equal(e.callFunction('probe',[]),oracle.probe(),name);assert.equal(e.callFunction('w',[300]),expected,name);}
    const calibration = [[],[]];
    for(let round=0;round<5;round++) for(const i of round%2?[1,0]:[0,1]) {
      assert.ok(engines[i].renewInstructionBudget());
      const start = performance.now();
      const value = engines[i].callFunction('w',[iterations]);
      calibration[i].push(performance.now()-start);
      assert.equal(value,expected,name);
    }
    iterations = Math.min(6000,300*Math.max(1,Math.ceil(5/Math.min(...calibration.map(median)))));
    expected = oracle.w(iterations);
    for(let round=-10;round<31;round++) for(const i of round%2?[1,0]:[0,1]) {
      assert.ok(engines[i].renewInstructionBudget());
      const start = performance.now();
      const value = engines[i].callFunction('w',[iterations]);
      const time = performance.now()-start;
      assert.equal(value,expected,name);
      if(round>=0)samples[i].push(time);
    }
  } finally {for(const e of engines){e.dispose();e.free();}}
  const before=median(samples[0]),after=median(samples[1]);
  results.cases[name]={baseline_ms:before,candidate_ms:after,ratio:after/before,iterations,samples_ms:samples};
}
console.log(JSON.stringify(results,null,2));
