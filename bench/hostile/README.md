# Hostile JavaScript benchmark corpus

This 17-case corpus asks how gracefully Zipp performs when JavaScript stops
looking like an ideal reducer: nested scopes, rotating closures, mixed local
types, shape churn, exceptions, retained allocation, async lifetimes,
applications, modules, and exact vendored package source.

It is deliberately separate from the frozen 10-row retained series. Hostile
results never rewrite that headline or silently change its historical meaning.
General benchmark rules and artifact policy live in [`bench/README.md`](../README.md).

## What it covers

Paired families compare a simpler baseline with a stressor:

| Family | Baseline | Stressor | Pressure |
|---|---|---|---|
| call shape | `calls-baseline` | `calls-closures` | nested functions, mutable captures, rotating closure targets |
| object shape | `shapes-stable` | `shapes-megamorphic` | sixteen layouts, transitions, dictionary behavior |
| local types | `types-stable` | `types-churn` | integer, double, string, object, array, boolean, and null locals |
| error control flow | `branch-control` | `throw-catch` | frequently thrown primitives and objects |
| object lifetime | `allocation-ephemeral` | `allocation-survival` | retained cohorts and GC pressure |
| async lifetime | `async-burst` | `async-lived` | mutable async closures and repeated `Promise.all` batches |

Standalone cases add a React-shaped reconciliation kernel, a warm router, a
long-running JavaScript bytecode VM, a multi-file ESM graph, and exact vendored
`nanoid@3.3.17/non-secure` source.

Every program is deterministic and host-free: no network, wall clock, or
ambient random source. Node and Zipp must emit byte-identical output on every
observation.

## Current canonical result

The public hostile result is the clean PGO capture
[`head_clean_21288c1_pgo_2026-08-30.json`](head_clean_21288c1_pgo_2026-08-30.json):

| Metric | Node | Bun | Deno |
|---|---:|---:|---:|
| ordinary cold geomean | **0.880739×** [0.874459, 0.886343] | **0.670217×** [0.664847, 0.676923] | **0.455125×** [0.450307, 0.460797] |
| category-balanced cold geomean | **0.913207×** [0.907014, 0.919216] | **0.685584×** [0.677898, 0.694681] | **0.467181×** [0.462099, 0.472631] |

It uses commit `21288c1`, a provenance-stamped PGO executable, the complete
manifest, 15 counterbalanced repetitions, exact output on all 17 cases, and
`publishable:true`. There are 36 / 51 point and Bonferroni exact-sign wins
across the three competitors; seven rows beat all three engines.

The clean capture is an aggregate win, not all-row parity. Against Node, eight
rows are point wins and nine are point gaps:

| Node point gap | Zipp / Node | 95% descriptive interval |
|---|---:|---:|
| calls-closures | **1.307092×** | [1.282812, 1.312543] |
| shapes-stable | **1.493168×** | [1.445199, 1.517249] |
| shapes-megamorphic | **1.483728×** | [1.451539, 1.518934] |
| allocation-survival | **1.771741×** | [1.702678, 1.804336] |
| async-lived | **1.082465×** | [1.078418, 1.132905] |
| reactish-reconcile | **1.645608×** | [1.629964, 1.709540] |
| warm-router | **1.673820×** | [1.644515, 1.707009] |
| bytecode-vm | **1.013865×** | [0.983551, 1.020502] |
| npm-nanoid | **1.000406×** | [0.991579, 1.025564] |

The last two are literal point gaps whose descriptive intervals cross parity;
they are not supported slowdowns. The root README therefore keeps the every-row
gate false instead of hiding these rows behind the aggregate.

## Run the corpus

From the repository root:

```powershell
cargo build --release -p zipp-cli
New-Item -ItemType Directory -Force target\bench-results | Out-Null
python tools\bench_hostile.py --list
python tools\bench_hostile.py `
  --zipp target\release\zipp.exe `
  --reps 15 `
  --bootstrap-samples 10000 `
  --json target\bench-results\hostile17.json
```

Focused diagnostics:

```powershell
python tools\bench_hostile.py --families object-shape --reps 16
python tools\bench_hostile.py --categories modules,npm --reps 16
python tools\bench_hostile.py --features closures --reps 16
python tools\bench_hostile.py --cases bytecode-vm,warm-router --reps 16
```

The runner balances engine order, pairs every full run with an empty launch,
hashes every declared input, compares stdout exactly, and rejects manifest,
input, source, harness, binary, environment, or process-health drift. A dirty or
non-HEAD override records its reason and cannot produce `publishable:true`.

Publication requires the canonical unfiltered manifest, exact engine order,
Node baseline, at least 15 repetitions and 10,000 bootstrap samples, a clean
provenance-stamped PGO Zipp binary, and tracked byte-clean inputs. Module imports
are not discovered automatically: every source and fixture in a graph must be
listed in the case's `inputs`.

## Read the metrics

- **Cold ratio** includes process launch and is the parity gate.
- **Adjusted ratio** subtracts the immediately paired empty launch. It is a
  sustained-work diagnostic; it can be unavailable on short cases.
- Ratios are Zipp / competitor. Lower is better; below `1.0×` is faster.
- The ordinary geomean weights every row equally.
- The category-balanced geomean weights every manifest category equally.
- Family degradation compares stressor/baseline ratios between engines. It
  describes the whole scenario, not one isolated language operation.

The aspirational corpus gate is a cold category-balanced geomean at or below
`1.05× Node`, no cold category above `1.15×`, and no individual cold row above
`1.50×`. The current capture clears the aggregate bound, but not the category or
row bounds: applications are **1.646×**, server **1.674×**, and objects
**1.488×** Node by category; allocation-survival (**1.772×**), warm-router
(**1.674×**), and reactish-reconcile (**1.646×**) exceed the individual-row cap.
The runner reports the evidence; it does not claim the current tree meets this
gate.

## Manifest and vendored inputs

[`manifest.json`](manifest.json) is the suite contract. Each case declares its
id, entry, goal, category, family, variant, required features, description,
timeout, and complete input list. Changing case membership or taxonomy starts a
new publication series and requires benchmark-integrity test updates.

The npm case vendors exact `nanoid@3.3.17/non-secure` ESM source and its MIT
license. `vendor/nanoid-3.3.17/PROVENANCE.json` records registry integrity and
source hashes. The driver supplies deterministic randomness and a checksum; it
does not rewrite package source for Zipp.

## Security boundary

These are reviewed, trusted programs. The benchmark runner is not a sandbox and
must not execute an unreviewed package or corpus. Use the dedicated WASM/Worker
or hardened native profile described in [`SECURITY.md`](../../SECURITY.md), plus
an OS boundary when required by the threat model.

## History

Wave 30 and Wave 39 were valuable development checkpoints, but their dirty
1.2866×–3.1023×-era scores are not current. Their narrative is preserved at
[`docs/archive/HOSTILE-W30-W39.md`](../../docs/archive/HOSTILE-W30-W39.md).
The complete experiment history through B252 is in the
[`performance ledger`](../../docs/archive/PERF_LEDGER-B001-B252.md).
