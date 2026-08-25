"use strict";

// Control for calls-closures.js: one stable leaf target, function-local
// let/const bindings, and the same amount of input data.
(function main() {
  const rounds = 2400000;

  function step(value, salt) {
    value = Math.imul(value ^ salt, 1664525);
    return (value + 1013904223) | 0;
  }

  let state = 0x13579bdf | 0;
  let checksum = 0;
  for (let i = 0; i < rounds; i++) {
    state = step(state, i & 1023);
    checksum = (checksum + (state & 65535)) | 0;
  }

  console.log("calls-baseline", state, checksum, rounds);
})();
