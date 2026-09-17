// GGUF reading is optional.
//
// It is a second WebAssembly module, separate from the engine's and from the
// compute kernels', and a host that only loads Safetensors need not ship it.
// So the question "is it there?" has to be answerable without throwing, and
// the answer has to say what to do about it.
import test from 'node:test';
import assert from 'node:assert/strict';

import {access, readFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';

import {ggufSupport, loadGgufModule, requireGgufModule} from '../src/index.mjs';

test('a host can supply the module itself', async () => {
  const stub = {GgufHeader: function () {}, dequantize: () => {}, supported_dtypes: () => []};
  const support = await ggufSupport({factory: () => stub});
  assert.equal(support.available, true);
  assert.equal(support.module, stub);
  assert.equal(support.source, 'factory');
});

test('something that is not the module is refused by shape, not trusted', async () => {
  await assert.rejects(loadGgufModule(() => ({})), /exposes no GgufHeader/);
  await assert.rejects(loadGgufModule('not a function'), /Supply a factory/);
});

test('a missing module is an answer, not an exception', async () => {
  const support = await ggufSupport({url: new URL('./nowhere/absent.mjs', import.meta.url).href});
  assert.equal(support.available, false);
  // The reason names what was tried, on one line: this string goes in a UI.
  assert.match(support.reason, /^url: /);
  assert.equal(support.reason.includes('\n'), false);
  assert.match(support.remedy, /fetch_gguf_wasm\.sh/);
});

test('a caller that needs it gets a refusal saying how to get one', async () => {
  await assert.rejects(
    requireGgufModule({url: new URL('./nowhere/absent.mjs', import.meta.url).href}),
    /GGUF reading is unavailable .*fetch_gguf_wasm\.sh/s);
});

test('the fetched module is found where the fetch script puts it', async () => {
  // Skips rather than fails when it is not there -- which is the point.
  const support = await ggufSupport();
  if (!support.available) { assert.match(support.remedy, /fetch_gguf_wasm/); return; }
  assert.equal(support.source, 'node module');
  assert.equal(typeof support.module.GgufHeader, 'function');
  assert.equal(typeof support.module.dequantize, 'function');
});

// ---- what is in the tree, and where it came from ----------------------------

test('the GGUF module in the tree is the one the lock file records', async () => {
  // A version number is a name and a name can come to mean different bytes, so
  // scripts/fetch_gguf_wasm.sh writes down the digest of everything it put
  // here. A file that changed without that script running should be a failing
  // test rather than a mystery.
  const lockPath = new URL('../wasm/gguf-wasm.lock.json', import.meta.url);
  if (!await access(lockPath).then(() => true, () => false)) return;  // not fetched
  const lock = JSON.parse(await readFile(lockPath, 'utf8'));

  assert.match(lock.tag, /^v\d+\.\d+\.\d+$/, 'the lock names a release');
  assert.match(lock.commit, /^[0-9a-f]{40}$/, 'and the revision that release was built from');
  assert.ok(Object.keys(lock.files).length > 0, 'and what it put in the tree');

  for (const [name, expected] of Object.entries(lock.files)) {
    const bytes = await readFile(new URL(`../wasm/${name}`, import.meta.url));
    const actual = createHash('sha256').update(bytes).digest('hex');
    assert.equal(actual, expected, `wasm/${name} is not what the lock records`);
  }
});

test('the Rust pin and the WebAssembly pin are the same revision', async () => {
  // A matmul that reads a quantized weight has to decode it exactly as the
  // reader does. They use the same code, which only means anything if they use
  // the same *version* of it -- so the two pins are checked against each other
  // rather than trusted to be moved together.
  const lockPath = new URL('../wasm/gguf-wasm.lock.json', import.meta.url);
  const manifestPath = new URL('../../rust/Cargo.toml', import.meta.url);
  for (const path of [lockPath, manifestPath]) {
    if (!await access(path).then(() => true, () => false)) return;
  }
  const lock = JSON.parse(await readFile(lockPath, 'utf8'));
  const manifest = await readFile(manifestPath, 'utf8');
  const pinned = /rev = "([0-9a-f]{40})"/.exec(manifest);
  assert.ok(pinned, 'rust/Cargo.toml pins gguf-quants by revision');
  assert.equal(pinned[1], lock.commit,
    'rust/Cargo.toml and wasm/gguf-wasm.lock.json name different revisions of gguf-wasm');
});
