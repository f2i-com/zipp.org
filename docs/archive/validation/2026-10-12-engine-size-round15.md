# Builtins that call compiled code, class construction, the collector's reach, fifteenth pass - 2026-10-12

Continues from `6f5d8819`, the fourteenth pass's record on `main`, through
68 commits to `41f40eba`, and was released as 0.0.23 from the commit after
it (`Prepare 0.0.23`). The first 49 (to `798ee808`) were pushed to `main`
during the pass at the user's request, before the final gates. Every
commit carries `[skip ci]`; CI and the full security workflow were
dispatched by hand on `main` before the tag, and the release workflow ran
them again at the tag.

The measurements are of `649595f8`: an ordinary release build
(`cargo build --release -p zipp-cli`, default features, not profile-guided)
and the four modules built from it, against `726ddfbf` (the fourteenth
pass's code; its modules are `r14y`), and of `20123116` against 0.0.22
(`887d2149`, and the released modules). The commits after `649595f8` are a
fix for deep bind chains in the checked profile (`20123116`) and the test
that encoded the old behaviour (`39f10de2`), a fix for a Lite regression
the release's screen found (`907851c4`,
[below](#after-the-measurements)), and a workflow change (`41f40eba`).

Three agents worked beside the main thread in their own worktrees: one on
module size (seven commits), two after each other on Python speed (sixteen
and six). Their commits were reviewed, cherry-picked and gated with the
rest.

## Outcome

- **Three wrong answers in the released build are fixed**, all in the
  native JIT's array-builtin callback window, and a fourth in the checked
  profile: a deep chain of bound functions threw where Node calls it
  ([below](#four-defects-fixed)).
- **Builtins call compiled functions cheaply.** A compiled callback runs
  frame-free over one reused register window in `map`, `filter`,
  `forEach`, `reduce`, `some`, `every`, `find`, `findIndex`, `sort`,
  `Array.from` and `Map`/`Set` `forEach`; any other builtin's call of a
  compiled function (a promise reaction, a getter, a `replace` function)
  skips the interpreter loop. Capturing callbacks took 1.8 to 4.2 times
  Node's time and take 1.0 to 1.3 times.
- **Class construction.** A class keeps its constructor's function value, a
  field is appended without a descriptor object, and a loop that constructs
  is compiled: a million constructions of a class with two fields take 274
  ms where 0.0.22 took 2,059 (Node: 34), a derived class 537 where it took
  864 (Node: 38). Construction is still several times Node's time ([Not
  claimed](#not-claimed)).
- **WebAssembly size.** Against the fourteenth pass, after Brotli:
  JavaScript 8,860 bytes smaller (0.7%), Python-base 11,553 (0.7%), Python +
  Torch 11,580 (0.6%); Lite is 2,996 bytes larger (0.4%). Against 0.0.22's
  modules every one is smaller: 1.6% to 3.8% after Brotli, 2.2% to 4.9% raw
  ([the table](#webassembly-size)).
- **WebAssembly speed.** Against 0.0.22's modules, the JavaScript module
  runs the corpus in 0.84 to 0.86 of the time and Lite in 0.84 ([the
  screen](#webassembly-speed-screen)); the Python modules were not screened.
  Lite's Unicode RegExp compile had become 17% slower in this pass by an
  inlining accident, and is fixed (`907851c4`).
- **Native.** `726ddfbf` to `649595f8` is -2.61% [-3.38%, -1.83%] over the
  13 real rows, and 0.0.22 to `20123116` -4.90% [-5.68%, -4.29%]; the
  headline ten are at 0.896x [0.890, 0.903] of Node 24 (0.914x at the
  fourteenth pass). `async-promise-chain` and `polymorphic-objects` are 8 to
  10% faster in the pass; `class-prototype-hot` is 3.2% slower ([the
  tables](#native-measurements)).
- **Python.** The native suite is at 1.30x of CPython 3.13 in-script, from
  1.69x, and at 0.80x of the previous build over its 44 programs ([the
  rows](#python-suite)). Twenty-two changes from two agents, one commit
  each.
- **Correctness.** Test262 passes with the expected-failure manifest (95,671
  of 95,680 executions) at `649595f8` and at `907851c4`. The default-feature
  `zipp-vm` suite passes (2,228 tests in 326 binaries), and the
  `safe-sandbox` suite passes in full for the first time since the props
  tests were written ([Correctness](#correctness)).

## Four defects fixed

Three are in 0.0.22's native build: each was reproduced on a build of
`887d2149`. They are in the array builtins' callback window, which came in with
`25df2f0f` on 31 May, and in the frame-call helpers it uses; how far back
each one goes was not bisected. They need the JIT: the WebAssembly
modules have none and are unaffected. Each is fixed on this branch with a
test pinned to Node's output.

**A callback's local kept the previous element's value** (`cca2c571`).
`map`, `filter`, `forEach`, `reduce`, the find family and `sort` call a
compiled callback over one register window they keep for every element,
and nothing reset it between calls. A local read before it is written
saw the previous element's value:

    [7, 1].map(function (x) { var t; if (x > 5) t = x; return t; })
    // 0.0.22: [7, 7]    Node: [7, undefined]

**A callback kept reading a deleted or redefined global** (`cca2c571`).
A callback compiled while `g` was a plain global read the slot directly,
and the window never checked the route again: after `delete globalThis.g`
it returned NaN where Node throws a ReferenceError, and after a getter
was defined on `g` it read the old value where Node calls the getter.

**A builtin run directly as a promise job panicked** (`4ecbfee8`).
`Promise.resolve().then(Array.prototype.forEach.bind(arr, f))` runs
`forEach` as a job, with no frame under it. Once `f` was compiled and
called another function, the frame-call helper indexed the empty frame
stack and the process aborted (`index out of bounds`).

**A long chain of bound functions threw in the checked profile**
(`20123116`), the profile the WebAssembly modules use. Calling or
constructing a bound function recursed once for each level of a
bind-of-bind chain, and that profile caps native recursion at 32 levels,
so a chain of 300 (`g = g.bind(null, i)` repeated) threw "Maximum call
stack size exceeded" where Node calls it. The chain is now unwrapped in a
loop. This is older than the pass: it is why
`audit_20260915_props::props_child` and `props_modes_match_node` have
failed in the `safe-sandbox` suite since they were written, and they pass
now.

## The changes

Times are wall time of the whole CLI process, minimum of five interleaved
runs, as each commit message records them; Node 24.19.0 beside where the
commit measured it. The [probe table](#probes) measures them again on the
final build.

### Builtins calling compiled functions

- `784e3eb2` a builtin's call of a function whose body is compiled enters
  the body directly once the frame is pushed, instead of through
  `run_loop` and the dispatch function's entry.
- `cca2c571` each call over an array builtin's callback window is a fresh
  activation (the first two defects above), and a capturing closure runs
  there too, with its cells installed as the native cross-call installs
  them: 6M calls of capturing `map` and `forEach` callbacks 190 to 77 ms
  (Node: 59), 600k capturing sort comparisons 211 to 82 ms (Node: 77). The
  reset costs a non-capturing callback 32 to 37 ms on the same count
  (Node: 58).
- `1ba123a5` any other builtin's call of a compiled function runs
  frame-free, as a compiled caller's cross-call does, when every condition
  of that call holds. `c9df2031` keeps a frame under it: Test262's
  `Atomics.waitAsync` timer test found the case of a job with none, which
  `1ba123a5` had broken, and it was fixed before anything else was built on
  it. `29580619` extends it to int-tier bodies (`Int32Array.prototype.map`
  with a compiled callback, 3M calls, 177 to 140 ms).
- `1f59b208` `Map` and `Set` `forEach` use the callback window: 4M calls of
  a capturing callback 98 to 52 ms (Node: 53).
- `e924e3c9` `s.replace(re, f)` takes the direct regex lane when `f` is
  callable, which is safe because the replacer only runs after the exec
  loop has collected every match: 800k matches 261 to 77 ms (Node: 62).
- `798ee808` `Array.from(array, f)` takes the array iterator's steps itself
  with no iterator or result objects (3M elements 611 to 59 ms; Node: 88),
  and `some`, `every`, `find`, `findIndex` and `Array.from` of an
  array-like use the callback window (6M predicate calls 159 to 85 ms;
  Node: 52).
- `4ecbfee8` the third defect above.

### Class construction, generators and iterators

- `ceb573d3` a class keeps its constructor's and field thunk's function
  values (`ClassData::ctor_fn`, `thunk_fn`), which every `new` made again
  with a copy of its upvalues and brand chain: a million derived-class
  constructions 780 to 546 ms, base-class 519 to 428 ms.
- `0859002d` a class field on an ordinary extensible object is appended
  directly instead of through `Object.defineProperty` with a four-property
  descriptor object: a class with two fields, a million constructions,
  1,783 to 335 ms (Node: 36).
- `649595f8` `new` compiles in memory regions and Tier C bodies through a
  construct helper; before, a loop that constructed was not compiled at
  all. A million `new P(i)` 309 to 284 ms, the two-field class 331 to
  258 ms.
- `54a63349` `for (v of gen)` takes each step without its `{value, done}`
  object: 3M steps 463 to 172 ms (Node: 62).
- `5be141fa` an iterator result object is built from one key plan the VM
  keeps, as a literal is: 3M `g.next()` and 1M Map-iterator `next()` calls
  721 to 549 ms (Node: 70).
- `0bb8c9e1`, `dc4d0f31`, `669e3ecb` the side tables that construction,
  closures and generators probe on every operation are slot tables, slot
  sets or Fx tables instead of the standard library's SipHash tables:
  `this_tdz`, `super_called`, `fn_name_cells`, `const_cells`, `gen_callee`,
  `method_brand`, `instance_brand`, `super_this`, `prototypes`,
  `closure_new_target`, `brand_owner`, `brand_private_names`, the
  read-only-length set, the per-shape key cache, and the shape tree's
  per-node index (which hashed an already-mixed hash again on every
  property add through a wide node).
- `20123116` a chain of bound functions is unwrapped in a loop (the fourth
  fix above).

### Collector and memory

- `c5eee651`, `dcc58ea4`, `44a4795e` the compiled helpers that allocate
  without a call (string concatenation, the static `Promise.resolve`,
  `substring`, a native callee) poll the collector. A compiled loop that
  built strings ran one collection in 1.5 million iterations and held
  675 MB where Node holds 30; it holds 30 MB now.
- `a4208408` the checked profile arms its footprint check from the
  allocator report and keeps the poll to three flag tests (the WebAssembly
  modules' integer and property loops 4 to 12% faster).
- `e6ae74d1` the CLI's reporting allocator takes mimalloc's plain entries
  for word-aligned requests (markdown and JSON rows 2 to 3%).
- `c1791b44` a newborn's payload is sized inline for the variants
  allocated by the million, and a slot's hot mirror is settled from the
  value in hand where the variant has none (within noise on the rows).
- `539670d9` a dead promise (and a resolver record) is freed without being
  moved out of its slot: 3M `Promise.resolve` 76 to 67 ms.

### Interpreter and object model

- `af0721c3` a shape caches its enumerable key list for `for-in` and
  `Object.keys`: 163 to 110 ms and 159 to 105 ms on the probes.
- `6a49a96c` a dictionary-mode object takes the own-key `for-in` path.
- `e3b18561` a string-key miss in the interpreter no longer builds the key
  (the WebAssembly word count 3,592 to 2,947 ms), which the fourteenth
  pass left as a lead.
- `8b9b25c5` a keyed access predicts its slot from the receiver's shape and
  the key: `o[k]` read 150 to 85 ms (Node: 70), read-modify-write 445 to
  253 ms.
- `4c34ac5e` the Map and Set index grows at half load.
- `a52215ae`, `b8ca822e` `s += t` appends in place through a two-slot
  borrow: 1,315 to 582 ms on 40,000 strings of 2,048 appends (Node: 204).
- `e34f1e9f` `join` and `String(array)` write an integer's digits directly
  (3M integers 154 to 137 ms; Node: 87).

### Compiled code (native)

- `3a9154bd` every region that pins an Array keeps the Array-epoch cache,
  which only regions with a cross-call had: a `Promise.resolve` fill loop
  93 to 78 ms.
- `ff116de0` a static op finds its namespace by the op.

### Size

- `89d0b354` Python float literals are parsed by the engine's decimal
  parser (a new crate, `zipp-numparse`), so the Python modules no longer
  link core's parser and its 10 KB table: 12 to 13 KB after Brotli off each
  Python module. The fourteenth pass left this as a lead.
- `3fd3964a`, `cfeb69fc`, `363524b3` the time zone transitions are day
  steps and times of day, the zone table is by column, the primary
  identifiers are listed: about 40 KB raw and 5 to 7 KB after Brotli off
  each full module, and time zone work a few percent faster.
- `74800613` the TypedArray default sort and two key sorts go through the
  integer sort the engine already has, so a 10 KB copy of the standard
  library's sort went. TypedArray sorts are faster and BigInt sorts exact
  ([below](#what-a-program-or-a-host-can-see)).
- `0b8994c3` the global setup's repeated steps are calls (12 KB raw).
- `997bd970` the stored-global scan is skipped where nothing reads it
  (every non-x86 build; the Python modules start 0.25 to 0.5 ms sooner).
- `e08834d0` a module names the engine's sources by their path inside the
  crate (about 1 KB raw).

### Python

Twenty-two commits from the two Python agents, one change each. Rows are
`tools/python_bench.py` programs, in-script time in ms as each commit
message records it (interleaved runs, the commit's own count; minimum or
median as the message gives it), against the build before that commit.

| Commit | Change | Rows, before to after |
| --- | --- | --- |
| `bd46e201` | an f-string's int part joins as an int, so its loop compiles | `fstring` 15.1 to 12.2 |
| `60d7e502` | a Python callee's frame is pushed from the call site's cache entry | `call_simple` 66.8 to 59.5, `fib_recursive` 59.8 to 52.9, `super_call` 28.4 to 24.4 |
| `2b217ea4` | a raise caught in its own frame is caught without leaving the compiled loop | `try_raise_catch` 49.4 to 40.5 |
| `f956c126` | a dict's missing key goes straight to the KeyError a subscript raises | `try_raise_catch` 39.7 to 33.1 |
| `2640fbe8` | a plain generator record's fields are read once a step | `generator_iter` 4% on its 10x copy |
| `28a82617` | a plain class is constructed inline and its `__init__` entered through PyCall | `instance_new` 28.1 to 24.8 |
| `d8858e98` | an imported name is asked for its module function first, through a cached entry | `math_sqrt` 0.6 to 0.7x, `nbody` 29.1 to 27.8 |
| `084b6928` | sort keys are computed through the key function's entry into a dense array | `sort_heapq` 22.1 to 20.7 |
| `9b6c54d5` | a traceback frame is recorded natively while unwinding | `oo_simulation` 55.8 to 40.6 |
| `422a38a8` | the exception record has its traceback slot from the start | a raise caught two frames up 157 to 126 (100,000 times) |
| `96defe87` | an exception class's instance is made at the call site | the same 127 to 114, `oo_simulation` 35.4 to 34.1 |
| `06cb9b12` | an int's or a str's truth is tested inline | `word_count` 36.9 to 32.9, `sort_heapq` 20.1 to 18.8 |
| `bf2d4b38` | a compiled subscript calls its own helper, not the generic Python step | `list_index` 36.6 to 32.3, `dict_str_key` 39.8 to 34.9, `nbody` 26.7 to 24.5 |
| `16e04183` | a list element is stored in place | `list_index` 32.3 to 26.5, `nbody` 24.5 to 22.4 |
| `8894e23c` | a compiled subscript store calls its own helper | `list_index` 26.8 to 23.2, `dict_str_key` 34.8 to 32.9 |
| `f1ce7da5` | a tuple key's fields are read at their slots when its record has the literal's shape | 300,000 bigram updates 249 to 220 |
| `621f798b` | small-int arithmetic and comparisons settle in the dispatch loop | `call_simple` 59.8 to 46.1, `fib_recursive` 53.3 to 42.4, `call_keywords` 13.6 to 11.0 |
| `48ab31f9` | a compiled value instruction calls its own helper | `global_read` 40.3 to 23.3, `builtin_len` 18.4 to 12.2, `builtin_isinstance` 18.4 to 12.5 |
| `f6b2af95` | a loop whose calls are of natives (which push no frame) compiles | `hash_str` 15.4 to 10.0, `str_methods` 32.2 to 29.1 |
| `5b439671` | a site's cache entry is asked in one small call before the Python step | `method_call` 21.1 to 18.3, `instance_new` 23.2 to 21.5 |
| `b213f66f` | a str method's ASCII result is made without scanning it again | `str_methods` 28.8 to 28.2 |
| `6fd12c4c` | a generator is stepped from a compiled loop through a helper of its own | `generator_iter` 52.3 to 46.5 |

`5b439671` read three rows 1 to 4% slower in every run (`oo_simulation`,
`generator_iter`, `try_raise_catch`); its message records them.

## What a program or a host can see

- **The four fixes above.** In JIT builds a callback's unassigned local
  reads `undefined`, a deleted or redefined global is seen as it is, and a
  builtin run as a promise job no longer aborts the process. In the
  checked profile (the WebAssembly modules) a deep chain of bound
  functions can be called and constructed.
- **BigInt typed arrays sort exactly** (`74800613`). `BigInt64Array` and
  `BigUint64Array` compared their elements as doubles, so two values
  closer than a double's spacing kept their order:
  `[2n**60n + 1n, 2n**60n]` stayed as it was, as did `BigUint64` values
  above 2**63. They now sort as the specification and Node sort them.
- **A class's static keys from `"4294967295"` up** list among the string
  keys in creation order (`74800613`), as Node lists them; they were
  sorted first as integers (natively any that fits a `usize`; in the
  modules `"4294967295"` itself).
- **Collections happen where they did not.** A compiled loop that only
  concatenates strings, calls `substring` or a native function, or makes
  promises through the static `Promise.resolve` now polls the collector,
  so finalization-registry callbacks and WeakRef clearing can come
  earlier.
- **The checked profile's footprint check is armed by the allocator
  report** (`a4208408`) instead of read at every poll. The figure is the
  process's, as before; with several heaps in one module, a heap whose own
  level is lower than the one armed sees its growth a step late at most.

## WebAssembly size

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125 (`-O1 -O1`), Node 24.19.0.
Brotli is quality 11 with the size hint, as the README states it. Module
bytes, raw / Brotli, Windows builds of `20123116`; "start" is `r14y`, the
fourteenth pass's modules (the same code as `726ddfbf`), and "released" is
the thirteenth pass's modules, which 0.0.22 was cut from.

| Variant | Released | Start of the pass | This pass | Against the start | Against released |
| --- | ---: | ---: | ---: | ---: | ---: |
| Lite | 2,634,413 / 710,436 | 2,573,185 / 694,745 | 2,573,530 / 697,741 | +345 (+0.01%) / +2,996 (+0.43%) | -60,883 (-2.31%) / -12,695 (-1.79%) |
| JavaScript | 4,876,583 / 1,227,452 | 4,703,604 / 1,189,469 | 4,636,144 / 1,180,609 | -67,460 (-1.43%) / -8,860 (-0.74%) | -240,439 (-4.93%) / -46,843 (-3.82%) |
| Python-base | 6,519,418 / 1,597,418 | 6,379,257 / 1,578,173 | 6,331,117 / 1,566,620 | -48,140 (-0.75%) / -11,553 (-0.73%) | -188,301 (-2.89%) / -30,798 (-1.93%) |
| Python + Torch | 8,443,452 / 1,950,080 | 8,303,296 / 1,929,593 | 8,254,998 / 1,918,013 | -48,298 (-0.58%) / -11,580 (-0.60%) | -188,454 (-2.23%) / -32,067 (-1.64%) |

Before post-processing (wasm-bindgen and the target-features strip, as the
README measures it), the same modules are Lite 2,678,584 / 700,429,
JavaScript 4,790,112 / 1,186,115, Python-base 6,519,475 / 1,574,300 and
Python + Torch 8,447,665 / 1,925,027.

The full modules lost the time zone tables' bytes (about 40 KB raw and 5
to 7 KB after Brotli each), the copy of the standard library's sort, the
global setup's repeated steps and the source paths; the Python modules
also lost core's number parser (12 to 13 KB after Brotli each), and gained
the Python agents' code. The JavaScript module's Brotli saving is smaller
than the Python modules' because it never carried that parser.

**Lite is level raw and 2,996 bytes larger after Brotli.** It has no time
zone tables and no Python, so of the size work it took only the sort, the
global setup and the paths, and it carries all of the pass's new
interpreter and object-model code. Raw, what left and what came are
level; after Brotli, what came costs more than what left saved. Builds taken
during the pass put
about 2.4 KB of the growth with the morning's interpreter changes (the
shape key cache, the string-key miss, the keyed slot prediction) and the
rest with the later class, generator and iterator changes; it was not
attributed commit by commit.

The speed screen below ran on the modules built from `649595f8`. The
modules of `20123116` differ from them in the bound-function arm of the
call and construct paths, which no screen case reaches: they are 58 to
154 bytes smaller raw, and after Brotli between 455 bytes smaller (Lite)
and 1,262 larger (Python-base).

## WebAssembly speed screen

The modules of `20123116` against the released ones (0.0.22's), Lite and
JavaScript. One process a case and configuration, pinned to one processor,
10 warm-up pairs and 31 alternating paired samples; four configurations
(released first and this pass first, at two affinities). Each cell is the
lowest and highest ratio of the four, this pass's time over the released
module's. `f` marks a case more than 2.5% faster in three of the four and
`s` one more than 2.5% slower in three of the four, the ninth pass's rule.

| Case | Lite | JavaScript |
| --- | ---: | ---: |
| `alloc-object` | 1.02 to 1.05 | 1.02 to 1.07 s |
| `arith-float` | 0.81 to 0.85 f | 0.83 to 0.91 f |
| `arith-int` | 0.79 to 0.92 f | 0.87 to 1.00 |
| `arith-mod` | 0.91 to 0.96 f | 0.89 to 1.01 f |
| `array-build` | 0.78 to 0.80 f | 0.79 to 0.83 f |
| `array-hof` | 0.89 to 0.90 f | 0.95 to 1.09 |
| `call-deep` | 0.11 to 0.12 f | 0.16 to 0.18 f |
| `enumeration` | 0.56 to 0.58 f | 0.51 to 0.60 f |
| `error-range` | 1.05 to 1.09 s | 0.92 to 0.95 f |
| `error-type` | 0.97 to 1.06 s | 0.98 to 1.05 |
| `error-typedarray` | 1.04 to 1.05 s | 1.01 to 1.02 |
| `javascript_init_0_functions` | 1.03 to 1.04 s | 0.97 to 1.00 |
| `javascript_init_200_functions` | 1.03 to 1.05 s | 1.00 to 1.01 |
| `json` | 0.98 to 1.01 | 0.96 to 1.00 |
| `promise-resolve` | 0.95 to 0.96 f | 0.96 to 1.00 |
| `prop-mono` | 0.80 to 0.81 f | 0.99 to 1.01 |
| `prop-poly` | 0.84 to 0.85 f | 0.57 to 0.97 f |
| `property-delete` | 1.05 to 1.08 s | 0.99 to 1.03 |
| `recursive-calls` | 0.97 to 1.00 | 0.98 to 1.06 |
| `regex` | 0.97 to 0.99 | 0.96 to 1.01 f |
| `regexp-matchall` | 1.04 to 1.08 s | 1.01 to 1.02 |
| `regexp-unicode-compile` | 1.17 to 1.18 s | 1.00 to 1.00 |
| `string-append-index` | 0.81 to 0.83 f | 0.82 to 0.85 f |
| `string-build` | 0.56 to 0.64 f | 0.60 to 0.61 f |
| geomean | **0.84 to 0.84** | **0.84 to 0.86** |


The screen of the two Python modules was stopped to free the machine for
the Lite investigation below and was not rerun; the fourteenth pass
measured them at 0.92 to 0.93 of 0.0.22's time. The pass's own screen,
against the fourteenth pass's modules, was stopped earlier, after 13 Lite
samples, when the release was asked for.

**Rows flagged slower.** `alloc-object` in the JavaScript module (1.02 to
1.07): every allocation is reported to the engine so that memory grown in
place is collected (the fourteenth pass measured 1.03 to 1.04 of it), and
`a4208408` added a compare to each report. In Lite, the three error rows,
`property-delete`, `regexp-matchall` and engine start (2% to 9%): the same
commit, by the bisect [below](#after-the-measurements). Lite's
`regexp-unicode-compile` (1.17 here) is fixed after this screen.

## Native measurements

Ordinary release builds (`cargo build --release -p zipp-cli`, default
features, not profile-guided), `726ddfbf` against `649595f8`, 12
interleaved repetitions, medians, paired ratios with percentile-bootstrap
intervals.

| Row | `726ddfbf` | This pass | Paired change | 95% interval |
| --- | ---: | ---: | ---: | ---: |
| `async-promise-chain` | 439 ms | 394 ms | -10.1% | -12.0% to -8.8% |
| `class-prototype-hot` | 240 ms | 247 ms | +3.2% | +2.5% to +3.7% |
| `json-large` | 290 ms | 282 ms | -1.8% | -4.4% to +2.4% |
| `map-set-heavy` | 925 ms | 906 ms | +2.0% | -4.5% to +8.5% |
| `markdown-render` | 238 ms | 239 ms | -0.8% | -2.2% to +1.6% |
| `parse-large-js` | 265 ms | 262 ms | -1.0% | -3.2% to +1.2% |
| `polymorphic-objects` | 386 ms | 354 ms | -8.1% | -8.8% to -7.0% |
| `polymorphic-objects-v2` | 30 ms | 29 ms | -3.7% | -5.3% to -0.3% |
| `property-ic-shapes` | 12 ms | 12 ms | -2.8% | -6.5% to +0.4% |
| `regex-log-scan` | 506 ms | 486 ms | -3.0% | -4.7% to -1.1% |
| `sparse-array` | 82 ms | 79 ms | -3.5% | -4.2% to -0.6% |
| `sparse-array-v2` | 113 ms | 110 ms | -2.3% | -2.9% to +0.4% |
| `typedarray-math` | 153 ms | 149 ms | -1.4% | -5.7% to +0.6% |


Geomean of the paired ratios: 0.9748x [0.967, 0.981] over the 10 headline
rows, 0.9709x [0.955, 0.993] over the 3 diagnostic rows
(`polymorphic-objects-v2`, `property-ic-shapes`, `sparse-array-v2`) and
0.9739x [0.966, 0.981] over all 13. Every row prints the same bytes in both
builds.

Against 0.0.22 (`887d2149`), the same way, with the release's code
(`20123116`):

| Row | 0.0.22 | This pass | Paired change | 95% interval |
| --- | ---: | ---: | ---: | ---: |
| `async-promise-chain` | 496 ms | 406 ms | -18.2% | -20.3% to -16.9% |
| `class-prototype-hot` | 240 ms | 248 ms | +3.2% | +2.2% to +5.0% |
| `json-large` | 345 ms | 296 ms | -15.0% | -17.1% to -13.3% |
| `map-set-heavy` | 986 ms | 987 ms | -1.3% | -6.8% to +3.9% |
| `markdown-render` | 245 ms | 243 ms | -1.3% | -3.6% to -0.2% |
| `parse-large-js` | 267 ms | 267 ms | +1.3% | -0.9% to +3.0% |
| `polymorphic-objects` | 430 ms | 352 ms | -18.3% | -20.1% to -17.3% |
| `polymorphic-objects-v2` | 30 ms | 30 ms | -3.5% | -6.0% to -1.7% |
| `property-ic-shapes` | 12 ms | 12 ms | +0.7% | -3.8% to +2.2% |
| `regex-log-scan` | 515 ms | 507 ms | -1.3% | -3.8% to +0.2% |
| `sparse-array` | 82 ms | 80 ms | -3.0% | -4.8% to -0.6% |
| `sparse-array-v2` | 113 ms | 111 ms | -2.0% | -3.1% to -1.1% |
| `typedarray-math` | 150 ms | 149 ms | -1.3% | -3.5% to +1.2% |


Geomean of the paired ratios: 0.9413x [0.934, 0.948] over the headline
ten, 0.9841x [0.963, 0.995] over the diagnostic three and 0.9510x
[0.943, 0.957] over all 13 (-4.90%, [-5.68%, -4.29%]). Every row prints
the same bytes. This run had background load the machine's other
programs made (about 14% of the processors, mostly the virus scanner and
a browser), which the absolute times show (`map-set-heavy` reads 987 ms
here and 688 in the Node run below); the ratios are paired and
interleaved.

**`class-prototype-hot` is 3.2% slower** in both runs ([+2.5%, +3.7%]
against `726ddfbf`, [+2.2%, +5.0%] against 0.0.22; the fourteenth pass
measured +0.3% against 0.0.22). Builds kept from the pass, minimum of 9
interleaved runs on one processor, put the step between `c5eee651`
(242 ms, as `726ddfbf`) and `0b8994c3` (249 ms), nine commits.
The likeliest of the nine is `44a4795e`, which polls the collector on
entry to every compiled region call (only a native callee needed it; a
user callee polls again at its frame push), and this row makes about 64
million such calls; it was not confirmed with a build of each commit.

Against Node 24.19.0, 10 repetitions:

| Row | Node 24 | This pass | This pass / Node | 95% interval |
| --- | ---: | ---: | ---: | ---: |
| `async-promise-chain` | 346 ms | 385 ms | 1.12x | 1.09 to 1.14 |
| `class-prototype-hot` | 305 ms | 242 ms | 0.80x | 0.78 to 0.80 |
| `json-large` | 261 ms | 258 ms | 0.98x | 0.94 to 1.00 |
| `map-set-heavy` | 784 ms | 688 ms | 0.89x | 0.85 to 0.94 |
| `markdown-render` | 279 ms | 217 ms | 0.78x | 0.77 to 0.79 |
| `parse-large-js` | 278 ms | 254 ms | 0.92x | 0.91 to 0.94 |
| `polymorphic-objects` | 336 ms | 342 ms | 1.02x | 1.01 to 1.03 |
| `polymorphic-objects-v2` | 84 ms | 28 ms | 0.33x | 0.33 to 0.34 |
| `property-ic-shapes` | 264 ms | 10 ms | 0.04x | 0.04 to 0.04 |
| `regex-log-scan` | 483 ms | 451 ms | 0.92x | 0.90 to 0.98 |
| `sparse-array` | 81 ms | 76 ms | 0.93x | 0.90 to 0.95 |
| `sparse-array-v2` | 174 ms | 108 ms | 0.62x | 0.61 to 0.63 |
| `typedarray-math` | 211 ms | 144 ms | 0.68x | 0.67 to 0.70 |


Geomean: 0.8958x [0.890, 0.903] of Node over the 10 headline rows (the
fourteenth pass measured 0.914x), 0.6345x [0.631, 0.638] over all 13.
`async-promise-chain` (1.12x) and `polymorphic-objects` (1.02x) are the
two rows where Node is faster.

## Probes

Wall time of the whole process, minimum of 5 interleaved runs, ms, taken
while the machine was compiling (the ratios are large; the absolute times
carry that load). Every engine prints Node's output.

| Probe | 0.0.22 | This pass | Node 24 |
| --- | ---: | ---: | ---: |
| A million constructions of a class with two fields | 2,059 | 274 | 34 |
| A million constructions of a derived class | 864 | 537 | 38 |
| 3M steps of a generator in `for-of` | 538 | 170 | 65 |
| Word count, `counts[w] = (counts[w] \|\| 0) + 1` | 640 | 377 | 140 |
| 6M capturing `map` and `forEach` callbacks | 232 | 79 | 64 |

## Python suite

`tools/python_bench.py`, 44 programs, 7 repetitions, in-script work time,
`726ddfbf` against `649595f8`, CPython 3.13 beside:

| Program | `726ddfbf` | This pass | CPython 3.13 | This pass / `726ddfbf` | This pass / CPython |
| --- | ---: | ---: | ---: | ---: | ---: |
| `micro/attr_read_write` | 3.5 | 3.4 | 8.0 | 0.98x | 0.43x |
| `micro/builtin_isinstance` | 19.2 | 12.2 | 9.6 | 0.64x | 1.27x |
| `micro/builtin_len` | 17.7 | 11.8 | 12.2 | 0.67x | 0.97x |
| `micro/call_defaults` | 26.1 | 18.5 | 12.9 | 0.71x | 1.43x |
| `micro/call_keywords` | 13.7 | 10.5 | 8.2 | 0.77x | 1.29x |
| `micro/call_simple` | 67.9 | 44.4 | 37.9 | 0.65x | 1.17x |
| `micro/dict_get` | 13.7 | 11.9 | 11.5 | 0.87x | 1.04x |
| `micro/dict_str_key` | 39.1 | 32.8 | 21.1 | 0.84x | 1.56x |
| `micro/float_arith` | 2.4 | 2.3 | 9.7 | 0.98x | 0.24x |
| `micro/for_list` | 17.1 | 16.2 | 17.2 | 0.95x | 0.94x |
| `micro/fstring` | 15.7 | 11.4 | 3.6 | 0.73x | 3.18x |
| `micro/generator_iter` | 60.4 | 43.3 | 14.9 | 0.72x | 2.91x |
| `micro/global_read` | 38.8 | 22.7 | 53.4 | 0.58x | 0.42x |
| `micro/hasattr_miss` | 5.0 | 4.5 | 2.9 | 0.90x | 1.58x |
| `micro/hash_str` | 16.8 | 9.5 | 8.0 | 0.57x | 1.18x |
| `micro/instance_new` | 26.8 | 21.6 | 5.9 | 0.80x | 3.64x |
| `micro/int_arith` | 71.9 | 67.6 | 147.2 | 0.94x | 0.46x |
| `micro/list_append` | 11.2 | 9.3 | 5.3 | 0.83x | 1.77x |
| `micro/list_index` | 35.7 | 22.6 | 27.1 | 0.63x | 0.84x |
| `micro/listcomp_small` | 16.4 | 15.0 | 7.5 | 0.91x | 2.01x |
| `micro/math_sqrt` | 25.0 | 13.8 | 10.3 | 0.55x | 1.34x |
| `micro/method_call` | 26.5 | 17.7 | 9.0 | 0.67x | 1.97x |
| `micro/property_read` | 24.1 | 22.5 | 9.2 | 0.94x | 2.45x |
| `micro/range_loop` | 20.2 | 19.9 | 107.9 | 0.99x | 0.18x |
| `micro/str_long_len_index` | 7.5 | 5.6 | 7.1 | 0.74x | 0.79x |
| `micro/str_methods` | 34.1 | 28.8 | 7.3 | 0.84x | 3.92x |
| `micro/super_call` | 28.2 | 20.3 | 8.6 | 0.72x | 2.36x |
| `micro/try_raise_catch` | 48.9 | 30.4 | 9.2 | 0.62x | 3.30x |
| `micro/tuple_swap` | 0.8 | 0.8 | 3.0 | 0.93x | 0.26x |
| `micro/tuple_unpack` | 12.8 | 9.3 | 9.5 | 0.73x | 0.97x |
| `macro/fib_recursive` | 60.0 | 41.2 | 23.3 | 0.69x | 1.77x |
| `macro/json_roundtrip` | 13.4 | 13.6 | 8.0 | 1.02x | 1.69x |
| `macro/nbody` | 28.1 | 20.0 | 9.4 | 0.71x | 2.13x |
| `macro/oo_simulation` | 51.6 | 31.3 | 13.1 | 0.61x | 2.38x |
| `macro/sort_heapq` | 19.4 | 16.9 | 6.1 | 0.87x | 2.76x |
| `macro/torch_cnn` | 45.3 | 44.0 |  | 0.97x |  |
| `macro/torch_dataloader` | 78.5 | 81.4 |  | 1.04x |  |
| `macro/torch_matmul` | 4.6 | 4.8 |  | 1.03x |  |
| `macro/torch_matmul_big` | 19.9 | 19.8 |  | 1.00x |  |
| `macro/torch_mlp` | 99.1 | 93.2 |  | 0.94x |  |
| `macro/torch_rnn` | 214.0 | 203.2 |  | 0.95x |  |
| `macro/torch_small_ops` | 123.6 | 114.7 |  | 0.93x |  |
| `macro/torch_transformer` | 114.4 | 109.3 |  | 0.96x |  |
| `macro/word_count` | 32.6 | 27.0 | 10.4 | 0.83x | 2.59x |


Median in-script work, ms. Geomeans as the tool prints them:

```
geomean zipp/cpython (work_s): micro 1.52x (n=30), macro 2.82x (n=6), all 1.69x (n=36)
geomean zipp-b/zipp (work_s): micro 0.77x (n=30), macro 0.88x (n=14), all 0.80x (n=44)
geomean zipp-b/cpython (work_s): micro 1.17x (n=30), macro 2.18x (n=6), all 1.30x (n=36)
```

Eleven of the 36 programs CPython runs take less time here than in CPython
(seven at the fourteenth pass). The slowest against CPython are
`str_methods` (3.92x), `instance_new` (3.64x), `try_raise_catch` (3.30x)
and `fstring` (3.18x). Three rows read slower than at the start:
`json_roundtrip` 1.02x, `torch_dataloader` 1.04x and `torch_matmul` 1.03x,
the last two Torch rows of 5 to 80 ms whose work is mostly in the
kernels; they were not investigated.

## Correctness

**Locally, at `649595f8`**, with a clean tree, every gate ran, the
WebAssembly ones on the modules built from it:

- **Test262** passes with the expected-failure manifest: 95,671 of 95,680
  executions, nothing unexpected, stale or skipped.
- **Default-feature `zipp-vm` suite:** 2,228 tests in 326 binaries, none
  failed.
- **`safe-sandbox` profile suite:** 1,728 passed, 2 failed: the two
  `audit_20260915_props` tests that failed before the pass (the bound chain,
  fixed in the next commit).
- **`wasm-lite` profile suite:** 1,695 passed, 45 failed in 22 binaries, the
  same 45 tests by name as at `726ddfbf`: tests of features Lite does not
  carry and expectations written for the full profiles.
- **`zipp-cli`** 80 passed; **`--features python --lib`** 641 passed; the
  Python integration tests 238 in 33 binaries; **regress** 108 passed, and
  124 with `utf16 compact-utf16 bounded-backtracking`; **zipp-sandbox** 8
  passed; **zipp-wasm** unit tests 11 passed.
- **`-D warnings`** is clean with `--tests` for the default features,
  `safe-sandbox`, `python`, `wasm-lite`, `safe-sandbox,wasm-compact-regex`
  and `safe-sandbox,python`, and without `--tests` for
  `safe-sandbox,meter-only,python`.
- **Node boundary suite:** 36 of 36 on JavaScript, Lite and Python + Torch,
  34 of 36 on Python-base, whose two failures are the existing
  Torch-dependent checks.
- **The Python corpus** through the CLI matches CPython on 174 of 179
  programs in each of four modes (default, `ZIPP_PY_NOFAST=1`,
  `ZIPP_NOJIT=1`, `ZIPP_PY_JIT=0`). The five that do not are the GPU and
  transport programs (`gpu_graph`, `ml_gpu_ops`, `ml_gpu_prepared`,
  `ml_gpu_session`, `ml_transport_graph_reference`), which failed before
  the pass too.

**After it.** `20123116` (the bound chain) was checked locally with
Test262's `built-ins/Function`, `built-ins/Reflect/construct`,
`language/expressions/new` and `built-ins/Proxy/construct` (1,089
executions, none failed), the `safe-sandbox` suite (1,729 passed, 1
failed), and the Node boundary suite on its modules (as above). The one
failure was `native_meta_recursion_limits`, which asserted that a 512-deep
bind chain throws in that profile; it is called now, as Node calls it, and
`39f10de2` makes the test expect Node's lines. The security workflow's
safe-interpreter job had found the same test on GitHub. With it, the
profile's suite passes in full, `props_child` and `props_modes_match_node`
included.

**After that**, `907851c4` (the regex change) was checked with full
Test262 on a release CLI built from it (95,671 of 95,680 executions with the
manifest, nothing unexpected, stale or skipped), the regex crate's
tests (109 passed, and 125 with `utf16 compact-utf16
bounded-backtracking`, one new each), and the Lite screen in
[After the measurements](#after-the-measurements).

**On GitHub**, dispatched by hand on `main`: CI at `20123116` passed every
job. The full security workflow at `39f10de2` passed every job it
finished; its native workspace suite was cancelled after 72 minutes to
change that job (`41f40eba`, below), and the workflow ran again on the
release commit before the tag. The release workflow runs both again at
the tag.

**New tests:** twelve `zipp-vm` test files (`array_callback_window`,
`builtin_calls_compiled`, `replace_function_direct`, `array_from_steps`,
`class_construction`, `generator_for_of`, `iterator_result_objects`,
`compiled_new`, `bound_chains`, `compiled_concat_collects`,
`enum_key_cache`, `keyed_slot_memo`), the sort tests of `74800613`, the
`zipp-numparse` unit tests, and 18 Python corpus programs. The tests that
pin a program's output expect Node 24's lines; the corpus programs expect
CPython 3.13's.

**Metering.** Over fourteen probe programs (loops, calls, property reads,
`push`, `for-of` over arrays and generators, `next()`, classes,
constructors, callbacks, string appends, `for-in`, bound calls,
`Array.from`, `replace`), the JavaScript module charges exactly the steps
0.0.22's charges. Python programs charge different counts: the runtime's
start charges about 270 steps more (246 of them since 0.0.22's modules, at
the fourteenth pass), and a loop of property reads, a comprehension, a
raise-and-catch loop and a keyed sort charge 2,000 to 4,600 fewer.

## After the measurements

**Lite's Unicode RegExp compile** (`907851c4`). The screen against 0.0.22
read Lite's `regexp-unicode-compile` 17% slower, and the JavaScript
module's level. Lite builds of the pass's commits, screened against the
fourteenth pass's module on that case, put the whole step at `a4208408`
(the first commit, which changes only the collector's footprint check); a
fresh Lite build of `726ddfbf` is byte-identical to the fourteenth pass's
module, and the unoptimized modules of the two commits ran the case at
the same speed. A profile of the two, post-processed as shipped with
names kept, showed why: the size-first build (`opt-level = "z"`) stopped
inlining `RangeInclusive::next` into the loop that collects a class's
UTF-8 first bytes, called once for each of `\p{Letter}`'s hundreds of
intervals. `cps_to_first_byte_bitmap` went from 306 to 384 ms of the
profile and the iterator's `next` took 63 ms more on its own. At "z" LLVM
inlines such a call by accident of the whole program. The loop now cuts
an interval only where its encoded length changes and sets each piece's
first bytes a 16-bit word at a time (`ByteBitmap::set_range`), with no
iterator to inline. 20,000 compiles in steady state: 542 and 594 ms at
`726ddfbf`, 659 and 663 at `20123116`, 508 and 501 with the fix. The Lite
module is 45 bytes smaller raw and 113 after Brotli.

Over the screen's corpus, that Lite build against the fourteenth pass's
(one round of both orders, on one processor while another program
compiled): `regexp-unicode-compile` 0.93 and 0.94, `enumeration` 0.61,
`string-build` 0.85 and 0.92, `arith-int` 0.94; slower, `alloc-object`
1.06 and 1.11, `property-delete` 1.05 and 1.07, `error-range` 1.03 and
1.05, `regexp-matchall` 1.02 and 1.03, engine start 1.01 to 1.02. The
same bisect puts those at `a4208408` too, 3% to 5% there; profiles of
`alloc-object` on the two builds show the same functions in the same
order. That is the compare the commit added to every allocation report,
kept for the poll it took out of every loop.

**The security workflow's native workspace suite** (`41f40eba`). It took
86 minutes of a release's time, where every other job takes under 17: 2
minutes compiling and 84 running, a third of that in three files that
rerun their programs under `ZIPP_GC_STRESS` and the other collector and
tier modes on an unoptimized build (`nursery_minor` 20 minutes,
`proto_method_shadow` 12, `audit_20260915_gc_heap` 8.5). The job now
compiles its tests at opt-level 1, with debug assertions and overflow
checks on as before. On a workstation the three took 19, 7 and 16 seconds
at level 1 and 628, 65 and 359 at level 0, for 107 seconds of compile
instead of 33.

## Tried and dropped

- **A key plan per constructor.** Properties a plain constructor adds in
  the same order every time were to be laid down from a plan kept for the
  function, as a literal's are. It was slower: a million `new A(i)` with
  two `this.x =` stores went from 286 to 339 ms, because the object map's
  position lookup and the construct path each grew a test. Reverted.
- **Entering a capturing callback's activation once per builtin call**,
  instead of once per element: four times slower on the capturing probe.
  Discarded.
- **A memo for a concatenated key's text:** no gain on its probe.
- **The site's cache hit inlined into the Python step** (four forms):
  slower on most rows (`fib_recursive` 1.05x, `word_count` 1.18x of the
  build before); the out-of-line call of `5b439671` was kept.
- **Packed DateTimeFormat pattern tables:** deleting the per-calendar
  pattern tables outright would save 42.7 KB raw and 3.9 KB after Brotli,
  and the packed form compresses worse. Not done.

## Not claimed

- **V8's speed on object construction.** A derived class takes 537 ms a
  million and Node 38, a class with two fields 274 and Node 34. Each new
  object still adds its properties
  one transition at a time through a helper; an inline cache for the add
  transition (shape before, shape after, slot) is the next step and was
  not built.
- **Generators.** `for-of` over a generator is 2.6 times Node's time and
  a `g.next()` loop 7.8 times.
- **A frame-free call from an `async` function's promise chain.** Its jobs
  run with no frame under them, so they keep `call_value`; the native
  `async-promise-chain` row is 1.12x of Node.
- **The rows flagged slower** in the screen, above.
- **CPython's speed.** The suite is at 1.30x of CPython in-script.
- **Lite's size.** Lite is 2,996 bytes larger after Brotli than at the
  start of the pass ([above](#webassembly-size)).
- **The allocation rows in Lite.** `a4208408`'s per-allocation compare
  costs Lite's allocation-heavy rows 3% to 5% and was kept for the loops
  it makes faster.
- Nothing here was run on Linux or by CI.
