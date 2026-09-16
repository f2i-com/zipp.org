import {check, ComputeError, DEFAULT_LIMITS} from '../graph.mjs';

// Every kernel reads its shape and scalars from this uniform block, so one
// pipeline per kernel serves every shape. No graph value enters shader text.
const PRELUDE = `
struct Params { n: u32, mode: u32, op: u32, len: u32, d: vec4<u32>, sa: vec4<u32>, sb: vec4<u32>, g: vec4<u32>, f: vec4<f32> };
@group(0) @binding(0) var<uniform> P: Params;
fn flat(gid: vec3<u32>, nwg: vec3<u32>) -> u32 { return gid.x + gid.y * nwg.x * 256u; }
fn strided(i: u32, s: vec4<u32>) -> u32 {
  let x3 = i % P.d.w; let r3 = i / P.d.w; let x2 = r3 % P.d.z; let r2 = r3 / P.d.z;
  return (r2 / P.d.y) * s.x + (r2 % P.d.y) * s.y + x2 * s.z + x3 * s.w;
}
fn erf_series(z: f32) -> f32 {
  let t = z * z;
  return z * (1.1283791670955126 + t * (-0.37612638903183754 + t * (0.11283791670955126 + t * (-0.026866170645131252 +
    t * (0.005223977625442188 + t * (-0.0008548327023450852 + t * 0.00012055332981789664))))));
}
fn erfc_fit(a: f32) -> f32 {
  let t = 1.0 / (1.0 + 0.5 * a);
  return t * exp(-a * a - 1.26551223 + t * (1.00002368 + t * (0.37409196 + t * (0.09678418 + t * (-0.18628806 +
    t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277)))))))));
}
fn cdf(x: f32) -> f32 {
  let z = x * 0.7071067811865476;
  if (abs(z) < 0.5) { return 0.5 + 0.5 * erf_series(z); }
  if (z >= 10.0) { return 1.0; }
  if (z <= -10.0) { return 0.0; }
  let c = 0.5 * erfc_fit(abs(z));
  return select(c, 1.0 - c, z > 0.0);
}
fn tanh_s(x: f32) -> f32 {
  if (x != x) { return x; }
  let a = abs(x);
  if (a < 0.25) { let z = x * x; return x * (1.0 + z * (-0.3333333333333333 + z * (0.13333333333333333 + z * (-0.05396825396825397 + z * 0.021869488536155203)))); }
  let t = exp(-2.0 * min(a, 20.0));
  let r = (1.0 - t) / (1.0 + t);
  return select(r, -r, x < 0.0);
}`;
const io = inputs => inputs.map((name, i) => `@group(0) @binding(${i + 1}) var<storage, read> ${name}: array<f32>;`).join('\n') +
  `\n@group(0) @binding(${inputs.length + 1}) var<storage, read_write> O: array<f32>;`;
const each = body => `@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
  let i = flat(gid, nwg);
  if (i >= P.n) { return; }
  ${body}
}`;
const ROW_STATS = `let base = i * P.len;
  var m = A[base];
  for (var j = 1u; j < P.len; j = j + 1u) { let v = A[base + j]; m = select(m, v, v > m || v != v); }
  var s = 0.0;
  for (var j = 0u; j < P.len; j = j + 1u) { s = s + exp(A[base + j] - m); }`;
