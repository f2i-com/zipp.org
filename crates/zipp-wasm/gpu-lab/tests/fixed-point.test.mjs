// `matmul_fixed`: a product accumulated in integers rather than in float32.
//
// Every other operation in this protocol is bit-for-bit across backends by
// *agreement* -- each one rounds float32 in the same order, and the tests that
// hold them there are in backend-bits.test.mjs. This operation is bit-for-bit
// by *construction*: integer addition is associative, so once two backends
// agree on the quants, no ordering, vectorisation or scheduling can make their
// sums differ. That is a stronger guarantee and it is the point, because a
// STARK or sumcheck argument works over a prime field, and a prime field can
// express an integer sum but not IEEE-754 rounding.
//
// So the tests here are of two kinds. The first assert exactness -- against a
// BigInt evaluation that shares no accumulator with either backend, and against
// each other bit for bit. The second measure the price: what accumulating in
// integers costs against the float32 answer, which is the number that decides
// whether this is usable at all.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createRuntime, ComputeRuntime} from '../src/runtime.mjs';
import {quantizeRow, FIXED_QMAX} from '../src/kernel-math.mjs';
import {FORMATS, decodeQ4K, decodeQ6K, quantizeWeight, readFixedQuants} from '../src/quant.mjs';
const wasmBytes = await readFile(new URL('../wasm/kernels.wasm', import.meta.url));

const rng = (seed = 1) => {
  let state = seed >>> 0;
  return () => ((state = (Math.imul(state, 1664525) + 1013904223) >>> 0) >>> 8) / 16777216;
};
/** Normal-ish values with a few large outliers, which is what a residual
 * stream looks like and is the whole difficulty of quantizing activations. */
function activations(n, seed, outliers = 0) {
  const r = rng(seed), out = new Float32Array(n);
  for (let i = 0; i < n; i++) {
    let s = 0;
    for (let j = 0; j < 12; j++) s += r();
    out[i] = s - 6;
  }
  for (let i = 0; i < outliers; i++) out[Math.floor(r() * n)] *= 30;
  return out;
}
/** Plausible K-quant blocks: modest f16 scales, arbitrary everything else. */
function blocks(count, seed = 1, dtype = 'q4_k') {
  const r = rng(seed), byte = () => Math.floor(r() * 256) & 0xff;
  const {bytes: SIZE} = FORMATS[dtype];
  const bytes = new Uint8Array(count * SIZE);
  for (let i = 0; i < count; i++) {
    const at = i * SIZE;
    for (let j = 0; j < SIZE; j++) bytes[at + j] = byte();
    if (dtype === 'q4_k') { bytes[at + 1] = 0x20 | (byte() & 7); bytes[at + 3] = 0x18 | (byte() & 7); }
    else bytes[at + 209] = 0x20 | (byte() & 7);
  }
  return bytes;
}
const decoded = (bytes, dtype) => {
  const {block, bytes: SIZE} = FORMATS[dtype];
  const out = new Float32Array((bytes.length / SIZE) * block);
  (dtype === 'q4_k' ? decodeQ4K : decodeQ6K)(bytes, 0, out.length, out);
  return out;
};

const fixedGraph = (a, b, [m, k], [n]) => ({
  version: 2,
  nodes: [
    {id: 0, op: 'input', shape: [m, k], data: Array.from(a)},
    {id: 1, op: 'input', shape: [n, k], data: Array.from(b)},
    {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true},
    {id: 3, op: 'matmul', a: 0, b: 1, transposed: true},
  ],
  outputs: [{name: 'fixed', id: 2}, {name: 'float', id: 3}],
});

/**
 * The same product, evaluated with BigInt.
 *
 * This shares no accumulator with either backend -- not a double, not an i64 --
 * so it is a real second opinion on the claim that the sum loses nothing. It
 * does use `quantizeRow`, because the quantisation is the definition of the
 * operation rather than part of what is under test here; what is under test is
 * that summing k products of two int16s is exact.
 */
function bigintReference(a, b, m, k, n) {
  const qa = new Int16Array(m * k), sa = new Float32Array(m);
  for (let r = 0; r < m; r++) sa[r] = quantizeRow(a, r * k, k, qa, r * k);
  const out = new Float32Array(m * n), qw = new Int16Array(k);
  for (let c = 0; c < n; c++) {
    const sw = quantizeRow(b, c * k, k, qw, 0);
    for (let r = 0; r < m; r++) {
      let acc = 0n;
      for (let j = 0; j < k; j++) acc += BigInt(qa[r * k + j]) * BigInt(qw[j]);
      out[r * n + c] = Math.fround(Math.fround(Number(acc)) * Math.fround(sa[r] * sw));
    }
  }
  return out;
}

