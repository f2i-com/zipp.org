import {check, ComputeError} from './graph.mjs';

/** Async host-call handler. Does not drain queues, guess tenants, or re-enter a running VM. */
export function createZippGPUHandler(runtime, {allowExecute=false}={}) {
  let active=true;
  return Object.freeze({
    // `options` reaches `runtime.execute` (`typedOutputs`).
    async handle(kind,args,options={}) {
      check(active,'DISPOSED','GPU handler has been invalidated');
      check(allowExecute===true,'DENIED','GPU compute is not granted to this tenant');
      check(kind==='gpu.execute','DENIED','Unknown GPU operation');
      check(Array.isArray(args)&&args.length===1,'PROTOCOL','gpu.execute expects exactly one graph');
      const result=await runtime.execute(args[0],options);
      check(active,'CANCELLED','Tenant was invalidated while compute was in flight');
      return result;
    },
    invalidate(){active=false;},
  });
}

/**
 * Adapter for requests ALREADY drained by the host's one central ZIPP queue owner.
 * Each closure belongs to one immutable Engine/Worker generation. No global ID routing.
 * It serializes dispatch and rejects duplicate IDs, and never pumps/drains unrelated kinds.
 * Call invalidate before terminating a tenant; await idle, then dispose the GPU runtime.
 */
export function createZippGPUAdapter(engine,runtime,{allowExecute=false,maxRequests=4096,maxPending=64}={}) {
  check(Number.isSafeInteger(maxRequests)&&maxRequests>0&&Number.isSafeInteger(maxPending)&&maxPending>0,
    'LIMIT','Adapter request limits must be positive integers');
  const handler=createZippGPUHandler(runtime,{allowExecute});
  let active=true,tail=Promise.resolve(),pending=0;
  const seen=new Set();
  const live=()=>active&&!engine.disposed;
  return Object.freeze({
    accepts(kind){return kind==='gpu.execute';},
    dispatch(call) {
      if(!live())return Promise.reject(new ComputeError('DISPOSED','Adapter generation has ended'));
      if(!call||call.kind!=='gpu.execute')return Promise.reject(new ComputeError('DENIED','Use the central dispatcher for non-GPU kinds'));
      if(!Number.isSafeInteger(call.id)||call.id<1)return Promise.reject(new ComputeError('PROTOCOL','Invalid callback ID'));
      if(seen.has(call.id))return Promise.reject(new ComputeError('DUPLICATE','Duplicate callback ID'));
      if(seen.size>=maxRequests||pending>=maxPending)return Promise.reject(new ComputeError('LIMIT','GPU request allowance exceeded'));
      const id=call.id;
      // The caller is trusted host code. Drained guest args must be plain structured data.
      // Own the request snapshot so queuing cannot observe later caller mutation.
      let args;
      try{args=structuredClone(call.args);}catch(error){return Promise.reject(new ComputeError('PROTOCOL','Arguments are not cloneable data'));}
      seen.add(id);pending++;
      const run=tail.then(async()=>{
        if(!live())return {delivered:false,cancelled:true};
        let reply;
        try{reply={ok:true,value:await handler.handle('gpu.execute',args)};}
        catch(error){reply={ok:false,error:{code:error.code||'GPU',message:String(error.message||error).slice(0,512)}};}
        if(!live())return {delivered:false,cancelled:true};
        // This is after asynchronous host work and outside an active Engine export.
        const delivered=engine.resolveHostCallback(id,reply);
        return {delivered:delivered!==false,cancelled:false};
      }).finally(()=>{pending--;});
      tail=run.catch(()=>{});return run;
    },
    invalidate(){active=false;handler.invalidate();},
    idle(){return tail;},
  });
}
