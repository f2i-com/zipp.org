// The float16 conversion, before anything is measured with it.
//
// A measurement of what a narrower seam costs is worth exactly as much as the
// conversion used to make it. So this checks the conversion against values
// whose half-precision representation is known independently, and against the
// cases a naive version gets wrong: subnormals at both ends, the boundary where
// a float32 stops fitting, and a NaN that must not become an infinity.
import test from 'node:test';
import assert from 'node:assert/strict';

import {toHalfBits, fromHalfBits, throughHalf} from './helpers/half.mjs';

test('known values round-trip to the bits IEEE 754 says they should', () => {
  // value, half-precision bits. From the format definition, not from this code.
  const known = [
    [0, 0x0000], [-0, 0x8000],
    [1, 0x3c00], [-1, 0xbc00], [2, 0x4000], [0.5, 0x3800],
    [1.0009765625, 0x3c01],                 // the smallest step above one
    [65504, 0x7bff],                        // the largest finite half
    // Written as the arithmetic that defines them rather than as decimals: a
    // truncated literal is a different float, and this table is the reference.
    [2 ** -14, 0x0400],                     // the smallest normal
    [1023 * 2 ** -24, 0x03ff],              // the largest subnormal
    [2 ** -24, 0x0001],                     // the smallest subnormal
    [Infinity, 0x7c00], [-Infinity, 0xfc00],
  ];
  for (const [value, bits] of known) {
    assert.equal(toHalfBits(value), bits,
      `${value} should be 0x${bits.toString(16)}, got 0x${toHalfBits(value).toString(16)}`);
    assert.equal(fromHalfBits(bits), value, `0x${bits.toString(16)} should be ${value}`);
  }
});

test('rounding is to nearest, ties to even', () => {
  // Exactly between two halves. 1 + 2^-11 sits midway between 1 and the next
  // representable value; ties to even keeps 1, whose last bit is zero.
  assert.equal(toHalfBits(1 + 2 ** -11), 0x3c00, 'a tie at 1 rounds down to even');
  // And midway between 1.0009765625 (odd last bit) and the next: rounds up.
  assert.equal(toHalfBits(1.0009765625 + 2 ** -11), 0x3c02, 'a tie at an odd value rounds up');
  // Above the midpoint always rounds away, regardless of parity.
  assert.ok(toHalfBits(1 + 2 ** -11 + 2 ** -20) === 0x3c01, 'above a tie rounds up');
});

test('what does not fit says so, and a NaN stays a NaN', () => {
  assert.equal(toHalfBits(65520), 0x7c00, 'past the largest finite half is infinity');
  assert.equal(toHalfBits(-65520), 0xfc00);
  assert.equal(toHalfBits(1e-9), 0x0000, 'below the smallest subnormal is zero');
  assert.equal(toHalfBits(-1e-9), 0x8000, 'and keeps its sign');
  // A NaN whose mantissa bits all fall outside f16 must not round to infinity:
  // that would turn a broken value into a plausible one.
  const bits = toHalfBits(NaN);
  assert.equal(bits & 0x7c00, 0x7c00, 'exponent says not-finite');
  assert.notEqual(bits & 0x03ff, 0, 'and the mantissa keeps it a NaN');
  assert.ok(Number.isNaN(fromHalfBits(bits)), 'and it comes back a NaN');
});

test('a value already representable survives unchanged', () => {
  // Anything a half can hold exactly must come back bit-identical, or the
  // conversion is losing something it did not need to.
  for (let bits = 0; bits < 0x10000; bits++) {
    if ((bits & 0x7c00) === 0x7c00) continue;              // skip inf and NaN
    const value = fromHalfBits(bits);
    assert.equal(toHalfBits(value), bits,
      `0x${bits.toString(16)} -> ${value} -> 0x${toHalfBits(value).toString(16)}`);
  }
});

test('a realistic vector loses about a thousandth of its magnitude', () => {
  // Hidden states are roughly standard-normal. This is the error a seam would
  // introduce per crossing, which is what the staging measurement compounds.
  let seed = 12345;
  const random = () => {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
    return seed / 0x7fffffff;
  };
  const values = new Float32Array(4096);
  for (let i = 0; i < values.length; i++) {
    values[i] = Math.fround((random() - 0.5) * 8);
  }
  const back = throughHalf(values);
  let worst = 0, worstRelative = 0;
  for (let i = 0; i < values.length; i++) {
    worst = Math.max(worst, Math.abs(back[i] - values[i]));
    if (values[i] !== 0) {
      worstRelative = Math.max(worstRelative, Math.abs((back[i] - values[i]) / values[i]));
    }
  }
  // Half precision has eleven significant bits, so the relative error cannot
  // exceed 2^-11. Anything larger means the rounding is wrong.
  assert.ok(worstRelative <= 2 ** -11,
    `relative error ${worstRelative} exceeds what eleven bits allow`);
  console.log(`      over 4096 values in [-4, 4]: worst absolute ${worst.toExponential(3)}, ` +
    `worst relative ${worstRelative.toExponential(3)}`);
});
