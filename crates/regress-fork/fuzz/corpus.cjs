// node corpus.cjs MINIMIZED.jsonl... : merge minimized findings (minimize.cjs
// output with `node` and `zipp` outputs) into corpus.js, the regression
// corpus that crates/zipp-vm/tests/regex_fuzz_corpus.rs runs. Each entry
// keeps the case, ZIPP's output when it was recorded (what the test pins)
// and Node's output without error messages (what the spec reading agrees
// with in most cases). Existing entries are kept; new cases are appended.
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const file = path.join(__dirname, 'corpus.js');
const strip = l => (l === undefined ? l : (l.indexOf(' || ') < 0 ? l : l.slice(0, l.indexOf(' || '))));

function load() {
  if (!fs.existsSync(file)) return [];
  const src = fs.readFileSync(file, 'utf8');
  const box = {};
  new Function('box', src + '\nbox.CASES = CASES; box.ZIPP = ZIPP; box.NODE = NODE;')(box);
  return box.CASES.map((c, i) => ({ case: c, zipp: box.ZIPP[i], node: box.NODE[i] }));
}

const entries = load();
const seen = new Set(entries.map(e => JSON.stringify(e.case)));
for (const f of process.argv.slice(2)) {
  for (const line of fs.readFileSync(f, 'utf8').split('\n').filter(Boolean)) {
    const r = JSON.parse(line);
    const key = JSON.stringify(r.case);
    if (seen.has(key) || r.zipp === undefined || r.node === undefined) continue;
    if (strip(r.zipp) === strip(r.node)) continue;
    seen.add(key);
    entries.push({ case: r.case, zipp: r.zipp, node: strip(r.node) });
  }
}
const q = x => JSON.stringify(x);
const out = [
  '// The differential fuzzer\'s regression corpus (fuzz/corpus.cjs writes it):',
  '// every case on which ZIPP and Node 24 disagreed, minimized. ZIPP[i] is',
  '// ZIPP\'s harness output for CASES[i] when it was recorded, and the test',
  '// crates/zipp-vm/tests/regex_fuzz_corpus.rs pins it; NODE[i] is Node\'s,',
  '// without error messages. Most entries are ZIPP bugs or V8 bugs left as',
  '// found; an entry that comes to agree with Node is a behaviour change to',
  '// explain, and its ZIPP line is then re-recorded.',
  'var CASES = [',
  ...entries.map(e => '  ' + q(e.case) + ','),
  '];',
  'var ZIPP = [',
  ...entries.map(e => '  ' + q(e.zipp) + ','),
  '];',
  'var NODE = [',
  ...entries.map(e => '  ' + q(e.node) + ','),
  '];',
  '',
];
fs.writeFileSync(file, out.join('\n'));
console.log(`${entries.length} entries in ${file}`);
