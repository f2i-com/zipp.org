// HOST code. Python plugins never get to supply these import URLs or this worker.
import init,{Engine} from '../../dist/all/zipp_wasm.js';
import {createRuntime} from '../../gpu-lab/src/runtime.mjs';
import {PluginRegistry,ModelSession,FileMapSource,downloadPluginSource,fetchBytes,readJSON,
        ggufSupport,openGGUF,WeightStore,prepareDecode,stepInputs,resolveLimits,
        sampleLogits} from '../src/index.mjs';
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
// ---- GGUF: a checkpoint that describes itself, read where it lies ----

const GB=1024*1024*1024;
// A GGUF checkpoint is gigabytes and its tensors are tens of millions of
// values; every default here is sized for a model a page can hold, so a host
// that means to read one raises them deliberately.
const GGUF_LIMITS={maxModelFileBytes:32*GB,maxModelBytes:32*GB,maxDecodedBytes:4*GB,
  maxBoundInputBytes:4*GB,maxTensorElements:2**31,maxDimension:1<<21,maxTensors:4096,
  maxNodes:8192,maxContext:4096};
const GGUF_COMPUTE={maxNodes:8192,maxElements:4194304,maxInputElements:200_000_000,
  // Every carried cache counts as an output even though none is read back;
  // 28 layers of keys and values at this context is far past the default.
  maxOutputElements:200_000_000,maxLogicalBytes:3*GB,maxWork:200_000_000_000,
  maxDimension:1<<21,maxSessions:4,maxStepsPerRun:64,
  // WebGL2 holds every tensor in a texture, so the model's own size has to fit
  // this budget; the default is sized for a page-sized model.
  maxWebGLTextureBytes:2*GB};
// Which architecture this lab has a plugin for. A checkpoint naming anything
// else is refused by name rather than run with the wrong arithmetic.
const GGUF_PLUGINS={qwen3:{id:'org.zipp.qwen3',manifest:'plugins/qwen3/plugin.json'}};
// How far back a repetition penalty looks. llama.cpp's default, and long
// enough to catch a repeating phrase without penalising ordinary grammar.
const REPETITION_WINDOW=64;

