# Performance

## Performance, measured honestly

Start with the recorded native results. These are stamped benchmark captures,
with the source revisions and raw evidence below; they are not a new measurement
of every change on `main`. Ratios are **Zipp / competitor**, so **lower is faster**.
All of them were measured on 18 October 2026 with the 0.0.26 engine.

| Native workload group | Zipp / Node geomean | Coverage |
|---|---:|---|
| All 30 workloads | **0.634×** | 13 normal + 17 hostile rows, weighted equally. |
| Normal workloads | **0.542×** | All 13 normal rows; faster than Node on every one. |
| Hostile workloads | **0.715×** | All 17 stress-oriented rows. |

Those are JIT against JIT: every engine runs as it ships. Run as a pure
interpreter (`ZIPP_NOJIT=1`), Zipp measures **0.720×** Bellard's QuickJS and
**0.410×** QuickJS-NG over the same 30 rows; see
[Interpreter versus interpreter](#interpreter-versus-interpreter). Its Python
frontend measures 1.057× CPython 3.13.14 by in-process work time over 36
programs (0.966× on the 30 micro kernels); see [Python versus CPython](#python-versus-cpython).

Median process launch in the canonical four-engine capture is **7.1 ms** for
Zipp, **31.7 ms** for Node, **49.4 ms** for Bun and **79.2 ms** for Deno.

> The goal is ambitious and literal: become faster than Node, Bun, and Deno on
> every maintained benchmark while preserving exact output and tier parity.
> Zipp is not there yet. The tables below show both the wins and the remaining
> gaps.

### Canonical public capture

<details>
<summary><strong>Full Node, Bun and Deno results, confidence intervals and methodology</strong></summary>

The canonical table is the clean PGO capture at engine commit `f1739644`
(2026-10-18), the 0.0.26 engine: `f1739644` changes nothing under `crates/`,
`tools/` or `bench/` from the release tag `5f1c0578`.
[`real13_f1739644_pgo_2026-10-18.json`](../bench/real13_f1739644_pgo_2026-10-18.json)
and [`head_clean_f1739644_pgo_2026-10-18.json`](../bench/hostile/head_clean_f1739644_pgo_2026-10-18.json).
Both artifacts record `publishable:true`, `ALL_CORRECT=1`, 15 complete
counterbalanced repetitions, 10,000 bootstrap samples, exact output, and no
source, engine, input, environment, process-health, or harness drift. Every
engine runs with its production JIT.

Node v24.19.0 · Bun 1.3.14 · Deno 2.6.10 · Zipp 0.0.26 canonical PGO SHA-256
`b9947268…c2c763`.

Cold medians include process launch; bold marks the lowest displayed median.

| Retained benchmark | Node | Bun | Deno | Zipp | Zipp / Node |
|---|---:|---:|---:|---:|---:|
| async-promise-chain | 330 ms | 371 ms | 352 ms | **259 ms** | **0.78×** |
| class-prototype-hot | 301 ms | 339 ms | 320 ms | **233 ms** | **0.77×** |
| json-large | 265 ms | **196 ms** | 310 ms | 209 ms | **0.79×** |
| map-set-heavy | 620 ms | 756 ms | 1,133 ms | **450 ms** | **0.72×** |
| markdown-render | 271 ms | 214 ms | 310 ms | **195 ms** | **0.73×** |
| parse-large-js | 273 ms | **231 ms** | 286 ms | 237 ms | **0.87×** |
| polymorphic-objects | 328 ms | 335 ms | 331 ms | **212 ms** | **0.64×** |
| regex-log-scan | 462 ms | 559 ms | 460 ms | **395 ms** | **0.85×** |
| sparse-array | 82 ms | 103 ms | 123 ms | **72 ms** | **0.89×** |
| typedarray-math | 202 ms | 912 ms | 165 ms | **139 ms** | **0.68×** |
| **Zipp / engine paired geomean** | **0.769×** [0.762, 0.775] | **0.649×** [0.644, 0.654] | **0.681×** [0.666, 0.692] | — | — |

The three architecture diagnostics remain outside the retained-ten headline:

| Diagnostic | Node | Bun | Deno | Zipp | Zipp / Node |
|---|---:|---:|---:|---:|---:|
| polymorphic-objects-v2 | 84 ms | 92 ms | 126 ms | **21 ms** | **0.25×** |
| property-ic-shapes | 259 ms | 161 ms | 308 ms | **9 ms** | **0.04×** |
| sparse-array-v2 | 173 ms | 371 ms | 180 ms | **94 ms** | **0.54×** |
| **Zipp / engine paired geomean** | **0.169×** [0.165, 0.175] | **0.148×** [0.147, 0.155] | **0.138×** [0.135, 0.145] | — | — |

Across all 13 normal rows, Zipp measures **0.542× Node** [0.537, 0.547],
**0.462× Bun** [0.459, 0.466], and **0.471× Deno** [0.462, 0.481]. It is
faster than Node on all 13, and wins 37 of 39 point comparisons and 37 of 39
Bonferroni exact-sign comparisons; Bun is faster on json-large (1.07×) and parse-large-js (1.03×).

The separately measured 17-case hostile corpus covers closures, mixed locals,
shape churn, GC survival, async lifetimes, modules, a React-shaped kernel, a
warm router, a JavaScript bytecode VM, and vendored NanoID:

| Hostile metric | vs Node | vs Bun | vs Deno |
|---|---:|---:|---:|
| ordinary equal-row geomean | **0.715×** [0.705, 0.722] | **0.537×** [0.533, 0.543] | **0.414×** [0.411, 0.419] |
| category-balanced geomean | **0.748×** [0.738, 0.754] | **0.553×** [0.548, 0.560] | **0.428×** [0.426, 0.434] |

For the requested project-wide view, the explicit equal-row point across
all 30 normal and hostile rows is **0.634× Node**, **0.503× Bun** and
**0.438× Deno**, calculated as `exp((13 × ln(G13) + 17 × ln(G17)) / 30)`
from the two suite geomeans above. No interval was computed for it this time;
each suite's own interval is given with it.

The aggregate is ahead, but the literal every-row target is not met. Zipp has
24 of 30 Node point wins: all 13 normal rows and 11 of the 17 hostile ones.
The Node point gaps are all in the hostile set: allocation survival (1.36×), warm router (1.31×), react-shaped reconcile (1.16×), closure calls (1.07×), megamorphic shapes (1.03×), stable shapes (1.01×).
The JavaScript bytecode VM, 3.13× Node in the previous capture, is now
0.97×.

```text
FASTER_THAN_NODE_ON_EVERY_ROW=0
FASTER_THAN_EVERY_ENGINE_ON_EVERY_ROW=0
```

Against the previous canonical capture at `fa8e3b86` (2026-10-15, `zipp 0.0.24`;
headline ten 0.795× Node, all 13 0.566×, hostile 0.829×, all 30 0.703×),
the headline ten moved to 0.769×, all 13 to 0.542× and the hostile suite to 0.715×.
That capture's artifacts stay in [`bench/`](../bench/README.md) as a prior series
point. See the [`bench` guide](../bench/README.md),
[hostile suite](../bench/hostile/README.md), and
[`docs/PERF_ROADMAP.md`](PERF_ROADMAP.md) for exact methodology and remaining work.

</details>

### The release build as downloaded

The published binaries are ordinary release builds, not profile-guided ones.
The 0.0.26 Windows CLI as downloaded ([`release_real13_5f1c0578_2026-10-18.json`](../bench/comparison/release_real13_5f1c0578_2026-10-18.json),
the same four engines, 15 repetitions) runs the 13 normal rows at **0.604× Node**
[0.599, 0.608] and the headline ten at 0.856× [0.848, 0.861], faster than Node on
every row. A paired A/B of the same rows
([`pgo_ab_real13_5f1c0578_f1739644_2026-10-18.json`](../bench/comparison/pgo_ab_real13_5f1c0578_f1739644_2026-10-18.json)) puts the PGO
build at 0.901× the release build's time [0.897, 0.905]: the download
takes about 1.11× the canonical figures' time.

### Interpreter versus interpreter

The tables above compare production JITs. This one asks the other question:
how fast is Zipp's interpreter against other interpreters? The same canonical
PGO binary runs with `ZIPP_NOJIT=1`, emitting no native code at all, against
Bellard's QuickJS 2026-06-04 and QuickJS-NG 0.17.0 (neither has a JIT), and
against the three JIT engines with their JITs switched off: Node `--jitless`
(V8's Ignition), Deno `--v8-flags=--jitless`, and Bun with `BUN_JSC_useJIT=0`
(JavaScriptCore's LLInt). [`run_native.py`](../bench/comparison/run_native.py)
staged identical inputs for every engine, checked every output against Node,
and ran 8 counterbalanced repetitions. The artifacts,
[`interp_real13_f1739644_2026-10-18.json`](../bench/comparison/interp_real13_f1739644_2026-10-18.json) and
[`interp_hostile17_f1739644_2026-10-18.json`](../bench/comparison/interp_hostile17_f1739644_2026-10-18.json),
are diagnostics, not canonical captures. Zipp's interpreter runs as it ships,
including its exact recognizers for a few loop shapes.

| Zipp interpreter / engine | Normal 13 | Hostile 17 | All 30 | Rows Zipp wins |
|---|---:|---:|---:|---:|
| QuickJS | 0.648× [0.645, 0.653] | 0.780× [0.774, 0.785] | **0.720×** | 21 / 30 |
| QuickJS-NG | 0.409× [0.408, 0.412] | 0.411× [0.410, 0.417] | **0.410×** | 30 / 30 |
| Node jitless | 1.280× [1.273, 1.292] | 1.055× [1.051, 1.066] | **1.147×** | 7 / 30 |
| Deno jitless | 1.233× [1.227, 1.242] | 0.944× [0.918, 0.961] | **1.060×** | 14 / 30 |
| Bun no-JIT | 0.887× [0.883, 0.893] | 0.833× [0.826, 0.839] | **0.856×** | 20 / 30 |

<details>
<summary><strong>Interpreter results, row by row</strong></summary>

Cold medians include process launch; bold marks the fastest interpreter.

| Benchmark | Zipp interpreter | QuickJS | QuickJS-NG | Node jitless | Deno jitless | Bun no-JIT |
|---|---:|---:|---:|---:|---:|---:|
| async-promise-chain | **409 ms** | 3,930 ms | 3,842 ms | 446 ms | 445 ms | 553 ms |
| class-prototype-hot | 3,769 ms | 3,793 ms | 7,254 ms | 1,726 ms | **1,711 ms** | 4,964 ms |
| json-large | 548 ms | 782 ms | 1,403 ms | **541 ms** | 551 ms | 631 ms |
| map-set-heavy | **676 ms** | 1,421 ms | 2,098 ms | 728 ms | 1,164 ms | 999 ms |
| markdown-render | **689 ms** | 1,065 ms | 2,062 ms | 773 ms | 792 ms | 968 ms |
| parse-large-js | 1,761 ms | 2,009 ms | 3,635 ms | 1,368 ms | **1,356 ms** | 1,581 ms |
| polymorphic-objects | 845 ms | 885 ms | 1,404 ms | 584 ms | **548 ms** | 1,301 ms |
| regex-log-scan | 1,718 ms | 2,540 ms | 3,970 ms | 1,557 ms | **1,519 ms** | 2,734 ms |
| sparse-array | 286 ms | 381 ms | 790 ms | **204 ms** | 218 ms | 268 ms |
| typedarray-math | 4,758 ms | 3,817 ms | 6,490 ms | **2,224 ms** | 2,271 ms | 3,258 ms |
| polymorphic-objects-v2 | 436 ms | 400 ms | 841 ms | **300 ms** | 310 ms | 414 ms |
| property-ic-shapes | 3,032 ms | 2,020 ms | 5,215 ms | 1,917 ms | 1,951 ms | **1,804 ms** |
| sparse-array-v2 | 334 ms | 1,825 ms | 872 ms | 322 ms | **304 ms** | 466 ms |
| **Zipp interpreter / engine geomean** | — | **0.648×** [0.645, 0.653] | **0.409×** [0.408, 0.412] | 1.280× [1.273, 1.292] | 1.233× [1.227, 1.242] | **0.887×** [0.883, 0.893] |

| Hostile row | Zipp interpreter | QuickJS | QuickJS-NG | Node jitless | Deno jitless | Bun no-JIT |
|---|---:|---:|---:|---:|---:|---:|
| calls-baseline | 110 ms | 110 ms | 250 ms | **104 ms** | 116 ms | 107 ms |
| calls-closures | 283 ms | 227 ms | 450 ms | **176 ms** | 196 ms | 246 ms |
| shapes-stable | 94 ms | 134 ms | 234 ms | **88 ms** | 101 ms | 89 ms |
| shapes-megamorphic | 166 ms | 192 ms | 324 ms | **115 ms** | 126 ms | 186 ms |
| types-stable | 164 ms | 163 ms | 373 ms | **129 ms** | 142 ms | 129 ms |
| types-churn | 104 ms | **99 ms** | 200 ms | 100 ms | 112 ms | 112 ms |
| branch-control | 103 ms | 108 ms | 236 ms | **101 ms** | 112 ms | 102 ms |
| throw-catch | 101 ms | **97 ms** | 292 ms | 165 ms | 137 ms | 151 ms |
| allocation-ephemeral | **117 ms** | 263 ms | 365 ms | 124 ms | 133 ms | 260 ms |
| allocation-survival | 112 ms | 178 ms | 264 ms | **105 ms** | 131 ms | 136 ms |
| async-burst | **33 ms** | 102 ms | 125 ms | 62 ms | 75 ms | 42 ms |
| async-lived | **32 ms** | 107 ms | 128 ms | 50 ms | 70 ms | 65 ms |
| reactish-reconcile | 87 ms | 124 ms | 176 ms | **74 ms** | 89 ms | 104 ms |
| warm-router | 130 ms | 123 ms | 474 ms | **78 ms** | 93 ms | 140 ms |
| bytecode-vm | 349 ms | 409 ms | 942 ms | **302 ms** | 307 ms | 478 ms |
| module-hot-graph | 141 ms | **95 ms** | 194 ms | 105 ms | 119 ms | 145 ms |
| npm-nanoid | 518 ms | 696 ms | 1,162 ms | **458 ms** | 465 ms | 799 ms |
| **Zipp interpreter / engine geomean** | — | **0.780×** [0.774, 0.785] | **0.411×** [0.410, 0.417] | 1.055× [1.051, 1.066] | **0.944×** [0.918, 0.961] | **0.833×** [0.826, 0.839] |

</details>

Zipp's interpreter is faster than both QuickJS builds in aggregate (21 of 30 rows
against Bellard's, all 30 against QuickJS-NG) and faster than JavaScriptCore's LLInt (0.856×),
but V8's Ignition is still faster (1.147× under Node, 1.060× under Deno).
Bellard's QuickJS wins property-ic-shapes (1.50×), module-hot-graph (1.48×), calls-closures (1.25×), typedarray-math (1.25×), polymorphic-objects-v2 (1.09×), types-churn (1.06×), warm-router (1.05×), throw-catch (1.04×), types-stable (1.01×).
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
beside it. The binary is the published 0.0.26 release build (the PGO build has
no Python), run with its JIT and again with `ZIPP_NOJIT=1`, since standard
CPython is an interpreter. The 8 torch programs have no CPython counterpart
and are left out.
Artifacts: [`cpython_5f1c0578_2026-10-18.json`](../bench/python/cpython_5f1c0578_2026-10-18.json) and
[`cpython_nojit_5f1c0578_2026-10-18.json`](../bench/python/cpython_nojit_5f1c0578_2026-10-18.json) (that run sets
`ZIPP_NOJIT=1` for the whole harness; CPython ignores it).

| Zipp / CPython, geometric mean | With JIT | No JIT |
|---|---:|---:|
| 30 micro kernels, work time | **0.966×** | 1.463× |
| 6 application-shaped programs, work time | 1.664× | 1.738× |
| all 36, work time | 1.057× | 1.506× |
| all 36, whole-process wall time | **0.537×** | **0.587×** |

With its JIT, Zipp's Python is faster than CPython on 15 of 36 programs, most
on range_loop (0.19×), float_arith (0.24×), tuple_swap (0.27×), attr_read_write (0.42×).
CPython is faster on the rest, most on str_methods (3.18×), try_raise_catch (2.70×), generator_iter (2.66×), instance_new (2.50×), word_count (2.27×):
string methods, exceptions, generators and object-heavy application code are
where the Python frontend's remaining work is. No intervals are computed for
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
