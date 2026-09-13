# Broader CI repair — 14 September 2026

The initial broad run at `11f7a5c0`
([34760541481](https://github.com/f2i-com/zipp-python/actions/runs/34760541481))
failed four lanes. These changes retain the full suites and strict failure gates.

## Native and dependency lanes

- Native workspace linking: limit parallel builds to two and omit duplicated
  debug symbols from test executables. Debug assertions and optimization levels
  remain unchanged. The previous linker terminated with SIGBUS.
- Native meta-operation tests: use the native sandbox's actual 256 MiB interpreter
  thread contract. Unoptimized native frames do not fit the optimized WASM 1 MiB
  budget. The separate production WASM boundary suite still checks that artifact
  with its original linker limits; runtime recursion caps are unchanged.
- Parser dependencies: vendor RustPython parser 0.4.0 with maintained Unicode 17
  tables. All three lockfile audits pass with warnings denied. See
  [fork provenance](../../crates/rustpython-parser-fork/FORK.md).

## Class binding repair

Class methods, accessors and constructors materialized hoisted nested functions
before allocating their captured local bindings. This made `let self = this`
resolve as a missing global inside a nested function and lost writes to captured
`var` bindings. Allocate hoisted vars and captured lexical cells at body entry,
using the same lexical preparation as ordinary functions. Regression coverage
includes destructuring, nested classes, TDZ, const writes and constructor closures.

## Test262 execution and upstream inconsistencies

The corpus remains pinned to `4249661388e5d3f92a85186213da140a6481490f`.
Date/DST cache tests do extensive nested iteration. Twelve workers on a small
hosted runner exceeded the old 20-second process deadline; the gate now uses two
workers and a bounded 120-second timeout. No Date tests are skipped or expected
failures.

The original three-entry manifest omitted eight failures caused by contradictions
in the pinned upstream tests/harness. The corrected manifest names eleven exact
expected execution identities. Every test still runs; unexpected failures,
unexpected passes of these identities and any skips fail the gate.

- `harness/nativeErrors.js` now includes `Error` itself. The three
  `staging/sm/Error/{constructor-proto,prototype-properties,prototype}.js` tests
  nevertheless require every listed constructor to inherit from `Error`, every
  prototype to inherit from `Error.prototype`, or omit `toString` from its own
  properties. These assertions contradict their preceding explicit assertions
  about `Error`. Node 24.19 also fails all three unchanged tests. Six identities
  cover their strict and sloppy executions.
- `testTypedArray.js` adds an immutable-buffer argument factory to the
  same-buffer `slice` test. Its custom species returns a view of that same
  immutable buffer and the test expects successful writes. The
  [immutable ArrayBuffer proposal](https://tc39.es/proposal-immutable-arraybuffer/#sec-typedarrayspeciescreate)
  requires a mutable destination for this write operation. ZIPP correctly throws
  TypeError; changing that would weaken immutability. Two identities cover both
  execution modes. Mutable overlap-copy behavior remains covered independently.
- `staging/sm/String/internalUsage.js` accounts for two executions. It requires
  German date strings, but ZIPP currently advertises only `en`/`en-US` locales
  and resolves `de` to `en`. This is a product limitation, not an Error/TypedArray
  harness contradiction. Node 24.19 passes this test. A local regression checks
  the test's underlying contract: replacing `String.prototype[Symbol.split]`
  cannot affect date formatting or the supported-locale fallback.
- `annexB/language/function-code/block-decl-func-skip-arguments.js` accounts for
  one execution. Its ES2017 expectation that the outer `arguments` object
  survives the block declaration is obsolete; Node 24.19 fails the same final
  assertion. The follow-up investigation also found a real ZIPP defect hidden
  by that expected failure: the block function was not available before its
  textual declaration. The compiler now creates its block binding at block
  entry and updates the outer binding only at declaration evaluation. Sloppy
  arrows receive the same Annex B var setup, and actual parameters (including
  rest/destructured parameters) remain protected. Four regressions cover these
  current semantics independently of the obsolete upstream assertion.

## Hosted result and follow-up

The earlier standard candidate `635b4183` passed the
[standard CI workflow](https://github.com/f2i-com/zipp-python/actions/runs/34761802416)
as confirmed by its checkout logs. The broader run at `0a5c1e2e` contains the
[Test262 job](https://github.com/f2i-com/zipp-python/actions/runs/34761803672/job/103735841023)
ran all 95,680 executions: **95,669 PASS, 11 FAIL, 0 SKIP**, with zero unexpected
failures or stale expectations. The native-workspace lane was still running
when this result was recorded. This is not a 100% conformance result.

The subsequent Annex B correction does not remove the obsolete expectation;
it changes ZIPP's failure from an early TypeError to the same final assertion
as Node. The exact eleven-entry manifest is unchanged. The README now reports
the current corpus/counts and explicitly distinguishes gate success from every
test passing. Neither the pinned corpus nor its tests are edited.

The Test262 log also exposed a provenance issue: checking the external corpus
out into unignored `.test262/` made the CLI identify the source as
`0a5c1e2e+dirty.unknown`. The corpus now lives under ignored `target/test262/`,
and CI requires the built CLI's source to equal the checked-out commit with
`dirty: false` before running conformance. The earlier result above remains
explicitly tied to that earlier run; fresh CI must validate the corrected build.

Local follow-up validation: all five new binding/locale tests pass in default
and safe-sandbox profiles, and all four binding programs produce the same
output in Node 24.19. The complete Annex B shard runs 1,377 executions with
1,376 passes and only the obsolete assertion failing; all 643 arrow-function
executions pass. Both shard gates pass with expectations scoped to their
selection. Related class and direct-eval regressions also pass, as do 554 VM
library tests (two existing opt-in tests remain ignored) and eleven runner tests.

The first follow-up safe-profile CI run completed its selected tests successfully
but then failed because a newly wrapped shell argument line lacked a continuation
backslash. That workflow typo is corrected; the new Intl regression and global
fingerprint test remain in the same Cargo invocation. The README headline also
retains the format consumed by both landing-page statistics adapters.

Do not infer release readiness from this repair note: the candidate must pass
both CI workflows at its final committed revision before promotion or tagging.
# Local follow-up: German DateTimeFormat and separate corrected Test262 reports

The user requested that all subsequent pushes remain on hold until tests pass.
The following work is local and has not been published or released.

The remaining two German `staging/sm/String/internalUsage.js` modes pass against
the original test source after adding service-specific CLDR 47 German date/time
data. `DateTimeFormat.supportedLocalesOf` now advertises `de`/`de-DE`; other Intl
services retain their English locale set. The generated names, calendar data,
patterns, flexible day periods, intervals and localized UTC name have recorded
input hashes and reproduce exactly with the generator and rustfmt. The Unicode
license is retained in `LICENSE-UNICODE`.

The user approved documented corrections for nine contradictory upstream
executions. [Five reviewable patches](../../tools/test262-corrections/README.md)
retain the original assertions where applicable and correct only the conflicting
cases. All five corrected files also run successfully under Node 24.19.0;
ZIPP's focused shards cover 17 executions in each of interpreter-only,
forced-JIT and major-only-GC modes, with no failures. Node's run covers the
factories supported by that Node build; the immutable factory is tested by ZIPP.

The two-suite driver records raw upstream and corrected reports separately,
with explicit corpus labels and executable/patch/report hashes. The corrected
profile cannot claim unmodified Test262 conformance. Its gate allows no expected
failures or skips. The preflight rejects revision drift, unlisted modifications,
incorrect file hashes and newline conversions. A Windows clone needs persistent
`--config core.autocrlf=false`, including during subsequent `git apply` calls.

Validation completed so far:

- Default VM: 554 library tests plus five locale regressions passed; two existing
  opt-in library tests remained ignored.
- Safe interpreter: 489 library tests plus 23 selected regression tests passed;
  one existing opt-in library test remained ignored.
- JavaScript-only and JavaScript+Python `wasm32-unknown-unknown` checks passed.
- Eighteen conformance-tool tests passed, including a real runner invocation
  that verifies both failed reports are retained. Actionlint passed for the
  changed workflows.
- Separate DateTimeFormat ECMA-402 shard: baseline clean `09704737` debug build
  had 454 PASS / 34 FAIL / 0 SKIP; the new release build has 458 PASS / 30 FAIL /
  0 SKIP, with no newly failing identities. Four hour-cycle executions now pass.
  Remaining numbering-system, unsupported-locale and calendar cases are not
  included in the core conformance percentage.

The German-only phase's full core comparison completed: original **95,671 PASS /
9 FAIL / 0 SKIP**, corrected **95,680 PASS / 0 FAIL / 0 SKIP**, both exact gates
passed. Reports are in `target/test262-dual-v2`. Its exact release binary is
`09704737235b2a285ee0fbce0ed5a39e27a8870e+dirty.b5241fb64da12d89b07ded5231273a40d07c3efc2980af4fd76d0fa2c23c8911`,
SHA-256 `25a532c11a1eebe041bb4befb1e638a8558dc35440d72e77934b44f8578e7d8f`.
This is explicitly a working-tree build, not a clean committed release.

# Local follow-up: all pinned DateTimeFormat tests pass

The later DateTimeFormat implementation uses service-specific CLDR 48 tables
for English, German, Japanese, Chinese and Egyptian Arabic. Other Intl services
retain their existing English data. Fixes include calendar-specific patterns,
related years and cyclic year names, Chinese/Dangi leap-month labels, Hebrew
common/leap-month names, Islamic/Coptic/Japanese eras, all four hour cycles,
Arabic digits, and Japanese/Chinese date-style numbering annotations. The data
generator reproduces all five tables from the pinned upstream inputs; headers
retain their hashes and `LICENSE-UNICODE` preserves the upstream license.

The **original, unmodified DateTimeFormat tests pass 488/488 executions, with
zero failures and zero skips**, in both debug and release builds. This is the
244-file shard at the same pinned Test262 revision, not the entire ECMA-402 suite.
It has no test patches or expected-failure allowance. Broad locale coverage,
all Intl services and exact parity with every ICU version remain outside this
claim. For example, time-only intervals spanning distinct dates are not yet
fully aligned with ICU's date-expansion behavior.

The release report is `target/dtf-final-release.json`; debug evidence is
`target/dtf-final-debug.json`. Its engine identity is
`09704737235b2a285ee0fbce0ed5a39e27a8870e+dirty.e4bf953edb8b95219c0c6789d60c1e3beaae34bf360956b04acc30e8387e1bfa`.
Only documentation, comments and runner-check coverage were updated after
freezing this binary; the runtime implementation and locale data values are
unchanged. An earlier shared-target comparison mixed cached baseline VM
artifacts and was discarded; final builds use the isolated `target/dtf-dev`.

Additional local checks:

- Default VM: 554 library tests and nine locale regressions passed; two existing
  opt-in library tests remained ignored.
- Safe interpreter: 489 library tests and 27 selected regressions passed; one
  existing opt-in library test remained ignored.
- Both JavaScript-only and JavaScript+Python WASM target checks passed.
- Twenty conformance-tool tests and actionlint passed. Verified reuse of a
  corrected checkout still rejects changed file hashes, and corpus preflight
  rejects untracked files that could alter the execution set.
- The full 488-execution DateTimeFormat shard also passes in interpreter-only,
  forced-JIT and majors-only-GC modes. The adjacent Locale.getHourCycles shard
  passes all ten original executions.

CI now retains both core reports and requires the original DateTimeFormat shard
to have zero failures/skips. These changes have not been pushed. The previously
dispatched broad CI run at committed `09704737` is separate evidence and does not
validate the uncommitted implementation.

The final release build's full core rerun completed successfully: **original
95,671 PASS / 9 FAIL / 0 SKIP; documented corrected profile 95,680 PASS / 0 FAIL /
0 SKIP**. Both gates pass, with the same binary and pinned corpus selection.
The original failure identities are unchanged. Final preflight also confirmed
that neither corpus contained untracked files after execution.

[The durable evidence summary](2026-09-14-test262-datetimeformat.json) records
the engine identity, original/corrected labels, exact counts and hashes for the
binary, patches and raw reports. Raw local reports are in
`target/test262-dtf-final` and `target/dtf-final-release.json`. Binary SHA-256:
`ffe2ae17e29950a0a295939d7395e2b1014b5f50a1d12db537b2d15cd2ddb833`.

At the last status check, the earlier hosted broad CI run had seven successful
jobs and its native workspace job still running. No release-readiness claim is
made for that incomplete run, and nothing from this follow-up has been pushed.

# Canonical promotion CI and native reference-runtime repair

Draft PR [zipp.org #19](https://github.com/f2i-com/zipp.org/pull/19) promotes
commit `1539eb4b8b50e270c42f7a197d7aab65dcde5949`. Its standard CI passed all
three jobs. The hosted Test262 job independently confirmed original core
95,671 PASS / 9 FAIL / 0 SKIP, corrected core 95,680 PASS / 0 FAIL / 0 SKIP,
and original DateTimeFormat 488 PASS / 0 FAIL / 0 SKIP on that clean commit.

The earlier native workspace run at `09704737` completed after about 86 minutes
with one failing target: `typedarray_interp_index_fast`. Its output matched
apart from the Float16Array row: ZIPP emitted the row while the runner's Node
reference omitted it because Float16Array was unavailable. The broad workflow
had not selected Node 24, unlike standard CI.

The broad native lane now explicitly selects Node 24 and checks its version
and Float16Array support before the expensive build. No runtime behavior or
test assertion was changed. Both tests in `typedarray_interp_index_fast` pass
locally with Node 24.19.0, including interpreter GC-stress comparison, and
actionlint passes. The updated PR head requires fresh CI; merge and tagging
remain gated on those results.
