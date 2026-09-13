# Handoff: integrate the GPU Lab into the real zipp-python repository

Inspect the actual repository before editing. This handoff comes with a standalone
experiment, not verified patches against your private checkout. Do not invent paths
or claim the Python interpreter is already connected to GPU compute.

## Objective

Run a Python-authored float32 tensor graph from zipp-python through the browser
GPU host, returning verified outputs through the runtime's real async/continuation
mechanism. Preserve the existing Python and ZIPP behavior.

## First inspect

Identify the Python parser/lowering/VM boundary, Python module registration,
operator-overloading and class support, float/int semantics, available `math`,
`struct` and `json` modules, async support, typed-array representation, Worker
lifecycle, host queue owner, tenant permissions, marshaling limits, and current
WASM release/testing procedures. Record the exact commit and artifact versions.

## Build the narrow integration first

1. Vendor/import the GPU Lab source into a clearly isolated module. Do not replace
   the ZIPP accelerator APIs or synchronous host bridge.
2. Instantiate one compute runtime per tenant/Worker and expose only an explicitly
   permitted `gpu.execute` queue kind. Route it through the existing single queue
   owner. Use generation-scoped completion and admission-failure cleanup.
3. Make the supplied guest JavaScript callback smoke test pass in real ZIPP WASM.
4. Register the Python graph-builder module or adapt it to the implemented subset
   without changing the versioned host schema. Do not pretend missing operator
   overloading or async support already exists.
5. Bind one Python graph submission function to the host completion mechanism.
   A callback/return-to-host graph submission is acceptable before async support.
6. Execute the exact vector, rectangular matmul, MLP inference and Life examples.
   Explicitly request WebGPU and WebGL2 in GPU acceptance tests so CPU fallback
   cannot masquerade as success. Record unsupported devices as skipped.
7. Keep JS/WASM references and compare with tolerances. Exercise odd lengths,
   scalar broadcasting, device loss, unauthorized operations, malformed inputs,
   queue limits, cancellation and replacement of the owning Worker.

## Do not implement prematurely

Do not add transparent NumPy imports, arbitrary Python-to-WGSL decorators, CUDA,
float64 promises, training, autograd, raw GPU handles exposed to guests, or a
rewritten VM. The current matmul is naive, but optimize only after verified wiring.
Never treat a WASM pointer/region string as a GPU address or retain raw guest views
across async work without an explicit lifetime/memory-growth contract.

## Deliverables

Return actual patches against the inspected commit, changed source, deterministic
regression tests, generated artifacts where appropriate, commands/results, browser
and adapter information, and a list of anything not executed. Provide a small
Python example that really runs in zipp-python WASM and prints its actual backend.
Preserve the independent GPU Lab smoke demo as a diagnostic isolation tool.

## Success criterion

The same Python-authored computation produces the expected result through the
actual private runtime and at least one real browser GPU backend. Host permission,
resource and teardown behavior are covered by regression tests. A standalone Python
or CPU-only pass is useful evidence but does not satisfy GPU integration acceptance.
