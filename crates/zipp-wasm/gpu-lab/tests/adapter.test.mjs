import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFile} from 'node:fs/promises';
import {createRuntime} from '../src/runtime.mjs';
import {createZippGPUAdapter,createZippGPUHandler} from '../src/zipp-adapter.mjs';
const program={version:1,nodes:[{id:0,op:'input',shape:[2],data:[3,7]}],outputs:[{name:'result',id:0}]};
const call=(id=1)=>({id,kind:'gpu.execute',args:[structuredClone(program)]});
function engine(){return {disposed:false,replies:[],resolveHostCallback(id,r){this.replies.push([id,r]);return true;}};}
test('GPU host capability is denied unless explicitly granted',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),h=createZippGPUHandler(rt);
  await assert.rejects(()=>h.handle('gpu.execute',[program]),e=>e.code==='DENIED');rt.dispose();
});
test('adapter serializes graph requests and completes exact callback IDs',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),e=engine(),a=createZippGPUAdapter(e,rt,{allowExecute:true});
  await Promise.all([a.dispatch(call(1)),a.dispatch(call(4294967297))]);
  assert.deepEqual(e.replies.map(r=>r[0]),[1,4294967297]);assert.deepEqual(e.replies[0][1].value.outputs.result.data,[3,7]);
  a.invalidate();await a.idle();rt.dispose();
});
test('unknown kinds, bad arity and duplicate IDs do not become GPU access',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),e=engine(),a=createZippGPUAdapter(e,rt,{allowExecute:true});
  await assert.rejects(()=>a.dispatch({...call(),kind:'file.read'}),x=>x.code==='DENIED');
  await a.dispatch({...call(),args:[]});assert.equal(e.replies[0][1].error.code,'PROTOCOL');
  await assert.rejects(()=>a.dispatch(call()),x=>x.code==='DUPLICATE');a.invalidate();rt.dispose();
});
test('tenant invalidation drops queued and in-flight completions',async()=>{
  let release;const gate=new Promise(r=>{release=r;});
  const rt={async execute(){await gate;return {value:1};}},e=engine(),a=createZippGPUAdapter(e,rt,{allowExecute:true});
  const one=a.dispatch(call(1)),two=a.dispatch(call(2));await Promise.resolve();a.invalidate();release();
  assert.equal((await one).cancelled,true);assert.equal((await two).cancelled,true);assert.deepEqual(e.replies,[]);
});
test('disposed Engine is never re-entered for a late result',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),e=engine(),a=createZippGPUAdapter(e,rt,{allowExecute:true});
  const run=a.dispatch(call());e.disposed=true;await run;assert.deepEqual(e.replies,[]);a.invalidate();rt.dispose();
});
test('queue and lifetime quotas reject excess work',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),e=engine(),a=createZippGPUAdapter(e,rt,{allowExecute:true,maxPending:1,maxRequests:1});
  const run=a.dispatch(call(1));await assert.rejects(()=>a.dispatch(call(2)),x=>x.code==='LIMIT');await run;
  await assert.rejects(()=>a.dispatch(call(3)),x=>x.code==='LIMIT');a.invalidate();rt.dispose();
});
test('guest callback and promise shim round-trip against documented queue shape (mock Engine, not ZIPP WASM)',async()=>{
  const rt=await createRuntime({backend:'cpu-js'});let next=0;const pending=new Map(),queue=[];
  const context=vm.createContext({host:{call(kind,args,cb){const id=++next;pending.set(id,cb);queue.push({id,kind,args});}}});
  vm.runInContext(await readFile(new URL('../src/zipp-guest.js',import.meta.url),'utf8'),context);
  const future=vm.runInContext(`gpuExecuteAsync(${JSON.stringify(program)})`,context);
  const e={disposed:false,resolveHostCallback(id,result){pending.get(id)?.(result);pending.delete(id);return true;}};
  const a=createZippGPUAdapter(e,rt,{allowExecute:true});await a.dispatch(queue.shift());
  assert.deepEqual((await future).outputs.result.data,[3,7]);assert.equal(pending.size,0);a.invalidate();rt.dispose();
});
