# Engine size reduction, seventh pass - 2026-09-29

Status: accepted after the revised comparison on 2026-09-30. Local distribution
artifacts contain the exact selected and tested bytes; source and evidence are
on `codex/engine-size-reduction`. The QuickJS size goal remains open.

Continues `codex/engine-size-reduction` from `eecf6508`. Lite retains its
existing JavaScript core and host API; its only feature exclusions remain
Intl, Temporal, Python and Torch. Full variants retain their existing features.

## Changes

The `wasm-no-fs-loader` profile already prevents the host and guest from
creating filesystem-backed deferred module namespaces. Most property access
paths compiled out their deferred-module trigger, but `has_property_dyn`
still referenced it directly. That single call retained the transitive module
loader, parser/linker routines and their support code. The same build-time
guard now covers this final trigger. Proxy traps, inherited property lookup,
symbol keys and dynamic-import coercion/rejection behavior are unchanged.
Native builds retain the trigger and filesystem loader.

TypedArray default sorting in the WASM profile now reads numeric primitives
directly during comparisons. It avoids repeatedly calling general-purpose
number coercion. BigInts retain the previous conversion path; the stable
sorting algorithm, signed-zero/NaN ordering, callback comparison order and
resource preflight remain unchanged. Native builds retain their former sorting path.

Promise `.then()` dispatch now shares one existing pristine-state proof with
handler registration. Previously method resolution and species selection
repeated checks of the same receiver and prototype. No guest code runs between
the shared proof and registration. Overrides, accessors, custom species and
foreign realms retain the generic path. The shared proof requires a warm cache,
so overrides installed before warmup do not pay for an extra failed proof; callable checks, scheduling and roots
are unchanged. The same guarded proof also accelerates ordinary `catch` and `finally` calls.
An own species getter is required after cache refresh, and finally wrappers
retain their existing roots and callback behavior. This consolidation also
applies to native builds.

Async activations now store and trace their creating function directly. This
removes an insertion into the generator-callee hash map and its lookup on every
resume. Settled-await loops reuse that immutable value when constructing each
frame. The activation's GC edge keeps named-function self references alive;
queue ordering, the initial yield and GC checkpoints are unchanged.

The integer-to-ASCII helper now uses `i32::unsigned_abs()` and 32-bit
arithmetic. Its unsigned magnitude still covers `i32::MIN`; the stack buffer
and output format are unchanged. Tests compare boundary and sampled values
against Rust and Node formatting.

In Lite, the JSON object parser keeps up to four temporary member pairs on the stack,
spilling remaining pairs to a Vec. Both ordinary and reviver-source parsing
share this storage. The final object still reserves the original exact member
count, including duplicates, before inserting keys in the same order. This
removes a temporary allocation for small objects without increasing their
retained property-storage capacity. The existing depth checks remain in place.

Lite internal errors now reserve their single message-property slot exactly and
skip coercion of the newly allocated message string. The generic Error
constructor keeps its prior path, including message conversion and cause
installation. In a 1,000-retained-RangeError probe, accounted heap decreases
from 777,380 to 717,380 bytes; instruction counts and other reported resources
match. These figures are engine accounting, not process RSS.

## Size

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125 `-O1`, Node 24.19.0,
Windows x64. Sizes exclude JavaScript glue; Brotli uses quality 11. The normal
prebuilt Rust standard library and existing optimization profiles are retained.

| Module | Previous raw | Current raw | Raw saved | Previous Brotli | Current Brotli |
| --- | ---: | ---: | ---: | ---: | ---: |
| Lite | 2,916,044 | 2,845,288 | 70,756 | 772,481 | 755,160 |
| JavaScript | 5,326,416 | 5,220,442 | 105,974 | 1,313,817 | 1,290,210 |
| Python-base | 7,001,211 | 6,896,451 | 104,760 | 1,685,440 | 1,663,170 |
| Python + Torch | 8,956,302 | 8,851,582 | 104,720 | 2,030,875 | 2,008,520 |

**The QuickJS size goal is not met.** Lite is still 1,706,899 raw bytes larger
than the 1,138,389-byte stripped QuickJS-NG v0.16.2 reference. The official
reactor includes 389,904 custom/debug bytes and totals 1,528,293 bytes.
Reference provenance and hashes are in the
[fourth audit](2026-09-29-engine-size-round4.md). These modules have different
host APIs and resource-isolation contracts; this is a size reference.

## Performance

The final byte measurements use both package load orders, Node
`--no-liftoff`, shared calibrated iteration counts, 10 warmup pairs and 31
alternating paired samples on an AMD Ryzen 9 9950X3D. No build, compression or
correctness-test job runs concurrently with timing. Ratios are candidate /
previous elapsed time, with reverse-order ratios inverted.

The new sorting screen covers default Array sorting, numeric-comparator Array
sorting and default Float64Array sorting, with random, sorted, reverse and
duplicate-heavy inputs of 32 and 2,048 elements. Node checks the complete
initial sorted output, signed zero, NaN and comparator stability. Every timed
result is checked; the timed work includes a fresh input copy. Calibration
uses five paired samples, with shared batches targeting 5 ms and capped at 64.

