"use strict";

(function main() {
  const rounds = 1800000;

  function parseRecord(value, index) {
    if ((index & 15) === 0) return -(value + index);
    if ((index & 31) === 7) return (value ^ index) + 17;
    return Math.imul(value + (index & 255), 31) | 0;
  }

  let state = 19;
  let failures = 0;
  let checksum = 0;
  for (let i = 0; i < rounds; i++) {
    const next = parseRecord(state, i);
    if ((i & 15) === 0 || (i & 31) === 7) failures++;
    state = next | 0;
    checksum = (checksum + (state & 65535)) | 0;
  }
  console.log("branch-control", state, checksum, failures, rounds);
})();
