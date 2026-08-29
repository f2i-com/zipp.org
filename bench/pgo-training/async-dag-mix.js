"use strict";

// A bounded promise dependency graph for release-profile training.  The scored
// async row is a serial chain + repeated await + fixed-width batch pipeline;
// this input deliberately uses many short branches, fan-in nodes, and recovered
// rejections instead.  It exercises the same public Promise machinery without
// reproducing the scored program's topology.
function stir(value, salt) {
  var mixed = Math.imul((value ^ salt) | 0, 1103515245) | 0;
  return (mixed + 12345 + (mixed >>> 11)) | 0;
}

function branch(seed, lane) {
  var root = Promise.resolve(stir(seed, lane + 17));
  var left = root.then(function (value) {
    return stir(value, 0x1a2b3c4d ^ lane);
  });
  var right = root.then(function (value) {
    return stir(value, 0x2468ace1 + lane);
  }).then(function (value) {
    return stir(value, seed ^ 0x10203040);
  });
  var recovered;
  if (((seed + lane) & 31) === 0) {
    recovered = Promise.reject(stir(seed, lane)).then(undefined, function (reason) {
      return stir(reason, 0x55aa33cc);
    });
  } else {
    recovered = root.then(function (value) {
      return stir(value, 0x7f4a7c15);
    });
  }
  return Promise.all([left, right, recovered]).then(function (values) {
    return stir(values[0] ^ values[1], values[2]);
  });
}

async function foldShard(shard, width) {
  var pending = [];
  for (var lane = 0; lane < width; lane++) {
    pending.push(branch((shard * 257 + lane * 13) | 0, lane));
  }
  var values = await Promise.all(pending);
  var fold = (0x5b8d80f1 ^ shard) | 0;
  for (var i = 0; i < values.length; i++) fold = stir(fold, values[i]);
  return fold;
}

var SHARDS = 192;
var WIDTH = 92;
var graph = [];
for (var shard = 0; shard < SHARDS; shard++) graph.push(foldShard(shard, WIDTH));

Promise.all(graph).then(function (values) {
  var answer = 0x31415926;
  for (var i = 0; i < values.length; i++) answer = stir(answer, values[i]);
  var summary = "async-dag=" + values.length + ":" + WIDTH + ":" + (answer >>> 0);
  var EXPECTED = "async-dag=192:92:2743580189";
  if (summary !== EXPECTED) throw new Error("async DAG PGO checksum mismatch: " + summary);
  console.log(summary);
});
