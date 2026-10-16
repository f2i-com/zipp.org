# Benchmark history

Condensed record of the routine benchmark captures that used to sit in
`bench/` and `bench/hostile/` as individual JSON files (276 normal-suite
files and 149 hostile-suite files, 2026-07-28 to 2026-09-11). Each file is one
row of a CSV here; the raw captures were deleted on 2026-10-17 to tidy the
tree. The current canonical captures stay beside their suites (see
[`bench/README.md`](../README.md#publication-rules)).

| File | One row per | Columns |
|---|---|---|
| [`results.csv`](results.csv) | normal-suite capture (`tools/bench.py`) | date, commit, kind, engines, baseline, metric, `ALL_CORRECT`, geomean paired ratio with its 95% interval, and each of the 13 rows' paired ratio |
| [`hostile_results.csv`](hostile_results.csv) | hostile-suite capture (`tools/bench_hostile.py`, or `tools/bench.py` on hostile cases) | date, commit, engines, cold geomean, and every case's cold paired ratio as `case=ratio;...` |
| [`averages.csv`](averages.csv) | month and kind (normal suite) | runs, geomean of the captures' geomeans, best and worst |

A ratio is the measured engine (or the A/B's new binary) over the baseline
(Node, or the old binary): below 1.0 is faster. `vs-node` rows are
cross-engine captures with Node as the baseline; `ab` rows compare two Zipp
binaries. Many A/Bs were filtered diagnostics on a few rows, so their geomeans
are not comparable with each other or with a full-suite capture; the month
averages only show the trend. Git history keeps every original file.

## Normal suite, by month

| Month | Kind | Runs | Geomean of geomeans | Best | Worst |
|---|---|---:|---:|---:|---:|
| undated | ab | 6 | 0.9852 | 0.9608 | 1.0027 |
| 2026-07 | ab | 48 | 0.9891 | 0.8506 | 1.0107 |
| 2026-07 | vs-node | 12 | 2.1887 | 1.8626 | 6.4230 |
| 2026-08 | ab | 157 | 0.9176 | 0.0137 | 1.4341 |
| 2026-08 | vs-node | 43 | 1.0432 | 0.5728 | 2.2361 |
| 2026-09 | ab | 1 | 0.9908 | 0.9908 | 0.9908 |
| 2026-09 | vs-node | 4 | 0.6340 | 0.6136 | 0.6766 |

## Hostile suite, by month

| Month | Kind | Runs | Geomean of geomeans | Best | Worst |
|---|---|---:|---:|---:|---:|
| 2026-08 | ab | 122 | 1.1280 | 0.2212 | 7.0083 |
| 2026-08 | vs-node | 20 | 1.0106 | 0.8662 | 1.2866 |
| 2026-09 | vs-node | 5 | 0.8613 | 0.8244 | 0.9475 |
