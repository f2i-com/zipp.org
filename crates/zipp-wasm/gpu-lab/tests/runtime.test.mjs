import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createRuntime, ComputeRuntime} from '../src/runtime.mjs';
import {validateProgram} from '../src/graph.mjs';
import {CPUBackend} from '../src/backends/cpu.mjs';
const wasmBytes=await readFile(new URL('../wasm/kernels.wasm',import.meta.url));
const load=async name=>JSON.parse(await readFile(new URL(`../generated/${name}.json`,import.meta.url)));
const input=(id,data,shape=[data.length])=>({id,op:'input',shape,data});
const graph=(nodes,outputs=[{name:'result',id:nodes.length-1}])=>({version:1,nodes,outputs});
function close(a,b,atol=1e-5,rtol=1e-5){assert.equal(a.length,b.length);a.forEach((x,i)=>assert.ok(Math.abs(x-b[i])<=atol+rtol*Math.abs(b[i]),`${i}: ${x} != ${b[i]}`));}

for(const backend of ['cpu-js','wasm']) {
  test(`${backend}: Python-generated vector recipe`,async()=>{
    const rt=await createRuntime({backend,wasmBytes});const r=await rt.execute(await load('vector'));
    assert.deepEqual(r.outputs.result.data,[14,44,94,164]);assert.deepEqual(r.outputs.total.data,[316]);
    assert.equal(r.backend,backend);assert.equal(r.stats.nodes,7);rt.dispose();
  });
  test(`${backend}: rectangular matrix multiplication`,async()=>{
    const rt=await createRuntime({backend,wasmBytes});const r=await rt.execute(await load('matmul'));
    assert.deepEqual(r.outputs.result.shape,[2,2]);assert.deepEqual(r.outputs.result.data,[58,64,139,154]);rt.dispose();
  });
  test(`${backend}: tiny MLP has expected explicit-weight result`,async()=>{
    const rt=await createRuntime({backend,wasmBytes});const r=await rt.execute(await load('tiny_mlp'));
    assert.deepEqual(r.outputs.result.data,[4.375,2.125,-1.125,3.375]);rt.dispose();
  });
  test(`${backend}: 24 Life steps translate a glider by six cells`,async()=>{
    const rt=await createRuntime({backend,wasmBytes});const r=await rt.execute(await load('life'));
    const active=r.outputs.result.data.flatMap((v,i)=>v?[Math.floor(i/32)+','+(i%32)]:[]);
    assert.deepEqual(active,['9,10','10,11','11,9','11,10','11,11']);assert.deepEqual(r.outputs.alive.data,[5]);rt.dispose();
  });
  test(`${backend}: odd reduction, scalar operations and output alias`,async()=>{
    const nodes=[input(0,[-2,1,3,4,5]),input(1,[2],[]),{id:2,op:'sub',a:1,b:0},{id:3,op:'relu',a:2},{id:4,op:'sum',a:3}];
    const rt=await createRuntime({backend,wasmBytes});
    const r=await rt.execute(graph(nodes,[{name:'total',id:4},{name:'alias',id:4},{name:'values',id:3}]));
    assert.deepEqual(r.outputs.values.data,[4,1,0,0,0]);assert.deepEqual(r.outputs.total.data,[5]);assert.deepEqual(r.outputs.alias.data,[5]);rt.dispose();
  });
  test(`${backend}: sum of one element, full scalar, unused node`,async()=>{
    const nodes=[input(0,[8],[]),{id:1,op:'sum',a:0},{id:2,op:'full',shape:[5],value:42}];
    const rt=await createRuntime({backend,wasmBytes});const r=await rt.execute(graph(nodes,[{name:'result',id:1}]));
    assert.deepEqual(r.outputs.result.data,[8]);rt.dispose();
  });
  test(`${backend}: repeated executions do not reuse stale data`,async()=>{
    const rt=await createRuntime({backend,wasmBytes});
    for(let v=0;v<40;v++)assert.deepEqual((await rt.execute(graph([{id:0,op:'full',shape:[3],value:v}]))).outputs.result.data,[v,v,v]);
    rt.dispose();await assert.rejects(()=>rt.execute(graph([input(0,[1])])),e=>e.code==='DISPOSED');
  });
  test(`${backend}: Float32Array inputs and typed outputs match the list form`,async()=>{
    const rt=await createRuntime({backend,wasmBytes});
    const list=graph([input(0,[0.1,-2,3.5,4],[2,2]),input(1,[1,2,3,4],[2,2]),{id:2,op:'matmul',a:0,b:1},{id:3,op:'relu',a:2},{id:4,op:'sum',a:3}],
      [{name:'values',id:3},{name:'alias',id:3},{name:'total',id:4}]);
    const typed=structuredClone(list);for(const n of typed.nodes)if(n.op==='input')n.data=Float32Array.from(n.data);
    const a=await rt.execute(list),b=await rt.execute(typed,{typedOutputs:true});
    for(const name of ['values','alias','total']){
      assert.ok(b.outputs[name].data instanceof Float32Array,name);assert.deepEqual(Array.from(b.outputs[name].data),a.outputs[name].data);
    }
    assert.notEqual(b.outputs.values.data,b.outputs.alias.data,'each output owns its array');
    assert.ok(Array.isArray((await rt.execute(typed)).outputs.values.data),'lists unless typed outputs are asked for');
    rt.dispose();
  });
  test(`${backend}: finite input that overflows computation rejects readback`,async()=>{
    const rt=await createRuntime({backend,wasmBytes});const nodes=[input(0,[3e38]),{id:1,op:'mul',a:0,b:0}];
    await assert.rejects(()=>rt.execute(graph(nodes)),e=>e.code==='NUMBER');
    assert.deepEqual((await rt.execute(graph([input(0,[2])]))).outputs.result.data,[2]);rt.dispose();
  });
}

