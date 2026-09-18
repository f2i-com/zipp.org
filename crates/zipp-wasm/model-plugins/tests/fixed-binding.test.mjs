// Binding a weight for `matmul_fixed`.
//
// That operation has two forms. The weight can arrive in the checkpoint's own
// format and be decoded and quantized on every step, or it can be quantized
// once when the graph is bound and arrive as `i16` quants with their per-row
// scales. They are the same arithmetic at different times and must produce the
// same float32 bits; this checks that through a real checkpoint and a real
// compute runtime, which is where a mismatch between the two packages' ideas of
// a shape or a layout would show up.
//
// The quantizer is handed in rather than imported. This package does not depend
// on the compute runtime -- the caller supplies that too -- and a second copy of
// a quantisation that four backends have to agree on bit for bit is exactly the
// thing not to have.
import test from 'node:test';
import assert from 'node:assert/strict';
import {open, stat, access} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';

import {openGGUF, WeightStore, resolveLimits, bindGraph} from '../src/index.mjs';

const modelPath = process.env.ZIPP_QWEN3_MODEL
  ?? fileURLToPath(new URL('fixtures/tiny-qwen3.gguf', import.meta.url));
const runtimeURL = new URL('../../gpu-lab/src/runtime.mjs', import.meta.url);
const quantURL = new URL('../../gpu-lab/src/quant.mjs', import.meta.url);
const kernelsURL = new URL('../../gpu-lab/wasm/kernels.wasm', import.meta.url);
const exists = async path => { try { await access(path); return true; } catch { return false; } };
const ready = await exists(modelPath) && await exists(runtimeURL) && await exists(quantURL);
const reason = 'Needs tests/fixtures/tiny-qwen3.gguf and the gpu-lab sibling package';

const GB = 1024 * 1024 * 1024;
const hostLimits = resolveLimits({
  maxModelFileBytes: 8 * GB, maxModelBytes: 8 * GB, maxDecodedBytes: GB,
  maxBoundInputBytes: GB, maxTensorElements: 2 ** 28, maxDimension: 1 << 21, maxTensors: 4096,
});
const computeLimits = {maxNodes: 64, maxElements: 4194304, maxInputElements: 1 << 24,
  maxOutputElements: 1 << 20, maxLogicalBytes: GB, maxWork: 2 ** 40};

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

