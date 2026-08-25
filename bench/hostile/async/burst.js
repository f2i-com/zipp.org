"use strict";

(function main() {
  const rounds = 90000;
  let promise = Promise.resolve(7);

  function step(value, index) {
    return (Math.imul(value ^ index, 33) + 17) | 0;
  }

  for (let i = 0; i < rounds; i++) {
    promise = promise.then((value) => step(value, i));
  }

  promise.then((value) => {
    console.log("async-burst", value, rounds);
  });
})();
