# ZIPP local-model Python plugins — experimental source overlay

**Status: host/Python foundation, not a verified ZIPP release.** This is an
implementation designed against `f2i-com/zipp.org` commit
`62da28d9bdcd66fccbf5627d887d574016501f9d`, and it is mostly additive: it adds this
directory, and it also extends `gpu-lab`'s backends with quantized matmul and
points `rust/` at the external `f2i-gguf-quants` crate so a kernel decodes blocks
the way the reader does. It does not modify the landing-page build or change SoftN.

415 standalone Node tests (136 here, 279 in `gpu-lab`), 30 CPython/NumPy tests and
the full-checkout gates pass in this repository: the plugin's graphs run through
the current ZIPP Graph v2 validator and CPU backend, and the plugin's Python
compiles and generates inside a locally built Python-enabled ZIPP WASM engine,
matching the stored PyTorch reference in both cases. Some suites need a checkpoint
on disk and skip without one, since none is redistributed here.

Browser and GPU acceptance **have** since run for the fixture and GPT-Neo models:
complete logit tensors on all four backends within 5e-5 of stored PyTorch, and the
demo generating the reference continuations in Chrome. See
[validation](docs/VALIDATION.md) for versions and error bounds.

These run in CI rather than only locally: the `python` job builds the
Python-enabled artifact and then runs this suite against it with
`ZIPP_REQUIRE_INTEGRATION=1`, so a missing engine fails instead of skipping.

Still not established: **SoftN acceptance**, the Gate B robustness checks
(cancellation, lost GPU device, budget exhaustion, other browsers), language
quality for any model, and a recorded comparison of quantized Qwen3 logits
against llama.cpp. See the GGUF section below for what is and is not measured.

## What is included

An approved architecture is ordinary Python source, compiled as a project by
ZIPP. It emits a Graph v2 template containing **named weight references**. The
JavaScript host supplies binary tensor data from local Safetensors, then submits
the graph to ZIPP's existing compute runtime. Model weights never enter Python's
project filesystem, JSON, or Python lists.

```text
plugin.json + architecture.py [+ helper modules]
                    |
              ZIPP Python Engine
                    |
          graph + named asset bindings
                    |
local Files / bundled asset Map --> host Safetensors reader
                    |
          validated Graph v2 + Float32Array inputs
                    |
       existing ZIPP compute runtime and backend
                    |
             logits / generated tokens
```

The registry, manifests, binary reader, bindings, generation/session driver and
browser Worker demo are implemented. They are not alternative tensor kernels.
No package name, model config field, or Python string becomes browser JavaScript.

`plugins/bigram/architecture.py` is a genuinely **single-file** architecture and
tokenizer plugin. `plugins/tiny-causal/` demonstrates a multi-file package with a
configurable pre-norm causal transformer. Both use exactly the same host APIs.
`plugin.json` is required even for one file: it carries identity, version, entry
module, required capability, the checkpoint and tokenizer families the source
implements, and one hash per Python file.

A multi-file plugin imports its own modules by absolute name under the fixed
`zipp_plugin` package the host installs into (`from zipp_plugin.graph import
Graph`). ZIPP's Python frontend has no relative imports, and that namespace is
also what stops a plugin file shadowing the `json` module the bootstrap uses.

## Activate without rebuilding the engine

A host installs verified Python source at runtime. Each model session then starts
a fresh ZIPP Engine with that source. A new architecture does not require changing
Rust or rebuilding WASM **when its operations are already supported**.

This is **session-time source installation**, not adding imports to an already
compiled, running Python program. An existing session retains its source snapshot;
upgrades take effect when a new session is opened. The registry is currently
in-memory, not an IndexedDB package manager. Pure-Python helper modules can travel
with the architecture, but arbitrary pip packages, native extensions, new VM
opcodes and new GPU kernels cannot be installed this way.

## Try it in a full ZIPP checkout

