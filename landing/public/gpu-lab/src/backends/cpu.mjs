/** Reference float32 host-JavaScript implementation, not advertised as WASM. */
export class CPUBackend {
  constructor() { this.name = 'cpu-js'; this.description = 'Host JavaScript float32 reference'; }
  async begin() {}
  async run(n, refs) {
    const out = new Float32Array(n.size), a = refs[0], b = refs[1];
    const f = Math.fround;
    switch (n.op) {
      case 'input': out.set(n.data); break;
      case 'full': out.fill(n.value); break;
      case 'add': case 'sub': case 'mul':
        for (let i = 0; i < n.size; i++) {
          const x = a[n.aScalar ? 0 : i], y = b[n.bScalar ? 0 : i];
          out[i] = n.op === 'add' ? x + y : n.op === 'sub' ? x - y : x * y;
        } break;
      case 'relu': for (let i = 0; i < n.size; i++) out[i] = Math.max(0, a[i]); break;
      case 'positive': for (let i=0;i<n.size;i++) out[i]=a[i]>0?1:0; break;
      case 'transpose': for (let i=0;i<n.size;i++) out[i]=a[(i%n.shape[1])*n.shape[0]+Math.floor(i/n.shape[1])]; break;
      case 'sum': {
        // Pairwise reduction matches the GPU tree topology more closely than a double-precision sum.
        let work = a.slice();
        while (work.length > 1) {
          const next = new Float32Array(Math.ceil(work.length / 2));
          for (let i = 0; i < next.length; i++) next[i] = f(work[2*i] + (work[2*i+1] ?? 0));
          work = next;
        }
        out[0] = work[0]; break;
      }
      case 'matmul':
        for (let r = 0; r < n.m; r++) for (let c = 0; c < n.n; c++) {
          let s = 0;
          for (let k = 0; k < n.k; k++) s = f(s + f(a[r*n.k+k] * b[k*n.n+c]));
          out[r*n.n+c] = s;
        } break;
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
