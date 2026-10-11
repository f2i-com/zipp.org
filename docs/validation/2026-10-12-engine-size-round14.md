# Memory that grows in place, string keys in compiled code, a table-free number parser, fourteenth pass - 2026-10-12

Continues `codex/engine-size-reduction` from `887d2149`, the commit release
0.0.22 and its Lite archive were cut from, through 35 commits to `726ddfbf`.
It was measured and gated locally and then pushed to `main` at the user's
request with CI skipped: CI has not run on it and no release has been cut.
The
WebAssembly figures are measured against the released modules (the
thirteenth pass's), and "released" below means them. The speed screen ran
on the modules built at `aa76bf5d`; the two commits after it leave their
code sections byte for byte as measured (see
[WebAssembly size](#webassembly-size)). Native figures are from an ordinary
release build (`cargo build --release -p zipp-cli`, default features, not
profile-guided) of `726ddfbf`, where the native gates also ran.

## Outcome

- **Two defects in the released build are fixed.** Memory that grows in
  place (a pushed array, a filling Map, an object gathering keys) never
  reached the collector: a WebAssembly instance trapped or a script was
  ended at its heap ceiling with a megabyte live, and `zipp` held 1.2 GB
  where Node peaks at 169 MB. An appending loop that started from the text
  `"number"` rewrote the string `typeof` returns
  ([below](#two-defects-in-the-released-build)).
- **WebAssembly size.** All four modules are smaller: Lite 61,228 bytes raw
  (2.3%) and 15,691 after Brotli (2.2%), JavaScript 172,979 (3.5%) and
  37,983 (3.1%), Python-base 140,161 (2.2%) and 19,245 (1.2%), Python +
  Torch 140,156 (1.7%) and 20,487 (1.1%)
  ([the table](#webassembly-size)).
- **WebAssembly speed.** Over its corpus each module takes less time than
  the released one: Lite 0.83 to 0.86, JavaScript 0.90 to 0.91, Python-base
  and Python + Torch 0.92 to 0.93 ([the screen](#webassembly-speed-screen)).
  A call that allocates nothing takes 0.11 to 0.17 of the time, a string
  built by appending 0.67 to 0.72. Rows are flagged slower in every module:
  `promise-resolve` in the three full ones (an artifact of the screen's
  statistic in the JavaScript module, 3 to 9% a call and unexplained in the
  two Python modules), `alloc-object` and `prop-mono` by 3 to 7% in two,
  `error-typedarray` in the two Python modules and four others in one
  module each. They are listed with what is known.
- **Native.** `887d2149` to `726ddfbf` is -3.59% [-4.06%, -3.13%] over the
  13 real rows, and the headline ten are at 0.914x [0.903, 0.929] of
  Node 24. `async-promise-chain`, `json-large` and `polymorphic-objects`
  are 12 to 14% faster; no row is more than 1.5% slower
  ([the tables](#native-measurements)). Outside those rows, a word count
  through `counts[w] = (counts[w] || 0) + 1` takes 361 ms where it took 603
  (Node: 126), a loop of `push` 790 where it took 1,314 (Node: 242), and a
  fill of an array that has survived a collection a third of the time.
- **Python.** The native suite is at 1.69x of CPython 3.13 in-script, from
  1.79x, and at 0.95x of the previous build over its 44 programs
  ([the rows](#python-suite)). Seven changes, one commit each.
- **Correctness.** Test262 passes with the expected-failure manifest
  (95,671 of 95,680 executions, nothing unexpected, stale or skipped). The
  default-feature `zipp-vm` suite passes: 2,218 tests in 315 binaries. The
  `safe-sandbox` profile's suite fails the two tests it failed before the
  pass and the Lite profile's none it did not fail before. The Node boundary
  suite passes 36 of 36 on three modules and 34 of 36 on Python-base, whose
  two failures are the existing Torch-dependent checks
  ([Correctness](#correctness)).
- **What a program can see** is listed [below](#what-a-program-or-a-host-can-see):
  identifiers follow Unicode 17, Python's `strip` and `split` use Python's
  whitespace, Python step counts drop, and collections run where none did.

## Two defects in the released build

Both are in 0.0.22 and in every release before it that has the code
concerned. They are fixed on this branch; no release has been cut.

**Memory that grows in place never reached the collector** (`d24fe3e8`).
An array is born empty and pushed, or index-stored, to a megabyte; an
object gathers twenty thousand keys; a Map fills. None of that allocates
an object, so neither the count of objects nor the size at birth moved,
and a loop that built and dropped such things ran no collection.

- In the WebAssembly modules the heap ceiling ended a script whose live
  set was a megabyte: `RangeError: script exceeded its memory budget` on
  the 512th call of a function that builds one 80,000-element array. An
  index-append or `length` loop inside one call reached the end of the
  linear memory first and trapped the instance (`unreachable`).
- Natively 2,000 such calls held 1.2 GB (Node: 120 to 170 MB over several
  runs).

The engine now reads what its allocator has outstanding. `zipp` and the
WebAssembly modules install an allocator that reports it
(`zipp_vm::note_large_block`, `zipp_vm::note_every_block`); a step of
growth in few objects asks for a collection, and under a ceiling for a
payload walk before the ceiling is judged. Not closed: growth past the
ceiling in straight-line code with no loop and no call between, where
there is no poll to judge it at.

**An appending loop rewrote the shared `typeof` strings** (`dcbfe67d`).
`var s = "number"; for (...) s += i` appended in place to the interned
string `typeof` hands out, so `typeof 1` printed `number012...`
afterwards. Present since 2 August. The engine's shared strings are now
frozen against in-place appends (`JsStr::freeze_shared`).

**The first fix was not the whole of it** (`04a31728`). `d24fe3e8` had
`zipp` report blocks of 4 KiB and more. An object given sixty properties
keeps some 4 KB in blocks of half a kilobyte to a kilobyte and a half, so
such objects, made and dropped by a loop that allocates nothing else, were
as invisible as before: 60,000 of them peaked at 232 MB, as they do in
0.0.22 (Node: 39 to 56 MB).
`bench/real/polymorphic-objects` only looked well because each object's
sixty key strings were allocations; when this pass's enumeration cache
(`f8f5fe81`) took those away, the row ran no collection at all and its peak
went from 34 MB to 150 MB. That regression was introduced and removed
inside this pass. Blocks are now reported from 256 bytes, a 16 MiB step in
few objects asks for a minor, and a major is asked for from what a minor
could not free (64 MiB, or a quarter, over the level the last major left).

Peak working set in MB, the median of three runs, Node beside:

| Program | 0.0.22 | This pass | Node 24 |
| --- | ---: | ---: | ---: |
| 2,000 calls building an 80,000-element array | 1,246 | 60 | 169 |
| 300 Maps of 50,000 numbers, dropped | 569 | 90 | 144 |
| 300 Sets of 50,000 numbers, dropped | 438 | 69 | 116 |
| 300 objects of 20,000 keys, dropped | 365 | 78 | 83 |
| 60,000 objects of sixty keys, dropped | 232 | 61 | 39 |
| `bench/real/polymorphic-objects` | 34 | 47 | 43 |
| A ring of 64 pushed arrays, replaced 40,000 times | 174 | 107 | 174 |

## The changes

### Collector and memory

- `d24fe3e8` the allocator's outstanding figure, the footprint schedule and
  the reporting allocators, above.
- `5e07f418` the checked profile's periodic heap walk is skipped while the
  allocator's figure is within 64 KiB of where the last walk left it: a call
  that allocates nothing went from 6.9 to 1.1 microseconds in the modules.
- `04a31728` blocks from 256 bytes, a minor first, above.
- `726ddfbf` what compiling a Python library module reserves is left out
  of the footprint step (native builds). The allocator's figure holds the
  bytecode an import compiles, tens of megabytes for a package, and each
  16 MiB of it asked for a collection that freed nothing: a Torch program
  that ran one collection ran six, and the suite's Torch rows were 0 to 78%
  slower until this commit. They are level with 0.0.22.
- `6cb2bb48` the two sweeps prune some fifty side tables through one
  out-of-line body a table type (`SlotKeyed`) instead of a closure a table.
- `f8f5fe81` `for (k in o)`, `Object.keys` and their kin hand out one heap
  string per key text (`Vm::key_str_cache`, bounded at 64 bytes a key and
  8,192 entries, frozen, rooted): json-large runs 5 minors where it ran 23.

### Interpreter (all builds; the WebAssembly modules most)

- `3d199910` a non-string constant is loaded without a call, and builds
  without the JIT cache and freeze the literals of appending functions:
  string-build 2.53 to 1.71 ms, arith-float 3.21 to 2.95 ms in the modules.
- `f3b8b73b` Lite's cached property read makes five calls fewer: prop-mono
  3.78 to 3.08 ms, prop-poly 5.46 to 5.04 ms, for 4,124 bytes raw.
- `cdbfc98f` `push` is proven from an inlined memo hit (Lite array-build
  5.98 to 5.27 ms), and `p.then` on a plain promise is read without the
  prototype walk (44 to 25 ns).
- `7a58d3da` a number that is n/2^d for d up to 7 is printed without the
  shortest-digits search: three million `x.5` values stringify in 61 ms
  where they took 255 (Node: 92).
- `0413bb38` an object member's key is written in one pass when it needs no
  escaping: a million four-key objects stringify in 177 ms from 245
  (Node: 135).
- `6400bd7f` `Promise.all` over an array presizes its vectors and skips
  barriers it cannot need: 120 to 77 ms on 30,000 arrays of 100 (Node: 102).
- `4ea758cd` three diagnostic switches are read from the environment once.

### Compiled code (native)

- `6a1ae44e` a captured `then` on a plain promise goes straight to
  PerformPromiseThen: a chain of thens builds in 148 ms from 171
  (Node: 143).
- `81d83180` property keys of up to sixteen bytes are compared inline.
- `e1d15acc` `o[k]` with a string key stays in compiled code on an own
  miss, on a new key and on a rope key. Before, each left the region, 32 of
  them evicted it and the function was blacklisted: a word count
  (`counts[w] = (counts[w] || 0) + 1`) took 574 ms and takes 366
  (Node: 126). The miss is answered by the walk the named-property helper
  makes; the append is the interpreter's own fast path, by its own
  predicate. `ZIPP_NO_JIT_STRKEY_LANE=1` restores the old behaviour.
- `9054860d` a key's bytes are hashed eight at a time (property tags and
  the Map and Set string hash; the checked profile's ordered indexes do not
  use it): 500 UUID-shaped keys through `map.get(k) + o[k]` take 107 ms from
  172 (Node: 54).
- `aa76bf5d` a compiled `push` reads its pin's flags back from the pin:
  40,000 arrays of 4,096 pushes take 793 ms from 1,156 (Node: 242).
- `1a567913` a compiled element store takes a value that is not a heap
  value into an array that has survived a collection. Before, only a young
  array was stored into inline and an old one paid a call an element,
  whatever was stored: a million-element fill took 15 ms on an array a
  collection had seen and 4 ms on one it had not, in 0.0.22 as here
  (Node: 2 to 3 ms). The footprint schedule moved the first collection of
  `bench/real/sparse-array` ahead of its fill, which is how this was found:
  the row read 87 ms for a day and reads 76 (0.0.22: 77).

### Size

- `2e981e3d` a regex literal's early errors are checked through the parser
  the VM already has, `d88bbf57` a subclassed builtin's instance is copied
  by its representation in every build, `4690ca69` the decorator list is
  handed over: 90,832 bytes raw and 20,160 after Brotli off the JavaScript
  module together.
- `5821990e` the lexer reads identifier characters from the regex engine's
  Unicode tables and the `unicode-id-start` crate is gone: 7,311 bytes raw.
  See the identifier change below.
- `04e6d2bf` the likely-subtags language table is three-byte keys and two
  byte columns where it was 86 KB of text and a 7,195-entry hash map built
  at first use: 39,585 bytes raw, 4,262 after Brotli (full modules only).
- `6cb2bb48` the prune bodies, above: 20,333 bytes raw in the JavaScript
  module, 11,262 in Lite.
- `13f07689` decimal text is parsed to `f64` by the engine's own
  Eisel-Lemire parser (`num_parse.rs`), whose power-of-five table is built
  at first use, so core's 10 KB table is no longer linked: about 12 KB after
  Brotli off each of the JavaScript and Lite modules. Bit-exact against core
  on 6.5 million exhaustive and 200 million random inputs. The Python
  variants still link core's parser through `zipp-pyparse`.

### Python

Seven of the workflow's confirmed findings, one commit each:

| Commit | Change | Row, before to after |
| --- | --- | --- |
| `ac5a63d7` | PyCall calls the runtime's native str entry where it is | `str_methods` 45.2 to 39.9 ms, `math_sqrt` 31.2 to 26.2 |
| `44828787` | PyMethod reads a builtin record's class at its slot | `list_append` 12.7 to 11.2, `dict_get` 16.8 to 13.7 |
| `44670116` | `strip` and `split` natively, at Python's whitespace | `str_methods` 39.7 to 33.0 |
| `e00252ec` | a str's hash fits the int the value holds | `hash_str` 24.7 to 17.1 |
| `8bf8f46a` | a loop over a list or tuple display makes no list | `listcomp_small` 24.6 to 17.9 |
| `8b8ef8a3` | heap and bisect items compared where they live | `sort_heapq` 25.5 to 20.2 |
| `45355a00` | a property's accessor called from the attribute instruction | `property_read` 34.5 to 25.1, `oo_simulation` 59.6 to 56.4 |

The eighth (a cache for `super()` calls) was not built: it needs a new
instruction across about fifteen shared sites for one micro row.

## What a program or a host can see

- **Identifiers follow Unicode 17.** The crate the lexer used accepted
  12,823 code points that are unassigned in Unicode 17 (`var \u{558}`); they
  are now rejected, as Node 24 rejects them.
- **Python `str.strip()` and `str.split()`** no longer treat U+FEFF as
  whitespace and do treat U+001C to U+001F and U+0085, as CPython does;
  `split(None, n)` no longer strips the remainder. `hash(str)` values change
  (CPython's change per process).
- **Metering.** A Python property access charges 7 fewer steps, a
  comprehension over a three-element display 22 fewer, and `strip()` and
  `startswith` no longer run two closure frames. JavaScript step counts are
  unchanged.
- **Collections happen where they did not.** A program that grows memory
  in place is collected, so finalization-registry callbacks and WeakRef
  clearing can come earlier.
- **`zipp_vm::LARGE_BLOCK_BYTES` is 256** (it was 4,096 when introduced in
  this pass), and an embedder that wants in-place growth collected reports
  its allocator's blocks with `note_large_block` or `note_every_block`.
  One that reports nothing behaves as 0.0.22 did.
- **The first number that needs the table** (17 significant digits, or an
  exponent past 22) costs 1.1 ms once per instance in the JavaScript module
  and 1.5 ms in Lite; text the exact path settles (`0.1`, `3.14159`,
  integers) never pays it. 17-digit text parses 3 to 5% slower in the
  JavaScript module; `JSON.parse` of floats is 11 to 12% faster.
- **`zipp_vm::embed::parse_f64`** is public, for hosts that parse numbers
  beside the engine without linking core's table.

## The workflow's findings

The pass began with a 159-agent workflow that produced 73 findings before
the session limit ended it with 108 verifier agents unfinished: 10 were
confirmed by a second agent, 8 came back needing rework, 1 was rejected and
54 were never verified. What was built:

- the ten confirmed ones, less the `super()` cache (seven Python changes,
  the regex literal check, the builtin instance copy);
- from the unverified ones, only what was re-measured here before it was
  built: the identifier tables, the likely-subtags packing, the prune
  bodies and the number parser;
- one unverified finding was built and reverted: a pool of object shells
  for the checked profile made `alloc-object` 6% slower (a 256-shell pool
  serves under 2% of allocations after a minor).

Everything else in the pass came from profiles and probes taken during it.
The findings file (`target/r14-findings.json`) is local and is not part of
the branch.

## WebAssembly size

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125 (`-O1 -O1`), Node 24.19.0.
Brotli is quality 11 with the size hint, as the README states it. Module
bytes, raw / Brotli, Windows builds; "released" is the thirteenth pass's
modules, which 0.0.22 was cut from.

| Variant | Released | This pass | Raw | Brotli | Before post-processing |
| --- | ---: | ---: | ---: | ---: | ---: |
| Lite | 2,634,413 / 710,436 | 2,573,185 / 694,745 | -61,228 (-2.32%) | -15,691 (-2.21%) | 2,682,006 / 698,541 |
| JavaScript | 4,876,583 / 1,227,452 | 4,703,604 / 1,189,469 | -172,979 (-3.55%) | -37,983 (-3.09%) | 4,858,294 / 1,193,751 |
| Python-base | 6,519,418 / 1,597,418 | 6,379,257 / 1,578,173 | -140,161 (-2.15%) | -19,245 (-1.20%) | 6,567,831 / 1,585,550 |
| Python + Torch | 8,443,452 / 1,950,080 | 8,303,296 / 1,929,593 | -140,156 (-1.66%) | -20,487 (-1.05%) | 8,496,220 / 1,936,227 |

The JavaScript module by step, raw bytes as the build script reports them:
the regex literal check, the builtin instance copy and the decorator list
4,879,542 to 4,788,710; the identifier tables 7,311 more; the likely-subtags
table 4,778,816 to 4,739,231; the prune bodies to 4,718,898; the number
parser and the rest to 4,703,604. The in-place growth fix and Lite's inlined
property reads cost bytes (4,124 in Lite for the reads). The Python variants
gain the seven Python changes' code and still link core's number parser
through `zipp-pyparse`, which is why their Brotli saving is half the
JavaScript module's.

The speed screen below ran on the modules built at `aa76bf5d`. Two commits
follow it. `1a567913` changes x86-64 code generation only and builds the
same four modules byte for byte. `726ddfbf` adds 23 lines to `heap.rs` that
the checked profiles do not compile: the final modules, which the table
above and the gates describe, have code sections identical to the measured
ones and differ from them in 58 to 64 bytes of data (the line numbers in
`heap.rs`'s panic locations), which moves Brotli's output by up to 584
bytes.

## WebAssembly speed screen

Against the released modules. One process a case and configuration, pinned
to one processor, 10 warm-up pairs and 31 alternating paired samples; four
configurations (released first and this pass first, at two affinities).
Each cell is the lowest and highest ratio of the four, this pass's time
over the released module's. `f` marks a case more than 2.5% faster in three
of the four and `s` one more than 2.5% slower in three of the four, the
ninth pass's rule.

| Case | Lite | JavaScript | Python-base | Python + Torch |
| --- | ---: | ---: | ---: | ---: |
| `alloc-object` | 0.96 to 0.99 | 1.03 to 1.04 s | 1.03 to 1.06 s | 1.01 to 1.02 |
| `arith-float` | 0.81 to 0.84 f | 0.87 to 0.92 f | 0.92 to 0.92 f | 0.90 to 0.93 f |
| `arith-int` | 0.51 to 0.94 f | 0.99 to 1.02 | 0.99 to 1.01 | 1.01 to 1.08 s |
| `arith-mod` | 0.89 to 0.94 f | 0.98 to 1.05 | 0.99 to 1.00 | 0.98 to 1.18 |
| `array-build` | 0.69 to 0.81 f | 0.84 to 0.86 f | 0.82 to 0.83 f | 0.70 to 1.25 f |
| `array-hof` | 0.89 to 0.92 f | 0.98 to 1.01 | 0.98 to 1.11 | 0.95 to 1.02 |
| `call-deep` | 0.11 to 0.12 f | 0.16 to 0.17 f | 0.16 to 0.17 f | 0.16 to 0.17 f |
| `enumeration` | 0.88 to 0.98 f | 0.89 to 0.92 f | 0.88 to 0.89 f | 0.88 to 0.91 f |
| `error-range` | 1.01 to 1.04 | 0.94 to 1.02 | 0.97 to 1.03 | 0.96 to 1.03 |
| `error-type` | 0.99 to 1.03 | 1.01 to 1.04 | 0.94 to 1.01 | 0.97 to 1.01 |
| `error-typedarray` | 0.99 to 1.03 | 0.99 to 1.02 | 1.02 to 1.09 s | 1.02 to 1.08 s |
| `javascript_init_0_functions` | 1.02 to 1.03 s | 0.99 to 1.01 | 0.99 to 1.00 | 0.99 to 1.01 |
| `javascript_init_200_functions` | 1.01 to 1.04 | 1.01 to 1.01 | 1.01 to 1.01 | 1.00 to 1.00 |
| `json` | 0.97 to 1.04 | 0.94 to 1.01 | 0.98 to 1.02 | 0.98 to 1.01 |
| `promise-resolve` | 0.96 to 1.01 | 1.08 to 1.09 s | 1.05 to 1.08 s | 1.05 to 1.08 s |
| `prop-mono` | 0.80 to 0.83 f | 1.00 to 1.05 s | 1.05 to 1.07 s | 0.98 to 1.10 |
| `prop-poly` | 0.84 to 0.89 f | 0.98 to 1.03 | 0.98 to 1.38 | 0.94 to 1.00 |
| `property-delete` | 0.96 to 1.01 | 0.97 to 0.99 | 0.95 to 0.98 f | 0.95 to 1.00 |
| `recursive-calls` | 0.99 to 1.00 | 0.96 to 0.99 | 0.97 to 1.01 | 1.01 to 1.03 s |
| `regex` | 0.94 to 1.00 | 0.98 to 0.99 | 1.01 to 1.03 | 1.00 to 1.04 |
| `regexp-matchall` | 1.02 to 1.03 | 1.00 to 1.03 | 1.00 to 1.02 | 0.98 to 1.02 |
| `regexp-unicode-compile` | 0.97 to 1.02 | 1.00 to 1.02 | 1.00 to 1.01 | 1.00 to 1.01 |
| `string-append-index` | 0.91 to 0.96 f | 0.93 to 0.97 f | 0.95 to 0.98 f | 0.96 to 0.98 f |
| `string-build` | 0.67 to 0.72 f | 0.70 to 0.71 f | 0.72 to 0.72 f | 0.70 to 0.71 f |
| `python-arithmetic-e2e` |  |  | 0.98 to 1.03 | 1.01 to 1.02 |
| `python-dictionary-e2e` |  |  | 1.00 to 1.02 | 1.00 to 1.03 |
| `python-list-e2e` |  |  | 0.98 to 1.00 | 0.98 to 1.00 |
| `python_init_1_functions` |  |  | 1.01 to 1.03 | 1.00 to 1.02 |
| `python_init_200_functions` |  |  | 1.02 to 1.04 | 1.03 to 1.04 s |
| geomean | **0.83 to 0.86** | **0.90 to 0.91** | **0.92 to 0.93** | **0.92 to 0.93** |

Every module is faster over its corpus. The large steps: a call that
allocates nothing no longer pays the periodic heap walk (`call-deep`),
appending loops share their literals (`string-build`), `push` is proven
from a memo (`array-build`), enumeration hands out cached key strings, and
in Lite a cached property read makes five calls fewer.

**Rows flagged slower, and what is known about each.**

- `promise-resolve`, the three full modules. In the JavaScript module this
  is the screen's known edge on a row that collects every few calls (a
  batch holds one collection or two, and the median reads one of two
  levels): the mean call, measured over 6,000 interleaved calls in both
  orders, is 0.96 and 0.99 of the released module's, and the median call
  0.95 and 0.98. In the two Python modules it is not an artifact: over
  eight such runs the mean call is 1.03 to 1.09 of the released module's
  and the median call 1.04 to 1.13. The cause was not found. It is not the
  one change the Python work made to the shared dispatcher: a module with
  that change folded back into the existing call arm measured the same.
- `alloc-object`, JavaScript and Python-base, 3 to 6%. A rough bisect over
  this pass's JavaScript builds (one pair of runs a build) puts 1 to 4% of
  it at the first commit: the reporting allocator adds a call to every
  block the engine takes and gives back, which is the price of collecting
  memory that grows in place. Lite pays the same and is not slower on the
  row, because its property reads got cheaper.
- `prop-mono`, JavaScript and Python-base, up to 7%. Nothing on this row's
  path changed. Over eight of the pass's JavaScript builds the same rough
  bisect read between 0.95 and 1.10 of the released module: 1.10 at the
  first, 0.95 to 0.98 in the middle, 1.04 to 1.07 at the last. Between
  the build that read 0.97 to 1.00 and the next, which read 0.99 to 1.07,
  the only functions that differ are the collector's (the prune bodies),
  which a loop of 200,000 property reads never calls. Where the machine
  code of the dispatch loop lands is the likely cause; it is not shown.
- `error-typedarray` in the two Python modules (2 to 9%), `arith-int` and
  `recursive-calls` in Python + Torch (1 to 8% and 1 to 3%), engine start in
  Lite (2 to 3%) and with 200 functions in Python + Torch (3 to 4%): not
  investigated. Start-up takes several thousand blocks from the allocator,
  each now reported.

The regex-mode harness, the three cliffs (a 300 KB CJK literal, a sticky
tokenizer and an `indexOf` scan over 200 KB) and the RegExp memory probe
read as they did or better; a loop of plain `new RegExp` holds 4 to 9 MiB
where it held 13 to 18.

## Native measurements

Ordinary release builds (`cargo build --release -p zipp-cli`, default
features, not profile-guided), `887d2149` (0.0.22) against `726ddfbf`,
12 interleaved repetitions, medians, paired ratios with percentile-bootstrap
intervals.

| Row | 0.0.22 | This pass | Paired change | 95% interval |
| --- | ---: | ---: | ---: | ---: |
| `async-promise-chain` | 478 ms | 419 ms | -12.5% | -13.4% to -11.9% |
| `class-prototype-hot` | 228 ms | 228 ms | +0.3% | -0.7% to +0.6% |
| `json-large` | 300 ms | 256 ms | -14.3% | -15.9% to -13.7% |
| `map-set-heavy` | 665 ms | 651 ms | -1.2% | -4.1% to +1.9% |
| `markdown-render` | 217 ms | 212 ms | -2.6% | -3.7% to -0.2% |
| `parse-large-js` | 245 ms | 248 ms | +1.5% | +0.7% to +2.1% |
| `polymorphic-objects` | 410 ms | 355 ms | -13.6% | -14.1% to -10.6% |
| `polymorphic-objects-v2` | 27 ms | 27 ms | -0.7% | -1.8% to +2.2% |
| `property-ic-shapes` | 10 ms | 10 ms | +0.1% | -4.0% to +2.0% |
| `regex-log-scan` | 442 ms | 440 ms | -0.7% | -2.0% to +0.4% |
| `sparse-array` | 76 ms | 75 ms | -0.9% | -2.8% to -0.2% |
| `sparse-array-v2` | 107 ms | 106 ms | -0.6% | -1.2% to -0.3% |
| `typedarray-math` | 140 ms | 142 ms | +0.8% | -0.2% to +1.6% |

Geomean of the paired ratios: 0.9547x [0.951, 0.960] over the 10 headline rows, 0.9958x [0.980, 1.006] over the 3 diagnostic rows (`polymorphic-objects-v2`, `property-ic-shapes`, `sparse-array-v2`) and 0.9641x [0.959, 0.968] over all 13. Every row prints the same bytes in both builds.

Against Node 24.19.0, 10 repetitions:

| Row | Node 24 | This pass | This pass / Node | 95% interval |
| --- | ---: | ---: | ---: | ---: |
| `async-promise-chain` | 350 ms | 420 ms | 1.21x | 1.19 to 1.24 |
| `class-prototype-hot` | 300 ms | 229 ms | 0.76x | 0.76 to 0.77 |
| `json-large` | 264 ms | 261 ms | 0.99x | 0.96 to 1.01 |
| `map-set-heavy` | 732 ms | 733 ms | 0.93x | 0.87 to 1.05 |
| `markdown-render` | 283 ms | 216 ms | 0.78x | 0.76 to 0.82 |
| `parse-large-js` | 275 ms | 253 ms | 0.91x | 0.90 to 0.92 |
| `polymorphic-objects` | 331 ms | 363 ms | 1.09x | 1.07 to 1.12 |
| `polymorphic-objects-v2` | 84 ms | 28 ms | 0.33x | 0.32 to 0.34 |
| `property-ic-shapes` | 260 ms | 10 ms | 0.04x | 0.04 to 0.04 |
| `regex-log-scan` | 478 ms | 455 ms | 0.95x | 0.93 to 0.98 |
| `sparse-array` | 82 ms | 78 ms | 0.94x | 0.92 to 0.98 |
| `sparse-array-v2` | 174 ms | 107 ms | 0.62x | 0.61 to 0.63 |
| `typedarray-math` | 204 ms | 143 ms | 0.69x | 0.68 to 0.70 |

Geomean: 0.9137x [0.903, 0.929] of Node over the 10 headline rows, 0.6440x [0.639, 0.651] over all 13.

Probes written during the pass, wall time of the whole process, minimum of
five runs, ms:

| Probe | 0.0.22 | This pass | Node 24 |
| --- | ---: | ---: | ---: |
| Word count (`counts[w]` read, defaulted to 0, incremented and stored), 2,000 rounds of 2,000 words | 603 | 361 | 126 |
| `o[k] = o[k] + 1` over 40 existing 7-character keys, 8M times | 473 | 411 | 110 |
| 60 keys appended to each of 60,000 objects | 352 | 279 | 100 |
| 500 UUID-shaped keys, `map.get(k) + o[k]`, 2M of each | 171 | 108 | 54 |
| 40,000 arrays of 4,096 pushes | 1,314 | 790 | 242 |
| 40,000 Maps of 512 `set`s | 1,220 | 1,036 | 218 |
| 40,000 strings of 2,048 appends | 1,331 | 1,226 | 192 |
| Ten fills of a million-element `new Array(n)` | 105 | 67 | 61 |

The gaps that remain on these probes are under [Not claimed](#not-claimed): a
string-keyed read or write still costs a helper call of about 14 ns where
Node's keyed inline cache costs 3 to 4, and a compiled `push` still calls.

## Python suite

`tools/python_bench.py`, 44 programs, 7 repetitions, in-script work time,
CPython 3.13 beside:

| Program | 0.0.22 | This pass | CPython 3.13 | This pass / 0.0.22 | This pass / CPython |
| --- | ---: | ---: | ---: | ---: | ---: |
| `micro/attr_read_write` | 3.3 | 3.3 | 7.8 | 0.99x | 0.42x |
| `micro/builtin_isinstance` | 19.8 | 18.6 | 9.3 | 0.94x | 1.99x |
| `micro/builtin_len` | 17.5 | 17.2 | 11.8 | 0.98x | 1.46x |
| `micro/call_defaults` | 24.6 | 25.5 | 12.3 | 1.04x | 2.07x |
| `micro/call_keywords` | 13.2 | 13.1 | 8.0 | 0.99x | 1.64x |
| `micro/call_simple` | 64.5 | 64.4 | 36.3 | 1.00x | 1.77x |
| `micro/dict_get` | 16.0 | 13.2 | 11.2 | 0.83x | 1.18x |
| `micro/dict_str_key` | 37.8 | 38.8 | 20.8 | 1.03x | 1.87x |
| `micro/float_arith` | 2.3 | 2.3 | 9.3 | 1.01x | 0.25x |
| `micro/for_list` | 16.4 | 16.5 | 16.6 | 1.01x | 0.99x |
| `micro/fstring` | 15.1 | 15.7 | 3.5 | 1.04x | 4.48x |
| `micro/generator_iter` | 58.4 | 59.1 | 14.6 | 1.01x | 4.05x |
| `micro/global_read` | 39.0 | 38.5 | 50.4 | 0.99x | 0.76x |
| `micro/hasattr_miss` | 5.0 | 4.9 | 2.8 | 0.98x | 1.77x |
| `micro/hash_str` | 25.6 | 16.3 | 7.7 | 0.64x | 2.12x |
| `micro/instance_new` | 26.0 | 26.5 | 5.8 | 1.02x | 4.56x |
| `micro/int_arith` | 70.3 | 70.4 | 145.5 | 1.00x | 0.48x |
| `micro/list_append` | 12.5 | 10.9 | 5.1 | 0.87x | 2.13x |
| `micro/list_index` | 35.0 | 34.9 | 26.2 | 1.00x | 1.33x |
| `micro/listcomp_small` | 22.9 | 15.8 | 7.0 | 0.69x | 2.26x |
| `micro/math_sqrt` | 27.9 | 24.9 | 9.9 | 0.89x | 2.52x |
| `micro/method_call` | 25.1 | 25.8 | 8.4 | 1.03x | 3.05x |
| `micro/property_read` | 33.0 | 23.7 | 8.8 | 0.72x | 2.69x |
| `micro/range_loop` | 19.3 | 19.6 | 102.0 | 1.01x | 0.19x |
| `micro/str_long_len_index` | 7.5 | 7.3 | 6.9 | 0.97x | 1.06x |
| `micro/str_methods` | 44.2 | 34.0 | 7.1 | 0.77x | 4.76x |
| `micro/super_call` | 24.6 | 27.3 | 8.7 | 1.11x | 3.15x |
| `micro/try_raise_catch` | 45.9 | 47.3 | 8.9 | 1.03x | 5.32x |
| `micro/tuple_swap` | 0.8 | 0.8 | 2.9 | 1.00x | 0.27x |
| `micro/tuple_unpack` | 12.1 | 12.0 | 9.2 | 0.99x | 1.31x |
| `macro/fib_recursive` | 58.1 | 58.2 | 22.8 | 1.00x | 2.56x |
| `macro/json_roundtrip` | 12.9 | 12.8 | 7.8 | 0.99x | 1.65x |
| `macro/nbody` | 28.0 | 27.3 | 9.2 | 0.97x | 2.97x |
| `macro/oo_simulation` | 54.1 | 49.4 | 12.8 | 0.91x | 3.85x |
| `macro/sort_heapq` | 23.4 | 18.9 | 5.9 | 0.81x | 3.23x |
| `macro/torch_cnn` | 43.8 | 43.4 | | 0.99x | |
| `macro/torch_dataloader` | 73.6 | 74.5 | | 1.01x | |
| `macro/torch_matmul` | 4.5 | 4.4 | | 0.98x | |
| `macro/torch_matmul_big` | 19.3 | 19.3 | | 1.00x | |
| `macro/torch_mlp` | 92.6 | 93.3 | | 1.01x | |
| `macro/torch_rnn` | 199.1 | 195.2 | | 0.98x | |
| `macro/torch_small_ops` | 113.2 | 114.1 | | 1.01x | |
| `macro/torch_transformer` | 108.7 | 107.8 | | 0.99x | |
| `macro/word_count` | 31.8 | 30.9 | 10.0 | 0.97x | 3.09x |

Median in-script work, ms. Geomeans as the tool prints them:

```
geomean zipp/cpython (work_s): micro 1.62x (n=30), macro 2.98x (n=6), all 1.79x (n=36)
geomean zipp-b/zipp (work_s): micro 0.94x (n=30), macro 0.97x (n=14), all 0.95x (n=44)
geomean zipp-b/cpython (work_s): micro 1.53x (n=30), macro 2.80x (n=6), all 1.69x (n=36)
```

## Correctness

Every gate below ran at `726ddfbf` with a clean tree, the WebAssembly ones on
the modules built from it. The whole set also ran at `aa76bf5d`, two commits
earlier, with the same outcome (one test fewer, added since).

- **Test262** passes with the expected-failure manifest: 95,671 of 95,680
  executions, nothing unexpected, stale or skipped.
- **Default-feature `zipp-vm` suite:** 2,218 tests in 315 binaries, none
  failed.
- **`safe-sandbox` profile suite:** 1,720 passed, 2 failed: the two
  `audit_20260915_props` tests that failed before the pass (`props_child`,
  `props_modes_match_node`: "Maximum call stack size exceeded" in that
  profile).
- **`wasm-lite` profile suite:** 1,687 passed, 45 failed in 22 binaries.
  Every one of the 45 failed at the thirteenth pass's gated commit, whose
  run failed 47; none is new. They are tests of features Lite does not
  carry and expectations written for the full profiles.
- **`zipp-cli`** 80 passed; **`--features python --lib`** 642 passed; the
  Python integration tests 245 passed when the Python commits were
  integrated, and the corpus, lazy-import and project tests again at the
  last commit; **regress** 108 passed, and 124 with `utf16 compact-utf16
  bounded-backtracking`; **zipp-sandbox** 8 passed; **zipp-wasm** unit
  tests 11 passed.
- **`-D warnings`** is clean with `--tests` for the default features,
  `safe-sandbox`, `python`, `wasm-lite` and
  `safe-sandbox,wasm-compact-regex`.
- **Node boundary suite** on the built modules: 36 of 36 on JavaScript,
  Lite and Python + Torch, 34 of 36 on Python-base, whose two failures are
  the existing Torch-dependent checks. The suite has one more check than
  before the pass (`in-place-growth.cjs`, 12 assertions).
- **New tests:** `in_place_growth_collects` (both profiles, with its own
  reporting allocator), `shared_string_accumulators`,
  `builtin_subclass_copy`, `enumeration_key_strings`, `string_key_lane`,
  `old_array_stores`, the packed-table, key-compare, key-hash and
  number-parser unit tests, and five Python corpus programs. The four that
  pin a program's output expect Node 24's lines.

The Python corpus through the CLI matched CPython on 155 of 161 programs in
each of four modes (default, `ZIPP_PY_NOFAST=1`, `ZIPP_NOJIT=1`,
`ZIPP_PY_JIT=0`) when the Python commits were made; the six that do not are
the six that did not before (five GPU and transport programs and
`py_lib_pathlib_glob`).

`cargo check --tests` with `safe-sandbox,meter-only,python` does not
compile one test file (`python_torch_native.rs` calls two tracing methods
that profile does not have). It did not before the pass either; the gate's
feature sets do not include it.

Two things went wrong while measuring and are worth knowing. A pipeline
that was stopped kept running underneath its replacement, so two test
builds linked the same binaries at once and one timing run overlapped the
other; both were discarded and run again alone. And single rows move with
where the linker puts code: `map-set-heavy` read +2.6% [+2.1, +4.3] at
`1a567913` and -1.2% [-4.1, +1.9] at `726ddfbf`, across two commits that
change nothing the row executes.

## Not claimed

- **V8's speed on dictionary-style objects.** The word count is 2.9 times
  Node's time after the string-key change (it was 4.8), a keyed
  read-modify-write 3.7 times, 36-character keys twice. The remaining cost
  is the helper call and the dependent loads behind it; an inline cache
  keyed on the key's identity is the next step and was not built.
- **The WebAssembly interpreter's string-keyed miss.** The compiled-code
  change does nothing for the modules, where a computed read that misses
  still builds the key's `String` before the generic lookup.
- **The rows flagged slower** in the screen, above.
- **CPython's speed.** The suite is at 1.69x of CPython in-script.
- **The ceiling in straight-line code.** Memory grown in place past the
  heap ceiling with no loop and no call between is still not judged until
  the next poll.
- **A collect-before-convict ceiling.** The ceiling still convicts on the
  payload high-water mark as the README states it; whether to collect
  first, or to judge the live figure, is a decision for the user.
- **The Python variants' number parser.** `zipp-pyparse` still links
  core's table into the two Python modules (about 12 KB after Brotli each).
- Nothing here was run on Linux or by CI.
