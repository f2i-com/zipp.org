// Exactness checks for the WebGPU backend's faster kernels, run natively by
// tests/protocol_cases.rs (and in Chrome the same way): the register-blocked
// matmul tile against the 16x16 kernel on the same device, and the axis-sum
// and index_add kernels against cpu-js, bit for bit (a zero's sign included).
// Each returns {failures, report}.
async function exactMatmulTiles(M) {
  const rt = await M.createRuntime({backend: 'webgpu', limits: {maxElements: 1 << 26, maxInputElements: 1 << 26, maxOutputElements: 1 << 26, maxWork: Number.MAX_SAFE_INTEGER, maxLogicalBytes: 8 * 1024 * 1024 * 1024}});
  const impl = rt.impl;
  let seed = 12345;
  const rnd = () => (seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 4294967296;
  const data = n => {
    const a = new Float32Array(n);
    for (let i = 0; i < n; i++) {
      const r = rnd();
      a[i] = r < 0.04 ? (r < 0.02 ? -0 : 0) : (rnd() - 0.5) * (r < 0.1 ? 3e-3 : r < 0.2 ? 300 : 4);
    }
    return a;
  };
  // [a shape, b shape, transposed]
  const shapes = [
    [[1, 1], [1, 1]], [[3, 5], [5, 7]], [[17, 13], [13, 9]], [[64, 784], [784, 256]], [[257, 300], [300, 129]],
    [[130, 17], [17, 500]], [[64, 15], [15, 64]], [[1024, 1024], [1024, 1024]], [[1000, 999], [999, 1001]],
    [[2048, 1024], [1024, 10]], [[1024, 2048], [2048, 784]], [[784, 1024], [1024, 2048]], [[96, 1000], [1000, 128]],
    [[1, 1024], [1024, 2048]], [[5, 4096], [4096, 33]], [[300, 7], [7, 1]], [[129, 129], [129, 129]],
    [[3, 70, 40], [3, 40, 90]], [[2, 65, 33], [33, 66]], [[4, 200, 600], [4, 600, 150]],
    [[1, 256], [4, 256], true], [[3, 512], [16, 512], true], [[200, 300], [130, 300], true], [[1024, 1024], [1024, 1024], true],
    [[64, 1000], [128, 1000], true], [[2, 70, 100], [2, 90, 100], true],
  ];
  const report = [];
  let failures = 0;
  for (const [sa, sb, transposed] of shapes) {
    const size = s => s.reduce((x, y) => x * y, 1);
    const nodes = [{id: 0, op: 'input', shape: sa, data: data(size(sa))}, {id: 1, op: 'input', shape: sb, data: data(size(sb))},
      transposed ? {id: 2, op: 'matmul', a: 0, b: 1, transposed: true} : {id: 2, op: 'matmul', a: 0, b: 1}];
    const program = {version: 2, nodes, outputs: [{name: 'r', id: 2}]};
    const bits = {};
    for (const tile of [1, 4]) {
      impl.matmulTile = tile;
      const out = await rt.execute(program, {typedOutputs: true});
      bits[tile] = new Uint32Array(out.outputs.r.data.buffer.slice(0));
    }
    impl.matmulTile = null;
    const row = {a: sa, b: sb, transposed: !!transposed, n: bits[1].length};
    for (const tile of [4]) {
      let diff = 0, first = -1;
      for (let i = 0; i < bits[1].length; i++) if (bits[1][i] !== bits[tile][i]) { diff++; if (first < 0) first = i; }
      row[`r${tile}`] = diff;
      if (diff) { failures++; row[`first${tile}`] = first; }
    }
    report.push(row);
  }
  rt.dispose();
  return {failures, report};
}

async function exactReductions(M) {
  const limits = {maxElements: 1 << 26, maxInputElements: 1 << 26, maxOutputElements: 1 << 26, maxWork: Number.MAX_SAFE_INTEGER, maxLogicalBytes: 8 * 1024 * 1024 * 1024};
  const gpu = await M.createRuntime({backend: 'webgpu', limits}), cpu = await M.createRuntime({backend: 'cpu-js', limits});
  let seed = 777;
  const rnd = () => (seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 4294967296;
  const data = n => Float32Array.from({length: n}, () => { const r = rnd(); return r < 0.05 ? (r < 0.025 ? -0 : 0) : (rnd() - 0.5) * (r < 0.2 ? 1e4 : 3); });
  const same = (x, y) => { const a = new Uint32Array(x.buffer.slice(0)), b = new Uint32Array(Float32Array.from(y).buffer); let d = 0; for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) d++; return d; };
  const report = [];
  let failures = 0;
  const run = async (name, program) => {
    const g = await gpu.execute(program, {typedOutputs: true}), c = await cpu.execute(program, {typedOutputs: true});
    const diff = same(g.outputs.r.data, c.outputs.r.data);
    if (diff) failures++;
    report.push({name, n: g.outputs.r.data.length, diff});
  };
  for (const [shape, axis] of [[[1024, 2048], 0], [[7, 3], 0], [[5, 13, 9], 1], [[3, 1000], 1], [[1, 1], 0], [[9, 17], 0], [[64, 257], 0], [[2, 3, 1031], 2], [[4096, 3], 0], [[15, 8, 2], 0]]) {
    const size = shape.reduce((a, b) => a * b, 1);
    await run(`sum ${shape} axis ${axis}`, {version: 2, nodes: [{id: 0, op: 'input', shape, data: data(size)}, {id: 1, op: 'sum', a: 0, axis}], outputs: [{name: 'r', id: 1}]});
  }
  for (const [rows, dim, count, dup] of [[8192, 128, 4096, 0], [11, 8, 12, 0], [5, 1, 300, 1], [100, 3, 1000, 1], [3, 200, 70, 1], [65536, 2, 90, 0], [1, 64, 129, 1]]) {
    const idx = Float32Array.from({length: count}, () => dup ? Math.floor(rnd() * Math.min(rows, 4)) : Math.floor(rnd() * rows));
    const nodes = [{id: 0, op: 'input', shape: [rows, dim], data: data(rows * dim)}, {id: 1, op: 'input', shape: [count, dim], data: data(count * dim)},
      {id: 2, op: 'input', shape: [count], data: idx}, {id: 3, op: 'index_add', a: 0, b: 1, c: 2, axis: 0}];
    await run(`index_add [${rows},${dim}] <- ${count}${dup ? ' (repeats)' : ''}`, {version: 4, nodes, outputs: [{name: 'r', id: 3}]});
  }
  // index_add along an inner axis: [outer, N, inner].
  {
    const idx = Float32Array.from({length: 40}, () => Math.floor(rnd() * 6));
    const nodes = [{id: 0, op: 'input', shape: [3, 6, 5], data: data(90)}, {id: 1, op: 'input', shape: [3, 40, 5], data: data(600)},
      {id: 2, op: 'input', shape: [40], data: idx}, {id: 3, op: 'index_add', a: 0, b: 1, c: 2, axis: 1}];
    await run('index_add axis 1 [3,6,5] <- 40', {version: 4, nodes, outputs: [{name: 'r', id: 3}]});
  }
  gpu.dispose(); cpu.dispose();
  return {failures, report};
}

// Adam groups in one pass (in place in a session, into fresh buffers in an
// execute) against the three separate kernels, on the same device: every
// loss, weight and moment the same bits after several steps.
async function exactAdam(M) {
  const limits = {maxElements: 1 << 26, maxInputElements: 1 << 26, maxOutputElements: 1 << 26, maxWork: Number.MAX_SAFE_INTEGER, maxLogicalBytes: 8 * 1024 * 1024 * 1024};
  const bits = a => Array.from(new Uint32Array(Float32Array.from(a).buffer));
  const report = [];
  let failures = 0;
  for (const [sizes, batch] of [[[784, 64, 10], 32], [[30, 17, 5], 7], [[784, 256, 256, 10], 64]]) {
    const out = {};
    for (const fuse of [true, false]) {
      const rt = await M.createRuntime({backend: 'webgpu', limits});
      rt.impl.fuseAdam = fuse;
      const spec = M.mlpSessionProgram({sizes, batch, lr: 0.01}), rnd = M.seeded(3);
      const feeds = Array.from({length: 6}, () => ({inputs: {0: Float32Array.from({length: batch * sizes[0]}, () => rnd(0, 1)),
        1: Float32Array.from({length: batch}, () => Math.floor(rnd(0, sizes[sizes.length - 1])))}}));
      const s = await rt.prepare(spec.program, {resident: spec.resident});
      const losses = [];
      for (const f of feeds.slice(0, 2)) losses.push(...(await s.run(f, {readback: ['loss']})).outputs.loss.data);
      for (const r of (await s.run(feeds.slice(2), {readback: ['loss']})).steps) losses.push(...r.outputs.loss.data);
      const params = (await s.download(spec.resident)).outputs;
      s.dispose();
      const step = M.mlpTrainingStep({sizes, batch, lr: 0.01, step: 3}).program;
      const executed = (await rt.execute(step, {typedOutputs: true})).outputs;
      rt.dispose();
      out[fuse] = [bits(losses), ...Object.keys(params).sort().map(k => bits(params[k].data)), ...Object.keys(executed).sort().map(k => bits(executed[k].data))];
    }
    let diff = 0;
    out[true].forEach((a, i) => { const b = out[false][i]; for (let j = 0; j < a.length; j++) if (a[j] !== b[j]) diff++; });
    if (diff) failures++;
    report.push({sizes, batch, arrays: out[true].length, diff});
  }
  return {failures, report};
}

// The vectorised elementwise kernels (four elements an invocation) against
// the one-element kernels on the same device, bit for bit: every unary and
// binary operation (same shape and either side a scalar), fills, the
// optimizer updates and Adam's fused pass, over sizes that end mid-vec4.
async function exactVectorised(M) {
  const limits = {maxElements: 1 << 26, maxInputElements: 1 << 26, maxOutputElements: 1 << 26, maxWork: Number.MAX_SAFE_INTEGER, maxLogicalBytes: 8 * 1024 * 1024 * 1024};
  let seed = 4242;
  const rnd = () => (seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 4294967296;
  const data = (n, positive) => Float32Array.from({length: n}, () => {
    const r = rnd(); const v = r < 0.05 ? (r < 0.025 ? -0 : 0) : (rnd() - 0.5) * (r < 0.15 ? 60 : 8);
    return positive ? Math.abs(v) + 0.25 : v;
  });
  const unary = ['relu', 'positive', 'neg', 'exp', 'log', 'sqrt', 'tanh', 'sigmoid', 'gelu', 'gelu_grad'];
  const binary = ['add', 'sub', 'mul', 'div', 'maximum', 'minimum', 'eq', 'ne', 'lt', 'le', 'gt', 'ge'];
  const report = [];
  let failures = 0;
  for (const shape of [[1], [3], [5], [8], [1023], [97, 1031]]) {
    const n = shape.reduce((a, b) => a * b, 1);
    const nodes = [{id: 0, op: 'input', shape, data: data(n)}, {id: 1, op: 'input', shape, data: data(n).map(v => v === 0 ? 0.5 : v)},
      {id: 2, op: 'input', shape, data: data(n, true)}, {id: 3, op: 'input', shape: [], data: [1.75]}, {id: 4, op: 'input', shape, data: data(n, true)}];
    const outputs = [], add = node => { node.id = nodes.length; nodes.push(node); outputs.push({name: 'o' + node.id, id: node.id}); };
    for (const op of unary) add({op, a: ['log', 'sqrt'].includes(op) ? 2 : 0});
    // Input 1 has no zeros (it divides); 0 has signed zeros (it never does).
    for (const op of binary) { add({op, a: 0, b: 1}); add({op, a: 3, b: op === 'div' ? 1 : 0}); add({op, a: 0, b: 3}); }
    add({op: 'full', shape, value: -0.375});
    add({op: 'sgd_update', a: 0, b: 1, lr: 0.01});
    add({op: 'momentum_update', a: 0, b: 1, momentum: 0.9, dampening: 0.1});
    add({op: 'adam_m', a: 0, b: 1, beta1: 0.9});
    add({op: 'adam_v', a: 4, b: 1, beta2: 0.999});
    add({op: 'adam_update', a: 0, b: 1, c: 4, lr: 1e-3, beta1: 0.9, beta2: 0.999, eps: 1e-8, step: 3});
    const bits = {};
    for (const vectorize of [true, false]) {
      const rt = await M.createRuntime({backend: 'webgpu', limits});
      rt.impl.vectorize = vectorize;
      const chunks = [];
      for (let k = 0; k < outputs.length; k += 60) {
        const out = await rt.execute({version: 3, nodes, outputs: outputs.slice(k, k + 60)}, {typedOutputs: true});
        for (const o of outputs.slice(k, k + 60)) chunks.push(new Uint32Array(out.outputs[o.name].data.buffer.slice(0)));
      }
      rt.dispose();
      bits[vectorize] = chunks;
    }
    let diff = 0;
    bits[true].forEach((a, i) => { const b = bits[false][i]; for (let j = 0; j < a.length; j++) if (a[j] !== b[j]) diff++; });
    if (diff) failures++;
    report.push({n, outputs: outputs.length, diff});
  }
  // Adam's fused pass (in place in a session, fresh buffers in an execute).
  for (const [sizes, batch] of [[[784, 64, 10], 32], [[30, 17, 5], 7]]) {
    const out = {};
    for (const vectorize of [true, false]) {
      const rt = await M.createRuntime({backend: 'webgpu', limits});
      rt.impl.vectorize = vectorize;
      const spec = M.mlpSessionProgram({sizes, batch, lr: 0.01}), rnd2 = M.seeded(3);
      const feeds = Array.from({length: 4}, () => ({inputs: {0: Float32Array.from({length: batch * sizes[0]}, () => rnd2(0, 1)),
        1: Float32Array.from({length: batch}, () => Math.floor(rnd2(0, sizes[sizes.length - 1])))}}));
      const s = await rt.prepare(spec.program, {resident: spec.resident});
      const losses = [];
      for (const r of (await s.run(feeds, {readback: ['loss']})).steps) losses.push(...r.outputs.loss.data);
      const params = (await s.download(spec.resident)).outputs;
      s.dispose();
      const executed = (await rt.execute(M.mlpTrainingStep({sizes, batch, lr: 0.01, step: 3}).program, {typedOutputs: true})).outputs;
      rt.dispose();
      const b = a => Array.from(new Uint32Array(Float32Array.from(a).buffer));
      out[vectorize] = [b(losses), ...Object.keys(params).sort().map(k => b(params[k].data)), ...Object.keys(executed).sort().map(k => b(executed[k].data))];
    }
    let diff = 0;
    out[true].forEach((a, i) => { for (let j = 0; j < a.length; j++) if (a[j] !== out[false][i][j]) diff++; });
    if (diff) failures++;
    report.push({sizes, batch, diff});
  }
  return {failures, report};
}
