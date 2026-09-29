# Engine size reduction, third pass - 2026-09-29

Continues `codex/engine-size-reduction` from `76045145`, with execution speed
as a constraint on size reductions. No further language features are removed.
Lite still omits only Intl, Temporal, Python and Torch. Full builds retain
`opt-level = 3`; Lite retains `opt-level = "z"`, which was already slower than
the full engine before this pass.

## Selected artifacts

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125 `-O1`, Node 24.19.0 on Windows.
The table measures the selected post-bindgen module without metadata sections;
JS glue is excluded. Compression is Brotli quality 11.

| Variant | Previous raw | Current raw | Raw saved | Previous Brotli | Current Brotli |
| --- | ---: | ---: | ---: | ---: | ---: |
| Lite | 2,952,989 | 2,942,452 | 10,537 | 776,971 | 774,683 |
| JavaScript | 5,356,850 | 5,326,416 | 30,434 | 1,322,063 | 1,313,817 |
| Python-base | 7,036,528 | 7,001,211 | 35,317 | 1,693,025 | 1,685,440 |
| Python + Torch | 8,991,628 | 8,956,302 | 35,326 | 2,037,775 | 2,030,875 |

## Retained changes

* Full WASM builds share construction of 883 fixed error messages through a
  cold function. The literal text, owned String representation and public
  error type are unchanged. Dispatch, property access, coercion, host-call
  entry and the recursion guard retain their original construction to avoid
  disturbing the busiest code. Lite and native builds inline the helper.
