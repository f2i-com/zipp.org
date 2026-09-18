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

/** What the module's own parameters can hold.
 *
 * The Rust parser is `u64` throughout, and a tensor's `offset` and `bytes` stay
 * that wide because reading a range never materialises anything inside the
 * module. But `dequantize` and `row_range` take `u32`: they produce values *in*
 * wasm32 memory, which tops out at four gigabytes anyway, so a wider parameter
 * would only move the failure.
 *
 * The part worth refusing is the silent conversion. wasm-bindgen turns a
 * JavaScript number into a `u32` by truncating, so a row index of 2^32 + 5
 * arrives as 5 -- a different row, read successfully, returned as though it
 * were the one asked for. Anything crossing that boundary is checked here
 * instead, where the number still means what the caller wrote.
 *
 * `bytes()` has no such ceiling and is the way to reach a tensor this cannot
 * decode in one go: the range is read outside the module and never passes
 * through a `u32`. */
const U32_MAX = 0xffffffff;

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

/** `Content-Range: bytes 12-34/5678`, as numbers. Null if it is not that.
 *
 * Null too if any of them is past 2^53. These are decimal digits from a header
 * and `Number` will take as many as it is given, returning something close to
 * but not equal to what was sent -- and the whole point of reading this header
 * is comparing it with what was asked for. The same rule as a tensor range:
 * nothing beyond what JavaScript can hold exactly. */
function parseContentRange(header) {
  const match = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(String(header ?? '').trim());
  if (!match) return null;
  const [start, end, total] = match.slice(1, 4).map(Number);
  if (![start, end, total].every(Number.isSafeInteger)) return null;
  return {start, end, total};
}

/** An `ETag` usable as an `If-Range` validator, or null.
 *
 * `If-Range` needs a *strong* validator: a weak one (`W/"..."`) promises only
 * that the entity is semantically equivalent, which is exactly the guarantee
 * that does not hold here -- two byte ranges from semantically equivalent but
 * differently encoded entities do not join up. RFC 9110 says a server must
 * ignore `If-Range` with a weak validator, so sending one would give a
 * confident-looking request that means nothing. */
