#!/usr/bin/env python3
"""Suite-wide scope sensitivity: how much of each row survives a rewrite.

Generates two semantics-preserving rewrites of every `bench/real/*.js`, checks
each against Node for identical output, then measures zipp/Node for the original
and the rewrite with engines and variants interleaved inside every repetition.

  iife  the whole program wrapped in `(function () { ... })()`
  let   every top-level/`for`-head `var` becomes `let`

`parse-large-js` is excluded from `let` only: it embeds JavaScript source as
DATA, so a textual var->let rewrite edits the corpus it parses and changes the
answer. That is a property of the rewrite, not of the engine.

  python bench/scope/sweep.py            # both modes, 9 reps
  python bench/scope/sweep.py 15 iife    # one mode, more reps
"""
from __future__ import annotations

import glob
import re
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
ZIPP = ROOT / "target" / "release" / "zipp.exe"
if not ZIPP.exists():
    ZIPP = ROOT / "target" / "release" / "zipp"

LET_EXCLUDE = {"parse-large-js.js"}      # embeds JS source as data
TIMEOUT_S = 600


def make_iife(text: str) -> str:
    lines = text.split("\n")
    head, i = [], 0
    while i < len(lines) and (
        lines[i].strip() == ""
        or lines[i].strip().startswith("//")
        or re.match(r'^\s*["\']use strict["\'];\s*$', lines[i])
    ):
        head.append(lines[i])
        i += 1
    return "\n".join(head) + "\n(function () {\n" + "\n".join(lines[i:]) + "\n})();\n"


def make_let(text: str) -> str:
    return re.sub(r'(^|[;{}\s(])var(\s)', r'\1let\2', text)


def timed(cmd: list[str]) -> float:
    t = time.perf_counter()
    try:
        p = subprocess.run(cmd, capture_output=True, cwd=ROOT, timeout=TIMEOUT_S)
    except subprocess.TimeoutExpired as exc:
        raise SystemExit(f"{' '.join(map(str, cmd))} exceeded {TIMEOUT_S}s") from exc
    dt = time.perf_counter() - t
    if p.returncode != 0:
        raise SystemExit(f"{' '.join(map(str, cmd))} failed:\n{p.stderr.decode()[:400]}")
    return dt


def main() -> int:
    reps = 9
    modes = ["iife", "let"]
    for a in sys.argv[1:]:
        if a.isdigit():
            reps = int(a)
        elif a in ("iife", "let"):
            modes = [a]
        else:
            raise SystemExit(f"usage: sweep.py [reps] [iife|let]  (got {a!r})")
    if reps <= 0:
        raise SystemExit("repetitions must be a positive integer")
    if not ZIPP.exists():
        raise SystemExit(f"no zipp binary at {ZIPP} -- cargo build --release first")

    rows = sorted(Path(p).name for p in glob.glob(str(ROOT / "bench" / "real" / "*.js")))
    for mode in modes:
        gen = make_iife if mode == "iife" else make_let
        skip = LET_EXCLUDE if mode == "let" else set()
        with tempfile.TemporaryDirectory(prefix=f"zipp-scope-{mode}-") as tmp:
            use = []
            for r in rows:
                if r in skip:
                    continue
                orig = ROOT / "bench" / "real" / r
                var = Path(tmp) / r
                var.write_text(gen(orig.read_text(encoding="utf-8")), encoding="utf-8")
                # a rewrite that changes the answer is not a fair comparison
                original = timed_output(["node", str(orig)])
                rewritten = timed_output(["node", str(var)])
                if original != rewritten:
                    print(f"  !! {r}: rewrite changes Node's output, skipping")
                    continue
                use.append((r, orig, var))

            acc = {r: {k: [] for k in ("zo", "no", "zv", "nv")} for r, _, _ in use}
            for _ in range(reps):
                for r, orig, var in use:                     # interleave all four cells
                    acc[r]["zo"].append(timed([str(ZIPP), "js", str(orig)]))
                    acc[r]["no"].append(timed(["node", str(orig)]))
                    acc[r]["zv"].append(timed([str(ZIPP), "js", str(var)]))
                    acc[r]["nv"].append(timed(["node", str(var)]))

            print(f"\n=== {mode}  ({reps} interleaved paired reps, {len(use)} rows) ===")
            print(f"{'row':<26}{'orig':>8}{'rewrite':>9}{'penalty':>10}")
            pens = []
            for r, _, _ in use:
                m = {k: statistics.median(v) * 1000 for k, v in acc[r].items()}
                ro, rv = m["zo"] / m["no"], m["zv"] / m["nv"]
                pens.append(rv / ro)
                cross = "  <-- crosses 1.0" if ro < 1.0 <= rv else ""
                print(f"{r[:-3]:<26}{ro:>8.3f}{rv:>9.3f}{(rv/ro-1)*100:>+9.1f}%{cross}")
            g = 1.0
            for p in pens:
                g *= p
            print(f"\ngeomean {mode} penalty over {len(pens)} rows: "
                  f"{(g ** (1/len(pens)) - 1) * 100:+.1f}%")
    return 0


def timed_output(cmd: list[str]) -> bytes:
    try:
        p = subprocess.run(cmd, capture_output=True, cwd=ROOT, timeout=TIMEOUT_S)
    except subprocess.TimeoutExpired as exc:
        raise SystemExit(f"{' '.join(map(str, cmd))} exceeded {TIMEOUT_S}s") from exc
    if p.returncode != 0:
        raise SystemExit(f"{' '.join(map(str, cmd))} failed:\n{p.stderr.decode()[:400]}")
    return p.stdout


if __name__ == "__main__":
    raise SystemExit(main())
