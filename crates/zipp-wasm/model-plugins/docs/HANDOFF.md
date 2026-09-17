# Developer handoff and release gates

Base reviewed: `62da28d9bdcd66fccbf5627d887d574016501f9d`.
This patch is intentionally additive. It must be treated as experimental until the
missing engine/backend gates have run against a full checkout.

## Gate A — actual engine integration, before any feature announcement

**Status: passing in this repository** (see `VALIDATION.md` for versions and error
bounds). Both gates ran in required mode against the checked-out ZIPP runtime and
a locally built `dist/all`. Rerun them on every engine or plugin change; a green
Gate A is engine integration only, and says nothing about Gates B through E.

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

**Status: partly done.** One Chrome on one Windows machine has run the lab end to
end on all four backends, and `demo/parity.html` compared complete logit tensors
against the PyTorch reference on each (`VALIDATION.md` has the numbers). The
failure, cancellation, memory and device-matrix work below has not run, and no
browser check is automated: treat the page as evidence a person reproduces, not a
gate that guards a merge.

Run the demo with the trained fixture on CPU JavaScript, WASM SIMD, WebGL2 and
WebGPU where available. Capture real backend names, engine/source hashes, browser/
OS/device, warm/cold timings, peak memory and numerical tolerance. Compare all
logits, not just plausible generated text — `demo/parity.html` does exactly that
for the bundled fixture, on every backend the browser offers, and reports each
backend's worst absolute error rather than a verdict alone. Explicit backend requests must fail
visibly when unavailable. The bundled fixture is intentionally tiny and does not
establish large-model performance or language quality.

Test local-folder selection in the intended browsers, module loading, file picker
clone behavior, cancel during compile/weight reads/compute, long Python hooks,
malformed source, budget exhaustion, context exhaustion, lost GPU device, repeated
loads and teardown. Worker deadlines must remain outside guest code. Check that
no local-file content or generated text is transmitted and that reloading a
catalogue cannot silently replace an installed pinned identity.

## Gate C — first externally sourced checkpoint

**This is the next model milestone.** Port GPT-Neo/TinyStories as a **new plugin**,
not special cases in ModelSession and not a configuration of `tiny-causal`.
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

The host already refuses that claim rather than relying on reviewers to catch it:
`tiny-causal` declares `checkpoint_format: zipp.tiny-causal-v1` with
`tokenizer_formats: [character-v1]`, and a model naming anything else is rejected
with `CHECKPOINT` or `TOKENIZER` before an engine is constructed
(`tests/host.test.mjs`, `tests/test_plugins.py`). The new plugin therefore
declares its own family — `hf.gpt-neo-v1` with a `gpt2-byte-bpe-v1` tokenizer, or
whatever names its conversion actually implements — and every converted checkpoint
names the same family. Widening `tiny-causal`'s declaration to make a foreign
checkpoint load, rather than shipping the plugin, is the failure this gate exists
to prevent: the refusal is the honest state until the mapping is verified.

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
