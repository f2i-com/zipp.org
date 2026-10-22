#!/usr/bin/env python3
"""Turn the published benchmark captures into the landing page's data.

    python landing/scripts/import-benchmarks.py [--check]

Reads the capture artifacts committed under bench/ (paths in CAPTURES below)
and writes landing/src/benchmark-data.json, which BenchmarkSection.tsx and the
home page's proof band render. Every median, ratio and interval is copied from
a harness report; the only figures computed here are the equal-row all-30
point estimates, exp((13 ln G13 + 17 ln G17) / 30), as docs/PERFORMANCE.md
defines them. The Bun and Deno geomeans of the normal suite come from
tools/bench.py's own functions replaying the saved observations, the report's
own baseline being Node. With --check, nothing is written and the exit status says whether
the committed file is current.
"""
from __future__ import annotations

import importlib.util
import json
import math
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "landing/src/benchmark-data.json"

CAPTURES = {
    "jit_normal": "bench/real13_84012a2d_pgo_2026-10-21.json",
    "jit_hostile": "bench/hostile/head_clean_84012a2d_pgo_2026-10-21.json",
    "interp_normal": "bench/comparison/interp_real13_84012a2d_2026-10-22.json",
    "interp_hostile": "bench/comparison/interp_hostile17_84012a2d_2026-10-22.json",
    "release_normal": "bench/comparison/release_real13_da0fe6b1_2026-10-21.json",
    "pgo_ab": "bench/comparison/pgo_ab_real13_da0fe6b1_84012a2d_2026-10-21.json",
    "python_jit": "bench/python/cpython_da0fe6b1_2026-10-21.json",
    "python_interp": "bench/python/cpython_nojit_da0fe6b1_2026-10-21.json",
}

NORMAL_NAMES = {
    "async-promise-chain": "Async / promises",
    "class-prototype-hot": "Class / prototype",
    "json-large": "JSON",
    "map-set-heavy": "Map / Set",
    "markdown-render": "Markdown render",
    "parse-large-js": "Parse JavaScript",
    "polymorphic-objects": "Polymorphic objects",
    "regex-log-scan": "RegExp log scan",
    "sparse-array": "Sparse array",
    "typedarray-math": "TypedArray math",
    "polymorphic-objects-v2": "Polymorphic objects v2",
    "property-ic-shapes": "Property IC shapes",
    "sparse-array-v2": "Sparse array v2",
}
HOSTILE_NAMES = {
    "calls-baseline": "Calls baseline",
    "calls-closures": "Closure calls",
    "shapes-stable": "Stable shapes",
    "shapes-megamorphic": "Megamorphic shapes",
    "types-stable": "Stable types",
    "types-churn": "Type churn",
    "branch-control": "Branch control",
    "throw-catch": "Throw / catch",
    "allocation-ephemeral": "Ephemeral allocation",
    "allocation-survival": "Allocation survival",
    "async-burst": "Async burst",
    "async-lived": "Long-lived async",
    "reactish-reconcile": "React-shaped reconcile",
    "warm-router": "Warm router",
    "bytecode-vm": "Bytecode VM",
    "module-hot-graph": "Hot module graph",
    "npm-nanoid": "npm nanoid",
}
INTERP_ENGINES = ["quickjs", "quickjs-ng", "node-jitless", "deno-jitless", "bun-nojit"]


def load(key: str) -> dict:
    return json.loads((ROOT / CAPTURES[key]).read_text(encoding="utf-8"))


def r(value: float | None, digits: int = 4) -> float | None:
    return None if value is None else round(value, digits)


def interval(pair) -> list[float] | None:
    return None if not pair else [r(pair[0]), r(pair[1])]


def require(condition: bool, message: str) -> None:
    if not condition:
        sys.exit(f"import-benchmarks: {message}")


def short(sha: str | None) -> str | None:
    return sha[:8] if sha else None


def version_number(text: str | None) -> str | None:
    """'deno 2.6.10 (stable, ...)' -> '2.6.10'; 'v24.19.0' stays as Node prints it."""
    if not text:
        return None
    first = text.strip().splitlines()[0]
    if first.startswith("v") and first[1:2].isdigit():
        return first.split()[0]
    for token in first.replace("(", " ").split():
        if token[:1].isdigit():
            return token
    return first[:40]


