// Transformers-style modelling code, running on ZIPP's Python.
//
// Everything else in this package expresses a model as Graph v2 nodes that the
// host binds and submits. This checks the other way of writing one: ordinary
// torch idiom -- matmul, softmax, silu, rsqrt, cos/sin, repeat_interleave --
// executed by the engine's own `torch` subset, with no graph authored by hand.
//
// What it does NOT show is that the `transformers` package runs here. That
// package needs numpy, PyTorch and Rust extensions for tokenizers and
// safetensors, and an installer to fetch them; this VM has no native extension
// loading and no pip. What runs is the modelling code such a package contains,
// which is the part that describes the model.
//
// The reference is a real `Qwen3DecoderLayer`. Generate it with
// `python tools/make_torch_block_case.py`; the test skips without it, because
// producing it needs transformers.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile, access} from 'node:fs/promises';

const engineURL = new URL('../../dist/all/zipp_wasm.js', import.meta.url);
const wasmURL = new URL('../../dist/all/zipp_wasm_bg.wasm', import.meta.url);
const caseURL = new URL('../models/qwen3-block-case.json', import.meta.url);
const fixtureURL = new URL('./fixtures/qwen3_block.py', import.meta.url);
const exists = async url => { try { await access(url); return true; } catch { return false; } };
const ready = await exists(engineURL) && await exists(wasmURL) && await exists(caseURL);
const reason = 'Needs dist/all and models/qwen3-block-case.json (see tools/make_torch_block_case.py)';

test('a Qwen3 decoder layer in torch idiom matches PyTorch inside the engine',
  {skip: !ready && reason}, async () => {
  const zipp = await import(engineURL);
  await zipp.default({module_or_path: await readFile(wasmURL)});
  const expected = JSON.parse(await readFile(caseURL, 'utf8')).expected;
  const engine = new zipp.Engine();
  try {
    engine.setSyncHostCapabilities([]);
    // A layer of tensor arithmetic in interpreted Python costs far more than
    // the engine's default allowance; the fuse still exists, higher up.
    engine.setInstructionBudget(2_000_000_000);
    engine.initPythonProject({
      'main.py': await readFile(fixtureURL, 'utf8'),
      'assets/case.json': await readFile(caseURL, 'utf8'),
    }, 'main.py', []);
    engine.renewInstructionBudget();
    const got = JSON.parse(engine.pythonCall('probe', []));
    assert.equal(got.length, expected.length);
    let worst = 0;
    for (let i = 0; i < expected.length; i++) worst = Math.max(worst, Math.abs(got[i] - expected[i]));
    // RMSNorm, rotary embeddings, grouped-query attention, SwiGLU: every piece
    // of a current open-weight decoder, agreeing with the reference to float32.
    assert.ok(worst < 1e-5, `max difference ${worst} from PyTorch's Qwen3DecoderLayer`);
    console.log('Qwen3 layer in ZIPP torch vs PyTorch: max_abs_error =', worst);
  } finally { try { engine.dispose(); } catch {} }
});
