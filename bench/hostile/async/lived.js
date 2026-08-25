"use strict";

(async function main() {
  const batches = 900;
  const width = 96;
  let total = 0;
  let mutable = 11;

  function makeTask(seed) {
    let calls = 0;
    return async function task(value) {
      calls++;
      await Promise.resolve();
      mutable = (mutable + seed + calls) | 0;
      return (Math.imul(value ^ mutable, 33) + seed) | 0;
    };
  }

  const tasks = [];
  for (let i = 0; i < 12; i++) tasks.push(makeTask(i * 13 + 5));

  for (let batch = 0; batch < batches; batch++) {
    const pending = new Array(width);
    for (let i = 0; i < width; i++) {
      const task = tasks[(batch + i) % tasks.length];
      pending[i] = task((batch * width + i) | 0);
    }
    const values = await Promise.all(pending);
    for (let i = 0; i < values.length; i++) total = (total + values[i]) | 0;
    if ((batch & 31) === 0) await Promise.resolve(batch);
  }

  console.log("async-lived", total, mutable, batches * width, tasks.length);
})();
