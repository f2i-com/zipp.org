// The Worker half of zipp-host (see zipp-host.mjs). One engine per Worker;
// the Worker's lifetime is the engine's. Every message carries the host's
// generation and is answered with it, so the main thread can drop a reply
// from a Worker it has already given up on.
//
// The engine's own contracts do the containment: capabilities are granted
// once before initialization, a resource ceiling disposes the engine, and a
// runaway guest is stopped only by the main thread terminating this Worker.
// This file adds no authority; it only shapes messages.

let engine = null;
let zipp = null;
let dead = null; // { category, message } once the engine is gone

function resourceOrGuest(message, phase) {
  const text = String(message);
  if (/exceeded|budget|limit|disposed this engine/i.test(text)) return "resource";
  if (phase === "init") return "source";
  return "guest";
}

function fail(category, message, detail) {
  return { category, message: String(message), detail };
}

async function handle(msg) {
  if (dead) return { error: fail("resource", `engine is dead: ${dead.message}`) };
  switch (msg.op) {
    case "init": {
      zipp = await import(msg.moduleUrl);
      await zipp.default({ module_or_path: msg.wasmUrl });
      engine = new zipp.Engine();
      // Configuration is immutable once initialization starts; set it all
      // now, capabilities first (an unknown name rejects the whole grant).
      engine.setSyncHostCapabilities(msg.capabilities || []);
      if (msg.bridgeModuleUrl) {
        const bridges = (await import(msg.bridgeModuleUrl)).default();
        if (bridges.db) engine.setDbBridge(bridges.db);
        if (bridges.localStorage) engine.setLocalStorageBridge(bridges.localStorage);
        if (bridges.clipboard) engine.setClipboardBridge(bridges.clipboard);
      }
      if (msg.instructionBudget != null) engine.setInstructionBudget(msg.instructionBudget);
      if (msg.fingerprintSeed) engine.setFingerprintSeed(msg.fingerprintSeed[0] >>> 0, msg.fingerprintSeed[1] >>> 0);
      try {
        return { value: engine.initScript(msg.source) };
      } catch (error) {
        const category = resourceOrGuest(error?.message ?? error, "init");
        // Any initialization failure disposed the engine.
        dead = { category, message: String(error?.message ?? error) };
        return { error: fail(category, error?.message ?? error) };
      }
    }
    case "call": return { value: engine.callFunction(msg.name, msg.args || []) };
    case "read": return { value: engine.getGlobalsBatch(msg.indices) };
    case "fingerprint": return { value: engine.getGlobalsFingerprint(msg.indices) };
    case "write": engine.setGlobalsBatch(msg.indices, msg.values); return { value: true };
    case "eval": return { value: engine.evalInContextRich(msg.expr) };
    case "dispatch": return { value: engine.dispatchEvent(msg.type, msg.event) };
    case "drain": return { value: engine.drainPendingHostCalls() };
    case "resolve": return { value: engine.resolveHostCallback(msg.callId, msg.result) };
    case "cancel": return { value: engine.cancelHostCallback(msg.callId) };
    case "console": return { value: engine.takeConsole() };
    case "usage": return { value: engine.resourceUsage() };
    default: return { error: fail("usage", `unknown op ${msg.op}`) };
  }
}

self.addEventListener("message", async (event) => {
  const msg = event.data;
  let reply;
  try {
    if (!engine && msg.op !== "init") {
      reply = { error: fail("usage", "init first") };
    } else {
      reply = await handle(msg);
    }
  } catch (error) {
    const message = error?.message ?? error;
    const category = resourceOrGuest(message, msg.op);
    if (category === "resource") dead = { category, message: String(message) };
    reply = { error: fail(category, message) };
  }
  if (reply.error) {
    self.postMessage({ gen: msg.gen, id: msg.id, ok: false, error: reply.error });
  } else {
    self.postMessage({ gen: msg.gen, id: msg.id, ok: true, value: reply.value });
  }
});
