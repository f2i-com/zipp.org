// gpu-lab's runtime on its own, without Python: the cases of
// crates/zipp-cli/tests/native_gpu/bench_vs_torch.py as graph programs,
// timed as prepared-session runs (one step per run and eight per run) and,
// for the MLPs, as per-call executes. The same function runs natively
// (tests/engine_bench.rs, over wgpu) and in Chrome
// (crates/zipp-cli/tests/native_gpu/bench_browser.py), so the two measure
// the same engine work. `M` holds createRuntime and tests/ml-cases.mjs's
// exports.
async function engineBench(M, backend, opts = {}) {
  const {createRuntime, mlpSessionProgram, mlpTrainingStep, embeddingSessionProgram, seeded} = M;
  const now = () => performance.now();
  const median = a => { const s = [...a].sort((x, y) => x - y); return s[Math.floor(s.length / 2)]; };
  const reps = opts.reps ?? 15, runs = opts.runs ?? 5, calls = opts.calls ?? 5;
  const want = name => !opts.only || opts.only.includes(name);
  const limits = {maxElements: 1 << 26, maxInputElements: 1 << 26, maxOutputElements: 1 << 26,
    maxWork: Number.MAX_SAFE_INTEGER, maxLogicalBytes: 8 * 1024 * 1024 * 1024};
  const rt = await createRuntime({backend, limits});
  // Natively, the pool the CLI's driver keeps (js/driver.js).
  if (typeof __zgpuInit === 'function' && rt.impl.poolBytes) rt.impl.poolBytes = opts.poolBytes ?? 2 * 1024 * 1024 * 1024;
  const out = {backend, info: rt.info(), cases: []};
  // A ramp exact in float32, like the Python side's.
  const ramp = (n, seed, scale = 1) => {
    const a = new Float32Array(n);
    for (let i = 0; i < n; i++) a[i] = (((i * 7919 + seed * 104729) % 2003) / 2003 - 0.5) * scale;
    return a;
  };
  async function session(name, program, resident, feeds, readback) {
    const t0 = now(), s = await rt.prepare(program, {resident}), prepareMs = now() - t0;
    try {
      const t1 = now(), first = await s.run(feeds[0], {readback}), firstMs = now() - t1;
      const values = [first.outputs[readback[0]].data[0]];
      const one = [];
      for (let i = 0; i < reps + 2; i++) { const t = now(); const r = await s.run(feeds[(i + 1) % feeds.length], {readback}); one.push(now() - t); if (i < 7) values.push(r.outputs[readback[0]].data[0]); }
      const eight = [];
      for (let i = 0; i < runs + 1; i++) { const t = now(); await s.run(Array.from({length: 8}, (_, j) => feeds[j % feeds.length]), {readback}); eight.push((now() - t) / 8); }
      return {case: name, prepareMs, firstMs, stepMs: median(one.slice(2)), steps8Ms: median(eight.slice(1)), values};
    } finally { s.dispose(); }
  }
  for (const [name, sizes, batch] of [['mlp_s', [784, 256, 10], 64], ['mlp_m', [784, 1024, 1024, 10], 256], ['mlp_l', [784, 2048, 2048, 10], 1024]]) {
    if (!want(name)) continue;
    const spec = mlpSessionProgram({sizes, batch, lr: 0.002}), rnd = seeded(11);
    const feeds = Array.from({length: 8}, () => ({inputs: {0: Float32Array.from({length: batch * sizes[0]}, () => rnd(0, 1)),
      1: Float32Array.from({length: batch}, () => Math.floor(rnd(0, sizes[sizes.length - 1])))}}));
    const rec = await session(name, spec.program, spec.resident, feeds, ['loss']);
    if (calls > 0) {
      const {program} = mlpTrainingStep({sizes, batch, lr: 0.002});
      for (const n of program.nodes) if (n.data) n.data = Float32Array.from(n.data);
      const times = [];
      for (let i = 0; i < calls + 1; i++) { const t = now(); await rt.execute(program, {typedOutputs: true}); times.push(now() - t); }
      rec.callMs = median(times.slice(1));
    }
    out.cases.push(rec);
  }
  for (const n of [1024, 2048, 4096]) {
    const name = `mm_${n}`;
    if (!want(name)) continue;
    const scale = 3.4641016 / Math.sqrt(n);
    const nodes = [{id: 0, op: 'input', shape: [n, n], data: ramp(n * n, 1, scale)}, {id: 1, op: 'input', shape: [n, n], data: ramp(n * n, 2, scale)},
      {id: 2, op: 'input', shape: []}, {id: 3, op: 'mul', a: 0, b: 2}];
    for (let i = 0; i < 4; i++) nodes.push({id: 4 + i, op: 'matmul', a: 3 + i, b: 1});
    nodes.push({id: 8, op: 'sum', a: 7});
    const feeds = [{inputs: {2: new Float32Array([1])}}];
    const rec = await session(name, {version: 2, nodes, outputs: [{name: 'r', id: 8}]}, [], feeds, ['r']);
    rec.gflops = 2 * n ** 3 * 4 / (Math.min(rec.stepMs, rec.steps8Ms) * 1e6);
    out.cases.push(rec);
  }
  if (want('ew_4m')) {
    const n = 2048, nodes = [{id: 0, op: 'input', shape: [n, n], data: ramp(n * n, 5, 4)}, {id: 1, op: 'input', shape: []},
      {id: 2, op: 'full', shape: [], value: 1.5}, {id: 3, op: 'full', shape: [], value: 0.25}, {id: 4, op: 'mul', a: 0, b: 1}];
    let y = 4;
    const add = (op, a, b) => { const id = nodes.length; nodes.push(b === undefined ? {id, op, a} : {id, op, a, b}); return id; };
    for (let i = 0; i < 4; i++) {
      y = add('tanh', add('add', add('mul', y, 2), 3));
      y = add('mul', add('sigmoid', y), y);
      y = add('add', add('exp', add('neg', add('mul', y, y))), y);
    }
    const r = add('sum', y);
    out.cases.push(await session('ew_4m', {version: 2, nodes, outputs: [{name: 'r', id: r}]}, [], [{inputs: {1: new Float32Array([1])}}], ['r']));
  }
  if (want('emb')) {
    const spec = embeddingSessionProgram({vocab: 8192, dim: 128, batch: 256, steps: 16, classes: 10});
    const feeds = spec.batches(8).map(b => ({inputs: Object.fromEntries(Object.entries(b.inputs).map(([k, v]) => [k, Float32Array.from(v)]))}));
    out.cases.push(await session('emb', spec.program, spec.resident, feeds, ['loss']));
  }
  rt.dispose();
  return out;
}
