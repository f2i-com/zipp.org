#!/usr/bin/env python3
"""Diagnostic Zipp/QuickJS-NG/Boa native comparison.

The public Zipp benchmark harness deliberately knows only its canonical engine
set.  This separate runner keeps ecosystem comparisons explicit and avoids
changing that publication series.  It stages identical source bytes for every
engine, checks stdout after only CRLF-to-LF canonicalization, counterbalances
process order, and retains every raw observation in JSON.
"""

from __future__ import annotations

import argparse
import dataclasses
import datetime as dt
import hashlib
import json
import math
import os
import platform
import random
import shutil
import stat
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_SEED = 0x5A17_2026

REAL13 = (
    "async-promise-chain",
    "class-prototype-hot",
    "json-large",
    "map-set-heavy",
    "markdown-render",
    "parse-large-js",
    "polymorphic-objects",
    "regex-log-scan",
    "sparse-array",
    "typedarray-math",
    "polymorphic-objects-v2",
    "property-ic-shapes",
    "sparse-array-v2",
)


@dataclasses.dataclass(frozen=True)
class Workload:
    name: str
    relative_source: str
    # (old token, new token, required occurrence count)
    replacements: tuple[tuple[str, str, int], ...] = ()


# These are existing project fixtures, not a new hand-picked source corpus.
# The long loop/sort inputs are deterministically downscaled so an interpreter
# comparison completes in minutes rather than hours.  The staged bytes and both
# original/staged hashes are retained in the result.
MICRO5 = (
    Workload(
        "fib-recursive",
        "bench/long/fib.js",
        (("fib(38)", "fib(32)", 1),),
    ),
    Workload(
        "loop-arithmetic",
        "bench/long/loop.js",
        (("500000000", "10000000", 1),),
    ),
    Workload(
        "array-hof",
        "bench/long/array.js",
        (("5000000", "500000", 1),),
    ),
    Workload(
        "object-properties",
        "bench/long/object.js",
        (("40000000", "2000000", 1),),
    ),
    Workload(
        "sort-comparator",
        "bench/long/sort.js",
        (
            ("1999999", "199999", 1),
            ("2000000", "200000", 2),
            ("1000000", "100000", 1),
        ),
    ),
)

CONTROL_ENV_PREFIXES = (
    "ZIPP_",
    "RUST",
    "LLVM_",
    "MIMALLOC_",
    "NODE_",
    "DENO_",
    "BUN_",
    "ASAN_",
    "LSAN_",
    "MSAN_",
    "TSAN_",
    "UBSAN_",
)


@dataclasses.dataclass(frozen=True)
class Engine:
    name: str
    command: tuple[str, ...]
    environment: dict[str, str]
    binary: Path


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def bytes_sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def canonical_stdout(value: bytes) -> bytes:
    """Remove only the Windows CLI line-ending presentation difference."""
    canonical = value.replace(b"\r\n", b"\n")
    if b"\r" in canonical:
        raise ValueError("stdout contains a lone carriage return")
    return canonical


def executable(value: str | Path) -> Path:
    text = str(value)
    located = shutil.which(text)
    path = Path(located) if located else Path(text)
    return path.expanduser().resolve(strict=True)


def controlled_environment(delta: dict[str, str]) -> dict[str, str]:
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.upper().startswith(CONTROL_ENV_PREFIXES)
    }
    environment.update(delta)
    return environment


def source_workloads(suite: str) -> tuple[Workload, ...]:
    if suite == "micro5":
        return MICRO5
    return tuple(
        Workload(name, f"bench/real/{name}.js") for name in REAL13
    )