test('a weight bound for matmul_fixed gives the same bits either way',
  {skip: !ready && reason}, async t => {
  const {createRuntime} = await import(runtimeURL);
  const {quantizeWeight} = await import(quantURL);
  const source = await fileSource(modelPath);
  const index = await openGGUF(source, 'model.gguf', null, hostLimits);
  const store = new WeightStore([index], hostLimits);
  try {
    // A matrix small enough to multiply here, preferring one the file keeps
    // quantized so both source paths of the binding get exercised.
    const matrices = [...index.tensors.values()]
      .filter(t => t.shape.length === 2 && t.elements <= (1 << 20) && t.readable)
      .sort((a, b) => a.elements - b.elements);
    // One of every block format the file has, so both dtypes reach
    // `quantizeWeight` rather than only whichever happens to be smallest.
    const perDtype = new Map();
    for (const t of matrices) if (!perDtype.has(t.dtype) && store.residentDtype(t.name)) perDtype.set(t.dtype, t);
    const quantized = [...perDtype.values()][0];
    assert.ok(quantized, 'the fixture has no usable quantized matrix');

    const m = 2;
    /** The run-time form: the weight in the file's own format. */
    const runTimeTemplate = (weight, [n, k], kind) => ({
      version: 1,
      graph: {version: 2, nodes: [
        {id: 0, op: 'input', shape: [m, k], data: Array.from({length: m * k}, (_, i) => ((i * 37) % 19) / 16 - 0.5)},
        {id: 1, op: 'input', shape: [n, k]},
        {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true},
      ], outputs: [{name: 'logits', id: 2}]},
      bindings: [{node: 1, kind, tensor: weight}],
    });
    /** The bind-time form: quants and scales, two bindings over one tensor. */
    const bindTimeTemplate = (weight, [n, k]) => ({
      version: 1,
      graph: {version: 2, nodes: [
        {id: 0, op: 'input', shape: [m, k], data: Array.from({length: m * k}, (_, i) => ((i * 37) % 19) / 16 - 0.5)},
        {id: 1, op: 'input', shape: [n, k]},
        {id: 2, op: 'input', shape: [n]},
        {id: 3, op: 'matmul_fixed', a: 0, b: 1, c: 2, transposed: true},
      ], outputs: [{name: 'logits', id: 3}]},
      bindings: [{node: 1, kind: 'fixed', tensor: weight}, {node: 2, kind: 'fixed_scales', tensor: weight}],
    });

    const kernels = await exists(kernelsURL) ? await (await import('node:fs/promises')).readFile(kernelsURL) : null;
    const backends = ['cpu-js', ...(kernels ? ['wasm'] : [])];

    // A store that keeps nothing resident, which is what a Safetensors file is:
    // the binding then decodes the weight and quantizes the values, instead of
    // reading blocks and quantizing what they decode to. Both have to reach the
    // same quants, because both quantize the same values.
    const decoding = Object.create(store);
    decoding.residentDtype = () => null;
    decoding.info = name => store.info(name);
    decoding.tensor = name => store.tensor(name);

    for (const [label, tensor, from] of [...perDtype.values()].flatMap(t =>
      [[`${t.dtype} blocks`, t, store], [`${t.dtype} decoded first`, t, decoding]])) {
      await t.test(`${label}: ${tensor.name}`, async () => {
        const shape = store.info(tensor.name).shape, [n, k] = shape;
        const kind = from.residentDtype(tensor.name) === null ? 'tensor' : 'blocks';
        const runTime = await bindGraph(runTimeTemplate(tensor.name, shape, kind), from, hostLimits);
        const bindTime = await bindGraph(bindTimeTemplate(tensor.name, shape), from, hostLimits,
          {quantize: quantizeWeight});

        assert.equal(bindTime.nodes[1].dtype, 'i16');
        assert.ok(bindTime.nodes[1].data instanceof Uint8Array);
        assert.equal(bindTime.nodes[1].data.length, n * k * 2, 'two bytes a value');
        assert.ok(bindTime.nodes[2].data instanceof Float32Array);
        assert.equal(bindTime.nodes[2].data.length, n, 'one scale an output column');

        for (const backend of backends) {
          const runtime = await createRuntime({backend, wasmBytes: kernels ?? undefined, limits: computeLimits});
          try {
            const x = (await runtime.execute(runTime, {typedOutputs: true})).outputs.logits.data;
            const y = (await runtime.execute(bindTime, {typedOutputs: true})).outputs.logits.data;
            assert.deepEqual([...x].length, m * n);
            for (let i = 0; i < x.length; i++) assert.ok(Object.is(x[i], y[i]),
              `${backend} ${label}[${i}]: run time ${x[i]} against bind time ${y[i]}`);
          } finally { runtime.dispose(); }
        }
      });
    }

    await t.test('the decode path binds it the same way, which is where it pays', async () => {
      // A decode step is one token, so quantising the weight per step costs
      // several times what the product itself does. `prepareDecode` is the path
      // a hosted stage actually runs, and it takes the same two bindings.
      const {prepareDecode} = await import('../src/index.mjs');
      const tensor = quantized, [n, k] = store.info(tensor.name).shape;
      const activation = Array.from({length: k}, (_, i) => ((i * 23) % 17) / 32 - 0.25);
      const decodeTemplate = weight => ({
        version: 1, kind: 'decode', context: 8,
        graph: {version: 2, nodes: [{id: 0, op: 'input', shape: [1, k], data: activation}, ...weight],
          outputs: [{name: 'logits', id: weight[weight.length - 1].id}]},
        bindings: weight.filter(node => !Object.hasOwn(node, 'data'))
          .map(node => node.op === 'input'
            ? {node: node.id, kind: node.shape.length === 1 ? 'fixed_scales' : (weight.length === 2 ? 'blocks' : 'fixed'), tensor: tensor.name}
            : null)
          .filter(Boolean),
      });
      const runTime = await prepareDecode(decodeTemplate([
        {id: 1, op: 'input', shape: [n, k]},
        {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true}]), store, hostLimits);
      const bindTime = await prepareDecode(decodeTemplate([
        {id: 1, op: 'input', shape: [n, k]},
        {id: 2, op: 'input', shape: [n]},
        {id: 3, op: 'matmul_fixed', a: 0, b: 1, c: 2, transposed: true}]), store, hostLimits,
        {quantize: quantizeWeight});
      assert.equal(bindTime.program.nodes[1].dtype, 'i16');
      assert.equal(bindTime.program.nodes[1].data.length, n * k * 2);
      assert.equal(bindTime.program.nodes[2].data.length, n);

      const runtime = await createRuntime({backend: 'cpu-js', limits: computeLimits});
      try {
        const x = (await runtime.execute(runTime.program, {typedOutputs: true})).outputs.logits.data;
        const y = (await runtime.execute(bindTime.program, {typedOutputs: true})).outputs.logits.data;
        for (let i = 0; i < x.length; i++) assert.ok(Object.is(x[i], y[i]),
          `decode[${i}]: run time ${x[i]} against bind time ${y[i]}`);
      } finally { runtime.dispose(); }

      await assert.rejects(prepareDecode(decodeTemplate([
        {id: 1, op: 'input', shape: [n, k]},
        {id: 2, op: 'input', shape: [n]},
        {id: 3, op: 'matmul_fixed', a: 0, b: 1, c: 2, transposed: true}]), store, hostLimits),
        /pass \{quantize\}/);
    });

    await t.test('one tensor, two bindings, read and quantized once', async () => {
      const tensor = quantized ?? plain;
      const shape = store.info(tensor.name).shape;
      // A store that counts what the binding asks it for. Quantising is the
      // expensive half of binding a large checkpoint; doing it twice because
      // the quants and the scales are two graph inputs would be a real cost.
      let reads = 0;
      const counting = Object.create(store);
      counting.info = name => store.info(name);
      counting.residentDtype = name => store.residentDtype(name);
      counting.blocks = async name => { reads++; return store.blocks(name); };
      counting.tensor = async name => { reads++; return store.tensor(name); };
      await bindGraph(bindTimeTemplate(tensor.name, shape), counting, hostLimits, {quantize: quantizeWeight});
      assert.equal(reads, 1, `the weight was read ${reads} times for two bindings over it`);
    });

    await t.test('refusals: the quantizer, the scale shape, and the byte budget', async () => {
      const tensor = quantized ?? plain;
      const shape = store.info(tensor.name).shape, [n, k] = shape;
      // Without the injected quantizer the binding says so, rather than failing
      // somewhere inside on an undefined call.
      await assert.rejects(bindGraph(bindTimeTemplate(tensor.name, shape), store, hostLimits),
        /pass \{quantize\}/);
      // Scales are one per output column, and a shape that is merely plausible
      // is still refused here rather than at the compute runtime.
      const wrong = bindTimeTemplate(tensor.name, shape);
      wrong.graph.nodes[2] = {id: 2, op: 'input', shape: [n + 1]};
      await assert.rejects(bindGraph(wrong, store, hostLimits, {quantize: quantizeWeight}),
        new RegExp(`its scales are \\[${n}\\]`));
      // Two bytes a value is the trade, and it is charged as two bytes a value:
      // a budget that admits the weight as blocks need not admit it as quants.
      const tight = {...hostLimits, maxBoundInputBytes: n * k * 2 - 1};
      await assert.rejects(bindGraph(bindTimeTemplate(tensor.name, shape), store, tight,
        {quantize: quantizeWeight}), /Bound graph inputs exceed budget/);
      // And the same weight as blocks fits in a budget the quants do not, which
      // is the whole shape of the decision.
      if (store.residentDtype(tensor.name) !== null) {
        await bindGraph(runTimeTemplate(tensor.name, shape, 'blocks'), store, tight);
      }
    });
  } finally { store.dispose(); await source.close(); }
});
