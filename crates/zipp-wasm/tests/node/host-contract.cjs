// Exercises the full Engine surface the SoftN adapter depends on.
const { Engine } = require("./pkg/zipp_wasm.js");

let pass = 0, fail = 0;
function eq(label, got, want) {
  const g = JSON.stringify(got), w = JSON.stringify(want);
  if (g === w) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label}\n         got  ${g}\n         want ${w}`); }
}
function ok(label, cond, extra = "") {
  if (cond) { pass++; console.log(`  ok   ${label}`); }
  else { fail++; console.log(`  FAIL ${label} ${extra}`); }
}

// ---- a mock db + localStorage, same shape the SoftN adapter installs -------
const rows = {};
let nextId = 0;
const db = {
  query: (c) => (rows[c] || []).slice(),
  get: (c, id) => (rows[c] || []).find((r) => r.id === id) || null,
  create: (c, d) => { const r = { ...d, id: "id" + ++nextId }; (rows[c] ||= []).push(r); return r; },
  update: (id, d) => ({ id, ...d }),
  delete: () => {},
  hardDelete: () => {},
  startSync: () => {}, stopSync: () => {},
  getSyncStatus: () => ({ connected: true, peers: 2, room: "r", peerId: "p" }),
  getSavedSyncRoom: () => null,
};
let forbiddenLookups = 0, forbiddenCalls = 0;
Object.defineProperty(db, "secret", {
  get: () => {
    forbiddenLookups++;
    return () => { forbiddenCalls++; return "ambient authority reached"; };
  },
});
// The real adapter prefixes keys; this mock is the post-prefix store, so the
// script's "hi" is the key it sees.
const store = { hi: "42" };
const ls = {
  getItem: (k) => (Object.prototype.hasOwnProperty.call(store, k) ? store[k] : null),
  setItem: (k, v) => { store[k] = String(v); },
  removeItem: (k) => { delete store[k]; },
  clear: () => { for (const k of Object.keys(store)) delete store[k]; },
};

const SRC = `
let score = 0
let best = 0
let grid = []
let settings = { theme: "dark", cells: [1, 2, 3] }
let people = []

function _init() {
  best = parseInt(localStorage.getItem("hi")) || 0
  people = db.query("people")
  window.addEventListener("keydown", function (e) {
    e.preventDefault()
    if (e.key === "ArrowUp") score = score + 1
    if (e.key === "ArrowDown") score = score - 1
  })
}