The separate Promise screen covers settled reactions, pending chains, an await
loop, catch, finally, subclasses, a patched prototype method and awaits of
primitive values. It performs
512 operations per call and checks the drained result against Node after each
call. It uses 10 warmup pairs, shared batches targeting 5 ms (cap 64) and 31
alternating paired samples. Timing includes host calls and result checks.

The full general screen also measures Python arithmetic, list construction and
dictionary lookup, checking every result, alongside one-function and
200-function Python initialization. These Python workloads measure the full
engine initialization, compilation, execution and teardown lifecycle; they are
not measurements of warm Python execution alone.

The JSON screen covers 0, 1, 3, 4, 5, 8, 64 and 2,048 members, with and
without an identity reviver. The error screen covers internal RangeError,
TypeError, typed-array and JSON errors, plus empty/message/cause/coercing and
subclass constructors. Every timed return is checked against Node. Both use
the same 10-warmup/31-pair protocol as the Promise screen.

A byte-identical two-package control measures the harness's own variation.
An additional error diagnostic retains results from eight sequential fresh
engines per shape, on the preceding Lite candidate whose executable code
section is identical to the scoped-storage revision. These diagnostics
supplement the original full-sequence measurements; they do not replace slower individual rows.

The first complete candidate still showed repeatable costs in Python-base
TypeError loops (1.047 / 1.028 and 1.062 / 1.081), and Python+Torch JSON
(1.033 / 1.074 and 1.087 / 1.080) and internal RangeError loops
(1.113 / 1.048 and 1.067 / 1.057). These results are retained. The JSON prefix
and exact internal-error capacity are now restricted to Lite; full profiles
retain their preceding allocation paths. The revised measurements below use
the rebuilt, scoped-storage modules.

Lite's revised 2,048-element Float64 random/reverse sorts take 43-45% less
elapsed time across the four comparisons (ratios 0.550-0.570). Pending Promise
chains take about 8-9% less time (0.911-0.918); ordinary catch/finally loops also
improve. Subclass and patched-method paths retain the generic behavior.

Individual variations remain: Lite's memoized `call-deep` ratios are
1.055 / 1.034 on the first core, then 1.005 / 1.019 on the second. Its numeric
Array sort with 2,048 duplicate-heavy elements measures 1.100 / 0.909 and
1.122 / 1.002. These order-dependent results are retained, not averaged away.

| General JavaScript corpus | Core 1 forward | Core 1 reverse | Core 2 forward | Core 2 reverse |
| --- | ---: | ---: | ---: | ---: |
| Lite | 1.000 | 1.018 | 1.001 | 1.002 |
| JavaScript | 1.015 | 1.017 | 1.001 | 0.997 |
| Python-base | 1.006 | 0.992 | 0.995 | 0.995 |
| Python + Torch | 1.022 | 1.007 | 1.004 | 0.996 |

This aggregate covers the established 12 JavaScript workloads. It excludes
the additional focused cases and Python lifecycle measurements; those results
are retained separately in the evidence. Ratios below one are faster.

No case is more than 2.5% slower in all four comparisons. Individual
slower rows remain in the evidence; this screening result does not prove
zero slowdown for every workload, host or runtime.

## Validation

* The safe-sandbox VM with the WASM no-loader profile and Python passes 540
  library tests, with four ignored.
* Twelve native Promise/realm integration suites pass 81 tests, including GC
  stress, custom capabilities, patched prototype values/accessors, deleted and
  reinserted properties, own shadows and cross-realm intrinsics.
* The no-loader native integration suite passes all five tests in the Lite
  feature configuration. Ordinary native no-loader semantics and module-cycle
  integration tests also pass without the artifact-only feature enabled.
* Lite, JavaScript and Python+Torch pass all 34 production boundary suites.
  Python-base passes 32; its two existing Torch-dependent failures remain
  `audit-2026-09-15-ml-transport.cjs` and `python-gpu.cjs`.
* The new module-loader boundary check covers direct/inherited Proxy `has`
  traps, symbol keys, revoked proxies, invariants, import argument effects,
  rejection ordering, source/defer phases and ShadowRealm import errors.
  It is included in the shared production boundary runner.
* A new WASM Promise mutation check warms dispatch before assigning, deleting,
  reinserting and redefining `then`, then adds and removes an instance override.
  Its complete output matches Node, including microtask order.
* TypedArray ordering checks now cover every Number typed-array kind and both
  BigInt kinds, including immediate and heap BigInts, for `sort` and `toSorted`.
* The named final Lite Rust module contains none of `import_module_inner`,
  `defer_ns_trigger`, `deferred_namespace_for`, `module_requests` or
  `ready_for_sync_execution`. Names are present for other functions, so this
  check is not performed on a stripped name section.
* The Python+Torch training checks match the stored CPU PyTorch reference
  values, including chained and hosted prepared sessions.
