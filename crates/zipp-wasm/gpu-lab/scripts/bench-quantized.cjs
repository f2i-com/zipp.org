// Where does a decode step's time actually go on a GPU backend?
// Two questions, separated: how fast is one big quantized matmul, and how much
// does dispatching ~2,800 nodes cost regardless of what they compute.
const {chromium} = require('playwright');
const ORIGIN = 'http://127.0.0.1:8767';
const BASE = `${ORIGIN}/crates/zipp-wasm/gpu-lab/`;

(async () => {
  const browser = await chromium.launch({
    channel: process.env.PLAYWRIGHT_CHANNEL || 'chrome',
    args: ['--enable-unsafe-webgpu', '--ignore-gpu-blocklist'],
  });
  const page = await browser.newPage();
  page.on('console', m => { if (m.type() === 'error') console.log('  [console]', m.text().slice(0, 200)); });
  await page.goto(BASE + 'demo/', {waitUntil: 'domcontentloaded'});

  const out = await page.evaluate(async base => {
    const {createRuntime} = await import(base + 'src/runtime.mjs');
    const {FORMATS} = await import(base + 'src/quant.mjs');
    const limits = {maxNodes: 8192, maxElements: 4194304, maxInputElements: 200000000,
      maxOutputElements: 200000000, maxLogicalBytes: 3 * 1024 ** 3, maxWork: 200000000000,
      maxDimension: 1 << 21, maxWebGLTextureBytes: 2 * 1024 ** 3, maxSessions: 4, maxStepsPerRun: 64};

    const blocks = (count, dtype) => {
      let s = 12345 >>> 0;
      const next = () => (s = (Math.imul(s, 1664525) + 1013904223) >>> 0);
      const b = new Uint8Array(count * FORMATS[dtype].bytes);
      for (let i = 0; i < b.length; i++) b[i] = next() & 0xff;
      for (let i = 0; i < count; i++) {
        const at = i * FORMATS[dtype].bytes;
        if (dtype === 'q4_k') { b[at + 1] = 0x20; b[at + 3] = 0x18; } else { b[at + 209] = 0x20; }
      }
      return b;
    };

    const report = {};
    for (const name of ['wasm', 'webgl2', 'webgpu']) {
      let runtime;
      try { runtime = await createRuntime({backend: name, limits}); }
      catch (e) { report[name] = {status: 'unavailable', why: String(e.message || e)}; continue; }
      const r = {};
      try {
        // 1. The output projection on its own: [1,1024] @ [151936,1024]^T, Q6_K.
        //    155 million weights, the single largest matmul in the model.
        const [k, n] = [1024, 151936];
        const weight = blocks((k * n) / 256, 'q6_k');
        const a = new Float32Array(k).fill(0.01);
        const program = {version: 2, nodes: [
          {id: 0, op: 'input', shape: [1, k], data: a},
          {id: 1, op: 'input', shape: [n, k], dtype: 'q6_k', data: weight},
          {id: 2, op: 'matmul', a: 0, b: 1, transposed: true},
        ], outputs: [{name: 'out', id: 2}]};
        const session = await runtime.prepare(program);
        await session.run([{inputs: {0: a}}], {readback: ['out']});   // warm
        const t1 = performance.now();
        for (let i = 0; i < 3; i++) await session.run([{inputs: {0: a}}], {readback: ['out']});
        r.logitsMatmulMs = (performance.now() - t1) / 3;
        session.dispose();

        // 1b. The same product with the weight already float32, to separate
        //     decoding from multiplying, and a decode-sized one to see whether
        //     the answer changes when there are few outputs.
        // 4096 rows is the largest float32 the protocol admits (a quantized one is
        // bounded by its bytes instead), so that is the wide case here.
        for (const [label, rows, cols] of [['big', 4096, 1024], ['layer', 2048, 1024]]) {
          const q = blocks((rows * cols) / 256, 'q4_k');
          const f = new Float32Array(rows * cols);
          for (let i = 0; i < f.length; i++) f[i] = ((i % 97) / 97) - 0.5;
          const x = new Float32Array(cols).fill(0.01);
          const build = (b, dtype) => ({version: 2, nodes: [
            {id: 0, op: 'input', shape: [1, cols], data: x},
            dtype ? {id: 1, op: 'input', shape: [rows, cols], dtype, data: b}
                  : {id: 1, op: 'input', shape: [rows, cols], data: b},
            {id: 2, op: 'matmul', a: 0, b: 1, transposed: true},
          ], outputs: [{name: 'out', id: 2}]});
          for (const [suffix, program] of [['Q4K', build(q, 'q4_k')], ['f32', build(f)]]) {
            const s2 = await runtime.prepare(program);
            await s2.run([{inputs: {0: x}}], {readback: ['out']});
            const t = performance.now();
            for (let i = 0; i < 5; i++) await s2.run([{inputs: {0: x}}], {readback: ['out']});
            r[`${label}${suffix}Ms`] = (performance.now() - t) / 5;
            s2.dispose();
          }
        }

        // 1c. Enough work in one dispatch to escape the round trip: 64 rows
        //     against a 4096 by 1024 weight is 268 million multiply-adds, the
        //     same order as a whole decode step, in one operation. This is the
        //     only place the decode's real cost shows.
        {
          const [rows, cols, m] = [4096, 1024, 64];
          const q = blocks((rows * cols) / 256, 'q4_k');
          const f = new Float32Array(rows * cols);
          for (let i = 0; i < f.length; i++) f[i] = ((i % 97) / 97) - 0.5;
          const x = new Float32Array(m * cols).fill(0.01);
          const build = (b, dtype) => ({version: 2, nodes: [
            {id: 0, op: 'input', shape: [m, cols], data: x},
            dtype ? {id: 1, op: 'input', shape: [rows, cols], dtype, data: b}
                  : {id: 1, op: 'input', shape: [rows, cols], data: b},
            {id: 2, op: 'matmul', a: 0, b: 1, transposed: true},
          ], outputs: [{name: 'out', id: 2}]});
          for (const [suffix, program] of [['Q4K', build(q, 'q4_k')], ['f32', build(f)]]) {
            const s3 = await runtime.prepare(program);
            await s3.run([{inputs: {0: x}}], {readback: ['out']});
            const t = performance.now();
            for (let i = 0; i < 5; i++) await s3.run([{inputs: {0: x}}], {readback: ['out']});
            r[`bulk${suffix}Ms`] = (performance.now() - t) / 5;
            s3.dispose();
          }
        }

        // 2. Dispatch cost: 2,800 tiny nodes that compute almost nothing.
        const nodes = [{id: 0, op: 'input', shape: [64], data: new Float32Array(64).fill(1)}];
        for (let i = 1; i < 2800; i++) nodes.push({id: i, op: 'add', a: i - 1, b: 0});
        const chain = await runtime.prepare({version: 2, nodes, outputs: [{name: 'out', id: nodes.length - 1}]});
        await chain.run([{inputs: {}}], {readback: ['out']});
        const t2 = performance.now();
        for (let i = 0; i < 3; i++) await chain.run([{inputs: {}}], {readback: ['out']});
        r.dispatch2800Ms = (performance.now() - t2) / 3;
        chain.dispose();
        r.status = 'ran';
      } catch (e) { r.status = 'error'; r.why = String(e.message || e); }
      runtime.dispose();
      report[name] = r;
    }
    return report;
  }, BASE);

  for (const [name, r] of Object.entries(out)) {
    if (r.status !== 'ran') { console.log(`  ${name.padEnd(8)} ${r.status}: ${r.why}`); continue; }
    console.log(`  ${name}`);
    console.log(`     155M Q6_K matmul ${r.logitsMatmulMs.toFixed(2).padStart(7)} ms` +
                `   2,800 trivial nodes ${r.dispatch2800Ms.toFixed(2).padStart(7)} ms`);
    console.log(`     [1,1024]@[4096,1024]  Q4_K ${r.bigQ4KMs.toFixed(2).padStart(7)} ms   f32 ${r.bigf32Ms.toFixed(2).padStart(7)} ms` +
                `   decode costs ${((r.bigQ4KMs / r.bigf32Ms - 1) * 100).toFixed(0)}%`);
    console.log(`     [1,1024]@[2048,1024]  Q4_K ${r.layerQ4KMs.toFixed(2).padStart(7)} ms   f32 ${r.layerf32Ms.toFixed(2).padStart(7)} ms` +
                `   decode costs ${((r.layerQ4KMs / r.layerf32Ms - 1) * 100).toFixed(0)}%`);
    console.log(`     [64,1024]@[4096,1024]  Q4_K ${r.bulkQ4KMs.toFixed(2).padStart(7)} ms   f32 ${r.bulkf32Ms.toFixed(2).padStart(7)} ms` +
                `   decode costs ${((r.bulkQ4KMs / r.bulkf32Ms - 1) * 100).toFixed(0)}%   <- 268M MACs, past the round trip`);
  }
  await browser.close();
})();
