// Host bridge preamble, prepended to every script the engine compiles.
//
// The engine it replaces implemented `db`, `localStorage`, `window` and
// `host` as VM builtins in Rust. zipp is a plain JavaScript engine, so they
// are ordinary JS here, and the only channel to the host is __zippHostCall
// (strings in, string out — anything structured crosses as JSON).
//
// Everything declared here is reported to the host as a preamble name and
// filtered out of the symbol map, so none of it is mistaken for script state.

// Dictionaries keyed by guest-chosen strings have no prototype: with a plain
// object, an event named "constructor", "toString" or "__proto__" found an
// inherited value where a listener array was expected and threw (the
// 6 September 2026 audit's Z08). Every read of them below is an own-key read.
var __zEvents = Object.create(null);
var __zHostQueue = [];
var __zHostCbs = Object.create(null);
var __zHostPending = 0;
var __zHostId = 0;
// Bounds on what a guest may leave waiting for the host: requests queued
// between drains, callbacks awaiting a completion, and the size of one
// request (kind plus arguments, in UTF-16 code units) so that any request
// this accepts can cross the engine's per-drain conversion budget. These are
// guest-visible bookkeeping, not the host's authority: the engine bounds the
// drain again on its own side and settles a request that still cannot cross.
var __zHostQueueMax = 4096;
var __zHostPendingMax = 65536;
var __zHostRequestMaxUnits = 4194304;

var window = {
  addEventListener: function (type, fn) {
    if (typeof fn !== "function") return;
    var key = String(type);
    (__zEvents[key] || (__zEvents[key] = [])).push(fn);
  },
  removeEventListener: function (type, fn) {
    var a = __zEvents[String(type)];
    if (!a) return;
    for (var i = 0; i < a.length; i++) {
      if (a[i] === fn) { a.splice(i, 1); return; }
    }
  },
  // A deliberately limited facade, and the limit is the contract: the event
  // is delivered to the listeners registered on THIS object for
  // `String(evt.type)`, synchronously, and the call always reports the
  // event as not cancelled. There is no DOM here — no `Event` class, no
  // bubbling or capture, no default actions — so a script must not treat
  // `window` as a document. It used to return true without dispatching at
  // all (the 11 September 2026 audit's ZIPP-17).
  dispatchEvent: function (evt) {
    if (evt === null || typeof evt !== "object" || typeof evt.type !== "string") {
      throw new TypeError("dispatchEvent: expected an event object with a string type");
    }
    __zDispatchEvent(evt.type, evt);
    return true;
  },
};

var navigator = {
  clipboard: {
    writeText: function (t) { __zippHostCall("nav.clipboardWrite", String(t)); },
    // Parsed, like every other reply: the host JSON-encodes what it returns, so
    // reading it raw hands the script the two-character string `""` for an empty
    // clipboard — which is truthy, and guards on it take the wrong branch.
    readText: function () { return JSON.parse(__zippHostCall("nav.clipboardRead")); },
  },
};

var localStorage = {
  getItem: function (k) { return JSON.parse(__zippHostCall("ls.getItem", String(k))); },
  setItem: function (k, v) { __zippHostCall("ls.setItem", String(k), String(v)); },
  removeItem: function (k) { __zippHostCall("ls.removeItem", String(k)); },
  clear: function () { __zippHostCall("ls.clear"); },
};

var db = {
  query: function (c, opts) {
    return JSON.parse(__zippHostCall("db.query", String(c), JSON.stringify(opts === undefined ? null : opts)));
  },
  get: function (c, id) { return JSON.parse(__zippHostCall("db.get", String(c), String(id))); },
  create: function (c, d) {
    return JSON.parse(__zippHostCall("db.create", String(c), JSON.stringify(d === undefined ? {} : d)));
  },
  update: function (id, d) {
    return JSON.parse(__zippHostCall("db.update", String(id), JSON.stringify(d === undefined ? {} : d)));
  },
  delete: function (id) { __zippHostCall("db.delete", String(id)); },
  hardDelete: function (c, id) { __zippHostCall("db.hardDelete", String(c), String(id)); },
  startSync: function (room) { __zippHostCall("db.startSync", String(room)); },
  stopSync: function (room) { __zippHostCall("db.stopSync", room === undefined ? "" : String(room)); },
  getSyncStatus: function (room) {
    return JSON.parse(__zippHostCall("db.getSyncStatus", room === undefined ? "" : String(room)));
  },
  getSavedSyncRoom: function () { return JSON.parse(__zippHostCall("db.getSavedSyncRoom")); },
};

