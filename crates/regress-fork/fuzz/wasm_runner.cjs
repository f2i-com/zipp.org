// node --no-liftoff wasm_runner.cjs PKG_DIR CASES.json : the fuzzer's runner
// for a ZIPP WebAssembly module (a Node package: zipp_wasm.js +
// zipp_wasm_bg.wasm). It loads harness.js once and calls `runOne(i)` per
// case, so a case that ends the engine (the sandbox's host-terminal regex
// limit, a trap) is recorded as that case's ABORT and a fresh engine takes
// the next case. Prints `i:RESULT` lines and `DONE n`, like the harness.
'use strict';
const path = require('node:path');
const fs = require('node:fs');
const m = require(path.resolve(process.argv[2], 'zipp_wasm.js'));
const cases = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
const harness = fs.readFileSync(path.join(__dirname, 'harness.js'), 'utf8');
const source = 'var NO_LOOP = true;\nvar CASES = ' + JSON.stringify(cases) + ';\n' + harness;

function fresh() {
  const e = new m.Engine();
  e.setInstructionBudget(2e9);
  const r = e.initScript(source);
  if (r && r.error) throw new Error('harness failed to load: ' + r.error);
  e.takeOutput();
  return e;
}

let e = fresh();
const out = [];
for (let i = 0; i < cases.length; i++) {
  try {
    e.renewInstructionBudget();
    const line = e.callFunction('runOne', [i]);
    out.push(i + ':' + line);
  } catch (x) {
    out.push(i + ': ABORT ' + String(x && x.message || x).replace(/\s+/g, ' ').slice(0, 300));
    try { e.dispose(); e.free(); } catch (_) { /* already gone */ }
    e = fresh();
  }
}
out.push('DONE ' + cases.length);
process.stdout.write(out.join('\n') + '\n');
