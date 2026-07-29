#!/usr/bin/env python3
"""Counterbalanced benchmark harness for ``bench/real/*.js``.

The primary metric is cold wall time.  A paired empty launch is also collected
immediately before every full launch, which keeps the historical
startup-adjusted metric available without subtracting an unrelated median.
Every schedule decision and raw observation is retained in schema-v2 JSON.

Examples:

  python tools/bench.py --reps 15 --json bench/run.json
  python tools/bench.py --ab old.exe new.exe --reps 21
  python tools/bench.py --metric adjusted --readme
  python tools/bench.py --read-json bench/results.json --historical
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import os
import platform
import random
import signal
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Iterable


SCHEMA_VERSION = 2
DEFAULT_SEED = 0x5A17_2026
BOOTSTRAP_SAMPLES = 10_000
ROOT = Path(__file__).resolve().parent.parent
BENCH_DIR = ROOT / "bench" / "real"


def discover_benches(bench_dir: Path = BENCH_DIR) -> list[str]:
    return sorted(path.stem for path in bench_dir.glob("*.js"))


def percentile(xs: Iterable[float], quantile: float) -> float:
    values = sorted(xs)
    if not values:
        raise ValueError("percentile requires at least one value")
    if len(values) == 1:
        return values[0]
    index = (len(values) - 1) * quantile
    lo = int(index)
    hi = min(lo + 1, len(values) - 1)
    return values[lo] + (values[hi] - values[lo]) * (index - lo)


def geometric_mean(values: Iterable[float]) -> float:
    values = list(values)
    if not values or any(value <= 0 for value in values):
        raise ValueError("geometric mean requires positive values")
    return math.exp(sum(math.log(value) for value in values) / len(values))


def nonnegative_finite_float(value: Any, label: str) -> float:
    try:
        parsed = float(value)
    except (TypeError, ValueError) as exc:
        raise ValueError(f"{label} is not numeric") from exc
    if not math.isfinite(parsed) or parsed < 0:
        raise ValueError(f"{label} must be finite and nonnegative")
    return parsed


def strict_int(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"{label} must be an integer")
    return value


def derived_seed(seed: int, *labels: str) -> int:
    encoded = "\0".join(labels).encode("utf-8")
    suffix = int.from_bytes(hashlib.sha256(encoded).digest()[:8], "big")
    return seed ^ suffix


def paired_ratios(numerators: list[float], denominators: list[float]) -> list[float]:
    if len(numerators) != len(denominators):
        raise ValueError("paired samples have different lengths")
    if any(num <= 0 or den <= 0 for num, den in zip(numerators, denominators)):
        raise ValueError("paired ratios require positive samples")
    return [num / den for num, den in zip(numerators, denominators)]


def bootstrap_median_ci(
    ratios: list[float],
    *,
    seed: int,
    samples: int = BOOTSTRAP_SAMPLES,
) -> tuple[float, float]:
    """Return a deterministic paired-bootstrap 95% CI for a median ratio."""
    if not ratios:
        raise ValueError("bootstrap requires at least one ratio")
    if len(ratios) == 1:
        return ratios[0], ratios[0]
    rng = random.Random(seed)
    size = len(ratios)
    medians = [
        statistics.median(ratios[rng.randrange(size)] for _ in range(size))
        for _ in range(samples)
    ]
    return percentile(medians, 0.025), percentile(medians, 0.975)


def bootstrap_geomean_of_medians_ci(
    ratios_by_bench: list[list[float]],
    *,
    seed: int,
    samples: int = BOOTSTRAP_SAMPLES,
) -> tuple[float, float]:
    """Cluster-bootstrap the suite geomean by paired repetition."""
    if not ratios_by_bench or not ratios_by_bench[0]:
        raise ValueError("suite bootstrap requires paired ratios")
    reps = len(ratios_by_bench[0])
    if any(len(ratios) != reps for ratios in ratios_by_bench):
        raise ValueError("suite bootstrap requires equal repetition counts")
    if any(ratio <= 0 for ratios in ratios_by_bench for ratio in ratios):
        raise ValueError("suite bootstrap requires positive ratios")
    if reps == 1:
        point = geometric_mean(
            statistics.median(ratios) for ratios in ratios_by_bench
        )
        return point, point

    rng = random.Random(seed)
    geomeans = []
    for _ in range(samples):
        indexes = [rng.randrange(reps) for _ in range(reps)]
        geomeans.append(
            geometric_mean(
                statistics.median(ratios[index] for index in indexes)
                for ratios in ratios_by_bench
            )
        )
    return percentile(geomeans, 0.025), percentile(geomeans, 0.975)


def engine_order_for_rep(
    engines: list[tuple[str, list[str]]], rep: int, seed: int
) -> list[tuple[str, list[str]]]:
    """Counterbalance two engines exactly; deterministically shuffle larger sets."""
    order = list(engines)
    if len(order) == 2:
        if rep % 2:
            order.reverse()
        return order
    random.Random(seed + rep * 0x9E37_79B1).shuffle(order)
    return order


def run_once(
    cmd: list[str],
    path: Path,
    *,
    timeout: float,
    env: dict[str, str] | None = None,
) -> dict[str, Any]:
    child_env = dict(os.environ)
    if env:
        child_env.update(env)
    start = time.perf_counter()
    popen_options: dict[str, Any] = {}
    if os.name == "nt":
        popen_options["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
    else:
        popen_options["start_new_session"] = True
    try:
        process = subprocess.Popen(
            cmd + [str(path)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=child_env,
            **popen_options,
        )
        stdout, stderr = process.communicate(timeout=timeout)
        return {
            "elapsed_s": time.perf_counter() - start,
            "stdout": stdout,
            "stderr": stderr,
            "returncode": process.returncode,
            "timed_out": False,
            "spawn_error": False,
        }
    except OSError as exc:
        return {
            "elapsed_s": time.perf_counter() - start,
            "stdout": b"",
            "stderr": str(exc).encode("utf-8", errors="replace"),
            "returncode": None,
            "timed_out": False,
            "spawn_error": True,
        }
    except subprocess.TimeoutExpired:
        if os.name == "nt":
            try:
                subprocess.run(
                    [
                        "taskkill",
                        "/PID",
                        str(process.pid),
                        "/T",
                        "/F",
                    ],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=min(timeout, 10.0),
                    check=False,
                )
            except (OSError, subprocess.TimeoutExpired):
                pass
        else:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except (OSError, ProcessLookupError):
                pass
        if process.poll() is None:
            process.kill()
        stdout, stderr = process.communicate()
        return {
            "elapsed_s": time.perf_counter() - start,
            "stdout": stdout,
            "stderr": stderr,
            "returncode": None,
            "timed_out": True,
            "spawn_error": False,
        }


def file_digest(path: Path) -> str | None:
    if not path.is_file():
        return None
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def resolved_executable(cmd: list[str]) -> Path | None:
    raw = Path(cmd[0])
    if raw.is_file():
        return raw.resolve()
    found = shutil.which(cmd[0])
    return Path(found).resolve() if found else None


def engine_metadata(name: str, cmd: list[str], timeout: float) -> dict[str, Any]:
    executable = resolved_executable(cmd)
    stat = executable.stat() if executable and executable.is_file() else None
    version = None
    version_probe_error = None
    if executable:
        try:
            probe = subprocess.run(
                [str(executable), "--version"],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                timeout=min(timeout, 10.0),
                check=False,
            )
            version_bytes = probe.stdout.strip() or probe.stderr.strip()
            if probe.returncode == 0 and version_bytes:
                version = version_bytes.decode(
                    "utf-8", errors="replace"
                ).splitlines()[0]
            elif probe.returncode != 0:
                version_probe_error = (
                    version_bytes.decode("utf-8", errors="replace").splitlines()[0]
                    if version_bytes
                    else f"version probe exited {probe.returncode}"
                )
        except (OSError, subprocess.TimeoutExpired):
            version_probe_error = "version probe failed"
    return {
        "name": name,
        "argv": cmd,
        "executable": str(executable) if executable else None,
        "version": version,
        "version_probe_error": version_probe_error,
        "size": stat.st_size if stat else None,
        "mtime_ns": stat.st_mtime_ns if stat else None,
        "sha256": file_digest(executable) if executable else None,
        # `zipp --version --json`: the binary's own account of the SOURCE it was
        # built from, including a digest of any uncommitted diff. The sha256
        # above identifies the file; this identifies the code. A benchmark
        # artifact recording only the parent commit for a dirty build is how a
        # result came to name the wrong source (PERF_ROADMAP B61).
        "build_identity": build_identity(executable, timeout),
    }


def build_identity(executable: Path | None, timeout: float) -> dict[str, Any] | None:
    """`zipp --version --json`, parsed. ``None`` for an engine without it (node)."""
    if not executable:
        return None
    try:
        probe = subprocess.run(
            [str(executable), "--version", "--json"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=min(timeout, 10.0),
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if probe.returncode != 0:
        return None
    try:
        parsed = json.loads(probe.stdout.decode("utf-8", errors="replace"))
    except (ValueError, UnicodeDecodeError):
        return None
    return parsed if isinstance(parsed, dict) else None


def reject_identical_ab_binaries(
    ab: list[str], ab_env: tuple[dict[str, str], dict[str, str]], *, allow: bool
) -> None:
    """Refuse an ``--ab`` whose two sides are the same executable.

    This is a hard error before any measurement because the failure mode is
    silent and expensive: a `git stash`/rebuild cycle that forgets to rebuild
    afterwards leaves both sides pointing at the same binary, every correctness
    gate "passes" because it is comparing a build against itself, and the only
    tell is a ratio that fails to move (PERF_ROADMAP B61).

    Two identical binaries ARE legitimate for an A/A calibration run and when the
    two sides differ only by ``--ab-env`` (the ablation-pricing idiom), so those
    pass: an explicit ``--allow-aa``, or per-side environments that actually
    differ.
    """
    old_path, new_path = resolved_executable([ab[0]]), resolved_executable([ab[1]])
    old_hash, new_hash = file_digest(old_path) if old_path else None, (
        file_digest(new_path) if new_path else None
    )
    if old_hash is None or new_hash is None or old_hash != new_hash:
        return
    if allow or ab_env[0] != ab_env[1]:
        return
    raise SystemExit(
        "refusing --ab: both sides are the same executable and no --ab-env "
        f"distinguishes them.\n  old: {old_path}\n  new: {new_path}\n"
        f"  sha256: {old_hash}\n"
        "This measures a build against itself. Rebuild the side you meant to "
        "change (and check `zipp --version` afterwards), or pass --allow-aa for "
        "a deliberate A/A calibration."
    )


def parse_env_assignments(value: str) -> dict[str, str]:
    if value in ("", "-"):
        return {}
    result: dict[str, str] = {}
    for assignment in value.split(","):
        key, separator, item = assignment.partition("=")
        if not separator or not key:
            raise argparse.ArgumentTypeError(
                f"expected comma-separated KEY=VALUE assignments, got {value!r}"
            )
        result[key] = item
    return result


def git_revision() -> str | None:
    try:
        result = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=5,
            check=False,
            text=True,
        )
        return result.stdout.strip() if result.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        return None


def power_mode() -> str | None:
    if os.name != "nt":
        governor = Path("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        try:
            return governor.read_text(encoding="utf-8").strip()
        except OSError:
            return None
    try:
        result = subprocess.run(
            ["powercfg", "/getactivescheme"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=5,
            check=False,
            text=True,
        )
        return result.stdout.strip() or None
    except (OSError, subprocess.TimeoutExpired):
        return None


def relevant_environment() -> dict[str, str]:
    prefixes = ("ZIPP_", "RUST", "MIMALLOC_", "NODE_", "DENO_", "BUN_")
    return {
        key: value
        for key, value in sorted(os.environ.items())
        if key.startswith(prefixes)
    }


def create_empty_script() -> Path:
    handle = tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        suffix=".js",
        prefix="zipp-bench-empty-",
        delete=False,
    )
    try:
        handle.write("\n")
        return Path(handle.name)
    finally:
        handle.close()


def decode_stderr(data: bytes, limit: int = 16_384) -> str:
    suffix = b"" if len(data) <= limit else b"\n...[truncated]"
    return (data[:limit] + suffix).decode("utf-8", errors="replace")


def result_is_healthy(result: dict[str, Any]) -> bool:
    return not result["timed_out"] and result["returncode"] == 0


def validate_comparison(
    engine_names: list[str],
    baseline: str,
    compare_name: str,
) -> None:
    if not engine_names or any(not name for name in engine_names):
        raise ValueError("engine subset must contain nonempty names")
    if len(set(engine_names)) != len(engine_names):
        raise ValueError("engine subset contains duplicate names")
    if len(engine_names) < 2:
        raise ValueError("at least two engines are required")
    if baseline not in engine_names:
        raise ValueError(
            f"baseline {baseline!r} is unavailable; found {', '.join(engine_names)}"
        )
    if compare_name not in engine_names:
        raise ValueError(
            f"comparison target {compare_name!r} is unavailable; "
            f"found {', '.join(engine_names)}"
        )
    if baseline == compare_name:
        raise ValueError("baseline and comparison target must be different engines")


def metric_summary(
    samples: dict[str, dict[str, list[float]]],
    engine_names: list[str],
    bench: str,
    baseline: str,
    compare_name: str,
    *,
    seed: int,
    bootstrap_samples: int,
) -> dict[str, Any]:
    medians_ms = {
        name: statistics.median(samples[name][bench]) * 1000
        for name in engine_names
    }
    paired = list(zip(samples[compare_name][bench], samples[baseline][bench]))
    nonpositive = sum(num <= 0 or den <= 0 for num, den in paired)
    if nonpositive:
        return {
            "median_ms": medians_ms,
            "paired_ratio": None,
            "paired_ratio_ci95": None,
            "nonpositive_pairs": nonpositive,
        }
    ratios = paired_ratios(
        samples[compare_name][bench],
        samples[baseline][bench],
    )
    ratio = statistics.median(ratios)
    ci_low, ci_high = bootstrap_median_ci(
        ratios,
        seed=seed,
        samples=bootstrap_samples,
    )
    return {
        "median_ms": medians_ms,
        "paired_ratio": ratio,
        "paired_ratio_ci95": [ci_low, ci_high],
        "nonpositive_pairs": 0,
    }


def normalize_result_data(data: dict[str, Any]) -> dict[str, Any]:
    """Normalize schema-v1/v2 JSON into the live harness's sample tables."""
    schema = strict_int(
        data.get("schema_version", 1),
        "benchmark schema version",
    )
    if schema not in (1, SCHEMA_VERSION):
        raise ValueError(f"unsupported benchmark schema version: {schema}")

    raw_benches = data.get("benches", [])
    if (
        not isinstance(raw_benches, list)
        or any(not isinstance(bench, str) or not bench for bench in raw_benches)
        or len(set(raw_benches)) != len(raw_benches)
    ):
        raise ValueError("benchmark result has invalid benches")
    benches = list(raw_benches)
    raw_engines = data.get("engines", [])
    if not isinstance(raw_engines, list):
        raise ValueError("benchmark result engines must be an array")
    if schema == 1:
        engine_names = list(raw_engines)
    else:
        engine_names = [
            engine.get("name") if isinstance(engine, dict) else engine
            for engine in raw_engines
        ]
    if (
        not benches
        or not engine_names
        or any(not isinstance(name, str) or not name for name in engine_names)
        or len(set(engine_names)) != len(engine_names)
    ):
        raise ValueError("benchmark result has no benches or engines")
    reps = strict_int(data.get("reps", 0), "benchmark repetition count")
    if reps < 1:
        raise ValueError("benchmark result has no positive repetition count")

    cold = {name: {bench: [] for bench in benches} for name in engine_names}
    startup = {name: {bench: [] for bench in benches} for name in engine_names}
    adjusted = {name: {bench: [] for bench in benches} for name in engine_names}
    health_failures: list[str] = []
    correctness_failures: list[str] = []

    if schema == 1:
        raw_cold = data.get("samples", {})
        raw_startup = data.get("startup_s", {})
        if not isinstance(raw_cold, dict) or not isinstance(raw_startup, dict):
            raise ValueError("schema-v1 samples/startup_s must be objects")
        for name in engine_names:
            launch_values = raw_startup.get(name, [])
            if not isinstance(launch_values, list):
                raise ValueError(
                    f"schema-v1 startup samples for {name} must be an array"
                )
            launches = [
                nonnegative_finite_float(
                    value,
                    f"schema-v1 startup {name} sample {index}",
                )
                for index, value in enumerate(launch_values)
            ]
            for bench in benches:
                engine_samples = raw_cold.get(name, {})
                if not isinstance(engine_samples, dict):
                    raise ValueError(
                        f"schema-v1 samples for {name} must be an object"
                    )
                run_values = engine_samples.get(bench, [])
                if not isinstance(run_values, list):
                    raise ValueError(
                        f"schema-v1 samples for {name}/{bench} must be an array"
                    )
                runs = [
                    nonnegative_finite_float(
                        value,
                        f"schema-v1 cold {name}/{bench} sample {index}",
                    )
                    for index, value in enumerate(run_values)
                ]
                if len(runs) != len(launches):
                    raise ValueError(
                        f"schema-v1 sample count mismatch for {name}/{bench}: "
                        f"{len(runs)} runs, {len(launches)} startups"
                    )
                cold[name][bench].extend(runs)
                startup[name][bench].extend(launches)
                adjusted[name][bench].extend(
                    run - launch for run, launch in zip(runs, launches)
                )
        baseline = data.get("baseline") or (
            "old" if "old" in engine_names and "new" in engine_names else None
        )
    else:
        seen: set[tuple[int, str, str]] = set()
        measurements = {
            name: {bench: {} for bench in benches}
            for name in engine_names
        }
        output_hashes = {
            name: {bench: set() for bench in benches}
            for name in engine_names
        }
        raw_observations = data.get("observations", [])
        if not isinstance(raw_observations, list):
            raise ValueError("schema-v2 observations must be an array")
        for observation in raw_observations:
            if not isinstance(observation, dict):
                raise ValueError("schema-v2 observation must be an object")
            name = observation.get("engine")
            bench = observation.get("bench")
            if name not in cold or bench not in cold[name]:
                raise ValueError(f"unknown observation target: {name}/{bench}")
            try:
                rep = strict_int(
                    observation["rep"],
                    f"observation repetition for {name}/{bench}",
                )
            except (KeyError, ValueError) as exc:
                raise ValueError(
                    f"invalid observation repetition for {name}/{bench}"
                ) from exc
            if not 0 <= rep < reps:
                raise ValueError(
                    f"observation repetition out of range for {name}/{bench}: {rep}"
                )
            observation_key = (rep, name, bench)
            if observation_key in seen:
                raise ValueError(
                    f"duplicate observation for rep {rep}, {name}/{bench}"
                )
            seen.add(observation_key)

            healthy = (
                not observation.get("startup_timed_out", False)
                and not observation.get("startup_spawn_error", False)
                and observation.get("startup_returncode") == 0
                and not observation.get("timed_out", False)
                and not observation.get("spawn_error", False)
                and observation.get("returncode") == 0
            )
            declared_healthy = observation.get("valid_for_stats")
            if (
                declared_healthy is not None
                and bool(declared_healthy) != healthy
            ):
                health_failures.append(
                    f"contradictory health marker for {name}/{bench}, rep {rep}"
                )
            if not healthy:
                health_failures.append(
                    f"invalid observation for {name}/{bench}, rep {rep}"
                )
                continue
            stdout_hash = observation.get("stdout_sha256")
            if (
                not isinstance(stdout_hash, str)
                or len(stdout_hash) != 64
                or any(ch not in "0123456789abcdefABCDEF" for ch in stdout_hash)
            ):
                correctness_failures.append(
                    f"missing or invalid stdout digest for {name}/{bench}, rep {rep}"
                )
            else:
                output_hashes[name][bench].add(stdout_hash.lower())
            for field in ("startup_stdout_bytes", "stdout_bytes"):
                if field in observation:
                    value = observation[field]
                    if (
                        isinstance(value, bool)
                        or not isinstance(value, int)
                        or value < 0
                    ):
                        raise ValueError(
                            f"{field} must be a nonnegative integer for "
                            f"{name}/{bench}, rep {rep}"
                        )
            launch = nonnegative_finite_float(
                observation.get("startup_s"),
                f"startup_s for {name}/{bench}, rep {rep}",
            )
            run = nonnegative_finite_float(
                observation.get("cold_s"),
                f"cold_s for {name}/{bench}, rep {rep}",
            )
            measurements[name][bench][rep] = (launch, run)
        baseline = data.get("baseline")

        expected = {
            (rep, name, bench)
            for rep in range(reps)
            for name in engine_names
            for bench in benches
        }
        missing = expected - seen
        if missing:
            preview = ", ".join(
                f"rep {rep} {name}/{bench}"
                for rep, name, bench in sorted(missing)[:3]
            )
            raise ValueError(
                f"schema-v2 result is missing {len(missing)} observation(s): "
                f"{preview}"
            )
        if not isinstance(baseline, str) or baseline not in output_hashes:
            raise ValueError(f"schema-v2 result has invalid baseline: {baseline!r}")
        for name in engine_names:
            for bench in benches:
                digests = output_hashes[name][bench]
                if len(digests) > 1:
                    correctness_failures.append(
                        f"{name} output not reproducible on {bench}"
                    )
                if (
                    name != baseline
                    and digests
                    and output_hashes[baseline][bench]
                    and digests != output_hashes[baseline][bench]
                ):
                    correctness_failures.append(
                        f"{name} output differs from {baseline} on {bench}"
                    )
                for rep in sorted(measurements[name][bench]):
                    launch, run = measurements[name][bench][rep]
                    cold[name][bench].append(run)
                    startup[name][bench].append(launch)
                    adjusted[name][bench].append(run - launch)

    for name in engine_names:
        for bench in benches:
            if not cold[name][bench] and not health_failures:
                raise ValueError(
                    f"benchmark result has no valid samples for {name}/{bench}"
                )

    failure_fields: dict[str, list[str]] = {}
    for field in ("health_failures", "correctness_failures", "failures"):
        values = data.get(field, [])
        if (
            not isinstance(values, list)
            or any(not isinstance(value, str) for value in values)
        ):
            raise ValueError(f"benchmark result {field} must be an array of strings")
        failure_fields[field] = values
    health_failures.extend(failure_fields["health_failures"])
    correctness_failures.extend(failure_fields["correctness_failures"])
    failures = list(failure_fields["failures"])
    failures.extend(health_failures)
    failures.extend(correctness_failures)
    stored_all_correct = data.get("all_correct", False)
    if not isinstance(stored_all_correct, bool):
        raise ValueError("benchmark result all_correct must be boolean")
    stored_seed = data.get("seed", DEFAULT_SEED)
    if stored_seed is None:
        stored_seed = DEFAULT_SEED
    stored_seed = strict_int(stored_seed, "benchmark seed")
    stored_bootstrap = data.get("bootstrap_samples", BOOTSTRAP_SAMPLES)
    if stored_bootstrap is None:
        stored_bootstrap = BOOTSTRAP_SAMPLES
    stored_bootstrap = strict_int(
        stored_bootstrap,
        "benchmark bootstrap sample count",
    )
    if stored_bootstrap < 1:
        raise ValueError("benchmark bootstrap sample count must be positive")
    stored_metric = data.get(
        "headline_metric",
        data.get("metric", "cold"),
    )
    if stored_metric not in ("cold", "adjusted", "historical-adjusted"):
        raise ValueError(f"benchmark result has invalid metric: {stored_metric!r}")
    return {
        "schema_version": schema,
        "reps": reps,
        "benches": benches,
        "engine_names": engine_names,
        "baseline": baseline,
        "seed": stored_seed,
        "bootstrap_samples": stored_bootstrap,
        "headline_metric": stored_metric,
        "cold": cold,
        "startup": startup,
        "adjusted": adjusted,
        "all_correct": (
            stored_all_correct
            and not health_failures
            and not correctness_failures
        ),
        "has_health_failures": bool(health_failures),
        "has_correctness_failures": bool(correctness_failures),
        "failures": list(dict.fromkeys(failures)),
    }


