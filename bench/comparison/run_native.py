#!/usr/bin/env python3
"""Diagnostic native comparison of Zipp against interpreters and no-JIT modes.

The public Zipp benchmark harness deliberately knows only its canonical engine
set (Node, Bun, Deno and Zipp, every one with its production JIT).  This
separate runner keeps the other comparisons explicit and avoids changing that
publication series: Zipp's interpreter (`ZIPP_NOJIT=1 ZIPP_NO_RX_JIT=1`, no
native code at all; see `ZIPP_LANES`) against QuickJS
(Bellard's and QuickJS-NG, neither of which has a JIT), Boa, and Node, Deno and
Bun with their JITs switched off (`--jitless`, `--v8-flags=--jitless`,
`BUN_JSC_useJIT=0`).  It stages identical source bytes for every engine,
checks stdout after only CRLF-to-LF canonicalization, counterbalances process
order, and retains every raw observation in JSON.
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
    # "module" runs the entry as an ES module (Zipp's `mjs` subcommand; Node
    # and QuickJS select it by the `.mjs` extension).
    goal: str = "script"
    category: str = ""
    # A suite directory staged whole, so a module's relative imports and a
    # vendored package resolve exactly as in the repository.
    suite_root: str = ""


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


# Zipp's lanes: one binary, its native tiers switched in the child's own
# environment (`controlled_environment` drops every inherited `ZIPP_` variable,
# so a setting exported around this runner never reaches a lane). Zipp has two
# native tiers with separate switches: the JavaScript JIT (`ZIPP_NOJIT`) and the
# regex JIT (`ZIPP_NO_RX_JIT`), which `ZIPP_NOJIT` alone leaves on.
#   zipp-jit            the product: both tiers on
#   zipp-jit-norx       the JavaScript JIT alone, to measure its effect apart
#                       from the regex tier's
#   zipp-interp         the strict interpreter: no native code at all, the lane
#                       an interpreter is compared with
#   zipp-interp-rxjit   the JavaScript interpreter with the regex JIT on: what
#                       `zipp-interp` ran until 2026-10-22, kept so its
#                       captures can be reproduced
ZIPP_LANES: dict[str, dict[str, Any]] = {
    "zipp-jit": {"environment": {}, "tiers": {"js_jit": True, "regex_jit": True}},
    "zipp-jit-norx": {
        "environment": {"ZIPP_NO_RX_JIT": "1"},
        "tiers": {"js_jit": True, "regex_jit": False},
    },
    "zipp-interp": {
        "environment": {"ZIPP_NOJIT": "1", "ZIPP_NO_RX_JIT": "1"},
        "tiers": {"js_jit": False, "regex_jit": False},
    },
    "zipp-interp-rxjit": {
        "environment": {"ZIPP_NOJIT": "1"},
        "tiers": {"js_jit": False, "regex_jit": True},
    },
}


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


def make_engine_for(name: str, binary: Path) -> Engine:
    """The engine `name` as `binary` runs it: its command and the settings its
    child environment gets (on top of `controlled_environment`)."""
    if name in ZIPP_LANES:
        return Engine(name, (str(binary), "js"), dict(ZIPP_LANES[name]["environment"]), binary)
    command, environment = {
        "node": ((str(binary),), {}),
        "node-jitless": ((str(binary), "--jitless"), {}),
        "deno": ((str(binary), "run", "-q"), {}),
        "deno-jitless": ((str(binary), "run", "-q", "--v8-flags=--jitless"), {}),
        "bun": ((str(binary),), {}),
        # JavaScriptCore's option switch: every JIT tier off, LLInt only.
        "bun-nojit": ((str(binary),), {"BUN_JSC_useJIT": "0"}),
        "quickjs": ((str(binary),), {}),
        "quickjs-ng": ((str(binary),), {}),
        "boa": ((str(binary),), {}),
        "boa-opt": ((str(binary), "--optimize"), {}),
    }[name]
    return Engine(name, command, environment, binary)


def read_manifest(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8-sig"))


def source_workloads(suite: str, sizes: tuple[str, ...] = ("m",)) -> tuple[Workload, ...]:
    if suite == "micro5":
        return MICRO5
    if suite == "real13":
        return tuple(
            Workload(name, f"bench/real/{name}.js") for name in REAL13
        )
    if suite == "hostile17":
        # The hostile suite's own manifest, unchanged: each case's declared
        # entry and goal, every file of the suite staged alongside.
        manifest = read_manifest(ROOT / "bench/hostile/manifest.json")
        return tuple(
            Workload(
                case["id"],
                f"bench/hostile/{case['entry']}",
                goal=case.get("goal", "module" if case["entry"].endswith(".mjs") else "script"),
                category=case.get("category", ""),
                suite_root="bench/hostile",
            )
            for case in manifest["cases"]
        )
    if suite == "expansion":
        # The expansion suite (bench/expansion/manifest.json): every case at
        # each requested size, the size substituted into the case's single
        # `const SIZE = <n>;` line. A case's medium size is its source as
        # committed.
        manifest = read_manifest(ROOT / "bench/expansion/manifest.json")
        out = []
        for case in manifest["cases"]:
            case_sizes = case["sizes"]
            for size in sizes:
                if size not in case_sizes:
                    continue
                token_old = f"const SIZE = {case_sizes['m']};"
                token_new = f"const SIZE = {case_sizes[size]};"
                out.append(
                    Workload(
                        f"{case['id']}@{size}",
                        f"bench/expansion/{case['entry']}",
                        () if size == "m" else ((token_old, token_new, 1),),
                        goal=case.get("goal", "script"),
                        category=case.get("category", ""),
                        suite_root="bench/expansion",
                    )
                )
        return tuple(out)
    raise ValueError(f"unknown suite {suite}")


def staged_name(workload: Workload) -> str:
    """The staged entry's path relative to the stage directory."""
    if not workload.suite_root:
        return f"{workload.name}.js"
    relative = Path(workload.relative_source).relative_to(workload.suite_root)
    if "@" in workload.name:
        # Size variants of one source are staged side by side.
        size = workload.name.rsplit("@", 1)[1]
        relative = relative.with_name(f"{relative.stem}@{size}{relative.suffix}")
    return relative.as_posix()


