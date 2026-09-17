// Lifecycle tests with explicit doubles. These do NOT execute Python or GPU code.
import test from 'node:test';import assert from 'node:assert/strict';
import {FileMapSource,PluginRegistry,ModelSession} from '../src/index.mjs';
import {directoryEntries,sourceDirectory} from './helpers.mjs';
const tokensFor=text=>[0,...Array.from(text).map(ch=>ch==='a'?3:4)];
class EngineDouble {
  disposed=false;calls=[];renewals=0;
  setSyncHostCapabilities(c){assert.deepEqual(c,[]);}
  setInstructionBudget(n){assert.ok(n>0);}
  initPythonProject(files,entry,args){assert.ok(files[entry].includes('zipp_model_graph'));assert.deepEqual(args,[]);}
  renewInstructionBudget(){this.renewals++;}
  pythonCall(name,args){this.calls.push(name);
    if(name==='zipp_model_describe')return JSON.stringify({task:'causal-lm',checkpoint_format:'zipp.bigram-v1',tokenizer_formats:['character-v1'],vocab_size:7,max_context:32});
    if(name==='zipp_model_encode')return JSON.stringify(tokensFor(args[0]));
    if(name==='zipp_model_decode')return JSON.parse(args[0]).map(t=>t===3?'a':'b').join('');
    if(name==='zipp_model_graph')return JSON.stringify({version:1,graph:{version:2,nodes:[{id:0,op:'input',shape:[1,7]}],outputs:[{name:'logits',id:0}]},bindings:[{node:0,kind:'rows',tensor:'transition_logits',indices:[JSON.parse(args[1]).at(-1)]}]});
    throw Error('Unexpected hook');
  }
  dispose(){this.disposed=true;}
}
async function fixture({execute,entries,engine=new EngineDouble()}={}){
  const plugin=await new PluginRegistry().install(await sourceDirectory('../plugins/bigram/'),{approve:()=>true});
  const runtime = {
    info: () => ({backend: 'test-double'}),
    execute: execute || (async graph => ({
      backend: 'test-double',
      outputs: {logits: {shape: [1, 7], data: Float32Array.from([0, -1, -1, 5, 1, 0, 0])}}
    }))
  };
  const session=await ModelSession.open({source:entries?new FileMapSource(entries):await sourceDirectory('../examples/bigram/'),plugin,engineFactory:()=>engine,runtime});
  return {session,engine,runtime};
}
test('open calls Python bootstrap and reports exact identity',async()=>{const {session,engine}=await fixture();assert.equal(session.info.backend,'test-double');assert.equal(engine.calls[0],'zipp_model_describe');session.dispose();assert.ok(engine.disposed);});
test('inference binds binary data and returns owned last-token logits',async()=>{let input;const {session}=await fixture({execute:async g=>{input=g.nodes[0].data;return {outputs:{logits:{shape:[1,7],data:new Float32Array(7)}}};}});assert.equal((await session.infer([0])).logits.length,7);assert.ok(input instanceof Float32Array);session.dispose();});
test('greedy streaming returns complete generated prefix',async()=>{const prefixes=[];const {session}=await fixture();const result=await session.generate('',{maxNewTokens:3,onToken:r=>prefixes.push(r.text)});assert.deepEqual(prefixes,['a','aa','aaa']);assert.equal(result.text,'aaa');assert.equal(result.finishReason,'length');session.dispose();});
test('EOS stops without emitting the special token',async()=>{const {session}=await fixture({execute:async()=>({outputs:{logits:{shape:[1,7],data:Float32Array.from([0,5,0,0,0,0,0])}}})});const result=await session.generate('',{maxNewTokens:3});assert.equal(result.finishReason,'eos');assert.equal(result.text,'');session.dispose();});
test('zero new tokens never submits a compute graph',async()=>{const {session}=await fixture({execute:()=>{throw Error('must not execute');}});assert.equal((await session.generate('',{maxNewTokens:0})).text,'');session.dispose();});
test('pre-aborted generation fails before invoking the model',async()=>{const {session,engine}=await fixture();const abort=new AbortController();abort.abort();await assert.rejects(session.generate('',{signal:abort.signal}));assert.deepEqual(engine.calls,['zipp_model_describe']);session.dispose();});
test('overlapping work and mid-flight disposal are refused',async()=>{let release;const pending=new Promise(r=>release=r);const {session}=await fixture({execute:async()=>{await pending;return {outputs:{logits:{shape:[1,7],data:new Float32Array(7)}}};}});const first=session.infer([0]);await assert.rejects(session.infer([0]),/previous/);assert.throws(()=>session.dispose(),/Await/);release();await first;session.dispose();});
test('callback failure releases busy state',async()=>{const {session}=await fixture();await assert.rejects(session.generate('',{maxNewTokens:1,onToken:()=>{throw Error('consumer failed');}}),/consumer failed/);await session.infer([0]);session.dispose();});
test('bad output shape fails closed',async()=>{const {session}=await fixture({execute:async()=>({outputs:{logits:{shape:[7],data:new Float32Array(7)}}})});await assert.rejects(session.infer([0]),/shaped/);session.dispose();});
test('non-finite model output fails closed',async()=>{const {session}=await fixture({execute:async()=>({outputs:{logits:{shape:[1,7],data:Float32Array.from([NaN,0,0,0,0,0,0])}}})});await assert.rejects(session.infer([0]),/Non-finite/);session.dispose();});
test('out-of-range token IDs and context are rejected',async()=>{const {session}=await fixture();for(const ids of [[],[7],[-1],Array(33).fill(0)])await assert.rejects(session.infer(ids));session.dispose();});
test('dispose is idempotent; future inference refused',async()=>{const {session}=await fixture();session.dispose();session.dispose();await assert.rejects(session.infer([0]),/closed/);});
// plugin.json is trusted to refuse a model early; the installed source is what
// decides. A manifest advertising a checkpoint family its Python does not
// implement must fail at open, not run against weights it cannot read.
test('installed source that contradicts its manifest cannot open a session',async()=>{
  class Contradicting extends EngineDouble {
    pythonCall(name,args){
      if(name==='zipp_model_describe')return JSON.stringify({task:'causal-lm',checkpoint_format:'hf.gpt-neo-v1',tokenizer_formats:['gpt2-byte-bpe-v1'],vocab_size:7,max_context:32});
      return super.pythonCall(name,args);
    }
  }
  const engine=new Contradicting();
  await assert.rejects(fixture({engine}),/disagree about the supported checkpoint format/);
  assert.ok(engine.disposed,'a refused session must not leave an engine behind');
});
test('weight hash mismatch is caught before constructing an engine',async()=>{const entries=await directoryEntries('../examples/bigram/');const bytes=entries.get('weights.safetensors');bytes[bytes.length-1]^=1;let created=false;const plugin=await new PluginRegistry().install(await sourceDirectory('../plugins/bigram/'),{approve:()=>true});await assert.rejects(ModelSession.open({source:new FileMapSource(entries),plugin,engineFactory:()=>{created=true;return new EngineDouble();},runtime:{execute(){}}}),/digest mismatch/);assert.equal(created,false);});
