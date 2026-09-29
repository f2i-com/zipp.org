# Engine size reduction, eighth pass - 2026-09-30

Continues `codex/engine-size-reduction` from `85885bb6`. The final change is
specific to ZIPP Lite. Full JavaScript and Python distribution packages retain
the previously accepted round7 bytes. Lite still excludes only Intl, Temporal,
Python and Torch; no additional feature, host API or resource limit is removed.

## Change

Lite intrinsic subclass construction previously referenced the derived
`HeapObj::clone` implementation. That retained copying machinery for every heap
representation, including suspended activations and compiler-owned class
metadata. The Lite path now copies only the representations its intrinsic
constructors return: arrays, functions/closures, boxed primitives, dates,
regular expressions, typed arrays and DataViews. The public `HeapObj: Clone`
implementation remains available.
The named raw Lite module no longer contains its derived clone function.

Lite also checks that the parent is an intrinsic constructor before selecting
this narrowed path. A user function may share an intrinsic's `.prototype` while
returning an arbitrary object; it must use ordinary construction to preserve
return identity and `new.target`. The Lite regression fixture covers this case.
Full builds retain their original implementation, including the pre-existing
edge case for such user functions; changing their constructor path was not
accepted in this size pass.

Feature-gated macros keep the original full-profile expressions at the call
sites. Heap replacement, roots, write barriers, side-table transfers, view
tracking and DataView invalidation retain their existing behavior. Payloads
are still copied; no live object is moved out of the heap.

## Selected size

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125 `-O1`, Node 24.19.0,
Windows x64. Module bytes exclude JS glue; Brotli uses quality 11.

| Module | Previous raw | Selected raw | Previous Brotli | Selected Brotli |
| --- | ---: | ---: | ---: | ---: |
| Lite | 2,845,288 | 2,836,723 | 755,160 | 754,122 |
| JavaScript | 5,220,442 | 5,220,442 | 1,290,210 | 1,290,210 |
| Python-base | 6,896,451 | 6,896,451 | 1,663,170 | 1,663,170 |
| Python + Torch | 8,851,582 | 8,851,582 | 2,008,520 | 2,008,520 |

Lite saves 8,565 raw bytes and 1,038 Brotli bytes in this pass. The final
optimizer gate accepts its `-O1` output against 2,940,728 raw / 754,792 Brotli
bytes before post-processing. Full distributions keep their preceding modules
and glue unchanged.

**The QuickJS size goal is not met.** Lite remains 1,698,334 raw bytes larger
than the 1,138,389-byte stripped QuickJS-NG v0.16.2 reference. The official
reactor is 1,528,293 bytes, including 389,904 custom/debug bytes. Reference
provenance and hashes are in the [fourth audit](2026-09-29-engine-size-round4.md).
The host APIs and resource-isolation contracts differ; this is a size reference.

## Performance

Final Lite measurements compare the exact selected bytes
with round7, in both package load orders on affinity masks 4 and 16, using Node
`--no-liftoff` on an AMD Ryzen 9 9950X3D. Each case has 10 warmup pairs and 31
alternating paired samples with a shared calibrated batch size. No build,
compression or correctness-test job runs concurrently with timing.

The general screen covers execution, focused runtime cases and JavaScript
initialization. A new subclass screen covers an ordinary user parent and
Array, TypedArray, DataView, Boolean, Number, String, Date, RegExp, Function and
GeneratorFunction parents. Each guest call constructs and uses 100 instances.
Every timed subclass result is checked against Node; timings include the host
calls and checks. Batches target 5 ms with a cap of 64, or two for dynamic
function constructors. Each dynamic construction retains two functions, so
the cap keeps the maximum at 14,400 under the unchanged 16,384 lifetime limit.
An initial cap of three exceeded that limit and was corrected; it is not
counted as an engine failure.

| General JavaScript corpus | Core 1 forward | Core 1 reverse | Core 2 forward | Core 2 reverse |
| --- | ---: | ---: | ---: | ---: |
| Lite | 0.994 | 0.994 | 0.975 | 0.998 |

