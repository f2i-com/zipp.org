// Graph IR v2: validation, reference numerics, cpu-js vs compiled-WASM
// differentials for every operation, finite-difference gradients, optimizer
// parity with PyTorch's update order, and MLP training.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createRuntime} from '../src/runtime.mjs';
import {validateProgram, broadcast, DEFAULT_LIMITS} from '../src/graph.mjs';
import {erf,cdf} from '../src/kernel-math.mjs';
import {builder, opCases, mlpTrainingStep, seeded} from './ml-cases.mjs';
const wasmBytes=await readFile(new URL('../wasm/kernels.wasm',import.meta.url));
const cpu=await createRuntime({backend:'cpu-js'}),wasm=await createRuntime({backend:'wasm',wasmBytes});
const TRANSCENDENTAL=/exp|log|tanh|sigmoid|gelu|softmax|cross-entropy|odd functions/;
const ulp=x=>{const a=Math.abs(Math.fround(x));if(a===0)return 2**-149;const e=Math.floor(Math.log2(a));return 2**Math.max(e-23,-149);};
const shapeErr=e=>e.code==='SHAPE',limitErr=e=>e.code==='LIMIT',numberErr=e=>e.code==='NUMBER',protoErr=e=>e.code==='PROTOCOL';

// ---- validation ------------------------------------------------------------------------
test('broadcasting follows NumPy right alignment and yields zero strides on expanded axes',()=>{
  assert.deepEqual(broadcast([4,1],[1,5]).shape,[4,5]);
  assert.deepEqual(broadcast([2,1,3],[4,1]).shape,[2,4,3]);
  assert.deepEqual(broadcast([],[2,3]).shape,[2,3]);
  const b=broadcast([4,1],[5]);assert.deepEqual(b.dims,[1,1,4,5]);assert.deepEqual(b.aStrides,[4,4,1,0]);assert.deepEqual(b.bStrides,[5,5,0,1]);
  for(const [x,y] of [[[3],[4]],[[2,3],[3,2]],[[4,1,3],[2,3,3]]])assert.throws(()=>broadcast(x,y),shapeErr);
});
test('versions 2 and 3 are accepted, versions other than 1, 2 and 3 are not',()=>{
  const g=builder();g.input([1,2]);
  assert.equal(validateProgram(g.program({r:0})).nodes.length,1);
  assert.equal(validateProgram({...g.program({r:0}),version:3}).nodes.length,1);
  for(const version of [0,4,'2','3',2.5])assert.throws(()=>validateProgram({...g.program({r:0}),version}),protoErr);
});
test('shapes: rank up to four, holes and overflow-sized products fail with the right code',()=>{
  const full=shape=>({version:2,nodes:[{id:0,op:'full',shape,value:1}],outputs:[{name:'r',id:0}]});
  assert.equal(validateProgram(full([2,3,4,5])).nodes[0].size,120);
  assert.throws(()=>validateProgram(full([1,1,1,1,1])),shapeErr);
  const sparse=[];sparse[1]=5;assert.throws(()=>validateProgram(full(sparse)),shapeErr);
  assert.throws(()=>validateProgram(full([65536,65536,65536,65536])),limitErr);
  assert.throws(()=>validateProgram(full([65537])),shapeErr);
  assert.throws(()=>validateProgram(full([2,-3])),shapeErr);
  const holes={version:2,nodes:[],outputs:[{name:'r',id:0}]};holes.nodes.length=1;assert.throws(()=>validateProgram(holes),protoErr);
  const outs=full([1]);outs.outputs=[];outs.outputs.length=1;assert.throws(()=>validateProgram(outs),protoErr);
});
test('operation fields are validated before any backend work',()=>{
  const bad=[
    g=>g.op('sum',g.input([1,2]),undefined,{axis:1}),
    g=>g.op('sum',g.input([1,2]),undefined,{axis:0.5}),
    g=>g.op('softmax',g.input([1,2,3,4],[2,2]),undefined,{axis:0}),
    g=>g.op('softmax',g.input([1],[])),
    g=>g.node('permute',{a:g.input([1,2,3,4],[2,2]),dims:[0,0]}),
    g=>g.node('permute',{a:g.input([1,2,3,4],[2,2]),dims:[1]}),
    g=>g.node('reshape',{a:g.input([1,2,3,4],[2,2]),shape:[3]}),
    g=>g.op('matmul',g.input([1,2,3,4],[2,2]),g.input([1,2,3],[3,1])),
    g=>g.op('matmul',g.input([1,2],[2]),g.input([1,2],[2,1])),
    g=>g.op('matmul',g.input(Array(8).fill(1),[2,2,2]),g.input(Array(12).fill(1),[3,2,2])),
    g=>g.op('transpose',g.input([1,2,3,4,5,6,7,8],[2,2,2])),
    g=>g.op('add',g.input([1,2]),g.input([1,2,3])),
  ];
  for(const build of bad){const g=builder();build(g);assert.throws(()=>validateProgram(g.program({r:g.nodes.length-1})),shapeErr);}
  const numeric=[
    g=>g.op('cross_entropy',g.input([1,2,3,4],[2,2]),g.input([0,2])),
    g=>g.op('cross_entropy',g.input([1,2,3,4],[2,2]),g.input([0,0.5])),
    g=>g.op('cross_entropy_grad',g.input([1,2,3,4],[2,2]),g.op('add',g.input([0,1]),g.full([],0))),
    g=>g.op('adam_m',g.input([1]),g.input([1]),{beta1:1}),
    g=>g.op('sgd_update',g.input([1]),g.input([1]),{lr:Infinity}),
    g=>g.node('adam_update',{a:g.input([1]),b:g.input([1]),c:g.input([1]),lr:0.1,beta1:0.9,beta2:0.999,eps:1e-8,step:0}),
    g=>g.node('adam_update',{a:g.input([1]),b:g.input([1]),c:g.input([1]),lr:0.1,beta1:0.9,beta2:0.999,eps:-1,step:1}),
  ];
  for(const build of numeric){const g=builder();build(g);assert.throws(()=>validateProgram(g.program({r:g.nodes.length-1})),numberErr);}
  const g=builder();g.op('sgd_update',g.input([1,2]),g.input([1,2]));assert.throws(()=>validateProgram(g.program({r:2})),protoErr);
  const k=builder();k.op('mean',k.input([1,2]),undefined,{keepdim:1});assert.throws(()=>validateProgram(k.program({r:1})),protoErr);
  const h=builder();h.op('sgd_update',h.input([1,2]),h.input([1]),{lr:1});assert.throws(()=>validateProgram(h.program({r:2})),shapeErr);
});
test('defaults accept an MNIST-scale MLP training step (784-256-10, batch 64, Adam)',()=>{
  const {program}=mlpTrainingStep();const plan=validateProgram(program);
  assert.ok(plan.work<DEFAULT_LIMITS.maxWork,`${plan.work}`);assert.ok(plan.work>5e7);
  assert.equal(program.outputs.length,13);
  assert.throws(()=>validateProgram(program,{maxWork:50000000}),limitErr);
});

