"use strict";

(function main() {
  const rounds = 3600000;

  function fold(value, index) {
    value = (value + (index & 255)) | 0;
    value = Math.imul(value ^ (index >>> 3), 33);
    return value | 0;
  }

  let value = 7;
  let checksum = 0;
  for (let i = 0; i < rounds; i++) {
    value = fold(value, i);
    checksum = (checksum ^ value) | 0;
  }
  console.log("types-stable", value, checksum, rounds);
})();
