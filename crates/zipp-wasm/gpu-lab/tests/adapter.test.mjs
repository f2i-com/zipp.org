import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFile} from 'node:fs/promises';
import {createRuntime, ComputeRuntime} from '../src/runtime.mjs';
import {CPUBackend} from '../src/backends/cpu.mjs';
import {ComputeError} from '../src/graph.mjs';
import {createZippGPUAdapter,createZippGPUHandler} from '../src/zipp-adapter.mjs';
import {createPythonGPUAdapter} from '../src/zipp-python-adapter.mjs';
import {existsSync} from 'node:fs';
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

// R244: an over-quota rejection used to be delivered synchronously inside
// drain(); a callback that resubmits then recursed drain -> deliver ->
// onDelivered -> drain until the stack overflowed with the Engine borrowed.
// This mock throws on re-entry the way wasm-bindgen's borrow flag does.
function reentrantEngine(){
  const e={disposed:false,queue:[],next:0,depth:0,maxDepth:0,results:0,rejections:0,retries:0,maxRetries:Infinity,
    submit(){this.queue.push({id:++this.next,kind:'gpu.execute',payload:structuredClone(program)});},
    takeHostRequests(){if(this.depth)throw Error('recursive use of an object');const q=this.queue;this.queue=[];return q;},
    pythonCall(name,[id,reply]){
      if(this.depth)throw Error('recursive use of an object');
      this.depth++;this.maxDepth=Math.max(this.maxDepth,this.depth);
      try{if(reply.ok)this.results++;else{this.rejections++;if(this.retries++<this.maxRetries)this.submit();}return true;}
      finally{this.depth--;}
    }};
  return e;
}
test('Python adapter: a callback that resubmits after LIMIT does not recurse into drain',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),e=reentrantEngine();let a,errors=0;
  a=createPythonGPUAdapter(e,rt,{allowExecute:true,maxPending:16,onDelivered:({error})=>{if(error)errors++;a.drain();}});
  for(let i=0;i<17;i++)e.submit();
  a.drain();assert.equal(e.maxDepth,0,'no delivery happens inside drain()');
  for(let i=0;i<50&&(e.results<17||a.pending);i++)await a.idle();
  assert.equal(e.results,17);assert.equal(e.rejections,1);assert.equal(e.maxDepth,1);assert.equal(errors,0);
  a.invalidate();rt.dispose();
});
test('Python adapter: an endless retry loop against the lifetime quota stays asynchronous',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),e=reentrantEngine();let a;e.maxRetries=40;
  a=createPythonGPUAdapter(e,rt,{allowExecute:true,maxRequests:2,onDelivered:()=>a.drain()});
  for(let i=0;i<3;i++)e.submit();
  a.drain();
  for(let i=0;i<1000&&e.rejections<=e.maxRetries;i++)await a.idle();
  assert.equal(e.results,2);assert.equal(e.rejections,41);assert.equal(e.maxDepth,1);
  a.invalidate();rt.dispose();
});
const playgroundRuntime=new URL('../../../../landing/public/playground-runtime/',import.meta.url);
test('Python adapter: the real Engine stays usable and disposable after 17 submits with a retrying on_error',
  {skip:!existsSync(new URL('zipp_wasm_bg.wasm',playgroundRuntime))&&'checked-in playground runtime not present'},async()=>{
  const wasm=await import(new URL('zipp_wasm.js',playgroundRuntime));
  await wasm.default({module_or_path:await readFile(new URL('zipp_wasm_bg.wasm',playgroundRuntime))});
  const rt=await createRuntime({backend:'cpu-js'}),engine=new wasm.Engine();engine.setInstructionBudget(2e9);
  let adapter,errors=0;
  adapter=createPythonGPUAdapter(engine,rt,{allowExecute:true,maxPending:16,maxRequests:100000,
    onDelivered:({error})=>{if(error)errors++;adapter.drain();}});
  engine.initPythonProject({'main.py':['from zipp_gpu import Graph','done = []','failures = []',
    'def ok(r):','    done.append(r["outputs"]["result"]["data"])',
    'def failed(e):','    failures.append(e.code)','    submit()',
    'def submit():','    g = Graph()','    g.submit(ok, failed, result=g.tensor([1.0, 2.0]))',
    'for i in range(17):','    submit()',
    'def report():','    print(len(done), failures)',''].join('\n')},'main.py',[]);
  adapter.drain();
  for(let i=0;i<50&&adapter.pending;i++)await adapter.idle();
  await adapter.idle();
  assert.equal(errors,0);
  engine.pythonCall('report',[]);
  assert.deepEqual(engine.takeConsole().map(x=>x.text),['17 [\'LIMIT\']']);
  assert.equal(engine.disposed,false);engine.dispose();assert.equal(engine.disposed,true);
  adapter.invalidate();rt.dispose();
});

