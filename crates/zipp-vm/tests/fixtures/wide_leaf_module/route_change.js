routeSalt = 0x6d2b79f5;
routeLanes = 8;
routeLive = 11;
routeReads = 0;

function routeTransform(value, lane) {
  let mixed = (value ^ routeSalt ^ routeLive ^ lane) | 0;
  mixed = Math.imul(mixed ^ (mixed >>> 16), 0x45d9f3b) | 0;
  mixed = (mixed ^ ((lane & (routeLanes - 1)) << 5)) | 0;
  mixed = Math.imul(mixed ^ (mixed >>> 13), 0x119de1f3) | 0;
  return (mixed ^ (mixed >>> 16)) | 0;
}

function routeChunk(start, end, checksum) {
  for (let i = start; i < end; i++) {
    checksum = routeTransform((checksum + i) | 0, i & 7);
  }
  return checksum;
}

let routeChecksum = routeChunk(0, 90000, 0x12345678);
Object.defineProperty(globalThis, "routeLive", {
  configurable: true,
  get: function () {
    routeReads++;
    return 13;
  },
});
routeChecksum = routeChunk(90000, 180000, routeChecksum);

console.log(routeChecksum + "|" + routeReads);