These geometric means cover the established 12 execution workloads. Additional
focused cases and initialization timings remain separate in the evidence.
No measured case exceeds a 2.5% slowdown in all four comparisons. Array
subclasses measure 0.988 / 0.989 and 0.990 / 0.970. Individual slower rows are
retained: Unicode RegExp compilation measures 0.997 / 1.027 and 0.964 / 1.069;
initialization with 200 functions measures 1.010 / 1.013 and 1.009 / 1.087.
This is a bounded screening result, not proof of identical speed for every
workload, host or runtime.

## Rejected candidates and control

Applying the compact copy routine and constructor lookup to full profiles
produced repeated subclass slowdowns. Trials with forced inlining, a shared
helper, and restoration of the full copy routine were also rejected as full
build combinations. For example, the initial Python-base Array subclass
ratios were 1.079 / 1.037 and 1.058 / 1.098; the later scoped-copy candidate
still measured Python+Torch Array at 1.055 / 1.084 and 1.147 / 1.061.
Ratios are candidate / previous elapsed time; lower is faster. All original
samples remain in the experiment archive.

The comparison also exposed a toolchain metadata difference: fresh full builds
no longer retained a separate `/rustc/.../library/alloc/src/string.rs` diagnostic
path alongside the installed Rust source path. In the raw JavaScript module,
function names and code-section sizes matched, while static data shrank by
64 bytes and addresses relocated. To distinguish this from the source change,
the committed, unchanged Python source was rebuilt with the current toolchain.
It produced exactly the same SHA-256 as the final Lite-gated full Python build:
`adb8af4cc0d8a9d6c9c11285e968b3fe279ac0ca11187bbb41db83d4f1b5cc97`.
Earlier full-build timing differences therefore cannot all be attributed to
the refactor. The selected full distribution artifacts remain the previously
accepted bytes; the fresh full rebuilds are retained only as diagnostics.

An additional Binaryen `--merge-similar-functions` pass produced 2,798,644
raw / 754,641 Brotli bytes on the initial Lite candidate. Following it with
another `-O1` produced 2,795,406 / 753,551 bytes. Although smaller raw, this
increased Brotli size relative to that source-only candidate and repeatedly
slowed integer arithmetic, property access, enumeration, string append/index
and Promise resolve across both cores and load orders. It was rejected;
production optimization flags are unchanged.

## Validation

* Safe-sandbox VM library with no filesystem loader and Python: 540 passed,
  four ignored. Targeted full-profile native integration: 26 passed across
  subclass payloads, TypedArray species, GC natives and DataView fast paths.
* Lite native subclass suite: four passed, including GC stress and foreign
  realm construction. Its identity fixture covers user functions sharing
  builtin prototypes while returning the same object with the subclass as
  `new.target`.
* Shared payload fixture matches Node for holes, all 11 TypedArray kinds,
  resizable views, dynamic functions, generators, async functions and async
  generators. Full native coverage also includes Intl and Temporal subclasses.
* Final Lite package: all 35 production boundary checks pass.
* All three retained full packages pass the new common subclass fixture.
  Their unchanged round7 bytes retain the preceding 34 boundary-suite results
  and full Python/Torch training validation. Python-base's two known
  Torch-dependent failures remain `audit-2026-09-15-ml-transport.cjs` and
  `python-gpu.cjs`.
* Selected browser and tested Node modules match. The unchanged host import
  counts are 51/52 and the memory maximum remains 16,384 pages (1 GiB).
  Local distribution hashes match the selected modules, and every distribution
  Brotli file decompresses to those exact bytes.
* Full test262, native throughput and cold-browser performance were not run.
  Final production behavior changes are scoped to `wasm-lite`; native
  correctness tests are not a throughput measurement. CI remains skipped at
  the user's request.

Paired samples, normalized ratios, source/artifact hashes, the control build
and validation summaries are in [the evidence file](2026-09-30-engine-size-round8-evidence.json).
Rejected experiments are retained in the [compressed archive](2026-09-30-engine-size-round8-experiments.json.gz).
