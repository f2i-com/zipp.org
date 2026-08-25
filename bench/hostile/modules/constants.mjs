export const ROUNDS = 900000;
export const LANES = 8;
export const SALT = 0x6d2b79f5;

export function rotateLeft(value, bits) {
  return ((value << bits) | (value >>> (32 - bits))) | 0;
}