def load_result(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as handle:
        data = json.load(handle)
    if not isinstance(data, dict):
        raise ValueError("benchmark result root must be an object")
    return normalize_result_data(data)


def write_json_result(
    path: Path,
    data: dict[str, Any],
    *,
    overwrite: bool,
) -> None:
    """Durably create a result, atomically replacing only when requested."""
    path.parent.mkdir(parents=True, exist_ok=True)

    def dump(handle: Any) -> None:
        json.dump(data, handle, indent=2)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())

    if not overwrite:
        with path.open("x", encoding="utf-8", newline="\n") as handle:
            dump(handle)
        return

    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.",
        suffix=".tmp",
        dir=path.parent,
        text=True,
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(
            descriptor,
            "w",
            encoding="utf-8",
            newline="\n",
        ) as handle:
            dump(handle)
        os.replace(temporary, path)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def print_historical_report(
    *,
    benches: list[str],
    engine_names: list[str],
    baseline: str,
    compare_name: str,
    cold: dict[str, dict[str, list[float]]],
    startup: dict[str, dict[str, list[float]]],
    all_correct: bool,
    ab: bool,
) -> dict[str, Any]:
    """Emit the pre-schema-v2 startup-adjusted table layout, without clamping."""
    startup_ms = {
        name: statistics.median(
            value
            for bench in benches
            for value in startup[name][bench]
        )
        * 1000
        for name in engine_names
    }
    width = max(len(bench) for bench in benches) + 2
    header = f"{'bench':<{width}}" + "".join(
        f"{name:>12}" for name in engine_names
    )
    header += f"{'delta':>9}" if ab else f"{'ratio':>8}"
    print("metric=historical-adjusted (median cold minus median startup)")
    print(header)
    print("-" * len(header))

    ratios: list[float] = []
    rows: dict[str, Any] = {}
    unavailable = False
    for bench in benches:
        cold_ms = {
            name: statistics.median(cold[name][bench]) * 1000
            for name in engine_names
        }
        adjusted_ms = {
            name: cold_ms[name] - startup_ms[name]
            for name in engine_names
        }
        row = f"{bench:<{width}}" + "".join(
            f"{adjusted_ms[name]:>9.0f}ms" for name in engine_names
        )
        numerator = adjusted_ms[compare_name]
        denominator = adjusted_ms[baseline]
        ratio = numerator / denominator if numerator > 0 and denominator > 0 else None
        if ratio is None:
            unavailable = True
            row += f"{'n/a':>9}" if ab else f"{'n/a':>8}"
        elif ab:
            ratios.append(ratio)
            row += f"{(ratio - 1) * 100:>+8.1f}%"
        else:
            ratios.append(ratio)
            row += f"{ratio:>7.2f}x"
        print(row)
        rows[bench] = {
            "median_ms": adjusted_ms,
            "ratio": ratio,
            "metrics": {
                "cold": {"median_ms": cold_ms},
                "startup": {"median_ms": dict(startup_ms)},
                "adjusted": {
                    "median_ms": adjusted_ms,
                    "ratio": ratio,
                },
            },
        }

    print("-" * len(header))
    headline = None if unavailable else geometric_mean(ratios)
    if headline is None:
        print("headline ratio: unavailable (nonpositive adjusted sample)")
    elif ab:
        print(
            f"geomean adjusted ratio: {headline:.4f}x "
            f"({(headline - 1) * 100:+.2f}%)"
        )
    else:
        print(f"geomean adjusted ratio: {headline:.2f}x {compare_name}/{baseline}")
    print(
        "startup(ms, median): "
        + "  ".join(f"{name}={startup_ms[name]:.0f}" for name in engine_names)
    )
    print(f"ALL_CORRECT={'1' if all_correct else '0'}  (exact bytes, no normalisation)")
    return {
        "rows": rows,
        "geomean_adjusted_ratio": headline,
        "startup_median_ms": startup_ms,
    }


