#!/usr/bin/env python3
"""Measure how much of a row's standing survives a semantics-preserving rewrite.

Engines and variants are interleaved within every repetition so machine drift
hits both sides equally; the reported number is a median over repetitions.
See bench/scope/README.md for what the variants are and what they showed.
"""
from __future__ import annotations

import statistics
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
ZIPP = ROOT / "target" / "release" / "zipp.exe"
if not ZIPP.exists():                      # non-Windows checkouts
    ZIPP = ROOT / "target" / "release" / "zipp"

ROWS = ("typedarray-math", "sparse-array")
VARIANTS = ("", ".R1_iife", ".R2_let", ".R3_rename")
TIMEOUT_S = 600


def time_once(cmd: list[str]) -> float:
    t = time.perf_counter()
    try:
        proc = subprocess.run(cmd, capture_output=True, cwd=ROOT, timeout=TIMEOUT_S)
    except subprocess.TimeoutExpired as exc:
        raise SystemExit(f"{' '.join(cmd)} exceeded {TIMEOUT_S}s") from exc
    dt = time.perf_counter() - t
    if proc.returncode != 0:
        raise SystemExit(f"{' '.join(cmd)} failed:\n{proc.stderr.decode()[:400]}")
    return dt


def main() -> int:
    if len(sys.argv) > 2:
        raise SystemExit("usage: run.py [positive-repetitions]")
    try:
        reps = int(sys.argv[1]) if len(sys.argv) > 1 else 9
    except ValueError as exc:
        raise SystemExit("repetitions must be a positive integer") from exc
    if reps <= 0:
        raise SystemExit("repetitions must be a positive integer")
    if not ZIPP.exists():
        raise SystemExit(f"no zipp binary at {ZIPP} -- cargo build --release first")

    for row in ROWS:
        paths = [HERE / f"{row}{v}.js" for v in VARIANTS]
        missing = [p for p in paths if not p.exists()]
        if missing:
            raise SystemExit(f"missing variant(s): {', '.join(map(str, missing))}")

        samples: dict[Path, dict[str, list[float]]] = {p: {"z": [], "n": []} for p in paths}
        for _ in range(reps):
            for p in paths:                      # interleave variant AND engine
                samples[p]["z"].append(time_once([str(ZIPP), "js", str(p)]))
                samples[p]["n"].append(time_once(["node", str(p)]))

        print(f"\n=== {row} ===  ({reps} interleaved paired reps)")
        print(f"{'variant':<30}{'zipp ms':>9}{'node ms':>9}{'ratio':>8}{'vs orig':>9}")
        base = None
        for p in paths:
            z = statistics.median(samples[p]["z"]) * 1000
            n = statistics.median(samples[p]["n"]) * 1000
            ratio = z / n
            base = ratio if base is None else base
            print(f"{p.name:<30}{z:>9.1f}{n:>9.1f}{ratio:>8.3f}{ratio / base:>8.2f}x")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
