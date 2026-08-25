"use strict";

// Deliberately awkward call traffic: nested factories, mutable captured cells,
// arrows, defaults, and sixteen targets selected at one call site.
(function main() {
  const rounds = 2400000;

  function makePipeline(seed) {
    let calls = 0;
    let offset = seed | 0;

    function rotate(value, amount = 5) {
      return (value << amount) | (value >>> (32 - amount));
    }

    return (value, salt) => {
      calls = (calls + 1) | 0;
      if ((calls & 1023) === 0) {
        offset = (offset + seed + calls) | 0;
      }
      value = Math.imul(value ^ salt ^ offset, 1664525);
      return (rotate(value, (seed & 7) + 1) + 1013904223) | 0;
    };
  }

  const pipelines = [];
  for (let i = 0; i < 16; i++) {
    pipelines.push(makePipeline((i * 97 + 11) | 0));
  }

  let state = 0x13579bdf | 0;
  let checksum = 0;
  for (let i = 0; i < rounds; i++) {
    const fn = pipelines[i & 15];
    state = fn(state, i & 1023);
    checksum = (checksum + (state & 65535)) | 0;
  }

  console.log("calls-closures", state, checksum, rounds, pipelines.length);
})();
