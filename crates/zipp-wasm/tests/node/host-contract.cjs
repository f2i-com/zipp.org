// Exercises the full Engine surface the SoftN adapter depends on.
const fs = require("node:fs");
const path = require("node:path");
const { Engine } = require("./pkg/zipp_wasm.js");

// Fail before the behavioral suite if generated artifacts came from an older
// source revision. Checking all four surfaces catches every mixed-package
// combination: stale high-level glue, either stale declaration file, or a
// module whose raw wasm-bindgen export is absent.
const pkg = path.join(__dirname, "pkg");
const engineTypes = fs.readFileSync(path.join(pkg, "zipp_wasm.d.ts"), "utf8");
const wasmTypes = fs.readFileSync(path.join(pkg, "zipp_wasm_bg.wasm.d.ts"), "utf8");
const wasmModule = new WebAssembly.Module(fs.readFileSync(path.join(pkg, "zipp_wasm_bg.wasm")));
if (typeof Engine.prototype.getGlobalsFingerprint !== "function") {
  throw new Error("stale generated Zipp glue: Engine.getGlobalsFingerprint is missing");
}
if (!/\bgetGlobalsFingerprint\s*\(indices:\s*any\)\s*:\s*any\s*;/.test(engineTypes)) {
  throw new Error("stale generated Zipp declarations: getGlobalsFingerprint is missing");
}
if (!/\bengine_getGlobalsFingerprint\b/.test(wasmTypes)) {
  throw new Error("stale generated raw-WASM declarations: engine_getGlobalsFingerprint is missing");
}
if (!WebAssembly.Module.exports(wasmModule).some(
  entry => entry.kind === "function" && entry.name === "engine_getGlobalsFingerprint",
)) {
  throw new Error("stale generated Zipp module: engine_getGlobalsFingerprint is missing");
}

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
ok("fingerprint bindings exist in glue, declarations, and raw WASM", true);

