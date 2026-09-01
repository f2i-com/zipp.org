# Zipp performance roadmap

> **Goal:** become faster than Node, Bun, and Deno on every maintained
> benchmark while preserving exact JavaScript semantics and tier parity.

This file is the current performance board. The append-only B001–B252 ledger,
including the negative results and original line-level evidence, is archived at
[`docs/archive/PERF_LEDGER-B001-B252.md`](docs/archive/PERF_LEDGER-B001-B252.md).
Search that file before reopening an older idea.

## Evidence legend

| Label | Meaning |
|---|---|
| **CANONICAL** | Clean, HEAD-matching, provenance-stamped PGO capture with the complete required engine/suite protocol and `publishable:true`. May update public Node/Bun/Deno ratios. |
| **DIAGNOSTIC A/B** | Focused, filtered, same-binary, dirty-comparator, non-PGO, or incomplete-engine evidence. Useful for decisions; never a public engine score. |
| **LANDED** | Default-on code is committed and pushed after its correctness and performance gates. |
| **VERIFIED** | Mechanism and result are established; the surrounding campaign or public capture may still be pending. |
| **REFUTED** | Built or measured and rejected. Do not retry without new evidence that invalidates the kill reason. |
| **REVERTED** | Experimental code is absent from the default tree. |

Cold wall time is the headline. Startup-adjusted time is diagnostic, especially
on short cases. Ratios are new/old for binary A/Bs and Zipp/competitor for engine
tables; below `1.0×` is faster.

## Current status — 2026-09-01

### Public canonical capture

The public score is the clean four-engine PGO capture at `21288c1`:

| Corpus | Node | Bun | Deno | Node point wins |
|---|---:|---:|---:|---:|
| retained ten | **0.921×** [0.913, 0.928] | 0.782× [0.778, 0.789] | 0.797× [0.790, 0.806] | 6 / 10 |
| diagnostics three | **0.192×** [0.191, 0.195] | 0.171× [0.168, 0.174] | 0.152× [0.150, 0.155] | 3 / 3 |
| all 13 | **0.642×** [0.638, 0.646] | 0.550× [0.547, 0.555] | 0.544× [0.540, 0.549] | 9 / 13 |
| hostile all 17, ordinary | **0.881×** [0.874, 0.886] | 0.670× [0.665, 0.677] | 0.455× [0.450, 0.461] | 8 / 17 |
| hostile category-balanced | **0.913×** [0.907, 0.919] | 0.686× [0.678, 0.695] | 0.467× [0.462, 0.473] | — |
| all 30, equal row weight | **0.768×** [0.764, 0.771] | 0.615× [0.613, 0.620] | 0.492× [0.489, 0.496] | 17 / 30 |

Both source artifacts are `publishable:true`, `ALL_CORRECT=1`, use 15
counterbalanced repetitions and 10,000 bootstrap samples, and have empty drift
and failure lists. The all-30 interval is a stratified descriptive bootstrap:
normal and hostile repetitions are resampled independently, while every row
inside one suite shares the same repetition indices.

