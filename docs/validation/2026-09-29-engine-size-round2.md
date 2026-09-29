# Engine size reduction, second pass — 2026-09-29

Continues `codex/engine-size-reduction`, using `065446f2` as the baseline.
Full JavaScript, Python and Torch functionality is retained. Lite still omits
only Intl, Temporal, Python and Torch; it retains its inline caches and core
language features.

## Measured artifacts

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125, Node 24.19.0 on Windows x64.
All numbers describe the post-bindgen module without name, producers or target
features sections. JS glue is excluded. Brotli uses quality 11.

| Variant | Previous raw | Current raw | Raw reduction | Previous Brotli | Current Brotli |
| --- | ---: | ---: | ---: | ---: | ---: |
| Lite | 3,076,218 | 2,952,989 | 123,229 (4.0%) | 780,480 | 776,971 |
| JavaScript | 5,534,466 | 5,356,850 | 177,616 (3.2%) | 1,325,640 | 1,322,063 |
| Python-base | 7,258,351 | 7,036,528 | 221,823 (3.1%) | 1,701,116 | 1,693,025 |
| Python + Torch | 9,217,822 | 8,991,628 | 226,194 (2.5%) | 2,048,552 | 2,037,775 |

The paired source changes alone save 6,428 raw bytes in Lite, 13,952 in
JavaScript, and 24,902 in each Python variant. The remaining reduction comes
from conservative WASM post-processing. Both stages reduce compressed size.

## Changes

* Class decorator application visits four fixed groups in source order instead
  of instantiating a general-purpose stable sort. The application order is
  unchanged, with no sort scratch allocation and linear work. The temporary
  list is released before compiling the remaining class initializers.
* Python integer hoisting visits allocating integers first, then immediates,
  preserving each group's original order and the 32-register cap. This removes
  a separate i128 stable sorter and its scratch allocation. No parser or Python
  runtime capability is removed.
* TypedArray `sort` and `toSorted` share one sorter. Callback order, ToNumber
  coercion, stable ties, abrupt completion and numeric ordering are retained.
* Local variant builds and release packaging try pinned Binaryen 125 `-O1`.
  The selection guard validates the modules and their import/export surfaces,
  then accepts only a smaller raw module with no Brotli regression. The normal
  memory/import audit runs afterwards. Release Node tests use the selected
  module, and packaging requires byte identity with that tested module.

`optimize-wasm.sh` enables only the existing Rust WASM target's relevant
features, rather than allowing every experimental WASM proposal. No resource
limits, overflow checks or sandbox guarantees are relaxed. The full builds
retain their existing release optimization level; Lite retains its size profile.

## Performance screening

The ordinary quick benchmark initially showed large, inconsistent changes
from V8 tiering. It was not used to make the performance decision. The new
`bench-size-pass.cjs` shares the existing 12-workload corpus, runs Node with
`--no-liftoff`, checks results, renews the instruction budget each call, and
uses 10 warmups plus 31 alternating paired samples per workload. No build or
compression jobs ran alongside these measurements.

| Candidate versus previous artifact | Geomean elapsed-time ratio |
| --- | ---: |
| Full JavaScript | 0.995 |
| Lite | 0.797 |
| Python-base, executing the same JS corpus | 0.994 |

Lower is better. Full JavaScript is effectively unchanged in this screen;
individual ratios range from 0.974 to 1.024. Lite improves on all 12 rows in
this run, but remains substantially slower than the full build. Python-base
initialization measures 6.693 → 6.754 ms for a one-function program and
9.043 → 8.754 ms for 200 functions. These warm measurements include parsing,
lowering, runtime seed cloning and execution; they are not cold browser startup
or a broad application performance guarantee. Raw samples are preserved.

One Python-base array-callback row was 16.3% slower in that run. A repeat with
package load order reversed measured that row 0.9% faster and the JS geomean
1.7% faster; the initialization ratios also varied. The first result is retained
in the evidence rather than discarded. This variability limits performance
claims, especially for allocation-heavy work; it does not change the measured
artifact size reductions.

## Rejected experiments

Disabling Lite's inline caches saved only 20,678 raw / 6,602 Brotli bytes and
made the recursive-call workload dramatically slower by also bypassing its
existing call specialization. That source experiment was reverted completely.

On the previous Lite baseline, function merging reduced raw size to 2,866,116
but increased Brotli to 787,946. Binaryen `-O2` produced 2,912,048 raw but
803,628 Brotli. Neither is shipped. The first audit's `-Oz` candidate also
increased compressed size. `-O1` earns its place through measurements, not
through its name or the smallest uncompressed result.

## Validation

* Rust VM library with Python and safe-sandbox: 528 passed, 4 ignored.
* Exact selected JavaScript and Lite WASM: 29/29 production boundary suites
  each, including syntax, resource limits, imports, memory ceiling and feature
  contracts.
* Python-base and Python + Torch: frontend 57/57, app bridge 16/16, feature
  contracts and new ordering regression checks pass.
* Python + Torch: GPU 47/47 and training tests against the checked-in PyTorch
  oracle pass, including the hosted prepared WASM session.
* New ordering checks cover interleaved decorator groups, TypedArray numeric
  order, signed zero, NaNs, stable ties, callback/coercion order, throwing
  coercion, and Python hoisting beyond its 32-constant budget.
* Selected modules for all four variants pass memory and import audits.
* Five optimizer selection checks pass, including refusal of a Brotli
  regression, changed interfaces and invalid WASM without overwriting the input.
  Bash, Node and release YAML syntax checks pass.

Full test262 and the release workflow were not run. The user requested skipped
CI; the workflow changes are checked locally, not claimed as a successful CI run.

## Remaining QuickJS gap

Lite is still **not at QuickJS size**. Against the repository's historical
QuickJS-NG 0.16.2 reactor measurement (1,528,293 raw / 417,087 Brotli), current
Lite is approximately 1.93× raw and 1.86× compressed. This is a directional
target, not a fresh comparison with matched interfaces, engine features,
compiler and sandbox settings. See the [first audit](2026-09-29-engine-size-audit.md)
for the reference and why native QuickJS headline sizes cannot be compared
directly with an embedded WASM artifact.

The remaining gap needs larger code-structure work: interpreter opcode
handlers, built-in dispatch and regex specializations dominate the residual
code. Extra feature removals were not used to improve these numbers. Further
work should keep raw size, Brotli size, live memory and execution cost separate;
source-level compression or deleting fast paths can improve one while hurting
the others.

## Reproduction and evidence

Install `binaryen@125.0.0`, then use `crates/zipp-wasm/build-variants.sh` with
`lite javascript python all`. Existing Rust, target and wasm-bindgen setup is
documented in the WASM README. `tools/measure_wasm_size.mjs` records exact bytes
and SHA-256; `bench-size-pass.cjs` takes baseline and candidate Node package
directories. The optimizer guard reports both candidates' sizes, so a future
build can reject a wire-size regression without silently shipping it.

[Machine-readable measurements and benchmark samples](2026-09-29-engine-size-round2-evidence.json)
include the previous artifact sizes, final SHA-256 values, rejected candidates,
optimizer decisions and validation counts. Local finished packages are under
`crates/zipp-wasm/dist/{lite,javascript,python,all}/`; generated binaries remain
ignored rather than being committed.
