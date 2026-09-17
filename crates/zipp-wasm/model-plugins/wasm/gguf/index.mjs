/**
 * An ergonomic wrapper over the WebAssembly module.
 *
 * The module itself is deliberately low-level: it parses a header out of bytes
 * you give it and tells you where every tensor lives. That is the right shape
 * for the library -- a checkpoint is routinely gigabytes and nothing should
 * hold one -- but it leaves every caller writing the same three things: read
 * the header by doubling until it parses, turn a tensor name into a byte
 * range, and fetch that range from wherever the file actually is.
 *
 * This does those three things over any source that can answer "give me bytes
 * [offset, offset+length)": a `File` or `Blob` a person chose, an HTTP server
 * that honours Range, or a Node file handle.
 *
 *   import {openGGUF, fromBlob, fromURL} from 'gguf-wasm/js';
 *
 *   const model = await openGGUF(fromBlob(file), {module});
 *   model.architecture                  // 'qwen3'
 *   model.tensors.get('blk.0.attn_q.weight')
 *   await model.bytes('blk.0.attn_q.weight')   // packed, undecoded
 *   await model.floats('blk.0.attn_q.weight')  // decoded
 *   await model.rows('token_embd.weight', [12095])
 *   model.tokenizer().encode('Hello world')
 */

/** The first header read, and the largest a header may be. Its size depends on
 * how large the tokenizer vocabulary in the metadata is, which is not known
 * before reading it, so the read doubles until it parses. */
const FIRST = 1 << 20;
const LARGEST = 64 << 20;

/** A `File` or `Blob` the person chose. Nothing is uploaded and nothing is
 * loaded: `slice` is a view, and only the slice is read. */
export function fromBlob(blob) {
  return {
    size: () => blob.size,
    async read(offset, length) {
      return new Uint8Array(await blob.slice(offset, offset + length).arrayBuffer());
    },
  };
}

/** A URL, over HTTP Range. The server has to honour it; one that ignores
 * Range and sends the whole file is caught here rather than silently read. */
export function fromURL(url, {fetch: fetcher = globalThis.fetch} = {}) {
  let size = null;
  return {
    async prepare() {
      const head = await fetcher(url, {method: 'HEAD'});
      if (!head.ok) throw new Error(`HEAD ${url}: ${head.status}`);
      size = Number(head.headers.get('content-length'));
      if (!Number.isFinite(size) || size <= 0) throw new Error(`${url} reports no length`);
      if ((head.headers.get('accept-ranges') ?? '') !== 'bytes') {
        throw new Error(`${url} does not accept range requests`);
      }
    },
    size: () => size,
    async read(offset, length) {
      const response = await fetcher(url, {headers: {Range: `bytes=${offset}-${offset + length - 1}`}});
      if (response.status !== 206) throw new Error(`${url} ignored a range request (${response.status})`);
      const bytes = new Uint8Array(await response.arrayBuffer());
      if (bytes.length !== length) throw new Error(`${url} returned ${bytes.length} of ${length} bytes`);
      return bytes;
    },
  };
}

/** A Node file handle, for a checkpoint on disk. */
export function fromFileHandle(handle, size) {
  return {
    size: () => size,
    async read(offset, length) {
      const out = new Uint8Array(length);
      const {bytesRead} = await handle.read(out, 0, length, offset);
      if (bytesRead !== length) throw new Error(`short read at ${offset}`);
      return out;
    },
  };
}

/**
 * Open a checkpoint. `module` is the wasm-bindgen module; supply it rather
 * than have this guess, because how it is loaded is the host's business.
 */
export async function openGGUF(source, {module}) {
  if (!module || typeof module.GgufHeader !== 'function') {
    throw new Error('Pass the gguf-wasm module: {module}');
  }
  await source.prepare?.();
  const size = source.size();

  let header = null, read = 0, failure = null;
  for (let want = Math.min(FIRST, size); want <= Math.min(LARGEST, size);) {
    try {
      header = new module.GgufHeader(await source.read(0, want));
      read = want;
      break;
    } catch (error) {
      failure = error;
      if (want >= size) break;
      want = Math.min(want * 4, size, LARGEST);
    }
  }
  if (!header) throw new Error(`could not read a GGUF header: ${failure?.message ?? failure}`);

  const metadata = JSON.parse(header.metadata());
  const listed = JSON.parse(header.tensors());
  const tensors = new Map(listed.map(entry => [entry.name, Object.freeze({
    ...entry,
    // GGUF writes a shape fastest-varying first. Both orders are offered so a
    // caller never has to remember which one it is looking at.
    stored: Object.freeze([...entry.shape]),
    shape: Object.freeze([...entry.shape].reverse()),
  })]));

  const info = name => {
    const entry = tensors.get(name);
    if (!entry) throw new Error(`no tensor named ${name}`);
    return entry;
  };

  return {
    size, headerBytes: read, metadata, tensors,
    architecture: metadata['general.architecture'],
    version: header.version(),

    /** A tensor's bytes, exactly as the file stores them. For a quantized
     * tensor these are its blocks: 144 bytes per 256 values for Q4_K, against
     * 1,024 once expanded. */
    async bytes(name) {
      const entry = info(name);
      return source.read(entry.offset, entry.bytes);
    },

    /** A tensor decoded to float32. */
    async floats(name) {
      const entry = info(name);
      if (!entry.readable) throw new Error(`${name} is ${entry.dtype}; this build does not decode it`);
      return module.dequantize(entry.dtype, await this.bytes(name), entry.elements);
    },

    /** Whole rows of a matrix, without touching the rest of it. Every block
     * format packs a whole number of blocks into a row, so a row is
     * individually addressable -- for an embedding table that is the
     * difference between a few kilobytes and a gigabyte. */
    async rows(name, indices) {
      const entry = info(name);
      if (entry.shape.length !== 2) throw new Error(`row gathering needs a matrix: ${name}`);
      const width = entry.shape[1];
      const out = new Float32Array(indices.length * width);
      for (let i = 0; i < indices.length; i++) {
        const range = JSON.parse(header.row_range(name, indices[i], 1));
        const bytes = await source.read(range.offset, range.bytes);
        out.set(module.dequantize(range.dtype, bytes, range.elements), i * width);
      }
      return out;
    },

    /** One metadata string array -- a vocabulary or its merges. */
    strings(key) { return header.strings(key); },

    /** The tokenizer this checkpoint was trained with, built inside the module
     * so its vocabulary never crosses the boundary. */
    tokenizer() { return header.tokenizer(); },
  };
}
