#!/usr/bin/env node
// The transposed and quantized matmuls, on every backend a real browser offers.
//
// Node reaches cpu-js and wasm, so the test suite holds those two to bit-for-bit
// agreement. WebGL2 and WebGPU need a GPU and a browser, which is this. It
// reports three things per backend, and the first is the one that matters:
//
//   quantVsDecodedSameBackend    the shader's own Q4_K decoder, against that
//                                same backend's matmul over decoded values.
//                                Must be 0: decoding is integer arithmetic and
//                                two f16 scales, so there is nothing to round
//                                differently. A non-zero here is a wrong
//                                decoder, not a float32 GPU.
//   transposedVsPlainSameBackend the [N,K] kernel against the [K,N] one over
//                                the transpose. Also 0.
//   quantVsCpuJs                 the whole product against the reference, which
//                                is a GPU accumulating float32 in its own order
//                                and is held to the suite's 2e-4, not to 0.
//
// Needs the playground server on 8767 and Playwright (a locally installed
// Chrome or Edge is used; no browser download):
//
//   cd crates/zipp-wasm
//   node playground/serve.cjs &                 # PORT=8767
//   npm install --no-save playwright
//   node gpu-lab/scripts/check-gpu-matmul.cjs   # PLAYWRIGHT_CHANNEL=chrome|msedge
"use strict";
const {chromium} = require('playwright');

const ORIGIN = 'http://127.0.0.1:8767';
const BASE = `${ORIGIN}/crates/zipp-wasm/gpu-lab/`;

