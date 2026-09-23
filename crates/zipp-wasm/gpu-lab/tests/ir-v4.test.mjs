// Graph IR v4: slice, slice_scatter, index_select, index_add, gather and
// scatter_add. Validation, the semantics against an independent statement of
// each operation (coordinates rather than strides), the accumulation order the
// protocol fixes, fed index inputs in sessions, and bit equality between the
// JavaScript reference and the compiled WASM kernels on every case.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createRuntime} from '../src/runtime.mjs';
import {validateProgram, VERSION_FOUR_OPS} from '../src/graph.mjs';
import {builder, opCases, seeded, embeddingSessionProgram} from './ml-cases.mjs';
const wasmBytes = await readFile(new URL('../wasm/kernels.wasm', import.meta.url));
const cpu = await createRuntime({backend: 'cpu-js'}), wasm = await createRuntime({backend: 'wasm', wasmBytes});
const shapeErr = e => e.code === 'SHAPE', numberErr = e => e.code === 'NUMBER', protoErr = e => e.code === 'PROTOCOL';
const same = (a, b, what) => {
  assert.equal(a.length, b.length, `${what}: length`);
  for (let i = 0; i < a.length; i++) assert.ok(Object.is(a[i], b[i]), `${what}[${i}]: ${a[i]} versus ${b[i]}`);
};
const size = s => s.reduce((x, y) => x * y, 1);
/** Row-major coordinates of flat index i in shape s, and back. */
const coords = (i, s) => { const c = []; for (let d = s.length - 1; d >= 0; d--) { c[d] = i % s[d]; i = Math.floor(i / s[d]); } return c; };
const flat = (c, s) => c.reduce((acc, v, d) => acc * s[d] + v, 0);
const f = Math.fround;

// ---- validation ------------------------------------------------------------------------
test('version-4 operations validate shapes, ranges, strides and index inputs', () => {
  const x = {id: 0, op: 'input', shape: [4, 5], data: Array.from({length: 20}, (_, i) => i)};
  const idx = {id: 1, op: 'input', shape: [3], data: [0, 3, 1]};
  const prog = (...nodes) => ({version: 4, nodes: [x, idx, ...nodes.map((n, i) => ({id: i + 2, ...n}))], outputs: [{name: 'r', id: nodes.length + 1}]});
  const s = validateProgram(prog({op: 'slice', a: 0, begin: [3, 4], stride: [-2, -1], shape: [2, 5]})).nodes[2];
  assert.deepEqual(s.shape, [2, 5]); assert.equal(s.offset, 19); assert.deepEqual(s.boxStrides, [0, 0, -10, -1]);
  // Out of range at either end, a zero stride, a rank mismatch, missing or unknown fields.
  for (const bad of [{begin: [0, 0], stride: [2, 1], shape: [3, 5]}, {begin: [4, 0], stride: [1, 1], shape: [1, 5]},
    {begin: [0, 0], stride: [0, 1], shape: [1, 5]}, {begin: [0], stride: [1], shape: [4]}, {begin: [1, 0], stride: [-2, 1], shape: [2, 5]}])
    assert.throws(() => validateProgram(prog({op: 'slice', a: 0, ...bad})), shapeErr, JSON.stringify(bad));
  assert.throws(() => validateProgram(prog({op: 'slice', a: 0, begin: [0, 0], stride: [1, 1]})), protoErr);
  assert.throws(() => validateProgram(prog({op: 'slice', a: 0, begin: [0, 0], stride: [1, 1], shape: [1, 1], axis: 0})), protoErr);
  assert.throws(() => validateProgram(prog({op: 'full', shape: [2, 2], value: 1}, {op: 'slice_scatter', a: 0, b: 2, begin: [3, 0], stride: [1, 1]})), shapeErr);
  assert.deepEqual(validateProgram(prog({op: 'index_select', a: 0, b: 1, axis: 1})).nodes[2].shape, [4, 3]);
  assert.deepEqual(validateProgram(prog({op: 'index_select', a: 0, b: 1, axis: -2})).nodes[2].shape, [3, 5]);
  // An index value outside the axis, a non-integer, a computed index, a 2-D index for index_select.
  const tooFar = {...idx, data: [0, 5, 1]};
  assert.throws(() => validateProgram({version: 4, nodes: [x, tooFar, {id: 2, op: 'index_select', a: 0, b: 1, axis: 1}], outputs: [{name: 'r', id: 2}]}), numberErr);
  const fraction = {...idx, data: [0, 1.5, 1]};
  assert.throws(() => validateProgram({version: 4, nodes: [x, fraction, {id: 2, op: 'index_select', a: 0, b: 1, axis: 1}], outputs: [{name: 'r', id: 2}]}), numberErr);
  assert.throws(() => validateProgram(prog({op: 'neg', a: 1}, {op: 'index_select', a: 0, b: 2, axis: 1})), numberErr);
  assert.throws(() => validateProgram(prog({op: 'index_select', a: 0, b: 0, axis: 1})), shapeErr);
  // Through a reshape the index is still the input's, and its bound is the smallest user's.
  const v = validateProgram(prog({op: 'reshape', a: 1, shape: [3]}, {op: 'index_select', a: 0, b: 2, axis: 1})).nodes;
  assert.equal(v[1].indexBound, 5);
  assert.throws(() => validateProgram(prog({op: 'index_add', a: 0, b: 0, c: 1, axis: 1})), shapeErr);
  const g = {id: 1, op: 'input', shape: [2, 5], data: [0, 1, 2, 3, 0, 3, 2, 1, 0, 3]};
  assert.deepEqual(validateProgram({version: 4, nodes: [x, g, {id: 2, op: 'gather', a: 0, b: 1, axis: 0}], outputs: [{name: 'r', id: 2}]}).nodes[2].shape, [2, 5]);
  const wide = {id: 1, op: 'input', shape: [2, 6], data: Array(12).fill(0)};
  assert.throws(() => validateProgram({version: 4, nodes: [x, wide, {id: 2, op: 'gather', a: 0, b: 1, axis: 0}], outputs: [{name: 'r', id: 2}]}), shapeErr);
  assert.throws(() => validateProgram({version: 4, nodes: [x, g, {id: 2, op: 'full', shape: [2, 4], value: 1},
    {id: 3, op: 'scatter_add', a: 0, b: 2, c: 1, axis: 0}], outputs: [{name: 'r', id: 3}]}), shapeErr);
  assert.deepEqual([...VERSION_FOUR_OPS].sort(), ['gather', 'index_add', 'index_select', 'scatter_add', 'slice', 'slice_scatter']);
});