def stage_inputs(
    workloads: tuple[Workload, ...], result_dir: Path
) -> tuple[Path, dict[str, dict[str, Any]]]:
    stage = Path(
        tempfile.mkdtemp(prefix="zipp-competitor-inputs-", dir=result_dir)
    )
    metadata: dict[str, dict[str, Any]] = {}
    empty = stage / "empty.js"
    empty.write_bytes(b";void 0;\n")
    # Suites staged whole: their modules' imports and vendored packages.
    for root in sorted({w.suite_root for w in workloads if w.suite_root}):
        shutil.copytree(
            ROOT / root,
            stage,
            dirs_exist_ok=True,
            ignore=shutil.ignore_patterns("*.json", "*.md", "node_modules"),
        )
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
        destination = stage / staged_name(workload)
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(staged_bytes)
        metadata[workload.name] = {
            "goal": workload.goal,
            "category": workload.category,
            "source": str(source),
            "source_sha256": bytes_sha256(raw),
            "source_bytes": len(raw),
            "staged": str(destination),
            "staged_sha256": bytes_sha256(staged_bytes),
            "staged_bytes": len(staged_bytes),
            "replacements": applied,
            "completion_suppression": ";void 0;",
        }
    for path in stage.rglob("*"):
        if path.is_file():
            path.chmod(stat.S_IREAD)
    return stage, metadata


def process_counters(handle: int) -> dict[str, Any]:
    """CPU cycles and peak working set of an exited process whose handle is
    still open (Windows); empty elsewhere. Cycles are what the process ran,
    whatever else the machine did meanwhile."""
    if os.name != "nt":
        return {}
    import ctypes
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    cycles = ctypes.c_ulonglong(0)
    counters = Counters()
    counters.cb = ctypes.sizeof(Counters)
    out: dict[str, Any] = {}
    if ctypes.windll.kernel32.QueryProcessCycleTime(wintypes.HANDLE(handle), ctypes.byref(cycles)):
        out["cycles"] = cycles.value
    if ctypes.windll.psapi.GetProcessMemoryInfo(
        wintypes.HANDLE(handle), ctypes.byref(counters), counters.cb
    ):
        out["peak_working_set_bytes"] = counters.PeakWorkingSetSize
        out["peak_pagefile_bytes"] = counters.PeakPagefileUsage
    return out


def engine_command(engine: Engine, source: Path) -> list[str]:
    command = list(engine.command)
    # Zipp names the goal by subcommand; the other engines take a `.mjs`
    # entry as a module.
    if engine.name.startswith("zipp-") and source.suffix == ".mjs":
        command[1] = "mjs"
    return [*command, str(source)]


