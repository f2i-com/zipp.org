#!/usr/bin/env python3
"""Benchmark harness for bench/real/*.js — paired, median-based, with raw samples.

Replaces `bench/run_real.sh` for anything where the answer matters. That script
takes best-of-5 with each engine run to completion in turn, discards stderr and
exit status, and — the part that actually misleads — strips non-ASCII bytes from
both outputs before comparing them, so its "output byte-identical to node" claim
was not true of any bench emitting non-ASCII.

The measurement problems are not cosmetic. Measured on this repo, back-to-back
runs of the SAME binary drift 3-10% (node's own map-set-heavy has ranged
609-966ms), and best-of-N reports the luckiest sample rather than the typical
one. A best-of-3 comparison credited one change with -2.0%; paired medians of 7
put the same change at -0.9%. Anything under a few percent needs this tool.

What it does differently:

  * PAIRED. One repetition runs every engine on the same benchmark back to back,
    then moves on. Machine drift (thermal, scheduler, another process) lands on
    all engines within a repetition instead of on whichever ran during it.
  * MEDIAN, with p10/p90 reported so the spread is visible rather than implied.
  * RAW SAMPLES kept, so a result can be re-analysed without re-running.
  * Output compared as EXACT BYTES, and a non-zero exit status is a failure
    rather than a silently-empty result.

Usage:
  python tools/bench.py                          # all engines, 7 reps
  python tools/bench.py --reps 11 --json r.json
  python tools/bench.py --ab old.exe new.exe     # A/B two zipp builds
  python tools/bench.py --benches json-large,markdown-render
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import time

BENCH_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "bench", "real")
BENCH_DIR = os.path.normpath(BENCH_DIR)


def discover_benches():
    return sorted(f[:-3] for f in os.listdir(BENCH_DIR) if f.endswith(".js"))


def run_once(cmd, path, env=None):
    """One timed run. Returns (seconds, stdout_bytes, returncode)."""
    e = dict(os.environ)
    if env:
        e.update(env)
    t0 = time.perf_counter()
    p = subprocess.run(cmd + [path], stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=e)
    dt = time.perf_counter() - t0
    return dt, p.stdout, p.returncode


def pct(xs, q):
    xs = sorted(xs)
    if len(xs) == 1:
        return xs[0]
    i = (len(xs) - 1) * q
    lo, hi = int(i), min(int(i) + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (i - lo)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--reps", type=int, default=7, help="repetitions (default 7)")
    ap.add_argument("--benches", help="comma-separated subset")
    ap.add_argument("--json", help="write raw samples here")
    ap.add_argument("--zipp", default=os.path.normpath(
        os.path.join(os.path.dirname(BENCH_DIR), "..", "target", "release", "zipp.exe")))
    ap.add_argument("--ab", nargs=2, metavar=("OLD", "NEW"),
                    help="compare two zipp builds instead of the engine field")
    ap.add_argument("--baseline", default="node",
                    help="engine the ratio column is against (default node)")
    args = ap.parse_args()

    benches = args.benches.split(",") if args.benches else discover_benches()

    # Engine table. A/B mode replaces it entirely: comparing two builds of the
    # same engine is the common case when optimising, and mixing in node/bun/deno
    # only adds runtime and noise to a question that does not involve them.
    if args.ab:
        engines = [("old", [args.ab[0], "js"]), ("new", [args.ab[1], "js"])]
        baseline = "old"
    else:
        engines = []
        for name, cmd in (("node", ["node"]), ("bun", ["bun", "run"]), ("deno", ["deno", "run", "-A"])):
            # Resolve to the real path: on Windows these are .cmd/.exe shims and
            # CreateProcess will not find the bare name.
            exe = shutil.which(cmd[0])
            if exe:
                engines.append((name, [exe] + cmd[1:]))
        engines.append(("zipp", [args.zipp, "js"]))
        baseline = args.baseline

    # Startup baseline, subtracted per engine so the table reports COMPUTE, not
    # process launch. zipp starts ~2x faster than node (no snapshot to load), so
    # leaving startup in would flatter it by a constant that shrinks with bench
    # length — and would make these numbers incomparable with the historical
    # series in PERF_ROADMAP, which has always been compute.
    empty = os.path.join(os.path.dirname(BENCH_DIR), "_empty_bench.js")
    with open(empty, "w") as fh:
        fh.write(chr(10))

    samples = {n: {b: [] for b in benches} for n, _ in engines}
    startup = {n: [] for n, _ in engines}
    outputs = {n: {} for n, _ in engines}
    failures = []

    for rep in range(args.reps):
        for name, cmd in engines:
            dt, _, _ = run_once(cmd, empty)
            startup[name].append(dt)
        for b in benches:
            path = os.path.join(BENCH_DIR, b + ".js")
            for name, cmd in engines:
                dt, out, rc = run_once(cmd, path)
                if rc != 0:
                    failures.append(f"{name} exited {rc} on {b}")
                samples[name][b].append(dt)
                # Keep the first run's bytes; later reps must match them exactly,
                # which also catches an engine that is nondeterministic on its own.
                if b not in outputs[name]:
                    outputs[name][b] = out
                elif outputs[name][b] != out:
                    failures.append(f"{name} output not reproducible on {b}")
        print(f"  rep {rep + 1}/{args.reps} done", file=sys.stderr)
    try:
        os.remove(empty)
    except OSError:
        pass
    base_ms = {n: statistics.median(startup[n]) * 1000 for n, _ in engines}

    # Correctness: EXACT bytes against the baseline engine. No normalisation —
    # that is the point.
    all_correct = True
    for b in benches:
        ref = outputs[baseline].get(b)
        for name, _ in engines:
            if name == baseline:
                continue
            if outputs[name].get(b) != ref:
                all_correct = False
                failures.append(f"{name} output differs from {baseline} on {b}")

    w = max(len(b) for b in benches) + 2
    hdr = f"{'bench':<{w}}" + "".join(f"{n:>12}" for n, _ in engines)
    if len(engines) == 2:
        hdr += f"{'delta':>9}"
    else:
        hdr += f"{'ratio':>8}"
    print(hdr)
    print("-" * len(hdr))

    ratios = []
    for b in benches:
        row = f"{b:<{w}}"
        med = {}
        for name, _ in engines:
            xs = samples[name][b]
            med[name] = max(statistics.median(xs) * 1000 - base_ms[name], 1.0)
            row += f"{med[name]:>9.0f}ms"
        if len(engines) == 2:
            d = (med[engines[1][0]] / med[engines[0][0]] - 1) * 100
            ratios.append(d)
            row += f"{d:>+8.1f}%"
        else:
            r = med["zipp"] / med[baseline]
            ratios.append(r)
            row += f"{r:>7.2f}x"
        # spread of the engine under test, so a wide row is visible not implied
        u = "new" if args.ab else "zipp"
        xs = samples[u][b]
        p10 = max(pct(xs, 0.10) * 1000 - base_ms[u], 1.0)
        p90 = max(pct(xs, 0.90) * 1000 - base_ms[u], 1.0)
        row += f"   [p10 {p10:.0f} p90 {p90:.0f}]"
        print(row)

    print("-" * len(hdr))
    if len(engines) == 2:
        print(f"mean delta: {sum(ratios) / len(ratios):+.1f}%")
    else:
        g = 2.718281828459045 ** (sum(__import__("math").log(r) for r in ratios) / len(ratios))
        print(f"geomean: {g:.2f}x slower than {baseline}")
    print("startup(ms, median): " + "  ".join(f"{n}={base_ms[n]:.0f}" for n, _ in engines))
    print(f"ALL_CORRECT={'1' if all_correct else '0'}  (exact bytes, no normalisation)")
    for f in dict.fromkeys(failures):
        print(f"  FAIL: {f}")

    if args.json:
        with open(args.json, "w") as fh:
            json.dump({"reps": args.reps, "benches": benches,
                       "startup_s": startup,
                       "engines": [n for n, _ in engines],
                       "samples": samples, "all_correct": all_correct,
                       "failures": failures}, fh, indent=1)
        print(f"raw samples -> {args.json}")

    return 0 if all_correct and not failures else 1


if __name__ == "__main__":
    sys.exit(main())
