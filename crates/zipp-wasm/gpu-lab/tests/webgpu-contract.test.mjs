// These are host API/lifecycle mocks. They do NOT compile shaders or establish GPU correctness.
import test from 'node:test';
import assert from 'node:assert/strict';
import {WebGPUBackend} from '../src/backends/webgpu.mjs';
import {validateProgram} from '../src/graph.mjs';
import {checkBackend} from './browser-cases.mjs';
import {readFile} from 'node:fs/promises';
globalThis.GPUBufferUsage={MAP_READ:1,COPY_SRC:4,COPY_DST:8,STORAGE:128};
globalThis.GPUMapMode={READ:1};
function fakeDevice(){
  const d={buffers:[],codes:[],submits:0,scopes:0,failCompile:false,failMap:false,
    limits:{maxStorageBufferBindingSize:4194304,maxBufferSize:4194304,maxComputeWorkgroupsPerDimension:65535},
    lost:new Promise(()=>{}),
    queue:{submit(){d.submits++;},async onSubmittedWorkDone(){}},
    pushErrorScope(){d.scopes++;},async popErrorScope(){d.scopes--;return null;},
    createBuffer(descriptor){const data=new ArrayBuffer(descriptor.size);const b={...descriptor,mapState:descriptor.mappedAtCreation?'mapped':'unmapped',destroyed:false,
      getMappedRange(){return data;},unmap(){this.mapState='unmapped';},async mapAsync(){if(d.failMap)throw Error('map rejected');this.mapState='mapped';},destroy(){this.destroyed=true;}};d.buffers.push(b);return b;},
    createShaderModule({code}){d.codes.push(code);return {code};},
    async createComputePipelineAsync(){if(d.failCompile)throw Error('compile rejected');return {getBindGroupLayout(){return {};}};},
    createBindGroup(x){return x;},
    createCommandEncoder(){return {beginComputePass(){return {setPipeline(){},setBindGroup(){},dispatchWorkgroups(){},end(){}};},copyBufferToBuffer(){},finish(){return {};}};},destroy(){d.destroyed=true;},
  };return d;
}
const plan=(n,op='relu')=>validateProgram({version:1,nodes:[{id:0,op:'input',shape:[n],data:Array(n).fill(1)},{id:1,op,a:0}],outputs:[{name:'result',id:1}]}).nodes;

test('WebGPU contract mock: bounds guard emitted for a non-workgroup-multiple length',async()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d),nodes=plan(65);await b.begin();const a=await b.run(nodes[0],[]),o=await b.run(nodes[1],[a]);
  assert.match(d.codes[0],/if \(i >= 65u\)/);assert.match(d.codes[0],/@workgroup_size\(64\)/);assert.equal(d.submits,1);
  b.free(a);b.free(o);await b.finish();assert.equal(d.scopes,0);b.dispose();
});
test('WebGPU contract mock: pairwise sum has guarded tails and releases scratch buffers',async()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d),nodes=plan(65,'sum');const a=await b.run(nodes[0],[]),o=await b.run(nodes[1],[a]);
  assert.equal(d.submits,7);assert.ok(d.codes.every(code=>code.includes('if (j + 1u <')));
  assert.equal(d.buffers.filter(x=>x.destroyed).length,6);b.free(a);b.free(o);b.dispose();
});
test('WebGPU contract mock: scalar sum uses guarded copy path',async()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d),nodes=plan(1,'sum');const a=await b.run(nodes[0],[]),o=await b.run(nodes[1],[a]);
  assert.equal(d.submits,1);assert.match(d.codes[0],/resultData\[i\] = aData\[0u\]/);b.free(a);b.free(o);b.dispose();
});
test('WebGPU contract mock: rejected compilation destroys the output allocation',async()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d),nodes=plan(3);const a=await b.run(nodes[0],[]);d.failCompile=true;
  await assert.rejects(()=>b.run(nodes[1],[a]),/compile rejected/);assert.equal(d.buffers[1].destroyed,true);b.free(a);b.dispose();
});
test('WebGPU contract mock: mapping rejection destroys staging allocation',async()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d),nodes=plan(3);const a=await b.run(nodes[0],[]);d.failMap=true;
  await assert.rejects(()=>b.read(a),/map rejected/);assert.equal(d.buffers[1].destroyed,true);b.free(a);b.dispose();
});
test('WebGPU contract mock: limit rejection happens before buffer creation',()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d);assert.throws(()=>b.alloc(1048577),e=>e.code==='LIMIT');assert.equal(d.buffers.length,0);b.dispose();
});
test('WebGPU contract mock: lost device fails closed',async()=>{
  const d=fakeDevice(),b=new WebGPUBackend(d);b.lost='test loss';await assert.rejects(()=>b.begin(),e=>e.code==='DEVICE_LOST');b.dispose();
});
for(const backend of ['cpu-js','wasm'])test(`portable browser acceptance cases also pass in Node: ${backend}`,async()=>{
  const wasmBytes=await readFile(new URL('../wasm/kernels.wasm',import.meta.url));const report=await checkBackend(backend,{wasmBytes});
  assert.equal(report.status,'passed',JSON.stringify(report));assert.equal(report.passed,17);
});
