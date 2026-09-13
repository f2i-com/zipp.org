import {check, ComputeError} from '../graph.mjs';

export class WebGPUBackend {
  static async create() {
    check(globalThis.navigator?.gpu, 'UNAVAILABLE', 'WebGPU is unavailable (use HTTPS or localhost)');
    const adapter = await navigator.gpu.requestAdapter({powerPreference: 'high-performance'});
    check(adapter, 'UNAVAILABLE', 'No WebGPU adapter is available');
    check(!(adapter.info?.isFallbackAdapter ?? adapter.isFallbackAdapter), 'UNAVAILABLE', 'Hardware WebGPU required; browser returned a fallback adapter');
    const device = await adapter.requestDevice();
    return new WebGPUBackend(device, adapter.info);
  }
  constructor(device, info) {
    this.name = 'webgpu'; this.description = 'WebGPU compute shaders (WGSL)';
    this.device = device; this.info = info ? {vendor: info.vendor, architecture: info.architecture,
      description: info.description, isFallbackAdapter: info.isFallbackAdapter ?? null} : null;
    this.pipelines = new Map(); this.lost = null; this.scopeOpen = false;
    device.lost.then(reason => {this.lost = reason.message || reason.reason || 'Device lost';});
  }
  live() { check(!this.lost, 'DEVICE_LOST', `WebGPU device is unavailable: ${this.lost}`); }
  async begin() {
    this.live(); this.device.pushErrorScope('out-of-memory'); this.device.pushErrorScope('validation');
    this.scopeOpen = true;
  }
  alloc(size, data) {
    this.live();
    check(size*4 <= this.device.limits.maxStorageBufferBindingSize && size*4 <= this.device.limits.maxBufferSize,
      'LIMIT', 'Tensor exceeds WebGPU buffer limits');
    const buffer = this.device.createBuffer({size: Math.max(4,size*4),
      usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST,
      mappedAtCreation: !!data});
    if(data) {new Float32Array(buffer.getMappedRange()).set(data); buffer.unmap();}
    return {buffer, size};
  }
  async pipeline(code) {
    if(this.pipelines.has(code)) return this.pipelines.get(code);
    const module = this.device.createShaderModule({code});
    const pipeline = await this.device.createComputePipelineAsync({layout: 'auto', compute: {module, entryPoint: 'main'}});
    if(this.pipelines.size >= 128) this.pipelines.delete(this.pipelines.keys().next().value);
    this.pipelines.set(code,pipeline); return pipeline;
  }
  async dispatch(out, inputs, expression) {
    this.live();
    const groups = Math.ceil(out.size/64);
    check(groups <= this.device.limits.maxComputeWorkgroupsPerDimension, 'LIMIT', 'Dispatch exceeds WebGPU workgroup limit');
    const names=['aData','bData'];
    const declarations=inputs.map((_,i)=>`@group(0) @binding(${i}) var<storage, read> ${names[i]}: array<f32>;`).join('\n');
    const code=`${declarations}
@group(0) @binding(${inputs.length}) var<storage, read_write> resultData: array<f32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let i = gid.x;
  if (i >= ${out.size}u) { return; }
  ${expression}
}`;
    const pipeline=await this.pipeline(code);
    const bindings=[...inputs,out].map((h,i)=>({binding:i,resource:{buffer:h.buffer}}));
    const group=this.device.createBindGroup({layout:pipeline.getBindGroupLayout(0),entries:bindings});
    const encoder=this.device.createCommandEncoder(); const pass=encoder.beginComputePass();
    pass.setPipeline(pipeline);pass.setBindGroup(0,group);pass.dispatchWorkgroups(groups);pass.end();
    this.device.queue.submit([encoder.finish()]);
  }
  async run(n, refs) {
    if(n.op==='input') return this.alloc(n.size,n.data);
    const out=this.alloc(n.size);
    try {
      switch(n.op) {
        case 'full': await this.dispatch(out,[],`resultData[i] = ${wgslFloat(n.value)};`); break;
        case 'add': case 'sub': case 'mul':
          await this.dispatch(out,refs,`resultData[i] = aData[${n.aScalar?'0u':'i'}] ${{add:'+',sub:'-',mul:'*'}[n.op]} bData[${n.bScalar?'0u':'i'}];`); break;
        case 'relu': await this.dispatch(out,refs,'resultData[i] = max(aData[i], 0.0);'); break;
        case 'matmul': await this.dispatch(out,refs,`
          let row = i / ${n.n}u; let col = i % ${n.n}u;
          var total: f32 = 0.0;
          for (var k: u32 = 0u; k < ${n.k}u; k = k + 1u) {
            total = total + aData[row * ${n.k}u + k] * bData[k * ${n.n}u + col];
          }
          resultData[i] = total;`); break;
        case 'sum': {
          let input=refs[0]; const scratch=[];
          try {
            while(input.size>1) {
              const length=Math.ceil(input.size/2), next=length===1?out:this.alloc(length);
              if(next!==out) scratch.push(next);
              await this.dispatch(next,[input],`let j = i * 2u;
                var other: f32 = 0.0;
                if (j + 1u < ${input.size}u) { other = aData[j + 1u]; }
                resultData[i] = aData[j] + other;`);
              input=next;
            }
            if(refs[0].size===1) await this.dispatch(out,refs,'resultData[i] = aData[0u];');
          } finally {for(const h of scratch)this.free(h);}
          break;
        }
        case 'life': {
          const [h,w]=n.shape;
          await this.dispatch(out,refs,`
            let x = i32(i % ${w}u); let y = i32(i / ${w}u);
            var neighbours: u32 = 0u;
            for(var dy: i32 = -1; dy <= 1; dy = dy + 1) {
              for(var dx: i32 = -1; dx <= 1; dx = dx + 1) {
                if(dx != 0 || dy != 0) {
                  let xx = (x + dx + ${w}) % ${w}; let yy = (y + dy + ${h}) % ${h};
                  neighbours = neighbours + select(0u,1u,aData[u32(yy * ${w} + xx)] > 0.5);
                }
              }
            }
            let alive = neighbours == 3u || (aData[i] > 0.5 && neighbours == 2u);
            resultData[i] = select(0.0,1.0,alive);`); break;
        }
      }
      return out;
    } catch(error) {this.free(out);throw error;}
  }
  async read(h) {
    this.live();
    const staging=this.device.createBuffer({size:h.size*4,usage:GPUBufferUsage.COPY_DST|GPUBufferUsage.MAP_READ});
    try {
      const encoder=this.device.createCommandEncoder();encoder.copyBufferToBuffer(h.buffer,0,staging,0,h.size*4);
      this.device.queue.submit([encoder.finish()]);await staging.mapAsync(GPUMapMode.READ);
      return new Float32Array(staging.getMappedRange()).slice();
    } finally {if(staging.mapState==='mapped') staging.unmap();staging.destroy();}
  }
  free(h) {h.buffer.destroy();}
  async finish() {
    if(!this.scopeOpen)return;
    this.scopeOpen=false;
    const validation=await this.device.popErrorScope(); const memory=await this.device.popErrorScope();
    if(validation || memory) throw new ComputeError('GPU', (validation || memory).message);
    await this.device.queue.onSubmittedWorkDone();this.live();
  }
  dispose() {this.pipelines.clear();this.device.destroy();}
}
// Only validated finite float32 values enter generated shader text.
function wgslFloat(value) {
  const s=String(value);return (s.includes('.')||s.includes('e')?s:`${s}.0`)+'f';
}
