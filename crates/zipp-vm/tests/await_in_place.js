// Settled awaits in an activation that runs alone (the queue empty), where
// an engine may continue in place, against jobs that appear mid-run.
var log = [];
function L(s) { log.push(s); }

async function s1() {
  // primitives and settled promises, alone
  var p = Promise.resolve(5), s = 0;
  for (var i = 0; i < 2000; i++) s = (s + await p + await i + (await ("" + i)).length) | 0;
  L("s1 " + s);
}
async function s2() {
  // a job queued from inside the loop runs before the next resume
  for (var i = 0; i < 6; i++) {
    if (i % 2 === 0) Promise.resolve(i).then(function (v) { L("s2 job " + v); });
    var v = await i;
    L("s2 after " + v);
  }
}
async function s3() {
  // a then on the async function's own result promise, and a then registered
  // on a settled promise before an await
  var done = Promise.resolve("x");
  done.then(function (v) { L("s3 then " + v); });
  L("s3 a");
  await 1;
  L("s3 b");
  await 2;
  L("s3 c");
  return "r3";
}
async function s4() {
  // rejection mid-loop, caught, then settled awaits continue
  var s = 0;
  for (var i = 0; i < 5; i++) {
    try {
      if (i === 2) await Promise.reject(new Error("e" + i));
      s += await i;
    } catch (e) { L("s4 caught " + e.message); }
  }
  L("s4 " + s);
}
async function s5() {
  // await of an async call that itself awaits settled values
  async function sub(x) { var a = await x; var b = await (x + 1); return a + b; }
  var t = 0;
  for (var i = 0; i < 4; i++) { var r = await sub(i); L("s5 r " + r); t += r; }
  L("s5 " + t);
}
async function s6() {
  // a generator and a sync callback in between awaits
  function* g() { yield 1; yield 2; yield 3; }
  var s = 0;
  for (var v of g()) s += await v;
  [1, 2, 3].forEach(function (x) { s += x; });
  s += await [4, 5].reduce(function (a, b) { return a + b; });
  L("s6 " + s);
}
async function s7() {
  // `arguments` and `this` survive in-place continuation
  async function f(a, b) { await a; arguments[0] = 9; var x = await b; return a + " " + x + " " + arguments.length; }
  L("s7 " + await f(1, 2));
  var o = { k: 3, m: async function () { await 0; await 1; return this.k; } };
  L("s7 this " + await o.m());
}
async function s8() {
  // an object with no then, a function, an array: awaited values that are objects
  var o = { a: 1 }, f = function () { return 2; }, a = [3];
  var r1 = await o, r2 = await f, r3 = await a;
  L("s8 " + (r1 === o) + " " + (r2 === f) + " " + (r3 === a));
}
async function s9() {
  // budget boundary: many settled awaits, with a chain started at a known point
  var s = 0;
  for (var i = 0; i < 2500; i++) {
    if (i === 1500) Promise.resolve().then(function () { L("s9 job at " + s); });
    s = (s + await 1) | 0;
  }
  L("s9 " + s);
}
async function s10() {
  // Promise.all over settled promises, then awaiting its result
  var arr = [];
  for (var i = 0; i < 10; i++) arr.push(Promise.resolve(i));
  var v = await Promise.all(arr);
  L("s10 " + v.join(","));
  var w = await Promise.all([1, Promise.resolve(2), 3]);
  L("s10b " + w.join(","));
}

async function s11() {
  // `then` getters that start an async generator or an async function at
  // the frame depth the resumed activation just left: their awaits suspend
  await null;
  async function* ag(tag) {
    L(tag + " start");
    var v = await 1;
    L(tag + " after await " + v);
    Promise.resolve().then(function () { L(tag + " post-await job"); });
    yield v;
  }
  var it = ag("ag1");
  var o = { get then() { L("getter1"); it.next().then(function (r) { L("ag1 next " + r.value); }); L("getter1 end"); return undefined; } };
  var r = await o;
  L("s11 awaited " + (r === o));
  var o2 = { get then() { L("getter2"); (async function () { L("af start"); await 2; L("af after"); })(); L("getter2 end"); return undefined; } };
  var r2 = await o2;
  L("s11 awaited2 " + (r2 === o2));
  // a getter that is the generator's own bound `next`: no frame of its own,
  // so the generator body runs at the depth the activation left
  var it4 = ag("ag4");
  var o4 = {};
  Object.defineProperty(o4, "then", { get: it4.next.bind(it4) });
  Promise.resolve().then(function () { L("s11 queued job"); });
  var r4 = await o4;
  L("s11 awaited4 " + (r4 === o4));
  var it5 = ag("ag5");
  var o5 = {};
  Object.defineProperty(o5, "then", { get: it5.next.bind(it5) });
  L("s11 before await o5 alone");
  var r5 = await o5;
  L("s11 awaited5 " + (r5 === o5));
  var it3 = ag("ag3");
  var o3 = { get then() { L("getter3"); it3.next(); L("getter3 end"); return undefined; } };
  return o3;
}

async function s12() {
  // the bound-`next` getter again, with nothing else queued
  async function* ag(tag) {
    L(tag + " start");
    var v = await 1;
    L(tag + " after await " + v);
    Promise.resolve().then(function () { L(tag + " post-await job"); });
    yield v;
  }
  await null;
  await null;
  await null;
  var it = ag("ag6");
  var o = {};
  Object.defineProperty(o, "then", { get: it.next.bind(it) });
  var r = await o;
  L("s12 awaited " + (r === o));
  var it2 = ag("ag7");
  var o2 = {};
  Object.defineProperty(o2, "then", { get: it2.next.bind(it2) });
  await null;
  await null;
  return o2;
}

var steps = [s1, s2, s3, s4, s5, s6, s7, s8, s9, s10, s11, s12];
var p = Promise.resolve();
steps.forEach(function (fn) {
  p = p.then(function () { L("-- " + fn.name); return fn().then(function (r) { L("ret " + r); }); });
});
p.then(function () { console.log(log.join("\n")); });
