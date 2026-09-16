// Prepared sessions: a training loop through `runtime.prepare` / `session.run`
// must produce the same float32 bits as the same loop through chained
// `execute()` calls, on the host reference and on compiled WASM, whether the
// steps run one per `run` or all in one; plus the validation, lifetime and
// budget contracts a host relies on.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createRuntime, ComputeRuntime} from '../src/runtime.mjs';
import {validateProgram, DEFAULT_LIMITS, ComputeError} from '../src/graph.mjs';
import {CPUBackend} from '../src/backends/cpu.mjs';
import {mlpTrainingStep, mlpSessionProgram, seeded} from './ml-cases.mjs';
const wasmBytes=await readFile(new URL('../wasm/kernels.wasm',import.meta.url));
const code=c=>e=>e.code===c;
const same=(a,b,what)=>{assert.equal(a.length,b.length,`${what}: length`);for(let i=0;i<a.length;i++)if(a[i]!==b[i]||(a[i]===0&&1/a[i]!==1/b[i]))assert.fail(`${what}: element ${i} is ${a[i]} versus ${b[i]}`);};

// A small step (20-16-5, batch 8): the shared fixture at test size.
const sessionProgram=(options={})=>mlpSessionProgram({sizes:[20,16,5],batch:8,...options});
function batches(sizes,batch,steps,seed=11){
  const rnd=seeded(seed);
  return Array.from({length:steps},()=>({x:Float32Array.from({length:batch*sizes[0]},()=>rnd(0,1)),
    y:Float32Array.from({length:batch},()=>Math.floor(rnd(0,sizes[sizes.length-1])))}));
}
/** `steps` chained executes fed by each step's outputs: the reference a session must match bit for bit. */
async function chained(runtime,{sizes,batch,seed,lr,parameters,state},data){
  let params=null,st=state;const losses=[];
  for(let step=1;step<=data.length;step++){
    const {program}=mlpTrainingStep({sizes,batch,seed,lr,params,state:st,step,x:Array.from(data[step-1].x),targets:Array.from(data[step-1].y)});
    const out=(await runtime.execute(program,{typedOutputs:true})).outputs;
    losses.push(out.loss.data[0]);
    params=Array.from({length:parameters},(_,i)=>out[`p${i}`].data);st=params.map((_,i)=>[out[`m${i}`].data,out[`v${i}`].data]);
  }
  return {losses,params,state:st};
}
const resident=parameters=>Array.from({length:parameters},(_,i)=>[`p${i}`,`m${i}`,`v${i}`]).flat();

