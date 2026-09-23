// Graph IR v3: maximum/minimum, comparisons, `where` and the counter-based
// `uniform` generator. Validation, exact semantics (NaN, signed zeros, ties),
// the generator's known answers and statistics, its per-step advance in a
// prepared session, and a dropout training session that the JavaScript
// reference and the compiled WASM kernels must run to the same bits.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createRuntime} from '../src/runtime.mjs';
import {validateProgram, uniformKeys, uniformValue} from '../src/graph.mjs';
import {builder, dropoutSessionProgram} from './ml-cases.mjs';
const wasmBytes = await readFile(new URL('../wasm/kernels.wasm', import.meta.url));
const cpu = await createRuntime({backend: 'cpu-js'}), wasm = await createRuntime({backend: 'wasm', wasmBytes});
const shapeErr = e => e.code === 'SHAPE', numberErr = e => e.code === 'NUMBER', protoErr = e => e.code === 'PROTOCOL';
const same = (a, b, what) => {
  assert.equal(a.length, b.length, `${what}: length`);
  for (let i = 0; i < a.length; i++) assert.ok(Object.is(a[i], b[i]), `${what}[${i}]: ${a[i]} versus ${b[i]}`);
};

// ---- validation ------------------------------------------------------------------------
test('where and uniform validate their fields, shapes and integers', () => {
  const base = [{id: 0, op: 'input', shape: [2, 3], data: [1, 2, 3, 4, 5, 6]}, {id: 1, op: 'input', shape: [3], data: [1, 0, 1]}];
  const prog = node => ({version: 3, nodes: [...base, {id: 2, ...node}], outputs: [{name: 'r', id: 2}]});
  const w = validateProgram(prog({op: 'where', c: 1, a: 0, b: 1})).nodes[2];
  assert.deepEqual(w.shape, [2, 3]); assert.deepEqual(w.refs, [1, 0, 1]); assert.equal(w.mode, 'general');
  // Strides on the padded leading axes of size 1 are never used; the two real axes matter.
  assert.deepEqual(w.dims, [1, 1, 2, 3]); assert.deepEqual(w.cStrides.slice(2), [0, 1]); assert.deepEqual(w.aStrides.slice(2), [3, 1]);
  assert.equal(validateProgram(prog({op: 'where', c: 0, a: 0, b: 0})).nodes[2].mode, 'same');
  assert.throws(() => validateProgram(prog({op: 'where', c: 1, a: 0})), protoErr);
  assert.throws(() => validateProgram({version: 3, nodes: [...base, {id: 2, op: 'input', shape: [2], data: [1, 2]},
    {id: 3, op: 'where', c: 2, a: 0, b: 1}], outputs: [{name: 'r', id: 3}]}), shapeErr);
  const u = validateProgram(prog({op: 'uniform', shape: [4], seed: 7})).nodes[2];
  assert.equal(u.step, 1); assert.equal(u.seed, 7);
  for (const seed of [-1, 2 ** 32, 1.5, '1']) assert.throws(() => validateProgram(prog({op: 'uniform', shape: [4], seed})), numberErr);
  for (const step of [0, 2 ** 31 + 1, 0.5]) assert.throws(() => validateProgram(prog({op: 'uniform', shape: [4], seed: 1, step})), numberErr);
  assert.throws(() => validateProgram(prog({op: 'uniform', shape: [4], seed: 1, value: 2})), protoErr);
  assert.throws(() => validateProgram(prog({op: 'gt', a: 0})), protoErr);
});

