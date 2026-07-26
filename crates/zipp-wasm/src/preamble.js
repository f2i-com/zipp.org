// Host bridge preamble, prepended to every script the engine compiles.
//
// The engine it replaces implemented `db`, `localStorage`, `window` and
// `host` as VM builtins in Rust. zipp is a plain JavaScript engine, so they
// are ordinary JS here, and the only channel to the host is __zippHostCall
// (strings in, string out — anything structured crosses as JSON).
//
// Everything declared here is reported to the host as a preamble name and
// filtered out of the symbol map, so none of it is mistaken for script state.

var __zEvents = {};
var __zHostQueue = [];
var __zHostCbs = {};
var __zHostId = 0;

var window = {
  addEventListener: function (type, fn) {
    if (typeof fn !== "function") return;
    (__zEvents[type] = __zEvents[type] || []).push(fn);
  },
  removeEventListener: function (type, fn) {
    var a = __zEvents[type];
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
    readText: function () { return __zippHostCall("nav.clipboardRead"); },
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
    var id = ++__zHostId;
    if (typeof cb === "function") __zHostCbs[id] = cb;
    var flat = [];
    if (args) for (var i = 0; i < args.length; i++) flat.push(String(args[i]));
    __zHostQueue.push({ id: id, kind: String(kind), args: flat });
  },
};

// ---- host-facing helpers (called by slot, never by eval) -------------------

function __zListenerTypes() {
  var out = [];
  for (var t in __zEvents) {
    if (Object.prototype.hasOwnProperty.call(__zEvents, t) && __zEvents[t].length) out.push(t);
  }
  return out;
}

function __zDispatchEvent(type, evt) {
  var hs = __zEvents[type];
  if (!hs || !hs.length) return 0;
  if (evt && typeof evt.preventDefault !== "function") {
    evt.preventDefault = function () {};
    evt.stopPropagation = function () {};
  }
  var n = 0;
  // Copy first: a handler may remove itself while we are iterating.
  var copy = hs.slice();
  for (var i = 0; i < copy.length; i++) {
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
  cb(result);
  return 1;
}