Across Node, Bun, and Deno, the normal suite has 29 / 39 point and exact-sign
wins; hostile has 36 / 51. The literal all-row target remains false. See
[`README.md`](README.md#performance-measured-honestly) for the tables and raw
artifact links.

### Historical v0.0.5 QuickJS-NG and Boa diagnostic

The release-default native executable at engine commit `7cb7210` reports
v0.0.5, the exact source commit, `dirty:false`, `opt-level:3`, default features,
and no PGO or ad-hoc Rust flags. Six counterbalanced repetitions with exact
output and 10,000 bootstrap samples measured:

| Suite | Comparator | Zipp / comparator | point wins |
|---|---|---:|---:|
| native interpreter real13 | QuickJS-NG v0.16.2 | **0.6413×** [0.6386, 0.6452] | 12 / 13 |
| native interpreter micro5 | QuickJS-NG v0.16.2 | **0.8556×** [0.8405, 0.8761] | 5 / 5 |
| native interpreter micro5 | Boa v0.22.0 | **0.2539×** [0.2501, 0.2590] | 5 / 5 |
| WASM adjusted execution | QuickJS-NG reactor | **2.1074×** | 0 / 5 |
| WASM adjusted execution | Boa package | **0.2274×** | 5 / 5 |

The one native real13 QuickJS-NG point gap is sparse-array at 1.0099×. The
v0.0.5 WASM artifact is 5,595,833 bytes raw / 1,254,075 Brotli-11, versus
1,528,293 / 417,087 for the QuickJS-NG reactor and 21,296,176 / 5,484,164 for
Boa. The WASM interfaces and feature sets differ, so this is diagnostic
attribution rather than a universal engine ranking. Exact commands and hashes
are in [`bench/comparison/README.md`](bench/comparison/README.md).

### Current v0.0.6 native QuickJS-NG confirmation

The clean default-feature release binary at engine-source commit `e3acee352074`
reran the current real13 protocol for six counterbalanced rounds with
`ZIPP_NOJIT=1`. All canonicalized output matched after documented QuickJS
CRLF-to-LF normalization, and Zipp led QuickJS-NG v0.16.2 on all 13 point
medians. Cold Zipp / QuickJS-NG was `0.6089665×` [0.6072021, 0.6122180];
startup-adjusted was `0.6058409×` [0.6041440, 0.6090422], with descriptive 95%
intervals. This closes the historical sparse-array point gap for the current
native engine source. It remains diagnostic native CLI evidence and does not
imply a WASM win.

### v0.0.6 exact-suite WASM status

The committed production WebAssembly module is 5,480,311 bytes raw, 1,825,812
gzip-9, and 1,233,575 Brotli-11, SHA-256
`318fc5cf7ee5d55751d829419d4de5af1ab2643b8f7fd30df2e3779c16ad1691`.
QuickJS-NG's official reactor is 1,528,293 raw / 417,087 Brotli-11, so Zipp is
`3.586×` as large raw and `2.958×` as large at Brotli-11.

The clean six-round attempt over the exact, unscaled current v0.0.6 normal 13
and hostile 17 sources used by the v0.0.6 Node/Bun/Deno reruns is
`target/comparison/results/wasm-suites-v006-c77829269703-final-6.json`. It has
clean provenance and complete capture attempts, but records
`publishable:false`, `capture_usable:false`, and `evidence_usable:false`:

- the production Zipp WASM API cannot load the two module rows;
- the official QuickJS-NG reactor cannot drain pending jobs for three async rows;
- Zipp validated 7 / 28 script rows. Fourteen stopped at its fixed instruction
  budget, three at its fixed approximate-heap budget, and four at other engine
  errors.

There are no comparable normal-suite rows and only five comparable hostile
rows. Their available-only Zipp / QuickJS-NG geomean is `0.9604×` persistent
and `0.9567×` adjusted, with one Zipp point win. QuickJS-NG leads
`shapes-stable` (`1.2811×` adjusted), `allocation-ephemeral` (`1.0482×`),
`allocation-survival` (`1.0732×`), and `reactish-reconcile` (`1.1694×`); Zipp
leads `warm-router` (`0.4755×`). These values are row-level diagnostics, not
complete suite aggregates. QuickJS-NG has the faster separately sampled compile
median (1.795 ms versus 5.080 ms), while Zipp has the faster instantiation/start
median (0.397 ms versus 1.676 ms). The sums of those phase medians are not
measured end-to-end times.

The prior five-workload speed-kernel experiment measured `0.0954663913×`
QuickJS-NG persistent, but it is specialization-sensitive: the exact-lanes-off
control measured `1.815×` QuickJS-NG persistent, and three adjusted rows sat at
the subtraction noise floor. That dirty-tree control also measured `0.1992×`
Boa persistent with five point wins, but no Boa run exists for the current
normal-13-plus-hostile-17 WASM inventory. Preserve these as mechanism
attribution, never as general WASM speed headlines.

The highest-value WASM work is therefore coverage before aggregate tuning:
provide a separately explicit comparison configuration without weakening the
production safety defaults, fix the async trap and string/typed-array capacity
failures, then optimize the four measured gaps. Do not claim a suite win until
all cross-engine-supported rows complete with exact output.

### Correctness

- test262: **95,939 / 95,942** required executions (99.997%)
- expected failures: one stale Annex B assertion and two German-CLDR rows
- former errored-module-cycle and deferred top-level-await gaps: fixed
- default JIT, interpreter-only, forced-JIT, and majors-only-GC identities are
  the conformance gate
- tier-differential fuzzing remains mandatory for JIT work

Do not edit historical B entries merely because their old pass counts were
correct for their own commit.

## Latest experiment registry

### B256 LANDED — dense-array pin snapshots stay warm across stores and helper calls

Commit `b1435c7`. PC-profiling the object-lifetime cluster (a `profiling`
build with the linker map, 30 accumulated `ZIPP_PROF_PC` runs per row,
`tools/pcmap.py` over the numeric dump files only) showed
`jit_ta_snapshot` + `dense_array_snap_flags` at 7–8.5% of allocation-survival,
shapes-stable and shapes-megamorphic: every cross call AND every
`jit_set_index` / method-IC helper re-derived the region's dense-Array pin
snapshots through the helper. Two causes, both fixed behind latches:

- `Heap::get_mut` advances `array_snapshot_epoch` on every Array borrow, and
  `jit_set_index` used it for an in-range element store, so B244's per-call
  epoch cache never hit inside a loop storing into the array it pins. An
  in-range store (present element, or a hole-fill below `len` the protector
  checks already licensed) now writes through `Heap::array_store_in_place`,
  which leaves base and length — the only facts a snapshot licenses —
  untouched. `ZIPP_NO_ARRAY_STORE_NOBUMP=1` restores the bump.
- Twenty-plus post-helper refetch sites in `region_mem.rs` / `inline.rs`
  called `emit_refetch_ta` unconditionally; they now go through
  `emit_cross_refetch_ta` (epoch + live-source identity check), the
  `ta_refetch` tuple carrying the region's epoch-cache offset.
- `emit_refetch_pinned` re-pins r13/r14 from the `versions_raw` /
  `ic_table_raw` mirrors (the B250 Tier-C entry loads) instead of two helper
  calls. `ZIPP_NO_DIRECT_REFETCH_BASES=1` restores the calls.

| DIAGNOSTIC A/B (same binary unless noted, 16 pairs, exact output) | Result |
|---|---|
| store no-bump latch: allocation-survival | **−3.7%** [−5.7, −1.7] |
| store no-bump latch: shapes-stable / shapes-megamorphic | **−2.4%** [−3.2, −2.0] / **−3.2%** [−4.8, −2.0] |
| cached post-helper refetch, two-binary over the first: shapes-stable / megamorphic / survival | **−4.5%** [−5.3, −3.6] / **−4.1%** [−4.8, −3.2] / −3.1% [−4.2, +0.1] |
| direct base refetch latch: warm-router / reactish-reconcile | **−3.7%** [−5.3, −2.5] / **−1.5%** [−2.8, −0.7] |
| all other cluster rows | null |

Method lessons recorded: a build-then-copy chain must gate the copy on the
`Finished` line (a failed build silently re-measured the previous binary,
producing plausible "results" for two runs); and `tools/pcmap.py` must be
fed only the numeric dump files, never the `[profpc]` stderr summaries.

### B255 LANDED — hostile-path routing and append cursor

Commit `21288c1` brings four independently guarded mechanisms together:

- stable paired-`typeof` fusion (`ZIPP_NO_TYPEOF_SAME=1` restores the prior
  route);
- call-free Tier-C loose-null checks
  (`ZIPP_NO_TIERC_LOOSE_NULL_INLINE=1`);
- a polymorphic-function-id Cross3 call router
  (`ZIPP_NO_CROSS3_POLY_FID=1`); and
- a deferred flat-ASCII append cursor (`ZIPP_NO_STR_APPEND_CURSOR=1`).

The same commit fixes protected returns that cross `finally` and adds focused
coverage for that control-flow boundary. The final PGO executable is
`c2ddb9e6…6a3cb5`, built from source `21288c1` with profile
`fbe16992…e91743`. Its complete normal and hostile captures are the current
public results above. Exact output held in all 1,800 four-engine observations.

The aggregate result is not the finish line. Current Node point gaps are the
four normal rows async-promise-chain, json-large, regex-log-scan, and
sparse-array, plus nine hostile rows: calls-closures, shapes-stable,
shapes-megamorphic, allocation-survival, async-lived, reactish-reconcile,
warm-router, bytecode-vm, and npm-nanoid.

### B254 LANDED — pinned call-result string length

Commit `4ff3bdf` uses the MEM string pin's immutable `{obj_bits, bytes, units}`
snapshot to answer `.length` before the eight-way identity IC and B190 helper.
The emitted lane checks the exact receiver identity, rejects an invalid
snapshot, loads UTF-16 units, boxes the value, and otherwise falls through to
the unchanged generic path. `ZIPP_NO_PINNED_STR_LEN=1` restores the prior route.

Final reviewed binary: commit `2869e91`, SHA-256 `bf85742b…f85952`, clean
release build.

| DIAGNOSTIC A/B | Result |
|---|---|
| same-binary NanoID, 32 pairs | **−13.11%**, 0.868869 [0.860467, 0.873758], 31 / 32 wins |
| final vs B252 NanoID, 32 pairs | **−12.85%**, 0.871516 [0.866427, 0.881037], 32 / 32 wins |
| final vs frozen B253, hostile all-17 | **−1.28%**, 0.987230 [0.984695, 0.991308] |
| final vs frozen B253, category-balanced | **−1.74%**, 0.982579 [0.979742, 0.987197] |
| final vs frozen B253, NanoID | **−12.88%**, 0.871198 [0.866817, 0.877921] |
| final vs frozen B253, React | **−2.12%**, 0.978798 [0.974462, 0.991749] |
| final vs frozen B253, normal all-13 | **−0.50%**, 0.9950 [0.9873, 1.0017], neutral |

Every output was exact and no cold row had a supported regression. The full
hostile artifact's adjusted analysis is incomplete only because the very short
allocation-ephemeral case had no usable adjusted ratio; the cold analysis is
complete. The B253 comparator is a frozen dirty feature build, so the sweep is
not publication evidence.

At the B254 checkpoint, filtered 16-pair Node diagnostics put NanoID at
**1.169× Node** [1.160, 1.195] and React at **1.673× Node** [1.618, 1.715].
Those dated figures are retained as attribution evidence; B255's canonical
ratios above supersede them for the current engine comparison.

### B254 hardening LANDED — impossible identity-IC empty marker

Commit `6f945ce` replaces zero in fresh identity-cache ways with
`0x7FFE000000000001`, a NaN bit pattern outside all five `Value` tags. Numeric
boxing canonicalizes it, so raw f64 `+0` cannot match an empty way when pinned
length or quick length is switched off. Direct shape-pair sites deliberately
retain zero because their packed pattern slots use it as the free marker.

Release tests prove raw `+0` reaches the target MEM property region under all
three feature off-switches. This is a safety fix with no speed claim.

### B253 LANDED — stable concat suffix memo

Commit `ff1c737` adds a weak, lazy 256-entry suffix memo for a size-bounded
B212-frozen `const + int` head followed by one pinned immutable one-byte ASCII
suffix. Exact keys plus left/result versions cover ABA; results stay frozen and
cannot seed recursively. The emitter admits only an adjacent terminal
`StrConcatChain` followed by the lowering's exact `Move`, and the runtime proves
the final leaf is tagged `Int`. `ZIPP_NO_CONCAT_SUFFIX_MEMO=1` restores the old
route.

Same-binary React evidence, 32 pairs: **−4.14%**, 0.958636 [0.949720,
0.965530], 31 / 32 wins. Complete normal and hostile safety sweeps were neutral.

### B253 hardening LANDED — validate both concat links

Commit `2869e91` requires both current and next chain links to satisfy
`dst == a == accumulator`, and the trailing `Move` to consume that accumulator,
before emitting the suffix hint. The compiler already generated that invariant;
the validation protects future rewrites. Focused invariant and all 19 concat
parity/mechanism tests pass. No speed claim.

### B252 LANDED — merge monotonic recycled-object address runs

Commit `f5c33df` proves an ascending retained prefix and monotonic newly-dead
suffix, then joins them with reverse/rotate; uncertain layouts retain the full
sort. `ZIPP_NO_OBJ_POOL_RUN_SORT=1` restores the old route. Allocation survival
improved **2.93%** [0.28%, 3.34%] across 31 pairs; shapes and the normal suite
were neutral. A MEM-to-nested-INT NanoID handoff was separately **REFUTED** at
**+86.11% slower** and fully reverted.

### B251 LANDED — scalar singleton `[[IsHTMLDDA]]`

Commit `c575105` mirrors the production VM's one HTMLDDA exotic as a scalar
comparison instead of paying a hash lookup in `typeof`, loose-null, truthiness,
callability, and Annex B paths. React improved **4.11%** [3.75%, 5.05%]; normal
all-13 was neutral. `ZIPP_NO_HTMLDDA_SCALAR=1` restores the old path.

### B250 VERIFIED — call-result string pins and call-entry cleanup

B250 admitted call-written string receivers only under a forward MUST
reaching-definition proof, refreshed snapshots after calls, loaded Tier-C bases
directly, and emitted same-`FuncProto` guards. Its combined exact hostile A/B
improved **3.07%** [1.57%, 4.30%], with NanoID and closure-call gains; the
normal all-13 sweep was neutral. B254 removes the remaining `.length` IC/helper
cost from that string-pin path.

### B249 CANONICAL — captured-call scaffolding closed

B249 restored fused direct and computed method calls where argument evaluation
cannot observe the property read, stopped recycling captured temporaries, and
added bare guarded `MathOp` lowering. The clean `0bff482` PGO capture moved the
retained-ten result to 0.918× Node, all-13 to 0.635×, and hostile ordinary to
0.866×. It was the public baseline before B255's `21288c1` capture.

Earlier entries, designs, hazards, and refutations remain searchable in the
[historical ledger](docs/archive/PERF_LEDGER-B001-B252.md).

## Active targets

Priorities are ordered by current measured gap and by whether the next question
can be answered with a bounded experiment.

### 1. Broader-corpus WASM robustness and transfer size

The exact v0.0.6 attempt exposes a coverage problem before a throughput ranking:
there are no comparable normal rows and only five comparable hostile rows, with
QuickJS-NG ahead on four. First make the fixed production-limit boundary
explicitly configurable for a separate diagnostic without weakening shipped
defaults, repair the async trap and string/typed-array capacity failures, and
require exact output across every cross-engine-supported row. Keep the existing
persistent-module boundary and frozen work/control sources as the continuity
anchor; do not tune only to the five speed-kernel shapes.

The Zipp artifact remains `3.586×` as large raw and `2.958×` as large at
Brotli-11 as QuickJS-NG's reactor.
Attribute host glue, Rust/runtime footprint, and interpreter code separately.
Compressed transfer size is the shipping objective. The remaining absolute gap
is large enough to justify a fresh symbol/section and feature-footprint audit.

### 2. Guard the native real13 win over QuickJS-NG

The clean final-engine-source diagnostic now leads all 13 QuickJS-NG point
medians, at `0.6058409×` adjusted overall; the closest row is sparse-array at
`0.9312122×`. Keep exact output and this full 13-row protocol as the regression
gate. Do not profile a native QuickJS gap unless a future clean rerun reveals
one; the present priority is WASM coverage and its four measured point gaps.

### 3. Allocation survival — 1.772× Node

This is the largest current Node gap and one of three hostile rows above the
aspirational 1.50× individual cap, alongside warm-router and
reactish-reconcile. Profile nursery promotion, survivor tracing, and
free-list/pool maintenance separately; do not infer the term from the much
faster allocation-ephemeral row.

### 4. Warm router and React reconcile — 1.674× / 1.646× Node

Both application-shaped rows need fresh attribution on B255. For the router,
separate closure dispatch from property and URL/string work. For React, measure
recursive framed calls, handler-bearing loops, shape checks, and allocation
before attempting another combined shortcut.

### 5. Object shapes — 1.493× / 1.484× Node

Stable and megamorphic shapes are almost equally slow, which argues against
assuming this is only an IC-polymorphism problem. Compare lookup, guard failure,
transition, and allocation counters on the same binary.

### 6. Calls, async, and retained normal gaps

Closure calls are **1.307× Node** and async-promise-chain is **1.232×**.
Long-lived hostile async is **1.082×**. JSON and sparse-array are about
**1.05×**, while regex is **1.017×** with an interval crossing parity. Scout one
mechanism at a time and require a complete normal-plus-hostile safety sweep.

Bytecode-vm (**1.014×**) and NanoID (**1.000×**) are point gaps but currently
near parity; neither should outrank a supported double-digit gap without a
cheap, isolated explanation.

## Standing gate

### Correctness

```powershell
cargo test --workspace --release
cargo check -p zipp-vm --no-default-features
cargo check -p zipp-vm --no-default-features --features safe-sandbox
cargo test -p zipp-vm --no-default-features --features safe-sandbox --no-fail-fast
```

The last line is the whole hardened-profile suite and is expected clean
(209 binaries, 0 failures as of 2026-09-02): the JIT-pinning suites carry the
x86-64 JIT cfg, the limit tests size themselves from `zipp_vm::safe_native_limits`,
and the drains that need sixty-seven million iterator steps run only under
`--release`. For changes that touch sandbox-only code, also run the standalone
sandbox workspace. For interpreter/JIT semantics, compare the test262 expected-failure
identity in default, `ZIPP_NOJIT=1`, `ZIPP_JIT_THRESHOLD=1`, and
`ZIPP_NO_NURSERY=1` modes. Run the tier-differential fuzz slice for codegen,
register-planning, deopt, or heap-layout changes.

### Focused performance

1. Add an off-switch where a same-binary A/B is practical.
2. Use at least 16 exactly balanced pairs; use 32 for marginal or
   layout-sensitive decisions.
3. Require exact output and empty health/correctness/drift failure lists.
4. Re-run the affected normal and hostile safety sets.
5. Fail a candidate when a supported unrelated cold regression exceeds 0.5%
   unless a documented trade is explicitly accepted.
6. Freeze and hash the final clean binary; do not benchmark a moving target.

Routine artifacts go under ignored `target/bench-results/`. See
[`bench/README.md`](bench/README.md) for commands and publication policy.

### Public capture

Use `tools/pgo.sh`, then the complete `tools/bench.py` and
`tools/bench_hostile.py` engine protocols. `bench/run_real.sh` is legacy and is
not the publication runner. Promote an artifact only after an independent audit
confirms `publishable:true`, clean source/binary provenance, full engine order,
complete suite selection, exact output, adequate repetitions/bootstrap samples,
and no drift.

## Experiment discipline

- Profile the current binary before editing; old attribution expires quickly.
- Change one mechanism at a time and push each verified small win.
- Prefer same-binary attribution, then a frozen-binary layout/generalisation
  comparison, then a fresh engine diagnostic.
- Record the exact comparator, binary hash, commit, switches, repetitions,
  ordering, medians, ratio, interval, strict wins, and correctness status.
- Neutral results are useful. Revert experiments that miss their gate and keep
  the kill reason here.
- Never use an aggregate to imply that every row wins.
- Never convert a security-policy change into a performance default without an
  explicit policy decision.

## Historical lookup

The archived ledger retains B001–B252, old section numbers, detailed hazard
proofs, and refuted experiments. Source comments that cite “`PERF_ROADMAP.md`
B59”, B9, B29, or another historical ID refer to that archive. The durable
nursery design remains at [`NURSERY_DESIGN.md`](NURSERY_DESIGN.md) because code
comments cite its numbered sections directly.
