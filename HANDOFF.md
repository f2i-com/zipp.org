# Zipp performance handoff

This is the current continuation note. Historical snapshots through B252 are
archived in [`docs/archive/HANDOFF-through-B252.md`](docs/archive/HANDOFF-through-B252.md),
and the B001–B252 experiment ledger is preserved in
[`docs/archive/PERF_LEDGER-B001-B252.md`](docs/archive/PERF_LEDGER-B001-B252.md).

## Current engine baseline

The v0.0.5 performance source is commit
`7cb72106c9591613b170ba057d3c07e1cee01379` on `main`. It lands the interpreter
fast paths and focused parity tests from the v0.0.5 campaign, the bounded
one-slot split-receiver bridge for the native integer tier, the persistent-WASM
fingerprint API, and fail-closed heap accounting for private side tables.

The clean, release-default comparison executable is:

```text
target/comparison/build/zipp-native-v005-7cb72106c959/release/zipp.exe
commit  7cb72106c9591613b170ba057d3c07e1cee01379
sha256 b36850e162f7a9d2221ac33e888c8e5ea3d7ee85e3833e6f2e5ad9a73c0c23be
profile release, opt-level 3, default features, no PGO or ad-hoc RUSTFLAGS
dirty  false
```

## v0.0.5 QuickJS-NG / Boa release diagnostic

All source output matched before timing. Six complete counterbalanced rounds and
10,000 bootstrap samples produced:

| Diagnostic | Zipp / competitor | 95% interval | point wins |
|---|---:|---:|---:|
| native interpreter real13 / QuickJS-NG | **0.6413×** | [0.6386, 0.6452] | 12 / 13 |
| native interpreter micro5 / QuickJS-NG | **0.8556×** | [0.8405, 0.8761] | 5 / 5 |
| native interpreter micro5 / Boa | **0.2539×** | [0.2501, 0.2590] | 5 / 5 |
| WASM adjusted execution / QuickJS-NG | **2.1074×** | descriptive | 0 / 5 |
| WASM adjusted execution / Boa | **0.2274×** | descriptive | 5 / 5 |

The native sparse-array point median is the only QuickJS-NG loss at 1.0099×;
do not turn the aggregate win into an every-row claim. In the historical v0.0.5
browser-WASM release capture, Zipp's 5,595,833-byte raw module (1,254,075
Brotli-11) sat between QuickJS-NG's 1,528,293-byte reactor (417,087 Brotli-11)
and Boa's 21,296,176-byte module (5,484,164 Brotli-11), while QuickJS-NG
measured about 2.1× faster on the five-row execution diagnostic. The current
development status is recorded separately below.

Raw release evidence is intentionally ignored under
`target/comparison/results/`: `native-real13-v005-qjsng-clean-6.json`,
`native-micro5-v005-clean-6.json`, and `wasm-v005-clean-6.json`. Reproduction
commands and artifact hashes are in
[`bench/comparison/README.md`](bench/comparison/README.md).

### Current post-v0.0.5 WASM development snapshot

The current uncommitted development artifact is
`target/comparison/candidates/wasm-kernels-final-commuted-20260901-web/zipp_wasm_bg.wasm`.
It is identified by SHA-256
`caf26214ffca1407fba46f3bb304e4bb78ebb01b12898bf4132fc4e7a21f05f3`:
5,480,576 bytes raw, 1,826,113 gzip-9, and 1,233,843 Brotli-11. It does not
replace the v0.0.5 release or checked-in landing module.

The 48-repetition result is
`target/comparison/results/wasm-kernels-final-48.json`, with 12 cold-compile and
12 startup samples and seed `1511464998`. Persistent work medians were:

| Workload | Zipp | QuickJS-NG | Zipp / QuickJS-NG |
|---|---:|---:|---:|
| fib-recursive | 1.1180 ms | 58.1861 ms | `0.0192142110×` |
| loop-arithmetic | 1.1003 ms | 27.6866 ms | `0.0397412467×` |
| array-hof | 5.37885 ms | 9.39055 ms | `0.5727939258×` |
| object-properties | 1.05105 ms | 16.83035 ms | `0.0624496817×` |
| sort-comparator | 5.3476 ms | 18.4204 ms | `0.2903085709×` |

The persistent-total geomean is `0.0954663913×` QuickJS-NG and
`0.0105336875×` Boa, with Zipp ahead on all five persistent point medians
against both. Cold compile / instantiation / module-ready medians were
4.70735 / 0.41760 / 5.12495 ms for Zipp, 1.51085 / 1.58345 / 3.09430 ms for
QuickJS-NG, and 8.91050 / 2.57755 / 11.48805 ms for Boa.

Do not headline the computed `0.0245653793×` adjusted geomean. Array HOF and
sort have 48/48 positive work-minus-control samples and adjusted ratios of
`0.4681596596×` and `0.2316111384×`; fib, loop, and object properties are at
or below subtraction resolution, with only 25/48, 24/48, and 24/48 positive
Zipp samples and a negative object adjusted median. This is an adapter-inclusive
five-workload diagnostic, not a universal interpreter-core or all-JavaScript
ranking. QuickJS-NG remains smaller: Zipp is `3.586×` its raw reactor and
`2.958×` its Brotli payload; Boa remains larger.

#### Prior development baseline (preserved)

The immediately preceding snapshot was 5,462,006 bytes raw, 1,818,299 gzip-9,
and 1,230,296 Brotli-11, SHA-256
`8cf6e8207d1852cfa31c6e38d3b8a60bdec6e4894a65c1d08f39b244c849903b`.
Its 48-repetition result,
`target/comparison/results/wasm-meter-countdown-final-48.json`, measured
`1.8115847493×` QuickJS-NG persistent and `1.7725499353×` adjusted, and
`0.1898384829×` Boa persistent and `0.1834251840×` adjusted. QuickJS-NG led
all five adjusted point medians in that prior capture. The current kernels cost
18,570 raw, 7,814 gzip, and 3,547 Brotli bytes relative to this baseline. Exact
commands, pinned artifacts, and interface caveats are in the comparison README;
no source commit is claimed for either working-tree snapshot.

### v0.0.5 release validation

Focused release gates were green: safe-sandbox library 463 passed / 1 ignored,
benchmark-tool tests 164 passed / 2 skipped, bool-home 11 passed / 1 worker
ignored, split-receiver 7 / 7, isolated WASM Rust host 4 / 4, Node host contract
137 / 137, and syntax corpus 23 / 23.

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