for(const backend of ['cpu-js','wasm'])test(`${backend}: five Adam steps through a session equal five chained executes bit for bit`,async()=>{
  const rt=await createRuntime({backend,wasmBytes}),spec=sessionProgram(),data=batches(spec.sizes,spec.batch,5);
  const expected=await chained(rt,spec,data);
  // One step per run.
  const one=await rt.prepare(spec.program,{resident:resident(spec.parameters)});
  const losses=[];
  for(const {x,y} of data){const r=await one.run({inputs:{0:x,1:y}},{readback:['loss']});losses.push(r.outputs.loss.data[0]);assert.equal(r.stats.readbackElements,1);}
  same(losses,expected.losses,'losses, one step per run');
  const got=(await one.download(resident(spec.parameters))).outputs;
  for(let i=0;i<spec.parameters;i++){same(got[`p${i}`].data,expected.params[i],`p${i}`);same(got[`m${i}`].data,expected.state[i][0],`m${i}`);same(got[`v${i}`].data,expected.state[i][1],`v${i}`);}
  // All five steps in one run, one readback at the end.
  const many=await rt.prepare(spec.program,{resident:resident(spec.parameters)});
  const r=await many.run(data.map(({x,y})=>({inputs:{0:x,1:y}})),{readback:['loss']});
  assert.equal(r.steps.length,5);same(r.steps.map(s=>s.outputs.loss.data[0]),expected.losses,'losses, five steps per run');
  assert.deepEqual(r.steps.map(s=>s.step),[1,2,3,4,5]);assert.equal(r.stats.readbackElements,5);
  const all=(await many.download(resident(spec.parameters))).outputs;
  for(let i=0;i<spec.parameters;i++)same(all[`p${i}`].data,expected.params[i],`p${i} after one five-step run`);
  assert.equal(rt.sessions.size,2);one.dispose();many.dispose();assert.equal(rt.sessions.size,0);rt.dispose();
});
test('a session keeps counting steps across runs and accepts an explicit step number',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),spec=sessionProgram(),data=batches(spec.sizes,spec.batch,4);
  const expected=await chained(rt,spec,data);
  const s=await rt.prepare(spec.program,{resident:resident(spec.parameters)});
  const a=await s.run([{inputs:{0:data[0].x,1:data[0].y}},{inputs:{0:data[1].x,1:data[1].y}}],{readback:['loss']});
  const b=await s.run([{inputs:{0:data[2].x,1:data[2].y}},{inputs:{0:data[3].x,1:data[3].y}}],{readback:['loss']});
  same([...a.steps,...b.steps].map(x=>x.outputs.loss.data[0]),expected.losses,'losses over two runs');
  assert.equal(s.stepNumber,5);
  // Restarting at step 1 with the same batch: Adam's bias correction differs from step 5's.
  const restart=await s.run({inputs:{0:data[0].x,1:data[0].y}},{readback:['loss'],step:1});
  assert.equal(restart.steps[0].step,1);assert.equal(s.stepNumber,2);
  await assert.rejects(()=>s.run({inputs:{0:data[0].x,1:data[0].y}},{step:0}),code('NUMBER'));
  rt.dispose();
});
test('resident outputs are downloadable while the default readback excludes them',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),spec=sessionProgram();
  const s=await rt.prepare(spec.program,{resident:['p0','m0']});
  const [{x,y}]=batches(spec.sizes,spec.batch,1);
  const r=await s.run({inputs:{0:x,1:y}});
  assert.ok(!('p0' in r.outputs)&&!('m0' in r.outputs)&&'loss' in r.outputs&&'p1' in r.outputs);
  assert.ok(r.outputs.loss.data instanceof Float32Array,'sessions return typed outputs');
  const d=(await s.download(['p0'])).outputs;assert.deepEqual(d.p0.shape,[20,16]);assert.equal(d.p0.data.length,320);
  await assert.rejects(()=>s.download(['p1']),code('REFERENCE'));
  await assert.rejects(()=>s.download(['nope']),code('REFERENCE'));
  await assert.rejects(()=>s.run({inputs:{0:x,1:y}},{readback:['nope']}),code('REFERENCE'));
  const fresh=await rt.prepare(spec.program,{resident:['p0']});
  await assert.rejects(()=>fresh.download(['p0']),code('REFERENCE'));
  rt.dispose();
});
test('a fed input without a value, a wrong length and out-of-range class targets are refused before any work',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),spec=sessionProgram(),s=await rt.prepare(spec.program);
  const [{x,y}]=batches(spec.sizes,spec.batch,1);
  await assert.rejects(()=>s.run({inputs:{0:x}}),code('REFERENCE'));
  await assert.rejects(()=>s.run({inputs:{0:x.subarray(1),1:y}}),code('SHAPE'));
  await assert.rejects(()=>s.run({inputs:{0:x,1:Float32Array.from(y).fill(5)}}),code('NUMBER'));
  await assert.rejects(()=>s.run({inputs:{0:x,1:y,2:x}}),code('SHAPE'));
  await assert.rejects(()=>s.run({inputs:{0:x,1:y,99:x}}),code('REFERENCE'));
  await assert.rejects(()=>s.run({inputs:{0:x,1:y},extra:1}),code('PROTOCOL'));
  await assert.rejects(()=>s.run([]),code('LIMIT'));
  assert.equal(s.busy,false);assert.equal(rt.busy,false);
  rt.dispose();
});
test('carry validation: shape, unknown output, computed source; execute() stays strict',async()=>{
  const rt=await createRuntime({backend:'cpu-js'});
  const base=[{id:0,op:'input',shape:[2],data:[1,2]},{id:1,op:'input',shape:[2],data:[3,4]},{id:2,op:'add',a:0,b:1}];
  const prog=(nodes,outputs)=>({version:2,nodes,outputs});
  const ok=await rt.prepare(prog([{...base[0],carry:'r'},base[1],base[2]],[{name:'r',id:2}]));ok.dispose();
  await assert.rejects(()=>rt.prepare(prog([{...base[0],carry:'zz'},base[1],base[2]],[{name:'r',id:2}])),code('REFERENCE'));
  await assert.rejects(()=>rt.prepare(prog([{...base[0],carry:'r'},base[1],{id:2,op:'sum',a:0}],[{name:'r',id:2}])),code('SHAPE'));
  await assert.rejects(()=>rt.prepare(prog([{...base[0],carry:'q'},base[1],base[2]],[{name:'r',id:2},{name:'q',id:1}])),code('REFERENCE'));
  await assert.rejects(()=>rt.prepare(prog([{...base[0],carry:5},base[1],base[2]],[{name:'r',id:2}])),code('PROTOCOL'));
  // Plain execute: no carry, no data-less input.
  await assert.rejects(()=>rt.execute(prog([{...base[0],carry:'r'},base[1],base[2]],[{name:'r',id:2}])),code('PROTOCOL'));
  await assert.rejects(()=>rt.execute(prog([{id:0,op:'input',shape:[2]},base[1],base[2]],[{name:'r',id:2}])),code('PROTOCOL'));
  assert.throws(()=>validateProgram(prog([{id:0,op:'input',shape:[2]},base[1],base[2]],[{name:'r',id:2}])),code('PROTOCOL'));
  rt.dispose();
});
test('a carried value flows without upload: a counter program advances one per step',async()=>{
  for(const backend of ['cpu-js','wasm']){
    const rt=await createRuntime({backend,wasmBytes});
    const s=await rt.prepare({version:2,nodes:[{id:0,op:'input',shape:[3],data:[0,10,20],carry:'next'},{id:1,op:'full',shape:[],value:1},{id:2,op:'add',a:0,b:1}],
      outputs:[{name:'next',id:2}]},{resident:['next']});
    const r=await s.run([{},{},{}],{readback:['next']});
    assert.deepEqual(r.steps.map(x=>Array.from(x.outputs.next.data)),[[1,11,21],[2,12,22],[3,13,23]]);
    assert.equal(r.stats.uploadElements,0);
    const again=await s.run({},{readback:['next']});assert.deepEqual(Array.from(again.outputs.next.data),[4,14,24]);
    assert.deepEqual(Array.from((await s.download(['next'])).outputs.next.data),[4,14,24]);
    // Feeding a carried input resets it for that step; the carry continues from the result.
    const reset=await s.run({inputs:{0:[100,200,300]}},{readback:['next']});assert.deepEqual(Array.from(reset.outputs.next.data),[101,201,301]);
    assert.deepEqual(Array.from((await s.run({},{readback:['next']})).outputs.next.data),[102,202,302]);
    assert.deepEqual(Object.keys((await s.run({})).outputs),[],'a resident output is not in the default readback');
    rt.dispose();
  }
});
test('a carried input with no initial value is fed once and then carried through a multi-step run',async()=>{
  for(const backend of ['cpu-js','wasm']){
    const rt=await createRuntime({backend,wasmBytes});
    const program={version:2,nodes:[{id:0,op:'input',shape:[3],carry:'next'},{id:1,op:'full',shape:[],value:1},{id:2,op:'add',a:0,b:1}],outputs:[{name:'next',id:2}]};
    const many=await rt.prepare(program);
    await assert.rejects(()=>many.run([{},{}]),code('REFERENCE'),'nothing has given input 0 a value');
    await assert.rejects(()=>many.run([{},{inputs:{0:[1,2,3]}}]),code('REFERENCE'),'the first step still lacks it');
    const r=await many.run([{inputs:{0:[1,2,3]}},{},{}]);
    assert.deepEqual(r.steps.map(s=>Array.from(s.outputs.next.data)),[[2,3,4],[3,4,5],[4,5,6]]);
    const one=await rt.prepare(program),got=[];
    for(const step of [{inputs:{0:[1,2,3]}},{},{}])got.push(Array.from((await one.run(step)).outputs.next.data));
    assert.deepEqual(got,r.steps.map(s=>Array.from(s.outputs.next.data)));
    rt.dispose();
  }
});
test('session budgets: count, resident bytes against maxLogicalBytes, steps per run and readback elements',async()=>{
  const rt=await createRuntime({backend:'cpu-js',limits:{maxSessions:2,maxStepsPerRun:3,maxOutputElements:5}});
  const program={version:2,nodes:[{id:0,op:'input',shape:[2],data:[1,2]},{id:1,op:'relu',a:0}],outputs:[{name:'r',id:1}]};
  const a=await rt.prepare(program),b=await rt.prepare(program);
  await assert.rejects(()=>rt.prepare(program),code('LIMIT'));
  b.dispose();const c=await rt.prepare(program);
  await assert.rejects(()=>c.run([{},{},{},{}]),code('LIMIT'));
  await assert.rejects(()=>c.run([{},{},{}]),code('LIMIT')); // 3 steps x 2 elements > 5
  assert.equal((await c.run([{},{}])).steps.length,2);
  a.dispose();c.dispose();rt.dispose();
  const tight=await createRuntime({backend:'cpu-js',limits:{maxLogicalBytes:64}});
  const big={version:2,nodes:[{id:0,op:'input',shape:[4],data:[1,2,3,4]},{id:1,op:'relu',a:0}],outputs:[{name:'r',id:1}]};
  const first=await tight.prepare(big,{resident:['r']}); // 16 static + 16 resident + 32 plan = 64
  assert.equal(first.residentBytes,32);assert.equal(tight.residentBytes(),32);
  await assert.rejects(()=>tight.prepare(big),code('LIMIT'));
  first.dispose();await (await tight.prepare(big)).run({});tight.dispose();
});
test('lifetime: disposal during a run is deferred, a disposed runtime disposes its sessions, busy is exclusive',async()=>{
  const rt=await createRuntime({backend:'cpu-js'});
  const program={version:2,nodes:[{id:0,op:'input',shape:[2],data:[1,2],carry:'r'},{id:1,op:'relu',a:0}],outputs:[{name:'r',id:1}]};
  const s=await rt.prepare(program,{resident:['r']});
  const running=s.run({});s.dispose();
  assert.equal(s.disposed,false);await running;assert.equal(s.disposed,true);assert.equal(rt.sessions.size,0);
  await assert.rejects(()=>s.run({}),code('DISPOSED'));
  const t=await rt.prepare(program);
  const inflight=t.run({});
  assert.throws(()=>rt.dispose(),code('BUSY'));
  const plain=rt.execute({...program,nodes:[{id:0,op:'input',shape:[2],data:[1,2]},{id:1,op:'relu',a:0}]}),second=t.run({});
  await assert.rejects(()=>plain,code('BUSY'));await assert.rejects(()=>second,code('BUSY'));
  await inflight;
  rt.dispose();assert.equal(t.disposed,true);
  await assert.rejects(()=>t.run({}),code('DISPOSED'));
});
test('describe() tells a host what the plan feeds, carries and keeps',async()=>{
  const rt=await createRuntime({backend:'cpu-js'}),spec=sessionProgram(),s=await rt.prepare(spec.program,{resident:['p0']});
  const d=s.describe();
  assert.equal(d.backend,'cpu-js');assert.equal(d.step,1);
  assert.deepEqual(d.inputs[0],{id:0,shape:[8,20],fed:true});assert.deepEqual(d.inputs[1],{id:1,shape:[8],fed:true,classes:5});
  assert.deepEqual(d.inputs[2],{id:2,shape:[20,16],fed:false,carry:'p0'});
  assert.ok(d.outputs.find(o=>o.name==='p0').resident&&!d.outputs.find(o=>o.name==='loss').resident);
  assert.equal(DEFAULT_LIMITS.maxSessions,16);
  rt.dispose();
});

