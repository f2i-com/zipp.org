# Zipp benchmark guide

Zipp uses several deliberately separate benchmark suites. They answer different
questions, so their scores are never folded together silently.

The public cross-engine result is the clean PGO capture linked from the
[root README](../README.md#performance-measured-honestly). Routine development
runs are diagnostics and belong under ignored `target/bench-results/`; only a
reviewed canonical artifact should be promoted into this directory.

## Quick start

Build the release CLI, create an ignored result directory, inspect the frozen
normal inputs, and list the hostile cases:

```powershell
cargo build --release -p zipp-cli
New-Item -ItemType Directory -Force target\bench-results | Out-Null
Get-ChildItem bench\real\*.js | Select-Object -ExpandProperty Name
python tools\bench_hostile.py --list
```

Run the retained suite against the installed engines:

```powershell
python tools\bench.py `
  --zipp target\release\zipp.exe `
  --engines node,bun,deno,zipp `
  --reps 15 `
  --bootstrap-samples 10000 `
  --json target\bench-results\real13.json
```

Run the full hostile corpus separately:

```powershell
python tools\bench_hostile.py `
  --zipp target\release\zipp.exe `
  --reps 15 `
  --bootstrap-samples 10000 `
  --json target\bench-results\hostile17.json
```

The runners reject dirty or non-HEAD Zipp binaries by default. Development
overrides such as `--allow-dirty-engine` preserve the reason in the artifact and
force `publishable:false`.

## Suite map

| Suite | Size | Purpose | Owner |
|---|---:|---|---|
| `bench/real` | 13 | Frozen cross-engine series: 10 retained headline rows plus 3 architecture diagnostics. | `tools/bench.py` |
| `bench/hostile` | 17 manifest cases / 22 code inputs | Generalisation stress: closures, shape churn, mixed locals, GC survival, async lifetimes, applications, modules, and vendored npm source. | `tools/bench_hostile.py` |
| Peak RSS | 6 generated shapes | Fresh-process retained-object memory A/B; no checked-in JS fixture. | `tools/bench_peak_rss.py` |
| `bench/scope` | 14 research fixtures | Dated scope/lowering investigations; useful for diagnosis, not a public score. | `bench/scope/run.py`, `sweep.py` |
| `bench/pgo-training` | 7 | Deterministic mechanism coverage used only to train PGO. These are never scored. | `tools/pgo.sh` |
| Root and `bench/long` micros | legacy | Small mechanism probes retained for regression research and PGO anti-leakage holdout coverage. | legacy shell runners |

The exact retained 10/3 split and hostile manifest membership are pinned by
benchmark-integrity tests. Changing either starts a new publication series.

## Focused development A/Bs

Compare two binaries on one or more retained rows with exact counterbalancing:

```powershell
python tools\bench.py `
  --ab target\bench-binaries\old.exe target\bench-binaries\new.exe `
  --ab-env - - `
  --benches json-large,markdown-render `
  --reps 16 `
  --seed 0x5a172026 `
  --json target\bench-results\json-markdown-ab.json `
  --allow-nonhead-engine
```

Use a filtered hostile run to inspect one family or case:

```powershell
python tools\bench_hostile.py --families object-shape --reps 16
python tools\bench_hostile.py --categories modules,npm --reps 16
python tools\bench_hostile.py --features closures --reps 16
python tools\bench_hostile.py --cases npm-nanoid,reactish-reconcile --reps 16
```

Filtered runs are diagnostic by design. For an optimization expected below
10%, start with at least 16 balanced pairs; use 32 or an independent rerun when
the decision is marginal or layout-sensitive. Do not run unrelated CPU-heavy
work at the same time.

## Reading a report

- **Cold time** includes process launch and is the public headline.
- **Adjusted time** subtracts the paired empty-program launch. It is a sustained
  work diagnostic, not the publication gate; very short rows can produce an
  unavailable or unstable adjusted ratio.
- A ratio below `1.0×` means the numerator engine or new binary was faster.
- The percentile-bootstrap interval is descriptive. A narrow interval wholly
  below `1.0` is stronger directional evidence than a point estimate alone.
- Exact sign tests count strict paired wins and provide the inferential public
  all-row gate. The full cross-engine table uses a Bonferroni family-wise
  threshold.
- `ALL_CORRECT=1` means stdout bytes matched exactly. There is no output
  normalization.

Hostile reports provide both an ordinary row geomean and a category-balanced
geomean. The latter gives each manifest category equal weight so a category
with several cases cannot dominate the summary.

## Publication rules

A canonical public capture must, at minimum:

1. use the complete Node/Bun/Deno/Zipp engine order with Node as the baseline;
2. use the complete frozen suite or complete canonical hostile manifest;
3. use at least 15 repetitions and 10,000 bootstrap samples;
4. use a clean, HEAD-matching, provenance-stamped PGO Zipp binary;
5. preserve exact engine, source, input, harness, environment, and process
   health metadata before and after measurement;
6. report exact output on every observation; and
7. remain independently reviewable before its claims reach the root README.

The benchmark runners stage reviewed inputs into private read-only trees and
fail closed on drift. A diagnostic override can permit measurement; it can
never turn a noncanonical run into publishable evidence.

The canonical public artifacts at the time of writing are:

- [`real13_0bff482_pgo_2026-08-30.json`](real13_0bff482_pgo_2026-08-30.json)
- [`hostile/head_clean_0bff482_pgo_2026-08-30.json`](hostile/head_clean_0bff482_pgo_2026-08-30.json)

## PGO boundary

Build the measured Windows PGO executable from an x64 Visual Studio Developer
PowerShell using native Git Bash:

```powershell
& 'C:\Program Files\Git\bin\bash.exe' tools/pgo.sh
```

The seven training programs are self-contained, bounded, deterministic, and
structurally checked against every scored input. Every tracked JavaScript or
module benchmark outside `bench/pgo-training` enters the publication holdout,
including legacy and research fixtures. Adding, deleting, or moving a tracked
benchmark source therefore changes the PGO recipe and must be intentional.
See [`pgo-training/README.md`](pgo-training/README.md) for the full anti-leakage
contract.

## Adding or removing a workload

Before adding a workload:

- state the question it answers and why an existing case cannot answer it;
- make it deterministic, host-free, bounded, and self-checking;
- avoid wall time, network, ambient randomness, and machine-specific output;
- list every module and non-code dependency explicitly in the hostile manifest;
- verify identical output on Node and Zipp; and
- update the suite-membership integrity tests.

Before removing one, prove it is unselected, unreferenced, superseded, or
duplicated. Remember that deletion changes the PGO holdout recipe even when no
runner selects the file. Preserve useful historical measurements in
[`docs/archive`](../docs/archive/README.md), but do not retain generated routine
results in the tracked tree.

## Artifact policy

| Artifact | Location | Tracked? |
|---|---|---|
| Routine A/B or filtered diagnostic | `target/bench-results/*.json` | No |
| Temporary frozen executable | `target/bench-binaries/` | No |
| Reviewed canonical capture | `bench/*.json` or `bench/hostile/*.json` | Yes, deliberately promoted |
| Manifest, input, provenance, or license | Beside its suite | Yes |
| Historical narrative | `docs/archive/` | Yes |

Do not commit terminal transcripts such as `results.txt`, duplicate fixture
copies, or nonpublishable exploratory captures that no document references.
The full experiment ledger records the conclusion and kill reason without
requiring every raw scratch artifact to live forever.

## Security boundary

Benchmark inputs are reviewed, trusted programs. The runners are measurement
tools, not sandboxes, and must not execute an unreviewed third-party corpus.
For hostile input, follow [`SECURITY.md`](../SECURITY.md) and use the dedicated
WASM/Worker or hardened native execution profile plus any required OS boundary.

## Integrity tests

The CI benchmark job checks manifests, staging, provenance, PGO separation, and
harness behavior; it does not use noisy shared runners to publish timed scores.
Run the local integrity suite after changing a harness, manifest, or workload:

```powershell
python -m unittest tools.test_bench
```

See [`PERF_ROADMAP.md`](../PERF_ROADMAP.md) for current optimization targets and
the [historical ledger](../docs/archive/README.md) for prior experiments.