// ---- sessions through the host boundary --------------------------------------------------
import {GPU_KINDS} from '../src/zipp-adapter.mjs';
const counter={version:2,nodes:[{id:0,op:'input',shape:[2],data:[1,2],carry:'next'},{id:1,op:'input',shape:[2]},{id:2,op:'add',a:0,b:1}],outputs:[{name:'next',id:2}]};
test('session kinds: create returns an opaque id, run feeds inputs and carries, download reads residents, dispose frees',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),h=createZippGPUHandler(rt,{allowExecute:true});
  const created=await h.handle('gpu.session.create',[{program:counter,resident:['next']}]);
  assert.match(created.session,/^s[0-9a-f]{32}$/);assert.equal(created.backend,'cpu-js');assert.deepEqual(created.inputs[1],{id:1,shape:[2],fed:true});
  assert.equal(h.sessions,1);assert.equal(rt.sessions.size,1);
  const run=await h.handle('gpu.session.run',[{session:created.session,steps:[{inputs:{1:new Float32Array([10,20])}},{inputs:{'1':[1,1]}}],readback:['next']}]);
  assert.deepEqual(run.steps.map(s=>Array.from(s.outputs.next.data)),[[11,22],[12,23]]);assert.equal(run.step,3);
  assert.deepEqual(Array.from((await h.handle('gpu.session.download',[{session:created.session,names:['next']}])).outputs.next.data),[12,23]);
  await assert.rejects(()=>h.handle('gpu.session.run',[{session:created.session,steps:[{inputs:{1:[1,1]}}],extra:1}]),e=>e.code==='PROTOCOL');
  await assert.rejects(()=>h.handle('gpu.session.run',[{session:created.session}]),e=>e.code==='PROTOCOL');
  assert.deepEqual(await h.handle('gpu.session.dispose',[{session:created.session}]),{version:1,disposed:true});
  assert.equal(h.sessions,0);assert.equal(rt.sessions.size,0);
  await assert.rejects(()=>h.handle('gpu.session.run',[{session:created.session,steps:[{}]}]),e=>e.code==='REFERENCE');
  h.invalidate();rt.dispose();
});
test('a guest can only name sessions its own handler minted; invalidation disposes them; the count is bounded',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),mine=createZippGPUHandler(rt,{allowExecute:true,maxSessions:2}),theirs=createZippGPUHandler(rt,{allowExecute:true});
  const a=await mine.handle('gpu.session.create',[{program:counter}]),b=await theirs.handle('gpu.session.create',[{program:counter}]);
  await assert.rejects(()=>mine.handle('gpu.session.run',[{session:b.session,steps:[{inputs:{1:[1,1]}}]}]),e=>e.code==='REFERENCE');
  await assert.rejects(()=>mine.handle('gpu.session.dispose',[{session:b.session}]),e=>e.code==='REFERENCE');
  for(const id of [1,null,{},'s'+'0'.repeat(32)])await assert.rejects(()=>mine.handle('gpu.session.run',[{session:id,steps:[{}]}]),e=>e.code==='REFERENCE');
  await mine.handle('gpu.session.create',[{program:counter}]);
  await assert.rejects(()=>mine.handle('gpu.session.create',[{program:counter}]),e=>e.code==='LIMIT');
  await assert.rejects(()=>theirs.handle('gpu.session.create',[{program:counter,backend:'webgpu'}]),e=>e.code==='BACKEND');
  await assert.rejects(()=>mine.handle('gpu.session.create',[{program:counter,nodes:[]}]),e=>e.code==='PROTOCOL');
  assert.equal(rt.sessions.size,3);
  mine.invalidate();assert.equal(rt.sessions.size,1);assert.equal(mine.sessions,0);
  await assert.rejects(()=>theirs.handle('gpu.session.run',[{session:a.session,steps:[{}]}]),e=>e.code==='REFERENCE');
  theirs.invalidate();assert.equal(rt.sessions.size,0);rt.dispose();
});
test('the JavaScript adapter accepts every GPU kind and delivers session replies to exact callback IDs',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),e=engine(),a=createZippGPUAdapter(e,rt,{allowExecute:true});
  assert.ok(GPU_KINDS.every(k=>a.accepts(k))&&!a.accepts('file.read'));
  await a.dispatch({id:1,kind:'gpu.session.create',args:[{program:counter,resident:['next']}]});
  const id=e.replies[0][1].value.session;assert.equal(typeof id,'string');
  await a.dispatch({id:2,kind:'gpu.session.run',args:[{session:id,steps:[{inputs:{1:new Float32Array([5,5])}}],readback:['next']}]});
  assert.deepEqual(Array.from(e.replies[1][1].value.outputs.next.data),[6,7]);
  await a.dispatch({id:3,kind:'gpu.session.download',args:[{session:id,names:['next']}]});
  assert.deepEqual(Array.from(e.replies[2][1].value.outputs.next.data),[6,7]);
  await a.dispatch({id:4,kind:'gpu.session.run',args:[{session:'nope',steps:[{}]}]});
  assert.equal(e.replies[3][1].error.code,'REFERENCE');
  assert.equal(a.sessions,1);a.invalidate();assert.equal(a.sessions,0);await a.idle();rt.dispose();
});
test('the Python adapter admits session requests from the drained queue and typed outputs come back',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),events=[];
  const e={disposed:false,queue:[],next:0,takeHostRequests(){const q=this.queue;this.queue=[];return q;},pythonCall(name,[id,reply]){events.push([id,reply]);return true;}};
  const a=createPythonGPUAdapter(e,rt,{allowExecute:true,maxSessions:1});
  e.queue.push({id:1,kind:'gpu.session.create',payload:{program:counter,resident:['next']}},{id:2,kind:'file.read',payload:{}});
  assert.deepEqual(a.drain().map(r=>r.kind),['file.read']);await a.idle();
  const id=events[0][1].value.session;
  e.queue.push({id:3,kind:'gpu.session.run',payload:{session:id,steps:[{inputs:{'1':[1,2]}},{inputs:{'1':[1,2]}}],readback:['next']}},
    {id:4,kind:'gpu.session.create',payload:{program:counter}},{id:5,kind:'gpu.session.dispose',payload:{session:id}});
  a.drain();await a.idle();
  assert.equal(events[1][1].value.steps.length,2);assert.ok(events[1][1].value.outputs.next.data instanceof Float32Array);
  assert.equal(events[2][1].error.code,'LIMIT');assert.equal(events[3][1].value.disposed,true);
  a.invalidate();rt.dispose();
});

