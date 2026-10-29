# Performance

## Performance, measured honestly

Start with the recorded native results. These are stamped benchmark captures,
with the source revisions and raw evidence below; they are not a new measurement
of every change on `main`. Ratios are **Zipp / competitor**, so **lower is faster**.
All of them were measured on 29 October 2026 with the 0.0.31 engine.

| Native workload group | Zipp / Node geomean | Coverage |
|---|---:|---|
| All 30 workloads | **0.629×** | 13 normal + 17 hostile rows, weighted equally. |
| Normal workloads | **0.543×** | All 13 normal rows; faster than Node on every one. |
| Hostile workloads | **0.704×** | All 17 stress-oriented rows. |

Those are JIT against JIT: every engine runs as it ships. Run as a pure
interpreter (`ZIPP_NOJIT=1 ZIPP_NO_RX_JIT=1`: no native code), Zipp measures **0.574×** Bellard's QuickJS and
**0.326×** QuickJS-NG over the same 30 rows; see
[Interpreter versus interpreter](#interpreter-versus-interpreter). Its Python
frontend measures 1.004× CPython 3.13.14 by in-process work time over 36
programs (0.916× on the 30 micro kernels); see [Python versus CPython](#python-versus-cpython).

Median process launch in the canonical four-engine capture is **9.1 ms** for
Zipp, **32.3 ms** for Node, **53.0 ms** for Bun and **84.9 ms** for Deno.

> The goal is ambitious and literal: become faster than Node, Bun, and Deno on
> every maintained benchmark while preserving exact output and tier parity.
> Zipp is not there yet. The tables below show both the wins and the remaining
> gaps.

### Canonical public capture

<details>
<summary><strong>Full Node, Bun and Deno results, confidence intervals and methodology</strong></summary>

The canonical table is the clean PGO capture of the release tag `fcfb1934`
(2026-10-29), the 0.0.31 engine.
[`real13_fcfb1934_pgo_2026-10-29.json`](../bench/real13_fcfb1934_pgo_2026-10-29.json)
and [`head_clean_fcfb1934_pgo_2026-10-29.json`](../bench/hostile/head_clean_fcfb1934_pgo_2026-10-29.json).
Both artifacts record `publishable:true`, `ALL_CORRECT=1`, 15 complete
counterbalanced repetitions, 10,000 bootstrap samples, exact output, and no
source, engine, input, environment, process-health, or harness drift. Every
engine runs with its production JIT.

Node v24.19.0 · Bun 1.3.14 · Deno 2.6.10 · Zipp 0.0.31 canonical PGO SHA-256
`8380e0a6…e07452`.

Cold medians include process launch; bold marks the lowest displayed median.

| Retained benchmark | Node | Bun | Deno | Zipp | Zipp / Node |
|---|---:|---:|---:|---:|---:|
| async-promise-chain | 345 ms | 380 ms | 370 ms | **238 ms** | **0.70×** |
| class-prototype-hot | 304 ms | 344 ms | 328 ms | **229 ms** | **0.76×** |
| json-large | 289 ms | **213 ms** | 335 ms | 223 ms | **0.78×** |
| map-set-heavy | 808 ms | 856 ms | 1,340 ms | **549 ms** | **0.68×** |
| markdown-render | 284 ms | 229 ms | 325 ms | **187 ms** | **0.66×** |
| parse-large-js | 278 ms | 240 ms | 291 ms | **223 ms** | **0.80×** |
| polymorphic-objects | 335 ms | 342 ms | 338 ms | **214 ms** | **0.64×** |
| regex-log-scan | 488 ms | 586 ms | 482 ms | **421 ms** | **0.88×** |
| sparse-array | 82 ms | 110 ms | 127 ms | **74 ms** | **0.90×** |
| typedarray-math | 205 ms | 927 ms | 173 ms | **141 ms** | **0.69×** |
| **Zipp / engine paired geomean** | **0.744×** [0.739, 0.750] | **0.627×** [0.624, 0.630] | **0.657×** [0.603, 0.668] | — | — |

The three architecture diagnostics remain outside the retained-ten headline:

| Diagnostic | Node | Bun | Deno | Zipp | Zipp / Node |
|---|---:|---:|---:|---:|---:|
| polymorphic-objects-v2 | 84 ms | 97 ms | 132 ms | **24 ms** | **0.29×** |
| property-ic-shapes | 260 ms | 167 ms | 322 ms | **11 ms** | **0.04×** |
| sparse-array-v2 | 174 ms | 379 ms | 189 ms | **97 ms** | **0.56×** |
| **Zipp / engine paired geomean** | **0.190×** [0.185, 0.193] | **0.162×** [0.160, 0.164] | **0.149×** [0.102, 0.153] | — | — |

Across all 13 normal rows, Zipp measures **0.543× Node** [0.538, 0.546],
**0.459× Bun** [0.456, 0.461], and **0.467× Deno** [0.376, 0.474]. It is
faster than Node on all 13, and wins 38 of 39 point comparisons and 38 of 39
Bonferroni exact-sign comparisons; Bun is faster on json-large (1.05×).

The separately measured 17-case hostile corpus covers closures, mixed locals,
shape churn, GC survival, async lifetimes, modules, a React-shaped kernel, a
warm router, a JavaScript bytecode VM, and vendored NanoID:

| Hostile metric | vs Node | vs Bun | vs Deno |
|---|---:|---:|---:|
| ordinary equal-row geomean | **0.704×** [0.700, 0.709] | **0.502×** [0.497, 0.508] | **0.390×** [0.386, 0.392] |
| category-balanced geomean | **0.738×** [0.734, 0.742] | **0.517×** [0.511, 0.524] | **0.404×** [0.400, 0.406] |

For the requested project-wide view, the explicit equal-row point across
all 30 normal and hostile rows is **0.629× Node**, **0.483× Bun** and
**0.422× Deno**, calculated as `exp((13 × ln(G13) + 17 × ln(G17)) / 30)`
from the two suite geomeans above. No interval was computed for it this time;
each suite's own interval is given with it.

The aggregate is ahead, but the literal every-row target is not met. Zipp has
25 of 30 Node point wins: all 13 normal rows and 12 of the 17 hostile ones.
The Node point gaps are all in the hostile set: allocation survival (1.34×), react-shaped reconcile (1.22×), warm router (1.13×), closure calls (1.12×), megamorphic shapes (1.04×).

```text
FASTER_THAN_NODE_ON_EVERY_ROW=0
FASTER_THAN_EVERY_ENGINE_ON_EVERY_ROW=0
```

Against the previous canonical capture at `84012a2d` (2026-10-21, `zipp 0.0.28`;
headline ten 0.766× Node, all 13 0.545×, hostile 0.748×, all 30 0.652×),
the headline ten moved to 0.744×, all 13 to 0.543×, the hostile suite to 0.704× and all 30 to 0.629×.
That capture's artifacts stay in [`bench/`](../bench/README.md) as a prior series
point. See the [`bench` guide](../bench/README.md),
[hostile suite](../bench/hostile/README.md), and
[`docs/PERF_ROADMAP.md`](PERF_ROADMAP.md) for exact methodology and remaining work.

</details>

### The release build as downloaded

From 0.0.31 the published Windows x86-64, Linux x86-64 and Linux arm64 CLIs
are profile-guided builds (`release-build.yml`, `tools/release/pgo-build.sh`):
the training corpus of the canonical build, run on the runner by an
instrumented build of the release feature set (Python, GPU and Wasmtime
included), so the download keeps its features and gains the profile. The armv7
CLI and the WebAssembly modules stay plain builds; up to 0.0.30 every published
binary was one.
The 0.0.31 Windows CLI as downloaded ([`release_real13_fcfb1934_2026-10-29.json`](../bench/comparison/release_real13_fcfb1934_2026-10-29.json),
the same four engines, 15 repetitions) runs the 13 normal rows at **0.549× Node**
[0.546, 0.553] and the headline ten at 0.757× [0.753, 0.759], faster than Node on
every row. A paired A/B of the same rows
([`pgo_ab_real13_fcfb1934_fcfb1934_2026-10-29.json`](../bench/comparison/pgo_ab_real13_fcfb1934_fcfb1934_2026-10-29.json)) puts the PGO
build at 0.996× the release build's time [0.991, 1.003]: the download
takes about 1.00× the canonical figures' time. The two builds differ in more
than their profiles, which come from different recipes: the canonical one has
JavaScript only (no GPU, Python or Wasmtime features, 11.6 MB against 24.9
MB), an explicit `x86-64` CPU target and the lld linker. The ratio compares the
release product with the canonical benchmark product; it is not the effect of
profile guidance alone, which would take a feature- and flag-matched pair to
measure.

### Release 0.0.31, as published

The 0.0.31 release notes measure the published 0.0.31 and 0.0.30 Windows
binaries against Bellard's QuickJS 2026-06-04 and Node 24.19.0 on 29 October
2026: an otherwise idle Ryzen 9 9950X3D, address-space randomization off,
each run pinned to one core, medians of interleaved runs. This is a release
comparison taken with scratch scripts, not a stamped capture like the ones
above. Ratios are 0.0.31 / the other engine.

| Bundled application, cache off (7 runs) | / 0.0.30 | / QuickJS, 96 MB-L3 die | / QuickJS, 32 MB-L3 die | / Node 24 |
|---|---:|---:|---:|---:|
| Babel | 0.87× | 0.83× | 0.98× | 3.94× |
| TypeScript | 0.95× | 0.84× | 0.89× | Node fails |
| Prettier | 0.89× | 0.93× | 0.96× | 1.14× |
| marked, lodash, acorn, terser, three.js, immer, RxJS, date-fns | 0.94× | 0.35× | 0.39× | 1.91× |
| Vue server rendering | 0.93× | 0.93× | 0.89× | 1.08× |
| Preact server rendering | 0.83× | 0.85× | 0.87× | 0.99× |
| React server rendering | 0.88× | QuickJS fails | QuickJS fails | 0.96× |

The canonical profile-guided build of the same source (`tools/pgo.sh`), which
the release does not ship, takes 0.83× to 0.91× QuickJS's time on Babel,
TypeScript, Prettier, Vue and Preact on the 96 MB-L3 die and 0.87× to 0.97×
on the other. Over the 78 benchmark programs of the release notes (3 runs
each, outputs identical), the interpreter (`ZIPP_NOJIT=1`, regex JIT on)
takes 0.912× 0.0.30's time and 0.742× QuickJS's, faster than QuickJS on 62;
the JIT lane takes 0.892× 0.0.30's time and 0.857× Node 24's. The full tables, with
peak working sets, the TypeScript benchmarks and WebAssembly, are in
[docs/releases/0.0.31.md](releases/0.0.31.md#performance).

### Release 0.0.30, as published

The 0.0.30 release notes measure the published 0.0.30 and 0.0.29 Windows
binaries against Bellard's QuickJS 2026-06-04 on 27 October 2026: an
otherwise idle Ryzen 9 9950X3D, address-space randomization off, each run
pinned to one core, medians of interleaved runs. This is a release
comparison taken with scratch scripts, not a stamped capture like the ones
above. Ratios are 0.0.30 / the other engine.

| Bundled application, cache off (7 runs) | / 0.0.29 | / QuickJS, 96 MB-L3 die | / QuickJS, 32 MB-L3 die |
|---|---:|---:|---:|
| Babel | 0.77× | 0.94× | 1.03× |
| TypeScript | 0.81× | 0.87× | 0.90× |
| Prettier | 0.83× | 1.02× | 1.04× |
| marked, lodash, acorn, terser, three.js, immer, RxJS, date-fns | 0.86× | 0.36× | 0.38× |
| Vue server rendering | 0.87× | 0.97× | 0.98× |
| Preact server rendering | 0.95× | 0.98× | 1.01× |
| React server rendering | 0.90× | QuickJS fails | QuickJS fails |

A profile-guided build of the same engine (`tools/pgo.sh`), which the
release does not ship, takes 0.82× to 0.94× QuickJS's time on Babel,
TypeScript, Prettier, Vue and Preact on the 96 MB-L3 die and 0.85× to 0.95×
on the other. Over the 78 benchmark programs of the release notes
(3 runs each, outputs identical), the interpreter (`ZIPP_NOJIT=1`, regex JIT
on) takes 0.917× 0.0.29's time and 0.818× QuickJS's, faster than QuickJS on
54; the JIT lane takes 0.971× 0.0.29's time. The full tables, with peak
working sets, are in
[docs/releases/0.0.30.md](releases/0.0.30.md#performance).

### Interpreter versus interpreter

The tables above compare production JITs. This one asks the other question:
how fast is Zipp's interpreter against other interpreters? The same canonical
PGO binary runs with `ZIPP_NOJIT=1 ZIPP_NO_RX_JIT=1`, emitting no native code at all (its JavaScript
JIT and its regex JIT both off; `ZIPP_NOJIT=1` alone leaves the regex JIT on,
as every capture before 22 October did), against
Bellard's QuickJS 2026-06-04 and QuickJS-NG 0.17.0 (neither has a JIT), and
against the three JIT engines with their JITs switched off: Node `--jitless`
(V8's Ignition), Deno `--v8-flags=--jitless`, and Bun with `BUN_JSC_useJIT=0`
(JavaScriptCore's LLInt). [`run_native.py`](../bench/comparison/run_native.py)
staged identical inputs for every engine, checked every output against Node,
and ran 8 counterbalanced repetitions. The artifacts,
[`interp_real13_fcfb1934_2026-10-29.json`](../bench/comparison/interp_real13_fcfb1934_2026-10-29.json) and
[`interp_hostile17_fcfb1934_2026-10-29.json`](../bench/comparison/interp_hostile17_fcfb1934_2026-10-29.json),
are diagnostics, not canonical captures. Zipp's interpreter runs as it ships,
including its exact recognizers for a few loop shapes.

| Zipp interpreter / engine | Normal 13 | Hostile 17 | All 30 | Rows Zipp wins |
|---|---:|---:|---:|---:|
| QuickJS | 0.514× [0.512, 0.517] | 0.625× [0.612, 0.627] | **0.574×** | 30 / 30 |
| QuickJS-NG | 0.326× [0.321, 0.327] | 0.327× [0.324, 0.328] | **0.326×** | 30 / 30 |
| Node jitless | 1.010× [1.001, 1.017] | 0.843× [0.835, 0.847] | **0.911×** | 21 / 30 |
| Deno jitless | 0.964× [0.957, 0.968] | 0.733× [0.727, 0.739] | **0.825×** | 24 / 30 |
| Bun no-JIT | 0.698× [0.693, 0.703] | 0.658× [0.652, 0.662] | **0.675×** | 28 / 30 |

<details>
<summary><strong>Interpreter results, row by row</strong></summary>

Cold medians include process launch; bold marks the fastest interpreter.

| Benchmark | Zipp interpreter | QuickJS | QuickJS-NG | Node jitless | Deno jitless | Bun no-JIT |
|---|---:|---:|---:|---:|---:|---:|
| async-promise-chain | **332 ms** | 3,989 ms | 3,898 ms | 457 ms | 448 ms | 562 ms |
| class-prototype-hot | 3,292 ms | 3,837 ms | 7,241 ms | **1,719 ms** | 1,786 ms | 5,008 ms |
| json-large | **463 ms** | 786 ms | 1,428 ms | 532 ms | 552 ms | 646 ms |
| map-set-heavy | **575 ms** | 1,532 ms | 2,164 ms | 819 ms | 1,229 ms | 1,107 ms |
| markdown-render | **571 ms** | 1,074 ms | 2,066 ms | 767 ms | 796 ms | 972 ms |
| parse-large-js | **1,321 ms** | 2,041 ms | 3,671 ms | 1,388 ms | 1,388 ms | 1,588 ms |
| polymorphic-objects | 663 ms | 861 ms | 1,411 ms | 596 ms | **561 ms** | 1,308 ms |
| regex-log-scan | **1,452 ms** | 2,533 ms | 4,005 ms | 1,546 ms | 1,538 ms | 2,771 ms |
| sparse-array | 257 ms | 387 ms | 782 ms | **208 ms** | 227 ms | 276 ms |
| typedarray-math | 3,814 ms | 3,848 ms | 6,528 ms | **2,262 ms** | 2,383 ms | 3,207 ms |
| polymorphic-objects-v2 | **298 ms** | 399 ms | 851 ms | 300 ms | 325 ms | 419 ms |
| property-ic-shapes | 1,984 ms | 2,027 ms | 5,187 ms | 1,955 ms | 1,978 ms | **1,810 ms** |
| sparse-array-v2 | **279 ms** | 1,843 ms | 875 ms | 325 ms | 306 ms | 484 ms |
| **Zipp interpreter / engine geomean** | — | **0.514×** [0.512, 0.517] | **0.326×** [0.321, 0.327] | 1.010× [1.001, 1.017] | **0.964×** [0.957, 0.968] | **0.698×** [0.693, 0.703] |

| Hostile row | Zipp interpreter | QuickJS | QuickJS-NG | Node jitless | Deno jitless | Bun no-JIT |
|---|---:|---:|---:|---:|---:|---:|
| calls-baseline | **86 ms** | 112 ms | 251 ms | 104 ms | 120 ms | 107 ms |
| calls-closures | 199 ms | 228 ms | 452 ms | **178 ms** | 196 ms | 247 ms |
| shapes-stable | **78 ms** | 132 ms | 236 ms | 88 ms | 104 ms | 89 ms |
| shapes-megamorphic | 121 ms | 194 ms | 328 ms | **116 ms** | 130 ms | 186 ms |
| types-stable | 132 ms | 163 ms | 375 ms | **130 ms** | 148 ms | 132 ms |
| types-churn | **80 ms** | 96 ms | 202 ms | 98 ms | 111 ms | 114 ms |
| branch-control | **91 ms** | 110 ms | 236 ms | 105 ms | 118 ms | 103 ms |
| throw-catch | **82 ms** | 98 ms | 293 ms | 164 ms | 145 ms | 153 ms |
| allocation-ephemeral | **102 ms** | 250 ms | 365 ms | 120 ms | 142 ms | 260 ms |
| allocation-survival | **98 ms** | 174 ms | 269 ms | 105 ms | 123 ms | 132 ms |
| async-burst | **28 ms** | 103 ms | 125 ms | 61 ms | 78 ms | 43 ms |
| async-lived | **28 ms** | 108 ms | 127 ms | 50 ms | 70 ms | 65 ms |
| reactish-reconcile | **66 ms** | 119 ms | 177 ms | 74 ms | 92 ms | 106 ms |
| warm-router | 90 ms | 121 ms | 474 ms | **79 ms** | 98 ms | 143 ms |
| bytecode-vm | **291 ms** | 411 ms | 946 ms | 302 ms | 316 ms | 486 ms |
| module-hot-graph | **95 ms** | 96 ms | 195 ms | 103 ms | 122 ms | 146 ms |
| npm-nanoid | **437 ms** | 699 ms | 1,168 ms | 454 ms | 472 ms | 813 ms |
| **Zipp interpreter / engine geomean** | — | **0.625×** [0.612, 0.627] | **0.327×** [0.324, 0.328] | **0.843×** [0.835, 0.847] | **0.733×** [0.727, 0.739] | **0.658×** [0.652, 0.662] |

</details>

Over all 30 rows Zipp's interpreter is faster than Bellard's QuickJS (0.574×, 30 of 30 rows;
0.683× in the previous capture), faster than QuickJS-NG (0.326×, 30 of 30),
faster than JavaScriptCore's LLInt (0.675×), faster than V8's Ignition under Deno
(0.825×; 0.995× before) and faster than it under Node (0.911×; 1.105× before).
Bellard's QuickJS wins no row.
The five downscaled micro kernels were last measured on 15 October
([`interp_micro5_fa8e3b86_2026-10-15.json`](../bench/comparison/interp_micro5_fa8e3b86_2026-10-15.json)):
there Zipp was 1.12× Bellard's QuickJS (179 ms against 108 on recursive `fib`),
0.71× QuickJS-NG and 1.26× Node `--jitless`; calls and tight loops are where the
remaining interpreter cost is.

Embedders who build the safe-sandbox profile (no `unsafe`, no JIT, with step
and heap limits set) run the 13 normal rows at **0.843×** Bellard's QuickJS and
**0.525×** QuickJS-NG, in 0.583× of release 0.0.24's time and with a peak
working set 1.18× Bellard's QuickJS's; see the
[0.0.25 release notes](releases/0.0.25.md#embedding-the-safe-sandbox-profile).

### Python versus CPython

Zipp compiles Python 3 to the same register bytecode.
[`tools/python_bench.py`](../tools/python_bench.py) runs every program under
`tools/python_bench/` as a fresh process per engine per repetition, interleaved,
15 repetitions, against CPython 3.13.14; every run's output must equal
CPython's. Each program times its own workload in-process, and that work time
is the ratio; wall time, which adds process start and compilation, is recorded
beside it. The binary is the published 0.0.31 release build (the PGO build has
no Python), run with its JIT and again with `ZIPP_NOJIT=1`, since standard
CPython is an interpreter. The 8 torch programs have no CPython counterpart
and are left out.
Artifacts: [`cpython_fcfb1934_2026-10-29.json`](../bench/python/cpython_fcfb1934_2026-10-29.json) and
[`cpython_nojit_fcfb1934_2026-10-29.json`](../bench/python/cpython_nojit_fcfb1934_2026-10-29.json) (that run sets
`ZIPP_NOJIT=1` for the whole harness; CPython ignores it).

| Zipp / CPython, geometric mean | With JIT | No JIT |
|---|---:|---:|
| 30 micro kernels, work time | **0.916×** | 1.384× |
| 6 application-shaped programs, work time | 1.591× | 1.630× |
| all 36, work time | 1.004× | 1.422× |
| all 36, whole-process wall time | **0.494×** | **0.538×** |

With its JIT, Zipp's Python is faster than CPython on 15 of 36 programs, most
on range_loop (0.19×), float_arith (0.25×), tuple_swap (0.26×), for_list (0.37×).
CPython is faster on the rest, most on str_methods (2.95×), generator_iter (2.57×), try_raise_catch (2.49×), instance_new (2.32×), sort_heapq (2.07×):
string methods, generators, exceptions, object creation and f-strings are
where the Python frontend's remaining work is.
No intervals are computed for
these geometric means; the artifacts hold every observation.

### Earlier QuickJS-NG, Boa and WebAssembly comparisons

These sections preserve earlier measurements and module snapshots. They
describe the named captures, including their limitations, rather than the
current release artifact; the native interpreter rows above supersede their
native results.

<details>
<summary><strong>Historical interpreter and WebAssembly comparisons</strong></summary>

#### Historical v0.0.5 release

The v0.0.5 release was also measured against pinned interpreter builds of
QuickJS-NG v0.16.2 and Boa v0.22.0. These are clean release-default builds on
the same Windows x86-64 host, with identical generated source, exact-output
validation, six counterbalanced repetitions, and 10,000 paired-bootstrap
samples. Ratios are Zipp / competitor, so lower is faster.

| Native diagnostic | Zipp interpreter / competitor | 95% CI | point wins |
|---|---:|---:|---:|
| frozen real13 vs QuickJS-NG | **0.6413×** | 0.6386–0.6452 | 12 / 13 |
| micro5 vs QuickJS-NG | **0.8556×** | 0.8405–0.8761 | 5 / 5 |
| micro5 vs Boa | **0.2539×** | 0.2501–0.2590 | 5 / 5 |
| micro5 vs Boa `--optimize` | **0.2522×** | 0.2493–0.2593 | 5 / 5 |

The native result is an aggregate win, not a universal claim: QuickJS-NG was
1.0099× faster at the point median on the retained sparse-array row, while
Zipp led the other twelve. In the historical v0.0.5 browser-WASM release
capture, Zipp measured **0.2274× Boa** but **2.1074× QuickJS-NG** on adjusted
execution across the five diagnostic workloads. That release's stripped module
is 5,595,833 bytes raw (1,254,075 Brotli-11), between QuickJS-NG's
1,528,293-byte reactor (417,087 Brotli-11) and Boa's 21,296,176-byte module
(5,484,164 Brotli-11).

#### v0.0.6 native confirmation

The clean default-feature v0.0.6 release binary at engine-source commit
`e3acee352074` reran all 13 current real13 inputs against QuickJS-NG v0.16.2,
with the runner selecting Zipp's interpreter through `ZIPP_NOJIT=1`. All 39
canonicalized validation outputs matched after the documented QuickJS CRLF-to-LF
normalization; raw output bytes and hashes remain recorded. Across six
counterbalanced rounds, Zipp won all 13 point medians: Zipp / QuickJS-NG was
`0.6089665×` (descriptive 95% interval 0.6072021–0.6122180) for cold
fresh-process time and `0.6058409×` (0.6041440–0.6090422) after paired
empty-launch subtraction. This confirms the native interpreter result on the
final engine code; it does not predict WASM performance.

#### v0.0.6 browser-WASM status

The production module recorded in this historical comparison, built from v0.0.13, is 5,558,860 bytes raw, 1,812,458 at
gzip-9, and 1,248,649 at Brotli-11 (SHA-256
`bd8614fe5f3a3b8ef67f4b917cdefebb3fe69afa39a9804a0d3f6b0b6b267126`). The
official QuickJS-NG v0.16.2 reactor is 1,528,293 bytes raw and 417,087 at
Brotli-11, so Zipp is `3.586×` as large raw and `2.958×` as large on the wire.

Main has moved past that module. On 2026-09-05 an external audit of the
WASM build was implemented as B274-B278 (see
[`docs/PERF_ROADMAP.md`](PERF_ROADMAP.md)): interpreter-side changes that close
four cliffs the native PGO capture never sees. Every unit-addressed read on a
non-ASCII string decoded from byte zero, so scanning loops were quadratic;
the string-part allocation preflight walked the whole heap on a window blind
to the heap's size; `eval` / `new Function` code owned no inline caches; and
an array with a named property lost its dense read path. The figures below
are interleaved A/B medians of the wasm artifact built from `400bcfe3`
against the same artifact with these changes, on a shared developer machine
with other work running, so they are diagnostic, not a canonical capture;
the control kernels (ASCII scans, plain and fused calls, main-code property
loops) moved within ±4%.

| Wasm kernel | `400bcfe3` | with B274-B278 |
|---|---:|---:|
| sequential `charCodeAt` over 64K non-ASCII units | 4,462 ms | 3.2 ms |
| word tokenizer over 64K mostly-ASCII units with a few accents | 9,828 ms | 8.2 ms |
| one-unit `slice` loop, 16K non-ASCII units | 449 ms | 4.3 ms |
| `join` of 4,000 parts × 200, 300K objects retained | 8,423 ms | 138 ms |
| `join` of 4,000 parts × 200, small heap | 548 ms | 130 ms |
| monomorphic property loop installed through `new Function` | 20.9 ms | 14.8 ms |
| `a[i]` loop on an array carrying a named property | 13.3 ms | 9.6 ms |

That recorded module predates these changes. The harness that produced the rows is
`crates/zipp-wasm/tests/node/bench.cjs`-style (persistent `Engine`, warmed,
interleaved builds).

We also attempted a direct, unscaled WASM run over the same v0.0.6
normal 13 and hostile 17 sources used by the v0.0.6 Node/Bun/Deno reruns in
`target/bench-results/real13-v006-6650647a718c-pgo-15.json` and
`target/bench-results/hostile17-v006-6650647a718c-pgo-15.json`. Those sources
are newer than the retained canonical public capture below. The WASM capture
preserves their exact bytes and Node output oracle, but it is explicitly
`publishable:false`: the production Zipp WASM API cannot load the two module
rows, QuickJS-NG's official reactor cannot drain pending jobs for three async
rows, and Zipp validated only 7 of the 28 script rows. Seventeen Zipp rows hit
the fixed production instruction or heap ceilings and four ended in other
engine errors. There were consequently no comparable normal-suite rows and
only five comparable hostile rows.

On those five available rows, Zipp / QuickJS-NG was `0.9604×` for persistent
time and `0.9567×` after paired-control subtraction, with Zipp ahead only on
`warm-router` (1 / 5 point wins). Those are incomplete row-level diagnostics,
not full-suite geomeans: this run does **not** establish that Zipp WASM is faster
than QuickJS-NG WASM. Zipp's separately sampled compile median was slower
(5.080 ms versus 1.795 ms), while its instantiation/start median was faster
(0.397 ms versus 1.676 ms). Their sums are not a measured end-to-end median.

The separate five-workload speed-kernel experiment remains useful attribution
evidence: it measured `0.0954663913×` QuickJS-NG on persistent time. It is highly
specialization-sensitive, however; disabling the exact workload lanes measured
`1.815×` QuickJS-NG but `0.199×` Boa, with Zipp ahead of Boa on all five rows.
That control used a dirty-tree diagnostic candidate, not the release artifact.
No current same-source normal-13-plus-hostile-17 Boa WASM run exists, so neither
micro result is a general interpreter ranking or a substitute for the
incomplete exact-suite result above.

The commands, exact revisions, all validation failures, per-row numbers, module
hashes, host-interface differences, and limitations are in
[`bench/comparison/README.md`](../bench/comparison/README.md). These ecosystem
comparisons are deliberately separate from the canonical Node/Bun/Deno series
below.

</details>