// ---- failure semantics ---------------------------------------------------------------------
// Before `begin` a run only validates; after it, carries and residents advance step by step.
// A failure past that point leaves them at no step in particular while stepNumber stays, so
// the session must refuse everything but dispose() rather than let a retry run the wrong step.
test('a failure after device work began poisons the session; a validation failure does not',async()=>{
  const program={version:2,nodes:[{id:0,op:'input',shape:[2],data:[1,1],carry:'next'},{id:1,op:'input',shape:[2]},{id:2,op:'add',a:0,b:1}],outputs:[{name:'next',id:2}]};
  class Fail extends CPUBackend{constructor(){super();this.adds=0;}async run(n,r){if(n.op==='add'&&++this.adds===3)throw new ComputeError('BACKEND','device lost');return super.run(n,r);}}
  const backend=new Fail(),rt=new ComputeRuntime(backend),s=await rt.prepare(program);
  await assert.rejects(()=>s.run({inputs:{1:[1,1]}},{readback:['nope']}),code('REFERENCE'));
  await assert.rejects(()=>s.run({inputs:{1:[1]}}),code('SHAPE'));
  assert.equal(s.poisoned,false);assert.equal(s.describe().poisoned,false);
  // The third add is step 3 of a three-step run: steps 1 and 2 already advanced the carry.
  await assert.rejects(()=>s.run([{inputs:{1:[1,1]}},{inputs:{1:[1,1]}},{inputs:{1:[1,1]}}]),code('BACKEND'));
  assert.equal(s.poisoned,true);assert.equal(s.describe().poisoned,true);assert.equal(s.stepNumber,1);
  assert.equal(s.busy,false);assert.equal(rt.busy,false);
  await assert.rejects(()=>s.run({inputs:{1:[1,1]}}),code('STATE'));
  await assert.rejects(()=>s.download(['next']),code('STATE'));
  s.dispose();assert.equal(s.disposed,true);assert.equal(rt.sessions.size,0);
  // The runtime is unharmed: a fresh session carries as it should.
  backend.adds=-1000;
  const fresh=await rt.prepare(program),r=await fresh.run([{inputs:{1:[1,1]}},{inputs:{1:[1,1]}}]);
  assert.deepEqual(r.steps.map(x=>Array.from(x.outputs.next.data)),[[2,2],[3,3]]);assert.equal(fresh.poisoned,false);
  fresh.dispose();rt.dispose();
});
test('a non-finite readback in a later step poisons the session too',async()=>{
  // next = a - x carries; log(next) is read back. Step 1 is fine, step 2 makes next negative.
  const program={version:2,nodes:[{id:0,op:'input',shape:[2],data:[1,1],carry:'next'},{id:1,op:'input',shape:[2]},{id:2,op:'sub',a:0,b:1},{id:3,op:'log',a:2}],outputs:[{name:'next',id:2},{name:'lg',id:3}]};
  for(const backend of ['cpu-js','wasm']){
    const rt=await createRuntime({backend,wasmBytes}),s=await rt.prepare(program,{resident:['next']});
    await assert.rejects(()=>s.run([{inputs:{1:[0,0]}},{inputs:{1:[2,2]}}],{readback:['lg']}),code('NUMBER'));
    assert.equal(s.poisoned,true,backend);
    await assert.rejects(()=>s.run({inputs:{1:[0,0]}},{readback:['lg']}),code('STATE'));
    await assert.rejects(()=>s.download(['next']),code('STATE'));
    s.dispose();rt.dispose();
  }
});