def stage_inputs(
    workloads: tuple[Workload, ...], result_dir: Path
) -> tuple[Path, dict[str, dict[str, Any]]]:
    stage = Path(
        tempfile.mkdtemp(prefix="zipp-competitor-inputs-", dir=result_dir)
    )
    metadata: dict[str, dict[str, Any]] = {}
    empty = stage / "empty.js"
    empty.write_bytes(b";void 0;\n")
    for workload in workloads:
        source = (ROOT / workload.relative_source).resolve(strict=True)
        raw = source.read_bytes()
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise ValueError(f"benchmark source is not UTF-8: {source}") from exc
        applied = []
        for old, new, expected_count in workload.replacements:
            count = text.count(old)
            if count != expected_count:
                raise ValueError(
                    f"{workload.name}: expected {expected_count} occurrence(s) "
                    f"of {old!r}, found {count}"
                )
            text = text.replace(old, new)
            applied.append({"old": old, "new": new, "count": count})
        # Boa's CLI prints a script's non-undefined completion value.  Appending
        # this to every engine's identical input keeps console output comparable
        # without changing the measured work.
        staged_bytes = (text.rstrip() + "\n;void 0;\n").encode("utf-8")
        destination = stage / f"{workload.name}.js"
        destination.write_bytes(staged_bytes)
        metadata[workload.name] = {
            "source": str(source),
            "source_sha256": bytes_sha256(raw),
            "source_bytes": len(raw),
            "staged": str(destination),
            "staged_sha256": bytes_sha256(staged_bytes),
            "staged_bytes": len(staged_bytes),
            "replacements": applied,
            "completion_suppression": ";void 0;",
        }
    for path in stage.iterdir():
        path.chmod(stat.S_IREAD)
    return stage, metadata


def run_once(
    engine: Engine,
    source: Path,
    *,
    timeout: float,
) -> dict[str, Any]:
    command = [*engine.command, str(source)]
    started = time.perf_counter_ns()
    try:
        result = subprocess.run(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=controlled_environment(engine.environment),
            timeout=timeout,
            check=False,
        )
        elapsed = (time.perf_counter_ns() - started) / 1_000_000_000
        canonical = canonical_stdout(result.stdout)
        return {
            "elapsed_s": elapsed,
            "returncode": result.returncode,
            "timed_out": False,
            "stdout_sha256": bytes_sha256(result.stdout),
            "stdout_bytes": len(result.stdout),
            "stdout_canonical_sha256": bytes_sha256(canonical),
            "stdout_canonical_bytes": len(canonical),
            "stdout": result.stdout.decode("utf-8", errors="replace"),
            "stderr": result.stderr.decode("utf-8", errors="replace"),
        }
    except subprocess.TimeoutExpired as exc:
        elapsed = (time.perf_counter_ns() - started) / 1_000_000_000
        stdout = exc.stdout or b""
        stderr = exc.stderr or b""
        canonical = canonical_stdout(stdout)
        return {
            "elapsed_s": elapsed,
            "returncode": None,
            "timed_out": True,
            "stdout_sha256": bytes_sha256(stdout),
            "stdout_bytes": len(stdout),
            "stdout_canonical_sha256": bytes_sha256(canonical),
            "stdout_canonical_bytes": len(canonical),
            "stdout": stdout.decode("utf-8", errors="replace"),
            "stderr": stderr.decode("utf-8", errors="replace"),
        }


def version_probe(engine: Engine, timeout: float) -> dict[str, Any]:
    if engine.name == "node":
        suffix = ("--version",)
    elif engine.name.startswith("zipp-"):
        suffix = ("--version", "--json")
    elif engine.name == "quickjs-ng":
        suffix = ("--version",)
    else:
        suffix = ("--version",)
    result = subprocess.run(
        [str(engine.binary), *suffix],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=controlled_environment(engine.environment),
        timeout=timeout,
        check=False,
    )
    return {
        "command": [str(engine.binary), *suffix],
        "returncode": result.returncode,
        "stdout": result.stdout.decode("utf-8", errors="replace"),
        "stderr": result.stderr.decode("utf-8", errors="replace"),
    }


def engine_identity(engine: Engine, timeout: float) -> dict[str, Any]:
    return {
        "binary": str(engine.binary),
        "binary_bytes": engine.binary.stat().st_size,
        "binary_sha256": file_sha256(engine.binary),
        "version_probe": version_probe(engine, timeout),
    }