async function runGguf(data){
  // The GGUF reader is a separate WebAssembly module and an optional one.
  const support=await ggufSupport();
  if(!support.available)throw Error(`GGUF support is not built (${support.reason}). ${support.remedy}`);
  progress('Reading the checkpoint header…');
  const limits=resolveLimits(GGUF_LIMITS);
  // A File is read by range. Nothing loads the whole checkpoint, here or anywhere.
  const source=new FileMapSource(new Map([['model.gguf',data.ggufFile]]),{maxTotalBytes:32*GB});
  const index=await openGGUF(source,'model.gguf',support.module,limits);
  const metadata=index.metadata();
  const architecture=metadata['general.architecture'];
  const known=GGUF_PLUGINS[architecture];
  if(!known)throw Error(`This checkpoint is ${architecture}; this lab has no plugin for that architecture yet.`);

  progress(`Downloading ${known.id} architecture support…`);
  const catalogueSource=new FileMapSource(new Map([['catalog.v1.json',await fetchBytes(new URL('catalog.v1.json',root),{maxBytes:65536})]]));
  const catalogue=await readJSON(catalogueSource,'catalog.v1.json',65536);
  const choice=catalogue.plugins.find(entry=>entry.id===known.id);
  if(!choice||choice.manifest!==known.manifest)throw Error('Catalogue has no entry for that architecture');
  const plugin=await new PluginRegistry().install(
    await downloadPluginSource(new URL(known.manifest,root),choice.sha256),
    {approve:()=>true,expectedHash:choice.sha256});

  progress('Starting the Python engine…');
  await init();
  const engine=new Engine();
  const store=new WeightStore([index],limits);
  let runtime=null,session=null;
  try{
    engine.setSyncHostCapabilities([]);
    engine.setInstructionBudget(limits.instructionBudget);
    engine.initPythonProject({...plugin.files},plugin.entry,[]);
    const call=(name,args)=>{engine.renewInstructionBudget();return JSON.parse(engine.pythonCall(name,args));};

    const tokenizer=index.tokenizer();
    const config=call('zipp_model_config',[JSON.stringify(metadata),JSON.stringify(tokenizer.vocab_size)]);
    const described=call('zipp_model_describe',[JSON.stringify(config)]);
    const context=Math.min(256,described.max_context);

    progress('Binding weights — quantized ones stay in their block format…');
    const template=call('zipp_model_decode_graph',[JSON.stringify(config),JSON.stringify(context)]);
    const plan=await prepareDecode(template,store,limits);
    let blocks=0,floats=0;
    for(const node of plan.program.nodes){
      if(node.op!=='input'||!node.data)continue;
      if(node.dtype)blocks+=node.data.length;else floats+=node.data.byteLength??node.data.length*4;
    }
    runtime=await createRuntime({backend:data.backend,
      wasmUrl:new URL('../../gpu-lab/wasm/kernels.wasm',import.meta.url),limits:GGUF_COMPUTE});
    session=await runtime.prepare(plan.program,{resident:plan.resident,typedOutputs:true});
    self.postMessage({type:'info',info:{
      architecture,name:metadata['general.name']??architecture,
      family:described.family,vocabSize:described.vocab_size,context,
      residentMB:Number(((blocks+floats)/1048576).toFixed(0)),
      quantizedMB:Number((blocks/1048576).toFixed(0)),
      asFloat32MB:Number((plan.program.nodes.reduce((sum,n)=>
        sum+(n.op==='input'&&n.data?(n.dtype?n.data.length/144*256:n.data.length):0),0)*4/1048576).toFixed(0)),
      tokenizer:{pre:tokenizer.pre,vocab:tokenizer.vocab_size},
      backend:runtime.info().backend,compute:runtime.info()}});

    const timings=[];
    const prompt=[...tokenizer.encode(data.prompt)];
    if(!prompt.length)throw Error('The prompt is empty once tokenized.');
    if(prompt.length>=context)throw Error(`The prompt is ${prompt.length} tokens and this cache holds ${context}.`);
    const generated=[];
    let token=prompt[0];
    for(let position=0;position<context-1;position++){
      const began=performance.now();
      const step=await stepInputs(plan,store,{token,position});
      const out=await session.run([step],{readback:['logits']});
      timings.push(performance.now()-began);
      const logits=out.outputs.logits.data;
      if(position+1<prompt.length){token=prompt[position+1];continue;}
      // Greedy decoding on a 0.6B model repeats a phrase forever; the tail cut
      // off and a penalty on what has just been said is what stops that, and
      // it is the model's quality being sampled rather than anything about the
      // backend. The window is the last 64 tokens, as llama.cpp defaults to.
      const next=sampleLogits(logits,{
        temperature:data.temperature??0,topK:data.topK??0,topP:data.topP??1,
        repetitionPenalty:data.repetitionPenalty??1,
        recent:[...prompt,...generated].slice(-REPETITION_WINDOW)});
      if(next===tokenizer.eos)break;
      generated.push(next);
      token=next;
      self.postMessage({type:'token',text:tokenizer.decode(Uint32Array.from(generated)),
        count:generated.length,backend:runtime.info().backend});
      if(generated.length>=data.maxNewTokens)break;
    }
    const text=tokenizer.decode(Uint32Array.from(generated));
    session.dispose();session=null;runtime.dispose();runtime=null;
    // Prompt steps prime the cache and are not what a reader means by speed;
    // the median of the generated ones is.
    const warm=timings.slice(prompt.length).sort((a,b)=>a-b);
    const median=warm.length?warm[warm.length>>1]:timings[timings.length-1];
    self.postMessage({type:'done',result:{text,tokens:generated,
      finishReason:generated.length>=data.maxNewTokens?'length':'stop',
      msPerToken:Number(median.toFixed(1)),
      tokensPerSecond:Number((1000/median).toFixed(1)),
      backend:'prepared decode'}});
  }finally{
    try{session?.dispose();}catch{}
    try{runtime?.dispose();}catch{}
    store.dispose();engine.dispose();
  }
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
    if(data.selection==='gguf'){await runGguf(data);return;}
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
