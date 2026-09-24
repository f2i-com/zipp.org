// Exactness checks for the WebGPU backend's faster kernels, run natively by
// tests/protocol_cases.rs (and in Chrome the same way): the 16x16 matmul
// kernel's deep-slice variant and the register-blocked tiles against the
// 16x16 kernel on the same device, and the axis-sum
// and index_add kernels against cpu-js, bit for bit (a zero's sign included).
// Each returns {failures, report}.

// The register-blocked tiles checked: the 128x128 one everywhere but the
// native host on Direct3D 12, which never runs it (FXC takes 15 minutes over
// its 64 accumulators, DXC half a minute; zipp-gpu's driver.js). There, too,
// fused matmuls are checked on the tile they choose only: FXC takes ~20 s
// over each 64x64 variant (and the native host keeps the 16x16 kernel on it).
// With FXC the native host also keeps the 16x16 kernel's plain 16-deep
// slices (FXC takes 9 s over the 64-deep variant): checked as it runs there.
const D3D12 = typeof __zgpuAdapter === 'string' && /\(dx12\b/.test(__zgpuAdapter);
const FXC = D3D12 && /\bfxc\)$/.test(__zgpuAdapter);
const TILES = D3D12 ? [4] : [4, 8], FUSED_TILES = D3D12 ? [] : TILES, DEEP = FXC ? [] : [1];
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
    // The reference: the 16x16 kernel staging 16 values of k per step.
    const bits = {};
    impl.deep = false;
    for (const tile of ['ref', ...DEEP, ...TILES]) {
      impl.matmulTile = tile === 'ref' ? 1 : tile;
      const out = await rt.execute(program, {typedOutputs: true});
      bits[tile] = new Uint32Array(out.outputs.r.data.buffer.slice(0));
      impl.deep = true;
    }
    impl.matmulTile = null;
    const row = {a: sa, b: sb, transposed: !!transposed, n: bits.ref.length};
    // r1: the same kernel staging 64 (`_d`); r4, r8: the register-blocked tiles.
    for (const tile of [...DEEP, ...TILES]) {
      let diff = 0, first = -1;
      for (let i = 0; i < bits.ref.length; i++) if (bits.ref[i] !== bits[tile][i]) { diff++; if (first < 0) first = i; }
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

// The WebGPU backend's fused execution (transposes read through by matmuls,
// one-dispatch sums and cross-entropy, elementwise chains) against every node as its own
// kernel, on the same device, bit for bit: matmuls reading one or both
// operands through a transpose (twice-transposed too) or writing their result
// transposed, over ragged and tiled sizes; whole sums and means of 1 to 2048
// values; cross-entropy over 1 to 1024 rows; and a training session shaped
// as torch.compile records one (bias by a ones-matmul, dead gradients, the
// weights' transposes), several steps, then its weights and moments.
async function exactFusion(M) {
  const limits = {maxElements: 1 << 26, maxInputElements: 1 << 26, maxOutputElements: 1 << 26, maxWork: Number.MAX_SAFE_INTEGER, maxLogicalBytes: 8 * 1024 * 1024 * 1024};
  let seed = 99;
  const rnd = () => (seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 4294967296;
  const data = n => Float32Array.from({length: n}, () => { const r = rnd(); return r < 0.04 ? (r < 0.02 ? -0 : 0) : (rnd() - 0.5) * (r < 0.1 ? 40 : 3); });
  // One runtime, the fused and unfused plans by turns (fusion.mjs caches each).
  const rt = await M.createRuntime({backend: 'webgpu', limits});
  rt.impl.deep = !FXC;
  const run = async (program, fuse) => {
    rt.impl.fuse = fuse;
    const out = await rt.execute(program, {typedOutputs: true});
    return Object.keys(out.outputs).sort().map(k => new Uint32Array(out.outputs[k].data.buffer.slice(0)));
  };
  const report = [];
  let failures = 0;
  const compare = async (name, program) => {
    const a = await run(program, true), b = await run(program, false);
    let diff = 0;
    a.forEach((x, i) => { for (let j = 0; j < x.length; j++) if (x[j] !== b[i][j]) diff++; });
    if (diff) failures++;
    report.push({name, diff});
  };
  // Matmuls through transposes: X [m,k], Y [k,n] given as stored transposes.
  for (const [m, k, n] of [[1, 1, 1], [3, 5, 7], [17, 13, 9], [64, 784, 256], [256, 64, 784], [130, 257, 70], [300, 700, 1], [1024, 1024, 1024], [5, 4096, 33]]) {
    const nodes = [{id: 0, op: 'input', shape: [k, m], data: data(m * k)}, {id: 1, op: 'input', shape: [n, k], data: data(k * n)},
      {id: 2, op: 'input', shape: [m, k], data: data(m * k)}, {id: 3, op: 'input', shape: [k, n], data: data(k * n)},
      {id: 4, op: 'transpose', a: 0}, {id: 5, op: 'transpose', a: 1}, {id: 6, op: 'transpose', a: 5},
      {id: 7, op: 'matmul', a: 4, b: 5}, {id: 8, op: 'matmul', a: 4, b: 3}, {id: 9, op: 'matmul', a: 2, b: 5},
      {id: 10, op: 'transpose', a: 6}, {id: 11, op: 'matmul', a: 2, b: 1, transposed: true}, {id: 12, op: 'transpose', a: 11},
      {id: 13, op: 'matmul', a: 4, b: 10}, {id: 14, op: 'transpose', a: 13}];
    // Each tile (the chosen one, and 4 and 8 pinned) reading through the transposes.
    for (const tile of [null, 'shallow', ...FUSED_TILES]) {
      if (typeof tile === 'number' && m * n < 4096) continue;
      rt.impl.matmulTile = typeof tile === 'number' ? tile : null;
      rt.impl.deep = tile !== 'shallow' && !FXC;
      await compare(`matmul ${m}x${k}x${n} through transposes${tile ? `, tile ${tile}` : ''}`, {version: 2, nodes,
        outputs: [{name: 'a', id: 7}, {name: 'b', id: 8}, {name: 'c', id: 9}, {name: 'd', id: 12}, {name: 'e', id: 14}]});
    }
    rt.impl.matmulTile = null;
    rt.impl.deep = !FXC;
  }
  for (const n of [1, 2, 3, 63, 64, 65, 255, 256, 257, 1000, 1023, 1024, 1025, 2047, 2048]) {
    const nodes = [{id: 0, op: 'input', shape: [n], data: data(n)}, {id: 1, op: 'sum', a: 0}, {id: 2, op: 'mean', a: 0}];
    await compare(`sum and mean of ${n}`, {version: 2, nodes, outputs: [{name: 's', id: 1}, {name: 'm', id: 2}]});
  }
  for (const [rows, cols] of [[1, 3], [2, 10], [64, 10], [255, 7], [256, 1000], [257, 5], [1000, 17], [1024, 2]]) {
    const nodes = [{id: 0, op: 'input', shape: [rows, cols], data: data(rows * cols)},
      {id: 1, op: 'input', shape: [rows], data: Float32Array.from({length: rows}, () => Math.floor(rnd() * cols))},
      {id: 2, op: 'cross_entropy', a: 0, b: 1}];
    await compare(`cross_entropy ${rows}x${cols}`, {version: 2, nodes, outputs: [{name: 'l', id: 2}]});
    // With its gradient (one dispatch up to 256 rows): alone, and times a
    // scalar on either side of the product.
    nodes.push({id: 3, op: 'cross_entropy_grad', a: 0, b: 1}, {id: 4, op: 'input', shape: [], data: [0.75]},
      {id: 5, op: 'cross_entropy_grad', a: 0, b: 1}, {id: 6, op: 'mul', a: 4, b: 5},
      {id: 7, op: 'cross_entropy', a: 0, b: 1}, {id: 8, op: 'cross_entropy_grad', a: 0, b: 1}, {id: 9, op: 'mul', a: 8, b: 4});
    await compare(`cross_entropy ${rows}x${cols} with its gradient`, {version: 2, nodes, outputs: [{name: 'l', id: 2}, {name: 'g', id: 3}]});
    await compare(`cross_entropy ${rows}x${cols} with its gradient times a scalar`, {version: 2, nodes,
      outputs: [{name: 'l', id: 2}, {name: 'g', id: 6}, {name: 'm', id: 7}, {name: 'h', id: 9}]});
  }
  // Elementwise chains: a product into a sum (x*y + z), scalar operands on
  // either side, a member read twice, members read after the chain, a K=1
  // matmul broadcast (a bias) into an add and an activation.
  for (const [r, c] of [[1, 1], [3, 5], [64, 256], [97, 1031]]) {
    const n = r * c;
    const nodes = [{id: 0, op: 'input', shape: [r, c], data: data(n)}, {id: 1, op: 'input', shape: [r, c], data: data(n)},
      {id: 2, op: 'input', shape: [r, c], data: data(n)}, {id: 3, op: 'input', shape: [], data: [1.375]},
      {id: 4, op: 'mul', a: 0, b: 1}, {id: 5, op: 'add', a: 4, b: 2}, {id: 6, op: 'mul', a: 5, b: 3}, {id: 7, op: 'sub', a: 3, b: 6},
      {id: 8, op: 'tanh', a: 7}, {id: 9, op: 'mul', a: 8, b: 8}, {id: 10, op: 'sigmoid', a: 9}, {id: 11, op: 'maximum', a: 10, b: 5},
      {id: 12, op: 'input', shape: [r, 1], data: data(r)}, {id: 13, op: 'input', shape: [1, c], data: data(c)},
      {id: 14, op: 'matmul', a: 12, b: 13}, {id: 15, op: 'add', a: 0, b: 14}, {id: 16, op: 'relu', a: 15}, {id: 17, op: 'gelu', a: 16},
      {id: 18, op: 'div', a: 17, b: 3}, {id: 19, op: 'exp', a: 10}, {id: 20, op: 'gt', a: 19, b: 3}, {id: 21, op: 'mul', a: 20, b: 15}];
    await compare(`chains ${r}x${c}`, {version: 3, nodes,
      outputs: [{name: 'a', id: 11}, {name: 'b', id: 18}, {name: 'c', id: 21}, {name: 'd', id: 6}]});
  }
  // Matmuls computed in the chain that reads them: a layer (x W^T + b,
  // activation), a gradient through an activation's mask, plain and through
  // transposes, ragged sizes, split along k (summed in the chain's kernel),
  // and one whose product another node also reads.
  for (const [m, k, n] of [[1, 1, 1], [5, 3, 7], [64, 256, 10], [33, 100, 257], [64, 64, 64], [17, 300, 31], [64, 784, 256], [16, 2048, 100], [3, 5000, 9]]) {
    const nodes = [{id: 0, op: 'input', shape: [m, k], data: data(m * k)}, {id: 1, op: 'input', shape: [n, k], data: data(n * k)},
      {id: 2, op: 'transpose', a: 1}, {id: 3, op: 'matmul', a: 0, b: 2}, {id: 4, op: 'input', shape: [m, 1], data: new Float32Array(m).fill(1)},
      {id: 5, op: 'input', shape: [1, n], data: data(n)}, {id: 6, op: 'matmul', a: 4, b: 5}, {id: 7, op: 'add', a: 3, b: 6}, {id: 8, op: 'relu', a: 7},
      {id: 9, op: 'input', shape: [m, k], data: data(m * k)}, {id: 10, op: 'matmul', a: 9, b: 1, transposed: true},
      {id: 11, op: 'positive', a: 7}, {id: 12, op: 'mul', a: 10, b: 11},
      {id: 13, op: 'input', shape: [k, m], data: data(m * k)}, {id: 14, op: 'transpose', a: 13}, {id: 15, op: 'matmul', a: 14, b: 2},
      {id: 16, op: 'tanh', a: 15}, {id: 17, op: 'exp', a: 16}, {id: 18, op: 'matmul', a: 0, b: 2}, {id: 19, op: 'neg', a: 18}, {id: 20, op: 'sigmoid', a: 19}];
    await compare(`matmul ${m}x${k}x${n} into chains`, {version: 2, nodes,
      outputs: [{name: 'a', id: 8}, {name: 'b', id: 12}, {name: 'c', id: 17}, {name: 'd', id: 20}, {name: 'e', id: 18}]});
  }
  // A chain reading more inputs than one shader may bind (split in two).
  {
    const nodes = Array.from({length: 12}, (_, i) => ({id: i, op: 'input', shape: [33, 7], data: data(231)}));
    for (let i = 0; i < 11; i++) nodes.push({id: 12 + i, op: i % 2 ? 'mul' : 'add', a: i ? 11 + i : 0, b: i + 1});
    await compare('chain of 12 inputs', {version: 2, nodes, outputs: [{name: 'r', id: 22}, {name: 's', id: 17}]});
  }
  // A torch.compile-shaped training session: several steps, then the state.
  const [I, H, C, B] = [40, 24, 5, 16];
  const W1 = data(H * I).map(v => v / 8), W2 = data(C * H).map(v => v / 8), b1 = data(H).map(v => v / 8), b2 = data(C).map(v => v / 8);
  const nodes = [{id: 0, op: 'input', shape: [B, I]}, {id: 1, op: 'input', shape: [H, I], data: W1, carry: 'w0'},
    {id: 2, op: 'transpose', a: 1}, {id: 3, op: 'matmul', a: 0, b: 2}, {id: 4, op: 'input', shape: [1, H], data: b1, carry: 'w1'},
    {id: 5, op: 'full', shape: [B, 1], value: 1}, {id: 6, op: 'matmul', a: 5, b: 4}, {id: 7, op: 'add', a: 3, b: 6}, {id: 8, op: 'relu', a: 7},
    {id: 9, op: 'input', shape: [C, H], data: W2, carry: 'w2'}, {id: 10, op: 'transpose', a: 9}, {id: 11, op: 'matmul', a: 8, b: 10},
    {id: 12, op: 'input', shape: [1, C], data: b2, carry: 'w3'}, {id: 13, op: 'full', shape: [B, 1], value: 1}, {id: 14, op: 'matmul', a: 13, b: 12},
    {id: 15, op: 'add', a: 11, b: 14}, {id: 16, op: 'input', shape: [B]}, {id: 17, op: 'cross_entropy', a: 15, b: 16},
    {id: 18, op: 'full', shape: [], value: 1}, {id: 19, op: 'cross_entropy_grad', a: 15, b: 16}, {id: 20, op: 'mul', a: 18, b: 19},
    {id: 21, op: 'transpose', a: 12}, {id: 22, op: 'matmul', a: 20, b: 21}, {id: 23, op: 'transpose', a: 13}, {id: 24, op: 'matmul', a: 23, b: 20},
    {id: 25, op: 'transpose', a: 10}, {id: 26, op: 'matmul', a: 20, b: 25}, {id: 27, op: 'transpose', a: 8}, {id: 28, op: 'matmul', a: 27, b: 20},
    {id: 29, op: 'transpose', a: 28}, {id: 30, op: 'positive', a: 7}, {id: 31, op: 'mul', a: 26, b: 30}, {id: 32, op: 'transpose', a: 4},
    {id: 33, op: 'matmul', a: 31, b: 32}, {id: 34, op: 'transpose', a: 5}, {id: 35, op: 'matmul', a: 34, b: 31}, {id: 36, op: 'transpose', a: 2},
    {id: 37, op: 'matmul', a: 31, b: 36}, {id: 38, op: 'transpose', a: 0}, {id: 39, op: 'matmul', a: 38, b: 31}, {id: 40, op: 'transpose', a: 39}];
  const outputs = [{name: 'result', id: 17}, {name: 'g0', id: 40}, {name: 'g1', id: 35}, {name: 'g2', id: 29}, {name: 'g3', id: 24}];
  const grads = [[1, 40, [H, I]], [4, 35, [1, H]], [9, 29, [C, H]], [12, 24, [1, C]]];
  grads.forEach(([p, g, shape], i) => {
    const at = nodes.length;
    nodes.push({id: at, op: 'input', shape, data: new Float32Array(shape[0] * shape[1]), carry: `m${i}`},
      {id: at + 1, op: 'input', shape, data: new Float32Array(shape[0] * shape[1]), carry: `v${i}`},
      {id: at + 2, op: 'adam_m', a: at, b: g, beta1: 0.9}, {id: at + 3, op: 'adam_v', a: at + 1, b: g, beta2: 0.999},
      {id: at + 4, op: 'adam_update', a: p, b: at + 2, c: at + 3, lr: 0.01, beta1: 0.9, beta2: 0.999, eps: 1e-8, step: 1});
    outputs.push({name: `w${i}`, id: at + 4}, {name: `m${i}`, id: at + 2}, {name: `v${i}`, id: at + 3});
  });
  const resident = outputs.map(o => o.name).filter(n => n !== 'result');
  const feeds = Array.from({length: 6}, () => ({inputs: {0: data(B * I), 16: Float32Array.from({length: B}, () => Math.floor(rnd() * C))}}));
  const bits = {};
  for (const fuse of [true, false]) {
    rt.impl.fuse = fuse;
    const s = await rt.prepare({version: 2, nodes, outputs}, {resident});
    const losses = [];
    for (const f of feeds.slice(0, 2)) losses.push(...(await s.run(f, {readback: ['result']})).outputs.result.data);
    for (const r of (await s.run(feeds.slice(2), {readback: ['result']})).steps) losses.push(...r.outputs.result.data);
    const state = (await s.download(resident)).outputs;
    s.dispose();
    bits[fuse] = [Float32Array.from(losses), ...resident.map(n => state[n].data)].map(a => new Uint32Array(Float32Array.from(a).buffer));
  }
  rt.dispose();
  let diff = 0;
  bits[true].forEach((a, i) => { for (let j = 0; j < a.length; j++) if (a[j] !== bits[false][i][j]) diff++; });
  if (diff) failures++;
  report.push({name: 'torch-shaped training session, 6 steps', diff});
  return {failures, report};
}
