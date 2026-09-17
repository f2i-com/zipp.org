import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile, readdir} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {parseJSON} from '../src/json.mjs';
import {safePath, resolveLimits, sha256} from '../src/common.mjs';
import {FileMapSource, sourceFromBundle, sourceFromFiles} from '../src/sources.mjs';
import {PluginRegistry} from '../src/plugins.mjs';
import {openSafetensors, WeightStore} from '../src/safetensors.mjs';
import {bindGraph} from '../src/bindings.mjs';
import {ModelSession, sampleLogits, seededRandom} from '../src/session.mjs';

const encoder = new TextEncoder(), limits = resolveLimits();
import {sourceDirectory} from './helpers.mjs';

function packed(header, data = new Uint8Array(), raw = false) {
  const json=encoder.encode(raw?header:JSON.stringify(header)),out=new Uint8Array(8+json.length+data.length);
  new DataView(out.buffer).setBigUint64(0,BigInt(json.length),true);out.set(json,8);out.set(data,8+json.length);return out;
}
function tensorHeader(shape=[2],dtype='F32',offsets=[0,8]) {return {w:{dtype,shape,data_offsets:offsets}};}
function source(bytes) {return new FileMapSource(new Map([['w.safetensors',bytes]]));}
const f32=values=>{const data=new Uint8Array(values.length*4);const v=new DataView(data.buffer);values.forEach((x,i)=>v.setFloat32(i*4,x,true));return data;};
const f16=values=>{const data=new Uint8Array(values.length*2);const v=new DataView(data.buffer);values.forEach((x,i)=>v.setUint16(i*2,x,true));return data;};
async function open(bytes,opts={}) {return openSafetensors(source(bytes),'w.safetensors',opts);}