// ---- reference numerics ----------------------------------------------------------------
function erfPrecise(x){ // double-precision series / continued fraction
  const a=Math.abs(x);let r;
  if(a<3){let term=a,sum=a;for(let n=1;n<200;n++){term*=-a*a/n;const add=term/(2*n+1);sum+=add;if(Math.abs(add)<1e-17)break;}r=sum*2/Math.sqrt(Math.PI);}
  else{let f=0;for(let n=60;n>=1;n--)f=n/2/(a+f);r=1-Math.exp(-a*a)/Math.sqrt(Math.PI)/(a+f);}
  return x<0?-r:r;
}
test('the shared erf approximation stays within its documented 1.2e-7 fractional bound',()=>{
  let worst=0;
  for(let x=-6;x<=6;x+=0.001){const e=erfPrecise(x),a=erf(x);worst=Math.max(worst,Math.abs(a-e)/Math.max(Math.abs(e),1e-30));}
  assert.ok(worst<1.3e-7,`${worst}`);
  assert.equal(erf(0),0);assert.ok(Object.is(erf(-0),-0));assert.ok(Number.isNaN(erf(NaN)));
  // The normal CDF keeps relative precision in its lower tail (no 1 + erf cancellation).
  const cdfPrecise=x=>{const z=-x/Math.SQRT2;if(z<3)return 0.5*(1+erfPrecise(x/Math.SQRT2));let f=0;for(let n=60;n>=1;n--)f=n/2/(z+f);return 0.5*Math.exp(-z*z)/Math.sqrt(Math.PI)/(z+f);};
  let tail=0;for(let x=-14;x<0;x+=0.01){const r=cdfPrecise(x);tail=Math.max(tail,Math.abs(cdf(x)-r)/r);}
  assert.ok(tail<2e-6,`${tail}`);assert.equal(cdf(-15),0);assert.equal(cdf(15),1);assert.ok(Number.isNaN(cdf(NaN)));
});
test('cpu-js reference matches an independent double-precision evaluation',async()=>{
  const g=builder(),rnd=seeded(3),xs=Array.from({length:64},()=>Math.fround(rnd(-5,5))),x=g.input(xs);
  const outs={};for(const op of ['exp','tanh','sigmoid','gelu','gelu_grad','neg'])outs[op]=g.op(op,x);
  const pos=g.input(xs.map(v=>Math.abs(v)+0.01));outs.log=g.op('log',pos);outs.sqrt=g.op('sqrt',pos);
  const r=(await cpu.execute(g.program(outs))).outputs;
  const phi=v=>0.5*(1+erfPrecise(v/Math.SQRT2)),want={exp:Math.exp,tanh:Math.tanh,sigmoid:v=>1/(1+Math.exp(-v)),gelu:v=>v*phi(v),
    gelu_grad:v=>phi(v)+v*Math.exp(-v*v/2)/Math.sqrt(2*Math.PI),neg:v=>-v};
  // The erf fit's error (<1.2e-7 of the CDF) is absolute for GELU terms, which cancel near gelu_grad's zero.
  for(const [op,fn] of Object.entries(want))r[op].data.forEach((v,i)=>assert.ok(Math.abs(v-fn(xs[i]))<=4*ulp(fn(xs[i]))+2e-7*Math.abs(fn(xs[i]))+(op.startsWith('gelu')?1.2e-7*(1+Math.abs(xs[i])):1e-12),`${op}(${xs[i]}) ${v}`));
  r.log.data.forEach((v,i)=>assert.ok(Math.abs(v-Math.log(Math.abs(xs[i])+0.01))<=2*ulp(v)+1e-7));
  const s=[[1,2,3],[1000,1001,1002],[-50,0,50]];
  const h=builder(),m=h.input(s.flat(),[3,3]),sm=(await cpu.execute(h.program({s:h.op('softmax',m),l:h.op('log_softmax',m)}))).outputs;
  s.forEach((row,i)=>{const mx=Math.max(...row),z=row.reduce((t,v)=>t+Math.exp(v-mx),0);
    row.forEach((v,j)=>{assert.ok(Math.abs(sm.s.data[i*3+j]-Math.exp(v-mx)/z)<1e-6);assert.ok(Math.abs(sm.l.data[i*3+j]-(v-mx-Math.log(z)))<1e-5);});});
});
test('reductions, broadcasting, permutations and batched matmul match naive loops',async()=>{
  const rnd=seeded(11),A=Array.from({length:2*3*4},()=>rnd()),B=Array.from({length:2*4*5},()=>rnd()),W=Array.from({length:4*5},()=>rnd());
  const g=builder(),a=g.input(A,[2,3,4]),b=g.input(B,[2,4,5]),w=g.input(W,[4,5]);
  const r=(await cpu.execute(g.program({bmm:g.op('matmul',a,b),bw:g.op('matmul',a,w),s1:g.op('sum',a,undefined,{axis:1}),
    m2:g.op('mean',a,undefined,{axis:-1,keepdim:true}),p:g.node('permute',{a,dims:[2,0,1]}),bc:g.op('sub',a,g.input([1,2,3],[3,1]))}))).outputs;
  const at=(i,j,k)=>A[(i*3+j)*4+k];
  for(let i=0;i<2;i++)for(let j=0;j<3;j++)for(let c=0;c<5;c++){
    let s=0,t=0;for(let k=0;k<4;k++){s+=at(i,j,k)*B[(i*4+k)*5+c];t+=at(i,j,k)*W[k*5+c];}
    assert.ok(Math.abs(r.bmm.data[(i*3+j)*5+c]-s)<1e-5);assert.ok(Math.abs(r.bw.data[(i*3+j)*5+c]-t)<1e-5);
  }
  assert.deepEqual(r.bmm.shape,[2,3,5]);assert.deepEqual(r.s1.shape,[2,4]);assert.deepEqual(r.m2.shape,[2,3,1]);assert.deepEqual(r.p.shape,[4,2,3]);
  for(let i=0;i<2;i++)for(let k=0;k<4;k++)assert.ok(Math.abs(r.s1.data[i*4+k]-(at(i,0,k)+at(i,1,k)+at(i,2,k)))<1e-5);
  for(let i=0;i<2;i++)for(let j=0;j<3;j++){let s=0;for(let k=0;k<4;k++)s+=at(i,j,k);assert.ok(Math.abs(r.m2.data[i*3+j]-s/4)<1e-6);}
  for(let k=0;k<4;k++)for(let i=0;i<2;i++)for(let j=0;j<3;j++){
    assert.equal(r.p.data[(k*2+i)*3+j],Math.fround(at(i,j,k)));
    assert.equal(r.bc.data[(i*3+j)*4+k],Math.fround(Math.fround(at(i,j,k))-(j+1)));
  }
});

