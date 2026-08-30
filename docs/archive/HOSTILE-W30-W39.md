# Hostile JavaScript benchmark corpus

> **Archived Wave 30 / Wave 39 snapshot.** Its dirty development scores are not
> current. Use [`bench/hostile/README.md`](../../bench/hostile/README.md) for
> the canonical capture and current runner instructions.

This 17-case corpus measures how gracefully Zipp degrades when JavaScript stops
looking like an ideal microbenchmark. It is deliberately separate from the
frozen `bench/real` retained-ten series, so adding or tuning these cases does
not alter the historical performance claim.

The paired families compare a simpler baseline scenario with a messier one:

| Family | Baseline | Stressor | Main pressure |
| --- | --- | --- | --- |
| `call-shape` | `calls-baseline` | `calls-closures` | nested functions, mutable captures, rotating closure targets |
| `object-shape` | `shapes-stable` | `shapes-megamorphic` | sixteen layouts, transitions, dictionary mode |
| `local-types` | `types-stable` | `types-churn` | integer/double/string/object/array/boolean/null locals |
| `error-control-flow` | `branch-control` | `throw-catch` | frequently thrown primitives and objects |
| `object-lifetime` | `allocation-ephemeral` | `allocation-survival` | retained cohorts and GC pressure |
| `async-lifetime` | `async-burst` | `async-lived` | mutable async closures and repeated `Promise.all` batches |

The call, shape, type, and error pairs are deliberately close controls. The
allocation and async pairs are composite application scenarios: they vary
structure and operations as well as lifetime, so their degradation ratios
describe the whole scenario rather than isolating one causal variable.

Standalone cases cover a React-shaped reconciliation kernel, a warm router, a
long-running JavaScript bytecode interpreter, a multi-file ESM graph, and exact
vendored npm source. Every workload is host-free and deterministic: it reads no
network, wall clock, or ambient random source, and Node and Zipp must emit
exactly the same output on every repetition.

## Run it

Build the release CLI first, then list or run the corpus from the repository
root:

```powershell
cargo build --release -p zipp-cli
python tools/bench_hostile.py --list
python tools/bench_hostile.py --reps 15 --json bench/hostile/results_YYYY-MM-DD.json
```

Useful focused runs include:

```powershell
python tools/bench_hostile.py --families object-shape --reps 15
python tools/bench_hostile.py --categories modules,npm --reps 15
python tools/bench_hostile.py --features closures --reps 15
python tools/bench_hostile.py --cases bytecode-vm,warm-router --reps 15
```

The harness refuses a headline run when the release binary reports dirty or
non-HEAD source. During development, `--allow-dirty-engine` permits a directional
run while recording `publishable:false`; never promote that artifact as a
committed result.

The harness balances engine order exactly for even repetition counts and within
one run for odd counts, records raw observations and hashes for every input
declared in the manifest, checks stdout exactly, rejects mid-run
input/manifest/harness/engine drift, and atomically publishes a fully written
JSON file without overwriting an existing result by default. It hashes both
`tools/bench_hostile.py` and the imported `tools/bench.py`; a diagnostic
override preserves every provenance reason and cannot make the artifact
publishable. `publishable:true` additionally requires the canonical default
manifest, the full unfiltered corpus, at least 15 repetitions, and at least
10,000 bootstrap samples. The manifest, both Python harnesses, and every declared
input must also be tracked and byte-clean against `HEAD`; stable local edits are
not publishable merely because they did not change during the run. The runner
compares actual working content with `HEAD` blobs before and after measurement,
independently of Git's `assume-unchanged`/`skip-worktree` hints. Any inherited
Zipp, allocator, or JS-runtime control makes a run diagnostic-only, even when its
value can be recorded safely. Environment metadata retains values only for an
explicit allowlist of audited numeric/boolean controls; unknown keys,
credentials, paths, and arbitrary
runtime values are redacted before JSON serialization. Module imports are not discovered by the runner: each reviewed
module graph must explicitly list every dependency in `inputs`. The
path-confinement check applies to those declared files, which is another reason
this runner is only for reviewed, trusted programs.

