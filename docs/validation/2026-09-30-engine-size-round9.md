# Engine size reduction, ninth pass - 2026-09-30

Continues `codex/engine-size-reduction` from `51187ea6`. This pass removes
Python-only instruction variants from Lite and shares register-remapping code
across instructions with the same operand shape. The selected runtime change
is confined to Lite; full distribution packages retain the accepted seventh
pass bytes. Lite still excludes only Intl, Temporal, Python and Torch. Full builds retain those features.

## Implementation

Lite no longer compiles the 31 `Py*` instructions that only the Python frontend
emits: its instruction set contains 228 variants instead of 259. The disabled
Python fallback dispatch and the corresponding register-remapping arms are
also excluded. The native Python cache generator retains the complete schema
and tag ordering; it accepts only the explicit Lite exclusion attribute and
continues rejecting unrelated conditional variants. Schema equivalence and
native cache roundtrip/damage/key checks cover this boundary.

The register mapper previously repeated a body for every instruction. It now
uses one exhaustive operand list and 15 mapping bodies, preserving each register
operand and callback order, including optional operands and argument-window
bases. The exhaustive match still prevents a newly added instruction from
silently escaping remapping. Native register-class tests and a fresh-engine
compiler benchmark exercise register classes, optional operands and argument
windows. A feature-gated macro expands grouped arms for Lite and a separate
arm per variant for full builds. This retains the original full separate-arm strategy
without repeating the operand descriptions or mapping logic in the source.

The initial Lite candidate used Rust's naturally narrowed one-byte instruction
tag. Although the instruction remained 32 bytes because of the inline i128
operand, it changed dispatch code generation. Three measured cases exceeded
a 2.5% slowdown in three of four comparisons, so that candidate was rejected.
The selected experiment explicitly retains a u16 tag for Lite. Full layouts
are unchanged; fresh full raw builds before and after this Lite-only attribute
are byte-identical. The final feature-gated mapper rebuild also produces
exactly the same raw Lite SHA-256 as the measured candidate; its Web/Node
post-processed module is byte-identical and its Brotli payload decompresses
to those selected bytes.

## Packaging

Full builds retain the default optimizer rule: raw size must decrease and
Brotli quality 11 must not grow. Lite explicitly passes `--prefer-raw-size`.
That preference permits a Brotli increase only when the raw saving is at least
64 KiB; growth must be at most 0.1% of the preceding compressed size and no
more than 1 KiB. Policy tests cover both defaults and the allowance boundaries.
Both modules validate and must have identical import/export surfaces. The
selected module's memory maximum remains 1 GiB.

For the wider-tag Lite candidate, Binaryen 125 `-O1` reduces 2,931,051 raw bytes
to 2,827,053, while Brotli grows from 753,271 to 753,477 bytes. This is a
103,998-byte raw saving for 206 compressed bytes. Relative to the accepted
eighth pass, both raw and compressed sizes decrease.

## Validation and measurements

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125 `-O1`, Node 24.19.0,
Windows x64. Module bytes exclude JS glue; Brotli uses quality 11.

| Module | Previous raw | Selected raw | Previous Brotli | Selected Brotli |
| --- | ---: | ---: | ---: | ---: |
| Lite | 2,836,723 | 2,827,053 | 754,122 | 753,477 |
| JavaScript | 5,220,442 | 5,220,442 | 1,290,210 | 1,290,210 |
| Python-base | 6,896,451 | 6,896,451 | 1,663,170 | 1,663,170 |
| Python + Torch | 8,851,582 | 8,851,582 | 2,008,520 | 2,008,520 |

Lite saves 9,670 raw bytes and 645 Brotli bytes. Full distribution modules and
glue remain byte-identical to their previously accepted packages.

## Performance and rejected full candidates

The exact selected Lite module is compared with the eighth pass in both package
orders on affinity masks 4 and 16, with Node `--no-liftoff` on an AMD Ryzen 9
9950X3D. Each case uses ten warmup pairs and 31 alternating paired samples;
a shared calibrated batch targets at least 5 ms. General cases run in separate
processes, while compiler and subclass suites run as grouped cases. General
results are checked against Node before timing; compiler/subclass results are
checked on every timed call.

| Lite general JavaScript corpus | Core 1 forward | Core 1 reverse | Core 2 forward | Core 2 reverse |
| --- | ---: | ---: | ---: | ---: |
| Candidate / previous elapsed time | 1.009 | 1.003 | 1.001 | 1.000 |

These means cover the established twelve execution workloads. Focused runtime
cases, initialization, three compiler cases and eleven subclass cases remain
separate in the evidence. No measured Lite case exceeds a 2.5% slowdown in
three of four comparisons, a stricter rejection criterion than the preceding
pass. Individual slower rows remain visible. This is bounded screening, not a
proof of equal speed on every workload or host.

This task ran no build, compression or correctness test concurrently with
timing. Unrelated Rust builds repeatedly interrupted measurements: each timing
process was guarded against `rustc`, and interrupted samples were discarded
and retried. The guard does not establish that every other host activity was
idle.

Grouped remapping in full builds saved roughly 5 KiB raw per module, but was
rejected after measured slowdowns. Full JavaScript array construction ratios
were 1.022 / 1.039 / 1.029 / 1.042, and Boolean subclass construction was
1.044 / 1.054 / 1.035 / 1.097. Python-base error handling was
1.031 / 1.034 / 1.008 / 1.070. Python+Torch error handling exceeded the threshold
in all three completed general comparisons. Its remaining confirmation samples
were stopped after rejection; they are not presented as completed checks.
The full mapper consequently retains its separate-arm expansion.

The rejected full candidates also had small compressed-size increases against
the preceding accepted artifacts. Fresh builds have the diagnostic metadata
variation documented in the eighth audit, so these differences must not all be
attributed to source changes. Full packages retain the accepted artifacts.

## Correctness

Final-source native Lite checks pass 510 library and 20 integration tests, with
one ignored library test. Full Python safe-sandbox checks pass 540 library and 21 integration
tests, with four ignored library tests. The exact selected Lite module passes 35/35 Node boundary checks. The rejected
full JavaScript and Python+Torch candidates also pass 35/35.
Python-base passes 33/35; the two existing Torch-dependent checks still fail
without its optional package. Python+Torch training parity checks pass.

No test262, native throughput or cold-browser measurement is claimed. The
QuickJS size target remains unmet: the wider-tag Lite candidate is 1,688,664
raw bytes larger than the audited 1,138,389-byte stripped QuickJS-NG v0.16.2
reference. Reference provenance is in the
[fourth audit](2026-09-29-engine-size-round4.md).

A broader native integration invocation also attempted the full-only
`audit_20260912_strict_tonumber_bigint` fixture under Lite. It requires `Intl`
and therefore fails under this intentionally subtractive profile; that run is
retained in the experiment archive and is not counted as a successful suite.
The final native checks explicitly select the six relevant integration targets.

Machine-readable module hashes, source hashes, optimizer decisions, samples,
validation results and rejected candidates are in
[the evidence](2026-09-30-engine-size-round9-evidence.json). Interrupted and
additional experimental samples are retained in the compressed experiment
[archive](2026-09-30-engine-size-round9-experiments.json.gz). CI is skipped at the user's request.
