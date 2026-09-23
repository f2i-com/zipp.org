import {createRuntime} from '../src/runtime.mjs';
import {opCases, mlpTrainingStep, mlpSessionProgram, dropoutSessionProgram, embeddingSessionProgram, seeded} from './ml-cases.mjs';
import {decodeQ4K, Q4_K_BLOCK, Q4_K_BYTES} from '../src/quant.mjs';
const input=(id,data,shape=[data.length])=>({id,op:'input',shape,data});
const program=nodes=>({version:1,nodes,outputs:[{name:'result',id:nodes.length-1}]});
function cases(){
  const result=[];
  for(const length of [1,3,63,64,65,129,1025]) {
    const a=Array.from({length},(_,i)=>(i%19-9)/3),b=Array.from({length},(_,i)=>(i%7-3)/5);
    result.push([`multiply/add/relu/sum length ${length}`,program([input(0,a),input(1,b),{id:2,op:'mul',a:0,b:1},{id:3,op:'add',a:2,b:0},{id:4,op:'relu',a:3},{id:5,op:'sum',a:4}])]);
  }
  result.push(['rectangular transpose',program([input(0,[1,2,3,4,5,6],[2,3]),{id:1,op:'transpose',a:0}])]);
  result.push(['ReLU gradient including zero',program([input(0,[-2,-0.0,0,1,3]),{id:1,op:'positive',a:0}])]);
  result.push(['scalar-left subtraction',program([input(0,[10],[]),input(1,[1,2,3,4,5]),{id:2,op:'sub',a:0,b:1}])]);
  result.push(['constant fill',program([{id:0,op:'full',shape:[7,9],value:0.125}])]);
  for(const [m,k,n] of [[1,1,1],[2,3,7],[17,13,9]])result.push([`matmul ${m}x${k}x${n}`,program([
    input(0,Array.from({length:m*k},(_,i)=>(i%11-5)*0.125),[m,k]),
    input(1,Array.from({length:k*n},(_,i)=>(i%5-2)*0.25),[k,n]),{id:2,op:'matmul',a:0,b:1}])]);
  for(const [h,w] of [[1,1],[3,5],[32,32]]){
    const data=Array.from({length:h*w},(_,i)=>(i*17%23)<7?1:0),nodes=[input(0,data,[h,w])];
    for(let id=1;id<=8;id++)nodes.push({id,op:'life',a:id-1});
    result.push([`Life torus ${h}x${w}, eight steps`,program(nodes)]);
  }
  // The two matmul shapes a checkpoint needs: a weight stored [N, K], and one
  // that is still Q4_K blocks on the device. Every backend decodes inside its
  // own matmul, so these exercise four separate decoders against the reference.
  for(const [m,k,n] of [[1,256,4],[2,512,7],[3,256,16]]){
    let state=(m*131+k+n)>>>0;const next=()=>(state=(Math.imul(state,1664525)+1013904223)>>>0);
    const w=new Uint8Array((k/Q4_K_BLOCK)*n*Q4_K_BYTES);
    for(let i=0;i<w.length/Q4_K_BYTES;i++){const at=i*Q4_K_BYTES;
      w[at]=next()&0xff;w[at+1]=0x20|(next()&7);w[at+2]=next()&0xff;w[at+3]=0x18|(next()&7);
      for(let j=4;j<Q4_K_BYTES;j++)w[at+j]=next()&0xff;}
    const values=new Float32Array(k*n);decodeQ4K(w,0,values.length,values);
    const a=Array.from({length:m*k},(_,i)=>((i*37)%19)/16-0.5);
    const v2=b=>({version:2,nodes:[{id:0,op:'input',shape:[m,k],data:a},b,{id:2,op:'matmul',a:0,b:1,transposed:true}],
      outputs:[{name:'result',id:2}]});
    result.push([`transposed matmul ${m}x${k}x${n}`,v2({id:1,op:'input',shape:[n,k],data:values})]);
    result.push([`Q4_K matmul ${m}x${k}x${n}`,v2({id:1,op:'input',shape:[n,k],dtype:'q4_k',data:w})]);
  }
  return [...result,...opCases()];
}
/** Largest |actual - expected| over every named output; throws past a float32 GPU tolerance.
 * `exact`: the same float32 bits, a zero's sign included (integer-exact and selecting operations). */
