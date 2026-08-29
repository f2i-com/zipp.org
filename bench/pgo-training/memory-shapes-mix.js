"use strict";

// Independent typed/dense/sparse/object-shape training workload.
function readPoint(point) {
  return ((point.x || 0) + (point.y || 0) + (point.z || 0) + (point.w || 0)) | 0;
}

var points = [];
for (var i = 0; i < 90000; i++) {
  switch (i & 3) {
    case 0: points.push({ x: i, y: i + 1 }); break;
    case 1: points.push({ y: i + 1, x: i, z: i + 2 }); break;
    case 2: points.push({ x: i, w: i + 3 }); break;
    default: points.push({ z: i + 2, y: i + 1, x: i }); break;
  }
}

var words = new Uint32Array(131072);
var checksum = 0;
for (var round = 0; round < 12; round++) {
  for (var j = 0; j < words.length; j++) {
    words[j] = (Math.imul(j + round, 2246822519) ^ (j >>> 3)) >>> 0;
  }
  for (var j = 0; j < words.length; j += 17) checksum ^= words[j];
}

var sparse = [];
for (var k = 0; k < 18000; k++) sparse[k * 19] = (k * 13) & 65535;
for (var k = 0; k < 18000; k++) checksum ^= sparse[k * 19];
for (var k = 0; k < points.length; k++) checksum = (checksum + readPoint(points[k])) | 0;
console.log(checksum | 0);
