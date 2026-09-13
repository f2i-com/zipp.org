const $=id=>document.getElementById(id);
const recipes={
vector:`from zipp_gpu import Graph\n\ngpu = Graph()\na = gpu.tensor([1, 2, 3, 4])\nb = gpu.tensor([10, 20, 30, 40])\n\nc = (a * b + 4).relu()\nprogram = gpu.program(\n    result=c,\n    total=c.sum(),\n)`,
matmul:`from zipp_gpu import Graph\n\ngpu = Graph()\na = gpu.tensor([[1, 2, 3],\n                [4, 5, 6]])\nb = gpu.tensor([[7, 8],\n                [9, 10],\n                [11, 12]])\n\nprogram = gpu.program(result=a @ b)`,
tiny_mlp:`from zipp_gpu import Graph\n\ngpu = Graph()\nx = gpu.tensor([[1, -2, 3], [0, 1, 2]])\nw1 = gpu.tensor([[.5, -1, 2, 0],\n                 [1, 0, -.5, 1],\n                 [-1, 1, 0, .5]])\nh = (x @ w1 + .25).relu()\nw2 = gpu.tensor([[1, 0], [.5, -.5],\n                 [1, 1], [-1, 2]])\nprogram = gpu.program(result=h @ w2)`,
life:`from zipp_gpu import Graph\n\ngpu = Graph()\n# Seed data is constructed in build_examples.py.\ninitial = gpu.tensor(seed, shape=(32, 32))\nstate = initial\n\nfor _ in range(24):\n    state = state.life()\n\nprogram = gpu.program(\n    initial=initial, result=state,\n    alive=state.sum(),\n)`};
let worker=null,sequence=0,selectedBackend=null,currentProgram=null,lastReport=null,previewGeneration=0;
const pending=new Map();
function status(text,error=false){$('status').textContent=text;$('statusDot').className='dot '+(error?'error':'active');}
function resetWorker(reason='Worker reset'){
  worker?.terminate();worker=null;selectedBackend=null;
  for(const task of pending.values()){clearTimeout(task.timer);task.reject(Error(reason));}pending.clear();
}
function ensureWorker(){
  if(worker)return;
  worker=new Worker(new URL('./compute-worker.mjs',import.meta.url),{type:'module'});
  worker.onmessage=({data})=>{const task=pending.get(data.id);if(!task)return;pending.delete(data.id);clearTimeout(task.timer);data.ok?task.resolve(data.value):task.reject(Error(data.error));};
  worker.onerror=e=>{resetWorker(e.message||'Worker failed');};
}
function rpc(kind,payload,timeout=30000){
  ensureWorker();const id=++sequence;
  return new Promise((resolve,reject)=>{
    const timer=setTimeout(()=>{resetWorker('Demo deadline reached. Worker stopped; already submitted GPU work may still complete.');},timeout);
    pending.set(id,{resolve,reject,timer});worker.postMessage({id,kind,payload});
  });
}
function disable(flag){for(const id of ['run','check','backend','example','upload'])$(id).disabled=flag;}
async function preview(){
  const generation=++previewGeneration,name=$('example').value;$('pythonCode').textContent=recipes[name];
  const response=await fetch(`../generated/${name}.json`);if(!response.ok)throw Error('Could not load example graph');
  const program=await response.json();if(generation!==previewGeneration)return;
  currentProgram=program;$('graphCode').textContent=JSON.stringify(program,null,2);
}
function showResult(r){
  $('outputTag').textContent=r.backend.toUpperCase()+' · COMPLETE';$('engineBadge').textContent=r.backend.toUpperCase();
  $('metrics').replaceChildren();
  for(const [value,label]of [[r.stats.nodes,'GRAPH NODES'],[r.stats.readbackElements,'VALUES READ'],[r.stats.totalWallMs.toFixed(2)+' ms','TOTAL WALL TIME']]){
    const div=document.createElement('div');div.className='metric';const strong=document.createElement('strong'),span=document.createElement('span');strong.textContent=value;span.textContent=label;div.append(strong,span);$('metrics').append(div);
  }
  const output=Object.fromEntries(Object.entries(r.outputs).map(([name,v])=>[name,{...v,data:v.data.length>80?`[${v.data.length} float32 values; first 16: ${v.data.slice(0,16).join(', ')}]`:v.data}]));
  $('resultCode').textContent=JSON.stringify(output,null,2);
  const life=r.outputs.result;$('lifeCanvas').hidden=!(life?.shape.length===2&&life.shape[0]===32&&life.shape[1]===32);
  if(!$('lifeCanvas').hidden){const ctx=$('lifeCanvas').getContext('2d');ctx.fillStyle='#0b131b';ctx.fillRect(0,0,384,384);for(let i=0;i<1024;i++){ctx.fillStyle=life.data[i]>0.5?'#b3f4d1':'#182a32';ctx.fillRect((i%32)*12+1,Math.floor(i/32)*12+1,10,10);}}
}
$('run').onclick=async()=>{
  disable(true);status('Preparing compute backend...');
  try{
    if(!currentProgram)await preview();const backend=$('backend').value;
    if(selectedBackend!==backend){const info=await rpc('init',{backend});selectedBackend=backend;$('diagnostics').textContent=JSON.stringify(info,null,2);$('engineBadge').textContent=info.backend.toUpperCase();}
    status('Executing graph in a dedicated Worker...');const r=await rpc('execute',currentProgram);showResult(r);lastReport=r;$('save').disabled=false;status(`Complete. Actual backend: ${r.backend}.`);
  }catch(e){status(e.message,true);$('outputTag').textContent='FAILED';}finally{disable(false);}
};
$('example').onchange=()=>preview().catch(e=>status(e.message,true));
$('check').onclick=async()=>{
  disable(true);$('checkOutput').textContent='';const reports=[];
  try{
    for(const backend of ['webgpu','webgl2','wasm','cpu-js']){
      status(`Checking ${backend} against the numerical reference...`);
      let report;try{report=await rpc('check',{backend},60000);}catch(e){report={backend,status:'failed',error:e.message};}
      reports.push(report);$('checkOutput').textContent=JSON.stringify(reports,null,2);
    }
    lastReport={testedAt:new Date().toISOString(),userAgent:navigator.userAgent,reports};$('save').disabled=false;
    status(reports.some(r=>r.status==='failed')?'Checks found a failure; inspect the report.':'Checks complete. Unavailable backends are listed separately.',reports.some(r=>r.status==='failed'));
  }finally{disable(false);}
};
$('upload').onchange=async e=>{
  const file=e.target.files[0];if(!file)return;
  try{if(file.size>24*1024*1024)throw Error('Graph file exceeds 24 MiB');currentProgram=JSON.parse(await file.text());previewGeneration++;$('pythonCode').textContent='# Custom graph loaded from '+file.name+'\n# The host validates it before execution.';$('graphCode').textContent=JSON.stringify(currentProgram,null,2);status('Custom graph loaded. Press Run graph.');}
  catch(error){status(error.message,true);}finally{e.target.value='';}
};
$('save').onclick=()=>{
  if(!lastReport)return;const url=URL.createObjectURL(new Blob([JSON.stringify(lastReport,null,2)],{type:'application/json'}));
  const a=document.createElement('a');a.href=url;a.download='zipp-gpu-report.json';a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);
};
window.addEventListener('beforeunload',()=>resetWorker());
preview().catch(e=>status(e.message,true));
