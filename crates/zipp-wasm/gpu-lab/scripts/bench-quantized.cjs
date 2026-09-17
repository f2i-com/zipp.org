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
    console.log(`  ${name.padEnd(8)} one 155M-weight Q6_K matmul: ${r.logitsMatmulMs.toFixed(1).padStart(7)} ms` +
                `   2,800 trivial nodes: ${r.dispatch2800Ms.toFixed(1).padStart(7)} ms`);
  }
  await browser.close();
})();
