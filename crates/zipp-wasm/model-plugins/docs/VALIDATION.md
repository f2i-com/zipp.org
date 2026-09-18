# Validation record

Two records, kept apart on purpose. The first is what this overlay's original
download claimed; the second is what actually ran in a full ZIPP checkout at
commit `62da28d9` with the improvements in this tree. Neither is a published
upstream ZIPP result. Browser and GPU checks appear below where they ran; SoftN
acceptance and the remaining Gate B robustness checks did not run at all.

## Run in this checkout — 17 September 2026

| Check | Result | Scope |
| --- | --- | --- |
| Node host and session tests | 89 passed, 0 failed | Loader, manifests, checkpoint/tokenizer refusal, binary bindings, bounds, hashing, lifecycle; session lifecycle uses explicit doubles. The whole suite, of which the GGUF and Qwen3 rows below are a part. Without `ZIPP_GGUF_MODEL` and `ZIPP_QWEN3_MODEL` on disk it is 71 tests with 3 suites skipped, since no checkpoint is redistributed here |
| Gate A1: upstream Graph v2 validator + CPU backend | passed, max absolute logit error 0.0000083446502685546875 | Five complete logit tensors from the plugin's own graphs, executed by `gpu-lab/src/runtime.mjs` on `cpu-js`, compared with stored PyTorch CPU outputs |
| Gate A2: Python-enabled ZIPP WASM engine | passed | Real `Engine`, real `initPythonProject`/`pythonCall` bootstrap, five prompts: logits within tolerance and greedy token sequences identical to the reference |
| Gate C1: a Hugging Face checkpoint folder, unconverted | passed | TinyStories-1M read as published through `openNative`: its own config.json, `model.safetensors` (including 8 BOOL mask buffers nothing binds), and its own vocab.json/merges.txt; logits within 2e-4 of transformers and greedy tokens identical |
| Gate C2: the same folder from a written pin | passed | `tools/pin_checkpoint.py` manifest, loaded through the ordinary pinned path |
| Cached decoding vs the full-context path | identical tokens, logits within 2e-4 of transformers | Carried key/value caches across steps, checked against both the eager graph and transformers, in NumPy and on the engine |
| Cached decoding, measured | 104 ms/token to 48 ms/token on CPU JavaScript; 2.6 ms/token on WASM SIMD and 3.5 ms/token on WebGPU in a browser | Uploads per step fall from 3,619,160 elements to 1,664; 18.6 MB stays resident |
| Recurrent primitives on the runtime | 2 passed | A causal depthwise convolution over a carried window and a gated delta-rule state update, each built from existing operations and checked against an independent implementation |
| CPython/NumPy plugin tests | 30 passed | 12 for the bundled fixtures, 15 for GPT-Neo: config reading, logit parity, greedy parity, checkpoint-order tensors, local-attention windows, tanh GELU, unscaled attention, family refusals, and 27 differential tokenizer cases against transformers |
| GPT-Neo logit parity | max absolute error 0.0000562667 | Complete final-position logits for five prompts, NumPy evaluation of the plugin's graphs vs transformers on the same checkpoint |
| GPT-Neo tokenizer | 27/27 exact | Emoji, CJK, Cyrillic, contractions, whitespace runs, NBSP, zero-width, URLs, backslashes; encode equality and decode round-trip against the reference tokenizer |
| GPT-Neo in the browser | passed on WebGPU, WASM SIMD and CPU JavaScript | The lab loaded the checkpoint folder, showed the digest of every file for approval, and generated the same text on each backend | Plugin math, tokenizer, causal masking, configuration, declared checkpoint family, alternative architecture |
| Logit parity (independent evaluator) | max absolute error 0.0000069141387939453125 | NumPy evaluation of the plugin's graphs vs stored PyTorch CPU outputs |
| Windows checkout integrity | passed | `.gitattributes` keeps this tree LF and the checkpoint exact, so the hashed fixtures survive a `core.autocrlf=true` checkout |
| Browser demo, end to end | passed | Chrome 153.0.8010.47 over `playground/serve.cjs`: bundled fixture, bundled bigram, and a local model folder each downloaded/approved, compiled Python in a Worker and generated the reference continuations (`hello ` → `alice!`, `zipp runs ` → `a tiny model.`, bigram → `abc.`) |
| Browser backend parity (`demo/parity.html`) | passed on 4 of 4 backends | Complete logit tensors for five prompts vs stored PyTorch: WebGPU 7.153e-6, WebGL2 7.153e-6, WASM SIMD 8.345e-6, CPU JavaScript 8.345e-6, tolerance 5e-5 |
| Browser checkpoint refusal | passed | A local folder whose `model.json` claims `hf.gpt-neo-v1` with a `gpt2-byte-bpe-v1` tokenizer is refused in the UI with the `CHECKPOINT` message, no engine constructed |
| Remaining Gate B checks | NOT RUN | Cancel during compile/read/compute, lost GPU device, malformed source, budget and context exhaustion, repeated load/teardown, peak memory, other browsers and devices |
| SoftN app packaging/runtime | NOT RUN | Boundary adapter only, no SoftN source changes |
| Quantized matmul on the backends Node can reach (`gpu-lab/tests/quantized.test.mjs`) | 7 passed | `cpu-js` and `wasm` hold a Q4_K/Q6_K matmul bit-for-bit equal to the same matmul over decoded values, and hold the decoder against `f2i-gguf-quants` rather than against a second copy of itself |
| Quantized matmul on WebGL2 and WebGPU | NOT RUN in this record | Needs a GPU and a browser: `gpu-lab/scripts/check-gpu-matmul.cjs`. The shader decoders are therefore verified by construction and by the browser demo generating sensible text, not by a recorded bit-for-bit result |
| GGUF reader (`tests/gguf.test.mjs`) | 9 passed | Header parsing, metadata, tensor ranges in this package's shape order, row gathering without decoding the table, budget refusals, malformed-shape refusal |
| GGUF module detection and provenance (`tests/gguf-module.test.mjs`) | 7 passed | Absent module reported rather than thrown, wrong module refused by shape, and the committed tree checked against every digest `scripts/fetch_gguf_wasm.sh` recorded — including that `rust/Cargo.toml` pins the revision the lock names. Which release that is, `wasm/gguf-wasm.lock.json` says; naming it here as well would be one more copy of a version number to go stale, which is the thing the lock exists to stop |
| GGUF tokenizer (`tests/tokenizer.test.mjs`) | 6 passed | Built inside the module from the file's own vocabulary and `tokenizer.ggml.pre`; qwen2 digit-splitting checked against the vocabulary's own rule, and an unimplemented pre-tokenizer name refused rather than guessed |
| Qwen3-0.6B Q4_K_M end to end (`tests/qwen3.test.mjs`) | 6 passed | Configuration read from the file; prompt tokenized to the ids the vocabulary defines; **373 MB resident, 372 MB of it still blocks**; a prefill of "The capital of France is" predicting ` Paris`; and a cached decode agreeing with the prefill before generating |
| Qwen3 architecture vs transformers (`tests/transformers-shim.test.mjs`) | passed, max absolute error 7.450580596923828e-8 | Hugging Face's own `modeling_qwen3.py` compiled unmodified inside ZIPP and compared with what transformers produced for the same weights. This validates the architecture, **not** the quantized path: it runs staged F32 weights, not GGUF blocks |
| Block decoding vs an independent implementation | 15 of 15 formats, bit for bit | The decoders ZIPP reads quantized weights with are checked against [gguf-py](https://github.com/ggml-org/llama.cpp/tree/master/gguf-py), the llama.cpp project's own numpy GGUF library, in `gguf-quants/tests/golden/`. This is the piece that was missing: the quantized-vs-decoded tests held both sides to the *same* decoder, so a decoder wrong in one place would have agreed with itself. The blocks are synthetic — pseudo-random bytes with finite f16 scales, which is a valid block of any format and harder than real weights, since a checkpoint's quants cluster where an off-by-one in a shift does not show. No model weights are vendored anywhere for this |
| Qwen3 quantized logits vs an external reference, end to end (`tests/qwen3-oracle.test.mjs`) | passed, worst difference 7.391e-5 over 455,808 logits | The chain, not its links. transformers reads the same GGUF with its own container reader, decodes the blocks with gguf-py and runs its own Qwen3 in PyTorch; ZIPP reads it, keeps the blocks packed and runs the plugin's Graph v2 on its own kernels. Complete vectors for three prompts — 151,936 logits each, not a top-k, because a wrong rotation moves the tail long before it moves the argmax. The checkpoint's sha256 is recorded with the fixtures, so reference logits cannot drift onto a different file. Held to 5e-4, which is roughly seven times the measured difference; equality is not available here since the two accumulate float32 in different orders over 28 layers |
| These checks as a release gate | passed in CI, all four jobs | The `python` job runs the suite with `ZIPP_REQUIRE_INTEGRATION=1` against the `dist/all` artifact it has just built, so the plugin ABI, the engine integration and the GGUF lock are mandatory before a release rather than run where someone remembers to. Dispatched at `7b15b96c`, [run 35289293326](https://github.com/f2i-com/zipp.org/actions/runs/35289293326) |
| `gpu-lab/wasm/kernels.wasm` reproduces | passed | Rebuilt from a different directory with a different `CARGO_HOME`: identical bytes. The build remaps the cargo path out, so the committed binary no longer records who built it and can be checked by rebuilding it |
| Qwen3-0.6B in the browser | RAN, NOT GATED | Loaded from a local GGUF file and generated text in Chrome on WebGL2. Observed interactively during development; nothing here establishes language quality |

The browser rows were driven through Playwright against the system Chrome on
Windows 11, over the playground's own loopback server. They are a real browser on
one machine, not a device matrix, and `demo/parity.html` is a page a person opens
rather than an automated gate: no browser runner is checked in, so nothing reruns
these on their own. Explicit backend selection was verified to fail rather than
substitute another backend, which is why each row names the backend it measured.

Environment: Node v24.19.0, Python 3.11.6, NumPy 2.4.3, Safetensors 0.5.3,
rustc 1.92.0, wasm-bindgen 0.2.126. The engine under test was built here with
`bash build-variants.sh all` (8,168,754 bytes of WASM), not downloaded. Gate A ran
with `ZIPP_REQUIRE_INTEGRATION=1`, where a missing artifact fails instead of
skipping. PyTorch 2.11.0+cu128 is installed but was **not** used: the checkpoint,
its oracle and the training report are the originals, unchanged and unretrained,
so the stored PyTorch 2.10.0+cpu reference still describes them.

Gate A2 is the evidence for a change this tree had to make: the delivered
multi-file plugin used relative imports (`from .graph import Graph`), which ZIPP's
Python frontend refuses outright (`crates/zipp-vm/src/frontend/python/stmts.rs`
answers `relative imports are not supported`). Confirmed directly against the
built engine, then fixed by importing siblings absolutely under the installed
`zipp_plugin` namespace. The graph fixtures regenerated byte-identically
afterwards, so the architecture itself did not move.

## Supplied with the original download — 17 September 2026

Raw logs are in the download bundle's `validation/` directory.

| Check | Result | Scope |
| --- | --- | --- |
| Node host and session tests | 40 passed, 0 failed | Loader, manifests, binary bindings, bounds, hashing, lifecycle |
| Full-checkout integration tests | 2 skipped | Upstream Graph v2/runtime and ZIPP WASM unavailable in that container |
| CPython/NumPy plugin tests | 10 passed | Plugin math, tokenizer, causal masking, configuration |
| Logit parity | max absolute error 0.0000069141387939453125 | Independent NumPy evaluation vs stored PyTorch CPU outputs |
| Greedy token parity | five prompts matched | CPython/NumPy evaluation vs stored PyTorch reference |
| Offline browser UI harness | 8 checks passed | HTML/CSS, inlined main-page modules, consent dismissal, folder controls, responsive layout; NOT inference |
| Actual ZIPP/WASM/GPU inference | NOT RUN | No speed/compatibility claims made |

That environment was Node 22.16.0, Python 3.13.5, PyTorch 2.10.0+cpu, Chromium
144.0.7559.96. Repository archives could not be downloaded into the container and
localhost navigation was blocked by policy, so the UI checks used an offline HTML
harness with source modules inlined. That is not equivalent to validating
deployment, Worker imports or GPU execution, and it has not been repeated here.

## About the GPT-Neo checkpoint

TinyStories-1M: GPT-Neo, 8 layers alternating global and 256-wide local
attention, 16 heads, hidden size 64, vocabulary 50,257, tied embeddings, about
3M parameters. It is the largest real pretrained checkpoint that fits the graph
protocol's default limits — its embedding is 3,216,448 elements against a
4,194,304 element ceiling — and it is **not** in this repository. It was
downloaded from Hugging Face, repacked out of pickle into safetensors with names,
shapes and dtypes unchanged, and read from there.

Reading it required raising two policy limits, both documented where they are
set: the graph node budget (eight GPT-Neo layers emit 529 nodes against a default
of 512) and the per-call instruction budget (a 50,000-entry tokenizer costs far
more than the engine default). The compute runtime keeps its own budgets and the
host passes matching ones; neither limit is a protocol ceiling.

What this does not establish: one checkpoint, one family, one machine. Speed was
not measured beyond noting that every generated token re-runs the whole model
over the whole context, because there is no KV cache and no resident weights.

## About the fixture

The custom toy checkpoint has 12,094 parameters, is 50,240 bytes as Safetensors,
and was trained on 16 original tiny sentences for 1,000 steps with seed 20260917.
Its recorded training loss is 0.11971975862979889. This is not a held-out benchmark.
For example the stored PyTorch reference continues `hello ` with `alice!` and
`zipp runs ` with `a tiny model.` These demonstrate serialization/architecture
agreement; they do not show useful general language ability. It is a
`zipp.tiny-causal-v1` checkpoint and nothing else: the host refuses to load it as
any other family, and refuses other families' checkpoints into this plugin.

`tools/train_demo.py` includes the full corpus, model, training, export and oracle
creation. Re-running it needs PyTorch, NumPy and Safetensors, and will produce a
different checkpoint on a different library version — the oracle and graph
fixtures must be regenerated together with it. `tools/emit_graph_fixtures.py`
regenerates graph plans from the actual architecture source.
`tools/build_manifests.py` regenerates source hashes, the catalogue and the
example models' identity pins; it reads each plugin's declared checkpoint family
from its source, so a manifest cannot claim one the code does not implement. The
bigram model is a hand-authored 7-by-7 transition table, not a trained transformer.

Before merging, complete the browser and SoftN acceptance checks in `HANDOFF.md`.
Do not relabel skipped tests as passes, and do not read a passing Gate A as
evidence for Gates B through E.
