import {check, ComputeError, float32Data, checkClassTargets, adamStep, sizeOf} from './graph.mjs';
const clock=()=>globalThis.performance?.now()??Date.now();

/**
 * A prepared plan with device-resident tensors: `runtime.prepare(program)`
 * validated the program ONCE; every `run` uploads only the inputs it is given,
 * dispatches the cached plan (several training steps back to back where the
 * backend can record them into one command buffer) and reads back only the
 * named outputs. Tensors the program marks `carry` and outputs listed in
 * `resident` stay on the device between runs; `download` fetches them.
 *
 * Handles cross a step boundary in the backend's *persisted* form (identity
 * for buffers, textures and host arrays; a copy out of the WASM arena, which
 * is reset per step) and are reference counted: a static input, a carried
 * input, a resident output and a pending readback each hold one reference,
 * and nothing held is ever returned to a backend pool.
 */
export class Session {
  constructor(runtime,plan,{resident,typedOutputs}) {
    this.runtime=runtime;this.impl=runtime.impl;this.plan=plan;this.resident=resident;this.typedOutputs=typedOutputs;
    this.inputs=plan.nodes.filter(n=>n.op==='input');
    this.carries=this.inputs.filter(n=>n.carry!==undefined);
    this.byName=new Map(plan.outputs.map(o=>[o.name,o]));
    // A carried output is a computed node: copying (WebGPU) or swapping two
    // resident inputs into each other in one step has no single order.
    for(const c of this.carries)check(plan.nodes[plan.root[this.byName.get(c.carry).id]].op!=='input','REFERENCE',`carry ${c.carry} must name a computed output`);
    this.held=new Map();       // node id -> persisted handle of a static or carried input
    this.residents=new Map();  // output name -> persisted handle
    this.retained=new Map();   // persisted handle -> holders
    this.stepNumber=1;this.busy=false;this.disposed=false;this.disposeRequested=false;this.runs=0;
    // Set when a run failed after `begin`: carries and residents may hold any step's values
    // while stepNumber did not advance, so nothing but dispose() may touch them again.
    this.poisoned=false;
    const bytes=new Map();
    for(const n of this.inputs)if(!n.fed||n.carry!==undefined)bytes.set(`in${n.id}`,n.size*4);
    for(const name of resident)bytes.set(`out${name}`,plan.nodes[this.byName.get(name).id].size*4);
    this.residentBytes=[...bytes.values()].reduce((a,b)=>a+b,0);
  }
  get backend(){return this.impl.name;}
  /** What the plan expects and keeps: for a host relaying the session to a guest. */
  describe(){
    return {backend:this.backend,step:this.stepNumber,residentBytes:this.residentBytes,poisoned:this.poisoned,
      inputs:this.inputs.map(n=>({id:n.id,shape:[...n.shape],fed:!!n.fed,...(n.carry!==undefined?{carry:n.carry}:{}),...(n.classes!==undefined?{classes:n.classes}:{})})),
      outputs:this.plan.outputs.map(o=>({name:o.name,shape:[...this.plan.nodes[o.id].shape],resident:this.resident.has(o.name)}))};
  }
  retain(h){this.retained.set(h,(this.retained.get(h)??0)+1);return h;}
  release(h){const n=this.retained.get(h);if(n===undefined)return;if(n>1)this.retained.set(h,n-1);else{this.retained.delete(h);this.impl.free(h);}}
  persist(h){return this.impl.persist?this.impl.persist(h):h;}
  materialize(h){return this.impl.materialize?this.impl.materialize(h):h;}
  /** Uploads the inputs that have data (static and initial carried values) once. */
  async init(){
    let began=false;
    try{
      await this.impl.begin(this.plan);began=true;
      for(const n of this.inputs)if(n.data)this.held.set(n.id,this.retain(this.persist(await this.impl.run(n,[]))));
    }catch(e){this.free();throw e;}
    finally{if(began)await this.impl.finish();}
  }
  /** Validates one run's step inputs before any device work; returns owned float32 copies by node id. */
  uploads(steps){
    // `covered`: inputs that have a value when a step starts (held now, or carried by an earlier step).
    const limits=this.plan.limits,nodes=this.plan.nodes,covered=new Set(this.held.keys());let elements=0;
    return steps.map((step,index)=>{
      check(step!==null&&typeof step==='object'&&!Array.isArray(step),'PROTOCOL',`Step ${index} must be an object`);
      for(const k of Object.keys(step))check(['inputs'].includes(k),'PROTOCOL',`Unknown step field: ${k}`);
      const given=step.inputs??{},fed=new Map();
      check(given!==null&&typeof given==='object'&&!Array.isArray(given),'PROTOCOL','inputs must map node ids to data');
      for(const key of Object.keys(given)){
        const id=Number(key),n=nodes[id];
        check(Number.isSafeInteger(id)&&String(id)===key&&n!==undefined&&n.op==='input','REFERENCE',`Input ${key} is not an input node`);
        const data=given[key];
        check((data instanceof Float32Array||Array.isArray(data))&&data.length===n.size,'SHAPE',`Input ${key} length does not match shape`);
        elements+=data.length;check(elements<=limits.maxInputElements,'LIMIT','Total input exceeds limit');
        const owned=float32Data(data);
        if(n.classes!==undefined)checkClassTargets(owned,n.classes);
        fed.set(id,owned);
      }
      // Every fed input needs a value until a carry has given it one.
      for(const n of this.inputs)if(n.fed&&!fed.has(n.id)&&!covered.has(n.id))
        throw new ComputeError('REFERENCE',`Input ${n.id} has no value: feed it in step ${index}`);
      for(const c of this.carries)covered.add(c.id);
      return fed;
    });
  }
  /**
   * `steps`: one `{inputs}` or a list of them, submitted back to back.
   * `readback`: output names read for every step (default: every output not
   * resident). `step`: the training-step number of the first step (default:
   * continues from the previous run); an `adam_update` node's bias correction
   * follows it, so one prepared step trains a whole run.
   */
  async run(steps,{readback,step}={}){
    check(!this.disposed,'DISPOSED','Session has been disposed');
    check(!this.runtime.disposed,'DISPOSED','Runtime has been disposed');
    check(!this.poisoned,'STATE','Session state is undefined after a failed run; dispose it');
    check(!this.busy&&!this.runtime.busy,'BUSY','Runtime supports one graph at a time; await the previous execution');
    const list=Array.isArray(steps)?steps:[steps],plan=this.plan,limits=plan.limits,root=plan.root,nodes=plan.nodes;
    check(list.length>=1&&list.length<=limits.maxStepsPerRun,'LIMIT',`A run submits between 1 and ${limits.maxStepsPerRun} steps`);
    const first=step===undefined?this.stepNumber:step;
    check(Number.isSafeInteger(first)&&first>=1&&first+list.length-1<=2**31,'NUMBER','step must be a positive integer');
    const names=readback===undefined?plan.outputs.map(o=>o.name).filter(n=>!this.resident.has(n)):readback;
    check(Array.isArray(names),'PROTOCOL','readback must list output names');
    const wanted=new Set();let readbackElements=0;
    for(const name of names){
      const o=this.byName.get(name);check(o!==undefined,'REFERENCE',`readback names no output: ${String(name)}`);
      if(wanted.has(name))continue;wanted.add(name);readbackElements+=nodes[o.id].size*list.length;
    }
    check(readbackElements<=limits.maxOutputElements,'LIMIT','Requested readback exceeds limit');
    const fed=this.uploads(list);
    let uploadElements=0;for(const m of fed)for(const d of m.values())uploadElements+=d.length;
    this.busy=this.runtime.busy=true;
    const start=clock(),pending=[],result={steps:[]};let began=false,error,finishError,submitted;
    // Step-local handles: freed when their last consumer ran, unless held.
    const handles=new Map(),freeLocal=id=>{const h=handles.get(id);if(h!==undefined){if(!this.retained.has(h))this.impl.free(h);handles.delete(id);}};
    try{
      await this.impl.begin(plan);began=true;
      for(let s=0;s<list.length;s++){
        if(s>0)this.impl.nextStep?.();
        const stepNo=first+s,uses=[...plan.uses];handles.clear();
        for(const n of this.inputs){
          const data=fed[s].get(n.id);
          handles.set(n.id,data?await this.impl.run({...n,data},[]):this.materialize(this.held.get(n.id)));
        }
        for(const n of this.inputs)if(uses[n.id]===0)freeLocal(n.id); // nothing reads it
        for(const n of nodes){
          if(n.alias||n.op==='input')continue;
          const node=n.op==='adam_update'&&stepNo!==1?{...n,...adamStep(n.raw,n.step+stepNo-1)}:n;
          handles.set(n.id,await this.impl.run(node,n.refs.map(r=>handles.get(root[r]))));
          for(const r of n.refs){uses[root[r]]--;if(uses[root[r]]===0)freeLocal(root[r]);}
          if(uses[n.id]===0)freeLocal(n.id);
        }
        // Outputs: carried into their inputs, kept resident, queued for readback.
        for(const o of plan.outputs){
          const h=handles.get(root[o.id]);let kept=null;
          for(const c of this.carries)if(c.carry===o.name){
            const old=this.held.get(c.id);
            if(old!==undefined&&this.impl.carry){if(old!==h)this.impl.carry(h,old);kept=old;}
            else{const p=this.retain(this.persist(h));this.held.set(c.id,p);if(old!==undefined)this.release(old);kept=p;}
          }
          if(this.resident.has(o.name)){
            const old=this.residents.get(o.name);
            // With a copy-capable backend the resident buffer stays fixed too.
            if(kept===null&&old!==undefined&&this.impl.carry){if(old!==h)this.impl.carry(h,old);}
            else{const p=kept??this.persist(h);this.retain(p);this.residents.set(o.name,p);if(old!==undefined)this.release(old);}
          }
          if(wanted.has(o.name))pending.push({s,name:o.name,id:o.id,h:this.retain(this.persist(h))});
        }
        for(const o of plan.outputs){const r=root[o.id];uses[r]--;if(uses[r]===0)freeLocal(r);}
        result.steps.push({step:stepNo,outputs:Object.create(null)});
      }
      submitted=clock();
      // One readback for the whole run (one GPU round trip), then release what it held.
      const values=pending.length===0?(this.impl.flush?.(),[]):this.impl.complete?await this.impl.complete(pending.map(p=>p.h)):
        this.impl.readAll?await this.impl.readAll(pending.map(p=>p.h)):await (async()=>{const all=[];for(const p of pending)all.push(await this.impl.read(p.h));return all;})();
      pending.forEach((p,i)=>{
        const v=values[i];
        for(let j=0;j<v.length;j++)if(!Number.isFinite(v[j]))throw new ComputeError('NUMBER','Output contains non-finite values; graph readback requires finite float32');
        result.steps[p.s].outputs[p.name]={shape:[...nodes[p.id].shape],dtype:'float32',data:this.typedOutputs?(v instanceof Float32Array?v:Float32Array.from(v)):Array.from(v)};
      });
    }catch(e){error=e;}
    finally{
      for(const p of pending){try{this.release(p.h);}catch(e){error??=e;}}
      for(const id of [...handles.keys()]){try{freeLocal(id);}catch(e){error??=e;}}
      if(began)try{await this.impl.finish();}catch(e){finishError=e;}
      // Everything up to `begin` only validated; after it, carries and residents advance step by
      // step, so a failure leaves them at no step in particular and a retry would run the wrong
      // Adam step on them. Validation failures leave the session as it was.
      if(began&&(error||finishError))this.poisoned=true;
      this.busy=this.runtime.busy=false;
      if(this.disposeRequested)this.dispose();
    }
    if(error)throw error;if(finishError)throw finishError;
    this.stepNumber=first+list.length;this.runs++;
    const end=clock();
    return {version:1,backend:this.backend,steps:result.steps,outputs:result.steps[result.steps.length-1].outputs,
      stats:{steps:list.length,nodes:nodes.length,estimatedWork:plan.work*list.length,logicalAllocationBytes:plan.logicalBytes,
        residentBytes:this.residentBytes,uploadElements,readbackElements,submitWallMs:submitted-start,readbackWallMs:end-submitted,
        totalWallMs:end-start,...this.impl.allocationStats?.()}};
  }
  /** Reads resident outputs (and carried inputs, by node id) on demand: `{name: {shape, dtype, data}}`. */
  async download(names){
    check(!this.disposed,'DISPOSED','Session has been disposed');
    check(!this.poisoned,'STATE','Session state is undefined after a failed run; dispose it');
    check(!this.busy&&!this.runtime.busy,'BUSY','Runtime supports one graph at a time; await the previous execution');
    check(Array.isArray(names)&&names.length>0,'PROTOCOL','download lists resident output names');
    const picks=names.map(name=>{
      const o=this.byName.get(name),h=this.residents.get(name);
      check(o!==undefined&&this.resident.has(name),'REFERENCE',`${String(name)} is not a resident output`);
      check(h!==undefined,'REFERENCE',`${name} has not been computed yet`);
      return {name,shape:[...this.plan.nodes[o.id].shape],h};
    });
    this.busy=this.runtime.busy=true;
    let began=false,error,values;
    try{
      await this.impl.begin(this.plan);began=true;
      const hs=picks.map(p=>p.h);
      values=this.impl.complete?await this.impl.complete(hs):this.impl.readAll?await this.impl.readAll(hs):await (async()=>{const all=[];for(const h of hs)all.push(await this.impl.read(h));return all;})();
    }catch(e){error=e;}
    finally{if(began)try{await this.impl.finish();}catch(e){error??=e;}this.busy=this.runtime.busy=false;if(this.disposeRequested)this.dispose();}
    if(error)throw error;
    const outputs=Object.create(null);
    picks.forEach((p,i)=>{const v=values[i];outputs[p.name]={shape:p.shape,dtype:'float32',data:this.typedOutputs?(v instanceof Float32Array?v:Float32Array.from(v)):Array.from(v)};});
    return {version:1,backend:this.backend,outputs};
  }
  free(){for(const h of this.retained.keys())this.impl.free(h);this.retained.clear();this.held.clear();this.residents.clear();}
  /** Releases every resident tensor; a run in flight finishes first. */
  dispose(){
    if(this.disposed)return;
    if(this.busy){this.disposeRequested=true;return;}
    this.disposed=true;this.free();this.runtime.sessions.delete(this);
  }
}