const runtimes = async () => ({
  cpu: await createRuntime({backend: 'cpu-js'}),
  wasm: await createRuntime({backend: 'wasm', wasmBytes}),
});

test('cpu-js and wasm reach the same integer, so the same bits, on plain weights', async () => {
  const {cpu, wasm} = await runtimes();
  try {
    let compared = 0;
    for (const [m, k, n] of [[1, 256, 512], [4, 512, 24], [3, 1024, 64], [8, 256, 33], [1, 2048, 1024]]) {
      const a = activations(m * k, m + k, 3), b = activations(n * k, n * k, 0);
      const program = fixedGraph(a, b, [m, k], [n]);
      const x = (await cpu.execute(program, {typedOutputs: true})).outputs.fixed.data;
      const y = (await wasm.execute(program, {typedOutputs: true})).outputs.fixed.data;
      for (let i = 0; i < x.length; i++) {
        compared++;
        assert.ok(Object.is(x[i], y[i]), `${m}x${k}x${n}[${i}]: cpu-js ${x[i]} versus wasm ${y[i]}`);
      }
    }
    assert.ok(compared > 2000, `compared ${compared}`);
  } finally { cpu.dispose(); wasm.dispose(); }
});

test('the accumulator is exact: both backends match a BigInt evaluation', async () => {
  const {cpu, wasm} = await runtimes();
  try {
    // k = 2048 with the largest quants either side reaches 2^41, far past what
    // a float32 accumulator could hold and still well inside a double.
    const [m, k, n] = [3, 2048, 12];
    const a = activations(m * k, 7, 5), b = activations(n * k, 9, 0);
    const want = bigintReference(a, b, m, k, n);
    const program = fixedGraph(a, b, [m, k], [n]);
    for (const [name, rt] of [['cpu-js', cpu], ['wasm', wasm]]) {
      const got = (await rt.execute(program, {typedOutputs: true})).outputs.fixed.data;
      for (let i = 0; i < want.length; i++) {
        assert.ok(Object.is(got[i], want[i]), `${name}[${i}]: ${got[i]} versus BigInt ${want[i]}`);
      }
    }
  } finally { cpu.dispose(); wasm.dispose(); }
});

test('a quantized weight is requantized from what it decodes to, alike on both backends', async () => {
  const {cpu, wasm} = await runtimes();
  try {
    for (const dtype of ['q4_k', 'q6_k']) {
      const [m, k, n] = [2, 512, 6];
      const bytes = blocks((k / 256) * n, 5, dtype);
      const a = activations(m * k, 3, 2);
      const program = {
        version: 2,
        nodes: [
          {id: 0, op: 'input', shape: [m, k], data: Array.from(a)},
          {id: 1, op: 'input', shape: [n, k], dtype, data: bytes},
          {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true},
        ],
        outputs: [{name: 'fixed', id: 2}],
      };
      const x = (await cpu.execute(program, {typedOutputs: true})).outputs.fixed.data;
      const y = (await wasm.execute(program, {typedOutputs: true})).outputs.fixed.data;
      for (let i = 0; i < x.length; i++) assert.ok(Object.is(x[i], y[i]), `${dtype}[${i}]: ${x[i]} / ${y[i]}`);
      // And that the blocks stand for exactly the values they decode to: the
      // same product over the decoded weight has to give the same answer.
      const expanded = {...program, nodes: [program.nodes[0],
        {id: 1, op: 'input', shape: [n, k], data: Array.from(decoded(bytes, dtype))}, program.nodes[2]]};
      const z = (await cpu.execute(expanded, {typedOutputs: true})).outputs.fixed.data;
      for (let i = 0; i < x.length; i++) assert.ok(Object.is(x[i], z[i]), `${dtype} decoded[${i}]: ${x[i]} / ${z[i]}`);
    }
  } finally { cpu.dispose(); wasm.dispose(); }
});

