// Reading a GGUF checkpoint, including a quantized one.
//
// The reader and the block dequantizers are the `gguf` and `ggml-quants` crates
// compiled to WebAssembly (see interop/GGUF.md). This checks the host side: that
// the byte ranges land on the tensors they claim, that shapes arrive in this
// package's order rather than GGUF's, and that a row of a quantized table can
// be read without decoding the table.
//
// Neither a checkpoint nor the module is in this repository, so the test skips
// without them:
//
//     ZIPP_GGUF_WASM=<path to gguf-wasm/pkg-node> ZIPP_GGUF_MODEL=<file.gguf> node --test
import test from 'node:test';
import assert from 'node:assert/strict';
import {open, stat, access} from 'node:fs/promises';
import {createRequire} from 'node:module';

import {openGGUF, WeightStore, resolveLimits, bindGraph} from '../src/index.mjs';

const modelPath = process.env.ZIPP_GGUF_MODEL;
const wasmPath = process.env.ZIPP_GGUF_WASM;
const exists = async path => { try { await access(path); return true; } catch { return false; } };
const ready = Boolean(modelPath && wasmPath) && await exists(modelPath) && await exists(wasmPath);
const reason = 'Set ZIPP_GGUF_WASM and ZIPP_GGUF_MODEL (see the file header)';

/** A checkpoint is gigabytes, so it is read the way a browser reads a File:
 * by range, never whole. */
async function fileSource(path) {
  const size = (await stat(path)).size;
  const handle = await open(path);
  return {
    size: () => size,
    async read(_name, offset, length) {
      const out = new Uint8Array(length);
      const {bytesRead} = await handle.read(out, 0, length, offset);
      assert.equal(bytesRead, length, 'short read');
      return out;
    },
    close: () => handle.close(),
  };
}

// A real checkpoint exceeds every default here, deliberately: the defaults are
// sized for a model a page can reasonably hold.
const generous = size => resolveLimits({
  maxModelFileBytes: size, maxModelBytes: size,
  maxDecodedBytes: 64 * 1024 * 1024 * 1024,
  maxTensorElements: 2 ** 31, maxDimension: 1 << 21, maxTensors: 4096,
});

test('a GGUF checkpoint reads by range, and a quantized row costs a row',
  {skip: !ready && reason}, async t => {
  const require = createRequire(import.meta.url);
  const wasm = require(wasmPath);
  const source = await fileSource(modelPath);
  const size = source.size();
  const limits = generous(size);
  try {
    const index = await openGGUF(source, 'model.gguf', wasm, limits);
    await t.test('the header is a small read of a large file', () => {
      assert.ok(index.headerBytes < size || size < 1 << 20,
        `read ${index.headerBytes} of ${size} bytes to parse the header`);
      assert.ok(index.tensors.size > 0);
      assert.equal(index.format, 'gguf');
    });

    await t.test('metadata names the architecture and stays small', () => {
      const meta = index.metadata();
      assert.equal(typeof meta['general.architecture'], 'string');
      // Long arrays are summarised, not inlined: a vocabulary is hundreds of
      // thousands of entries and no reader of the metadata should pay for it.
      for (const value of Object.values(meta)) {
        if (value && typeof value === 'object' && !Array.isArray(value)) continue;
        assert.ok(!Array.isArray(value) || value.length <= 64, 'a long array was inlined');
      }
    });

    const matrices = [...index.tensors.values()].filter(t => t.readable && t.shape.length === 2);
    assert.ok(matrices.length > 0, 'the file has no readable matrices');
    const biggest = matrices.reduce((a, b) => (a.elements > b.elements ? a : b));

    await t.test('shapes arrive row-major, not GGUF-major', () => {
      // GGUF writes fastest-varying first; an embedding table is rows of the
      // embedding width, and that is how it must appear here.
      assert.equal(biggest.shape.length, 2);
      assert.ok(biggest.shape[0] >= biggest.shape[1],
        `${biggest.name} looks transposed: ${JSON.stringify(biggest.shape)}`);
      assert.equal(biggest.shape[0] * biggest.shape[1], biggest.elements);
    });

    const store = new WeightStore([index], limits);
    try {
      await t.test('rows are read without decoding the tensor', async () => {
        const width = biggest.shape[1];
        const wanted = [0, 1, biggest.shape[0] - 1];
        const rows = await store.rows(biggest.name, wanted);
        assert.equal(rows.length, wanted.length * width);
        assert.ok(rows.every(Number.isFinite), 'a dequantized row is not finite');
        // Reading the same row twice gives the same values.
        const again = await store.rows(biggest.name, [wanted[1]]);
        assert.deepEqual([...again], [...rows.slice(width, 2 * width)]);
      });

      await t.test('a small tensor decodes whole and binds into Graph v2', async () => {
        const small = [...index.tensors.values()]
          .filter(t => t.readable && t.elements > 1 && t.elements < 1 << 16)
          .sort((a, b) => a.elements - b.elements)[0];
        assert.ok(small, 'the file has no small readable tensor');
        const data = await store.tensor(small.name);
        assert.equal(data.length, small.elements);
        const bound = await bindGraph({
          version: 1,
          graph: {version: 2, nodes: [{id: 0, op: 'input', shape: [...small.shape]}],
                  outputs: [{name: 'logits', id: 0}]},
          bindings: [{node: 0, kind: 'tensor', tensor: small.name}],
        }, store, limits);
        assert.ok(bound.nodes[0].data instanceof Float32Array);
        assert.equal(bound.nodes[0].data.length, small.elements);
      });

      await t.test('an unreadable dtype is refused by name, not silently', () => {
        const unreadable = [...index.tensors.values()].find(t => !t.readable);
        if (!unreadable) return; // every dtype in this file decodes
        assert.throws(() => store.info(unreadable.name), /does not decode/);
      });
    } finally { store.dispose(); }
  } finally { await source.close(); }
});
