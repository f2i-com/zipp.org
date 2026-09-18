// The seam across a real boundary, and what happens when one side of it dies.
//
// `qwen3-stages.test.mjs` proves the arithmetic survives being divided, with
// the hidden state round-tripped through a structured clone. That is an honest
// stand-in but it is still one heap, one event loop and one failure domain.
// This runs each stage in its own worker thread, so the seam is a real
// `postMessage`: serialised, transferred, and crossing between isolates that
// can be killed independently.
//
// Two things it tests that in-process staging cannot:
//
//   * a transferred buffer. The hidden state is moved rather than copied, so
//     the sender is left holding a detached buffer -- which is what a real
//     hand-off does and what a test passing a live array never exercises.
//   * a peer that goes away. A stage that dies takes its caches with it, and
//     the right behaviour is an error naming which stage, not a hang and not a
//     plausible token. Both of those are worse than a crash.
//
// Still not a peer protocol: no discovery, no routing, no driver. The workers
// here are the smallest thing that makes the boundary real.
//
//   ZIPP_QWEN3_MODEL=.../qwen3-0.6b-q4_k_m.gguf node --test tests/qwen3-workers.test.mjs
import test from 'node:test';
import assert from 'node:assert/strict';
import {Worker} from 'node:worker_threads';
import {readFile, open, stat, access} from 'node:fs/promises';

import {PluginRegistry, openGGUF, WeightStore, prepareDecode, stepInputs,
        resolveLimits} from '../src/index.mjs';
import {sourceDirectory} from './helpers.mjs';

const modelPath = process.env.ZIPP_QWEN3_MODEL;
const runtimeURL = new URL('../../gpu-lab/src/runtime.mjs', import.meta.url);
const engineURL = new URL('../../dist/all/zipp_wasm.js', import.meta.url);
const wasmURL = new URL('../../dist/all/zipp_wasm_bg.wasm', import.meta.url);
const kernelsURL = new URL('../../gpu-lab/wasm/kernels.wasm', import.meta.url);
const workerURL = new URL('helpers/stage-worker.mjs', import.meta.url);

const exists = async url => { try { await access(url); return true; } catch { return false; } };
const ready = Boolean(modelPath) && await exists(modelPath) &&
  await exists(runtimeURL) && await exists(engineURL) && await exists(wasmURL);
const reason = 'Set ZIPP_QWEN3_MODEL to a Qwen3 GGUF, with dist/all built';

const GB = 1024 * 1024 * 1024;
const CONTEXT = 32;
const LIMITS = {
  maxModelFileBytes: 8 * GB, maxModelBytes: 8 * GB,
  maxDecodedBytes: 4 * GB, maxBoundInputBytes: 4 * GB,
  maxTensorElements: 2 ** 31, maxDimension: 1 << 21, maxTensors: 4096, maxNodes: 8192,
  maxContext: 4096,
};
const COMPUTE = {
  maxNodes: 8192, maxElements: 4194304, maxInputElements: 200000000,
  maxOutputElements: 200000000, maxLogicalBytes: 3 * GB,
  maxWork: 200000000000, maxDimension: 1 << 21, maxSessions: 4, maxStepsPerRun: 64,
};
// A dead worker never answers, so every round-trip is bounded. Without this a
// peer going away is a test that hangs rather than a test that fails.
const REPLY_TIMEOUT = 120000;

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

