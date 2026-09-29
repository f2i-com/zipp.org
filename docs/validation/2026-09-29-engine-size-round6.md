# Rejected regex parser/table size experiments - 2026-09-29

Continues `codex/engine-size-reduction` from `49a88aad`. **No production engine
change is selected in this pass.** Smaller candidates passed correctness checks
but failed the performance screen. Their source changes were reverted, and
the shipping artifacts retain the previous bytes. Benchmark improvements and
a Unicode string-property differential probe are retained for future work.

## Final rejected candidate

The candidate's Unicode string-property parser creates a character node directly for a
one-code-point string, avoiding a temporary concatenation vector and the
optimizer work needed to remove it. Both the bare-property and class-set paths
share an outlined constructor. The class-set path also collects into an
exact-sized vector rather than growing one with repeated pushes.

Its cold error helper accepts `Into<String>`. All callers supply
string literals or owned formatted strings; the latter move into the error
instead of being cloned. Outlining this path reduces full-build code size.
Error messages remain unchanged.

The general benchmark accepts an optional case name for isolated diagnosis.
The new Unicode string-property benchmark checks Node results and sandbox
rejections before timing distinct-pattern compilation with fresh engines.
The full corpus remains the acceptance screen: isolated cases cannot excuse
regressions caused by preceding workloads in the same module instance.

## Candidate size (not shipped)

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125 `-O1`, Node 24.19.0,
Windows x64. Sizes exclude JavaScript glue; Brotli uses quality 11.

| Module | Shipping raw | Rejected raw | Shipping Brotli | Rejected Brotli |
| --- | ---: | ---: | ---: | ---: |
| Lite | 2,916,044 | 2,916,028 | 772,481 | 771,399 |
| JavaScript | 5,326,416 | 5,321,426 | 1,313,817 | 1,312,596 |
| Python-base | 7,001,211 | 6,996,221 | 1,685,440 | 1,682,713 |
| Python + Torch | 8,956,302 | 8,951,312 | 2,030,875 | 2,028,924 |

**The QuickJS size goal is not met.** Shipping Lite remains 1,777,655 raw bytes larger than
the 1,138,389-byte stripped QuickJS-NG v0.16.2 reference. The official reactor
is 1,528,293 bytes, including 389,904 custom/debug bytes. The reference and its
verification are documented in the [fourth audit](2026-09-29-engine-size-round4.md).
This is a size comparison, not an assertion of equivalent host APIs or sandbox
contracts.

## Performance

Candidate measurements are recorded in the accompanying evidence JSON. Ratios are
candidate / previous elapsed time; reverse-order measurements are inverted.
Benchmarks use Node's optimizing WASM compiler (`--no-liftoff`), 10 warmup
pairs and 31 alternating sample pairs, with shared calibrated iteration counts.
No build, compression or correctness-test job runs alongside timing.

| General JS corpus | Forward | Reverse |
| --- | ---: | ---: |
| Lite, affinity 4 | 1.006 | 1.045 |
| JavaScript, affinity 4 | 1.003 | 0.997 |
| Python-base, affinity 4 | 1.012 | 1.002 |
| Python + Torch, affinity 4 | 1.013 | 0.999 |

These aggregate results conceal regressions in individual rows. Python-base
Promise resolution is 1.079 / 1.103 on affinity mask 4 and 1.105 / 1.054 on
mask 16. Python + Torch is 1.089 / 1.066 on mask 4 and 1.112 / 1.067 on
mask 16. Lite's second-core check
shows string construction at 1.052 / 1.043 and property deletion at
1.045 / 1.079. This is sufficient reason to reject the candidate, even though
some rows vary by load order and most aggregate differences are small.

The candidate does improve Unicode-property compilation: Lite Basic Emoji is
0.850 / 0.856 and combined RGI Emoji is 0.930 / 0.911. Full JavaScript has
similar improvements (0.903 / 0.895 and 0.931 / 0.935 respectively), but flag
sequence compilation is slower at 1.076 / 1.081 in the first pair and
1.072 / 1.042 on the second core. A speedup
in these cases does not compensate for the regressions elsewhere.