test('batched, and with one side broadcast, still bit-identical', async () => {
  const {cpu, wasm} = await runtimes();
  try {
    const [batch, m, k, n] = [3, 2, 256, 5];
    const a = activations(batch * m * k, 21, 4);
    for (const bShape of [[batch, n, k], [n, k]]) {
      const bSize = bShape.reduce((x, y) => x * y, 1);
      const program = {
        version: 2,
        nodes: [
          {id: 0, op: 'input', shape: [batch, m, k], data: Array.from(a)},
          {id: 1, op: 'input', shape: bShape, data: Array.from(activations(bSize, 22, 0))},
          {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true},
        ],
        outputs: [{name: 'fixed', id: 2}],
      };
      const x = (await cpu.execute(program, {typedOutputs: true})).outputs.fixed.data;
      const y = (await wasm.execute(program, {typedOutputs: true})).outputs.fixed.data;
      assert.equal(x.length, batch * m * n);
      for (let i = 0; i < x.length; i++) assert.ok(Object.is(x[i], y[i]), `${bShape}[${i}]: ${x[i]} / ${y[i]}`);
    }
  } finally { cpu.dispose(); wasm.dispose(); }
});

/** The same product with the weight quantized once beforehand instead of on
 * every step: `b` becomes an `i16` input and its row scales a float32 [N]. */
const boundGraph = (a, quants, scales, [m, k], [n]) => ({
  version: 2,
  nodes: [
    {id: 0, op: 'input', shape: [m, k], data: Array.from(a)},
    {id: 1, op: 'input', shape: [n, k], dtype: 'i16', data: quants},
    {id: 2, op: 'input', shape: [n], data: Array.from(scales)},
    {id: 3, op: 'matmul_fixed', a: 0, b: 1, c: 2, transposed: true},
  ],
  outputs: [{name: 'fixed', id: 3}],
});

test('the bind-time form is the run-time form moved in time, not a second answer', async () => {
  // The claim that makes `i16` worth having at all. Quantising the weight when
  // the graph is built, rather than on every step, has to reach the identical
  // float32 bits -- otherwise it is a different operation wearing the same name,
  // and a checker comparing two peers could not tell which form either ran.
  const {cpu, wasm} = await runtimes();
  try {
    const [m, k, n] = [3, 512, 17];
    const a = activations(m * k, 61, 4);
    const weights = [
      ['float32', activations(n * k, 62, 0), 'f32'],
      ['q4_k', blocks((k / 256) * n, 63, 'q4_k'), 'q4_k'],
      ['q6_k', blocks((k / 256) * n, 64, 'q6_k'), 'q6_k'],
    ];
    for (const [label, source, dtype] of weights) {
      const {quants, scales} = quantizeWeight(source, n, k, dtype);
      assert.equal(quants.length, n * k * 2, `${label}: two bytes a value`);
      assert.equal(scales.length, n, `${label}: one scale an output column`);
      const runTime = dtype === 'f32'
        ? fixedGraph(a, source, [m, k], [n])
        : {version: 2, nodes: [
            {id: 0, op: 'input', shape: [m, k], data: Array.from(a)},
            {id: 1, op: 'input', shape: [n, k], dtype, data: source},
            {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true}],
           outputs: [{name: 'fixed', id: 2}]};
      const bindTime = boundGraph(a, quants, scales, [m, k], [n]);
      for (const [name, rt] of [['cpu-js', cpu], ['wasm', wasm]]) {
        const x = (await rt.execute(runTime, {typedOutputs: true})).outputs.fixed.data;
        const y = (await rt.execute(bindTime, {typedOutputs: true})).outputs.fixed.data;
        for (let i = 0; i < x.length; i++) assert.ok(Object.is(x[i], y[i]),
          `${label} on ${name}[${i}]: run-time ${x[i]} against bind-time ${y[i]}`);
      }
    }
  } finally { cpu.dispose(); wasm.dispose(); }
});

