#!/usr/bin/env python3
"""Per-row report of a `run_native.py` artifact: Zipp's interpreter against one
competitor (default Bellard's QuickJS).

For every case: the median ratio of startup-adjusted process cycles (Windows;
startup-adjusted wall time elsewhere), a 95% percentile-bootstrap interval of
that ratio (repetitions resampled within the case), whether the interval
excludes 1 (otherwise the row is AMBIGUOUS, neither a win nor a loss), the
cold-time ratio, the startup ratio and the peak-working-set ratio. Then the
ordinary geomean, the category-balanced geomean (each category's geomean
weighted equally), the best and worst rows, and every excluded case with its
reason.

usage: expansion_report.py ARTIFACT.json [--competitor quickjs] [--markdown OUT.md]
"""
from __future__ import annotations

import argparse
import json
import math
import random
import statistics
import sys
from pathlib import Path


def geomean(values):
    values = [v for v in values if v > 0 and math.isfinite(v)]
    return math.exp(statistics.fmean(math.log(v) for v in values)) if values else float("nan")


def per_case(artifact, engine, metric):
    """{case: [value per rep]} for a metric: 'cycles' (adjusted), 'wall'
    (adjusted), 'cold', 'startup', 'peak'."""
    out = {}
    for o in artifact["observations"]:
        if o["engine"] != engine:
            continue
        full, start = o["full"], o["startup"]
        if metric == "cycles":
            if "cycles" not in full:
                return None
            value = max(1.0, float(full["cycles"] - start["cycles"]))
        elif metric == "wall":
            value = max(1e-6, o["adjusted_s"])
        elif metric == "cold":
            value = full["elapsed_s"]
        elif metric == "startup":
            value = start["elapsed_s"]
        elif metric == "peak":
            if "peak_working_set_bytes" not in full:
                return None
            value = float(full["peak_working_set_bytes"])
        else:
            raise ValueError(metric)
        out.setdefault(o["case"], []).append(value)
    return out


def ratio_ci(a, b, rng, samples=4000):
    point = statistics.median(a) / statistics.median(b)
    boot = []
    for _ in range(samples):
        ra = statistics.median(rng.choice(a) for _ in a)
        rb = statistics.median(rng.choice(b) for _ in b)
        boot.append(ra / rb)
    boot.sort()
    return point, boot[int(0.025 * len(boot))], boot[min(len(boot) - 1, int(0.975 * len(boot)))]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("artifact", type=Path)
    ap.add_argument("--competitor", default="quickjs")
    ap.add_argument("--zipp", default="zipp-interp")
    ap.add_argument("--markdown", type=Path)
    args = ap.parse_args()
    art = json.loads(args.artifact.read_text(encoding="utf-8"))
    rng = random.Random(0x5A17)
    cases = art["cases"]
    categories = {c: art["inputs"].get(c, {}).get("category", "") for c in cases}
    primary = "cycles" if per_case(art, args.zipp, "cycles") else "wall"
    metrics = {m: (per_case(art, args.zipp, m), per_case(art, args.competitor, m))
               for m in ("cycles", "wall", "cold", "startup", "peak")}
    rows = []
    for case in cases:
        z, c = metrics[primary]
        point, lo, hi = ratio_ci(z[case], c[case], rng)
        row = {"case": case, "category": categories[case], "ratio": point, "lo": lo, "hi": hi,
               "verdict": "zipp faster" if hi < 1 else ("competitor faster" if lo > 1 else "AMBIGUOUS")}
        for m in ("cold", "startup", "peak"):
            mz, mc = metrics[m]
            if mz and mc:
                row[m] = statistics.median(mz[case]) / statistics.median(mc[case])
        rows.append(row)
    by_cat = {}
    for r in rows:
        by_cat.setdefault(r["category"] or "-", []).append(r["ratio"])
    cat_geo = {k: geomean(v) for k, v in sorted(by_cat.items())}
    lines = []
    reps = art["reps"]
    lines.append(f"Zipp ({args.zipp}) / {args.competitor}, startup-adjusted "
                 f"{'process cycles' if primary == 'cycles' else 'wall time'}, {reps} reps, "
                 f"suite {art['method'].get('suite')}{' sizes ' + art['sizes'] if art.get('sizes') else ''}")
    lines.append("")
    lines.append("| case | category | ratio | 95% interval | verdict | cold | startup | peak memory |")
    lines.append("|---|---|---|---|---|---|---|---|")
    for r in sorted(rows, key=lambda r: -r["ratio"]):
        lines.append(f"| {r['case']} | {r['category']} | {r['ratio']:.3f} | [{r['lo']:.3f}, {r['hi']:.3f}] | "
                     f"{r['verdict']} | {r.get('cold', float('nan')):.3f} | {r.get('startup', float('nan')):.3f} | "
                     f"{r.get('peak', float('nan')):.2f} |")
    lines.append("")
    lines.append(f"ordinary geomean {geomean([r['ratio'] for r in rows]):.3f} over {len(rows)} rows; "
                 f"category-balanced geomean {geomean(list(cat_geo.values())):.3f} over {len(cat_geo)} categories")
    lines.append("categories: " + ", ".join(f"{k} {v:.3f} ({len(by_cat[k])})" for k, v in cat_geo.items()))
    wins = sum(r["verdict"] == "zipp faster" for r in rows)
    losses = sum(r["verdict"] == "competitor faster" for r in rows)
    lines.append(f"zipp faster on {wins}, {args.competitor} faster on {losses}, ambiguous {len(rows) - wins - losses}")
    if art.get("excluded"):
        lines.append("")
        lines.append("excluded: " + "; ".join(
            f"{case} ({', '.join(e['engine'] + ': ' + e['reason'][:80] for e in why)})"
            for case, why in sorted(art["excluded"].items())))
    text = "\n".join(lines)
    print(text)
    if args.markdown:
        args.markdown.write_text(text + "\n", encoding="utf-8")


if __name__ == "__main__":
    sys.exit(main())
