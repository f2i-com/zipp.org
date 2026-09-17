// Can a recurrent layer run on ZIPP without new kernels?
//
// I previously said a Gated-DeltaNet linear-attention layer -- the kind that
// makes up 18 of Qwen3.5-0.8B's 24 layers -- needs scan and convolution
// primitives Graph v2 does not have, and that a sequential unroll would blow
// the node budget. That was wrong in an important way, and this file is the
// correction.
//
// A scan is only needed to process a whole sequence in one graph. A prepared
// session already carries state across runs, so the recurrence can be one step
// per run with the state living on the device, exactly like the key/value cache
// in decode.mjs. The node count then depends on the layer, not the sequence
// length. What follows builds both pieces out of existing operations and checks
// them against an independent implementation, on the real runtime:
//
//   * a causal depthwise convolution over a window of 4, as a carried window
//     shifted by a matmul with a fixed shift matrix;
//   * a gated delta-rule state update, S' = S * decay + k(x)v, as a batched
//     outer product added to a decayed carried state.
//
// This is evidence about primitives, not support for any checkpoint. Qwen3.5
// remains out of reach for the reasons in docs/HANDOFF.md, which are about
// memory and precision rather than about these two operations.
import test from 'node:test';
import assert from 'node:assert/strict';
import {access} from 'node:fs/promises';

const runtimeURL = new URL('../../gpu-lab/src/runtime.mjs', import.meta.url);
const present = await access(runtimeURL).then(() => true, () => false);
const HEADS = 2, KEYS = 3, VALUES = 4, WINDOW = 4, CHANNELS = 5, STEPS = 6;

/** x[c] shifted into a carried window of WINDOW columns, then weighted and summed. */
function convolutionGraph(nodes, bindings, outputs, {window, kernel}) {
  const id = () => nodes.length - 1;
  // shift[a][b] = 1 when a == b + 1: multiplying moves every column one left.
  const shift = new Float32Array(WINDOW * WINDOW);
  for (let b = 0; b < WINDOW - 1; b++) shift[(b + 1) * WINDOW + b] = 1;
  const last = new Float32Array(WINDOW); last[WINDOW - 1] = 1;
  nodes.push({id: nodes.length, op: 'input', shape: [CHANNELS, WINDOW], carry: 'window', data: window});
  const carried = id();
  nodes.push({id: nodes.length, op: 'input', shape: [WINDOW, WINDOW], data: [...shift]});
  const shifted = id();
  nodes.push({id: nodes.length, op: 'input', shape: [1, WINDOW], data: [...last]});
  const slot = id();
  nodes.push({id: nodes.length, op: 'input', shape: [CHANNELS, 1]});
  const sample = id();
  bindings.push({node: sample, slot: 'sample'});
  nodes.push({id: nodes.length, op: 'matmul', a: carried, b: shifted});
  const moved = id();
  nodes.push({id: nodes.length, op: 'matmul', a: sample, b: slot});
  const placed = id();
  nodes.push({id: nodes.length, op: 'add', a: moved, b: placed});
  const updated = id();
  outputs.push({name: 'window', id: updated});
  nodes.push({id: nodes.length, op: 'input', shape: [CHANNELS, WINDOW], data: kernel});
  const weights = id();
  nodes.push({id: nodes.length, op: 'mul', a: updated, b: weights});
  nodes.push({id: nodes.length, op: 'sum', a: id(), axis: -1, keepdim: true});
  outputs.push({name: 'convolved', id: id()});
}

/** S' = S * decay + k v, read as o = q S'. */
function stateGraph(nodes, bindings, outputs, {state}) {
  const id = () => nodes.length - 1;
  nodes.push({id: nodes.length, op: 'input', shape: [HEADS, KEYS, VALUES], carry: 'state', data: state});
  const carried = id();
  nodes.push({id: nodes.length, op: 'input', shape: [HEADS, 1, 1]});
  const decay = id();
  bindings.push({node: decay, slot: 'decay'});
  nodes.push({id: nodes.length, op: 'input', shape: [HEADS, KEYS, 1]});
  const keys = id();
  bindings.push({node: keys, slot: 'keys'});
  nodes.push({id: nodes.length, op: 'input', shape: [HEADS, 1, VALUES]});
  const values = id();
  bindings.push({node: values, slot: 'values'});
  nodes.push({id: nodes.length, op: 'input', shape: [HEADS, 1, KEYS]});
  const query = id();
  bindings.push({node: query, slot: 'query'});
  nodes.push({id: nodes.length, op: 'mul', a: carried, b: decay});
  const decayed = id();
  nodes.push({id: nodes.length, op: 'matmul', a: keys, b: values});
  const written = id();
  nodes.push({id: nodes.length, op: 'add', a: decayed, b: written});
  const updated = id();
  outputs.push({name: 'state', id: updated});
  nodes.push({id: nodes.length, op: 'matmul', a: query, b: updated});
  outputs.push({name: 'read', id: id()});
}

