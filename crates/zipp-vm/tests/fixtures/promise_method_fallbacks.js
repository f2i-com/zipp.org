// Promise catch/finally fallbacks: observable getters and settlement order.
(function () {
  var p = Promise.resolve(1);
  for (var i = 0; i < 100; i++) p.then(function () {});
  var log = [];
  Object.defineProperty(p, "then", { configurable: true, get: function () {
    log.push("then");
    return function (a, b) { log.push("args:" + a + ":" + b); return 99; };
  }});
  console.log("catch:" + p.catch(7) + ":" + log.join(","));
  log = [];
  Object.defineProperty(p, "constructor", { get: function () {
    log.push("constructor");
    var c = {};
    Object.defineProperty(c, Symbol.species, { get: function () {
      log.push("species"); return Promise;
    }});
    return c;
  }});
  console.log("finally:" + p.finally(7) + ":" + log.join(","));
})();

(function () {
  var p = Promise.resolve(1);
  for (var i = 0; i < 100; i++) p.then(function () {});
  var own = Object.getOwnPropertyDescriptor(Promise, Symbol.species);
  var parent = Object.getPrototypeOf(Promise);
  var reads = 0;
  delete Promise[Symbol.species];
  Object.defineProperty(parent, Symbol.species, { configurable: true,
    get: function () { reads++; return Promise; }
  });
  p.finally(function () {});
  console.log("inherited:" + (reads > 0));
  delete parent[Symbol.species];
  Object.defineProperty(Promise, Symbol.species, own);
})();

(function () {
  Promise.resolve(0).then(function () {});
  Promise.resolve(5).finally(function () {}).then(function (v) { console.log("fulfilled:" + v); });
  Promise.reject("reason").finally(function () {}).catch(function (e) { console.log("rejected:" + e); });
  Promise.resolve(7).finally(null).then(function (v) { console.log("null:" + v); });
})();