test('deterministic random differential tests across JS and compiled WASM',async()=>{
  const a=await createRuntime({backend:'cpu-js'}),b=await createRuntime({backend:'wasm',wasmBytes});
  let seed=1234567;const random=()=>{seed=(Math.imul(seed,1664525)+1013904223)>>>0;return seed/4294967296*4-2;};
  for(const len of [1,2,3,63,64,65,127,129,1025]){
    const nodes=[input(0,Array.from({length:len},random)),input(1,Array.from({length:len},random)),
      {id:2,op:'mul',a:0,b:1},{id:3,op:'add',a:2,b:0},{id:4,op:'relu',a:3},{id:5,op:'sum',a:4}];
    const p=graph(nodes,[{name:'values',id:4},{name:'total',id:5}]);
    const x=await a.execute(p),y=await b.execute(p);close(x.outputs.values.data,y.outputs.values.data);close(x.outputs.total.data,y.outputs.total.data);
  }
  for(const [m,k,n] of [[1,1,1],[2,3,7],[7,5,3],[17,13,9]]){
    const p=graph([input(0,Array.from({length:m*k},random),[m,k]),input(1,Array.from({length:k*n},random),[k,n]),{id:2,op:'matmul',a:0,b:1}]);
    close((await a.execute(p)).outputs.result.data,(await b.execute(p)).outputs.result.data);
  }
  a.dispose();b.dispose();
});

test('WASM module has no imports and grows/rebinds its memory safely',async()=>{
  const module=await WebAssembly.compile(wasmBytes);assert.deepEqual(WebAssembly.Module.imports(module),[]);
  const rt=await createRuntime({backend:'wasm',wasmBytes});
  const p=graph([{id:0,op:'full',shape:[512,512],value:1},{id:1,op:'sum',a:0}]);
  assert.deepEqual((await rt.execute(p)).outputs.result.data,[262144]);rt.dispose();
});

