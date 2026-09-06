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
// between drains, and callbacks awaiting a completion.
var __zHostQueueMax = 4096;
var __zHostPendingMax = 65536;

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
  dispatchEvent: function () { return true; },
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
    if (args) for (var i = 0; i < args.length; i++) flat.push(String(args[i]));
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

function __zDrainHostCalls() {
  var q = __zHostQueue;
  __zHostQueue = [];
  return q;
}

function __zResolveHostCall(id, result) {
  var cb = __zHostCbs[id];
  if (!cb) return 0;
  delete __zHostCbs[id];
  __zHostPending--;
  cb(result);
  return 1;
}
