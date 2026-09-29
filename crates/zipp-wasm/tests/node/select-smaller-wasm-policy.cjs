'use strict';
const assert = require('node:assert/strict');
const {chooseCandidate} = require('./select-smaller-wasm.cjs');
const before = {raw:2931051, brotli11:753271};
const wideTag = {raw:2827053, brotli11:753477};
assert.equal(chooseCandidate(before, wideTag), false, 'full builds retain the strict download rule');
assert.equal(chooseCandidate(before, wideTag, true), true, 'Lite saves 104 KB raw for 206 compressed bytes');
assert.equal(chooseCandidate(before, {raw:before.raw-1000, brotli11:before.brotli11+1}, true), false, 'small raw savings do not justify a tradeoff');
assert.equal(chooseCandidate(before, {raw:wideTag.raw, brotli11:before.brotli11+754}, true), false, 'relative download cap applies');
assert.equal(chooseCandidate({raw:5000000,brotli11:2000000}, {raw:4900000,brotli11:2001025}, true), false, 'absolute download cap also applies');
for (const preferRawSize of [false, true]) {
  assert.equal(chooseCandidate(before, {raw:before.raw+1,brotli11:before.brotli11-1000}, preferRawSize), false, 'raw size must shrink');
  assert.equal(chooseCandidate(before, {raw:before.raw-1,brotli11:before.brotli11}, preferRawSize), true, 'no download penalty needs no allowance');
}
console.log('WASM size policy: strict defaults and bounded Lite tradeoffs passed');
