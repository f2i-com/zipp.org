// A published transformers modelling file, unmodified, inside the engine.
//
// `interop/transformers/` is not the Hugging Face package -- see its README --
// but the names a modelling file imports from it. This test assembles that shim
// plus `modeling_<model>.py` and `configuration_<model>.py` exactly as
// published, builds the model inside ZIPP and compares its logits with what
// transformers produced for the same weights.
//
// It skips unless the files have been staged, because they belong to Hugging
// Face and are not checked in:
//
//     python tools/stage_transformers_model.py --model qwen3
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile, readdir, access} from 'node:fs/promises';
import {createHash} from 'node:crypto';

const engineURL = new URL('../../dist/all/zipp_wasm.js', import.meta.url);
const wasmURL = new URL('../../dist/all/zipp_wasm_bg.wasm', import.meta.url);
const shimURL = new URL('../interop/', import.meta.url);
const modelsURL = new URL('../models/', import.meta.url);
const exists = async url => { try { await access(url); return true; } catch { return false; } };
// Every model staged, not one: the claim is that the shim reads published files
// generally, so whatever has been staged is what gets checked.
const only = process.env.ZIPP_TRANSFORMERS_MODEL;
const staged = await exists(modelsURL)
  ? (await readdir(modelsURL, {withFileTypes: true}))
      .filter(entry => entry.isDirectory() && entry.name.startsWith('transformers-'))
      .map(entry => entry.name.slice('transformers-'.length))
      .filter(name => !only || name === only)
  : [];
// Files that are known not to load, and exactly why. Asserted rather than
// skipped: if one starts working, this list is what should change.
// (GPT-Neo was here, for flex_attention: the frontend then resolved every
// import while compiling, so a guarded optional backend was still looked up.)
const KNOWN_UNSUPPORTED = {};
const haveEngine = await exists(engineURL);
const reason = 'Needs dist/all and a staged model (tools/stage_transformers_model.py)';

async function collect(dir, prefix = '') {
  const files = {};
  for (const entry of await readdir(dir, {withFileTypes: true})) {
    if (entry.name === '__pycache__') continue;
    const child = new URL(`${entry.name}${entry.isDirectory() ? '/' : ''}`, dir);
    if (entry.isDirectory()) Object.assign(files, await collect(child, `${prefix}${entry.name}/`));
    else if (entry.name.endsWith('.py')) files[`${prefix}${entry.name}`] = await readFile(child, 'utf8');
  }
  return files;
}

const driverFor = recorded => `import json

import torch
from transformers.models.${recorded.model}.configuration_${recorded.model} import ${recorded.config_class}
from transformers.models.${recorded.model}.modeling_${recorded.model} import ${recorded.model_class}

with open("assets/case.json") as handle:
    CASE = json.load(handle)


def probe():
    config = ${recorded.config_class}(**CASE["config"])
    model = ${recorded.model_class}(config)
    state = {name: torch.tensor(values).reshape(CASE["shapes"][name])
             for name, values in CASE["weights"].items()}
    model.load_state_dict(state, strict=False)
    model.eval()
    with torch.no_grad():
        out = model(input_ids=torch.tensor([CASE["input_ids"]]))
    return json.dumps(out.logits.reshape(-1).tolist())
`;

test('published transformers modelling files run unmodified on ZIPP',
  {skip: (!haveEngine || staged.length === 0) && reason}, async t => {
  const zipp = await import(engineURL);
  await zipp.default({module_or_path: await readFile(wasmURL)});
  for (const model of staged) {
    await t.test(model, () => runOne(zipp, model));
  }
});

async function runOne(zipp, model) {
  const stagedURL = new URL(`transformers-${model}/`, modelsURL);
  const caseText = await readFile(new URL('case.json', stagedURL), 'utf8');
  const recorded = JSON.parse(caseText);
  const files = await collect(shimURL);
  const shimModules = Object.keys(files).length;
  for (const [name, digest] of Object.entries(recorded.sources)) {
    const bytes = await readFile(new URL(name, stagedURL));
    // The claim is that the file is unmodified, so it is hashed, not trusted.
    assert.equal(createHash('sha256').update(bytes).digest('hex'), digest,
      `${name} is not the file transformers was recorded from`);
    files[`transformers/models/${recorded.model}/${name}`] = bytes.toString('utf8');
  }
  files['main.py'] = driverFor(recorded);
  files['assets/case.json'] = caseText;

  const engine = new zipp.Engine();
  try {
    engine.setSyncHostCapabilities([]);
    engine.setInstructionBudget(2_000_000_000);
    const compiled = Date.now();
    const expected = KNOWN_UNSUPPORTED[model];
    if (expected) {
      assert.throws(() => engine.initPythonProject(files, 'main.py', []), error =>
        expected.test(String(error?.message ?? error)),
        `${model} is listed as unsupported for a reason that no longer applies`);
      console.log(`${model}: still unsupported, as recorded (${expected})`);
      return;
    }
    engine.initPythonProject(files, 'main.py', []);
    engine.renewInstructionBudget();
    const got = JSON.parse(engine.pythonCall('probe', []));
    assert.equal(got.length, recorded.expected.length);
    let worst = 0;
    for (let i = 0; i < got.length; i++) worst = Math.max(worst, Math.abs(got[i] - recorded.expected[i]));
    assert.ok(worst < 1e-5, `max difference ${worst} from transformers`);
    console.log(`${model}: ${shimModules}-module shim + ${Object.keys(recorded.sources).length} ` +
      `unmodified files compiled in ${Date.now() - compiled} ms; max_abs_error = ${worst}`);
  } finally { try { engine.dispose(); } catch {} }
}
