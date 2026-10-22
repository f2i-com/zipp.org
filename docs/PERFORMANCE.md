# Performance

## Performance, measured honestly

Start with the recorded native results. These are stamped benchmark captures,
with the source revisions and raw evidence below; they are not a new measurement
of every change on `main`. Ratios are **Zipp / competitor**, so **lower is faster**.
All of them were measured on 21 October 2026 with the 0.0.28 engine.

| Native workload group | Zipp / Node geomean | Coverage |
|---|---:|---|
| All 30 workloads | **0.652×** | 13 normal + 17 hostile rows, weighted equally. |
| Normal workloads | **0.545×** | All 13 normal rows; faster than Node on every one. |
| Hostile workloads | **0.748×** | All 17 stress-oriented rows. |

Those are JIT against JIT: every engine runs as it ships. Run as a pure
interpreter (`ZIPP_NOJIT=1`), Zipp measures **0.685×** Bellard's QuickJS and
**0.387×** QuickJS-NG over the same 30 rows; see
[Interpreter versus interpreter](#interpreter-versus-interpreter). Its Python
frontend measures 1.067× CPython 3.13.14 by in-process work time over 36
programs (0.975× on the 30 micro kernels); see [Python versus CPython](#python-versus-cpython).

Median process launch in the canonical four-engine capture is **7.5 ms** for
Zipp, **30.7 ms** for Node, **50.9 ms** for Bun and **79.3 ms** for Deno.

> The goal is ambitious and literal: become faster than Node, Bun, and Deno on
> every maintained benchmark while preserving exact output and tier parity.
> Zipp is not there yet. The tables below show both the wins and the remaining
> gaps.

### Canonical public capture

<details>
<summary><strong>Full Node, Bun and Deno results, confidence intervals and methodology</strong></summary>

The canonical table is the clean PGO capture at engine commit `84012a2d`
(2026-10-21), the 0.0.28 engine: `84012a2d` changes nothing under `crates/`,
`tools/` or `bench/` from the release tag `da0fe6b1`.
[`real13_84012a2d_pgo_2026-10-21.json`](../bench/real13_84012a2d_pgo_2026-10-21.json)
and [`head_clean_84012a2d_pgo_2026-10-21.json`](../bench/hostile/head_clean_84012a2d_pgo_2026-10-21.json).
Both artifacts record `publishable:true`, `ALL_CORRECT=1`, 15 complete
counterbalanced repetitions, 10,000 bootstrap samples, exact output, and no
source, engine, input, environment, process-health, or harness drift. Every
engine runs with its production JIT.

Node v24.19.0 · Bun 1.3.14 · Deno 2.6.10 · Zipp 0.0.28 canonical PGO SHA-256
`dedff1a3…32aa83`.

Cold medians include process launch; bold marks the lowest displayed median.

| Retained benchmark | Node | Bun | Deno | Zipp | Zipp / Node |
|---|---:|---:|---:|---:|---:|
| async-promise-chain | 338 ms | 376 ms | 353 ms | **258 ms** | **0.77×** |
| class-prototype-hot | 301 ms | 339 ms | 322 ms | **234 ms** | **0.78×** |
| json-large | 262 ms | **200 ms** | 316 ms | 216 ms | **0.84×** |
| map-set-heavy | 648 ms | 757 ms | 1,139 ms | **462 ms** | **0.71×** |
| markdown-render | 268 ms | 215 ms | 302 ms | **183 ms** | **0.68×** |
| parse-large-js | 270 ms | 236 ms | 282 ms | **233 ms** | **0.87×** |
| polymorphic-objects | 328 ms | 337 ms | 335 ms | **211 ms** | **0.64×** |
| regex-log-scan | 479 ms | 562 ms | 457 ms | **390 ms** | **0.83×** |
| sparse-array | 80 ms | 107 ms | 124 ms | **71 ms** | **0.90×** |
| typedarray-math | 200 ms | 916 ms | 164 ms | **139 ms** | **0.69×** |
| **Zipp / engine paired geomean** | **0.766×** [0.758, 0.771] | **0.641×** [0.635, 0.645] | **0.682×** [0.672, 0.687] | — | — |

The three architecture diagnostics remain outside the retained-ten headline:

| Diagnostic | Node | Bun | Deno | Zipp | Zipp / Node |
|---|---:|---:|---:|---:|---:|
| polymorphic-objects-v2 | 81 ms | 95 ms | 127 ms | **22 ms** | **0.27×** |
| property-ic-shapes | 258 ms | 165 ms | 305 ms | **9 ms** | **0.04×** |
| sparse-array-v2 | 171 ms | 370 ms | 181 ms | **94 ms** | **0.55×** |
| **Zipp / engine paired geomean** | **0.176×** [0.172, 0.180] | **0.150×** [0.147, 0.153] | **0.140×** [0.138, 0.143] | — | — |

Across all 13 normal rows, Zipp measures **0.545× Node** [0.540, 0.549],
**0.458× Bun** [0.455, 0.461], and **0.473× Deno** [0.467, 0.477]. It is
faster than Node on all 13, and wins 38 of 39 point comparisons and 37 of 39
Bonferroni exact-sign comparisons; Bun is faster on json-large (1.10×).

The separately measured 17-case hostile corpus covers closures, mixed locals,
shape churn, GC survival, async lifetimes, modules, a React-shaped kernel, a
warm router, a JavaScript bytecode VM, and vendored NanoID:

| Hostile metric | vs Node | vs Bun | vs Deno |
|---|---:|---:|---:|
| ordinary equal-row geomean | **0.748×** [0.740, 0.757] | **0.527×** [0.522, 0.536] | **0.407×** [0.403, 0.412] |
| category-balanced geomean | **0.784×** [0.775, 0.793] | **0.542×** [0.537, 0.552] | **0.421×** [0.418, 0.427] |

For the requested project-wide view, the explicit equal-row point across
all 30 normal and hostile rows is **0.652× Node**, **0.496× Bun** and
**0.434× Deno**, calculated as `exp((13 × ln(G13) + 17 × ln(G17)) / 30)`
from the two suite geomeans above. No interval was computed for it this time;
each suite's own interval is given with it.

The aggregate is ahead, but the literal every-row target is not met. Zipp has
23 of 30 Node point wins: all 13 normal rows and 10 of the 17 hostile ones.
The Node point gaps are all in the hostile set: allocation survival (1.38×), react-shaped reconcile (1.26×), warm router (1.25×), closure calls (1.12×), megamorphic shapes (1.10×), bytecode vm (1.05×), stable shapes (1.04×).

```text
FASTER_THAN_NODE_ON_EVERY_ROW=0
FASTER_THAN_EVERY_ENGINE_ON_EVERY_ROW=0
```

Against the previous canonical capture at `f1739644` (2026-10-18, `zipp 0.0.26`;
headline ten 0.769× Node, all 13 0.542×, hostile 0.715×, all 30 0.634×),
the headline ten moved to 0.766×, all 13 to 0.545×, the hostile suite to 0.748× and all 30 to 0.652×.
Zipp's own medians are 1.2% lower than in that capture, but Node's, the same
v24.19.0 binary, are 4.4% lower (7.5% on the short hostile rows), so
most of the ratios' movement is in the Node runs, not in Zipp.
That capture's artifacts stay in [`bench/`](../bench/README.md) as a prior series
point. See the [`bench` guide](../bench/README.md),
[hostile suite](../bench/hostile/README.md), and
[`docs/PERF_ROADMAP.md`](PERF_ROADMAP.md) for exact methodology and remaining work.

</details>

### The release build as downloaded

The published binaries are ordinary release builds, not profile-guided ones.
The 0.0.28 Windows CLI as downloaded ([`release_real13_da0fe6b1_2026-10-21.json`](../bench/comparison/release_real13_da0fe6b1_2026-10-21.json),
the same four engines, 15 repetitions) runs the 13 normal rows at **0.608× Node**
[0.605, 0.617] and the headline ten at 0.856× [0.850, 0.869], faster than Node on
every row. A paired A/B of the same rows
([`pgo_ab_real13_da0fe6b1_84012a2d_2026-10-21.json`](../bench/comparison/pgo_ab_real13_da0fe6b1_84012a2d_2026-10-21.json)) puts the PGO
build at 0.891× the release build's time [0.881, 0.901]: the download
takes about 1.12× the canonical figures' time. The two builds differ in more
than the profile: the PGO build has JavaScript only (no GPU or Python
features, 9.1 MB against 17.8 MB), an explicit `x86-64` CPU target and the lld
linker. The ratio compares the release product with the canonical benchmark
product; it is not the effect of profile guidance alone, which would take a
feature- and flag-matched pair to measure.

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
[`interp_real13_84012a2d_2026-10-21.json`](../bench/comparison/interp_real13_84012a2d_2026-10-21.json) and
[`interp_hostile17_84012a2d_2026-10-21.json`](../bench/comparison/interp_hostile17_84012a2d_2026-10-21.json),
are diagnostics, not canonical captures. Zipp's interpreter runs as it ships,
including its exact recognizers for a few loop shapes.

| Zipp interpreter / engine | Normal 13 | Hostile 17 | All 30 | Rows Zipp wins |
|---|---:|---:|---:|---:|
| QuickJS | 0.614× [0.611, 0.617] | 0.745× [0.740, 0.758] | **0.685×** | 24 / 30 |
| QuickJS-NG | 0.385× [0.382, 0.388] | 0.389× [0.386, 0.396] | **0.387×** | 30 / 30 |
| Node jitless | 1.207× [1.175, 1.213] | 1.009× [0.988, 1.028] | **1.090×** | 10 / 30 |
| Deno jitless | 1.164× [1.154, 1.173] | 0.881× [0.874, 0.899] | **0.994×** | 15 / 30 |
| Bun no-JIT | 0.834× [0.829, 0.838] | 0.790× [0.785, 0.801] | **0.809×** | 25 / 30 |

<details>
<summary><strong>Interpreter results, row by row</strong></summary>

Cold medians include process launch; bold marks the fastest interpreter.

| Benchmark | Zipp interpreter | QuickJS | QuickJS-NG | Node jitless | Deno jitless | Bun no-JIT |
|---|---:|---:|---:|---:|---:|---:|
| async-promise-chain | **397 ms** | 3,888 ms | 3,822 ms | 444 ms | 443 ms | 555 ms |
| class-prototype-hot | 3,625 ms | 3,776 ms | 7,128 ms | **1,708 ms** | 1,710 ms | 4,942 ms |
| json-large | **512 ms** | 771 ms | 1,409 ms | 523 ms | 533 ms | 622 ms |
| map-set-heavy | **665 ms** | 1,406 ms | 2,148 ms | 777 ms | 1,167 ms | 1,032 ms |
| markdown-render | **651 ms** | 1,069 ms | 2,060 ms | 764 ms | 791 ms | 965 ms |
| parse-large-js | 1,694 ms | 2,001 ms | 3,630 ms | 1,360 ms | **1,356 ms** | 1,587 ms |
| polymorphic-objects | 807 ms | 860 ms | 1,411 ms | 590 ms | **557 ms** | 1,294 ms |
| regex-log-scan | 1,631 ms | 2,523 ms | 3,958 ms | 1,539 ms | **1,522 ms** | 2,737 ms |
| sparse-array | 285 ms | 381 ms | 790 ms | **205 ms** | 218 ms | 272 ms |
| typedarray-math | 4,143 ms | 3,841 ms | 6,410 ms | **2,213 ms** | 2,272 ms | 3,186 ms |
| polymorphic-objects-v2 | 390 ms | 395 ms | 844 ms | **295 ms** | 312 ms | 415 ms |
| property-ic-shapes | 2,666 ms | 2,021 ms | 5,271 ms | 1,907 ms | 1,946 ms | **1,799 ms** |
| sparse-array-v2 | 318 ms | 1,842 ms | 866 ms | 324 ms | **300 ms** | 475 ms |
| **Zipp interpreter / engine geomean** | — | **0.614×** [0.611, 0.617] | **0.385×** [0.382, 0.388] | 1.207× [1.175, 1.213] | 1.164× [1.154, 1.173] | **0.834×** [0.829, 0.838] |

| Hostile row | Zipp interpreter | QuickJS | QuickJS-NG | Node jitless | Deno jitless | Bun no-JIT |
|---|---:|---:|---:|---:|---:|---:|
| calls-baseline | **100 ms** | 111 ms | 251 ms | 104 ms | 116 ms | 107 ms |
| calls-closures | 231 ms | 227 ms | 452 ms | **177 ms** | 189 ms | 248 ms |
| shapes-stable | 88 ms | 131 ms | 232 ms | **86 ms** | 101 ms | 89 ms |
| shapes-megamorphic | 157 ms | 192 ms | 325 ms | **114 ms** | 126 ms | 187 ms |
| types-stable | 156 ms | 163 ms | 376 ms | **127 ms** | 144 ms | 129 ms |
| types-churn | 104 ms | **96 ms** | 200 ms | 101 ms | 111 ms | 112 ms |
| branch-control | 102 ms | 109 ms | 239 ms | **100 ms** | 113 ms | 102 ms |
| throw-catch | 99 ms | **97 ms** | 291 ms | 163 ms | 140 ms | 149 ms |
| allocation-ephemeral | **113 ms** | 251 ms | 364 ms | 121 ms | 160 ms | 257 ms |
| allocation-survival | 113 ms | 173 ms | 272 ms | **104 ms** | 131 ms | 133 ms |
| async-burst | **33 ms** | 103 ms | 123 ms | 62 ms | 76 ms | 43 ms |
| async-lived | **33 ms** | 107 ms | 126 ms | 49 ms | 69 ms | 64 ms |
| reactish-reconcile | 81 ms | 120 ms | 177 ms | **72 ms** | 90 ms | 106 ms |
| warm-router | 118 ms | 123 ms | 477 ms | **78 ms** | 99 ms | 140 ms |
| bytecode-vm | 331 ms | 404 ms | 941 ms | **298 ms** | 308 ms | 480 ms |
| module-hot-graph | 115 ms | **98 ms** | 200 ms | 102 ms | 117 ms | 145 ms |
| npm-nanoid | 510 ms | 697 ms | 1,158 ms | **460 ms** | 462 ms | 800 ms |
| **Zipp interpreter / engine geomean** | — | **0.745×** [0.740, 0.758] | **0.389×** [0.386, 0.396] | 1.009× [0.988, 1.028] | **0.881×** [0.874, 0.899] | **0.790×** [0.785, 0.801] |

</details>

Over all 30 rows Zipp's interpreter is faster than Bellard's QuickJS (0.685×, 24 of 30 rows;
0.720× in the previous capture), faster than QuickJS-NG (0.387×, 30 of 30),
faster than JavaScriptCore's LLInt (0.809×), faster than V8's Ignition under Deno
(0.994×; 1.060× before) and slower than it under Node (1.090×; 1.147× before).
Bellard's QuickJS wins property-ic-shapes (1.32×), module-hot-graph (1.18×), types-churn (1.09×), typedarray-math (1.08×), calls-closures (1.02×), throw-catch (1.02×).
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
beside it. The binary is the published 0.0.28 release build (the PGO build has
no Python), run with its JIT and again with `ZIPP_NOJIT=1`, since standard
CPython is an interpreter. The 8 torch programs have no CPython counterpart
and are left out.
Artifacts: [`cpython_da0fe6b1_2026-10-21.json`](../bench/python/cpython_da0fe6b1_2026-10-21.json) and
[`cpython_nojit_da0fe6b1_2026-10-21.json`](../bench/python/cpython_nojit_da0fe6b1_2026-10-21.json) (that run sets
`ZIPP_NOJIT=1` for the whole harness; CPython ignores it).

| Zipp / CPython, geometric mean | With JIT | No JIT |
|---|---:|---:|
| 30 micro kernels, work time | **0.975×** | 1.400× |
| 6 application-shaped programs, work time | 1.674× | 1.680× |
| all 36, work time | 1.067× | 1.443× |
| all 36, whole-process wall time | **0.539×** | **0.576×** |

With its JIT, Zipp's Python is faster than CPython on 15 of 36 programs, most
on range_loop (0.20×), float_arith (0.25×), tuple_swap (0.26×), attr_read_write (0.42×).
CPython is faster on the rest, most on str_methods (3.18×), generator_iter (3.04×), try_raise_catch (2.84×), instance_new (2.56×), word_count (2.25×):
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
