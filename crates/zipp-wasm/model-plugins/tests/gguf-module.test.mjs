// GGUF reading is optional.
//
// It is a second WebAssembly module, separate from the engine's and from the
// compute kernels', and a host that only loads Safetensors need not ship it.
// So the question "is it there?" has to be answerable without throwing, and
// the answer has to say what to do about it.
import test from 'node:test';
import assert from 'node:assert/strict';

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
