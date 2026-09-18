// A prompt a chunk at a time, against the same prompt a token at a time.
//
// A decode step reads every weight to process one token, and on a CPU that is
// most of what a step costs. A chunk is the same decode stage over several
// consecutive positions: it attends over and writes into the same caches, row
// by row, and reads each weight once for all of them. So a prompt can be run a
// chunk at a time, its caches handed to the one-token graph, and generation
// carry on from there.
//
// That is only worth having if it is the same computation. These are the
// claims, each measured rather than assumed:
//
//   * The logits at the prompt's last token agree with the step-by-step ones.
//   * A decode session seeded with a chunked prompt's caches then generates
//     what a step-by-step one does, logits and all.
//   * A padded final chunk is inert: its padding changes nothing real.
//   * Stages compose: head, middle and tail chunks give what the whole does.
//
//   ZIPP_QWEN3_MODEL=.../qwen3-0.6b-q4_k_m.gguf node --test tests/qwen3-chunks.test.mjs
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile, open, stat, access} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';

import {PluginRegistry, openGGUF, WeightStore, prepareDecode, stepInputs,
        resolveLimits} from '../src/index.mjs';
import {sourceDirectory} from './helpers.mjs';

const modelPath = process.env.ZIPP_QWEN3_MODEL
  ?? fileURLToPath(new URL('fixtures/tiny-qwen3.gguf', import.meta.url));
const runtimeURL = new URL('../../gpu-lab/src/runtime.mjs', import.meta.url);
const engineURL = new URL('../../dist/all/zipp_wasm.js', import.meta.url);
const wasmURL = new URL('../../dist/all/zipp_wasm_bg.wasm', import.meta.url);
const kernelsURL = new URL('../../gpu-lab/wasm/kernels.wasm', import.meta.url);
const quantURL = new URL('../../gpu-lab/src/quant.mjs', import.meta.url);
const exists = async url => { try { await access(url); return true; } catch { return false; } };
const ready = await exists(modelPath) && await exists(runtimeURL) &&
  await exists(engineURL) && await exists(wasmURL) && await exists(kernelsURL);
const reason = 'Needs dist/all built, the gpu-lab sibling, and tests/fixtures/tiny-qwen3.gguf';

