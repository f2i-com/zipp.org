// A Qwen3 divided across stages, and the seams between them.
//
// This is the machinery LEASED needs, tested as the thing it has to be: a range
// of layers on one runtime, the next range on another, a hidden state passed
// between them, and logits identical to running the whole model in one graph.
//
//   prefill: prompt -> [0..6] -> [7..13] -> [14..20] -> [21..27] -> logits
//   decode:  token  -> [0..6] -> [7..13] -> [14..20] -> [21..27] -> logits
//                       14 caches each, its own layers only
//
// Four ways of dividing it are covered: in half, in four equal parts, in four
// uneven ones, and for both prefill and decode. The uneven case is the one
// LEASED is actually for -- a phone taking four layers where a workstation
// takes twelve -- and equal divisions would never have shown a bug that
// depended on a share boundary falling somewhere unexpected.
//
// Identical, not close. Every path runs the same kernels over the same weights
// in the same order; the only difference is that some read a vector out and
// write it back partway. If float32 came back different from that, the seam
// would be lossy, and a lossy seam is not a seam.
//
// So the assertions are bit-for-bit, and a hidden state is round-tripped
// through a structured clone first -- the cheapest honest stand-in for the
// worker, process or network hop it would really take. A test that passed a
// live Float32Array between two runtimes in one heap would prove less than it
// appeared to.
//
// Decode is checked over several positions rather than one, which matters more
// than it looks: a cache written or read wrongly is right for the first token
// and wrong from the second.
//
// What this does NOT do is route anything. There is no peer protocol, no
// discovery, no multi-stage driver. This is the plugin ABI and the proof that
// the arithmetic survives being divided.
//
//   ZIPP_QWEN3_MODEL=.../qwen3-0.6b-q4_k_m.gguf node --test tests/qwen3-stages.test.mjs
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
const reason = 'Set ZIPP_QWEN3_MODEL to a Qwen3 GGUF, with dist/all built';