## Read the metrics

`cold ratio` is Zipp process time divided by Node process time. For each
observation, `adjusted ratio` subtracts the immediately paired empty-program
launch from that full run before ratios and medians are calculated. Lower is
better; below `1.0x` means Zipp was faster.

Family degradation is a ratio of ratios: stressor/baseline for each engine, then
Zipp degradation divided by Node degradation. A relative value above `1.0x`
means the whole messy scenario hurt Zipp more than it hurt Node. These are close
scenario controls, not operation-matched microbenchmarks: several stressors also
change iteration mix, selection work, or object construction, so the figures do
not isolate one causal language feature. This is still the primary "graceful
degradation" signal.

The category-balanced geomean gives every manifest category equal weight; five
currently contain a single case. The category names and membership are frozen by
the versioned canonical manifest for a publication series. Changing that
taxonomy starts a new series rather than silently reweighting the old one.

The async and warm-router programs are long-lived *within one engine process*,
including explicit warmup where applicable. The harness still launches a fresh
process for each observation; these are not persistent-host request-latency
measurements.

An aspirational parity gate for this evolving corpus is a **cold**,
category-balanced geomean at or below `1.05x` Node, no cold category above
`1.15x`, and no individual cold row above `1.50x`. Adjusted results remain a
sustained-work diagnostic, but many current Node kernels have only 2–20 ms of
work after launch subtraction, so they are too noise-sensitive to gate parity
until those cases are scaled or their confidence intervals stabilize. The
harness reports evidence; it does not claim that Zipp currently meets the gate.

## Wave 39 development checkpoint

The current full-corpus development result is
[`w39_final_cleanenv_dirty_2026-08-25.json`](../../bench/hostile/w39_final_cleanenv_dirty_2026-08-25.json):
15 counterbalanced repetitions, 10,000 bootstrap samples, no inherited
benchmark-control environment, exact output on all 17 cases, no process or
correctness failure, and no source/harness/input/engine drift. It correctly
records `publishable:false`: the engine and publication inputs are dirty or
untracked on top of `e166220`, so this is not a clean release publication.

The cold category-balanced Zipp/Node ratio is **1.3564×** (95% CI
**1.350–1.363**) and the ordinary row geomean is **1.2866×**
(**1.281–1.293**), down from Wave 30's 3.1023× / 2.7173×. Cold rows:

| case | Zipp / Node |
|---|---:|
| allocation-ephemeral | **0.368×** |
| module-hot-graph | **0.396×** |
| types-stable | **0.464×** |
| throw-catch | **0.527×** |
| async-burst | **0.635×** |
| bytecode-vm | **0.895×** |
| branch-control | **0.975×** |
| async-lived | **1.076×** |
| types-churn | **1.686×** |
| calls-closures | **1.976×** |
| npm-nanoid | **2.566×** |
| reactish-reconcile | **3.272×** |
| warm-router | **3.792×** |
| shapes-megamorphic | **3.848×** |
| shapes-stable | **3.871×** |
| allocation-survival | **4.623×** |

The corpus still fails its aspirational gate: objects, applications, server,
npm, and allocation category geomeans remain above 1.15×, and multiple rows
remain above 1.50×. Relative cold degradation is object shape **0.984×**,
calls **4.250×**, mixed locals **3.679×**, survivor lifetime **12.539×**,
async lifetime **1.695×**, and exceptions **0.526×**. This is the intended
answer to “how gracefully does Zipp degrade?”—shape churn is now graceful,
while survivor allocation, closure calls, and mixed locals are not.

Use cold results as the parity gate. Startup-adjusted work estimates remain
diagnostic because several Node intervals are short and launch subtraction is
noise-sensitive. See B153–B161 in `PERF_ROADMAP.md` and the Wave 39 snapshot in
`HANDOFF.md` for mechanisms, safety proofs, focused A/B evidence, and remaining
targets.

### Post-Wave-39 focused checkpoint

