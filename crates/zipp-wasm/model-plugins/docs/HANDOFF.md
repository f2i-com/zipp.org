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

**Status: done for GPT-Neo, on one checkpoint.** `plugins/gpt-neo/` reads
GPT-Neo/TinyStories checkpoints with the GPT-2 byte-level BPE tokenizer they were
trained with, matching transformers to 5.6e-05 on complete logits with identical
greedy continuations and exact tokenizer agreement over a differential corpus
(`tests/test_gpt_neo.py`, and two checkout gates that skip without a checkpoint
because third-party weights are not in this repository). It reads a Hugging Face
folder as published — no conversion, no rewritten tensors.

What that does **not** establish: it is one checkpoint of one family at 3M
parameters. Another GPT-Neo size, another vocabulary, or a checkpoint with
different config fields has not been run. Every further family needs its own
plugin and its own verification; the paragraphs below are the checklist that was
followed and is the checklist for the next one.

Port each further family as a **new plugin**, not special cases in ModelSession
and not a configuration of an existing one.
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

A checkpoint folder is read where it lives. The plugin declares the config file
it understands and the tokenizer files it needs; `ModelSession.openNative` reads
them, hashes everything including each shard, and requires the host to approve
those digests before any of it reaches guest code. There is no pin, because a
folder published by someone else carries none, and that is why it is a separate
entry point rather than a fallback `open` slides into.
`tools/pin_checkpoint.py` writes the stronger, pinned manifest when one is wanted.

The host already refuses a compatibility claim rather than relying on reviewers:
`tiny-causal` declares `checkpoint_format: zipp.tiny-causal-v1` with
`tokenizer_formats: [character-v1]`, and a model naming anything else is rejected
with `CHECKPOINT` or `TOKENIZER` before an engine is constructed
(`tests/host.test.mjs`, `tests/test_plugins.py`). The new plugin therefore
declares its own family — `hf.gpt-neo-v1` with a `gpt2-byte-bpe-v1` tokenizer, or
whatever names its conversion actually implements — and every converted checkpoint
names the same family. Widening `tiny-causal`'s declaration to make a foreign
checkpoint load, rather than shipping the plugin, is the failure this gate exists
to prevent: the refusal is the honest state until the mapping is verified.

### What a checkpoint still costs here

Measured on TinyStories-1M (3M parameters, 50,257 vocabulary) with the CPU
JavaScript backend: a session opens in about two seconds, of which most is
hashing 46 MiB for approval, and each generated token re-uploads and re-runs the
whole model over the whole context. There is no KV cache and no resident weight
on *this* path, so the embedding matrix crosses the boundary once per token.
That was Gate D's subject and Gate D has since answered it -- the cached decode
path keeps weights resident and uploads 1,664 elements a step. The measurement
above is what the eager path costs, which is why it is kept as an oracle rather
than as a way to serve a model.

Two engine costs shaped the plugin and are worth fixing at the source rather than
working around again:

* `json.loads` is superlinear in the number of **integers** it decodes: 40,000 of
  them take 6.5s where 40,000 strings take 0.12s, and a 50,000-entry vocabulary
  never finished. String values and Python-level `d[k] = v` are both linear, so
  the cost is in decoding numbers, not in objects or dictionaries.
* `str.find` and `str.split` are regular-expression backed, so scanning a
  megabyte from guest Python is quadratic — a vocabulary CPython reads in 0.06s
  took over 200 seconds. The engine also derives a regular expression's step
  ceiling from the remaining instruction budget, so exhausting that budget
  surfaces as "regular expression exceeded its execution budget" rather than as
  the instruction-budget error it actually is.

The host therefore hands a plugin its tokenizer assets already decomposed, by a
declared transport (`text`, `lines`, `json-pairs`), and the plugin builds its
tables from host values. That crosses the boundary in 0.07s. If the two costs
above are fixed, the transports stay useful but a plugin could also just read the
files.

## Gate D — scale without hiding costs

**Status: the KV cache and resident weights are done.** A plugin may build a
second graph, `build_decode_graph(config)`, which the host prepares once through
`runtime.prepare`. Weights become static inputs, uploaded once; key and value
caches are inputs marked `carry`, so they live on the device and never cross the
boundary; each token attends over the cache instead of recomputing the context.
The eager full-context path is unchanged and is still the oracle: the checkout
gate runs both and requires identical tokens.

Measured on TinyStories-1M, generating 16 tokens:

| | per token | uploaded per step |
| --- | --- | --- |
| Recomputing the context (eager) | 104 ms, CPU JavaScript | 3,619,160 elements |
| Cached decode | 48 ms, CPU JavaScript | 1,664 elements |
| Cached decode, WASM SIMD in a browser | 2.6 ms | 1,664 elements |
| Cached decode, WebGPU in a browser | 3.5 ms | 1,664 elements |

