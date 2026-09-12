// The Worker half of zipp-host (see zipp-host.mjs). One engine per Worker;
// the Worker's lifetime is the engine's. Every message carries the host's
// generation and is answered with it, so the main thread can drop a reply
// from a Worker it has already given up on.
//
// The engine's own contracts do the containment: capabilities are granted
// once before initialization, a resource ceiling disposes the engine, and a
// runaway guest is stopped only by the main thread terminating this Worker.
// This file adds no authority; it only shapes messages.
//
// Every error reply is a structured envelope `{ category, message, detail?,
// terminal }`. The category comes from the ENGINE's own account of what it
// threw (`lastErrorKind()`: guest, conversion, usage, source, resource) and
// `terminal` from its lifecycle (`disposed`), never from the words in the
// message — a guest throwing `Error("business limit reached")` used to be
// classified as resource exhaustion and cost the application its Worker
// (the 11 September 2026 close audit's ZA-01).

import { categorizeEngineError, describeThrownError } from "./zipp-host.mjs";

let engine = null;
let zipp = null;
let dead = null; // the terminal envelope, once the engine is gone

function fail(category, message, detail, terminal = false) {
  return { category, message: String(message), detail, terminal };
}

/** Run one engine call; a throw becomes the engine-classified envelope. */
function engineCall(phase, fn) {
  try {
    return { value: fn() };
  } catch (error) {
    const failure = { kind: engine.lastErrorKind(), terminal: engine.disposed };
    const category = categorizeEngineError(failure, phase);
    const envelope = fail(category, describeThrownError(error).message, undefined, failure.terminal);
    if (failure.terminal) dead = envelope;
    return { error: envelope };
  }
}

async function handle(msg) {
  if (dead) return { error: fail(dead.category, `engine is dead: ${dead.message}`, undefined, true) };
  switch (msg.op) {
    case "init": {
      // Module and bridge loading are HOST failures: nothing the guest wrote
      // is involved, and the engine does not exist yet (or is unconfigured).
      try {
        zipp = await import(msg.moduleUrl);
        await zipp.default({ module_or_path: msg.wasmUrl });
      } catch (error) {
        dead = fail("host", `engine module failed to load: ${describeThrownError(error).message}`, undefined, true);
        return { error: dead };
      }
      engine = new zipp.Engine();
      // Configuration is immutable once initialization starts; set it all
      // now, capabilities first (an unknown name rejects the whole grant).
      const configured = engineCall("init", () => {
        engine.setSyncHostCapabilities(msg.capabilities || []);
        if (msg.instructionBudget != null) engine.setInstructionBudget(msg.instructionBudget);
        if (msg.fingerprintSeed) engine.setFingerprintSeed(msg.fingerprintSeed[0] >>> 0, msg.fingerprintSeed[1] >>> 0);
      });
      if (configured.error) {
        // A configuration the engine refuses (an unknown capability name) is
        // the host's mistake, and this Worker cannot be initialized again.
        dead = fail("usage", configured.error.message, undefined, true);
        return { error: dead };
      }
      if (msg.bridgeModuleUrl) {
        try {
          const bridges = (await import(msg.bridgeModuleUrl)).default();
          if (bridges.db) engine.setDbBridge(bridges.db);
          if (bridges.localStorage) engine.setLocalStorageBridge(bridges.localStorage);
          if (bridges.clipboard) engine.setClipboardBridge(bridges.clipboard);
        } catch (error) {
          dead = fail("host", `bridge module failed: ${describeThrownError(error).message}`, undefined, true);
          return { error: dead };
        }
      }
      // Any initialization failure disposed the engine (the envelope says
      // whether the recorder or the source did it).
      return engineCall("init", () => engine.initScript(msg.source));
    }
    case "call": return engineCall("call", () => engine.callFunction(msg.name, msg.args || []));
    case "read": return engineCall("read", () => engine.getGlobalsBatch(msg.indices));
    case "fingerprint": return engineCall("fingerprint", () => engine.getGlobalsFingerprint(msg.indices));
    case "write": return engineCall("write", () => { engine.setGlobalsBatch(msg.indices, msg.values); return true; });
    case "eval": return engineCall("eval", () => engine.evalInContextRich(msg.expr));
    case "dispatch": return engineCall("dispatch", () => engine.dispatchEvent(msg.type, msg.event));
    case "drain": return engineCall("drain", () => engine.drainPendingHostCalls());
    case "drainStatus": return engineCall("drainStatus", () => engine.drainPendingHostCallsStatus());
    case "resolve": return engineCall("resolve", () => engine.resolveHostCallback(msg.callId, msg.result));
    case "cancel": return engineCall("cancel", () => engine.cancelHostCallback(msg.callId));
    case "console": return engineCall("console", () => engine.takeConsole());
    case "usage": return engineCall("usage", () => engine.resourceUsage());
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
    // Not an engine throw (those are caught in engineCall): the adapter's
    // own failure. Recoverable unless the engine is gone.
    reply = { error: fail("host", describeThrownError(error).message, undefined, Boolean(engine?.disposed)) };
  }
  if (reply.error) {
    self.postMessage({ gen: msg.gen, id: msg.id, ok: false, error: reply.error });
  } else {
    let posted = false;
    try {
      self.postMessage({ gen: msg.gen, id: msg.id, ok: true, value: reply.value });
      posted = true;
    } catch (error) {
      // A value the engine produced that structured clone refuses cannot
      // happen for its data-only results; report it rather than leave the
      // request pending until its deadline.
      if (!posted) self.postMessage({ gen: msg.gen, id: msg.id, ok: false, error: fail("host", `reply could not be sent: ${describeThrownError(error).message}`) });
    }
  }
});
