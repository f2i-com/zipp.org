#!/usr/bin/env node
// Where one decode step's time goes, per operation, on a GPU backend.
//
// The graph is a real one and the weights are synthetic, so the shapes, dtypes
// and node count are exactly what a real step has without needing a checkpoint
// in the browser. It reads gpu-lab/skeleton.json, which is a bound decode plan
// with its tensor data replaced by sizes:
//
//   node -e "..." // see model-plugins/tests/qwen3.test.mjs for the plan, then
//                 // map each input's data to {blocks: n} or {f32: n}
//
// Read the numbers with care on WebGL2: a draw call returns before the GPU has
// done anything, so per-node timings there measure issue cost and the real work
// lands in the readback at the end. That is itself the finding -- if the
// attributed total is a fraction of the step, the step is GPU-bound, not
// dispatch-bound.
//
//   node playground/serve.cjs &   # PORT=8767
//   npm install --no-save playwright
//   BACKEND=webgl2 node gpu-lab/scripts/profile-decode.cjs
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

  const backend = process.env.BACKEND || 'webgl2';
  const out = await page.evaluate(async ({base, backend}) => {
    const {createRuntime} = await import(base + 'src/runtime.mjs');
    const skeleton = await (await fetch(base + 'skeleton.json')).json();
    const limits = {maxNodes: 8192, maxElements: 4194304, maxInputElements: 200000000,
      maxOutputElements: 200000000, maxLogicalBytes: 3 * 1024 ** 3, maxWork: 200000000000,
      maxDimension: 1 << 21, maxWebGLTextureBytes: 2 * 1024 ** 3, maxSessions: 4, maxStepsPerRun: 64};

    let seed = 1 >>> 0;
    const next = () => (seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0);
    const nodes = skeleton.nodes.map(n => {
      const node = {...n};
      if (n.op === 'input' && n.data) {
        if (n.data.blocks !== undefined) {
          const b = new Uint8Array(n.data.blocks);
          for (let i = 0; i < b.length; i++) b[i] = next() & 0xff;
          // Keep the f16 scales modest so products stay in range.
          const size = n.dtype === 'q4_k' ? 144 : 210;
          for (let i = 0; i < b.length / size; i++) {
            const at = i * size;
            if (n.dtype === 'q4_k') { b[at + 1] = 0x20; b[at + 3] = 0x18; } else { b[at + 209] = 0x20; }
          }
          node.data = b;
        } else {
          const f = new Float32Array(n.data.f32);
          for (let i = 0; i < f.length; i++) f[i] = ((next() % 1000) / 1000) - 0.5;
          node.data = f;
        }
      }
      return node;
    });

    const runtime = await createRuntime({backend, limits});
    const session = await runtime.prepare({version: 2, nodes, outputs: skeleton.outputs},
      {resident: skeleton.outputs.filter(o => o.name !== 'logits').map(o => o.name), typedOutputs: true});

    // Per-step inputs, shaped as stepInputs would make them.
    const inputs = {};
    for (const step of skeleton.steps) {
      const node = nodes[step.node];
      const size = node.shape.reduce((a, b) => a * b, 1);
      const data = new Float32Array(size);
      if (step.slot === 'mask') { /* nothing masked at position 0 beyond 0 */ for (let i = 1; i < size; i++) data[i] = -1e9; }
      else if (step.slot === 'write') data[0] = 1;
      else if (step.slot === 'rope') for (let i = 0; i < size; i++) data[i] = step.part === 'cos' ? 1 : 0;
      else for (let i = 0; i < size; i++) data[i] = ((next() % 1000) / 1000) - 0.5;
      inputs[step.node] = data;
    }

    // Attribute time per operation.
    const impl = session.impl ?? session;
    const inner = impl.impl ?? impl;
    if (!inner || typeof inner.run !== 'function') return {error: 'cannot reach the backend'};
    const original = inner.run.bind(inner);
    const spent = new Map(), counts = new Map(), bytes = new Map();
    inner.run = async (n, refs) => {
      const key = n.op === 'matmul'
        ? (n.bQuant ? `matmul(${n.bQuant.dtype})` : n.transposed ? 'matmul(transposed)' : 'matmul')
        : n.op;
      const t = performance.now();
      const result = await original(n, refs);
      const ms = performance.now() - t;
      spent.set(key, (spent.get(key) ?? 0) + ms);
      counts.set(key, (counts.get(key) ?? 0) + 1);
      bytes.set(key, (bytes.get(key) ?? 0) + (n.size ?? 0));
      return result;
    };

    await session.run([{inputs}], {readback: ['logits']});  // warm
    // Three numbers that apportion a step: the whole thing, the same without
    // reading 151,936 logits back, and how many dispatches it actually issues
    // -- a reshape is an alias and issues none.
    const time = async options => {
      const started = performance.now();
      for (let i = 0; i < 3; i++) await session.run([{inputs}], options);
      return (performance.now() - started) / 3;
    };
    const withReadback = await time({readback: ['logits']});
    const withoutReadback = await time({readback: []});
    spent.clear(); counts.clear(); bytes.clear();
    const t0 = performance.now();
    await session.run([{inputs}], {readback: ['logits']});
    const total = performance.now() - t0;
    session.dispose(); runtime.dispose();
    return {total, withReadback, withoutReadback,
      dispatches: [...counts.values()].reduce((a, b) => a + b, 0),
      rows: [...spent.entries()].sort((a, b) => b[1] - a[1])
        .map(([op, ms]) => ({op, ms, calls: counts.get(op), elements: bytes.get(op)}))};
  }, {base: BASE, backend});

  if (out.error) { console.log('  ' + out.error); await browser.close(); return; }
  console.log(`  ${backend}: ${out.withReadback.toFixed(1)} ms a token, ` +
    `${out.withoutReadback.toFixed(1)} ms without reading the logits back, ` +
    `${out.dispatches} dispatches`);
  console.log(`  (the attribution below carries the cost of measuring it: ` +
    `${out.total.toFixed(1)} ms instrumented)\n`);
  let acc = 0;
  for (const r of out.rows) {
    acc += r.ms;
    console.log(`    ${r.op.padEnd(22)} ${r.ms.toFixed(1).padStart(7)} ms  ${String(r.calls).padStart(5)} calls  ` +
                `${(r.elements / 1e6).toFixed(1).padStart(7)} M out  ${(100 * r.ms / out.total).toFixed(1).padStart(5)}%`);
  }
  console.log(`    ${'(attributed)'.padEnd(22)} ${acc.toFixed(1).padStart(7)} ms`);
  await browser.close();
})();