def healthy(result: dict[str, Any]) -> bool:
    return not result["timed_out"] and result["returncode"] == 0


def percentile(sorted_values: list[float], probability: float) -> float:
    if not sorted_values:
        raise ValueError("percentile of empty data")
    position = (len(sorted_values) - 1) * probability
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return sorted_values[lower]
    weight = position - lower
    return sorted_values[lower] * (1 - weight) + sorted_values[upper] * weight


def geometric_mean(values: list[float]) -> float:
    if not values or any(value <= 0 or not math.isfinite(value) for value in values):
        raise ValueError("geometric mean requires positive finite values")
    return math.exp(statistics.fmean(math.log(value) for value in values))


def derived_seed(seed: int, *parts: str) -> int:
    digest = hashlib.sha256(str(seed).encode("ascii"))
    for part in parts:
        digest.update(b"\0")
        digest.update(part.encode("utf-8"))
    return int.from_bytes(digest.digest()[:8], "little")


def summarize_metric(
    samples: dict[str, dict[str, list[float]]],
    *,
    engine_names: list[str],
    cases: list[str],
    reps: int,
    seed: int,
    bootstrap_samples: int,
    metric: str,
) -> dict[str, Any]:
    medians = {
        engine: {
            case: statistics.median(samples[engine][case])
            for case in cases
        }
        for engine in engine_names
    }
    comparisons: dict[str, Any] = {}
    for numerator in ("zipp-jit", "zipp-interp"):
        if numerator not in engine_names:
            continue
        comparisons[numerator] = {}
        for competitor in engine_names:
            if competitor == numerator:
                continue
            if competitor.startswith("zipp-") and not (
                numerator == "zipp-interp" and competitor == "zipp-jit"
            ):
                continue
            by_case: dict[str, float] = {}
            unavailable = []
            for case in cases:
                numerator_value = medians[numerator][case]
                denominator_value = medians[competitor][case]
                if numerator_value <= 0 or denominator_value <= 0:
                    unavailable.append(case)
                else:
                    by_case[case] = numerator_value / denominator_value
            if unavailable:
                comparisons[numerator][competitor] = {
                    "available": False,
                    "unavailable_cases": unavailable,
                    "by_case": by_case,
                }
                continue
            point = geometric_mean(list(by_case.values()))
            rng = random.Random(
                derived_seed(seed, metric, numerator, competitor)
            )
            bootstrap: list[float] = []
            for _ in range(bootstrap_samples):
                indices = [rng.randrange(reps) for _ in range(reps)]
                ratios = []
                valid = True
                for case in cases:
                    left = statistics.median(
                        samples[numerator][case][index] for index in indices
                    )
                    right = statistics.median(
                        samples[competitor][case][index] for index in indices
                    )
                    if left <= 0 or right <= 0:
                        valid = False
                        break
                    ratios.append(left / right)
                if valid:
                    bootstrap.append(geometric_mean(ratios))
            bootstrap.sort()
            comparisons[numerator][competitor] = {
                "available": True,
                "geomean_ratio": point,
                "bootstrap_95": (
                    [
                        percentile(bootstrap, 0.025),
                        percentile(bootstrap, 0.975),
                    ]
                    if bootstrap
                    else None
                ),
                "bootstrap_samples_retained": len(bootstrap),
                "point_wins": sum(value < 1 for value in by_case.values()),
                "case_count": len(cases),
                "by_case": by_case,
            }
    return {"median_seconds": medians, "comparisons": comparisons}


def git_output(*args: str) -> str | None:
    result = subprocess.run(
        ["git", *args],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        check=False,
    )
    return result.stdout.strip() if result.returncode == 0 else None


