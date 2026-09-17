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
 * loaded: `slice` is a view, and only the slice is read.
 *
 * A `Blob` is immutable, so unlike a URL there is no question of the bytes
 * changing between the header read and a tensor read. */
export function fromBlob(blob) {
  return {
    size: () => blob.size,
    async read(offset, length) {
      return new Uint8Array(await blob.slice(offset, offset + length).arrayBuffer());
    },
  };
}

/** `Content-Range: bytes 12-34/5678`, as numbers. Null if it is not that. */
function parseContentRange(header) {
  const match = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(String(header ?? '').trim());
  if (!match) return null;
  const [, start, end, total] = match;
  return {start: Number(start), end: Number(end), total: Number(total)};
}

/**
 * A URL, over HTTP Range.
 *
 * Reading a model over the network means many requests against one object, and
 * the thing that goes wrong is not a failed request -- it is a *successful* one
 * against a different object. A header parsed from version A and a weight
 * fetched from version B both succeed, and the result is a model that is
 * quietly wrong. So the object's identity is pinned when it is opened and
 * carried on every read afterwards:
 *
 *   * the open is a one-byte range request, not a HEAD. A server that honours
 *     Range answers 206 with a `Content-Range` that states the total length,
 *     which is the same information a HEAD would give and is proof rather than
 *     a promise. Some perfectly good servers and CDNs do not expose
 *     `Accept-Ranges` or `Content-Length` to a cross-origin HEAD at all.
 *   * whatever validator comes back -- `ETag`, else `Last-Modified` -- is sent
 *     as `If-Range` on every subsequent read. A server whose object has changed
 *     answers 200 with the whole entity instead of 206, and that is refused.
 *   * every response's `Content-Range` must be the range that was asked for,
 *     out of a total that has not changed.
 *
 * None of this makes an HTTP source trustworthy. It makes it *consistent*: what
 * is read is all from one object, or it is an error.
 */
export function fromURL(url, {fetch: fetcher = globalThis.fetch, headers = {}, signal} = {}) {
  let size = null, validator = null;

  const request = extra => ({headers: {...headers, ...extra}, signal});

  const checkRange = (response, offset, length) => {
    if (response.status === 200) {
      throw new Error(`${url} returned the whole entity: the object changed since it was opened`);
    }
    if (response.status !== 206) {
      throw new Error(`${url} did not honour a range request (${response.status})`);
    }
    const range = parseContentRange(response.headers.get('content-range'));
    if (!range) throw new Error(`${url} returned no usable Content-Range`);
    if (range.start !== offset || range.end !== offset + length - 1) {
      throw new Error(`${url} returned bytes ${range.start}-${range.end}, not ${offset}-${offset + length - 1}`);
    }
    if (size !== null && range.total !== size) {
      throw new Error(`${url} is now ${range.total} bytes, was ${size}: the object changed`);
    }
    const now = response.headers.get('etag') ?? response.headers.get('last-modified');
    if (validator && now && now !== validator) {
      throw new Error(`${url} changed identity (${validator} -> ${now})`);
    }
    return range;
  };

  return {
    async prepare() {
      const response = await fetcher(url, request({Range: 'bytes=0-0'}));
      const range = checkRange(response, 0, 1);
      await response.arrayBuffer();
      size = range.total;
      if (!Number.isFinite(size) || size <= 0) throw new Error(`${url} reports no length`);
      // Pinned here, and required to still hold on every read after this.
      validator = response.headers.get('etag') ?? response.headers.get('last-modified') ?? null;
    },
    size: () => size,
    async read(offset, length) {
      if (length === 0) return new Uint8Array(0);
      const response = await fetcher(url, request({
        Range: `bytes=${offset}-${offset + length - 1}`,
        ...(validator ? {'If-Range': validator} : {}),
      }));
      checkRange(response, offset, length);
      const bytes = new Uint8Array(await response.arrayBuffer());
      if (bytes.length !== length) throw new Error(`${url} returned ${bytes.length} of ${length} bytes`);
      return bytes;
    },
    /** What identity this source pinned, if the server offered one. A caller
     * that stores a model reference alongside a digest wants this. */
    identity: () => validator,
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
