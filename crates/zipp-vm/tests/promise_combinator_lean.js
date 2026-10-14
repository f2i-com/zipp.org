// Promise.all and friends where the capability, GetPromiseResolve's Get,
// GetIterator's @@iterator Get and the result's `then` lookup are observable
// or patched. Each step runs alone and logs in order; Node's output beside.
var log = [];
function L(s) { log.push(s); }
function show(v) {
  if (Array.isArray(v)) return "[" + v.map(show).join(",") + "]";
  if (v && typeof v === "object") return "{" + Object.keys(v).map(function (k) { return k + ":" + show(v[k]); }).join(",") + "}";
  return String(v);
}

var steps = [];
function step(name, fn) { steps.push([name, fn]); }

step("plain", function () {
  return Promise.all([1, Promise.resolve(2), 3]).then(function (v) { L("plain " + show(v)); });
});
step("empty", function () {
  return Promise.all([]).then(function (v) { L("empty " + show(v) + " " + Array.isArray(v)); });
});
step("ordering", function () {
  var p = Promise.all([Promise.resolve(1), 2]);
  Promise.resolve().then(function () { L("ord job 1"); }).then(function () { L("ord job 2"); }).then(function () { L("ord job 3"); });
  return p.then(function (v) { L("ord all " + show(v)); });
});
step("resolve patched", function () {
  var orig = Promise.resolve;
  var calls = 0;
  Promise.resolve = function (x) { calls++; return orig.call(this, x); };
  var p = Promise.all([1, 2, 3]);
  Promise.resolve = orig;
  return p.then(function (v) { L("resolve patched " + show(v) + " calls " + calls); });
});
step("resolve getter", function () {
  var orig = Promise.resolve;
  var gets = 0;
  Object.defineProperty(Promise, "resolve", { configurable: true, get: function () { gets++; return orig; } });
  var p = Promise.all([1, 2]);
  Object.defineProperty(Promise, "resolve", { configurable: true, writable: true, value: orig });
  return p.then(function (v) { L("resolve getter " + show(v) + " gets " + gets); });
});
step("resolve non-callable", function () {
  var orig = Promise.resolve;
  Promise.resolve = 42;
  var p = Promise.all([1]);
  Promise.resolve = orig;
  return p.then(function (v) { L("noncallable fulfilled " + show(v)); }, function (e) { L("noncallable rejected " + e.constructor.name); });
});
step("iterator patched on array", function () {
  var a = [1, 2, 3];
  a[Symbol.iterator] = function* () { yield 7; yield 8; };
  return Promise.all(a).then(function (v) { L("own iter " + show(v)); });
});
step("iterator patched on prototype", function () {
  var orig = Array.prototype[Symbol.iterator];
  var calls = 0;
  Array.prototype[Symbol.iterator] = function () { calls++; return orig.call(this); };
  var p = Promise.all([4, 5]);
  Array.prototype[Symbol.iterator] = orig;
  return p.then(function (v) { L("proto iter " + show(v) + " calls " + calls); });
});
step("iterator getter on prototype", function () {
  var orig = Array.prototype[Symbol.iterator];
  var gets = 0;
  Object.defineProperty(Array.prototype, Symbol.iterator, { configurable: true, get: function () { gets++; return orig; } });
  var p = Promise.all([6]);
  Object.defineProperty(Array.prototype, Symbol.iterator, { configurable: true, writable: true, value: orig });
  return p.then(function (v) { L("iter getter " + show(v) + " gets " + gets); });
});
step("next patched", function () {
  var ai = Object.getPrototypeOf([][Symbol.iterator]());
  var orig = ai.next;
  var calls = 0;
  ai.next = function () { calls++; return orig.call(this); };
  var p = Promise.all([1, 2]);
  ai.next = orig;
  return p.then(function (v) { L("next patched " + show(v) + " calls " + calls); });
});
step("array then on Array.prototype", function () {
  Array.prototype.then = function (res) { L("array then called"); res("from then"); };
  var p = Promise.all([1, 2]);
  return p.then(function (v) {
    delete Array.prototype.then;
    L("array then result " + show(v));
  });
});
step("then on Object.prototype", function () {
  var gets = 0;
  Object.defineProperty(Object.prototype, "then", { configurable: true, get: function () { gets++; return undefined; } });
  var p = Promise.all([3]);
  return p.then(function (v) {
    delete Object.prototype.then;
    L("object then getter " + show(v) + " gets>0 " + (gets > 0));
  });
});
step("async returns array after then removed", function () {
  async function f() { return [1, 2]; }
  return f().then(function (v) { L("async array " + show(v)); });
});
step("resolve with array having own then", function () {
  var a = [9];
  a.then = function (res) { L("own then"); res("own"); };
  return new Promise(function (r) { r(a); }).then(function (v) { L("own then result " + show(v)); });
});
step("subclass all", function () {
  class P extends Promise {
    constructor(ex) { L("P ctor"); super(ex); }
  }
  var p = P.all([1, P.resolve(2)]);
  L("subclass instance " + (p instanceof P));
  return p.then(function (v) { L("subclass all " + show(v)); });
});
step("all.call custom ctor", function () {
  function C(ex) {
    var self = this;
    ex(function (v) { L("custom resolve " + show(v)); }, function (e) { L("custom reject " + e); });
  }
  C.resolve = function (x) { return Promise.resolve(x); };
  var r = Promise.all.call(C, [1, 2]);
  L("custom ret " + (r instanceof C));
  return Promise.resolve().then(function () {}).then(function () {}).then(function () {});
});
step("withResolvers and resolve twice", function () {
  var w = Promise.withResolvers();
  w.resolve(1);
  w.resolve(2);
  w.reject(3);
  return w.promise.then(function (v) { L("withResolvers " + v); });
});
step("new Promise resolve then reject", function () {
  var p = new Promise(function (res, rej) { res("a"); rej("b"); res("c"); });
  var q = new Promise(function (res, rej) { rej("x"); res("y"); });
  return p.then(function (v) { L("np " + v); }).then(function () { return q; }).catch(function (e) { L("nq " + e); });
});
step("allSettled any race", function () {
  return Promise.allSettled([1, Promise.reject(2)]).then(function (v) {
    L("allSettled " + show(v));
    return Promise.any([Promise.reject(1), 2]);
  }).then(function (v) {
    L("any " + v);
    return Promise.race([Promise.resolve(3), 4]);
  }).then(function (v) { L("race " + v); });
});
step("all rejects", function () {
  return Promise.all([1, Promise.reject(new Error("boom")), 3]).then(function () { L("no"); }, function (e) { L("all rejected " + e.message); });
});
step("holes and array-likes", function () {
  var a = [1, , 3];
  return Promise.all(a).then(function (v) { L("holes " + show(v)); });
});
step("array with own props", function () {
  var a = [1, 2];
  a.extra = 5;
  return Promise.all(a).then(function (v) { L("own props " + show(v)); });
});
step("resolved pairs many", function () {
  var ps = [];
  for (var i = 0; i < 200; i++) {
    ps.push(new Promise(function (res, rej) { res(i); res(-1); rej(-2); }));
  }
  return Promise.all(ps).then(function (v) { L("pairs sum " + v.reduce(function (a, b) { return a + b; }, 0)); });
});
step("await Promise.all in loop", function () {
  return (async function () {
    var t = 0;
    for (var i = 0; i < 50; i++) {
      var v = await Promise.all([i, Promise.resolve(i * 2)]);
      t += v[0] + v[1];
    }
    L("loop all " + t);
  })();
});

var chain = Promise.resolve();
steps.forEach(function (s) {
  chain = chain.then(function () { L("-- " + s[0]); return s[1](); });
});
chain.then(function () { console.log(log.join("\n")); }, function (e) { console.log(log.join("\n") + "\nFAILED " + e); });