The protocol has no scatter, so a cache is written arithmetically:
`cache * (1 - write) + write @ new`, where `write` is a one-hot column the host
feeds for the current position. Caches need no clearing between generations,
because every position is written before the mask ever unmasks it.

**Quantized formats and kernels are done too, and are how a real checkpoint
fits.** Q4_K and Q6_K weights stay in the file's own block format on the device
and are decoded inside the matmul, on all four backends. Qwen3-0.6B holds at
373 MB where float32 would need 2,274 MB. The decoders are checked against
gguf-py — an independent implementation — for all fifteen formats bit for bit,
and the whole path is checked end to end against transformers reading the same
GGUF: 455,808 logits, worst difference 7.391e-5. See `docs/VALIDATION.md`.

**A model can also be divided across devices.** `build_decode_stage` and
`build_prefill_stage` build a graph over any inclusive range of layers; a stage
loads only its own tensors and carries only its own caches, and hands the next
one a residual stream. Halves, quarters and uneven shares all reproduce the
whole model's logits bit for bit, including across worker threads with the
buffer transferred rather than copied. `describe_stage` gives a stage an
identity so a peer cannot be fed activations from a different checkpoint that
happens to share a residual width.

What is still missing here: a **batched prefill that warms the decode caches**.
`build_prefill_stage` is a batched forward and produces the residual stream, not
the key and value tensors a decode session carries — those still start at zero,
so a prompt is fed through the decode graphs one position at a time. That is
correct and costs a step per prompt token where one pass would do. Making a
prefill emit `k{n}`/`v{n}` for a decode session to begin from is the next real
piece of work in this gate. `run` also accepts up to 64 steps per submission and
the driver does not yet use that.

Keep the eager full-context path as a correctness oracle. Remaining: GPU
embedding/gather as appropriate, cache-warming prefill, and explicit
valid-length accounting; prefill and decode need separate accounting. Use per-layer rank-three/four cache tensors according to the backend ABI;
combining layers and an additional batch axis can exceed the supported tensor
rank. Measure upload/readback and CPU planning separately from kernel time.

The current snapshot has graph tensors up to rank four but matmul only ranks two
and three. Do not infer support for one operation from a global rank ceiling.

### Recurrent layers need no new kernels

An earlier assessment in this work said that a Gated-DeltaNet linear-attention
layer — 18 of Qwen3.5-0.8B's 24 layers — needs scan and convolution primitives
Graph v2 lacks, and that unrolling one would exhaust the node budget. That was
wrong, and `tests/recurrence.test.mjs` is the correction: both pieces run on the
real runtime today, out of operations that already exist.

* A causal depthwise convolution over a window of four is a carried window
  matrix multiplied by a fixed shift matrix, with the new sample placed by a
  one-hot column — then weighted and summed.
* A gated delta rule, `S' = S * decay + k v`, is a batched outer product added
  to a decayed carried state, read back as `q S'`.

A scan operation is only needed to process a whole sequence inside one graph. A
prepared session already carries state between runs, so the recurrence is one
step per run and **the graph does not grow with the sequence** — a recurrent
step is thirteen nodes whether it runs once or a thousand times. The cost is
that prefill is sequential, which is the same cost the KV cache path already
pays.

### What Qwen3.5-0.8B would still need

Not operations. Memory and precision:

| | measured | ceiling here |
| --- | --- | --- |
| Tied embedding, as the output projection | 248,320 x 1024 = 254,279,680 elements | `maxElements` 4,194,304 — 60x over |
| The same tensor as F32 | 1,017 MiB | — |
| Whole checkpoint, bf16 as published | 1.63 GiB | — |
| Whole checkpoint, expanded to F32 by this loader | 3.26 GiB | WASM linear memory maximum is 1 GiB |
| Resident allocation | 1.63 GiB at best | `maxLogicalBytes` 64 MiB — 26x over |

So the work is: storing and computing in the checkpoint's own dtype instead of
expanding to F32, a chunked or much higher element ceiling for the vocabulary
projection, and a resident budget that a device can actually hold. Quantization
would cut the first by two to four times again. Until those land, a plugin for
that family would be a plugin that cannot load its own checkpoint — which is the
thing this repository refuses to ship.
Keep cache ownership, sessions, work bounds and aggregate resident memory under
host control. Quantized formats and kernels came after F32 reference parity, as
this said they should, and are now in place: see Gate D above and
`interop/GGUF.md`. F16/BF16 file conversion alone still does not supply
quantized compute, and did not.

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
