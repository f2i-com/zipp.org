#!/usr/bin/env python3
"""Peak-RSS A/B gate for retained-object layout experiments.

Each observation is a fresh engine process.  The generated JavaScript retains a
calibrated population of objects with 0, 1, 2, 4, 5, or 12 properties, which
makes fixed object-layout changes visible without adding benchmark sources to
the repository.  Runs are counterbalanced by case position and engine order.
Engine order is exactly balanced for an even repetition count; case position
is exactly balanced after a complete cycle of the selected cases.  The artifact
records whether each condition held for a particular invocation.

On Windows the process handle is waited to the signalled state, queried with
``GetProcessMemoryInfo`` for ``PeakWorkingSetSize``, and only then closed.  On
Linux/macOS ``wait4`` supplies the child high-water mark.  JSON contains every
raw peak observation and the hashes needed to reproduce its inputs.

The default gate permits ``max(2 MiB, 1%)`` median paired growth.  At the
default 250,000-object population, a 16-byte fixed-layout increase is 3.81 MiB,
so it is no longer hidden by the fixed allowance (and fails while the baseline
is below about 381 MiB).  Raise either threshold only for a recorded calibration.

Example::

    python tools/bench_peak_rss.py --ab old-zipp.exe new-zipp.exe \
      --reps 6 --json bench/hostile/objmap-rss.json

This is a trusted developer harness, not an untrusted-code sandbox.  It runs
the selected engine binaries and generated sources with host permissions.
"""

from __future__ import annotations

import argparse
import ctypes
import datetime as dt
import hashlib
import importlib.util
import json
import math
import os
import platform
import random
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Iterable


CORE_PATH = Path(__file__).with_name("bench.py")
CORE_SPEC = importlib.util.spec_from_file_location("zipp_bench_core_rss", CORE_PATH)
if CORE_SPEC is None or CORE_SPEC.loader is None:  # pragma: no cover
    raise RuntimeError(f"cannot import benchmark helpers from {CORE_PATH}")
core = importlib.util.module_from_spec(CORE_SPEC)
CORE_SPEC.loader.exec_module(core)


SCHEMA_VERSION = 1
DEFAULT_REPS = 6
DEFAULT_OBJECTS = 250_000
DEFAULT_KEY_COUNTS = (0, 1, 2, 4, 5, 12)
DEFAULT_SEED = 0x5253_5326
DEFAULT_TIMEOUT_S = 120.0
DEFAULT_ABSOLUTE_GATE_MIB = 2.0
DEFAULT_RELATIVE_GATE_PERCENT = 1.0
MIB = 1024 * 1024
POSIX_SIGKILL = int(getattr(signal, "SIGKILL", 9))

# Keep this policy tied to bench.py's audited environment namespace.  Matching
# is case-insensitive so a Windows environment cannot bypass cleaning with key
# casing.  Ordinary process essentials such as PATH, SystemRoot, TEMP and HOME
# do not match and remain inherited.
CONTROL_ENV_PREFIXES = tuple(
    prefix.upper() for prefix in core._RECORDED_ENV_PREFIXES
)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def canonical_json_digest(value: Any) -> str:
    encoded = json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=True,
    ).encode("utf-8")
    return sha256_bytes(encoded)


def clean_child_environment(
    ambient: dict[str, str] | None = None,
    explicit: dict[str, str] | None = None,
) -> dict[str, str]:
    """Remove ambient runtime controls, then apply the explicit A/B side env.

    Starting from a filtered ambient environment, rather than a tiny hand-made
    allowlist, preserves OS loader/runtime necessities across Windows and POSIX.
    Explicit values are experiment inputs. Artifact metadata passes them
    through bench.py's audited redaction policy before serialization.
    """

    source = os.environ if ambient is None else ambient
    cleaned = {
        key: value
        for key, value in source.items()
        if not any(key.upper().startswith(prefix) for prefix in CONTROL_ENV_PREFIXES)
    }
    if explicit:
        cleaned.update(explicit)
    return cleaned


def balance_assessment(reps: int, case_count: int) -> dict[str, Any]:
    return {
        "engine_order_exact": reps % 2 == 0,
        "engine_order_condition": "reps is even",
        "case_position_exact": case_count > 0 and reps % case_count == 0,
        "case_position_condition": "reps is a multiple of selected case count",
        "selected_case_count": case_count,
    }


