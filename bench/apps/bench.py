#!/usr/bin/env python3
"""Unmodified npm applications under Node and ZIPP: first run, warm run, wall.

    cd bench/apps && npm ci
    python bench/apps/bench.py --zipp target/release/zipp.exe
    python bench/apps/bench.py --zipp head=old.exe --zipp new=new.exe --reps 9
    python bench/apps/bench.py --zipp target/release/zipp.exe --zipp-env nojit:ZIPP_NOJIT=1

Every observation is a fresh process running `run.mjs`: one cold run of the
workload, then `--iters` warm ones. Engines are interleaved and the order is
reversed on alternate repetitions. Reported per application and engine, as
medians over the repetitions:

    first   the cold run, as the program timed it (module loading excluded)
    warm    the median warm iteration
    wall    the whole process, as this script timed it

and each as a ratio to Node's. An engine whose output differs from Node's is
flagged and its row is not a result.

Node is given a whole 8-core die (`--mask`, default 0xFFFF), not one core: its
compiler and collector run on helper threads, and one core triples its first
run. ZIPP runs on one thread either way. The code cache is off for ZIPP
(`ZIPP_CACHE_DIR` empty) so a first run is a first run.
"""
import argparse
import json
import os
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
KINDS = ["linkedom", "defuddle", "webforai"]


def pin(process, mask):
    """Hold a child to `mask`'s logical processors (Windows; elsewhere use taskset)."""
    if os.name != "nt" or not mask:
        return
    import ctypes

    ctypes.windll.kernel32.SetProcessAffinityMask(int(process._handle), mask)


def observe(cmd, env, mask):
    start = time.perf_counter()
    process = subprocess.Popen(cmd, cwd=HERE, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    pin(process, mask)
    out, err = process.communicate()
    wall = (time.perf_counter() - start) * 1000.0
    line = None
    for text in out.decode("utf-8", "replace").splitlines():
        if text.startswith("{"):
            line = text
    if process.returncode != 0 or line is None:
        tail = (err or out).decode("utf-8", "replace").strip().replace("\n", " ")
        return None, wall, tail[-200:]
    return json.loads(line), wall, ""


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--zipp", action="append", default=[], metavar="[LABEL=]EXE",
                        help="a ZIPP binary to measure (repeatable)")
    parser.add_argument("--zipp-env", action="append", default=[], metavar="LABEL:KEY=VALUE[,KEY=VALUE]",
                        help="the first --zipp binary again under these settings (repeatable)")
    parser.add_argument("--node", default="node")
    parser.add_argument("--kinds", default=",".join(KINDS))
    parser.add_argument("--reps", type=int, default=7)
    parser.add_argument("--iters", type=int, default=15)
    parser.add_argument("--mask", type=lambda s: int(s, 0), default=0xFFFF,
                        help="processor affinity mask for every child, 0 for none (default 0xFFFF)")
    parser.add_argument("--json", help="write every observation here")
    args = parser.parse_args()
    if not os.path.isdir(os.path.join(HERE, "node_modules")):
        sys.exit("bench/apps/node_modules is missing: run `npm ci` in bench/apps first")
    if not args.zipp:
        sys.exit("give at least one --zipp binary")

    engines = [("node", [args.node], {})]
    first_exe = None
    for spec in args.zipp:
        label, _, exe = spec.rpartition("=")
        exe = os.path.abspath(exe)
        first_exe = first_exe or exe
        engines.append((label or "zipp", [exe, "run"], {}))
    for spec in args.zipp_env:
        label, _, settings = spec.partition(":")
        env = dict(item.split("=", 1) for item in settings.split(",") if item)
        engines.append((label, [first_exe, "run"], env))

    base_env = dict(os.environ)
    base_env["ZIPP_CACHE_DIR"] = ""
    kinds = [k for k in args.kinds.split(",") if k]
    rows = {}
    reference = {}
    raw = []
    for kind in kinds:
        for rep in range(args.reps):
            order = engines if rep % 2 == 0 else list(reversed(engines))
            for label, cmd, extra in order:
                env = dict(base_env)
                env.update(extra)
                line, wall, error = observe(cmd + ["run.mjs", kind, str(args.iters)], env, args.mask)
                row = rows.setdefault((kind, label), {"first": [], "warm": [], "wall": [], "note": ""})
                if line is None:
                    row["note"] = "FAILED: " + error
                    continue
                shown = (line["outLen"], line["head"], line["tail"])
                if label == "node":
                    reference.setdefault(kind, shown)
                elif kind in reference and reference[kind] != shown:
                    row["note"] = "OUTPUT DIFFERS FROM NODE"
                row["first"].append(line["firstMs"])
                row["warm"].append(statistics.median(line["warm"]))
                row["wall"].append(wall)
                raw.append({"kind": kind, "engine": label, "rep": rep, "wall_ms": wall, **line})

    median = statistics.median
    print(f"reps={args.reps} iters={args.iters} mask={args.mask:#x}  (ms; ratios are engine / node)")
    print(f"{'app':10} {'engine':14} {'first':>9} {'warm':>9} {'wall':>9}   {'first/n':>8} {'warm/n':>8} {'wall/n':>8}  note")
    for kind in kinds:
        node = rows.get((kind, "node"))
        for label, _, _ in engines:
            row = rows[(kind, label)]
            if not row["first"]:
                print(f"{kind:10} {label:14} {row['note']}")
                continue
            first, warm, wall = median(row["first"]), median(row["warm"]), median(row["wall"])
            ratios = ""
            if node and node["first"]:
                ratios = (f"{first / median(node['first']):8.2f} {warm / median(node['warm']):8.2f} "
                          f"{wall / median(node['wall']):8.2f}")
            print(f"{kind:10} {label:14} {first:9.2f} {warm:9.2f} {wall:9.1f}   {ratios}  {row['note']}")
    if args.json:
        with open(args.json, "w", encoding="utf-8") as out:
            json.dump({"reps": args.reps, "iters": args.iters, "mask": args.mask, "observations": raw}, out, indent=1)


if __name__ == "__main__":
    main()