function compare(actual,expected,tolerance=2e-4,exact=false){
  let worst=0;
  for(const name of Object.keys(expected.outputs)){
    const a=actual.outputs[name]?.data,b=expected.outputs[name].data;
    if(!a||a.length!==b.length)throw Error(`Wrong result length for ${name}`);
    for(let i=0;i<a.length;i++){
      const delta=Math.abs(a[i]-b[i]);worst=Math.max(worst,delta);
      if(exact?!Object.is(a[i],b[i]):(!Number.isFinite(a[i])||delta>tolerance+tolerance*Math.abs(b[i])))throw Error(`Mismatch in ${name} at ${i}: ${a[i]} versus ${b[i]}`);
    }
  }
  return worst;
}
/**
 * A prepared dropout MLP (masks drawn on the device by `uniform`, a fresh one
 * per step) run for six steps on `runtime` and on cpu-js: the masks must be
 * the same bits, the losses and parameters within the float32 tolerance.
 */
async function dropoutSession(runtime,reference){
  const spec=dropoutSessionProgram(),batches=spec.batches(6),out={};
  for(const [key,rt] of [['actual',runtime],['expected',reference]]){
    const session=await rt.prepare(spec.program,{resident:spec.resident});
    try{
      const run=await session.run(batches,{readback:['loss','noise']});
      out[key]={steps:run.steps,params:(await session.download(spec.resident)).outputs};
    }finally{session.dispose();}
  }
  let worst=0;const masks=new Set();
  out.expected.steps.forEach((s,i)=>{
    const a=out.actual.steps[i].outputs;
    compare({outputs:{noise:a.noise}},{outputs:{noise:s.outputs.noise}},0,true);
    worst=Math.max(worst,compare({outputs:{loss:a.loss}},{outputs:{loss:s.outputs.loss}}));
    masks.add(Array.from(a.noise.data,v=>v>0?1:0).join(''));
  });
  if(masks.size!==batches.length)throw Error(`Dropout masks repeat across steps (${masks.size} distinct of ${batches.length})`);
  return Math.max(worst,compare({outputs:out.actual.params},{outputs:out.expected.params}));
}
/**
 * A prepared version-4 session (embedding lookup by a fed index, a last-token
 * slice, a column slice, and their gradients through slice_scatter and
 * index_add) for six steps on `runtime` and on cpu-js: the first step's
 * looked-up rows are the same bits, the losses and weights within tolerance.
 */