(async () => {
  const browser = await chromium.launch({
    channel: process.env.PLAYWRIGHT_CHANNEL || 'chrome',
    args: ['--enable-unsafe-webgpu', '--enable-features=Vulkan', '--ignore-gpu-blocklist'],
  });
  const page = await browser.newPage();
  page.on('console', m => { if (m.type() === 'error') console.log('  [console]', m.text()); });
  await page.goto(BASE + 'demo/', {waitUntil: 'domcontentloaded'}).catch(async () => {
    await page.goto(ORIGIN + '/crates/zipp-wasm/playground/', {waitUntil: 'domcontentloaded'});
  });

  const out = await page.evaluate(async base => {
    const {createRuntime} = await import(base + 'src/runtime.mjs');
    const {decodeQ4K, decodeQ6K, FORMATS} = await import(base + 'src/quant.mjs');

    function blocks(count, seed, dtype) {
      let state = seed >>> 0;
      const next = () => (state = (Math.imul(state, 1664525) + 1013904223) >>> 0);
      const size = FORMATS[dtype].bytes;
      const b = new Uint8Array(count * size);
      for (let i = 0; i < count; i++) {
        const at = i * size;
        for (let j = 0; j < size; j++) b[at+j] = next() & 0xff;
        if (dtype === 'q4_k') { b[at+1] = 0x20 | (next() & 7); b[at+3] = 0x18 | (next() & 7); }
        else { b[at+209] = 0x20 | (next() & 7); }
      }
      return b;
    }
    const decoded = (b, dtype) => {
      const o = new Float32Array((b.length / FORMATS[dtype].bytes) * 256);
      (dtype === 'q4_k' ? decodeQ4K : decodeQ6K)(b, 0, o.length, o);
      return o;
    };
    const RESIDENT = ['q4_k', 'q6_k'];

    // [m, k, n], and [m, k, n, batch] where each batch carries its own blocks --
    // the offset arithmetic every backend does for itself.
    const cases = [[1, 256, 4], [2, 512, 7], [3, 256, 16], [1, 768, 32], [1, 256, 4, 2], [2, 512, 5, 3]];
    const report = {};
    let reference = null;

    for (const name of ['cpu-js', 'wasm', 'webgl2', 'webgpu']) {
      let runtime;
      try { runtime = await createRuntime({backend: name}); }
      catch (e) { report[name] = {status: 'unavailable', why: String(e.message || e)}; continue; }
      const got = {quant: [], plain: [], transposedRef: []};
      try {
        for (const dtype of RESIDENT) for (const [m, k, n, batch = 1] of cases) {
          const weight = blocks(batch * (k / 256) * n, m * 131 + k + n + batch, dtype);
          const values = decoded(weight, dtype);
          const a = Float32Array.from({length: batch * m * k}, (_, i) => ((i * 37) % 19) / 16 - 0.5);
          const shape = (...tail) => batch > 1 ? [batch, ...tail] : tail;
          const prog = (b, dtype) => ({version: 2, nodes: [
            {id: 0, op: 'input', shape: shape(m, k), data: a},
            dtype ? {id: 1, op: 'input', shape: shape(n, k), dtype, data: b}
                  : {id: 1, op: 'input', shape: shape(n, k), data: b},
            {id: 2, op: 'matmul', a: 0, b: 1, transposed: true},
          ], outputs: [{name: 'out', id: 2}]});
          // Column-major copy per batch, so the plain (untransposed) kernel computes the same product.
          const cm = new Float32Array(batch * k * n);
          for (let t = 0; t < batch; t++)
            for (let r = 0; r < n; r++) for (let c = 0; c < k; c++)
              cm[t*k*n + c * n + r] = values[t*k*n + r * k + c];
          const plainProg = {version: 2, nodes: [
            {id: 0, op: 'input', shape: shape(m, k), data: a},
            {id: 1, op: 'input', shape: shape(k, n), data: cm},
            {id: 2, op: 'matmul', a: 0, b: 1},
          ], outputs: [{name: 'out', id: 2}]};

          got.quant.push([...(await runtime.execute(prog(weight, dtype), {typedOutputs: true})).outputs.out.data]);
          got.plain.push([...(await runtime.execute(prog(values), {typedOutputs: true})).outputs.out.data]);
          got.transposedRef.push([...(await runtime.execute(plainProg, {typedOutputs: true})).outputs.out.data]);
        }
      } catch (e) { report[name] = {status: 'error', why: String(e.message || e)}; runtime.dispose(); continue; }
      runtime.dispose();

      // Absolute error, and how much of the suite's acceptance band it uses:
      // 2e-4 + 2e-4*|expected|, the same rule browser-cases applies. A GPU
      // summing float32 in its own order is allowed to differ; a wrong kernel
      // is not, and the ratio is what tells them apart.
      const worst = (x, y) => {
        let w = 0, band = 0, scale = 0;
        for (let i = 0; i < x.length; i++) for (let j = 0; j < x[i].length; j++) {
          const error = Math.abs(x[i][j] - y[i][j]);
          w = Math.max(w, error);
          scale = Math.max(scale, Math.abs(y[i][j]));
          band = Math.max(band, error / (2e-4 + 2e-4 * Math.abs(y[i][j])));
        }
        return {error: w, band, scale};
      };
      if (!reference) reference = got;
      report[name] = {
        status: 'ran',
        // The shader's own decoder: quantized against this backend's own f32 transposed matmul.
        quantVsDecodedSameBackend: worst(got.quant, got.plain),
        // The transposed kernel: against this backend's untransposed matmul over the transpose.
        transposedVsPlainSameBackend: worst(got.plain, got.transposedRef),
        // And against the reference backend.
        quantVsCpuJs: worst(got.quant, reference.quant),
        transposedVsCpuJs: worst(got.plain, reference.plain),
      };
    }
    return report;
  }, BASE);

  await browser.close();

  let bad = 0;
  for (const [name, r] of Object.entries(out)) {
    if (r.status === 'unavailable') { console.log(`  --   ${name}: unavailable (${r.why})`); continue; }
    // An error is a broken kernel, not an absent GPU.
    if (r.status !== 'ran') { bad++; console.log(`  FAIL ${name}: ${r.why}`); continue; }
    // Exactness for the decoder and the layout; the suite's GPU tolerance for
    // the product itself.
    const fails = [];
    if (r.quantVsDecodedSameBackend.error !== 0) fails.push(`decoder off by ${r.quantVsDecodedSameBackend.error}`);
    if (r.transposedVsPlainSameBackend.error !== 0) fails.push(`transpose off by ${r.transposedVsPlainSameBackend.error}`);
    if (r.quantVsCpuJs.band > 1) fails.push(`quantized ${r.quantVsCpuJs.error} from cpu-js (${r.quantVsCpuJs.band.toFixed(2)}x the band)`);
    if (r.transposedVsCpuJs.band > 1) fails.push(`transposed ${r.transposedVsCpuJs.error} from cpu-js (${r.transposedVsCpuJs.band.toFixed(2)}x the band)`);
    if (fails.length) { bad++; console.log(`  FAIL ${name}: ${fails.join('; ')}`); }
    else {
      const e = Math.max(r.quantVsCpuJs.error, r.transposedVsCpuJs.error);
      const band = Math.max(r.quantVsCpuJs.band, r.transposedVsCpuJs.band);
      console.log(`  ok   ${name}: decoder exact, product within ${e.toExponential(1)} of cpu-js ` +
                  `(${(100 * band).toFixed(0)}% of the accepted band, values up to ${r.quantVsCpuJs.scale.toFixed(0)})`);
    }
  }
  process.exit(bad ? 1 : 0);
})();
