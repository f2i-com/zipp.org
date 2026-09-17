// Qwen3 from a GGUF file, without expanding it.
//
// This is the whole path end to end: the real ZIPP engine runs the plugin's
// Python, which emits Graph v2 and a binding list; the host reads the
// checkpoint's own metadata for the configuration and its own tensors for the
// weights; the quantized ones stay in the file's block format and a backend
// decodes them inside the matmul. Nothing is converted and nothing is rewritten
// on disk.
//
// The assertion that matters is the last one. A model that loads, binds and
// produces finite logits can still be wrong in a way that only shows up as
// plausible nonsense -- a rotation applied to the wrong half of a head, a
// key head repeated in the wrong order, a norm over the wrong axis. So this
// asks it a question with one answer, and checks it gives that answer.
//
// The checkpoint is not in this repository; third-party weights are not
// redistributed with ZIPP. Point ZIPP_QWEN3_MODEL at a Qwen3 GGUF:
//
//   ZIPP_QWEN3_MODEL=.../qwen3-0.6b-q4_k_m.gguf node --test tests/qwen3.test.mjs
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile, open, stat, access} from 'node:fs/promises';

import {PluginRegistry, openGGUF, WeightStore, bindGraph, prepareDecode, stepInputs,
        resolveLimits} from '../src/index.mjs';
import {sourceDirectory} from './helpers.mjs';

const modelPath = process.env.ZIPP_QWEN3_MODEL;
const runtimeURL = new URL('../../gpu-lab/src/runtime.mjs', import.meta.url);
const engineURL = new URL('../../dist/all/zipp_wasm.js', import.meta.url);
const wasmURL = new URL('../../dist/all/zipp_wasm_bg.wasm', import.meta.url);
const kernelsURL = new URL('../../gpu-lab/wasm/kernels.wasm', import.meta.url);
const exists = async url => { try { await access(url); return true; } catch { return false; } };
const ready = Boolean(modelPath) && await exists(modelPath) &&
  await exists(runtimeURL) && await exists(engineURL) && await exists(wasmURL);
const reason = 'Set ZIPP_QWEN3_MODEL to a Qwen3 GGUF, with dist/all built (see the file header)';

const GB = 1024 * 1024 * 1024;
// A real checkpoint exceeds every default here, deliberately: the defaults are
// sized for a model a page can reasonably hold, and raising them is the host's
// decision to make explicitly.
const hostLimits = resolveLimits({
  maxModelFileBytes: 8 * GB, maxModelBytes: 8 * GB,
  maxDecodedBytes: 4 * GB, maxBoundInputBytes: 4 * GB,
  maxTensorElements: 2 ** 31, maxDimension: 1 << 21, maxTensors: 4096, maxNodes: 8192,
  maxContext: 4096,
});
const computeLimits = {
  maxNodes: 8192, maxElements: 4194304, maxInputElements: 200000000,
  // A carried cache is an output too, and there are two per layer.
  maxOutputElements: 200000000, maxLogicalBytes: 3 * GB,
  maxWork: 200000000000, maxDimension: 1 << 21,
  maxSessions: 4, maxStepsPerRun: 64,
};

/** Gigabytes are read by range, the way a browser reads a File. */
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