function randomData(count, seed) {
  let state = seed >>> 0;
  return Float32Array.from({length: count}, () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 4294967296 - 0.5;
  });
}

test('a carried window and a shift matrix are a causal depthwise convolution',
  {skip: !present && 'gpu-lab runtime not present'}, async () => {
  const {createRuntime} = await import(runtimeURL);
  const runtime = await createRuntime({backend: 'cpu-js'});
  const kernel = randomData(CHANNELS * WINDOW, 7);
  const nodes = [], bindings = [], outputs = [];
  convolutionGraph(nodes, bindings, outputs, {window: new Float32Array(CHANNELS * WINDOW), kernel: [...kernel]});
  let session;
  try {
    session = await runtime.prepare({version: 2, nodes, outputs}, {resident: ['window']});
    // An independent reading of the same recurrence.
    const window = Array.from({length: CHANNELS}, () => new Array(WINDOW).fill(0));
    for (let step = 0; step < STEPS; step++) {
      const sample = randomData(CHANNELS, 11 + step);
      for (let c = 0; c < CHANNELS; c++) { window[c].shift(); window[c].push(sample[c]); }
      const expected = window.map((row, c) => row.reduce((sum, v, j) => sum + v * kernel[c * WINDOW + j], 0));
      const feed = Object.fromEntries(bindings.map(b => [b.node, sample]));
      const result = await session.run([{inputs: feed}], {readback: ['convolved']});
      const got = result.outputs.convolved.data;
      assert.equal(got.length, CHANNELS);
      for (let c = 0; c < CHANNELS; c++) assert.ok(Math.abs(got[c] - expected[c]) < 1e-5, `channel ${c} step ${step}`);
    }
  } finally { session?.dispose(); runtime.dispose(); }
});

test('a decayed carried state and an outer product are a gated delta rule',
  {skip: !present && 'gpu-lab runtime not present'}, async () => {
  const {createRuntime} = await import(runtimeURL);
  const runtime = await createRuntime({backend: 'cpu-js'});
  const nodes = [], bindings = [], outputs = [];
  stateGraph(nodes, bindings, outputs, {state: new Float32Array(HEADS * KEYS * VALUES)});
  let session;
  try {
    session = await runtime.prepare({version: 2, nodes, outputs}, {resident: ['state']});
    const state = Array.from({length: HEADS}, () => Array.from({length: KEYS}, () => new Array(VALUES).fill(0)));
    for (let step = 0; step < STEPS; step++) {
      const decay = randomData(HEADS, 3 + step).map(v => Math.abs(v) + 0.25);
      const keys = randomData(HEADS * KEYS, 23 + step);
      const values = randomData(HEADS * VALUES, 41 + step);
      const query = randomData(HEADS * KEYS, 59 + step);
      for (let h = 0; h < HEADS; h++) {
        for (let k = 0; k < KEYS; k++) {
          for (let v = 0; v < VALUES; v++) {
            state[h][k][v] = state[h][k][v] * decay[h] + keys[h * KEYS + k] * values[h * VALUES + v];
          }
        }
      }
      const expected = [];
      for (let h = 0; h < HEADS; h++) {
        for (let v = 0; v < VALUES; v++) {
          let sum = 0;
          for (let k = 0; k < KEYS; k++) sum += query[h * KEYS + k] * state[h][k][v];
          expected.push(sum);
        }
      }
      const feed = {};
      for (const b of bindings) feed[b.node] = {decay, keys, values, query}[b.slot];
      const result = await session.run([{inputs: feed}], {readback: ['read']});
      const got = result.outputs.read.data;
      assert.equal(got.length, expected.length);
      for (let i = 0; i < expected.length; i++) {
        assert.ok(Math.abs(got[i] - expected[i]) < 1e-5, `element ${i} at step ${step}: ${got[i]} vs ${expected[i]}`);
      }
    }
    // The point of the exercise: the graph does not grow with the sequence.
    assert.ok(nodes.length < 16, `a recurrent step is ${nodes.length} nodes regardless of how many steps run`);
  } finally { session?.dispose(); runtime.dispose(); }
});