Build the regular Python WASM variant with the repository's documented toolchain,
then run everything from this directory:

```sh
bash crates/zipp-wasm/build-variants.sh all
cd crates/zipp-wasm/model-plugins
ZIPP_REQUIRE_INTEGRATION=1 node --test
```

Expect 53 passing tests: 49 standalone plus four checkout gates, two of which
need a GPT-Neo checkpoint folder and skip without one.
Without `dist/all` the second gate skips instead, which is why required mode is
the one to run before believing anything. On PowerShell, set
`$env:ZIPP_REQUIRE_INTEGRATION='1'` first.
The Python differential tests additionally need NumPy and Safetensors:

```sh
python -m unittest discover -s tests -p 'test_*.py' -v
```

The lab runs on the playground's own loopback server, which already serves the
repository root and prints the lab's URL once `demo/` is present:

```sh
node crates/zipp-wasm/playground/serve.cjs
# zipp playground: http://127.0.0.1:8765/crates/zipp-wasm/playground/
# model lab:      http://127.0.0.1:8765/crates/zipp-wasm/model-plugins/demo/
# backend parity: http://127.0.0.1:8765/crates/zipp-wasm/model-plugins/demo/parity.html
```

That server is loopback-only, answers its own Host header, and serves no dot-files;
deployment is HTTPS and a separate decision. `demo/parity.html` runs the plugin's
recorded graphs on every backend the browser offers and compares complete logit
tensors with the stored PyTorch reference, so a backend that produces believable
text with wrong numbers is still caught. It never asks an explicit backend request
to settle for another one.

Choose the tiny fixture, download only a website plugin with your own local model,
or select both a plugin folder and a matching model folder.
The explicit approval button is the only route that starts a Worker or downloads
the bundled example. Local files are not uploaded. The demo reports the actual
backend; explicit GPU selection must fail rather than silently substitute another
backend. WASM compute kernels may require their separate existing GPU-lab build.
Use `dist/all`, **never the trusted-code `interop` variant**, for untrusted plugins.

The complete static tree must be included by the website build before deployment.
The current landing page/navigation/deployment pipeline is intentionally untouched.
No website changes have been published, and no service worker/offline installer is
included. Locally loaded weights do not, by themselves, make the website available
after an offline reload.

## Host API

Run this inside a host-owned, deadline-limited Worker. `Engine` and
`createRuntime` come from the existing ZIPP build, not this package.

```javascript
import {PluginRegistry, ModelSession, sourceFromFiles} from './src/index.mjs';

const registry = new PluginRegistry();
const plugin = await registry.install(sourceFromFiles(selectedPluginFiles), {
  approve: identity => hostApprovalFor(identity),
});
const runtime = await createRuntime({backend: 'auto'});
const model = await ModelSession.open({
  source: sourceFromFiles(selectedModelFiles),
  plugin,
  engineFactory: () => new Engine(),
  runtime,
});
try {
  const result = await model.generate('hello ', {
    maxNewTokens: 32,
    temperature: 0,
    onToken: ({text, backend}) => postMessage({text, backend}),
  });
} finally {
  model.dispose(); // after outstanding work settles
  runtime.dispose();
}
```

The complete runnable host is `demo/worker.mjs`. When passing browser-selected
files across a Worker, send a Map of explicit relative paths to File objects as
`demo/app.mjs` does; do not rely on `webkitRelativePath` surviving structured clone.

## Model support and present limits

