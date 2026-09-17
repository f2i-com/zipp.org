// The tokenizer a checkpoint carries.
//
// Byte-level BPE is where a model quietly goes wrong. Ids that decode back to
// the original text prove almost nothing -- the byte alphabet round-trips
// whatever you give it -- so the checks here are against ids the vocabulary
// itself defines, and against the rule that distinguishes this family: Qwen2
// splits digits one at a time where Llama-3 takes three and GPT-2 takes a run.
//
// Needs a GGUF checkpoint, which this repository does not redistribute:
//
//   ZIPP_GGUF_MODEL=.../qwen3-0.6b-q4_k_m.gguf node --test tests/tokenizer.test.mjs
import test from 'node:test';
import assert from 'node:assert/strict';
import {open, stat, access} from 'node:fs/promises';

import {openGGUF, resolveLimits} from '../src/index.mjs';

const modelPath = process.env.ZIPP_GGUF_MODEL;
const ready = Boolean(modelPath) && await access(modelPath).then(() => true, () => false);
const reason = 'Set ZIPP_GGUF_MODEL to a .gguf file (see the file header)';

const GB = 1024 * 1024 * 1024;
const limits = resolveLimits({
  maxModelFileBytes: 8 * GB, maxModelBytes: 8 * GB, maxDecodedBytes: 4 * GB,
  maxTensorElements: 2 ** 31, maxDimension: 1 << 21, maxTensors: 4096,
});

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

test('a checkpoint tokenizes with its own vocabulary', {skip: !ready && reason}, async t => {
  const source = await fileSource(modelPath);
  try {
    const index = await openGGUF(source, 'model.gguf', null, limits);
    const started = Date.now();
    const tokenizer = index.tokenizer();
    const built = Date.now() - started;
    const vocab = index.strings('tokenizer.ggml.tokens');

    await t.test('it is built from the file, not handed the file', () => {
      assert.equal(tokenizer.vocab_size, vocab.length);
      // Fast because the tables never cross the boundary: building these maps
      // in guest Python took minutes.
      assert.ok(built < 5000, `took ${built} ms to build`);
      assert.equal(typeof tokenizer.pre, 'string');
    });

    await t.test('encoding agrees with the vocabulary itself', () => {
      // Each of these is one token in this vocabulary, so the ids the encoder
      // produces must be the ids the vocabulary lists. This is the check that
      // a round trip cannot make.
      for (const [text, pieces] of [
        ['The capital of France is', ['The', 'Ġcapital', 'Ġof', 'ĠFrance', 'Ġis']],
        ['Hello, world', ['Hello', ',', 'Ġworld']],
      ]) {
        const want = pieces.map(piece => {
          const id = vocab.indexOf(piece);
          assert.notEqual(id, -1, `vocabulary has no ${piece}`);
          return id;
        });
        assert.deepEqual([...tokenizer.encode(text)], want, `encoding ${JSON.stringify(text)}`);
      }
    });

    await t.test('digits follow the rule this vocabulary was trained with', () => {
      const ids = [...tokenizer.encode('12345')];
      const back = ids.map(id => tokenizer.token(id));
      if (tokenizer.pre === 'qwen2') {
        // One token per digit: the rule that separates Qwen2 from Llama-3.
        assert.equal(ids.length, 5, `qwen2 split 12345 into ${JSON.stringify(back)}`);
      }
      assert.equal(tokenizer.decode(Uint32Array.from(ids)), '12345');
    });

    await t.test('text survives the round trip, including what is not ASCII', () => {
      for (const text of ['The capital of France is Paris.', 'Mr.Smith said 12345.',
                          '  two  spaces ', 'caf\u00e9 \u{1f44b}', 'a\n\nb', '']) {
        const ids = Uint32Array.from(tokenizer.encode(text));
        assert.equal(tokenizer.decode(ids), text, `round trip for ${JSON.stringify(text)}`);
      }
    });

    await t.test('a chat marker stays one token', () => {
      const marker = '<|im_start|>';
      if (vocab.indexOf(marker) === -1) return; // not an instruction-tuned vocabulary
      const ids = [...tokenizer.encode(`${marker}hello`)];
      assert.equal(ids[0], vocab.indexOf(marker), 'the marker was shredded into bytes');
    });
  } finally { await source.close(); }
});
