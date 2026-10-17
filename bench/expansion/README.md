# Expansion suite (diagnostic): where might QuickJS still beat Zipp?

**Status: diagnostic, not canonical.** These workloads look for engine weak
spots the canonical suites do not reach. They live in their own versioned
series (`manifest.json`, schema `zipp-expansion-v1`) and never change the
canonical 13-row geomean, the five micro kernels or the hostile 17.

## What exists, and what this adds

Existing coverage, by the operations each row is dominated by:

| Suite | Row | Dominant operations |
|---|---|---|
| real13 | async-promise-chain | promise reactions, microtasks |
| real13 | class-prototype-hot | class methods through the prototype |
| real13 | json-large | JSON.parse / stringify, one large document |
| real13 | map-set-heavy | Map/Set with string and int keys |
| real13 | markdown-render | string scanning, regex replace |
| real13 | parse-large-js | parsing and compiling a large script |
| real13 | polymorphic-objects, -v2 | 8-way polymorphic property reads |
| real13 | property-ic-shapes | the receiver-count cliff, dictionary and prototype reads |
| real13 | regex-log-scan | regex exec over log lines |
| real13 | sparse-array, -v2 | holes, `in`, for-in, indexOf over sparse arrays |
| real13 | typedarray-math | typed-array arithmetic |
| micro5 | fib-recursive, loop-arithmetic, array-hof, object-properties, sort-comparator | calls, integer loops, map/filter, property writes, comparator sort |
| hostile17 | calls-baseline, calls-closures | stable calls; closures with captured state |
| hostile17 | shapes-stable, shapes-megamorphic | one shape; many shapes |
| hostile17 | types-stable, types-churn | stable types; a value changing type every iteration |
| hostile17 | branch-control, throw-catch | branches; exceptions as control flow |
| hostile17 | allocation-ephemeral, allocation-survival | short-lived objects; survivor cohorts |
| hostile17 | async-burst, async-lived | promise bursts; long-lived chains |
| hostile17 | reactish-reconcile, warm-router | a React-shaped diff; a router |
| hostile17 | bytecode-vm | an interpreter written in JS |
| hostile17 | module-hot-graph, npm-nanoid | live module bindings, re-exports; a vendored npm module |

What the 35 rows leave out, and the 21 workloads added for it (each case in
`manifest.json` lists the operations it `covers` and why it was chosen):

| Category | Case | Gap it fills |
|---|---|---|
| regex | regex-compile-exec | compiling many distinct regexes vs reusing one; captures, named groups, `lastIndex` loops, replace callbacks |
| regex | regex-unicode-sticky | `u` over non-ASCII and astral text, `\p{...}`, sticky tokenizing, lookbehind, split |
| strings | strings-build-slice | `+=` ropes, join, slice/indexOf/split/replaceAll, random access into a large string |
| strings | strings-unicode-scan | surrogate pairs, `for-of` over strings, non-ASCII case mapping, normalize, URI coding |
| json | json-small-many | many small parse/stringify round trips with reviver, replacer, escaping |
| collections | collections-map-mixed | Map/Set with mixed key kinds (NaN, -0, objects) at 8 / 1k / 100k live keys, delete churn, WeakMap |
| numbers | numbers-format-parse | toString(radix), toFixed/toPrecision/toExponential, parse*, Math, BigInt |
| objects | accessors-proxy | getters/setters, Proxy traps, Reflect, frozen objects |
| objects | dict-churn | objects as dictionaries: add/delete, keys/entries/assign, spread, for-in over changing objects |
| classes | class-features | private fields and methods, static blocks, `#x in`, 3-level `super`, `new.target` |
| arrays | mutators | push/pop/shift/unshift/splice, holey arrays, mixed element kinds |
| arrays | sort-variants | default and comparator sorts on numbers, strings and objects; sorted, reversed and duplicate-heavy inputs |
| arrays | dataview-binary | a binary codec: DataView in both endiannesses, Uint8Array set/subarray, CRC32, varints |
| arrays | destructuring-spread | destructuring with defaults and rest, spread calls, map/filter/reduce chains |
| calls | recursion-shapes | mutual recursion, Ackermann, tree build/walk, backtracking, merge sort |
| calls | generators-iterators | generators, `yield*`, custom iterables, spread/Array.from of iterables, early `return()` |
| control | finally-labels | `finally` on every exit, labelled break/continue, string and int `switch`, rethrow |
| gc | old-to-young | a large old heap receiving fresh young objects (the write barrier) |
| gc | large-heap-churn | an LRU cache under steady churn with a survivor cohort; the large size is an extended run |
| async | await-sustained | async call chains and trees, Promise.all/allSettled/race, thenables, caught rejections |
| dynamic | eval-function | `new Function` and indirect eval after warm-up, one large generated function, direct eval |

Deliberately not added: a second recursive-fib, a second promise burst, a
second large-JSON or large-parse row (all already measured). Recursion depth
stays near 700 frames: QuickJS's default stack ends at about 1,550 frames of
simple mutual recursion, and a stack overflow is not what is measured.

## Rules every workload keeps

- One classic script (or module, `goal: "module"`), deterministic: no
  `Math.random`, clock or host API; a seeded generator where data is needed.
- Output: a few `console.log` checksum lines that depend on all the work.
  Every engine's stdout must equal Node's byte for byte (after CRLF to LF);
  a case an engine fails or answers differently is excluded from timing and
  reported, never silently dropped.
- Portable to Node 24, Zipp and Bellard's QuickJS 2026-06-04: ES2022, no
  Intl, no locale-dependent methods, no timers.
- One `const SIZE = <n>;` line scales the work. The committed source is the
  medium size; `sizes.s` (about 1/100 of the work: a cold, mostly startup
  run) and `sizes.l` (5-10x: steady state, and for the GC rows an extended
  run) are substituted into that line by the harness.

## Adding or changing a workload

Each group keeps its cases in `cases-<group>.json`; `build_manifest.py`
merges them into `manifest.json` with every entry's source SHA-256 (and
checks the single `SIZE` line). `--check` exits 1 when the manifest is
stale.

## Running it

```powershell
python bench\comparison\run_native.py --zipp <zipp.exe> --quickjs <qjs.exe> `
  --suite expansion --sizes s,m,l --reps 15 --engines node,zipp-interp,quickjs `
  --skip-unsupported --json target\bench-results\expansion.json
python bench\comparison\expansion_report.py target\bench-results\expansion.json
```

`--suite hostile17` runs the hostile manifest the same way (module entries
through `zipp mjs`). The report gives, per row, the median ratio of
startup-adjusted process cycles (Zipp ÷ QuickJS), its 95% bootstrap interval
and a verdict (an interval that contains 1 is AMBIGUOUS, neither a win nor a
loss), the cold-time, startup and peak-working-set ratios, the ordinary and
the category-balanced geomean, and every excluded case with its reason.
