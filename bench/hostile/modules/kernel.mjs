import { LANES, SALT, rotateLeft } from "./constants.mjs";
import { version } from "./state.mjs";

export default function transform(value, lane) {
  let mixed = (value ^ SALT ^ version ^ lane) | 0;
  mixed = Math.imul(mixed ^ (mixed >>> 16), 0x45d9f3b) | 0;
  mixed = rotateLeft(mixed, (lane & (LANES - 1)) + 5);
  mixed = Math.imul(mixed ^ (mixed >>> 13), 0x119de1f3) | 0;
  return (mixed ^ (mixed >>> 16)) | 0;
}

export function consume(seed, count) {
  let value = seed | 0;
  for (let i = 0; i < count; i++) {
    value = transform((value + i) | 0, i & (LANES - 1));
  }
  return value;
}
