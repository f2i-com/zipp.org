// HOST code. Python plugins never get to supply these import URLs or this worker.
import init,{Engine} from '../../dist/all/zipp_wasm.js';
import {createRuntime} from '../../gpu-lab/src/runtime.mjs';
import {PluginRegistry,ModelSession,FileMapSource,downloadPluginSource,fetchBytes,readJSON} from '../src/index.mjs';
const root=new URL('../',import.meta.url);let running=false,pendingApproval=null;
const progress=message=>self.postMessage({type:'progress',message});
async function builtInSource(selection,localModelEntries=null){
  const id=selection==='bigram'?'org.zipp.bigram':selection==='gpt-neo'?'org.zipp.gpt-neo':'org.zipp.tiny-causal';
  const catalogueSource=new FileMapSource(new Map([['catalog.v1.json',await fetchBytes(new URL('catalog.v1.json',root),{maxBytes:65536})]]));
  const catalogue=await readJSON(catalogueSource,'catalog.v1.json',65536);
  if(catalogue.version!==1||!Array.isArray(catalogue.plugins))throw Error('Unsupported catalogue');
  const choice=catalogue.plugins.find(p=>p.id===id);if(!choice)throw Error('Bundled plugin missing');
  // Known host-owned catalogue URLs only. downloadPluginSource validates hashes
  // and same-origin source fetches; no model field can cause this call.
  const manifestPath=selection==='bigram'?'plugins/bigram/plugin.json':selection==='gpt-neo'?'plugins/gpt-neo/plugin.json':'plugins/tiny-causal/plugin.json';
  if(choice.manifest!==manifestPath)throw Error('Unexpected catalogue entry');
  const plugin=await downloadPluginSource(new URL(manifestPath,root),choice.sha256);
  // Mixed mode downloads architecture support only. The checkpoint is supplied
  // by the user and never fetched from model metadata or a remote registry.
  if(localModelEntries!==null)return {pluginSource:plugin,modelSource:new FileMapSource(localModelEntries),expectedHash:choice.sha256};
  const modelRoot=new URL(`examples/${selection}/`,root),entries=new Map();
  for(const path of ['model.json','weights.safetensors'])entries.set(path,await fetchBytes(new URL(path,modelRoot),{maxBytes:4*1024*1024}));
  return {pluginSource:plugin,modelSource:new FileMapSource(entries),expectedHash:choice.sha256};
}
// An unpinned checkpoint's digests are only known here, after the folder has
// been read; the page decides with them in hand.
function askApproval(identity){
  return new Promise(resolve=>{pendingApproval=resolve;self.postMessage({type:'approve',identity});});
}
self.onmessage=async({data})=>{
  if(data.type==='approved'){const resolve=pendingApproval;pendingApproval=null;resolve?.(data.ok===true);return;}
  if(running||data.type!=='run')return;running=true;
  let session=null,runtime=null;
  try{
    if(!['tiny-char','bigram','local','catalogue-local','hf-checkpoint'].includes(data.selection))throw Error('Unknown source selection');
    if(data.selection==='catalogue-local'&&!['tiny-causal','bigram'].includes(data.cataloguePlugin))throw Error('Unknown catalogue architecture');
    if(data.selection==='hf-checkpoint'&&data.cataloguePlugin!=='gpt-neo')throw Error('Only the gpt-neo plugin reads a checkpoint folder');
    progress(data.selection==='local'?'Reading explicitly supplied local assets…':['catalogue-local','hf-checkpoint'].includes(data.selection)?'Downloading approved architecture source only; using your local checkpoint…':'Downloading approved same-origin example assets…');
    const catalogueSelection=data.selection==='catalogue-local'?(data.cataloguePlugin==='bigram'?'bigram':'tiny-char'):data.selection==='hf-checkpoint'?'gpt-neo':data.selection;
    const inputs=data.selection==='local'?{pluginSource:new FileMapSource(data.pluginEntries),modelSource:new FileMapSource(data.modelEntries)}:await builtInSource(catalogueSelection,['catalogue-local','hf-checkpoint'].includes(data.selection)?data.modelEntries:null);
    const registry=new PluginRegistry();
    const plugin=await registry.install(inputs.pluginSource,{approve:()=>true,expectedHash:inputs.expectedHash});
    progress('Initializing the Python-enabled ZIPP engine and compute backend…');
    // dist/all is the regular Python build, NOT the trusted-code interop build.
    await init();
    // A real checkpoint is deeper and heavier than the bundled fixtures, and the
    // compute runtime keeps its own budget: both have to admit the graph.
    runtime=await createRuntime({backend:data.backend,wasmUrl:new URL('../../gpu-lab/wasm/kernels.wasm',import.meta.url),
      limits:{maxNodes:4096,maxWork:2_000_000_000}});
    const open={source:inputs.modelSource,plugin,engineFactory:()=>new Engine(),runtime};
    if(data.selection==='hf-checkpoint'){
      progress('Reading the checkpoint folder and hashing what it contains…');
      session=await ModelSession.openNative({...open,approve:askApproval});
    }else{
      session=await ModelSession.open(open);
    }
    self.postMessage({type:'info',info:{...session.info,compute:runtime.info()}});
    const result=await session.generate(data.prompt,{maxNewTokens:data.maxNewTokens,temperature:0,onToken:token=>self.postMessage({type:'token',...token})});
    session.dispose();session=null;runtime.dispose();runtime=null;
    self.postMessage({type:'done',result});
  }catch(error){self.postMessage({type:'error',code:error.code||'MODEL',message:String(error.message||error)});}
  finally{try{session?.dispose();}catch{}try{runtime?.dispose();}catch{}running=false;}
};
