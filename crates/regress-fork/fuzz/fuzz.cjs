// Differential fuzzer for the regex engine: random ECMAScript patterns and
// subjects (gen.cjs) run through harness.js in Node and in ZIPP builds.
//
//   node crates/regress-fork/fuzz/fuzz.cjs [--seed S] [--batches B] [--size N]
//        [--out FINDINGS.jsonl] [--keep DIR] ENGINE...
//
// ENGINE is LABEL=PATH_TO_ZIPP_EXE, LABEL=jit:PATH_TO_ZIPP_EXE (the regex JIT
// compiling on the first attempt) or LABEL=wasm:PKG_DIR (a Node package of a
// WebAssembly module). The first ZIPP engine is the reference: every other
// one must print exactly what it prints, error messages included ("CHANGE").
// Each ZIPP engine is also compared with Node by everything but the error
// messages ("NODE"): a difference there that the reference shares is an
// existing disagreement, not a change. A case that ends an engine's run (a
// panic, or the sandbox's host-terminal regex limit) is recorded as ABORT and
// the batch resumes after it.
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { spawnSync } = require('node:child_process');
const { generate } = require('./gen.cjs');

const HARNESS = fs.readFileSync(path.join(__dirname, 'harness.js'), 'utf8');
const WASM_RUNNER = path.join(__dirname, 'wasm_runner.cjs');

function parseArgs(argv) {
  const a = { seed: 1, batches: 10, size: 2000, out: null, keep: null, engines: [] };
  for (let i = 0; i < argv.length; i++) {
    const x = argv[i];
    if (x === '--seed') a.seed = +argv[++i];
    else if (x === '--batches') a.batches = +argv[++i];
    else if (x === '--size') a.size = +argv[++i];
    else if (x === '--out') a.out = argv[++i];
    else if (x === '--keep') a.keep = argv[++i];
    else {
      const k = x.indexOf('=');
      if (k < 0) throw new Error('engine must be LABEL=SPEC: ' + x);
      a.engines.push({ label: x.slice(0, k), spec: x.slice(k + 1) });
    }
  }
  return a;
}

// `jit:EXE` is the CLI with its regex JIT compiling every regex on its first
// attempt (ZIPP_RX_JIT_THRESHOLD=1), so a few execs per case reach it.
function runEngine(spec, file, casesFile) {
  let argv, env = process.env;
  if (spec === 'node') argv = [process.execPath, [file]];
  else if (spec.startsWith('wasm:')) argv = [process.execPath, ['--no-liftoff', WASM_RUNNER, spec.slice(5), casesFile]];
  else if (spec.startsWith('jit:')) {
    argv = [spec.slice(4), ['js', file]];
    env = { ...process.env, ZIPP_RX_JIT_THRESHOLD: '1' };
  } else argv = [spec, ['js', file]];
  // Node gets 20 s and a ZIPP build 30 s for the rest of a batch: a case
  // that runs longer (exponential backtracking) is recorded as TIMEOUT.
  const timeout = spec === 'node' ? 20000 : 30000;
  const p = spawnSync(argv[0], argv[1], { encoding: 'utf8', maxBuffer: 1 << 30, timeout, env });
  const timedOut = !!(p.error && p.error.code === 'ETIMEDOUT');
  return { out: p.stdout || '', err: (p.stderr || '') + (p.error ? String(p.error) : ''), status: p.status, timedOut };
}