def print_modern_report(
    *,
    benches: list[str],
    engine_names: list[str],
    baseline: str,
    compare_name: str,
    cold: dict[str, dict[str, list[float]]],
    startup: dict[str, dict[str, list[float]]],
    adjusted: dict[str, dict[str, list[float]]],
    metric: str,
    seed: int,
    bootstrap_samples: int,
    all_correct: bool,
    ab: bool,
    readme: bool,
    reps: int,
) -> dict[str, Any]:
    sources = {"cold": cold, "startup": startup, "adjusted": adjusted}
    all_metrics = [
        {
            name: metric_summary(
                source,
                engine_names,
                bench,
                baseline,
                compare_name,
                seed=derived_seed(seed, bench, name),
                bootstrap_samples=bootstrap_samples,
            )
            for name, source in sources.items()
        }
        for bench in benches
    ]
    for bench, metrics in zip(benches, all_metrics):
        selected = metrics[metric]
        if selected["paired_ratio"] is None:
            raise ValueError(
                f"{metric} ratio unavailable for {bench}: "
                f"{selected['nonpositive_pairs']} nonpositive paired sample(s)"
            )

    width = max(len(bench) for bench in benches) + 2
    header = f"{'bench':<{width}}" + "".join(
        f"{name:>12}" for name in engine_names
    )
    header += f"{'paired':>11}{'95% CI':>19}"
    print(f"metric={metric}; times are medians; ratios use paired observations")
    print(header)
    print("-" * len(header))

    result_rows: dict[str, dict[str, Any]] = {}
    readme_rows: list[tuple[str, float, float, float, float, float]] = []
    selected_ratios: list[float] = []
    metric_ratios: dict[str, list[float]] = {name: [] for name in sources}
    metric_available = {name: True for name in sources}

    for bench, metrics in zip(benches, all_metrics):
        for name, detail in metrics.items():
            ratio = detail["paired_ratio"]
            if ratio is None:
                metric_available[name] = False
            else:
                metric_ratios[name].append(ratio)

        selected = metrics[metric]
        ratio = selected["paired_ratio"]
        assert ratio is not None
        ci_low, ci_high = selected["paired_ratio_ci95"]
        medians_ms = selected["median_ms"]
        selected_ratios.append(ratio)

        row = f"{bench:<{width}}" + "".join(
            f"{medians_ms[name]:>9.0f}ms" for name in engine_names
        )
        if ab:
            row += f"{(ratio - 1) * 100:>+10.1f}%"
            row += (
                f" [{(ci_low - 1) * 100:+6.1f},"
                f"{(ci_high - 1) * 100:+6.1f}]"
            )
        else:
            row += f"{ratio:>10.2f}x"
            row += f" [{ci_low:>6.2f},{ci_high:>6.2f}]"

        target = sources[metric][compare_name][bench]
        p10_ms = percentile(target, 0.10) * 1000
        p90_ms = percentile(target, 0.90) * 1000
        row += f"  [p10 {p10_ms:.0f} p90 {p90_ms:.0f}]"
        if metric == "cold":
            adjusted_ratio = metrics["adjusted"]["paired_ratio"]
            row += (
                f"  adjusted {adjusted_ratio:.2f}x"
                if adjusted_ratio is not None
                else "  adjusted n/a"
            )
        print(row)

        result_rows[bench] = {
            # Retain the schema-v2 headline keys for existing consumers.
            "median_ms": medians_ms,
            "paired_ratio": ratio,
            "paired_ratio_ci95": [ci_low, ci_high],
            # Preserve every measured phase, including signed adjusted medians.
            "metrics": metrics,
        }
        if not ab:
            readme_rows.append(
                (
                    bench,
                    medians_ms[baseline],
                    medians_ms[compare_name],
                    ratio,
                    ci_low,
                    ci_high,
                )
            )

    headline = geometric_mean(selected_ratios)
    headline_ci_low, headline_ci_high = bootstrap_geomean_of_medians_ci(
        [
            paired_ratios(
                sources[metric][compare_name][bench],
                sources[metric][baseline][bench],
            )
            for bench in benches
        ],
        seed=derived_seed(seed, "suite", metric),
        samples=bootstrap_samples,
    )
    metric_headlines = {
        name: (
            geometric_mean(metric_ratios[name])
            if metric_available[name] and len(metric_ratios[name]) == len(benches)
            else None
        )
        for name in sources
    }
    print("-" * len(header))
    if ab:
        print(
            f"geomean paired ratio: {headline:.4f}x "
            f"({(headline - 1) * 100:+.2f}%), 95% CI "
            f"[{(headline_ci_low - 1) * 100:+.2f}%, "
            f"{(headline_ci_high - 1) * 100:+.2f}%]"
        )
    else:
        print(
            f"geomean paired ratio: {headline:.2f}x {compare_name}/{baseline}, "
            f"95% CI [{headline_ci_low:.2f}x, {headline_ci_high:.2f}x]"
        )
    startup_ms = {
        name: statistics.median(
            value
            for bench in benches
            for value in startup[name][bench]
        )
        * 1000
        for name in engine_names
    }
    print(
        "startup(ms, paired launches): "
        + "  ".join(f"{name}={startup_ms[name]:.1f}" for name in engine_names)
    )
    print(f"ALL_CORRECT={'1' if all_correct else '0'}  (exact bytes, no normalisation)")

    if readme and readme_rows:
        def parity(*names: str) -> float:
            return geometric_mean(
                1.0 if bench in names else ratio
                for bench, _, _, ratio, _, _ in readme_rows
            )

        worst = sorted(readme_rows, key=lambda item: -item[3])[:2]
        print()
        print(
            f"**Performance — geomean {headline:.2f}× "
            f"{compare_name}/{baseline} ({metric} wall time; 95% CI "
            f"{headline_ci_low:.2f}×–{headline_ci_high:.2f}×)** on "
            f"{len(readme_rows)} benchmarks, {reps} paired observations with "
            "recorded order:"
        )
        print()
        print(
            f"| bench | {baseline} | {compare_name} | "
            "paired ratio (95% CI) |"
        )
        print("|---|---|---|---|")
        for bench, base_ms, compare_ms, ratio, ci_low, ci_high in sorted(
            readme_rows, key=lambda item: item[3]
        ):
            cell = f"{ratio:.2f}× [{ci_low:.2f}, {ci_high:.2f}]"
            if ratio < 1:
                cell = f"**{cell}**"
            print(
                f"| {bench} | {base_ms:.0f}ms | {compare_ms:.0f}ms | {cell} |"
            )
        print()
        print("| scenario | geomean |")
        print("|---|---|")
        print(f"| today | {headline:.2f}× |")
        for name, *_ in worst:
            scenario = parity(name)
            cell = f"**{scenario:.2f}×**" if scenario < 2 else f"{scenario:.2f}×"
            print(f"| `{name}` at {baseline} parity | {cell} |")
        if len(worst) == 2:
            print(
                f"| both of the two worst at {baseline} parity | "
                f"**{parity(worst[0][0], worst[1][0]):.2f}×** |"
            )
        uniform = 100 * (1 - 2.0 / headline) if headline > 2 else 0.0
        print(f"| the uniform alternative | every benchmark {uniform:.1f}% faster |")

    return {
        "headline_metric": metric,
        "rows": result_rows,
        "geomean_paired_ratio": headline,
        "geomean_paired_ratio_ci95": [headline_ci_low, headline_ci_high],
        "metric_geomean_paired_ratio": metric_headlines,
        "startup_median_ms": startup_ms,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--reps",
        type=int,
        default=15,
        help="paired repetitions (default 15; use 21 for marginal changes)",
    )
    parser.add_argument("--benches", help="comma-separated subset")
    parser.add_argument(
        "--bench-dir",
        default=str(BENCH_DIR),
        help="directory containing benchmark .js files (default bench/real)",
    )
    parser.add_argument("--json", help="write schema-v2 raw observations here")
    parser.add_argument(
        "--read-json",
        help="read a schema-v1/v2 result instead of running benchmarks",
    )
    parser.add_argument(
        "--overwrite-json",
        action="store_true",
        help="allow replacing an existing --json result",
    )
    parser.add_argument(
        "--readme",
        action="store_true",
        help="emit README ratio and parity-scenario tables from this run",
    )
    parser.add_argument(
        "--metric",
        choices=("cold", "adjusted"),
        default=None,
        help="headline metric (default cold; adjusted subtracts each paired launch)",
    )
    parser.add_argument(
        "--historical",
        action="store_true",
        help="emit the legacy startup-adjusted table layout",
    )
    parser.add_argument(
        "--seed",
        type=lambda value: int(value, 0),
        default=None,
        help=f"deterministic schedule/bootstrap seed (default {DEFAULT_SEED})",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=300.0,
        help="per-process timeout in seconds (default 300)",
    )
    parser.add_argument(
        "--bootstrap-samples",
        type=int,
        default=None,
        help=f"paired-bootstrap resamples (default {BOOTSTRAP_SAMPLES})",
    )
    parser.add_argument(
        "--zipp",
        default=str(
            ROOT
            / "target"
            / "release"
            / ("zipp.exe" if os.name == "nt" else "zipp")
        ),
    )
    parser.add_argument(
        "--ab",
        nargs=2,
        metavar=("OLD", "NEW"),
        help="compare two zipp builds instead of the engine table",
    )
    parser.add_argument(
        "--allow-aa",
        action="store_true",
        help=(
            "permit --ab with two byte-identical executables (a deliberate A/A "
            "calibration run); without it an accidental HEAD-vs-HEAD A/B is a "
            "hard error before any measurement"
        ),
    )
    parser.add_argument(
        "--ab-env",
        nargs=2,
        type=parse_env_assignments,
        metavar=("OLD_ENV", "NEW_ENV"),
        help="per-side comma-separated KEY=VALUE settings for --ab ('-' for none)",
    )
    parser.add_argument(
        "--baseline",
        default="node",
        help="engine the ratio column is against (default node)",
    )
    parser.add_argument(
        "--engines",
        default="node,bun,deno,zipp",
        help="comma-separated engine subset outside --ab (default all)",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.reps < 1:
        raise SystemExit("--reps must be positive")
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        raise SystemExit("--timeout must be finite and positive")
    if args.bootstrap_samples is not None and args.bootstrap_samples < 1:
        raise SystemExit("--bootstrap-samples must be positive")

    if args.read_json:
        if args.json or args.overwrite_json or args.ab or args.ab_env:
            raise SystemExit(
                "--read-json cannot be combined with --json/--overwrite-json/"
                "--ab/--ab-env"
            )
        try:
            replay = load_result(Path(args.read_json))
        except (OSError, ValueError, json.JSONDecodeError) as exc:
            raise SystemExit(f"cannot read benchmark result: {exc}") from exc
        benches = replay["benches"]
        if args.benches:
            requested = args.benches.split(",")
            unknown = sorted(set(requested) - set(benches))
            if unknown:
                raise SystemExit(
                    f"unknown benchmark(s) in result: {', '.join(unknown)}"
                )
            benches = requested
        engine_names = replay["engine_names"]
        baseline = replay["baseline"] or (
            "old"
            if "old" in engine_names and "new" in engine_names
            else args.baseline
        )
        compare_name = (
            "new"
            if "old" in engine_names and "new" in engine_names
            else "zipp"
        )
        try:
            validate_comparison(engine_names, baseline, compare_name)
        except ValueError as exc:
            raise SystemExit(str(exc)) from exc
        stored_metric = replay["headline_metric"]
        replay_metric = (
            stored_metric
            if stored_metric in ("cold", "adjusted")
            else "cold"
        )
        metric = args.metric or replay_metric
        seed = args.seed if args.seed is not None else replay["seed"]
        bootstrap_samples = (
            args.bootstrap_samples
            if args.bootstrap_samples is not None
            else replay["bootstrap_samples"]
        )
        if replay["schema_version"] == 1 and not args.historical:
            raise SystemExit(
                "schema-v1 replay requires --historical; paired modern "
                "statistics require schema-v2 observations"
            )
        replay_summary = None
        if replay["has_health_failures"]:
            print(
                "statistics unavailable: unhealthy observations were excluded; "
                "rerun after fixing the process failures"
            )
            print("ALL_CORRECT=0  (one or more benchmark processes failed)")
        elif args.historical:
            replay_summary = print_historical_report(
                benches=benches,
                engine_names=engine_names,
                baseline=baseline,
                compare_name=compare_name,
                cold=replay["cold"],
                startup=replay["startup"],
                all_correct=replay["all_correct"],
                ab=compare_name == "new",
            )
        else:
            try:
                replay_summary = print_modern_report(
                    benches=benches,
                    engine_names=engine_names,
                    baseline=baseline,
                    compare_name=compare_name,
                    cold=replay["cold"],
                    startup=replay["startup"],
                    adjusted=replay["adjusted"],
                    metric=metric,
                    seed=seed,
                    bootstrap_samples=bootstrap_samples,
                    all_correct=replay["all_correct"],
                    ab=compare_name == "new",
                    readme=args.readme,
                    reps=replay["reps"],
                )
            except ValueError as exc:
                replay["failures"].append(f"statistics unavailable: {exc}")
                print(f"statistics unavailable: {exc}")
                print(
                    f"ALL_CORRECT={'1' if replay['all_correct'] else '0'}  "
                    "(exact bytes, no normalisation)"
                )
        for failure in replay["failures"]:
            print(f"  FAIL: {failure}")
        ratio_available = (
            replay_summary is not None
            and (
                replay_summary.get("geomean_adjusted_ratio")
                if args.historical
                else replay_summary.get("geomean_paired_ratio")
            )
            is not None
        )
        if args.historical and replay_summary is not None and not ratio_available:
            print(
                "  FAIL: historical adjusted ratio unavailable because an "
                "adjusted median was nonpositive"
            )
        return (
            0
            if replay["all_correct"]
            and not replay["failures"]
            and ratio_available
            else 1
        )

    args.metric = args.metric or "cold"
    args.seed = args.seed if args.seed is not None else DEFAULT_SEED
    args.bootstrap_samples = (
        args.bootstrap_samples
        if args.bootstrap_samples is not None
        else BOOTSTRAP_SAMPLES
    )

    bench_dir = Path(args.bench_dir).resolve()
    if not bench_dir.is_dir():
        raise SystemExit(f"benchmark directory does not exist: {bench_dir}")
    available_benches = discover_benches(bench_dir)
    benches = args.benches.split(",") if args.benches else available_benches
    if not benches or any(not bench for bench in benches):
        raise SystemExit("benchmark subset must contain nonempty names")
    if len(set(benches)) != len(benches):
        raise SystemExit("benchmark subset contains duplicate names")
    unknown = sorted(set(benches) - set(available_benches))
    if unknown:
        raise SystemExit(f"unknown benchmark(s): {', '.join(unknown)}")

    if args.ab:
        engines = [
            ("old", [args.ab[0], "js"]),
            ("new", [args.ab[1], "js"]),
        ]
        baseline = "old"
        ab_env = args.ab_env or ({}, {})
        engine_env = {"old": ab_env[0], "new": ab_env[1]}
        reject_identical_ab_binaries(args.ab, ab_env, allow=args.allow_aa)
    else:
        if args.ab_env:
            raise SystemExit("--ab-env requires --ab")
        requested_engines = args.engines.split(",")
        if not requested_engines or any(not name for name in requested_engines):
            raise SystemExit("engine subset must contain nonempty names")
        if len(set(requested_engines)) != len(requested_engines):
            raise SystemExit("engine subset contains duplicate names")
        valid_engines = {"node", "bun", "deno", "zipp"}
        invalid_engines = sorted(set(requested_engines) - valid_engines)
        if invalid_engines:
            raise SystemExit(f"unknown engine(s): {', '.join(invalid_engines)}")
        engines: list[tuple[str, list[str]]] = []
        for name, cmd in (
            ("node", ["node"]),
            ("bun", ["bun", "run"]),
            ("deno", ["deno", "run", "-A"]),
        ):
            if name not in requested_engines:
                continue
            executable = shutil.which(cmd[0])
            if executable:
                engines.append((name, [executable] + cmd[1:]))
        if "zipp" in requested_engines:
            engines.append(("zipp", [args.zipp, "js"]))
        baseline = args.baseline
        engine_env = {name: {} for name, _ in engines}

    engine_names = [name for name, _ in engines]
    compare_name = "new" if args.ab else "zipp"
    try:
        validate_comparison(engine_names, baseline, compare_name)
    except ValueError as exc:
        raise SystemExit(str(exc)) from exc

    json_path = Path(args.json).resolve() if args.json else None
    if json_path and json_path.exists() and not args.overwrite_json:
        raise SystemExit(
            f"refusing to overwrite existing result: {json_path} "
            "(pass --overwrite-json to replace it)"
        )

    cold = {name: {bench: [] for bench in benches} for name in engine_names}
    startup = {name: {bench: [] for bench in benches} for name in engine_names}
    adjusted = {name: {bench: [] for bench in benches} for name in engine_names}
    outputs: dict[str, dict[str, bytes]] = {name: {} for name in engine_names}
    failures: list[str] = []
    health_failures: list[str] = []
    correctness_failures: list[str] = []
    observations: list[dict[str, Any]] = []
    schedules: list[dict[str, Any]] = []
    empty = create_empty_script()

    try:
        for rep in range(args.reps):
            bench_order = list(benches)
            random.Random(args.seed + rep).shuffle(bench_order)
            ordered_engines = engine_order_for_rep(engines, rep, args.seed)
            schedules.append(
                {
                    "rep": rep,
                    "bench_order": bench_order,
                    "engine_order": [name for name, _ in ordered_engines],
                }
            )
            for bench_position, bench in enumerate(bench_order):
                bench_path = bench_dir / f"{bench}.js"
                for engine_position, (name, cmd) in enumerate(ordered_engines):
                    launch = run_once(
                        cmd,
                        empty,
                        timeout=args.timeout,
                        env=engine_env[name],
                    )
                    full = run_once(
                        cmd,
                        bench_path,
                        timeout=args.timeout,
                        env=engine_env[name],
                    )
                    launch_s = float(launch["elapsed_s"])
                    cold_s = float(full["elapsed_s"])
                    adjusted_s = cold_s - launch_s
                    valid_for_stats = result_is_healthy(
                        launch
                    ) and result_is_healthy(full)

                    observation = {
                        "rep": rep,
                        "bench": bench,
                        "bench_position": bench_position,
                        "engine": name,
                        "engine_position": engine_position,
                        "startup_s": launch_s,
                        "cold_s": cold_s,
                        "adjusted_s": adjusted_s,
                        "startup_returncode": launch["returncode"],
                        "returncode": full["returncode"],
                        "startup_timed_out": launch["timed_out"],
                        "timed_out": full["timed_out"],
                        "startup_spawn_error": launch.get("spawn_error", False),
                        "spawn_error": full.get("spawn_error", False),
                        "startup_stdout_bytes": len(launch["stdout"]),
                        "stdout_bytes": len(full["stdout"]),
                        "stdout_sha256": hashlib.sha256(full["stdout"]).hexdigest(),
                        "valid_for_stats": valid_for_stats,
                    }
                    if launch["stderr"]:
                        observation["startup_stderr"] = decode_stderr(launch["stderr"])
                    if full["stderr"]:
                        observation["stderr"] = decode_stderr(full["stderr"])
                    observations.append(observation)

                    if launch["timed_out"]:
                        health_failures.append(
                            f"{name} startup timed out on {bench}, rep {rep + 1}"
                        )
                    elif launch.get("spawn_error", False):
                        health_failures.append(
                            f"{name} startup could not launch on {bench}, "
                            f"rep {rep + 1}: {decode_stderr(launch['stderr'])}"
                        )
                    elif launch["returncode"] != 0:
                        health_failures.append(
                            f"{name} startup exited {launch['returncode']} "
                            f"on {bench}, rep {rep + 1}: "
                            f"{decode_stderr(launch['stderr'])}"
                        )
                    if full["timed_out"]:
                        health_failures.append(
                            f"{name} timed out on {bench}, rep {rep + 1}"
                        )
                    elif full.get("spawn_error", False):
                        health_failures.append(
                            f"{name} could not launch on {bench}, rep {rep + 1}: "
                            f"{decode_stderr(full['stderr'])}"
                        )
                    elif full["returncode"] != 0:
                        health_failures.append(
                            f"{name} exited {full['returncode']} on {bench}, "
                            f"rep {rep + 1}: {decode_stderr(full['stderr'])}"
                        )

                    if valid_for_stats:
                        startup[name][bench].append(launch_s)
                        cold[name][bench].append(cold_s)
                        adjusted[name][bench].append(adjusted_s)

                    if result_is_healthy(full):
                        out = full["stdout"]
                        if bench not in outputs[name]:
                            outputs[name][bench] = out
                        elif outputs[name][bench] != out:
                            correctness_failures.append(
                                f"{name} output not reproducible on {bench}, "
                                f"rep {rep + 1}"
                            )
            print(f"  rep {rep + 1}/{args.reps} done", file=sys.stderr)
    finally:
        try:
            empty.unlink()
        except OSError:
            pass

    failures.extend(health_failures)
    failures.extend(correctness_failures)
    all_correct = not health_failures and not correctness_failures
    for bench in benches:
        reference = outputs[baseline].get(bench)
        for name in engine_names:
            if name != baseline and outputs[name].get(bench) != reference:
                all_correct = False
                failures.append(f"{name} output differs from {baseline} on {bench}")

    summary: dict[str, Any] | None = None
    if health_failures:
        all_correct = False
        print(
            "statistics unavailable: unhealthy observations were excluded; "
            "rerun after fixing the process failures"
        )
        print("ALL_CORRECT=0  (one or more benchmark processes failed)")
    elif args.historical:
        summary = print_historical_report(
            benches=benches,
            engine_names=engine_names,
            baseline=baseline,
            compare_name=compare_name,
            cold=cold,
            startup=startup,
            all_correct=all_correct,
            ab=bool(args.ab),
        )
        if summary["geomean_adjusted_ratio"] is None:
            failures.append(
                "historical adjusted ratio unavailable because an adjusted "
                "median was nonpositive"
            )
    else:
        try:
            summary = print_modern_report(
                benches=benches,
                engine_names=engine_names,
                baseline=baseline,
                compare_name=compare_name,
                cold=cold,
                startup=startup,
                adjusted=adjusted,
                metric=args.metric,
                seed=args.seed,
                bootstrap_samples=args.bootstrap_samples,
                all_correct=all_correct,
                ab=bool(args.ab),
                readme=args.readme,
                reps=args.reps,
            )
        except ValueError as exc:
            failures.append(f"statistics unavailable: {exc}")
            print(f"statistics unavailable: {exc}")
            print(
                f"ALL_CORRECT={'1' if all_correct else '0'}  "
                "(exact bytes, no normalisation)"
            )

    unique_failures = list(dict.fromkeys(failures))
    for failure in unique_failures:
        print(f"  FAIL: {failure}")

    if json_path:
        metadata = {
            "schema_version": SCHEMA_VERSION,
            "created_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
            "git_commit": git_revision(),
            "seed": args.seed,
            "reps": args.reps,
            "metric": "historical-adjusted" if args.historical else args.metric,
            "report_mode": "historical" if args.historical else "modern",
            "headline_metric": (
                "historical-adjusted" if args.historical else args.metric
            ),
            "timeout_s": args.timeout,
            "bootstrap_samples": args.bootstrap_samples,
            "benches": benches,
            "bench_dir": str(bench_dir),
            "baseline": baseline,
            "engines": [
                {
                    **engine_metadata(name, cmd, args.timeout),
                    "environment": engine_env[name],
                }
                for name, cmd in engines
            ],
            "host": {
                "platform": platform.platform(),
                "machine": platform.machine(),
                "processor": platform.processor(),
                "python": platform.python_version(),
                "power_mode": power_mode(),
            },
            "environment": relevant_environment(),
            "schedule": schedules,
            "observations": observations,
            "summary": summary,
            "all_correct": all_correct,
            "health_failures": list(dict.fromkeys(health_failures)),
            "correctness_failures": list(dict.fromkeys(correctness_failures)),
            "failures": unique_failures,
        }
        write_json_result(
            json_path,
            metadata,
            overwrite=args.overwrite_json,
        )
        print(f"raw observations -> {json_path}")

    return 0 if all_correct and not unique_failures else 1


if __name__ == "__main__":
    sys.exit(main())