// ---- semantics against an independent statement ------------------------------------------
function reference(op, fields, a, as, b, bs, c) {
  if (op === 'slice') return Array.from({length: size(fields.shape)}, (_, i) =>
    a[flat(coords(i, fields.shape).map((x, d) => fields.begin[d] + x * fields.stride[d]), as)]);
  if (op === 'slice_scatter') {
    const out = [...a];
    for (let i = 0; i < b.length; i++) out[flat(coords(i, bs).map((x, d) => fields.begin[d] + x * fields.stride[d]), as)] = b[i];
    return out;
  }
  const axis = fields.axis;
  if (op === 'index_select') {
    const os = as.map((d, i) => i === axis ? b.length : d);
    return Array.from({length: size(os)}, (_, i) => { const x = coords(i, os); x[axis] = b[x[axis]]; return a[flat(x, as)]; });
  }
  if (op === 'gather') return Array.from({length: b.length}, (_, i) => { const x = coords(i, bs); x[axis] = b[i]; return a[flat(x, as)]; });
  // The accumulations, written as "for every output, walk the contributors in order".
  const out = new Float32Array(a);
  for (let o = 0; o < a.length; o++) {
    const x = coords(o, as);
    for (let k = 0; k < (op === 'index_add' ? c.length : bs[axis]); k++) {
      if (op === 'index_add') { if (c[k] !== x[axis]) continue; const y = [...x]; y[axis] = k; out[o] = f(out[o] + b[flat(y, bs)]); }
      else {
        const y = [...x]; y[axis] = k;
        if (y.some((v, d) => v >= bs[d])) continue;
        const j = flat(y, bs); if (c[j] === x[axis]) out[o] = f(out[o] + b[j]);
      }
    }
  }
  return Array.from(out);
}
test('each operation equals an independent coordinate-wise statement of it, on cpu-js and wasm', async () => {
  const rnd = seeded(404), values = n => Array.from({length: n}, (_, i) => i % 7 === 2 ? -0 : f(rnd(-4, 4)));
  const cases = [];
  for (const [as, begin, stride, shape] of [[[10], [9], [-3], [4]], [[3, 4], [2, 0], [-1, 3], [3, 2]], [[2, 3, 5], [0, 2, 4], [1, -2, -2], [2, 2, 3]],
    [[3, 2, 2, 4], [1, 1, 0, 0], [1, -1, 1, 3], [2, 2, 2, 2]]]) {
    cases.push({op: 'slice', as, fields: {begin, stride, shape}});
    cases.push({op: 'slice_scatter', as, bs: shape, fields: {begin, stride}});
  }
  for (const [as, axis, index] of [[[5], 0, [4, 4, 0]], [[3, 4], 1, [3, 0, 3, 3, 1]], [[2, 3, 4], 0, [1, 1, 0]], [[2, 2, 3, 2], 2, [2, 0]]]) {
    cases.push({op: 'index_select', as, index, fields: {axis}});
    cases.push({op: 'index_add', as, bs: as.map((d, i) => i === axis ? index.length : d), index, fields: {axis}});
  }
  for (const [as, axis, is] of [[[5], 0, [8]], [[3, 4], 1, [2, 6]], [[3, 4], 0, [5, 4]], [[2, 3, 4], 2, [2, 2, 7]], [[2, 2, 3, 3], 3, [1, 2, 3, 5]]]) {
    const index = Array.from({length: size(is)}, () => Math.floor(rnd(0, as[axis])));
    cases.push({op: 'gather', as, is, index, fields: {axis}});
    cases.push({op: 'scatter_add', as, bs: is, is, index, fields: {axis}});
  }
  for (const c of cases) {
    const g = builder(), aData = values(size(c.as)), a = g.input(aData, c.as);
    let node, bData, idx;
    if (c.op === 'slice') node = g.node('slice', {a, ...c.fields});
    else if (c.op === 'slice_scatter') { bData = values(size(c.bs)); node = g.node('slice_scatter', {a, b: g.input(bData, c.bs), ...c.fields}); }
    else if (c.op === 'index_select' || c.op === 'gather') node = g.node(c.op, {a, b: g.input(c.index, c.is ?? [c.index.length]), ...c.fields});
    else { bData = values(size(c.bs)); node = g.node(c.op, {a, b: g.input(bData, c.bs), c: g.input(c.index, c.is ?? [c.index.length]), ...c.fields}); }
    const program = g.program({r: node});
    assert.equal(program.version, 4);
    const want = c.op === 'index_select' || c.op === 'gather'
      ? reference(c.op, c.fields, aData.map(f), c.as, c.index, c.is ?? [c.index.length])
      : reference(c.op, c.fields, aData.map(f), c.as, bData?.map(f), c.bs, c.index);
    for (const rt of [cpu, wasm]) same((await rt.execute(program)).outputs.r.data, want, `${rt.backend} ${c.op} [${c.as}] ${JSON.stringify(c.fields)}`);
  }
});