| Implemented in this overlay | Not implemented or not verified |
| --- | --- |
| Versioned, source-hashed Python architecture plugins | Live mutation of an existing compiled VM; a general pip/plugin marketplace |
| Local Blob/File/typed-array sources and a scoped bundle-entry adapter | SoftN extraction, permissions/UI integration, production package migration |
| Safetensors F32/F16/BF16 with bounds checks and F32 conversion; GGUF through a pinned `gguf-wasm` release, with Q4_K and Q6_K weights kept resident as blocks | Pickle checkpoints, zero-copy device import, block formats other than Q4_K/Q6_K staying resident (the rest decode) |
| Custom tiny transformer, single-file bigram, GPT-Neo/TinyStories and Qwen3 plugins, each declaring the one checkpoint family it implements; Qwen3 also builds any inclusive range of its layers, so a model can be run in stages that hand each other a residual stream | Any other family — Llama, GPT-2 proper, SentencePiece tokenizers; loading one is refused, not attempted |
| Hugging Face folders read as published: own config, own tensor names, own tokenizer files, safetensors shard index | Pickled `pytorch_model.bin` (never unpickled) |
| Eager full-context planning as the oracle, plus cached decoding with carried KV caches and resident weights, quantized or not; a Qwen3 stage's decode graph over a chunk of positions, for prompts | Fused decode kernels, throughput claims beyond the measurements below |
| Existing Graph v2 runtime integration code; cross-backend parity verified on all four backends | ZIPP WASM/CLI integration, SoftN, Gate B robustness |

There is no new native `zipp py` model command or built-in `zipp_llm` Python
module in this overlay. The browser/embedding host owns model sessions.

## GPT-Neo: a real checkpoint, read where it lives

`plugins/gpt-neo/` reads GPT-Neo checkpoints — TinyStories among them — with the
GPT-2 byte-level BPE tokenizer those checkpoints were trained with. It is a
**separate** plugin, not a configuration of the fixtures above, and it is
verified against Hugging Face transformers rather than against a plausible
reading of its own output:

| Check | Result |
| --- | --- |
| Complete final-position logits, five prompts | max absolute error 5.6e-05 vs transformers, every argmax equal |
| Greedy continuations | identical token sequences |
| Tokenizer, 27 differential cases | encodes exactly as the reference, including emoji, CJK, Cyrillic, contractions, whitespace runs, NBSP and zero-width |
| In the browser | WebGPU, WASM SIMD and CPU JavaScript all produce the same text |

**It needs no conversion.** The plugin reads the checkpoint folder as the project
that published it laid it out: its own `config.json`, its own tensor names, and
its own `vocab.json`/`merges.txt`. A PyTorch `[out, in]` projection is read
through a transposing binding, the tied output projection is the embedding read
the other way round, and the BOOL causal-mask buffers GPT-Neo ships are indexed
and ignored rather than making the file unreadable.

```sh
# Only if the repo ships pickle rather than safetensors: ZIPP will not unpickle.
python tools/repack_safetensors.py --source ~/models/TinyStories-1M
```

Then choose **Website plugin + my Hugging Face checkpoint folder** in the lab, or:

```javascript
const session = await ModelSession.openNative({
  source, plugin, engineFactory, runtime,
  approve: identity => confirmWith(identity),   // digests of every file, before anything runs
});
```

A folder from someone else carries no pin for this host, so `openNative` hashes
the config, every shard and every tokenizer asset and hands them to the host to
approve — a deliberately weaker claim than a pinned model, behind a deliberately
separate entry point. `tools/pin_checkpoint.py` writes a `model.json` beside the
checkpoint when you want the stronger one; it changes nothing else in the folder.

What this cost, and what it did not: the fixture still declares
`zipp.tiny-causal-v1` and still refuses everything else. GPT-Neo is loadable
because a plugin was written and checked against it, not because the host learned
to be flexible.

## Cached decoding

A plugin can build a second graph for one token at a time. The host prepares it
once, so weights are uploaded once and key/value caches live on the device as
`carry` inputs; each token then costs one position instead of the whole context.
Generating 16 tokens from TinyStories-1M:

| | per token | uploaded per step |
| --- | --- | --- |
| Recomputing the context | 104 ms (CPU JavaScript) | 3,619,160 elements |
| Cached | 48 ms (CPU JavaScript) | 1,664 elements |
| Cached, WASM SIMD in a browser | 2.6 ms | 1,664 elements |
| Cached, WebGPU in a browser | 3.5 ms | 1,664 elements |

