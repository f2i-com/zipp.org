# Architecture and expansion plan

## The first useful boundary is a graph, not arbitrary Python

The implemented path is:

```text
Python Graph / Tensor objects
          |
          | versioned plain-data graph
          v
Async transport boundary (one request per graph)
          |
          v
Host validation + exact operation allowlist
          |
          v
WebGPU / WebGL2 / standalone WASM / JavaScript reference
          |
          | named output readbacks only
          v
Plain result object + backend identity + wall-clock measurements
```

Python executes the orchestration. Adding tensors records operations. A Python
loop that repeats `state.life()` records bounded graph nodes, rather than forcing
a CPU readback after every step. Backend implementations provide the numerical
kernels. This does not require compiling Python control flow to shaders.

Three mechanisms should remain separate:

1. **Python implementation:** parsing, objects, imports, Python control flow, and
   any Python-to-ZIPP lowering. This repository is not available in the lab.
2. **Guest/host transport:** moving data-only graph requests and results across a
   runtime boundary, with tenant-scoped grants and lifetime handling.
3. **Compute backend:** numerical kernels and their actual device allocations.

The CPU/WASM fallback is a small freestanding C module. It is not the ZIPP VM and
not a Python runtime. Keeping it independent makes the host numerical API testable
without waiting for the private Python frontend's integration.

## Graph v1

Nodes use consecutive integer IDs. Inputs precede their consumers, so there are no
cycles, forward references, or name-based runtime lookups. The validator derives
operation shapes rather than trusting result metadata. Inputs and fills declare
shapes; other nodes reference earlier nodes by `a` and optionally `b`.

```json
{
  "version": 1,
  "nodes": [
    {"id": 0, "op": "input", "shape": [2], "data": [1, 2]},
    {"id": 1, "op": "input", "shape": [], "data": [3]},
    {"id": 2, "op": "mul", "a": 0, "b": 1}
  ],
  "outputs": [{"name": "result", "id": 2}]
}
```

An empty shape means one scalar, not an empty array. Shapes are restricted to rank
zero through two. Empty tensors and general broadcasting are intentionally absent.
This keeps initial semantics explicit and testable.

There is no guest-provided shader string, dynamic property dispatch, URL, JavaScript
function, or host memory address in the protocol. Only fixed operation names enter
the kernel generators. Source specialization uses host-validated finite numbers and
bounded positive dimensions, not concatenated guest code.

## Memory ownership

`validateProgram()` owns a float32 copy of each input before the first asynchronous
yield. Later caller mutation cannot change that execution. GPU buffers and textures
are local to one runtime execution and never escape to guest code.

Reference counts identify each node's last consumer. GPU resources can be released
when no consumer or output still needs them. Runtime cleanup also runs on failures.
Requested outputs are read before the final release; duplicate output names are
forbidden, while multiple different names may refer to the same tensor.

WebGPU's storage buffers are not aliases of WebAssembly linear memory. An input is
copied into a mapped upload allocation. A readback uses a separate MAP_READ staging
buffer, awaits mapping, copies the data, unmaps, and destroys the staging buffer.
It is not zero-copy CPU-to-GPU sharing.

The WebGL2 backend stores one scalar in the red channel of each RGBA32F texel. This
is deliberately simple and wasteful: texture storage uses four float channels.
Texel indexing can cross texture rows; padding does not become logical output.
`readPixels` is synchronous in this backend. Calling it from an async function does
not make that native call non-blocking, which is one reason to run it in a Worker.

The WASM backend uses a bounded arena, rebinding views after memory growth. The arena
is reset between serial graph executions. Individual `free()` calls do not shrink
its arena; tests exercise large allocations and repeated executions. Its module has
no imports. Memory committed by the WebAssembly instance may remain at its previous
high-water mark until the instance is released.

## Lifecycle and availability

Auto-selection tests backend initialization in this order: WebGPU, WebGL2, WASM,
JavaScript. It reports failed initialization attempts. Explicit backend selection
must either initialize that backend or fail. A graph that fails after execution
starts is never rerun silently on another backend.

Only one graph may run on a runtime at a time. Concurrent calls fail with BUSY;
the ZIPP adapter serializes accepted host requests. Runtime disposal while active
is rejected. An owner invalidates its adapter generation first, waits for queued
work to settle, then disposes the runtime. Late results are not delivered to a new
Engine merely because it reuses a numeric callback ID.

Device/context loss is terminal to the affected backend. Recreate the runtime
rather than treating old device allocations as valid. A demo Worker deadline can
stop host execution but cannot promise to cancel work already submitted to a GPU.

