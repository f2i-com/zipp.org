import {check, ComputeError, DEFAULT_LIMITS} from '../graph.mjs';
const UNSUPPORTED=new Set(['matmul_fixed']);

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
// NaN by bits: WGSL lets a compiler assume no NaN reaches a comparison.
fn is_nan(x: f32) -> bool { return (bitcast<u32>(x) & 0x7fffffffu) > 0x7f800000u; }
// Binary operations by code: arithmetic, then maximum/minimum (NaN from either
// side, a tie to x) and 0/1 comparisons (NaN compares unequal).
fn binop(o: u32, x: f32, y: f32) -> f32 {
  switch o {
    case 0u: { return x + y; }
    case 1u: { return x - y; }
    case 2u: { return x * y; }
    case 3u: { return x / y; }
    default: {}
  }
  let unordered = is_nan(x) || is_nan(y);
  // A tie (-0 against +0 included) goes to x before any ordering is asked,
  // so no compiler rewrite into max()/min() can pick the zero's sign.
  if (o == 4u) { return select(select(select(x, y, x < y), x, x == y), x + y, unordered); }
  if (o == 5u) { return select(select(select(x, y, y < x), x, x == y), x + y, unordered); }
  if (o == 7u) { return select(0.0, 1.0, unordered || x != y); }
  if (unordered) { return 0.0; }
  switch o {
    case 6u: { return select(0.0, 1.0, x == y); }
    case 8u: { return select(0.0, 1.0, x < y); }
    case 9u: { return select(0.0, 1.0, x <= y); }
    case 10u: { return select(0.0, 1.0, x > y); }
    default: { return select(0.0, 1.0, x >= y); }
  }
}
// The uniform generator's hash: lowbias32 in u32 arithmetic, which wraps.
fn mix32(v: u32) -> u32 {
  var x = v;
  x = x ^ (x >> 16u); x = x * 0x7feb352du; x = x ^ (x >> 15u); x = x * 0x846ca68bu; return x ^ (x >> 16u);
}
fn tanh_s(x: f32) -> f32 {
  if (x != x) { return x; }
  let a = abs(x);
  if (a < 0.25) { let z = x * x; return x * (1.0 + z * (-0.3333333333333333 + z * (0.13333333333333333 + z * (-0.05396825396825397 + z * 0.021869488536155203)))); }
  let t = exp(-2.0 * min(a, 20.0));
  let r = (1.0 - t) / (1.0 + t);
  return select(r, -r, x < 0.0);
}`;
// A binds as array<f32>; B:u32 binds the same buffer as raw words, which is
// how a quantized weight arrives -- blocks, not values.
const io = inputs => inputs.map((name, i) => {
  const [id, type = 'f32'] = name.split(':');
  return `@group(0) @binding(${i + 1}) var<storage, read> ${id}: array<${type}>;`;
}).join('\n') + `\n@group(0) @binding(${inputs.length + 1}) var<storage, read_write> O: array<f32>;`;
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
  O[i] = binop(P.op, A[ia], B[ib]);`)],
  // where(c, a, b) (C = condition): nonzero bits (NaN included) pick A. g holds b's strides.
  where: [['C', 'A', 'B'], each(`var ic = i; var ia = i; var ib = i;
  if (P.mode == 3u) { ic = strided(i, P.sa); ia = strided(i, P.sb); ib = strided(i, P.g); }
  O[i] = select(B[ib], A[ia], (bitcast<u32>(C[ic]) & 0x7fffffffu) != 0u);`)],
  // Counter-based uniform numbers: g.x the seed, g.y the step.
  uniform: [[], each(`let k1 = mix32(P.g.x ^ 0x9e3779b9u); let k2 = mix32(P.g.y ^ k1);
  O[i] = f32(mix32(mix32(i ^ k2) + k1) >> 8u) * 5.9604644775390625e-8;`)],
  gather: [['A'], each('O[i] = A[strided(i, P.sa)];')],
  // Version 4. slice: the strided gather from the box's first element (g.x);
  // negative strides arrive as two's-complement words and u32 arithmetic wraps
  // modulo 2^32, so the sum is the signed one.
  slice: [['A'], each('O[i] = A[P.g.x + strided(i, P.sa)];')],
  // slice_scatter over the base's padded dims (d): inside the box (begin sa,
  // signed stride sb, extent g) the source's value, elsewhere the base's.
  slice_scatter: [['A', 'B'], each(`let x3 = i % P.d.w; let r3 = i / P.d.w; let x2 = r3 % P.d.z; let r2 = r3 / P.d.z;
  let x = vec4<i32>(i32(r2 / P.d.y), i32(r2 % P.d.y), i32(x2), i32(x3));
  let begin = bitcast<vec4<i32>>(P.sa); let stride = bitcast<vec4<i32>>(P.sb); let ext = vec4<i32>(P.g);
  var q = vec4<i32>(0); var inside = true;
  for (var k = 0; k < 4; k = k + 1) {
    let s = abs(stride[k]); let t = select(begin[k] - x[k], x[k] - begin[k], stride[k] > 0);
    if (t < 0 || t % s != 0 || t / s >= ext[k]) { inside = false; break; }
    q[k] = t / s;
  }
  if (inside) { O[i] = B[u32(((q.x * ext.y + q.y) * ext.z + q.z) * ext.w + q.w)]; } else { O[i] = A[i]; }`)],
  // index_select over [outer, N = g.x, inner = g.y]; len is the index's (B) length.
  index_select: [['A', 'B'], each(`let inner = P.g.y; let r = i % inner; let k = (i / inner) % P.len; let o = i / (inner * P.len);
  let p = u32(clamp(i32(B[k]), 0, i32(P.g.x) - 1));
  O[i] = A[(o * P.g.x + p) * inner + r];`)],
  // index_add: the base (A) plus every source row (B, [outer, len, inner]) whose
  // index (C) names this position, in ascending k.
  index_add: [['A', 'B', 'C'], each(`let inner = P.g.y; let r = i % inner; let p = (i / inner) % P.g.x; let o = i / (inner * P.g.x);
  var v = A[i];
  for (var k = 0u; k < P.len; k = k + 1u) { if (i32(C[k]) == i32(p)) { v = v + B[(o * P.len + k) * inner + r]; } }
  O[i] = v;`)],
  // gather over the index's padded dims (d): a's strides with the axis zeroed
  // (sa), plus the index value (B, clamped below g.y) times the axis stride g.x.
  gather_axis: [['A', 'B'], each('O[i] = A[strided(i, P.sa) + u32(clamp(i32(B[i]), 0, i32(P.g.y) - 1)) * P.g.x];')],
  // scatter_add over the base's padded dims (d): the index's padded dims (sa)
  // and the padded axis (g.x); every index element along the axis at this
  // position off the axis that names it adds its source, in ascending order.
  // The index element at k along the axis is j0 + k * its axis stride (no
  // vector written through a runtime index: Direct3D's FXC refuses that).
  scatter_add: [['A', 'B', 'C'], each(`let x3 = i % P.d.w; let r3 = i / P.d.w; let x2 = r3 % P.d.z; let r2 = r3 / P.d.z;
  let x = vec4<u32>(r2 / P.d.y, r2 % P.d.y, x2, x3);
  let axis = P.g.x;
  let onAxis = vec4<bool>(axis == 0u, axis == 1u, axis == 2u, axis == 3u);
  var v = A[i];
  if (!any(select(x >= P.sa, vec4<bool>(false), onAxis))) {
    let q = select(x, vec4<u32>(0u), onAxis);
    let j0 = ((q.x * P.sa.y + q.y) * P.sa.z + q.z) * P.sa.w + q.w;
    let strides = vec4<u32>(P.sa.y * P.sa.z * P.sa.w, P.sa.z * P.sa.w, P.sa.w, 1u);
    let skip = strides[axis]; let count = P.sa[axis]; let want = i32(x[axis]);
    for (var k = 0u; k < count; k = k + 1u) {
      let j = j0 + k * skip;
      if (i32(C[j]) == want) { v = v + B[j]; }
    }
  }
  O[i] = v;`)],
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
  // The same tile, over a weight stored [N, K]: one contiguous row per
  // output column, which is how a checkpoint writes a linear layer.
  matmul_t: [['A', 'B'], `var<workgroup> As: array<array<f32, 16>, 16>;
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
    if (kb < K && col < N) { bv = B[bBase + col * K + kb]; }
    As[lid.y][lid.x] = av; Bs[lid.y][lid.x] = bv;
    workgroupBarrier();
    let span = min(16u, K - t);
    for (var k = 0u; k < span; k = k + 1u) { acc = acc + As[lid.y][k] * Bs[k][lid.x]; }
    workgroupBarrier();
  }
  if (row < M && col < N) { O[wg.z * M * N + row * N + col] = acc; }
}`],
  // And over a weight that is still Q4_K blocks: q4k decodes the one value
  // the tile is about to load, so the device holds 144 bytes per 256 weights
  // and never the 1024 they expand to.
  matmul_q4k: [['A', 'B:u32'], `// Block-quantized weights, decoded where they are used.
//
// Ports of dequantize_row_q4_K and dequantize_row_q6_K; src/quant.mjs is the
// same thing in JavaScript and the gguf-quants crate is it in Rust. Addressing is by
// absolute byte, not by word: a Q6_K block is 210 bytes, so blocks after the
// first are not word-aligned and a word-relative accessor would be wrong.
fn qbyte(off: u32) -> u32 { return (B[off >> 2u] >> ((off & 3u) * 8u)) & 0xffu; }
// One f16 from two bytes that may straddle a word.
fn qhalf(off: u32) -> f32 { return unpack2x16float(qbyte(off) | (qbyte(off + 1u) << 8u)).x; }
// Where element e of row col begins, for a format of bytes per 256 values.
fn qbase(block0: u32, col: u32, K: u32, e: u32, bytes: u32) -> u32 {
  return (block0 + col * (K / 256u) + e / 256u) * bytes;
}
// Q4_K: f16 scale and minimum, eight 6-bit sub-block scales and minimums packed
// into 12 bytes, then 256 nibbles. Sub-blocks 0..3 are a plain six bits; 4..7
// borrow their high two bits from the bytes of the first four.
fn q4k_scale_min(base: u32, j: u32) -> vec2<f32> {
  if (j < 4u) { return vec2<f32>(f32(qbyte(base + 4u + j) & 63u), f32(qbyte(base + 8u + j) & 63u)); }
  let hi = qbyte(base + j + 8u);
  let lo = qbyte(base + j);
  let me = qbyte(base + j + 4u);
  return vec2<f32>(f32((hi & 15u) | ((lo >> 6u) << 4u)), f32((hi >> 4u) | ((me >> 6u) << 4u)));
}
fn q4k(block0: u32, col: u32, K: u32, e: u32) -> f32 {
  let base = qbase(block0, col, K, e, 144u);
  let within = e % 256u;
  let pair = within / 64u;
  let rem = within % 64u;
  let byte = qbyte(base + 16u + pair * 32u + rem % 32u);
  let nibble = select(byte >> 4u, byte & 15u, rem < 32u);
  let j = pair * 2u + select(1u, 0u, rem < 32u);
  let sm = q4k_scale_min(base, j);
  return (qhalf(base) * sm.x) * f32(nibble) - qhalf(base + 2u) * sm.y;
}
// Q6_K: 128 low nibbles, 64 bytes of high pairs, sixteen signed group scales,
// then the f16 super-block scale. Each value is (low4 | high2 << 4) - 32.
fn q6k(block0: u32, col: u32, K: u32, e: u32) -> f32 {
  let base = qbase(block0, col, K, e, 210u);
  let within = e % 256u;
  let half = within / 128u;
  let rem = within % 128u;
  let sub = rem / 32u;
  let l = rem % 32u;
  let ql = qbyte(base + 64u * half + l + 32u * (sub & 1u));
  let low = select(ql >> 4u, ql & 15u, sub < 2u);
  let high = (qbyte(base + 128u + 32u * half + l) >> (2u * sub)) & 3u;
  let scale = f32(bitcast<i32>(qbyte(base + 192u + 8u * half + l / 16u + 2u * sub) << 24u) >> 24u);
  return (qhalf(base + 208u) * scale) * f32(bitcast<i32>(low | (high << 4u)) - 32);
}
var<workgroup> As: array<array<f32, 16>, 16>;
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
    if (kb < K && col < N) { bv = q4k(bBase / 256u, col, K, kb); }
    As[lid.y][lid.x] = av; Bs[lid.y][lid.x] = bv;
    workgroupBarrier();
    let span = min(16u, K - t);
    for (var k = 0u; k < span; k = k + 1u) { acc = acc + As[lid.y][k] * Bs[k][lid.x]; }
    workgroupBarrier();
  }
  if (row < M && col < N) { O[wg.z * M * N + row * N + col] = acc; }
}`],
  matmul_q6k: [['A', 'B:u32'], `// Block-quantized weights, decoded where they are used.
//
// Ports of dequantize_row_q4_K and dequantize_row_q6_K; src/quant.mjs is the
// same thing in JavaScript and the gguf-quants crate is it in Rust. Addressing is by
// absolute byte, not by word: a Q6_K block is 210 bytes, so blocks after the
// first are not word-aligned and a word-relative accessor would be wrong.
fn qbyte(off: u32) -> u32 { return (B[off >> 2u] >> ((off & 3u) * 8u)) & 0xffu; }
// One f16 from two bytes that may straddle a word.
fn qhalf(off: u32) -> f32 { return unpack2x16float(qbyte(off) | (qbyte(off + 1u) << 8u)).x; }
// Where element e of row col begins, for a format of bytes per 256 values.
fn qbase(block0: u32, col: u32, K: u32, e: u32, bytes: u32) -> u32 {
  return (block0 + col * (K / 256u) + e / 256u) * bytes;
}
// Q4_K: f16 scale and minimum, eight 6-bit sub-block scales and minimums packed
// into 12 bytes, then 256 nibbles. Sub-blocks 0..3 are a plain six bits; 4..7
// borrow their high two bits from the bytes of the first four.
fn q4k_scale_min(base: u32, j: u32) -> vec2<f32> {
  if (j < 4u) { return vec2<f32>(f32(qbyte(base + 4u + j) & 63u), f32(qbyte(base + 8u + j) & 63u)); }
  let hi = qbyte(base + j + 8u);
  let lo = qbyte(base + j);
  let me = qbyte(base + j + 4u);
  return vec2<f32>(f32((hi & 15u) | ((lo >> 6u) << 4u)), f32((hi >> 4u) | ((me >> 6u) << 4u)));
}
fn q4k(block0: u32, col: u32, K: u32, e: u32) -> f32 {
  let base = qbase(block0, col, K, e, 144u);
  let within = e % 256u;
  let pair = within / 64u;
  let rem = within % 64u;
  let byte = qbyte(base + 16u + pair * 32u + rem % 32u);
  let nibble = select(byte >> 4u, byte & 15u, rem < 32u);
  let j = pair * 2u + select(1u, 0u, rem < 32u);
  let sm = q4k_scale_min(base, j);
  return (qhalf(base) * sm.x) * f32(nibble) - qhalf(base + 2u) * sm.y;
}
// Q6_K: 128 low nibbles, 64 bytes of high pairs, sixteen signed group scales,
// then the f16 super-block scale. Each value is (low4 | high2 << 4) - 32.
fn q6k(block0: u32, col: u32, K: u32, e: u32) -> f32 {
  let base = qbase(block0, col, K, e, 210u);
  let within = e % 256u;
  let half = within / 128u;
  let rem = within % 128u;
  let sub = rem / 32u;
  let l = rem % 32u;
  let ql = qbyte(base + 64u * half + l + 32u * (sub & 1u));
  let low = select(ql >> 4u, ql & 15u, sub < 2u);
  let high = (qbyte(base + 128u + 32u * half + l) >> (2u * sub)) & 3u;
  let scale = f32(bitcast<i32>(qbyte(base + 192u + 8u * half + l / 16u + 2u * sub) << 24u) >> 24u);
  return (qhalf(base + 208u) * scale) * f32(bitcast<i32>(low | (high << 4u)) - 32);
}
var<workgroup> As: array<array<f32, 16>, 16>;
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
    if (kb < K && col < N) { bv = q6k(bBase / 256u, col, K, kb); }
    As[lid.y][lid.x] = av; Bs[lid.y][lid.x] = bv;
    workgroupBarrier();
    let span = min(16u, K - t);
    for (var k = 0u; k < span; k = k + 1u) { acc = acc + As[lid.y][k] * Bs[k][lid.x]; }
    workgroupBarrier();
  }
  if (row < M && col < N) { O[wg.z * M * N + row * N + col] = acc; }
}`],
  // Split along the reduced axis, then sum the parts.
  //
  // A decode step multiplies [1, 1024] by [2048, 1024]. The tile above is
  // 16 by 16, so fifteen of every sixteen rows in it do nothing and the whole
  // product is 128 workgroups -- far too few to fill a GPU, and measurably
  // slower per weight than one matmul with a hundred times more outputs.
  // Slicing the reduced axis multiplies the workgroups by the slice count,
  // and `reduce` adds the slices back. Within a slice nothing about the
  // arithmetic changes, so quantized still equals decoded here exactly.
  matmul_split: [['A', 'B'], `var<workgroup> As: array<array<f32, 16>, 16>;
var<workgroup> Bs: array<array<f32, 16>, 16>;
@compute @workgroup_size(16, 16)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
  let M = P.g.x; let K = P.g.y; let N = P.g.z;
  let batches = max(P.g.w, 1u);
  let part = wg.z / batches; let b = wg.z % batches;
  let row = wg.y * 16u + lid.y; let col = wg.x * 16u + lid.x;
  let aBase = b * P.sa.x; let bBase = b * P.sa.y;
  let k0 = part * P.sa.z; let k1 = min(K, k0 + P.sa.z);
  var acc = 0.0;
  for (var t = k0; t < k1; t = t + 16u) {
    let ka = t + lid.x; let kb = t + lid.y;
    var av = 0.0; var bv = 0.0;
    if (row < M && ka < k1) { av = A[aBase + row * K + ka]; }
    if (kb < k1 && col < N) { bv = B[bBase + kb * N + col]; }
    As[lid.y][lid.x] = av; Bs[lid.y][lid.x] = bv;
    workgroupBarrier();
    let span = min(16u, k1 - t);
    for (var k = 0u; k < span; k = k + 1u) { acc = acc + As[lid.y][k] * Bs[k][lid.x]; }
    workgroupBarrier();
  }
  if (row < M && col < N) { O[(part * batches + b) * M * N + row * N + col] = acc; }
}`],
  matmul_t_split: [['A', 'B'], `var<workgroup> As: array<array<f32, 16>, 16>;
var<workgroup> Bs: array<array<f32, 16>, 16>;
@compute @workgroup_size(16, 16)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
  let M = P.g.x; let K = P.g.y; let N = P.g.z;
  let batches = max(P.g.w, 1u);
  let part = wg.z / batches; let b = wg.z % batches;
  let row = wg.y * 16u + lid.y; let col = wg.x * 16u + lid.x;
  let aBase = b * P.sa.x; let bBase = b * P.sa.y;
  let k0 = part * P.sa.z; let k1 = min(K, k0 + P.sa.z);
  var acc = 0.0;
  for (var t = k0; t < k1; t = t + 16u) {
    let ka = t + lid.x; let kb = t + lid.y;
    var av = 0.0; var bv = 0.0;
    if (row < M && ka < k1) { av = A[aBase + row * K + ka]; }
    if (kb < k1 && col < N) { bv = B[bBase + col * K + kb]; }
    As[lid.y][lid.x] = av; Bs[lid.y][lid.x] = bv;
    workgroupBarrier();
    let span = min(16u, k1 - t);
    for (var k = 0u; k < span; k = k + 1u) { acc = acc + As[lid.y][k] * Bs[k][lid.x]; }
    workgroupBarrier();
  }
  if (row < M && col < N) { O[(part * batches + b) * M * N + row * N + col] = acc; }
}`],
  matmul_q4k_split: [['A', 'B:u32'], `// Block-quantized weights, decoded where they are used.
//
// Ports of dequantize_row_q4_K and dequantize_row_q6_K; src/quant.mjs is the
// same thing in JavaScript and the gguf-quants crate is it in Rust. Addressing is by
// absolute byte, not by word: a Q6_K block is 210 bytes, so blocks after the
// first are not word-aligned and a word-relative accessor would be wrong.
fn qbyte(off: u32) -> u32 { return (B[off >> 2u] >> ((off & 3u) * 8u)) & 0xffu; }
// One f16 from two bytes that may straddle a word.
fn qhalf(off: u32) -> f32 { return unpack2x16float(qbyte(off) | (qbyte(off + 1u) << 8u)).x; }
// Where element e of row col begins, for a format of bytes per 256 values.
fn qbase(block0: u32, col: u32, K: u32, e: u32, bytes: u32) -> u32 {
  return (block0 + col * (K / 256u) + e / 256u) * bytes;
}
// Q4_K: f16 scale and minimum, eight 6-bit sub-block scales and minimums packed
// into 12 bytes, then 256 nibbles. Sub-blocks 0..3 are a plain six bits; 4..7
// borrow their high two bits from the bytes of the first four.
fn q4k_scale_min(base: u32, j: u32) -> vec2<f32> {
  if (j < 4u) { return vec2<f32>(f32(qbyte(base + 4u + j) & 63u), f32(qbyte(base + 8u + j) & 63u)); }
  let hi = qbyte(base + j + 8u);
  let lo = qbyte(base + j);
  let me = qbyte(base + j + 4u);
  return vec2<f32>(f32((hi & 15u) | ((lo >> 6u) << 4u)), f32((hi >> 4u) | ((me >> 6u) << 4u)));
}
fn q4k(block0: u32, col: u32, K: u32, e: u32) -> f32 {
  let base = qbase(block0, col, K, e, 144u);
  let within = e % 256u;
  let pair = within / 64u;
  let rem = within % 64u;
  let byte = qbyte(base + 16u + pair * 32u + rem % 32u);
  let nibble = select(byte >> 4u, byte & 15u, rem < 32u);
  let j = pair * 2u + select(1u, 0u, rem < 32u);
  let sm = q4k_scale_min(base, j);
  return (qhalf(base) * sm.x) * f32(nibble) - qhalf(base + 2u) * sm.y;
}
// Q6_K: 128 low nibbles, 64 bytes of high pairs, sixteen signed group scales,
// then the f16 super-block scale. Each value is (low4 | high2 << 4) - 32.
fn q6k(block0: u32, col: u32, K: u32, e: u32) -> f32 {
  let base = qbase(block0, col, K, e, 210u);
  let within = e % 256u;
  let half = within / 128u;
  let rem = within % 128u;
  let sub = rem / 32u;
  let l = rem % 32u;
  let ql = qbyte(base + 64u * half + l + 32u * (sub & 1u));
  let low = select(ql >> 4u, ql & 15u, sub < 2u);
  let high = (qbyte(base + 128u + 32u * half + l) >> (2u * sub)) & 3u;
  let scale = f32(bitcast<i32>(qbyte(base + 192u + 8u * half + l / 16u + 2u * sub) << 24u) >> 24u);
  return (qhalf(base + 208u) * scale) * f32(bitcast<i32>(low | (high << 4u)) - 32);
}
var<workgroup> As: array<array<f32, 16>, 16>;
var<workgroup> Bs: array<array<f32, 16>, 16>;
@compute @workgroup_size(16, 16)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
  let M = P.g.x; let K = P.g.y; let N = P.g.z;
  let batches = max(P.g.w, 1u);
  let part = wg.z / batches; let b = wg.z % batches;
  let row = wg.y * 16u + lid.y; let col = wg.x * 16u + lid.x;
  let aBase = b * P.sa.x; let bBase = b * P.sa.y;
  let k0 = part * P.sa.z; let k1 = min(K, k0 + P.sa.z);
  var acc = 0.0;
  for (var t = k0; t < k1; t = t + 16u) {
    let ka = t + lid.x; let kb = t + lid.y;
    var av = 0.0; var bv = 0.0;
    if (row < M && ka < k1) { av = A[aBase + row * K + ka]; }
    if (kb < k1 && col < N) { bv = q4k(bBase / 256u, col, K, kb); }
    As[lid.y][lid.x] = av; Bs[lid.y][lid.x] = bv;
    workgroupBarrier();
    let span = min(16u, k1 - t);
    for (var k = 0u; k < span; k = k + 1u) { acc = acc + As[lid.y][k] * Bs[k][lid.x]; }
    workgroupBarrier();
  }
  if (row < M && col < N) { O[(part * batches + b) * M * N + row * N + col] = acc; }
}`],
  matmul_q6k_split: [['A', 'B:u32'], `// Block-quantized weights, decoded where they are used.
//
// Ports of dequantize_row_q4_K and dequantize_row_q6_K; src/quant.mjs is the
// same thing in JavaScript and the gguf-quants crate is it in Rust. Addressing is by
// absolute byte, not by word: a Q6_K block is 210 bytes, so blocks after the
// first are not word-aligned and a word-relative accessor would be wrong.
fn qbyte(off: u32) -> u32 { return (B[off >> 2u] >> ((off & 3u) * 8u)) & 0xffu; }
// One f16 from two bytes that may straddle a word.
fn qhalf(off: u32) -> f32 { return unpack2x16float(qbyte(off) | (qbyte(off + 1u) << 8u)).x; }
// Where element e of row col begins, for a format of bytes per 256 values.
fn qbase(block0: u32, col: u32, K: u32, e: u32, bytes: u32) -> u32 {
  return (block0 + col * (K / 256u) + e / 256u) * bytes;
}
// Q4_K: f16 scale and minimum, eight 6-bit sub-block scales and minimums packed
// into 12 bytes, then 256 nibbles. Sub-blocks 0..3 are a plain six bits; 4..7
// borrow their high two bits from the bytes of the first four.
fn q4k_scale_min(base: u32, j: u32) -> vec2<f32> {
  if (j < 4u) { return vec2<f32>(f32(qbyte(base + 4u + j) & 63u), f32(qbyte(base + 8u + j) & 63u)); }
  let hi = qbyte(base + j + 8u);
  let lo = qbyte(base + j);
  let me = qbyte(base + j + 4u);
  return vec2<f32>(f32((hi & 15u) | ((lo >> 6u) << 4u)), f32((hi >> 4u) | ((me >> 6u) << 4u)));
}
fn q4k(block0: u32, col: u32, K: u32, e: u32) -> f32 {
  let base = qbase(block0, col, K, e, 144u);
  let within = e % 256u;
  let pair = within / 64u;
  let rem = within % 64u;
  let byte = qbyte(base + 16u + pair * 32u + rem % 32u);
  let nibble = select(byte >> 4u, byte & 15u, rem < 32u);
  let j = pair * 2u + select(1u, 0u, rem < 32u);
  let sm = q4k_scale_min(base, j);
  return (qhalf(base) * sm.x) * f32(nibble) - qhalf(base + 2u) * sm.y;
}
// Q6_K: 128 low nibbles, 64 bytes of high pairs, sixteen signed group scales,
// then the f16 super-block scale. Each value is (low4 | high2 << 4) - 32.
fn q6k(block0: u32, col: u32, K: u32, e: u32) -> f32 {
  let base = qbase(block0, col, K, e, 210u);
  let within = e % 256u;
  let half = within / 128u;
  let rem = within % 128u;
  let sub = rem / 32u;
  let l = rem % 32u;
  let ql = qbyte(base + 64u * half + l + 32u * (sub & 1u));
  let low = select(ql >> 4u, ql & 15u, sub < 2u);
  let high = (qbyte(base + 128u + 32u * half + l) >> (2u * sub)) & 3u;
  let scale = f32(bitcast<i32>(qbyte(base + 192u + 8u * half + l / 16u + 2u * sub) << 24u) >> 24u);
  return (qhalf(base + 208u) * scale) * f32(bitcast<i32>(low | (high << 4u)) - 32);
}
var<workgroup> As: array<array<f32, 16>, 16>;
var<workgroup> Bs: array<array<f32, 16>, 16>;
@compute @workgroup_size(16, 16)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
  let M = P.g.x; let K = P.g.y; let N = P.g.z;
  let batches = max(P.g.w, 1u);
  let part = wg.z / batches; let b = wg.z % batches;
  let row = wg.y * 16u + lid.y; let col = wg.x * 16u + lid.x;
  let aBase = b * P.sa.x; let bBase = b * P.sa.y;
  let k0 = part * P.sa.z; let k1 = min(K, k0 + P.sa.z);
  var acc = 0.0;
  for (var t = k0; t < k1; t = t + 16u) {
    let ka = t + lid.x; let kb = t + lid.y;
    var av = 0.0; var bv = 0.0;
    if (row < M && ka < k1) { av = A[aBase + row * K + ka]; }
    if (kb < k1 && col < N) { bv = q6k(bBase / 256u, col, K, kb); }
    As[lid.y][lid.x] = av; Bs[lid.y][lid.x] = bv;
    workgroupBarrier();
    let span = min(16u, k1 - t);
    for (var k = 0u; k < span; k = k + 1u) { acc = acc + As[lid.y][k] * Bs[k][lid.x]; }
    workgroupBarrier();
  }
  if (row < M && col < N) { O[(part * batches + b) * M * N + row * N + col] = acc; }
}`],
};
const UNARY = {relu: 0, positive: 1, neg: 2, exp: 3, log: 4, sqrt: 5, tanh: 6, sigmoid: 7, gelu: 8, gelu_grad: 9};
const BINARY = {add: 0, sub: 1, mul: 2, div: 3, maximum: 4, minimum: 5, eq: 6, ne: 7, lt: 8, le: 9, gt: 10, ge: 11};
const MODE = {same: 0, aScalar: 1, bScalar: 2, general: 3};
const OPTIM = {sgd_update: 0, momentum_update: 1, adam_m: 2, adam_v: 3, adam_update: 4};
// One 256-byte uniform slot per dispatch in a single buffer (dynamic offsets),
// so a bind group depends only on its kernel and storage buffers and is
// reused across steps and runs. UNIFORM_SLOTS bounds the dispatches of one
// submission (a 64-step session run of a 512-node graph fits).
// Enough outputs to fill a GPU, and the smallest slice of the reduced axis
// worth a pass of its own. Round numbers: the win is the order of magnitude.
const WIDE_ENOUGH = 65536, MIN_SPLIT_K = 512, MIN_SPLIT_CHUNK = 128, MAX_SPLIT = 32;
const SLOT = 256, UNIFORM_SLOTS = 16384, POOL_BYTES = 256 * 1024 * 1024, GROUP_CACHE = 8192;
const COMPUTE = () => globalThis.GPUShaderStage?.COMPUTE ?? 4;
/** Blocks as whole four-byte words. A Q6_K block is 210 bytes, so a buffer of
 * them need not be a multiple of four and the tail is padded rather than
 * read short. */
