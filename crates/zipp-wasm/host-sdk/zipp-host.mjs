// zipp-host: a small reference host adapter for the zipp-wasm engine in a
// browser, version 1.1. The 11 September 2026 audit's ZIPP-17 asked for one
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
// Lifecycle: created → initializing → ready → dead, forwards only. `dead`
// is terminal and monotonic: nothing that completes later — an
// initialization reply that was already in flight, a late Worker message —
// can move a dead host back (the 11 September 2026 close audit's ZA-03).
// Ordinary operations need `ready`; before that they are refused locally
// with `usage` and nothing is posted to a Worker that may still be loading.
//
// Error categories (`error.category`):
//   source     the guest failed to compile or its top level threw
//              (terminal: initialization failure disposes the engine)
//   resource   the engine crossed a resource ceiling (terminal: host is dead)
//   guest      an ordinary guest throw during a call (host stays usable)
//   conversion a value did not fit the engine's host-value budget, in
//              either direction (host stays usable)
//   host       a bridge/adapter failure, a module that failed to load, or a
//              Worker crash (terminal when the Worker is gone)
//   deadline   the request outlived its deadline (terminal: host is dead)
//   terminated the host was terminated before the request completed
//   usage      the caller used the API wrongly: after terminate, before
//              init, a bad deadline, an argument structured clone refuses
//
// Categories come from the ENGINE's own account of each failure (its
// `lastErrorKind()` and `disposed` flag, relayed by the Worker as a
// structured envelope), never from the words in a message: a guest that
// throws `Error("business limit reached")` is a `guest` error and the host
// stays usable (ZA-01). `error.terminal` says whether the engine is gone.
//
// Capabilities are set once, before initialization, and are immutable
// afterwards — the engine enforces that, this adapter merely never asks
// twice. Nothing here widens the engine's authority.

export const ZIPP_HOST_SDK_VERSION = "1.1.0";

/** The longest deadline a timer can hold exactly (2^31 − 1 ms). */
export const MAX_DEADLINE_MS = 2147483647;

export class ZippHostError extends Error {
  constructor(category, message, detail, terminal = false) {
    super(message);
    this.name = "ZippHostError";
    this.category = category;
    this.terminal = terminal;
    if (detail !== undefined) this.detail = detail;
  }
}

/**
 * Map an engine failure to a category. `failure` is the structured account
 * the Worker takes from the engine — `{ kind, terminal }`, where `kind` is
 * the engine's `lastErrorKind()` — or, for a failure that carries no such
 * account (a bare message), an ordinary guest error. Text is never
 * consulted: the engine's recorder decides what is resource exhaustion.
 */
export function categorizeEngineError(failure, phase) {
  const kind = failure && typeof failure === "object" ? failure.kind : null;
  switch (kind) {
    case "resource": return "resource";
    case "source": return "source";
    case "conversion": return "conversion";
    case "usage": return "usage";
    case "guest": return phase === "init" ? "source" : "guest";
    default: return phase === "init" ? "source" : "guest";
  }
}

const TERMINAL = new Set(["resource", "deadline", "terminated"]);

