// zipp-host: a small reference host adapter for the zipp-wasm engine in a
// browser, version 1. The 11 September 2026 audit's ZIPP-17 asked for one
// consistent lifecycle/error/capability contract for embedders; this is it,
// kept deliberately small so an application can read it in one sitting.
//
// The shape:
//
//   const host = createZippHost({
//     moduleUrl: "/zipp/zipp_wasm.js",      // the web-target wasm-bindgen glue
//     wasmUrl: "/zipp/zipp_wasm_bg.wasm",   // its module
//     workerUrl: "/host-sdk/zipp-host.worker.mjs",
//     capabilities: ["ls.getItem"],         // exact sync grants; default none
//     bridgeModuleUrl: "/my/bridges.mjs",   // optional: builds db/ls/clipboard
//                                           // objects INSIDE the Worker
//     instructionBudget: 200_000_000,       // optional, before initScript
//     deadlineMs: 2000,                     // per request; expiry terminates
//   });
//   const symbols = await host.init(source);
//   await host.call("render", [state]);
//   ...
//   host.terminate();
//
// Every guest runs in its own dedicated Worker and WASM instance, so
// termination is the complete lifetime boundary: `terminate()` — or a
// request that outlives its deadline — ends the Worker, and with it every
// engine allocation, including the compiled code the engine's `dispose()`
// cannot give back. A terminated host is not reused; create another.
//
// Messages carry the host's GENERATION. A reply from a Worker that has since
// been terminated is dropped, and every pending request of a terminated
// generation is rejected with the `deadline` or `terminated` category, so a
// completion can never be routed by a guest-provided id alone.
//
// Error categories (`error.category`):
//   source     the guest failed to compile or its top level threw
//   resource   the engine crossed a resource ceiling (terminal: host is dead)
//   guest      an ordinary guest throw during a call (host stays usable)
//   host       a bridge/adapter failure or a Worker crash
//   deadline   the request outlived its deadline (terminal: host is dead)
//   terminated the host was terminated before the request completed
//   usage      the caller used the API wrongly (after terminate, bad args)
//
// Capabilities are set once, before initialization, and are immutable
// afterwards — the engine enforces that, this adapter merely never asks
// twice. Nothing here widens the engine's authority.

export const ZIPP_HOST_SDK_VERSION = "1.0.0";

export class ZippHostError extends Error {
  constructor(category, message, detail) {
    super(message);
    this.name = "ZippHostError";
    this.category = category;
    if (detail !== undefined) this.detail = detail;
  }
}

/** Classify an engine error message into a category. */
export function categorizeEngineError(message, phase) {
  const text = String(message);
  if (/exceeded|budget|limit|memory budget|disposed this engine/i.test(text)) return "resource";
  if (phase === "init") return "source";
  return "guest";
}

const TERMINAL = new Set(["resource", "deadline", "terminated"]);

export function createZippHost(config) {
  const {
    moduleUrl,
    wasmUrl,
    workerUrl,
    capabilities = [],
    bridgeModuleUrl = null,
    instructionBudget = null,
    fingerprintSeed = null,
    deadlineMs = 5000,
  } = config;
  if (!moduleUrl || !wasmUrl || !workerUrl) {
    throw new ZippHostError("usage", "createZippHost needs moduleUrl, wasmUrl and workerUrl");
  }
  const generation = (createZippHost.generation = (createZippHost.generation || 0) + 1);
  const pending = new Map();
  let nextId = 1;
  let worker = new Worker(workerUrl, { type: "module", name: `zipp-host-${generation}` });
  let state = "created"; // created -> initializing -> ready -> dead
  let deathCause = null;

  function die(category, message) {
    if (state === "dead") return;
    state = "dead";
    deathCause = new ZippHostError(category, message);
    try { worker.terminate(); } catch {}
    for (const [, req] of pending) {
      clearTimeout(req.timer);
      req.reject(new ZippHostError(category, message));
    }
    pending.clear();
  }

  worker.addEventListener("message", (event) => {
    const msg = event.data;
    if (!msg || msg.gen !== generation) return; // a stale generation: dropped
    const req = pending.get(msg.id);
    if (!req) return;
    pending.delete(msg.id);
    clearTimeout(req.timer);
    if (msg.ok) {
      req.resolve(msg.value);
    } else {
      const error = new ZippHostError(msg.error.category, msg.error.message, msg.error.detail);
      if (TERMINAL.has(error.category)) die(error.category, error.message);
      req.reject(error);
    }
  });
  worker.addEventListener("error", (event) => {
    die("host", `worker crashed: ${event.message || "unknown error"}`);
  });

  function request(op, payload, options = {}) {
    if (state === "dead") {
      return Promise.reject(new ZippHostError("usage", `host is terminated (${deathCause?.category})`));
    }
    const id = nextId++;
    const limit = options.deadlineMs ?? deadlineMs;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        // The Worker cannot be interrupted from inside; ending it is the
        // deadline. Everything pending dies with it.
        pending.delete(id);
        die("deadline", `${op} exceeded its ${limit} ms deadline`);
        reject(new ZippHostError("deadline", `${op} exceeded its ${limit} ms deadline`));
      }, limit);
      pending.set(id, { resolve, reject, timer });
      worker.postMessage({ gen: generation, id, op, ...payload });
    });
  }

  return {
    generation,
    get state() { return state; },
    get dead() { return state === "dead"; },
    get deathCause() { return deathCause; },

    /** Load the engine, apply the immutable configuration, compile and run the guest. */
    async init(source, options = {}) {
      if (state !== "created") throw new ZippHostError("usage", `init called in state ${state}`);
      state = "initializing";
      try {
        const symbols = await request("init", {
          moduleUrl, wasmUrl, capabilities, bridgeModuleUrl, instructionBudget, fingerprintSeed, source,
        }, { deadlineMs: options.deadlineMs ?? Math.max(deadlineMs, 30000) });
        state = "ready";
        return symbols;
      } catch (error) {
        if (state !== "dead") {
          // Any initialization failure disposes the engine: the host is dead.
          die(error.category || "source", error.message);
        }
        throw error;
      }
    },
    call: (name, args = [], options) => request("call", { name, args }, options),
    readGlobals: (indices, options) => request("read", { indices }, options),
    fingerprint: (indices, options) => request("fingerprint", { indices }, options),
    writeGlobals: (indices, values, options) => request("write", { indices, values }, options),
    evalRich: (expr, options) => request("eval", { expr }, options),
    dispatchEvent: (type, event, options) => request("dispatch", { type, event }, options),
    drainHostCalls: (options) => request("drain", {}, options),
    resolveHostCall: (callId, result, options) => request("resolve", { callId, result }, options),
    cancelHostCall: (callId, options) => request("cancel", { callId }, options),
    takeConsole: (options) => request("console", {}, options),
    resourceUsage: (options) => request("usage", {}, options),
    /** End the Worker and the WASM instance. Idempotent. */
    terminate() { die("terminated", "host terminated"); },
  };
}