// ---- cpu-js versus compiled WASM, every operation ---------------------------------------
for(const [name,program] of opCases())test(`cpu-js and WASM agree: ${name}`,async()=>{
  const x=await cpu.execute(program),y=await wasm.execute(program),exact=!TRANSCENDENTAL.test(name);
  for(const out of Object.keys(x.outputs)){
    const a=x.outputs[out],b=y.outputs[out];assert.deepEqual(b.shape,a.shape);
    a.data.forEach((v,i)=>{
      if(exact)assert.equal(b.data[i],v,`${out}[${i}]`);
      else assert.ok(Math.abs(b.data[i]-v)<=2*ulp(v)+1e-7*Math.abs(v)||Math.abs(b.data[i]-v)<=1e-6,`${out}[${i}]: ${b.data[i]} vs ${v}`);
    });
  }
});

// ---- non-finite intermediates (R248) ------------------------------------------------------
test('NaN produced inside a graph reaches the finite-readback check on every Node backend',async()=>{
  for(const op of ['relu','sum','tanh','gelu','softmax']){
    const g=builder(),x=g.input([3e38,1]),big=g.op('mul',x,g.full([],10)),nan=g.op('sub',big,big);
    const program=g.program({r:g.op(op,nan)});
    for(const rt of [cpu,wasm])await assert.rejects(()=>rt.execute(program),numberErr,`${rt.backend} ${op}`);
  }
  const g=builder(),x=g.input([3e38,-3e38]),inf=g.op('mul',x,g.full([],10));
  const program=g.program({mask:g.op('positive',g.op('sub',inf,inf)),finite:g.op('positive',inf)});
  for(const rt of [cpu,wasm])assert.deepEqual((await rt.execute(program)).outputs.mask.data,[0,0]);
  const d=builder(),z=d.op('div',d.input([1,0]),d.input([0,0]));
  for(const rt of [cpu,wasm])await assert.rejects(()=>rt.execute(d.program({r:z})),numberErr);
});

