# zipp-host: the reference host adapter (v1.0.0)

Two ES modules a browser application copies next to the released web package:

- `zipp-host.mjs` — the main-thread API: `createZippHost(config)`.
- `zipp-host.worker.mjs` — the Worker it spawns, one per guest.

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

**Lifecycle.** `created → initializing → ready → dead`. Every guest runs in its
own dedicated Worker and WASM instance, so `terminate()` is the complete
boundary: it ends the engine, its guest heap, and the compiled code the
engine's own `dispose()` cannot reclaim. A dead host is never reused; create
another. `host.dead` and `host.deathCause` say why.

**Generations.** Every message carries the host's generation. A reply from a
Worker that has since been terminated is dropped, and every request pending
when a host dies is rejected with that death's category, so a completion is
never routed by a guest-provided id alone.

**Deadlines.** The engine cannot interrupt a running guest from inside, so a
request that outlives its deadline is ended by terminating the Worker; the
request rejects with `deadline`, and so does everything else pending. Choose
`deadlineMs` per host (and per request through the `options` argument) for the
work you expect; `init` defaults to at least 30 s because it also fetches and
compiles the module.

**Error categories** (`error.category` on a `ZippHostError`):

| category | meaning | host afterwards |
| --- | --- | --- |
| `source` | the guest failed to compile or its top level threw | dead |
| `resource` | the engine crossed a resource ceiling | dead |
| `guest` | an ordinary guest throw during a call | usable |
| `host` | a bridge failure or a Worker crash | dead |
| `deadline` | the request outlived its deadline | dead |
| `terminated` | `terminate()` was called with requests pending | dead |
| `usage` | the API was used wrongly (after death, bad arguments) | unchanged |

**Capabilities.** Sync operations are granted once, before initialization, by
exact name; the engine makes the grant immutable afterwards. Bridge objects are
built inside the Worker by the module `bridgeModuleUrl` names, so the main
thread never holds them and they never cross a message boundary. Keep them
frozen, null-prototype adapters with own data-property methods, scoped to the
tenant; the engine's README lists the recognized operation names.

**Methods.** `init(source)`, `call(name, args)`, `readGlobals(indices)`,
`writeGlobals(indices, values)`, `fingerprint(indices)`, `evalRich(expr)`,
`dispatchEvent(type, event)`, `drainHostCalls()`, `resolveHostCall(id, result)`,
`cancelHostCall(id)`, `takeConsole()`, `resourceUsage()`, `terminate()`. Each
returns a promise (except `terminate`) and accepts an optional trailing
`{ deadlineMs }`.

Proven in real Workers by `tests/browser/worker-smoke.mjs` on Chromium,
Firefox and WebKit.
