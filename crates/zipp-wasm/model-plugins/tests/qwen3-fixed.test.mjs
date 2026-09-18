// A whole Qwen3 stage whose projections are multiplied as integers.
//
// `matmul_fixed` reaches the same answer on every backend by construction --
// integer addition is associative -- rather than by four implementations
// agreeing about float32 rounding order. That is what lets a second peer check
// a stage's compute by equality, and it is the arithmetic a proof over a prime
// field could be written about. This is the plugin end of it: the graph the
// Qwen3 architecture builds when it is asked for that form, run against the
// same checkpoint as the float32 one.
//
// Two things are being checked and they are different claims.
//
//   * That it is *the same model*. Fixed-point is an approximation, so this is
//     a measured distance rather than an equality, and the measurement is here
//     rather than asserted from a projection's error: twenty-eight layers
//     compound, and a per-matmul figure says nothing about that on its own.
//   * That it is *reproducible*. Two runtimes running the fixed-point stage
//     must agree bit for bit, which is the property the float32 path only has
//     because its kernels were made to agree.
//
//   ZIPP_QWEN3_MODEL=.../qwen3-0.6b-q4_k_m.gguf node --test tests/qwen3-fixed.test.mjs
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile, open, stat, access} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';

import {PluginRegistry, openGGUF, WeightStore, prepareDecode, stepInputs,
        resolveLimits, validateStageManifest} from '../src/index.mjs';
import {sourceDirectory} from './helpers.mjs';

const modelPath = process.env.ZIPP_QWEN3_MODEL
  ?? fileURLToPath(new URL('fixtures/tiny-qwen3.gguf', import.meta.url));
const runtimeURL = new URL('../../gpu-lab/src/runtime.mjs', import.meta.url);
const quantURL = new URL('../../gpu-lab/src/quant.mjs', import.meta.url);
const engineURL = new URL('../../dist/all/zipp_wasm.js', import.meta.url);
const wasmURL = new URL('../../dist/all/zipp_wasm_bg.wasm', import.meta.url);
const kernelsURL = new URL('../../gpu-lab/wasm/kernels.wasm', import.meta.url);

const exists = async url => { try { await access(url); return true; } catch { return false; } };
const ready = await exists(modelPath) && await exists(runtimeURL) &&
  await exists(engineURL) && await exists(wasmURL) && await exists(kernelsURL);
const reason = 'Needs dist/all built, the gpu-lab sibling, and tests/fixtures/tiny-qwen3.gguf';

const GB = 1024 * 1024 * 1024;
const CONTEXT = 32;
const hostLimits = resolveLimits({
  maxModelFileBytes: 8 * GB, maxModelBytes: 8 * GB, maxDecodedBytes: 4 * GB,
  maxBoundInputBytes: 4 * GB, maxTensorElements: 2 ** 31, maxDimension: 1 << 21,
  maxTensors: 4096, maxNodes: 8192, maxContext: 4096,
});
const computeLimits = {
  maxNodes: 8192, maxElements: 4194304, maxOutputElements: 200000000,
  maxLogicalBytes: 4 * GB, maxWork: 2 ** 46,
  // A real Qwen3 vocabulary is 151,936 wide, past the 65,536 default; and the
  // fixed-point form uploads int16 quants, which are counted in the float32
  // units this budget is expressed in and are 3.56 times what blocks cost.
  maxDimension: 1 << 21, maxInputElements: 2 ** 31 - 1,
};

async function fileSource(path) {
  const size = (await stat(path)).size;
  const handle = await open(path);
  return {
    size: () => size,
    async read(_name, offset, length) {
      const out = new Uint8Array(length);
      const {bytesRead} = await handle.read(out, 0, length, offset);
      assert.equal(bytesRead, length, 'short read');
      return out;
    },
    close: () => handle.close(),
  };
}