`generate` takes this path whenever the plugin offers one and the runtime can
prepare a plan, and falls back to recomputing otherwise. The eager path is
unchanged, is what `infer` uses, and the checkout gate requires both to produce
the same tokens: a cache that is subtly wrong still reads like English.

### A prompt in chunks

One token a step is right for generating and wasteful for a prompt: a step
reads every weight to process one token. Qwen3's `build_decode_stage` takes
`tokens`, and with more than one it builds the same stage over that many
consecutive positions -- hidden rows `[tokens, width]` in and out, a rotation,
a mask row and a cache-write column per token, and a `[1, tokens]` selector so
the last stage projects only its last real token onto the vocabulary.
`prepareDecode` reads the chunk size from the input shapes (`plan.tokens`), and
`stepInputs` takes `{position, tokens | hidden, count}` for up to that many
positions, padding the rest inertly: padded rows write nothing and no real row
can see them.

It writes the same caches, so a host prepares both graphs, runs the prompt in
chunks, then downloads the chunk session's carried caches and feeds them to the
one-token session on its first step. Layers 1..26 of Qwen3-0.6B on WASM:

| | per token |
| --- | --- |
| One token a step | 182 ms |
| Chunks of 8 | 72 ms |
| Chunks of 16 | 65 ms |

`tests/qwen3-chunks.test.mjs` compares it with the step-by-step path -- the
prompt's last logits, the tokens generated afterwards from the seeded caches,
head/middle/tail chunks against the whole, and different paddings -- and on the
fixture and on the real 0.6B every one of them is identical, bit for bit.

## Two ways to define a model

A plugin emits Graph v2 nodes the host validates, binds and submits; weights
never enter Python and the compute runs on any backend, including the GPU. That
is what everything above does.

A model can also just be *ordinary torch code*. `interop/` holds a
`transformers`-shaped shim — the names a modelling file imports, 21 small
modules — and with it `modeling_qwen3.py`, `modeling_qwen2.py`,
`modeling_llama.py` and `modeling_mistral.py` run **exactly as published**,
within 2e-07 of transformers. A new architecture becomes a file you copy.

The trade is real: that path holds tensors in the guest and runs on the engine's
CPU kernels, where the plugin path keeps weights outside Python and reaches the
GPU. See [architecture §4c](docs/ARCHITECTURE.md) and [interop/](interop/README.md).

## GGUF and quantized checkpoints

