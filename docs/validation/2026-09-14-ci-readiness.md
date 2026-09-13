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

At `0a5c1e2e`, the
[standard CI workflow](https://github.com/f2i-com/zipp-python/actions/runs/34761802416)
passed. The
[Test262 job](https://github.com/f2i-com/zipp-python/actions/runs/34761803672/job/103735841023)
ran all 95,680 executions: **95,669 PASS, 11 FAIL, 0 SKIP**, with zero unexpected
failures or stale expectations. The native-workspace lane was still running
when this result was recorded. This is not a 100% conformance result.

The subsequent Annex B correction does not remove the obsolete expectation;
it changes ZIPP's failure from an early TypeError to the same final assertion
as Node. The exact eleven-entry manifest is unchanged. The README now reports
the current corpus/counts and explicitly distinguishes gate success from every
test passing. Neither the pinned corpus nor its tests are edited.

Local follow-up validation: all five new binding/locale tests pass in default
and safe-sandbox profiles, and all four binding programs produce the same
output in Node 24.19. The complete Annex B shard runs 1,377 executions with
1,376 passes and only the obsolete assertion failing; all 643 arrow-function
executions pass. Both shard gates pass with expectations scoped to their
selection. Related class and direct-eval regressions also pass, as do 554 VM
library tests (two existing opt-in tests remain ignored) and eleven runner tests.

Do not infer release readiness from this repair note: the candidate must pass
both CI workflows at its final committed revision before promotion or tagging.