// ---- gradients by finite differences ------------------------------------------------------
function forwardLoss(xs,shape,targets,activation,params,sizes){
  const g=builder(),x=g.input(xs,shape);let h=x;
  for(let l=0;l<sizes.length-1;l++){
    const z=g.op('add',g.op('matmul',h,g.input(params[2*l],[sizes[l],sizes[l+1]])),g.input(params[2*l+1],[sizes[l+1]]));
    h=l<sizes.length-2?g.op(activation,z):z;
  }
  return g.program({loss:g.op('cross_entropy',h,g.input(targets))});
}
for(const activation of ['gelu','tanh','sigmoid'])test(`MLP backward graph matches finite differences (${activation})`,async()=>{
  const sizes=[5,6,3],batch=4,rnd=seeded(21),xs=Array.from({length:batch*5},()=>rnd()),targets=[0,2,1,2];
  const params=[];for(let l=0;l<2;l++){params.push(Array.from({length:sizes[l]*sizes[l+1]},()=>rnd(-1,1)));params.push(Array.from({length:sizes[l+1]},()=>rnd(-0.5,0.5)));}
  // lr = 1 turns each SGD update into p - grad, recovering the backward graph's gradient.
  const {program}=mlpTrainingStep({sizes,batch,optimizer:'sgd',lr:1,activation,params,x:xs,targets});
  for(const rt of [cpu,wasm]){
    const out=(await rt.execute(program)).outputs;
    for(let p=0;p<params.length;p++)for(let i=0;i<params[p].length;i++){
      const grad=Math.fround(params[p][i])-out[`p${p}`].data[i],h=1e-2,probe=delta=>{const q=params.map(v=>[...v]);q[p][i]+=delta;return forwardLoss(xs,[batch,5],targets,activation,q,sizes);};
      const fd=((await cpu.execute(probe(h))).outputs.loss.data[0]-(await cpu.execute(probe(-h))).outputs.loss.data[0])/(2*h);
      assert.ok(Math.abs(grad-fd)<=2e-4+2e-2*Math.abs(fd),`${rt.backend} ${activation} p${p}[${i}]: ${grad} vs ${fd}`);
    }
  }
});
test('cross-entropy, softmax and GELU gradient ops match double-precision finite differences',async()=>{
  const rnd=seeded(8),rows=3,cols=5,L=Array.from({length:rows*cols},()=>rnd(-3,3)),T=[4,0,2];
  const ce=l=>{let s=0;for(let r=0;r<rows;r++){const row=l.slice(r*cols,r*cols+cols),m=Math.max(...row),z=row.reduce((t,v)=>t+Math.exp(v-m),0);s+=Math.log(z)+m-row[T[r]];}return s/rows;};
  const g=builder(),logits=g.input(L,[rows,cols]),t=g.input(T),xs=[-4,-1.5,-0.3,0,0.2,0.7,2.5,5],x=g.input(xs);
  const program=g.program({grad:g.op('cross_entropy_grad',logits,t),loss:g.op('cross_entropy',logits,t),gg:g.op('gelu_grad',x)});
  const phi=v=>0.5*(1+erfPrecise(v/Math.SQRT2));
  for(const rt of [cpu,wasm]){
    const out=(await rt.execute(program)).outputs;
    assert.ok(Math.abs(out.loss.data[0]-ce(L.map(Math.fround)))<1e-6);
    L.forEach((_,i)=>{const h=1e-5,p=[...L],m=[...L];p[i]+=h;m[i]-=h;assert.ok(Math.abs(out.grad.data[i]-(ce(p)-ce(m))/(2*h))<2e-6,`${rt.backend} ce ${i}`);});
    xs.forEach((v,i)=>{const h=1e-5,fd=((v+h)*phi(v+h)-(v-h)*phi(v-h))/(2*h);assert.ok(Math.abs(out.gg.data[i]-fd)<1e-6,`${rt.backend} gelu' ${v}`);});
  }
});

