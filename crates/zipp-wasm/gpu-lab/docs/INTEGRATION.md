# Integrating with the real ZIPP / zipp-python runtime

## Access boundary

No private zipp-python source was available. This document identifies concrete host
integration contracts, not verified filenames or APIs inside that private project.
Do not claim this package is a merge-ready patch until the actual frontend, Python
module system and embedding surface have been inspected.

The public zipp-wasm README and source were fetched from the moving main branch on
13 September 2026. No immutable upstream checkout or build was obtained. Their
observed host APIs were `host.call(kind, args, cb)`,
`drainPendingHostCallsStatus()`, `resolveHostCallback(id, result)` and
`cancelHostCallback(id)`. The status-bearing drain returns `calls`, `hasMore` and
`stopReason`. Synchronous `__zippHostCall` cannot await. Queue kinds require host
allowlisting and generation-scoped ownership. [Z1, Z2]

## Host-side addition

Copy this package into a new directory, for example `gpu-lab/`, or import its source
as a separate local package. No existing code needs to be overwritten to try it.

Inside the trusted runtime Worker:

```javascript
import {createRuntime} from './gpu-lab/src/runtime.mjs';
import {createZippGPUAdapter} from './gpu-lab/src/zipp-adapter.mjs';

const compute = await createRuntime({
  backend: 'auto',
  limits: {maxNodes: 256, maxWork: 50_000_000},
});

// engine must be the actual live Engine instance belonging to this Worker/tenant.
const gpuAdapter = createZippGPUAdapter(engine, compute, {
  allowExecute: true, // derive this explicit grant from trusted app permissions
  maxPending: 16,
  maxRequests: 4096,
});
```

Create it once per Engine/Worker generation, not once per callback. The adapter does
not drain queues. Your existing central host dispatcher remains the only drain
owner. When it encounters an accepted request:

```javascript
if (gpuAdapter.accepts(call.kind)) {
  const completion = gpuAdapter.dispatch(call);
  // Attach host error handling and schedule the normal guest pump/drain after
  // completion. See the rejection rules below; do not discard rejected Promises.
}
```

`dispatch()` consumes a previously drained `{id, kind, args}` record. It waits its
turn, runs compute, and resolves that exact callback with:

```javascript
{ok: true, value: {version: 1, backend: 'webgpu', outputs: {...}, stats: {...}}}
// or
{ok: false, error: {code: 'SHAPE', message: '...'}}
```

The illustrative result objects above show the envelope, not literal executable
objects because the omitted properties are placeholders.

Normal execution/validation errors are returned to the guest in the error envelope.
Early admission failures such as invalid IDs, unknown kinds, exceeded quotas,
already invalidated generations, or duplicate IDs reject `dispatch()` itself. The
central dispatcher must handle that rejection. For a valid, newly admitted callback
ID rejected for quota, resolve an error or cancel it according to host policy.
Never settle an existing in-flight callback in response to a duplicate-ID rejection.
Never resolve a callback into a disposed or replaced generation. Unknown non-GPU
kinds belong to the existing dispatcher, not the GPU adapter.

The central event loop must continue draining when `hasMore` is true, even when a
bounded drain produces no calls. Do not synchronously re-enter an Engine during an
active exported call. GPU work is asynchronous host work; callback delivery and the
normal guest job pump happen after the active guest call has returned.

## Guest JavaScript smoke test

Prepend `src/zipp-guest.js` to trusted guest helper source before initialization.
It supplies `gpuExecute(program, done)` and `gpuExecuteAsync(program)` using the
queued `host.call` transport. It does not use the synchronous accelerator bridge.

```javascript
var p = {
  version: 1,
  nodes: [
    {id: 0, op: 'input', shape: [3], data: [1, 2, 3]},
    {id: 1, op: 'input', shape: [], data: [2]},
    {id: 2, op: 'mul', a: 0, b: 1}
  ],
  outputs: [{name: 'result', id: 2}]
};

gpuExecute(p, function (error, result) {
  if (error) { console.error(error.message); return; }
  console.log(result.backend, result.outputs.result.data);
});
```

The included adapter test uses a mock host queue and JavaScript VM. It validates the
contract shape and callback handling, not execution inside actual ZIPP WASM.

## Python module integration

`python/zipp_gpu.py` needs classes, operator methods, ordinary lists/dictionaries,
iteration, exceptions, and a small amount of `math`/`struct` support for float32
validation. `to_json()` additionally needs `json`. `Graph.run()` uses async/await.
Check these against the actual private runtime; do not assume it implements them.

The smallest integration is a function with this conceptual contract:

```text
submit_gpu_graph(program: plain data) -> eventually a plain result object
```

It must return the completed host result, not serialize a host Promise as a Python
value. One guest coroutine may suspend while the host runs `gpu.execute`, then
resume when the callback arrives. Do not block the browser main thread waiting for
that callback or try to force asynchronous buffer mapping through a synchronous
host import.

When Python async/await is not ready, use the implemented `gpu.program(...)` API.
Return that plain graph to the trusted host at the end of Python execution, then
call `compute.execute(program)` there. Deliver the result through whatever explicit
re-entry/continuation mechanism the actual runtime already supports. This does not
require the Python guest to read an asynchronous value inline.

If `math`/`struct` are absent, extract the graph builder's float32 conversion into
a small Python-host numeric helper, retaining authoritative validation on the host.
Do not remove host validation or silently broaden dtypes. If operator overloading
is absent, expose explicit operations backed by the same graph schema until the
frontend can support the nicer syntax.

## Ownership and teardown

```javascript
// First make this generation permanently ineligible for result delivery.
gpuAdapter.invalidate();
// The host also cancels pending guest callbacks / tears down the owning Worker
// according to its normal lifecycle. Adapter invalidation alone does not free
// those guest callback records.
await gpuAdapter.idle();
compute.dispose();
```

Do not call `compute.dispose()` during active execution. Do not reuse this adapter
for a replacement Engine. On an unrecoverable timeout, terminate the owning Worker
and create a fresh runtime rather than reusing uncertain state. Browser/device work
already submitted might finish even after the originating Worker ends.

## Acceptance gates before merging

Run all package tests and the real-origin demo checks. Then run the guest smoke
script in the actual current ZIPP WASM artifact and the equivalent Python graph in
the private Python runtime. Capture artifact hashes and browser/adapter identity.
Test malformed shapes, unauthorized compute, mixed non-GPU queue traffic, callback
cancellation, more-than-one-drain bursts, device loss and Worker replacement.
Verify no late callback is routed to a different generation and no guest callbacks
are retained after host-side admission failure.

Start with small requests. Graph JSON arrays have additional transport and parsing
cost, and the actual guest runtime's marshaling/queue limits can be lower than the
standalone host's graph limits. Host limits are upper bounds, not promises that all
such graphs fit through every frontend.

## Sources

[Z1] Public ZIPP WASM README, observed 2026-09-13:
https://raw.githubusercontent.com/f2i-com/zipp.org/main/crates/zipp-wasm/README.md

[Z2] Public ZIPP WASM source, observed 2026-09-13:
https://raw.githubusercontent.com/f2i-com/zipp.org/main/crates/zipp-wasm/src/lib.rs
