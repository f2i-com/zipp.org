import {check} from '../graph.mjs';
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
    this.name = 'wasm'; this.description = 'Compiled standalone C float32 kernels in WebAssembly';
    this.e = exports; this.base = Number(exports.__heap_base.value); this.cursor = this.base;
  }
  async begin() { this.cursor = this.base; }
  alloc(size) {
    const ptr = this.cursor; this.cursor += Math.ceil(size*4/16)*16;
    check(this.cursor <= 128*1024*1024, 'LIMIT', 'WASM arena limit exceeded');
    const deficit = this.cursor - this.e.memory.buffer.byteLength;
    if (deficit > 0) this.e.memory.grow(Math.ceil(deficit/65536));
    return {ptr, size};
  }
  view(h) { return new Float32Array(this.e.memory.buffer, h.ptr, h.size); }
  async run(n, refs) {
    const o = this.alloc(n.size), a = refs[0]?.ptr, b = refs[1]?.ptr;
    switch(n.op) {
      case 'input': this.view(o).set(n.data); break;
      case 'full': this.e.fill(o.ptr,n.size,n.value); break;
      case 'add': case 'sub': case 'mul':
        this.e.binary(a,b,o.ptr,n.size,+n.aScalar,+n.bScalar,{add:0,sub:1,mul:2}[n.op]); break;
      case 'relu': this.e.relu(a,o.ptr,n.size); break;
      case 'matmul': this.e.matmul(a,b,o.ptr,n.m,n.k,n.n); break;
      case 'life': this.e.life(a,o.ptr,n.shape[0],n.shape[1]); break;
      case 'sum': {
        let input=refs[0];
        // Scratch is temporary: reuse the arena after the scalar output.
        const mark=this.cursor;
        while(input.size>1){const next=this.alloc(Math.ceil(input.size/2));this.e.pair_sum(input.ptr,next.ptr,input.size);input=next;}
        this.view(o)[0]=this.view(input)[0]; this.cursor=mark; break;
      }
    }
    return o;
  }
  async read(h) { return this.view(h).slice(); }
  free() {} // Arena reclaimed between serial graph executions, not individual nodes.
  async finish() {}
  dispose() { this.e = null; }
}