const GB = 1024 * 1024 * 1024;
const CONTEXT = 32;
const hostLimits = resolveLimits({
  maxModelFileBytes: 8 * GB, maxModelBytes: 8 * GB,
  maxDecodedBytes: 4 * GB, maxBoundInputBytes: 4 * GB,
  maxTensorElements: 2 ** 31, maxDimension: 1 << 21, maxTensors: 4096, maxNodes: 8192,
  maxContext: 4096,
});
const computeLimits = {
  maxNodes: 8192, maxElements: 4194304, maxInputElements: 200000000,
  maxOutputElements: 200000000, maxLogicalBytes: 3 * GB,
  maxWork: 200000000000, maxDimension: 1 << 21, maxSessions: 8, maxStepsPerRun: 64,
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

/** What a hidden state survives on its way to another device.
 *
 * structuredClone copies the buffer rather than sharing it, which is what a
 * postMessage to a Worker does and the nearest thing in one process to what a
 * socket does. If the values changed here, they would change there. */
function acrossTheSeam(hidden) {
  const copy = structuredClone(hidden.buffer.slice(
    hidden.byteOffset, hidden.byteOffset + hidden.byteLength));
  return new Float32Array(copy);
}

test('Qwen3 split across two stages gives the logits of the whole model',
  {skip: !ready && reason}, async t => {
  const zipp = await import(engineURL);
  await zipp.default({module_or_path: await readFile(wasmURL)});
  const {createRuntime} = await import(runtimeURL);

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
    const cut = Math.floor(layers / 2) - 1;          // 13 of 0..27
    const tokens = [...index.tokenizer().encode('The capital of France is')];

    await t.test('a stage lists only the tensors it holds', () => {
      const whole = call('zipp_model_tensors', [JSON.stringify(config)]);
      const front = call('zipp_model_tensors',
        [JSON.stringify(config), JSON.stringify(0), JSON.stringify(cut)]);
      const back = call('zipp_model_tensors',
        [JSON.stringify(config), JSON.stringify(cut + 1), JSON.stringify(layers - 1)]);

      // Eleven tensors a layer, plus the ends. Neither stage is told to load
      // the other's weights, which is the difference between splitting a model
      // and running all of it twice.
      assert.equal(front.length, 11 * (cut + 1) + 1, 'front: its layers and the embedding');
      assert.equal(back.length, 11 * (layers - 1 - cut) + 2, 'back: its layers, the norm and the head');
      assert.ok(front.includes('token_embd.weight'), 'the front looks tokens up');
      assert.ok(!front.includes('output_norm.weight'), 'the front does not end the model');
      assert.ok(back.includes('output_norm.weight'), 'the back ends it');
      assert.ok(!back.some(name => name.startsWith(`blk.${cut}.`)), 'no overlap at the cut');

      // Together they are the model, with token_embd shared by both ends.
      const union = new Set([...front, ...back]);
      assert.deepEqual([...union].sort(), [...new Set(whole)].sort(),
        'the two stages do not add up to the model');
      console.log(`      ${layers} layers -> ${front.length} tensors + ${back.length} tensors ` +
        `(whole model: ${whole.length})`);
    });

    // The model in one graph, which is what the halves have to reproduce.
    const wholePlan = await prepareDecode(
      call('zipp_model_decode_graph', [JSON.stringify(config), JSON.stringify(CONTEXT)]),
      store, hostLimits);
    const frontPlan = await prepareDecode(
      call('zipp_model_decode_stage',
        [JSON.stringify(config), JSON.stringify(CONTEXT), JSON.stringify(0), JSON.stringify(cut)]),
      store, hostLimits);
    const backPlan = await prepareDecode(
      call('zipp_model_decode_stage',
        [JSON.stringify(config), JSON.stringify(CONTEXT), JSON.stringify(cut + 1),
         JSON.stringify(layers - 1)]),
      store, hostLimits);

    await t.test('each stage says which layers it runs', () => {
      assert.deepEqual(frontPlan.stage, {first_layer: 0, last_layer: cut});
      assert.deepEqual(backPlan.stage, {first_layer: cut + 1, last_layer: layers - 1});
      assert.equal(frontPlan.hidden_size, config.hidden_size, 'the seam is the residual width');
    });

    // Three separate runtimes, because two stages on two devices is the point.
    const kernels = await readFile(kernelsURL);
    for (let i = 0; i < 3; i++) {
      runtimes.push(await createRuntime(
        {backend: 'wasm', wasmBytes: kernels, limits: computeLimits}));
    }
    const [wholeRt, frontRt, backRt] = runtimes;

    const whole = await wholeRt.prepare(wholePlan.program);
    const front = await frontRt.prepare(frontPlan.program);
    const back = await backRt.prepare(backPlan.program);

    let seamBytes = 0;
    for (const [position, token] of tokens.entries()) {
      await t.test(`position ${position}: ${JSON.stringify(vocab[token])}`, async () => {
        const one = await whole.run([await stepInputs(wholePlan, store, {token, position})],
          {readback: ['logits']});

        const a = await front.run([await stepInputs(frontPlan, store, {token, position})],
          {readback: ['hidden']});
        const hidden = a.outputs.hidden.data;
        assert.deepEqual(a.outputs.hidden.shape, [1, config.hidden_size]);
        assert.ok(hidden.every(Number.isFinite), 'the seam carried a non-finite value');

        // Out of one device and into another.
        const carried = acrossTheSeam(hidden);
        seamBytes = carried.byteLength;
        assert.deepEqual([...carried], [...hidden], 'the crossing changed the hidden state');

        const b = await back.run(
          [await stepInputs(backPlan, store, {position, hidden: carried})],
          {readback: ['logits']});

        // Bit for bit: same kernels, same weights, same order. Anything else
        // would mean the seam itself computes something.
        const want = one.outputs.logits.data, got = b.outputs.logits.data;
        assert.equal(got.length, want.length);
        for (let i = 0; i < want.length; i++) {
          if (!Object.is(got[i], want[i])) {
            assert.fail(`logit ${i} at position ${position}: ${got[i]} split, ${want[i]} whole`);
          }
        }
      });
    }

    await t.test('and the split model predicts what the whole one does', async () => {
      const position = tokens.length - 1;
      const a = await front.run(
        [await stepInputs(frontPlan, store, {token: tokens[position], position})],
        {readback: ['hidden']});
      const b = await back.run(
        [await stepInputs(backPlan, store, {position, hidden: acrossTheSeam(a.outputs.hidden.data)})],
        {readback: ['logits']});
      const logits = b.outputs.logits.data;
      let best = 0;
      for (let i = 1; i < logits.length; i++) if (logits[i] > logits[best]) best = i;
      assert.equal(vocab[best], 'ĠParis');
      console.log(`      ${tokens.length} positions, logits identical bit for bit; ` +
        `the seam moved ${seamBytes} bytes a token while 372 MB stayed put`);
    });

    await t.test('a stage that begins mid-model refuses to run without one', async () => {
      // The failure that matters if a peer is missing: not wrong numbers, an
      // error. And a hidden state of the wrong width is a different model.
      await assert.rejects(
        stepInputs(backPlan, store, {token: tokens[0], position: 0}),
        /needs \{hidden\}/);
      await assert.rejects(
        stepInputs(backPlan, store, {position: 0, hidden: new Float32Array(8)}),
        /is 1024 values, got 8/);
      const wrong = new Float32Array(config.hidden_size);
      wrong[3] = Number.NaN;
      await assert.rejects(
        stepInputs(backPlan, store, {position: 0, hidden: wrong}),
        /Non-finite value in a hidden state/);
    });
  } finally {
    for (const runtime of runtimes) await runtime?.dispose?.();
    engine.free?.();
    await source.close();
  }
});

