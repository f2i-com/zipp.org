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

- `bench/real13_b65aa353_pgo_2026-09-02.json`
- `bench/hostile/head_clean_b65aa353_pgo_2026-09-02.json`

Both use Node v24.12.0, Bun 1.3.14, Deno 2.6.10, and Zipp 0.0.10. Both report
`publishable:true`, `ALL_CORRECT=1`, 15 complete counterbalanced repetitions,
10,000 bootstrap samples, and empty provenance, publication, correctness,
health, source-drift, engine-drift, input-drift, and harness-drift failures.

| Corpus | vs Node | vs Bun | vs Deno |
|---|---:|---:|---:|
| retained ten | **0.903×** [0.899, 0.908] | 0.771× [0.767, 0.776] | 0.784× [0.777, 0.790] |
| diagnostics three | **0.187×** [0.183, 0.189] | 0.166× [0.165, 0.169] | 0.147× [0.144, 0.149] |
| normal all 13 | **0.628×** [0.624, 0.631] | 0.541× [0.539, 0.545] | 0.533× [0.528, 0.537] |
| hostile all 17 | **0.836×** [0.820, 0.843] | 0.662× [0.652, 0.667] | 0.434× [0.430, 0.438] |
| hostile category-balanced | **0.870×** [0.854, 0.876] | 0.678× [0.664, 0.683] | 0.447× [0.443, 0.451] |
| all 30, equal row weight | **0.739×** [0.730, 0.742] | 0.607× [0.601, 0.610] | 0.474× [0.471, 0.477] |

The all-30 point is
`exp((13 × ln(G13) + 17 × ln(G17)) / 30)`. Its 10,000-sample descriptive
bootstrap shares resampled repetition indices within a suite and resamples the
normal and hostile captures independently. It is not a hypothesis test and does
not estimate machine-to-machine variability.

Normal has 32 / 39 point and 31 / 39 Bonferroni exact-sign wins across all
competitors; hostile has 40 / 51 point and 36 / 51 exact-sign wins. Against
Node alone, Zipp has 20 / 30 point wins. The literal all-row target remains
false.

## Current Node gaps

| Row | Zipp / Node | Descriptive 95% interval |
|---|---:|---:|
| reactish-reconcile | **1.563×** | [1.533, 1.580] |
| allocation-survival | **1.548×** | [1.471, 1.606] |
| warm-router | **1.493×** | [1.240, 1.530] |
| shapes-megamorphic | **1.249×** | [1.231, 1.293] |
| shapes-stable | **1.239×** | [1.141, 1.251] |
| calls-closures | **1.177×** | [1.155, 1.206] |
| async-promise-chain | **1.137×** | [1.116, 1.162] |
| json-large | **1.042×** | [1.006, 1.049] |
| sparse-array | **1.011×** | [0.997, 1.020] |
| bytecode-vm | **1.008×** | [0.999, 1.028] |

sparse-array and bytecode-vm are point gaps whose descriptive intervals cross
parity; async-lived (0.984× [0.778, 1.033]) and npm-nanoid (0.991× [0.969,
1.028]) are point wins whose intervals also cross parity. Do not describe any
of the four as supported either way, but keep the two gaps in the literal
point-estimate inventory. regex-log-scan (0.932×) left the gap list with this
capture.

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

1. **React reconcile and warm router (`1.563×` / `1.493×`).** Re-profile at
   `b65aa353` and split call dispatch from property/string/allocation costs
   before designing a lane; the B256 maps in the session scratchpad are the
   starting point.
2. **Allocation survival (`1.548×`).** Separate promotion, survivor tracing,
   free-list, and pool-maintenance costs; the ephemeral row is already 0.36×.
3. **Stable and megamorphic shapes (`1.239×` / `1.249×`).** Their similar gaps
   make a purely polymorphic-IC explanation unlikely; compare counters directly.
4. **Closure calls and async promises (`1.177×`, `1.137×`).** Price one
   mechanism at a time and require both full safety suites after each candidate.
5. **Near-parity rows.** Only pursue JSON, sparse arrays, or bytecode-vm when a
   cheap isolated term is visible; avoid trading away larger wins.
6. **Register allocation follow-ups (B263).** The interval prover re-widens a
   compare-narrowed loop bound inside the body (widening applies at every
   merge after pass 8), so `o + 2` keeps its i53 guard; a widening restricted
   to loop heads would free r13/r14 on more INT-GPR regions.

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
