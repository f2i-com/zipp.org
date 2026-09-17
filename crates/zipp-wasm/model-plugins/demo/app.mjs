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
$('source').addEventListener('change',()=>{
  const source=$('source').value;
  $('local-fields').hidden=!['local','catalogue-local'].includes(source);
  $('plugin-fields').hidden=source!=='local';
  $('catalogue-fields').hidden=source!=='catalogue-local';
  if($('source').value==='bigram')$('prompt').value='';
});
$('cancel').addEventListener('click',()=>stop('Stopped. Model engine and Worker discarded.'));
$('run').addEventListener('click',()=>{
  try{
    const selection=$('source').value,maxNewTokens=Number($('count').value);
    if(!Number.isInteger(maxNewTokens)||maxNewTokens<0||maxNewTokens>128)throw Error('Use 0–128 new tokens.');
    const payload={type:'run',selection,backend:$('backend').value,prompt:$('prompt').value,maxNewTokens};
    if(selection==='local')payload.pluginEntries=entries($('plugin'));
    if(['local','catalogue-local'].includes(selection))payload.modelEntries=entries($('model'));
    if(selection==='catalogue-local')payload.cataloguePlugin=$('catalogue-plugin').value;
    const warning=selection==='catalogue-local'?'Download the selected Python architecture support from this website, then run your local model? Model weights will not be downloaded or uploaded.':selection==='local'?'Run Python from the selected plugin folder inside ZIPP? Only approve code whose origin you trust. No local file will be uploaded.':'Download the selected tiny example and Python plugin from this website, then run it locally inside ZIPP?';
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
      if(data.type==='error')stop(`${data.code||'ERROR'}: ${data.message}`);
    };
    worker.onerror=event=>{if(gen===generation)stop(`Worker could not start: ${event.message}. Check that dist/all/zipp_wasm.js and its WASM binary have been built.`);};
    worker.postMessage(payload);
  }catch(error){stop(String(error.message||error));}
});
window.addEventListener('pagehide',()=>stop());
