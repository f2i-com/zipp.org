// One stage of a model, in a worker thread, answering messages.
//
// This is test infrastructure, not a peer: there is no discovery, no routing
// and no protocol beyond what the tests need. What it is for is making the
// seam a real boundary. In one heap a hidden state can be handed along as a
// live Float32Array and nothing about that is a crossing; here it is
// serialised by `postMessage`, and the parent transfers the buffer rather than
// copying it, so the test can prove the values survived a move between two
// isolates.
//
// It also gives the tests a peer that can be killed. A stage that dies takes
// its caches with it, and what should happen next is an error naming which
// stage -- not a hang and not a plausible-looking token.
//
//   workerData: {modelPath, engineURL, wasmURL, runtimeURL, kernelsURL,
//                pluginDir, first, last, context}
//   in:  {id, kind: 'prefill', tokens, hidden?}
//        {id, kind: 'decode', position, token?, hidden?}
//   out: {id, ok: true, hidden|logits} | {id, ok: false, error}
import {parentPort, workerData} from 'node:worker_threads';
import {readFile, open, stat} from 'node:fs/promises';

import {PluginRegistry, openGGUF, WeightStore, bindGraph, prepareDecode,
        stepInputs, resolveLimits} from '../../src/index.mjs';

const {modelPath, pluginDir, first, last, context, limits, compute} = workerData;
// workerData is structured-cloned, so URLs arrive as hrefs; readFile and import
// both want a URL rather than a string that looks like one.
const engineURL = new URL(workerData.engineURL);
const wasmURL = new URL(workerData.wasmURL);
const runtimeURL = new URL(workerData.runtimeURL);
const kernelsURL = new URL(workerData.kernelsURL);

async function fileSource(path) {
  const size = (await stat(path)).size;
  const handle = await open(path);
  return {
    size: () => size,
    async read(_name, offset, length) {
      const out = new Uint8Array(length);
      const {bytesRead} = await handle.read(out, 0, length, offset);
      if (bytesRead !== length) throw new Error('short read');
      return out;
    },
  };
}

const hostLimits = resolveLimits(limits);
const zipp = await import(engineURL);
await zipp.default({module_or_path: await readFile(wasmURL)});
const {createRuntime} = await import(runtimeURL);

const plugin = await new PluginRegistry().install(
  await (await import('../helpers.mjs')).sourceDirectory(pluginDir), {approve: () => true});
const index = await openGGUF(await fileSource(modelPath), 'model.gguf', null, hostLimits);
const store = new WeightStore([index], hostLimits);

const engine = new zipp.Engine();
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
const ends = last === config.num_layers - 1;

const runtime = await createRuntime(
  {backend: 'wasm', wasmBytes: await readFile(kernelsURL), limits: compute});

// The decode graph is prepared once: its weights are uploaded here and stay,
// which is the whole reason a stage is worth being a long-lived thing rather
// than a function somebody calls.
const decodePlan = await prepareDecode(call('zipp_model_decode_stage',
  [JSON.stringify(config), JSON.stringify(context),
   JSON.stringify(first), JSON.stringify(last)]), store, hostLimits);
const decodeSession = await runtime.prepare(decodePlan.program);

parentPort.postMessage({ready: true, first, last, caches: decodePlan.resident.length});

parentPort.on('message', async message => {
  const {id, kind} = message;
  try {
    const want = ends ? 'logits' : 'hidden';
    let data;
    if (kind === 'prefill') {
      const graph = await bindGraph(call('zipp_model_prefill_stage',
        [JSON.stringify(config), JSON.stringify(message.tokens),
         JSON.stringify(first), JSON.stringify(last)]),
        store, hostLimits, message.hidden ? {feed: {hidden: message.hidden}} : {});
      const out = await runtime.execute(graph, {typedOutputs: true});
      data = out.outputs[want].data;
    } else {
      const inputs = first === 0
        ? await stepInputs(decodePlan, store, {token: message.token, position: message.position})
        : await stepInputs(decodePlan, store, {position: message.position, hidden: message.hidden});
      const out = await decodeSession.run([inputs], {readback: [want]});
      data = out.outputs[want].data;
    }
    // A copy, because what a graph returns may be a view into memory the next
    // run reuses -- and because the buffer is about to be transferred away.
    const sent = Float32Array.from(data);
    parentPort.postMessage({id, ok: true, [want]: sent}, [sent.buffer]);
  } catch (error) {
    parentPort.postMessage({id, ok: false, error: String(error?.message ?? error)});
  }
});