Affinity masks 4 and 16 select individual logical processors on the same
AMD Ryzen 9 9950X3D host. Measurements are a regression screen for these inputs
and this host, not a universal speed guarantee. Local filenames containing
`selected` identify the proposed final candidate; it was subsequently rejected.

## Correctness

* All regex unit, integration and doc tests pass in compact bounded, compact
  unbounded, full bounded and default configurations; one doc test is ignored.
  The unbounded `no_std` compile check passes.
* The safe-sandbox VM with Python passes 528 tests, with four ignored.
* The rejected Lite, JavaScript and Python + Torch bytes pass all 29 boundary
  suites. Python-base passes 27/29, with the same two Torch-dependent failures
  as its baseline (`audit-2026-09-15-ml-transport.cjs`, `python-gpu.cjs`).
  Its 57 Python frontend checks pass. Full Python training agrees with the
  stored PyTorch reference outputs.
* The existing regex differential probe matches the previous full engine over
  622,080 cases, including captures, budgets and exhaustion. Output SHA-256:
  `531579c164456f49eb692da98dbb8999a8f8bffc1dccb48fd16852a3f1da5e9e`.
* `tools/regex_string_properties_probe.rs` adds 27,972 matching cases and 84
  compiled-residency records across all seven string properties, four pattern
  shapes, Unicode and ignore-case modes, optimizer settings, start positions,
  lone surrogates and step allowances. Previous and candidate outputs match:
  `6d6d9d556c48b7df2a3046e0510d65efb829b2646d33c37a80f7319dba708ca6`.
  Unoptimized ignore-case mode is excluded because the baseline omits required
  case folding and panics in that configuration. Production uses optimization.
* Browser and tested Node WASM bytes match. Import counts remain 51 for Lite/JS
  and 52 for Python variants; the memory maximum remains 1 GiB.
* Full test262, native throughput and cold-browser performance were not run.
  CI remains skipped at the user's request.

## Earlier rejected experiments

Packing the seven Unicode string-property tables into shared code points and
fixed-width runs saved roughly 50 KB raw. Coallocating alternation children
also reduced allocation overhead. Several versions passed correctness checks
and improved large-property compilation, but repeatedly slowed retained-state
Promise workloads in Python variants. Those production changes were reverted.
The original tables and alternation tree allocations are retained.

Fresh-process Promise runs did not reproduce the slowdown. That does not
invalidate the full-corpus result: the module instance retains state after
earlier engines are disposed. An identical-binary control did not explain the
observed 10-18% regression, and CPU profiling did not establish its cause.
The evidence retains these experiments, controls and raw timing samples.

The first Unicode compilation harness reused one engine across timed batches,
allowing garbage to accumulate and timings to drift. Final measurements use
a fresh engine per batch, with setup and disposal outside the timer. This
protocol distinction matters when comparing intermediate experiments.

## Reproduction

The parser candidate patch is retained beside this report for reproduction;
it is not applied to production source. Apply it with `git apply --unidiff-zero`
to `49a88aad` in an isolated checkout. Build each variant through
`crates/zipp-wasm/build-variants.sh`.
Generate Node bindings from the same Rust artifact and replace their WASM with
the post-processed web module before correctness checks and timing. Compare
against Lite from the fourth pass and full variants from the third pass.

```
node --no-liftoff crates/zipp-wasm/tests/node/bench-size-pass.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
node --no-liftoff crates/zipp-wasm/tests/node/bench-unicode-string-properties.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
node --no-liftoff crates/zipp-wasm/tests/node/bench-regex-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
node tools/measure_wasm_size.mjs MODULE.wasm
```

Repeat with package arguments reversed. An optional case name after the two
paths in `bench-size-pass.cjs` isolates that case for diagnosis. Compile both
differential probes against separate previous/current `regress` libraries as
described in the fourth audit. Compare output byte-for-byte.

Detailed sizes, hashes, validation results and raw samples are in
[the evidence JSON](2026-09-29-engine-size-round6-evidence.json).
