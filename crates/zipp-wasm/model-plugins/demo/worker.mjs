// HOST code. Python plugins never get to supply these import URLs or this worker.
import init,{Engine} from '../../dist/all/zipp_wasm.js';
import {createRuntime} from '../../gpu-lab/src/runtime.mjs';
import {PluginRegistry,ModelSession,FileMapSource,downloadPluginSource,fetchBytes,readJSON} from '../src/index.mjs';
const root=new URL('../',import.meta.url);let running=false;
const progress=message=>self.postMessage({type:'progress',message});
async function builtInSource(selection,localModelEntries=null){
  const id=selection==='bigram'?'org.zipp.bigram':'org.zipp.tiny-causal';
  const catalogueSource=new FileMapSource(new Map([['catalog.v1.json',await fetchBytes(new URL('catalog.v1.json',root),{maxBytes:65536})]]));
  const catalogue=await readJSON(catalogueSource,'catalog.v1.json',65536);
  if(catalogue.version!==1||!Array.isArray(catalogue.plugins))throw Error('Unsupported catalogue');
  const choice=catalogue.plugins.find(p=>p.id===id);if(!choice)throw Error('Bundled plugin missing');
  // Known host-owned catalogue URLs only. downloadPluginSource validates hashes
  // and same-origin source fetches; no model field can cause this call.
  const manifestPath=selection==='bigram'?'plugins/bigram/plugin.json':'plugins/tiny-causal/plugin.json';
  if(choice.manifest!==manifestPath)throw Error('Unexpected catalogue entry');
  const plugin=await downloadPluginSource(new URL(manifestPath,root),choice.sha256);
  // Mixed mode downloads architecture support only. The checkpoint is supplied
  // by the user and never fetched from model metadata or a remote registry.
  if(localModelEntries!==null)return {pluginSource:plugin,modelSource:new FileMapSource(localModelEntries),expectedHash:choice.sha256};
  const modelRoot=new URL(`examples/${selection}/`,root),entries=new Map();
  for(const path of ['model.json','weights.safetensors'])entries.set(path,await fetchBytes(new URL(path,modelRoot),{maxBytes:4*1024*1024}));
  return {pluginSource:plugin,modelSource:new FileMapSource(entries),expectedHash:choice.sha256};
}
self.onmessage=async({data})=>{
  if(running||data.type!=='run')return;running=true;
  let session=null,runtime=null;
  try{
    if(!['tiny-char','bigram','local','catalogue-local'].includes(data.selection))throw Error('Unknown source selection');
    if(data.selection==='catalogue-local'&&!['tiny-causal','bigram'].includes(data.cataloguePlugin))throw Error('Unknown catalogue architecture');
    progress(data.selection==='local'?'Reading explicitly supplied local assets…':data.selection==='catalogue-local'?'Downloading approved architecture source only; using local model weights…':'Downloading approved same-origin example assets…');
    const catalogueSelection=data.selection==='catalogue-local'?(data.cataloguePlugin==='bigram'?'bigram':'tiny-char'):data.selection;
    const inputs=data.selection==='local'?{pluginSource:new FileMapSource(data.pluginEntries),modelSource:new FileMapSource(data.modelEntries)}:await builtInSource(catalogueSelection,data.selection==='catalogue-local'?data.modelEntries:null);
    const registry=new PluginRegistry();
    const plugin=await registry.install(inputs.pluginSource,{approve:()=>true,expectedHash:inputs.expectedHash});
    progress('Initializing the Python-enabled ZIPP engine and compute backend…');
    // dist/all is the regular Python build, NOT the trusted-code interop build.
    await init();
    runtime=await createRuntime({backend:data.backend,wasmUrl:new URL('../../gpu-lab/wasm/kernels.wasm',import.meta.url)});
    session=await ModelSession.open({source:inputs.modelSource,plugin,engineFactory:()=>new Engine(),runtime});
    self.postMessage({type:'info',info:{...session.info,compute:runtime.info()}});
    const result=await session.generate(data.prompt,{maxNewTokens:data.maxNewTokens,temperature:0,onToken:token=>self.postMessage({type:'token',...token})});
    session.dispose();session=null;runtime.dispose();runtime=null;
    self.postMessage({type:'done',result});
  }catch(error){self.postMessage({type:'error',code:error.code||'MODEL',message:String(error.message||error)});}
  finally{try{session?.dispose();}catch{}try{runtime?.dispose();}catch{}running=false;}
};
