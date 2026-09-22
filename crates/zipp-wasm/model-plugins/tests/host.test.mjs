import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile, readdir, access} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {parseJSON} from '../src/json.mjs';
import {safePath, resolveLimits, sha256} from '../src/common.mjs';
import {FileMapSource, sourceFromBundle, sourceFromFiles} from '../src/sources.mjs';
import {PluginRegistry} from '../src/plugins.mjs';
import {openSafetensors, WeightStore} from '../src/safetensors.mjs';
import {bindGraph} from '../src/bindings.mjs';
import {ModelSession, sampleLogits, seededRandom} from '../src/session.mjs';
import {stepInputs} from '../src/decode.mjs';

const encoder = new TextEncoder(), limits = resolveLimits();
import {directoryEntries, sourceDirectory} from './helpers.mjs';

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
test('malformed shapes fail before decoding',async()=>{
  await assert.rejects(open(packed(tensorHeader([-1]),f32([1,2]))));
  await assert.rejects(open(packed(tensorHeader([1,1,1,1,2]),f32([1,2]))));
  await assert.rejects(open(packed({w:{dtype:'not a dtype',shape:[1],data_offsets:[0,8]}},new Uint8Array(8))),/dtype/);
});
// A real checkpoint carries buffers this host never decodes: GPT-Neo ships a
// BOOL causal mask for every layer. Indexing such a file has to succeed, or a
// loadable checkpoint is refused over a tensor nothing binds.
test('an unreadable dtype is indexed and only refused when something reads it',async()=>{
  const index=await open(packed({keep:{dtype:'F32',shape:[2],data_offsets:[0,8]},mask:{dtype:'BOOL',shape:[4],data_offsets:[8,12]}},
    new Uint8Array([...f32([1,-2]),1,0,1,0])));
  assert.deepEqual([...await index.readTensor('keep')],[1,-2]);
  assert.equal(index.tensors.get('mask').readable,false);
  assert.equal(index.decodedBytes,8,'an undecodable tensor is not charged to the decode budget');
  await assert.rejects(index.readTensor('mask'),/dtype BOOL is unsupported/);
  const store=new WeightStore([index],limits);
  assert.throws(()=>store.info('mask'),/dtype BOOL is unsupported/);
  assert.deepEqual([...store.info('keep').shape],[2]);
  store.dispose();
});
// The shape limits bound what is decoded, so a tensor that never is -- a
// 4096-square BOOL mask, a rank-5 index buffer -- does not refuse the file.
test('a tensor that is never decoded is not held to the decode shape limits',async()=>{
  const index=await open(packed({keep:{dtype:'F32',shape:[2],data_offsets:[0,8]},
    mask:{dtype:'BOOL',shape:[1,1,4096,4096],data_offsets:[8,8+4096*4096]},
    ids:{dtype:'I64',shape:[1,1,1,1,1],data_offsets:[8+4096*4096,16+4096*4096]}},
    new Uint8Array(16+4096*4096)));
  assert.equal(index.tensors.get('mask').elements,4096*4096);
  await assert.rejects(index.readTensor('mask'),/dtype BOOL is unsupported/);
  await assert.rejects(open(packed({w:{dtype:'BOOL',shape:[-1],data_offsets:[0,0]}})),/dimension/);
});
test('a transposing binding reads a checkpoint-order matrix',async()=>{
  const index=await open(packed(tensorHeader([2,3],'F32',[0,24]),f32([1,2,3,4,5,6]))),store=new WeightStore([index],limits);
  const template=(shape,transpose)=>({version:1,graph:{version:2,
    nodes:[{id:0,op:'input',shape}],outputs:[{name:'logits',id:0}]},
    bindings:[transpose===undefined?{node:0,kind:'tensor',tensor:'w'}:{node:0,kind:'tensor',tensor:'w',transpose}]});
  assert.deepEqual([...(await bindGraph(template([2,3]),store,limits)).nodes[0].data],[1,2,3,4,5,6]);
  assert.deepEqual([...(await bindGraph(template([3,2],true),store,limits)).nodes[0].data],[1,4,2,5,3,6]);
  await assert.rejects(bindGraph(template([2,3],true),store,limits),/Weight shape mismatch/);
  await assert.rejects(bindGraph(template([3,2],false),store,limits),/transpose is true when present/);
  // The store's cached copy must survive a transposed binding unchanged.
  assert.deepEqual([...(await bindGraph(template([2,3]),store,limits)).nodes[0].data],[1,2,3,4,5,6]);
  store.dispose();
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
// The next model milestone is a SEPARATE GPT-Neo/TinyStories plugin carrying
// that family's tokenizer and state-dict mapping. Until such a plugin exists and
// has been verified against those checkpoints, the custom fixture must refuse
// them: emitting the same transformer operations is not checkpoint
// compatibility, and these two tests are what keeps that from being advertised.
async function modelEntriesWith(changes){
  const entries=await directoryEntries('../examples/tiny-char/');
  const model=JSON.parse(new TextDecoder().decode(entries.get('model.json')));
  entries.set('model.json',encoder.encode(JSON.stringify(changes(model))));
  return entries;
}
test('a GPT-Neo/TinyStories checkpoint is refused before any engine is created',async()=>{
  const plugin=await new PluginRegistry().install(await sourceDirectory('../plugins/tiny-causal/'),{approve:()=>true});
  const entries=await modelEntriesWith(model=>({...model,
    architecture:{...model.architecture,sha256:plugin.identity.sha256},
    checkpoint_format:'hf.gpt-neo-v1',
    tokenizer:{...model.tokenizer,type:'gpt2-byte-bpe-v1'}}));
  let made=0;
  await assert.rejects(ModelSession.open({source:new FileMapSource(entries),plugin,
    engineFactory:()=>{made++;},runtime:{execute(){}}}),
    error=>error.code==='CHECKPOINT'&&/not checkpoint compatibility/.test(error.message));
  assert.equal(made,0);
});
test('a foreign tokenizer is refused even for the right checkpoint family',async()=>{
  const plugin=await new PluginRegistry().install(await sourceDirectory('../plugins/tiny-causal/'),{approve:()=>true});
  const entries=await modelEntriesWith(model=>({...model,tokenizer:{...model.tokenizer,type:'gpt2-byte-bpe-v1'}}));
  await assert.rejects(ModelSession.open({source:new FileMapSource(entries),plugin,
    engineFactory:()=>{throw Error('must not construct an engine');},runtime:{execute(){}}}),
    error=>error.code==='TOKENIZER'&&/tokenizer its checkpoints were trained with/.test(error.message));
});
test('the bundled plugins advertise only their own checkpoint family',async()=>{
  for(const name of ['tiny-causal','bigram']){
    const plugin=await new PluginRegistry().install(await sourceDirectory(`../plugins/${name}/`),{approve:()=>true});
    assert.equal(plugin.support.checkpoint_format,name==='tiny-causal'?'zipp.tiny-causal-v1':'zipp.bigram-v1');
    assert.deepEqual([...plugin.support.tokenizer_formats],['character-v1']);
    assert.ok(Object.isFrozen(plugin.support));
  }
});
test('a plugin manifest missing its declared support is rejected',async()=>{
  const entries=await directoryEntries('../plugins/bigram/');
  const manifest=JSON.parse(new TextDecoder().decode(entries.get('plugin.json')));
  delete manifest.checkpoint_format;
  entries.set('plugin.json',encoder.encode(JSON.stringify(manifest)));
  await assert.rejects(new PluginRegistry().install(new FileMapSource(entries),{approve:()=>true}),/Missing field: checkpoint_format/);
  const bad={...manifest,checkpoint_format:'GPT-Neo v1',tokenizer_formats:['character-v1']};
  entries.set('plugin.json',encoder.encode(JSON.stringify(bad)));
  await assert.rejects(new PluginRegistry().install(new FileMapSource(entries),{approve:()=>true}),/format identifier/);
});
test('greedy selection, stable seeded sampling, extreme temperature',()=>{
  assert.equal(sampleLogits(new Float32Array([2,3,3])),1);assert.equal(sampleLogits([1000,999],{temperature:Number.MIN_VALUE}),0);
  assert.equal(sampleLogits([1,4,2],{temperature:1,topK:1}),1);
  const a=seededRandom(0),b=seededRandom(0);assert.deepEqual(Array.from({length:20},()=>a()),Array.from({length:20},()=>b()));
  assert.throws(()=>sampleLogits([NaN,1]));assert.throws(()=>sampleLogits([1,2],{topK:3}));
});
// Top-p is cut before temperature, as the documented order says: the nucleus
// of softmax([2,1,0]) is {0,1} at any temperature, so a cold sampler still
// picks token 1 sometimes rather than collapsing onto token 0.
test('top-p takes its nucleus before temperature is applied',()=>{
  const counts=[0,0,0];
  for(let i=0;i<1000;i++)counts[sampleLogits([2,1,0],{temperature:0.5,topP:0.7,random:()=>(i+0.5)/1000})]++;
  assert.equal(counts[2],0,'token 2 is outside the nucleus');
  assert.ok(counts[1]>50,`token 1 is inside the nucleus and should be drawn, got ${counts}`);
});


test('Python packages may begin with an empty __init__.py', async () => {
  const code = encoder.encode('def describe(config):\n    return config\n');
  const empty = new Uint8Array();
  const manifest = {
    format: 'zipp.python-model-plugin', version: 1,
    id: 'org.example.empty-init', plugin_version: '0.1.0',
    entry: 'architecture', capabilities: ['graph-v2'],
    checkpoint_format: 'org.example.empty-v1', tokenizer_formats: ['character-v1'],
    sources: {'__init__.py': await sha256(empty), 'architecture.py': await sha256(code)}
  };
  const entries = new Map([
    ['plugin.json', encoder.encode(JSON.stringify(manifest))],
    ['__init__.py', empty], ['architecture.py', code]
  ]);
  const plugin = await new PluginRegistry().install(new FileMapSource(entries), {approve: () => true});
  assert.equal(plugin.files['zipp_plugin/__init__.py'], '');
});

test('a matrix binding decodes where a source has no block form', async () => {
  // `matrix` asks for the checkpoint's own [out, in] weight and lets the host
  // keep it quantized where it can. A Safetensors file has no block form at
  // all, so this is the fallback path -- decoded to float32, still in the
  // stored layout, because the graph multiplies it transposed either way.
  // Every tensor in the Qwen3 checkpoint is Q4_K or Q6_K, so nothing there
  // reaches this branch; a Q5_K checkpoint would hit it first.
  const [outs, ins] = [3, 4];
  const values = Array.from({length: outs * ins}, (_, i) => (i % 7) / 3 - 1);
  const bytes = packed({w: {dtype: 'F32', shape: [outs, ins], data_offsets: [0, outs * ins * 4]}}, f32(values));
  const store = new WeightStore([await open(bytes)], limits);
  try {
    assert.equal(store.residentDtype('w'), null, 'Safetensors has no block form');
    await assert.rejects(store.blocks('w'), /no block form/);

    const a = [0.5, -1.0, 0.25, 2.0];
    const graph = await bindGraph({
      version: 1,
      graph: {version: 2, nodes: [
        {id: 0, op: 'input', shape: [1, ins], data: a},
        {id: 1, op: 'input', shape: [outs, ins]},
        {id: 2, op: 'matmul', a: 0, b: 1, transposed: true},
      ], outputs: [{name: 'logits', id: 2}]},
      bindings: [{node: 1, kind: 'matrix', tensor: 'w'}],
    }, store, limits);

    // Decoded floats in the stored layout, no dtype, no transposed copy.
    assert.ok(graph.nodes[1].data instanceof Float32Array);
    assert.equal(graph.nodes[1].dtype, undefined);
    assert.deepEqual([...graph.nodes[1].data], values.map(Math.fround));

    const runtimeURL = new URL('../../gpu-lab/src/runtime.mjs', import.meta.url);
    try { await access(runtimeURL); } catch { return; } // sibling package, not a dependency
    const {createRuntime} = await import(runtimeURL);
    const runtime = await createRuntime({backend: 'cpu-js'});
    try {
      const got = await runtime.execute(graph, {typedOutputs: true});
      const want = Array.from({length: outs}, (_, o) => {
        let sum = 0;
        for (let i = 0; i < ins; i++) sum = Math.fround(sum + Math.fround(a[i] * values[o * ins + i]));
        return sum;
      });
      assert.deepEqual([...got.outputs.logits.data], want);
    } finally { runtime.dispose(); }
  } finally { store.dispose(); }
});

test('the catalogue pins every plugin, at the digest it actually has', async () => {
  // How the demo broke: a plugin's source changed, `plugin.json` was rehashed,
  // and `catalog.v1.json` still pinned the digest of the manifest before that
  // -- so downloading it failed the hash check at the point of use, which is
  // the right refusal arriving far too late to be useful.
  //
  // Nothing checked this. tools/build_manifests.py regenerates the catalogue
  // and had a hardcoded plugin list that qwen3 was never added to, so running
  // it *removed* the entry instead of refreshing it. Both failures are the
  // same shape: a generated file nobody compares against its sources.
  const root = new URL('../', import.meta.url);
  const catalogue = JSON.parse(await readFile(new URL('catalog.v1.json', root), 'utf8'));
  assert.equal(catalogue.version, 1);
  assert.ok(catalogue.plugins.length > 0, 'a catalogue with no plugins offers nothing');

  const plugins = (await readdir(new URL('plugins/', root), {withFileTypes: true}))
    .filter(entry => entry.isDirectory()).map(entry => entry.name).sort();
  const listed = catalogue.plugins.map(p => p.manifest.split('/')[1]).sort();
  assert.deepEqual(listed, plugins,
    'every plugin directory must be in the catalogue and vice versa');

  for (const entry of catalogue.plugins) {
    const bytes = await readFile(new URL(entry.manifest, root));
    const digest = createHash('sha256').update(bytes).digest('hex');
    assert.equal(digest, entry.sha256,
      `${entry.id}: the catalogue pins ${entry.sha256.slice(0, 16)} but ` +
      `${entry.manifest} is ${digest.slice(0, 16)}. Run tools/build_manifests.py.`);

    // And the manifest's own claims must match what it pins, since the
    // catalogue repeats them and a reader may believe either.
    const manifest = JSON.parse(bytes.toString('utf8'));
    assert.equal(manifest.id, entry.id);
    assert.equal(manifest.plugin_version, entry.version);
    assert.equal(manifest.checkpoint_format, entry.checkpoint_format);
    assert.deepEqual(manifest.tokenizer_formats, entry.tokenizer_formats);

    // The sources a manifest pins have to be the sources on disk, which is the
    // check the registry makes at install time -- here so it fails in CI first.
    for (const [name, sha] of Object.entries(manifest.sources)) {
      const source = await readFile(new URL(`plugins/${entry.manifest.split('/')[1]}/${name}`, root));
      assert.equal(createHash('sha256').update(source).digest('hex'), sha,
        `${entry.id}: ${name} does not match the digest ${entry.manifest} pins`);
    }
  }
});

// A selector over a one-token step names that one token; it is not a write
// column, which is what the single-step path used to feed it.
test('a one-token step feeds a select slot one value', async () => {
  const plan = {tokens: 1, context: 8, steps: [{node: 's', slot: 'select'}, {node: 'w', slot: 'write'}]};
  const {inputs} = await stepInputs(plan, null, {position: 3});
  assert.deepEqual([...inputs.s], [1]);
  assert.deepEqual([...inputs.w], [0, 0, 0, 1, 0, 0, 0, 0]);
});