async function embeddingSession(runtime,reference){
  const spec=embeddingSessionProgram(),batches=spec.batches(6),out={};
  for(const [key,rt] of [['actual',runtime],['expected',reference]]){
    const session=await rt.prepare(spec.program,{resident:spec.resident});
    try{
      const run=await session.run(batches,{readback:['loss','rows']});
      out[key]={steps:run.steps,params:(await session.download(spec.resident)).outputs};
    }finally{session.dispose();}
  }
  compare({outputs:{rows:out.actual.steps[0].outputs.rows}},{outputs:{rows:out.expected.steps[0].outputs.rows}},0,true);
  let worst=0;
  out.expected.steps.forEach((s,i)=>{worst=Math.max(worst,compare({outputs:out.actual.steps[i].outputs},{outputs:s.outputs}));});
  return Math.max(worst,compare({outputs:out.actual.params},{outputs:out.expected.params}));
}
/** Explicit backend selection: unsupported is a skip; a numerical/shader failure is a failure. */
export async function checkBackend(backend,options={}){
  let runtime,reference;
  try{runtime=await createRuntime({backend,...options});}
  catch(error){return {backend,status:'unavailable',passed:0,error:String(error.message),checks:[]};}
  const report={backend,status:'passed',passed:0,info:runtime.info(),checks:[]};
  try{
    reference=await createRuntime({backend:'cpu-js'});
    for(const [name,p] of cases()) {
      try {
        const actual=await runtime.execute(p),expected=await reference.execute(p),maxAbsError=compare(actual,expected,2e-4,name.startsWith('exact '));
        report.checks.push({name,status:'passed',maxAbsError,totalWallMs:actual.stats.totalWallMs});report.passed++;
      }catch(error){report.status='failed';report.checks.push({name,status:'failed',error:String(error.message)});}
    }
    for(const [name,run] of [['prepared dropout MLP: fresh device masks per step, bit-identical to cpu-js',dropoutSession],
      ['prepared embedding/slice step (version 4): fed index, exact lookups, trains like cpu-js',embeddingSession]]){
      try{const maxAbsError=await run(runtime,reference);report.checks.push({name,status:'passed',maxAbsError});report.passed++;}
      catch(error){report.status='failed';report.checks.push({name,status:'failed',error:String(error.message)});}
    }
  }finally{reference?.dispose();runtime.dispose();}
  return report;
}
/**
 * One MLP classification training step (forward, cross-entropy, backward and
 * Adam in one graph) on `backend`: per-output max abs error against cpu-js,
 * then cold and warm wall-clock times. `steps` chains real updates.
 *
 * Then the same step through prepared sessions, all with typed outputs: (a)
 * `execute()` reading every output back, (b) a session run one step at a
 * time (parameters and moments resident, x and targets fed, the loss read
 * back) and (c) a session run eight steps per `run`, plus the largest
 * difference between five session steps and five chained executes on this
 * backend fed the same batches.
 */
export async function benchmarkTraining(backend,{sizes=[784,256,10],batch=64,warm=10,steps=5,...options}={}){
  const runtime=await createRuntime({backend,...options}),reference=await createRuntime({backend:'cpu-js'});
  try{
    const {program,parameters}=mlpTrainingStep({sizes,batch});
    const cold=performance.now(),first=await runtime.execute(program),coldMs=performance.now()-cold;
    const expected=await reference.execute(program),errors={};
    for(const name of Object.keys(expected.outputs)){let worst=0;expected.outputs[name].data.forEach((v,i)=>{worst=Math.max(worst,Math.abs(v-first.outputs[name].data[i]));});errors[name]=worst;}
    const median=async p=>{const times=[];for(let i=0;i<warm;i++){const t=performance.now();await runtime.execute(p);times.push(performance.now()-t);}
      times.sort((x,y)=>x-y);return times;};
    const times=await median(program);
    // The same step reading back only the loss (the device work plus a readback's
    // latency), and at batch 512 where arithmetic, not transfer, dominates.
    const lossOnly=await median({...program,outputs:[program.outputs[0]]});
    let batch512;
    try{const big=mlpTrainingStep({sizes,batch:512}).program;await runtime.execute({...big,outputs:[big.outputs[0]]});
      batch512=(await median({...big,outputs:[big.outputs[0]]}))[Math.floor(warm/2)];}
    catch(error){batch512=error.code||String(error.message);}
    // A short training run fed by each step's outputs: losses must fall.
    let params=null,state=null;const losses=[];
    for(let step=1;step<=steps;step++){
      const next=mlpTrainingStep({sizes,batch,params,state,step,lr:0.002}).program,out=(await runtime.execute(next)).outputs;
      losses.push(out.loss.data[0]);params=Array.from({length:parameters},(_,i)=>out[`p${i}`].data);state=params.map((_,i)=>[out[`m${i}`].data,out[`v${i}`].data]);
    }
    const sessions=await benchmarkSessions(runtime,{sizes,batch,warm,parameters});
    return {backend,info:runtime.info(),sizes,batch,estimatedWork:first.stats.estimatedWork,uploadElements:first.stats.uploadElements,
      readbackElements:first.stats.readbackElements,maxAbsErrorVsCpuJs:errors,maxAbsError:Math.max(...Object.values(errors)),
      coldMs,warmMedianMs:times[Math.floor(times.length/2)],warmMinMs:times[0],lossOnlyWarmMedianMs:lossOnly[Math.floor(lossOnly.length/2)],
      batch512LossOnlyWarmMedianMs:batch512,losses,...sessions};
  }finally{reference.dispose();runtime.dispose();}
}

