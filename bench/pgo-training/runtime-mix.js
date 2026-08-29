"use strict";

// PGO-only workload.  This is intentionally independent of every scored
// benchmark: it exercises representative VM paths without training their exact
// programs or data.
function next(state) {
  state.value = Math.imul(state.value ^ (state.value >>> 13), 1597334677) | 0;
  return state.value >>> 0;
}

class Bucket {
  constructor(tag) {
    this.tag = tag;
    this.total = 0;
    this.count = 0;
  }
  add(value) {
    this.total = (this.total + value) | 0;
    this.count++;
  }
  score() {
    return (this.total ^ Math.imul(this.count, 97) ^ this.tag.length) | 0;
  }
}

var state = { value: 0x4f1bbcdc };
var buckets = [new Bucket("red"), new Bucket("green"), new Bucket("blue")];
var seen = new Set();
var counts = new Map();
var values = [];
for (var i = 0; i < 280000; i++) {
  var value = next(state) & 65535;
  var bucket = buckets[value % buckets.length];
  bucket.add(value);
  seen.add(value & 4095);
  var key = "k" + (value & 63);
  counts.set(key, (counts.get(key) || 0) + 1);
  if ((i & 63) === 0) values.push(value);
}

values.sort(function (a, b) { return a - b; });
var folded = values
  .map(function (value, index) { return (value ^ index) & 65535; })
  .filter(function (value) { return (value & 3) !== 0; })
  .reduce(function (sum, value) { return (sum + value) | 0; }, 0);
var answer = folded ^ seen.size ^ counts.size;
for (var j = 0; j < buckets.length; j++) answer ^= buckets[j].score();
console.log(answer | 0);
