# Zipp performance handoff

This is the current continuation note. Historical snapshots through B252 are
archived in [`docs/archive/HANDOFF-through-B252.md`](docs/archive/HANDOFF-through-B252.md).
The current roadmap is [`PERF_ROADMAP.md`](PERF_ROADMAP.md); the full B001–B252
experiment ledger is preserved separately in
[`docs/archive/PERF_LEDGER-B001-B252.md`](docs/archive/PERF_LEDGER-B001-B252.md).

## Current engine baseline

The reviewed engine baseline is `2869e91` on `main`, pushed to `origin/main`.
The clean release binary frozen for the final development gates is:

```text
target/bench-binaries/zipp-b254-final-2869e91.exe
commit  2869e9195a18d383ca020c79fbc2668c3bdbe13d
sha256 bf85742b29500e1623f4ccaec1b56cae37de10084ef9ade1b3ad119f5ef85952
dirty  false
```

The latest small commits are:

| Commit | Result |
|---|---|
| `ff1c737` | B253: memoize the stable pinned-ASCII suffix between guarded integer-concat memos. |
| `4ff3bdf` | B254: serve `.length` for a proven pinned flat-ASCII call result directly from its immutable MEM snapshot. |
| `6f945ce` | Safety hardening: identity IC empty ways use an impossible canonicalized-NaN marker, so raw numeric `+0` cannot match an empty way under feature off-switches. |
| `2869e91` | Safety hardening: suffix admission verifies both concat links have `dst == a == accumulator` and the trailing move consumes that accumulator. |

The two hardening commits deliberately claim no speed delta.

## Canonical public state

Do not replace the public engine ratios with development A/B results.

- Canonical normal capture: `bench/real13_0bff482_pgo_2026-08-30.json`
- Canonical hostile capture: `bench/hostile/head_clean_0bff482_pgo_2026-08-30.json`
- Zipp commit: `0bff482`, provenance-stamped PGO, `publishable:true`
- Normal retained-ten: `0.918× Node` [0.914, 0.922]
- Normal all-13: `0.635× Node` [0.633, 0.637]
- Hostile all-17 ordinary: `0.866177× Node` [0.858901, 0.870652]
- Hostile category-balanced: `0.906139× Node` [0.899437, 0.911396]
- Literal all-engine/all-row gate: false; 29/39 point and exact-sign wins
- Conformance: 95,939 / 95,942 required test262 executions

A fresh clean PGO Node/Bun/Deno/Zipp capture is required before any of those
engine-facing ratios changes.

## Latest development evidence

All results below are exact-output diagnostics with `publishable:false`.

### B253 — React-shaped stable suffix

On one feature-candidate binary, 32 balanced pairs of `reactish-reconcile`
measured:

```text
81.4764 ms -> 77.9637 ms
new / old  0.958636
95% CI     [0.949720, 0.965530]
strict     31 / 32 wins
```

The complete same-binary normal and hostile safety sweeps were neutral. The
mechanism is guarded by `ZIPP_NO_CONCAT_SUFFIX_MEMO=1`.

### B254 final — NanoID pinned length

Final reviewed same-binary latch, 32 balanced pairs:

```text
119.0435 ms -> 103.0110 ms
new / old    0.868869  (-13.11%)
95% CI       [0.860467, 0.873758]
strict       31 / 32 wins
```

Final versus B252, 32 balanced pairs:

```text
117.631 ms -> 102.625 ms
new / old   0.871516  (-12.85%)
95% CI      [0.866427, 0.881037]
strict      32 / 32 wins
```

Final versus the nearer frozen B253 feature build, complete hostile corpus,
32 balanced pairs:

| Metric | New / old | Descriptive 95% CI |
|---|---:|---:|
| all-17 cold geomean | **0.987230** | [0.984695, 0.991308] |
| category-balanced cold geomean | **0.982579** | [0.979742, 0.987197] |
| NanoID | **0.871198** | [0.866817, 0.877921] |
| React-shaped reconcile | **0.978798** | [0.974462, 0.991749] |

Every output was exact and no cold row had a supported regression. The runner
reported `analysis_complete:false` only because startup-adjusted work was
unavailable for the very short `allocation-ephemeral` case; its cold evidence
is intact. The old B253 comparator is a frozen dirty feature build, so this is
decision evidence rather than publication evidence.

The final normal all-13 safety sweep used 16 balanced pairs and was neutral:
`0.9950×` [0.9873, 1.0017], `ALL_CORRECT=1`.

The direct filtered Node diagnostic is also honest about what remains:

| Case | Final Zipp / Node | Status |
|---|---:|---|
| npm-nanoid | **1.169×** [1.160, 1.195] | Much improved from canonical 1.396×; still behind. |
| reactish-reconcile | **1.673×** [1.618, 1.715] | Major open target. |

## Verification completed

- `cargo test -p zipp-vm --release --test call_result_str_pin`: 8 / 8
- identity empty-marker release unit: 1 / 1
- concat suffix invariant release unit: 1 / 1
- `cargo test -p zipp-vm --release --test concat_chain`: 19 / 19
- default, `--no-default-features`, and `safe-sandbox` compile checks
- raw `+0` fallback under `ZIPP_NO_PINNED_STR_LEN=1`,
  `ZIPP_NO_QUICK_LEN=1`, and `ZIPP_NO_CALL_RESULT_STR_PIN=1`
- exact-output NanoID, full hostile, normal all-13, and filtered Node diagnostics

The repository-wide release gate and a new canonical PGO capture should be run
after the documentation cleanup and before changing public engine ratios.

## Highest-value next work

1. **React-shaped reconcile (`1.673× Node`).** Re-profile the post-B253 binary;
   do not reuse the pre-B253 attribution blindly. Focus on recursive framed
   calls, handler-bearing loop admission, repeated `typeof`/loose-null helpers,
   and allocation inside regions only after counters confirm the current split.
2. **NanoID (`1.169× Node`).** The `.length` term is substantially reduced.
   Re-profile `charCodeAt` and the outer-MEM/inner-INT composition boundary;
   price each term before adding a wider string shortcut.
3. **Canonical refresh.** Produce one clean provenance-stamped PGO
   Node/Bun/Deno/Zipp capture for both normal and hostile suites. This decides
   whether the public ratios move.
4. **Other canonical gaps.** Allocation survival, warm router,
   async-promise-chain, and shape cases remain above Node in the last clean
   public hostile table. Scout one row at a time and land only isolated wins.

## Commands for the next session

```powershell
git status --short --branch
git log -6 --oneline --decorate
cargo build -p zipp-cli --release
& target\release\zipp.exe --version --json
```

Run a normal binary A/B:

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

Run a filtered hostile engine diagnostic:

```powershell
python tools\bench_hostile.py `
  --cases <case> `
  --engines node,zipp `
  --zipp target\release\zipp.exe `
  --reps 16 `
  --json target\bench-results\focused-node.json
```

Build the publication PGO binary from an x64 Visual Studio Developer
PowerShell:

```powershell
& 'C:\Program Files\Git\bin\bash.exe' tools/pgo.sh
```

## Working rules

- Keep implementation, validation, docs, and cleanup in small commits; push
  each verified win.
- Prefer a same-binary off-switch A/B for attribution, then compare frozen
  binaries for layout/generalisation.
- Cold time is the gate. Treat adjusted time as diagnostic on short rows.
- Preserve exact output and empty health/correctness/drift failure lists.
- Keep routine artifacts under ignored `target/bench-results/`.
- Never promote a dirty, filtered, non-PGO, or incomplete-engine artifact as a
  public score.
- Record neutral and refuted ideas in the roadmap so they are not repeated.
