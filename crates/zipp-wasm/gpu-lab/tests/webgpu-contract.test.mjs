// These are host API/lifecycle mocks. They do NOT compile shaders or establish GPU correctness.
import test from 'node:test';
import assert from 'node:assert/strict';
import {WebGPUBackend} from '../src/backends/webgpu.mjs';
import {ComputeRuntime} from '../src/runtime.mjs';
import {validateProgram} from '../src/graph.mjs';
import {checkBackend} from './browser-cases.mjs';
import {readFile} from 'node:fs/promises';
globalThis.GPUBufferUsage={MAP_READ:1,COPY_SRC:4,COPY_DST:8,UNIFORM:64,STORAGE:128};
globalThis.GPUMapMode={READ:1};
globalThis.GPUShaderStage={COMPUTE:4};
export function fakeDevice(){
  const d={buffers:[],codes:[],submits:0,scopes:0,writes:[],dispatches:[],events:[],copies:[],bindGroups:0,layouts:[],failCompile:false,failMap:false,
    limits:{maxStorageBufferBindingSize:4194304,maxBufferSize:4194304,maxComputeWorkgroupsPerDimension:65535},
    lost:new Promise(()=>{}),
    queue:{submit(){d.submits++;d.events.push('submit');},writeBuffer(buffer,offset,data){d.writes.push(buffer);d.events.push('write');},async onSubmittedWorkDone(){}},
    pushErrorScope(){d.scopes++;},async popErrorScope(){d.scopes--;return null;},
    createBuffer(descriptor){const data=new ArrayBuffer(descriptor.size);const b={...descriptor,mapState:descriptor.mappedAtCreation?'mapped':'unmapped',destroyed:false,
      getMappedRange(offset=0,size){return size===undefined?data:data.slice(offset,offset+size);},unmap(){this.mapState='unmapped';},async mapAsync(){if(d.failMap)throw Error('map rejected');this.mapState='mapped';},destroy(){this.destroyed=true;d.events.push('destroy');}};d.buffers.push(b);return b;},
    createShaderModule({code}){d.codes.push(code);return {code};},
    createBindGroupLayout(x){d.layouts.push(x);return x;},createPipelineLayout(x){return x;},
    async createComputePipelineAsync(){if(d.failCompile)throw Error('compile rejected');return {getBindGroupLayout(){return {};}};},
    createBindGroup(x){d.bindGroups++;return x;},
    createCommandEncoder(){return {beginComputePass(){return {setPipeline(){},setBindGroup(i,g,offsets){d.bound=g;d.offsets=offsets;},dispatchWorkgroups(...g){d.dispatches.push(g);},end(){}};},
      copyBufferToBuffer(src,so,dst,doff,bytes){d.copies.push({src,dst,bytes});d.events.push('copy');},finish(){return {};}};},destroy(){d.destroyed=true;},
  };return d;
}
const graph=(n,op='relu')=>({version:1,nodes:[{id:0,op:'input',shape:[n],data:Array(n).fill(1)},{id:1,op,a:0}],outputs:[{name:'result',id:1}]});
const plan=(n,op)=>validateProgram(graph(n,op)).nodes;
const storage=d=>d.buffers.filter(b=>b.usage&GPUBufferUsage.STORAGE);