function bump(n) { score = score + n; return { score: score, tag: "s" + score } }
function addPerson(name) { let p = db.create("people", { name: name }); people = db.query("people"); return p }
function save() { localStorage.setItem("hi", String(score)); return true }
function buildGrid(n) { grid = []; for (let i = 0; i < n; i++) { grid.push({ i: i, on: i % 2 === 0 }) } return grid.length }
function fetchIt() { softn.net.fetch("http://x/y", {}, function (r) { score = r.value }) }
function syncInfo() { return db.getSyncStatus() }
function probeForbidden() { return __zippHostCall("db.secret", "payload") }
`;

// SoftN prepends its own softn.* namespace; mimic the shape minimally.
const SOFTN_NS = `
let softn = { net: { fetch: function (url, options, callback) {
  host.call("net.fetch", [url, JSON.stringify(options || {})], callback);
} } };
`;

const e = new Engine();
e.setDbBridge(db);
e.setLocalStorageBridge(ls);

console.log("— initScript —");
const syms = e.initScript(SOFTN_NS + SRC);
const names = Object.keys(syms).sort();
// window/navigator/host are deliberately reported: the host needs a stable
// slot for the bridge objects it also writes to. Everything else the preamble
// declares stays hidden.
eq("symbol names", names, ["addPerson", "best", "buildGrid", "bump", "fetchIt", "grid", "people", "probeForbidden", "save", "score", "settings", "softn", "syncInfo", "_init", "window", "navigator", "host"].sort());
eq("scope of a function", syms.bump.scope, "function");
eq("scope of a variable", syms.score.scope, "variable");
ok("engine internals stay hidden", !names.some((n) => n.startsWith("__z") || ["db", "localStorage"].includes(n)), JSON.stringify(names));
ok("bridge objects are addressable", ["window", "navigator", "host"].every((n) => syms[n]), JSON.stringify(names));

console.log("— top level + _init —");
e.callFunction("_init", []);
eq("localStorage read during _init", e.getGlobalByIndex(syms.best.index), 42);

console.log("— structured globals —");
eq("nested object global", e.getGlobalByIndex(syms.settings.index), { theme: "dark", cells: [1, 2, 3] });
e.setGlobalByIndex(syms.settings.index, { theme: "light", cells: [9], extra: { deep: true } });
eq("write-back visible to script", e.evalInContext("settings.extra.deep && settings.theme"), "light");

console.log("— callFunction —");
eq("returns a structure", e.callFunction("bump", [5]), { score: 5, tag: "s5" });
eq("mutation is visible", e.getGlobalByIndex(syms.score.index), 5);
eq("builds an array of objects", e.callFunction("buildGrid", [3]), 3);
eq("array global marshals", e.getGlobalByIndex(syms.grid.index), [{ i: 0, on: true }, { i: 1, on: false }, { i: 2, on: true }]);

console.log("— db bridge (synchronous) —");
eq("create returns the row", e.callFunction("addPerson", ["Ada"]), { name: "Ada", id: "id1" });
eq("query reflects the write", e.getGlobalByIndex(syms.people.index), [{ name: "Ada", id: "id1" }]);
eq("object-returning bridge call", e.callFunction("syncInfo", []), { connected: true, peers: 2, room: "r", peerId: "p" });

console.log("— synchronous bridge capability boundary —");
let forbiddenError = "";
try { e.callFunction("probeForbidden", []); } catch (err) { forbiddenError = String(err); }
ok("unknown synchronous host kind is rejected", forbiddenError.includes("unknown host call 'db.secret'"), forbiddenError);
eq("unknown kind is rejected before property lookup", forbiddenLookups, 0);
eq("unknown bridge method is never called", forbiddenCalls, 0);
eq("known kind still works after rejection", e.callFunction("syncInfo", []), { connected: true, peers: 2, room: "r", peerId: "p" });

console.log("— localStorage bridge —");
e.callFunction("save", []);
eq("setItem reached the host", store["hi"], "5");

console.log("— batch —");
const idx = [syms.score.index, syms.best.index, syms.bump.index];
eq("getGlobalsBatch (function reads as null)", e.getGlobalsBatch(idx), [5, 42, null]);
e.setGlobalsBatch(idx, [100, 200, 12345]);
eq("setGlobalsBatch wrote the variables", e.getGlobalsBatch([syms.score.index, syms.best.index]), [100, 200]);
ok("function slot survived the batch write", e.callFunction("bump", [0]).score === 100);

console.log("— events —");
eq("listener types", e.getEventListenerTypes(), ["keydown"]);
eq("dispatch count", e.dispatchEvent("keydown", { type: "keydown", key: "ArrowUp" }), 1);
eq("handler ran (preventDefault did not throw)", e.getGlobalByIndex(syms.score.index), 101);
e.dispatchEvent("keydown", { type: "keydown", key: "ArrowDown" });
eq("second dispatch", e.getGlobalByIndex(syms.score.index), 100);
eq("unknown type dispatches to nobody", e.dispatchEvent("mousemove", { type: "mousemove" }), 0);

console.log("— host.call queue —");
eq("queue starts empty", e.drainPendingHostCalls(), []);
e.callFunction("fetchIt", []);
const q = e.drainPendingHostCalls();
eq("one queued call", q.length, 1);
eq("kind", q[0].kind, "net.fetch");
eq("args are strings", q[0].args, ["http://x/y", "{}"]);
eq("drain clears", e.drainPendingHostCalls(), []);
e.resolveHostCallback(q[0].id, { value: 777 });
eq("callback delivered a structure", e.getGlobalByIndex(syms.score.index), 777);

console.log("— errors stay recoverable —");
let threw = false;
try { e.callFunction("nope", []); } catch (_) { threw = true; }
ok("unknown function throws", threw);
ok("engine still usable after a throw", e.callFunction("bump", [1]).score === 778);

const bad = new Engine();
let compileErr = "";
try { bad.initScript("function ("); } catch (err) { compileErr = String(err); }
ok("syntax error is reported, not a trap", compileErr.includes("SyntaxError"), compileErr);

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
