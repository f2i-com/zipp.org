# Python frontend performance suite

Fixed workloads for measuring `zipp py` before and after an optimization,
against CPython and against another Zipp build.

- `micro/`: one program per hot construct from the performance map, such as a
  range loop, float arithmetic, attribute access, method and keyword calls,
  `dict.get`, generators, `a, b = b, a`, f-strings, `hasattr` misses and
  raise/catch. Each is sized to take 0.2-1.0 s of work on the build used to
  calibrate the suite: zipp 0.0.18, release, not PGO, source 39304730 with a
  dirty tree.
- `macro/`: realistic programs:
  - recursive `fib`
  - n-body
  - word count
  - an OO bank simulation
  - JSON round trip
  - sorting and `heapq`
  - torch workloads, all zipp-only: an MLP train step, a 128x128 matmul, a
    256x256 matmul plus an [8, 64, 64] bmm, a small CNN train step, an
    LSTM+GRU train step, a transformer block train step (AdamW), a loop of
    tiny autograd ops, and a shuffled DataLoader epoch over a TensorDataset

## Running

```text
py -3.13 tools/python_bench.py --zipp target/release/zipp.exe                  # zipp vs CPython, 5 reps
py -3.13 tools/python_bench.py --zipp old.exe --zipp-b new.exe --reps 7        # A/B plus CPython
py -3.13 tools/python_bench.py --zipp zipp.exe --zipp-b new.exe --no-cpython   # A/B only
py -3.13 tools/python_bench.py --zipp zipp.exe --group micro --only call_ --reps 3
py -3.13 -m unittest tools.test_python_bench                                   # harness unit tests
```

| Option | Meaning |
|---|---|
| `--cpython CMD` | Interpreter command. Defaults to `py -3.13` on Windows and `python3.13` elsewhere. |
| `--timeout S` | Per-run limit. A timed-out run's process tree is killed. |
| `--metric wall` | Shows wall time in the table instead of work time. |
| `--json PATH` | Moves the report. By default it goes to `target/bench-results/python-bench-<utc>.json`, which is ignored by git. |

The exit status is 1 in any of these cases:
- a run fails or times out
- a checksum differs
- a binary or benchmark file changes during the run

The exit status is 2 for usage errors.

## Methodology

**Work time, not wall time.** Each program times its own workload with
`time.perf_counter()` and prints `@bench-time SECONDS` on stderr. Ratios and
geomeans use that work time by default. Wall time is still recorded for every
run, and the report keeps the difference as `overhead_s_median`.

Wall time would distort the ratios for two reasons:
- It includes process start and, for Zipp, the ~50 ms runtime compile.
- On this Windows host `py -3.13` needs about 70-130 ms just to reach the script.

A micro program that does 10 ms of work under CPython would therefore show a
ratio several times too small.

Zipp's `perf_counter` is built on `Date.now()`, so it has 1 ms resolution.
That is below 0.5% of a 0.2 s workload.

**Checksums.** Stdout is a deterministic checksum. It must match byte for byte
(after CRLF normalization) on every run of every engine:
- Ordinary programs are checked against CPython's first successful run.
- zipp-only programs are checked against the first run of the `--zipp` (A) binary.
- Under `--no-cpython`, every program is checked against A.

Timings from a benchmark whose checksum failed are dropped from the ratios and
geomeans, and the table marks the row `INVALID`.

**Interleaving.** Every repetition runs every program on every engine, so slow
drift on the host (thermal changes, background builds) lands on all engines
alike rather than on whichever ran last.
- The program order rotates by one position per repetition.
- Each program's engine order also rotates per repetition. With as many
  repetitions as engines, every engine runs first once for every program.

The report records the exact schedule.

**Working directory.** Each run starts in its group directory (`micro/` or
`macro/`). `zipp py` loads the current directory into its virtual filesystem
when the script is inside it, and the repository root would add seconds. Keep
the group directories free of anything but benchmark `.py` files.

**Noisy hosts.** Medians are the headline and minima are shown beside them.
- Compare only runs from the same host and session.
- On a busy machine, use `--reps 7` or more and prefer the A/B ratio
  (`zipp-b/zipp`), which shares the noise, over absolute times.
- CPython micro workloads last only 3-30 ms, so their medians are the noisiest
  numbers in the table.
- A 3-5% move on a single row is within noise here; a geomean move is more
  trustworthy.

**zipp-only.** A program whose leading comment block contains
`# zipp-bench: zipp-only` never runs under CPython. The host CPython has no
torch; Zipp has its bundled subset.
- These rows get only Zipp columns and the `zipp-b/zipp` ratio.
- They are excluded from `zipp/cpython` geomeans.
- Their inputs come from `arange` formulas, not the RNG.
- Their printed values are rounded, so a kernel change that only reorders
  float32 accumulation still validates, while a wrong result does not.

**Provenance.** Each report records:
- the harness sha256
- every benchmark's sha256, rechecked after the run
- each Zipp binary's sha256, `--version` output and `--version --json` build identity
- CPython's `sys.version` and executable
- the host platform
- the workspace commit
- start and finish timestamps from the host clock
- every raw observation, including stdout

## Writing a benchmark

Copy an existing file. Its `print("<name>", ...)` line is the checksum and
`@bench-time` goes to stderr. The program must run identically under CPython 3.13.

**Keep the output deterministic across engines:**
- Never print `hash()` values. CPython randomizes string hashes.
- Never print set-of-`str` iteration order; sort first.
- Generate data with an explicit LCG, not `random`.
- Keep float checksums to `+ - * /` and `math.sqrt`, printed at fixed
  precision. A fractional `**` goes through the platform `pow`.
- Avoid integral floats in `json.dumps` output. Zipp 0.0.18 prints `1.0` as
  `1`; see `macro/json_roundtrip.py`.

**Keep the timing honest:**
- Do the work inside functions, as real code does. Module-level loops measure
  global-variable access instead.
- Size the workload to 0.2-1.0 s of Zipp work time.
- Name the file for the construct and do not rename it later: the stem is the
  key that reports are compared by.
