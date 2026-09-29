// Exact i32 decimal formatting through both string-concatenation directions.
'use strict';
const assert = require('node:assert/strict');
const {Engine} = require('./pkg/zipp_wasm.js');
const numbers = [-2147483648, -2147483647, -1, 0, 1, 2147483646, 2147483647];
for (let power = 10; power <= 1e9; power *= 10) {
  for (let delta = -1; delta <= 1; delta++) numbers.push(power + delta, -power + delta);
}
let sample = 0;
for (let i = 0; i < 2048; i++) {
  sample = (Math.imul(sample, 1664525) + 1013904223) | 0;
  numbers.push(sample);
}
const expected = numbers.map(String);
const engine = new Engine();
try {
  engine.initScript(`
    const numbers = ${JSON.stringify(numbers)};
    const expected = ${JSON.stringify(expected)};
    for (let i = 0; i < numbers.length; i++) {
      const n = numbers[i], s = expected[i];
      if ("L" + n !== "L" + s || n + "R" !== s + "R") throw Error("format:" + i);
      let a = "A"; a += n;
      if (a !== "A" + s) throw Error("append:" + i);
    }
    print("ok", numbers.length);
  `);
  assert.deepEqual(engine.takeOutput(), ['ok ' + numbers.length]);
} finally { engine.dispose(); engine.free(); }
console.log('i32 decimal boundaries and sampled values match Node');
