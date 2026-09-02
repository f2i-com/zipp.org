# Zipp performance handoff

This is the current continuation note. Historical snapshots through B252 are
archived in [`docs/archive/HANDOFF-through-B252.md`](docs/archive/HANDOFF-through-B252.md),
and the B001–B252 experiment ledger is preserved in
[`docs/archive/PERF_LEDGER-B001-B252.md`](docs/archive/PERF_LEDGER-B001-B252.md).

## Current engine baseline

Main is at v0.0.12 with the B263, B264 and B267 levers and the c28781cf
canonical capture. The tracked production WebAssembly module was rebuilt from
that engine (sandbox limits at their v0.0.10 sizes, B269's regex fix): the
landing page ships 5,558,860 bytes raw, 1,812,458 gzip-9, 1,248,649
Brotli-11, SHA-256 `bd8614fe5f3a3b8ef67f4b917cdefebb3fe69afa39a9804a0d3f6b0b6b267126`.

## v0.0.6 native interpreter / QuickJS-NG confirmation

The clean default-feature release binary at `e3acee352074` reran the current
real13 suite for six counterbalanced rounds with `ZIPP_NOJIT=1`. All 39
canonicalized validation outputs matched after QuickJS CRLF-to-LF normalization
and all 234 measured launch pairs completed. Zipp / QuickJS-NG v0.16.2 was
`0.6089665×` cold [0.6072021, 0.6122180] and `0.6058409×` startup-adjusted
[0.6041440, 0.6090422], with 13 / 13 point wins; intervals are descriptive 95%.
Raw evidence is
`target/comparison/results/native-real13-v006-e3acee352074-clean-6.json`
(SHA-256 `38915c58…2fe5`). This is native CLI evidence, not WASM evidence.

## v0.0.6 same-source WASM / QuickJS-NG diagnostic

The clean six-round capture is
`target/comparison/results/wasm-suites-v006-c77829269703-final-6.json`. It uses
the exact, unscaled current v0.0.6 source bytes used by the v0.0.6 Node/Bun/Deno
reruns, validates against Node output, and records empty source, artifact,
harness, and Git drift. Capture integrity passed, but `capture_usable:false`
and `evidence_usable:false` reflect four unexpected Zipp validation errors and
their skipped timed rows. Publication also failed because the cross-engine
aggregate is incomplete.

- Two hostile module rows cannot run through the production Zipp WASM API,
  which is built without a filesystem loader.
- The official QuickJS-NG v0.16.2 reactor cannot drain pending jobs for the
  three async rows.
- Zipp validated 7 / 28 scripts. Fourteen rows reached its fixed 50-million
  instruction budget, three reached its fixed 128 MiB approximate heap budget,
  and four ended in other errors: an async trap, two invalid-string-length
  errors, and one typed-array-length error.

There are no comparable normal-suite rows. Only five hostile rows produced
complete samples for both engines:

| Hostile row | Zipp / QuickJS-NG persistent | adjusted |
|---|---:|---:|
| shapes-stable | `1.2860×` | `1.2811×` |
| allocation-ephemeral | `1.0507×` | `1.0482×` |
| allocation-survival | `1.0777×` | `1.0732×` |
| reactish-reconcile | `1.1755×` | `1.1694×` |
| warm-router | `0.4773×` | `0.4755×` |

The available five-row geomeans are `0.9604×` persistent and `0.9567×`
adjusted, with Zipp ahead on only one row. They are not complete hostile or
combined-suite aggregates and must not be presented as such. The separately
sampled compile / instantiation medians were 5.07995 / 0.39700 ms for Zipp and
1.79525 / 1.67570 ms for QuickJS-NG. Their unpaired sums, 5.47695 / 3.47095 ms,
are not measured end-to-end medians. The direct result does not establish that
Zipp WASM is faster than QuickJS-NG WASM.

### Specialization-sensitive micro diagnostic

The earlier five-workload speed-kernel experiment remains attribution evidence.
Its persistent geomean was `0.0954663913×` QuickJS-NG, but three adjusted rows
were at the subtraction noise floor. More importantly, disabling the exact
workload lanes measured `1.815×` QuickJS-NG persistent. Treat the apparent
10.5-fold result as adapter-plus-specialization behavior, not general
interpreter throughput. The older pre-kernel baseline measured
`1.8115847493×` persistent and `1.7725499353×` adjusted.

That exact-lanes-off candidate measured `0.1992206×` Boa persistent and
`0.1922208×` adjusted with five point wins, but it records a dirty heap source
and candidate module SHA `09c0772f…a6bc`; it is not release evidence. No Boa run
exists for the current exact normal-13-plus-hostile-17 WASM inventory.

QuickJS-NG also retains the payload-size lead: its reactor is 1,528,293 bytes
raw and 417,087 Brotli-11, making Zipp `3.586×` as large raw and `2.958×` as
large at Brotli-11. Exact commands, artifacts, and interface caveats are in
[`bench/comparison/README.md`](bench/comparison/README.md).

## Historical v0.0.5 QuickJS-NG / Boa release diagnostic

All source output matched before timing. Six complete counterbalanced rounds and
10,000 bootstrap samples produced:

| Diagnostic | Zipp / competitor | 95% interval | point wins |
|---|---:|---:|---:|
| native interpreter real13 / QuickJS-NG | **0.6413×** | [0.6386, 0.6452] | 12 / 13 |
| native interpreter micro5 / QuickJS-NG | **0.8556×** | [0.8405, 0.8761] | 5 / 5 |
| native interpreter micro5 / Boa | **0.2539×** | [0.2501, 0.2590] | 5 / 5 |
| WASM adjusted execution / QuickJS-NG | **2.1074×** | descriptive | 0 / 5 |
| WASM adjusted execution / Boa | **0.2274×** | descriptive | 5 / 5 |

The native sparse-array point median was the only Zipp loss at 1.0099×;
the aggregate was never an every-row claim. Historical raw files remain under
`target/comparison/results/`: `native-real13-v005-qjsng-clean-6.json`,
`native-micro5-v005-clean-6.json`, and `wasm-v005-clean-6.json`.

### Historical v0.0.5 release validation

Focused release gates were green: safe-sandbox library 463 passed / 1 ignored,
benchmark-tool tests 164 passed / 2 skipped, bool-home 11 passed / 1 worker
ignored, split-receiver 7 / 7, isolated WASM Rust host 4 / 4, Node host contract
137 / 137, and syntax corpus 23 / 23. These counts describe v0.0.5, not the
v0.0.6 release gate.

## Canonical public state

The separate Node/Bun/Deno PGO publication series below remains the current
canonical public capture; the v0.0.5 ecosystem comparison above does not
silently replace it.

The current raw captures are:

- `bench/real13_c28781cf_pgo_2026-09-02.json`
- `bench/hostile/head_clean_c28781cf_pgo_2026-09-02.json`

Both use Node v24.12.0, Bun 1.3.14, Deno 2.6.10, and Zipp 0.0.11. Both report
`publishable:true`, `ALL_CORRECT=1`, 15 complete counterbalanced repetitions,
10,000 bootstrap samples, and empty provenance, publication, correctness,
health, source-drift, engine-drift, input-drift, and harness-drift failures.

| Corpus | vs Node | vs Bun | vs Deno |
|---|---:|---:|---:|
| retained ten | **0.886×** [0.880, 0.892] | 0.757× [0.752, 0.764] | 0.770× [0.761, 0.778] |
| diagnostics three | **0.189×** [0.186, 0.192] | 0.171× [0.169, 0.175] | 0.149× [0.146, 0.153] |
| normal all 13 | **0.620×** [0.616, 0.624] | 0.537× [0.534, 0.542] | 0.527× [0.521, 0.532] |
| hostile all 17 | **0.824×** [0.800, 0.838] | 0.657× [0.648, 0.666] | 0.429× [0.422, 0.436] |
| hostile category-balanced | **0.860×** [0.834, 0.873] | 0.676× [0.663, 0.683] | 0.442× [0.435, 0.449] |
| all 30, equal row weight | **0.729×** [0.716, 0.736] | 0.602× [0.597, 0.607] | 0.469× [0.464, 0.474] |

The all-30 point is
`exp((13 × ln(G13) + 17 × ln(G17)) / 30)`. Its 10,000-sample descriptive
bootstrap shares resampled repetition indices within a suite and resamples the
normal and hostile captures independently. It is not a hypothesis test and does
not estimate machine-to-machine variability.

Normal has 33 / 39 point and 29 / 39 Bonferroni exact-sign wins across all
competitors; hostile has 39 / 51 point and 34 / 51 exact-sign wins. Against
Node alone, Zipp has 21 / 30 point wins. The literal all-row target remains
false.

## Current Node gaps

| Row | Zipp / Node | Descriptive 95% interval |
|---|---:|---:|
| reactish-reconcile | **1.574×** | [1.477, 1.618] |
| warm-router | **1.563×** | [1.491, 1.638] |
| allocation-survival | **1.484×** | [1.455, 1.670] |
| shapes-megamorphic | **1.226×** | [1.169, 1.273] |
| shapes-stable | **1.199×** | [1.126, 1.244] |
| calls-closures | **1.100×** | [0.834, 1.120] |
| async-promise-chain | **1.074×** | [1.057, 1.104] |
| async-lived | **1.065×** | [1.015, 1.081] |
| json-large | **1.022×** | [1.009, 1.031] |

calls-closures is a point gap whose descriptive interval crosses parity;
bytecode-vm (0.994× [0.967, 1.007]) is a point win whose interval also
crosses. sparse-array (0.924×), regex-log-scan (0.953×) and npm-nanoid
(0.966×) left the gap list with this capture.

## Verification completed

- `cargo test -p zipp-vm --lib` plus the compiler and tier suites
  (`reg_classes`, `jit_tier_parity`, `jit_tier_fuzz`, `typeof_alias`,
  `int_split`, `int_gpr_homes`, `int_splice`, `shell_cell`, `int32_trunc_add`,
  `json_plain_key`, `combinator_job_order`, `real_program_corpus`,
  `instr_uses_exhaustive`, `double_mod`)
- `cargo check -p zipp-vm --no-default-features`,
  `--no-default-features --features safe-sandbox`, and the sandbox and wasm
  workspaces
- a 188-run four-mode output identity (default, `ZIPP_NOJIT=1`,
  `ZIPP_JIT_THRESHOLD=1`, `ZIPP_NO_NURSERY=1`) against node over every bench
  and syntax-corpus program
- clean provenance-stamped PGO build from committed source
- complete normal and hostile Node/Bun/Deno/Zipp captures with exact output

## Highest-value next work

0. **Handler-op bodies on the emitted lanes.** B272 gives them a
   frame-backed entry through the generic helper (reactish ~4-5%); two
   follow-ups remain: an exception-edge-aware may-read-before-write pass so
   the masked fast fill applies (today every such call pays the full
   zero-fill `resize`, deliberately — see B272), and a frame-pushing variant
   of the CROSS3 lane for
   the recursive `diff`-shaped site, whose per-call helper cost (~20 ns) is
   now the dominant remaining call overhead on that row.
1. **React reconcile and warm router (`1.574×` / `1.563×`).** The birth/death
   pipeline dominates both (`free_slot`, `refit_finalized_inner`,
   `alloc_finalized`, `alloc_settled`, malloc/free ≈ 19–24% of each row);
   B265's slot resurrection lost, so the next design must remove work from the
   pool refit or the sweep itself, not move it. The router's Map path (about
   21%) is the intrinsic proof plus index lookup plus key hashing — B266
   showed the byte compare is not it.