test('a batch of activations against one bind-time weight', async () => {
  // The only path that exercises the activation batch stride against an i16
  // weight, whose own batch is fixed at one because its scales are [N]. It is
  // the shape a prefill takes, and it had no test until it had a bug to have.
  const {cpu, wasm} = await runtimes();
  try {
    const [batch, m, k, n] = [2, 2, 256, 5];
    const a = activations(batch * m * k, 91, 3), source = activations(n * k, 92, 0);
    const {quants, scales} = quantizeWeight(source, n, k);
    const graph = weight => ({version: 2, nodes: [
      {id: 0, op: 'input', shape: [batch, m, k], data: Array.from(a)}, ...weight],
      outputs: [{name: 'fixed', id: weight[weight.length - 1].id}]});
    const runTime = graph([{id: 1, op: 'input', shape: [n, k], data: Array.from(source)},
      {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true}]);
    const bindTime = graph([{id: 1, op: 'input', shape: [n, k], dtype: 'i16', data: quants},
      {id: 2, op: 'input', shape: [n], data: Array.from(scales)},
      {id: 3, op: 'matmul_fixed', a: 0, b: 1, c: 2, transposed: true}]);
    const want = (await cpu.execute(runTime, {typedOutputs: true})).outputs.fixed.data;
    assert.equal(want.length, batch * m * n);
    for (const [name, rt] of [['cpu-js', cpu], ['wasm', wasm]]) {
      for (const [form, program] of [['run-time', runTime], ['bind-time', bindTime]]) {
        const got = (await rt.execute(program, {typedOutputs: true})).outputs.fixed.data;
        for (let i = 0; i < want.length; i++) assert.ok(Object.is(got[i], want[i]),
          `${name} ${form}[${i}]: ${got[i]} against ${want[i]}`);
      }
    }
    // A batched i16 weight has no meaning without batched scales, and is refused
    // rather than read as though the batch were not there.
    await assert.rejects(() => cpu.execute(graph([
      {id: 1, op: 'input', shape: [batch, n, k], dtype: 'i16', data: new Uint8Array(batch * n * k * 2)},
      {id: 2, op: 'input', shape: [n], data: Array.from(scales)},
      {id: 3, op: 'matmul_fixed', a: 0, b: 1, c: 2, transposed: true}]), {typedOutputs: true}),
      e => { assert.equal(e.code, 'SHAPE'); assert.match(e.message, /batched i16 weight/); return true; });
  } finally { cpu.dispose(); wasm.dispose(); }
});

test('quantizeWeight refuses a weight that is not the shape it was told', () => {
  // It reads by computed offset, so a mismatched length reads past the end and
  // quantizes `undefined` to NaN -- a plausible weight of nothing. The graph
  // validator would refuse the result's length later, naming the wrong thing.
  const [n, k] = [4, 256];
  assert.throws(() => quantizeWeight(new Float32Array(n * k - 1), n, k), /is 1024 values; got 1023/);
  assert.throws(() => quantizeWeight(blocks(n, 1, 'q4_k').slice(0, 100), n, k, 'q4_k'), /is 576 bytes; got 100/);
  assert.throws(() => quantizeWeight(new Float32Array(n * k), n, k, 'q8_0'), /unknown dtype q8_0/);
  // And the shape it was told is the one it produces.
  const {quants, scales} = quantizeWeight(new Float32Array(n * k).fill(0.5), n, k);
  assert.equal(quants.length, n * k * 2);
  assert.equal(scales.length, n);
});

test('i16 quants are little-endian, so both backends read the same weight', async () => {
  // wasm memory is little-endian and reads these natively; the JavaScript
  // reference decodes the bytes explicitly rather than laying an Int16Array
  // over them, so a big-endian host would still agree. This checks the bytes
  // are what that says they are, negatives included.
  const w = Float32Array.from([1, -1, 0.5, -0.5]);
  const {quants} = quantizeWeight(w, 1, 4);
  assert.deepEqual([...readFixedQuants(quants, 4)], [32767, -32767, 16384, -16383]);
  assert.deepEqual([...quants.slice(0, 4)], [0xff, 0x7f, 0x01, 0x80], 'low byte first');
});

test('a bind-time weight survives a prepared session across steps', async () => {
  // The case the format exists for: the weight is uploaded once at prepare and
  // held, and every step reads it without decoding or requantising anything.
  const {cpu, wasm} = await runtimes();
  try {
    const [k, n] = [256, 6];
    const source = activations(n * k, 71, 0);
    const {quants, scales} = quantizeWeight(source, n, k);
    const program = {
      version: 2,
      nodes: [
        {id: 0, op: 'input', shape: [1, k]},
        {id: 1, op: 'input', shape: [n, k], dtype: 'i16', data: quants},
        {id: 2, op: 'input', shape: [n], data: Array.from(scales)},
        {id: 3, op: 'matmul_fixed', a: 0, b: 1, c: 2, transposed: true},
      ],
      outputs: [{name: 'fixed', id: 3}],
    };
    const steps = [activations(k, 72, 2), activations(k, 73, 3), activations(k, 72, 2)];
    for (const [name, rt] of [['cpu-js', cpu], ['wasm', wasm]]) {
      const session = await rt.prepare(program, {});
      try {
        const seen = [];
        for (const input of steps) seen.push((await session.run({inputs: {0: input}})).steps[0].outputs.fixed.data);
        // The third step feeds the first step's activations again: a held
        // weight that had been consumed or overwritten would not repeat.
        for (let i = 0; i < seen[0].length; i++) assert.ok(Object.is(seen[0][i], seen[2][i]),
          `${name}: step 3 repeats step 1 at [${i}]: ${seen[0][i]} against ${seen[2][i]}`);
        assert.notEqual(seen[1][0], seen[0][0], `${name}: the steps are not all the same`);
      } finally { session.dispose(); }
    }
  } finally { cpu.dispose(); wasm.dispose(); }
});

