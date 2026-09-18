// What a float16 seam would cost, measured rather than assumed.
//
// The seam is float32 today, which is why every staging test can assert
// bit-for-bit equality -- and that property is most of what makes those tests
// worth anything. Halving the seam would halve what crosses between peers:
// 2 KB a token instead of 4, and for a prompt, 10 KB instead of 20. At the
// scale LEASED is for that is a real saving, and it would end bit-for-bit.
//
// So this does not change anything. It measures, on the same prompts the
// external oracle uses, and reports:
//
//   * the worst logit difference against the float32 whole model;
//   * the same against transformers, the independent reference;
//   * whether the argmax moves, and whether the top five reorder.
//
// The last is what actually decides it. A logit difference of 1e-3 is
// meaningless if the ranking is unchanged and fatal if it is not.
//
// Narrowing is a transport decision, made between a readback and the next
// stage's input, so nothing here touches the plugin or the graph. If it were
// ever adopted it would be an option on the driver with a name, not a quiet
// change of what a seam is.
//
//   ZIPP_QWEN3_MODEL=.../qwen3-0.6b-q4_k_m.gguf node --test tests/qwen3-half-seam.test.mjs
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile, open, stat, access} from 'node:fs/promises';

import {PluginRegistry, openGGUF, WeightStore, prepareDecode, stepInputs,
        resolveLimits} from '../src/index.mjs';
import {sourceDirectory} from './helpers.mjs';
import {throughHalf} from './helpers/half.mjs';

const modelPath = process.env.ZIPP_QWEN3_MODEL;
const runtimeURL = new URL('../../gpu-lab/src/runtime.mjs', import.meta.url);
const engineURL = new URL('../../dist/all/zipp_wasm.js', import.meta.url);
const wasmURL = new URL('../../dist/all/zipp_wasm_bg.wasm', import.meta.url);
const kernelsURL = new URL('../../gpu-lab/wasm/kernels.wasm', import.meta.url);
const fixtures = new URL('fixtures/qwen3-oracle/', import.meta.url);

const exists = async url => { try { await access(url); return true; } catch { return false; } };
const ready = Boolean(modelPath) && await exists(modelPath) && await exists(runtimeURL) &&
  await exists(engineURL) && await exists(wasmURL) && await exists(new URL('qwen3_oracle.json', fixtures));
const reason = 'Set ZIPP_QWEN3_MODEL to the Qwen3 GGUF the oracle fixtures were made from';

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

const topK = (logits, k) => [...logits.keys()]
  .sort((a, b) => logits[b] - logits[a]).slice(0, k);

