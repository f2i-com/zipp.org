import {createRuntime} from '../src/runtime.mjs';
import {opCases, mlpTrainingStep} from './ml-cases.mjs';
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
  return [...result,...opCases()];
}
/** Largest |actual - expected| over every named output; throws past a float32 GPU tolerance. */
function compare(actual,expected,tolerance=2e-4){
  let worst=0;
  for(const name of Object.keys(expected.outputs)){
    const a=actual.outputs[name]?.data,b=expected.outputs[name].data;
    if(!a||a.length!==b.length)throw Error(`Wrong result length for ${name}`);
    for(let i=0;i<a.length;i++){
      const delta=Math.abs(a[i]-b[i]);worst=Math.max(worst,delta);
      if(!Number.isFinite(a[i])||delta>tolerance+tolerance*Math.abs(b[i]))throw Error(`Mismatch in ${name} at ${i}: ${a[i]} versus ${b[i]}`);
    }
  }
  return worst;
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
        const actual=await runtime.execute(p),expected=await reference.execute(p),maxAbsError=compare(actual,expected);
        report.checks.push({name,status:'passed',maxAbsError,totalWallMs:actual.stats.totalWallMs});report.passed++;
      }catch(error){report.status='failed';report.checks.push({name,status:'failed',error:String(error.message)});}
    }
  }finally{reference?.dispose();runtime.dispose();}
  return report;
}
/**
 * One MLP classification training step (forward, cross-entropy, backward and
 * Adam in one graph) on `backend`: per-output max abs error against cpu-js,
 * then cold and warm wall-clock times. `steps` chains real updates.
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
    return {backend,info:runtime.info(),sizes,batch,estimatedWork:first.stats.estimatedWork,uploadElements:first.stats.uploadElements,
      readbackElements:first.stats.readbackElements,maxAbsErrorVsCpuJs:errors,maxAbsError:Math.max(...Object.values(errors)),
      coldMs,warmMedianMs:times[Math.floor(times.length/2)],warmMinMs:times[0],lossOnlyWarmMedianMs:lossOnly[Math.floor(lossOnly.length/2)],
      batch512LossOnlyWarmMedianMs:batch512,losses};
  }finally{reference.dispose();runtime.dispose();}
}
