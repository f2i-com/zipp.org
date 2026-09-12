# Correctness audit — 12 September 2026

Starting checkout: `ee107d4b78996dc9caf3593780920e02486da0b3` (v0.0.16).
`git pull --ff-only` reported that main was already current. Changes below are
working-tree changes; no release or performance claim accompanies this audit.

## Reproduced and fixed

### Host values and exceptions

Host-created strings and structured arguments existed only in Rust locals
before the called function's frame was installed. A callable Proxy's `apply`
getter could allocate and collect them in that window. Under forced collection,
a structured argument returned as `Opaque` and a string as an unrelated Date.
Host calls now root their callee and arguments across the invocation, keeping
collection enabled. Nested entries restore their own root-stack prefix.

A compatibility script's top-level return value also lacked a root while its
queued jobs ran. The return value now survives the event-loop drain.

The primitive embedding API deliberately swallows result `toString` failures,
but left the pending guest exception behind. Its next independent JIT call
then threw that old error. Similarly, an error handled by a `HostCtx` callback
remained pending. Both boundaries now consume delivered exceptions. Result
rendering itself retains the result across guest conversion code.

`audit_20260912_host_exceptions.rs` and `audit_20260912_host_input_roots.rs`
contain six reproduced defect regressions and one conversion-error control.
The previous host-root suite also passes with these changes.

Final cross-review reproduced an adjacent nested-Proxy lifetime bug. An
`apply`, `toString` or `@@toPrimitive` getter returning a fresh callable Proxy
could lose that Proxy's target, or the generated argument array, during its
own allocating `apply` lookup. Three additional regressions reproduce an
`Invalid Date is not a function` error or an empty rendered result. The rare
Proxy call path retains these temporary values across trap lookup and dispatch,
including target and handler edges that a revoking getter can remove.
Two neighboring-behavior tests also check revocation, null/non-callable traps
and root-stack cleanup.

### TypedArray species and byte copies

- Same-type `slice()` with a species result aliasing its source now copies
  raw bytes in the specified ascending order. Floating-point NaN payloads are
  preserved, including overlapping copies; a number round trip or `memmove`
  has different observable semantics here.
- Species results must have matching Number/BigInt content types, including
  empty results. `map` validates this before invoking its callback.
- Custom `subarray` species results are validated for detached/out-of-bounds
  storage. Short and immutable results remain legal for this read-mode path.
- The default `subarray` constructor rechecks bounds after argument coercion
  or species lookup shrinks a resizable buffer.

Eight regressions cover Float16/32/64 payloads, ordinary/resizable/shared
storage, copy directions, validation order and valid neighboring cases.
The hardened profile omits shared memory, so its tests exercise the ordinary
and resizable cases; native default tests additionally exercise shared storage.

### Parser early errors and honest Test262 scoring

The old runner accepted arbitrary nonzero exits for parse-negative tests and
returned exit status zero even when the run failed. It could therefore count
crashes or the wrong error type as passing conformance tests.

The runner now requires the CLI's reported error type and a normal language
error exit, with an explicit narrow list for legacy compiler SyntaxError
diagnostics. It returns nonzero for unexpected failures, stale expected
failures, skipped tests and empty selections. `--expected-failures` compares
the exact failure set for the selected run; `--json` records counts, corpus
revision, executable-reported identity and expectation differences.

A probe of all 8,755 negative executions exposed 42 wrong-error failures
(21 files). Parser checks now reject rest-parameter defaults, yield expressions
in arrow parameter initializers, private object literal/destructuring names,
and private accesses through `super` as SyntaxErrors. Five parser regression
groups retain valid neighboring grammar. Eleven Python tests pin the runner.

The new Rust and runner regressions are included in minimum CI; the Rust
regressions also run in the hardened-profile workflow lane.

## Remaining conformance and lifetime work

The corpus used here is Test262 `defaaf1571cd13b183e3f505c6a06e8db316e593`
(27 July 2026), with no tracked corpus edits. Core runs include staging and
exclude the separately measured ECMA-402 directory. Counts are executions,
including required strict/sloppy variants, not file counts.