def gate_sensitivity(
    object_count: int, absolute_gate_bytes: int, relative_gate: float
) -> dict[str, Any]:
    sixteen_byte_growth = 16 * object_count
    return {
        "object_count": object_count,
        "fixed_layout_probe_bytes_per_object": 16,
        "fixed_layout_probe_total_bytes": sixteen_byte_growth,
        "absolute_gate_bytes": absolute_gate_bytes,
        "probe_exceeds_absolute_gate": sixteen_byte_growth > absolute_gate_bytes,
        "relative_gate_dominates_above_baseline_bytes": (
            absolute_gate_bytes / relative_gate if relative_gate > 0 else None
        ),
        "probe_can_pass_relative_gate_above_baseline_bytes": (
            sixteen_byte_growth / relative_gate if relative_gate > 0 else None
        ),
    }


def parse_key_counts(value: str) -> tuple[int, ...]:
    try:
        result = tuple(int(item) for item in value.split(","))
    except ValueError as exc:
        raise argparse.ArgumentTypeError("--keys must be comma-separated integers") from exc
    if not result or any(item < 0 for item in result):
        raise argparse.ArgumentTypeError("--keys must contain non-negative integers")
    if len(set(result)) != len(result):
        raise argparse.ArgumentTypeError("--keys must not contain duplicates")
    if any(item > 64 for item in result):
        raise argparse.ArgumentTypeError("--keys supports at most 64 properties per case")
    return result


def generated_case(key_count: int, object_count: int) -> dict[str, Any]:
    """Return deterministic source and the one accepted stdout byte string."""

    if key_count < 0 or key_count > 64:
        raise ValueError("key_count must be between 0 and 64")
    if object_count < 2:
        raise ValueError("object_count must be at least two")

    fields = ", ".join(f"p{index}: i + {index}" for index in range(key_count))
    literal = "{" + fields + "}"
    if key_count:
        checksum_expression = " + ".join(
            f"probe.p{index}" for index in range(key_count)
        )
        checksum = key_count * (object_count - 1) + key_count * (key_count - 1) // 2
    else:
        checksum_expression = "retained[0] === probe ? 1 : 0"
        checksum = 0

    source = (
        '"use strict";\n'
        f"const COUNT = {object_count};\n"
        "const retained = new Array(COUNT);\n"
        "for (let i = 0; i < COUNT; i++) {\n"
        f"  retained[i] = {literal};\n"
        "}\n"
        # Make the complete population escape.  Reading only the final element
        # would allow a sufficiently aggressive engine to eliminate dead stores.
        "globalThis.__zippPeakRssRetained = retained;\n"
        "const probe = retained[COUNT - 1];\n"
        f"const checksum = {checksum_expression};\n"
        f'console.log("zipp-peak-rss", {key_count}, retained.length, checksum);\n'
    )
    source_bytes = source.encode("utf-8")
    expected_stdout = (
        f"zipp-peak-rss {key_count} {object_count} {checksum}\n".encode("ascii")
    )
    return {
        "id": f"retained-{key_count}-keys",
        "key_count": key_count,
        "object_count": object_count,
        "source": source,
        "source_bytes": len(source_bytes),
        "source_sha256": sha256_bytes(source_bytes),
        "expected_stdout": expected_stdout,
        "expected_stdout_sha256": sha256_bytes(expected_stdout),
    }


def case_order_for_rep(case_ids: list[str], rep: int, seed: int) -> list[str]:
    """Use seeded cyclic rotations, balancing every position over one cycle."""

    if not case_ids:
        return []
    base = list(case_ids)
    random.Random(seed).shuffle(base)
    cycle, offset = divmod(rep, len(base))
    if cycle % 2:
        base.reverse()
    return base[offset:] + base[:offset]


def engine_order_for_case(
    engines: list[tuple[str, list[str]]], rep: int, stable_case_index: int
) -> list[tuple[str, list[str]]]:
    """Alternate which of the two A/B processes runs first for every case."""

    ordered = list(engines)
    # Use the case's stable manifest position, not its scheduled position.  The
    # latter rotates with `rep` and would accidentally give a case the same
    # engine order every time when the number of cases is even.
    if (rep + stable_case_index) % 2:
        ordered.reverse()
    return ordered


class _ProcessMemoryCounters(ctypes.Structure):
    _fields_ = [
        ("cb", ctypes.c_ulong),
        ("PageFaultCount", ctypes.c_ulong),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
    ]


