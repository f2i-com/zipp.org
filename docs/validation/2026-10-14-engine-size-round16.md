# Compiled loops for the rest of the language, construction, Python, torch and a smaller regex engine, sixteenth pass - 2026-10-14

Continues from `5cffc37a` (0.0.23's code and the commits that recorded
its release) through 63 commits and 4 merges to `1df54024`, on the local
branch `perf-round16`. Nothing is pushed and no release is cut. Every
commit carries `[skip ci]`.

Four agents worked beside the main thread in their own worktrees: Python
speed (13 commits), WebAssembly size (7), the torch implementation (14)
and the regex engine (13, added mid-pass at the user's request to make
`regress-fork` ZIPP's own). Their branches were reviewed and merged
whole (`04c91998`, `0e9feaad`, `bab3e05f`, `1df54024`); the merged state
was gated as one.

The measurements are of `1df54024`: an ordinary release build
(`cargo build --release -p zipp-cli`, default features, not
profile-guided) and the four modules built from it, against `5cffc37a`
(`head-5cffc37a.exe`, and the modules `r16base`).

## Outcome

- **Nine wrong answers in 0.0.23 are fixed, and one performance defect**
  ([below](#defects-fixed)). JavaScript, five: iterators lost their own
  properties, a replaced generator method was ignored, object spread ran
  setters and could replace the prototype, and a Proxy's `ownKeys` ran
  twice per `for-in` level. torch, four: optimizer steps replaced the
  parameters, `randn`/`normal_` did not follow PyTorch's stream, float64
  `rand` lost precision, GRU's update differed. And `pop` on a large
  array took time proportional to its length.
- **JavaScript: most of the loops that compiled nothing now compile.**
  A loop that builds literals, creates closures, tests `instanceof`,
  switches on `typeof`, fills a template, pops, destructures, spreads,
  lists keys or runs a `for-in` declined its compiled region and ran on
  the interpreter; functions with a rest parameter were never compiled.
  Those patterns run 1.2 to 6 times faster than in 0.0.23 (construction
  2x, `instanceof` 3.4x, object spread 6x, a `push`/`pop` loop 15
  minutes to 123 ms); the
  [probe table](#probes) has 43 patterns against 0.0.23 and Node 24.
- **Native, the 13 real rows: level with 0.0.23.** A first A/B read
  1.0111x [1.003, 1.016] over the headline ten, a small slowdown; it did
  not replicate on fresh copies of both binaries, which read 1.0033x
  [0.996, 1.010] (all 13: 0.9973x [0.992, 1.003]). The pass's work is in
  patterns these rows barely use. Against Node 24 the headline
  ten are at **0.905x** [0.893, 0.912] and all 13 at 0.638x;
  `async-promise-chain` (1.14x) and `polymorphic-objects` (1.05x) are
  the two rows where Node is faster ([the tables](#native-measurements)).
- **Python: 1.31x to 1.21x CPython 3.13's time** over the 36-program suite
  (micro 1.18x to 1.11x, macro 2.22x to 1.84x), measured in one run
  beside 0.0.23; `json_roundtrip` and `dict_get` are now faster than
  CPython ([the table](#python-suite)).
- **torch: 16.7 to 5.2 times PyTorch's warm time** on one thread (geomean
  of the eight programs, the same run); on a cold first run ZIPP is
  faster than PyTorch on `cnn`, `mlp`, `rnn` and `transformer`
  ([the table](#torch-against-pytorch)).
- **WebAssembly size, all four modules smaller** after Brotli: Lite
  -19,957 bytes (-2.86%), JavaScript -38,613 (-3.27%), Python-base
  -40,480 (-2.58%), Python + Torch -22,355 (-1.17%; the torch kernels
  grew it 88 KB raw) ([the table](#webassembly-size)).
- **WebAssembly speed is level** with 0.0.23's modules: Lite 1.003,
  JavaScript 0.990 (geomean of 24 cases, both orders); the regex cases
  read 2 to 3.5% slower in both modules ([the screen](#webassembly-speed-screen)).
- **The regex crate is about a third smaller** and is now tested against
  Node by a differential fuzzer; two backtracking-heavy patterns are 4 to
  30 times faster.

## Defects fixed

Nine wrong answers in 0.0.23 are fixed (five in JavaScript, four in
torch), and one performance defect, each found by a differential program
against Node 24 or PyTorch and pinned by the commit that fixes it:

- **An iterator's own properties were dropped and a replaced generator
  `next`/`return`/`throw` was ignored** (`5723ea2a`). A user iterator
  object with own data beside `next` lost it across `for-of`, spread,
  `yield*` and `for await`; replacing `gen.next` (own or on the
  prototype) changed nothing a loop did.
- **`{ ...src }` ran setters and could replace the prototype**
  (`c8aa305c`). Object spread was `Object.assign`: a setter on
  `Object.prototype` ran for each copied key, and
  `{ ...JSON.parse('{"__proto__": {...}}') }` got that object as its
  prototype. Spread is CopyDataProperties now: own data properties
  defined on the new object.
- **A Proxy's `ownKeys` trap ran twice per `for-in` level** (`472897d5`);
  Node runs it once.
- **`pop` on a large array took time proportional to its length**
  (`c51ffa4c`): 80,000 pops off a 160,000-element array took 2.3 s
  (46 ms now), three million push/push/pop iterations several minutes.
- **torch: optimizer steps replaced parameter tensors instead of
  updating them in place**, `randn`/`normal_` did not follow PyTorch's
  stream, float64 `rand` lost precision, and GRU's hidden update used a
  different formula than PyTorch's (`30d21b0f`, `76e4ec9e`, `3c9093f3`).
  `randn` and `normal_` now match PyTorch bit for bit for float32 and
  float64.

## The changes

Times are wall time of the whole CLI process (Node's start is about 35 to
40 ms of its figures), minimum of five or seven interleaved runs, as each
commit message records them; the machine was loaded by the agents'
builds for most of the pass, which the absolute figures carry. The
[probe table](#probes) measures them again on the final build.

### JavaScript: loops that compiled nothing

A memory region (a compiled loop) declined whole when it held one
instruction it had no arm for, and the loop then ran on the interpreter.
The pass gives most of the remaining ones an arm, each through a helper
that runs the interpreter's own code for the instruction (shared with
the interpreter arm where the arm was long), with the call protocol where
user code can run: a throw leaves the region with the exception pending,
the site refetches its pinned bases and TypedArray snapshot, and the
call depth is counted. Helpers that allocate every iteration poll the
collector first.

| Commit | Instructions | Loop, before to after (Node 24) |
| --- | --- | --- |
| `4515054f` | `NewArray`, `NewObject`, `NewPlannedObject`, `AppendDataProp`, `FinalizeObject` | 2M `{ a: i, b: { c: i + 1 } }` 199 to 119 ms (39) |
| `4515054f` | `InstanceOfDyn` (and an `instanceof` fast path, below) | 5M `b instanceof A`, derived class, 330 to 104 (42) |
| `4515054f` | `TypeOf` unfused | 3M `switch (typeof v)` 154 to 58 (42) |
| `4515054f` | `ToStr` (a template's `${v}`) | 1M `` `a${i}b${i & 7}` `` 189 to 176 (50) |
| `ebfb8db1` | `MakeFunc`, `MakeClosure`, `MakeArrow`, `MakeCell*`, `MarkCellConst` | 3M `var f = (x) => x + i; f(1)` 262 to 132 (4 in-loop) |
| `496e23b8` | `a.pop()` (a lane of its own, and the call-mix gate admits it) | 3M push, push, pop 277 to 165 (90) |
| `054f54d4` | `IterToArray`, `ArrayAppend`, `CallSpread` | 3M `var [x, y, z] = arr` 262 to 178 (46); 2M `f(...arr)` 248 to 214 (50) |
| `175e7299` | `ObjectKeys`, `ObjectValues`, `ObjectEntries`, `ForInKeys`, `LenOf` | 1M `Object.keys(o).length` 118 to 87 (47); 500K `for (k in o)` 122 to 97 (43) |
| `bc1d0ceb` | `ObjectSpread`, `InitDataProp` | 1M `{ ...o, c: i }` 184 to 173 (48) |

Closure creation in a region (`ebfb8db1`) takes the region's frame as the
activation: the new helper `jit_region_make_fn` checks the top frame is
the creating function's and the window is its own, validates every
capture source before the safe point, and runs
`Vm::make_closure_in_frame`, which the interpreter's `MakeClosure` and
`MakeArrow` arms now call too, so a region-made closure inherits `this`,
`[[HomeObject]]`, `new.target`, a class body's private scope, an
EvalScope and the realm exactly as an interpreted one.

### JavaScript: construction, iterators, calls

- `5fb80a81` objects are constructed through compiled fast paths: the
  JIT's add ways cover class instances, a constructor's key plan is
  shared by shape, and `new` of a plain function is direct (and
  frame-free where its body allows). 1M `new A(i)` of a two-property
  constructor 318 to 158 ms, a class 424 to 200, a constructor function
  336 to 180 (Node about 40 each).
- `ef0a5057` the interpreter's set IC caches a property append
  (`AddData`/`AddClass` entries): constructions without the JIT 75 to
  60 ms and 90 to 63.
- `e0de4a8d` a constructor's frame runs in the interpreter loop
  (`Frame::construct_this`) instead of a nested `run_loop` per `new`.
- `a2d2f938` private fields live in a slot table keyed by interned names
  instead of a cloned name per instance: construction with private
  fields 825 to 366 ms.
- `5723ea2a` `it.next()` steps generators and built-in iterators
  directly from compiled code, and the call-mix gate lets a loop holding
  one compile: 3M Map-iterator `next()` 594 to 261 ms (46), 3M
  `g.next()` 413 to 323 (68). With the iterator fixes above.
- `4515054f` `x instanceof C` answers a plain function or class without
  the general `@@hasInstance` lookup (%Function.prototype%'s is
  non-writable and non-configurable) and reads `C.prototype` directly;
  every other shape takes the general path, having done nothing
  observable. The TypeError for a non-object prototype names it as Node
  does.
- `fed9e60d` Tier C compiles functions with a rest parameter, and the
  generic frame-free cross call builds their array from the arguments
  past the parameters; every other frame-free entry declines such a
  body. 2M calls of `function rp(...xs)` 161 to 135 ms (41).
  `ZIPP_NO_TIERC_REST=1` restores the old gate.
- `75a6ea71` an `arguments` object's own properties come from one shared
  key plan instead of three key strings and growing vectors per call:
  2M calls 475 to 305 ms (43). Functions that use `arguments` are still
  not compiled.
- `4515054f` the interpreter's string-constant cache has a dense mirror by
  function and slot: 14% of an interpreted `switch (typeof x)` loop.

### Python

Thirteen commits from the Python agent (`04c91998`). Suite geomean
against CPython 3.13, its final run: 1.34x to 1.23x (micro 1.20x to
1.13x, macro 2.26x to 1.92x). Rows, in-script ms as each commit message
records them:

| Commit | Change | Rows, before to after |
| --- | --- | --- |
| `840e3fe4` | a str method's native is called from a `PyCall` site without finding its callee again (compiled code only after `e92e2da8`) | `hash_str` -13%, `math_sqrt` -10% |
| `e8f79785` | an f-string's parts are joined in one native call | `fstring` 11.0 to 8.8 |
| `7a893ab1` | a runtime record is allocated from its template's key plan without cloning it | class construction -7%, `try_raise_catch` -4% |
| `0d5eeb20` | a plain class is constructed from a compiled loop through `jit_py_new` | `instance_new` -5% |
| `35e3373f` | five more compiled Python instructions have helpers of their own | `hasattr_miss` -26%, `dict_get` -14% |
| `83f1d212` | a property that only reads or stores an attribute is answered without its frame | `property_read` 22.5 to 12.8 |
| `54aa8677` | a loopless function reads a global from its own record | `fib` -6 to -8% |
| `03fe8a09` | a container's truth is tested by its length | `sort_heapq` -10% |
| `8b2f516b` | `==` of two exact lists or dicts is native | `json_roundtrip` 13.4 to 7.0 |
| `4d9bf5e7` | a loop over a list, tuple or range starts without `seqiter` | `nbody` -10% |
| `b3b39c7c` | `str % values` is formatted in the arithmetic step | `sort_heapq` -7% |
| `ffd3aede`, `e92e2da8` | a test; the interpreter's call step kept as it was (840e3fe4's interpreter part had cost 3-4% a call) | |

### Machine learning (torch)

Fourteen commits from the ML agent (`bab3e05f`): per-op interpreter and
module overheads cut, the DataLoader batched and gathered with
`index_select`, matmul tiled, `arange` filled natively, SGD and Adam as
one kernel per parameter, LSTM and GRU cells as fused kernels with two
autograd nodes, add/sub/mul/div on their direct path at every size, the
native elementwise loops run straight over the typed arrays, and
`tools/python_bench.py` able to time the torch rows against real
PyTorch (`--torch-cpython`, `--torch-threads`, `--warm`). Every speed
commit leaves values bit-identical (checked by `float.hex` output of the
differential programs and the benchmarks, plus each commit's own
byte-for-byte test); the four correctness commits move values toward
PyTorch's. `docs/TORCH_COMPATIBILITY.md` says where eager torch matches
PyTorch's bits and where not.

### WebAssembly size work

Seven commits from the size agent (`0e9feaad`): functions laid out by
signature and body with redundant sets dropped in `optimize-wasm.sh`
(9 to 12 KB of each module after Brotli), DateTimeFormat patterns read
without building literal strings and its skeleton, interval and name
lists packed into strings at compile time, combining marks ordered
without the standard library's stable sort, likely subtags found through
a bitmap of language codes, decomposition texts by length, and tz first
transitions and final rules taken from the counts.

### Regex engine

Thirteen commits from the regex agent (`1df54024`), toward an engine of
ZIPP's own: a benchmark set timed in-process (`bench/regex/`), a
differential fuzzer against Node 24 whose 306 minimised disagreements
run as `tests/regex_fuzz_corpus.rs`, the unused PikeVM backend removed,
lookbehind bodies as right-to-left instructions (one matcher per input
type instead of two), lazy one-character loops that read ahead only as
far as the match needs, Unicode property names looked up in tables, one
search loop per input type instead of six, one byte-sequence instruction
instead of sixteen, and a subject converted to UTF-16 without the
per-unit iterator. The crate's code is about a third smaller (207,778 to
137,412 bytes of named functions in Lite, 329,120 to 218,652 in
JavaScript); lazy-scan 22.1 to 0.74 ms natively and 48 to 1.9 ms in the
JavaScript module, backref 13.4 to 2.3 ms natively. About 200,000 fuzz
cases per engine change found no difference against the start build.

## What a program or a host can see

- **The fixes above.** Iterators keep their own properties and a replaced
  generator method is honoured everywhere; object spread defines instead
  of setting; a Proxy's `ownKeys` runs once per `for-in` level, so a trap
  that counts or logs sees Node's count; torch's optimizers update in
  place and its random streams are PyTorch's.
- **A `for-in` over a Proxy calls its traps in V8's order**: `ownKeys`,
  then `getOwnPropertyDescriptor` per string key, then `getPrototypeOf`.
- **The `instanceof` TypeError for a non-object prototype** reads
  "Function has non-object prototype 'undefined' in instanceof check", as
  Node's does.
- **More code runs compiled.** Loops that build literals, create
  closures, destructure, spread, list keys or pop, and functions with a
  rest parameter, now run native code in JIT builds; each helper polls
  the collector where it allocates, so finalization callbacks and WeakRef
  clearing can come at different points than before.
- **Same-binary switches added:** `ZIPP_NO_TIERC_REST=1` (rest functions
  stay out of Tier C). The existing `ZIPP_NO_TIERC_CLOSURE_MAKE=1` now
  also keeps closure creation out of memory regions.
- **Regex:** the step charges of a metered engine change on two lazy-loop
  rows (75 to 49, 59 to 39) and one lookbehind row (32 to 34); the
  regex crate's `step_counts.rs` pins them.

## WebAssembly size

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125, Node 24.19.0. Brotli is
quality 11 with the size hint, as the README states it. Module bytes,
raw / Brotli, Windows builds; "start" is `r16base`, the modules of
`5cffc37a` (0.0.23's code), and "this pass" is built from `1df54024`.

| Variant | Start of the pass | This pass | Change |
| --- | ---: | ---: | ---: |
| Lite | 2,573,473 / 697,512 | 2,506,646 / 677,555 | -66,827 (-2.60%) / -19,957 (-2.86%) |
| JavaScript | 4,636,188 / 1,180,196 | 4,505,981 / 1,141,583 | -130,207 (-2.81%) / -38,613 (-3.27%) |
| Python-base | 6,331,149 / 1,567,394 | 6,196,781 / 1,526,914 | -134,368 (-2.12%) / -40,480 (-2.58%) |
| Python + Torch | 8,255,034 / 1,918,625 | 8,205,824 / 1,896,270 | -49,210 (-0.60%) / -22,355 (-1.17%) |

The size agent's seven commits account for about 20 KB (Lite) to 36 KB
(Python + Torch) of the Brotli savings, the regex agent's for 4.7 KB
(Lite) and 12.4 KB (JavaScript); the main thread's compiled-region
helpers are native-only and add nothing to the modules but the
interpreter changes. The torch kernels added about 88 KB raw to Python +
Torch, which is why it saves least.

## WebAssembly speed screen

`crates/zipp-wasm/tests/node/bench-size-pass.cjs` (optimizing compiler,
10 warm-up pairs, 31 alternating paired samples), the pass's modules
against `r16base`, run in both orders; each cell is the geometric mean of
the two orders' ratios (this pass's time over the start's).

| Case | Lite | JavaScript |
| --- | ---: | ---: |
| `alloc-object` | 1.006 | 0.970 |
| `arith-float` | 1.012 | 0.981 |
| `arith-int` | 1.011 | 1.031 |
| `arith-mod` | 1.009 | 0.952 |
| `array-build` | 1.010 | 0.983 |
| `array-hof` | 1.011 | 1.023 |
| `call-deep` | 1.045 | 0.942 |
| `enumeration` | 0.988 | 0.984 |
| `error-range` | 0.969 | 0.966 |
| `error-type` | 0.982 | 0.947 |
| `error-typedarray` | 0.986 | 1.000 |
| `javascript_init_0_functions` | 1.001 | 0.993 |
| `javascript_init_200_functions` | 0.997 | 1.006 |
| `json` | 0.999 | 0.987 |
| `promise-resolve` | 1.007 | 0.917 |
| `prop-mono` | 1.025 | 1.010 |
| `prop-poly` | 1.002 | 1.033 |
| `property-delete` | 0.995 | 1.023 |
| `recursive-calls` | 1.007 | 1.026 |
| `regex` | 1.029 | 0.974 |
| `regexp-matchall` | 1.021 | 1.020 |
| `regexp-unicode-compile` | 1.035 | 1.029 |
| `string-append-index` | 0.974 | 1.000 |
| `string-build` | 0.966 | 0.984 |
| geomean | **1.003** | **0.990** |

The modules run no compiled code, so the pass's region work does not
reach them; their changes are the interpreter's (constructor frames, the
set-IC append, private fields, object spread, the arguments map, the
dense constant cache), the size agent's and the regex engine's.
`regexp-matchall` and `regexp-unicode-compile` read 2 to 3.5% slower in
both modules in both orders; the regex agent measured its engine rows
level in the modules on its own builds, and this was not run down.
`call-deep` in Lite reads 1.12 one way and 0.97 the other, which is
noise, not a direction.

## Native measurements

Ordinary release builds (`cargo build --release -p zipp-cli`, default
features, not profile-guided), `tools/bench.py`, 12 interleaved
repetitions for the A/B and 10 against Node, medians, paired ratios with
percentile-bootstrap intervals. Every row prints the same bytes in every
engine.

`5cffc37a` (0.0.23's code) against `1df54024`:

| Row | 0.0.23 | This pass | Paired change | 95% interval |
| --- | ---: | ---: | ---: | ---: |
| `async-promise-chain` | 381 ms | 381 ms | -0.2% | -2.2% to +0.8% |
| `class-prototype-hot` | 236 ms | 237 ms | -0.6% | -2.8% to +0.9% |
| `json-large` | 266 ms | 267 ms | +3.7% | -1.2% to +5.5% |
| `map-set-heavy` | 617 ms | 634 ms | +2.9% | -1.4% to +4.4% |
| `markdown-render` | 219 ms | 216 ms | -0.1% | -2.1% to +2.6% |
| `parse-large-js` | 250 ms | 250 ms | -0.4% | -1.6% to +0.7% |
| `polymorphic-objects` | 332 ms | 337 ms | +2.1% | +0.7% to +3.0% |
| `polymorphic-objects-v2` | 28 ms | 27 ms | -4.7% | -6.0% to -1.7% |
| `property-ic-shapes` | 10 ms | 10 ms | +2.0% | -2.6% to +7.0% |
| `regex-log-scan` | 452 ms | 461 ms | +2.8% | -2.2% to +5.9% |
| `sparse-array` | 75 ms | 76 ms | +0.8% | +0.1% to +4.0% |
| `sparse-array-v2` | 105 ms | 106 ms | +0.3% | -0.9% to +0.8% |
| `typedarray-math` | 144 ms | 142 ms | +0.2% | -0.9% to +0.8% |

Geomean of the paired ratios: 1.0111x [1.003, 1.016] over the 10
headline rows, 0.9914x [0.976, 1.007] over the 3 diagnostic rows and
1.0065x [0.998, 1.011] over all 13.

That headline interval lies above 1, so the run was repeated with fresh
copies of both executables (one file can stay slow on its own on this
machine) and 15 repetitions: 1.0033x [0.996, 1.010] over the headline
ten, 0.9774x [0.969, 0.993] over the diagnostic three, 0.9973x
[0.992, 1.003] over all 13, every row's interval but the two
improved diagnostics' containing zero (`polymorphic-objects` +1.9%
[-0.7%, +4.5%], `json-large` -1.5%, `map-set-heavy` +1.4%,
`regex-log-scan` +1.4%). A third run of just the four rows that had read
slowest, on another copy pair, put them at -0.2%, -3.0%, +0.3% and
+1.1%, all intervals containing zero. The rows are level with 0.0.23.

`polymorphic-objects`' outer dictionary loop compiles in this pass (it
holds a `for-in`, which `175e7299` admits); a same-binary switch
(`ZIPP_NO_REGION_KEYLIST=1`, built for the check and not committed)
moved the row by +0.0% [-1.7%, +2.6%].

Against Node 24.19.0, 10 repetitions:

| Row | Node 24 | This pass | This pass / Node | 95% interval |
| --- | ---: | ---: | ---: | ---: |
| `async-promise-chain` | 343 ms | 386 ms | 1.14x | 1.11 to 1.16 |
| `class-prototype-hot` | 309 ms | 238 ms | 0.78x | 0.75 to 0.79 |
| `json-large` | 285 ms | 270 ms | 0.96x | 0.91 to 0.99 |
| `map-set-heavy` | 902 ms | 772 ms | 0.90x | 0.82 to 0.93 |
| `markdown-render` | 287 ms | 225 ms | 0.77x | 0.75 to 0.79 |
| `parse-large-js` | 283 ms | 260 ms | 0.93x | 0.90 to 1.00 |
| `polymorphic-objects` | 330 ms | 345 ms | 1.05x | 1.03 to 1.06 |
| `polymorphic-objects-v2` | 83 ms | 26 ms | 0.32x | 0.31 to 0.32 |
| `property-ic-shapes` | 267 ms | 11 ms | 0.04x | 0.04 to 0.05 |
| `regex-log-scan` | 499 ms | 475 ms | 0.96x | 0.93 to 0.97 |
| `sparse-array` | 82 ms | 77 ms | 0.95x | 0.93 to 1.01 |
| `sparse-array-v2` | 172 ms | 107 ms | 0.62x | 0.61 to 0.64 |
| `typedarray-math` | 207 ms | 143 ms | 0.69x | 0.69 to 0.71 |

Geomean: 0.9047x [0.893, 0.912] of Node over the 10 headline rows (the
fifteenth pass measured 0.896x), 0.6380x [0.633, 0.644] over all 13.

## Probes

`target/r16-scratch/broad/` (one pattern a file, each run in a function,
a checksum printed): wall time of the whole process, minimum of 5 runs,
ms. Node's start (about 35 ms) is in its figures, ZIPP's (about 7) in
its. Every engine prints Node's checksum.

| Probe | 0.0.23 | This pass | Node 24 | This pass / Node |
| --- | ---: | ---: | ---: | ---: |
| `arguments_obj` | 483 | 293 | 36 | 8.14x |
| `array_filter_reduce` | 61 | 60 | 39 | 1.54x |
| `array_index_loop` | 9 | 10 | 33 | 0.30x |
| `array_literal` | 98 | 64 | 35 | 1.83x |
| `array_map_arrow` | 25 | 26 | 42 | 0.62x |
| `array_push_pop` | 914,932 (15 min) | 123 | 81 | 1.52x |
| `arrow_capture` | 164 | 90 | 37 | 2.43x |
| `closure_make` | 146 | 159 | 36 | 4.42x |
| `destructure_arr` | 128 | 94 | 39 | 2.41x |
| `destructure_obj` | 22 | 23 | 34 | 0.68x |
| `for_in_obj` | 92 | 67 | 36 | 1.86x |
| `for_of_array` | 27 | 26 | 36 | 0.72x |
| `getter_setter` | 19 | 19 | 197 | 0.10x |
| `indexof_big` | 187 | 182 | 90 | 2.02x |
| `instanceof` | 321 | 94 | 42 | 2.24x |
| `json_parse_small` | 96 | 93 | 81 | 1.15x |
| `json_stringify_small` | 67 | 72 | 65 | 1.11x |
| `map_set_get` | 157 | 158 | 55 | 2.87x |
| `math_ops` | 65 | 67 | 45 | 1.49x |
| `method_call_proto` | 24 | 26 | 38 | 0.68x |
| `obj_literal_nested` | 176 | 109 | 38 | 2.87x |
| `object_entries` | 162 | 137 | 102 | 1.34x |
| `object_keys` | 92 | 61 | 39 | 1.56x |
| `optional_chain` | 93 | 93 | 38 | 2.45x |
| `queue` | 305 | 103 | 69 | 1.49x |
| `queue_big` | 4,951 | 1,042 | 848 | 1.23x |
| `regexp_test` | 73 | 81 | 53 | 1.53x |
| `rest_params` | 127 | 102 | 35 | 2.91x |
| `set_add_has` | 154 | 154 | 51 | 3.02x |
| `sort_numbers` | 69 | 69 | 71 | 0.97x |
| `splice_mid` | 99 | 56 | 52 | 1.08x |
| `spread_arr` | 68 | 71 | 47 | 1.51x |
| `spread_call` | 164 | 143 | 44 | 3.25x |
| `spread_obj` | 825 | 137 | 41 | 3.34x |
| `string_concat` | 91 | 91 | 51 | 1.78x |
| `string_indexof_slice` | 101 | 103 | 43 | 2.40x |
| `string_replace` | 177 | 187 | 62 | 3.02x |
| `string_split_join` | 139 | 136 | 66 | 2.06x |
| `template_lit` | 154 | 129 | 49 | 2.63x |
| `try_catch_throw` | 45 | 43 | 412 | 0.10x |
| `typed_array` | 12 | 12 | 34 | 0.35x |
| `typeof_switch` | 134 | 54 | 38 | 1.42x |
| `unshift_q` | 178 | 52 | 55 | 0.95x |

The rows above 2x are the patterns Node serves by escape analysis and
inlining (a closure factory, an arguments object, object spread, a spread
call) or by a faster collection and string core (Map and Set, `replace`,
templates). Two rows are slower than 0.0.23, and stay slower on three
fresh copies of each executable: `closure_make` by about 6% (171-177 ms
to 184) and `regexp_test` by about 3% (97-98 to 100-102). Builds kept
from the pass put `closure_make`'s step in 2 to 4 ms increments across
several commits from `ebfb8db1` on (inlining the cell helper that commit
split out changed nothing), and `regexp_test`'s at the regex merge,
while the regex lane's own in-process `small-calls` case is level.
Neither was run down.

## Python suite

`tools/python_bench.py`, 44 programs (36 against CPython 3.13, 8 torch
programs ZIPP-only), 5 repetitions, in-script work time, medians, 0.0.23
(`head-5cffc37a`) and this pass (`1df54024`) interleaved with CPython in
one run:

| Program | 0.0.23 | This pass | CPython | 0.0.23 / CPython | This pass / CPython |
| --- | ---: | ---: | ---: | ---: | ---: |
| `attr_read_write` | 3.4 | 3.3 | 7.9 | 0.43x | 0.42x |
| `builtin_isinstance` | 11.6 | 11.7 | 9.2 | 1.27x | 1.27x |
| `builtin_len` | 11.4 | 11.4 | 11.7 | 0.97x | 0.97x |
| `call_defaults` | 17.8 | 18.2 | 12.4 | 1.43x | 1.47x |
| `call_keywords` | 10.3 | 10.4 | 7.9 | 1.31x | 1.32x |
| `call_simple` | 43.0 | 44.3 | 35.6 | 1.21x | 1.25x |
| `dict_get` | 11.6 | 10.1 | 11.4 | 1.02x | 0.89x |
| `dict_str_key` | 31.9 | 30.8 | 20.5 | 1.56x | 1.50x |
| `float_arith` | 2.3 | 2.3 | 9.4 | 0.25x | 0.25x |
| `for_list` | 15.2 | 15.9 | 17.0 | 0.89x | 0.94x |
| `fstring` | 11.0 | 8.6 | 3.5 | 3.16x | 2.48x |
| `generator_iter` | 41.4 | 41.7 | 14.6 | 2.84x | 2.86x |
| `global_read` | 22.1 | 22.3 | 50.4 | 0.44x | 0.44x |
| `hasattr_miss` | 4.4 | 3.1 | 2.8 | 1.56x | 1.10x |
| `hash_str` | 9.4 | 8.0 | 7.9 | 1.19x | 1.02x |
| `instance_new` | 24.7 | 19.7 | 5.8 | 4.28x | 3.42x |
| `int_arith` | 66.5 | 66.8 | 145.9 | 0.46x | 0.46x |
| `list_append` | 9.1 | 9.1 | 5.1 | 1.77x | 1.77x |
| `list_index` | 21.8 | 22.1 | 26.1 | 0.84x | 0.85x |
| `listcomp_small` | 14.4 | 14.3 | 7.1 | 2.03x | 2.01x |
| `math_sqrt` | 13.5 | 11.9 | 9.9 | 1.36x | 1.19x |
| `method_call` | 17.7 | 17.4 | 8.4 | 2.11x | 2.07x |
| `property_read` | 22.1 | 12.6 | 8.7 | 2.53x | 1.44x |
| `range_loop` | 19.5 | 19.6 | 101.0 | 0.19x | 0.19x |
| `str_long_len_index` | 5.4 | 5.5 | 6.9 | 0.79x | 0.80x |
| `str_methods` | 27.5 | 25.1 | 7.1 | 3.85x | 3.51x |
| `super_call` | 19.9 | 19.5 | 8.4 | 2.37x | 2.32x |
| `try_raise_catch` | 28.2 | 25.7 | 8.7 | 3.26x | 2.96x |
| `tuple_swap` | 0.8 | 0.8 | 3.0 | 0.26x | 0.26x |
| `tuple_unpack` | 9.0 | 9.0 | 9.2 | 0.98x | 0.98x |
| `fib_recursive` | 40.7 | 37.3 | 22.6 | 1.80x | 1.65x |
| `json_roundtrip` | 13.4 | 6.8 | 7.7 | 1.73x | 0.88x |
| `nbody` | 19.6 | 17.3 | 9.1 | 2.16x | 1.90x |
| `oo_simulation` | 31.0 | 30.3 | 12.7 | 2.44x | 2.39x |
| `sort_heapq` | 16.4 | 13.5 | 5.9 | 2.78x | 2.29x |
| `word_count` | 26.5 | 26.0 | 10.0 | 2.64x | 2.59x |

Geomean against CPython: 1.31x to 1.21x over the 36 (micro 1.18x to
1.11x, macro 2.22x to 1.84x). The Python agent's final run measured
1.34x to 1.23x on its own binaries. Still above 2x: `instance_new`
(3.42x), `str_methods` (3.51x), `try_raise_catch` (2.96x),
`generator_iter` (2.86x), `word_count` (2.59x), `fstring` (2.48x),
`oo_simulation` (2.39x), `super_call` (2.32x), `sort_heapq` (2.29x),
`method_call` (2.07x), `listcomp_small` (2.01x).

## torch against PyTorch

The eight torch programs, `--torch-cpython "py -3.11"` (PyTorch 2.11 CPU,
one thread), `--warm 5`, 3 repetitions, in-script work time, medians, ms:

| Program | PyTorch | 0.0.23 | This pass | This pass / PyTorch |
| --- | ---: | ---: | ---: | ---: |
| `torch_cnn` | 3.4 | 24.7 | 14.5 | 4.31x |
| `torch_dataloader` | 3.9 | 67.3 | 10.6 | 2.71x |
| `torch_matmul` | 0.2 | 3.7 | 1.0 | 5.32x |
| `torch_matmul_big` | 0.7 | 17.8 | 2.8 | 4.21x |
| `torch_mlp` | 4.2 | 66.6 | 26.6 | 6.37x |
| `torch_rnn` | 6.1 | 167.9 | 42.4 | 6.92x |
| `torch_small_ops` | 8.5 | 96.1 | 41.4 | 4.85x |
| `torch_transformer` | 4.6 | 81.5 | 42.3 | 9.28x |

Geomean: 16.67x to 5.18x PyTorch's warm time (0.31 of 0.0.23's). The
first run in a process is a different picture: PyTorch's `cnn` takes
684 ms cold against this pass's 31; its kernel initialisation is in it.
`torch_transformer` prints PyTorch's numbers to the third decimal and
differs in the fourth (AdamW amplifies 1e-8 differences in the step-0
gradients; with SGD the two agree to seven digits).

## Correctness

On `1df54024` (the merged tip):

- Full Test262 with the expected-failure manifest: PASS (0 unexpected
  failures, 0 stale expectations, 0 skips).
- The default-feature zipp-vm suite: 328 of 328 test binaries pass.
- The Python integration tests (`--features python`, the library and
  every `tests/*py*.rs`): pass.
- The Python corpus in four modes (default, `ZIPP_PY_NOFAST=1`,
  `ZIPP_NOJIT=1`, `ZIPP_PY_JIT=0`): 183 of 188 in each; the five that do
  not match are the GPU programs that need a GPU host (`gpu_graph`,
  `ml_gpu_ops`, `ml_gpu_prepared`, `ml_gpu_session`,
  `ml_transport_graph_reference`), as at the start.
- `cargo test -p zipp-cli`: pass.
- The safe-sandbox zipp-vm suite (`--no-default-features --features
  safe-sandbox`, every test binary): pass.
- `-D warnings` checks: default, `python`, `safe-sandbox,python` and
  `wasm-lite` with `--tests`, and `safe-sandbox,meter-only,python`: all
  clean.
- The Node boundary suite on the four modules: Lite 36 of 36, JavaScript
  36 of 36, Python + Torch 36 of 36, Python-base 34 of 36 (the ML
  transport audit and `python-gpu` need the torch package, which
  Python-base does not carry; the pass-start module fails the same two).
- Differential matrices against Node 24, each run with the JIT, with
  `ZIPP_NOJIT=1` and under `ZIPP_GC_STRESS=1` (under
  `target/r16-scratch`): construction, iterators and generator patching,
  private fields, classes, instanceof, typeof, literals, templates,
  object spread semantics, closures, `arguments`, pop, array spread and
  destructuring, key lists and `for-in`, rest parameters (also with
  `ZIPP_NO_TIERC_REST=1`), object spread in loops. Every row prints
  Node's output except the one recorded [below](#not-claimed).

Two process mistakes were caught and corrected during the pass, before
anything left the machine:

- `6cb01a60` had been committed without a `mut` its body needed (a
  partial hunk stage left it out), so it and the three commits after it
  did not compile on their own. They were rebuilt with the `mut`
  restored (`e0de4a8d`, `a2d2f938`, `c51ffa4c`, `c8aa305c`); every later
  partial stage was type-checked as a commit object in a side worktree,
  in the default and safe-sandbox profiles, before it was committed.
- The first arguments change built the property map without
  `has_element_key`, which `length` sets; the element read paths decline
  an arguments object by it, and with `ZIPP_NO_JIT_SPARSE_GET=1` a
  compiled `arguments[0]` read the stale element store. The off-switch
  child of `sparse_get_fast` caught it; the committed change sets the
  flag, and a unit test pins the map field for field against the three
  `define`s it replaces.

## Tried and dropped

- A size-class free-list cache in front of dlmalloc in the modules'
  reporting allocator: `json` 18% slower, `property-delete` and
  `error-range` 61% slower.
- (Size agent) Dead-argument elimination in wasm-opt (2.7 to 4.2 KB more
  off each full module, 2 to 4% slower on two rows); merging similar
  functions (-30 KB raw, +1 KB Brotli); other function orders; a length
  byte per packed DateTimeFormat entry; composition pairs by length.
- (Python agent) Pushing a Python callee's frame directly from compiled
  code (no gain); whole-function Python Tier C (`ZIPP_PY_TIERC=1`, 8 to
  47% slower on call rows); the first form of lazy globals (+11% on
  `global_read`).
- (ML agent) In-place optimizer state with an int `step`; emulating
  PyTorch's FMA; an AVX2 matmul (needs `unsafe`).
- (Regex agent) The IR walkers through `dyn` (compile 1.16 to 1.34x
  slower); derived Unicode tables (-3.6 KB Brotli in Lite, compile 1.4x
  slower; the patch is kept at `target/rx/tables-experiment.patch` in
  the agent's worktree).

## Not claimed

- **Faster than V8 everywhere.** The headline rows are ahead of Node
  except where the table above says; the broad probes are not: the
  [probe table](#probes) lists the patterns still several times Node's
  time (an `arguments` function, a closure factory, Map and Set, object
  spread, spread calls). Node removes most of those allocations by escape
  analysis and inlining, which ZIPP does not do.
- **Parity with CPython.** The suite geomean is 1.23x CPython's time;
  the rows still above it are in the [Python table](#python-suite).
- **torch parity.** Eager torch is about 5.6 times PyTorch's warm time on
  one thread (geomean of the eight programs), and faster on a cold first
  run of four of them.
- **One divergence from Node predates the pass and is unchanged:**
  `Object.defineProperty(arguments, "length", { value: 2 })` on a
  three-element arguments object truncates its element store, so
  `Object.keys(arguments)` then lists two keys where Node lists three.
- **Found by the agents, not fixed** (their reports have the detail):
  a list subclass with `__eq__` but no `__ne__` gives the wrong `!=`;
  `"%z" % 1` raises TypeError (CPython: ValueError); a lone surrogate's
  repr; `traceback`'s `f.line` empty for single-module programs; torch's
  optimizer state replaced each step and an int `step`, SGD/Adam rounded
  per operation (PyTorch fuses), `view` on a non-contiguous tensor does
  not raise; a lazy or zero-minimum regex loop over a lone surrogate on
  a one-byte subject fails in the interpreter and matches in the native
  regex JIT; the regex corpus's 306 pinned disagreements with Node,
  mostly `v`-mode classes.

## Leads

- **`arguments` in compiled code.** With rest functions compiled, a
  function that only reads `arguments.length` and `arguments[i]` could
  be compiled as one with a hidden rest array of every argument; the
  helper behind a computed read must still give the full answer for a
  non-index key (Tier C cannot deopt mid-body).
- **Closure allocation.** A closure's capture list is its own heap
  allocation (`Vec<u32>`), and that malloc/free pair is most of what a
  closure-creating loop still costs.
- **Object spread** copies each key as a new string; a literal's planned
  keys could be shared until the first change.
- **The torch module grew about 88 KB raw (1%)** with the native
  kernels; `b13da1e5` (elementwise loops over the typed arrays, 16 KB)
  gains nothing in the modules and could be dropped where size matters.
- **Regex:** cache the derived Unicode group tables (3.6 KB Brotli in
  Lite); `try_at_pos` is still four copies in the JavaScript module;
  `parse.rs` is the largest remaining piece; the lone-surrogate loop fix.
- **Python modules:** docstrings are about 28 KB of Brotli in Python +
  Torch and the Unicode character-name table 16.8 KB; dropping either is
  a behaviour decision.

