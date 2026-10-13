// node --no-liftoff run_js.cjs PKG_DIR FILE.js : run a script in a ZIPP
// WebAssembly module (a Node package: zipp_wasm.js + zipp_wasm_bg.wasm) and
// print its console output. The instruction budget is raised so a benchmark
// is not cut off by the module's default meter.
const path = require('node:path'), fs = require('node:fs');
const m = require(path.resolve(process.argv[2], 'zipp_wasm.js'));
const e = new m.Engine();
e.setInstructionBudget(1e12);
let failure = null;
try {
  const r = e.initScript(fs.readFileSync(process.argv[3], 'utf8'));
  if (r && r.error) failure = 'ERROR ' + r.error;
} catch (x) { failure = 'THROWN ' + String(x); }
try {
  for (const line of e.takeOutput()) console.log(line);
} catch (x) { console.log('OUTPUT ' + String(x)); }
if (failure) console.log(failure);