* Lite resolves 58 cached native experiment switches to their existing
  absent-environment defaults. Native, WASI and full WASM builds retain their
  previous switches. Fast paths stay enabled. A Rust/Node probe confirmed
  that `std::env::var_os` on this bare WASM target stays absent even when the
  host's Node environment contains the variable. This matches the target's
  [lack of host OS integration](https://doc.rust-lang.org/rustc/platform-support/wasm32-unknown-unknown.html).
* JavaScript and Python parser error constructors are outlined only for bare
  WASM. Error messages and source offsets remain unchanged.
* Packed Unicode intervals use a decoder with at most three byte reads rather
  than a variable-shift loop. Compile-time table validation bounds every gap
  and width to U+10FFFF. Matching does not decode these tables. The generated
  source tables remain authoritative, with all 367 tables checked for exact
  round trips and an additional test at encoding-width boundaries.

## Performance screening

The screen uses Node `--no-liftoff`, checks workload answers, renews the
instruction budget for each call, warms both packages 10 times, and takes 31
alternating paired samples. A shared batch count targets at least 5 ms per
sample, capped at 512 calls; reported times are per call. The original
12-workload geomean is retained, with separate focused rows for actual
recursive calls with observable side effects, caught errors, property deletion,
enumeration, strings, regex compilation/matching, promises, and JS/Python
initialization. The old `call-deep` workload can be memoized; it is not treated
as evidence of actual recursion throughput.

Final runs use processor affinity mask 4 on an AMD Ryzen 9 9950X3D, with no
other build, compression or test job running. Both package load orders are
reported. All ratios below are normalized to selected / previous elapsed time;
lower is faster.

| Variant, executing JS corpus | Forward order | Reverse order |
| --- | ---: | ---: |
| JavaScript | 0.956 | 1.003 |
| Lite | 0.999 | 0.990 |
| Python-base | 1.002 | 0.996 |
| Python + Torch | 0.996 | 1.001 |

There is no large slowdown repeated in both orders in the final screen.
This does not prove identical performance for every workload. The JavaScript
forward run's large property-access gains did not repeat in reverse order;
they are not claimed as a reliable speedup. Python-base's JSON row was 8.6%
slower forward but 4.8% faster in reverse. Sub-millisecond exception and
memoized-call results also varied. These rows remain in the evidence.

Python-base initialization ratios were 1.003 / 1.029 for one function and
0.996 / 1.004 for 200 functions (forward / reverse). The full Torch build's
corresponding ratios were 0.991 / 0.999 and 0.992 / 0.989. These are warm
parse/lower/initialize measurements, not cold browser startup.

Against the original pre-optimization JavaScript build at `09862c66`, the JS
corpus ratios are 0.988 / 0.994. In that same screen, Unicode regex compilation
is 1.022 / 0.979. A separate repeat of the first audit's shorter focused
harness gives 1.011 / 1.077 for regex compilation and 0.985 / 0.969 for named
timezone lookup. Consequently, the earlier packed-table compilation overhead
cannot be declared eliminated, despite the broadly preserved overall engine
throughput. The shorter screen uses five warmups and 21 unbatched samples;
its protocol, samples and both orders are retained separately.

## Rejected or narrowed experiments

* Lite at Rust `opt-level = "s"` grew to 3,582,356 raw / 901,323 Brotli bytes.
  Its existing `"z"` profile is retained.
* Explicit error outlining made Lite slightly larger than its flags-only
  candidate (101 raw bytes and 1,531 Brotli bytes), so Lite inlines the helper.
* Specializing native experiment switches in all WASM variants saved another
  few kilobytes, but the full JavaScript candidate had inconsistent and
  sometimes repeatable slower arithmetic/property-access rows. Specialization
  is restricted to Lite. Error outlining was also removed from the hot paths
  listed above, accepting a smaller size reduction in exchange for the final
  measured performance.

## Validation

* Rust VM with Python and safe-sandbox: 528 passed, 4 ignored. Native default
  JIT-feature compilation also passes.
* The complete applicable regex unit/integration/doc suite passes, including
  all generated-table round trips and the new decoder boundaries.
* Python parser: 17 integration tests and 1 doc test passed, 2 ignored.
* Exact selected JavaScript, Lite and Python + Torch modules: 29/29 production
  boundary suites each, including resource ceilings and feature contracts.
* Python-base: 27/29 suites pass, including 57 frontend and 16 app-bridge checks.
  The ML transport and GPU suites additionally assume bundled `_zipp_tensor`
  and `torch`; those parts fail with ModuleNotFoundError on both the previous
  and selected Python-base builds. They pass on the full Python + Torch build.
  These failures are retained in the evidence, not relabeled as passing tests.
* Python + Torch: 47 GPU checks pass, and the training suite matches the
  checked-in PyTorch oracle, including hosted prepared sessions.
* All four selected modules pass host-import and 1 GiB memory-maximum audits.
  Their shipping web bytes and tested Node bytes are identical.
* All 883 moved fixed error literals match the previous source byte-for-byte.
  Node benchmark syntax and Git whitespace checks pass.

Full test262, cold-browser and native-throughput benchmarks were not run.
CI is skipped at the user's request; these are local validation results.

The prior audit's prose contained stale performance numbers despite its table
and evidence being current. This pass corrects that prose against the retained
second-pass evidence; it does not replace the older measurements.

## Remaining gap and reproduction

Lite is still approximately 1.93x the raw size and 1.86x the compressed
size of the repository's historical QuickJS-NG 0.16.2 reactor reference
(1,528,293 raw / 417,087 Brotli). This is not a fresh matched-feature comparison,
and this pass does not establish QuickJS size parity. Native binary size,
cold browser startup and general application throughput are not claimed here.

Use `crates/zipp-wasm/build-variants.sh lite javascript python all` with the
versions above. `tools/measure_wasm_size.mjs` records size and SHA-256.
`node --no-liftoff crates/zipp-wasm/tests/node/bench-size-pass.cjs BASE CANDIDATE`
runs the paired screen; swap package arguments for a reverse-order repeat.
For the final Windows measurements, the Node process was pinned to processor
affinity mask 4 (logical processor 2) before the timed workloads. Neither builds,
compression nor other test jobs ran alongside those measurements.

[Machine-readable evidence](2026-09-29-engine-size-round3-evidence.json)
contains artifact hashes, optimizer decisions, final samples, and summaries of
the exploratory runs (including slower results). Local selected web packages
are in `crates/zipp-wasm/dist/{lite,javascript,python,all}/`; generated binaries
remain ignored. The Node test packages contain those same selected WASM bytes.
