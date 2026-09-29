# Compact error payload experiment - 2026-09-29

This follow-up to `40453bf5` was **rejected and reverted**. The selected Lite
artifact remains the [fourth pass](2026-09-29-engine-size-round4.md).

The prototype stored completed Lite WASM error messages as `Box<str>` rather
than `String`, removing spare-capacity storage that errors do not use. A WASM
layout probe confirmed these sizes:

| Type | String error | Boxed string error |
| --- | ---: | ---: |
| Error payload | 12 | 8 |
| Result of u32 or unit | 12 | 8 |
| Result of u64 / VM value-sized payload | 16 | 16 |

The first candidate adapted message construction and ownership boundaries. The
second shared formatted-message conversion through an outlined cold helper,
while leaving native/full profiles on their original String representation.

Both were built with Rust 1.92.0, wasm-bindgen 0.2.126 and the normal Binaryen
125 `-O1` selection pipeline. Sizes exclude JavaScript glue.

| Candidate | Raw bytes | Brotli-11 bytes |
| --- | ---: | ---: |
| Selected fourth pass | 2,916,044 | 772,481 |
| Compact errors, direct conversion | 2,891,378 | 778,588 |
| Compact errors, shared conversion | 2,891,491 | 778,196 |

Raw size fell by about 25 KB, but download size grew by 5,715 to 6,107 bytes.
The smaller Rust payload did not improve both size measures, so neither change
is retained. This also shows why Rust structure size alone is insufficient
justification for a rewrite.

Lite compilation, native/Python compilation, and the candidates' host-import
and 1 GiB memory-maximum audits passed. Exception and general performance
benchmarks were not run after the size rejection; no speed claim is made.
All 36 modified source files were restored to `40453bf5`. The local selected
Lite module remains byte-identical to the fourth-pass artifact.

Hashes, section sizes and layout-probe results are in
[the evidence JSON](2026-09-29-engine-size-round5-evidence.json).
The QuickJS size goal remains unmet.
