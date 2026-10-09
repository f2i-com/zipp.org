# Python start, compact Unicode tables and engine size, eleventh pass - 2026-10-09

Continues `codex/engine-size-reduction` from `baf077c3` (the tenth pass) to
`9df8019c`. Nothing is pushed and CI has not run. Every native figure is from
an ordinary release build (`cargo build --release -p zipp-cli`, default
features, not profile-guided), so the published table is unchanged.

## Outcome

- **WebAssembly size.** The four modules are 1.9% to 5.5% smaller raw and
  1.6% to 4.4% smaller after Brotli than the tenth pass's, and 3.2% to 6.1% /
  2.2% to 4.9% smaller than the accepted ninth-pass packages. Most of it is
  two tables: Unicode normalization, which was the largest block of
  compressed data in every module, and the regex properties of strings.
- **WebAssembly speed.** Every module is 1% to 4% faster in geomean than
  the accepted packages. By the ninth pass's rule only the JavaScript module
  passes: one row is flagged in Lite, two in Python-base (promise-resolve 8%
  to 11% slower) and two in Python + Torch, and run alone the JavaScript
  module has a string loop 11% slower. The code on those rows' paths is
  unchanged; they move with whatever else was moved, differently in each
  module ([the screen](#webassembly-speed-screen)). The modules are reported
  as measured, not as accepted packages.
- **Python start.** A script run from inside a large folder no longer reads
  the folder first: the dict_get benchmark run from this repository's root
  takes 62 ms instead of 460. The code cache no longer
  grows without bound, and no longer misses when a run is profiled.
- **Process exit.** `zipp js` and `zipp py` no longer free the heap object by
  object before exiting. The 13 native rows take 1.42% less time [0.62, 1.95];
  the headline ten are at 0.972x of Node 24 [0.958, 0.980], with three rows
  still behind (async-promise-chain 1.42x, polymorphic-objects 1.28x,
  json-large 1.15x).
- **Python in-script time is unchanged**: 1.81x of CPython 3.13, 0.99x of the
  tenth pass's build.
- **A tenth-pass finding is withdrawn.** The generator_iter slowdown it
  located at `436116b6` is link layout, not the source
  ([below](#link-layout)).
- **Correctness.** Test262 passes with the expected-failure manifest
  (95,671 of 95,680 executions, nothing unexpected, stale or skipped). The
  full `zipp-vm` suite passes: 2,179 tests in 307 binaries. The Node boundary
  suite passes 35/35 on three modules and 33/35 on Python-base, whose two
  failures are the existing Torch-dependent checks.

## Link layout

The tenth pass reported Python generator_iter as 11% slower from `436116b6`
and could not explain it: that commit's switch changed nothing, and the
compiled loop had simply stopped beating the interpreter. The cause is not in
the source. `c48fc4d2` linked a second time, with a linker map requested and
nothing else changed, gives a binary 8.7 KB larger that runs the row at the
old speed, and runs another row far slower:

| Row | `c48fc4d2` as measured | The same commit, linked again | `ZIPP_PY_JIT=0`, either |
| --- | ---: | ---: | ---: |
| generator_iter | 67.7 ms | 59.8 ms | 67 ms |
| dict_get | 16.9 ms | 24.0 ms | 25.4 ms |
| math_sqrt | 33.0 ms | 30.0 ms | |
| word_count | 35.9 ms | 32.7 ms | |

Medians of 5 on one pinned processor. Both binaries log the same compiled
region for each row. In one layout the compiled loop saves a third of
dict_get and in the other almost nothing, and the two rows disagree about
which layout is the good one. Across the 36 rows that CPython also runs the
two links differ by 0.4% in geomean, and a second pair (`131aa66c`, linked
twice) differs by under 3% on every row.

So a step on one Python row between two builds is not evidence about the
commit between them, and no Python figure of this pass is read that way: a
change is judged on one binary with a switch, or on the geomean. This pass's
final binary happens to land on the fast side of both rows (16.2 ms and
59.3 ms). What makes a compiled region's helper calls cost 25 ns more in one
layout was not found.

## Python: before the first line and after the last

| Commit | Change |
| --- | --- |
| `ee34f2cb` | `zipp py script.py` loads the folder it is run from as the program's virtual filesystem. Every file became a base64 string constant that the runtime decoded at start: up to 64 MiB encoded, compiled and decoded before the first line. The bytes now stay with the compiled program and the runtime asks for a file's through a hidden native, `__zipp_py_vfs_read`, when the program first reads it. The command line goes further for everything but Python sources, which it still reads at the start because any may be imported: a data file is opened on disk only when the program opens it, under the checks the walk applied (a regular file below the root, no link in its path, the per-file limit). |
| `8d909598`, `7bde65ac` | The code cache keyed an entry on every `ZIPP_*` variable and value and never removed an entry from a folder in use, so a caller that varied one value from run to run missed every run and left 0.76 MB behind each time: 210 profiled runs left 158 MB. A folder now keeps 64 MiB of entries, those used most recently (a load counts as a use, recorded at most hourly), and the variables that only observe a run (`ZIPP_PROF`, `ZIPP_PROF_PC`, `ZIPP_PROF_PC_DUMP`, `ZIPP_JITLOG`) and `ZIPP_CACHE_DIR` are not part of the key. |
| `5253e4e0` | A finished run freed the VM, the compiled program and the syntax tree object by object, and a script's output was printed only after that. `run_then_exit` leaves all three for the process's exit; the command line uses it for a plain script, and the Python driver does the same once the program's file changes are written back. Hosts that run more than one program keep the freeing paths. |

Wall time, median of 9 (15 for the empty program) on a quiet machine, `c48fc4d2` then
`9df8019c`:

| Run | Before | After |
| --- | ---: | ---: |
| `zipp py tools/python_bench/micro/dict_get.py` from the repository root | 460 ms | 62 ms |
| The same program from its own folder | 36.4 ms | 36.8 ms |
| An empty Python program | 18.5 ms | 18.8 ms |
| A script that ends with 1.8 million objects alive, time outside the script | 132 ms | 55 ms (Node: 55 ms) |

What a deferred file changes for a program: a file that cannot be read when
the program gets to it reads as missing, is reported when the run ends, and
counts as never loaded, so what the program then writes to that name is
refused as before. A file the program never opens is not read, so an
unreadable one it does not touch is no longer mentioned. The 64 MiB total is
counted from the sizes the walk saw; a file that grows afterwards is still
held to 8 MiB when read, but the total is not counted again.

The cache was checked against an empty folder: the whole Python suite,
torch programs included, wrote 24 entries (13.0 MB), and a second pass wrote
nothing and changed no entry's time.

The cached start itself was profiled and left alone: of an empty program's
18 ms, about 30% is decoding the cached runtime (2,300 functions, each with
its own vectors and strings) and 18% is outside the executable.

## WebAssembly size

| Commit | Change |
| --- | --- |
| `88b060ed`, `9df8019c` | `String.prototype.normalize`, the collators, the segmenter and the special-casing rules used the `unicode-normalization` crate, whose tables are minimal perfect hashes: 111 KB in a 32-bit module, 42 KB of it left after Brotli, because a hashed table has no order for a compressor to find. `src/unorm.rs` holds the same mappings in code point order: a table's keys are a bitmap for each 256-code-point page that has any, with a count before each 64 code points, and a member's rank (one population count) is its index into the values. The tables are generated from the crate, which stays a development dependency. The engine no longer links it or `tinyvec`. |
| `ba63d8a0`, `482bb8c3` | The seven regex properties of strings (`RGI_Emoji` and the six sets it is made of) were slices of slices: 7,906 fat pointers, 63 KB, over 12,389 code points, with `RGI_Emoji` repeating the other six entry for entry. The six literals stay as the source of truth and are packed by const evaluation into one pool of three-byte code points and a length byte per string; `RGI_Emoji` is the whole pool. This is the change [the sixth pass](2026-09-29-engine-size-round6.md) tried and reverted; see [the screen](#webassembly-speed-screen) for its test repeated. |
| `867badd1` | `ID_Start` and `ID_Continue` were `const` items, instantiated again in every crate their accessor was inlined into: the engine carried a second copy of both, 11,904 bytes. They are statics. |
| `ee34f2cb` | A Python project's files cross into the program as bytes, without the base64 encoder. |

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125 `-O1 -O1`, Node 24.19.0,
Windows x64, built from `9df8019c`. Module bytes exclude JS glue; Brotli is
quality 11 with the size hint.

| Module | Tenth pass raw | This pass raw | Change | Tenth pass Brotli | This pass Brotli | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Lite | 2,808,464 | 2,654,515 | -153,949 (-5.48%) | 750,138 | 716,795 | -33,343 (-4.44%) |
| JavaScript | 5,145,953 | 4,977,978 | -167,975 (-3.26%) | 1,279,443 | 1,239,416 | -40,027 (-3.13%) |
| Python-base | 6,808,551 | 6,644,132 | -164,419 (-2.41%) | 1,651,669 | 1,612,708 | -38,961 (-2.36%) |
| Python + Torch | 8,733,543 | 8,568,374 | -165,169 (-1.89%) | 1,995,301 | 1,963,702 | -31,599 (-1.58%) |

Against the accepted ninth-pass packages the same modules are 172,538 /
242,464 / 252,319 / 283,208 bytes smaller raw (6.10% / 4.64% / 3.66% /
3.20%) and 36,682 / 50,794 / 50,462 / 44,818 bytes smaller after Brotli
(4.87% / 3.94% / 3.03% / 2.23%). No module contains an absolute path.

By step, JavaScript module, raw / Brotli:

| Build | Raw | Brotli-11 |
| --- | ---: | ---: |
| Tenth pass | 5,145,953 | 1,279,443 |
| Properties of strings packed | 5,075,272 | 1,274,609 |
| Normalization from sorted tables | 5,010,946 | 1,245,280 |
| Identifier tables held once | 4,999,042 | 1,246,814 |
| Normalization from ranked bitmaps | 4,977,978 | 1,239,416 |

Holding the identifier tables once removes 11,904 raw bytes from every
module and no compressed ones: Brotli had already found the second copy, and
the JavaScript and Python-base modules came out 1.5 KB and 3.5 KB larger
after it. It is kept for the raw size.

Where a Lite module's data is, from a linker map of the tenth-pass source
(534 KB of read-only data): the regex crate 191 KB, the engine 136 KB,
`unicode-normalization` 111 KB, the standard library 51 KB, the embedding
20 KB (11 KB of it the host preamble's source, half comments), identifier
tables 10 KB, big integers 7 KB. Standalone Brotli of those blocks: 33, 34,
42, 25, 6, 2 and 3 KB. 86 KB of the data is binary constants of 16 bytes or
fewer.

The QuickJS size target remains unmet: Lite is 1,516,126 raw bytes larger
than the audited 1,138,389-byte reference.

## Normalization and collation speed

The first version of the compact tables found an entry by binary search.
`normalize()` then took 2.2 times as long on accented Latin and an
`Intl.Collator` sort half as long again, because the collators decompose
both strings on every comparison. The ranked bitmaps (`9df8019c`) brought a
lookup back to a few loads. Five sorts of 4,000 words with `localeCompare`,
the same with `Intl.Collator`, and 60 passes of NFC and NFKD over the words,
milliseconds, previous then this pass:

| Text | Native `localeCompare` | Native `Intl.Collator` | Native `normalize` | JavaScript module `Intl.Collator` | JavaScript module `normalize` | Lite `localeCompare` | Lite `normalize` |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| ASCII | 334 / 336 | 89 / 82 | 38 / 28 | 143 / 148 | 57 / 42 | 131 / 129 | 107 / 77 |
| Accented Latin | 362 / 356 | 121 / 130 | 95 / 127 | 214 / 226 | 149 / 157 | 165 / 180 | 256 / 330 |
| Kana | 392 / 402 | 154 / 158 | 90 / 96 | 259 / 265 | 141 / 145 | 160 / 167 | 247 / 269 |
| Hangul | 471 / 447 | 222 / 209 | 104 / 90 | 394 / 361 | 173 / 151 | 165 / 157 | 282 / 228 |
| CJK | 360 / 348 | 116 / 116 | 56 / 51 | 220 / 209 | 89 / 77 | 144 / 141 | 160 / 127 |

Native: `c48fc4d2` and `9df8019c`, one pinned run each. Modules: the
accepted ninth-pass package and this pass's, the best of three runs each.
`normalize()` is 34% slower natively on text in which nearly every character
decomposes and recomposes (29% in Lite, 5% in the JavaScript module), 7% to
9% slower on kana, and 9% to 26% faster on ASCII, Hangul and CJK. Sorting
with a collator is within 7% everywhere and 9% slower with Lite's
`localeCompare` on accented Latin.

The tables are checked against the crate for every code point (class, mark
test and all four forms), every pair of characters that takes part in any
composition, and 300,000 generated strings of marks and decomposable
characters.

## WebAssembly speed screen

`node --no-liftoff`, one pinned processor, this pass's modules against the
accepted ninth-pass packages. Three ways of running the same harness were
used, because they do not agree.

**Paired, one process per case** (the ninth pass's method and rule: a case is
flagged when it is more than 2.5% slower in three of four runs, both load
orders on two processors). Ratios are candidate over accepted, medians of 31
batches:

| Module | Geomean, four runs | Flagged | Its four readings |
| --- | --- | --- | --- |
| Lite | 0.978, 0.976, 0.983, 0.977 | string-append-index | 1.037, 1.025, 1.041, 1.005 |
| JavaScript | 0.982, 0.977, 0.994, 0.994 | none | |
| Python-base | 0.985, 0.982, 0.984, 0.982 | promise-resolve | 1.108, 1.102, 1.081, 1.099 |
| | | prop-poly | 1.025, 1.030, 1.022, 1.031 |
| Python + Torch | 0.974, 0.984, 0.957, 0.978 | error-type | 1.035, 1.033, 1.027, 1.062 |
| | | property-delete | 1.031, 1.020, 1.031, 1.033 |

Faster in three of four runs everywhere: array-build (0.57 to 0.75) and regex
(0.88 to 0.97); in the Python modules also python-list-e2e (0.92 to 0.94) and
python-dictionary-e2e (0.94 to 0.98). The tenth pass's flags are gone where
they were (Lite promise-resolve reads 0.991, 1.003, 1.005, 0.998) and others
have appeared. By the ninth pass's rule only the JavaScript module passes.

**The whole corpus in one process** (the sixth pass's method), four runs:
Python-base, Python + Torch and Lite have no flagged row (Python-base
promise-resolve 0.991, 1.014, 1.016, 1.017). JavaScript has three:
string-build 1.044, 1.116, 1.069, 1.059, string-append-index 1.051, 1.116,
1.058, 1.048 and enumeration 0.992, 1.031, 1.033, 1.029, none of which the
paired runs show (string-build 0.991, 0.983, 0.998, 0.979).

**One module per process**, processes alternated, median of 6 to 10
processes, each within 1.5% of its median:

| Row | Accepted | Tenth pass | This pass |
| --- | ---: | ---: | ---: |
| Python-base promise-resolve | 0.2716 ms | 0.2682 ms (0.988x) | 0.2757 ms (1.015x) |
| Lite promise-resolve | 0.6597 ms | 0.6723 ms (1.019x) | 0.6608 ms (1.002x) |
| JavaScript, 20,000 `s += i` | 1.017 ms | 1.027 ms (1.010x) | 1.133 ms (1.114x) |
| Python-base, the same loop | 1.251 ms | | 1.245 ms (0.995x) |
| Lite, the same loop | 1.810 ms | | 1.809 ms (0.999x) |

So the Python-base module's promise-resolve is 8% to 11% slower in the paired
runs, 1% slower in the corpus and 1.5% slower alone, and the JavaScript
module has a string loop that is 11% slower alone, string rows 4% to 12%
slower in the corpus, and the same rows unchanged in the paired runs.

### What moves these rows

Not the code on their paths. Between the tenth pass's modules and these,
46 functions differ in size in the JavaScript module and 68 in Python-base:
all of them normalization, collation, segmentation, special casing, the
regex property parser or the Python project compiler. Dispatch, promises,
strings, the collector and the allocator are byte for byte the size they
were. What changed for them is where things are.

The string-set packing (`ba63d8a0`) is the change the sixth pass reverted
for slowing Promise workloads in the Python modules, so its test was
repeated on builds of this pass with only that change taken out:

| Row | With the packing | Without it |
| --- | ---: | ---: |
| Python-base promise-resolve, paired (four runs) | 1.108, 1.102, 1.081, 1.099 | 1.022, 1.049, 1.035, 1.045 |
| Python-base promise-resolve, alone | 1.015x | 0.961x |
| JavaScript `s += i`, alone | 1.114x | 1.183x |

The sixth pass's observation reproduces in Python-base: the packing costs
promise-resolve there about 6%, paired and alone, and without it the row is
still flagged. The same step makes the JavaScript module's string loop 6%
faster, and across this pass's JavaScript builds that loop read 1.18x after
the packing, 1.12x after each of the next three changes and 1.18x with the
packing removed. A change that only deletes table data speeds one module's
row and slows another's.

Two things were ruled out. It is not where the heap starts: leaking 8 bytes
to 9.7 KB from the allocator before the engine exists moves either module's
promise-resolve by 2% in both directions and leaves the gap. And the
statistic is not all of it: promise-resolve collects on about every fifth
call, a batch holds one collection or two, and the median of the batches
reads one of two levels 5% apart, so medians over successive windows of one
module's own run differed by 6%. The screen now also reports a trimmed mean
(`67b07838`); on the tenth pass's recorded samples it still flags
promise-resolve in Lite and Python-base (1.04 to 1.08 and 1.03 to 1.07), so
it does not change that pass's answer. Here it agrees with the median on
Python-base and also flags JavaScript promise-resolve (1.04, 1.06, 1.02,
1.05), which the median does not.

The cause was not found. The practical reading is that a single row of this
screen can differ by 10% between two builds of one source whose hot code is
identical, in a direction that depends on the module and on how the row is
run, while the geomeans (0.96 to 0.99 here) do not.

`tests/node/bench-unicode-string-properties.cjs`, the benchmark written for
the string-set tables: compiling a pattern over each of the seven properties
costs 0.96x to 1.04x of the accepted module's time in the three full
modules and 1.01x to 1.11x in Lite (`RGI_Emoji` 1.10x).

## Native measurements

`tools/bench.py`, cold metric, paired, one processor's worth of work at a
time on a machine waited quiet. `c48fc4d2` (the tenth pass's final engine
commit) to `9df8019c`, 12 repetitions:

| Row | Before | After | Paired change [95%] |
| --- | ---: | ---: | ---: |
| async-promise-chain | 476 ms | 470 ms | -0.7% [-1.6, +0.6] |
| class-prototype-hot | 227 ms | 228 ms | +0.8% [-0.6, +1.5] |
| json-large | 305 ms | 298 ms | -2.8% [-3.6, -0.8] |
| map-set-heavy | 817 ms | 791 ms | -3.3% [-6.6, -1.3] |
| markdown-render | 224 ms | 219 ms | -2.3% [-5.7, +2.0] |
| parse-large-js | 251 ms | 244 ms | -3.0% [-3.7, -2.4] |
| polymorphic-objects | 413 ms | 413 ms | +0.5% [-1.1, +1.9] |
| polymorphic-objects-v2 | 28 ms | 28 ms | -1.9% [-3.6, +1.5] |
| property-ic-shapes | 10 ms | 10 ms | -0.6% [-4.8, +5.3] |
| regex-log-scan | 465 ms | 453 ms | -2.6% [-4.7, -0.3] |
| sparse-array | 76 ms | 76 ms | -0.4% [-1.4, +1.0] |
| sparse-array-v2 | 105 ms | 106 ms | +0.6% [+0.1, +1.1] |
| typedarray-math | 141 ms | 138 ms | -2.6% [-3.2, -1.9] |
| Geomean, 13 rows | | | -1.42% [-1.95, -0.62] |

Output is byte-identical on every row and process start is unchanged (6.6
and 6.5 ms). The only native change on these rows' paths is the exit: the
rows that end with a large heap alive are the ones that moved. Rows within
3% between two different binaries are also within what a link moves.

Against Node 24.19.0, 10 repetitions: headline ten 0.972x [0.958, 0.980],
all 13 rows 0.678x. Behind: async-promise-chain 1.42x [1.40, 1.45],
polymorphic-objects 1.28x [1.26, 1.28], json-large 1.15x [1.05, 1.17].
map-set-heavy, a point behind in the tenth pass, is at 0.93x [0.87, 0.95].
This is not parity with V8 on every row, and it is not the published,
profile-guided comparison.

## Python suite

`tools/python_bench.py`, 7 repetitions, each program run from its group
folder, CPython 3.13.14. In-script time: 1.81x of CPython in geomean (micro
1.63x, macro 3.01x), and 0.99x of `c48fc4d2`. No row moved by more than 3%
except the four that follow the link (generator_iter 0.90x, math_sqrt 0.90x,
word_count 0.92x, dict_get 0.98x). Time outside the script is 21.5 ms a
program, as before: these programs start in folders of thirty files and end
with small heaps, so neither change above shows here.

The rows furthest behind CPython are unchanged: str_methods 6.9x,
try_raise_catch 5.3x, instance_new 4.5x, fstring 4.3x, oo_simulation 4.1x,
generator_iter 4.1x, sort_heapq 4.0x. `ZIPP_PY_JIT_RUNTIME=1` (compile the
JavaScript runtime too) and `ZIPP_PY_TIERC=1` (compile whole Python
functions) were tried on this pass's binary and are still losses: 0.7% and
5.9% slower in geomean.

## Correctness

All on `9df8019c` with a clean tree, run one after another with nothing else
building:

- Test262, the full corpus with `tools/test262-expected-failures.txt`: 95,671
  of 95,680 executions pass, the nine failures are the manifest's, and there
  is no unexpected failure, stale expectation or skip. That includes
  `built-ins/String/prototype/normalize`,
  `built-ins/RegExp/property-escapes/generated/strings` and `intl402/Collator`,
  the directories over the changed tables. The local corpus is an exported
  tree under `target/`, so the runner prints the engine repository's commit
  as the corpus revision; the manifest pins corpus `42496613`.
- `cargo test -p zipp-vm --no-fail-fast` with the `tools/ci_quarantine.py`
  skips: 2,179 passed, 0 failed, 22 ignored, in 307 test binaries. Four of
  them are new: the normalization tables are regenerated from the
  `unicode-normalization` crate and compared, and the class, the mark test
  and all four forms are compared with the crate for every code point, every
  pair of characters that takes part in a composition, and 300,000 generated
  strings.
- `cargo test -p zipp-cli`: 80 passed, 1 ignored, including the native GPU
  tests on a hardware adapter and two new driver tests (a data file the
  program never opens is not read; one that cannot be read when opened reads
  as missing, is reported and is not overwritten).
- `cargo test -p zipp-vm --features python --lib`: 619 passed, 5 ignored.
- `cargo test -p zipp-regress`: 108 passed, 1 ignored, including the decode
  of every property of strings against its generated literal.
- `zipp-vm` checks with warnings denied in the default, `safe-sandbox`,
  `python` and `wasm-lite` profiles, and the sandbox workspace against its
  changed lockfile.
- The Node boundary suite on the exact modules of the size table: JavaScript
  35/35, Lite 35/35, Python + Torch 35/35, Python-base 33/35. The two
  Python-base failures are `audit-2026-09-15-ml-transport.cjs` and
  `python-gpu.cjs`, which need the optional Torch package, as in the ninth
  and tenth passes.
- A probe of `normalize()` in all four forms over ten strings (Hangul,
  blocked marks, compatibility forms, a lone surrogate) and a
  `localeCompare` sort prints what Node 24 and the tenth pass's binary
  print.

## Not claimed

- No profile-guided capture: the published Node comparison is unchanged, and
  the native figures above are from default-feature release builds.
- CI has not run. Besides the tenth pass's untested items (the
  `remap-paths.sh` workflow steps and the default-feature MSVC link with
  `/DELAYLOAD`), the sandbox and WebAssembly lockfiles changed here.
- Nothing was measured on Linux, macOS or in a browser.
- CPython parity is not reached, and this pass did not move in-script time.
- The cause of the layout sensitivity of compiled Python loops.
- The modules are reported as measured. Whether they replace the accepted
  ninth-pass packages is the open decision from the tenth pass, now over
  these builds.
