# Python frontend against CPython

Captures of [`tools/python_bench.py`](../../tools/python_bench.py), which runs
every program under `tools/python_bench/{micro,macro}/` as a fresh process per
engine per repetition, interleaving the engines, and requires every run's
output to equal CPython's. Each program times its own workload in-process (the
work time the ratios use); wall time, with process start and compilation, is
recorded beside it.

- [`cpython_5f1c0578_2026-10-18.json`](cpython_5f1c0578_2026-10-18.json): the published
  `zipp-0.0.26-windows-x86_64-portable.exe` (source `5f1c0578`; the PGO
  benchmark build has no Python) against CPython 3.13.14, 15 repetitions.
  Zipp / CPython work time: 0.966× over the micro kernels, 1.664× over the
  application-shaped programs, 1.057× over all 36.
- [`cpython_nojit_5f1c0578_2026-10-18.json`](cpython_nojit_5f1c0578_2026-10-18.json): the same
  run with `ZIPP_NOJIT=1` set for the whole harness (CPython ignores it, so
  Zipp runs as a pure interpreter, like CPython). The artifact does not record
  the variable itself; this file name and this note do. 1.506× over all 36.

The 8 programs marked `zipp-only` (torch) have no CPython counterpart and are
left out of every ratio. The command, from the repository root at `v0.0.26`:

```powershell
py -3.13 tools\python_bench.py --zipp zipp-0.0.26-windows-x86_64-portable.exe --reps 15 --json <out>.json
$env:ZIPP_NOJIT = '1'
py -3.13 tools\python_bench.py --zipp zipp-0.0.26-windows-x86_64-portable.exe --reps 15 --json <out-nojit>.json
```
