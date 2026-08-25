"use strict";

(function main() {
  const rounds = 1800000;

  function parseRecord(value, index) {
    if ((index & 15) === 0) throw -(value + index);
    if ((index & 31) === 7) throw { code: index & 255, value: value ^ index };
    return Math.imul(value + (index & 255), 31) | 0;
  }

  let state = 19;
  let failures = 0;
  let checksum = 0;
  for (let i = 0; i < rounds; i++) {
    try {
      state = parseRecord(state, i);
    } catch (error) {
      failures++;
      if (typeof error === "number") {
        state = error | 0;
      } else {
        state = (error.value + error.code + 17) | 0;
      }
    }
    checksum = (checksum + (state & 65535)) | 0;
  }
  console.log("throw-catch", state, checksum, failures, rounds);
})();