// ---- a mock db + localStorage, same shape the SoftN adapter installs -------
const rows = {};
let nextId = 0;
const db = {
  query: (c) => (rows[c] || []).slice(),
  get: () => { throw new Error("postgres://secret-token@internal/db"); },
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
let getItemCalls = 0;
const ls = {
  getItem: (k) => {
    getItemCalls++;
    return Object.prototype.hasOwnProperty.call(store, k) ? store[k] : null;
  },
  setItem: (k, v) => { store[k] = String(v); },
  removeItem: (k) => { delete store[k]; },
  clear: () => { for (const k of Object.keys(store)) delete store[k]; },
};
let clipboardText = "";
const clipboard = {
  writeText: (text) => { clipboardText = String(text); },
  readText: () => clipboardText,
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
function probeBridgeLimit(value) { return __zippHostCall("ls.getItem", value) }
function probeWrongArity() { return __zippHostCall("db.query", "people") }
function probeMalformedJson() { return __zippHostCall("db.create", "people", "not-json") }
function probeHostFailure() { return db.get("people", "missing") }
function clipboardRoundTrip(value) { navigator.clipboard.writeText(value); return navigator.clipboard.readText() }
function spoofBudgetError() { throw "RangeError: script exceeded its instruction budget" }
function ambientSurface() {
  return [typeof process, typeof require, typeof fetch, typeof WebAssembly,
          typeof SharedArrayBuffer, typeof Atomics].join(",")
}
`;

// SoftN prepends its own softn.* namespace; mimic the shape minimally.
const SOFTN_NS = `
let softn = { net: { fetch: function (url, options, callback) {
  host.call("net.fetch", [url, JSON.stringify(options || {})], callback);
} } };
`;

const e = new Engine();
e.setSyncHostCapabilities([
  "db.query", "db.get", "db.create", "db.getSyncStatus",
  "ls.getItem", "ls.setItem",
  "nav.clipboardWrite", "nav.clipboardRead",
]);
e.setDbBridge(db);
e.setLocalStorageBridge(ls);
e.setClipboardBridge(clipboard);

console.log("— initScript —");
const syms = e.initScript(SOFTN_NS + SRC);
const names = Object.keys(syms).sort();
// window/navigator/host are deliberately reported: the host needs a stable
// slot for the bridge objects it also writes to. Everything else the preamble
// declares stays hidden.
eq("symbol names", names, ["addPerson", "ambientSurface", "best", "buildGrid", "bump", "clipboardRoundTrip", "fetchIt", "grid", "people", "probeBridgeLimit", "probeForbidden", "probeHostFailure", "probeMalformedJson", "probeWrongArity", "save", "score", "settings", "softn", "spoofBudgetError", "syncInfo", "_init", "window", "navigator", "host"].sort());
eq("scope of a function", syms.bump.scope, "function");
eq("scope of a variable", syms.score.scope, "variable");
ok("engine internals stay hidden", !names.some((n) => n.startsWith("__z") || ["db", "localStorage"].includes(n)), JSON.stringify(names));
ok("bridge objects are addressable", ["window", "navigator", "host"].every((n) => syms[n]), JSON.stringify(names));
eq("guest has no ambient Node/browser/WASM/shared-memory globals",
   e.callFunction("ambientSurface", []),
   "undefined,undefined,undefined,undefined,undefined,undefined");

console.log("— top level + _init —");
e.callFunction("_init", []);
eq("localStorage read during _init", e.getGlobalByIndex(syms.best.index), 42);

console.log("— structured globals —");
eq("nested object global", e.getGlobalByIndex(syms.settings.index), { theme: "dark", cells: [1, 2, 3] });
e.setGlobalByIndex(syms.settings.index, { theme: "light", cells: [9], extra: { deep: true } });
eq("write-back visible to script", e.evalInContext("settings.extra.deep && settings.theme"), "light");

console.log("— prototype-safe object marshalling —");
const protoEngine = new Engine();
const protoSyms = protoEngine.initScript(`
  let __proto__ = 7;
  let payload = JSON.parse('{"__proto__":{"admin":true},"safe":1}');
`);
ok("symbol map has an own __proto__ key", Object.prototype.hasOwnProperty.call(protoSyms, "__proto__"));
eq("__proto__ symbol metadata survives", protoSyms.__proto__.scope, "variable");
const protoPayload = protoEngine.getGlobalByIndex(protoSyms.payload.index);
ok("marshalled object has an own __proto__ key", Object.prototype.hasOwnProperty.call(protoPayload, "__proto__"));
ok("__proto__ data does not change the result prototype", Object.getPrototypeOf(protoPayload) === Object.prototype);
eq("prototype payload does not become inherited authority", protoPayload.admin, undefined);
eq("ordinary object data survives", protoPayload.safe, 1);

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
let arityError = "";
try { e.callFunction("probeWrongArity", []); } catch (err) { arityError = String(err); }
ok("known synchronous kinds require exact arity", arityError.includes("requires exactly 2 arguments"), arityError);
let malformedJsonError = "";
try { e.callFunction("probeMalformedJson", []); } catch (err) { malformedJsonError = String(err); }
ok("malformed structured bridge arguments are rejected", malformedJsonError.includes("malformed JSON"), malformedJsonError);
let opaqueHostError = "";
try { e.callFunction("probeHostFailure", []); } catch (err) { opaqueHostError = String(err); }
ok("host bridge exceptions are opaque to the guest", opaqueHostError.includes("host bridge call failed") && !opaqueHostError.includes("secret-token"), opaqueHostError);
let bridgeLimitErr = "";
const getItemCallsBeforeLimit = getItemCalls;
try { e.callFunction("probeBridgeLimit", ["x".repeat(1024 * 1024 + 1)]); }
catch (err) { bridgeLimitErr = String(err); }
ok("the VM string ceiling rejects an oversized bridge argument", bridgeLimitErr.includes("Invalid string length"), bridgeLimitErr);
eq("an oversized bridge argument is rejected before the bridge runs", getItemCalls, getItemCallsBeforeLimit);
eq("known kind still works after rejection", e.callFunction("syncInfo", []), { connected: true, peers: 2, room: "r", peerId: "p" });

const reentrant = new Engine();
reentrant.setSyncHostCapabilities(["db.query"]);
reentrant.setDbBridge({
  query() { return reentrant.evalInContext("1"); },
});
reentrant.initScript(`
  function probeReentry() { return db.query("rows"); }
  function aliveAfterReentry() { return 9; }
`);
let reentrantError = "";
try { reentrant.callFunction("probeReentry", []); }
catch (err) { reentrantError = String(err); }
ok("synchronous bridge re-entry is rejected opaquely",
   reentrantError.includes("host bridge call failed") && !reentrantError.includes("recursive use"),
   reentrantError);
eq("rejected bridge re-entry does not poison the Engine borrow",
   reentrant.callFunction("aliveAfterReentry", []), 9);

const denied = new Engine();
denied.setDbBridge(db);
const deniedSyms = denied.initScript("function probe() { return db.query('people'); }");
let deniedError = "";
try { denied.callFunction("probe", []); } catch (err) { deniedError = String(err); }
ok("installing a bridge grants no authority by default", deniedError.includes("capability denied"), deniedError);

const unknownCapability = new Engine();
let unknownCapabilityError = "";
try { unknownCapability.setSyncHostCapabilities(["db.query", "db.secret"]); }
catch (err) { unknownCapabilityError = String(err); }
ok("unknown capability names reject configuration", unknownCapabilityError.includes("unknown synchronous host capability"), unknownCapabilityError);

const excessiveCapabilities = new Engine();
let excessiveCapabilitiesError = "";
try { excessiveCapabilities.setSyncHostCapabilities(new Array(33).fill("db.query")); }
catch (err) { excessiveCapabilitiesError = String(err); }
ok("capability configuration input is bounded", excessiveCapabilitiesError.includes("too many synchronous host capability entries"), excessiveCapabilitiesError);

let lateCapabilityError = "";
try { e.setSyncHostCapabilities(["db.query"]); } catch (err) { lateCapabilityError = String(err); }
ok("capabilities are immutable after initialization starts", lateCapabilityError.includes("host bridge configuration is immutable"), lateCapabilityError);
let lateBridgeError = "";
try { e.setClipboardBridge(clipboard); } catch (err) { lateBridgeError = String(err); }
ok("bridge handles are immutable after initialization starts", lateBridgeError.includes("host bridge configuration is immutable"), lateBridgeError);

console.log("— localStorage bridge —");
e.callFunction("save", []);
eq("setItem reached the host", store["hi"], "5");

console.log("— clipboard bridge —");
eq("clipboard has a separately granted bridge", e.callFunction("clipboardRoundTrip", ["copy-me"]), "copy-me");

console.log("— batch —");
const idx = [syms.score.index, syms.best.index, syms.bump.index];
eq("getGlobalsBatch (function reads as null)", e.getGlobalsBatch(idx), [5, 42, null]);
const firstFingerprints = e.getGlobalsFingerprint(idx);
ok(
  "fingerprints are one exact non-negative JS integer per slot",
  Array.isArray(firstFingerprints)
    && firstFingerprints.length === idx.length
    && firstFingerprints.every(value => Number.isSafeInteger(value) && value >= 0),
  JSON.stringify(firstFingerprints),
);
eq("unchanged globals keep their fingerprints", e.getGlobalsFingerprint(idx), firstFingerprints);
const duplicateFingerprints = e.getGlobalsFingerprint([syms.score.index, syms.score.index]);
eq("fingerprint order and duplicate slots are preserved", duplicateFingerprints, [firstFingerprints[0], firstFingerprints[0]]);
e.setGlobalsBatch(idx, [100, 200, 12345]);
eq("setGlobalsBatch wrote the variables", e.getGlobalsBatch([syms.score.index, syms.best.index]), [100, 200]);
ok("function slot survived the batch write", e.callFunction("bump", [0]).score === 100);
const writtenFingerprints = e.getGlobalsFingerprint(idx);
ok(
  "changed data slots change while the protected function slot stays stable",
  writtenFingerprints[0] !== firstFingerprints[0]
    && writtenFingerprints[1] !== firstFingerprints[1]
    && writtenFingerprints[2] === firstFingerprints[2],
  JSON.stringify({ firstFingerprints, writtenFingerprints }),
);

// A slot-generation counter would miss this: the global still points at the
// same Array, but nested data changed in place. The fingerprint must track the
// value getGlobalsBatch would actually marshal, and an equal replacement must
// recover the original content digest even though it has a new identity.
const originalGrid = e.getGlobalsBatch([syms.grid.index])[0];
const originalGridFingerprint = e.getGlobalsFingerprint([syms.grid.index])[0];
e.evalInContext("(grid[1].on = true, grid.push({ i: 3, on: false }))");
const mutatedGridFingerprint = e.getGlobalsFingerprint([syms.grid.index])[0];
ok(
  "an in-place nested mutation changes the fingerprint",
  mutatedGridFingerprint !== originalGridFingerprint,
  JSON.stringify({ originalGridFingerprint, mutatedGridFingerprint }),
);
e.setGlobalByIndex(syms.grid.index, originalGrid);
eq(
  "a structurally equal replacement restores the content fingerprint",
  e.getGlobalsFingerprint([syms.grid.index]),
  [originalGridFingerprint],
);

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
let spoofErr = "";
try { e.callFunction("spoofBudgetError", []); } catch (err) { spoofErr = String(err); }
ok("guest budget-error text is an ordinary recoverable throw", spoofErr.includes("instruction budget"), spoofErr);
eq("spoofed budget text does not dispose", e.callFunction("bump", [0]).score, 778);

const bad = new Engine();
bad.setDbBridge(db);
let compileErr = "";
try { bad.initScript("function ("); } catch (err) { compileErr = String(err); }
ok("syntax error is reported, not a trap", compileErr.includes("SyntaxError"), compileErr);
let failedInitTerminalErr = "";
try { bad.initScript("let fresh = 1;"); } catch (err) { failedInitTerminalErr = String(err); }
ok("a failed init terminally clears the engine", failedInitTerminalErr.includes("disposed"), failedInitTerminalErr);
let failedInitBridgeErr = "";
try { bad.setDbBridge(db); } catch (err) { failedInitBridgeErr = String(err); }
ok("a failed init clears and cannot reacquire bridges", failedInitBridgeErr.includes("disposed"), failedInitBridgeErr);

const runtimeBad = new Engine();
runtimeBad.setLocalStorageBridge(ls);
let runtimeInitErr = "";
try { runtimeBad.initScript("throw new Error('top-level failure');"); }
catch (err) { runtimeInitErr = String(err); }
ok("top-level init failure is reported", runtimeInitErr.includes("top-level failure"), runtimeInitErr);
let runtimeInitTerminalErr = "";
try { runtimeBad.initScript("let unreachable = 1;"); }
catch (err) { runtimeInitTerminalErr = String(err); }
ok("top-level init failure also disposes", runtimeInitTerminalErr.includes("disposed"), runtimeInitTerminalErr);

console.log("— engine resource controls —");
const sourceBound = new Engine();
let sourceLimitErr = "";
try { sourceBound.initScript(" ".repeat(2 * 1024 * 1024 + 1)); }
catch (err) { sourceLimitErr = String(err); }
ok("initial source is capped before compilation", sourceLimitErr.includes("initial script source exceeds"), sourceLimitErr);
let sourceTerminalErr = "";
try { sourceBound.initScript("let unreachable = 1;"); }
catch (err) { sourceTerminalErr = String(err); }
ok("initial source exhaustion disposes", sourceTerminalErr.includes("disposed"), sourceTerminalErr);

const evalBound = new Engine();
evalBound.initScript("let live = 1;");
let evalLimitErr = "";
try { evalBound.evalInContext("0/*" + "x".repeat(64 * 1024) + "*/"); }
catch (err) { evalLimitErr = String(err); }
ok("eval source has a per-call cap", evalLimitErr.includes("evalInContext source exceeds"), evalLimitErr);
let evalTerminalErr = "";
try { evalBound.evalInContext("1 + 1"); }
catch (err) { evalTerminalErr = String(err); }
ok("eval source exhaustion disposes", evalTerminalErr.includes("disposed"), evalTerminalErr);

const dynamicBound = new Engine();
dynamicBound.initScript(`
  function catchOversizedEval(code) {
    try { eval(code); } catch (_) {}
    return 7;
  }
`);
let dynamicLimitErr = "";
try { dynamicBound.callFunction("catchOversizedEval", [" ".repeat(64 * 1024 + 1)]); }
catch (err) { dynamicLimitErr = String(err); }
ok("caught guest eval cannot bypass the VM-wide source cap", dynamicLimitErr.includes("dynamic code source exceeds"), dynamicLimitErr);
let dynamicTerminalErr = "";
try { dynamicBound.evalInContext("1 + 1"); }
catch (err) { dynamicTerminalErr = String(err); }
ok("caught dynamic compilation exhaustion disposes", dynamicTerminalErr.includes("disposed"), dynamicTerminalErr);

const runaway = new Engine();
let instructionLimitErr = "";
try { runaway.initScript("for (;;) {}"); }
catch (err) { instructionLimitErr = String(err); }
ok("top-level infinite loop hits the instruction budget", instructionLimitErr.includes("instruction budget"), instructionLimitErr);
let instructionTerminalErr = "";
try { runaway.initScript("let unreachable = 1;"); }
catch (err) { instructionTerminalErr = String(err); }
ok("instruction exhaustion disposes", instructionTerminalErr.includes("disposed"), instructionTerminalErr);

const outputEdge = new Engine();
outputEdge.initScript("for (let i = 0; i < 96 * 1024; i++) console.log('');");
eq("exact output limit remains drainable", outputEdge.takeOutput().length, 96 * 1024);
eq("successful output drain clears the buffer", outputEdge.takeOutput().length, 0);

const outputOver = new Engine();
let outputLimitErr = "";
try { outputOver.initScript("for (let i = 0; i <= 96 * 1024; i++) console.log('');"); }
catch (err) { outputLimitErr = String(err); }
ok("one byte beyond the lifetime output cap fails closed", outputLimitErr.includes("output budget"), outputLimitErr);
let outputTerminalErr = "";
try { outputOver.initScript("let unreachable = 1;"); }
catch (err) { outputTerminalErr = String(err); }
ok("output exhaustion disposes", outputTerminalErr.includes("disposed"), outputTerminalErr);

console.log("— bounded host marshalling —");
const limits = new Engine();
const limitSyms = limits.initScript(`
  let dag = 0;
  for (let i = 0; i < 32; i++) dag = [dag, dag];
  let incoming = null;
`);
let dagErr = "";
try { limits.getGlobalByIndex(limitSyms.dag.index); } catch (err) { dagErr = String(err); }
ok("shared guest DAG hits a controlled node limit", dagErr.includes("conversion node limit"), dagErr);

const cyclic = {};
cyclic.self = cyclic;
cyclic.again = cyclic;
limits.setGlobalByIndex(limitSyms.incoming.index, cyclic);
eq("inbound cycles retain the null back-edge behavior", limits.evalInContext("incoming.self === null && incoming.again === null"), true);

let nodeLimitErr = "";
try { limits.setGlobalByIndex(limitSyms.incoming.index, new Array(100000).fill(0)); }
catch (err) { nodeLimitErr = String(err); }
ok("wide inbound arrays hit a controlled node limit", nodeLimitErr.includes("conversion node limit"), nodeLimitErr);

let stringLimitErr = "";
try { limits.setGlobalByIndex(limitSyms.incoming.index, "x".repeat(16 * 1024 * 1024 + 1)); }
catch (err) { stringLimitErr = String(err); }
ok("large inbound strings hit a controlled byte limit", stringLimitErr.includes("conversion string limit"), stringLimitErr);
eq("marshal limit errors leave the VM usable", limits.evalInContext("1 + 1"), 2);

console.log("— hostile host-value inspection —");
function proxyRejectionIsRecoverable(label, value) {
  let err = "";
  try { limits.setGlobalByIndex(limitSyms.incoming.index, value); }
  catch (caught) { err = String(caught); }
  ok(`${label} is a controlled marshalling error`, err.includes("could not be inspected safely"), err);
  eq(`${label} does not poison the Engine borrow`, limits.evalInContext("1 + 1"), 2);
}

proxyRejectionIsRecoverable("throwing object ownKeys trap", new Proxy({}, {
  ownKeys() { throw new Error("ownKeys must not unwind through WASM"); },
}));
proxyRejectionIsRecoverable("throwing object getter", Object.defineProperty({}, "x", {
  enumerable: true,
  get() { throw new Error("getter must not unwind through WASM"); },
}));
proxyRejectionIsRecoverable("throwing array length trap", new Proxy([1], {
  get(target, key, receiver) {
    if (key === "length") throw new Error("length must not unwind through WASM");
    return Reflect.get(target, key, receiver);
  },
}));
proxyRejectionIsRecoverable("throwing array index trap", new Proxy([1], {
  get(target, key, receiver) {
    if (key === "0") throw new Error("index must not unwind through WASM");
    return Reflect.get(target, key, receiver);
  },
}));
const revokedObject = Proxy.revocable({}, {});
revokedObject.revoke();
proxyRejectionIsRecoverable("revoked object proxy", revokedObject.proxy);
const revokedArray = Proxy.revocable([], {});
revokedArray.revoke();
proxyRejectionIsRecoverable("revoked array proxy", revokedArray.proxy);

const capabilityProxyEngine = new Engine();
const capabilityProxy = new Proxy(["db.query"], {
  get(target, key, receiver) {
    if (key === "length") throw new Error("capability length must be caught");
    return Reflect.get(target, key, receiver);
  },
});
let capabilityProxyErr = "";
try { capabilityProxyEngine.setSyncHostCapabilities(capabilityProxy); }
catch (err) { capabilityProxyErr = String(err); }
ok("throwing capability arrays are controlled", capabilityProxyErr.includes("could not be inspected safely"), capabilityProxyErr);
capabilityProxyEngine.setSyncHostCapabilities([]);
capabilityProxyEngine.initScript("function alive() { return 7; }");
eq("capability inspection does not poison the Engine borrow", capabilityProxyEngine.callFunction("alive", []), 7);

const throwingIndices = new Proxy([limitSyms.incoming.index], {
  get(target, key, receiver) {
    if (key === "0") throw new Error("index list access must be caught");
    return Reflect.get(target, key, receiver);
  },
});
let throwingIndicesErr = "";
try { limits.getGlobalsBatch(throwingIndices); }
catch (err) { throwingIndicesErr = String(err); }
ok("throwing index arrays are controlled", throwingIndicesErr.includes("could not be inspected safely"), throwingIndicesErr);
eq("index inspection does not poison the Engine borrow", limits.evalInContext("1 + 1"), 2);
let throwingFingerprintIndicesErr = "";
try { limits.getGlobalsFingerprint(throwingIndices); }
catch (err) { throwingFingerprintIndicesErr = String(err); }
ok(
  "throwing fingerprint index arrays are controlled",
  throwingFingerprintIndicesErr.includes("could not be inspected safely"),
  throwingFingerprintIndicesErr,
);
eq("fingerprint index inspection does not poison the Engine borrow", limits.evalInContext("1 + 1"), 2);

for (const [label, invalidIndex] of [
  ["negative", -1],
  ["fractional", 0.5],
  ["infinite", Infinity],
  ["NaN", NaN],
  ["out-of-range", 2 ** 32],
  ["nonnumeric", "0"],
]) {
  let err = "";
  try { limits.getGlobalsBatch([invalidIndex]); }
  catch (caught) { err = String(caught); }
  ok(`${label} slot indices fail closed`, err.includes("finite unsigned 32-bit integers"), err);
  eq(`${label} slot indices do not poison the Engine borrow`, limits.evalInContext("1 + 1"), 2);

  let fingerprintErr = "";
  try { limits.getGlobalsFingerprint([invalidIndex]); }
  catch (caught) { fingerprintErr = String(caught); }
  ok(
    `${label} fingerprint slot indices fail closed`,
    fingerprintErr.includes("finite unsigned 32-bit integers"),
    fingerprintErr,
  );
  eq(
    `${label} fingerprint indices do not poison the Engine borrow`,
    limits.evalInContext("1 + 1"),
    2,
  );
}

const throwingValues = new Proxy([1], {
  get(target, key, receiver) {
    if (key === "0") throw new Error("batch value access must be caught");
    return Reflect.get(target, key, receiver);
  },
});
let throwingValuesErr = "";
try { limits.setGlobalsBatch([limitSyms.incoming.index], throwingValues); }
catch (err) { throwingValuesErr = String(err); }
ok("throwing batch value arrays are controlled", throwingValuesErr.includes("could not be inspected safely"), throwingValuesErr);
eq("batch value inspection does not poison the Engine borrow", limits.evalInContext("1 + 1"), 2);

console.log("— terminal engine lifecycle —");
const tenant = new Engine();
const tenantSyms = tenant.initScript("let secret = 42;");
eq("first tenant initialized", tenant.getGlobalByIndex(tenantSyms.secret.index), 42);
let repeatErr = "";
try { tenant.initScript("let other = 1;"); } catch (err) { repeatErr = String(err); }
ok("repeated init fails closed and disposes", repeatErr.includes("repeated initialization"), repeatErr);
let oldStateErr = "";
try { tenant.getGlobalByIndex(tenantSyms.secret.index); } catch (err) { oldStateErr = String(err); }
ok("old tenant state is inaccessible", oldStateErr.includes("disposed"), oldStateErr);
let bridgeAfterDisposeErr = "";
try { tenant.setDbBridge(db); } catch (err) { bridgeAfterDisposeErr = String(err); }
ok("disposed engine cannot reacquire host capabilities", bridgeAfterDisposeErr.includes("disposed"), bridgeAfterDisposeErr);

const disposed = new Engine();
const disposedSyms = disposed.initScript("let value = 1;");
disposed.dispose();
let terminalErr = "";
try { disposed.initScript("let replacement = 2;"); } catch (err) { terminalErr = String(err); }
ok("explicit dispose is terminal", terminalErr.includes("disposed"), terminalErr);
let disposedFingerprintErr = "";
try { disposed.getGlobalsFingerprint([disposedSyms.value.index]); }
catch (err) { disposedFingerprintErr = String(err); }
ok(
  "disposed engine rejects fingerprint reads",
  disposedFingerprintErr.includes("disposed"),
  disposedFingerprintErr,
);

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
