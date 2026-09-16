# Zipp integration contracts

This directory is part of the Zipp checkout. Python integration is implemented in
`../playground/engine.worker.js` and `src/zipp-python-adapter.mjs`; the Python
runtime records graphs through its built-in `zipp_gpu` module.

## Python inside Zipp WASM

The Worker creates one engine per run. After Python initialization and each hook,
it drains `takeHostRequests()`, admits the GPU kinds through an explicitly enabled
adapter, executes the work and delivers the matching request ID through
`pythonCall("__zipp_py_deliver", ...)`. Replies are serialized between engine calls.
Invalidating the run cancels queued work; completion never re-enters a disposed engine.
Host limits bound nodes, tensor sizes, work, pending requests and request lifetime.

The kinds (`GPU_KINDS` in `src/zipp-adapter.mjs`; the Python runtime's `_zipp_gpu`
module raises no other):

| kind | payload | reply |
|---|---|---|
| `gpu.execute` | a graph | `{outputs, stats}` |
| `gpu.session.create` | `{program, resident?, backend?}` | `{session, backend, inputs, outputs, step}` — `session` is an opaque id |
| `gpu.session.run` | `{session, steps: [{inputs: {nodeId: data}}], readback?, step?}` | `{steps: [{step, outputs}], outputs, step, stats}` |
| `gpu.session.download` | `{session, names}` | `{outputs}` |
| `gpu.session.dispose` | `{session}` | `{disposed: true}` |

Session ids live in the handler that minted them, one handler per Engine
generation, so a guest can never name another tenant's session; `invalidate()`
disposes them all, `maxSessions` bounds them per tenant on top of the runtime's
limit, and their resident bytes count against the runtime's `maxLogicalBytes`.
`zipp_gpu.Graph.prepare` is the Python surface (`Session.run`, `run_steps`,
`download`, `dispose`); the same program runs on the module's float32 reference
without a host, which is what `tests/python_corpus/ml_gpu_session.py` pins.

The actual WASM contract is tested by `../tests/node/python-gpu.cjs`; adapter-only
mocks additionally exercise failure and lifetime cases. The page has a Worker
execution deadline and reports the selected backend and initialization fallbacks.

## Browser JavaScript

Use `createRuntime({backend, limits})` from `src/runtime.mjs`, then
`await runtime.execute(graph)`. Await outstanding work before `runtime.dispose()`.
This is ordinary browser JavaScript and requires no guest engine.

## JavaScript guests inside Zipp

`src/zipp-guest.js` wraps the asynchronous `host.call` queue.
`src/zipp-adapter.mjs` integrates `drainPendingHostCallsStatus()`,
`resolveHostCallback()` and `cancelHostCallback()` with the same compute runtime,
for every kind in `GPU_KINDS` (`adapter.accepts(kind)`).
An embedder must explicitly grant `allowExecute`, keep generation ownership,
route accepted calls, and invalidate the adapter before destroying its engine.
The stock project playground wires the Python adapter only; guest JavaScript
has no implicit DOM, WebGL or GPU capability.

## Virtual filesystem changes

Python's `__zipp_py_vfs_changed` returns JSON `{version:1, changes:[...]}`.
An entry contains `{path, base64}` for a write (empty data is a real empty file),
or `{path, deleted:true}` for deletion. Rename produces both entries.
Hosts must use the protocol supplied by the paired engine build; do not infer
operations from data length. The landing build fingerprints glue and WASM together.

## Scope

The separate diagnostic demo executes exported graph JSON; it does not run its
displayed Python source. The native authoring harness uses installed CPython.
Neither path substitutes for testing Python inside Zipp's WASM artifact.
See [validation](VALIDATION.md) for commands and evidence boundaries.
