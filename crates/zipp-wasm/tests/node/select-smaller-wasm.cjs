// Full builds require raw savings without Brotli growth. Lite can allow
// a tiny Brotli increase for a substantial raw saving. Semantic and boundary
// tests still run on the selected artifact before release.
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const zlib = require('node:zlib');
function chooseCandidate(before, after, preferRawSize = false) {
  const saved = before.raw - after.raw;
  if (saved <= 0) return false;
  const growth = after.brotli11 - before.brotli11;
  return growth <= 0 || (preferRawSize && saved >= 65536 &&
    growth <= Math.min(1024, Math.floor(before.brotli11 / 1000)));
}
module.exports = {chooseCandidate};

if (require.main === module) {
  const [originalPath, candidatePath, ...extra] = process.argv.slice(2);
  if (!originalPath || !candidatePath || extra.length > 1 ||
      (extra.length && extra[0] !== '--prefer-raw-size') ||
      path.resolve(originalPath) === path.resolve(candidatePath)) {
    throw new Error('usage: node select-smaller-wasm.cjs ORIGINAL.wasm CANDIDATE.wasm [--prefer-raw-size]');
  }
  const preferRawSize = extra.length === 1;
  const original = fs.readFileSync(originalPath);
  const candidate = fs.readFileSync(candidatePath);
  const modules = [original, candidate].map(bytes => new WebAssembly.Module(bytes));
  for (const surface of ['imports', 'exports']) {
    const describe = module => WebAssembly.Module[surface](module).map(item => JSON.stringify(item)).sort();
    assert.deepEqual(describe(modules[1]), describe(modules[0]), `optimizer changed ${surface}`);
  }
  const size = bytes => ({ raw: bytes.length, brotli11: zlib.brotliCompressSync(bytes, {
    params: { [zlib.constants.BROTLI_PARAM_QUALITY]: 11 },
  }).length });
  const before = size(original);
  const after = size(candidate);
  const accepted = chooseCandidate(before, after, preferRawSize);
  if (accepted) fs.writeFileSync(originalPath, candidate);
  console.log(JSON.stringify({ optimizer: 'Binaryen 125 -O1', preferRawSize, accepted, before, candidate: after }));
}
