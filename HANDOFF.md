# Zipp performance handoff

This is the current continuation note. Historical snapshots through B252 are
archived in [`docs/archive/HANDOFF-through-B252.md`](docs/archive/HANDOFF-through-B252.md),
and the B001–B252 experiment ledger is preserved in
[`docs/archive/PERF_LEDGER-B001-B252.md`](docs/archive/PERF_LEDGER-B001-B252.md).

## Current engine baseline

The reviewed engine source is commit
`21288c1219b06f1c4c30128b5eb9beb02a996503` on `main`. It lands:

- stable paired-`typeof` fusion (`ZIPP_NO_TYPEOF_SAME=1`);
- call-free Tier-C loose-null checks
  (`ZIPP_NO_TIERC_LOOSE_NULL_INLINE=1`);
- polymorphic-function-id Cross3 routing (`ZIPP_NO_CROSS3_POLY_FID=1`);
- a deferred flat-ASCII append cursor (`ZIPP_NO_STR_APPEND_CURSOR=1`); and
- correct protected returns through `finally`, with new parity coverage.

The clean publication executable is:

```text
target/canonical-21288c1/zipp-21288c1-pgo.exe
commit  21288c1219b06f1c4c30128b5eb9beb02a996503
sha256 c2ddb9e6562edd165310362c918670b4a1539385b0912924aef1fbf4fb6a3cb5
profile fbe1699292bc6ff3c7fd5c4609dee59a648c3213e375dd339dd73dc5ffe91743
dirty  false
```

## Canonical public state

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