test('a prompt prefills across stages the same way a token decodes across them',
  {skip: !ready && reason}, async t => {
  // A peer holding layers 8..15 has to prefill as well as decode: the prompt
  // must reach its layers before a cached step means anything. The prefill
  // seam is wider -- the residual for every position rather than one -- and
  // still nothing beside the weights: 20 KB for five tokens against 372 MB.
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
    const layers = config.num_layers;
    const tokens = [...index.tokenizer().encode('The capital of France is')];
    runtime = await createRuntime(
      {backend: 'wasm', wasmBytes: await readFile(kernelsURL), limits: computeLimits});

    const whole = await runtime.execute(
      await bindGraph(call('zipp_model_graph',
        [JSON.stringify(config), JSON.stringify(tokens)]), store, hostLimits),
      {typedOutputs: true});

    // Four stages rather than two, because "it divides" and "it divides in
    // half" are different claims. 28 layers into 7 + 7 + 7 + 7.
    const parts = 4, per = layers / parts;
    assert.equal(per % 1, 0, 'this model divides evenly into four');

    let carried = null, seam = 0;
    for (let part = 0; part < parts; part++) {
      const first = part * per, last = first + per - 1;
      const template = call('zipp_model_prefill_stage',
        [JSON.stringify(config), JSON.stringify(tokens),
         JSON.stringify(first), JSON.stringify(last)]);
      assert.deepEqual(template.stage, {first_layer: first, last_layer: last});

      const graph = await bindGraph(template, store, hostLimits,
        carried ? {feed: {hidden: carried}} : {});
      const out = await runtime.execute(graph, {typedOutputs: true});

      if (last === layers - 1) {
        // The last stage ends the model, so this is the comparison.
        const got = out.outputs.logits.data, want = whole.outputs.logits.data;
        assert.equal(got.length, want.length);
        for (let i = 0; i < want.length; i++) {
          if (!Object.is(got[i], want[i])) {
            assert.fail(`logit ${i}: ${got[i]} in ${parts} stages, ${want[i]} in one`);
          }
        }
      } else {
        assert.deepEqual(out.outputs.hidden.shape, [tokens.length, config.hidden_size]);
        carried = acrossTheSeam(out.outputs.hidden.data);
        seam = carried.byteLength;
      }
    }
    console.log(`      ${layers} layers in ${parts} stages of ${per}, logits identical; ` +
      `the seam carried ${seam} bytes between them`);
  } finally {
    await runtime?.dispose?.();
    engine.free?.();
    await source.close();
  }
});

