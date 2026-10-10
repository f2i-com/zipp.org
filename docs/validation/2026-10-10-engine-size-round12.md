# WebAssembly interpreter speed, string and RegExp cliffs, and engine size, twelfth pass - 2026-10-10

Continues `codex/engine-size-reduction` from `ea517745` (the eleventh pass's
note) to `c913b447`. Nothing is pushed and CI has not run. The user accepted
the eleventh-pass modules on 10 October (`d5ab3f95`); they are the modules
every WebAssembly figure here is measured against, and "accepted" below means
them. The user accepted this pass's modules later the same day, with the
four flagged rows and Lite's string-build cost in front of them; they are
the packages the next pass is measured against. Native figures are from an
ordinary release build
(`cargo build --release -p zipp-cli`, default features, not profile-guided),
so the published table is unchanged.

## Outcome

- **WebAssembly speed.** Against the accepted modules, one process a
  case, four runs: the JavaScript module's corpus takes 0.90 of the time,
  Python-base 0.90 to 0.91, Python + Torch 0.91 to 0.92 and Lite 0.97 to 0.98, each
  without the one row that is a bug fixed rather than a speed
  ([the screen](#webassembly-speed-screen)). Integer loops take 0.74 to
  0.81 in the full modules, property reads 0.72 to 0.80, the regex row 0.43
  to 0.44. By the ninth pass's rule one row is flagged in the JavaScript
  module (promise-resolve, 4% to 10% slower), one in Python + Torch
  (error-typedarray, 5% to 10%), two in Lite (string-build, 1% to 15%, and
  json) and none in Python-base, where promise-resolve is 5% to 10% faster.
  Lite's string-build is a cost of this pass. The other three were not
  profiled in their own modules, so whether they are costs or layout is
  not known.
- **Cliffs.** Four workloads whose cost in the WebAssembly modules grew with
  the square of a string's length now grow with its length
  ([the table](#string-and-regexp-cliffs)): a script holding 300 KB of
  non-ASCII text took 5 s to tokenize and takes 3 ms; a sticky-regex
  tokenizer over 190 KB took 18 s and takes 164 ms.
- **A script that built and dropped large RegExps died.** A loop creating
  `/\p{Letter}+/u`-sized patterns and dropping them ended with "script
  exceeded its memory budget" at 460 MB of uncollected programs, in the
  accepted modules too. A compiled program now counts toward the
  collection trigger.
- **WebAssembly size.** The three full modules are 79,473 to 79,918 bytes
  smaller raw (0.9% to 1.6%) and 10,430 to 11,637 smaller after Brotli (0.5%
  to 0.8%), from sharing the regex matcher bodies Lite already shared. Lite
  is 675 bytes larger raw and 899 smaller after Brotli.
- **Native.** Unchanged, as it should be for changes to the
  interpreter and the checked profile: the 13 rows take 0.31% more time
  than at `9df8019c`, interval [-0.16%, +0.77%], and the headline ten are at
  0.976x of Node 24 [0.963, 0.987].
- **Python.** Unchanged natively: 1.80x of CPython 3.13 on in-script time,
  1.00x of the previous build. In the WebAssembly modules the three
  `python-*-e2e` rows take 0.89 to 0.97 of the time and a Python engine's
  start 0.88 to 0.92.
- **Correctness.** Test262 passes with the expected-failure manifest
  (95,671 of 95,680 executions, nothing unexpected, stale or skipped). The
  default-feature `zipp-vm` suite passes: 2,182 tests in 308 binaries. The
  same suite built in the `safe-sandbox` profile, where most of this pass
  lives, passes 1,681 and fails one test that failed before the pass; run
  first at `13b56e42`, it caught a metering hole that every default gate
  passed ([below](#what-the-sandbox-suite-caught)). The Node boundary suite
  passes 35/35 on three modules and 33/35 on Python-base, whose two failures
  are the existing Torch-dependent checks.

## What a profile of the modules showed

The earlier passes measured these modules row by row, and their notes hold no
profile of one (the sixth profiled the Promise rows without a finding). This
pass built each module with its function names kept (`wasm-bindgen
--keep-debug`, then the shipping `wasm-opt -O1 -O1` with `-g`) and sampled
each corpus case under Node's optimizing compiler, starting the profiler
after the case had warmed up. Started with the process (`--cpu-prof`), the
profile charges TurboFan's one compile of each function to that function, and
`run_loop` reads as 15% of every case.

In the accepted JavaScript module, by self time:

| Case | Where the time was |
| --- | --- |
| regex (`s.match(/\d+/g)` over 23 KB) | 55% in `core::str::from_utf8`, called by `JsStr::as_str_wf` for each exec |
| regexp-unicode-compile | 28% in the heap audit, 22% sorting the compiled programs, 14% sizing them: the audit ran twice a pattern |
| arith-int | 14.7% in `add_values` and 6.3% in `cmp_lt_values`, each called for two integers |
| prop-mono | 7% in `ic_recv_map`, 10% once the two above were gone |
| string-build, string-append-index | 3% in `String::clone` under `resolve_const`, for a one-character literal |
| call-deep | 80% in the heap audit, under the poll each 7-microsecond call reaches |
| every case | 2% to 3% in the audit walk, which a heap of the realm's size takes at every poll |

The first five are changed below. The last two are the same walk and are left
alone ([open](#not-claimed)).

## The changes

Each is one commit; the effect is the JavaScript module's unless it says
otherwise.

- **A well-formed string holds its bytes as text** (`4e8cb084`). The
  `safe-sandbox` profile forbids `from_utf8_unchecked`, so a `&str` view went
  through the checked conversion, which validates the whole buffer. A
  well-formed string is now a `String` in that profile and only one with a
  lone surrogate is plain bytes; appends and slices compose what the operands
  already prove (`push_js`, an ASCII slice of an ASCII string) instead of
  scanning. The first form of this made the string rows 11% slower, by a
  check on each appended piece; the appends were then rewritten.
- **The lexer decodes one character, not the rest of the source**
  (`213dc92a`). The same checked conversion, handed everything from a
  non-ASCII character to the end of the script.
- **Two Numbers are added and compared in the dispatch loop** (`91be9445`),
  and **an inline cache's receiver is checked in the caller** (`e5c94927`).
- **A one-character literal names a pinned string** (`12919d1f`).
- **A compiled RegExp program is admitted on the O(1) estimate when it fits**
  (`5376061b`). The programs admitted since the latest walk are summed beside
  the estimate, kept through one further walk, and only decide that a walk
  is due; the walk still gives the verdict.
- **A compiled program counts toward the collection trigger** (`76420e1f`).
- **A rope is flattened before an exec** (`58735a8a`, `2379accf`) and **an
  exec subject's UTF-16 units are kept between execs** (`78493a25`,
  `6beea69a`), both under the RegExp working allowance, in the profile that
  did neither.
- **The host preamble is embedded without its comment lines** (`376f020b`):
  5,661 of 11,453 bytes that every `initScript` lexed.
- **The full modules share the UTF-16 and UCS-2 matcher bodies**
  (`13b56e42`), as Lite has since the fourth pass.

## String and RegExp cliffs

`cliff_probe.cjs` (scratch): a first engine compiles the module's functions,
then each workload runs once in a fresh engine. Milliseconds; Node is the
same JavaScript run directly.

| Workload | JavaScript, accepted | JavaScript, this pass | Lite, accepted | Lite, this pass | Node |
| --- | ---: | ---: | ---: | ---: | ---: |
| Load a script holding one 300 KB literal of CJK text | 5,302 | 3.1 | 4,720 | 4.3 | not applicable |
| Sticky-regex tokenizer over 191,650 units built by `+=`: 108,000 execs | 17,832 | 164 | 56,727 | 198 | 4.7 |
| `indexOf` from a moving position over 228,890 units: 24,000 calls | 202 | 29.6 | 208 | 33.3 | 2.1 |
| `while (re.exec(text))` over 280,000 units with accents: 40,000 execs | 32,605 | 33.1 | 65,280 | 53.0 | 5.0 |

The four have one shape: an operation that was O(1) or O(match) in the
unchecked profile did something O(string) in the checked one, so a loop of
them was quadratic.

| Cliff | What was O(string) | Commit |
| --- | --- | --- |
| Tokenize | the checked decode of one character validated the rest of the source | `213dc92a` |
| `indexOf` from a moving position; any `&str` view | the view validated the whole string | `4e8cb084` |
| Exec over a string built by `+=` | a rope was never flattened; each exec walked it into a fresh UTF-16 copy | `58735a8a` |
| Exec over text that is not ASCII | each exec encoded the subject to UTF-16 | `78493a25` |

Two limits remain, both by choice. A rope is flattened only when twelve bytes
a unit fit the 16 MiB working allowance (1.4 million units): four for the
string's buffer and eight for the stack the flattening walk keeps, which a
left-leaning rope of one unit a level can need. A subject's units are kept
only up to 1,048,576 units, and never across a return to the host. Past
either limit an exec costs what it did.

## A RegExp program was invisible to the collector

Collections are scheduled on the count of allocations and on the bytes of
large payloads. A RegExp object's compiled program is outside its payload, so
a dropped `/\p{Letter}+/u` (a 23 KiB program and an ASCII twin) weighed
nothing. `regex_leak.cjs` (scratch), heap figure read after each fifth of the
calls:

| Loop | JavaScript, accepted | JavaScript, this pass |
| --- | --- | --- |
| unicode RegExp, 600 calls of 100 | 264MiB FAILED: script exceeded its memory budget | 5MiB 1MiB 5MiB 1MiB 5MiB |
| unicode RegExp, 3 calls of 20000 | 442MiB FAILED: script exceeded its memory budget | 2MiB 4MiB 5MiB |
| plain RegExp, 600 calls of 1000 | 18MiB 15MiB 13MiB 17MiB 15MiB | 18MiB 15MiB 13MiB 17MiB 15MiB |
| regex literal, 600 calls of 1000 | 12MiB 12MiB 12MiB 12MiB 12MiB | 12MiB 12MiB 12MiB 12MiB 12MiB |
| objects, 600 calls of 1000 | 14MiB 14MiB 14MiB 14MiB 14MiB | 14MiB 14MiB 14MiB 14MiB 14MiB |

The accepted Lite module fails the same two loops at the same 264 and
442 MiB, and this pass's Lite reads as this pass's JavaScript module does,
to within 1 MiB.

The regexp-unicode-compile row of the screen is this bug. Without the
collection trigger its time per call grows for as long as the run lasts
(10.6 ms a call over the first 20 calls and 40.0 over calls 181 to 200, in a
build of this pass before the trigger; the accepted module, which also
audited every program, read 55 ms a call over its first 28), so a ratio for
that row measures the length of the harness, not a speed. It is reported apart from the geomean below.

## WebAssembly size

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125 (`-O1 -O1`), Node 24.19.0.
Brotli is quality 11 with the size hint, as the README states it. Module
bytes, raw / Brotli.

| Variant | Accepted | This pass | Raw | Brotli | Before post-processing |
| --- | ---: | ---: | ---: | ---: | ---: |
| Lite | 2,654,515 / 716,795 | 2,655,190 / 715,896 | +675 (+0.03%) | -899 (-0.13%) | 2,768,128 / 717,330 |
| JavaScript | 4,977,978 / 1,239,416 | 4,898,060 / 1,228,986 | -79,918 (-1.61%) | -10,430 (-0.84%) | 5,059,943 / 1,234,620 |
| Python-base | 6,644,132 / 1,612,708 | 6,564,659 / 1,601,071 | -79,473 (-1.20%) | -11,637 (-0.72%) | 6,759,818 / 1,609,261 |
| Python + Torch | 8,568,374 / 1,963,702 | 8,488,699 / 1,952,907 | -79,675 (-0.93%) | -10,795 (-0.55%) | 8,688,206 / 1,959,541 |

The saving in the full modules is the shared matcher bodies: the JavaScript
module built one commit before them was 4,982,935 / 1,239,247, which is 4,957
bytes larger raw than accepted and 169 smaller after Brotli. Lite has had
the shared bodies since the fourth pass, and is where the rest of this pass
shows: more code for the string type's two forms and the inlined guards,
5,661 bytes less preamble.

`bench-regex-modes.cjs`, 25 cases, this pass over accepted, each order:

- JavaScript: geomean 0.819 and 0.821 of the time. Fastest: unicode-bmp-class 0.60, ucs2-bmp-class 0.61, unicode-property 0.69, unicode-casefold 0.70, ucs2-surrogate-units 0.73. Slowest: ascii-short 0.97 and 1.00, ascii-empty-loop 0.96 and 0.98, zero-max-loop 0.94 and 0.96. No case is slower in both orders by more than 2.5%.
- Lite: geomean 0.898 and 0.880 of the time. Fastest: unicode-bmp-class 0.73, ucs2-bmp-any 0.74, ucs2-bmp-class 0.75, unicode-property 0.78, unicode-casefold 0.79. Slowest: ucs2-lookbehind 1.00 and 1.05, ascii-short 1.04 and 1.02, ascii-class 1.04 and 1.00. No case is slower in both orders by more than 2.5%.

## WebAssembly speed screen

`node --no-liftoff`, one pinned processor, one process per case, both load
orders on two processors; ratios are this pass over accepted, medians of 31
batches. A case is flagged when it is more than 2.5% slower in three of the
four runs, the ninth pass's rule.

| Module | Geomean, four runs | Without regexp-unicode-compile | Flagged slower | Its four readings |
| --- | --- | --- | --- | --- |
| Lite | 0.844, 0.833, 0.833, 0.838 | 0.983, 0.970, 0.973, 0.977 | json | 1.048, 0.944, 1.050, 1.033 |
|  |  |  | string-build | 1.014, 1.151, 1.050, 1.100 |
| JavaScript | 0.746, 0.740, 0.739, 0.740 | 0.901, 0.902, 0.899, 0.901 | promise-resolve | 1.052, 1.045, 1.039, 1.096 |
| Python-base | 0.775, 0.770, 0.771, 0.764 | 0.913, 0.906, 0.907, 0.899 | none | |
| Python + Torch | 0.771, 0.777, 0.776, 0.776 | 0.907, 0.915, 0.906, 0.914 | error-typedarray | 1.088, 1.076, 1.100, 1.048 |

- Lite, faster in three of four runs: arith-mod 0.94 to 0.97, call-deep 0.97 to 1.01, prop-mono 0.89 to 1.18, regex 0.60 to 0.63.
- JavaScript, faster in three of four runs: alloc-object 0.83 to 0.89, arith-float 0.83 to 0.92, arith-int 0.76 to 0.81, arith-mod 0.78 to 0.84, array-hof 0.92 to 0.95, enumeration 0.94 to 0.97, prop-mono 0.72 to 0.78, prop-poly 0.79 to 0.86, property-delete 0.91 to 0.92, recursive-calls 0.91 to 0.95, regex 0.43 to 0.44, string-append-index 0.90 to 0.91.
- Python-base, faster in three of four runs: alloc-object 0.89 to 0.98, arith-float 0.87 to 1.06, arith-int 0.74 to 0.80, arith-mod 0.84 to 0.88, array-build 0.84 to 0.95, array-hof 0.93 to 0.99, enumeration 0.94 to 0.99, promise-resolve 0.90 to 0.95, prop-mono 0.75 to 0.80, prop-poly 0.82 to 0.90, property-delete 0.93 to 0.97, python-arithmetic-e2e 0.93 to 0.94, python-dictionary-e2e 0.94 to 0.97, python-list-e2e 0.91 to 0.92, python_init_1_functions 0.88 to 0.90, python_init_200_functions 0.90 to 0.91, recursive-calls 0.91 to 0.95, regex 0.43 to 0.44, string-append-index 0.88 to 0.93.
- Python + Torch, faster in three of four runs: alloc-object 0.89 to 0.92, arith-float 0.86 to 0.88, arith-int 0.77 to 0.80, arith-mod 0.82 to 0.84, array-hof 0.94 to 0.96, error-range 0.97 to 0.97, prop-mono 0.74 to 0.80, prop-poly 0.81 to 0.89, property-delete 0.89 to 0.92, python-arithmetic-e2e 0.93 to 0.94, python-list-e2e 0.89 to 0.91, python_init_1_functions 0.90 to 0.91, python_init_200_functions 0.91 to 0.92, recursive-calls 0.93 to 0.97, regex 0.44 to 0.44, string-append-index 0.90 to 1.01.

The flagged rows, four readings each (median of batches; the trimmed mean
agrees on all of them):

- **Lite string-build** (1.014, 1.151, 1.050, 1.100) is real. The row is
  `s += "ab" + i`. A text-backed string can only take the digits as text, so
  the checked profile validates them, where the byte vector it replaced
  copied them. In the full modules that is an inlined loop and the row is
  0% to 2% faster. In Lite, built for size, it was a call a digit;
  `c913b447` made it one check of the slice, and the screen does not show
  that as a gain: the module before that commit read 1.059, 1.088, 1.041
  and 1.071. Only the profiling builds, which keep function names and are
  not the shipped module, run the row level (6.09 ms against 6.14).
- **Lite json** (1.049, 0.944, 1.050, 1.033) and **JavaScript
  promise-resolve** (1.052, 1.045, 1.039, 1.096) were not profiled in the
  final modules. The same promise-resolve source reads 0.948, 0.954, 0.916,
  0.899 in Python-base and 0.974, 0.995, 0.992, 1.002 in Python + Torch,
  which is how the tenth and eleventh passes saw that row move with layout;
  that is a likeness, not a finding.
- **Python + Torch error-typedarray** (1.088, 1.076, 1.100, 1.048) reads
  1.022, 1.023, 1.061, 1.034 in Python-base, 1.057, 1.022, 1.003, 1.001 in
  JavaScript and 0.990, 0.989, 1.005, 1.004 in Lite. It was profiled only in
  an earlier JavaScript build of this pass, where it took 0.529 ms against
  0.525; the Python + Torch module was not.

regexp-unicode-compile reads 0.008 to 0.010 in the full modules and 0.024
to 0.025 in Lite, for the reason given above; the geomeans in the second
column leave it out. Without the regex row as well they are 0.92 to 0.94
for the three full modules and 0.99 to 1.00 for Lite.

## Native measurements

`tools/bench.py`, cold metric, ordinary release builds, one pinned processor.

- `9df8019c` to `c913b447`, 12 paired repetitions: geomean +0.31%
  [-0.16%, +0.77%] over the 13 rows; start 6.5 and 6.4 ms. One row's
  interval excludes zero: regex-log-scan +2.5% [+1.3%, +5.3%] (448 to
  468 ms). This pass is on that row's path in the unchecked profile too:
  the string type's constructors, its ASCII slice and its appends
  (`4e8cb084`; the row takes about 1.8 million ASCII slices) and the
  inlined `+`, `<` and cache guard in the interpreter. The eleventh pass
  measured single native rows moving by more than this between two links
  of one commit, so the row may be either; it was not isolated. The
  others: markdown-render +1.2%
  [+0.0%, +2.7%], map-set-heavy +1.2% [-1.9%, +2.9%], polymorphic-objects-v2
  -2.6% [-4.4%, +1.6%], the rest within 0.7%.
- `c913b447` against Node 24.19.0, 10 paired repetitions: headline ten
  0.976x [0.963, 0.987], all 13 rows 0.679x [0.672, 0.688]. Behind Node:
  async-promise-chain 1.38x [1.33, 1.41], polymorphic-objects 1.26x
  [1.22, 1.27], json-large 1.17x [1.09, 1.19], map-set-heavy 1.04x
  [0.95, 1.23].

This pass did not look for native speed.

## Python suite

`tools/python_bench.py`, 7 repetitions, `9df8019c` and `c913b447` against
CPython 3.13: in-script time 1.80x of CPython on the 36 programs both run
(micro 1.62x, macro 2.97x), 1.00x of the previous build on all 44; process
overhead 21.0 ms. The pass did not touch the Python frontend.

## Correctness

All on `c913b447` with a clean tree, run one after another with nothing else
building:

- Test262, the full corpus with `tools/test262-expected-failures.txt`: 95,671
  of 95,680 executions pass, the nine failures are the manifest's, and there
  is no unexpected failure, stale expectation or skip. The local corpus is an
  exported tree under `target/`, so the runner prints the engine repository's
  commit as the corpus revision; the manifest pins corpus `42496613`.
- `cargo test -p zipp-vm --no-fail-fast` with the `tools/ci_quarantine.py`
  skips: 2,182 passed, 0 failed, 22 ignored, in 308 test binaries.
- The same with `--no-default-features --features safe-sandbox`: 1,681
  passed, 19 ignored, and `audit_20260915_props::props_modes_match_node`
  failed (its child process reaches "Maximum call stack size exceeded" in a
  debug build of this profile). That test fails identically at `9df8019c`.
- The same with `--no-default-features --features wasm-lite`: 1,648 passed
  and 45 failed in 22 binaries. The same 45 fail at `9df8019c`,
  where 1,642 pass; the six more are this pass's new tests. Most are tests of
  Intl, Temporal and module loading, which Lite's feature set omits. The
  suite is not written for that profile, and why each of the others fails
  there was not looked into; it is run here as a second build of the
  checked code paths, and it shows no test this pass broke.
- `cargo test -p zipp-cli`: 80 passed, 1 ignored.
- `cargo test -p zipp-vm --features python --lib`: 620 passed, 5 ignored.
- `cargo test -p zipp-regress`: 108 passed, 1 ignored; with
  `utf16 compact-utf16 bounded-backtracking`, the matcher the full modules
  now share: 124 passed, 1 ignored.
- `zipp-vm` checks with warnings denied in the default, `safe-sandbox`,
  `python`, `wasm-lite` and `safe-sandbox` + `wasm-compact-regex` profiles;
  the sandbox workspace's check against its lockfile and its 8 tests; the
  11 unit tests of `zipp-wasm`, which hold constants to the embedded
  preamble.
- The Node boundary suite on the exact modules of the size table: JavaScript
  35/35, Lite 35/35, Python + Torch 35/35, Python-base 33/35. The two
  Python-base failures are `audit-2026-09-15-ml-transport.cjs` and
  `python-gpu.cjs`, which need the optional Torch package, as in every pass
  since the ninth.

New tests: `tests/lexer_non_ascii.rs` (characters of every width beside each
other; 1.8 MB of three-byte characters in linear time), a unit test of the
string type's two forms through every append, slice and concatenation, two
tests in `regex_exec_heap_estimate.rs` (exec loops over a rope and over text
that is not ASCII; a kept subject buffer against a second subject, an
appended one and one too large to keep) and one in
`regex_memory_limits.rs` (8,000 dropped programs under a 48 MiB allowance).

### What the sandbox suite caught

Most of this pass is code only the `safe-sandbox` profile compiles, and
neither Test262 (run on the native CLI) nor the default-feature suite builds
it. Run whole in that profile at `13b56e42`, the suite failed
`regex_execution_limits.rs::left_deep_rope_scratch_is_bounded_and_released`:
the rope flattening of `58735a8a` asked only for the string's bytes, and
`Heap::flatten` also keeps an unmetered stack of pending children, which
that test's 8,192-level rope requires to be charged. `2379accf` asks for the
walk's worst case as well, and the modules and every figure here were rebuilt
and measured after it.

The same run fails `audit_20260915_props.rs::props_modes_match_node` ("Maximum
call stack size exceeded" in a debug build of the checked profile). It fails
identically at `9df8019c`, before this pass, and is not addressed here.

## Not claimed

- CI has not run. Nothing was measured on Linux, macOS or in a browser, and
  no profile-guided capture was taken.
- Test262 runs on the native CLI. The checked profile's string, lexer and
  RegExp paths are covered by the two feature suites and the Node boundary
  suite, not by Test262.
- The audit walk at each poll: 2% to 3% of every row and 80% of call-deep.
  A heap of the realm's size is walked at every poll by design
  (`heap_walk_stride_for`), because growth in place inside one object is the
  whole of what a small heap can hide. Widening that stride is a sandbox
  trade-off this pass did not make.
- The Python modules clone the runtime seed, 2,300 function prototypes, for
  each program: 10% to 12% of each `python-*-e2e` row
  (`<FuncProto as Clone>::clone` under `runtime_seed`). Sharing it means
  putting the prototypes' code behind a shared pointer across the compiler.
- Lite runs an integer loop three times slower than the JavaScript module
  (arith-int 9.2 ms against 3.1 ms in the accepted modules), and that row
  reads 0.90 to 1.00 here where the full modules read 0.74 to 0.81. Lite
  is built with `opt-level = "z"`, a different feature set and its
  experiment switches folded to constants; which of those holds the row
  back was not measured.
- Why promise-resolve in the JavaScript module, json in Lite and
  error-typedarray in Python + Torch read slower. None was profiled in its
  final module.
- A subject past 1,048,576 units that is not ASCII, and a rope past
  1.4 million units, still cost their length on every exec.
