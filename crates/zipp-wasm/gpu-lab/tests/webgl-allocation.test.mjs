// Allocation bookkeeping only; real shader correctness is checked in a browser.
import test from 'node:test';
import assert from 'node:assert/strict';
import {WebGL2Backend} from '../src/backends/webgl2.mjs';

function backend(limit,{r32f=false}={}) {
  let created=0,deleted=0,errors=0;
  const gl={NO_ERROR:0,R32F:1,RGBA32F:2,isContextLost:()=>false,createTexture:()=>({id:++created}),
    bindTexture(){},texParameteri(){},texStorage2D(){},texSubImage2D(){},getError:()=>{errors++;return 0;},deleteTexture(){deleted++;}};
  const b=Object.assign(Object.create(WebGL2Backend.prototype),{gl,lost:false,maxWidth:1024,maxHeight:4096,r32f,texelBytes:r32f?4:16,
    textureBytes:0,peakTextureBytes:0,maxTextureBytes:limit,pool:new Map(),pooledBytes:0,dispatch(){}});
  return {b,created:()=>created,deleted:()=>deleted,errors:()=>errors};
}
test('WebGL charges RGBA32F padded texels before allocating',()=>{
  const {b,created}=backend(32767);
  assert.throws(()=>b.alloc(1025),/budget/);
  assert.equal(created(),0);
  b.maxTextureBytes=32768;
  const h=b.alloc(1025);
  assert.equal(b.textureBytes,32768);
  b.free(h);b.free(h);
  assert.equal(b.pooledBytes,32768);
  assert.equal(b.peakTextureBytes,32768);
});
test('WebGL R32F textures cost four bytes per texel',()=>{
  const {b}=backend(1024*4,{r32f:true});
  const h=b.alloc(1024);assert.equal(h.bytes,4096);assert.throws(()=>b.alloc(1),/budget.*R32F/);b.free(h);
});
test('WebGL pooled textures are reused without new allocations or driver error checks',()=>{
  const {b,created,errors}=backend(1<<20);
  const first=b.alloc(300);b.free(first);const again=b.alloc(300);
  assert.equal(again.texture,first.texture);assert.equal(created(),1);assert.equal(errors(),1);
  const other=b.alloc(301);assert.notEqual(other.texture,first.texture);assert.equal(created(),2);
});
test('WebGL evicts idle pooled textures before refusing an allocation',()=>{
  const {b,deleted}=backend(8*16);
  const small=b.alloc(8);b.free(small);assert.equal(b.textureBytes,128);
  const big=b.alloc(6);assert.equal(deleted(),1);assert.equal(b.textureBytes,96);assert.equal(b.pooledBytes,0);
  assert.throws(()=>b.alloc(3),/budget/);b.free(big);
});
test('WebGL reduction scratch is charged and released on a budget failure',async()=>{
  const {b}=backend(12*16);
  const input=b.alloc(8);
  await assert.rejects(()=>b.run({op:'sum',whole:true,size:1},[input]),/budget/);
  // The output and any scratch went back to the pool; only the input is still in use.
  assert.equal(b.textureBytes-b.pooledBytes,8*16);
  b.free(input);
  assert.equal(b.textureBytes-b.pooledBytes,0);
});
test('WebGL failed allocation does not consume texture budget',()=>{
  const {b}=backend(1024);
  b.gl.getError=()=>1285;
  assert.throws(()=>b.alloc(8),/allocation error/);
  assert.equal(b.textureBytes,0);
});
