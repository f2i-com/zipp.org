// FULL-CHECKOUT GATES. A skip is not a pass. Set ZIPP_REQUIRE_INTEGRATION=1
// before release to make missing upstream/runtime artifacts a hard failure.
import test from 'node:test';import assert from 'node:assert/strict';
import {readFile,access} from 'node:fs/promises';
import {PluginRegistry,ModelSession,openSafetensors,WeightStore,bindGraph,resolveLimits} from '../src/index.mjs';
import {sourceDirectory} from './helpers.mjs';
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
