# Developer handoff and release gates

Base reviewed: `62da28d9bdcd66fccbf5627d887d574016501f9d`.
This patch is intentionally additive. It must be treated as experimental until the
missing engine/backend gates have run against a full checkout.

## Gate A — actual engine integration, before any feature announcement

Build the normal `all` WASM variant. Run:

```sh
ZIPP_REQUIRE_INTEGRATION=1 node --test tests/integration.test.mjs
```

The first test binds graphs emitted by the actual plugin under CPython, passes
them through the current ZIPP validator and executes the current CPU backend,
comparing five complete logit tensors with stored PyTorch outputs. The second
compiles the real source with the real ZIPP WASM Engine, invokes the actual
bootstrap/hooks via `pythonCall`, and checks both logits and greedy generation.
Missing artifacts are a hard error in required mode. Fix actual compiler/module/
conversion incompatibilities before proceeding; do not replace the production
engine with a mock to turn this gate green.

Our standalone `session.test.mjs` intentionally uses named test doubles for
lifecycle behavior only. Our NumPy evaluator is an independent math check, not
ZIPP. Neither substitutes for these gates.

## Gate B — browser and backend acceptance

Run the demo with the trained fixture on CPU JavaScript, WASM SIMD, WebGL2 and
WebGPU where available. Capture real backend names, engine/source hashes, browser/
OS/device, warm/cold timings, peak memory and numerical tolerance. Compare all
logits, not just plausible generated text. Explicit backend requests must fail
visibly when unavailable. The bundled fixture is intentionally tiny and does not
establish large-model performance or language quality.

Test local-folder selection in the intended browsers, module loading, file picker
clone behavior, cancel during compile/weight reads/compute, long Python hooks,
malformed source, budget exhaustion, context exhaustion, lost GPU device, repeated
loads and teardown. Worker deadlines must remain outside guest code. Check that
no local-file content or generated text is transmitted and that reloading a
catalogue cannot silently replace an installed pinned identity.

## Gate C — first externally sourced checkpoint

Port GPT-Neo/TinyStories as a **new plugin**, not special cases in ModelSession.
Check real configuration, all state-dict names, tensor orientation, tied versus
untied embeddings, position embeddings, attention scaling, global/local attention,
normalization epsilon and activation variant. Implement a GPT-2 byte-BPE tokenizer
with its actual assets and differential Unicode/whitespace/special-token cases.
The existing character-v1 tokenizer is not a substitute.

Generate a conversion manifest and compare every tensor, several prompt logits,
and greedy continuation against the reference implementation. Test direct local
supply; no runtime internet fetch should be necessary. Do not call the custom toy
transformer checkpoint-compatible with GPT-Neo merely because both use attention.
Address larger tokenizer assets and vocabulary/output/memory limits explicitly.

## Gate D — scale without hiding costs

Keep the eager full-context path as a correctness oracle. Next add prepared,
resident model weights, GPU embedding/gather as appropriate, a fixed-capacity
per-layer KV cache and explicit cache writes/valid lengths; prefill and decode
need separate accounting. Use per-layer rank-three/four cache tensors according to the backend ABI;
combining layers and an additional batch axis can exceed the supported tensor
rank. Measure upload/readback and CPU planning separately from kernel time.

The current snapshot has graph tensors up to rank four but matmul only ranks two
and three. Do not infer support for one operation from a global rank ceiling.
Keep cache ownership, sessions, work bounds and aggregate resident memory under
host control. Add quantized formats/kernels only after F32 reference parity.
F16/BF16 file conversion alone does not supply quantized compute.

## Gate E — general runtime extensions and packaging

The initial registry is for causal-model Python plugins. Extract a library-only
plugin category and additional task drivers as separate versioned capabilities
when needed. Reuse source integrity/install logic rather than silently broadening
this ABI. Pure-Python algorithms can compose existing primitives; missing native
operations remain core/backend work.

Wire the static lab/catalogue into the actual ZIPP landing-page build, and then
add SoftN's host adapter/permissions/package tests from `SOFTN.md`. Design persistent
installation, provenance/signatures and compatible version selection deliberately.
Never let a model manifest turn into an unapproved network/package resolver.
