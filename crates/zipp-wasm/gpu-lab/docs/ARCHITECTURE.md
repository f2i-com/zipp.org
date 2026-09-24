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
independently of the integrated Python frontend.

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

An input's `data` is a flat list of numbers or a `Float32Array` (a ZIPP engine's
binary tensor transport), copied by the validator either way and checked in one
finiteness pass for the typed form. `runtime.execute(program, {typedOutputs: true})`
returns each output's `data` as a `Float32Array` of its own instead of a list;
`zipp-python-adapter.mjs` asks for that when a request's inputs are typed.

An empty shape means one scalar, not an empty array. Version 1 restricted shapes to
rank zero through two and matched shapes (or a scalar) elementwise. Empty tensors
are still intentionally absent. This keeps initial semantics explicit and testable.

There is no guest-provided shader string, dynamic property dispatch, URL, JavaScript
function, or host memory address in the protocol. Only fixed operation names enter
the kernel generators. Source specialization uses host-validated finite numbers and
bounded positive dimensions, not concatenated guest code.

## Graph IR v2

`"version": 2` names the extended operation set. One validator serves both versions
and every version-1 graph means exactly the same thing under version 2, so a host
that only ever sees v1 programs keeps its behaviour. Stage 1 targets dense
classification: a whole MLP training step — forward, loss, backward and the update
of every parameter and optimizer moment — is expressible as one graph, so optimizer
state stays on the device between steps instead of being read back and re-uploaded.

| Family | Operations | Shape rule |
|---|---|---|
| Sources | `input`, `full` | declared shape, rank 0-4 |
| Elementwise binary | `add`, `sub`, `mul`, `div` | NumPy broadcasting, right-aligned |
| Elementwise unary | `neg`, `exp`, `log`, `sqrt`, `tanh`, `sigmoid`, `relu`, `positive`, `gelu`, `gelu_grad` | shape preserved |
| Shape | `reshape`, `permute` (`dims`), `transpose` (matrices) | element count preserved |
| Reductions | `sum`, `mean`, with an optional `axis` and `keepdim` | axis removed, kept as 1, or whole tensor |
| Rows | `softmax`, `log_softmax` | last axis only, shape preserved |
| Linear algebra | `matmul` | `[M,K]@[K,N]`, or batched `[B,M,K]@[B,K,N]` where a batch of 1 or a matrix broadcasts |
| Losses | `cross_entropy`, `cross_entropy_grad` | logits `[N,C]` with integer class targets `[N]` |
| Optimizers | `sgd_update`, `momentum_update`, `adam_m`, `adam_v`, `adam_update` | equal shapes |
| Other | `life` | matrix, shape preserved |

Every kernel addresses a tensor as four dimensions padded on the left, with a
stride vector per operand; a stride of zero repeats a broadcast axis and a
permutation is the same strided gather. Binary nodes also carry a `mode`
(`same`, `aScalar`, `bScalar`, `general`) so the common cases skip stride
arithmetic. The validator derives all of this, including the padded dims and
strides, before any backend allocates: kernels never recompute shape logic from
guest fields.

Numerical rules that all four backends share:

- **Rounding.** Every intermediate is float32. Matrix products and axis reductions
  accumulate each output in index order, so the WASM kernels reproduce the
  JavaScript reference bit for bit; transcendental functions are evaluated in
  double (or by the shared polynomial approximations on the GPUs) and rounded once,
  agreeing to about one float32 ulp.
- **Whole-tensor reductions** use a pairwise tree, the same topology on all
  backends, so `sum` does not depend on the device's reduction width.
- **Softmax family** subtracts the row maximum before exponentiating.
  `cross_entropy` is fused: it reuses those row statistics rather than composing
  `log_softmax` with a gather, and its gradient is `(softmax - onehot)/N`.
- **GELU** is the exact-erf form `0.5*x*(1 + erf(x/sqrt(2)))`, not the tanh
  approximation. No backend language has `erf`, so all of them evaluate one shared
  approximation: an odd Taylor series for `|z| < 0.5` and Numerical Recipes' erfc
  Chebyshev fit above it (fractional error below 1.2e-7). `src/kernel-math.mjs`
  is its normative statement; the WGSL, GLSL and C copies mirror it line for line.
- **NaN.** `relu` keeps NaN rather than folding it to zero (`max(x, 0)` would
  differ between platforms), row maxima propagate NaN, and the readback check
  rejects any non-finite output with `NUMBER`. A graph that diverges therefore
  fails the same way on every backend instead of returning plausible numbers on
  one and an error on another.
