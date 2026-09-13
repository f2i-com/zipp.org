import {createRuntime} from '../src/runtime.mjs';
const input=(id,data,shape=[data.length])=>({id,op:'input',shape,data});
const program=nodes=>({version:1,nodes,outputs:[{name:'result',id:nodes.length-1}]});
function cases(){
  const result=[];
  for(const length of [1,3,63,64,65,129,1025]) {
    const a=Array.from({length},(_,i)=>(i%19-9)/3),b=Array.from({length},(_,i)=>(i%7-3)/5);
    result.push([`multiply/add/relu/sum length ${length}`,program([input(0,a),input(1,b),{id:2,op:'mul',a:0,b:1},{id:3,op:'add',a:2,b:0},{id:4,op:'relu',a:3},{id:5,op:'sum',a:4}])]);
  }
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
  return result;
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
        const actual=await runtime.execute(p),expected=await reference.execute(p),a=actual.outputs.result.data,b=expected.outputs.result.data;
        if(a.length!==b.length)throw Error('Wrong result length');
        for(let i=0;i<a.length;i++)if(!Number.isFinite(a[i])||Math.abs(a[i]-b[i])>2e-4+2e-4*Math.abs(b[i]))
          throw Error(`Mismatch at ${i}: ${a[i]} versus ${b[i]}`);
        report.checks.push({name,status:'passed',totalWallMs:actual.stats.totalWallMs});report.passed++;
      }catch(error){report.status='failed';report.checks.push({name,status:'failed',error:String(error.message)});}
    }
  }finally{reference?.dispose();runtime.dispose();}
  return report;
}