2. **Allocation survival (`1.484×`).** `free_slot` 12%, `trace_edges` 6%,
   `jit_get_index` 4% (Tier C reads `this.values[i]` through a helper), the
   method-call ICs 5.6%.
3. **Stable and megamorphic shapes (`1.199×` / `1.226×`).** Now the same
   alloc pipeline; B264 removed the store helper.
4. **Async (`1.074×`, `1.065×`).** partA's `p.then(addOne)` loop is declined
   by the call-mix gate (a native-callee site); partB's `await` body is
   interpreted by the async tier gate. A native `then` lane in regions is the
   bounded next step.
5. **json-large (`1.022×`).** Parsed objects miss the object pool (a fresh
   `Box` and key `String` per object); the pooled birth is drafted as B268.
5b. **Tier A for cell-captured self-recursion.** Tier A (the leaf-int whole
   function tier) admits the self-call only as `LoadGlobal(name_global)`; a
   pure-int recursive function declared inside a closure now takes the Tier-C
   CROSS3 self arm (B270: 106 ms for IIFE fib(32)) but Tier A's direct
   `call self_entry` would put it at the top-level figure (47 ms). Extend
   `is_self_call`/`can_compile` to an `UpvalGet` whose cell holds the
   running closure, with the entry-time self-binding re-check reading the
   cell instead of the global.
