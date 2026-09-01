# Zipp performance handoff

This is the current continuation note. Historical snapshots through B252 are
archived in [`docs/archive/HANDOFF-through-B252.md`](docs/archive/HANDOFF-through-B252.md),
and the B001–B252 experiment ledger is preserved in
[`docs/archive/PERF_LEDGER-B001-B252.md`](docs/archive/PERF_LEDGER-B001-B252.md).

## Current engine baseline

The v0.0.6 performance candidate includes the runtime work through `52645a0`,
the tracked production WebAssembly rebuild at `f9bf8d3`, and the fail-closed
same-suite harness repair at `c778292`. The production browser module is
5,480,311 bytes raw, 1,825,812 gzip-9, and 1,233,575 Brotli-11, with SHA-256
`318fc5cf7ee5d55751d829419d4de5af1ab2643b8f7fd30df2e3779c16ad1691`.

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

- `bench/real13_21288c1_pgo_2026-08-30.json`
- `bench/hostile/head_clean_21288c1_pgo_2026-08-30.json`

Both use Node v24.12.0, Bun 1.3.14, Deno 2.6.10, and Zipp 0.0.1. Both report
`publishable:true`, `ALL_CORRECT=1`, 15 complete counterbalanced repetitions,
10,000 bootstrap samples, and empty provenance, publication, correctness,
health, source-drift, engine-drift, input-drift, and harness-drift failures.

| Corpus | vs Node | vs Bun | vs Deno |
|---|---:|---:|---:|
| retained ten | **0.921×** [0.913, 0.928] | 0.782× [0.778, 0.789] | 0.797× [0.790, 0.806] |
| diagnostics three | **0.192×** [0.191, 0.195] | 0.171× [0.168, 0.174] | 0.152× [0.150, 0.155] |
| normal all 13 | **0.642×** [0.638, 0.646] | 0.550× [0.547, 0.555] | 0.544× [0.540, 0.549] |
| hostile all 17 | **0.881×** [0.874, 0.886] | 0.670× [0.665, 0.677] | 0.455× [0.450, 0.461] |
| hostile category-balanced | **0.913×** [0.907, 0.919] | 0.686× [0.678, 0.695] | 0.467× [0.462, 0.473] |
| all 30, equal row weight | **0.768×** [0.764, 0.771] | 0.615× [0.613, 0.620] | 0.492× [0.489, 0.496] |

The all-30 point is
`exp((13 × ln(G13) + 17 × ln(G17)) / 30)`. Its 10,000-sample descriptive
bootstrap shares resampled repetition indices within a suite and resamples the
normal and hostile captures independently. It is not a hypothesis test and does
not estimate machine-to-machine variability.

Normal has 29 / 39 point and Bonferroni exact-sign wins across all competitors;
hostile has 36 / 51. Against Node alone, Zipp has 17 / 30 point and exact-sign
wins. The literal all-row target remains false.

## Current Node gaps

| Row | Zipp / Node | Descriptive 95% interval |
|---|---:|---:|
| allocation-survival | **1.772×** | [1.703, 1.804] |
| warm-router | **1.674×** | [1.645, 1.707] |
| reactish-reconcile | **1.646×** | [1.630, 1.710] |
| shapes-stable | **1.493×** | [1.445, 1.517] |
| shapes-megamorphic | **1.484×** | [1.452, 1.519] |
| calls-closures | **1.307×** | [1.283, 1.313] |
| async-promise-chain | **1.232×** | [1.218, 1.247] |
| async-lived | **1.082×** | [1.078, 1.133] |
| json-large | **1.051×** | [1.019, 1.074] |
| sparse-array | **1.050×** | [1.022, 1.061] |
| regex-log-scan | **1.017×** | [0.983, 1.079] |
| bytecode-vm | **1.014×** | [0.984, 1.021] |
| npm-nanoid | **1.000×** | [0.992, 1.026] |

NanoID, bytecode-vm, and regex are point gaps with descriptive intervals that
cross parity. Do not describe them as supported regressions, but do keep them in
the literal point-estimate inventory.

## Verification completed

- `cargo test --workspace --release`
- focused protected-return, append-cursor, poly-FID, Tier-C object-literal, and
  instruction-use tests
- `cargo check -p zipp-vm --no-default-features`
- `cargo check -p zipp-vm --no-default-features --features safe-sandbox`
- clean provenance-stamped PGO build from committed source
- complete normal and hostile Node/Bun/Deno/Zipp captures with exact output

## Highest-value next work

1. **Allocation survival (`1.772× Node`).** Separate promotion, survivor tracing,
   free-list, and pool-maintenance costs; the ephemeral row is already fast.
2. **Warm router and React (`1.674×` / `1.646×`).** Re-profile B255 and split
   call dispatch from property/string/allocation costs before designing a lane.
3. **Stable and megamorphic shapes (`1.493×` / `1.484×`).** Their similar gaps
   make a purely polymorphic-IC explanation unlikely; compare counters directly.
4. **Closure calls and async (`1.307×`, `1.232×`, `1.082×`).** Price one
   mechanism at a time and require both full safety suites after each candidate.
5. **Near-parity rows.** Only pursue JSON, sparse, regex, bytecode-vm, or NanoID
   when a cheap isolated term is visible; avoid trading away larger wins.

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