test('index_add and scatter_add add the base first, then contributions in ascending position, one rounding each', async () => {
  const e = 2 ** -24, g = builder(), base = g.input([1, -0, 0, 3]), src = g.input([e, -0, e, -0, 2 ** 24, -(2 ** 24)]), idx = g.input([0, 1, 0, 2, 3, 3]);
  const program = g.program({ia: g.node('index_add', {a: base, b: src, c: idx, axis: 0}), sa: g.node('scatter_add', {a: base, b: src, c: idx, axis: 0})});
  for (const rt of [cpu, wasm]) {
    const r = (await rt.execute(program)).outputs;
    // 1 + e rounds to 1 twice (not 1 + 2e); -0 + -0 stays -0; +0 + -0 is +0; (3 + 2^24) rounds to 2^24 + 4 before the subtraction.
    same(r.ia.data, [1, -0, 0, 4], `${rt.backend} index_add`); same(r.sa.data, [1, -0, 0, 4], `${rt.backend} scatter_add`);
  }
});

// ---- every shared case, bit for bit ------------------------------------------------------
test('every version-4 fixture case is bit-identical on cpu-js and wasm', async () => {
  let compared = 0;
  for (const [name, program] of opCases()) {
    if (!program.nodes.some(n => VERSION_FOUR_OPS.includes(n.op))) continue;
    assert.equal(program.version, 4, name);
    const a = (await cpu.execute(program)).outputs, b = (await wasm.execute(program)).outputs;
    for (const key of Object.keys(a)) { same(b[key].data, a[key].data, `${name} ${key}`); compared++; }
  }
  assert.ok(compared >= 50, `compared ${compared} outputs`);
});