def engine_versions(report: dict) -> dict:
    out = {}
    for engine in report["engines"]:
        out[engine["name"]] = {"version": version_number(engine.get("version")), "sha256": engine.get("sha256")}
    return out


def _bench_module():
    spec = importlib.util.spec_from_file_location("zipp_bench_harness", ROOT / "tools/bench.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def bench_geomeans(path: str, baseline: str) -> dict:
    """tools/bench.py's row-set geomeans of a capture against `baseline`.

    The capture's own row_set_summaries are against its baseline, Node. For
    Bun and Deno this replays the saved observations through the harness's
    own load_result and subset_geomean, with the seeds and bootstrap count
    the capture used, exactly as the harness builds row_set_summaries."""
    bench = _bench_module()
    report = json.loads((ROOT / path).read_text(encoding="utf-8"))
    replay = bench.load_result(ROOT / path)
    row_sets = bench.classify_benches(replay["benches"])
    found = {}
    for set_name, benches in (("headline", row_sets["headline_benches"]), ("diagnostic", row_sets["diagnostic_benches"]), ("all_measured", replay["benches"])):
        detail = bench.subset_geomean(replay["cold"], benches, baseline, "zipp",
                                      seed=bench.derived_seed(report["seed"], set_name), bootstrap_samples=report["bootstrap_samples"])
        require(detail is not None, f"no {set_name} geomean against {baseline} in {path}")
        found[set_name] = {"value": r(detail["geomean_paired_ratio"]), "ci": interval(detail["ci95"])}
    if baseline == report["baseline"]:
        for set_name, detail in report["row_set_summaries"].items():
            require(abs(detail["geomean_paired_ratio"] - found[set_name]["value"]) < 1e-4 and interval(detail["ci95"]) == found[set_name]["ci"],
                    f"replaying {path} does not reproduce its own {set_name} geomean")
    return found


def jit_section() -> dict:
    normal = load("jit_normal")
    hostile = load("jit_hostile")
    for name, report in (("normal", normal), ("hostile", hostile)):
        require(report["publishable"] is True, f"the canonical {name} capture is not publishable: {report.get('provenance_reasons')} {report.get('publication_reasons')}")
        require(report["all_correct"] is True, f"the canonical {name} capture has output mismatches")
    groups = {b: "headline" for b in normal["headline_benches"]} | {b: "diagnostic" for b in normal["diagnostic_benches"]}
    rows = []
    for bench, row in normal["summary"]["rows"].items():
        cold = row["metrics"]["cold"]
        rows.append({
            "id": bench, "name": NORMAL_NAMES[bench], "group": groups[bench],
            "times": {k: r(v, 3) for k, v in cold["median_ms"].items()},
            "ratio": r(cold["paired_ratio"], 6), "ci": interval(cold["paired_ratio_ci95"]),
        })
    order = list(NORMAL_NAMES)
    rows.sort(key=lambda row: order.index(row["id"]))
    normal_geo = {"node": {k: v for k, v in bench_geomeans(CAPTURES["jit_normal"], "node").items()}}
    for baseline in ("bun", "deno"):
        normal_geo[baseline] = bench_geomeans(CAPTURES["jit_normal"], baseline)
    require(abs(normal_geo["node"]["all_measured"]["value"] - normal["summary"]["geomean_paired_ratio"]) < 1e-3, "bench.py's Node geomean disagrees with the capture's own")

    hrows = []
    for case in hostile["cases"]:
        summary = hostile["summary"]["case_summaries"][case["id"]]["metrics"]["cold"]
        hrows.append({
            "id": case["id"], "name": HOSTILE_NAMES[case["id"]], "group": case["category"],
            "times": {k: r(v, 3) for k, v in summary["median_ms"].items()},
            "ratio": r(summary["zipp_vs"]["node"]["paired_ratio"], 6),
            "ci": interval(summary["zipp_vs"]["node"]["paired_ratio_ci95"]),
            "vs": {c: r(summary["zipp_vs"][c]["paired_ratio"], 6) for c in ("node", "bun", "deno")},
        })
    cold = hostile["summary"]["suite_summaries"]["cold"]
    hostile_geo = {c: {"value": r(v["geomean_paired_ratio"]), "ci": interval(v["ci95"])} for c, v in cold["overall_by_competitor"].items()}
    balanced = {c: {"value": r(v["geomean_paired_ratio"]), "ci": interval(v["ci95"])} for c, v in cold["category_balanced_by_competitor"].items()}

    all30 = {}
    for c in ("node", "bun", "deno"):
        g13 = normal_geo[c]["all_measured"]["value"]
        g17 = hostile_geo[c]["value"]
        all30[c] = r(math.exp((13 * math.log(g13) + 17 * math.log(g17)) / 30))
    wins = sum(row["ratio"] < 1 for row in rows) + sum(row["ratio"] < 1 for row in hrows)
    zipp_engine = next(e for e in normal["engines"] if e["name"] == "zipp")
    startup = normal["summary"]["startup_median_ms"]
    return {
        "capturedAt": normal["created_utc"][:10],
        "reps": normal["reps"], "bootstrapSamples": normal["bootstrap_samples"],
        "zipp": {"version": zipp_engine["version"], "sha256": zipp_engine["sha256"], "source": short((zipp_engine.get("build_identity") or {}).get("source"))},
        "engines": {k: v["version"] for k, v in engine_versions(normal).items() if k != "zipp"},
        "startupMs": {k: r(v, 1) for k, v in startup.items()},
        "normal": {"rows": rows, "geomean": normal_geo},
        "hostile": {"rows": hrows, "geomean": hostile_geo, "categoryBalanced": balanced},
        "all30": all30,
        "nodeWins": wins,
        "artifacts": {"normal": CAPTURES["jit_normal"], "hostile": CAPTURES["jit_hostile"]},
    }


def interp_suite(key: str, names: dict) -> dict:
    report = load(key)
    require(report["all_correct"] is True, f"{key} has output mismatches")
    cold = report["summary"]["cold"]
    medians = cold["median_seconds"]
    comparisons = cold["comparisons"]["zipp-interp"]
    cases = [c for c in names if c in medians["zipp-interp"]]
    rows = []
    for case in cases:
        rows.append({
            "id": case, "name": names[case],
            "times": {engine: r(medians[engine][case] * 1000, 1) for engine in ["zipp-interp", *INTERP_ENGINES, "node"] if case in medians.get(engine, {})},
            "vs": {engine: r(comparisons[engine]["by_case"].get(case), 4) for engine in INTERP_ENGINES if comparisons.get(engine, {}).get("by_case", {}).get(case) is not None},
        })
    geo = {}
    for engine in INTERP_ENGINES + ["node"]:
        comparison = comparisons.get(engine)
        if comparison and comparison.get("available"):
            geo[engine] = {"value": r(comparison["geomean_ratio"]), "ci": interval(comparison["bootstrap_95"]),
                           "wins": comparison["point_wins"], "of": comparison["case_count"]}
        else:
            geo[engine] = {"unavailable": (comparison or {}).get("unavailable_cases", [])}
    engines = {name: version_number((e.get("version_probe") or {}).get("stdout")) for name, e in report["engines"].items() if not name.startswith("zipp-")}
    return {"rows": rows, "geomean": geo, "excluded": report.get("excluded") or {}, "reps": report["reps"],
            "capturedAt": report["created_at"][:10], "engines": engines}


def python_section() -> dict:
    jit = load("python_jit")
    interp = load("python_interp")
    require(jit["all_valid"] and interp["all_valid"], "a Python capture has failures or checksum differences")
    cpython = next(e for e in jit["engines"] if e["name"] == "cpython")
    zipp = next(e for e in jit["engines"] if e["name"] == "zipp")
    rows = []
    for bench, s in jit["summary"].items():
        if s["zipp_only"]:
            continue
        si = interp["summary"][bench]
        rows.append({
            "id": bench, "name": bench.split("/", 1)[1].replace("_", " "), "group": s["group"],
            "workMs": {"zipp": r(s["engines"]["zipp"]["work_s"]["median"] * 1000, 2),
                       "zippInterp": r(si["engines"]["zipp"]["work_s"]["median"] * 1000, 2),
                       "cpython": r(s["engines"]["cpython"]["work_s"]["median"] * 1000, 2)},
            "ratio": r(s["ratios"]["work_s"]["zipp/cpython"]),
            "ratioInterp": r(si["ratios"]["work_s"]["zipp/cpython"]),
            "wallRatio": r(s["ratios"]["wall_s"]["zipp/cpython"]),
            "wallRatioInterp": r(si["ratios"]["wall_s"]["zipp/cpython"]),
        })
    def geo(report, metric):
        return {group: r(report["geomeans"][metric][group]["zipp/cpython"]["value"]) for group in ("micro", "macro", "all")}
    zipp_only = sum(1 for s in jit["summary"].values() if s["zipp_only"])
    return {
        "capturedAt": jit["generated_at_utc"][:10],
        "reps": jit["configuration"]["reps"] if isinstance(jit.get("configuration"), dict) and "reps" in jit["configuration"] else None,
        "zipp": {"version": zipp.get("version"), "sha256": zipp.get("sha256")},
        "cpython": version_number((cpython.get("version") or "").replace("Python ", "")),
        "rows": rows,
        "geomean": {"work": {"jit": geo(jit, "work_s"), "interp": geo(interp, "work_s")},
                    "wall": {"jit": geo(jit, "wall_s"), "interp": geo(interp, "wall_s")}},
        "zippOnlyPrograms": zipp_only,
        "artifacts": {"jit": CAPTURES["python_jit"], "interp": CAPTURES["python_interp"]},
    }


def release_section() -> dict:
    release = load("release_normal")
    zipp_engine = next(e for e in release["engines"] if e["name"] == "zipp")
    sets = release["row_set_summaries"]
    ab = load("pgo_ab")
    ab_sets = ab.get("row_set_summaries") or {}
    return {
        "zipp": {"version": zipp_engine["version"], "sha256": zipp_engine["sha256"]},
        "normalVsNode": {name: {"value": r(v["geomean_paired_ratio"]), "ci": interval(v["ci95"])} for name, v in sets.items()},
        "allCorrect": release["all_correct"],
        # tools/bench.py --ab OLD NEW reports NEW / OLD: the PGO build's time
        # over the release build's. The two differ in features, CPU target and
        # linker as well as the profile, so this compares products, not PGO alone.
        "pgoOverRelease": {name: {"value": r(v["geomean_paired_ratio"]), "ci": interval(v["ci95"])} for name, v in ab_sets.items()},
        "artifacts": {"release": CAPTURES["release_normal"], "pgoAb": CAPTURES["pgo_ab"]},
    }


def interp_section() -> dict:
    normal = interp_suite("interp_normal", NORMAL_NAMES)
    hostile = interp_suite("interp_hostile", HOSTILE_NAMES)
    all30 = {}
    for engine in INTERP_ENGINES + ["node"]:
        a, b = normal["geomean"].get(engine, {}), hostile["geomean"].get(engine, {})
        if "value" in a and "value" in b:
            n13, n17 = a["of"], b["of"]
            all30[engine] = {
                "value": r(math.exp((n13 * math.log(a["value"]) + n17 * math.log(b["value"])) / (n13 + n17))),
                "wins": a["wins"] + b["wins"], "of": n13 + n17,
            }
    return {"normal": normal, "hostile": hostile, "all30": all30,
            "artifacts": {"normal": CAPTURES["interp_normal"], "hostile": CAPTURES["interp_hostile"]}}


def build() -> dict:
    return {
        "_comment": "Generated by landing/scripts/import-benchmarks.py from the captures under bench/; do not edit.",
        "jit": jit_section(),
        "interp": interp_section(),
        "python": python_section(),
        "release": release_section(),
    }


def main() -> None:
    text = json.dumps(build(), indent=2, ensure_ascii=False) + "\n"
    if "--check" in sys.argv:
        current = OUT.read_text(encoding="utf-8") if OUT.exists() else ""
        require(current.replace("\r\n", "\n") == text, f"{OUT.relative_to(ROOT)} is stale: run landing/scripts/import-benchmarks.py")
        print(f"{OUT.relative_to(ROOT)} is current")
        return
    OUT.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
