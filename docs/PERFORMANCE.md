# Performance

## Performance, measured honestly

Start with the recorded native results. These are stamped benchmark captures,
with the source revisions and raw evidence below; they are not a new measurement
of every change on `main`. Ratios are **Zipp / competitor**, so **lower is faster**.
All of them were measured on 24 November 2026 with the **0.0.40** engine (`0cd3eab8`).

| Native workload group | Zipp / Node geomean | Coverage |
|---|---:|---|
| All 30 workloads | **0.583×** | 13 normal + 17 hostile rows, weighted equally (**29 / 30** wins). |
| Normal workloads | **0.532×** | All 13 normal rows; faster than Node on every one. |
| Hostile workloads | **0.624×** | All 17 stress-oriented rows; faster than Node on 16 of 17. |

Those are JIT against JIT: every engine runs as it ships. Run as a pure
interpreter (`ZIPP_NOJIT=1 ZIPP_NO_RX_JIT=1`: no native code), Zipp measures **0.430×** Bellard's QuickJS and
**0.330×** QuickJS-NG over the same 30 rows (winning 30 / 30 against both); see
[Interpreter versus interpreter](#interpreter-versus-interpreter). Its Python
frontend measures **0.626×** CPython 3.13.14 by in-process work time over 36
programs (**0.601×** micro, **0.767×** macro, 26 / 36 wins); see [Python versus CPython](#python-versus-cpython).

Median process launch in the canonical four-engine capture is **11.1 ms** for
Zipp, **36.3 ms** for Node, **55.6 ms** for Bun and **92.3 ms** for Deno.

> The goal is ambitious and literal: become faster than Node, Bun, and Deno on
> every maintained benchmark while preserving exact output and tier parity.
> Zipp is one Node row away (`allocation-survival`, 1.14×). The tables below show both the wins and the remaining
> gaps.

### Canonical public capture

<details>
<summary><strong>Full Node, Bun and Deno results, confidence intervals and methodology</strong></summary>

The canonical table is the clean PGO capture of the release tag `0cd3eab8`
(2026-11-24), the 0.0.40 engine.
[`real13_0cd3eab8_pgo_2026-11-24.json`](../bench/real13_0cd3eab8_pgo_2026-11-24.json)
and [`head_clean_0cd3eab8_pgo_2026-11-24.json`](../bench/hostile/head_clean_0cd3eab8_pgo_2026-11-24.json).
Both artifacts record `publishable:true`, `ALL_CORRECT=1`, 15 complete
counterbalanced repetitions, 10,000 bootstrap samples, exact output, and no
source, engine, input, environment, process-health, or harness drift. Every
engine runs with its production JIT.

Node v24.19.0 · Bun 1.3.14 · Deno 2.6.10 · Zipp 0.0.40 canonical PGO SHA-256
`c34bb496…73ec43`.

Cold medians include process launch; bold marks the lowest displayed median.

| Retained benchmark | Node | Bun | Deno | Zipp | Zipp / Node |
|---|---:|---:|---:|---:|---:|
| async-promise-chain | 371 ms | 400 ms | 400 ms | **241 ms** | **0.65×** |
| class-prototype-hot | 312 ms | 353 ms | 341 ms | **219 ms** | **0.70×** |
| json-large | 324 ms | **243 ms** | 380 ms | 266 ms | **0.82×** |
| map-set-heavy | 1,143 ms | 1,086 ms | 1,738 ms | **810 ms** | **0.74×** |
| markdown-render | 309 ms | 244 ms | 372 ms | **200 ms** | **0.67×** |
| parse-large-js | 298 ms | 253 ms | 318 ms | **227 ms** | **0.76×** |
| polymorphic-objects | 348 ms | 350 ms | 355 ms | **220 ms** | **0.64×** |
| regex-log-scan | 513 ms | 625 ms | 525 ms | **440 ms** | **0.86×** |
| sparse-array | 88 ms | 121 ms | 139 ms | **59 ms** | **0.67×** |
| typedarray-math | 216 ms | 959 ms | 196 ms | **148 ms** | **0.69×** |
| **Zipp / engine paired geomean** | **0.717×** [0.708, 0.725] | **0.613×** [0.610, 0.620] | **0.616×** [0.456, 0.634] | — | — |

The three architecture diagnostics remain outside the retained-ten headline:

| Diagnostic | Node | Bun | Deno | Zipp | Zipp / Node |
|---|---:|---:|---:|---:|---:|
| polymorphic-objects-v2 | 90 ms | 101 ms | 151 ms | **27 ms** | **0.30×** |
| property-ic-shapes | 275 ms | 174 ms | 334 ms | **15 ms** | **0.05×** |
| sparse-array-v2 | 183 ms | 389 ms | 196 ms | **87 ms** | **0.47×** |
| **Zipp / engine paired geomean** | **0.197×** [0.195, 0.201] | **0.171×** [0.170, 0.174] | **0.150×** [0.099, 0.160] | — | — |

Across all 13 normal rows, Zipp measures **0.532× Node** [0.527, 0.537],
**0.457× Bun** [0.455, 0.461], and **0.445× Deno** [0.321, 0.460]. It is
faster than Node on all 13, and wins 38 of 39 point comparisons (Bun is faster on `json-large`).

The separately measured 17-case hostile corpus covers closures, mixed locals,
shape churn, GC survival, async lifetimes, modules, a React-shaped kernel, a
warm router, a JavaScript bytecode VM, and vendored NanoID:

| Hostile metric | vs Node | vs Bun | vs Deno |
|---|---:|---:|---:|
| ordinary equal-row geomean | **0.624×** [0.619, 0.630] | **0.464×** [0.460, 0.468] | **0.351×** [0.348, 0.354] |
| category-balanced geomean | **0.652×** [0.646, 0.659] | **0.476×** [0.471, 0.482] | **0.363×** [0.359, 0.366] |

For the requested project-wide view, the explicit equal-row point across
all 30 normal and hostile rows is **0.583× Node**, **0.461× Bun** and
**0.389× Deno**, calculated as `exp((13 × ln(G13) + 17 × ln(G17)) / 30)`
from the two suite geomeans above.

Zipp now wins **29 of 30** Node point comparisons: all 13 normal rows and **16 of the 17** hostile ones.
The single remaining Node point gap is in the hostile set: allocation-survival (1.14×) (down from five Node gaps in v0.0.31: allocation survival 1.34×, reactish-reconcile 1.22×, warm-router 1.13×, calls-closures 1.12×, shapes-megamorphic 1.04×).

```text
FASTER_THAN_NODE_ON_EVERY_ROW=0
FASTER_THAN_EVERY_ENGINE_ON_EVERY_ROW=0
```

Against the previous canonical capture at `fcfb1934` (2026-10-29, `zipp 0.0.31`;
headline ten 0.744× Node, all 13 0.543×, hostile 0.704×, all 30 0.629×, 25/30 wins),
the headline ten moved to 0.717×, all 13 to 0.532×,
the hostile suite to 0.624× and all 30 to 0.583× (29/30 wins).
Prior captures stay in [`bench/`](../bench/README.md) as earlier series points. See the [`bench` guide](../bench/README.md),
[hostile suite](../bench/hostile/README.md), and
[`docs/PERF_ROADMAP.md`](PERF_ROADMAP.md) for exact methodology and remaining work.

</details>

### The release build as downloaded

From 0.0.31 the published Windows x86-64, Linux x86-64 and Linux arm64 CLIs
are profile-guided builds (`release-build.yml`, `tools/release/pgo-build.sh`):
the training corpus of the canonical build, run on the runner by an
instrumented build of the release feature set (Python, PHP, Rust, GPU and Wasmtime
included), so the download keeps its features and gains the profile. The armv7
CLI and the WebAssembly modules stay plain builds; up to 0.0.30 every published
binary was one.
The 0.0.40 Windows CLI as downloaded ([`release_real13_0cd3eab8_2026-11-24.json`](../bench/comparison/release_real13_0cd3eab8_2026-11-24.json),
the same four engines, 15 repetitions) runs the 13 normal rows at **0.533× Node**
[0.530, 0.538] and the headline ten at **0.716×** [0.711, 0.724], faster than Node on
every row. A paired A/B of the same rows
([`pgo_ab_real13_0cd3eab8_0cd3eab8_2026-11-24.json`](../bench/comparison/pgo_ab_real13_0cd3eab8_0cd3eab8_2026-11-24.json)) puts the canonical PGO
build at **0.997×** the release build's time [0.988, 1.008]: the download
takes essentially 1.00× the canonical figures' time.

### Release 0.0.40, as measured

Release **0.0.40** (24 November 2026) captures a multi-round performance pass
across official JS and Python suites, real-world third-party JS and Python
applications (`linkedom`, `defuddle`, `webforai`, `html2text`, `bs4_css`,
`markdownify`), the regex engine, `safe-sandbox` WebAssembly builds, and the
PHP and Rust runtimes. All measurements below were run on Windows x86-64 with
**byte-identical output** against **Node.js v24.19.0** and **CPython 3.13**,
comparing a local **non-PGO fat-LTO** `v0.0.40` build against the published
**PGO** `v0.0.39` portable binary (see [`docs/releases/0.0.40.md`](releases/0.0.40.md)):

#### Official JS Suite (`bench/real` — 3-rep cold medians via `tools/bench.py`)

Startup: **ZIPP 10.2 ms** vs. Node.js 34.3 ms, Bun 52.7 ms, Deno 87.6 ms.

| Benchmark | Node.js | Bun | Deno | ZIPP `v0.0.40` | ZIPP / Node.js |
| --- | ---: | ---: | ---: | ---: | ---: |
| `async-promise-chain` | 357 ms | 405 ms | 386 ms | **277 ms** | **0.77×** |
| `class-prototype-hot` | 313 ms | 351 ms | 339 ms | **218 ms** | **0.70×** |
| `json-large` | 310 ms | 241 ms | 374 ms | **275 ms** | **0.90×** |
| `map-set-heavy` | 941 ms | 979 ms | 1612 ms | **837 ms** | **0.85×** |
| `markdown-render` | 297 ms | 244 ms | 348 ms | **199 ms** | **0.67×** |
| `parse-large-js` | 294 ms | 263 ms | 305 ms | **251 ms** | **0.82×** |
| `polymorphic-objects` | 353 ms | 351 ms | 346 ms | **307 ms** | **0.88×** |
| `polymorphic-objects-v2` | 92 ms | 103 ms | 143 ms | **33 ms** | **0.36×** |
| `property-ic-shapes` | 273 ms | 170 ms | 320 ms | **16 ms** | **0.06×** |
| `regex-log-scan` | 532 ms | 627 ms | 492 ms | **469 ms** | **0.88×** |
| `sparse-array` | 89 ms | 119 ms | 141 ms | **59 ms** | **0.68×** |
| `sparse-array-v2` | 187 ms | 389 ms | 192 ms | **91 ms** | **0.49×** |
| `typedarray-math` | 212 ms | 964 ms | 179 ms | **151 ms** | **0.70×** |
| **Geomean (13/13 wins vs. Node, 38/39 overall)** | — | — | — | — | **0.58×** *(0.78× headline)* |

#### Real-World Applications (Warm Steady-State Median & 8-Run Wall Clock)

| App | Reference | Ref Warm / Wall (8×) | `v0.0.39` (PGO) | `v0.0.40` Warm / Wall (8×) | Speedup vs. `v0.0.39` | `v0.0.40` / Ref (Warm / Wall) |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| **linkedom** | Node.js v24.19.0 | 1.31 ms / 336–337 ms | 9.56 ms | **6.14–6.21 ms** / **467–473 ms** | **1.55× faster** | 4.69–4.74× / **1.38–1.41×** |
| **defuddle** | Node.js v24.19.0 | 26.05–26.73 ms / 597–604 ms | 160.32 ms | **139.76–146.77 ms** / **1595–1650 ms** | **1.09×–1.15× faster** | 5.36–5.49× / **2.64–2.76×** |
| **webforai** | Node.js v24.19.0 | 17.57–18.30 ms / 521–522 ms | 107.57 ms | **102.18–107.16 ms** / **1277–1351 ms** | **1.00×–1.05× faster** | 5.58–6.10× / **2.45–2.59×** |
| **html2text** | CPython 3.13 | 5.60–5.67 ms / 198–206 ms | 41.15 ms | **14.31–15.39 ms** / **282–289 ms** | **2.67×–2.88× faster** | 2.55–2.72× / **1.37–1.46×** |
| **bs4_css** (BS4 + SoupSieve) | CPython 3.13 | 5.74–5.89 ms / 282–300 ms | 65.73 ms | **36.12–38.44 ms** / **482–502 ms** | **1.71×–1.82× faster** | 6.13–6.70× / **1.68–1.71×** |
| **markdownify** | CPython 3.13 | 7.87–8.10 ms / 227–239 ms | 104.80 ms | **54.03–56.54 ms** / **645–671 ms** | **1.85×–1.94× faster** | 6.67–7.19× / **2.81–2.84×** |

#### Other Suites in `v0.0.40`

| Suite | `v0.0.39` (PGO) | `v0.0.40` (non-PGO fat-LTO) | Highlights |
| --- | --- | --- | --- |
| **Python `tools/python_bench.py`** (44 programs vs. CPython 3.13) | `0.79×` geomean | **`0.62×` geomean** (~1.27× faster) | `micro 0.60×`, `macro 0.75×`, **26/36 wins** over CPython; `try_raise_catch` `2.4× → 0.29×`, `instance_new` `2.6× → 1.37×`, `sort_heapq` `2.3× → 1.27×`, `builtin_isinstance` `0.63×`, `generator_iter` `0.95×` |
| **JS `bench/regex`** (16 patterns vs. Node.js) | `2.44×` geomean | **`1.90×` geomean** (~1.28× faster) | `literal-utf16` `4.0× → 0.82×` (now beating Node), `lookbehind` **1.41× faster** |
| **JS `bench/hostile`** (stress tests vs. Node.js) | 4 slowest lagged Node | **3 of 4 flipped to beating Node** | `reactish-reconcile` `1.21× → 0.97×`, `shapes-megamorphic` `1.04× → 0.85×`, `calls-closures` `1.12× → 0.93×`, `allocation-survival` `1.35× → 1.24×` |
| **PHP & Rust runtimes** (O(N²) fixes) | O(N²) / O(N² log N) | **O(1) / O(N)** | PHP `array_pop` stack **~65×** (`614 ms → 9.5 ms`), PHP cursor loop **~180×** (`2266 ms → 12.5 ms`), Rust `VecDeque`/`BTreeMap`/`String::pop` **8.6×** (`481 ms → 56 ms`) |

### Release 0.0.35, as published

The 0.0.35 release notes measure the 0.0.35 Windows binary from the release
build's dry run (the release's engine source, the same profile-guided recipe)
against the published 0.0.34 and Node 24.19.0 on 11 November 2026: an
otherwise idle Ryzen 9 9950X3D, address-space randomization off, each run
pinned to one core, the code cache off, medians of 9 runs with every engine
interleaved (7 for the TypeScript bundle). This is a release comparison taken
with scratch scripts, not a stamped capture like the ones above. Ratios are
0.0.35 / the other engine; every engine that finishes a bundle prints the
same output as ZIPP.

| Bundled application, cache off | / 0.0.34 | / Node 24 | interpreted / 0.0.34 | interpreted / `node --jitless` |
|---|---:|---:|---:|---:|
| Babel | 1.00× | 3.83× | 0.99× | 1.79× |
| TypeScript | 1.01× | Node fails | 1.00× | Node fails |
| Prettier | 1.02× | 1.12× | 1.02× | 1.49× |
| marked, lodash, acorn, terser, three.js, immer, RxJS, date-fns | 1.01× | 1.68× | 1.00× | 1.60× |
| Vue server rendering | 1.00× | 1.00× | 0.99× | 0.96× |
| Preact server rendering | 1.00× | 0.97× | 0.91× | 0.84× |
| React server rendering | 0.98× | 0.90× | 1.00× | 0.88× |

Over 90 benchmark programs (outputs identical to 0.0.34's in both lanes)
0.0.35 takes 0.984× 0.0.34's time with the JIT and 0.595× Node 24's, ahead of
Node on 72 of the 89 programs Node runs (0.0.34 in the same runs: 71); the
compiled code's bail-out blocks, now placed after a function's code, take
`class-prototype-hot` to 0.81× and `sparse-array` to 0.85×. Interpreted, with
no native code at all (`ZIPP_NOJIT=1 ZIPP_NO_RX_JIT=1`), it takes 0.997×
0.0.34's time and 0.781× `node --jitless`'s, ahead on 71 of 89 (70). The
published binary, built afterwards from the same source by the same recipe, is
within 2% of the measured one on 35 of 40 program-and-lane comparisons; the
notes list the other five. The full tables are in
[docs/releases/0.0.35.md](releases/0.0.35.md#performance).

### Release 0.0.33, as published

The 0.0.33 release notes measure the 0.0.33 Windows binary from the release
build's dry run (the release's engine source, the same profile-guided recipe)
against the published 0.0.32, Bellard's QuickJS 2026-06-04, QuickJS-NG 0.18.0
and Node 24.19.0 on 7 November 2026: an otherwise idle Ryzen 9 9950X3D,
address-space randomization off, each run pinned to one core, the code cache
off, medians of 7 runs with every engine interleaved. This is a release
comparison taken with scratch scripts, not a stamped capture like the ones
above. Ratios are 0.0.33 / the other engine; every engine that finishes a
bundle prints the same output as ZIPP.

| Bundled application, cache off (7 runs) | / 0.0.32 | / Bellard's QuickJS | / QuickJS-NG | / Node 24 | interpreted / `node --jitless` |
|---|---:|---:|---:|---:|---:|
| Babel | 0.96× | 0.78× | 0.57× | 3.80× | 1.75× |
| TypeScript | 0.97× | 0.81× | 0.63× | Node fails | Node fails |
| Prettier | 0.98× | 0.89× | 0.67× | 1.11× | 1.52× |
| marked, lodash, acorn, terser, three.js, immer, RxJS, date-fns | 0.88× | 0.30× | 0.29× | 1.66× | 1.61× |
| Vue server rendering | 0.98× | 0.86× | 0.70× | 1.00× | 1.04× |
| Preact server rendering | 1.02× | 0.84× | 0.60× | 0.98× | 0.92× |
| React server rendering | 1.00× | QuickJS fails | QuickJS-NG fails | 0.94× | 0.93× |

QuickJS fails React server rendering for want of `TextEncoder`. A program of
256 or more functions runs interpreted even with the JIT on (every bundle here
but Preact's), so the bundles' JIT and interpreter figures are close. Over 90
benchmark programs (5 runs each, outputs identical to 0.0.32's in both lanes)
0.0.33 takes 0.664× 0.0.32's time with the JIT and 0.609× Node 24's, ahead of
Node on 72 of the 89 programs Node runs (0.0.32: 53). Interpreted, with no
native code at all (`ZIPP_NOJIT=1 ZIPP_NO_RX_JIT=1`), it takes 0.872× 0.0.32's
time, 0.948× `node --jitless`'s (ahead on 54 of 89), 0.649× Bellard's
QuickJS's (faster on 79 of 89) and 0.464× QuickJS-NG's (faster on 87 of 88).
The full tables are in [docs/releases/0.0.33.md](releases/0.0.33.md#performance).

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
how fast is Zipp's interpreter against other interpreters? The same 0.0.40 canonical
PGO binary (`0cd3eab8`) runs with `ZIPP_NOJIT=1 ZIPP_NO_RX_JIT=1`, emitting no native code at all (its JavaScript
JIT and its regex JIT both off), against
Bellard's QuickJS 2026-06-04 and QuickJS-NG 0.18.0 (neither has a JIT), and
against the three JIT engines with their JITs switched off: Node `--jitless`
(V8's Ignition), Deno `--v8-flags=--jitless`, and Bun with `BUN_JSC_useJIT=0`
(JavaScriptCore's LLInt). [`run_native.py`](../bench/comparison/run_native.py)
staged identical inputs for every engine, checked every output against Node,
and ran 8 counterbalanced repetitions. The artifacts,
[`interp_real13_0cd3eab8_2026-11-24.json`](../bench/comparison/interp_real13_0cd3eab8_2026-11-24.json) and
[`interp_hostile17_0cd3eab8_2026-11-24.json`](../bench/comparison/interp_hostile17_0cd3eab8_2026-11-24.json),
are diagnostics, not canonical captures.

| Zipp interpreter / engine | Normal 13 | Hostile 17 | All 30 | Rows Zipp wins |
|---|---:|---:|---:|---:|
| QuickJS | 0.376× [0.374, 0.389] | 0.476× [0.473, 0.484] | **0.430×** | 30 / 30 |
| QuickJS-NG | 0.319× [0.313, 0.330] | 0.339× [0.337, 0.347] | **0.330×** | 30 / 30 |
| Node jitless | 0.779× [0.776, 0.795] | 0.655× [0.649, 0.664] | **0.706×** | 28 / 30 |
| Deno jitless | 0.771× [0.764, 0.788] | 0.573× [0.554, 0.587] | **0.652×** | 29 / 30 |
| Bun no-JIT | 0.542× [0.538, 0.557] | 0.470× [0.461, 0.473] | **0.500×** | 30 / 30 |

<details>
<summary><strong>Interpreter results, row by row</strong></summary>

Cold medians include process launch; bold marks the fastest interpreter.

| Benchmark | Zipp interpreter | QuickJS | QuickJS-NG | Node jitless | Deno jitless | Bun no-JIT |
|---|---:|---:|---:|---:|---:|---:|
| async-promise-chain | **343 ms** | 5,163 ms | 3,642 ms | 518 ms | 520 ms | 656 ms |
| class-prototype-hot | 2,555 ms | 4,838 ms | 7,137 ms | 1,825 ms | **1,806 ms** | 6,265 ms |
| json-large | **504 ms** | 1,078 ms | 1,333 ms | 695 ms | 643 ms | 723 ms |
| map-set-heavy | **984 ms** | 2,328 ms | 2,856 ms | 1,470 ms | 1,997 ms | 1,608 ms |
| markdown-render | **573 ms** | 1,488 ms | 1,648 ms | 902 ms | 866 ms | 1,068 ms |
| parse-large-js | **1,077 ms** | 2,697 ms | 2,867 ms | 1,547 ms | 1,460 ms | 1,696 ms |
| polymorphic-objects | **578 ms** | 848 ms | 1,280 ms | 629 ms | 598 ms | 1,426 ms |
| regex-log-scan | **1,594 ms** | 3,236 ms | 3,830 ms | 1,891 ms | 1,637 ms | 2,952 ms |
| sparse-array | **158 ms** | 416 ms | 929 ms | 222 ms | 244 ms | 331 ms |
| typedarray-math | **2,348 ms** | 4,962 ms | 5,724 ms | 2,361 ms | 2,431 ms | 3,538 ms |
| polymorphic-objects-v2 | **223 ms** | 442 ms | 656 ms | 313 ms | 344 ms | 470 ms |
| property-ic-shapes | **1,660 ms** | 1,927 ms | 3,514 ms | 2,012 ms | 2,066 ms | 1,905 ms |
| sparse-array-v2 | **209 ms** | 1,936 ms | 724 ms | 344 ms | 332 ms | 529 ms |
| **Zipp interpreter / engine geomean** | — | **0.376×** [0.374, 0.389] | **0.319×** [0.313, 0.330] | **0.779×** [0.776, 0.795] | **0.771×** [0.764, 0.788] | **0.542×** [0.538, 0.557] |

| Hostile row | Zipp interpreter | QuickJS | QuickJS-NG | Node jitless | Deno jitless | Bun no-JIT |
|---|---:|---:|---:|---:|---:|---:|
| calls-baseline | **66 ms** | 129 ms | 202 ms | 134 ms | 143 ms | 134 ms |
| calls-closures | **164 ms** | 241 ms | 376 ms | 200 ms | 220 ms | 278 ms |
| shapes-stable | **68 ms** | 150 ms | 187 ms | 97 ms | 122 ms | 120 ms |
| shapes-megamorphic | **92 ms** | 218 ms | 253 ms | 120 ms | 154 ms | 221 ms |
| types-stable | **111 ms** | 185 ms | 304 ms | 171 ms | 160 ms | 159 ms |
| types-churn | **72 ms** | 110 ms | 165 ms | 106 ms | 127 ms | 147 ms |
| branch-control | **65 ms** | 119 ms | 181 ms | 120 ms | 132 ms | 133 ms |
| throw-catch | **73 ms** | 108 ms | 323 ms | 188 ms | 158 ms | 186 ms |
| allocation-ephemeral | **85 ms** | 280 ms | 348 ms | 132 ms | 150 ms | 304 ms |
| allocation-survival | 121 ms | 232 ms | 264 ms | **119 ms** | 149 ms | 173 ms |
| async-burst | **29 ms** | 114 ms | 110 ms | 71 ms | 98 ms | 71 ms |
| async-lived | **32 ms** | 137 ms | 104 ms | 60 ms | 85 ms | 95 ms |
| reactish-reconcile | **67 ms** | 144 ms | 174 ms | 82 ms | 106 ms | 134 ms |
| warm-router | **80 ms** | 153 ms | 472 ms | 90 ms | 112 ms | 172 ms |
| bytecode-vm | **219 ms** | 459 ms | 653 ms | 315 ms | 340 ms | 526 ms |
| module-hot-graph | **86 ms** | 117 ms | 157 ms | 129 ms | 136 ms | 175 ms |
| npm-nanoid | **413 ms** | 924 ms | 1,029 ms | 517 ms | 502 ms | 865 ms |
| **Zipp interpreter / engine geomean** | — | **0.476×** [0.473, 0.484] | **0.339×** [0.337, 0.347] | **0.655×** [0.649, 0.664] | **0.573×** [0.554, 0.587] | **0.470×** [0.461, 0.473] |

</details>

Over all 30 rows Zipp 0.0.40's pure interpreter is faster than Bellard's QuickJS (**0.430×**, 30 of 30 rows;
down from 0.574× in v0.0.31), faster than QuickJS-NG (**0.330×**, 30 of 30),
faster than JavaScriptCore's LLInt (**0.500×**, 30 of 30), faster than V8's Ignition under Deno
(**0.652×**, 29 of 30) and faster than V8's Ignition under Node (**0.706×**, 28 of 30).

### Python versus CPython

Zipp compiles Python 3.13 to the same register bytecode.
[`tools/python_bench.py`](../tools/python_bench.py) runs every program under
`tools/python_bench/` as a fresh process per engine per repetition, interleaved,
15 repetitions, against CPython 3.13.14; every run's output must equal
CPython's. Each program times its own workload in-process, and that work time
is the ratio; wall time, which adds process start and compilation, is recorded
beside it. The binary is the published **0.0.40** release build (the canonical PGO build has
no Python), run with its JIT and again with `ZIPP_NOJIT=1`, since standard
CPython is an interpreter. The 8 torch programs have no CPython counterpart
and are left out.
Artifacts: [`cpython_0cd3eab8_2026-11-24.json`](../bench/python/cpython_0cd3eab8_2026-11-24.json) and
[`cpython_nojit_0cd3eab8_2026-11-24.json`](../bench/python/cpython_nojit_0cd3eab8_2026-11-24.json).

| Zipp / CPython, geometric mean | With JIT | No JIT |
|---|---:|---:|
| 30 micro kernels, work time | **0.601×** | 1.298× |
| 6 application-shaped programs, work time | **0.767×** | 1.639× |
| all 36, work time | **0.626×** | 1.350× |
| all 36, whole-process wall time | **0.456×** | **0.537×** |

With its JIT, Zipp 0.0.40's Python is faster than CPython on **26 of 36** programs
(up from 15 of 36 and 1.004× in v0.0.31), most on fib_recursive (0.06×), property_read (0.21×), range_loop (0.22×), float_arith (0.23×), tuple_swap (0.26×).
CPython is faster on the rest, most on oo_simulation (1.95×), str_methods (1.88×), word_count (1.79×), fstring (1.64×), super_call (1.41×).

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