def run_once(
    engine: Engine,
    source: Path,
    *,
    timeout: float,
) -> dict[str, Any]:
    command = engine_command(engine, source)
    started = time.perf_counter_ns()
    try:
        process = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=controlled_environment(engine.environment),
            cwd=source.parent,
        )
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            process.kill()
            stdout, stderr = process.communicate()
            raise subprocess.TimeoutExpired(command, timeout, stdout, stderr)
        elapsed = (time.perf_counter_ns() - started) / 1_000_000_000
        counters = process_counters(int(process._handle)) if os.name == "nt" else {}
        canonical = canonical_stdout(stdout)
        return {
            "elapsed_s": elapsed,
            "returncode": process.returncode,
            "timed_out": False,
            **counters,
            "stdout_sha256": bytes_sha256(stdout),
            "stdout_bytes": len(stdout),
            "stdout_canonical_sha256": bytes_sha256(canonical),
            "stdout_canonical_bytes": len(canonical),
            "stdout": stdout.decode("utf-8", errors="replace"),
            "stderr": stderr.decode("utf-8", errors="replace"),
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


# Bellard's QuickJS CLI has no version flag: `--version` prints its version line
# and usage, then exits 1. The probe records that output; the exit status is
# not a failure for this engine.
PROBE_NONZERO_OK = frozenset({"quickjs"})


def version_probe(engine: Engine, timeout: float) -> dict[str, Any]:
    if engine.name.startswith("zipp-"):
        suffix = ("--version", "--json")
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
    for numerator in ZIPP_LANES:
        if numerator not in engine_names:
            continue
        comparisons[numerator] = {}
        for competitor in engine_names:
            if competitor == numerator:
                continue
            # Zipp lanes are compared with the other engines, and each other
            # lane with the product's.
            if competitor in ZIPP_LANES and competitor != "zipp-jit":
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
    parser.add_argument("--quickjs", type=Path, help="Bellard's QuickJS qjs")
    parser.add_argument("--quickjs-ng", type=Path)
    parser.add_argument("--boa", type=Path)
    parser.add_argument("--node", default="node")
    parser.add_argument("--deno", default="deno")
    parser.add_argument("--bun", default="bun")
    parser.add_argument(
        "--suite", choices=("micro5", "real13", "hostile17", "expansion"), default="micro5"
    )
    parser.add_argument(
        "--sizes",
        default="m",
        help="expansion suite: comma-separated size variants to run (s, m, l)",
    )
    parser.add_argument(
        "--cases",
        help="comma-separated case names to keep (default: every case of the suite)",
    )
    parser.add_argument(
        "--skip-unsupported",
        action="store_true",
        help=(
            "record a case an engine fails or answers differently as excluded "
            "(with the reason) and time the rest, instead of stopping"
        ),
    )
    parser.add_argument("--reps", type=int, default=12)
    parser.add_argument("--bootstrap-samples", type=int, default=10_000)
    parser.add_argument("--seed", type=lambda value: int(value, 0), default=DEFAULT_SEED)
    parser.add_argument("--timeout", type=float, default=300.0)
    parser.add_argument("--json", type=Path, required=True)
    parser.add_argument("--overwrite", action="store_true")
    parser.add_argument(
        "--engines",
        default="node,zipp-jit,zipp-interp,quickjs-ng,boa,boa-opt",
        help=(
            "comma-separated subset of node, node-jitless, deno, deno-jitless, "
            "bun, bun-nojit, zipp-jit, zipp-jit-norx, zipp-interp, "
            "zipp-interp-rxjit, quickjs, quickjs-ng, boa, boa-opt (default: "
            "the original six)"
        ),
    )
    parser.add_argument("--zipp-revision")
    parser.add_argument("--quickjs-revision")
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

    requested = args.engines.split(",")
    # Each engine is built only when requested, so a run needs only the
    # binaries it names.
    def binary_for(name: str) -> Path:
        flag = args.zipp if name in ZIPP_LANES else {
            "node": args.node,
            "node-jitless": args.node,
            "deno": args.deno,
            "deno-jitless": args.deno,
            "bun": args.bun,
            "bun-nojit": args.bun,
            "quickjs": args.quickjs,
            "quickjs-ng": args.quickjs_ng,
            "boa": args.boa,
            "boa-opt": args.boa,
        }[name]
        if flag is None:
            raise SystemExit(f"engine {name} needs its binary flag")
        return executable(flag)

    def make_engine(name: str) -> Engine:
        return make_engine_for(name, binary_for(name))

    known = {
        "node", "node-jitless", "deno", "deno-jitless", "bun", "bun-nojit",
        "quickjs", "quickjs-ng", "boa", "boa-opt", *ZIPP_LANES,
    }
    unknown = sorted(set(requested) - known)
    if unknown or not requested or len(requested) != len(set(requested)):
        raise SystemExit(f"invalid --engines list; unknown={unknown}")
    available = {name: make_engine(name) for name in requested}
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
        and name not in PROBE_NONZERO_OK
    ]
    if failed_version_probes:
        raise SystemExit(
            "version probe failed for: " + ", ".join(failed_version_probes)
        )
    if args.zipp_revision:
        for name in ZIPP_LANES:
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

    workloads = source_workloads(args.suite, tuple(args.sizes.split(",")))
    if args.cases:
        keep = set(args.cases.split(","))
        workloads = tuple(w for w in workloads if w.name in keep or w.name.split("@")[0] in keep)
    cases = [workload.name for workload in workloads]
    source_hashes_before = {
        workload.name: file_sha256(ROOT / workload.relative_source)
        for workload in workloads
    }
    stage, input_metadata = stage_inputs(workloads, result_path.parent)
    empty = stage / "empty.js"
    staged = {workload.name: stage / staged_name(workload) for workload in workloads}
    excluded: dict[str, list[dict[str, str]]] = {}

    # Untimed validation also warms filesystem/binary pages.  This preserves the
    # intended fresh-process metric while keeping one engine from uniquely paying
    # first-touch I/O in its measured first repetition.
    validation: list[dict[str, Any]] = []
    expected: dict[str, str] = {}
    node_engine = available["node"]
    print("validating exact output", file=sys.stderr, flush=True)
    def reject(engine_name: str, case: str, reason: str) -> None:
        if not args.skip_unsupported:
            raise SystemExit(f"{engine_name} {reason} on {case}")
        excluded.setdefault(case, []).append({"engine": engine_name, "reason": reason})

    for workload in workloads:
        result = run_once(
            node_engine, staged[workload.name], timeout=args.timeout
        )
        validation.append({"engine": "node", "case": workload.name, **result})
        if not healthy(result):
            reject("node", workload.name, "validation failed: " + result["stderr"][:300])
            continue
        expected[workload.name] = result["stdout_canonical_sha256"]
    for engine in engines:
        if engine.name == "node":
            continue
        for workload in workloads:
            if workload.name not in expected:
                continue
            result = run_once(
                engine, staged[workload.name], timeout=args.timeout
            )
            validation.append(
                {"engine": engine.name, "case": workload.name, **result}
            )
            if not healthy(result):
                reject(engine.name, workload.name, "validation failed: " + result["stderr"][:300])
            elif result["stdout_canonical_sha256"] != expected[workload.name]:
                reject(engine.name, workload.name, "stdout differs from node")
    if excluded:
        print(f"excluded: {sorted(excluded)}", file=sys.stderr, flush=True)
    cases = [case for case in cases if case not in excluded]
    if not cases:
        raise SystemExit("every case was excluded")

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
            source = staged[case]
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
        workload.name: file_sha256(staged[workload.name])
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
    # Process CPU cycles (Windows): what each run itself executed, full run
    # less its preceding empty launch, independent of what else ran.
    have_cycles = all("cycles" in o["full"] and "cycles" in o["startup"] for o in observations)
    adjusted_cycles = {
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
        if have_cycles:
            adjusted_cycles[engine][case][rep] = max(
                1.0, float(observation["full"]["cycles"] - observation["startup"]["cycles"])
            )

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
    if have_cycles:
        summary["adjusted_cycles"] = summarize_metric(
            adjusted_cycles,
            engine_names=engine_names,
            cases=cases,
            reps=args.reps,
            seed=args.seed,
            bootstrap_samples=args.bootstrap_samples,
            metric="adjusted_cycles",
        )

    revisions = {
        "zipp": args.zipp_revision,
        "quickjs": args.quickjs_revision,
        "quickjs-ng": args.quickjs_ng_revision,
        "boa": args.boa_revision,
    }
    revision_keys = {
        "node": None,
        "node-jitless": None,
        "deno": None,
        "deno-jitless": None,
        "bun": None,
        "bun-nojit": None,
        **{lane: "zipp" for lane in ZIPP_LANES},
        "quickjs": "quickjs",
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
        "sizes": args.sizes if args.suite == "expansion" else None,
        "excluded": excluded,
        "stage": str(stage),
        "inputs": input_metadata,
        "engines": {
            engine.name: {
                "command": list(engine.command),
                "environment": engine.environment,
                # Which of Zipp's native tiers the lane ran (`ZIPP_LANES`).
                "zipp_tiers": ZIPP_LANES.get(engine.name, {}).get("tiers"),
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
