import {check, fields, integer, safePath, shapeSize, resolveLimits} from './common.mjs';
import {decodeUTF8, parseJSON} from './json.mjs';

/**
 * GGUF as a weight source, alongside Safetensors.
 *
 * The reader and the block dequantizers are the `gguf` and `ggml-quants` crates
 * compiled to WebAssembly; this is the host side of that boundary. What matters
 * about the arrangement is that neither side ever holds the file: the header is
 * parsed from the first few megabytes, and every tensor after that is a byte
 * range this module reads through the same bounded `source` interface a
 * Safetensors model uses. A quantized checkpoint is routinely several
 * gigabytes, so holding one was never an option.
 *
 * Two things follow from the format:
 *
 *   * GGUF writes a shape fastest-varying first, the mirror of the convention
 *     everything here uses. Shapes are reversed on the way in, once, so a
 *     plugin sees `[rows, width]` like every other tensor and never has to know
 *     which file it came from.
 *   * A quantized row is individually addressable, because every block format
 *     packs a whole number of blocks per row. `rows` therefore reads and
 *     dequantizes only the rows asked for. For an embedding table that is the
 *     difference between a few kilobytes and a gigabyte, and it is why a
 *     vocabulary-sized tensor is usable at all.
 *
 * What this does not do is keep anything quantized. Every tensor becomes
 * float32 as it crosses into the graph, because the compute protocol is
 * float32. Quantization here buys a smaller file and a smaller read, not a
 * smaller tensor on the device.
 */
const HEADER_FIRST = 1 << 20;
const HEADER_MAX = 64 * 1024 * 1024;

/** Read the header by doubling until it parses: its size depends on how large
 * the tokenizer vocabulary embedded in the metadata is, which is not known
 * before reading it. */
async function readHeader(source, path, wasm, limits) {
  const size = source.size(path);
  integer(size, 8, Number.MAX_SAFE_INTEGER, 'GGUF file bytes');
  let want = Math.min(HEADER_FIRST, size), error = null;
  while (want <= Math.min(HEADER_MAX, size)) {
    const head = await source.read(path, 0, want);
    try { return {header: new wasm.GgufHeader(head), read: want}; }
    catch (failure) {
      error = failure;
      if (want === size) break;
      want = Math.min(want * 4, size);
    }
  }
  check(false, 'FORMAT', `Could not read a GGUF header from ${path}: ${error?.message ?? error}`);
}

/**
 * Open a GGUF file as a weight index with the same shape `openSafetensors`
 * returns, so everything downstream — the binder, the weight store, the
 * prepared decode plan — is unchanged.
 */