test('session kinds: a run that fails after device work began marks the error poisoned and only dispose remains',async()=>{
  class Fail extends CPUBackend{constructor(){super();this.adds=0;}async run(n,r){if(n.op==='add'&&++this.adds===2)throw new ComputeError('BACKEND','device lost');return super.run(n,r);}}
  const rt=new ComputeRuntime(new Fail()),h=createZippGPUHandler(rt,{allowExecute:true});
  const created=await h.handle('gpu.session.create',[{program:counter}]);
  // Validation failures carry no flag and leave the session usable.
  await assert.rejects(()=>h.handle('gpu.session.run',[{session:created.session,steps:[{inputs:{1:[1]}}]}]),e=>e.code==='SHAPE'&&e.poisoned===undefined);
  await assert.rejects(()=>h.handle('gpu.session.run',[{session:created.session,steps:[{inputs:{1:[1,1]}},{inputs:{1:[1,1]}}]}]),e=>e.code==='BACKEND'&&e.poisoned===true);
  await assert.rejects(()=>h.handle('gpu.session.run',[{session:created.session,steps:[{inputs:{1:[1,1]}}]}]),e=>e.code==='STATE'&&e.poisoned===true);
  await assert.rejects(()=>h.handle('gpu.session.download',[{session:created.session,names:['next']}]),e=>e.code==='STATE');
  assert.deepEqual(await h.handle('gpu.session.dispose',[{session:created.session}]),{version:1,disposed:true});
  assert.equal(h.sessions,0);rt.dispose();
});