test('i16 costs what it costs, and the plan says so', async () => {
  // 3.56x Q4_K on the device, which is the whole trade and should be visible in
  // the graph's own accounting rather than discovered when a device runs out.
  const {validateProgram} = await import('../src/graph.mjs');
  const [k, n] = [256, 64];
  const bytesOf = nodes => validateProgram({version: 2, nodes,
    outputs: [{name: 'o', id: nodes.length - 1}]}).logicalBytes;
  const a = {id: 0, op: 'input', shape: [1, k], data: new Array(k).fill(0.5)};
  const asBlocks = bytesOf([a, {id: 1, op: 'input', shape: [n, k], dtype: 'q4_k', data: blocks((k / 256) * n, 1)},
    {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true}]);
  const {quants, scales} = quantizeWeight(activations(n * k, 2, 0), n, k);
  const asFixed = bytesOf([a, {id: 1, op: 'input', shape: [n, k], dtype: 'i16', data: quants},
    {id: 2, op: 'input', shape: [n], data: Array.from(scales)},
    {id: 3, op: 'matmul_fixed', a: 0, b: 1, c: 2, transposed: true}]);
  const weightBlocks = (k / 256) * n * 144, weightFixed = n * k * 2 + n * 4;
  assert.equal(asFixed - asBlocks, weightFixed - weightBlocks, 'the difference is the weight, counted as what it is');
  assert.ok(weightFixed / weightBlocks > 3.5 && weightFixed / weightBlocks < 3.6,
    `i16 is ${(weightFixed / weightBlocks).toFixed(2)}x Q4_K`);
});

test('validation: i16 belongs to matmul_fixed, and only with its scales', async () => {
  const rt = await createRuntime({backend: 'cpu-js'});
  const [k, n] = [256, 4];
  const {quants, scales} = quantizeWeight(activations(n * k, 81, 0), n, k);
  const a = {id: 0, op: 'input', shape: [1, k], data: new Array(k).fill(0.25)};
  const w = {id: 1, op: 'input', shape: [n, k], dtype: 'i16', data: quants};
  const s = {id: 2, op: 'input', shape: [n], data: Array.from(scales)};
  const fails = async (nodes, code, match) =>
    assert.rejects(() => rt.execute({version: 2, nodes, outputs: [{name: 'o', id: nodes.length - 1}]}),
      e => { assert.equal(e.code, code, `${match}: got ${e.code} ${e.message}`);
             assert.match(e.message, match); return true; });
  try {
    // A float32 matmul would look up a kernel named after the dtype and find
    // none, so it refuses rather than leaving an output at zero.
    await fails([a, w, {id: 2, op: 'matmul', a: 0, b: 1, transposed: true}], 'PROTOCOL', /belongs to matmul_fixed/);
    await fails([a, w, {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true}], 'PROTOCOL', /needs the row scales/);
    await fails([a, {id: 1, op: 'input', shape: [n, k], data: new Array(n * k).fill(1)}, s,
      {id: 3, op: 'matmul_fixed', a: 0, b: 1, c: 2, transposed: true}], 'PROTOCOL', /only an i16 weight has/);
    await fails([a, w, {id: 2, op: 'input', shape: [n + 1], data: new Array(n + 1).fill(1)},
      {id: 3, op: 'matmul_fixed', a: 0, b: 1, c: 2, transposed: true}], 'SHAPE', /scales must be \[N\]/);
    // Its bytes are its values: half of them is not a shorter weight.
    await fails([a, {id: 1, op: 'input', shape: [n, k], dtype: 'i16', data: quants.slice(0, n * k)}, s,
      {id: 3, op: 'matmul_fixed', a: 0, b: 1, c: 2, transposed: true}], 'SHAPE', /needs \d+ bytes/);
    // And it is not a tensor anyone can read back.
    await assert.rejects(() => rt.execute({version: 2, nodes: [a, w, s,
      {id: 3, op: 'matmul_fixed', a: 0, b: 1, c: 2, transposed: true}], outputs: [{name: 'o', id: 1}]}),
      e => { assert.equal(e.code, 'PROTOCOL'); assert.match(e.message, /no float32 readback/); return true; });
  } finally { rt.dispose(); }
});