- `annexB/language/function-code/block-decl-func-skip-arguments.js` encodes
  obsolete ES2017 wording. Current FunctionDeclarationInstantiation adds the
  implicit arguments name to `paramBindings`, not `paramNames`; the legacy
  block function therefore updates the existing binding. Upstream tracks the
  contradictory tests in [Test262 issue #5113](https://github.com/tc39/test262/issues/5113).
- Both modes of `staging/sm/String/internalUsage.js` assume German date
  formatting. Zipp bundles English CLDR data and correctly falls back to it.
  Proper German support requires locale-specific data routing, patterns,
  names, intervals, defaults and service-specific locale availability. No
  one-test formatting override or falsely advertised German locale was added.
- ZA-10's runtime-compiled function/class reclamation remains open. Function
  references currently escape the VM borrow with the program lifetime;
  reclaiming them safely requires an ownership redesign.

## Verification evidence

Logs, executable snapshots, hashes and machine-readable Test262 results are
under `target/audit-20260912/`. The final CLI snapshot is `zipp-after.exe`,
SHA-256 `9e23b84ee922446785b73e9fb015d2b66c19e0cd6e9d80030e4fb3a9d6e2f7ff`.
It reports the parent revision above plus dirty source fingerprint
`de83d9b67244c9cac14d989a8ec30515f49eeeb03fcaf5fc769e939ac5209871`.

The whole-repository formatting check reports pre-existing formatting drift
in 34 files. This audit does not mix those unrelated formatting edits into the
correctness changes. `git diff --check` passes.

The broad native stress sweep was compiled before the final nested-Proxy
patch. The final-build native checks listed below, all 25 new regressions,
the complete final Test262 run and the refreshed WebAssembly/browser checks
include that last patch. Earlier CLI and Test262 snapshots are retained as
`zipp-pre-proxy.exe` and `test262-before-proxy.*` for comparison.

That broad sweep completed successfully: **1,967 passed, zero failed,
22 ignored, 15 filtered** across 237 library/integration/documentation test
summaries. The filtered tests are the existing quarantine manifest entries;
no quarantine was added or widened. It took about 41 minutes on this host,
including long forced-GC probes. `native-full-summary.json` records the counts
and stage; `native-full.log` retains the complete output.

The final CLI passed **95,939 / 95,942 core Test262 executions (99.997%)**,
with the three documented failures and zero skips. The exact-failure gate
passed with no unexpected failures or stale expectations. The repository
manifest now matches this observed three-entry set.

The complete pre-fix CLI run under the corrected scorer recorded **95,897
passed, 45 failed, zero skipped** out of 95,942 executions (99.953%). Forty-two
were the wrong-error failures fixed above. Eight entries in the old expected
manifest were stale: six removed upstream Error tests and two TypedArray tests
whose upstream harness now excludes immutable buffers. These are corpus changes,
not eight additional engine fixes.

Completed focused checks:

- All 42 formerly hidden negative executions pass on the final CLI snapshot.
- Final native build: 594 library and focused integration tests pass;
  two existing library tests are ignored.
- All 25 new Rust regressions pass with `ZIPP_NOJIT=1`,
  `ZIPP_JIT_THRESHOLD=1` and `ZIPP_NO_NURSERY=1` (75 executions).
- Hardened profile: 481 library tests and 25 new regression tests pass;
  one existing library test is ignored.
- Eleven Test262 runner unit tests pass.
- Benchmark tooling: 162 Python tests pass, two skip; all 25 JavaScript
  comparison-harness tests pass. The quarantine manifest validates.
- Fresh hardened WebAssembly build: eight isolated host tests and all 16
  Node boundary checks pass. Worker smoke tests pass 40/40 on each of
  Chromium 153, Firefox 155 and WebKit 26.6 (120 browser checks).
  `wasm-build-metadata.json` records artifact hashes and the explicit dirty
  build label; the profile's SHA is null because it accepts only hexadecimal
  commit IDs. The shipped landing-page module was not replaced by this audit.