/** A stage running somewhere else, addressed by messages. */
class StagePeer {
  #worker; #pending = new Map(); #next = 1; #dead = null;
  constructor(worker, {first, last}) {
    this.#worker = worker;
    this.first = first; this.last = last;
    worker.on('message', message => {
      const entry = this.#pending.get(message.id);
      if (!entry) return;
      this.#pending.delete(message.id);
      clearTimeout(entry.timer);
      if (message.ok) entry.resolve(message);
      else entry.reject(new Error(`stage ${first}..${last}: ${message.error}`));
    });
    // A worker that exits with requests outstanding fails them, rather than
    // leaving a caller waiting for an answer that is never coming.
    worker.on('exit', code => {
      this.#dead = new Error(`stage ${first}..${last} is gone (worker exited, code ${code})`);
      for (const [, entry] of this.#pending) {
        clearTimeout(entry.timer);
        entry.reject(this.#dead);
      }
      this.#pending.clear();
    });
  }
  static async start(options) {
    const worker = new Worker(workerURL, {workerData: options});
    await new Promise((resolve, reject) => {
      worker.once('message', resolve);
      worker.once('error', reject);
    });
    return new StagePeer(worker, options);
  }
  ask(message, transfer = []) {
    if (this.#dead) return Promise.reject(this.#dead);
    const id = this.#next++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(
        () => { this.#pending.delete(id); reject(new Error(`stage ${this.first}..${this.last} did not answer`)); },
        REPLY_TIMEOUT);
      this.#pending.set(id, {resolve, reject, timer});
      this.#worker.postMessage({id, ...message}, transfer);
    });
  }
  terminate() { return this.#worker.terminate(); }
}

test('a model divided across worker threads gives the same logits',
  {skip: !ready && reason}, async t => {
  const zipp = await import(engineURL);
  await zipp.default({module_or_path: await readFile(wasmURL)});
  const {createRuntime} = await import(runtimeURL);

  const plugin = await new PluginRegistry().install(
    await sourceDirectory('../plugins/qwen3/'), {approve: () => true});
  const source = await fileSource(modelPath);
  const index = await openGGUF(source, 'model.gguf', null, LIMITS);
  const store = new WeightStore([index], resolveLimits(LIMITS));
  const engine = new zipp.Engine();
  const peers = [];
  let runtime;
  try {
    engine.setSyncHostCapabilities([]);
    engine.setInstructionBudget(resolveLimits(LIMITS).instructionBudget);
    engine.initPythonProject({...plugin.files}, plugin.entry, []);
    const call = (name, args) => {
      engine.renewInstructionBudget();
      return JSON.parse(engine.pythonCall(name, args));
    };
    const vocab = index.strings('tokenizer.ggml.tokens');
    const config = call('zipp_model_config',
      [JSON.stringify(index.metadata()), JSON.stringify(vocab.length)]);
    const layers = config.num_layers, parts = 3;
    const tokens = [...index.tokenizer().encode('The capital of France is')];

    // The whole model here, as the thing three workers have to reproduce.
    const wholePlan = await prepareDecode(
      call('zipp_model_decode_graph', [JSON.stringify(config), JSON.stringify(CONTEXT)]),
      store, resolveLimits(LIMITS));
    runtime = await createRuntime(
      {backend: 'wasm', wasmBytes: await readFile(kernelsURL), limits: COMPUTE});
    const whole = await runtime.prepare(wholePlan.program);

    const options = {
      modelPath, pluginDir: '../plugins/qwen3/', context: CONTEXT,
      engineURL: engineURL.href, wasmURL: wasmURL.href,
      runtimeURL: runtimeURL.href, kernelsURL: kernelsURL.href,
      limits: LIMITS, compute: COMPUTE,
    };
    const bounds = [[0, 9], [10, 18], [19, layers - 1]];
    for (const [first, last] of bounds) {
      peers.push(await StagePeer.start({...options, first, last}));
    }

    /** One token through every stage, hidden states transferred between them. */
    const acrossPeers = async (position, token) => {
      let hidden = null;
      for (const peer of peers) {
        const reply = await peer.ask(
          peer.first === 0 ? {kind: 'decode', position, token}
                           : {kind: 'decode', position, hidden},
          hidden ? [hidden.buffer] : []);
        if (reply.logits) return reply.logits;
        // The buffer we just sent is gone: transferred, not copied.
        if (hidden) assert.equal(hidden.buffer.byteLength, 0, 'the hidden state was copied, not moved');
        hidden = reply.hidden;
      }
      throw new Error('no stage produced logits');
    };

    await t.test('three workers, each holding a third, agree bit for bit', async () => {
      for (const [position, token] of tokens.entries()) {
        const one = await whole.run(
          [await stepInputs(wholePlan, store, {token, position})], {readback: ['logits']});
        const split = await acrossPeers(position, token);
        const want = one.outputs.logits.data;
        assert.equal(split.length, want.length);
        for (let i = 0; i < want.length; i++) {
          if (!Object.is(split[i], want[i])) {
            assert.fail(`position ${position}, logit ${i}: ${split[i]} across workers, ${want[i]} here`);
          }
        }
      }
      const logits = await acrossPeers(tokens.length - 1, tokens[tokens.length - 1]);
      let best = 0;
      for (let i = 1; i < logits.length; i++) if (logits[i] > logits[best]) best = i;
      assert.equal(vocab[best], 'ĠParis');
      console.log(`      ${layers} layers across ${parts} worker threads, ` +
        `logits identical, hidden states transferred not copied`);
    });

    await t.test('a stage that goes away is an error, not a hang or a wrong token', async () => {
      // The failure LEASED will actually meet. What must not happen is a
      // caller waiting forever, or -- worse -- a token that looks fine.
      await peers[1].terminate();
      await assert.rejects(
        acrossPeers(tokens.length, 100),
        error => {
          assert.match(error.message, /stage 10\.\.18/, 'the error names the stage that died');
          return true;
        },
        'a dead stage should refuse, and say which one it was');
      console.log('      a terminated stage refuses by name rather than hanging');
    });

    await t.test('and restarting it is not enough, because the caches died with it', async () => {
      // A stage is not stateless. Its caches hold every position it has seen,
      // so a replacement is a stage that has seen nothing -- and continuing
      // into it produces confident nonsense rather than an error. That is the
      // design fact worth writing down: recovering a peer means re-prefilling
      // its range from the seam before it, not just starting a process.
      peers[1] = await StagePeer.start({
        modelPath, pluginDir: '../plugins/qwen3/', context: CONTEXT,
        engineURL: engineURL.href, wasmURL: wasmURL.href,
        runtimeURL: runtimeURL.href, kernelsURL: kernelsURL.href,
        limits: LIMITS, compute: COMPUTE, first: 10, last: 18,
      });
      const position = tokens.length - 1;
      const fresh = await acrossPeers(position, tokens[position]);
      const one = await whole.run(
        [await stepInputs(wholePlan, store, {token: tokens[position], position})],
        {readback: ['logits']});

      let differs = 0;
      for (let i = 0; i < fresh.length; i++) if (!Object.is(fresh[i], one.outputs.logits.data[i])) differs++;
      assert.ok(differs > 0,
        'a replacement stage with empty caches somehow agreed; then the caches do nothing');
      let best = 0;
      for (let i = 1; i < fresh.length; i++) if (fresh[i] > fresh[best]) best = i;
      console.log(`      a replacement with empty caches differs in ${differs} of ` +
        `${fresh.length} logits and now predicts ${JSON.stringify(vocab[best])} ` +
        `-- a dead peer means re-prefilling its range, not restarting it`);
    });
  } finally {
    for (const peer of peers) await peer?.terminate().catch(() => {});
    await runtime?.dispose?.();
    engine.free?.();
    await source.close();
  }
});