def machine_metadata() -> dict[str, Any]:
    cpu = platform.processor()
    if os.name == "nt":
        probe = subprocess.run(
            [
                "powershell",
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_Processor | Select-Object -First 1).Name",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            check=False,
        )
        if probe.returncode == 0 and probe.stdout.strip():
            cpu = probe.stdout.strip()
    return {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "processor": cpu,
        "logical_cpus": os.cpu_count(),
        "python": sys.version,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--zipp", type=Path, required=True)
    parser.add_argument("--quickjs-ng", type=Path, required=True)
    parser.add_argument("--boa", type=Path, required=True)
    parser.add_argument("--node", default="node")
    parser.add_argument("--suite", choices=("micro5", "real13"), default="micro5")
    parser.add_argument("--reps", type=int, default=12)
    parser.add_argument("--bootstrap-samples", type=int, default=10_000)
    parser.add_argument("--seed", type=lambda value: int(value, 0), default=DEFAULT_SEED)
    parser.add_argument("--timeout", type=float, default=300.0)
    parser.add_argument("--json", type=Path, required=True)
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument(
        "--engines",
        default="node,zipp-jit,zipp-interp,quickjs-ng,boa,boa-opt",
        help="comma-separated subset (default all six)",
    )
    parser.add_argument("--zipp-revision")
    parser.add_argument("--quickjs-ng-revision")
    parser.add_argument("--boa-revision")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.reps < 1 or args.bootstrap_samples < 1:
        raise SystemExit("--reps and --bootstrap-samples must be positive")
    if args.timeout <= 0 or not math.isfinite(args.timeout):
        raise SystemExit("--timeout must be positive and finite")
    result_path = args.json.expanduser().resolve()
    if result_path.exists() and not args.overwrite:
        raise SystemExit(f"refusing to overwrite {result_path}; pass --overwrite")
    result_path.parent.mkdir(parents=True, exist_ok=True)

    node = executable(args.node)
    zipp = executable(args.zipp)
    quickjs_ng = executable(args.quickjs_ng)
    boa = executable(args.boa)
    available = {
        "node": Engine("node", (str(node),), {}, node),
        "zipp-jit": Engine("zipp-jit", (str(zipp), "js"), {}, zipp),
        "zipp-interp": Engine(
            "zipp-interp", (str(zipp), "js"), {"ZIPP_NOJIT": "1"}, zipp
        ),
        "quickjs-ng": Engine(
            "quickjs-ng", (str(quickjs_ng),), {}, quickjs_ng
        ),
        "boa": Engine("boa", (str(boa),), {}, boa),
        "boa-opt": Engine("boa-opt", (str(boa), "--optimize"), {}, boa),
    }
    requested = args.engines.split(",")
    unknown = sorted(set(requested) - set(available))
    if unknown or not requested or len(requested) != len(set(requested)):
        raise SystemExit(f"invalid --engines list; unknown={unknown}")
    engines = [available[name] for name in requested]
    engine_names = [engine.name for engine in engines]
    if "node" not in engine_names:
        raise SystemExit("the exact-output reference requires node")
    engine_identities_before = {
        engine.name: engine_identity(engine, args.timeout) for engine in engines
    }
    failed_version_probes = [
        name
        for name, identity in engine_identities_before.items()
        if identity["version_probe"]["returncode"] != 0
    ]
    if failed_version_probes:
        raise SystemExit(
            "version probe failed for: " + ", ".join(failed_version_probes)
        )
    if args.zipp_revision:
        for name in ("zipp-jit", "zipp-interp"):
            if name not in engine_identities_before:
                continue
            raw = engine_identities_before[name]["version_probe"]["stdout"]
            try:
                reported = json.loads(raw)
            except json.JSONDecodeError as exc:
                raise SystemExit(f"{name} did not return valid version JSON") from exc
            if reported.get("commit") != args.zipp_revision:
                raise SystemExit(
                    f"{name} reports commit {reported.get('commit')!r}, "
                    f"not --zipp-revision {args.zipp_revision!r}"
                )
            if reported.get("dirty") is not False:
                raise SystemExit(
                    f"{name} is not a clean provenance build: "
                    f"dirty={reported.get('dirty')!r}"
                )

    workloads = source_workloads(args.suite)
    cases = [workload.name for workload in workloads]
    source_hashes_before = {
        workload.name: file_sha256(ROOT / workload.relative_source)
        for workload in workloads
    }
    stage, input_metadata = stage_inputs(workloads, result_path.parent)
    empty = stage / "empty.js"

    # Untimed validation also warms filesystem/binary pages.  This preserves the
    # intended fresh-process metric while keeping one engine from uniquely paying
    # first-touch I/O in its measured first repetition.
    validation: list[dict[str, Any]] = []
    expected: dict[str, str] = {}
    node_engine = available["node"]
    print("validating exact output", file=sys.stderr, flush=True)
    for workload in workloads:
        result = run_once(
            node_engine, stage / f"{workload.name}.js", timeout=args.timeout
        )
        validation.append({"engine": "node", "case": workload.name, **result})
        if not healthy(result):
            raise SystemExit(f"node validation failed on {workload.name}")
        expected[workload.name] = result["stdout_canonical_sha256"]
    for engine in engines:
        if engine.name == "node":
            continue
        for workload in workloads:
            result = run_once(
                engine, stage / f"{workload.name}.js", timeout=args.timeout
            )
            validation.append(
                {"engine": engine.name, "case": workload.name, **result}
            )
            if not healthy(result):
                raise SystemExit(
                    f"{engine.name} validation failed on {workload.name}: "
                    f"{result['stderr'][:300]}"
                )
            if result["stdout_canonical_sha256"] != expected[workload.name]:
                raise SystemExit(
                    f"{engine.name} stdout differs from node on {workload.name}"
                )

    observations: list[dict[str, Any]] = []
    schedules: list[dict[str, Any]] = []
    for rep in range(args.reps):
        bench_order = list(cases)
        random.Random(args.seed + rep).shuffle(bench_order)
        cycle = rep // len(engines)
        base = engines if cycle % 2 == 0 else list(reversed(engines))
        offset = rep % len(engines)
        ordered = [*base[offset:], *base[:offset]]
        schedules.append(
            {
                "rep": rep,
                "case_order": bench_order,
                "engine_order": [engine.name for engine in ordered],
            }
        )
        for case_position, case in enumerate(bench_order):
            source = stage / f"{case}.js"
            for engine_position, engine in enumerate(ordered):
                startup = run_once(engine, empty, timeout=args.timeout)
                full = run_once(engine, source, timeout=args.timeout)
                observation = {
                    "rep": rep,
                    "case": case,
                    "case_position": case_position,
                    "engine": engine.name,
                    "engine_position": engine_position,
                    "startup": startup,
                    "full": full,
                    "adjusted_s": full["elapsed_s"] - startup["elapsed_s"],
                }
                observations.append(observation)
                if not healthy(startup) or not healthy(full):
                    raise SystemExit(
                        f"unhealthy observation {engine.name}/{case}/rep-{rep}"
                    )
                if full["stdout_canonical_sha256"] != expected[case]:
                    raise SystemExit(
                        f"output drift {engine.name}/{case}/rep-{rep}"
                    )
        print(f"rep {rep + 1}/{args.reps} complete", file=sys.stderr, flush=True)

    source_hashes_after = {
        workload.name: file_sha256(ROOT / workload.relative_source)
        for workload in workloads
    }
    staged_hashes_after = {
        workload.name: file_sha256(stage / f"{workload.name}.js")
        for workload in workloads
    }
    if source_hashes_before != source_hashes_after:
        raise SystemExit("source benchmark changed during measurement")
    for case, metadata in input_metadata.items():
        if metadata["staged_sha256"] != staged_hashes_after[case]:
            raise SystemExit(f"staged benchmark changed during measurement: {case}")
    engine_identities_after = {
        engine.name: engine_identity(engine, args.timeout) for engine in engines
    }
    if engine_identities_before != engine_identities_after:
        raise SystemExit("engine binary or version probe changed during measurement")

    cold = {
        engine: {case: [0.0] * args.reps for case in cases}
        for engine in engine_names
    }
    startup = {
        engine: {case: [0.0] * args.reps for case in cases}
        for engine in engine_names
    }
    adjusted = {
        engine: {case: [0.0] * args.reps for case in cases}
        for engine in engine_names
    }
    for observation in observations:
        engine = observation["engine"]
        case = observation["case"]
        rep = observation["rep"]
        cold[engine][case][rep] = observation["full"]["elapsed_s"]
        startup[engine][case][rep] = observation["startup"]["elapsed_s"]
        adjusted[engine][case][rep] = observation["adjusted_s"]

    summary = {
        "cold": summarize_metric(
            cold,
            engine_names=engine_names,
            cases=cases,
            reps=args.reps,
            seed=args.seed,
            bootstrap_samples=args.bootstrap_samples,
            metric="cold",
        ),
        "startup": summarize_metric(
            startup,
            engine_names=engine_names,
            cases=cases,
            reps=args.reps,
            seed=args.seed,
            bootstrap_samples=args.bootstrap_samples,
            metric="startup",
        ),
        "adjusted": summarize_metric(
            adjusted,
            engine_names=engine_names,
            cases=cases,
            reps=args.reps,
            seed=args.seed,
            bootstrap_samples=args.bootstrap_samples,
            metric="adjusted",
        ),
    }

    revisions = {
        "zipp": args.zipp_revision,
        "quickjs-ng": args.quickjs_ng_revision,
        "boa": args.boa_revision,
    }
    revision_keys = {
        "node": None,
        "zipp-jit": "zipp",
        "zipp-interp": "zipp",
        "quickjs-ng": "quickjs-ng",
        "boa": "boa",
        "boa-opt": "boa",
    }
    artifact = {
        "schema_version": 1,
        "diagnostic_only": True,
        "created_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "method": {
            "headline": "cold fresh-process wall time with warm filesystem cache",
            "adjusted": "same-engine full time minus immediately preceding empty launch",
            "output": (
                "stdout against Node after CRLF-to-LF canonicalization only; "
                "raw bytes and hashes retained"
            ),
            "order": "cyclic counterbalance; alternating direction each full engine cycle",
            "bootstrap": (
                "descriptive percentile bootstrap; shared repetition indices "
                "across cases within each engine comparison"
            ),
            "suite": args.suite,
            "completion_suppression": (
                "append ;void 0 to identical staged source for every engine "
                "because Boa CLI prints non-undefined script completions"
            ),
        },
        "machine": machine_metadata(),
        "repository": {
            "head": git_output("rev-parse", "HEAD"),
            "status": git_output("status", "--short"),
        },
        "seed": args.seed,
        "reps": args.reps,
        "bootstrap_samples": args.bootstrap_samples,
        "cases": cases,
        "stage": str(stage),
        "inputs": input_metadata,
        "engines": {
            engine.name: {
                "command": list(engine.command),
                "environment": engine.environment,
                **engine_identities_before[engine.name],
                "revision": revisions.get(revision_keys[engine.name]),
            }
            for engine in engines
        },
        "all_correct": True,
        "validation": validation,
        "schedules": schedules,
        "summary": summary,
        "observations": observations,
    }
    temporary = result_path.with_name(f".{result_path.name}.tmp")
    temporary.write_text(json.dumps(artifact, indent=2) + "\n", encoding="utf-8")
    os.replace(temporary, result_path)

    print(f"wrote {result_path}")
    for metric in ("cold", "adjusted"):
        print(f"{metric} geomean ratios (Zipp / competitor; <1 is faster)")
        comparisons = summary[metric]["comparisons"]
        for zipp_name, competitors in comparisons.items():
            for competitor, result in competitors.items():
                if result["available"]:
                    interval = result["bootstrap_95"]
                    rendered_interval = (
                        f"[{interval[0]:.4f}, {interval[1]:.4f}]"
                        if interval is not None
                        else "[bootstrap unavailable]"
                    )
                    print(
                        f"  {zipp_name:12s} / {competitor:8s} "
                        f"{result['geomean_ratio']:.4f} {rendered_interval}"
                    )
                else:
                    print(
                        f"  {zipp_name:12s} / {competitor:8s} unavailable: "
                        f"{','.join(result['unavailable_cases'])}"
                    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
