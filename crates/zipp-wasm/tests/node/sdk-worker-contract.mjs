// Execute the actual browser Worker adapter in a Node Worker with the browser
// message API shimmed. Small host modules inject arbitrary thrown values;
// these checks exercise adapter recovery without requiring a WASM rebuild.
import assert from "node:assert/strict";
import { Worker } from "node:worker_threads";

const adapter = new URL("../../host-sdk/zipp-host.worker.mjs", import.meta.url).href;
const moduleUrl = (source) => `data:text/javascript,${encodeURIComponent(source)}`;
const engineModule = moduleUrl(`
  export default async function init() {}
  export class Engine {
    disposed = false;
    setSyncHostCapabilities() {}
    initScript() { return {}; }
    lastErrorKind() { return "guest"; }
    callFunction(name) {
      if (name === "message") throw { get message() { throw 1; } };
      if (name === "conversion") throw { message: { toString() { throw 1; } } };
      if (name === "null-prototype") throw Object.create(null);
      if (name === "revoked") {
        const pair = Proxy.revocable({}, {}); pair.revoke(); throw pair.proxy;
      }
      if (name === "symbol") throw Symbol("guest error");
      return "healthy";
    }
    drainPendingHostCalls() { return []; }
    drainPendingHostCallsStatus() { return { calls: [], hasMore: false, stopReason: "empty" }; }
  }
`);

async function withWorker(run) {
  const worker = new Worker(`
    const { parentPort } = require("node:worker_threads");
    globalThis.self = {
      addEventListener(type, listener) {
        if (type === "message") parentPort.on("message", data => listener({ data }));
      },
      postMessage(message) { parentPort.postMessage(message); }
    };
    import(${JSON.stringify(adapter)}).then(() => parentPort.postMessage({ ready: true }));
  `, { eval: true });
  const pending = new Map();
  let nextId = 0;
  const rejectAll = (error) => {
    for (const item of pending.values()) { clearTimeout(item.timer); item.reject(error); }
    pending.clear();
  };
  worker.on("error", rejectAll);
  worker.on("exit", (code) => rejectAll(new Error(`adapter Worker exited ${code}`)));
  worker.on("message", (reply) => {
    const item = pending.get(reply.ready ? "ready" : reply.id);
    if (!item) return;
    pending.delete(reply.ready ? "ready" : reply.id);
    clearTimeout(item.timer);
    item.resolve(reply);
  });
  function waitFor(id) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`adapter did not reply to ${id}`));
      }, 5000);
      pending.set(id, { resolve, reject, timer });
    });
  }
  async function request(op, payload = {}) {
    const id = ++nextId;
    const response = waitFor(id);
    worker.postMessage({ gen: 1, id, op, ...payload });
    return response;
  }
  try {
    await waitFor("ready");
    await run(request);
  } finally {
    rejectAll(new Error("test finished"));
    await worker.terminate();
  }
}

await withWorker(async (request) => {
  assert.equal((await request("init", { moduleUrl: engineModule, wasmUrl: "unused", source: "" })).ok, true);
  for (const name of ["message", "conversion", "null-prototype", "revoked", "symbol"]) {
    const reply = await request("call", { name });
    assert.equal(reply.ok, false, name);
    assert.equal(reply.error.category, "guest", name);
    assert.equal(reply.error.terminal, false, name);
    assert.equal(typeof reply.error.message, "string", name);
    const healthy = await request("call", { name: "healthy" });
    assert.equal(healthy.value, "healthy", name);
  }
});
console.log("  ok   arbitrary engine throws produce recoverable, cloneable replies");

await withWorker(async (request) => {
  const reply = await request("init", {
    moduleUrl: moduleUrl("throw Object.create(null);"), wasmUrl: "unused", source: "",
  });
  assert.equal(reply.ok, false);
  assert.equal(reply.error.category, "host");
  assert.equal(reply.error.terminal, true);
  assert.match(reply.error.message, /engine module failed to load/);
});
console.log("  ok   unprintable module-load failure sends a terminal host envelope");

await withWorker(async (request) => {
  const reply = await request("init", {
    moduleUrl: engineModule, wasmUrl: "unused", source: "",
    bridgeModuleUrl: moduleUrl("export default function () { throw { get message() { throw 1; } }; }"),
  });
  assert.equal(reply.ok, false);
  assert.equal(reply.error.category, "host");
  assert.equal(reply.error.terminal, true);
  assert.match(reply.error.message, /bridge module failed/);
  assert.equal((await request("call", { name: "healthy" })).error.terminal, true);
});
console.log("  ok   unprintable bridge failure sends a terminal host envelope");
console.log("3 Worker adapter checks passed");
