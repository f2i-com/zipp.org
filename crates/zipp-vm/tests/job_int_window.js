// Reaction callbacks the host runs as jobs, with no frame under them, whose
// bodies the int tier compiles: hot chains, a bail past the entry (an int
// overflow to a double, a division), self-recursion deep enough to leave the
// native path, a reaction that calls one later in its life, and strict `this`.
"use strict";
var log = [];
function L(s) { log.push(s); }

function addOne(x) { return x + 1; }
function dbl(x) { return x * 2; }
function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
function down(n) { return n <= 0 ? 0 : down(n - 1) + 1; }
function half(x) { return x / 2; }
function thisOf() { return this; }

var steps = [];
function step(name, fn) { steps.push([name, fn]); }

step("chain", function () {
  var p = Promise.resolve(0);
  for (var i = 0; i < 5000; i++) p = p.then(addOne);
  return p.then(function (v) { L("chain " + v); });
});
step("overflow to double", function () {
  var p = Promise.resolve(1);
  for (var i = 0; i < 40; i++) p = p.then(dbl);
  return p.then(function (v) { L("dbl " + v); });
});
step("division bail", function () {
  var p = Promise.resolve(1 << 20);
  for (var i = 0; i < 25; i++) p = p.then(half);
  return p.then(function (v) { L("half " + v); });
});
step("fib as reaction", function () {
  var ps = [];
  for (var i = 0; i < 300; i++) ps.push(Promise.resolve(i % 20).then(fib));
  return Promise.all(ps).then(function (v) { L("fib sum " + v.reduce(function (a, b) { return a + b; }, 0)); });
});
step("deep recursion", function () {
  var p = Promise.resolve(0);
  for (var i = 0; i < 300; i++) p = p.then(function (k) { return function () { return k; }; }(i)).then(addOne);
  return p.then(function () { return Promise.resolve(5000).then(down); }).then(function (v) { L("down " + v); });
});
step("this is undefined", function () {
  return Promise.resolve(1).then(thisOf).then(function (v) { L("this " + v); });
});
step("rejection callback", function () {
  var p = Promise.reject(10);
  return p.then(null, addOne).then(function (v) { L("rejected+1 " + v); });
});
step("mixed callbacks", function () {
  var p = Promise.resolve(3);
  for (var i = 0; i < 2000; i++) p = p.then(i % 3 === 0 ? addOne : i % 3 === 1 ? dbl : half);
  return p.then(function (v) { L("mixed " + v); });
});

var chain = Promise.resolve();
steps.forEach(function (s) {
  chain = chain.then(function () { L("-- " + s[0]); return s[1](); });
});
chain.then(function () { console.log(log.join("\n")); }, function (e) { console.log(log.join("\n") + "\nFAILED " + e); });
