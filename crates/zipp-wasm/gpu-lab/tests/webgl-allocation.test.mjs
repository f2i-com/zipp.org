// Allocation bookkeeping only; real shader correctness is checked in a browser.
import test from 'node:test';
import assert from 'node:assert/strict';
import {WebGL2Backend} from '../src/backends/webgl2.mjs';

function backend(limit) {
  let created=0;
  const gl={NO_ERROR:0,isContextLost:()=>false,createTexture:()=>({id:++created}),
    bindTexture(){},texParameteri(){},texImage2D(){},getError:()=>0,deleteTexture(){}};
  const b=Object.assign(Object.create(WebGL2Backend.prototype),{gl,lost:false,maxWidth:1024,maxHeight:4096,
    textureBytes:0,peakTextureBytes:0,maxTextureBytes:limit,dispatch(){}});
  return {b,created:()=>created};
}
test('WebGL charges RGBA32F padded texels before allocating',()=>{
  const {b,created}=backend(32767);
  assert.throws(()=>b.alloc(1025),/budget/);
  assert.equal(created(),0);
  b.maxTextureBytes=32768;
  const h=b.alloc(1025);
  assert.equal(b.textureBytes,32768);
  b.free(h);b.free(h);
  assert.equal(b.textureBytes,0);
  assert.equal(b.peakTextureBytes,32768);
});
test('WebGL reduction scratch is charged and released on a budget failure',async()=>{
  const {b}=backend(12*16);
  const input=b.alloc(8);
  await assert.rejects(()=>b.run({op:'sum',size:1},[input]),/budget/);
  assert.equal(b.textureBytes,8*16);
  b.free(input);
  assert.equal(b.textureBytes,0);
});
test('WebGL failed allocation does not consume texture budget',()=>{
  const {b}=backend(1024);
  b.gl.getError=()=>1285;
  assert.throws(()=>b.alloc(8),/allocation error/);
  assert.equal(b.textureBytes,0);
});
