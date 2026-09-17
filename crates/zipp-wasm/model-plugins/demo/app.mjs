import {safePath} from '../src/common.mjs';
const $=id=>document.getElementById(id);
let worker=null, deadline=null, generation=0;
function busy(value){$('run').disabled=value;$('cancel').disabled=!value;}
function stop(message){generation++;clearTimeout(deadline);worker?.terminate();worker=null;busy(false);if(message)$('status').textContent=message;}
// A main-thread deadline is essential: a Promise timeout inside a Worker cannot
// interrupt a synchronous Python hook. A new worker is created for every run.
function arm(){clearTimeout(deadline);deadline=setTimeout(()=>stop('Stopped: no progress before the host deadline. The Worker was terminated.'),15000);}
function entries(input){
  const files=Array.from(input.files);if(!files.length)throw Error(`Select the ${input.id} folder first.`);
  const map=new Map();const root=files[0].webkitRelativePath?.split('/')[0];
  for(const f of files){
    const raw=f.webkitRelativePath||f.name;
    if(root&&!raw.startsWith(root+'/'))throw Error('Select one root folder.');
    const path=safePath(root?raw.slice(root.length+1):raw);
    if(map.has(path))throw Error('Duplicate selected path');map.set(path,f);
  }
  // Store explicit paths: nonstandard File.webkitRelativePath need not survive
  // structured cloning across the Worker boundary.
  return map;
}
// Whether the optional GGUF WebAssembly module was built, answered once at
// startup so the page can say so instead of failing at run time.
import('../src/index.mjs').then(async ({ggufSupport})=>{
  const support=await ggufSupport();
  $('gguf-support').textContent=support.available
    ?'GGUF support is built and ready.'
    :`GGUF support is not built. ${support.remedy}`;
  $('gguf-support').classList.toggle('warn',!support.available);
}).catch(error=>{$('gguf-support').textContent=`GGUF support could not be checked: ${error.message}`;});

$('source').addEventListener('change',()=>{
  const source=$('source').value;
  $('gguf-fields').hidden=source!=='gguf';
  $('local-fields').hidden=!['local','catalogue-local','hf-checkpoint'].includes(source);
  $('plugin-fields').hidden=source!=='local';
  $('catalogue-fields').hidden=!['catalogue-local','hf-checkpoint'].includes(source);
  $('hf-hint').hidden=source!=='hf-checkpoint';
  if(source==='hf-checkpoint')$('catalogue-plugin').value='gpt-neo';
  if($('source').value==='bigram')$('prompt').value='';
});
$('cancel').addEventListener('click',()=>stop('Stopped. Model engine and Worker discarded.'));
$('run').addEventListener('click',()=>{
  try{
    const selection=$('source').value,maxNewTokens=Number($('count').value);
    if(!Number.isInteger(maxNewTokens)||maxNewTokens<0||maxNewTokens>128)throw Error('Use 0–128 new tokens.');
    const payload={type:'run',selection,backend:$('backend').value,prompt:$('prompt').value,maxNewTokens};
    if(selection==='gguf'){
      const file=$('gguf').files[0];
      if(!file)throw Error('Choose a .gguf file first.');
      payload.ggufFile=file;
      payload.temperature=Number($('temperature').value);
      payload.topK=Number($('topk').value);
      if(!Number.isFinite(payload.temperature)||payload.temperature<0||payload.temperature>2)throw Error('Use a temperature between 0 and 2.');
      if(!Number.isInteger(payload.topK)||payload.topK<0||payload.topK>200)throw Error('Use a top-k between 0 and 200.');
    }
    if(selection==='local')payload.pluginEntries=entries($('plugin'));
    if(['local','catalogue-local','hf-checkpoint'].includes(selection))payload.modelEntries=entries($('model'));
    if(['catalogue-local','hf-checkpoint'].includes(selection))payload.cataloguePlugin=$('catalogue-plugin').value;
    const warning=selection==='gguf'?'Download the architecture support this checkpoint names from this website, then read your GGUF file as it is? Nothing is uploaded, and the file is read by range rather than loaded.':selection==='hf-checkpoint'?'Download the selected Python architecture support from this website, then read your checkpoint folder as it is? You will be shown the digest of every file before it runs. Nothing is uploaded.':selection==='catalogue-local'?'Download the selected Python architecture support from this website, then run your local model? Model weights will not be downloaded or uploaded.':selection==='local'?'Run Python from the selected plugin folder inside ZIPP? Only approve code whose origin you trust. No local file will be uploaded.':'Download the selected tiny example and Python plugin from this website, then run it locally inside ZIPP?';
    if(!window.confirm(warning))return;
    stop();const gen=++generation;busy(true);$('output').textContent='';$('details').textContent='';$('status').textContent='Starting isolated model Worker…';
    worker=new Worker(new URL('./worker.mjs',import.meta.url),{type:'module'});arm();
    worker.onmessage=({data})=>{
      if(gen!==generation)return;
      arm();
      if(data.type==='progress')$('status').textContent=data.message;
      if(data.type==='info'){$('details').textContent=JSON.stringify(data.info,null,2);$('status').textContent=`Running on ${data.info.backend}`;}
      if(data.type==='token'){$('output').textContent=data.text;$('status').textContent=`${data.count} tokens · ${data.backend}`;}
      if(data.type==='done'){$('output').textContent=data.result.text;$('details').textContent+='\n\n'+JSON.stringify(data.result,null,2);stop(`Finished · ${data.result.finishReason} · ${data.result.backend}`);}
      if(data.type==='approve'){
        // An unpinned checkpoint: the digests are only known once the Worker has
        // read the folder, so the decision belongs here, with them in hand.
        const files=[`config ${data.identity.config.path} ${data.identity.config.sha256.slice(0,16)}…`,
          ...data.identity.weights.map(w=>`weights ${w.path} ${(w.bytes/1048576).toFixed(1)} MiB ${w.sha256.slice(0,16)}…`),
          ...data.identity.assets.map(a=>`${a.name} ${a.path} ${(a.bytes/1024).toFixed(0)} KiB ${a.sha256.slice(0,16)}…`)];
        $('details').textContent=JSON.stringify(data.identity,null,2);
        const ok=window.confirm(`Load this unpinned checkpoint with ${data.identity.plugin.id}@${data.identity.plugin.version}?\n\nIt declares no pin for this lab, so these are the files that will be read:\n\n${files.join('\n')}`);
        worker?.postMessage({type:'approved',ok});
        if(!ok)$('status').textContent='Waiting…';
      }
      if(data.type==='error')stop(`${data.code||'ERROR'}: ${data.message}`);
    };
    worker.onerror=event=>{if(gen===generation)stop(`Worker could not start: ${event.message}. Check that dist/all/zipp_wasm.js and its WASM binary have been built.`);};
    worker.postMessage(payload);
  }catch(error){stop(String(error.message||error));}
});
window.addEventListener('pagehide',()=>stop());