function validDeadline(ms) {
  return typeof ms === "number" && Number.isFinite(ms) && ms > 0 && ms <= MAX_DEADLINE_MS;
}

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
  if (!validDeadline(deadlineMs)) {
    throw new ZippHostError("usage", `deadlineMs must be a finite number of milliseconds in (0, ${MAX_DEADLINE_MS}], not ${String(deadlineMs)}`);
  }
  // The configuration of this generation, frozen: a caller mutating its
  // own arrays afterwards changes nothing here.
  const grants = Object.freeze([...capabilities]);
  const seed = fingerprintSeed ? Object.freeze([...fingerprintSeed]) : null;
  const generation = (createZippHost.generation = (createZippHost.generation || 0) + 1);
  const pending = new Map();
  let nextId = 1;
  let worker = new Worker(workerUrl, { type: "module", name: `zipp-host-${generation}` });
  let state = "created"; // created -> initializing -> ready -> dead
  let deathCause = null;

  // One idempotent settlement per request: success, an error reply, a send
  // failure, the deadline and termination all go through here, so a promise
  // settles exactly once and a timer is cleared exactly by its own request.
  function settle(req, error, value) {
    if (req.settled) return;
    req.settled = true;
    clearTimeout(req.timer);
    pending.delete(req.id);
    if (error) req.reject(error);
    else req.resolve(value);
  }

  function die(category, message) {
    if (state === "dead") return;
    state = "dead";
    deathCause = new ZippHostError(category, message, undefined, true);
    try { worker.terminate(); } catch {}
    for (const req of [...pending.values()]) {
      settle(req, new ZippHostError(category, message, undefined, true));
    }
    pending.clear();
  }

  worker.addEventListener("message", (event) => {
    const msg = event.data;
    if (!msg || msg.gen !== generation) return; // a stale generation: dropped
    if (state === "dead") return; // monotonic: nothing settles after death
    const req = pending.get(msg.id);
    if (!req) return;
    if (msg.ok) {
      settle(req, null, msg.value);
    } else {
      const e = msg.error || {};
      const terminal = Boolean(e.terminal) || TERMINAL.has(e.category);
      const error = new ZippHostError(e.category || "host", e.message, e.detail, terminal);
      if (terminal) die(error.category, error.message);
      settle(req, error);
    }
  });
  worker.addEventListener("error", (event) => {
    die("host", `worker crashed: ${event.message || "unknown error"}`);
  });

  function request(op, payload, options = {}) {
    if (state === "dead") {
      return Promise.reject(new ZippHostError("usage", `host is terminated (${deathCause?.category})`, undefined, true));
    }
    if (op !== "init" && state !== "ready") {
      // Nothing is posted before initialization completes: the Worker may
      // still be loading the module or configuring bridges, and this
      // adapter promises no initialization queue (ZA-03).
      return Promise.reject(new ZippHostError("usage", `${op} called in state ${state}; await init() first`));
    }
    const limit = options.deadlineMs ?? deadlineMs;
    if (!validDeadline(limit)) {
      return Promise.reject(new ZippHostError("usage", `deadlineMs must be a finite number of milliseconds in (0, ${MAX_DEADLINE_MS}], not ${String(limit)}`));
    }
    const id = nextId++;
    return new Promise((resolve, reject) => {
      const req = { id, resolve, reject, timer: null, settled: false };
      req.timer = setTimeout(() => {
        // The Worker cannot be interrupted from inside; ending it is the
        // deadline. Everything pending dies with it.
        const message = `${op} exceeded its ${limit} ms deadline`;
        settle(req, new ZippHostError("deadline", message, undefined, true));
        die("deadline", message);
      }, limit);
      pending.set(id, req);
      try {
        worker.postMessage({ gen: generation, id, op, ...payload });
      } catch (error) {
        // The payload could not be encoded (a function, a symbol, a getter
        // that threw during structured clone, a fake transport): this
        // request's mistake, nobody else's. Nothing was sent, so the host
        // stays usable and no timer or entry outlives the rejection (ZA-02).
        settle(req, new ZippHostError("usage", `${op} could not be sent: ${error && error.message ? error.message : String(error)}`, { name: error && error.name }));
      }
    });
  }

  return {
    generation,
    get state() { return state; },
    get dead() { return state === "dead"; },
    get deathCause() { return deathCause; },
    /** How many requests are awaiting a reply (0 after any settlement). */
    get pendingRequests() { return pending.size; },

    /** Load the engine, apply the immutable configuration, compile and run the guest. */
    async init(source, options = {}) {
      if (state !== "created") throw new ZippHostError("usage", `init called in state ${state}`);
      state = "initializing";
      try {
        const symbols = await request("init", {
          moduleUrl, wasmUrl, capabilities: grants, bridgeModuleUrl, instructionBudget, fingerprintSeed: seed, source,
        }, { deadlineMs: options.deadlineMs ?? Math.max(deadlineMs, 30000) });
        // The reply may have been in flight when this host died; a dead
        // host never becomes ready (ZA-03).
        if (state !== "initializing") {
          throw new ZippHostError(deathCause?.category || "terminated", `host ${deathCause?.category || "terminated"} during initialization`, undefined, true);
        }
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