/** How far the integer answer lands from the float32 one, in scale-free terms. */
function cost(fixed, float) {
  let worst = 0, dot = 0, nf = 0, nx = 0;
  const rel = [];
  for (let i = 0; i < float.length; i++) {
    worst = Math.max(worst, Math.abs(fixed[i] - float[i]));
    rel.push(Math.abs(fixed[i] - float[i]) / Math.max(Math.abs(float[i]), 1e-6));
    dot += fixed[i] * float[i]; nf += fixed[i] ** 2; nx += float[i] ** 2;
  }
  rel.sort((x, y) => x - y);
  const rms = Math.sqrt(nx / float.length);
  return {worst, rms, worstOverRms: worst / rms, medianRel: rel[rel.length >> 1], cosine: dot / Math.sqrt(nf * nx)};
}

test('what it costs against float32, and that the cost is relative rather than absolute', async () => {
  const {cpu} = await runtimes();
  try {
    const [m, k, n] = [1, 1024, 256];
    // Eight outliers at 30 sigma, which is the case that makes activation
    // quantisation hard and the reason the scale is per row.
    const a = activations(m * k, 31, 8), b = activations(n * k, 32, 0);
    const {fixed, float} = (await cpu.execute(fixedGraph(a, b, [m, k], [n]), {typedOutputs: true})).outputs;
    const c = cost(fixed.data, float.data);
    // Stated against the output's own RMS, because an absolute figure only
    // means anything for one tensor's magnitude. On a real projection from a
    // real checkpoint the same measurement is 2.25e-3 absolute; here the
    // weights are unit-normal rather than a checkpoint's ~0.02, so the outputs
    // are fifty times larger and so is any absolute error.
    assert.ok(c.worstOverRms < 1e-3, `worst difference ${c.worstOverRms.toExponential(2)} of the output RMS`);
    assert.ok(c.medianRel < 5e-4, `median relative error ${c.medianRel.toExponential(2)}`);
    assert.ok(c.cosine > 0.999999, `cosine ${c.cosine.toFixed(7)}`);

    // Scaling the weights scales the answer and the error together: both sides
    // are quantized against their own row's maximum, so nothing in this depends
    // on the units the tensors happen to be in. A checkpoint's weights are two
    // orders of magnitude smaller than these and cost exactly the same.
    const small = Float32Array.from(b, x => x * 0.02);
    const shrunk = (await cpu.execute(fixedGraph(a, small, [m, k], [n]), {typedOutputs: true})).outputs;
    const d = cost(shrunk.fixed.data, shrunk.float.data);
    assert.ok(Math.abs(d.worstOverRms / c.worstOverRms - 1) < 0.05,
      `scale-free: ${c.worstOverRms.toExponential(2)} against ${d.worstOverRms.toExponential(2)}`);
  } finally { cpu.dispose(); }
});

test('where it stops being close to float32: an outlier whose weights are zero', async () => {
  // The honest limit of the whole scheme. A row's scale is set by its largest
  // magnitude, so a single value a thousand times the rest crushes everything
  // else towards a quant of zero. That is usually harmless, because a value
  // that large usually dominates the output too and is carried exactly -- but
  // not if the weights it meets are zero, in which case the answer is made
  // entirely of the terms that were crushed.
  //
  // This is the adversarial construction, and it is pinned here so that a
  // change which makes it worse is caught rather than discovered. The ratios a
  // real residual stream produces are 30 to 100, the first two rows.
  const {cpu} = await runtimes();
  try {
    const [k, n] = [1024, 64];
    const measured = [];
    for (const ratio of [1, 100, 1e4]) {
      const a = activations(k, 5, 0); a[13] = ratio;
      const b = activations(n * k, 6, 0);
      for (let c = 0; c < n; c++) b[c * k + 13] = 0;   // the outlier contributes nothing
      const {fixed, float} = (await cpu.execute(fixedGraph(a, b, [1, k], [n]), {typedOutputs: true})).outputs;
      measured.push(cost(fixed.data, float.data));
    }
    const [ordinary, hard, extreme] = measured;
    assert.ok(ordinary.worstOverRms < 2e-4, `no outlier: ${ordinary.worstOverRms.toExponential(2)}`);
    assert.ok(hard.worstOverRms < 5e-3, `100x outlier: ${hard.worstOverRms.toExponential(2)}`);
    assert.ok(hard.cosine > 0.99999, `100x outlier cosine ${hard.cosine.toFixed(6)}`);
    // And that the degradation past that is real rather than imagined, so the
    // documented ceiling is not quietly optimistic.
    assert.ok(extreme.worstOverRms > 1e-2, `10000x outlier should be visibly worse, was ${extreme.worstOverRms.toExponential(2)}`);
  } finally { cpu.dispose(); }
});

