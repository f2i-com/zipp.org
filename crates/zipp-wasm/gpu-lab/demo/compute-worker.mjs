import {createRuntime} from '../src/runtime.mjs';
import {checkBackend} from '../tests/browser-cases.mjs';
let runtime=null,busy=false;
self.onmessage=async({data})=>{
  const {id,kind,payload}=data||{};
  if(busy){self.postMessage({id,ok:false,error:'Worker is busy'});return;}
  busy=true;
  try {
    let value;
    switch(kind){
      case 'init':runtime?.dispose();runtime=null;runtime=await createRuntime({backend:payload.backend});value=runtime.info();break;
      case 'execute':if(!runtime)throw Error('Initialize the runtime first');value=await runtime.execute(payload);break;
      case 'check':value=await checkBackend(payload.backend);break;
      default:throw Error('Unknown demo operation');
    }
    self.postMessage({id,ok:true,value});
  }catch(error){self.postMessage({id,ok:false,error:String(error.message||error),code:error.code||'ERROR'});}
  finally{busy=false;}
};