Two default-on changes cleared same-binary gates after the full Wave 39 capture,
so they are intentionally **not** folded into the table or suite aggregates
above. Direct, semantics-preserving `[[HomeObject]]` side-table storage moved
`allocation-survival` from 301 ms to 260 ms across 15 pairs (**14.1% faster**,
paired 95% CI 11.2%..15.5%, exact output). A live-guarded integer-only
same-prototype lexical-arrow cross-call descriptor measured 92.438 ms enabled
versus 97.340 ms disabled across 30 pairs (mean saving 4.836 ms, bootstrap 95%
CI 3.548..6.439 ms). A pointer-free object-literal transition cache was also
tested, but made stable and megamorphic shape rows 1.915x and 1.865x slower; it
was fully reverted. See B162-B164 in `PERF_ROADMAP.md`. A new full hostile run
is still required before updating any Node ratio or parity-gate claim.

## Historical Wave 30 directional baseline

The historical first seven-repetition dirty sweep produced exact output on all
17 cases and measured **3.0076× cold category-balanced / 2.6501× ordinary row
geomean**. It was not retained as publication evidence after a provenance defect
was found and corrected; those numbers remain context rather than the current
baseline.

The [final combined diagnostic](../../bench/hostile/w30_combined_dirty_2026-08-25.json) also uses
seven repetitions and records `ALL_CORRECT=1`, no health/output failure, and no
mid-run source, harness, input, or engine drift. Its engine was built from the
dirty Wave 30 tree, so the artifact correctly records `publishable:false`. Cold
results are **3.1023× Node category-balanced** and **2.7173× ordinary row
geomean**. This is the final Wave 30 development capture, not a clean release
publication and not Node parity.

Notable rows are exact vendored `nanoid` 10.237×, ephemeral allocation 7.870×,
mutable closures 7.576×, allocation survival 5.336×, the JavaScript bytecode VM
4.000×, and the ESM graph 3.914×. Stable numeric locals 0.497×, throw/catch
0.548×, and async burst 0.635× remain useful controls. Cold relative degradation
is calls 15.722×, mixed locals 7.941×, async lifetime 1.826×, object shape
1.542×, object lifetime 0.680×, and exceptions 0.171×. Adjusted values remain
diagnostic: launch subtraction is noisy on the shortest Node kernels, and a
non-positive paired work estimate makes the final adjusted suite aggregate
unavailable.

Wave 30 also corrected an accidental GC root: `closure_home` and
`closure_new_target` now behave as directed edges from reachable closures, with
the required old-to-young write barriers. On allocation survival this reduced
peak slots 81.4%, average live slots 88.6%, and GC time 45.6%; the workload now
passes the sandbox's 128 MiB approximate heap limit. A separate native shape-way
experiment measured about 10.6% faster on the stable-shape row but was fully
reverted after independent security review found VM-exotic shape collisions and
stale metadata/pointer paths. A zero-symbol audit confirmed that no experimental
unsafe shape-way code ships. See B146–B152 in `PERF_ROADMAP.md`; at Wave 30 the
next targets were a guarded closure lane and local-allocation SROA. Their later
work, results, and remaining gaps are recorded in B153–B161.

## Vendored package policy

The npm case vendors the exact `nanoid@3.3.17/non-secure` ESM source and its MIT
license. `vendor/nanoid-3.3.17/PROVENANCE.json` records the package tarball,
registry integrity, and source hashes. The adjacent driver supplies only a
deterministic `Math.random` implementation and checksum loop; the package source
is not rewritten for Zipp. Zipp now resolves ordinary `Math.random()` calls
dynamically, so replacing that property is observed rather than bypassed by a
syntax-only random opcode.

## Security boundary

These checked-in inputs are reviewed, trusted benchmark programs. The benchmark
runner is a measurement tool, not a sandbox, and must not be used to execute an
unreviewed third-party corpus. Run untrusted scripts through Zipp's sandboxed
execution path with explicit capabilities and resource limits instead. Even
that runner provides language/process/resource containment, not kernel or OS
isolation; see the [root security boundary](../../README.md#choose-the-right-execution-profile)
and add an external restricted account/container for genuinely hostile code.
