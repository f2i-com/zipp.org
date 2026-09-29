// Select between two equivalent builds, never trading a smaller raw file for a
// larger Brotli download. The optimizer is pinned by optimize-wasm.sh; semantic
// and host-boundary tests still run on the selected artifact before release.
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const zlib = require('node:zlib');
const [originalPath, candidatePath, ...extra] = process.argv.slice(2);
if (!originalPath || !candidatePath || extra.length || path.resolve(originalPath) === path.resolve(candidatePath)) {
  throw new Error('usage: node select-smaller-wasm.cjs ORIGINAL.wasm CANDIDATE.wasm');
}
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
const accepted = after.raw < before.raw && after.brotli11 <= before.brotli11;
if (accepted) fs.writeFileSync(originalPath, candidate);
console.log(JSON.stringify({ optimizer: 'Binaryen 125 -O1', accepted, before, candidate: after }));