// ---- optimizers --------------------------------------------------------------------------
function torchLikeStep(kind,p,g,state,step){ // float32 transcription of torch.optim's single-tensor loops
  const f=Math.fround;
  if(kind==='momentum'){const buf=state.buf?state.buf.map((b,i)=>f(f(f(0.9)*b)+f(f(1-0.1)*g[i]))):g.slice();state.buf=buf;return p.map((v,i)=>f(v-f(f(0.05)*buf[i])));}
  const w=f(1-0.9),b2=f(0.999),m=(state.m??p.map(()=>0)).map((v,i)=>f(v+f(w*f(g[i]-v)))),v=(state.v??p.map(()=>0)).map((x,i)=>f(f(x*b2)+f(f(f(1-0.999)*g[i])*g[i])));
  state.m=m;state.v=v;const stepSize=f(0.01/(1-Math.pow(0.9,step))),bc=f(Math.sqrt(1-Math.pow(0.999,step)));
  return p.map((x,i)=>f(x-f(stepSize*f(m[i]/f(f(f(Math.sqrt(v[i]))/bc)+f(1e-8))))));
}
for(const kind of ['momentum','adam'])test(`${kind} update nodes reproduce PyTorch's update order over five steps`,async()=>{
  const rnd=seeded(4);let p=Array.from({length:9},()=>Math.fround(rnd())),mine=p.slice(),st={};
  let state=null;
  for(let step=1;step<=5;step++){
    const grad=Array.from({length:9},()=>Math.fround(rnd()));
    const g=builder(),P=g.input(mine,[3,3]),G=g.input(grad,[3,3]);let outs;
    if(kind==='momentum'){const next=state?g.op('momentum_update',g.input(state.buf,[3,3]),G,{momentum:0.9,dampening:0.1}):G;outs={p:g.op('sgd_update',P,next,{lr:0.05}),buf:next};}
    else{const m=g.op('adam_m',state?g.input(state.m,[3,3]):g.full([3,3],0),G,{beta1:0.9}),v=g.op('adam_v',state?g.input(state.v,[3,3]):g.full([3,3],0),G,{beta2:0.999});
      outs={p:g.node('adam_update',{a:P,b:m,c:v,lr:0.01,beta1:0.9,beta2:0.999,eps:1e-8,step}),m,v};}
    const program=g.program(outs),x=(await cpu.execute(program)).outputs,y=(await wasm.execute(program)).outputs;
    for(const k of Object.keys(x))assert.deepEqual(y[k].data,x[k].data,`${kind} step ${step} ${k}`);
    p=torchLikeStep(kind,p,grad,st,step);assert.deepEqual(x.p.data,p,`${kind} step ${step}`);
    mine=x.p.data;state=kind==='momentum'?{buf:x.buf.data}:{m:x.m.data,v:x.v.data};
  }
});