* Browser, tested Node and local distribution modules are byte-identical.
  Each distribution Brotli file decompresses to those exact bytes. The optimizer
  audits the unchanged 51/52 host imports and 1 GiB memory maximum.
* An additional collector run passed 19 tests, including parity across 13
  collector modes. Its multi-million-object debug old-garbage soak was
  deliberately stopped after more than 15 minutes (959.32 seconds suite time).
  The complete soak is unverified; its interruption is not counted as a pass.
* Full test262, cold-browser performance and native throughput were not run.
  CI remains skipped at the user's request.

## Rejected standard-library experiment

An isolated `-Z build-std` experiment rebuilt the matching Rust 1.92.0 sources,
first with default standard-library features and then with
`backtrace,panic-unwind,optimize_for_size`. Panic handling was preserved.
`RUSTC_BOOTSTRAP=1` was set only in those experimental processes; no production
build setting uses it. Rust documents `build-std` as
[unstable](https://doc.rust-lang.org/cargo/reference/unstable.html#build-std).

The default rebuild was larger (2,949,490 raw / 776,766 Brotli). The compact
version was smaller than the preceding Lite build (2,894,423 / 765,840), but sorting
regressed substantially: sorted 2,048-element default Arrays took
2.407 / 2.292 times the baseline, and duplicate-heavy TypedArrays took
1.883 / 1.915 times the baseline. The default rebuild also changed performance,
so not all differences can be attributed solely to the compact algorithms.
Both experiments are rejected; neither affects shipping artifacts.

The initial loader-only candidate also exposed smaller TypedArray sorting
regressions. The direct numeric comparison path addresses that cost. Raw
samples for intermediate candidates are retained with the final measurements.

## Reproduction

Use the ordinary `crates/zipp-wasm/build-variants.sh` pipeline. Generate Node
bindings from the same Rust artifact, then copy the selected post-processed
web WASM over the Node package's module before testing. Baselines are Lite
from round four and full variants from round three.

```
node --no-liftoff crates/zipp-wasm/tests/node/bench-size-pass.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
node --no-liftoff crates/zipp-wasm/tests/node/bench-sort-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
node --no-liftoff crates/zipp-wasm/tests/node/bench-promise-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
node --no-liftoff crates/zipp-wasm/tests/node/bench-json-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
node --no-liftoff crates/zipp-wasm/tests/node/bench-error-modes.cjs BASE_NODE_DIR CANDIDATE_NODE_DIR
node crates/zipp-wasm/tests/node/run-boundary-suite.cjs
```

Reverse package arguments for the second measurement. Final artifact/source
hashes, optimizer decisions, validation results and original timing samples are
in [the evidence JSON](2026-09-29-engine-size-round7-evidence.json). Original
intermediate and control samples are retained in the
[compressed experiment JSON](2026-09-29-engine-size-round7-experiments.json.gz);
the evidence file records its compressed and uncompressed hashes.

## Promise performance follow-up

The first loader-plus-sort candidate repeatedly slowed the retained-state
Promise workload in full variants. On the second core, Python-base ratios were
1.114 / 1.049 and Python+Torch 1.079 / 1.084. These were not accepted as noise
or hidden by the aggregate throughput result. An intermediate cache-slot reuse
change still measured 1.040 / 1.070 for Python-base, so it was discarded.

The next candidate consolidated the complete method/species proof.
Its first Python-base comparison measured 0.910 / 0.923 for that workload.
Final per-variant timing results are recorded separately in the evidence file. Measurements
retain the original workload sequence; isolated fresh-process Promise results
were not substituted for the sequence that exposed the slowdown.

A preliminary GC-root test invocation used the artifact no-filesystem profile
for a suite that also asserts successful filesystem imports; that import
assertion failed. The complete suite passes in the ordinary native profile
with the filesystem loader enabled. This does not weaken the artifact's
separate no-loader contract checks.

The expanded Promise screen found additional await/override costs in the
intermediate shared-proof build. The warm-cache gate and activation-owned
callee address these paths. A force-inline handler helper alone did not fix the remaining catch/finally
cost in the full build. Guarded shortcuts now reuse the same proof in catch
and finally, while preserving observable constructor, species and then lookups
on the fallback path. The guard checks the refreshed cache for an own species
getter so an inherited accessor cannot be skipped. Original intermediate
samples are retained alongside the final measurements.

The catch/finally candidate still exposed a repeated Python-base settled-then
cost (1.041 / 1.043 on the first core, 1.058 / 1.071 on the second) and small
Lite string costs. The full builds now inline the small shared-proof wrapper;
Lite retains the shared function because forcing it inline worsened the
optimizer's Brotli choice. Integer formatting uses a 32-bit unsigned magnitude
instead of widening to 64 bits. These choices are checked again on the final
selected artifacts; earlier passing aggregate ratios do not substitute for
that verification.

A 1,000-suspended-activation resource probe reports 12,000 more accounted heap
bytes after the callee moves into the payload. The old `gen_callee` hash-map
allocation was not included in this estimate, so the figures are not a net
resident-memory comparison. This pass makes no measured RSS-reduction claim.