test('the quantiser rounds halves the same way everywhere: floor(x + 0.5)', () => {
  // A row whose largest magnitude is 2 puts 2.5 and -2.5 exactly on halves.
  // `Math.round` would give 3 and -2; WGSL's `round` would give 2 and -2;
  // `floor(x + 0.5)` gives 3 and -2 in every language, and being the same
  // everywhere is the only property that matters.
  const unit = 2 / FIXED_QMAX;
  const src = Float32Array.from([2.5 * unit, -2.5 * unit, 0, 2]);
  const dst = new Int16Array(4);
  const scale = quantizeRow(src, 0, 4, dst, 0);
  assert.equal(scale, Math.fround(2 / FIXED_QMAX));
  assert.deepEqual([...dst], [3, -2, 0, FIXED_QMAX], 'halves round up, not to even and not away from zero');
});

test('a row with no magnitude has no scale, and contributes nothing', async () => {
  const {cpu, wasm} = await runtimes();
  try {
    const k = 256, m = 2, n = 2;
    const a = new Float32Array(m * k);                            // row 0 all zero
    for (let j = 0; j < k; j++) a[k + j] = (j % 7) - 3;
    const b = new Float32Array(n * k);                            // column 0 all zero
    for (let j = 0; j < k; j++) b[k + j] = (j % 5) - 2;
    const program = fixedGraph(a, b, [m, k], [n]);
    for (const [name, rt] of [['cpu-js', cpu], ['wasm', wasm]]) {
      const got = (await rt.execute(program, {typedOutputs: true})).outputs.fixed.data;
      assert.deepEqual([...got.slice(0, 3)], [0, 0, 0], `${name}: a zero row or column is zero, not NaN`);
      assert.ok(Number.isFinite(got[3]) && got[3] !== 0, `${name}: the remaining output is real`);
    }
    // Reaching this line is half the assertion on its own: readback refuses a
    // non-finite value, so a zero scale that became a division by zero would
    // have thrown above rather than compared unequal.
  } finally { cpu.dispose(); wasm.dispose(); }
});

test('the quant range is symmetric, so negating an input negates the output exactly', async () => {
  const {cpu, wasm} = await runtimes();
  try {
    const [m, k, n] = [2, 512, 8];
    const a = activations(m * k, 41, 3), b = activations(n * k, 42, 0);
    const negated = Float32Array.from(a, x => -x);
    for (const [name, rt] of [['cpu-js', cpu], ['wasm', wasm]]) {
      const x = (await rt.execute(fixedGraph(a, b, [m, k], [n]), {typedOutputs: true})).outputs.fixed.data;
      const y = (await rt.execute(fixedGraph(negated, b, [m, k], [n]), {typedOutputs: true})).outputs.fixed.data;
      for (let i = 0; i < x.length; i++) assert.ok(Object.is(-x[i], y[i]) || (x[i] === 0 && y[i] === 0),
        `${name}[${i}]: ${x[i]} negated is ${y[i]}`);
    }
  } finally { cpu.dispose(); wasm.dispose(); }
});

