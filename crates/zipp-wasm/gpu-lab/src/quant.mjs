/** Q4_K block decoding, for backends that read a quantized weight as they use it.
 *
 * A port of `dequantize_row_q4_K` as ggml defines it, kept deliberately literal
 * so it can be read against that source rather than trusted. The layout of one
 * 144-byte block, holding 256 values:
 *
 *   [0..2]     f16 d        super-block scale
 *   [2..4]     f16 dmin     super-block minimum
 *   [4..16]    u8  scales   eight 6-bit scales and eight 6-bit minimums, packed
 *   [16..144]  u8  qs       256 four-bit quants, low nibbles then high
 *
 * The eight sub-blocks of 32 are walked two at a time: the low nibbles of 32
 * bytes give one sub-block, the high nibbles of the same 32 bytes give the next.
 *
 * Nothing here is approximate. Decoding is exact arithmetic on integers and two
 * half-precision scales, so a matmul that decodes as it reads produces exactly
 * what the same matmul over decoded values produces -- which is the contract
 * the tests hold it to, and the reason a quantized weight changes what a device
 * must hold without changing what it computes.
 */
export const Q4_K_BLOCK = 256;
export const Q4_K_BYTES = 144;

/** One IEEE half, as the float32 it names exactly. */
export function readHalf(bytes, at) {
  const bits = bytes[at] | (bytes[at + 1] << 8);
  const sign = bits & 0x8000 ? -1 : 1, exponent = (bits >>> 10) & 0x1f, fraction = bits & 0x3ff;
  if (exponent === 0) return sign * 2 ** -14 * (fraction / 1024);
  if (exponent === 31) return fraction ? NaN : sign * Infinity;
  return sign * 2 ** (exponent - 15) * (1 + fraction / 1024);
}

/** The scale and minimum of sub-block `j`, unpacked from the 12 scale bytes.
 * Sub-blocks 0 to 3 are a plain six bits; 4 to 7 have their high two bits
 * borrowed from the bytes of the first four. */
function scaleMin(scales, at, j, out) {
  if (j < 4) {
    out[0] = scales[at + j] & 0x3f;
    out[1] = scales[at + j + 4] & 0x3f;
  } else {
    out[0] = (scales[at + j + 4] & 0x0f) | ((scales[at + j - 4] >>> 6) << 4);
    out[1] = (scales[at + j + 4] >>> 4) | ((scales[at + j] >>> 6) << 4);
  }
}

/**
 * Decode one block into `out` at `outAt`. `bytes` is the whole weight and
 * `at` the block's first byte, so a caller decodes a row without copying it
 * out first.
 */
export function decodeQ4KBlock(bytes, at, out, outAt) {
  const d = readHalf(bytes, at), dmin = readHalf(bytes, at + 2);
  const scales = at + 4, qs = at + 16;
  const pair = [0, 0];
  let y = outAt, q = qs;
  for (let is = 0; is < 8; is += 2) {
    scaleMin(bytes, scales, is, pair);
    const d1 = d * pair[0], m1 = dmin * pair[1];
    scaleMin(bytes, scales, is + 1, pair);
    const d2 = d * pair[0], m2 = dmin * pair[1];
    for (let l = 0; l < 32; l++) out[y + l] = d1 * (bytes[q + l] & 0x0f) - m1;
    for (let l = 0; l < 32; l++) out[y + 32 + l] = d2 * ((bytes[q + l] >>> 4) & 0x0f) - m2;
    y += 64;
    q += 32;
  }
}

/** Decode `count` values starting at block `block`, into `out`. */
export function decodeQ4K(bytes, block, count, out) {
  const blocks = count / Q4_K_BLOCK;
  for (let i = 0; i < blocks; i++) {
    decodeQ4KBlock(bytes, (block + i) * Q4_K_BYTES, out, i * Q4_K_BLOCK);
  }
}