test('a token decodes across four stages, each carrying only its own caches',
  {skip: !ready && reason}, async t => {
  // Decode is the serving path, and the one where the seam is paid per token
  // rather than per prompt. Four stages of seven layers, each with its own
  // caches, generating several tokens in sequence -- because a cache that is
  // wrong is wrong only from the second token onwards.
  const zipp = await import(engineURL);
  await zipp.default({module_or_path: await readFile(wasmURL)});
  const {createRuntime} = await import(runtimeURL);

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
    const layers = config.num_layers, parts = 4, per = layers / parts;
    const tokens = [...index.tokenizer().encode('The capital of France is')];
    const kernels = await readFile(kernelsURL);

    const whole = {plan: await prepareDecode(
      call('zipp_model_decode_graph', [JSON.stringify(config), JSON.stringify(CONTEXT)]),
      store, hostLimits)};
    runtimes.push(await createRuntime({backend: 'wasm', wasmBytes: kernels, limits: computeLimits}));
    whole.session = await runtimes[0].prepare(whole.plan.program);

    const stages = [];
    for (let part = 0; part < parts; part++) {
      const first = part * per, last = first + per - 1;
      const plan = await prepareDecode(call('zipp_model_decode_stage',
        [JSON.stringify(config), JSON.stringify(CONTEXT),
         JSON.stringify(first), JSON.stringify(last)]), store, hostLimits);
      // Two caches a layer, and only for this stage's layers.
      assert.equal(plan.resident.length, 2 * per,
        `stage ${part} carries ${plan.resident.length} caches, not ${2 * per}`);
      for (const name of plan.resident) {
        const layer = Number(name.slice(1));
        assert.ok(layer >= first && layer <= last,
          `stage ${part} carries ${name}, which is not one of layers ${first}..${last}`);
      }
      const runtime = await createRuntime({backend: 'wasm', wasmBytes: kernels, limits: computeLimits});
      runtimes.push(runtime);
      stages.push({plan, session: await runtime.prepare(plan.program), first, last});
    }

    // Several tokens, so a cache that is written or read wrongly shows up.
    let token = tokens[0], generated = [];
    for (let position = 0; position < tokens.length + 2; position++) {
      const feeding = position < tokens.length ? tokens[position] : token;
      const one = await whole.session.run(
        [await stepInputs(whole.plan, store, {token: feeding, position})], {readback: ['logits']});

      let hidden = null, split = null;
      for (const stage of stages) {
        const inputs = stage.first === 0
          ? await stepInputs(stage.plan, store, {token: feeding, position})
          : await stepInputs(stage.plan, store, {position, hidden});
        const out = await stage.session.run([inputs],
          {readback: [stage.last === layers - 1 ? 'logits' : 'hidden']});
        if (stage.last === layers - 1) split = out.outputs.logits.data;
        else hidden = acrossTheSeam(out.outputs.hidden.data);
      }

      const want = one.outputs.logits.data;
      for (let i = 0; i < want.length; i++) {
        if (!Object.is(split[i], want[i])) {
          assert.fail(`position ${position}, logit ${i}: ${split[i]} split, ${want[i]} whole`);
        }
      }
      let best = 0;
      for (let i = 1; i < split.length; i++) if (split[i] > split[best]) best = i;
      token = best;
      if (position >= tokens.length - 1) generated.push(vocab[best]);
    }
    assert.equal(generated[0], 'ĠParis');
    console.log(`      ${parts} stages of ${per} layers, ${tokens.length + 2} positions, ` +
      `logits identical throughout; generated ${JSON.stringify(generated.join(''))}`);
  } finally {
    for (const runtime of runtimes) await runtime?.dispose?.();
    engine.free?.();
    await source.close();
  }
});

