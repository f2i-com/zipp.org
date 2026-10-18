# Performance

## Performance, measured honestly

Start with the recorded native results. These are stamped benchmark captures,
with the source revisions and raw evidence below; they are not a new measurement
of every change on `main`. Ratios are **Zipp / competitor**, so **lower is faster**.

| Native workload group | Zipp / Node geomean | Coverage |
|---|---:|---|
| All 30 workloads | **0.703×** | 13 normal + 17 hostile rows, weighted equally. |
| Normal workloads | **0.566×** | All 13 normal rows; faster than Node on every one. |
| Hostile workloads | **0.829×** | All 17 stress-oriented rows. |

Those are JIT against JIT: every engine runs as it ships. Run as a pure
interpreter (`ZIPP_NOJIT=1`), Zipp measures **0.713×** Bellard's QuickJS and
**0.442×** QuickJS-NG over the same 13 normal rows; see
[Interpreter versus interpreter](#interpreter-versus-interpreter).

Median process launch in the canonical four-engine capture is **8.3 ms** for
Zipp, **33.0 ms** for Node, **50.8 ms** for Bun and **79.0 ms** for Deno.

> The goal is ambitious and literal: become faster than Node, Bun, and Deno on
> every maintained benchmark while preserving exact output and tier parity.
> Zipp is not there yet. The tables below show both the wins and the remaining
> gaps.

### Canonical public capture

<details>
<summary><strong>Full Node, Bun and Deno results, confidence intervals and methodology</strong></summary>

The canonical table is the clean PGO capture at engine commit `fa8e3b86`
(2026-10-15):
[`real13_fa8e3b86_pgo_2026-10-15.json`](../bench/real13_fa8e3b86_pgo_2026-10-15.json)
and [`head_clean_fa8e3b86_pgo_2026-10-15.json`](../bench/hostile/head_clean_fa8e3b86_pgo_2026-10-15.json).
Both artifacts record `publishable:true`, `ALL_CORRECT=1`, 15 complete
counterbalanced repetitions, 10,000 bootstrap samples, exact output, and no
source, engine, input, environment, process-health, or harness drift. Every
engine runs with its production JIT.

Node v24.19.0 · Bun 1.3.14 · Deno 2.6.10 · Zipp 0.0.24 canonical PGO SHA-256
`318619b2…5699f3`.

Cold medians include process launch; bold marks the lowest displayed median.

| Retained benchmark | Node | Bun | Deno | Zipp | Zipp / Node |
|---|---:|---:|---:|---:|---:|
| async-promise-chain | 337 ms | 375 ms | 359 ms | **266 ms** | **0.80×** |
| class-prototype-hot | 299 ms | 340 ms | 324 ms | **235 ms** | **0.79×** |
| json-large | 256 ms | **201 ms** | 311 ms | 216 ms | **0.85×** |
| map-set-heavy | 626 ms | 783 ms | 1,099 ms | **534 ms** | **0.84×** |
| markdown-render | 270 ms | 219 ms | 317 ms | **188 ms** | **0.69×** |
| parse-large-js | 273 ms | **236 ms** | 280 ms | 239 ms | **0.88×** |
| polymorphic-objects | 330 ms | 339 ms | 331 ms | **233 ms** | **0.71×** |
| regex-log-scan | 468 ms | 566 ms | 462 ms | **406 ms** | **0.86×** |
| sparse-array | 81 ms | 106 ms | 124 ms | **72 ms** | **0.89×** |
| typedarray-math | 204 ms | 918 ms | 167 ms | **140 ms** | **0.68×** |
| **Zipp / engine paired geomean** | **0.795×** [0.790, 0.801] | **0.663×** [0.655, 0.668] | **0.701×** [0.695, 0.706] | — | — |

The three architecture diagnostics remain outside the retained-ten headline:

| Diagnostic | Node | Bun | Deno | Zipp | Zipp / Node |
|---|---:|---:|---:|---:|---:|
| polymorphic-objects-v2 | 83 ms | 95 ms | 126 ms | **22 ms** | **0.27×** |
| property-ic-shapes | 258 ms | 165 ms | 309 ms | **10 ms** | **0.04×** |
| sparse-array-v2 | 173 ms | 376 ms | 182 ms | **100 ms** | **0.57×** |
| **Zipp / engine paired geomean** | **0.182×** [0.180, 0.184] | **0.158×** [0.155, 0.160] | **0.147×** [0.146, 0.150] | — | — |

Across all 13 normal rows, Zipp measures **0.566× Node** [0.563, 0.570],
**0.476× Bun** [0.471, 0.479], and **0.489× Deno** [0.486, 0.492]. It is
faster than Node on all 13, and wins 37 of 39 point comparisons and 36 of 39
Bonferroni exact-sign comparisons; Bun is faster on json-large (1.08×) and
parse-large-js (1.01×).

The separately measured 17-case hostile corpus covers closures, mixed locals,
shape churn, GC survival, async lifetimes, modules, a React-shaped kernel, a
warm router, a JavaScript bytecode VM, and vendored NanoID:

| Hostile metric | vs Node | vs Bun | vs Deno |
|---|---:|---:|---:|
| ordinary equal-row geomean | **0.829×** [0.824, 0.837] | **0.611×** [0.604, 0.616] | **0.461×** [0.456, 0.467] |
| category-balanced geomean | **0.895×** [0.890, 0.905] | **0.650×** [0.641, 0.657] | **0.491×** [0.486, 0.498] |

For the requested project-wide view, the explicit equal-row aggregate across
all 30 normal and hostile rows is **0.703× Node** [0.700, 0.707], **0.548× Bun**
[0.544, 0.551], and **0.473× Deno** [0.470, 0.477]. It is calculated as
`exp((13 × ln(G13) + 17 × ln(G17)) / 30)`; its descriptive bootstrap resamples
the two separately captured suites as independent strata.

The aggregate is ahead, but the literal every-row target is not met. Zipp has
23 of 30 Node point wins: all 13 normal rows and 10 of the 17 hostile ones.
The Node point gaps are all in the hostile set: closure calls (1.11×), both
shape stressors (1.08× stable, 1.13× megamorphic), allocation survival
(1.37×), React reconcile (1.28×), the warm router (1.38×), and the JavaScript
bytecode VM (3.13×). The hostile guide reports each ratio rather than hiding
these behind the geomean.

```text
FASTER_THAN_NODE_ON_EVERY_ROW=0
FASTER_THAN_EVERY_ENGINE_ON_EVERY_ROW=0
```

Against the previous canonical capture at `8229b3fc` (2026-09-02; headline ten
0.878× Node, all 13 0.614×, all 30 0.728×), the normal rows moved most:
async-promise-chain went from 1.12× to 0.80× Node and json-large from 1.01× to
0.85×. That capture's artifacts stay in [`bench/`](../bench/README.md) as a prior
series point, as do the 2026-09-11 `14770703` pair recorded without a headline
claim. See the [`bench` guide](../bench/README.md),
[hostile suite](../bench/hostile/README.md), and
[`docs/PERF_ROADMAP.md`](PERF_ROADMAP.md) for exact methodology and remaining work.

</details>

### Interpreter versus interpreter

The tables above compare production JITs. This one asks the other question:
how fast is Zipp's interpreter against other interpreters? The same canonical
PGO binary runs with `ZIPP_NOJIT=1`, emitting no native code at all, against
Bellard's QuickJS 2026-06-04 and QuickJS-NG 0.17.0 (neither has a JIT), and
against the three JIT engines with their JITs switched off: Node `--jitless`
(V8's Ignition), Deno `--v8-flags=--jitless`, and Bun with `BUN_JSC_useJIT=0`
(JavaScriptCore's LLInt). Each switch was checked to disable its JIT: a tight
numeric loop runs 2-27× slower with it. [`run_native.py`](../bench/comparison/run_native.py)
staged identical inputs for every engine, checked every output against Node,
and ran 8 counterbalanced repetitions. The artifact,
[`interp_real13_fa8e3b86_2026-10-15.json`](../bench/comparison/interp_real13_fa8e3b86_2026-10-15.json),
is a diagnostic, not a canonical capture. Zipp's interpreter runs as it ships,
including its exact recognizers for a few loop shapes.

<details>
<summary><strong>Interpreter results on the 13 normal rows</strong></summary>

Cold medians include process launch; bold marks the fastest interpreter.

| Benchmark | Zipp interpreter | QuickJS | QuickJS-NG | Node jitless | Deno jitless | Bun no-JIT |
|---|---:|---:|---:|---:|---:|---:|
| async-promise-chain | **433 ms** | 4,261 ms | 4,124 ms | 450 ms | 447 ms | 587 ms |
| class-prototype-hot | 4,406 ms | 3,964 ms | 7,865 ms | **1,734 ms** | 1,774 ms | 5,824 ms |
| json-large | 636 ms | 779 ms | 1,418 ms | 547 ms | **546 ms** | 666 ms |
| map-set-heavy | **674 ms** | 1,465 ms | 2,215 ms | 772 ms | 1,214 ms | 1,140 ms |
| markdown-render | 828 ms | 1,063 ms | 2,072 ms | **772 ms** | 810 ms | 1,003 ms |
| parse-large-js | 1,879 ms | 2,044 ms | 3,761 ms | **1,352 ms** | 1,372 ms | 1,613 ms |
| polymorphic-objects | 895 ms | 876 ms | 1,425 ms | 609 ms | **561 ms** | 1,341 ms |
| regex-log-scan | 1,961 ms | 2,548 ms | 4,277 ms | 1,550 ms | **1,533 ms** | 2,774 ms |
| sparse-array | 358 ms | 386 ms | 795 ms | **205 ms** | 220 ms | 293 ms |
| typedarray-math | 6,314 ms | 4,106 ms | 7,347 ms | **2,237 ms** | 2,290 ms | 3,348 ms |
| polymorphic-objects-v2 | 438 ms | 401 ms | 850 ms | **300 ms** | 316 ms | 433 ms |
| property-ic-shapes | 3,194 ms | 2,044 ms | 5,406 ms | 1,892 ms | 1,941 ms | **1,827 ms** |
| sparse-array-v2 | 394 ms | 1,850 ms | 871 ms | 325 ms | **306 ms** | 504 ms |
| **Zipp interpreter / engine geomean** | — | **0.713×** [0.711, 0.722] | **0.442×** [0.439, 0.448] | 1.427× [1.415, 1.442] | 1.368× [1.352, 1.383] | **0.941×** [0.933, 0.953] |

</details>

Zipp's interpreter is faster than both QuickJS builds in aggregate (8 of 13
rows against Bellard's, all 13 against QuickJS-NG) and level with
JavaScriptCore's LLInt, but V8's Ignition is still about 1.4× faster. Bellard's
QuickJS wins five rows: property-ic-shapes (1.56×), typedarray-math (1.54×),
class-prototype-hot (1.11×), polymorphic-objects-v2 (1.09×) and
polymorphic-objects (1.02×). The five downscaled micro kernels in
[`interp_micro5_fa8e3b86_2026-10-15.json`](../bench/comparison/interp_micro5_fa8e3b86_2026-10-15.json)
show where the remaining interpreter cost is, calls and tight loops: there Zipp
is 1.12× Bellard's QuickJS (179 ms against 108 on recursive `fib`), 0.71×
QuickJS-NG and 1.26× Node `--jitless`.

Embedders who build the safe-sandbox profile (no `unsafe`, no JIT, with step
and heap limits set) run the same 13 rows at **0.843×** Bellard's QuickJS and
**0.525×** QuickJS-NG, in 0.583× of release 0.0.24's time and with a peak
working set 1.18× Bellard's QuickJS's; see the
[0.0.25 release notes](releases/0.0.25.md#embedding-the-safe-sandbox-profile).

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