// host.call is ASYNCHRONOUS by contract: it queues a request the host drains
// after the current VM re-entry returns, and invokes the callback later via
// resolveHostCallback. It must NOT reach __zippHostCall, which is synchronous.
var host = {
  call: function (kind, args, cb) {
    // Normalize first, register second. The callback used to be stored
    // before the arguments were converted, so a conversion that threw (a
    // toString that does) left a callback registered for a request that was
    // never queued, which no completion would ever release (the 6 September
    // 2026 audit's Z09). Nothing is retained until the request is whole.
    var k = String(kind);
    var flat = [];
    var units = k.length;
    if (args) for (var i = 0; i < args.length; i++) { var s = String(args[i]); units += s.length; flat.push(s); }
    if (units > __zHostRequestMaxUnits) throw new RangeError("host.call: request exceeds the " + __zHostRequestMaxUnits + "-code-unit transport limit");
    if (__zHostQueue.length >= __zHostQueueMax) throw new RangeError("host.call: too many requests queued");
    var wantsCb = typeof cb === "function";
    if (wantsCb && __zHostPending >= __zHostPendingMax) throw new RangeError("host.call: too many requests awaiting a reply");
    var id = ++__zHostId;
    if (wantsCb) { __zHostCbs[id] = cb; __zHostPending++; }
    __zHostQueue.push({ id: id, kind: k, args: flat });
  },
};

// accel: the host compiles a numeric function the script generates -- with
// its own engine, which runs such code many times faster than this one --
// and runs it over views of the script's typed arrays. Every call is
// synchronous and answers a number. `compile(params, body)` validates and
// compiles, answering an id (or throwing); `make(id, spec)` calls that
// function with arguments described by `spec` ("NAME=g:GLOBAL" binds a view
// of a typed-array global, "NAME=c:GLOBAL" a callback into a global
// function, "NAME=a:ID" another compiled function, "NAME=t" the host's
// trace table, "NAME=n:NUMBER" a number) and answers the id of the function
// it returned; `state(GLOBAL)` names the typed array `run` passes as the
// first argument; `run(id, h)` calls a made function with (state, h);
// `install(slot, id)` fills the host's trace table. Denied unless the host
// granted it, in which case every method throws.
var accel = {
  compile: function (params, body) {
    return Number(__zippHostCall("accel.compile", JSON.stringify(params), String(body)));
  },
  make: function (id, spec) { return Number(__zippHostCall("accel.make", String(id), String(spec))); },
  state: function (name) { __zippHostCall("accel.state", String(name)); },
  run: function (id, h) { return Number(__zippHostCall("accel.run", String(id), String(h))); },
  install: function (slot, id) { __zippHostCall("accel.install", String(slot), String(id)); },
};

// ---- host-facing helpers (called by slot, never by eval) -------------------

function __zListenerTypes() {
  var out = [];
  for (var t in __zEvents) {
    if (__zEvents[t].length) out.push(t);
  }
  return out;
}

function __zDispatchEvent(type, evt) {
  var hs = __zEvents[String(type)];
  if (!hs || !hs.length) return 0;
  if (evt && typeof evt.preventDefault !== "function") {
    evt.preventDefault = function () {};
    evt.stopPropagation = function () {};
  }
  var n = 0;
  // Copy first: a handler may remove itself while we are iterating, and it
  // must not skip the unrelated listener after it.
  var copy = hs.slice();
  for (var i = 0; i < copy.length; i++) {
    if (typeof copy[i] !== "function") continue;
    copy[i](evt);
    n++;
  }
  return n;
}

// The drain is a two-phase transfer. The engine PEEKS a bounded prefix,
// converts it to host values, and only then COMMITS that many off the queue;
// a conversion that fails leaves the queue exactly as it was and the engine
// retries with less. The old single-step drain emptied the queue before its
// return value crossed the converter, so a conversion failure lost every
// queued request while their callbacks stayed registered forever (the
// 11 September 2026 audit's ZIPP-02). No guest code runs between a peek and
// its commit, so the committed prefix is the peeked one.
function __zPeekHostCalls(limit) {
  return __zHostQueue.slice(0, limit);
}

function __zCommitHostCalls(count) {
  __zHostQueue.splice(0, count);
  return __zHostQueue.length;
}

// A request that cannot cross even on its own is settled here rather than
// left queued forever or dropped silently: it leaves the queue and its
// callback receives a RangeError. A throw from that callback is reported to
// the console, as an uncaught exception in any asynchronous callback would
// be, so that it cannot abort the drain that other requests are part of.
function __zRejectHostCall(reason) {
  var req = __zHostQueue.shift();
  if (!req) return 0;
  var cb = __zHostCbs[req.id];
  if (cb) {
    delete __zHostCbs[req.id];
    __zHostPending--;
    try { cb(new RangeError(String(reason))); }
    catch (e) { console.error("host.call: callback for rejected request " + req.id + " threw: " + e); }
  }
  return 1;
}

function __zResolveHostCall(id, result) {
  var cb = __zHostCbs[id];
  if (!cb) return 0;
  delete __zHostCbs[id];
  __zHostPending--;
  cb(result);
  return 1;
}

// Release a pending callback WITHOUT invoking it: the host cancelled or
// timed out the request. A later completion for the same id is then a
// no-op, exactly like a completion for an unknown id.
function __zCancelHostCall(id) {
  var cb = __zHostCbs[id];
  if (!cb) return 0;
  delete __zHostCbs[id];
  __zHostPending--;
  return 1;
}