const invalidCases=[
  ['version',p=>{p.version=3;}], ['unknown op',p=>{p.nodes[1].op='__proto__';}],
  ['extra field',p=>{p.nodes[1].source='arbitrary shader';}],['forward reference',p=>{p.nodes[1].a=2;}],
  ['negative reference',p=>{p.nodes[1].a=-1;}],['duplicate ID',p=>{p.nodes[1].id=0;}],
  ['wrong data size',p=>{p.nodes[0].data=[];}],['NaN',p=>{p.nodes[0].data[0]=NaN;}],
  ['float32 overflow',p=>{p.nodes[0].data[0]=1e100;}],['boolean',p=>{p.nodes[0].data[0]=true;}],
  ['zero dimension',p=>{p.nodes[0].shape=[0];}],['fractional dimension',p=>{p.nodes[0].shape=[1.5];}],
  ['unsafe output',p=>{p.outputs[0].name='constructor';}],['duplicate output',p=>{p.outputs.push({...p.outputs[0]});}],
  ['invalid output reference',p=>{p.outputs[0].id=9;}],['empty outputs',p=>{p.outputs=[];}],
];
for(const [name,mutate]of invalidCases)test(`validator rejects ${name}`,()=>{
  const p=graph([input(0,[1,2]),{id:1,op:'sum',a:0}]);mutate(p);assert.throws(()=>validateProgram(p));
});
test('validator rejects data accessors and inherited fields',()=>{
  const p=graph([input(0,[1])]);Object.defineProperty(p.nodes[0],'data',{get(){throw Error('not called');},enumerable:true});
  assert.throws(()=>validateProgram(p),e=>e.code==='PROTOCOL');
  assert.throws(()=>validateProgram(Object.create(p)),e=>e.code==='PROTOCOL');
});
test('shape mismatch and invalid matmul fail before backend allocation',()=>{
  assert.throws(()=>validateProgram(graph([input(0,[1,2]),input(1,[1,2,3]),{id:2,op:'add',a:0,b:1}])),e=>e.code==='SHAPE');
  assert.throws(()=>validateProgram(graph([input(0,[1,2]),{id:1,op:'matmul',a:0,b:0}])),e=>e.code==='SHAPE');
});
test('explicit node, memory, work, input and output limits are enforced',()=>{
  const p=graph([input(0,[1,2]),{id:1,op:'sum',a:0}]);
  for(const limits of [{maxNodes:1},{maxLogicalBytes:4},{maxWork:1},{maxInputElements:1}])assert.throws(()=>validateProgram(p,limits),e=>e.code==='LIMIT');
  assert.throws(()=>validateProgram(graph([input(0,[1,2])]),{maxOutputElements:1}),e=>e.code==='LIMIT');
});
test('busy execution and dispose-while-running reject without interrupting work',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),p=graph([input(0,[1])]);const first=rt.execute(p);
  await assert.rejects(()=>rt.execute(p),e=>e.code==='BUSY');assert.throws(()=>rt.dispose(),e=>e.code==='BUSY');
  await first;rt.dispose();
});
test('backend failure frees already allocated resources and remains diagnosable',async()=>{
  class Fail extends CPUBackend {constructor(){super();this.freed=0;}async run(n,r){if(n.op==='sum')throw Error('test failure');return super.run(n,r);}free(){this.freed++;}}
  const backend=new Fail(),rt=new ComputeRuntime(backend);
  await assert.rejects(()=>rt.execute(graph([input(0,[1,2]),{id:1,op:'sum',a:0}])),/test failure/);
  assert.equal(backend.freed,1);assert.equal(rt.busy,false);rt.dispose();
});
test('invalid backend selection does not silently switch implementation',async()=>{
  await assert.rejects(()=>createRuntime({backend:'cuda'}),e=>e.code==='BACKEND');
  await assert.rejects(()=>createRuntime({backend:'webgpu'}),e=>e.code==='UNAVAILABLE');
});
test('validated graph owns input data before awaiting',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),p=graph([input(0,[1,2])]);const result=rt.execute(p);p.nodes[0].data[0]=999;
  assert.deepEqual((await result).outputs.result.data,[1,2]);
  const typed=graph([input(0,new Float32Array([1,2]))]);const later=rt.execute(typed);typed.nodes[0].data[0]=999;
  assert.deepEqual((await later).outputs.result.data,[1,2]);rt.dispose();
});
test('validator rejects non-finite, mis-sized and non-float32 typed input',()=>{
  for(const data of [new Float32Array([1,NaN]),new Float32Array([Infinity,1]),new Float32Array([1]),new Float64Array([1,2]),new Uint8Array([1,2])])
    assert.throws(()=>validateProgram(graph([input(0,data,[2])])),e=>['NUMBER','SHAPE'].includes(e.code),String(data.constructor.name));
});
