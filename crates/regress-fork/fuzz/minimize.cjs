// node minimize.cjs IN.jsonl OUT.jsonl A=SPEC B=SPEC : shrink the subject (and
// lastIndex) of each fuzz.cjs finding while engines A and B still disagree on
// it. SPEC is `node` or as in fuzz.cjs; error messages are ignored when one
// side is Node. Candidates of every finding run in one batch per round.
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { spawnSync } = require('node:child_process');

const HARNESS = fs.readFileSync(path.join(__dirname, 'harness.js'), 'utf8');
const WASM_RUNNER = path.join(__dirname, 'wasm_runner.cjs');
const [inFile, outFile, ...specs] = process.argv.slice(2);
const engines = specs.map(s => { const k = s.indexOf('='); return { label: s.slice(0, k), spec: s.slice(k + 1) }; });
if (engines.length !== 2) throw new Error('give two engines A=SPEC B=SPEC');
const anyNode = engines.some(e => e.spec === 'node');
const strip = l => (l === undefined || !anyNode ? l : (l.indexOf(' || ') < 0 ? l : l.slice(0, l.indexOf(' || '))));
const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'rxmin-'));

function run(spec, cases) {
  const file = path.join(dir, 'b.js'), json = path.join(dir, 'b.json');
  let argv, env = process.env;
  if (spec.startsWith('wasm:')) {
    fs.writeFileSync(json, JSON.stringify(cases));
    argv = [process.execPath, ['--no-liftoff', WASM_RUNNER, spec.slice(5), json]];
  } else {
    fs.writeFileSync(file, 'var CASES = ' + JSON.stringify(cases) + ';\n' + HARNESS);
    if (spec === 'node') argv = [process.execPath, [file]];
    else if (spec.startsWith('jit:')) { argv = [spec.slice(4), ['js', file]]; env = { ...process.env, ZIPP_RX_JIT_THRESHOLD: '1' }; }
    else argv = [spec, ['js', file]];
  }
  const p = spawnSync(argv[0], argv[1], { encoding: 'utf8', maxBuffer: 1 << 30, timeout: 300000, env });
  const out = new Array(cases.length);
  for (const line of (p.stdout || '').split(/\r?\n/)) {
    const k = line.indexOf(':');
    if (k > 0 && /^\d+$/.test(line.slice(0, k))) out[+line.slice(0, k)] = line.slice(k + 1);
  }
  return out;
}

function candidates(c) {
  const [p, f, s, li] = c;
  const out = [];
  if (li !== 0) out.push([p, f, s, 0]);
  for (let size = Math.ceil(s.length / 2); size >= 1; size = Math.floor(size / 2)) {
    for (let at = 0; at < s.length && out.length < 40; at += size) out.push([p, f, s.slice(0, at) + s.slice(at + size), Math.min(li, s.length - size)]);
    if (size === 1) break;
  }
  return out;
}

const findings = fs.readFileSync(inFile, 'utf8').split('\n').filter(Boolean).map(l => JSON.parse(l));
let current = findings.map(r => r.case);
for (let round = 0; round < 12; round++) {
  const batch = [], owner = [];
  current.forEach((c, i) => { for (const x of candidates(c)) { batch.push(x); owner.push(i); } });
  if (!batch.length) break;
  const [a, b] = engines.map(e => run(e.spec, batch));
  let improved = 0;
  const best = current.slice();
  batch.forEach((x, k) => {
    const i = owner[k];
    if (a[k] === undefined || b[k] === undefined || strip(a[k]) === strip(b[k])) return;
    const size = x[2].length + (x[3] ? 1 : 0), now = best[i][2].length + (best[i][3] ? 1 : 0);
    if (size < now) best[i] = x;
  });
  best.forEach((x, i) => { if (x !== current[i]) improved++; });
  current = best;
  process.stderr.write(`round ${round + 1}: ${batch.length} candidates, ${improved} findings shrank\n`);
  if (!improved) break;
}
const [a, b] = engines.map(e => run(e.spec, current));
const lines = current.map((c, i) => JSON.stringify({ case: c, [engines[0].label]: a[i], [engines[1].label]: b[i] }));
fs.writeFileSync(outFile, lines.join('\n') + '\n');
fs.rmSync(dir, { recursive: true, force: true });
console.log(`${current.length} findings minimized into ${outFile}`);