// ---- training ------------------------------------------------------------------------------
test('an MLP classifier trains on both Node backends in single-graph steps, in lockstep',async()=>{
  const sizes=[8,16,3],batch=24,rnd=seeded(77),xs=[],targets=[];
  for(let i=0;i<batch;i++){const c=i%3;targets.push(c);for(let j=0;j<8;j++)xs.push(rnd(-0.3,0.3)+(j%3===c?1:0));}
  let params=null,state=null,losses={};
  for(const rt of [cpu,wasm]){
    params=null;state=null;losses[rt.backend]=[];
    for(let step=1;step<=30;step++){
      const {program,parameters}=mlpTrainingStep({sizes,batch,optimizer:'adam',lr:0.02,step,params,state,x:xs,targets,seed:9});
      const out=(await rt.execute(program)).outputs;losses[rt.backend].push(out.loss.data[0]);
      params=Array.from({length:parameters},(_,i)=>out[`p${i}`].data);state=params.map((_,i)=>[out[`m${i}`].data,out[`v${i}`].data]);
    }
  }
  const l=losses['cpu-js'];assert.ok(l[29]<l[0]*0.2,JSON.stringify(l));
  losses.wasm.forEach((v,i)=>assert.ok(Math.abs(v-l[i])<=1e-5*Math.abs(l[i])+1e-6,`step ${i}: ${v} vs ${l[i]}`));
});
test('an MNIST-sized training step runs on both Node backends and agrees',async()=>{
  const {program}=mlpTrainingStep();
  const x=await cpu.execute(program),y=await wasm.execute(program);
  assert.ok(Math.abs(x.outputs.loss.data[0]-y.outputs.loss.data[0])<1e-5);
  for(const k of Object.keys(x.outputs)){let worst=0;x.outputs[k].data.forEach((v,i)=>{worst=Math.max(worst,Math.abs(v-y.outputs[k].data[i]));});assert.ok(worst<1e-5,`${k} ${worst}`);}
});

