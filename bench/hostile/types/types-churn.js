"use strict";

(function main() {
  const rounds = 1200000;

  // The same local intentionally moves through int, double, string, object,
  // boolean and null states before returning to an integer accumulator.
  function fold(seed, index) {
    let value = (seed + (index & 255)) | 0;
    switch (index & 7) {
      case 0:
        value = value + 0.5;
        return (value * 2) | 0;
      case 1:
        value = "v" + (value & 1023);
        return (value.length + seed) | 0;
      case 2:
        value = { number: value, text: "x" + (index & 31) };
        return (value.number + value.text.length) | 0;
      case 3:
        value = (value & 1) === 0;
        return value ? (seed ^ index) : (seed + index);
      case 4:
        value = null;
        return value === null ? (seed - index) | 0 : seed;
      case 5:
        value = [value, index & 63];
        return (value[0] + value[1]) | 0;
      case 6:
        value = String(value);
        return (seed + value.charCodeAt(0)) | 0;
      default:
        return Math.imul(value ^ (index >>> 3), 33) | 0;
    }
  }

  let value = 7;
  let checksum = 0;
  for (let i = 0; i < rounds; i++) {
    value = fold(value, i);
    checksum = (checksum ^ value) | 0;
  }
  console.log("types-churn", value, checksum, rounds);
})();
