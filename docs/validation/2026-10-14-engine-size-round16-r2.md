# The reported bugs, compiled calls and construction, Python at 1.10x CPython and torch at 4.2x PyTorch: sixteenth pass, round two - 2026-10-14

Continues from `828038b6` (the first round's record) through 87 commits to
`35e6d1dc`: 29 from the main thread, 6 Dependabot updates applied as
commits, and four agents' branches (Python speed 16 commits, torch 18,
promises and async 8, WebAssembly size 6) merged whole in 4 merges.
Every commit carries `[skip ci]`. Pushed to `main` in three steps:
`2cb7176f` (the main thread's work, the fixes for #32 to #34 and the
Dependabot updates), `7da3519d` (the agents' merges and the final-sigma
fix), and `35e6d1dc` (the fixes for #41 to #45).

The native and Python measurements are of `35e6d1dc`, the release's
source (`fin.exe`, gate A's build), against `5cffc37a` (0.0.23's code,
`head-5cffc37a.exe`): ordinary release builds (`cargo build --release -p
zipp-cli`, default features, not profile-guided). The torch and probe
measurements are of `7da3519d` (`r2m`), before the #41 to #45 fixes and
`79759baa`, and were not repeated. The WebAssembly modules are built from
`35e6d1dc` (`r2f`), against
`5cffc37a`'s (`r16base`).

## Outcome

- **Eight reported issues fixed** (f2i-com/zipp.org #32 to #34 and #41
  to #45, all from @takahashim), and nine further wrong answers found by
  differential programs ([below](#defects-fixed)).
- **Native, the 13 real rows: 0.9730x of 0.0.23's time** over the
  headline ten [0.970, 0.978], 0.9739x over all 13 [0.970, 0.981];
  `async-promise-chain` -17.2%, `json-large` -5.1%, `polymorphic-objects`
  -4.6%; `map-set-heavy` +2.8% [+0.7%, +6.8%]. Against Node 24 the
  headline ten are at **0.8819x** [0.873, 0.887] (round one: 0.905x) and
  all 13 at 0.622x; ZIPP's median is below Node's on every row,
  `async-promise-chain` (0.94x) and `polymorphic-objects` (0.98x)
  included, both behind Node in round one.
- **Python: 1.09x CPython 3.13's time** over the 36 programs (micro
  1.00x, macro 1.68x), measured beside 0.0.23 at 1.31x; the suite's 44
  programs in 0.67x of 0.0.23's time; 15 programs faster than CPython.
- **torch: 4.22x PyTorch 2.11's warm one-thread time** (0.0.23: 16.96x;
  round one: 5.2x), 0.25x of 0.0.23's time.
- **The 43 broad probes**: 0.60x of 0.0.23's time and 0.97x of round
  one's; 1.39x Node's (whole-process time, Node's 35 ms start included).
- **WebAssembly, after Brotli: Lite -3.8%, JavaScript -4.0%,
  Python-base -2.8%, Python + Torch -1.0%** against 0.0.23's modules;
  speed level (Lite 0.986, JavaScript 0.995 on the 24-case screen), with
  Unicode RegExp compiles in 0.63 (Lite) and 0.90 of the time.

## Defects fixed

Reported by @takahashim (each with a test in
`crates/zipp-vm/tests/reported_property_bugs.rs` or
`reported_semantics_bugs.rs`, expecting Node 24's output):

| Issue | Commit | What was wrong |
| --- | --- | --- |
| #32 | `8ce16ff8` | A typed array's in-range indices were not own properties. |
| #33 | `5980d5a7` | A function's `prototype` made non-writable stayed writable. |
| #34 | `861cda39` | `obj.hasOwnProperty(k)`, `propertyIsEnumerable` and `isPrototypeOf` ran the built-in for any receiver (Proxy traps skipped, replacements ignored, no throw for a null prototype). |
| #41 | `acca1331` | A class or function on another object's chain was walked past; a Map/Set `size` and inherited getters used the wrong receiver. |
| #42 | `2ba3b5a0` | `typeof name` inside `with` read the global. |
| #43 | `6333c421`, `35e6d1dc` | A replaced `console` method was never called. |
| #44 | `6333c421`, `f229dc1c` | There was no `performance` object; it now has `now` and `timeOrigin`. |
| #45 | `18643638` | A derived construction read `newTarget.prototype` itself. |

Found by differential programs against Node 24, CPython 3.13 and
PyTorch:

- `ea1a9d8b`: a global redefined by `Object.defineProperty(globalThis,
  ...)` went on being called as the old function by a compiled loop
  (since at least 0.0.23).
- `afed3f49`: `arguments.length = n` deleted the elements past `n`; a
  length written as a non-integer left iteration covering every element.
- `7da3519d`: `toLocaleLowerCase` in tr, az and lt gave the medial sigma
  at the end of a word.
- `05704850` (Python agent): a zero-argument `super()` beside a lambda
  capturing `self` did not compile, and in a comprehension took the
  iterator as its receiver.
- torch agent: `uniform_` with asymmetric bounds rounded differently from
  PyTorch (`832c3756`); `x**3`, `x**-2`, `x**0.5` and `x**-0.5` differed
  in the last bit or at -0 and -inf (`10d8465e`); the error for a saved
  tensor modified in place named the wrong node (`4dce85f2`).

One performance regression of this round was found and fixed before
release: `1e836768` moved the interpreter's `StaticFn` arm out of line
and every interpreted Python call got about 5% slower (bisected over the
round's commits on fresh copies of each build); `79759baa` inlines it
again (`call_simple` 46.2 to 44.1 ms).

## The changes

### JavaScript, main thread

Same binary or previous build, loaded machine, ms (Node 24 beside), as
each commit message records them:

| Commit | Change | Before to after (Node) |
| --- | --- | --- |
| `fc7da637` | a compiled `===` against null or undefined inline | 3M optional chains and tests 126 to 74 (7) |
| `7b87a99c` | a plain string replaced without copying the receiver | 1M `replace` 221-248 to 156-166 (62) |
| `1f8cdb7f` | a string's intrinsic method from the memo in a region's Get | 3M `slice` 155 to 146 (10) |
| `30f79d31` | methods that name their own class compile | (enables the next) |
| `8f050ea2` | a base class constructed from compiled code directly | 400K `new Vec` 70-77 to 51-54 |
| `85e8bd43` | loops holding a for-of, a destructuring or a finally compile | destructure_heavy ~205 to ~104 (16); `finally` loop 56 to 16 (1) |
| `3f593fcb` | a plain object's integer keys without spelling them | 2M `g[i % 10]` reads 95 to 23 (4) |
| `2cc045b5` | a new object's payload sized inline | `new Vec` 678 to 658-664 |
| `9470f615` | a store-only constructor builds its object as a literal does | 2M `new Vec(i, 1)` 409-495 to 106-160 (1-5) |
| `8d363095` | a compiled function called from a compiled loop without a frame | class_sim 146-167 to 111-113 (13) |
| `4eaf90eb` | an integer turned into a string with one allocation | 300K templates 94-100 to 59-70 (8) |
| `3a42a658` | functions that use `arguments` compile in Tier C | 1M sums over `arguments` 285-300 to 206-286 (4) |
| `e9e314f7` | `Object.assign` copies a plain source directly | 1M `Object.assign({}, src)` 1002 to 241 (25) |
| `1e836768` | code calling `Object.assign`, `String.fromCharCode` and the other statics compiles | 1M `Object.assign` + store 474-666 to 246-263 (46) |

### Python (agent, 16 commits)

A store-only `__init__` called without its frame, inline method lookups
and construction sites keyed by the last class, sort keys of one form
sorted by columns, list/tuple/dict records read once per subscript,
`get` and `len`, a compiled `raise`, loops planned only where the header
dominates the back edge, cheaper `super().m()`, and the zero-argument
`super()` fix; seven corpus programs with CPython 3.13 output. The
agent measured 1.25x to 1.17x CPython on its branch; the merged build
measures 1.10x ([below](#python-suite)).

### torch (agent, 18 commits)

Random numbers drawn natively, native tensor loops from 16 elements,
`torch._gpu` loaded lazily, broadcast-gradient sums, ReLU's gradient and
gradient accumulation in one kernel each, a transposed product operand
read in place, lighter nn module construction, small optimizer updates
run natively, cheaper unbind/cat backward, one-call add/sub/mul/div and
matrix products, attention masks built once per size, one copy of each
arithmetic op's byte loops, and the value fixes above. `ce8e1494`
moved the torch package's wire version to 3 (op codes 33 to 38): the
package and the engine must come from the same release.

The agent reported torch_cnn's median warm run in the complete
WebAssembly module under Node slower since `69471503` (36 to 54 ms).
It does not reproduce: 20 warm runs, two processes each, medians 22.7
and 22.2 ms before that commit, 22.6 and 23.3 after, 23.7 and 25.3 in
the merged module, against 44.0 and 42.0 in 0.0.23's (first runs 400 to
700 ms in 0.0.23's, 150 to 200 now).

### Promises and async (agent, 8 commits)

An await of a settled promise continued in place, profiler phase tags
without an atomic swap, a promise reaction's int-tier callback run
without a frame, resolving-function pairs in a bitmap, the unobservable
steps taken out of an intrinsic `Promise.all`, one register window for a
run of jobs calling the same callback, and a minor collection that skips
the jobs queued before the last one. `async-promise-chain` went from
1.14x Node's time to 0.94x.

### WebAssembly size (agent, 6 commits)

One regex matcher per input type in the full modules, case mapping
without the standard library's conversion tables, the regex crate's
derived Unicode tables as unions built once, front-coded emoji sequences,
and tz positions stored as lengths.

### The reported issues

`acca1331`: `proto_member_get` hands any non-plain object on the chain
to `get_member`; `exotic_own_or_proto` keeps the receiver; the Map/Set
`size` shortcut only for a plain collection read directly.
`2ba3b5a0`: `typeof_operand` emits the `with` probe chain before the
global load. `18643638`: a derived construction defers the
`newTarget.prototype` read to the base (`pending_ctor_proto`, settled in
`run_class_ctor`). `6333c421`: `Print` reads the method before the
arguments and calls a replacement; `performance` is a real namespace and
`performance.now()` a `StaticFn`; the `Now` instruction is gone.
`35e6d1dc`: the local-SROA lane admits `LoadGlobal` and the console
call's method goes in its result register, so a function ending in
`console.log` keeps the lane (the gate's local_sroa, object_finalize and
plan_region tests caught the loss). `f229dc1c`: `performance.timeOrigin`.

## Native measurements

`tools/bench.py`, 12 interleaved repetitions for the A/B and 10 against
Node, medians, paired ratios with percentile-bootstrap intervals; every
row prints the same bytes in every engine. `5cffc37a` (0.0.23's code) against `35e6d1dc`:

| Row | 0.0.23 | This round | Paired change | 95% interval |
| --- | ---: | ---: | ---: | ---: |
| `async-promise-chain` | 379 ms | 314 ms | -17.2% | -17.6% to -16.1% |
| `class-prototype-hot` | 235 ms | 236 ms | +0.4% | -1.5% to +1.6% |
| `json-large` | 255 ms | 243 ms | -5.1% | -6.3% to -2.4% |
| `map-set-heavy` | 595 ms | 616 ms | +2.8% | +0.7% to +6.8% |
| `markdown-render` | 211 ms | 210 ms | -0.2% | -2.0% to +1.0% |
| `parse-large-js` | 249 ms | 245 ms | -1.8% | -2.7% to -0.4% |
| `polymorphic-objects` | 333 ms | 318 ms | -4.6% | -5.8% to -3.6% |
| `polymorphic-objects-v2` | 27 ms | 26 ms | -3.6% | -7.1% to -0.7% |
| `property-ic-shapes` | 10 ms | 10 ms | -3.2% | -5.8% to +3.4% |
| `regex-log-scan` | 437 ms | 439 ms | +0.3% | -2.1% to +3.7% |
| `sparse-array` | 74 ms | 74 ms | +0.3% | -0.5% to +1.2% |
| `sparse-array-v2` | 104 ms | 104 ms | -0.2% | -0.6% to +0.5% |
| `typedarray-math` | 142 ms | 141 ms | -0.2% | -1.2% to +0.8% |

Geomean of the paired ratios: 0.9730x [0.970, 0.978] over the 10
headline rows, 0.9767x [0.961, 1.002] over the 3 diagnostic rows and
0.9739x [0.970, 0.981] over all 13. A first run on `7da3519d` read
0.9725x [0.961, 0.983] and 0.9766x. `map-set-heavy` read +3.9%
[-3.2%, +10.4%] there and +2.8% [+0.7%, +6.8%] here, and round one read
it +1.4% to +2.9%: a small slowdown of that row, not run down.

Against Node 24.19.0, 10 repetitions:

| Row | Node 24 | This round | This round / Node | 95% interval |
| --- | ---: | ---: | ---: | ---: |
| `async-promise-chain` | 333 ms | 315 ms | 0.94x | 0.93 to 0.96 |
| `class-prototype-hot` | 296 ms | 234 ms | 0.79x | 0.78 to 0.84 |
| `json-large` | 256 ms | 245 ms | 0.95x | 0.93 to 0.98 |
| `map-set-heavy` | 649 ms | 603 ms | 0.94x | 0.83 to 0.96 |
| `markdown-render` | 276 ms | 212 ms | 0.77x | 0.75 to 0.81 |
| `parse-large-js` | 272 ms | 246 ms | 0.90x | 0.90 to 0.92 |
| `polymorphic-objects` | 324 ms | 319 ms | 0.98x | 0.97 to 1.00 |
| `polymorphic-objects-v2` | 82 ms | 26 ms | 0.32x | 0.31 to 0.33 |
| `property-ic-shapes` | 255 ms | 10 ms | 0.04x | 0.04 to 0.04 |
| `regex-log-scan` | 478 ms | 443 ms | 0.94x | 0.91 to 0.97 |
| `sparse-array` | 78 ms | 74 ms | 0.94x | 0.92 to 0.97 |
| `sparse-array-v2` | 169 ms | 105 ms | 0.62x | 0.61 to 0.63 |
| `typedarray-math` | 201 ms | 141 ms | 0.70x | 0.69 to 0.71 |

Geomean: 0.8819x [0.873, 0.887] of Node over the 10 headline rows,
0.6221x [0.617, 0.626] over all 13. ZIPP's median is below Node's on
all 13 rows; the strict sign test (Bonferroni, family alpha 0.05)
proves 8 of them. Startup, paired launches: Node 30.4 ms, ZIPP 7.2 ms;
startup-adjusted, `async-promise-chain` is at 1.01x, `json-large` 1.05x
and `polymorphic-objects` 1.05x.

Beside the rows, against 0.0.23 and Node in one process each, best of 3:
two million `new Vec(i, 1)` 760 to 100 ms (Node 4), a million
`Object.assign({}, src)` 830 to 140 (Node 18), `class_sim.js` 530 to 155
ms of process time (Node 65), a million `arr.push(Promise.resolve(i))`
73 to 74 (Node 60).

## Probes

`target/r16-scratch/broad/` (one pattern a file, a checksum printed),
whole-process wall time, minimum of 5 runs; the 0.0.23 column is round
one's run of the same executable on the same machine (its `array_push_pop`
takes 915 s, the `pop` defect). Geomean over the 43: 0.599x of 0.0.23,
0.966x of round one, 1.385x of Node. Furthest from Node:
`arguments_obj` 7.6x, `closure_make` 4.4x, `spread_obj` 3.4x,
`spread_call` 3.3x, `set_add_has` 3.0x, `rest_params` 2.9x,
`map_set_get` 2.8x, `obj_literal_nested` 2.8x. Moved most against round
one: `string_replace` 187 to 116 ms, `template_lit` 129 to 91,
`optional_chain` 93 to 61, `arguments_obj` 293 to 272.

## Python suite

`tools/python_bench.py`, 5 interleaved repetitions, CPython 3.13, work
time medians, `head-5cffc37a.exe` and `fin.exe` in one run: ZIPP over
CPython 1.31x to **1.09x** (micro 1.18x to 1.00x, macro 2.22x to
1.68x); the 44 programs in 0.67x of 0.0.23's time (micro 0.84x, macro
0.42x). Faster than CPython (15): `range_loop` 0.20x, `float_arith`
0.25x, `tuple_swap` 0.27x, `attr_read_write` 0.42x, `global_read` 0.43x,
`int_arith` 0.47x, `dict_get` 0.51x, `list_index` 0.64x, `builtin_len`
0.71x, `str_long_len_index` 0.77x, `json_roundtrip` 0.85x,
`tuple_unpack` 0.87x, `hash_str` 0.93x, `list_append` 0.93x, `for_list`
0.96x. Still above 2x: `str_methods` 3.43x, `generator_iter` 2.84x,
`try_raise_catch` 2.72x, `instance_new` 2.63x, `word_count` 2.32x,
`fstring` 2.17x, `sort_heapq` 2.17x, `super_call` 2.10x, `oo_simulation`
2.03x.

An earlier run on `7da3519d` read 1.10x (micro 1.01x, macro 1.72x); its
`call_simple`, `call_defaults` and `call_keywords` were 1.05x to 1.08x of
0.0.23 on fresh copies, which `79759baa` fixed: 0.98x to 0.99x here,
`fib_recursive` 0.90x.

## torch against PyTorch

`--only torch --torch-cpython "py -3.11" --warm 5 --reps 3`, PyTorch
2.11 on one thread, warm medians:

| Program | PyTorch | 0.0.23 | This round | This round / PyTorch |
| --- | ---: | ---: | ---: | ---: |
| `torch_cnn` | 3.3 | 24.8 | 11.8 | 3.60x |
| `torch_dataloader` | 3.8 | 67.8 | 10.2 | 2.67x |
| `torch_matmul` | 0.2 | 3.8 | 0.9 | 4.99x |
| `torch_matmul_big` | 0.7 | 18.3 | 2.6 | 3.79x |
| `torch_mlp` | 4.1 | 65.8 | 18.5 | 4.50x |
| `torch_rnn` | 6.1 | 164.8 | 29.6 | 4.88x |
| `torch_small_ops` | 8.5 | 95.4 | 33.8 | 3.98x |
| `torch_transformer` | 4.5 | 87.5 | 28.7 | 6.33x |

Geomean 16.96x to 4.22x PyTorch's time. On a cold first run ZIPP is
faster than PyTorch on `cnn` (16.7 against 695.8 ms), `mlp`, `rnn` and
`transformer`. `torch_transformer` prints PyTorch's numbers to the third
decimal and differs in the fourth, as in round one.

## WebAssembly size

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125, Node 24.19.0; Brotli
quality 11 with the size hint. Module bytes, raw / Brotli, Windows builds
of `5cffc37a` (0.0.23's code, `r16base`) and `35e6d1dc` (`r2f`):

| Variant | 0.0.23 | 0.0.24 | Change |
| --- | ---: | ---: | ---: |
| Lite | 2,573,473 / 697,512 | 2,450,334 / 671,251 | -123,139 (-4.78%) / -26,261 (-3.76%) |
| JavaScript | 4,636,188 / 1,180,196 | 4,435,704 / 1,133,411 | -200,484 (-4.32%) / -46,785 (-3.96%) |
| Python-base | 6,331,149 / 1,567,394 | 6,146,540 / 1,524,284 | -184,609 (-2.92%) / -43,110 (-2.75%) |
| Python + Torch | 8,255,034 / 1,918,625 | 8,178,282 / 1,898,530 | -76,752 (-0.93%) / -20,095 (-1.05%) |

The torch package is 2,156,516 / 401,594 (0.0.23: about 0.38 MB after
Brotli). The commits after the agents' merge (the final-sigma fix, the
#41 to #45 fixes, `timeOrigin` and `79759baa`) moved the four modules by
+2,968 to +3,821 bytes raw and -281 to +1,651 after Brotli, against the
merged tree's build (`r16m`).

## WebAssembly speed screen

`crates/zipp-wasm/tests/node/bench-size-pass.cjs` (optimizing compiler,
10 warm-up pairs, 31 alternating paired samples), `r2f` against
`r16base`, both orders; each cell is the geometric mean of the two orders'
ratios (0.0.24's time over 0.0.23's).

| Case | Lite | JavaScript |
| --- | ---: | ---: |
| `alloc-object` | 1.027 | 0.998 |
| `arith-float` | 1.012 | 1.019 |
| `arith-int` | 1.109 | 0.991 |
| `arith-mod` | 1.119 | 1.015 |
| `array-build` | 0.999 | 0.995 |
| `array-hof` | 1.018 | 1.010 |
| `call-deep` | 0.971 | 1.008 |
| `enumeration` | 1.008 | 1.007 |
| `error-range` | 0.966 | 0.928 |
| `error-type` | 0.967 | 0.980 |
| `error-typedarray` | 0.982 | 0.982 |
| `javascript_init_0_functions` | 1.001 | 0.997 |
| `javascript_init_200_functions` | 0.997 | 1.007 |
| `json` | 0.988 | 0.999 |
| `promise-resolve` | 0.993 | 0.967 |
| `prop-mono` | 1.012 | 1.012 |
| `prop-poly` | 1.013 | 1.012 |
| `property-delete` | 0.971 | 1.016 |
| `recursive-calls` | 1.019 | 1.024 |
| `regex` | 0.982 | 0.977 |
| `regexp-matchall` | 0.995 | 1.022 |
| `regexp-unicode-compile` | 0.631 | 0.897 |
| `string-append-index` | 0.989 | 1.021 |
| `string-build` | 0.993 | 1.006 |
| geomean | **0.986** | **0.995** |

Lite's `arith-int` and `arith-mod` read 1.195 and 1.196 in one order and
1.028 and 1.046 in the other; run alone (the case argument, a fresh
process each), three times in each order, they read 1.016 (0.951 to
1.046) and 0.986 (0.908 to 1.008). The full screen's order, not the
modules, made them slow.

torch in the complete module under Node, 20 warm runs of `torch_cnn` in
one process, two processes each, quiet machine: medians 21.0 and 21.1
ms before `69471503`, 20.6 and 21.2 after it, 21.7 and 19.6 in `r2f`,
37.4 and 37.5 in `r16base`; first runs 118 to 126 ms against 341 to 344.

## Correctness

- Test262: 95,671 of 95,680 executions, the nine documented expected
  failures.
- Gate A (release CLI, full Test262 with the expected-failure manifest,
  the Python corpus in four modes, `cargo test -p zipp-cli`) and gate B
  (the default zipp-vm suite from a snapshot of its test executables,
  the Python lib and integration tests, the safe-sandbox suite, five
  `-D warnings` profiles) on `2cb7176f`, on the merged `d83fb358`, and
  on `35e6d1dc`. Test262: 0 unexpected failures, 0 stale expectations, 0
  skips each time. The corpus: 183 of 188 programs on `2cb7176f`, 190 of
  195 after the Python agent's seven, the five GPU and transport programs
  as always.
- GitHub CI and the full security workflow at `7da3519d`: both passed.
- Each commit of the #41 to #45 series checked alone (default and
  safe-sandbox `cargo check --tests`).
- The Node boundary suite on the merged modules (`r16m`) and on the
  release's (`r2f`): JavaScript, Lite and Python + Torch 36 of 36,
  Python-base 34 of 36 (the two programs that need the torch package, as
  in round one).
- Each fix's matrix against Node 24 with the JIT, without it and under
  ZIPP_GC_STRESS=1.

Two process faults this round, both caught. `2cb7176f` was pushed when
gate B's default suite had finished but its Python, safe-sandbox and
`-D warnings` steps had not (the suite's `DONE` line is not the gate's
end); those steps were run on `2cb7176f` afterwards in a side worktree
and passed, and its `python_corpus` failure in the interrupted run came
from the merge adding the Python agent's corpus programs to the checkout
mid-run. And the first security dispatch named a mistyped commit; it
was cancelled and dispatched again with the full SHA.

## Leads

- `performance.mark`/`measure` with the rest of User Timing (entries,
  their getters, `detail` cloned, Node's errors).
- `queueMicrotask` is not defined (the async agent's finding).
- `arr.push(Promise.resolve(x))`: a million take 74 ms, as in 0.0.23,
  against Node's 60 (the async agent read it slower on a loaded
  machine).
- Probes furthest from Node: `arguments_obj` 7.6x, `closure_make` 4.4x,
  object spread 3.4x, spread calls 3.3x, Map/Set 2.8x to 3.0x.
- Python rows above 2x CPython (above), and the remaining time in the
  heap's allocation and sweep, the dispatch loop and generator resume
  (the Python agent's profile).
- Python: `__class__`'s cell is empty while the class body runs (the
  Python agent's finding).
- A `with` object's `has` trap runs twice for a read, as the
  specification has it; V8 runs it once.