## Resource guardrails, not a new security proof

Defaults include 512 graph nodes, at most 1,048,576 elements per tensor, 4096 per
dimension, at most 1,048,576 aggregate input elements and requested output elements,
16 named outputs, a 32 MiB summed logical node-allocation budget, and 100 million
estimated work units per graph. These defaults are development policy, not a
universal hardware optimum. The trusted host can provide tighter limits.

The work estimate counts matrix multiply by its dimension product, elementwise
operations by element count, reductions by roughly twice input length, and cellular
updates by nine reads per cell. It is not a GPU-time predictor or a substitute for
browser watchdogs. Logical bytes are not exact VRAM usage: texture packing, padding,
reduction scratch, staging, drivers, and in-flight command retention add overhead.

The guest VM's instruction budget does not meter shaders. Keep an independent
compute permission, per-tenant request limits, queue bounds, and host deadlines.
Do not call this prototype a hardened multi-tenant GPU sandbox before additional
review and actual-device testing. Raw custom shaders are not exposed in v1.

## What to optimize next, in order

**First: real integration and correctness.** Load the Python module in the private
runtime, implement exactly one transport function, pass the numerical checks on
actual WebGPU and WebGL2 devices, and verify teardown with the real ZIPP WASM
artifact. Record its version/profile/hash, browser, adapter and failures.

**Second: bulk data transport.** JSON arrays are convenient but expensive. Add a
separate host-owned binary upload path with explicit ownership, element type,
length, and per-tenant handles. Respect the actual ZIPP conversion/queue limits.
Do not keep views into guest WASM memory across asynchronous work without a correct
lifetime and memory-growth contract. GPU uploads still involve copies unless an
actual supported sharing mechanism exists.

**Third: fuse easy expressions.** Lower an elementwise subgraph such as
`relu(a*b+scalar)` to one shader dispatch. The present implementation emits separate
nodes and kernels. Cache by structural expression and relevant dtype/shape policy.
Compare kernel launch savings against shader compile cost and cache growth. Keep a
reference interpreter and a way to disable fusion during differential tests.

**Fourth: tiled matrix kernels and batching.** Replace naive per-output matrix
loops with a tuned tiled WebGPU kernel, measuring workgroup memory, register
pressure, non-square shapes, and edge tiles. Avoid placing workgroup barriers inside
non-uniform bounds branches. Group graph commands into fewer submissions where
resource/error semantics permit. Do not generalize WebGPU-only features to WebGL.

**Fifth: persistent device sessions.** Retain weights and simulation states across
graph requests. Introduce opaque generation-scoped tensor handles, explicit close,
per-tenant quotas, and a disposal path for lost devices/Workers. A current Graph is
not a persistent GPU allocation: each `execute()` uploads its inputs afresh.

**Sixth: a Python kernel subset.** Only after the above works, add a restricted
Python AST -> typed kernel IR -> WGSL/GLSL pipeline. Specify float/int types,
indexing rules, local variables, bounded loops and side effects. Reject unsupported
Python early. Shared compiler IR should distinguish parallel invocation IDs from
ordinary Python loop variables. This feature is not implemented by decorators in
this package; pretending a decorator is already a shader compiler would hide the
hard part.

## NCA and small learning experiments

The included cellular automaton is a fixed rule and has no trainable parameters.
A practical next NCA inference experiment would keep `[H,W,C]` state in flat device
storage, run neighborhood perception, apply small learned channel transforms and
nonlinearity, then add a residual update. Rank-three tensor support and convolution/
channel-mixing kernels would need to be implemented and verified first.

Training requires an explicit backward pass or automatic differentiation, loss
reduction, parameter updates, and optimizer state. None is supplied by the tiny MLP
inference demo. Begin with a supervised tiny network and finite-difference gradient
checks before making claims about an online-learning NCA. Separate persistent cell
state, adaptive memory and trainable parameters in its API and tests.

## Measurement plan

Compare JS reference, WASM, WebGL2 and WebGPU only when every backend produces a
correct output. Log cold initialization/compilation separately from warm execution.
Test size ladders, odd lengths, rectangular matrices and repeated state updates.
Run reference calculations outside the timed GPU path. Report both end-to-end time
including input upload/readback and GPU-resident multi-step work where applicable.

Current demo statistics are wall-clock segments, not timestamp-query GPU execution
times. `submitWallMs` includes host work, pipeline creation and awaiting host API
operations; `readbackWallMs` includes transfers and waits; `totalWallMs` includes
cleanup/finish. No speedup is claimed by the validation artifacts.
