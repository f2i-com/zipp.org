// FULL-CHECKOUT GATES. A skip is not a pass. Set ZIPP_REQUIRE_INTEGRATION=1
// before release to make missing upstream/runtime artifacts a hard failure.
import test from 'node:test';import assert from 'node:assert/strict';
import {readFile,access} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
import {PluginRegistry,ModelSession,openSafetensors,WeightStore,bindGraph,resolveLimits} from '../src/index.mjs';
import {sourceDirectory, lazyDirectory} from './helpers.mjs';
const runtimeURL=new URL('../../gpu-lab/src/runtime.mjs',import.meta.url);
const engineURL=new URL('../../dist/all/zipp_wasm.js',import.meta.url);
const wasmURL=new URL('../../dist/all/zipp_wasm_bg.wasm',import.meta.url);
const exists=async url=>{try{await access(url);return true;}catch{return false;}};
const hasRuntime=await exists(runtimeURL),hasEngine=hasRuntime&&await exists(engineURL)&&await exists(wasmURL);
const required=process.env.ZIPP_REQUIRE_INTEGRATION==='1';
const gate=(present,reason)=>({skip:!present&&!required?reason:false});
const json=async path=>JSON.parse(await readFile(new URL(path,import.meta.url),'utf8'));
function compare(actual,expected,tolerance=5e-5){assert.equal(actual.length,expected.length);let max=0;for(let i=0;i<actual.length;i++){const error=Math.abs(actual[i]-expected[i]);max=Math.max(max,error);assert.ok(error<=tolerance+Math.abs(expected[i])*2e-5,`logit ${i}: error ${error}`);}return max;}
test('checkout gate: real ZIPP Graph v2 validator + CPU runtime versus PyTorch',gate(hasRuntime,'Upstream GPU runtime not present in this source overlay'),async()=>{
  assert.ok(hasRuntime,'Full ZIPP checkout required');const {createRuntime}=await import(runtimeURL);const runtime=await createRuntime({backend:'cpu-js'});
  const source=await sourceDirectory('../examples/tiny-char/'),limits=resolveLimits();const store=new WeightStore([await openSafetensors(source,'weights.safetensors',limits)],limits);
  try{const plans=await json('../examples/tiny-char/graph-cases.json'),oracle=await json('../examples/tiny-char/oracle.json');let max=0;
    for(let i=0;i<plans.cases.length;i++){const graph=await bindGraph(plans.cases[i].template,store,limits);const result=await runtime.execute(graph,{typedOutputs:true});max=Math.max(max,compare(result.outputs.logits.data,oracle.cases[i].logits.flat()));}
    console.log('Upstream Graph v2/CPU max logit error:',max);
  }finally{store.dispose();runtime.dispose();}
});
test('checkout gate: real Python ZIPP WASM ModelSession logits and generation',gate(hasEngine,'Python-enabled ZIPP WASM build not present in this source overlay'),async()=>{
  assert.ok(hasEngine,'Build crates/zipp-wasm/dist/all before this gate');const zipp=await import(engineURL);await zipp.default({module_or_path:await readFile(wasmURL)});
  const {createRuntime}=await import(runtimeURL);const runtime=await createRuntime({backend:'cpu-js'});let session;
  try{const plugin=await new PluginRegistry().install(await sourceDirectory('../plugins/tiny-causal/'),{approve:()=>true});
    session=await ModelSession.open({source:await sourceDirectory('../examples/tiny-char/'),plugin,engineFactory:()=>new zipp.Engine(),runtime});
    const oracle=await json('../examples/tiny-char/oracle.json');
    for(const c of oracle.cases){const result=await session.infer(c.tokens);compare(result.logits,c.logits.at(-1));const generated=await session.generate(c.prompt,{maxNewTokens:32});assert.deepEqual(generated.tokens,c.greedy_tokens);}
  }finally{session?.dispose();runtime.dispose();}
});

