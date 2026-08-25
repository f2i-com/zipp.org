"use strict";

let randomState = 0x9e3779b9;
Math.random = function seededRandom() {
  randomState ^= randomState << 13;
  randomState ^= randomState >>> 17;
  randomState ^= randomState << 5;
  return (randomState >>> 0) / 4294967296;
};

const { customAlphabet, nanoid } = await import(
  "./nanoid-3.3.17/non-secure/index.mjs"
);

const compactId = customAlphabet("346789ABCDEFGHJKLMNPQRTUVWXY", 17);
const ITERATIONS = 240000;
let checksum = 0x811c9dc5;
let characters = 0;

for (let i = 0; i < ITERATIONS; i++) {
  const id = (i & 1) === 0 ? nanoid(21) : compactId();
  characters += id.length;
  for (let j = 0; j < id.length; j++) {
    checksum = Math.imul(checksum ^ id.charCodeAt(j), 0x01000193) >>> 0;
  }
}

console.log(
  "nanoid checksum=" + checksum +
    " characters=" + characters +
    " randomState=" + (randomState >>> 0)
);