def _windows_peak_working_set(process: subprocess.Popen[Any]) -> int:
    """Query an already-signalled child while its process handle is still open."""

    if os.name != "nt":  # pragma: no cover - guarded by caller
        raise OSError("Windows peak working set requested on a non-Windows host")
    counters = _ProcessMemoryCounters()
    counters.cb = ctypes.sizeof(counters)
    get_process_memory_info = ctypes.WinDLL(
        "psapi", use_last_error=True
    ).GetProcessMemoryInfo
    get_process_memory_info.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(_ProcessMemoryCounters),
        ctypes.c_ulong,
    ]
    get_process_memory_info.restype = ctypes.c_int
    # Popen.wait() has observed the process handle in the signalled state.  Popen
    # keeps _handle alive until its context exits, which happens after this call.
    handle = ctypes.c_void_p(int(process._handle))  # type: ignore[attr-defined]
    if not get_process_memory_info(handle, ctypes.byref(counters), counters.cb):
        error = ctypes.get_last_error()
        raise ctypes.WinError(error)
    return int(counters.PeakWorkingSetSize)


def _terminate_windows_tree(process: subprocess.Popen[Any], timeout: float) -> None:
    try:
        subprocess.run(
            ["taskkill", "/PID", str(process.pid), "/T", "/F"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=min(timeout, 10.0),
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        pass
    try:
        process.kill()
    except OSError:
        pass


def _signal_posix_tree(process: subprocess.Popen[Any]) -> None:
    try:
        os.killpg(process.pid, POSIX_SIGKILL)
    except (OSError, ProcessLookupError):
        try:
            os.kill(process.pid, POSIX_SIGKILL)
        except (OSError, ProcessLookupError):
            pass


def _bounded_wait4(pid: int, deadline: float) -> tuple[int, Any] | None:
    """Poll wait4 until ``deadline``; retry EINTR and never block in wait4."""

    while True:
        try:
            waited_pid, status, usage = os.wait4(pid, os.WNOHANG)
        except InterruptedError:
            continue
        if waited_pid == pid:
            return status, usage
        if time.monotonic() >= deadline:
            return None
        time.sleep(0.005)


def _bounded_posix_kill_and_reap(
    process: subprocess.Popen[Any], timeout: float = 10.0
) -> int | None:
    """Best-effort bounded cleanup after a wait4 collector failure.

    Popen.__exit__ otherwise performs an unbounded wait when ``returncode`` is
    still ``None``.  Marking the handle after a failed bounded reap prevents the
    error-reporting path from turning into a harness hang.
    """

    _signal_posix_tree(process)
    try:
        reaped = _bounded_wait4(process.pid, time.monotonic() + timeout)
    except OSError:
        reaped = None
    if reaped is not None:
        status, _usage = reaped
        process.returncode = os.waitstatus_to_exitcode(status)
        return process.returncode
    process.returncode = -POSIX_SIGKILL
    return None


def _wait4_process(
    process: subprocess.Popen[Any], timeout: float
) -> tuple[int, int, bool]:
    """Reap a POSIX child once, returning (returncode, maxrss bytes, timed_out)."""

    if not hasattr(os, "wait4"):  # pragma: no cover - unsupported POSIX
        raise OSError("this platform does not provide wait4 peak-RSS accounting")
    reaped = _bounded_wait4(process.pid, time.monotonic() + timeout)
    timed_out = reaped is None
    if reaped is None:
        _signal_posix_tree(process)
        reaped = _bounded_wait4(
            process.pid,
            time.monotonic() + min(max(timeout, 1.0), 10.0),
        )
        if reaped is None:
            # Keep Popen.__exit__ bounded even in the pathological case where a
            # SIGKILLed child cannot be reaped before the cleanup deadline.
            process.returncode = -POSIX_SIGKILL
            raise OSError("SIGKILLed child could not be reaped before deadline")

    status, usage = reaped

    returncode = os.waitstatus_to_exitcode(status)
    process.returncode = returncode
    raw_maxrss = int(usage.ru_maxrss)
    # Linux reports KiB; Darwin reports bytes.  Both values become exact bytes
    # in the artifact, with the native collector named alongside them.
    peak_bytes = raw_maxrss if sys.platform == "darwin" else raw_maxrss * 1024
    return returncode, peak_bytes, timed_out


def run_peak_once(
    argv: list[str],
    *,
    expected_stdout: bytes,
    timeout: float,
    env: dict[str, str] | None = None,
) -> dict[str, Any]:
    """Run one fresh process and retain exact health and peak-RSS observations."""

    child_env = clean_child_environment(explicit=env)
    popen_options: dict[str, Any] = {}
    if os.name == "nt":
        popen_options["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
        collector = "GetProcessMemoryInfo.PeakWorkingSetSize.after_wait"
    else:
        popen_options["start_new_session"] = True
        collector = "wait4.ru_maxrss"

    started = time.perf_counter()
    with tempfile.TemporaryFile() as stdout_file, tempfile.TemporaryFile() as stderr_file:
        try:
            with subprocess.Popen(
                argv,
                stdout=stdout_file,
                stderr=stderr_file,
                env=child_env,
                **popen_options,
            ) as process:
                measurement_error = None
                timed_out = False
                peak_rss_bytes = None
                if os.name == "nt":
                    try:
                        returncode = process.wait(timeout=timeout)
                    except subprocess.TimeoutExpired:
                        timed_out = True
                        _terminate_windows_tree(process, timeout)
                        returncode = process.wait(timeout=min(timeout, 10.0))
                    try:
                        # This must remain inside the Popen context: __exit__
                        # closes the Windows process handle.
                        peak_rss_bytes = _windows_peak_working_set(process)
                    except OSError as exc:
                        measurement_error = f"{type(exc).__name__}: {exc}"
                else:
                    try:
                        returncode, peak_rss_bytes, timed_out = _wait4_process(
                            process, timeout
                        )
                    except OSError as exc:
                        returncode = _bounded_posix_kill_and_reap(process)
                        measurement_error = f"{type(exc).__name__}: {exc}"
        except OSError as exc:
            return {
                "elapsed_s": time.perf_counter() - started,
                "peak_rss_bytes": None,
                "collector": collector,
                "returncode": None,
                "timed_out": False,
                "spawn_error": True,
                "error_type": type(exc).__name__,
                "stdout_bytes": 0,
                "stdout_sha256": sha256_bytes(b""),
                "stdout_exact": False,
                "stderr_bytes": 0,
                "stderr_sha256": sha256_bytes(b""),
                "stderr_empty": True,
                "status_ok": False,
                "valid": False,
            }

        stdout_file.seek(0)
        stderr_file.seek(0)
        stdout = stdout_file.read()
        stderr = stderr_file.read()

    stdout_exact = stdout == expected_stdout
    stderr_empty = stderr == b""
    status_ok = returncode == 0 and not timed_out
    valid = bool(
        status_ok
        and stdout_exact
        and stderr_empty
        and peak_rss_bytes is not None
        and peak_rss_bytes > 0
        and measurement_error is None
    )
    return {
        "elapsed_s": time.perf_counter() - started,
        "peak_rss_bytes": peak_rss_bytes,
        "collector": collector,
        "returncode": returncode,
        "timed_out": timed_out,
        "spawn_error": False,
        "measurement_error": measurement_error,
        "stdout_bytes": len(stdout),
        "stdout_sha256": sha256_bytes(stdout),
        "stdout_exact": stdout_exact,
        "stderr_bytes": len(stderr),
        "stderr_sha256": sha256_bytes(stderr),
        "stderr_empty": stderr_empty,
        "status_ok": status_ok,
        "valid": valid,
    }


def summarize(
    observations: Iterable[dict[str, Any]],
    *,
    case_ids: list[str],
    reps: int,
    absolute_gate_bytes: int,
    relative_gate: float,
) -> tuple[dict[str, Any], list[str]]:
    observations = list(observations)
    result: dict[str, Any] = {}
    failures: list[str] = []
    for case_id in case_ids:
        by_rep: dict[int, dict[str, int]] = {}
        duplicate_pair_members = False
        for observation in observations:
            if observation["case"] != case_id or not observation["valid"]:
                continue
            pair = by_rep.setdefault(observation["rep"], {})
            if observation["engine"] in pair:
                duplicate_pair_members = True
            pair[observation["engine"]] = observation["peak_rss_bytes"]
        expected_reps = set(range(reps))
        complete = (
            not duplicate_pair_members
            and set(by_rep) == expected_reps
            and all(set(pair) == {"baseline", "candidate"} for pair in by_rep.values())
        )
        if not complete:
            failures.append(f"{case_id}: missing valid observations")
            result[case_id] = {
                "complete": False,
                "valid_paired_reps": sum(
                    set(pair) == {"baseline", "candidate"}
                    for pair in by_rep.values()
                ),
                "duplicate_pair_members": duplicate_pair_members,
                "gate_passed": False,
            }
            continue

        ordered_pairs = [by_rep[rep] for rep in range(reps)]
        baselines = [pair["baseline"] for pair in ordered_pairs]
        candidates = [pair["candidate"] for pair in ordered_pairs]
        paired_deltas = [
            candidate - baseline
            for baseline, candidate in zip(baselines, candidates)
        ]
        paired_ratios = [
            candidate / baseline
            for baseline, candidate in zip(baselines, candidates)
        ]
        baseline_median = statistics.median(baselines)
        candidate_median = statistics.median(candidates)
        median_paired_delta = statistics.median(paired_deltas)
        allowed_delta = max(absolute_gate_bytes, baseline_median * relative_gate)
        gate_passed = median_paired_delta <= allowed_delta
        if not gate_passed:
            failures.append(
                f"{case_id}: candidate median paired growth is "
                f"{median_paired_delta / MIB:.2f} MiB"
            )
        result[case_id] = {
            "complete": True,
            "baseline_median_peak_rss_bytes": baseline_median,
            "candidate_median_peak_rss_bytes": candidate_median,
            "marginal_median_delta_bytes": candidate_median - baseline_median,
            "median_paired_delta_bytes": median_paired_delta,
            "median_paired_ratio": statistics.median(paired_ratios),
            "paired_delta_bytes": paired_deltas,
            "paired_ratios": paired_ratios,
            "allowed_delta_bytes": allowed_delta,
            "gate_passed": gate_passed,
        }
    return result, failures


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--ab",
        nargs=2,
        metavar=("BASELINE", "CANDIDATE"),
        required=True,
        help="Zipp executables to compare (each is invoked as: EXE js SOURCE)",
    )
    parser.add_argument(
        "--ab-env",
        nargs=2,
        type=core.parse_env_assignments,
        metavar=("BASELINE_ENV", "CANDIDATE_ENV"),
        help=(
            "comma-separated KEY=VALUE controls for each A/B side; use '-' for "
            "none. Only audited numeric/boolean controls are recorded verbatim; "
            "unknown or sensitive values are redacted from artifacts"
        ),
    )
    parser.add_argument(
        "--reps",
        type=int,
        default=DEFAULT_REPS,
        help="fresh-process repetitions (even counts exactly balance engine order)",
    )
    parser.add_argument("--objects", type=int, default=DEFAULT_OBJECTS)
    parser.add_argument(
        "--keys",
        type=parse_key_counts,
        default=DEFAULT_KEY_COUNTS,
        help="comma-separated retained-object property counts",
    )
    parser.add_argument("--seed", type=int, default=DEFAULT_SEED)
    parser.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT_S)
    parser.add_argument("--json", help="write a schema-v1 result artifact")
    parser.add_argument(
        "--overwrite-json",
        action="store_true",
        help="replace --json atomically (the default refuses overwrites)",
    )
    parser.add_argument(
        "--allow-aa",
        action="store_true",
        help="permit identical baseline/candidate bytes (harness self-tests only)",
    )
    parser.add_argument(
        "--absolute-gate-mib",
        type=float,
        default=DEFAULT_ABSOLUTE_GATE_MIB,
        help="allowed candidate median growth floor",
    )
    parser.add_argument(
        "--relative-gate-percent",
        type=float,
        default=DEFAULT_RELATIVE_GATE_PERCENT,
        help="allowed candidate median growth percentage",
    )
    return parser


def _resolved_binary(value: str) -> Path:
    path = Path(value).resolve()
    if not path.is_file():
        raise SystemExit(f"engine executable does not exist: {path}")
    return path


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.reps < 2:
        raise SystemExit("--reps must be at least two")
    if args.objects < 2:
        raise SystemExit("--objects must be at least two")
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        raise SystemExit("--timeout must be positive")
    if (
        not math.isfinite(args.absolute_gate_mib)
        or not math.isfinite(args.relative_gate_percent)
        or args.absolute_gate_mib < 0
        or args.relative_gate_percent < 0
    ):
        raise SystemExit("gate thresholds must be finite and non-negative")
    if args.overwrite_json and not args.json:
        raise SystemExit("--overwrite-json requires --json")
    if os.name != "nt" and not hasattr(os, "wait4"):
        raise SystemExit("peak-RSS collection requires Windows or a wait4 platform")

    json_path = Path(args.json).resolve() if args.json else None
    if json_path and json_path.exists() and not args.overwrite_json:
        raise SystemExit(
            f"refusing to overwrite existing result: {json_path} "
            "(pass --overwrite-json to replace it)"
        )

    baseline_path, candidate_path = (_resolved_binary(value) for value in args.ab)
    engines = [
        ("baseline", [str(baseline_path), "js"]),
        ("candidate", [str(candidate_path), "js"]),
    ]
    ab_env = tuple(args.ab_env or ({}, {}))
    engine_env = {
        "baseline": ab_env[0],
        "candidate": ab_env[1],
    }
    recorded_engine_env = {
        name: core.recorded_environment(environment)
        for name, environment in engine_env.items()
    }
    core.reject_identical_ab_binaries(
        [str(baseline_path), str(candidate_path)],
        ab_env,
        allow=args.allow_aa,
    )
    cases = [generated_case(key_count, args.objects) for key_count in args.keys]
    case_ids = [case["id"] for case in cases]
    cases_by_id = {case["id"]: case for case in cases}
    balance = balance_assessment(args.reps, len(cases))
    if not balance["engine_order_exact"]:
        print(
            "warning: odd --reps cannot exactly balance A/B engine order",
            file=sys.stderr,
        )
    if not balance["case_position_exact"]:
        print(
            "warning: --reps is not a complete case-position cycle",
            file=sys.stderr,
        )

    workspace_commit_before = core.git_revision()
    harness_metadata_before = {
        "bench_peak_rss_py_sha256": core.file_digest(Path(__file__).resolve()),
        "bench_py_sha256": core.file_digest(CORE_PATH.resolve()),
    }
    ambient_environment = core.relevant_environment()
    engine_metadata_before = [
        {
            **core.engine_metadata(name, command, args.timeout),
            "explicit_environment": recorded_engine_env[name],
        }
        for name, command in engines
    ]
    input_manifest = {
        "generator": "bench_peak_rss.py:generated_case:v1",
        "objects": args.objects,
        "explicit_engine_environment": recorded_engine_env,
        "cases": [
            {
                key: case[key]
                for key in (
                    "id",
                    "key_count",
                    "object_count",
                    "source_bytes",
                    "source_sha256",
                    "expected_stdout_sha256",
                )
            }
            for case in cases
        ],
    }

    observations: list[dict[str, Any]] = []
    schedules: list[dict[str, Any]] = []
    with tempfile.TemporaryDirectory(prefix="zipp-peak-rss-") as directory:
        temp_root = Path(directory)
        source_paths: dict[str, Path] = {}
        for case in cases:
            path = temp_root / f"{case['id']}.js"
            path.write_text(case["source"], encoding="utf-8", newline="\n")
            if core.file_digest(path) != case["source_sha256"]:
                raise RuntimeError(f"generated source hash mismatch: {case['id']}")
            source_paths[case["id"]] = path

        sequence = 0
        for rep in range(args.reps):
            case_order = case_order_for_rep(case_ids, rep, args.seed)
            schedule = {"rep": rep, "case_order": case_order, "engine_orders": {}}
            for case_id in case_order:
                case = cases_by_id[case_id]
                stable_case_index = case_ids.index(case_id)
                ordered_engines = engine_order_for_case(
                    engines, rep, stable_case_index
                )
                schedule["engine_orders"][case_id] = [
                    name for name, _ in ordered_engines
                ]
                for name, command in ordered_engines:
                    print(
                        f"rss rep {rep + 1}/{args.reps} {case_id} {name}",
                        file=sys.stderr,
                    )
                    raw = run_peak_once(
                        command + [str(source_paths[case_id])],
                        expected_stdout=case["expected_stdout"],
                        timeout=args.timeout,
                        env=engine_env[name],
                    )
                    observations.append(
                        {
                            "sequence": sequence,
                            "rep": rep,
                            "case": case_id,
                            "engine": name,
                            **raw,
                        }
                    )
                    sequence += 1
            schedules.append(schedule)

    engine_metadata_after = [
        {
            **core.engine_metadata(name, command, args.timeout),
            "explicit_environment": recorded_engine_env[name],
        }
        for name, command in engines
    ]
    workspace_commit_after = core.git_revision()
    harness_metadata_after = {
        "bench_peak_rss_py_sha256": core.file_digest(Path(__file__).resolve()),
        "bench_py_sha256": core.file_digest(CORE_PATH.resolve()),
    }
    health_failures = [
        f"rep {item['rep']} {item['case']} {item['engine']}: invalid process result"
        for item in observations
        if not item["valid"]
    ]
    for before, after in zip(engine_metadata_before, engine_metadata_after):
        if before.get("sha256") != after.get("sha256"):
            health_failures.append(
                f"{before['name']}: executable changed during measurement"
            )
    if harness_metadata_before != harness_metadata_after:
        health_failures.append("benchmark harness source changed during measurement")
    if workspace_commit_before != workspace_commit_after:
        health_failures.append("workspace commit changed during measurement")

    summary, gate_failures = summarize(
        observations,
        case_ids=case_ids,
        reps=args.reps,
        absolute_gate_bytes=int(args.absolute_gate_mib * MIB),
        relative_gate=args.relative_gate_percent / 100.0,
    )
    all_healthy = not health_failures
    gate_passed = all_healthy and not gate_failures

    artifact = {
        "schema_version": SCHEMA_VERSION,
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "trusted_host_execution": True,
        "reps": args.reps,
        "seed": args.seed,
        "configuration": {
            "cwd": str(Path.cwd().resolve()),
            "timeout_s": args.timeout,
            "allow_aa": args.allow_aa,
            "objects": args.objects,
            "key_counts": list(args.keys),
            "explicit_side_environment": recorded_engine_env,
            "balance": balance,
        },
        "host": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "python": platform.python_version(),
            "peak_rss_collector": (
                "GetProcessMemoryInfo.PeakWorkingSetSize.after_wait"
                if os.name == "nt"
                else "wait4.ru_maxrss"
            ),
        },
        "workspace": {
            "commit_before": workspace_commit_before,
            "commit_after": workspace_commit_after,
        },
        "harness_before": harness_metadata_before,
        "harness_after": harness_metadata_after,
        "environment": {
            "ambient_relevant": ambient_environment,
            "stripped_control_prefixes": list(CONTROL_ENV_PREFIXES),
            "explicit_by_engine": recorded_engine_env,
            "child_policy": (
                "inherit non-control variables; remove ambient control prefixes; "
                "apply the selected side's explicit environment"
            ),
        },
        "engines_before": engine_metadata_before,
        "engines_after": engine_metadata_after,
        "input_manifest": input_manifest,
        "input_manifest_sha256": canonical_json_digest(input_manifest),
        "gate": {
            "absolute_growth_mib": args.absolute_gate_mib,
            "relative_growth_percent": args.relative_gate_percent,
            "rule": (
                "median(candidate_peak - baseline_peak) within matched reps <= "
                "max(absolute, median(baseline_peak) * relative)"
            ),
            "sensitivity": gate_sensitivity(
                args.objects,
                int(args.absolute_gate_mib * MIB),
                args.relative_gate_percent / 100.0,
            ),
        },
        "schedules": schedules,
        "observations": observations,
        "summary": summary,
        "all_healthy": all_healthy,
        "gate_passed": gate_passed,
        "health_failures": health_failures,
        "gate_failures": gate_failures,
    }

    for case_id in case_ids:
        item = summary[case_id]
        if not item["complete"]:
            print(f"{case_id}: INCOMPLETE")
            continue
        print(
            f"{case_id}: baseline {item['baseline_median_peak_rss_bytes'] / MIB:.2f} MiB, "
            f"candidate {item['candidate_median_peak_rss_bytes'] / MIB:.2f} MiB, "
            f"paired delta {item['median_paired_delta_bytes'] / MIB:+.2f} MiB, "
            f"{'PASS' if item['gate_passed'] else 'FAIL'}"
        )
    if json_path:
        core.write_json_result(json_path, artifact, overwrite=args.overwrite_json)
        print(f"wrote {json_path}")

    if health_failures:
        print("process health checks failed:", file=sys.stderr)
        for failure in health_failures:
            print(f"  {failure}", file=sys.stderr)
    if gate_failures:
        print("peak-RSS gate failed:", file=sys.stderr)
        for failure in gate_failures:
            print(f"  {failure}", file=sys.stderr)
    return 0 if gate_passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