- **Adam and momentum** follow PyTorch's evaluation order, including its
  `torch.lerp` branch for the first moment. Bias corrections are host constants
  computed once during validation, so no backend evaluates `pow`.

Integer class targets are validated as data (an `input` node holding integers in
`[0, C)`) before any kernel indexes with them.

## Prepared sessions

`execute()` pays for everything every time: it validates and copies every input
(1.3 ms of a 6.6 ms MNIST-scale Adam step on an RTX 5090), records every bind
group, uploads the weights it was just handed back, and reads 2.4 MB of weights
and moments it will upload again next step. `runtime.prepare(program, {resident})`
does that work once and returns a **Session**:

- **Fed inputs.** An `input` node without `data` is fed at each
  `session.run({inputs: {id: Float32Array}})`; every other input keeps the data
  it was validated with and is uploaded once, at `prepare`. A fed
  cross-entropy target is checked at each upload (integer class indices in
  `[0, C)`), where a static one is checked at validation.
- **Carried tensors.** `{id, op: 'input', shape, carry: 'p0'}` takes the value
  of output `p0` for the *next* step. The shapes are checked against the outputs
  at `prepare`; the source must be a computed node. Weights and optimizer state
  therefore never leave the device: on WebGPU the step's output is copied into
  the input's fixed buffer in stream order (`copyBufferToBuffer`), so every
  buffer of the plan is fixed and every bind group is reused; elsewhere the
  handles are swapped.
- **Resident outputs.** `resident: ['p0', ...]` names outputs kept on the device
  and excluded from the default readback; `session.download(names)` fetches
  them (one round trip of its own).
- **Multi-step runs.** `session.run([{inputs}, {inputs}, ...], {readback})`
  submits the steps back to back — one command buffer on WebGPU, one arena pass
  per step on WASM — and reads the named outputs of *every* step in one
  readback at the end (a per-step loss is eight scalars in one staging buffer).
  `adam_update` nodes advance their `step` per executed step from the program's
  own doubles (`adamStep`), so one prepared step trains a run; `{step}` restarts
  the count. Sessions return typed outputs.