test('a Qwen3 stage on the fixed-point path', {skip: !ready && reason}, async t => {
  const zipp = await import(engineURL);
  await zipp.default({module_or_path: await readFile(wasmURL)});
  const {createRuntime} = await import(runtimeURL);
  const {quantizeWeight} = await import(quantURL);
  const kernels = await readFile(kernelsURL);

  const plugin = await new PluginRegistry().install(
    await sourceDirectory('../plugins/qwen3/'), {approve: () => true});
  const source = await fileSource(modelPath);
  const index = await openGGUF(source, 'model.gguf', null, hostLimits);
  const store = new WeightStore([index], hostLimits);
  const engine = new zipp.Engine();
  const runtimes = [];
  try {
    engine.setSyncHostCapabilities([]);
    engine.setInstructionBudget(hostLimits.instructionBudget);
    engine.initPythonProject({...plugin.files}, plugin.entry, []);
    const call = (name, args) => {
      engine.renewInstructionBudget();
      return JSON.parse(engine.pythonCall(name, args));
    };
    const vocab = index.strings('tokenizer.ggml.tokens');
    const config = call('zipp_model_config',
      [JSON.stringify(index.metadata()), JSON.stringify(vocab.length)]);
    const layers = config.num_layers;
    const tokens = [...index.tokenizer().encode('The capital of France is')];

    const stage = (policy) => call('zipp_model_decode_stage',
      [JSON.stringify(config), JSON.stringify(CONTEXT), JSON.stringify(0),
       JSON.stringify(layers - 1), ...(policy === undefined ? [] : [JSON.stringify(policy)])]);

    await t.test('the default is unchanged, and says so', () => {
      const template = stage();
      assert.equal(template.manifest.fixed, 'none');
      assert.ok(!template.bindings.some(b => b.kind === 'fixed' || b.kind === 'fixed_scales'),
        'a default stage bound a weight for matmul_fixed');
      assert.ok(!template.graph.nodes.some(n => n.op === 'matmul_fixed'),
        'a default stage emitted matmul_fixed');
    });

    await t.test('asking for it changes the projections and nothing else', () => {
      const template = stage('layers');
      assert.equal(template.manifest.fixed, 'layers');
      const fixed = template.bindings.filter(b => b.kind === 'fixed');
      const scales = template.bindings.filter(b => b.kind === 'fixed_scales');
      // Seven projections a layer: q, k, v, attn_output, gate, up, down.
      assert.equal(fixed.length, 7 * layers, 'not every projection was bound fixed');
      assert.equal(scales.length, fixed.length, 'every fixed weight has its scales');
      assert.deepEqual(fixed.map(b => b.tensor).sort(), scales.map(b => b.tensor).sort());
      // The norms are vectors and the embedding is read a row at a time; both
      // stay exactly as they were.
      assert.ok(!fixed.some(b => /norm|token_embd/.test(b.tensor)),
        'a norm or the embedding was bound for a matmul it is not part of');
      assert.equal(template.graph.nodes.filter(n => n.op === 'matmul_fixed').length, 7 * layers);
      // `all` reaches the tied head as well, which is the largest tensor here.
      const everything = stage('all');
      assert.equal(everything.manifest.fixed, 'all');
      assert.ok(everything.bindings.some(b => b.kind === 'fixed' && b.tensor === 'token_embd.weight'),
        'the tied projection was left on the float path');
    });

    await t.test('an unknown policy is refused by name', () => {
      assert.throws(() => stage('most'), /fixed-weight policy/);
    });

    // Both stages, prepared and run over the same prompt.
    const floatPlan = await prepareDecode(stage(), store, hostLimits);
    const fixedPlan = await prepareDecode(stage('layers'), store, hostLimits,
      {quantize: quantizeWeight});
    for (let i = 0; i < 3; i++) {
      runtimes.push(await createRuntime({backend: 'wasm', wasmBytes: kernels, limits: computeLimits}));
    }
    const [floatRt, fixedRt, secondRt] = runtimes;
    const asFloat = await floatRt.prepare(floatPlan.program);
    const asFixed = await fixedRt.prepare(fixedPlan.program);
    // A second runtime holding the same fixed stage, which is what a checker is.
    const checker = await secondRt.prepare(fixedPlan.program);

    let worstRelative = 0, argmaxMoved = 0;
    for (const [position, token] of tokens.entries()) {
      await t.test(`position ${position}: ${JSON.stringify(vocab[token])}`, async () => {
        const want = (await asFloat.run([await stepInputs(floatPlan, store, {token, position})],
          {readback: ['logits']})).outputs.logits.data;
        const got = (await asFixed.run([await stepInputs(fixedPlan, store, {token, position})],
          {readback: ['logits']})).outputs.logits.data;
        const also = (await checker.run([await stepInputs(fixedPlan, store, {token, position})],
          {readback: ['logits']})).outputs.logits.data;

        // Reproducible: two runtimes running the integer path agree exactly.
        // This is the claim the operation exists for, and it is an equality.
        assert.equal(got.length, also.length);
        for (let i = 0; i < got.length; i++) {
          if (!Object.is(got[i], also[i])) {
            assert.fail(`logit ${i} at position ${position}: ${got[i]} and ${also[i]} on two runtimes`);
          }
        }

        // The same model: measured, not asserted from a per-matmul figure.
        let scale = 0;
        for (let i = 0; i < want.length; i++) scale = Math.max(scale, Math.abs(want[i]));
        let best = 0, bestFixed = 0;
        for (let i = 0; i < want.length; i++) {
          worstRelative = Math.max(worstRelative, Math.abs(got[i] - want[i]) / scale);
          if (want[i] > want[best]) best = i;
          if (got[i] > got[bestFixed]) bestFixed = i;
        }
        if (best !== bestFixed) argmaxMoved++;
        assert.ok(got.every(Number.isFinite), 'the fixed-point stage produced a non-finite logit');
      });
    }

    await t.test('what twenty-eight fixed-point layers cost', () => {
      // Printed as well as asserted: the number is the point of the test, and a
      // bound with no measurement behind it drifts into being wrong quietly.
      console.log(`      worst logit difference ${worstRelative.toExponential(2)} of the largest logit, ` +
        `argmax moved at ${argmaxMoved} of ${tokens.length} positions`);
      assert.ok(worstRelative < 5e-2,
        `the fixed-point stage is ${worstRelative.toExponential(2)} away from the float32 one`);
    });

    await t.test('the manifest distinguishes the two, so a checker can', async () => {
      // Two peers running the same layers of the same checkpoint compute
      // different numbers if one is on this path. The manifest is what a
      // checker compares before it decides two answers should match.
      const a = call('zipp_model_stage_manifest',
        [JSON.stringify(config), JSON.stringify(0), JSON.stringify(layers - 1), JSON.stringify(CONTEXT)]);
      const b = call('zipp_model_stage_manifest',
        [JSON.stringify(config), JSON.stringify(0), JSON.stringify(layers - 1),
         JSON.stringify(CONTEXT), JSON.stringify('layers')]);
      assert.equal(a.config_digest, b.config_digest, 'the checkpoint is the same checkpoint');
      assert.notEqual(a.fixed, b.fixed, 'nothing in the manifest tells the two apart');
      validateStageManifest(a); validateStageManifest(b);
    });
  } finally {
    for (const runtime of runtimes) { try { runtime.dispose(); } catch {} }
    try { engine.dispose(); } catch {}
    store.dispose(); await source.close();
  }
});