// ---- sessions ------------------------------------------------------------------------------
test('a session feeds an index input every step and checks it against the smallest extent it indexes', async () => {
  // An embedding lookup and its gradient: rows = W[idx], loss = sum(rows * rows) / 2, dW = index_add(0, idx, rows).
  const g = builder(), W = g.input(Array.from({length: 24}, (_, i) => f((i % 5) - 1.75)), [6, 4]);
  const idx = g.node('input', {shape: [2, 3]}), flatIdx = g.node('reshape', {a: idx, shape: [6]});
  const rows = g.node('index_select', {a: W, b: flatIdx, axis: 0}), last = g.node('slice', {a: g.node('reshape', {a: rows, shape: [2, 3, 4]}), begin: [0, 2, 0], stride: [1, 1, 1], shape: [2, 1, 4]});
  const dW = g.node('index_add', {a: g.full([6, 4], 0), b: rows, c: flatIdx, axis: 0});
  const next = g.op('sgd_update', W, dW, {lr: 0.25});
  g.nodes[W].carry = 'W';
  const program = g.program({last, W: next});
  const steps = [[0, 5, 5, 1, 2, 5], [3, 3, 3, 3, 3, 3], [4, 0, 2, 2, 1, 0]].map(v => ({inputs: {[idx]: v}}));
  const results = {};
  for (const rt of [cpu, wasm]) {
    const session = await rt.prepare(program, {resident: ['W']});
    assert.equal(session.describe().inputs.find(i => i.id === idx).indexBound, 6);
    await assert.rejects(session.run({inputs: {[idx]: [0, 6, 0, 0, 0, 0]}}), numberErr);
    await assert.rejects(session.run({inputs: {[idx]: [0, -1, 0, 0, 0, 0]}}), numberErr);
    const run = await session.run(steps, {readback: ['last']});
    results[rt.backend] = {last: run.steps.map(s => Array.from(s.outputs.last.data)), W: Array.from((await session.download(['W'])).outputs.W.data)};
    session.dispose();
  }
  assert.deepEqual(results.wasm, results['cpu-js']);
  // Step by step with execute(), feeding each step's weights: the same numbers.
  let weights = Array.from({length: 24}, (_, i) => f((i % 5) - 1.75));
  for (const [s, step] of steps.entries()) {
    const h = builder(), w = h.input(weights, [6, 4]), ix = h.input(step.inputs[idx], [6]);
    const r = h.node('index_select', {a: w, b: ix, axis: 0});
    const out = (await cpu.execute(h.program({last: h.node('slice', {a: h.node('reshape', {a: r, shape: [2, 3, 4]}), begin: [0, 2, 0], stride: [1, 1, 1], shape: [2, 1, 4]}),
      W: h.op('sgd_update', w, h.node('index_add', {a: h.full([6, 4], 0), b: r, c: ix, axis: 0}), {lr: 0.25})}))).outputs;
    same(results['cpu-js'].last[s], out.last.data, `step ${s} last`);
    weights = out.W.data;
  }
  same(results['cpu-js'].W, weights, 'weights after three steps');
});

test('the prepared embedding/slice step trains to the same bits on cpu-js and wasm', async () => {
  const spec = embeddingSessionProgram(), batches = spec.batches(8), results = {};
  assert.equal(spec.program.version, 4);
  for (const rt of [cpu, wasm]) {
    const session = await rt.prepare(spec.program, {resident: spec.resident});
    const run = await session.run(batches, {readback: ['loss', 'rows']});
    results[rt.backend] = {steps: run.steps, params: (await session.download(spec.resident)).outputs};
    session.dispose();
  }
  const a = results['cpu-js'], b = results.wasm;
  a.steps.forEach((s, i) => { same(b.steps[i].outputs.loss.data, s.outputs.loss.data, `loss ${i}`); same(b.steps[i].outputs.rows.data, s.outputs.rows.data, `rows ${i}`); });
  for (const name of spec.resident) same(b.params[name].data, a.params[name].data, name);
  const losses = a.steps.map(s => s.outputs.loss.data[0]);
  assert.ok(losses[losses.length - 1] < losses[0], `losses ${losses}`);
});
test('graphs without version-4 operations are unchanged and still run labelled 4', async () => {
  const g = builder(), x = g.input([1, -2, 3]);
  const program = {...g.program({r: g.op('relu', x)}), version: 4};
  for (const rt of [cpu, wasm]) assert.deepEqual((await rt.execute(program)).outputs.r.data, [1, 0, 3]);
  assert.throws(() => validateProgram({...program, version: 5}), protoErr);
});
test.after(() => { cpu.dispose(); wasm.dispose(); });