// ---- storage aliasing --------------------------------------------------------------------
test('reshape shares storage without freeing it early or late',async()=>{
  let freed=0;const base=await createRuntime({backend:'cpu-js'});const impl=base.impl,free=impl.free.bind(impl);impl.free=h=>{freed++;free(h);};
  const g=builder(),a=g.input([1,2,3,4,5,6],[2,3]),r=g.node('reshape',{a,shape:[3,2]}),unused=g.node('reshape',{a,shape:[6]});
  const s=g.op('sum',r,undefined,{axis:0}),t=g.node('reshape',{a:s,shape:[1,2]});
  const out=(await base.execute(g.program({t,r}))).outputs;
  assert.deepEqual(out.t.data,[9,12]);assert.deepEqual(out.t.shape,[1,2]);assert.deepEqual(out.r.data,[1,2,3,4,5,6]);assert.deepEqual(out.r.shape,[3,2]);
  assert.equal(freed,2);assert.ok(unused>0);base.dispose();
});

// ---- randomized differential, cpu-js versus compiled WASM --------------------------------
// The fixed cases above pin the shapes that are easy to get wrong; this walks a
// fixed seed through shape space instead, so an operation that only misbehaves at
// some rank, broadcast pattern, axis or batch combination still has to show up.
test('a seeded walk over every operation family agrees between cpu-js and WASM',async()=>{
  const rnd=seeded(20260916),pick=a=>a[Math.floor(rnd(0,a.length))],dim=()=>pick([1,1,2,3,4,5,7,8,16,17]);
  const shape=r=>Array.from({length:r},dim),size=s=>s.reduce((a,b)=>a*b,1),rank=(lo,hi)=>lo+Math.floor(rnd(0,hi-lo+1));
  // A partner shape for broadcasting: fewer axes, some of them collapsed to one.
  const partner=s=>s.slice(rank(0,Math.min(3,s.length))).map(d=>rnd(0,1)<0.35?1:d);
  const families={
    binary:g=>{const op=pick(['add','sub','mul','div']),s=shape(rank(0,4)),t=partner(s);
      return[`${op} [${s}] [${t}]`,g.op(op,g.random(s,rnd),g.random(t,rnd,op==='div'?0.7:-3,3))];},
    unary:g=>{const op=pick(['relu','positive','neg','exp','log','sqrt','tanh','sigmoid','gelu','gelu_grad']),s=shape(rank(1,3));
      return[`${op} [${s}]`,g.op(op,g.random(s,rnd,op==='log'||op==='sqrt'?0.05:-5,5))];},
    reduce:g=>{const op=pick(['sum','mean']),s=shape(rank(1,4)),whole=rnd(0,1)<0.3,keepdim=rnd(0,1)<0.5;
      const axis=Math.floor(rnd(0,s.length))-(rnd(0,1)<0.3?s.length:0),f=keepdim?{keepdim:true}:{};
      return[`${op} [${s}] ${whole?'whole':'axis '+axis} keepdim=${keepdim}`,
        g.op(op,g.random(s,rnd),undefined,whole?f:{axis,...f})];},
    rows:g=>{const op=pick(['softmax','log_softmax']),s=shape(rank(1,3));
      return[`${op} [${s}]`,g.op(op,g.random(s,rnd,-8,8))];},
    matmul:g=>{const B=dim(),M=dim(),K=dim(),N=dim(),batched=rnd(0,1)<0.5;
      const sa=batched&&rnd(0,1)<0.8?[B,M,K]:[M,K],sb=batched&&rnd(0,1)<0.8?[B,K,N]:[K,N];
      if(sa.length===3&&sb.length===3)sb[0]=sa[0];
      return[`matmul [${sa}] [${sb}]`,g.op('matmul',g.random(sa,rnd),g.random(sb,rnd))];},
    permute:g=>{const s=shape(rank(1,4)),dims=[...s.keys()];
      for(let i=dims.length-1;i>0;i--){const j=Math.floor(rnd(0,i+1));[dims[i],dims[j]]=[dims[j],dims[i]];}
      return[`permute [${s}] ${dims}`,g.node('permute',{a:g.random(s,rnd),dims})];},
    reshape:g=>{const s=shape(rank(1,3)),a=g.random(s,rnd);
      return[`reshape [${s}]`,g.op('mul',g.node('reshape',{a,shape:[size(s)]}),g.full([],2))];},
    loss:g=>{const rows=dim(),cols=1+Math.floor(rnd(0,12)),op=pick(['cross_entropy','cross_entropy_grad']);
      const targets=g.input(Array.from({length:rows},()=>Math.floor(rnd(0,cols))),[rows]);
      return[`${op} ${rows}x${cols}`,g.op(op,g.random([rows,cols],rnd,-6,6),targets)];},
    optimizer:g=>{const op=pick(['sgd_update','momentum_update','adam_m','adam_v','adam_update']),s=shape(rank(1,3));
      const fields={sgd_update:{lr:0.01},momentum_update:{momentum:0.9,dampening:0.1},adam_m:{beta1:0.9},adam_v:{beta2:0.999},
        adam_update:{lr:0.001,beta1:0.9,beta2:0.999,eps:1e-8,step:1+Math.floor(rnd(0,50))}}[op];
      const a=g.random(s,rnd),b=g.random(s,rnd),c=g.random(s,rnd,0.01,3);
      return[`${op} [${s}]`,g.node(op,op==='adam_update'?{a,b,c,...fields}:{a,b,...fields})];},
  };
  const names=Object.keys(families),counts={};let worst=0,worstCase='',ran=0;
  for(let i=0;i<600;i++){
    const family=names[i%names.length],g=builder();
    const [name,id]=families[family](g),program=g.program({r:id});
    counts[family]=(counts[family]??0)+1;
    const x=await cpu.execute(program),y=await wasm.execute(program);ran++;
    assert.deepEqual(y.outputs.r.shape,x.outputs.r.shape,name);
    x.outputs.r.data.forEach((v,j)=>{
      const delta=Math.abs(y.outputs.r.data[j]-v);
      if(delta>worst){worst=delta;worstCase=`${name} @${j}: ${y.outputs.r.data[j]} vs ${v}`;}
    });
  }
  assert.equal(ran,600);
  for(const family of names)assert.ok(counts[family]>=60,`${family} ran ${counts[family]} times`);
  // Only the transcendental paths may differ at all, and then by about one ulp.
  assert.ok(worst<=1e-6,`worst divergence ${worst} at ${worstCase}`);
});

// -1 names the only axis a scalar has, as it does for any other rank.
test('reducing a scalar over axis -1 is the scalar',async()=>{
  const rt=await createRuntime({backend:'cpu-js'});
  const out=(await rt.execute({version:2,nodes:[{id:0,op:'input',shape:[],data:[5]},
    {id:1,op:'sum',a:0,axis:-1},{id:2,op:'mean',a:0,axis:-1}],outputs:[{name:'s',id:1},{name:'m',id:2}]})).outputs;
  assert.deepEqual([out.s.data[0],out.m.data[0]],[5,5]);
  rt.dispose();
});
