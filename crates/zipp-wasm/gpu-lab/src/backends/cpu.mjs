import {gelu, geluGrad, sigmoid, relu} from '../kernel-math.mjs';

/**
 * Reference float32 host-JavaScript implementation, not advertised as WASM.
 * Every operation rounds to float32 (a Float32Array store rounds once). Matrix
 * products and axis reductions accumulate in index order, so the WASM kernels
 * reproduce them bit for bit; transcendental functions are evaluated in double
 * and rounded, so they agree with WASM to about one float32 ulp.
 */
import {decodeQ4KBlock, Q4_K_BLOCK, Q4_K_BYTES} from '../quant.mjs';
const f = Math.fround;
const UNARY = {relu, positive: x => x > 0 ? 1 : 0, neg: x => -x, exp: Math.exp, log: Math.log, sqrt: Math.sqrt,
  tanh: Math.tanh, sigmoid, gelu, gelu_grad: geluGrad};
const BINARY = {add: (x, y) => x + y, sub: (x, y) => x - y, mul: (x, y) => x * y, div: (x, y) => x / y};

/** Pairwise float32 reduction: the tree topology the GPU kernels use. */
export function pairwiseSum(a) {
  let work = a;
  while (work.length > 1) {
    const next = new Float32Array(Math.ceil(work.length / 2));
    for (let i = 0; i < next.length; i++) next[i] = work[2*i] + (2*i + 1 < work.length ? work[2*i+1] : 0);
    work = next;
  }
  return work[0];
}
/** Visits every output index of a padded four-dimensional shape with two strided sources. */
function strided(n, out, fn) {
  const [d0, d1, d2, d3] = n.dims, [as0, as1, as2, as3] = n.aStrides ?? n.srcStrides, [bs0, bs1, bs2, bs3] = n.bStrides ?? [0, 0, 0, 0];
  let i = 0;
  for (let x0 = 0; x0 < d0; x0++) for (let x1 = 0; x1 < d1; x1++) for (let x2 = 0; x2 < d2; x2++) {
    const ab = x0*as0 + x1*as1 + x2*as2, bb = x0*bs0 + x1*bs1 + x2*bs2;
    for (let x3 = 0; x3 < d3; x3++) out[i++] = fn(ab + x3*as3, bb + x3*bs3);
  }
}
/** Row-wise max, then the float32 sum of exp(x - max) in index order. */
function rowStats(a, r, cols) {
  let m = a[r*cols];
  for (let j = 1; j < cols; j++) m = Math.max(m, a[r*cols+j]);
  let s = 0;
  for (let j = 0; j < cols; j++) s = f(s + f(Math.exp(f(a[r*cols+j] - m))));
  return [m, s];
}

