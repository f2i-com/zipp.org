"""Run the regex benchmark set in several engines, interleaved, and print a table.

  python bench/regex/run.py [--rounds N] [--case NAME ...] ENGINE...

ENGINE is `node`, a path to a `zipp` executable (`zipp js FILE`), or
`wasm:PKG_DIR` for a Node package of a WebAssembly module (`zipp_wasm.js` +
`zipp_wasm_bg.wasm`, run through `run_js.cjs` beside this file with
`node --no-liftoff`). Give an engine a label with `LABEL=ENGINE`.

Each case times its own `w()` seven times and prints the minimum; this takes
the minimum of that over the rounds, so start-up and module compilation are
not in the numbers. Every engine must print the same result for a case, or
the row is marked MISMATCH.
"""
import argparse
import math
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def cases():
    return sorted(f[:-3] for f in os.listdir(HERE) if f.endswith(".js"))


def argv_for(engine, path):
    if engine == "node":
        return ["node", path]
    if engine.startswith("wasm:"):
        return ["node", "--no-liftoff", os.path.join(HERE, "run_js.cjs"), engine[5:], path]
    return [engine, "js", path]


def run_once(engine, path):
    env = dict(os.environ)
    p = subprocess.run(argv_for(engine, path), cwd=HERE, capture_output=True, text=True,
                       encoding="utf-8", errors="replace", env=env, timeout=1200)
    for line in p.stdout.splitlines():
        parts = line.split(" ", 3)
        if len(parts) == 4 and parts[2] == "ms":
            return float(parts[1]), parts[3]
    return None, (p.stdout + p.stderr).strip()[-300:]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--case", action="append")
    ap.add_argument("engines", nargs="+")
    a = ap.parse_args()
    engines = []
    for e in a.engines:
        label, _, spec = e.partition("=") if "=" in e and not e.startswith("wasm:") else (None, None, e)
        if label is None:
            label = "node" if e == "node" else (os.path.basename(e[5:].rstrip("/\\")) if e.startswith("wasm:") else os.path.basename(e))
            spec = e
        engines.append((label, spec))
    names = a.case or cases()
    best = {}
    outs = {}
    for name in names:
        path = os.path.join(HERE, name + ".js")
        for _ in range(a.rounds):
            for label, spec in engines:
                t, out = run_once(spec, path)
                key = (name, label)
                outs.setdefault(key, out)
                if t is None:
                    outs[key] = "FAILED: " + out
                    continue
                if key not in best or t < best[key]:
                    best[key] = t
    w = max(len(l) for l, _ in engines)
    print("case".ljust(18) + "".join(l.rjust(max(w, 10) + 2) for l, _ in engines) + "   ratios vs " + engines[0][0])
    logs = {l: [] for l, _ in engines}
    for name in names:
        row = name.ljust(18)
        base = best.get((name, engines[0][0]))
        ratios = []
        for label, _ in engines:
            t = best.get((name, label))
            row += (("%.2f" % t) if t is not None else "fail").rjust(max(w, 10) + 2)
            if t is not None and base:
                ratios.append("%.2f" % (t / base))
                logs[label].append(math.log(t / base))
        results = {outs.get((name, l)) for l, _ in engines}
        row += "   " + " ".join(ratios)
        if len(results) != 1:
            row += "   MISMATCH " + " | ".join(str(outs.get((name, l)))[:80] for l, _ in engines)
        print(row)
    print("geomean".ljust(18) + "".join(("%.3f" % math.exp(sum(v) / len(v)) if v else "-").rjust(max(w, 10) + 2) for v in logs.values()))


if __name__ == "__main__":
    main()