const KERNELS = {
  fill: [[], each('O[i] = P.f.x;')],
  unary: [['A'], each(`let x = A[i];
  var r: f32;
  switch P.op {
    case 0u: { r = select(0.0, x, x > 0.0 || x != x); }
    case 1u: { r = select(0.0, 1.0, x > 0.0); }
    case 2u: { r = -x; }
    case 3u: { r = exp(x); }
    case 4u: { r = log(x); }
    case 5u: { r = sqrt(x); }
    case 6u: { r = tanh_s(x); }
    case 7u: { let e = exp(-abs(x)); r = select(e / (1.0 + e), 1.0 / (1.0 + e), x >= 0.0); }
    case 8u: { r = x * cdf(x); }
    default: { r = cdf(x) + x * 0.3989422804014327 * exp(-0.5 * min(x * x, 200.0)); }
  }
  O[i] = r;`)],
  binary: [['A', 'B'], each(`var ia = i; var ib = i;
  if (P.mode == 1u) { ia = 0u; } else if (P.mode == 2u) { ib = 0u; } else if (P.mode == 3u) { ia = strided(i, P.sa); ib = strided(i, P.sb); }
  let x = A[ia]; let y = B[ib];
  var r: f32;
  switch P.op { case 0u: { r = x + y; } case 1u: { r = x - y; } case 2u: { r = x * y; } default: { r = x / y; } }
  O[i] = r;`)],
  gather: [['A'], each('O[i] = A[strided(i, P.sa)];')],
  pair: [['A'], each('let j = i * 2u; var other = 0.0; if (j + 1u < P.len) { other = A[j + 1u]; } O[i] = A[j] + other;')],
  scale: [['A'], each('O[i] = A[i] / P.f.x;')],
  reduce: [['A'], each(`let inner = P.g.x; let base = (i / inner) * P.len * inner + i % inner;
  var s = 0.0;
  for (var j = 0u; j < P.len; j = j + 1u) { s = s + A[base + j * inner]; }
  O[i] = select(s, s / f32(P.len), P.mode == 1u);`)],
  softmax: [['A'], each(`${ROW_STATS}
  if (P.mode == 1u) { let ls = log(s); for (var j = 0u; j < P.len; j = j + 1u) { O[base + j] = (A[base + j] - m) - ls; } }
  else { for (var j = 0u; j < P.len; j = j + 1u) { O[base + j] = exp(A[base + j] - m) / s; } }`)],
  ce_rows: [['A', 'T'], each(`${ROW_STATS}
  let t = min(u32(max(T[i], 0.0)), P.len - 1u);
  O[i] = log(s) - (A[base + t] - m);`)],
  ce_grad: [['A', 'T'], each(`${ROW_STATS}
  let t = min(u32(max(T[i], 0.0)), P.len - 1u);
  for (var j = 0u; j < P.len; j = j + 1u) { O[base + j] = (exp(A[base + j] - m) / s - select(0.0, 1.0, j == t)) / f32(P.n); }`)],
  optim: [['A', 'B', 'C'], each(`let a = A[i]; let b = B[i];
  var r: f32;
  switch P.op {
    case 0u: { r = a - P.f.x * b; }
    case 1u: { r = P.f.x * a + P.f.y * b; }
    case 2u: { let w = P.f.x; if (w < 0.5) { r = a + w * (b - a); } else { r = b - (b - a) * (1.0 - w); } }
    case 3u: { r = a * P.f.x + P.f.y * b * b; }
    default: { r = a - P.f.x * (b / (sqrt(C[i]) / P.f.y + P.f.z)); }
  }
  O[i] = r;`)],
  life: [['A'], each(`let h = i32(P.g.x); let w = i32(P.g.y);
  let x = i32(i % P.g.y); let y = i32(i / P.g.y);
  var count = 0u;
  for (var dy = -1; dy <= 1; dy = dy + 1) {
    for (var dx = -1; dx <= 1; dx = dx + 1) {
      if (dx != 0 || dy != 0) { count = count + select(0u, 1u, A[u32(((y + dy + h) % h) * w + (x + dx + w) % w)] > 0.5); }
    }
  }
  O[i] = select(0.0, 1.0, count == 3u || (A[i] > 0.5 && count == 2u));`)],
  // 16x16 output tiles staged through workgroup memory. Each output still
  // accumulates its k products in order; bounds are handled by zero padding
  // so every invocation reaches both barriers.
  matmul: [['A', 'B'], `var<workgroup> As: array<array<f32, 16>, 16>;
var<workgroup> Bs: array<array<f32, 16>, 16>;
@compute @workgroup_size(16, 16)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
  let M = P.g.x; let K = P.g.y; let N = P.g.z;
  let row = wg.y * 16u + lid.y; let col = wg.x * 16u + lid.x;
  let aBase = wg.z * P.sa.x; let bBase = wg.z * P.sa.y;
  var acc = 0.0;
  for (var t = 0u; t < K; t = t + 16u) {
    let ka = t + lid.x; let kb = t + lid.y;
    var av = 0.0; var bv = 0.0;
    if (row < M && ka < K) { av = A[aBase + row * K + ka]; }
    if (kb < K && col < N) { bv = B[bBase + kb * N + col]; }
    As[lid.y][lid.x] = av; Bs[lid.y][lid.x] = bv;
    workgroupBarrier();
    let span = min(16u, K - t);
    for (var k = 0u; k < span; k = k + 1u) { acc = acc + As[lid.y][k] * Bs[k][lid.x]; }
    workgroupBarrier();
  }
  if (row < M && col < N) { O[wg.z * M * N + row * N + col] = acc; }
}`],
};
const UNARY = {relu: 0, positive: 1, neg: 2, exp: 3, log: 4, sqrt: 5, tanh: 6, sigmoid: 7, gelu: 8, gelu_grad: 9};
const BINARY = {add: 0, sub: 1, mul: 2, div: 3}, MODE = {same: 0, aScalar: 1, bScalar: 2, general: 3};
const OPTIM = {sgd_update: 0, momentum_update: 1, adam_m: 2, adam_v: 3, adam_update: 4};
const SLOT = 256, CHUNK = 256 * SLOT, POOL_BYTES = 256 * 1024 * 1024;