test('stages need not be the same size, because peers are not the same machine',
  {skip: !ready && reason}, async t => {
  // A phone takes four layers where a workstation takes twelve. Nothing in the
  // code assumes equal shares -- but "takes arbitrary ranges" and "was ever run
  // with unequal ones" are different claims, and only one of them is evidence.
  //
  // 28 layers as 3 + 12 + 4 + 9, which is deliberately lopsided and prime-ish:
  // no split point falls where an even division would put one.
  const zipp = await import(engineURL);
  await zipp.default({module_or_path: await readFile(wasmURL)});
  const {createRuntime} = await import(runtimeURL);

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
    // Lopsided on purpose and derived from the model, so a different Qwen3
    // divides unevenly too rather than tripping an assertion written for this
    // one. For 28 layers this is 3 + 12 + 4 + 9.
    const shares = unevenShares(layers);
    assert.equal(shares.reduce((a, b) => a + b, 0), layers, 'the shares must be the model');
    assert.ok(shares.every(share => share >= 1), 'every peer holds at least one layer');
    assert.ok(new Set(shares).size > 1, 'these shares are not uneven');
    const tokens = [...index.tokenizer().encode('The capital of France is')];
    const kernels = await readFile(kernelsURL);

    const ranges = [];
    let at = 0;
    for (const share of shares) { ranges.push([at, at + share - 1]); at += share; }

    await t.test('an uneven share is still exactly its own tensors and caches', () => {
      let tensors = 0;
      for (const [first, last] of ranges) {
        const names = call('zipp_model_tensors',
          [JSON.stringify(config), JSON.stringify(first), JSON.stringify(last)]);
        const own = names.filter(n => n.startsWith('blk.'));
        assert.equal(own.length, 11 * (last - first + 1),
          `layers ${first}..${last}: eleven tensors a layer`);
        for (const name of own) {
          const layer = Number(name.split('.')[1]);
          assert.ok(layer >= first && layer <= last, `${name} is not in ${first}..${last}`);
        }
        tensors += own.length;
      }
      assert.equal(tensors, 11 * layers, 'the shares do not cover the model exactly once');
      console.log(`      shares ${shares.join(' + ')} = ${layers} layers`);
    });

    // Prefill and decode both, through the same lopsided division.
    const whole = await runtime0(runtimes, createRuntime, kernels);
    const oneShot = await whole.execute(
      await bindGraph(call('zipp_model_graph',
        [JSON.stringify(config), JSON.stringify(tokens)]), store, hostLimits),
      {typedOutputs: true});

    let carried = null, prefilled = null;
    for (const [first, last] of ranges) {
      const graph = await bindGraph(call('zipp_model_prefill_stage',
        [JSON.stringify(config), JSON.stringify(tokens),
         JSON.stringify(first), JSON.stringify(last)]),
        store, hostLimits, carried ? {feed: {hidden: carried}} : {});
      const out = await whole.execute(graph, {typedOutputs: true});
      if (last === layers - 1) prefilled = out.outputs.logits.data;
      else carried = acrossTheSeam(out.outputs.hidden.data);
    }

    await t.test('an uneven prefill gives the whole model logits', () => {
      const want = oneShot.outputs.logits.data;
      for (let i = 0; i < want.length; i++) {
        if (!Object.is(prefilled[i], want[i])) {
          assert.fail(`logit ${i}: ${prefilled[i]} in ${shares.join('+')}, ${want[i]} in one`);
        }
      }
      let best = 0;
      for (let i = 1; i < prefilled.length; i++) if (prefilled[i] > prefilled[best]) best = i;
      assert.equal(vocab[best], 'ĠParis');
      console.log(`      uneven prefill: identical, still ${JSON.stringify(vocab[best])}`);
    });

    // And decode, which is where the caches are and so where an uneven share
    // could go wrong in a way prefill would not show.
    const wholePlan = await prepareDecode(
      call('zipp_model_decode_graph', [JSON.stringify(config), JSON.stringify(CONTEXT)]),
      store, hostLimits);
    const wholeSession = await (await runtime0(runtimes, createRuntime, kernels))
      .prepare(wholePlan.program);

    const stages = [];
    for (const [first, last] of ranges) {
      const plan = await prepareDecode(call('zipp_model_decode_stage',
        [JSON.stringify(config), JSON.stringify(CONTEXT),
         JSON.stringify(first), JSON.stringify(last)]), store, hostLimits);
      assert.equal(plan.resident.length, 2 * (last - first + 1),
        `layers ${first}..${last} should carry two caches a layer`);
      const runtime = await runtime0(runtimes, createRuntime, kernels);
      stages.push({plan, session: await runtime.prepare(plan.program), first, last});
    }

    await t.test('an uneven decode agrees at every position', async () => {
      let token = tokens[0];
      for (let position = 0; position < tokens.length + 1; position++) {
        const feeding = position < tokens.length ? tokens[position] : token;
        const one = await wholeSession.run(
          [await stepInputs(wholePlan, store, {token: feeding, position})],
          {readback: ['logits']});
        let hidden = null, split = null;
        for (const stage of stages) {
          const inputs = stage.first === 0
            ? await stepInputs(stage.plan, store, {token: feeding, position})
            : await stepInputs(stage.plan, store, {position, hidden});
          const out = await stage.session.run([inputs],
            {readback: [stage.last === layers - 1 ? 'logits' : 'hidden']});
          if (stage.last === layers - 1) split = out.outputs.logits.data;
          else hidden = acrossTheSeam(out.outputs.hidden.data);
        }
        const want = one.outputs.logits.data;
        for (let i = 0; i < want.length; i++) {
          if (!Object.is(split[i], want[i])) {
            assert.fail(`position ${position}, logit ${i}: ${split[i]} vs ${want[i]}`);
          }
        }
        let best = 0;
        for (let i = 1; i < split.length; i++) if (split[i] > split[best]) best = i;
        token = best;
      }
      console.log(`      uneven decode: ${tokens.length + 1} positions, identical throughout`);
    });
  } finally {
    for (const runtime of runtimes) await runtime?.dispose?.();
    engine.free?.();
    await source.close();
  }
});

/** Four unequal shares of `layers`, summing to it exactly.
 *
 * Fractions chosen so no boundary lands where an even division would put one:
 * for 28 layers, 3 + 12 + 4 + 9. */