const words = bytes => {
  const total = Math.ceil(bytes.byteLength / 4) * 4;
  const padded = total === bytes.byteLength ? bytes : (() => { const b = new Uint8Array(total); b.set(bytes); return b; })();
  return new Uint32Array(padded.buffer, padded.byteOffset, total / 4);
};

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
    this.groups = new Map(); this.bufferIds = new WeakMap(); this.nextBufferId = 1;
    this.uniformBuffer = null; this.uniformData = new ArrayBuffer(UNIFORM_SLOTS * SLOT); this.slot = 0;
    this.staging = null; this.encoder = null; this.pass = null;
    this.peakBufferBytes = 0; this.liveBufferBytes = 0;
    device.lost.then(reason => {this.lost = reason.message || reason.reason || 'Device lost';});
  }
  live() { check(!this.lost, 'DEVICE_LOST', `WebGPU device is unavailable: ${this.lost}`); }
  limitHints() {
    const cap = Math.floor(Math.min(this.device.limits.maxStorageBufferBindingSize, this.device.limits.maxBufferSize) / 4);
    return {maxElements: Math.min(DEFAULT_LIMITS.maxElements, cap), maxWork: 1000000000};
  }
  allocationStats() { return {webgpuBufferPeakBytes: this.peakBufferBytes}; }
  /** Integer accumulation needs 64 bits -- products reach 2^30 and their sums
   * 2^40 -- and WGSL has no 64-bit integer type. Until that is emulated in
   * paired u32s, this backend says so rather than returning zeros. See
   * docs/FIXED-POINT.md. */
  unsupported(){return UNSUPPORTED;}
  async begin() {
    this.live(); this.device.pushErrorScope('out-of-memory'); this.device.pushErrorScope('validation');
    this.scopeOpen = true; this.slot = 0;
    if (!this.uniformBuffer) this.uniformBuffer = this.device.createBuffer({size: UNIFORM_SLOTS * SLOT, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST});
  }
  idOf(buffer) { let id = this.bufferIds.get(buffer); if (!id) { id = this.nextBufferId++; this.bufferIds.set(buffer, id); } return id; }
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
      if (data) {
        const view = data instanceof Uint32Array
          ? new Uint32Array(buffer.getMappedRange(), 0, size)
          : new Float32Array(buffer.getMappedRange(), 0, size);
        view.set(data); buffer.unmap();
      }
      this.liveBufferBytes += bytes; this.peakBufferBytes = Math.max(this.peakBufferBytes, this.liveBufferBytes);
    }
    return {buffer, size, bytes, freed: false};
  }
  /** One pipeline per kernel with an explicit layout: a dynamic-offset uniform, read-only inputs, one output. */
  async pipeline(name) {
    let entry = this.pipelines.get(name);
    if (!entry) {
      const [inputs, body] = KERNELS[name];
      const layout = this.device.createBindGroupLayout({entries: [
        {binding: 0, visibility: COMPUTE(), buffer: {type: 'uniform', hasDynamicOffset: true}},
        ...inputs.map((_, i) => ({binding: i + 1, visibility: COMPUTE(), buffer: {type: 'read-only-storage'}})),
        {binding: inputs.length + 1, visibility: COMPUTE(), buffer: {type: 'storage'}}]});
      const module = this.device.createShaderModule({code: `${PRELUDE}\n${io(inputs)}\n${body}`});
      const pipeline = await this.device.createComputePipelineAsync({layout: this.device.createPipelineLayout({bindGroupLayouts: [layout]}),
        compute: {module, entryPoint: 'main'}});
      entry = {pipeline, layout}; this.pipelines.set(name, entry);
    }
    return entry;
  }
  /** How many ways to slice a product's reduced axis, or null to leave it whole.
   *
   * The tile is 16 by 16, so a product with one row uses a sixteenth of each
   * workgroup and a few thousand outputs make only a hundred groups. The slice
   * is a multiple of 16 so every slice starts where a tile does. */
  splitParts(n) {
    if (n.size >= WIDE_ENOUGH || n.k < MIN_SPLIT_K) return null;
    const want = Math.min(Math.ceil(WIDE_ENOUGH / n.size), MAX_SPLIT);
    const chunk = Math.max(MIN_SPLIT_CHUNK, Math.ceil(Math.ceil(n.k / want) / 16) * 16);
    const parts = Math.ceil(n.k / chunk);
    return parts > 1 ? {parts, chunk} : null;
  }
  /** Fills the next 256-byte uniform slot and returns its dynamic offset; slots upload once, at submit. */
  uniform(fill) {
    check(this.slot < UNIFORM_SLOTS, 'LIMIT', `One submission dispatches at most ${UNIFORM_SLOTS} kernels; run fewer steps at a time`);
    const offset = this.slot++ * SLOT, u = new Uint32Array(this.uniformData, offset, 24), f = new Float32Array(this.uniformData, offset, 24);
    u.fill(0); fill(u, f);
    return offset;
  }
  /** Bind groups are cached by kernel and storage buffers: fixed buffers (a session's) never rebuild one. */
  bindGroup(kernel, layout, handles) {
    let key = kernel;
    for (const h of handles) key += `|${this.idOf(h.buffer)}:${h.size}`;
    let group = this.groups.get(key);
    if (!group) {
      if (this.groups.size >= GROUP_CACHE) this.groups.clear();
      group = this.device.createBindGroup({layout, entries: [{binding: 0, resource: {buffer: this.uniformBuffer, offset: 0, size: 96}},
        ...handles.map((h, i) => ({binding: i + 1, resource: {buffer: h.buffer, size: h.size * 4}}))]});
      this.groups.set(key, group);
    }
    return group;
  }
  async dispatch(kernel, fill, inputs, out, count, groups = null) {
    this.live();
    const {pipeline, layout} = await this.pipeline(kernel), limit = this.device.limits.maxComputeWorkgroupsPerDimension;
    let [x, y, z] = groups ?? [Math.ceil(count / 256), 1, 1];
    if (!groups && x > limit) { y = Math.ceil(x / limit); x = limit; }
    check(x <= limit && y <= limit && z <= limit, 'LIMIT', 'Dispatch exceeds WebGPU workgroup limit');
    const offset = this.uniform(fill), group = this.bindGroup(kernel, layout, [...inputs, out]);
    if (!this.encoder) this.encoder = this.device.createCommandEncoder();
    if (!this.pass) this.pass = this.encoder.beginComputePass();
    this.pass.setPipeline(pipeline); this.pass.setBindGroup(0, group, [offset]); this.pass.dispatchWorkgroups(x, y, z);
  }
  /** A session carry: the step's output is copied into the input's fixed buffer, in stream order. */
  carry(src, dst) {
    this.live();
    if (this.pass) { this.pass.end(); this.pass = null; }
    if (!this.encoder) this.encoder = this.device.createCommandEncoder();
    this.encoder.copyBufferToBuffer(src.buffer, 0, dst.buffer, 0, src.size * 4);
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
    // A quantized input's buffer is its blocks: size counts four-byte words,
    // so binding, pooling and accounting are unchanged, and nothing expands it.
    if (n.op === 'input') {
      return n.quant ? this.alloc(words(n.data).length, words(n.data)) : this.alloc(n.size, n.data);
    }
    const out = this.alloc(n.size), [a] = refs;
    // Debug mode attributes validation errors to the node that caused them.
    let scoped = this.debug;
    if (scoped) this.device.pushErrorScope('validation');
    try {
      switch (n.op) {
        case 'full': await this.dispatch('fill', (u, f) => {u[0] = n.size; f[20] = n.value;}, [], out, n.size); break;
        case 'add': case 'sub': case 'mul': case 'div': case 'maximum': case 'minimum':
        case 'eq': case 'ne': case 'lt': case 'le': case 'gt': case 'ge':
          await this.dispatch('binary', u => {u[0] = n.size; u[1] = MODE[n.mode]; u[2] = BINARY[n.op];
            if (n.mode === 'general') {u.set(n.dims, 4); u.set(n.aStrides, 8); u.set(n.bStrides, 12);}}, refs, out, n.size); break;
        case 'relu': case 'positive': case 'neg': case 'exp': case 'log': case 'sqrt':
        case 'tanh': case 'sigmoid': case 'gelu': case 'gelu_grad':
          await this.dispatch('unary', u => {u[0] = n.size; u[2] = UNARY[n.op];}, refs, out, n.size); break;
        case 'where':
          await this.dispatch('where', u => {u[0] = n.size; u[1] = n.mode === 'same' ? 0 : 3;
            u.set(n.dims, 4); u.set(n.cStrides, 8); u.set(n.aStrides, 12); u.set(n.bStrides, 16);}, refs, out, n.size); break;
        case 'uniform':
          await this.dispatch('uniform', u => {u[0] = n.size; u[16] = n.seed >>> 0; u[17] = n.step >>> 0;}, [], out, n.size); break;
        case 'transpose': case 'permute':
          await this.dispatch('gather', u => {u[0] = n.size; u.set(n.dims, 4); u.set(n.srcStrides, 8);}, refs, out, n.size); break;
        // Signed strides and begins are stored as their 32-bit two's complement.
        case 'slice':
          await this.dispatch('slice', u => {u[0] = n.size; u.set(n.boxDims, 4); u.set(n.boxStrides.map(v => v >>> 0), 8); u[16] = n.offset;}, refs, out, n.size); break;
        case 'slice_scatter':
          await this.dispatch('slice_scatter', u => {u[0] = n.size; u.set(n.dims, 4); u.set(n.begin4, 8); u.set(n.stride4.map(v => v >>> 0), 12); u.set(n.boxDims, 16);},
            refs, out, n.size); break;
        case 'index_select': case 'index_add':
          await this.dispatch(n.op, u => {u[0] = n.size; u[3] = n.count; u[16] = n.len; u[17] = n.inner;}, refs, out, n.size); break;
        case 'gather':
          await this.dispatch('gather_axis', u => {u[0] = n.size; u.set(n.dims, 4); u.set(n.srcStrides, 8); u[16] = n.axisStride; u[17] = n.len;}, refs, out, n.size); break;
        case 'scatter_add':
          await this.dispatch('scatter_add', u => {u[0] = n.size; u.set(n.dims, 4); u.set(n.indexDims, 8); u[16] = n.axis4;}, refs, out, n.size); break;
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
        // Not yet on this backend, and refused rather than skipped. An
        // operation with no case here would leave its output buffer at zero and
        // return a plausible tensor of nothing, which for a result that is
        // meant to be *checkable* is the worst available failure. Integer
        // accumulation needs 64 bits -- products reach 2^30 and their sums 2^40
        // -- and neither WGSL nor GLSL ES has a 64-bit integer, so this waits
        // on paired-u32 arithmetic. See docs/FIXED-POINT.md.
        // A throw and not a `check(false, ...)`, so that no reading of this
        // can end in a fall-through to the float32 `matmul` below.
        case 'matmul_fixed': throw new ComputeError('UNSUPPORTED',
          'matmul_fixed needs a 64-bit integer accumulator, which WebGPU has no native type for; run it on the wasm or cpu-js backend');
        case 'matmul': {
          const kernel = n.bQuant ? `matmul_${n.bQuant.dtype.replace('_', '')}` : n.transposed ? 'matmul_t' : 'matmul';
          const split = this.splitParts(n);
          if (!split) {
            await this.dispatch(kernel, u => {u.set([n.m, n.k, n.n, n.batch], 16); u[8] = n.aBatchStride; u[9] = n.bBatchStride;},
              refs, out, n.size, [Math.ceil(n.n / 16), Math.ceil(n.m / 16), n.batch]);
            break;
          }
          const {parts, chunk} = split, partials = this.alloc(n.size * parts);
          try {
            await this.dispatch(`${kernel}_split`, u => {
              u.set([n.m, n.k, n.n, n.batch], 16);
              u[8] = n.aBatchStride; u[9] = n.bBatchStride; u[10] = chunk;
            }, refs, partials, n.size * parts, [Math.ceil(n.n / 16), Math.ceil(n.m / 16), n.batch * parts]);
            // `reduce` sums a [parts, outputs] layout straight down the parts.
            await this.dispatch('reduce', u => {u[0] = n.size; u[1] = 0; u[3] = parts; u[16] = n.size;},
              [partials], out, n.size);
          } finally { this.free(partials); }
          break;
        }
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
    if (this.slot > 0) this.device.queue.writeBuffer(this.uniformBuffer, 0, this.uniformData, 0, this.slot * SLOT);
    const commands = this.encoder.finish(); this.encoder = null;
    this.device.queue.submit([commands]);
  }
  /** A MAP_READ buffer kept across session runs (grown when a readback outgrows it). */
  stagingFor(total) {
    if (this.staging && this.staging.size >= total) return this.staging;
    if (this.staging) this.staging.destroy();
    this.staging = this.device.createBuffer({size: Math.max(total, 4096), usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ});
    return this.staging;
  }
  /**
   * A session run's end: submit, then await the readback map, both error
   * scopes and the queue in ONE round trip to the GPU process (readAll then
   * finish take two). Leaves nothing for finish to do.
   */
  async complete(handles) {
    this.live();
    const offsets = []; let total = 0;
    for (const h of handles) {offsets.push(total); total += h.size * 4;}
    const staging = total ? this.stagingFor(total) : null;
    this.flush(encoder => handles.forEach((h, i) => encoder.copyBufferToBuffer(h.buffer, 0, staging, offsets[i], h.size * 4)));
    const pops = [this.device.popErrorScope(), this.device.popErrorScope()]; this.scopeOpen = false;
    try {
      const [validation, memory] = await Promise.all([...pops, staging ? staging.mapAsync(GPUMapMode.READ, 0, total) : null]);
      if (validation || memory) throw new ComputeError('GPU', (validation || memory).message);
      this.live();
      if (!staging) return [];
      const all = new Float32Array(staging.getMappedRange(0, total));
      return handles.map((h, i) => all.slice(offsets[i] / 4, offsets[i] / 4 + h.size));
    } catch (error) {
      if (this.staging) { try { this.staging.destroy(); } catch { /* lost */ } this.staging = null; }
      throw error;
    } finally {
      if (this.staging?.mapState === 'mapped') this.staging.unmap();
      this.reclaim();
    }
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
  // Freed during an execution: recycled until it settles. Freed between
  // executions (a session releasing a resident buffer): idle at once.
  free(h) { if (!h.freed) { h.freed = true; if (this.scopeOpen) this.recycled.push(h); else this.pool(h); } }
  // Idle lists stay sorted by buffer id, so a session run that starts from
  // the same pool allocates the same buffers every time (and its cached bind
  // groups keep matching); alloc takes from the end.
  pool(h) {
    if (this.idleBytes + h.bytes > POOL_BYTES) {h.buffer.destroy(); this.liveBufferBytes -= h.bytes; return;}
    if (!this.idle.has(h.bytes)) this.idle.set(h.bytes, []);
    const list = this.idle.get(h.bytes), id = this.idOf(h.buffer);
    let i = list.length; while (i > 0 && this.idOf(list[i - 1]) > id) i--;
    list.splice(i, 0, h.buffer); this.idleBytes += h.bytes;
  }
  reclaim() { for (const h of this.recycled) this.pool(h); this.recycled = []; }
  async finish() {
    if (!this.scopeOpen) { this.reclaim(); return; }
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
    } finally { this.reclaim(); }
  }
  dispose() {
    for (const list of this.idle.values()) for (const b of list) b.destroy();
    for (const h of this.recycled) h.buffer.destroy();
    if (this.uniformBuffer) this.uniformBuffer.destroy();
    if (this.staging) this.staging.destroy();
    this.idle.clear(); this.recycled = []; this.groups.clear(); this.pipelines.clear(); this.uniformBuffer = null; this.staging = null; this.device.destroy();
  }
}