// A real pretrained checkpoint, which is NOT in this repository: third-party
// weights are not redistributed with ZIPP. This gate skips without one even in
// required mode, because no build step can produce it. Point
// ZIPP_GPT_NEO_MODEL at a Hugging Face GPT-Neo folder, or see
// tests/test_gpt_neo.py for how the default one is prepared.
const checkpoint=process.env.ZIPP_GPT_NEO_MODEL?pathToFileURL(process.env.ZIPP_GPT_NEO_MODEL+'/'):new URL('../models/tinystories-1m-src/',import.meta.url);
const hasCheckpoint=hasEngine&&await exists(new URL('config.json',checkpoint))&&await exists(new URL('oracle.json',checkpoint));
const oracleURL=new URL('oracle.json',checkpoint);
async function openEngine(){const zipp=await import(engineURL);await zipp.default({module_or_path:await readFile(wasmURL)});return zipp;}
test('checkout gate: a Hugging Face checkpoint folder loads with no conversion',{skip:!hasCheckpoint&&'No GPT-Neo checkpoint folder present (see tests/test_gpt_neo.py)'},async()=>{
  const zipp=await openEngine();const {createRuntime}=await import(runtimeURL);
  // A real model is deeper and heavier than a fixture; both policies must admit it.
  const runtime=await createRuntime({backend:'cpu-js',limits:{maxNodes:4096,maxWork:2_000_000_000}});
  let session=null,approved=null;
  try{
    const plugin=await new PluginRegistry().install(await sourceDirectory('../plugins/gpt-neo/'),{approve:()=>true});
    assert.ok(plugin.support.native,'the gpt-neo plugin declares native checkpoint support');
    // Refusing the digests must stop the load before any of the folder reaches
    // guest code. This is the whole security property of the unpinned path.
    let built=0;
    await assert.rejects(ModelSession.openNative({source:await lazyDirectory(checkpoint),plugin,
      engineFactory:()=>{built++;return new zipp.Engine();},runtime,approve:()=>false}),
      error=>error.code==='APPROVAL');
    assert.equal(built,0,'a refused checkpoint never reaches an engine');
    session=await ModelSession.openNative({source:await lazyDirectory(checkpoint),plugin,
      engineFactory:()=>new zipp.Engine(),runtime,
      approve:identity=>{approved=identity;return true;}});
    assert.ok(approved,'an unpinned folder must be approved against its digests');
    assert.equal(approved.checkpoint_format,'hf.gpt-neo-v1');
    assert.ok(approved.weights.length>=1&&approved.weights.every(w=>/^[a-f0-9]{64}$/.test(w.sha256)));
    assert.ok(approved.assets.some(a=>a.name==='vocab'),'the tokenizer comes from the checkpoint folder');
    const oracle=JSON.parse(await readFile(oracleURL,'utf8'));
    for(const c of oracle.cases.slice(0,2)){
      const result=await session.infer(c.tokens);
      compare(result.logits,c.logits,2e-4);
    }
    // The tokenizer, the graph and the sampler together, against transformers.
    // Generation takes the cached path: one prepared plan, weights uploaded
    // once, key and value caches carried on the device between tokens.
    const first=oracle.cases[0];
    const generated=await session.generate(first.prompt,{maxNewTokens:8,temperature:0});
    assert.deepEqual(generated.tokens,first.greedy_tokens.slice(0,generated.tokens.length));
    assert.equal(generated.cached,true,'a plugin offering a decode graph must not fall back to recomputing');
    assert.ok(generated.stats.uploadElements<4096,
      `a cached step uploaded ${generated.stats.uploadElements} elements; the point is that weights stay resident`);
    assert.ok(generated.stats.residentBytes>1e6,'the weights and caches are resident');
    // A second generation reuses the prepared plan. Every position is written
    // before it is unmasked, so no cache reset is needed between prompts.
    const second=await session.generate(oracle.cases[1].prompt,{maxNewTokens:6,temperature:0});
    assert.deepEqual(second.tokens,oracle.cases[1].greedy_tokens.slice(0,second.tokens.length));
    // And the eager path still agrees, which is what makes it the oracle.
    const eager=new Proxy(runtime,{get:(t,k)=>k==='prepare'?undefined:Reflect.get(t,k)});
    let plain=null;
    try{
      plain=await ModelSession.openNative({source:await lazyDirectory(checkpoint),plugin,
        engineFactory:()=>new zipp.Engine(),runtime:eager,approve:()=>true});
      const slow=await plain.generate(first.prompt,{maxNewTokens:8,temperature:0});
      assert.equal(slow.cached,undefined,'the eager path reports no cache');
      assert.deepEqual(slow.tokens,generated.tokens,'cached and recomputed generation must agree');
    }finally{plain?.dispose();}
  }finally{session?.dispose();runtime.dispose();}
});
test('checkout gate: the same folder loads again from a written pin',{skip:!hasCheckpoint&&'No GPT-Neo checkpoint folder present (see tests/test_gpt_neo.py)'},async()=>{
  if(!await exists(new URL('model.json',checkpoint)))return;
  const zipp=await openEngine();const {createRuntime}=await import(runtimeURL);
  const runtime=await createRuntime({backend:'cpu-js',limits:{maxNodes:4096,maxWork:2_000_000_000}});
  let session=null;
  try{
    const plugin=await new PluginRegistry().install(await sourceDirectory('../plugins/gpt-neo/'),{approve:()=>true});
    session=await ModelSession.open({source:await lazyDirectory(checkpoint),plugin,engineFactory:()=>new zipp.Engine(),runtime});
    assert.equal(session.info.checkpoint_format,'hf.gpt-neo-v1');
    assert.equal(session.info.unpinned,undefined,'a pinned load reports no unpinned identity');
    const oracle=JSON.parse(await readFile(oracleURL,'utf8'));
    compare((await session.infer(oracle.cases[0].tokens)).logits,oracle.cases[0].logits,2e-4);
  }finally{session?.dispose();runtime.dispose();}
});