function unevenShares(layers) {
  const first = Math.max(1, Math.round(layers * 0.107));
  const second = Math.max(1, Math.round(layers * 0.43));
  const third = Math.max(1, Math.round(layers * 0.143));
  return [first, second, third, layers - first - second - third];
}

/** One runtime, kept in the list the test disposes. */
async function runtime0(runtimes, createRuntime, kernels) {
  const runtime = await createRuntime(
    {backend: 'wasm', wasmBytes: kernels, limits: computeLimits});
  runtimes.push(runtime);
  return runtime;
}

test('a later stage is told a length, not the prompt', {skip: !ready && reason}, async t => {
  // What crosses to a peer should be what it can use. A stage running layers
  // 20..27 never looks a token up -- it needs the prompt's *shape*, and
  // sending it the ids would be handing over the text to establish a
  // dimension. So the seam is activations plus a length, and the plugin
  // refuses the combinations that would make that untrue.
  const zipp = await import(engineURL);
  await zipp.default({module_or_path: await readFile(wasmURL)});
  const {createRuntime} = await import(runtimeURL);

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
    const layers = config.num_layers, cut = Math.floor(layers / 2) - 1;
    const tokens = [...index.tokenizer().encode('The capital of France is')];
    const kernels = await readFile(kernelsURL);
    const runtime = await runtime0(runtimes, createRuntime, kernels);

    await t.test('a length builds the same graph the token list did', async () => {
      const byTokens = call('zipp_model_prefill_stage',
        [JSON.stringify(config), JSON.stringify(tokens),
         JSON.stringify(cut + 1), JSON.stringify(layers - 1)]);
      const byLength = call('zipp_model_prefill_stage',
        [JSON.stringify(config), JSON.stringify(tokens.length),
         JSON.stringify(cut + 1), JSON.stringify(layers - 1)]);
      assert.deepEqual(byLength, byTokens,
        'the ids changed a graph that is not supposed to read them');
    });

    await t.test('the first stage still needs the ids, and says so', () => {
      assert.throws(() => call('zipp_model_prefill_stage',
        [JSON.stringify(config), JSON.stringify(tokens.length),
         JSON.stringify(0), JSON.stringify(cut)]),
        /looks tokens up and needs their ids/);
    });

    await t.test('and a length-only stage composes into the same logits', async () => {
      const head = await bindGraph(call('zipp_model_prefill_stage',
        [JSON.stringify(config), JSON.stringify(tokens),
         JSON.stringify(0), JSON.stringify(cut)]), store, hostLimits);
      const hidden = acrossTheSeam(
        (await runtime.execute(head, {typedOutputs: true})).outputs.hidden.data);
      // The tail is given a number, and nothing else about the prompt.
      const tail = await bindGraph(call('zipp_model_prefill_stage',
        [JSON.stringify(config), JSON.stringify(tokens.length),
         JSON.stringify(cut + 1), JSON.stringify(layers - 1)]),
        store, hostLimits, {feed: {hidden}});
      const split = (await runtime.execute(tail, {typedOutputs: true})).outputs.logits.data;
      const want = (await runtime.execute(await bindGraph(call('zipp_model_graph',
        [JSON.stringify(config), JSON.stringify(tokens)]), store, hostLimits),
        {typedOutputs: true})).outputs.logits.data;
      for (let i = 0; i < want.length; i++) {
        if (!Object.is(split[i], want[i])) assert.fail(`logit ${i}: ${split[i]} vs ${want[i]}`);
      }
      console.log('      a tail stage given only a length reproduces the model exactly');
    });

    await t.test('a layer index that is not an integer is refused, not rounded', () => {
      // Coercion at a boundary between machines is how a peer ends up running
      // a range nobody asked for and saying nothing about it.
      // null is not in this list: it is how a caller says "unspecified", and
      // means the default end of the model rather than a coerced index.
      for (const bad of [3.7, '5', true]) {
        assert.throws(() => call('zipp_model_decode_stage',
          [JSON.stringify(config), JSON.stringify(CONTEXT),
           JSON.stringify(bad), JSON.stringify(layers - 1)]),
          /must be an integer|Invalid layer range/,
          `first_layer ${JSON.stringify(bad)}`);
      }
      assert.throws(() => call('zipp_model_decode_stage',
        [JSON.stringify(config), JSON.stringify(CONTEXT),
         JSON.stringify(layers - 1), JSON.stringify(0)]), /Invalid layer range/);
    });
  } finally {
    for (const r of runtimes) await r?.dispose?.();
    engine.free?.();
    await source.close();
  }
});
