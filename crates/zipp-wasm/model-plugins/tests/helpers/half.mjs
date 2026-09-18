// float32 <-> float16, for measuring what a narrower seam would cost.
//
// A hidden state is float32 today and the seam is therefore exact, which is
// what lets every staging test assert bit-for-bit equality. Halving it would
// halve what crosses between peers -- 2 KB a token instead of 4 -- and it would
// stop being exact. That trade needs a number, not an opinion, so this is here
// to produce one.
//
// Nothing in the model uses it. Narrowing is a *transport* decision, made
// between a readback and the next stage's input, so the graph still emits and
// accepts float32 and bit-for-bit stays the default.
//
// Node has no Float16Array here, so the conversion is written out. It is
// round-to-nearest-even, and it handles what a naive version gets wrong:
// subnormals at both ends, the overflow-to-infinity boundary, and a NaN that
// must stay a NaN rather than becoming an infinity.

/** float32 -> float16 bits, round half to even. */
export function toHalfBits(value) {
  const f32 = new Float32Array(1);
  const u32 = new Uint32Array(f32.buffer);
  f32[0] = value;
  const bits = u32[0];
  const sign = (bits >>> 16) & 0x8000;
  const exponent = (bits >>> 23) & 0xff;
  const mantissa = bits & 0x7fffff;

  if (exponent === 0xff) {
    // Infinity stays infinity; a NaN stays a NaN, which means keeping at least
    // one mantissa bit set -- rounding it away would turn it into infinity.
    return sign | 0x7c00 | (mantissa ? 0x200 : 0);
  }
  // Unbiased for f16: 127 - 15.
  const shifted = exponent - 112;
  if (shifted >= 0x1f) return sign | 0x7c00;          // overflows to infinity
  if (shifted <= 0) {
    // Subnormal, or too small to be one. The implicit leading bit comes back
    // for the shift, and everything below the last kept bit decides rounding.
    if (shifted < -10) return sign;                    // underflows to zero
    const full = mantissa | 0x800000;
    const shift = 14 - shifted;
    let half = full >>> shift;
    const remainder = full & ((1 << shift) - 1);
    const midpoint = 1 << (shift - 1);
    if (remainder > midpoint || (remainder === midpoint && (half & 1))) half += 1;
    return sign | half;
  }
  let half = (shifted << 10) | (mantissa >>> 13);
  const remainder = mantissa & 0x1fff;
  // Exactly halfway rounds to even; above it rounds up. A carry out of the
  // mantissa lands in the exponent, which is what the addition already does.
  if (remainder > 0x1000 || (remainder === 0x1000 && (half & 1))) half += 1;
  return sign | half;
}

/** float16 bits -> float32. */
export function fromHalfBits(half) {
  const sign = (half & 0x8000) << 16;
  const exponent = (half >>> 10) & 0x1f;
  const mantissa = half & 0x3ff;
  const f32 = new Float32Array(1);
  const u32 = new Uint32Array(f32.buffer);

  if (exponent === 0) {
    if (mantissa === 0) { u32[0] = sign; return f32[0]; }
    // A subnormal in f16 is an ordinary number in f32: normalise it by hand.
    let e = -1, m = mantissa;
    do { e += 1; m <<= 1; } while ((m & 0x400) === 0);
    u32[0] = sign | ((127 - 15 - e) << 23) | ((m & 0x3ff) << 13);
    return f32[0];
  }
  if (exponent === 0x1f) { u32[0] = sign | 0x7f800000 | (mantissa << 13); return f32[0]; }
  u32[0] = sign | ((exponent + 112) << 23) | (mantissa << 13);
  return f32[0];
}

/** A whole vector, narrowed and widened again -- what a f16 seam would deliver. */
export function throughHalf(values) {
  const out = new Float32Array(values.length);
  for (let i = 0; i < values.length; i++) out[i] = fromHalfBits(toHalfBits(values[i]));
  return out;
}

/** The bytes a f16 seam would actually carry. */
export function toHalf(values) {
  const out = new Uint16Array(values.length);
  for (let i = 0; i < values.length; i++) out[i] = toHalfBits(values[i]);
  return out;
}

export function fromHalf(half) {
  const out = new Float32Array(half.length);
  for (let i = 0; i < half.length; i++) out[i] = fromHalfBits(half[i]);
  return out;
}