6. **Sandbox RegExp exec overhead.** After B269 a sticky `exec` costs about
   13 µs in the WASM build against 4 µs natively (the spec `split` loop runs
   one per character); the remaining per-exec work is the limited backtracker
   setup, the transient reservation and the lastIndex property round trips.
7. **Interval prover.** Widening after pass 8 re-widens a compare-narrowed
   loop bound inside the body, so `o + 2` keeps its i53 guard; head-only
   widening would free r13/r14 on more INT-GPR regions.

## Commands for the next session

```powershell
git status --short --branch
git log -6 --oneline --decorate
cargo build -p zipp-cli --release
& target\release\zipp.exe --version --json
```

Run a focused normal binary A/B:

```powershell
python tools\bench.py `
  --ab target\bench-binaries\old.exe target\bench-binaries\new.exe `
  --ab-env - - `
  --benches <case> `
  --reps 16 `
  --seed 0x5a172026 `
  --json target\bench-results\focused-ab.json `
  --allow-nonhead-engine
```

Run a focused hostile engine diagnostic:

```powershell
python tools\bench_hostile.py `
  --cases <case> `
  --engines node,zipp `
  --zipp target\release\zipp.exe `
  --reps 16 `
  --json target\bench-results\focused-node.json
```

## Working rules

- Keep implementation, validation, documentation, and cleanup in small commits.
- Prefer a same-binary off-switch A/B for attribution, then compare frozen
  binaries for layout and generalisation.
- Cold time is the publication gate; adjusted time is diagnostic on short rows.
- Preserve exact output and empty health, correctness, and drift failure lists.
- Keep routine artifacts under ignored `target/bench-results/`.
- Never promote a dirty, filtered, non-PGO, or incomplete-engine artifact.
- Record neutral and refuted ideas so they are not repeated.