test('WebGPU contract mock: an execution records every dispatch into one submitted command buffer',async()=>{
  const d=fakeDevice(),rt=new ComputeRuntime(new WebGPUBackend(d));
  const r=await rt.execute({version:2,nodes:[{id:0,op:'input',shape:[65],data:Array(65).fill(1)},{id:1,op:'relu',a:0},{id:2,op:'exp',a:1},
    {id:3,op:'sum',a:2},{id:4,op:'mean',a:1}],outputs:[{name:'s',id:3},{name:'m',id:4}]});
  assert.equal(d.submits,1);assert.deepEqual(Object.keys(r.outputs),['s','m']);assert.equal(d.scopes,0);
  // Uniforms are uploaded before the one submit; nothing was destroyed while the commands were pending.
  assert.ok(d.events.indexOf('submit')>d.events.lastIndexOf('write'));assert.equal(d.buffers.filter(b=>b.destroyed&&b.usage&GPUBufferUsage.STORAGE).length,0);
  rt.dispose();
});
test('WebGPU contract mock: pipelines are keyed by kernel, not by shape, and carry no graph values',async()=>{
  const d=fakeDevice(),rt=new ComputeRuntime(new WebGPUBackend(d));
  for(const n of [65,1000,3])await rt.execute(graph(n,'relu'));
  await rt.execute({version:2,nodes:[{id:0,op:'full',shape:[5],value:1234.5},{id:1,op:'tanh',a:0}],outputs:[{name:'r',id:1}]});
  assert.equal(d.codes.length,2);assert.ok(d.codes.every(code=>!/\b(65|1000|1234\.5)u?\b/.test(code)));
  // Every invocation stops at the tensor's end: one element, or (vectorised) four.
  assert.ok(d.codes.every(code=>code.includes('if (i >= P.n) { return; }')||code.includes('if (i >= (P.n + 3u) / 4u) { return; }')));assert.ok(d.codes.some(code=>code.includes('@workgroup_size(256)')));
  rt.dispose();
});
test('WebGPU contract mock: storage buffers are pooled across executions',async()=>{
  const d=fakeDevice(),rt=new ComputeRuntime(new WebGPUBackend(d));
  await rt.execute(graph(300));const created=storage(d).length;
  for(let i=0;i<5;i++)await rt.execute(graph(300));
  assert.equal(storage(d).length,created);rt.dispose();assert.ok(storage(d).every(b=>b.destroyed));
});
test('WebGPU contract mock: a buffer freed during an execution is reused for outputs but never written as an input',async()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d);await b.begin();
  const nodes=validateProgram({version:2,nodes:[{id:0,op:'input',shape:[8],data:Array(8).fill(1)},{id:1,op:'relu',a:0},
    {id:2,op:'input',shape:[8],data:Array(8).fill(2)},{id:3,op:'neg',a:1}],outputs:[{name:'r',id:3}]}).nodes;
  const a=await b.run(nodes[0],[]),r=await b.run(nodes[1],[a]);b.free(a);
  const c=await b.run(nodes[2],[]);assert.notEqual(c.buffer,a.buffer,'input must not land in a buffer the pending encoder reads');
  const o=await b.run(nodes[3],[r]);assert.equal(o.buffer,a.buffer,'outputs may reuse it in stream order');
  await b.readAll([o]);for(const h of [r,c,o])b.free(h);await b.finish();
  assert.equal(d.writes.filter(w=>w.usage&GPUBufferUsage.STORAGE).length,0);b.dispose();
});
test('WebGPU contract mock: pairwise sum records guarded tails and returns scratch to the pool',async()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d),nodes=plan(65,'sum');await b.begin();
  const a=await b.run(nodes[0],[]),o=await b.run(nodes[1],[a]);
  assert.equal(d.dispatches.length,7);assert.ok(d.codes.some(code=>code.includes('if (j + 1u < P.len)')));
  assert.equal(b.recycled.length,6);b.free(a);b.free(o);await b.finish();assert.equal(b.recycled.length,0);b.dispose();
});
test('WebGPU contract mock: rejected compilation releases the output allocation',async()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d),nodes=plan(3);await b.begin();const a=await b.run(nodes[0],[]);d.failCompile=true;
  await assert.rejects(()=>b.run(nodes[1],[a]),/compile rejected/);assert.equal(b.recycled.length,1);b.free(a);await b.finish();b.dispose();
});
test('WebGPU contract mock: mapping rejection destroys staging allocations',async()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d),nodes=plan(3);await b.begin();const a=await b.run(nodes[0],[]);d.failMap=true;
  await assert.rejects(()=>b.readAll([a]),/map rejected/);assert.ok(d.buffers.filter(x=>x.usage&GPUBufferUsage.MAP_READ).every(x=>x.destroyed));
  b.free(a);await b.finish();b.dispose();
});
test('WebGPU contract mock: limit rejection happens before buffer creation',()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d);assert.throws(()=>b.alloc(1048577),e=>e.code==='LIMIT');assert.equal(d.buffers.length,0);
  assert.equal(b.limitHints().maxElements,1048576);b.dispose();
});
test('WebGPU contract mock: large element counts dispatch in two dimensions',async()=>{
  const d=fakeDevice();d.limits.maxComputeWorkgroupsPerDimension=4;const b=new WebGPUBackend(d),nodes=plan(2000);await b.begin();
  const a=await b.run(nodes[0],[]);await b.run(nodes[1],[a]);
  // Vectorised: 2000 elements are 500 invocations, two workgroups.
  assert.deepEqual(d.dispatches[0],[2,1,1]);
  b.vectorize=false;await b.run(nodes[1],[a]);assert.deepEqual(d.dispatches[1],[4,2,1]);b.dispose();
});
test('WebGPU contract mock: lost device fails closed',async()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d);b.lost='test loss';await assert.rejects(()=>b.begin(),e=>e.code==='DEVICE_LOST');b.dispose();
});
for(const backend of ['cpu-js','wasm'])test(`portable browser acceptance cases also pass in Node: ${backend}`,async()=>{
  const wasmBytes=await readFile(new URL('../wasm/kernels.wasm',import.meta.url));const report=await checkBackend(backend,{wasmBytes});
  assert.equal(report.status,'passed',JSON.stringify(report.checks.filter(c=>c.status!=='passed')));
  assert.equal(report.passed,report.checks.length);assert.ok(report.passed>150);
});

