# Engine size reduction, fourth pass - 2026-09-29

Continues `codex/engine-size-reduction` from `6b854cdc`. The runtime changes
in this pass are enabled only by Lite. Full JavaScript, Python and native
profiles retain their existing regex input types and scalar metering paths.
No additional JavaScript features or safety limits are removed.

## Size and comparison

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125 `-O1`, Node 24.19.0,
Windows x64. Module bytes exclude JavaScript glue; Brotli uses quality 11.

| Module | Raw bytes | Brotli bytes |
| --- | ---: | ---: |
| Previous Lite | 2,942,452 | 774,683 |
| Selected Lite | 2,916,044 | 772,481 |
| Reduction | 26,408 | 2,202 |
| QuickJS-NG v0.16.2 official reactor | 1,528,293 | 417,087 |
| Same QuickJS reactor without custom sections | 1,138,389 | 304,391 |

**The QuickJS size goal is not met.** The selected Lite module is still
1,777,655 raw bytes larger than the stripped reference. The official reference
contains 389,904 bytes of custom sections, including debug information; those
sections must not be counted as necessary engine code when comparing against
stripped ZIPP. This is a size reference, not a claim that the modules expose the
same host API or resource-isolation contract.

The reference was downloaded again from the official
[QuickJS-NG v0.16.2 release](https://github.com/quickjs-ng/quickjs/releases/tag/v0.16.2)
and verified against the repository's previously recorded SHA-256:
`fc638ef0bad35edb860ca93fe5c0ea288a6ad137888b34afa8ca2c2513727cf0`.
Removing only custom sections produces SHA-256
`7ce9564a6efd675e45316c2fc4f939c0fa5f83140caf6744e3064764a260acb4`.
Both modules validate, and their import/export descriptors match.

Selected Lite SHA-256:
`fe4b7635c481e8f2fa97420fc1c903fa4376de12646ebd2e2e6ae7b0c009a3e5`.
A final source rebuild produced identical bytes. The browser module and the
Node module used by tests and benchmarks are byte-identical. Full-variant
artifacts retain the [third pass](2026-09-29-engine-size-round3.md) results.

## Changes

* `regress/compact-utf16`, enabled by `wasm-lite`, shares the UCS-2 and UTF-16
  backtracker. ASCII keeps its specialized executor. A surrogate mask selects
  code-unit versus code-point iteration independently of Unicode case folding.
  Subinputs and captured backreferences preserve the original input mode.
* Single-character loops share an outlined scanner. Before reading input, it
  bounds the attempted matches by the available allowance. It charges exactly
  the attempted matches, including a mismatch, and reports exhaustion at the
  same boundary as the former scalar loop. These matchers cannot allocate
  backtracking state or recursively consume the budget. Zero/one mandatory
  characters and zero optional iterations retain direct fast paths.
* Lite derives consumed steps from the initial allowance minus the remainder,
  eliminating a redundant per-operation counter update. Successful consumption
  cannot exceed the remainder, so subtraction preserves the former usage count
  without overflow. A failed reservation or consumption does not spend steps.

## Performance

Ratios below are selected / previous elapsed time; lower is faster. The general
screen retains the earlier 12-workload aggregate plus separate focused rows.

| General JS corpus | Forward order | Reverse order |
| --- | ---: | ---: |
| Final build, affinity mask 4 | 0.996 | 1.015 |
| Confirmation, affinity mask 16 | 0.995 | 0.968 |

Overall throughput is approximately flat across these screens; this is not a
claim of zero slowdown on every row. The first reverse run had a 13.9% integer
arithmetic outlier that did not repeat on the second core. Memoized `call-deep`
was 6-7% slower in the first pair but did not repeat in confirmation; actual
recursive calls were 0.940 / 1.010 and 1.009 / 0.993. String append/index was
1.036 / 1.018 in the first pair, then 0.990 / 0.983 on confirmation. Regex
`matchAll` was 1.017 / 1.009 on confirmation, a small slower row retained in the
evidence. No large slowdown repeats across both cores and load orders.

Final focused matching ratios (affinity 4, forward / reverse) include:

| Case | Forward | Reverse |
| --- | ---: | ---: |
| ASCII character class | 0.685 | 0.656 |
| UCS-2 BMP character class | 0.854 | 0.847 |
| Unicode BMP character class | 0.862 | 0.863 |
| Unicode property | 0.878 | 0.892 |
| UCS-2 alternation | 0.977 | 1.029 |
| UCS-2 empty match | 1.005 | 0.988 |

Character-class improvements repeat in both orders. Short matches and branching
patterns show smaller, mixed differences; no focused case has a greater than
1% slowdown in both final load orders. This is a measured result for these
inputs and this host, not a universal performance guarantee.

The focused regex harness now covers 25 cases: short/empty matches, zero bounds,
lazy loops, ASCII and non-ASCII classes, surrogate units, astral characters,
lookbehind, backreferences, Unicode case folding/properties, and alternation.
It checks match/capture offsets against Node and checks every timed checksum.
Both engines receive the same match count, calibrated to at least 5 ms where
possible (300 minimum, 6,000 maximum). It then takes 10 warmup pairs and 31
alternating paired samples with Node `--no-liftoff`.

Final benchmark processes use processor affinity mask 4 on an AMD Ryzen 9
9950X3D, with the general confirmation pair using mask 16. Build, compression and correctness-test jobs do not run concurrently
with timing. Both package load orders are retained. This is measured coverage,
not proof of identical performance for every workload or browser.

## Correctness and limits

* `tools/regex_compact_probe.rs` compares full and compact regex builds over
  622,080 cases: 18 patterns, four flag combinations, optimizer enabled/disabled,
  ten texts, allowances 0 through 79, multiple starting positions and both
  input APIs. Match ranges, captures, step usage and exhaustion are identical.
  Both output files have SHA-256
  `531579c164456f49eb692da98dbb8999a8f8bffc1dccb48fd16852a3f1da5e9e`.
* Unit tests cover every individual UTF-16 code unit in both modes, boundary
  triples, forward/backward cursor stepping, subinputs and a lone-surrogate
  backreference. Scalar-reference tests exercise loop bounds, prior exhaustion,
  pre-consumed steps and both directions. Usage is also checked at `u64::MAX`.
* All applicable regex unit/integration/doc tests pass in compact bounded,
  compact unbounded, full bounded and default configurations. Compact bounded
  has 40 library unit tests; full/default retain 35. One doc test is ignored.
* The safe-sandbox VM with Python passes 528 tests, with four ignored.
* The exact selected Lite bytes pass all 29 production boundary suites,
  including resource limits and the Lite feature contract. Import auditing
  confirms 51 host functions; the linear-memory maximum remains 1 GiB.
* An extra unbounded `no_std` compile check passes. An optional `no_std` plus
  bounded unit-test attempt fails on existing missing imports in `exec.rs`
  (`Vec`) and `unicode.rs`'s test module (`std`). Those files are unchanged by
  this pass; this configuration is not used by ZIPP WASM.
* Full test262, cold-browser benchmarks and native throughput benchmarks were
  not run. CI remains skipped at the user's request.

## Rejected intermediate candidates

Sharing only the input type saved about 30 KB but repeatedly slowed two
non-Unicode matching cases by 1-2%. Moving budget updates out of character loops
helped, and outlining the scanner recovered both size and speed. The smallest
outlined candidate saved about 34 KB, but short-match probes then exposed
avoidable call overhead. The selected fast paths trade some of that size
saving for short-match speed. Removing the redundant usage counter addresses
branching patterns as well. Raw samples for these intermediate candidates are
retained in the evidence; only the final artifact is selected.

## Reproduction

Build Lite using the normal `crates/zipp-wasm/build-variants.sh lite` pipeline
with the tool versions above. Generate Node and web bindings from the same Rust
artifact, run the standard metadata stripping/optimizer selection, and copy the
selected web module over the Node package's `zipp_wasm_bg.wasm` before testing.

```
node --no-liftoff crates/zipp-wasm/tests/node/bench-size-pass.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
node --no-liftoff crates/zipp-wasm/tests/node/bench-regex-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
node tools/measure_wasm_size.mjs MODULE.wasm
cargo +1.92.0 test -p zipp-regress --locked --features compact-utf16,prohibit-unsafe,bounded-backtracking
cargo +1.92.0 test -p zipp-regress --locked --features compact-utf16,prohibit-unsafe
cargo +1.92.0 test -p zipp-regress --locked --features utf16,prohibit-unsafe,bounded-backtracking
cargo +1.92.0 test -p zipp-regress --locked
```

Repeat each benchmark with package arguments reversed. The differential probe
is a standalone Rust program: build `zipp-regress` into separate target
directories with `utf16,prohibit-unsafe,bounded-backtracking` and
`compact-utf16,prohibit-unsafe,bounded-backtracking`; compile the probe against
each resulting `libregress-*.rlib` with `rustc +1.92.0 --edition 2024 --extern
regress=PATH -L dependency=DEPS_DIR tools/regex_compact_probe.rs -o PROBE`.
Redirect each probe's output to a UTF-8 file and compare byte-for-byte.

Detailed sizes, artifact/source hashes, raw timing samples, intermediate
experiments and validation outcomes are in
[the evidence JSON](2026-09-29-engine-size-round4-evidence.json).
