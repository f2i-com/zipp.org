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
[`head_clean_0bff482_pgo_2026-08-30.json`](head_clean_0bff482_pgo_2026-08-30.json):

| Metric | Zipp / Node | 95% descriptive interval |
|---|---:|---:|
| ordinary cold geomean | **0.866177×** | [0.858901, 0.870652] |
| category-balanced cold geomean | **0.906139×** | [0.899437, 0.911396] |

It uses commit `0bff482`, a provenance-stamped PGO executable, the complete
manifest, 15 counterbalanced repetitions, exact output on all 17 cases, and
`publishable:true`.

The clean capture is an aggregate win, not all-row parity. Its largest open
Node ratios include allocation survival **1.705×**, React reconcile **1.682×**,
and warm router **1.616×**; NanoID was **1.396×**. The root README keeps the
literal every-row gate false.

### Post-capture development evidence

B253 and B254 are not folded into the canonical table. Their focused artifacts
are diagnostic and `publishable:false`:

- B253 stable concat suffix memo: React reconcile **−4.14%**, ratio 0.958636
  [0.949720, 0.965530], 31 / 32 same-binary wins.
- B254 final pinned string length: NanoID **−13.11%**, ratio 0.868869
  [0.860467, 0.873758], 31 / 32 same-binary wins.
- Final B254 versus the nearer frozen B253 feature build: all-17 ordinary
  **0.987230×** [0.984695, 0.991308], category-balanced **0.982579×**
  [0.979742, 0.987197], with exact output and no supported cold regression.
- A fresh filtered Node diagnostic puts final NanoID at **1.169× Node** and
  React at **1.673× Node**. Both remain open.

A new clean full PGO engine capture is required before the public table moves.

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
`1.50×`. The runner reports the evidence; it does not claim the current tree
meets this gate.

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
