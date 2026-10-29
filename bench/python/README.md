# Python frontend against CPython

Captures of [`tools/python_bench.py`](../../tools/python_bench.py), which runs
every program under `tools/python_bench/{micro,macro}/` as a fresh process per
engine per repetition, interleaving the engines, and requires every run's
output to equal CPython's. Each program times its own workload in-process (the
work time the ratios use); wall time, with process start and compilation, is
recorded beside it.

- [`cpython_fcfb1934_2026-10-29.json`](cpython_fcfb1934_2026-10-29.json): the published
  `zipp-0.0.31-windows-x86_64-portable.exe` (source `fcfb1934`; the PGO
  benchmark build has no Python) against CPython 3.13.14, 15 repetitions.
  Zipp / CPython work time: 0.916× over the micro kernels, 1.591× over the
  application-shaped programs, 1.004× over all 36.
- [`cpython_nojit_fcfb1934_2026-10-29.json`](cpython_nojit_fcfb1934_2026-10-29.json): the same
  run with `ZIPP_NOJIT=1` set for the whole harness (CPython ignores it, so
  Zipp runs as a pure interpreter, like CPython). The artifact does not record
  the variable itself; this file name and this note do. 1.422× over all 36.
- [`cpython_da0fe6b1_2026-10-21.json`](cpython_da0fe6b1_2026-10-21.json) and
  [`cpython_nojit_da0fe6b1_2026-10-21.json`](cpython_nojit_da0fe6b1_2026-10-21.json): the same pair
  for the published 0.0.28 binary (source `da0fe6b1`, 21 October 2026):
  1.067× and 1.443× over all 36.
- `cpython_5f1c0578_2026-10-18.json` and `cpython_nojit_5f1c0578_2026-10-18.json`:
  the published 0.0.26 binary, 18 October 2026.

The 8 programs marked `zipp-only` (torch) have no CPython counterpart and are
left out of every ratio. The command, from the repository root at `v0.0.31`:

```powershell
py -3.13 tools\python_bench.py --zipp zipp-0.0.31-windows-x86_64-portable.exe --reps 15 --json <out>.json
$env:ZIPP_NOJIT = '1'
py -3.13 tools\python_bench.py --zipp zipp-0.0.31-windows-x86_64-portable.exe --reps 15 --json <out-nojit>.json
```