export class CPUBackend {
  constructor() { this.name = 'cpu-js'; this.description = 'Host JavaScript float32 reference'; }
  async begin() {}
  async run(n, refs) {
    // A quantized input's handle is its blocks. Nothing expands it: that is the
    // point, and only a transposed matmul knows how to read it.
    if (n.op === 'input' && n.quant) return n.data;
    const out = new Float32Array(n.size), a = refs[0], b = refs[1];
    switch (n.op) {
      case 'input': out.set(n.data); break;
      case 'full': out.fill(n.value); break;
      case 'add': case 'sub': case 'mul': case 'div': {
        const op = BINARY[n.op];
        if (n.mode === 'same') for (let i = 0; i < n.size; i++) out[i] = op(a[i], b[i]);
        else if (n.mode === 'aScalar') { const x = a[0]; for (let i = 0; i < n.size; i++) out[i] = op(x, b[i]); }
        else if (n.mode === 'bScalar') { const y = b[0]; for (let i = 0; i < n.size; i++) out[i] = op(a[i], y); }
        else strided(n, out, (i, j) => op(a[i], b[j]));
        break;
      }
      case 'relu': case 'positive': case 'neg': case 'exp': case 'log': case 'sqrt':
      case 'tanh': case 'sigmoid': case 'gelu': case 'gelu_grad': {
        const op = UNARY[n.op];
        for (let i = 0; i < n.size; i++) out[i] = op(a[i]);
        break;
      }
      case 'transpose': case 'permute': strided(n, out, i => a[i]); break;
      case 'sum': case 'mean': {
        if (n.whole) {
          const total = pairwiseSum(a);
          out[0] = n.op === 'mean' ? f(total / n.inputSize) : total; break;
        }
        for (let o = 0; o < n.outer; o++) for (let i = 0; i < n.inner; i++) {
          let s = 0;
          for (let j = 0; j < n.len; j++) s = f(s + a[(o*n.len + j)*n.inner + i]);
          out[o*n.inner + i] = n.op === 'mean' ? s / n.len : s;
        }
        break;
      }
      case 'softmax': case 'log_softmax':
        for (let r = 0; r < n.rows; r++) {
          const [m, s] = rowStats(a, r, n.cols), ls = f(Math.log(s));
          for (let j = 0, k = r*n.cols; j < n.cols; j++, k++) {
            const d = f(a[k] - m);
            out[k] = n.op === 'softmax' ? f(Math.exp(d)) / s : d - ls;
          }
        }
        break;
      case 'cross_entropy': case 'cross_entropy_grad': {
        const losses = new Float32Array(n.rows);
        for (let r = 0; r < n.rows; r++) {
          const [m, s] = rowStats(a, r, n.cols), t = b[r];
          if (n.op === 'cross_entropy') { losses[r] = f(Math.log(s)) - f(a[r*n.cols + t] - m); continue; }
          for (let j = 0, k = r*n.cols; j < n.cols; j++, k++)
            out[k] = f(f(f(Math.exp(f(a[k] - m))) / s) - (j === t ? 1 : 0)) / n.rows;
        }
        if (n.op === 'cross_entropy') out[0] = pairwiseSum(losses) / n.rows;
        break;
      }
      case 'matmul': {
        // Every output sums its k products in k order, rounding each step, in
        // all three shapes below. The loops differ in what they walk first, not
        // in what any one output accumulates, so the results are identical.
        const {m, k, n: cols} = n;
        if (n.bQuant) {
          // b holds [N, K] blocks. Each output row of b is decoded once, into
          // scratch, and reused by every row of a: the weight is never expanded.
          const perRow = k / Q4_K_BLOCK, scratch = new Float32Array(k);
          for (let batch = 0; batch < n.batch; batch++) {
            const ao = batch * n.aBatchStride, bo = (batch * n.bBatchStride) / Q4_K_BLOCK, oo = batch * m * cols;
            for (let c = 0; c < cols; c++) {
              for (let t = 0; t < perRow; t++) {
                decodeQ4KBlock(b, (bo + c * perRow + t) * Q4_K_BYTES, scratch, t * Q4_K_BLOCK);
              }
              for (let r = 0; r < m; r++) {
                const at = oo + r * cols + c, base = ao + r * k;
                for (let j = 0; j < k; j++) out[at] += f(a[base + j] * scratch[j]);
              }
            }
          }
        } else if (n.transposed) {
          // b holds [N, K]: one contiguous row per output column.
          for (let batch = 0; batch < n.batch; batch++) {
            const ao = batch * n.aBatchStride, bo = batch * n.bBatchStride, oo = batch * m * cols;
            for (let r = 0; r < m; r++) {
              for (let c = 0; c < cols; c++) {
                const at = oo + r * cols + c, base = ao + r * k, brow = bo + c * k;
                for (let j = 0; j < k; j++) out[at] += f(a[base + j] * b[brow + j]);
              }
            }
          }
        } else {
          // ikj order over [K, N].
          for (let batch = 0; batch < n.batch; batch++) {
            const ao = batch * n.aBatchStride, bo = batch * n.bBatchStride, oo = batch * m * cols;
            for (let r = 0; r < m; r++) {
              const row = out.subarray(oo + r*cols, oo + (r+1)*cols);
              for (let j = 0; j < k; j++) {
                const x = a[ao + r*k + j], base = bo + j*cols;
                for (let c = 0; c < cols; c++) row[c] += f(x * b[base + c]);
              }
            }
          }
        }
        break;
      }
      case 'sgd_update': { const lr = n.lr; for (let i = 0; i < n.size; i++) out[i] = a[i] - f(lr * b[i]); break; }
      case 'momentum_update': {
        const mu = n.momentum, w = n.w;
        for (let i = 0; i < n.size; i++) out[i] = f(mu * a[i]) + f(w * b[i]); break;
      }
      case 'adam_m': {
        // torch.lerp(m, grad, 1 - beta1), in the branch PyTorch takes for that weight.
        const w = n.w;
        if (w < 0.5) for (let i = 0; i < n.size; i++) out[i] = a[i] + f(w * f(b[i] - a[i]));
        else { const v = f(1 - w); for (let i = 0; i < n.size; i++) out[i] = b[i] - f(f(b[i] - a[i]) * v); }
        break;
      }
      case 'adam_v': {
        const beta = n.beta2, w = n.w;
        for (let i = 0; i < n.size; i++) out[i] = f(a[i] * beta) + f(f(w * b[i]) * b[i]); break;
      }
      case 'adam_update': {
        const c = refs[2], step = n.stepSize, bc = n.bc2Sqrt, eps = n.eps;
        for (let i = 0; i < n.size; i++) out[i] = a[i] - f(step * f(b[i] / f(f(f(Math.sqrt(c[i])) / bc) + eps)));
        break;
      }
      case 'life': {
        const [h,w] = n.shape;
        for (let y=0;y<h;y++) for(let x=0;x<w;x++) {
          let count=0;
          for(let dy=-1;dy<=1;dy++) for(let dx=-1;dx<=1;dx++)
            if(dx || dy) count += a[((y+dy+h)%h)*w+(x+dx+w)%w] > 0.5 ? 1 : 0;
          out[y*w+x] = count===3 || (a[y*w+x]>0.5 && count===2) ? 1 : 0;
        } break;
      }
    }
    return out;
  }
  async read(handle) { return handle.slice(); }
  free() {}
  async finish() {}
  dispose() {}
}
