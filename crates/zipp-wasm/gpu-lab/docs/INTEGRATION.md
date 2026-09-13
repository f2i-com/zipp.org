# Zipp integration contracts

This directory is part of the Zipp checkout. Python integration is implemented in
`../playground/engine.worker.js` and `src/zipp-python-adapter.mjs`; the Python
runtime records graphs through its built-in `zipp_gpu` module.

## Python inside Zipp WASM

The Worker creates one engine per run. After Python initialization and each hook,
it drains `takeHostRequests()`, admits `gpu.execute` through an explicitly enabled
adapter, executes the graph and delivers the matching request ID through
`pythonCall("__zipp_py_deliver", ...)`. Replies are serialized between engine calls.
Invalidating the run cancels queued work; completion never re-enters a disposed engine.
Host limits bound nodes, tensor sizes, work, pending requests and request lifetime.

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
`resolveHostCallback()` and `cancelHostCallback()` with the same compute runtime.
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