test('validation: the weight layout, and the quantized case that motivates it', async () => {
  const rt = await createRuntime({backend: 'cpu-js'});
  const fails = async (nodes, code, match) => {
    await assert.rejects(() => rt.execute({version: 2, nodes, outputs: [{name: 'o', id: nodes.length - 1}]}),
      e => { assert.equal(e.code, code, `${match}: got ${e.code} ${e.message}`);
             assert.match(e.message, match); return true; });
  };
  const a = {id: 0, op: 'input', shape: [2, 4], data: [1, 2, 3, 4, 5, 6, 7, 8]};
  const b = {id: 1, op: 'input', shape: [2, 4], data: [1, 0, 0, 1, 0, 1, 1, 0]};
  try {
    await fails([a, b, {id: 2, op: 'matmul_fixed', a: 0, b: 1}], 'PROTOCOL', /transposed is required/);
    await fails([a, b, {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: false}], 'PROTOCOL', /transposed is required/);
    await fails([a, {id: 1, op: 'input', shape: [4, 3], data: new Array(12).fill(1)},
                 {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true}], 'SHAPE', /\[M,K\] @ \[N,K\]/);
    // A quantized weight is admitted here exactly as `matmul` admits it.
    const ok = await rt.execute({version: 2, nodes: [
      {id: 0, op: 'input', shape: [1, 256], data: new Array(256).fill(0.25)},
      {id: 1, op: 'input', shape: [1, 256], dtype: 'q4_k', data: blocks(1, 2, 'q4_k')},
      {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true}],
      outputs: [{name: 'o', id: 2}]});
    assert.equal(ok.outputs.o.data.length, 1);
  } finally { rt.dispose(); }
});

test('the accumulator ceiling is checked, not assumed', async () => {
  // Past k = 2^23, k products of two int16s can exceed what a double holds
  // exactly. No real projection is that wide; an untrusted graph can still ask,
  // and the answer has to be a refusal rather than a silently inexact sum.
  const {validateProgram} = await import('../src/graph.mjs');
  const k = 2 ** 24;
  const nodes = [
    {id: 0, op: 'input', shape: [1, k]},
    {id: 1, op: 'input', shape: [1, k]},
    {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true},
  ];
  // Generous limits everywhere else, so the refusal that lands is this one and
  // not an allocation budget that would have refused any wide graph.
  const limits = {maxLogicalBytes: 2 ** 40, maxWork: 2 ** 50, maxElements: 2 ** 30,
    maxInputElements: 2 ** 30, maxOutputElements: 2 ** 20, maxDimension: 2 ** 24};
  assert.throws(() => validateProgram({version: 2, nodes, outputs: [{name: 'o', id: 2}]}, limits, {session: true}),
    e => { assert.equal(e.code, 'LIMIT'); assert.match(e.message, /exact integer accumulator/); return true; });
});

test('a backend without the kernel refuses the graph instead of returning zeros', async () => {
  // The failure this guards against is silent: an operation with no case in a
  // backend's switch leaves its output at zero and hands back a plausible
  // tensor of nothing. WebGPU and WebGL2 are in that position today.
  const stub = {
    name: 'stub', unsupported: () => new Set(['matmul_fixed']),
    begin: async () => {}, run: async n => new Float32Array(n.size).fill(1),
    read: async h => h, free() {}, finish: async () => {}, dispose() {},
  };
  const rt = new ComputeRuntime(stub, {}, []);
  const nodes = [
    {id: 0, op: 'input', shape: [1, 4], data: [1, 2, 3, 4]},
    {id: 1, op: 'input', shape: [1, 4], data: [1, 1, 1, 1]},
    {id: 2, op: 'matmul_fixed', a: 0, b: 1, transposed: true},
  ];
  await assert.rejects(() => rt.execute({version: 2, nodes, outputs: [{name: 'o', id: 2}]}),
    e => { assert.equal(e.code, 'UNSUPPORTED'); assert.match(e.message, /stub backend has no matmul_fixed/); return true; });
  // And that the gate is specific: an ordinary matmul still runs.
  const plain = [...nodes.slice(0, 2), {id: 2, op: 'matmul', a: 0, b: 1, transposed: true}];
  await rt.execute({version: 2, nodes: plain, outputs: [{name: 'o', id: 2}]});
  rt.dispose();
});

test('the GPU backends declare the gap rather than leaving it to be discovered', async () => {
  // Read from the prototypes, not from an instance: neither backend can be
  // created in Node, and the declaration is what the runtime consults.
  const {WebGPUBackend} = await import('../src/backends/webgpu.mjs');
  const {WebGL2Backend} = await import('../src/backends/webgl2.mjs');
  for (const Backend of [WebGPUBackend, WebGL2Backend]) {
    const set = Backend.prototype.unsupported.call({});
    assert.ok(set.has('matmul_fixed'), `${Backend.name} does not declare matmul_fixed unsupported`);
  }
});
