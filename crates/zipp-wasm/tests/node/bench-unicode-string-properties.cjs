// node --no-liftoff bench-unicode-string-properties.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
// Compile Unicode properties of strings with distinct patterns to avoid cache
// hits. Compare matching with Node, and sandbox failures between both builds.
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
const england = String.fromCodePoint(0x1f3f4, 0xe0067, 0xe0062, 0xe0065, 0xe006e, 0xe0067, 0xe007f);
const cases = [
  ['Basic_Emoji', '😀'],
  ['Emoji_Keycap_Sequence', '1️⃣'],
  ['RGI_Emoji_Flag_Sequence', '🇦🇺'],
  ['RGI_Emoji_Modifier_Sequence', '👍🏽'],
  ['RGI_Emoji_Tag_Sequence', england],
  ['RGI_Emoji_ZWJ_Sequence', '👩‍💻'],
  ['RGI_Emoji', '👨‍👩‍👧'],
];
const inputs = [...cases.map(([, input]) => input), 'x', '', '\ud800', '🇦', '👩‍', '1'];
const median = a => [...a].sort((x, y) => x - y)[Math.floor(a.length / 2)];
const result = {
  node: process.version, v8: process.versions.v8, cpu: os.cpus()[0].model,
  flags: process.execArgv, packages: names,
  description: 'Distinct-pattern compilation; 10 warmup pairs, 31 alternating paired samples. Shared batch count targets 10 ms, capped at 64. Each timed batch uses a fresh engine initialized outside the timer, avoiding accumulated garbage across samples. Matching probes checked against Node; resource rejections checked between builds.',
  cases: {},
};
for (const [property, input] of cases) {
  const pattern = `^\\p{${property}}$`;
  const source = `let serial=0; const pattern=${JSON.stringify(pattern)}; const input=${JSON.stringify(input)};
    function probe(s){const m=new RegExp(pattern,'v').exec(s);return JSON.stringify(m?[m.index,m[0]]:null)}
    function probePattern(p,s,flags){const m=new RegExp(p,flags).exec(s);return JSON.stringify(m?[m.index,...m]:null)}
    function compile(n){let count=0;for(let i=0;i<n;i++){const r=new RegExp(pattern+'|never_match_'+serial++,'v');count+=r.test(input)?1:0}return count}
    function rejection(){try{new RegExp('\\\\p{RGI_Emoji}\\\\p{RGI_Emoji}','v');return 'accepted'}catch(e){return e.name+': '+e.message}}`;
  const engines = modules.map(m => { const e = new m.Engine(); e.initScript(source); return e; });
  const samples = [[], []];
  let iterations = 1;
  function call(i, count) {
    const engine = new modules[i].Engine();
    try {
      engine.initScript(source);
      assert.ok(engine.renewInstructionBudget());
      const start = performance.now();
      const value = engine.callFunction('compile', [count]);
      const ms = performance.now() - start;
      assert.equal(value, count, property);
      return ms;
    } finally {
      engine.dispose(); engine.free();
    }
  }
  try {
    const oracle = new RegExp(pattern, 'v');
    for (const text of inputs) {
      const match = oracle.exec(text);
      const expected = JSON.stringify(match ? [match.index, match[0]] : null);
      for (const e of engines) {
        assert.ok(e.renewInstructionBudget());
        assert.equal(e.callFunction('probe', [text]), expected, `${property}: ${JSON.stringify(text)}`);
      }
    }
    // Exercise the separate class-set copy path and captured alternation,
    // including Unicode ignore-case mode, outside every timing interval.
    for (const p of [`^([\\p{${property}}])$`, `^(\\p{${property}}|x)$`]) {
      for (const flags of ['v', 'iv']) {
        const reference = new RegExp(p, flags);
        for (const text of inputs) {
          const m = reference.exec(text);
          const expected = JSON.stringify(m ? [m.index, ...m] : null);
          for (const e of engines) {
            assert.ok(e.renewInstructionBudget());
            assert.equal(e.callFunction('probePattern', [p, text, flags]), expected,
              `${p}/${flags}: ${JSON.stringify(text)}`);
          }
        }
      }
    }
    const rejected = engines.map(e => { e.renewInstructionBudget(); return e.callFunction('rejection', []); });
    assert.equal(rejected[0], rejected[1]);
    assert.match(rejected[0], /sandbox limit/);
    const calibration = [[], []];
    for (let round = 0; round < 5; round++) {
      for (const i of round % 2 ? [1, 0] : [0, 1]) calibration[i].push(call(i, 1));
    }
    iterations = Math.min(64, Math.max(1, Math.ceil(10 / Math.min(...calibration.map(median)))));
    for (let round = -10; round < 31; round++) {
      for (const i of round % 2 ? [1, 0] : [0, 1]) {
        const time = call(i, iterations) / iterations;
        if (round >= 0) samples[i].push(time);
      }
    }
  } finally {
    for (const e of engines) { e.dispose(); e.free(); }
  }
  const before = median(samples[0]), after = median(samples[1]);
  result.cases[property] = {
    baseline_ms: before, candidate_ms: after, ratio: after / before,
    iterations, samples_ms: samples,
  };
}
console.log(JSON.stringify(result, null, 2));
