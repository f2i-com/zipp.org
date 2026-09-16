// The host reference (cpu-js) and the compiled kernels (wasm) must agree bit
// for bit on whole training steps, not within a tolerance: sessions, the
// Python reference in zipp_gpu.py and the corpus all treat cpu-js as the
// float32 oracle, and a double kept unrounded between two float32 operations
// (as cross_entropy_grad's exp/s once was) shows up here as thousands of
// last-bit differences.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createRuntime} from '../src/runtime.mjs';
import {mlpTrainingStep} from './ml-cases.mjs';
const wasmBytes = await readFile(new URL('../wasm/kernels.wasm', import.meta.url));

test('cpu-js and wasm produce the same float32 bits for relu and gelu cross-entropy Adam steps', async () => {
  const cpu = await createRuntime({backend: 'cpu-js'}), wasm = await createRuntime({backend: 'wasm', wasmBytes});
  try {
    let compared = 0;
    for (const seed of [1, 2, 3, 4, 5, 6, 7, 8]) {
      for (const activation of ['relu', 'gelu']) {
        const {program} = mlpTrainingStep({sizes: [20, 16, 5], batch: 8, seed, activation});
        const a = (await cpu.execute(program, {typedOutputs: true})).outputs;
        const b = (await wasm.execute(program, {typedOutputs: true})).outputs;
        assert.deepEqual(Object.keys(a).sort(), Object.keys(b).sort());
        for (const name of Object.keys(a)) {
          const x = a[name].data, y = b[name].data;
          assert.equal(x.length, y.length, `${name}: length`);
          for (let i = 0; i < x.length; i++) {
            compared++;
            if (x[i] !== y[i] || (x[i] === 0 && 1 / x[i] !== 1 / y[i])) {
              assert.fail(`seed ${seed} ${activation} ${name}[${i}]: cpu-js ${x[i]} versus wasm ${y[i]}`);
            }
          }
        }
      }
    }
    assert.ok(compared > 20000, `compared ${compared} elements`);
  } finally { cpu.dispose(); wasm.dispose(); }
});