// ---- exact semantics ---------------------------------------------------------------------
test('comparisons, maximum/minimum and where follow IEEE and PyTorch on NaN, ties and signed zeros', async () => {
  const g = builder();
  const x = g.input([1, 2, 3, -0, 0, -1]), y = g.input([1, 3, 2, 0, -0, -2]);
  const nan = g.op('div', g.full([6], 0), g.full([6], 0));
  const out = {gt: g.op('gt', x, y), ge: g.op('ge', x, y), lt: g.op('lt', x, y), le: g.op('le', x, y), eq: g.op('eq', x, y), ne: g.op('ne', x, y),
    max: g.op('maximum', x, y), min: g.op('minimum', x, y), nanNe: g.op('ne', nan, nan), nanEq: g.op('eq', nan, x), nanGe: g.op('ge', x, nan),
    maxNanIsNan: g.op('ne', g.op('maximum', x, nan), g.op('maximum', x, nan)), pickNan: g.node('where', {c: nan, a: x, b: y}),
    pick: g.node('where', {c: g.input([0, -0, 2, -3, 0, 1]), a: x, b: g.full([], 9)})};
  for (const rt of [cpu, wasm]) {
    const r = (await rt.execute(g.program(out))).outputs, v = k => r[k].data;
    assert.equal(g.program(out).version, 3);
    assert.deepEqual(v('gt'), [0, 0, 1, 0, 0, 1]); assert.deepEqual(v('ge'), [1, 0, 1, 1, 1, 1]);
    assert.deepEqual(v('lt'), [0, 1, 0, 0, 0, 0]); assert.deepEqual(v('le'), [1, 1, 0, 1, 1, 0]);
    assert.deepEqual(v('eq'), [1, 0, 0, 1, 1, 0]); assert.deepEqual(v('ne'), [0, 1, 1, 0, 0, 1]);
    // A tie goes to the first operand, so maximum(-0, +0) is -0 (PyTorch 2.11 gives the same).
    same(v('max'), [1, 3, 3, -0, 0, -1], `${rt.backend} maximum`); same(v('min'), [1, 2, 2, -0, 0, -2], `${rt.backend} minimum`);
    assert.deepEqual(v('nanNe'), [1, 1, 1, 1, 1, 1]); assert.deepEqual(v('nanEq'), [0, 0, 0, 0, 0, 0]); assert.deepEqual(v('nanGe'), [0, 0, 0, 0, 0, 0]);
    assert.deepEqual(v('maxNanIsNan'), [1, 1, 1, 1, 1, 1]);
    same(v('pickNan'), [1, 2, 3, -0, 0, -1], `${rt.backend} NaN condition picks a`);
    same(v('pick'), [9, 9, 3, -0, 9, -1], `${rt.backend} zero of either sign picks b`);
  }
});

// ---- the uniform generator -----------------------------------------------------------------
/** The definition again, in BigInt arithmetic: an independent statement of the hash. */
function referenceUniform(seed, step, i) {
  const M = 0xffffffffn, mix = v => {
    let x = BigInt(v) & M;
    x ^= x >> 16n; x = (x * 0x7feb352dn) & M; x ^= x >> 15n; x = (x * 0x846ca68bn) & M; return x ^ (x >> 16n);
  };
  const k1 = mix(BigInt(seed) ^ 0x9e3779b9n), k2 = mix(BigInt(step) ^ k1);
  return Number(mix((mix(BigInt(i) ^ k2) + k1) & M) >> 8n) / 16777216;
}
test('uniform: known answers, the BigInt statement of the hash and every Node backend agree exactly', async () => {
  // Pinned so that a change to the generator is a visible protocol change.
  const g = builder(), u = g.node('uniform', {shape: [2, 5], seed: 12345, step: 3});
  const want = [0.7625014185905457, 0.13164889812469482, 0.7715473175048828, 0.8630410432815552, 0.5749761462211609];
  for (const rt of [cpu, wasm]) same((await rt.execute(g.program({u}))).outputs.u.data.slice(0, 5), want, rt.backend);
  for (const [seed, step] of [[0, 1], [1, 1], [0xffffffff, 2 ** 31], [2 ** 31, 77]]) {
    const [k1, k2] = uniformKeys(seed, step);
    for (const i of [0, 1, 2, 1000, 65535, 4194303]) assert.equal(uniformValue(k1, k2, i), referenceUniform(seed, step, i), `${seed}/${step}/${i}`);
  }
});
test('uniform: values are multiples of 2^-24 in [0, 1) with uniform statistics and independent steps and seeds', async () => {
  const n = 1 << 18, draw = async (seed, step) => {
    const g = builder(); return (await wasm.execute(g.program({u: g.node('uniform', {shape: [512, 512], seed, step})}), {typedOutputs: true})).outputs.u.data;
  };
  const a = await draw(5, 1), b = await draw(5, 2), c = await draw(6, 1);
  let sum = 0, sq = 0, kept = 0;
  for (const v of a) { assert.ok(v >= 0 && v < 1 && Number.isInteger(v * 16777216)); sum += v; sq += v * v; if (v >= 0.3) kept++; }
  const mean = sum / n, variance = sq / n - mean * mean;
  assert.ok(Math.abs(mean - 0.5) < 0.003, `mean ${mean}`); assert.ok(Math.abs(variance - 1 / 12) < 0.002, `variance ${variance}`);
  assert.ok(Math.abs(kept / n - 0.7) < 0.004, `keep fraction ${kept / n}`);
  const corr = (x, y) => { const m = Math.min(x.length, y.length); let s = 0; for (let i = 0; i < m; i++) s += (x[i] - 0.5) * (y[i] - 0.5); return s / m * 12; };
  for (const [x, y, what] of [[a, b, 'consecutive steps'], [a, c, 'neighbouring seeds'], [a, a.subarray(1), 'neighbouring elements']])
    assert.ok(Math.abs(corr(x, y)) < 0.01, `${what}: correlation ${corr(x, y)}`);
  const buckets = new Array(16).fill(0); for (const v of a) buckets[Math.floor(v * 16)]++;
  const chi = buckets.reduce((s, k) => s + (k - n / 16) ** 2 / (n / 16), 0);
  assert.ok(chi < 40, `chi-square over 16 buckets ${chi}`); // 15 degrees of freedom: p < 0.001 above 37.7
});
test('a session draws a fresh uniform tensor every step: recorded step + session step - 1', async () => {
  const g = builder(), u = g.node('uniform', {shape: [3, 7], seed: 42, step: 5});
  const program = g.program({u}), expected = async step => {
    const h = builder(); return (await cpu.execute(h.program({u: h.node('uniform', {shape: [3, 7], seed: 42, step})}))).outputs.u.data;
  };
  for (const rt of [cpu, wasm]) {
    const session = await rt.prepare(program);
    const run = await session.run([{}, {}, {}]);
    for (let s = 0; s < 3; s++) same(run.steps[s].outputs.u.data, await expected(5 + s), `${rt.backend} step ${s + 1}`);
    const again = await session.run({}, {step: 1});
    same(again.outputs.u.data, await expected(5), `${rt.backend} restarted at step 1`);
    session.dispose();
  }
});