test('JSON escaped strings, primitives and null prototypes',()=>{
  const x=parseJSON('{"q":"a\\\"b","n":-2.5e2,"a":[true,false,null]}');assert.equal(x.q,'a"b');assert.equal(x.n,-250);assert.equal(Object.getPrototypeOf(x),null);
});
test('duplicate keys including escaped names are rejected',()=>{
  assert.throws(()=>parseJSON('{"w":1,"\\u0077":2}'),/Duplicate/);
});
test('JSON invalid syntax, depth and item budgets',()=>{
  for(const text of ['[1,]','{"a":1,}','01','NaN','1e999','{} trailing'])assert.throws(()=>parseJSON(text));
  assert.throws(()=>parseJSON('[[[0]]]',{maxDepth:2}));assert.throws(()=>parseJSON('[1,2,3]',{maxItems:3}));
});
test('canonical paths and bundle prefix scoping',async()=>{
  for(const path of ['../x','a/../x','/root','a\\x','a//x','https://x','a/%2e%2e/x'])assert.throws(()=>safePath(path));
  const s=sourceFromBundle(new Map([['models/a/w',new Uint8Array([1])],['models/b/w',new Uint8Array([2])]]),'models/a');
  assert.deepEqual([...await s.read('w',0,1)],[1]);assert.throws(()=>s.size('../b/w'));
});
test('typed source ownership and range checks',async()=>{
  const bytes=new Uint8Array([1,2]),s=new FileMapSource(new Map([['x',bytes]]));bytes[0]=9;
  assert.deepEqual([...await s.read('x',0,2)],[1,2]);await assert.rejects(s.read('x',1,2));
});
test('duplicate browser file names are rejected',()=>{
  assert.throws(()=>sourceFromFiles([new File(['a'],'x.py'),new File(['b'],'x.py')]),/Duplicate/);
});
test('F32 tensors decode exactly',async()=>{
  const index=await open(packed(tensorHeader(),f32([1,-2])));assert.deepEqual([...await index.readTensor('w')],[1,-2]);
});
test('F16 normals, subnormals and negative zero',async()=>{
  const index=await open(packed(tensorHeader([4],'F16',[0,8]),f16([0x3c00,0xc000,0x0001,0x8000])));
  const out=await index.readTensor('w');assert.equal(out[0],1);assert.equal(out[1],-2);assert.equal(out[2],2**-24);assert.ok(Object.is(out[3],-0));
});
test('BF16 conversion',async()=>{
  const index=await open(packed(tensorHeader([2],'BF16',[0,4]),f16([0x3f80,0xc000])));assert.deepEqual([...await index.readTensor('w')],[1,-2]);
});
test('scalar and empty tensors are valid containers',async()=>{
  const scalar=await open(packed(tensorHeader([],'F32',[0,4]),f32([3])));assert.equal((await scalar.readTensor('w'))[0],3);
  const empty=await open(packed(tensorHeader([0],'F32',[0,0])));assert.equal((await empty.readTensor('w')).length,0);
});
test('unsupported dtype and malformed shapes fail before decoding',async()=>{
  await assert.rejects(open(packed(tensorHeader([1],'I64',[0,8]),new Uint8Array(8))),/Unsupported dtype/);
  await assert.rejects(open(packed(tensorHeader([-1]),f32([1,2]))));
  await assert.rejects(open(packed(tensorHeader([1,1,1,1,2]),f32([1,2]))));
});
test('duplicate Safetensors keys rejected',async()=>{
  const h='{"w":{"dtype":"F32","shape":[1],"data_offsets":[0,4]},"w":{"dtype":"F32","shape":[1],"data_offsets":[0,4]}}';
  await assert.rejects(open(packed(h,f32([1]),true)),/Duplicate/);
});
test('truncation, gaps, overlap, trailing bytes, size mismatch',async()=>{
  for(const [h,d] of [
    [tensorHeader([2],'F32',[0,9]),f32([1,2])],
    [tensorHeader([1],'F32',[4,8]),f32([1,2])],
    [{a:{dtype:'F32',shape:[1],data_offsets:[0,4]},b:{dtype:'F32',shape:[1],data_offsets:[0,4]}},f32([1])],
    [tensorHeader([1],'F32',[0,4]),f32([1,2])],
    [tensorHeader([2],'F32',[0,4]),f32([1])],
  ])await assert.rejects(open(packed(h,d)));
});
test('huge header rejected before header read',async()=>{
  const bytes=new Uint8Array(8);new DataView(bytes.buffer).setBigUint64(0,2n**63n,true);await assert.rejects(open(bytes));
});
test('decoded memory budget includes F16 expansion',async()=>{
  await assert.rejects(open(packed(tensorHeader([2],'F16',[0,4]),f16([0,0])),{maxDecodedBytes:7}),/Decoded model bytes/);
});
test('nonfinite Safetensors numbers explicitly rejected for graph compatibility',async()=>{
  const i=await open(packed(tensorHeader(),f32([NaN,Infinity])));await assert.rejects(i.readTensor('w'),/Non-finite/);
});
test('weight cache shares concurrent reads; disposal invalidates',async()=>{
  let reads=0;const index=await open(packed(tensorHeader(),f32([1,2])));const read=index.readTensor;
  index.readTensor=async name=>{reads++;await new Promise(r=>setTimeout(r,2));return read(name);};
  const store=new WeightStore([index],limits);const [a,b]=await Promise.all([store.tensor('w'),store.tensor('w')]);assert.equal(reads,1);assert.equal(a,b);store.dispose();assert.throws(()=>store.info('w'));
});
test('dispose during asynchronous load cannot revive model data',async()=>{
  const index=await open(packed(tensorHeader(),f32([1,2]))),read=index.readTensor;index.readTensor=async n=>{await new Promise(r=>setTimeout(r,5));return read(n);};
  const store=new WeightStore([index],limits),pending=store.tensor('w');store.dispose();await assert.rejects(pending,/disposed/);
});
test('aggregate shards and duplicate tensor names',async()=>{
  const i=await open(packed(tensorHeader(),f32([1,2])));assert.throws(()=>new WeightStore([i,i],limits),/Duplicate/);
});
test('approval mandatory and source integrity verified',async()=>{
  const pluginSource=await sourceDirectory('../plugins/tiny-causal/'),r=new PluginRegistry();
  await assert.rejects(r.install(pluginSource),/approve/);await assert.rejects(r.install(pluginSource,{approve:()=>false}),/not approved/);
  const p=await r.install(pluginSource,{approve:()=>true});assert.equal(p.identity.id,'org.zipp.tiny-causal');assert.ok(Object.isFrozen(p.files));
  assert.ok(Object.keys(p.files).every(n=>n.endsWith('.py')));assert.equal(r.get(p.identity.id,p.identity.version),p);
});
test('tampered Python source cannot be installed',async()=>{
  const s=await sourceDirectory('../plugins/tiny-causal/');const altered={size:p=>p==='architecture.py'?1:s.size(p),read:(p,o,l)=>p==='architecture.py'?Promise.resolve(new Uint8Array([0])):s.read(p,o,l)};
  await assert.rejects(new PluginRegistry().install(altered,{approve:()=>true}),/digest mismatch/);
});
test('pinned manifest digest rejects a wrong package before execution',async()=>{
  const s=await sourceDirectory('../plugins/tiny-causal/');await assert.rejects(new PluginRegistry().install(s,{approve:()=>true,expectedHash:'0'.repeat(64)}),/digest mismatch/);
});
test('actual generated Python graphs bind binary weight arrays',async()=>{
  const modelSource=await sourceDirectory('../examples/tiny-char/'),index=await openSafetensors(modelSource,'weights.safetensors'),store=new WeightStore([index],limits);
  const cases=JSON.parse(await readFile(new URL('../examples/tiny-char/graph-cases.json',import.meta.url),'utf8')).cases;
  for(const c of cases){const graph=await bindGraph(c.template,store,limits);assert.ok(graph.nodes.filter(n=>n.op==='input').every(n=>n.data instanceof Float32Array));assert.equal(graph.outputs[0].name,'logits');}
  store.dispose();
});
test('binding shape/index/capability errors reject',async()=>{
  const index=await open(packed(tensorHeader([1,2]),f32([1,2]))),store=new WeightStore([index],limits);
  const template=b=>({version:1,graph:{version:2,nodes:[{id:0,op:'input',shape:[1,2]}],outputs:[{name:'logits',id:0}]},bindings:[b]});
  await assert.rejects(bindGraph(template({node:0,kind:'rows',tensor:'w',indices:[1]}),store,limits),/Embedding index/);
  await assert.rejects(bindGraph(template({node:0,kind:'fetch',url:'https://x'}),store,limits),/Unknown/);
  await assert.rejects(bindGraph(template({node:0,kind:'tensor',tensor:'not-there'}),store,limits),/Missing tensor/);
  const t=template({node:0,kind:'tensor',tensor:'w'});t.graph.nodes[0].data=[1,2];await assert.rejects(bindGraph(t,store,limits),/exactly one/);
});
test('model manifest cannot silently switch architecture plugins',async()=>{
  const plugin=await new PluginRegistry().install(await sourceDirectory('../plugins/bigram/'),{approve:()=>true});let made=0;
  await assert.rejects(ModelSession.open({source:await sourceDirectory('../examples/tiny-char/'),plugin,engineFactory:()=>{made++;},runtime:{execute(){}}}),/different exact plugin/);assert.equal(made,0);
});
test('greedy selection, stable seeded sampling, extreme temperature',()=>{
  assert.equal(sampleLogits(new Float32Array([2,3,3])),1);assert.equal(sampleLogits([1000,999],{temperature:Number.MIN_VALUE}),0);
  assert.equal(sampleLogits([1,4,2],{temperature:1,topK:1}),1);
  const a=seededRandom(0),b=seededRandom(0);assert.deepEqual(Array.from({length:20},()=>a()),Array.from({length:20},()=>b()));
  assert.throws(()=>sampleLogits([NaN,1]));assert.throws(()=>sampleLogits([1,2],{topK:3}));
});


test('Python packages may begin with an empty __init__.py', async () => {
  const code = encoder.encode('def describe(config):\n    return config\n');
  const empty = new Uint8Array();
  const manifest = {
    format: 'zipp.python-model-plugin', version: 1,
    id: 'org.example.empty-init', plugin_version: '0.1.0',
    entry: 'architecture', capabilities: ['graph-v2'],
    sources: {'__init__.py': await sha256(empty), 'architecture.py': await sha256(code)}
  };
  const entries = new Map([
    ['plugin.json', encoder.encode(JSON.stringify(manifest))],
    ['__init__.py', empty], ['architecture.py', code]
  ]);
  const plugin = await new PluginRegistry().install(new FileMapSource(entries), {approve: () => true});
  assert.equal(plugin.files['zipp_plugin/__init__.py'], '');
});
