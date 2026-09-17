// A quantized weight, read as blocks and never expanded.
//
// The contract is exact, not approximate: a matmul over a Q4_K weight must
// produce bit for bit what the same matmul over that weight's decoded values
// produces. Decoding is integer arithmetic and two half-precision scales, so
// there is nothing to round differently; if these ever disagree, the block
// decoder is wrong, not imprecise.
//
// Ground truth for the decoder itself comes from a second implementation --
// the `ggml-quants` crate, through its WebAssembly build -- because a decoder
// checked only against itself would agree with its own mistakes. That part
// skips unless ZIPP_GGUF_WASM points at the module.
import test from 'node:test';
import assert from 'node:assert/strict';
import {access, readFile} from 'node:fs/promises';
import {createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';

import {createRuntime} from '../src/runtime.mjs';
import {decodeQ4K, decodeQ6K, FORMATS, Q4_K_BLOCK, Q4_K_BYTES, readHalf} from '../src/quant.mjs';

/** Plausible blocks of either format: real scales, every nibble exercised.
 * The f16 scales get modest exponents so the products stay in range; every
 * other byte is arbitrary, which is what makes this a decoder test. */
function blocks(count, seed = 1, dtype = 'q4_k') {
  let state = seed >>> 0;
  const next = () => (state = (Math.imul(state, 1664525) + 1013904223) >>> 0);
  const {bytes: SIZE} = FORMATS[dtype];
  const bytes = new Uint8Array(count * SIZE);
  for (let i = 0; i < count; i++) {
    const at = i * SIZE;
    for (let j = 0; j < SIZE; j++) bytes[at + j] = next() & 0xff;
    if (dtype === 'q4_k') {
      bytes[at + 1] = 0x20 | (next() & 0x07);     // d
      bytes[at + 3] = 0x18 | (next() & 0x07);     // dmin
    } else {
      bytes[at + 209] = 0x20 | (next() & 0x07);   // d, at the block's end
    }
  }
  return bytes;
}

const decoded = (bytes, dtype = 'q4_k') => {
  const {block, bytes: SIZE} = FORMATS[dtype];
  const out = new Float32Array((bytes.length / SIZE) * block);
  (dtype === 'q4_k' ? decodeQ4K : decodeQ6K)(bytes, 0, out.length, out);
  return out;
};

/** Both formats a backend can read without expanding. */
const RESIDENT = ['q4_k', 'q6_k'];

test('a half is read exactly', () => {
  const cases = [[0x00, 0x3c, 1], [0x00, 0xbc, -1], [0x00, 0x00, 0], [0x00, 0x80, -0],
                 [0x00, 0x40, 2], [0x01, 0x00, 2 ** -24]];
  for (const [low, high, want] of cases) {
    assert.equal(readHalf(Uint8Array.from([low, high]), 0), want);
  }
});

test('a quantized matmul equals the same matmul over decoded values', async () => {
  const runtime = await createRuntime({backend: 'cpu-js'});
  try {
    for (const dtype of RESIDENT) for (const [m, k, n] of [[1, 256, 4], [3, 512, 2], [2, 256, 7]]) {
      const weight = blocks((k / 256) * n, m * 31 + k, dtype);
      const values = decoded(weight, dtype);
      assert.equal(values.length, k * n);
      const activations = Float32Array.from({length: m * k}, (_, i) => ((i * 37) % 19) / 16 - 0.5);

      const program = (b, dtype) => ({version: 2, nodes: [
        {id: 0, op: 'input', shape: [m, k], data: activations},
        dtype ? {id: 1, op: 'input', shape: [n, k], dtype, data: b}
              : {id: 1, op: 'input', shape: [n, k], data: b},
        {id: 2, op: 'matmul', a: 0, b: 1, transposed: true},
      ], outputs: [{name: 'out', id: 2}]});

      const quantized = await runtime.execute(program(weight, dtype), {typedOutputs: true});
      const plain = await runtime.execute(program(values), {typedOutputs: true});
      assert.deepEqual(quantized.outputs.out.shape, [m, n]);
      // Bit for bit, not close: same products, same order, same rounding.
      assert.deepEqual([...quantized.outputs.out.data], [...plain.outputs.out.data],
        `${dtype} ${m}x${k}x${n} quantized and decoded results differ`);
    }
  } finally { runtime.dispose(); }
});

test('a transposed matmul equals the untransposed one over the transpose', async () => {
  const runtime = await createRuntime({backend: 'cpu-js'});
  try {
    const [m, k, n] = [3, 8, 5];
    const a = Float32Array.from({length: m * k}, (_, i) => (i % 7) / 3 - 1);
    const rowMajor = Float32Array.from({length: n * k}, (_, i) => (i % 11) / 5 - 1); // [n, k]
    const columnMajor = new Float32Array(k * n);                                     // [k, n]
    for (let r = 0; r < n; r++) for (let c = 0; c < k; c++) columnMajor[c * n + r] = rowMajor[r * k + c];
    const run = (b, shape, transposed) => runtime.execute({version: 2, nodes: [
      {id: 0, op: 'input', shape: [m, k], data: a},
      {id: 1, op: 'input', shape, data: b},
      {id: 2, op: 'matmul', a: 0, b: 1, ...(transposed ? {transposed: true} : {})},
    ], outputs: [{name: 'out', id: 2}]}, {typedOutputs: true});
    const t = await run(rowMajor, [n, k], true);
    const plain = await run(columnMajor, [k, n], false);
    assert.deepEqual([...t.outputs.out.data], [...plain.outputs.out.data]);
  } finally { runtime.dispose(); }
});

test('the protocol refuses what it cannot decode', async () => {
  const runtime = await createRuntime({backend: 'cpu-js'});
  const program = node => ({version: 2, nodes: [
    {id: 0, op: 'input', shape: [1, 256], data: new Float32Array(256)},
    node,
    {id: 2, op: 'matmul', a: 0, b: 1, transposed: true},
  ], outputs: [{name: 'out', id: 2}]});
  try {
    await assert.rejects(runtime.execute(program(
      {id: 1, op: 'input', shape: [1, 256], dtype: 'q8_0', data: blocks(1)})), /Unsupported input dtype/);
    // Rows must be whole blocks: a quantized row cannot be cut in half.
    await assert.rejects(runtime.execute(program(
      {id: 1, op: 'input', shape: [2, 128], dtype: 'q4_k', data: blocks(1)})), /whole blocks/);
    await assert.rejects(runtime.execute(program(
      {id: 1, op: 'input', shape: [1, 256], dtype: 'q4_k', data: blocks(2)})), /needs 144 bytes/);
    // And a quantized weight is stored [N, K]; reading it the other way is not
    // a transpose that can be done to blocks. Square, so the shapes would
    // otherwise be acceptable and the rule is what refuses it.
    await assert.rejects(runtime.execute({version: 2, nodes: [
      {id: 0, op: 'input', shape: [1, 256], data: new Float32Array(256)},
      {id: 1, op: 'input', shape: [256, 256], dtype: 'q4_k', data: blocks(256)},
      {id: 2, op: 'matmul', a: 0, b: 1},
    ], outputs: [{name: 'out', id: 2}]}), /must be transposed/);
  } finally { runtime.dispose(); }
});

// The in-repo build by default (scripts/build_gguf_wasm.sh in model-plugins),
// so this runs rather than skips; the variable overrides it for a build
// elsewhere.
const wasmPath = process.env.ZIPP_GGUF_WASM ??
  fileURLToPath(new URL('../../model-plugins/wasm/gguf-node/zipp_model_wasm.js', import.meta.url));
const haveWasm = await access(wasmPath).then(() => true, () => false);

test('the block decoder agrees with ggml-quants',
  {skip: !haveWasm && 'Build it: model-plugins/scripts/build_gguf_wasm.sh'}, async () => {
  const require = createRequire(import.meta.url);
  const gguf = require(wasmPath);
  // Many seeds, not a few: a rounding difference in the decoder shows up in the
  // last bit of a minority of values, so a handful of blocks can agree by luck.
  for (let seed = 1; seed <= 60; seed++) {
    for (const dtype of RESIDENT) {
      const blocked = blocks(3, seed * 2654435761 % 2 ** 31, dtype);
      const theirs = gguf.dequantize(dtype.toUpperCase(), blocked, 3 * 256);
      const ours = decoded(blocked, dtype);
      assert.equal(ours.length, theirs.length);
      // Two implementations of the same exact arithmetic: equal, not close.
      assert.deepEqual([...ours], [...theirs], `${dtype} seed ${seed} decodes differently`);
    }
  }
});

test('the compiled kernels agree with the reference, bit for bit', async () => {
  // The contract the whole backend is held to (see backend-bits.test.mjs)
  // extended to the two matmul shapes a checkpoint needs: a transposed f32
  // weight, and a weight that stays quantized. SIMD across four columns keeps
  // each output's k products in index order, which is what makes this exact
  // rather than close.
  const wasmBytes = await readFile(new URL('../wasm/kernels.wasm', import.meta.url));
  const cpu = await createRuntime({backend: 'cpu-js'});
  const wasm = await createRuntime({backend: 'wasm', wasmBytes});
  try {
    // Sizes either side of the four-column block and the 256-value block, so
    // the vector path, the scalar remainder and multi-block rows all run.
    for (const dtype of RESIDENT)
    for (const [m, k, n] of [[1, 256, 4], [1, 256, 7], [3, 512, 8], [2, 768, 5], [5, 256, 1], [4, 512, 13]]) {
      const weight = blocks((k / 256) * n, m * 131 + k + n, dtype);
      const values = decoded(weight, dtype);
      const activations = Float32Array.from({length: m * k}, (_, i) => ((i * 37) % 19) / 16 - 0.5);
      const program = (b, dtype) => ({version: 2, nodes: [
        {id: 0, op: 'input', shape: [m, k], data: activations},
        dtype ? {id: 1, op: 'input', shape: [n, k], dtype, data: b}
              : {id: 1, op: 'input', shape: [n, k], data: b},
        {id: 2, op: 'matmul', a: 0, b: 1, transposed: true},
      ], outputs: [{name: 'out', id: 2}]});
      const run = (r, ...args) => r.execute(program(...args), {typedOutputs: true});

      // One at a time: a runtime executes serially and says so.
      const cq = await run(cpu, weight, dtype), cf = await run(cpu, values);
      const wq = await run(wasm, weight, dtype), wf = await run(wasm, values);
      const bits = x => [...x.outputs.out.data];
      const what = `${dtype} ${m}x${k}x${n}`;
      assert.deepEqual(bits(wf), bits(cf), `${what}: transposed f32 wasm differs from cpu-js`);
      assert.deepEqual(bits(wq), bits(cq), `${what}: quantized wasm differs from cpu-js`);
      // And on each backend, quantized equals the same matmul over decoded values.
      assert.deepEqual(bits(cq), bits(cf), `${what}: cpu-js quantized differs from decoded`);
      assert.deepEqual(bits(wq), bits(wf), `${what}: wasm quantized differs from decoded`);
    }

    // Batched, where the block offset of batch t is bBatchStride/256 rather
    // than zero. Every backend computes that offset for itself, and nothing
    // above reaches the arithmetic that does it.
    for (const dtype of RESIDENT) for (const [batch, m, k, n] of [[2, 1, 256, 4], [3, 2, 512, 5]]) {
      const weight = blocks(batch * (k / 256) * n, batch * 17 + k, dtype);
      const values = decoded(weight, dtype);
      const activations = Float32Array.from({length: batch * m * k}, (_, i) => ((i * 23) % 13) / 8 - 0.5);
      const program = (b, dtype) => ({version: 2, nodes: [
        {id: 0, op: 'input', shape: [batch, m, k], data: activations},
        dtype ? {id: 1, op: 'input', shape: [batch, n, k], dtype, data: b}
              : {id: 1, op: 'input', shape: [batch, n, k], data: b},
        {id: 2, op: 'matmul', a: 0, b: 1, transposed: true},
      ], outputs: [{name: 'out', id: 2}]});
      const run = (r, ...args) => r.execute(program(...args), {typedOutputs: true});
      const cq = await run(cpu, weight, dtype), cf = await run(cpu, values);
      const wq = await run(wasm, weight, dtype), wf = await run(wasm, values);
      const bits = x => [...x.outputs.out.data];
      assert.deepEqual(cq.outputs.out.shape, [batch, m, n]);
      assert.deepEqual(bits(cq), bits(cf), `${dtype} batch ${batch}: cpu-js quantized differs from decoded`);
      assert.deepEqual(bits(wq), bits(cq), `${dtype} batch ${batch}: wasm quantized differs from cpu-js`);
      assert.deepEqual(bits(wf), bits(cf), `${dtype} batch ${batch}: wasm transposed differs from cpu-js`);
    }
  } finally { cpu.dispose(); wasm.dispose(); }
});

test('a prepared session holds a quantized weight as blocks', async () => {
  const runtime = await createRuntime({backend: 'cpu-js'});
  let quantized = null, plain = null;
  try {
    // One weight, two ways: the same values, stored as blocks and as floats.
    const [k, n] = [512, 64];
    const weight = blocks((k / Q4_K_BLOCK) * n, 99);
    const values = decoded(weight);
    const program = (b, dtype) => ({version: 2, nodes: [
      {id: 0, op: 'input', shape: [1, k]},
      dtype ? {id: 1, op: 'input', shape: [n, k], dtype, data: b}
            : {id: 1, op: 'input', shape: [n, k], data: b},
      {id: 2, op: 'matmul', a: 0, b: 1, transposed: true},
    ], outputs: [{name: 'out', id: 2}]});
    quantized = await runtime.prepare(program(weight, 'q4_k'));
    plain = await runtime.prepare(program(values));
    // 144 bytes per 256 values against 1024: the weight is 7.1 times smaller
    // on the device, and that ratio is the answer to whether a model fits.
    assert.equal(quantized.residentBytes, weight.length);
    assert.equal(plain.residentBytes, values.length * 4);
    assert.ok(plain.residentBytes / quantized.residentBytes > 7);

    // And it still computes the same thing, step after step, with the weight
    // uploaded once and never expanded.
    const activations = Float32Array.from({length: k}, (_, i) => ((i * 13) % 17) / 8 - 1);
    for (let step = 0; step < 3; step++) {
      const q = await quantized.run([{inputs: {0: activations}}], {readback: ['out']});
      const p = await plain.run([{inputs: {0: activations}}], {readback: ['out']});
      assert.deepEqual([...q.outputs.out.data], [...p.outputs.out.data], `step ${step}`);
      assert.equal(q.stats.uploadElements, k, 'only the activations cross per step');
    }
  } finally { quantized?.dispose(); plain?.dispose(); runtime.dispose(); }
});
