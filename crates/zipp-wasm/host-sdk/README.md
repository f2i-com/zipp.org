# zipp-host: the reference host adapter (v1.1.1)

Two ES modules a browser application copies next to the released web package:

- `zipp-host.mjs` — the main-thread API: `createZippHost(config)`.
- `zipp-host.worker.mjs` — the Worker it spawns, one per guest (it imports
  the classifier from `zipp-host.mjs`, so keep the two files together).

They shape messages and enforce nothing the engine does not already enforce;
they exist so every embedder gets the same lifecycle, error and capability
contract (the 11 September 2026 audit's ZIPP-17) instead of writing its own.

```js
import { createZippHost } from "./host-sdk/zipp-host.mjs";

const host = createZippHost({
  moduleUrl: "/zipp/zipp_wasm.js",
  wasmUrl: "/zipp/zipp_wasm_bg.wasm",
  workerUrl: "/host-sdk/zipp-host.worker.mjs",
  capabilities: ["ls.getItem", "ls.setItem"],   // exact sync grants; default none
  bridgeModuleUrl: "/my/bridges.mjs",           // default export builds { db, localStorage, clipboard } IN the Worker
  instructionBudget: 200_000_000,               // optional, applied before initScript
  deadlineMs: 2000,                             // per request; expiry terminates the Worker
});
const symbols = await host.init(source);        // compile + run the top level
await host.call("render", [state]);
const [snapshot] = await host.readGlobals([symbols.state.index]);
for (const req of await host.drainHostCalls()) {
  // dispatch by an exact allowlist of your own, then:
  await host.resolveHostCall(req.id, result);   // true if a callback ran
}
host.terminate();                               // the Worker and its WASM instance end
```

**Distribution.** The released web archive (`zipp-wasm-<version>-web.zip`)
ships this directory as `host-sdk/` beside the generated bindings, at the
exact revision the module was built from (`BUILD-INFO.txt` names both the
commit and this SDK version). Serve the two files from one directory; the
Worker resolves `./zipp-host.mjs` relative to its own URL.

**Lifecycle.** `created → initializing → ready → dead`, forwards only. Every
guest runs in its own dedicated Worker and WASM instance, so `terminate()` is
the complete boundary: it ends the engine, its guest heap, and the compiled
code the engine's own `dispose()` cannot reclaim. A dead host is never reused;
create another. `host.dead` and `host.deathCause` say why. `dead` is
monotonic: an initialization reply that was in flight when the host was
terminated cannot make it `ready`, and a message from a dead generation
settles nothing. Ordinary operations require `ready`; called earlier they
reject locally with `usage` and nothing is posted to a Worker that may still
be loading — there is no initialization queue.

**Generations.** Every message carries the host's generation. A reply from a
Worker that has since been terminated is dropped, and every request pending
when a host dies is rejected with that death's category, so a completion is
never routed by a guest-provided id alone.

**Settlement.** Each request settles exactly once — by its reply, by a send
failure, by its deadline or by the host's death — and its own timer and
pending entry go with it. A payload that structured clone refuses (a
function, a symbol, a getter that throws) rejects with `usage` before
anything is posted; the host stays usable and no deadline is left armed.

**Deadlines.** The engine cannot interrupt a running guest from inside, so a
request that outlives its deadline is ended by terminating the Worker; the
request rejects with `deadline`, and so does everything else pending. Choose
`deadlineMs` per host (and per request through the `options` argument) for the
work you expect; `init` defaults to at least 30 s because it also fetches and
compiles the module. A deadline must be a finite number of milliseconds in
`(0, 2147483647]` (`MAX_DEADLINE_MS`); anything else is a `usage` error rather
than a timer that fires at once or never.

**Error categories** (`error.category` on a `ZippHostError`; `error.terminal`
says whether the engine is gone):

| category | meaning | host afterwards |
| --- | --- | --- |
| `source` | the guest failed to compile or its top level threw | dead |
| `resource` | the engine's recorder reported a resource ceiling | dead |
| `guest` | an ordinary guest throw during a call | usable |
| `conversion` | a value did not fit the engine's host-value budget, either way | usable |
| `host` | a module or bridge failed to load, or the Worker crashed | dead |
| `deadline` | the request outlived its deadline | dead |
| `terminated` | `terminate()` was called with requests pending | dead |
| `usage` | the API was used wrongly (after death, before `ready`, a bad deadline, a payload that cannot be cloned) | unchanged |

Categories are taken from the engine's own account of each failure — its
`lastErrorKind()` (`guest`, `conversion`, `usage`, `source`, `resource`) and
its `disposed` flag, relayed by the Worker as a structured envelope
`{ category, message, terminal }` — never from the words in a message. A guest
that throws `Error("business limit reached")` is a `guest` error and the host
stays usable; only the recorder's verdict, a deadline or termination is
terminal. `categorizeEngineError(failure, phase)` is the shared classifier;
given a bare message instead of an envelope it answers `guest` (`source`
during initialization).

**Capabilities.** Sync operations are granted once, before initialization, by
exact name; the engine makes the grant immutable afterwards, and this adapter
freezes its copy of the configuration at creation. Bridge objects are built
inside the Worker by the module `bridgeModuleUrl` names, so the main thread
never holds them and they never cross a message boundary. Keep them frozen,
null-prototype adapters with own data-property methods, scoped to the tenant;
the engine's README lists the recognized operation names.

**Methods.** `init(source)`, `call(name, args)`, `readGlobals(indices)`,
`writeGlobals(indices, values)`, `fingerprint(indices)`, `evalRich(expr)`,
`dispatchEvent(type, event)`, `drainHostCalls()`, `resolveHostCall(id, result)`,
`cancelHostCall(id)`, `takeConsole()`, `resourceUsage()`, `terminate()`. Each
returns a promise (except `terminate`) and accepts an optional trailing
`{ deadlineMs }`. `host.pendingRequests` counts requests awaiting a reply.

Proven in real Workers by `tests/browser/worker-smoke.mjs` on Chromium,
Firefox and WebKit; the main-thread state handling is pinned by
`tests/node/sdk-contract.mjs` under a mocked Worker, which the boundary
suite runs without an engine build. `tests/node/sdk-worker-contract.mjs` runs
the actual Worker adapter with small test modules to verify error envelopes
and recovery. Thrown values whose `message`, `name`, or string conversion
also throws are reported safely; they cannot leave a request or its deadline
pending merely because the error could not be printed.
