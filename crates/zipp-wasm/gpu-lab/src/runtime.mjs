import {validateProgram, check, ComputeError} from './graph.mjs';
import {CPUBackend} from './backends/cpu.mjs';
import {WasmBackend} from './backends/wasm.mjs';
import {WebGPUBackend} from './backends/webgpu.mjs';
import {WebGL2Backend} from './backends/webgl2.mjs';
const clock=()=>globalThis.performance?.now()??Date.now();

export async function createRuntime({backend='auto', limits={}, wasmBytes, wasmUrl}={}) {
  check(['auto','webgpu','webgl2','wasm','cpu-js'].includes(backend),'BACKEND','Unknown backend');
  const candidates=backend==='auto'?['webgpu','webgl2','wasm','cpu-js']:[backend],attempts=[];
  for(const name of candidates){
    try {
      const implementation=name==='webgpu'?await WebGPUBackend.create():name==='webgl2'?await WebGL2Backend.create():
        name==='wasm'?await WasmBackend.create({wasmBytes,wasmUrl}):new CPUBackend();
      return new ComputeRuntime(implementation,limits,attempts);
    }catch(error){attempts.push({backend:name,error:String(error.message||error)});}
  }
  throw new ComputeError('UNAVAILABLE',`Requested backend unavailable: ${attempts.map(a=>`${a.backend}: ${a.error}`).join('; ')}`);
}

export class ComputeRuntime {
  constructor(backend,limits={},attempts=[]){this.impl=backend;this.limits=limits;this.attempts=attempts;this.busy=false;this.disposed=false;}
  get backend(){return this.impl.name;}
  info(){return {backend:this.backend,description:this.impl.description,adapter:this.impl.info??null,fallbackAttempts:this.attempts};}
  // `typedOutputs`: each output's `data` is a Float32Array of its own rather
  // than a list of numbers (a ZIPP engine takes it as tensor storage).
  async execute(program,{typedOutputs=false}={}){
    check(!this.disposed,'DISPOSED','Runtime has been disposed');
    check(!this.busy,'BUSY','Runtime supports one graph at a time; await the previous execution');
    // Validation and owned input copies happen before any asynchronous work or GPU allocation.
    const plan=validateProgram(program,this.limits);this.busy=true;
    const start=clock(),handles=new Map(),uses=[...plan.uses];
    let value,error,began=false,finishError;
    const free=id=>{const h=handles.get(id);if(h!==undefined){this.impl.free(h);handles.delete(id);}};
    try {
      await this.impl.begin(plan);began=true;
      for(const n of plan.nodes){
        const h=await this.impl.run(n,n.refs.map(r=>handles.get(r)));handles.set(n.id,h);
        for(const r of n.refs){uses[r]--;if(uses[r]===0)free(r);}
        if(uses[n.id]===0)free(n.id);
      }
      const submitted=clock(),outputs=Object.create(null),cache=new Map();
      for(const o of plan.outputs){
        let data=cache.get(o.id);
        if(!data){const read=await this.impl.read(handles.get(o.id));data=!typedOutputs?Array.from(read):read instanceof Float32Array?read:Float32Array.from(read);cache.set(o.id,data);}
        check(data.every(Number.isFinite),'NUMBER','Output contains non-finite values; graph v1 readback requires finite float32');
        outputs[o.name]={shape:[...plan.nodes[o.id].shape],dtype:'float32',data:typedOutputs?data.slice():[...data]};
      }
      value={version:1,backend:this.backend,outputs,stats:{nodes:plan.nodes.length,estimatedWork:plan.work,
        logicalAllocationBytes:plan.logicalBytes,uploadElements:plan.inputElements,
        readbackElements:plan.outputElements,submitWallMs:submitted-start,
        readbackWallMs:clock()-submitted,totalWallMs:0,...this.impl.allocationStats?.()}};
    }catch(e){error=e;}
    finally {
      for(const id of [...handles.keys()]){try{free(id);}catch(e){error??=e;}}
      if(began)try{await this.impl.finish();}catch(e){finishError=e;}
      this.busy=false;
    }
    if(error)throw error;if(finishError)throw finishError;
    value.stats.totalWallMs=clock()-start;
    return value;
  }
  dispose(){check(!this.busy,'BUSY','Await execution before disposing');if(!this.disposed){this.disposed=true;this.impl.dispose();}}
}
