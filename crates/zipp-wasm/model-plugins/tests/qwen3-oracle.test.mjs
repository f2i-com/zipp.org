// ZIPP's logits against an implementation that shares nothing with it.
//
// Every other quantized test here compares ZIPP with ZIPP. The packed matmul is
// held to the decoded matmul, the decoded matmul to a JavaScript reference, the
// architecture to transformers on *staged float32 weights* -- each of which is
// worth having, and none of which would notice a mistake made consistently on
// both sides of the comparison. The decoders are now checked against gguf-py in
// the gguf-wasm repository, which closes that for one link. This closes it for
// the whole chain.
//
// The reference path and this one start from the same file and meet nowhere
// else:
//
//   qwen3-0.6b-q4_k_m.gguf
//     |
//     +-- transformers reads the GGUF, gguf-py decodes the blocks,
//     |   PyTorch runs transformers' own Qwen3        -> reference logits
//     |
//     +-- this package reads the GGUF, the blocks stay packed, the plugin's
//         Graph v2 runs on ZIPP's kernels            -> logits under test
//
// Different container reader, different block decoder, different model code,
// different framework, different arithmetic order. What they share is the
// checkpoint, and `tests/fixtures/qwen3-oracle/qwen3_oracle.json` records its
// sha256 so a fixture cannot drift onto a different file.
//
// Complete vectors, all 151,936 logits per prompt, not a top-k: a wrong
// rotation or a transposed key head moves the tail long before it moves the
// argmax, and the argmax is the part a demo would have shown you anyway.
//
//   ZIPP_QWEN3_MODEL=.../qwen3-0.6b-q4_k_m.gguf node --test tests/qwen3-oracle.test.mjs
//
// tools/make_qwen3_oracle.py regenerates the fixtures; it needs transformers,
// torch and gguf, none of which this package otherwise depends on.
import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFile, open, stat, access} from 'node:fs/promises';

import {PluginRegistry, openGGUF, WeightStore, bindGraph, resolveLimits} from '../src/index.mjs';
import {sourceDirectory} from './helpers.mjs';

const modelPath = process.env.ZIPP_QWEN3_MODEL;
const runtimeURL = new URL('../../gpu-lab/src/runtime.mjs', import.meta.url);
const engineURL = new URL('../../dist/all/zipp_wasm.js', import.meta.url);
const wasmURL = new URL('../../dist/all/zipp_wasm_bg.wasm', import.meta.url);
const kernelsURL = new URL('../../gpu-lab/wasm/kernels.wasm', import.meta.url);
const fixtures = new URL('fixtures/qwen3-oracle/', import.meta.url);

const exists = async url => { try { await access(url); return true; } catch { return false; } };
const ready = Boolean(modelPath) && await exists(modelPath) && await exists(runtimeURL) &&
  await exists(engineURL) && await exists(wasmURL) && await exists(new URL('qwen3_oracle.json', fixtures));
const reason = 'Set ZIPP_QWEN3_MODEL to the Qwen3 GGUF the fixtures were made from (see the file header)';

const GB = 1024 * 1024 * 1024;
const hostLimits = resolveLimits({
  maxModelFileBytes: 8 * GB, maxModelBytes: 8 * GB,
  maxDecodedBytes: 4 * GB, maxBoundInputBytes: 4 * GB,
  maxTensorElements: 2 ** 31, maxDimension: 1 << 21, maxTensors: 4096, maxNodes: 8192,
  maxContext: 4096,
});
const computeLimits = {
  maxNodes: 8192, maxElements: 4194304, maxInputElements: 200000000,
  maxOutputElements: 200000000, maxLogicalBytes: 3 * GB,
  maxWork: 200000000000, maxDimension: 1 << 21, maxSessions: 4, maxStepsPerRun: 64,
};

// ZIPP accumulates float32 in its own order over 28 layers; PyTorch accumulates
// float32 in another. Neither is more correct and the difference compounds with
// depth, so this is a tolerance rather than equality -- unlike the block
// decoders, where there is nothing to round and the test demands equality.
//
// The number is the measured agreement with headroom, not a bound chosen to
// make the test pass: it is reported on every run so a regression shows as a
// number getting worse before it shows as a failure. Measured 7.4e-5 on the
// WASM backend over three prompts, so this is about seven times what it needs
// -- enough that a different machine's float32 does not fail it, tight enough
// that a real divergence cannot hide under it.
const TOLERANCE = 5e-4;

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

