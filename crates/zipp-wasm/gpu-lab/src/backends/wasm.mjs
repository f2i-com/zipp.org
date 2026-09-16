import {check} from '../graph.mjs';
const BINARY={add:0,sub:1,mul:2,div:3},MODE={same:0,aScalar:1,bScalar:2};
const UNARY={relu:0,positive:1,neg:2,exp:3,log:4,sqrt:5,tanh:6,sigmoid:7,gelu:8,gelu_grad:9};
const ARENA=128*1024*1024;
export class WasmBackend {
  static async create({wasmBytes, wasmUrl = new URL('../../wasm/kernels.wasm', import.meta.url)} = {}) {
    if (!wasmBytes) {
      const res = await fetch(wasmUrl); check(res.ok, 'WASM', `WASM fetch failed: ${res.status}`);
      wasmBytes = await res.arrayBuffer();
    }
    const {instance} = await WebAssembly.instantiate(wasmBytes, {});
    return new WasmBackend(instance.exports);
  }
  constructor(exports) {
    this.name = 'wasm'; this.description = 'Compiled standalone C float32 kernels in WebAssembly (SIMD)';
    this.e = exports; this.base = Number(exports.__heap_base.value); this.cursor = this.base;
  }
  /** SIMD kernels sustain more work than the JavaScript reference within a frame deadline. */
  limitHints() { return {maxWork: 400000000}; }
  async begin() { this.cursor = this.base; }
  alloc(size) {
    const ptr = this.cursor; this.cursor += Math.ceil(size*4/16)*16;
    check(this.cursor <= ARENA, 'LIMIT', 'WASM arena limit exceeded');
    const deficit = this.cursor - this.e.memory.buffer.byteLength;
    if (deficit > 0) this.e.memory.grow(Math.ceil(deficit/65536));
    return {ptr, size};
  }
  view(h) { return new Float32Array(this.e.memory.buffer, h.ptr, h.size); }
  /** Pairwise tree over scratch that is released afterwards; returns the float32 total. */
  pairwise(input) {
    const mark=this.cursor;
    while(input.size>1){const next=this.alloc(Math.ceil(input.size/2));this.e.pair_sum(input.ptr,next.ptr,input.size);input=next;}
    const total=this.view(input)[0];this.cursor=mark;return total;
  }
  async run(n, refs) {
    const o = this.alloc(n.size), [a, b, c] = refs.map(r => r?.ptr), e = this.e;
    switch(n.op) {
      case 'input': this.view(o).set(n.data); break;
      case 'full': e.fill(o.ptr,n.size,n.value); break;
      case 'add': case 'sub': case 'mul': case 'div':
        if (n.mode !== 'general') e.binary(a,b,o.ptr,n.size,MODE[n.mode],BINARY[n.op]);
        else e.binary_strided(a,b,o.ptr,BINARY[n.op],...n.dims,...n.aStrides,...n.bStrides);
        break;
      case 'relu': case 'positive': case 'neg': case 'exp': case 'log': case 'sqrt':
      case 'tanh': case 'sigmoid': case 'gelu': case 'gelu_grad': e.unary(a,o.ptr,n.size,UNARY[n.op]); break;
      case 'transpose': case 'permute': e.gather4(a,o.ptr,...n.dims,...n.srcStrides); break;
      case 'matmul': e.bmm(a,b,o.ptr,n.batch,n.m,n.k,n.n,n.aBatchStride,n.bBatchStride); break;
      case 'life': e.life(a,o.ptr,n.shape[0],n.shape[1]); break;
      case 'sum': case 'mean':
        if (n.whole) {
          // Scratch is temporary: reuse the arena after the output.
          const total=this.pairwise(refs[0]);
          this.view(o)[0]=n.op==='mean'?Math.fround(total/n.inputSize):total;
        } else e.reduce_axis(a,o.ptr,n.outer,n.len,n.inner,+(n.op==='mean'));
        break;
      case 'softmax': case 'log_softmax': e.softmax_rows(a,o.ptr,n.rows,n.cols,+(n.op==='log_softmax')); break;
      case 'cross_entropy': {
        const mark=this.cursor,rows=this.alloc(n.rows);e.ce_rows(a,b,rows.ptr,n.rows,n.cols);
        const total=this.pairwise(rows);this.cursor=mark;this.view(o)[0]=Math.fround(total/n.rows);break;
      }
      case 'cross_entropy_grad': e.ce_grad(a,b,o.ptr,n.rows,n.cols); break;
      case 'sgd_update': e.sgd_update(a,b,o.ptr,n.size,n.lr); break;
      case 'momentum_update': e.momentum_update(a,b,o.ptr,n.size,n.momentum,n.w); break;
      case 'adam_m': e.adam_m(a,b,o.ptr,n.size,n.w); break;
      case 'adam_v': e.adam_v(a,b,o.ptr,n.size,n.beta2,n.w); break;
      case 'adam_update': e.adam_update(a,b,c,o.ptr,n.size,n.stepSize,n.bc2Sqrt,n.eps); break;
    }
    return o;
  }
  async read(h) { return h instanceof Float32Array ? h.slice() : this.view(h).slice(); }
  free() {} // Arena reclaimed between serial graph executions, not individual nodes.
  // Sessions: the arena is reset per step, so a handle that outlives a step is
  // a host copy, uploaded again where a step reads it.
  persist(h) { return h instanceof Float32Array ? h : this.view(h).slice(); }
  materialize(h) { if (!(h instanceof Float32Array)) return h; const o = this.alloc(h.length); this.view(o).set(h); return o; }
  nextStep() { this.cursor = this.base; }
  async finish() {}
  dispose() { this.e = null; }
}