test('a float16 seam, measured against float32 and against transformers',
  {skip: !ready && reason}, async t => {
  const oracle = JSON.parse(await readFile(new URL('qwen3_oracle.json', fixtures), 'utf8'));
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
    const kernels = await readFile(kernelsURL);
    const newRuntime = async () => {
      const runtime = await createRuntime(
        {backend: 'wasm', wasmBytes: kernels, limits: computeLimits});
      runtimes.push(runtime);
      return runtime;
    };

    const wholePlan = await prepareDecode(
      call('zipp_model_decode_graph', [JSON.stringify(config), JSON.stringify(CONTEXT)]),
      store, hostLimits);
    const whole = await (await newRuntime()).prepare(wholePlan.program);

    /** `parts` stages, each its own runtime, with an optional narrowing at each seam. */
    const build = async parts => {
      const per = layers / parts, stages = [];
      for (let part = 0; part < parts; part++) {
        const first = part * per, last = first + per - 1;
        const plan = await prepareDecode(call('zipp_model_decode_stage',
          [JSON.stringify(config), JSON.stringify(CONTEXT),
           JSON.stringify(first), JSON.stringify(last)]), store, hostLimits);
        stages.push({plan, session: await (await newRuntime()).prepare(plan.program), first, last});
      }
      return stages;
    };

    const run = async (stages, position, token, narrow) => {
      let hidden = null;
      for (const stage of stages) {
        const inputs = stage.first === 0
          ? await stepInputs(stage.plan, store, {token, position})
          : await stepInputs(stage.plan, store, {position, hidden});
        const out = await stage.session.run([inputs],
          {readback: [stage.last === layers - 1 ? 'logits' : 'hidden']});
        if (stage.last === layers - 1) return out.outputs.logits.data;
        // The seam. Float32 is exact; float16 is the thing being measured.
        hidden = narrow ? throughHalf(out.outputs.hidden.data)
                        : Float32Array.from(out.outputs.hidden.data);
      }
      throw new Error('no stage produced logits');
    };

    const halves = await build(2), quarters = await build(4);
    const report = [];

    for (const [n, entry] of oracle.prompts.entries()) {
      const raw = await readFile(new URL(`qwen3_logits_${n + 1}.f32`, fixtures));
      const reference = new Float32Array(raw.buffer, raw.byteOffset, raw.byteLength / 4);
      const ids = entry.ids;

      // The same prompt through the cache, one position at a time.
      const feed = async (stages, narrow) => {
        let last = null;
        for (const [position, token] of ids.entries()) {
          last = await run(stages, position, token, narrow);
        }
        return last;
      };
      let exact = null;
      for (const [position, token] of ids.entries()) {
        const out = await whole.run(
          [await stepInputs(wholePlan, store, {token, position})], {readback: ['logits']});
        exact = out.outputs.logits.data;
      }
      exact = Float32Array.from(exact);

      for (const [label, stages, seams] of [['2 stages', halves, 1], ['4 stages', quarters, 3]]) {
        const got = await feed(stages, true);
        let worstExact = 0, worstOracle = 0;
        for (let i = 0; i < got.length; i++) {
          worstExact = Math.max(worstExact, Math.abs(got[i] - exact[i]));
          worstOracle = Math.max(worstOracle, Math.abs(got[i] - reference[i]));
        }
        const before = topK(exact, 5), after = topK(got, 5);
        report.push({prompt: entry.prompt, label, seams, worstExact, worstOracle,
          argmaxMoved: after[0] !== before[0],
          top5Reordered: after.join() !== before.join(),
          predicted: vocab[after[0]], expected: vocab[before[0]]});
      }
    }

    await t.test('the cost is bounded and the ranking survives', () => {
      let worstExact = 0, worstOracle = 0, moved = 0, reordered = 0;
      for (const row of report) {
        worstExact = Math.max(worstExact, row.worstExact);
        worstOracle = Math.max(worstOracle, row.worstOracle);
        if (row.argmaxMoved) moved++;
        if (row.top5Reordered) reordered++;
        console.log(`      ${row.label} (${row.seams} seam${row.seams > 1 ? 's' : ''}) ` +
          `${JSON.stringify(row.prompt)}: vs float32 ${row.worstExact.toExponential(3)}, ` +
          `vs transformers ${row.worstOracle.toExponential(3)}, ` +
          `argmax ${row.argmaxMoved ? `MOVED to ${JSON.stringify(row.predicted)}` : 'held'}` +
          `${row.top5Reordered ? ', top-5 reordered' : ''}`);
      }
      console.log(`      worst over everything: ${worstExact.toExponential(3)} vs float32, ` +
        `${worstOracle.toExponential(3)} vs transformers; ` +
        `argmax moved ${moved}/${report.length}, top-5 reordered ${reordered}/${report.length}`);

      // Asserted on what was measured, with headroom -- not a bound chosen to
      // pass. Worst observed is 5.952e-3 over three seams; this is about three
      // times that. If a change makes a float16 seam cost more, the number is
      // the thing to argue about rather than the bound.
      assert.ok(worstExact < 2e-2,
        `a float16 seam costs ${worstExact} against float32, more than expected`);
      assert.equal(moved, 0,
        'a float16 seam moved the most likely token: that is the finding, not a tolerance to widen');
    });

    await t.test('and float32 across the same seams is still exact', async () => {
      // The control. Without it, a small float16 difference could be blamed on
      // staging rather than on the narrowing.
      for (const entry of oracle.prompts) {
        for (const stages of [halves, quarters]) {
          let split = null, exact = null;
          for (const [position, token] of entry.ids.entries()) {
            split = await run(stages, position, token, false);
            exact = (await whole.run(
              [await stepInputs(wholePlan, store, {token, position})],
              {readback: ['logits']})).outputs.logits.data;
          }
          for (let i = 0; i < exact.length; i++) {
            if (!Object.is(split[i], exact[i])) {
              assert.fail(`float32 seam differs at logit ${i}: ${split[i]} vs ${exact[i]}`);
            }
          }
        }
      }
      console.log('      float32 across 1 and 3 seams: bit-for-bit, as before');
    });
  } finally {
    for (const runtime of runtimes) await runtime?.dispose?.();
    engine.free?.();
    await source.close();
  }
});