`src/gguf.mjs` reads GGUF files — including Q4_K, Q6_K and the rest of the ggml
block formats — through a WebAssembly build of [gguf-wasm](https://github.com/f2i-com/gguf-wasm),
which is its own project rather than part of this one: reading the container, the
block formats and the tokenizer is generic and useful without ZIPP. A pinned
release is fetched by `scripts/fetch_gguf_wasm.sh` and committed, the way
`gpu-lab/wasm/kernels.wasm` is; `wasm/gguf-wasm.lock.json` records the tag, the
revision and the digest of every file, and the tests check the tree against it.
The module is optional — `ggufSupport()` reports whether it is present, and a
checkout without it loads Safetensors models as before.

Neither side holds the file: a 6.8 GB Q4_K_M checkpoint opens in 35 ms from a
16 MiB header read, and five rows of its 788 MiB quantized embedding table come
back in 3 ms.

Quantization buys a smaller file, a smaller read, a vocabulary-sized table you can
touch a row at a time — and, since Graph v2 learned a quantized input, a smaller
tensor on the device. A weight whose format a backend can decode is bound as
blocks and decoded inside the matmul. Every backend implements that, and the two
Node can reach -- `cpu-js` and `wasm` -- are held to bit-for-bit agreement with
the same matmul over decoded values in the test suite; WebGL2 and WebGPU need a
browser, which is `gpu-lab/scripts/check-gpu-matmul.cjs` rather than a recorded
gate. For Qwen3-0.6B that is 373 MB
resident rather than 2,274 MB. Anything else still arrives dequantized. See
[interop/GGUF.md](interop/GGUF.md) for the protocol and the measurements.

A weight can also be bound for `matmul_fixed`, the integer-accumulation product
that reaches the same answer on every backend by construction rather than by
agreeing about float32 rounding order. The `fixed` and `fixed_scales` bindings
quantize the tensor to int16 once, here, instead of on every decode step: two
bindings over one tensor, which is read and quantized a single time. Both
`bindGraph` and `prepareDecode` take them, and both need the quantizer passed in
as `{quantize}` -- `quantizeWeight` from `gpu-lab` -- because this package does
not depend on the compute runtime and a second copy of that arithmetic is the
one thing not to have. It costs 3.56 times what the same weight costs as Q4_K
blocks, and at one token a step it is faster than the float32 matmul it
replaces. `gpu-lab/docs/FIXED-POINT.md` has the measurements.

The Qwen3 plugin asks for it by policy: `build_decode_stage` and
`build_prefill_stage` take `fixed` as `none` (the default, and what every stage
did before this existed), `layers` (the seven projections in each transformer
block) or `all` (those and the tied `token_embd.weight`). A stage's manifest
records which, because two peers running the same layers of the same checkpoint
compute different numbers if one of them is on the integer path, and a checker
has to compare peers answering the same question. Against Qwen3-0.6B with every
projection on that path, the worst logit lands 1.19e-03 of the largest logit
from the float32 answer, the argmax does not move, and two runtimes running it
agree bit for bit.

## Next model milestone

Each further checkpoint family gets a **separate** plugin, carrying that family's
own tokenizer and an explicit state-dict mapping verified against the reference
implementation. It is not a configuration of the plugins here, and not a flag on
them. `plugins/qwen3/` is the most recent one and reads its configuration,
tokenizer and weights from the GGUF file itself.

A plugin is **not** advertised as compatible with a checkpoint family merely
because it uses transformer operations. Attention, LayerNorm and GELU say
nothing about tensor names, weight orientation, tied embeddings, attention
scaling, local/global attention patterns, normalization epsilon, activation
variant or tokenization — the things a checkpoint actually depends on. So the
claim is made in data rather than prose: `plugins/tiny-causal/plugin.json`
declares `"checkpoint_format": "zipp.tiny-causal-v1"` and `"tokenizer_formats":
["character-v1"]`, every `model.json` names the family it belongs to, and
`ModelSession.open` refuses a mismatch with a `CHECKPOINT` or `TOKENIZER` error
before it constructs an engine. Renaming a checkpoint's `checkpoint_format` does
not convert it; it only moves the failure to a plugin that will read the wrong
bytes. `docs/HANDOFF.md` Gate C lists what such a plugin must prove before the
name `GPT-Neo` appears anywhere in a release.

The generic session driver currently serves causal language models only. A plugin
can replace architecture and tokenizer logic but must return that driver ABI.
Adding general library-only plugins or image/audio task drivers is a separate
extension, not something this patch silently claims to provide.

F16/BF16 checkpoints are expanded to Float32Array; this is not half-precision GPU
execution. Q4_K and Q6_K are the exception and are held as the file's own blocks,
decoded inside the matmul — which is a smaller upload and a smaller residency, not
a different arithmetic: the result is bit-for-bit what decoding first would give.

Weights stay on the device across decode steps and the graph carries its KV cache,
so a step uploads a token rather than a context. Everything else about a step is
still a full graph bound and submitted, and there is no claim of zero-copy weights:
the bytes are read, uploaded once, and kept.

See [architecture and ABI](docs/ARCHITECTURE.md), [SoftN integration handoff](docs/SOFTN.md),
[release gates](docs/HANDOFF.md), and [validation](docs/VALIDATION.md).