- **One round trip.** A session run ends in `complete()`: the map of the
  staging buffer, both error scopes and the queue are awaited together, where
  `execute()` awaits the map, then the scopes and `onSubmittedWorkDone`
  (two round trips of roughly a millisecond each to Chrome's GPU process).
  The staging buffer is kept across runs.
- **Ownership and budgets.** Handles that outlive a step are reference counted
  (a static input, a carried input, a resident output and a pending readback
  each hold one) and never reach a backend pool; a WASM handle that outlives a
  step is a host copy, re-uploaded where the next step reads it. A runtime
  keeps at most `maxSessions` sessions, a run at most `maxStepsPerRun` steps and
  `maxOutputElements` read-back elements across them, and every live session's
  resident bytes plus the plan's own allocation count against
  `maxLogicalBytes`. `session.dispose()` releases the tensors (deferred past a
  run in flight); disposing the runtime disposes its sessions first.
- **Failure.** Everything before `begin` only validates, so a rejected run
  (`SHAPE`, `REFERENCE`, `LIMIT`, ...) leaves the session usable. Once device
  work has begun, carries and residents advance step by step, so a backend,
  readback or `finish` failure leaves them at no step in particular while
  `stepNumber` did not advance: the session is *poisoned* (`describe().poisoned`),
  `run` and `download` refuse with `STATE`, and only `dispose()` remains. The
  error reply of `gpu.session.run` carries `poisoned: true` in that case, and
  the Python `Session` (hosted or on the reference) applies the same rule.

Every backend has the same Session semantics: cpu-js and WASM keep arrays,
WebGL2 keeps textures, WebGPU keeps buffers. `tests/sessions.test.mjs` pins that
five Adam steps through a session equal five chained `execute()` calls bit for
bit on cpu-js and WASM, one step per run and five per run alike; the GPUs are
held to the differential tolerance in the browser harness.

## Memory ownership

`validateProgram()` owns a float32 copy of each input before the first asynchronous
yield. Later caller mutation cannot change that execution. GPU buffers and textures
are local to one runtime execution and never escape to guest code; a session's
resident tensors belong to that session and its runtime, reachable by a guest only
through the opaque id its own adapter minted.

Reference counts identify each node's last consumer. GPU resources can be released
when no consumer or output still needs them. Runtime cleanup also runs on failures.
Requested outputs are read before the final release; duplicate output names are
forbidden, while multiple different names may refer to the same tensor.

WebGPU's storage buffers are not aliases of WebAssembly linear memory. An input is
copied into a mapped upload allocation. A readback uses a separate MAP_READ staging
buffer, awaits mapping, copies the data, unmaps, and destroys the staging buffer.
It is not zero-copy CPU-to-GPU sharing.

One execution records **one** command encoder and one compute pass: every node's
dispatch goes into it and the queue is submitted once, when the outputs are copied
into a single staging buffer. Buffers are pooled by rounded byte size across
executions, so a repeated training step stops calling `createBuffer`. A buffer
freed *during* an execution is only reused for an output, never re-uploaded as an
input, because a `writeBuffer` would land ahead of commands already recorded
against it. Shapes and scalars travel in a 96-byte uniform block written into
256-byte slots of one uniform buffer (16,384 slots, the bound on dispatches per
submission), uploaded once at submit and bound through a dynamic offset; pipelines
are therefore keyed by kernel name, not by shape, and no graph value ever enters
shader text. Bind groups depend only on their kernel and storage buffers and are
cached (idle buffers are kept sorted, so a session run that starts from the same
pool allocates the same buffers and rebuilds nothing). Validation and out-of-memory
error scopes are pushed once per execution (per node in `debug` mode) and awaited
together with the queue drain.

Before recording, a plan is turned into its execution items (`src/fusion.mjs`,
cached per plan and backend): nodes no output depends on are dropped on every
backend; in a prepared session a node that depends only on constants is
computed once and kept; and a backend may run several nodes as one item.
WebGPU does so only where each element's arithmetic is unchanged: Adam's three
updates of a parameter, a matmul reading a transposed operand in place or
writing its result transposed, whole sums and means of up to 2048 values and
cross-entropy of up to 1024 rows in one workgroup (the same pairwise tree), and
chains of elementwise nodes of one size (a K=1 bias matmul may start one). A
chain kernel passes every intermediate through a bit operation against a
uniform zero, so no compiler can contract a product into a following sum; it
binds at most the device's storage buffers per stage. Use counts are recounted
over the items, so a fused intermediate never exists as a buffer.
`impl.fuse = false` turns backend fusion off for differential tests
(`crates/zipp-gpu/tests/exact_kernels.js` compares both, bit for bit, on
Vulkan, Direct3D 12 and Chrome).

The WebGL2 backend stores one scalar per texel: R32F (4 bytes) where the driver
reports it renderable, falling back to RGBA32F (16 bytes) where it does not, with
readback through `RED/FLOAT` only where `IMPLEMENTATION_COLOR_READ_FORMAT` allows
it. Texel indexing can cross texture rows; padding does not become logical output.
Programs are uniform-parameterized, so one program per kernel serves every shape,
and textures are pooled by layout. `gl.getError()` and `checkFramebufferStatus`
are *not* called per dispatch: one `getError()` after the execution's readbacks
covers every draw and read, and `createRuntime({debug: true})` restores the
per-dispatch checks. The one remaining eager check is a `getError()` after a
texture's *first* allocation, so an out-of-memory driver is attributed to the
allocation rather than to a later draw; pooled reuses skip it. `readPixels` is
synchronous in this backend.
Calling it from an async function does not make that native call non-blocking,
which is one reason to run it in a Worker.

The WASM backend uses a bounded arena, rebinding views after memory growth. The arena
is reset between serial graph executions. Individual `free()` calls do not shrink
its arena; tests exercise large allocations and repeated executions. Its module has
no imports. Memory committed by the WebAssembly instance may remain at its previous
high-water mark until the instance is released. The kernels are compiled with
`-msimd128 -ffp-contract=off`: matrix products use a 4x8 register block holding
eight `v128` accumulators across `k`, which keeps each output's summation in `k`
order, so the result still matches the JavaScript reference bit for bit. Browsers
without WebAssembly SIMD cannot instantiate this module and fall back to the
JavaScript reference, which is a backend-availability failure, not a wrong answer.

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

Defaults include 512 graph nodes, at most 4,194,304 elements per tensor, 65,536 per
dimension, at most 4,194,304 aggregate input elements and requested output elements,
64 named outputs, a 64 MiB summed logical node-allocation budget, and 100 million
estimated work units per graph. These defaults are development policy, not a
universal hardware optimum. They are sized so that an MNIST-scale MLP training step
(784-256-10, batch 64, Adam: ~58M work units, 254k uploaded and 611k read-back
elements) is accepted; they are not sized for a model that does not fit in a browser
tab.

Limits are resolved in three layers: these policy defaults, then the backend's
`limitHints()` — what the device and implementation can actually sustain — and then
the host's own `createRuntime({limits: ...})`, which always wins. `runtime.info()`
reports the result. The hints raise the work budget where the implementation is
faster (SIMD WASM 400M, WebGL2 500M, WebGPU 1G) and *lower* the element ceiling
where the device is smaller: WebGPU derives it from
`min(maxStorageBufferBindingSize, maxBufferSize)/4` and WebGL2 from its maximum
texture/viewport height. A hint never raises a size limit past the policy default.

Element counts are checked factor by factor while a shape is parsed, so a product
never leaves the safe-integer range before it is compared, and both the logical
byte total and the work total are re-checked after every node. A hostile graph is
bounded by node count, per-tensor elements, aggregate input and output elements,
logical bytes and work — raising the ceilings above does not remove any of those
checks.

The work estimate counts matrix multiply by twice its dimension product (batched by
its batch count), elementwise operations by element count (transcendental ones by
four times that), reductions by roughly twice input length, softmax and
cross-entropy by four passes, optimizer steps by eight units per element, and
cellular updates by nine reads per cell. It is not a GPU-time predictor or a
substitute for browser watchdogs. Logical bytes are not exact VRAM usage: texture
packing, padding, reduction scratch, staging, drivers, and in-flight command
retention add overhead.

The guest VM's instruction budget does not meter shaders. Keep an independent
compute permission, per-tenant request limits, queue bounds, and host deadlines.
Do not call this prototype a hardened multi-tenant GPU sandbox before additional
review and actual-device testing. Raw custom shaders are exposed in neither version.

## What to optimize next, in order

**First: real integration and correctness.** Exercise the Python module in the integrated
runtime, implement exactly one transport function, pass the numerical checks on
actual WebGPU and WebGL2 devices, and verify teardown with the real ZIPP WASM
artifact. Record its version/profile/hash, browser, adapter and failures.

**Second: bulk data transport.** JSON arrays are convenient but expensive. Float32
tensor inputs and outputs now cross as `Float32Array` copies (see Graph v1);
still open is a separate host-owned binary upload path with explicit ownership, element type,
length, and per-tenant handles. Respect the actual ZIPP conversion/queue limits.
Do not keep views into guest WASM memory across asynchronous work without a correct
lifetime and memory-growth contract. GPU uploads still involve copies unless an
actual supported sharing mechanism exists.

**Third: fuse easy expressions.** Partly done (see `src/fusion.mjs` above):
WebGPU runs elementwise chains, transposes into matmuls and small reductions
as single dispatches with unchanged arithmetic, keyed by the chain's
structure. Still open: fusing across reductions and softmax-shaped patterns,
and weighing shader compile cost against launch savings for rarely run chains.

**Fourth: tiled matrix kernels and batching.** Partly done. WebGPU multiplies
through 16x16 workgroup tiles (bounds handled by zero padding, so every invocation
reaches both barriers rather than branching around them) and the WASM kernels use a
4x8 register block; a graph is now one submission with pooled buffers. Still naive:
the WebGL2 matmul remains a per-output loop over `k`, because fragment shaders have
no workgroup memory and the equivalent needs multiple render targets and scissor
tiling. Still unmeasured: workgroup-memory and register-pressure tuning against
non-square and edge-tile shapes. Do not generalize WebGPU-only features to WebGL.

**Fifth: persistent device sessions.** Done (see *Prepared sessions*): weights
and optimizer state stay on the device as carried inputs and resident outputs,
sessions are generation-scoped behind opaque ids with per-tenant quotas, and a
lost device fails the next run closed. Still open: a resident tensor cannot be
shared between two sessions or passed to a plain `execute()`, a session's program
cannot change shape (prepare another), and per-step scalars other than Adam's
`step` (a learning-rate schedule) still need a fed scalar input.

**Sixth: a Python kernel subset.** Only after the above works, add a restricted
Python AST -> typed kernel IR -> WGSL/GLSL pipeline. Specify float/int types,
indexing rules, local variables, bounded loops and side effects. Reject unsupported
Python early. Shared compiler IR should distinguish parallel invocation IDs from
ordinary Python loop variables. This feature is not implemented by decorators in
this package; pretending a decorator is already a shader compiler would hide the
hard part.


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


## WebGL physical texture budget

`maxWebGLTextureBytes` (128 MiB by default) is checked before each texture allocation.
It charges padded width × height × 4 bytes on R32F devices and × 16 bytes on the
RGBA32F fallback, including intermediate reduction and row-statistic textures.
Freed textures stay charged while they sit in the pool and are evicted (oldest
first) when a new allocation would otherwise exceed the ceiling; `stats.webglTextureFormat`
reports which format is in use. The result's `webglTexturePeakBytes` reports peak
*live* requested texture storage — pooled-but-unused textures are excluded. Driver
overhead, program objects and CPU-side staging/readback allocations are outside
this counter.