export async function openGGUF(source, path, wasm, overrides = {}) {
  const limits = resolveLimits(overrides);
  safePath(path);
  // The module comes from another repository and is coupled to this one by the
  // shape of what it returns, not by a version. Every field it reports is
  // checked below, so a changed shape fails by name rather than by producing
  // tensors that are quietly wrong.
  check(wasm && typeof wasm.GgufHeader === 'function' && typeof wasm.dequantize === 'function'
    && typeof wasm.supported_dtypes === 'function',
    'HOST', 'Supply the GGUF WebAssembly module (GgufHeader, dequantize, supported_dtypes)');
  const size = source.size(path);
  const {header, read} = await readHeader(source, path, wasm, limits);
  const listed = parseJSON(header.tensors(), {maxChars: limits.maxHeaderBytes});
  check(Array.isArray(listed) && listed.length > 0, 'FORMAT', 'GGUF file lists no tensors');
  integer(listed.length, 1, limits.maxTensors, 'Tensor count');

  const tensors = new Map();
  // What decoding the whole file would cost, which for a quantized checkpoint
  // is several times the file itself. It is reported, not enforced: nothing
  // decodes a whole checkpoint here, and charging for it up front would make a
  // file unopenable that a caller only ever reads a few rows of. The budget is
  // charged against what is actually decoded, below.
  let fullyDecodedBytes = 0;
  for (const entry of listed) {
    fields(entry, ['name', 'shape', 'dtype', 'elements', 'offset', 'bytes', 'readable'],
      ['name', 'shape', 'dtype', 'elements', 'offset', 'bytes', 'readable']);
    check(typeof entry.name === 'string' && entry.name.length > 0 && entry.name.length <= 256,
      'FORMAT', 'Invalid tensor name');
    check(!tensors.has(entry.name), 'TENSOR', `Duplicate tensor: ${entry.name}`);
    // Reversed once, here, so no plugin ever needs to know about GGUF's order.
    const shape = Object.freeze([...entry.shape].reverse());
    const elements = shapeSize(shape, limits, true);
    check(elements === entry.elements, 'SHAPE', `Element count disagrees for ${entry.name}`);
    integer(entry.offset, 0, size, 'Tensor start');
    integer(entry.bytes, 0, size - entry.offset, 'Tensor byte length');
    if (entry.readable) fullyDecodedBytes += elements * 4;
    tensors.set(entry.name, Object.freeze({
      name: entry.name, dtype: entry.dtype, shape, elements,
      offset: entry.offset, bytes: entry.bytes, readable: entry.readable === true,
    }));
  }

  // Charged as tensors are decoded, so the budget bounds real memory rather
  // than a hypothetical.
  let decoded = 0;
  function charge(elements, name) {
    decoded += elements * 4;
    check(decoded <= limits.maxDecodedBytes, 'LIMIT',
      `Decoding ${name} would pass the decoded-weight budget of ${limits.maxDecodedBytes} bytes`);
  }
  async function bytesOf(offset, length) {
    integer(length, 0, limits.maxModelFileBytes, 'Tensor bytes');
    const data = await source.read(path, offset, length);
    check(data instanceof Uint8Array && data.byteLength === length, 'SOURCE', 'Short or invalid read');
    return data;
  }
  function checked(values, expected, name) {
    check(values instanceof Float32Array || Array.isArray(values), 'SOURCE', 'Expected float data');
    const out = values instanceof Float32Array ? values : Float32Array.from(values);
    check(out.length === expected, 'SHAPE', `Dequantized ${name} has ${out.length} of ${expected} elements`);
    for (let i = 0; i < out.length; i++) {
      // The graph protocol admits finite float32 only, and a corrupt block
      // reads as a plausible tensor full of infinities rather than an error.
      check(Number.isFinite(out[i]), 'NUMBER', `Non-finite weight in ${name}`);
    }
    return out;
  }
  function info(name) {
    check(tensors.has(name), 'TENSOR', `Missing tensor: ${name}`);
    const entry = tensors.get(name);
    check(entry.readable, 'DTYPE', `Cannot read ${name}: this build does not decode ${entry.dtype}`);
    return entry;
  }

  return {
    path, size, tensors, format: 'gguf', headerBytes: read,
    // Zero at first: a weight store sums what its indexes have decoded, and
    // nothing has been decoded yet.
    get decodedBytes() { return decoded; },
    fullyDecodedBytes,
    /** The file's own metadata, for a plugin that reads its configuration. */
    metadata() { return parseJSON(header.metadata(), {maxChars: limits.maxHeaderBytes}); },
    /** One metadata string array — a tokenizer vocabulary or its merges. */
    strings(key) {
      check(typeof key === 'string', 'FORMAT', 'Expected a metadata key');
      return header.strings(key);
    },
    async readTensor(name) {
      const entry = info(name);
      charge(entry.elements, name);
      return checked(wasm.dequantize(entry.dtype, await bytesOf(entry.offset, entry.bytes), entry.elements),
        entry.elements, name);
    },
    /** Gather whole rows without decoding the rest of the tensor. */
    async readRows(name, indices) {
      const entry = info(name);
      check(entry.shape.length === 2, 'SHAPE', `Row gathering needs a matrix: ${name}`);
      const width = entry.shape[1];
      charge(indices.length * width, name);
      const out = new Float32Array(indices.length * width);
      for (let i = 0; i < indices.length; i++) {
        integer(indices[i], 0, entry.shape[0] - 1, 'Row index');
        const range = parseJSON(header.row_range(name, indices[i], 1), {maxChars: 512});
        const row = checked(wasm.dequantize(range.dtype, await bytesOf(range.offset, range.bytes), range.elements),
          width, name);
        out.set(row, i * width);
      }
      return out;
    },
  };
}

/** Load the GGUF module, whose bytes the host supplies: this package downloads
 * nothing and knows no URLs. */
export async function loadGgufModule(factory) {
  check(typeof factory === 'function', 'HOST', 'Supply a factory that returns the GGUF WebAssembly module');
  const wasm = await factory();
  check(wasm && typeof wasm.GgufHeader === 'function', 'HOST', 'That module exposes no GgufHeader');
  return wasm;
}
