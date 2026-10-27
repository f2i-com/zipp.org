# Correctness and language coverage

The unmodified pinned core Test262 corpus passes
**99.999% of test262**: 96,227 / 96,228 executions, with one documented expected
failure and zero skips. The original DateTimeFormat ECMA-402 shard also passes
**490 / 490**, without changes or skips. The corpus is pinned to
`0b4e0812022b18236b5276d2804948fdd9f71603` (26 October 2026), including staging
and excluding the separate ECMA-402 suite; hosted CI runs it once, unmodified.

| Validation profile | Passed | Failed | Skipped |
| --- | ---: | ---: | ---: |
| Original pinned core Test262 | 96,227 | 1 | 0 |
| Original DateTimeFormat ECMA-402 tests | 490 | 0 | 0 |

The one failure, `annexB/language/function-code/block-decl-func-skip-arguments.js`
in sloppy mode, asserts ES2017 wording that ES2018 deleted
([tc39/test262#5113](https://github.com/tc39/test262/issues/5113)); current Node
fails it too, and local regression tests cover the current semantics. It is the
only entry in the [expected-failure manifest](../tools/test262-expected-failures.txt):
the gate fails on any other failure, on an entry that starts passing, and on a
skip. The previous pin needed five test corrections and a second, patched run;
upstream has since fixed the Error-harness and immutable-buffer
inconsistencies four of them addressed, so that profile is retired. The German
formatting failures are fixed in the VM without test changes. Passing the
DateTimeFormat shard does not establish complete ECMA-402 coverage or universal
ICU parity.

Moving the pin found 236 executions the engine failed: the new stage 4
`Iterator.prototype.chunks`, `windows` and `includes`, the `take`/`drop` limit
bound, `Promise.try`'s unwrapped result and the test host's module-source
prototype. All are fixed; the iterator and Promise.try regression tests run in
every tier and under GC stress.

The 26 October 2026 pin (`0b4e0812`, one commit later) expects a DataView's
`byteLength` and `byteOffset` to read 0 once its buffer is detached or shrinks
beneath it, as a typed array's do (tc39/ecma262#3972, which has consensus).
The engine threw a `TypeError` there, failing 12 executions; it now answers
0, and the old pin's two detached-buffer tests, merged into one, leave the
corpus two executions smaller.

The [12 September 2026 correctness audit](archive/audits/2026-09-12-correctness.md)
recorded an earlier **95,939 / 95,942** result against corpus `defaaf1571`.
That older corpus has a different execution count. The audit tightened negative-test scoring and fixed 42
executions previously counted as passes despite reporting the wrong error type.
The runner's `--expected-failures` option now rejects unexpected failures, stale
expectations and skips; `--json` records the engine and corpus identities.
The former errored-module-cycle and deferred top-level-await failures are fixed.

The [deep follow-up audit](archive/audits/2026-09-12-follow-up.md) then exercises
re-entrant buffer operations, strict numeric coercion, Promise capabilities,
temporary GC roots, live collection iteration, weak references/finalization,
compiler metadata boundaries and hostile browser-host exceptions. Its focused
regressions run in both the ordinary and safe-sandbox profiles, and its weak
reference/finalization Test262 shards pass 603 of 603 executions.

The standing correctness strategy compares default JIT, interpreter-only,
forced-JIT, and majors-only-GC modes. A tier-differential fuzzer also generates
self-checking programs and compares Node, the interpreter, and tier-forcing
switches; benchmarks alone are never treated as proof of language correctness.

ES2015–ES2025 is essentially complete, including:

- classes, private elements, static blocks, and all eight decorator kinds;
- generators, async generators, promises, iterator helpers, and explicit
  resource management;
- 12 TypedArray kinds including `Float16Array`, `DataView`, shared memory, and
  atomics;
- `BigInt`, `Proxy`/`Reflect`, modern regular expressions, and `Temporal` with
  fifteen calendars;
- ES modules, dynamic/typed/deferred/source-phase imports, and top-level await;
- `eval`, `Function`, `ShadowRealm`, and browser-oriented embedding APIs (the
  WASM host SDK structured-clones data across its Worker boundary; guest code
  has no `structuredClone` global).

DateTimeFormat ships CLDR 48 data for `en`/`en-US`, `de`/`de-DE`, `ja`/`ja-JP`,
`zh`/`zh-CN` and `ar-EG`. Other Intl services
currently ship English locale data only. The detailed support notes and durable
architecture reference live in [`docs/REFERENCE.md`](REFERENCE.md).