const GB = 1024 * 1024 * 1024;
const CONTEXT = 32;
const CHUNK = 4;
const hostLimits = resolveLimits({
  maxModelFileBytes: 8 * GB, maxModelBytes: 8 * GB, maxDecodedBytes: 4 * GB,
  maxBoundInputBytes: 4 * GB, maxTensorElements: 2 ** 31, maxDimension: 1 << 21,
  maxTensors: 4096, maxNodes: 8192, maxContext: 4096,
});
const computeLimits = {
  maxNodes: 8192, maxElements: 4194304, maxOutputElements: 200000000,
  maxLogicalBytes: 4 * GB, maxWork: 2 ** 46, maxDimension: 1 << 21, maxInputElements: 2 ** 31 - 1,
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
const worst = (a, b) => { let w = 0; for (let i = 0; i < a.length; i++) w = Math.max(w, Math.abs(a[i] - b[i])); return w; };
const argmax = a => { let best = 0; for (let i = 1; i < a.length; i++) if (a[i] > a[best]) best = i; return best; };

test('a Qwen3 prompt a chunk at a time', {skip: !ready && reason}, async t => {
  const zipp = await import(engineURL);
  await zipp.default({module_or_path: await readFile(wasmURL)});
  const {createRuntime} = await import(runtimeURL);
  const kernels = await readFile(kernelsURL);
  const plugin = await new PluginRegistry().install(
    await sourceDirectory('../plugins/qwen3/'), {approve: () => true});
  const source = await fileSource(modelPath);
  const index = await openGGUF(source, 'model.gguf', null, hostLimits);
  const store = new WeightStore([index], hostLimits);
  const engine = new zipp.Engine();
  const runtime = await createRuntime({backend: 'wasm', wasmBytes: kernels, limits: {...computeLimits, maxSessions: 8}});
  try {
    engine.setSyncHostCapabilities([]);
    engine.setInstructionBudget(hostLimits.instructionBudget);
    engine.initPythonProject({...plugin.files}, plugin.entry, []);
    const call = (name, args) => { engine.renewInstructionBudget(); return JSON.parse(engine.pythonCall(name, args)); };
    const config = call('zipp_model_config',
      [JSON.stringify(index.metadata()), JSON.stringify(index.strings('tokenizer.ggml.tokens').length)]);
    const layers = config.num_layers, width = config.hidden_size;
    const plan = async (first, last, tokens) => prepareDecode(call('zipp_model_decode_stage',
      [JSON.stringify(config), JSON.stringify(CONTEXT), JSON.stringify(first), JSON.stringify(last),
       JSON.stringify(null), JSON.stringify(tokens)]), store, hostLimits);
    const output = r => r.outputs.logits ?? r.outputs.hidden;
    // Eleven tokens: two whole chunks and a padded one of three.
    const prompt = [...index.tokenizer().encode(
      'The capital of France is Paris, the capital of Italy is Rome, and the capital of Spain is')].slice(0, 11);
    assert.equal(prompt.length, 11, 'the tokenizer gave fewer than eleven tokens');

    /** The prompt one token at a time, then `more` greedy tokens. */
    async function stepwise(more) {
      const p = await plan(0, layers - 1, 1);
      const s = await runtime.prepare(p.program);
      let logits;
      for (const [position, token] of prompt.entries()) {
        logits = output(await s.run([await stepInputs(p, store, {position, token})], {readback: ['logits']})).data;
      }
      const seen = [Float32Array.from(logits)];
      for (let i = 0; i < more; i++) {
        const next = argmax(logits);
        logits = output(await s.run([await stepInputs(p, store, {position: prompt.length + i, token: next})], {readback: ['logits']})).data;
        seen.push(Float32Array.from(logits));
      }
      s.dispose();
      return seen;
    }
    /** The prompt a chunk at a time, returning the last token's logits and the caches. */
    async function chunked(p, s) {
      let logits;
      for (let position = 0; position < prompt.length; position += CHUNK) {
        const tokens = prompt.slice(position, position + CHUNK);
        const r = await s.run([await stepInputs(p, store, {position, tokens, count: tokens.length})], {readback: ['logits']});
        logits = output(r).data;
      }
      return Float32Array.from(logits);
    }

    const reference = await stepwise(3);
    const chunkPlan = await plan(0, layers - 1, CHUNK);
    assert.equal(chunkPlan.tokens, CHUNK);
    const chunkSession = await runtime.prepare(chunkPlan.program, {resident: chunkPlan.resident});
    const last = await chunked(chunkPlan, chunkSession);

    await t.test('the last token of a chunked prompt agrees with the step-by-step one', () => {
      const w = worst(last, reference[0]);
      t.diagnostic(`worst logit difference: ${w.toExponential(2)}`);
      assert.ok(w <= 1e-4, `chunked logits differ by ${w}`);
      assert.equal(argmax(last), argmax(reference[0]), 'a different next token');
    });

    await t.test('a decode session seeded with its caches generates the same', async () => {
      const caches = await chunkSession.download(chunkPlan.resident);
      const p = await plan(0, layers - 1, 1);
      const s = await runtime.prepare(p.program);
      // Each carried cache input is fed its chunk-written value on the first
      // step, and carried from there as if the decode graph had written it.
      const seed = {};
      for (const node of p.program.nodes) if (node.carry !== undefined) seed[node.id] = caches.outputs[node.carry].data;
      let logits = last, worstSeen = 0;
      for (let i = 0; i < 3; i++) {
        const step = await stepInputs(p, store, {position: prompt.length + i, token: argmax(logits)});
        if (i === 0) Object.assign(step.inputs, seed);
        logits = output(await s.run([step], {readback: ['logits']})).data;
        worstSeen = Math.max(worstSeen, worst(logits, reference[i + 1]));
        assert.equal(argmax(logits), argmax(reference[i + 1]), `a different token at step ${i}`);
      }
      t.diagnostic(`worst logit difference while generating: ${worstSeen.toExponential(2)}`);
      assert.ok(worstSeen <= 1e-4, `seeded generation differs by ${worstSeen}`);
      s.dispose();
    });

    await t.test('head, middle and tail chunks compose to the whole', async () => {
      const head = await plan(0, 0, CHUNK), middle = await plan(1, layers - 2, CHUNK), tail = await plan(layers - 1, layers - 1, CHUNK);
      // One at a time: a runtime prepares one graph at a time.
      const hs = await runtime.prepare(head.program), ms = await runtime.prepare(middle.program), ts = await runtime.prepare(tail.program);
      let logits;
      for (let position = 0; position < prompt.length; position += CHUNK) {
        const tokens = prompt.slice(position, position + CHUNK), count = tokens.length;
        // Only the real rows cross a seam, as they would between two peers.
        const a = output(await hs.run([await stepInputs(head, store, {position, tokens, count})], {readback: ['hidden']})).data.slice(0, count * width);
        const b = output(await ms.run([await stepInputs(middle, store, {position, hidden: a, count, from: head.manifest})], {readback: ['hidden']})).data.slice(0, count * width);
        logits = output(await ts.run([await stepInputs(tail, store, {position, hidden: b, count, from: middle.manifest})], {readback: ['logits']})).data;
      }
      const w = worst(logits, last);
      t.diagnostic(`worst difference, split against whole: ${w.toExponential(2)}`);
      assert.ok(w <= 1e-4, `split chunks differ from the whole by ${w}`);
      for (const s of [hs, ms, ts]) s.dispose();
    });

    await t.test('padding is inert', async () => {
      // The same eleven tokens with the last chunk padded differently: as one
      // chunk of three, and as three chunks of one. Nothing real may move.
      const p = chunkPlan, s = await runtime.prepare(p.program);
      let logits;
      for (let position = 0; position < 8; position += CHUNK) {
        logits = output(await s.run([await stepInputs(p, store, {position, tokens: prompt.slice(position, position + CHUNK), count: CHUNK})], {readback: ['logits']})).data;
      }
      for (let position = 8; position < 11; position++) {
        logits = output(await s.run([await stepInputs(p, store, {position, tokens: [prompt[position]], count: 1})], {readback: ['logits']})).data;
      }
      const w = worst(logits, last);
      t.diagnostic(`worst difference between paddings: ${w.toExponential(2)}`);
      assert.ok(w <= 1e-4, `padding changed the answer by ${w}`);
      s.dispose();
    });

    await t.test('on the fixed-point path too, chunks and steps agree', async () => {
      // matmul_fixed accumulates in integers, so a chunk -- many rows through
      // the same quants -- has no rounding order to differ in.
      const {quantizeWeight} = await import(quantURL);
      const fixedPlan = tokens => prepareDecode(call('zipp_model_decode_stage',
        [JSON.stringify(config), JSON.stringify(CONTEXT), JSON.stringify(0), JSON.stringify(layers - 1),
         JSON.stringify('layers'), JSON.stringify(tokens)]), store, hostLimits, {quantize: quantizeWeight});
      const one = await fixedPlan(1), many = await fixedPlan(CHUNK);
      const so = await runtime.prepare(one.program);
      let want;
      for (const [position, token] of prompt.entries()) want = output(await so.run([await stepInputs(one, store, {position, token})], {readback: ['logits']})).data;
      so.dispose();
      const sm = await runtime.prepare(many.program);
      let got;
      for (let position = 0; position < prompt.length; position += CHUNK) {
        const tokens = prompt.slice(position, position + CHUNK);
        got = output(await sm.run([await stepInputs(many, store, {position, tokens, count: tokens.length})], {readback: ['logits']})).data;
      }
      sm.dispose();
      const w = worst(got, want);
      t.diagnostic(`worst difference on the fixed-point path: ${w.toExponential(2)}`);
      assert.equal(w, 0, `fixed-point chunks differ from fixed-point steps by ${w}`);
    });

    await t.test('a chunk is refused where it does not fit, or with the wrong count', async () => {
      await assert.rejects(stepInputs(chunkPlan, store, {position: CONTEXT - 2, tokens: prompt.slice(0, 3), count: 3}), /Chunk position/);
      await assert.rejects(stepInputs(chunkPlan, store, {position: 0, tokens: prompt.slice(0, 3), count: 2}), /needs 2 token ids/);
      await assert.rejects(stepInputs(chunkPlan, store, {position: 0, tokens: prompt.slice(0, 5), count: 5}), /Tokens in this chunk/);
    });
    chunkSession.dispose();
  } finally {
    await runtime.dispose();
    engine.free?.();
    await source.close();
  }
});
