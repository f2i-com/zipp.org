"use strict";
// real-world bench 8: promise/microtask throughput.
// (a) a long then-chain built iteratively, (b) an async function looping
// awaits of an already-resolved promise, (c) Promise.all over many small
// batches. Ends deterministically via a final .then; no timers.
var CHAIN = 1500000;  // then-chain links
var AWAITS = 1500000; // awaits of a resolved promise
var BATCHES = 30000, WIDTH = 100; // Promise.all batches

function addOne(x) { return x + 1; }

function partA() {
  var p = Promise.resolve(0);
  for (var i = 0; i < CHAIN; i++) p = p.then(addOne);
  return p;
}

async function partB() {
  var pre = Promise.resolve(3);
  var s = 0;
  for (var i = 0; i < AWAITS; i++) s = (s + await pre) | 0;
  return s;
}

async function partC() {
  var total = 0;
  for (var b = 0; b < BATCHES; b++) {
    var arr = new Array(WIDTH);
    for (var j = 0; j < WIDTH; j++) arr[j] = Promise.resolve(((b & 1023) + j) | 0);
    var vals = await Promise.all(arr);
    for (var k = 0; k < WIDTH; k++) total = (total + vals[k]) | 0;
  }
  return total;
}

partA().then(function (a) {
  return partB().then(function (b) {
    return partC().then(function (c) {
      console.log("chain=" + a + " awaitSum=" + b + " allSum=" + c);
    });
  });
});
