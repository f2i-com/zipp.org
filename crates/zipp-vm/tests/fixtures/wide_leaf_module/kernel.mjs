export const SALT = 0x6d2b79f5;
export const LANES = 8;
export let live = 11;

export function bump() {
  live = (live + 1) | 0;
}

export function setLive(value) {
  live = value;
}

export function rotateLeft(value, bits) {
  return ((value << bits) | (value >>> (32 - bits))) | 0;
}

export function transform(value, lane) {
  let mixed = (value ^ SALT ^ live ^ lane) | 0;
  mixed = Math.imul(mixed ^ (mixed >>> 16), 0x45d9f3b) | 0;
  mixed = rotateLeft(mixed, (lane & (LANES - 1)) + 5);
  mixed = Math.imul(mixed ^ (mixed >>> 13), 0x119de1f3) | 0;
  return (mixed ^ (mixed >>> 16)) | 0;
}