// Run `cases` in an engine; returns one output line per case. A run that ends
// early marks the next case ABORT and continues after it (a WebAssembly
// module's runner does that itself, case by case).
function runAll(spec, cases, dir, tag) {
  const results = new Array(cases.length);
  let start = 0;
  while (start < cases.length) {
    const file = path.join(dir, `${tag}-${start}.js`);
    const casesFile = path.join(dir, `${tag}-${start}.json`);
    const rest = JSON.stringify(cases.slice(start));
    if (spec.startsWith('wasm:')) fs.writeFileSync(casesFile, rest);
    else fs.writeFileSync(file, 'var CASES = ' + rest + ';\n' + HARNESS);
    const r = runEngine(spec, file, casesFile);
    let done = false, last = -1;
    for (const line of r.out.split(/\r?\n/)) {
      if (line.startsWith('DONE ')) { done = true; continue; }
      const k = line.indexOf(':');
      if (k <= 0) continue;
      const idx = +line.slice(0, k);
      if (!Number.isInteger(idx) || idx < 0 || idx >= cases.length - start) continue;
      results[start + idx] = line.slice(k + 1);
      last = Math.max(last, idx);
    }
    if (done) break;
    const abortAt = start + last + 1;
    if (abortAt < cases.length) {
      // The batch file's name is the engine's own: leave it out, so two
      // engines that abort the same way print the same.
      const why = (r.out.split(/\r?\n/).filter(l => l && !/^\d+:/.test(l)).join(' ') + ' ' + r.err)
        .split(file).join('<batch>').trim().replace(/\s+/g, ' ').slice(0, 300);
      results[abortAt] = r.timedOut ? ' TIMEOUT' : ' ABORT ' + why;
    }
    start = abortAt + 1;
  }
  return results;
}

function stripMessages(line) {
  if (line === undefined) return line;
  const k = line.indexOf(' || ');
  return k < 0 ? line : line.slice(0, k);
}

function main() {
  const a = parseArgs(process.argv.slice(2));
  const zipps = a.engines;
  if (!zipps.length) throw new Error('give at least one ZIPP engine');
  const dir = a.keep || fs.mkdtempSync(path.join(os.tmpdir(), 'rxfuzz-'));
  fs.mkdirSync(dir, { recursive: true });
  const findings = [];
  const counts = { cases: 0, change: 0, timeout: 0, node: {}, abort: {} };
  for (const e of zipps) { counts.node[e.label] = 0; counts.abort[e.label] = 0; }
  for (let b = 0; b < a.batches; b++) {
    const seed = a.seed * 100003 + b;
    const cases = generate(seed, a.size);
    const nodeOut = runAll('node', cases, dir, `node-${seed}`);
    // A case Node cannot finish is not run anywhere else: an empty pattern
    // stands in for it so the batch keeps its numbering.
    const runnable = cases.map((c, i) => nodeOut[i] === ' TIMEOUT' ? ['', '', '', 0] : c);
    counts.timeout += nodeOut.filter(o => o === ' TIMEOUT').length;
    const outs = zipps.map(e => runAll(e.spec, runnable, dir, `${e.label}-${seed}`));
    for (let i = 0; i < cases.length; i++) {
      if (nodeOut[i] === ' TIMEOUT') continue;
      const ref = outs[0][i];
      const rec = { seed, i, case: cases[i], node: nodeOut[i] };
      let interesting = false;
      zipps.forEach((e, j) => {
        rec[e.label] = outs[j][i];
        if (outs[j][i] && outs[j][i].startsWith(' ABORT')) counts.abort[e.label]++;
        if (stripMessages(outs[j][i]) !== stripMessages(nodeOut[i])) { counts.node[e.label]++; interesting = true; rec.nodeDiff = (rec.nodeDiff || []).concat(e.label); }
        if (j > 0 && outs[j][i] !== ref) { counts.change++; interesting = true; rec.change = (rec.change || []).concat(e.label); }
      });
      if (interesting) findings.push(rec);
    }
    counts.cases += cases.length;
    process.stderr.write(`batch ${b + 1}/${a.batches} seed ${seed}: cases ${counts.cases} changes ${counts.change} node-diffs ${JSON.stringify(counts.node)} aborts ${JSON.stringify(counts.abort)} timeouts ${counts.timeout}\n`);
  }
  if (a.out) fs.writeFileSync(a.out, findings.map(f => JSON.stringify(f)).join('\n') + (findings.length ? '\n' : ''));
  if (!a.keep) fs.rmSync(dir, { recursive: true, force: true });
  console.log(JSON.stringify(counts));
  process.exitCode = counts.change ? 1 : 0;
}

main();
