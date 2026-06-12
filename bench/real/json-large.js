"use strict";
// real-world bench 2: deterministically build a nested object tree (~150k
// nodes, depth ~8, mixed arrays/objects/strings/numbers/bools/null),
// JSON.stringify it, JSON.parse it back, deep-walk summing numbers and
// counting nodes. Numbers are integers or integer+0.5 so stringify/parse and
// the printed sum are exact on every engine.
function mulberry32(seed) {
  var a = seed | 0;
  return function () {
    a = (a + 0x6D2B79F5) | 0;
    var t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
function fnv1a(str) {
  var h = 0x811c9dc5;
  for (var i = 0; i < str.length; i++) {
    h = Math.imul(h ^ str.charCodeAt(i), 16777619);
  }
  return h >>> 0;
}
var rnd = mulberry32(0x15A4E5);
function ri(n) { return (rnd() * n) | 0; }
var WORDS = [];
var LETTERS = "abcdefghijklmnopqrstuvwxyz";
for (var wi = 0; wi < 256; wi++) {
  var w = "";
  var wl = 3 + ri(10);
  for (var wj = 0; wj < wl; wj++) w += LETTERS[ri(26)];
  WORDS.push(w);
}

var TARGET = 150000;
var built = 0;
function leaf() {
  built++;
  var t = ri(6);
  if (t === 0) return ri(1000000);
  if (t === 1) return ri(2000) + 0.5;
  if (t === 2) return WORDS[ri(256)] + "-" + ri(1000);
  if (t === 3) return true;
  if (t === 4) return false;
  return null;
}
function build(depth) {
  if (depth >= 8 || built >= TARGET) return leaf();
  built++;
  var kids = 2 + ri(5);
  if (ri(2) === 0) {
    var arr = [];
    for (var i = 0; i < kids; i++) arr.push(build(depth + 1));
    return arr;
  }
  var obj = {};
  for (var j = 0; j < kids; j++) obj[WORDS[ri(256)] + "_" + j] = build(depth + 1);
  return obj;
}
var roots = [];
while (built < TARGET) roots.push(build(0));
var tree = { meta: { built: built, roots: roots.length }, data: roots };

// deep walk: count nodes per type, sum numbers (x2 so the total stays integer)
var nodes = 0, numSum2x = 0, strs = 0, bools = 0, nulls = 0, strLen = 0;
function walk(v) {
  nodes++;
  if (v === null) { nulls++; return; }
  var t = typeof v;
  if (t === "number") { numSum2x += v * 2; return; }
  if (t === "string") { strs++; strLen += v.length; return; }
  if (t === "boolean") { bools++; return; }
  if (Array.isArray(v)) {
    for (var i = 0; i < v.length; i++) walk(v[i]);
    return;
  }
  for (var k in v) walk(v[k]);
}
var ROUNDS = 6, json = "", jhash = 0;
for (var round = 0; round < ROUNDS; round++) {
  json = JSON.stringify(tree);
  jhash = (jhash + fnv1a(json)) >>> 0;
  var back = JSON.parse(json);
  nodes = 0; numSum2x = 0; strs = 0; bools = 0; nulls = 0; strLen = 0;
  walk(back);
}
console.log("jsonLen=" + json.length + " jsonHashAcc=" + jhash);
console.log("nodes=" + nodes + " numSum2x=" + numSum2x + " strs=" + strs + " strLen=" + strLen + " bools=" + bools + " nulls=" + nulls);