// ---- dropout training on the device ---------------------------------------------------------
test('a prepared dropout MLP trains with fresh masks each step, bit for bit on cpu-js and wasm', async () => {
  const spec = dropoutSessionProgram(), batches = spec.batches(6), results = {};
  for (const rt of [cpu, wasm]) {
    const session = await rt.prepare(spec.program, {resident: spec.resident});
    const one = await session.run(batches[0], {readback: ['loss', 'noise']});
    const rest = await session.run(batches.slice(1), {readback: ['loss', 'noise']});
    const params = (await session.download(spec.resident)).outputs;
    results[rt.backend] = {steps: [...one.steps, ...rest.steps], params};
    session.dispose();
  }
  const a = results['cpu-js'], b = results.wasm;
  a.steps.forEach((s, i) => {
    same(b.steps[i].outputs.loss.data, s.outputs.loss.data, `loss ${i}`);
    same(b.steps[i].outputs.noise.data, s.outputs.noise.data, `noise ${i}`);
  });
  for (const name of spec.resident) same(b.params[name].data, a.params[name].data, name);
  const masks = a.steps.map(s => Array.from(s.outputs.noise.data, v => v > 0 ? 1 : 0).join(''));
  assert.equal(new Set(masks).size, masks.length, 'every step draws a different mask');
  const scale = Math.fround(1 / Math.fround(0.75));
  let kept = 0, total = 0;
  for (const s of a.steps) for (const v of s.outputs.noise.data) { assert.ok(v === 0 || v === scale); kept += v > 0; total++; }
  assert.ok(Math.abs(kept / total - 0.75) < 0.08, `keep fraction ${kept / total}`);
  const losses = a.steps.map(s => s.outputs.loss.data[0]);
  assert.ok(losses.every(Number.isFinite));
});
test('graphs using only version-2 operations keep working when labelled version 3', async () => {
  const g = builder(), x = g.input([1, -2, 3]);
  const program = {...g.program({r: g.op('relu', x)}), version: 3};
  for (const rt of [cpu, wasm]) assert.deepEqual((await rt.execute(program)).outputs.r.data, [1, 0, 3]);
});
test.after(() => { cpu.dispose(); wasm.dispose(); });