function strongETag(value) {
  return typeof value === 'string' && !/^\s*W\//.test(value) ? value : null;
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
 *   * whatever validator comes back -- a *strong* `ETag`, else `Last-Modified`
 *     -- is sent as `If-Range` on every subsequent read. A server whose object
 *     has changed answers 200 with the whole entity instead of 206, and that is
 *     refused. A weak ETag is passed over: `If-Range` requires a strong
 *     validator and a server must ignore a weak one, so sending it would look
 *     like a guarantee while being none.
 *   * every response's `Content-Range` must be the range that was asked for,
 *     out of a total that has not changed.
 *
 * None of this makes an HTTP source trustworthy. It makes it *consistent*:
 * given a validator, what is read is all from one object or it is an error.
 *
 * A server that offers neither a strong `ETag` nor `Last-Modified` cannot support that
 * promise -- an object could be replaced by a different one of the same length
 * between two reads and nothing here would see it. That is refused by default
 * rather than quietly downgraded, because the failure it produces is a model
 * that is subtly wrong rather than one that does not load. Pass
 * `allowUnvalidated: true` to read anyway, and know that the size check is
 * then the only thing standing between you and a mixed read.
 */
export function fromURL(url, {
  fetch: fetcher = globalThis.fetch, headers = {}, signal, allowUnvalidated = false,
} = {}) {
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
    const now = strongETag(response.headers.get('etag')) ?? response.headers.get('last-modified');
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
      // A weak ETag is not usable here, so it is passed over for
      // Last-Modified rather than sent as an If-Range a server must ignore.
      validator = strongETag(response.headers.get('etag'))
        ?? response.headers.get('last-modified') ?? null;
      if (validator === null && !allowUnvalidated) {
        throw new Error(
          `${url} offers neither a strong ETag nor Last-Modified, so reads cannot be pinned ` +
          `to one `+
          `version of it. Pass {allowUnvalidated: true} to read it anyway.`);
      }
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
 * A byte range out of the header, checked against the file it claims to be in.
 *
 * Everything here arrives as JSON from a header the caller did not write, and
 * two things go wrong with that. A GGUF length is a `u64`, and JSON numbers
 * are doubles: past 2^53 a value arrives *near* what the file said rather than
 * equal to it, so a read silently covers the wrong bytes. And an offset past
 * the end of the file is not an error in any source here -- `Blob.slice`
 * clamps, so the read comes back short and a decoder sees a truncated tensor
 * as a valid one full of whatever followed.
 *
 * So a range is refused before it is ever read, and the refusal names the
 * tensor rather than surfacing as a decode failure somewhere downstream.
 */
function checkRange(range, name, size) {
  for (const field of ['offset', 'bytes', 'elements']) {
    const value = range[field];
    if (!Number.isSafeInteger(value) || value < 0) {
      throw new Error(
        `${name}: ${field} is ${value}, which is not a byte count this can represent exactly`);
    }
  }
  if (range.offset + range.bytes > size) {
    throw new Error(
      `${name}: bytes ${range.offset}-${range.offset + range.bytes - 1} are past the end of a ${size}-byte file`);
  }
  return range;
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
    const head = await source.read(0, want);
    try {
      header = new module.GgufHeader(head);
      read = want;
      break;
    } catch (error) {
      failure = error;
      // Only a short read is worth reading more for. A file that is not a
      // GGUF file, or whose counts are past the parser's limits, says so on
      // the first megabyte and says the same thing on the sixty-fourth --
      // without this, opening the wrong file costs the whole doubling
      // schedule before it fails.
      if (module.header_needs_more_bytes && !module.header_needs_more_bytes(head)) break;
      if (want >= size) break;
      want = Math.min(want * 4, size, LARGEST);
    }
  }
  if (!header) throw new Error(`could not read a GGUF header: ${failure?.message ?? failure}`);

  const metadata = JSON.parse(header.metadata());
  const listed = JSON.parse(header.tensors());
  for (const entry of listed) checkRange(entry, entry.name, size);
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
      if (entry.elements > U32_MAX) {
        throw new Error(
          `${name} has ${entry.elements} values, past the ${U32_MAX} this module can decode in ` +
          `one call. Read it as blocks with bytes() and decode it in pieces.`);
      }
      return module.dequantize(entry.dtype, await this.bytes(name), entry.elements);
    },

    /** Whole rows of a matrix, without touching the rest of it. Every block
     * format packs a whole number of blocks into a row, so a row is
     * individually addressable -- for an embedding table that is the
     * difference between a few kilobytes and a gigabyte. */
    async rows(name, indices) {
      const entry = info(name);
      if (entry.shape.length !== 2) throw new Error(`row gathering needs a matrix: ${name}`);
      const [count, width] = entry.shape;
      // These come from the caller rather than the file, so this is about a
      // public API being hard to misuse rather than about a hostile
      // checkpoint. A row index crosses into the module as a `u32`, and
      // JavaScript will hand 4294967297, -1 or 1.5 to that conversion without
      // complaint -- each of which reads some *other* row and returns it as
      // though it were the one asked for.
      if (!Array.isArray(indices) && !ArrayBuffer.isView(indices)) {
        throw new Error('rows() takes a list of row indices');
      }
      for (const row of indices) {
        if (!Number.isSafeInteger(row) || row < 0 || row >= count) {
          throw new Error(`${name}: row ${row} is not one of its ${count} rows`);
        }
        if (row > U32_MAX) {
          throw new Error(
            `${name}: row ${row} is past the ${U32_MAX} this module addresses. The tensor is ` +
            `reachable with bytes(), which reads a range without going through the module.`);
        }
      }
      const total = indices.length * width;
      if (!Number.isSafeInteger(total)) {
        throw new Error(`${name}: ${indices.length} rows of ${width} is too large to gather`);
      }
      const out = new Float32Array(total);
      for (let i = 0; i < indices.length; i++) {
        // Checked here too, not just at open: this one comes fresh out of the
        // module for each call, computed from a row index the caller chose.
        const range = checkRange(JSON.parse(header.row_range(name, indices[i], 1)), name, size);
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
