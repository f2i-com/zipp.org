# Validation record — 17 September 2026

This evidence belongs to this source overlay, not a published upstream ZIPP build.
Full raw logs are in the download bundle's `validation/` directory.

| Check | Result | Scope |
| --- | --- | --- |
| Node host and session tests | 40 passed, 0 failed | Loader, manifests, binary bindings, bounds, hashing, lifecycle; session lifecycle uses explicit doubles |
| Full-checkout integration tests | 2 skipped | Actual upstream Graph v2/runtime and ZIPP WASM unavailable locally |
| CPython/NumPy plugin tests | 10 passed | Actual Python plugin math, tokenizer, causal masking, config, alternative architecture |
| Logit parity | max absolute error 0.0000069141387939453125 | Five complete logit tensors: independent NumPy evaluation vs stored PyTorch CPU outputs |
| Greedy token parity | five prompts matched | CPython/NumPy evaluation vs stored PyTorch reference |
| Offline browser UI harness | 8 checks passed | HTML/CSS, inlined main-page modules, consent dismissal, folder controls, responsive layout; NOT inference |
| Actual ZIPP/WASM/GPU inference | NOT RUN | No speed/compatibility claims made |
| SoftN app packaging/runtime | NOT RUN | Boundary adapter only, no SoftN source changes |

The test environment was Node 22.16.0, Python 3.13.5, PyTorch 2.10.0+cpu. The UI
harness used Chromium 144.0.7559.96. Repository archives could not be downloaded
into the container; GitHub source was inspected through the connected read tools.
Localhost browser navigation was blocked by environment policy, so UI checks used
an offline HTML harness with source modules inlined. That is not equivalent to
validating deployment, Worker imports or GPU execution.

The custom toy checkpoint has 12,094 parameters, is 50,240 bytes as Safetensors,
and was trained on 16 original tiny sentences for 1,000 steps with seed 20260917.
Its recorded training loss is 0.11971975862979889. This is not a held-out benchmark.
For example the stored PyTorch reference continues `hello ` with `alice!` and
`zipp runs ` with `a tiny model.` These demonstrate serialization/architecture
agreement; they do not show useful general language ability.

`tools/train_demo.py` includes the full corpus, model, training, export and oracle
creation. Re-running it needs PyTorch, NumPy and Safetensors. Exact training results
may differ with library/platform versions. `tools/emit_graph_fixtures.py` regenerates
graph plans from the actual architecture source. `tools/build_manifests.py`
regenerates source hashes/catalogue; model plugin pins must be updated deliberately
when source changes. The bigram model is a hand-authored 7-by-7 transition table,
not a trained transformer.

Before merging, run the required integration mode and complete the browser/SoftN
acceptance checks in `HANDOFF.md`. Do not relabel skipped tests as passes.
