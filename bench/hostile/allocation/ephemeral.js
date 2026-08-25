"use strict";

(function main() {
  const rounds = 1400000;
  let checksum = 0;

  for (let i = 0; i < rounds; i++) {
    const point = { x: i & 1023, y: (i * 3) & 2047, tag: "p" + (i & 31) };
    const pair = [point.x + point.y, point.tag.length];
    checksum = (checksum + pair[0] + pair[1]) | 0;
  }

  console.log("allocation-ephemeral", checksum, rounds);
})();