const medianOf=times=>{const t=[...times].sort((x,y)=>x-y);return t[Math.floor(t.length/2)];};
/** The typed-output timings and the session-versus-chained check `benchmarkTraining` reports. */
async function benchmarkSessions(runtime,{sizes,batch,warm,parameters}){
  const spec=mlpSessionProgram({sizes,batch,lr:0.002}),rnd=seeded(11),classes=sizes[sizes.length-1];
  const batchOf=()=>({inputs:{0:Float32Array.from({length:batch*sizes[0]},()=>rnd(0,1)),1:Float32Array.from({length:batch},()=>Math.floor(rnd(0,classes)))}});
  // (a) execute() as today, but with typed outputs: every output (weights, m, v) read back per step.
  const step=mlpTrainingStep({sizes,batch,lr:0.002}).program,typed=[];
  await runtime.execute(step,{typedOutputs:true});
  for(let i=0;i<warm;i++){const t=performance.now();await runtime.execute(step,{typedOutputs:true});typed.push(performance.now()-t);}
  // (b) one step per run: only x and the targets go up, only the loss comes back.
  const session=await runtime.prepare(spec.program,{resident:spec.resident}),one=[],eight=[];
  await session.run(batchOf(),{readback:['loss']});
  for(let i=0;i<warm;i++){const b=batchOf();const t=performance.now();await session.run(b,{readback:['loss']});one.push(performance.now()-t);}
  // (c) eight steps per run in one submission, eight losses read back at the end.
  for(let i=0;i<Math.max(3,Math.ceil(warm/2));i++){const bs=Array.from({length:8},batchOf);const t=performance.now();await session.run(bs,{readback:['loss']});eight.push((performance.now()-t)/8);}
  const residentBytes=session.residentBytes;session.dispose();
  // Five session steps against five chained executes on this backend, fed the same batches.
  const data=Array.from({length:5},batchOf),check=await runtime.prepare(spec.program,{resident:spec.resident});
  const run=await check.run(data,{readback:['loss']}),sessionLoss=run.steps.map(s=>s.outputs.loss.data[0]),sessionP0=(await check.download(['p0'])).outputs.p0.data;
  check.dispose();
  let params=null,state=spec.state,chainedP0=null;const chainedLoss=[];
  for(let s=1;s<=5;s++){
    const next=mlpTrainingStep({sizes,batch,seed:spec.seed,lr:0.002,params,state,step:s,x:Array.from(data[s-1].inputs[0]),targets:Array.from(data[s-1].inputs[1])}).program;
    const out=(await runtime.execute(next,{typedOutputs:true})).outputs;
    chainedLoss.push(out.loss.data[0]);params=Array.from({length:parameters},(_,i)=>out[`p${i}`].data);state=params.map((_,i)=>[out[`m${i}`].data,out[`v${i}`].data]);chainedP0=out.p0.data;
  }
  let sessionVsChainedMaxAbsError=0;
  sessionLoss.forEach((v,i)=>{sessionVsChainedMaxAbsError=Math.max(sessionVsChainedMaxAbsError,Math.abs(v-chainedLoss[i]));});
  chainedP0.forEach((v,i)=>{sessionVsChainedMaxAbsError=Math.max(sessionVsChainedMaxAbsError,Math.abs(v-sessionP0[i]));});
  return {executeTypedWarmMedianMs:medianOf(typed),sessionOneStepWarmMedianMs:medianOf(one),sessionEightStepsPerStepMedianMs:medianOf(eight),
    sessionResidentBytes:residentBytes,sessionLosses:sessionLoss,sessionVsChainedMaxAbsError};
}