export class WebGPUBackend {
  static async create({debug = false} = {}) {
    check(globalThis.navigator?.gpu, 'UNAVAILABLE', 'WebGPU is unavailable (use HTTPS or localhost)');
    const adapter = await navigator.gpu.requestAdapter({powerPreference: 'high-performance'});
    check(adapter, 'UNAVAILABLE', 'No WebGPU adapter is available');
    check(!(adapter.info?.isFallbackAdapter ?? adapter.isFallbackAdapter), 'UNAVAILABLE', 'Hardware WebGPU required; browser returned a fallback adapter');
    // Ask for the storage sizes the adapter supports (bounded), not the portable minimum.
    const requiredLimits = {};
    for (const key of ['maxStorageBufferBindingSize', 'maxBufferSize'])
      if (adapter.limits?.[key]) requiredLimits[key] = Math.min(adapter.limits[key], 1024 * 1024 * 1024);
    const device = await adapter.requestDevice({requiredLimits});
    return new WebGPUBackend(device, adapter.info, {debug});
  }
  constructor(device, info, {debug = false} = {}) {
    this.name = 'webgpu'; this.description = 'WebGPU compute shaders (WGSL)';
    this.device = device; this.debug = debug; this.info = info ? {vendor: info.vendor, architecture: info.architecture,
      description: info.description, isFallbackAdapter: info.isFallbackAdapter ?? null} : null;
    this.pipelines = new Map(); this.lost = null; this.scopeOpen = false;
    // Idle buffers are free across executions; recycled ones were freed during the
    // current one and may still be read by commands recorded before the free.
    this.idle = new Map(); this.recycled = []; this.idleBytes = 0;
    this.chunks = []; this.chunk = 0; this.slot = 0; this.encoder = null; this.pass = null;
    this.peakBufferBytes = 0; this.liveBufferBytes = 0;
    device.lost.then(reason => {this.lost = reason.message || reason.reason || 'Device lost';});
  }
  live() { check(!this.lost, 'DEVICE_LOST', `WebGPU device is unavailable: ${this.lost}`); }
  limitHints() {
    const cap = Math.floor(Math.min(this.device.limits.maxStorageBufferBindingSize, this.device.limits.maxBufferSize) / 4);
    return {maxElements: Math.min(DEFAULT_LIMITS.maxElements, cap), maxWork: 1000000000};
  }
  allocationStats() { return {webgpuBufferPeakBytes: this.peakBufferBytes}; }
  async begin() {
    this.live(); this.device.pushErrorScope('out-of-memory'); this.device.pushErrorScope('validation');
    this.scopeOpen = true; this.chunk = 0; this.slot = 0;
  }
  bytesFor(size) { return Math.max(SLOT, Math.ceil(size * 4 / SLOT) * SLOT); }
  alloc(size, data) {
    this.live();
    check(size * 4 <= this.device.limits.maxStorageBufferBindingSize && size * 4 <= this.device.limits.maxBufferSize,
      'LIMIT', 'Tensor exceeds WebGPU buffer limits');
    const bytes = this.bytesFor(size), list = this.idle.get(bytes);
    let buffer = list?.pop();
    if (buffer) this.idleBytes -= bytes;
    // A buffer freed during this execution is safe for outputs only: a queue write
    // would land before commands recorded earlier in the pending encoder.
    else if (!data) { const i = this.recycled.findIndex(b => b.bytes === bytes); if (i >= 0) buffer = this.recycled.splice(i, 1)[0].buffer; }
    if (buffer) {
      if (data) this.device.queue.writeBuffer(buffer, 0, data.buffer, data.byteOffset, size * 4);
    } else {
      buffer = this.device.createBuffer({size: bytes, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST,
        mappedAtCreation: !!data});
      if (data) {new Float32Array(buffer.getMappedRange(), 0, size).set(data); buffer.unmap();}
      this.liveBufferBytes += bytes; this.peakBufferBytes = Math.max(this.peakBufferBytes, this.liveBufferBytes);
    }
    return {buffer, size, bytes, freed: false};
  }
  async pipeline(name) {
    let pipeline = this.pipelines.get(name);
    if (!pipeline) {
      const [inputs, body] = KERNELS[name];
      const module = this.device.createShaderModule({code: `${PRELUDE}\n${io(inputs)}\n${body}`});
      pipeline = await this.device.createComputePipelineAsync({layout: 'auto', compute: {module, entryPoint: 'main'}});
      this.pipelines.set(name, pipeline);
    }
    return pipeline;
  }
  /** A 256-byte uniform slot for one dispatch; chunks are uploaded once, at submit. */
  uniform(fill) {
    if (this.slot === CHUNK / SLOT) { this.chunk++; this.slot = 0; }
    let chunk = this.chunks[this.chunk];
    if (!chunk) {
      chunk = {buffer: this.device.createBuffer({size: CHUNK, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST}), data: new ArrayBuffer(CHUNK)};
      this.chunks.push(chunk);
    }
    const offset = this.slot++ * SLOT, u = new Uint32Array(chunk.data, offset, 24), f = new Float32Array(chunk.data, offset, 24);
    u.fill(0); fill(u, f);
    return {buffer: chunk.buffer, offset, size: 96};
  }
  async dispatch(kernel, fill, inputs, out, count, groups = null) {
    this.live();
    const pipeline = await this.pipeline(kernel), limit = this.device.limits.maxComputeWorkgroupsPerDimension;
    let [x, y, z] = groups ?? [Math.ceil(count / 256), 1, 1];
    if (!groups && x > limit) { y = Math.ceil(x / limit); x = limit; }
    check(x <= limit && y <= limit && z <= limit, 'LIMIT', 'Dispatch exceeds WebGPU workgroup limit');
    const entries = [{binding: 0, resource: this.uniform(fill)},
      ...[...inputs, out].map((h, i) => ({binding: i + 1, resource: {buffer: h.buffer, size: h.size * 4}}))];
    const group = this.device.createBindGroup({layout: pipeline.getBindGroupLayout(0), entries});
    if (!this.encoder) this.encoder = this.device.createCommandEncoder();
    if (!this.pass) this.pass = this.encoder.beginComputePass();
    this.pass.setPipeline(pipeline); this.pass.setBindGroup(0, group); this.pass.dispatchWorkgroups(x, y, z);
  }
  async pairwise(input, out) {
    const scratch = [];
    try {
      if (input.size === 1) { await this.dispatch('scale', (u, f) => {u[0] = 1; f[20] = 1;}, [input], out, 1); return; }
      while (input.size > 1) {
        const length = Math.ceil(input.size / 2), next = length === 1 ? out : this.alloc(length), len = input.size;
        if (next !== out) scratch.push(next);
        await this.dispatch('pair', u => {u[0] = length; u[3] = len;}, [input], next, length);
        input = next;
      }
    } finally {for (const h of scratch) this.free(h);}
  }
  async run(n, refs) {
    if (n.op === 'input') return this.alloc(n.size, n.data);
    const out = this.alloc(n.size), [a] = refs;
    // Debug mode attributes validation errors to the node that caused them.
    let scoped = this.debug;
    if (scoped) this.device.pushErrorScope('validation');
    try {
      switch (n.op) {
        case 'full': await this.dispatch('fill', (u, f) => {u[0] = n.size; f[20] = n.value;}, [], out, n.size); break;
        case 'add': case 'sub': case 'mul': case 'div':
          await this.dispatch('binary', u => {u[0] = n.size; u[1] = MODE[n.mode]; u[2] = BINARY[n.op];
            if (n.mode === 'general') {u.set(n.dims, 4); u.set(n.aStrides, 8); u.set(n.bStrides, 12);}}, refs, out, n.size); break;
        case 'relu': case 'positive': case 'neg': case 'exp': case 'log': case 'sqrt':
        case 'tanh': case 'sigmoid': case 'gelu': case 'gelu_grad':
          await this.dispatch('unary', u => {u[0] = n.size; u[2] = UNARY[n.op];}, refs, out, n.size); break;
        case 'transpose': case 'permute':
          await this.dispatch('gather', u => {u[0] = n.size; u.set(n.dims, 4); u.set(n.srcStrides, 8);}, refs, out, n.size); break;
        case 'sum': case 'mean':
          if (n.whole) {
            if (n.op === 'sum') { await this.pairwise(a, out); break; }
            const total = this.alloc(1);
            try {
              await this.pairwise(a, total);
              await this.dispatch('scale', (u, f) => {u[0] = 1; f[20] = n.inputSize;}, [total], out, 1);
            } finally {this.free(total);}
          } else await this.dispatch('reduce', u => {u[0] = n.outer * n.inner; u[1] = +(n.op === 'mean'); u[3] = n.len; u[16] = n.inner;}, refs, out, n.outer * n.inner);
          break;
        case 'softmax': case 'log_softmax':
          await this.dispatch('softmax', u => {u[0] = n.rows; u[1] = +(n.op === 'log_softmax'); u[3] = n.cols;}, refs, out, n.rows); break;
        case 'cross_entropy': {
          const rows = this.alloc(n.rows), total = this.alloc(1);
          try {
            await this.dispatch('ce_rows', u => {u[0] = n.rows; u[3] = n.cols;}, refs, rows, n.rows);
            await this.pairwise(rows, total);
            await this.dispatch('scale', (u, f) => {u[0] = 1; f[20] = n.rows;}, [total], out, 1);
          } finally {this.free(rows); this.free(total);}
          break;
        }
        case 'cross_entropy_grad': await this.dispatch('ce_grad', u => {u[0] = n.rows; u[3] = n.cols;}, refs, out, n.rows); break;
        case 'matmul':
          await this.dispatch('matmul', u => {u.set([n.m, n.k, n.n, n.batch], 16); u[8] = n.aBatchStride; u[9] = n.bBatchStride;},
            refs, out, n.size, [Math.ceil(n.n / 16), Math.ceil(n.m / 16), n.batch]); break;
        case 'sgd_update': case 'momentum_update': case 'adam_m': case 'adam_v': case 'adam_update': {
          const scalars = {sgd_update: [n.lr], momentum_update: [n.momentum, n.w], adam_m: [n.w], adam_v: [n.beta2, n.w],
            adam_update: [n.stepSize, n.bc2Sqrt, n.eps]}[n.op];
          await this.dispatch('optim', (u, f) => {u[0] = n.size; u[2] = OPTIM[n.op]; f.set(scalars, 20);},
            [refs[0], refs[1], refs[2] ?? refs[1]], out, n.size); break;
        }
        case 'life': await this.dispatch('life', u => {u[0] = n.size; u[16] = n.shape[0]; u[17] = n.shape[1];}, refs, out, n.size); break;
      }
      if (scoped) {
        scoped = false;
        const error = await this.device.popErrorScope();
        check(!error, 'GPU', `${n.op}: ${error?.message}`);
      }
      return out;
    } catch(error) {
      if (scoped) await this.device.popErrorScope().catch(() => {});
      this.free(out); throw error;
    }
  }
  /** Uploads this execution's uniforms and submits its one command buffer. */
  flush(extra = () => {}) {
    if (this.pass) {this.pass.end(); this.pass = null;}
    if (!this.encoder) this.encoder = this.device.createCommandEncoder();
    extra(this.encoder);
    for (let i = 0; i <= this.chunk && i < this.chunks.length; i++)
      this.device.queue.writeBuffer(this.chunks[i].buffer, 0, this.chunks[i].data, 0, i < this.chunk ? CHUNK : Math.max(this.slot, 1) * SLOT);
    const commands = this.encoder.finish(); this.encoder = null;
    this.device.queue.submit([commands]);
  }
  /** Every requested output through one staging buffer: one submit, one map. */
  async readAll(handles) {
    this.live();
    const offsets = [];let total = 0;
    for (const h of handles) {offsets.push(total); total += h.size * 4;}
    const staging = this.device.createBuffer({size: total, usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ});
    try {
      this.flush(encoder => handles.forEach((h, i) => encoder.copyBufferToBuffer(h.buffer, 0, staging, offsets[i], h.size * 4)));
      await staging.mapAsync(GPUMapMode.READ);
      const all = new Float32Array(staging.getMappedRange());
      return handles.map((h, i) => all.slice(offsets[i] / 4, offsets[i] / 4 + h.size));
    } finally {if (staging.mapState === 'mapped') staging.unmap(); staging.destroy();}
  }
  async read(h) { return (await this.readAll([h]))[0]; }
  free(h) { if (!h.freed) { h.freed = true; this.recycled.push(h); } }
  async finish() {
    if (!this.scopeOpen) return;
    this.scopeOpen = false;
    // Unsubmitted commands (a failed execution) are dropped with their encoder.
    if (this.pass) {this.pass.end(); this.pass = null;}
    this.encoder = null;
    try {
      // Scopes pop in order; their answers and the queue drain are awaited together.
      const [validation, memory] = await Promise.all([this.device.popErrorScope(), this.device.popErrorScope(),
        this.device.queue.onSubmittedWorkDone()]);
      if (validation || memory) throw new ComputeError('GPU', (validation || memory).message);
      this.live();
    } finally {
      for (const h of this.recycled) {
        if (this.idleBytes + h.bytes > POOL_BYTES) {h.buffer.destroy(); this.liveBufferBytes -= h.bytes; continue;}
        if (!this.idle.has(h.bytes)) this.idle.set(h.bytes, []);
        this.idle.get(h.bytes).push(h.buffer); this.idleBytes += h.bytes;
      }
      this.recycled = [];
    }
  }
  dispose() {
    for (const list of this.idle.values()) for (const b of list) b.destroy();
    for (const h of this.recycled) h.buffer.destroy();
    for (const c of this.chunks) c.buffer.destroy();
    this.idle.clear(); this.recycled = []; this.chunks = []; this.pipelines.clear(); this.device.destroy();
  }
}