test('ZIPP agrees with transformers on the same GGUF file, logit for logit',
  {skip: !ready && reason}, async t => {
  const oracle = JSON.parse(await readFile(new URL('qwen3_oracle.json', fixtures), 'utf8'));

  await t.test('the fixtures describe the checkpoint being tested', async () => {
    // Reference logits are only evidence about the file they came from. This is
    // the same guard as the GGUF module's lock file, for the same reason.
    const digest = createHash('sha256').update(await readFile(modelPath)).digest('hex');
    assert.equal(digest, oracle.model_sha256,
      `ZIPP_QWEN3_MODEL is not the checkpoint these reference logits were made from ` +
      `(expected ${oracle.model}). Point it at that file, or regenerate with ` +
      `tools/make_qwen3_oracle.py.`);
  });

  const zipp = await import(engineURL);
  await zipp.default({module_or_path: await readFile(wasmURL)});
  const {createRuntime} = await import(runtimeURL);

  const plugin = await new PluginRegistry().install(
    await sourceDirectory('../plugins/qwen3/'), {approve: () => true});
  const source = await fileSource(modelPath);
  const index = await openGGUF(source, 'model.gguf', null, hostLimits);
  const store = new WeightStore([index], hostLimits);
  const engine = new zipp.Engine();
  let runtime;
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
    assert.equal(config.vocab_size, oracle.vocab_size, 'vocabulary size');

    const tokenizer = index.tokenizer();
    runtime = await createRuntime(
      {backend: 'wasm', wasmBytes: await readFile(kernelsURL), limits: computeLimits});

    let overall = 0;
    for (const [n, entry] of oracle.prompts.entries()) {
      await t.test(`${JSON.stringify(entry.prompt)}`, async () => {
        // The tokenizer is checked first and separately. Comparing logits for
        // a differently tokenized prompt would be comparing two answers to two
        // questions, and would fail in a way that pointed at the model.
        const ids = [...tokenizer.encode(entry.prompt)];
        assert.deepEqual(ids, entry.ids, 'tokenized differently from the reference');

        const raw = await readFile(new URL(`qwen3_logits_${n + 1}.f32`, fixtures));
        const want = new Float32Array(raw.buffer, raw.byteOffset, raw.byteLength / 4);
        assert.equal(want.length, oracle.vocab_size, 'reference vector length');
        assert.equal(createHash('sha256').update(raw).digest('hex'), entry.logits_sha256,
          'the reference vector does not match the digest recorded for it');

        const template = call('zipp_model_graph', [JSON.stringify(config), JSON.stringify(ids)]);
        const graph = await bindGraph(template, store, hostLimits);
        const result = await runtime.execute(graph, {typedOutputs: true});
        const got = result.outputs.logits.data;

        assert.deepEqual(result.outputs.logits.shape, [1, oracle.vocab_size]);

        let worst = 0, worstAt = -1;
        for (let i = 0; i < want.length; i++) {
          const delta = Math.abs(got[i] - want[i]);
          if (delta > worst) { worst = delta; worstAt = i; }
        }
        overall = Math.max(overall, worst);

        // The whole distribution, and then the thing a person would notice.
        assert.ok(worst < TOLERANCE,
          `logit ${worstAt} differs by ${worst} (${got[worstAt]} vs ${want[worstAt]}), ` +
          `past the ${TOLERANCE} these are held to`);

        let best = 0;
        for (let i = 1; i < got.length; i++) if (got[i] > got[best]) best = i;
        assert.equal(best, entry.argmax,
          `predicted ${JSON.stringify(vocab[best])}, reference says ` +
          `${JSON.stringify(vocab[entry.argmax])}`);

        console.log(`      ${JSON.stringify(entry.prompt)} -> ` +
          `${JSON.stringify(vocab[best])}, worst of ${want.length} logits: ${worst.toExponential(3)}`);
      });
    }
    console.log(`      worst across every prompt: ${overall.toExponential(3)} ` +
      `(held to ${TOLERANCE})`);
  } finally {
    await runtime?.dispose?.();
    engine.free?.();
    await source.close();
  }
});