test('Qwen3 loads from GGUF, stays quantized, and answers a question it knows',
  {skip: !ready && reason}, async t => {
  const zipp = await import(engineURL);
  await zipp.default({module_or_path: await readFile(wasmURL)});
  const {createRuntime} = await import(runtimeURL);

  const plugin = await new PluginRegistry().install(await sourceDirectory('../plugins/qwen3/'), {approve: () => true});
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

    // The plugin reads the checkpoint's own metadata. There is no config.json
    // and nothing to convert: a GGUF file describes itself.
    const vocab = index.strings('tokenizer.ggml.tokens');
    const config = call('zipp_model_config', [JSON.stringify(index.metadata()), JSON.stringify(vocab.length)]);
    await t.test('the configuration comes from the file', () => {
      assert.equal(config.vocab_size, vocab.length);
      assert.equal(config.num_heads % config.num_kv_heads, 0, 'grouped-query heads must divide');
      assert.ok(config.rope_base >= 10000, 'a rotary base is present');
      const description = call('zipp_model_describe', [JSON.stringify(config)]);
      assert.equal(description.family, 'qwen3');
      assert.equal(description.tied_embeddings, true);
    });

    // Tokenized by the checkpoint's own vocabulary. The ids are checked
    // against the vocabulary directly as well, so a tokenizer regression
    // cannot quietly become a model regression.
    const tokenizer = index.tokenizer();
    const prompt = [...tokenizer.encode('The capital of France is')];
    await t.test('the prompt tokenizes to the ids the vocabulary defines', () => {
      assert.deepEqual(prompt, ['The', 'Ġcapital', 'Ġof', 'ĠFrance', 'Ġis'].map(piece => {
        const found = vocab.indexOf(piece);
        assert.notEqual(found, -1, `vocabulary has no ${piece}`);
        return found;
      }));
    });

    const template = call('zipp_model_graph', [JSON.stringify(config), JSON.stringify(prompt)]);
    const graph = await bindGraph(template, store, hostLimits);

    let blockBytes = 0, floatBytes = 0;
    for (const node of graph.nodes) {
      if (node.op !== 'input' || !node.data) continue;
      if (node.dtype) blockBytes += node.data.length; else floatBytes += node.data.byteLength ?? node.data.length * 4;
    }
    await t.test('the weights never leave their block format', () => {
      // Every matrix in this family is Q4_K or Q6_K, so nothing is decoded and
      // the only float32 is the norms. As float32 the same weights would be
      // about six times this, which is the difference between a model a browser
      // can hold and one it cannot.
      assert.ok(blockBytes > 0, 'nothing stayed quantized');
      assert.ok(blockBytes / (blockBytes + floatBytes) > 0.99,
        `only ${(100 * blockBytes / (blockBytes + floatBytes)).toFixed(1)}% stayed quantized`);
      const total = (blockBytes + floatBytes) / 1048576;
      assert.ok(total < 600, `${total.toFixed(0)} MB resident is more than this model should need`);
      console.log(`      Qwen3 resident: ${total.toFixed(0)} MB (${(blockBytes / 1048576).toFixed(0)} MB of blocks)`);
    });

    runtime = await createRuntime({backend: 'wasm', wasmBytes: await readFile(kernelsURL), limits: computeLimits});
    const started = Date.now();
    const result = await runtime.execute(graph, {typedOutputs: true});
    const logits = result.outputs.logits.data;

    await t.test('and it knows the capital of France', () => {
      assert.deepEqual(result.outputs.logits.shape, [1, config.vocab_size]);
      assert.ok(logits.every(Number.isFinite), 'a logit is not finite');
      let best = 0;
      for (let i = 1; i < logits.length; i++) if (logits[i] > logits[best]) best = i;
      // Not "a plausible token": the right one. Every part of this architecture
      // -- the per-head norm, the rotation, the repeated key heads, the gated
      // MLP, the tied output projection -- has to be right for this to hold.
      assert.equal(vocab[best], 'ĠParis',
        `predicted ${JSON.stringify(vocab[best])} instead of Paris`);
      console.log(`      forward pass: ${((Date.now() - started) / 1000).toFixed(1)} s, predicted ${JSON.stringify(vocab[best])}`);
    });
    await t.test('a cached decode agrees with the prefill and then generates', async () => {
      // `build_graph` recomputes the prompt every token and is the oracle;
      // `build_decode_graph` keeps the keys and values on the device. Fed the
      // same prompt, the second must reach the same answer at the same
      // position -- different graphs, same arithmetic.
      const context = 64;
      const template = call('zipp_model_decode_graph', [JSON.stringify(config), JSON.stringify(context)]);
      const plan = await prepareDecode(template, store, hostLimits);
      assert.equal(plan.resident.length, 2 * config.num_layers, 'one key and one value cache per layer');

      const session = await runtime.prepare(plan.program);
      try {
        // The weights are uploaded once, not once per token: that is the point
        // of preparing, and a quantized weight is uploaded as its blocks.
        assert.ok(session.residentBytes > blockBytes, 'the caches are resident too');

        let token = prompt[0], best = 0;
        for (let position = 0; position < prompt.length; position++) {
          const step = await stepInputs(plan, store, {token, position});
          const out = await session.run([step], {readback: ['logits']});
          const logits = out.outputs.logits.data;
          best = 0;
          for (let i = 1; i < logits.length; i++) if (logits[i] > logits[best]) best = i;
          token = position + 1 < prompt.length ? prompt[position + 1] : best;
        }
        assert.equal(vocab[best], 'ĠParis',
          `cached decode predicted ${JSON.stringify(vocab[best])} instead of Paris`);

        // And it keeps going, which the prefill path cannot do cheaply.
        const generated = [];
        for (let position = prompt.length; position < prompt.length + 6; position++) {
          const step = await stepInputs(plan, store, {token, position});
          const out = await session.run([step], {readback: ['logits']});
          const logits = out.outputs.logits.data;
          let next = 0;
          for (let i = 1; i < logits.length; i++) if (logits[i] > logits[next]) next = i;
          generated.push(next);
          token = next;
        }
        const text = tokenizer.decode(Uint32Array.from(generated));
        assert.ok(generated.every(id => id >= 0 && id < config.vocab_size));
        console.log(`      continued: ${JSON.stringify(text)}`);
      } finally { session.dispose(); }
    });
  } finally {
    runtime?.dispose(); engine.dispose(); store.dispose(); await source.close();
  }
});