// ---- sessions: what a prepared plan must and must not do on the device ------------------
import {mlpSessionProgram} from './ml-cases.mjs';
const sessionProgram=()=>mlpSessionProgram({sizes:[20,16,5],batch:8});
const idle=b=>[...b.idle.values()].flat();
test('WebGPU session mock: an eight-step run is one submit, one map, carries as in-stream copies, scopes balanced',async()=>{
  const d=fakeDevice(),rt=new ComputeRuntime(new WebGPUBackend(d)),spec=sessionProgram();
  const s=await rt.prepare(spec.program,{resident:['p0','m0','v0','p1','m1','v1','p2','m2','v2','p3','m3','v3']});
  const x=new Float32Array(8*20),y=new Float32Array(8);
  d.submits=0;d.copies=[];d.events=[];
  const r=await s.run(Array.from({length:8},()=>({inputs:{0:x,1:y}})),{readback:['loss']});
  assert.equal(d.submits,1,'one command buffer for eight steps');
  assert.equal(r.steps.length,8);assert.equal(d.scopes,0);
  // Adam updates the held p, m and v in place (webgpu.mjs `adam`), so the
  // only copies are one loss per step into the staging buffer.
  assert.equal(d.copies.length,8);
  assert.equal(d.copies.filter(c=>c.dst.usage&GPUBufferUsage.MAP_READ).length,8,'eight scalar losses into one staging buffer');
  assert.ok(d.events.indexOf('submit')>d.events.lastIndexOf('copy'),'copies are recorded before the submit');
  assert.ok(d.events.indexOf('submit')>d.events.lastIndexOf('write'),'uniforms upload before the submit');
  // Without the fused pass: 12 carries per step as in-stream copies.
  rt.impl.fuseAdam=false;d.submits=0;d.copies=[];
  await s.run(Array.from({length:8},()=>({inputs:{0:x,1:y}})),{readback:['loss']});
  assert.equal(d.submits,1);assert.equal(d.copies.length,8*12+8);
  rt.dispose();
});
test('WebGPU session mock: resident buffers never enter the idle pool and bind groups stop being created once warm',async()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d),rt=new ComputeRuntime(b),spec=sessionProgram();
  const s=await rt.prepare(spec.program,{resident:['p0']});
  const x=new Float32Array(8*20),y=new Float32Array(8),step={inputs:{0:x,1:y}};
  const resident=[...s.retained.keys()].map(h=>h.buffer);
  assert.ok(resident.length>=10);
  for(let i=0;i<3;i++)await s.run(step,{readback:['loss']});
  for(const buffer of resident)assert.ok(!idle(b).includes(buffer)&&!buffer.destroyed,'a resident buffer stays out of the pool');
  const warm=d.bindGroups;await s.run(step,{readback:['loss']});await s.run(step,{readback:['loss']});
  assert.equal(d.bindGroups,warm,'warm runs reuse every bind group');
  assert.ok(d.offsets&&d.offsets.length===1,'uniforms are bound through a dynamic offset');
  assert.equal(d.buffers.filter(x=>x.usage&GPUBufferUsage.MAP_READ&&!x.destroyed).length,1,'one staging buffer is kept');
  // Disposing the session returns its buffers to the pool without destroying them; disposing the runtime destroys everything.
  s.dispose();
  for(const buffer of resident)assert.ok(idle(b).includes(buffer)&&!buffer.destroyed);
  rt.dispose();assert.ok(d.buffers.every(x=>x.destroyed));
});
test('WebGPU session mock: download is a round trip of its own and a failed map leaves the backend usable',async()=>{
  const d=fakeDevice(),rt=new ComputeRuntime(new WebGPUBackend(d)),spec=sessionProgram();
  const s=await rt.prepare(spec.program,{resident:['p0']}),x=new Float32Array(8*20),y=new Float32Array(8);
  await s.run({inputs:{0:x,1:y}},{readback:['loss']});
  d.submits=0;const got=await s.download(['p0']);assert.equal(d.submits,1);assert.equal(got.outputs.p0.data.length,320);assert.equal(d.scopes,0);
  d.failMap=true;await assert.rejects(()=>s.run({inputs:{0:x,1:y}},{readback:['loss']}),/map rejected/);assert.equal(d.scopes,0);assert.equal(rt.busy,false);
  // The map failed after the step advanced its carries on the device, so this session is poisoned
  // and refuses further work; the backend itself is unharmed, so a fresh session on it still runs.
  assert.equal(s.poisoned,true);await assert.rejects(()=>s.run({inputs:{0:x,1:y}},{readback:['loss']}),e=>e.code==='STATE');
  d.failMap=false;const fresh=await rt.prepare(spec.program,{resident:['p0']});
  assert.equal((await fresh.run({inputs:{0:x,1:y}},{readback:['loss']})).steps.length,1);
  rt.dispose();
});
