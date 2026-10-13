#!/usr/bin/env python3
"""Python frontend performance suite: `zipp py` against CPython, and Zipp A/B.

Runs every program under `tools/python_bench/{micro,macro}/` as a fresh
process per engine per repetition, interleaving the engines inside each
repetition. Each program times its own workload and prints one
`@bench-time SECONDS` line on stderr; its stdout is a deterministic checksum
that must equal CPython's (or, for `# zipp-bench: zipp-only` programs, the
baseline Zipp's) on every run.

    py -3.13 tools/python_bench.py --zipp target/release/zipp.exe
    py -3.13 tools/python_bench.py --zipp old.exe --zipp-b new.exe --reps 7
    py -3.13 tools/python_bench.py --zipp zipp.exe --no-cpython --group macro
    py -3.13 tools/python_bench.py --zipp zipp.exe --only dict --reps 3
    py -3.13 tools/python_bench.py --zipp zipp.exe --no-cpython --only torch --torch-cpython "py -3.11" --warm 5

`--torch-cpython CMD` adds a reference engine, `torch`: the zipp-only programs
that import torch run under a CPython with real PyTorch (one thread unless
`--torch-threads` says otherwise). `--warm N` runs each program's timed tail N
times in one process, for every engine, and times the warm runs (PyTorch's
first call in a process pays for its kernel initialisation); the first run's
time is kept as the cold one.

Ratios and geometric means use the in-process work time by default
(`--metric work`); wall time, which adds process start and Zipp's runtime
compile, is recorded beside it. A JSON report with every observation and its
provenance goes to target/bench-results/python-bench-<utc>.json unless
`--json` names another path. The exit status is non-zero when any run fails,
any checksum differs, or a binary or benchmark file changes during the run.

This is a trusted developer harness: it executes the given binaries and the
checked-in benchmark programs with the caller's permissions.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import math
import os
import platform
import re
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Sequence

SCHEMA_VERSION = 1
ROOT = Path(__file__).resolve().parent.parent
SUITE_DIR = Path(__file__).resolve().with_name("python_bench")
RESULTS_DIR = ROOT / "target" / "bench-results"
GROUPS = ("micro", "macro")
DEFAULT_REPS = 5
DEFAULT_TIMEOUT_S = 120.0
DEFAULT_CPYTHON = "py -3.13" if os.name == "nt" else "python3.13"
METRICS = {"work": "work_s", "wall": "wall_s"}
KNOWN_DIRECTIVES = frozenset({"zipp-only"})
DIRECTIVE_RE = re.compile(r"^#\s*zipp-bench:\s*(.*?)\s*$")
WORK_TIME_RE = re.compile(r"^@bench-time\s+([0-9]+(?:\.[0-9]+)?(?:[eE][-+]?[0-9]+)?)\s*$", re.MULTILINE)
STDERR_TAIL_CHARS = 2000

ENGINE_ZIPP = "zipp"
ENGINE_ZIPP_B = "zipp-b"
ENGINE_CPYTHON = "cpython"
# A CPython with PyTorch, timing the zipp-only torch programs (--torch-cpython).
ENGINE_TORCH = "torch"
# (label, numerator engine, denominator engine)
RATIO_PAIRS = (
    ("zipp/cpython", ENGINE_ZIPP, ENGINE_CPYTHON),
    ("zipp-b/zipp", ENGINE_ZIPP_B, ENGINE_ZIPP),
    ("zipp-b/cpython", ENGINE_ZIPP_B, ENGINE_CPYTHON),
    ("zipp/torch", ENGINE_ZIPP, ENGINE_TORCH),
    ("zipp-b/torch", ENGINE_ZIPP_B, ENGINE_TORCH),
)
DEFAULT_TORCH_THREADS = 1
# The line every benchmark's timed tail starts with (`--warm` repeats the
# program from there on).
TIMED_TAIL_MARKER = "\nt0 = time.perf_counter()\n"
IMPORTS_TORCH_RE = re.compile(r"^\s*(?:import\s+torch\b|from\s+torch\b)", re.MULTILINE)
TORCH_PROBE = "import json, torch; print(json.dumps({'torch': torch.__version__, 'threads': torch.get_num_threads()}))"

CPYTHON_PROBE = (
    "import json, platform, sys; print(json.dumps({"
    "'version': sys.version, 'version_info': list(sys.version_info), "
    "'implementation': sys.implementation.name, 'executable': sys.executable, "
    "'platform': platform.platform()}))"
)


# ---------------------------------------------------------------------------
# Benchmarks and engines


@dataclass(frozen=True)
class Benchmark:
    id: str
    group: str
    name: str
    path: Path
    zipp_only: bool
    sha256: str
    uses_torch: bool = False


@dataclass(frozen=True)
class Engine:
    name: str
    kind: str  # "zipp", "cpython" or "torch" (a CPython with PyTorch)
    prefix: tuple[str, ...]

    def argv(self, script: str) -> list[str]:
        return [*self.prefix, script]


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_sha256(path: Path) -> str | None:
    try:
        return sha256_bytes(Path(path).read_bytes())
    except OSError:
        return None


def parse_directives(source: str) -> frozenset[str]:
    """Directives from the leading comment block, e.g. `# zipp-bench: zipp-only`."""
    found: set[str] = set()
    for line in source.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if not stripped.startswith("#"):
            break
        match = DIRECTIVE_RE.match(stripped)
        if not match:
            continue
        for token in match.group(1).replace(",", " ").split():
            if token not in KNOWN_DIRECTIVES:
                raise ValueError(f"unknown zipp-bench directive {token!r}")
            found.add(token)
    return frozenset(found)


def discover(suite_dir: Path, group: str = "all", only: str = "") -> list[Benchmark]:
    groups = GROUPS if group == "all" else (group,)
    found: list[Benchmark] = []
    for group_name in groups:
        for path in sorted((suite_dir / group_name).glob("*.py")):
            bench_id = f"{group_name}/{path.stem}"
            if only and only not in bench_id:
                continue
            data = path.read_bytes()
            try:
                directives = parse_directives(data.decode("utf-8"))
            except ValueError as exc:
                raise ValueError(f"{bench_id}: {exc}") from None
            found.append(
                Benchmark(
                    id=bench_id,
                    group=group_name,
                    name=path.stem,
                    path=path,
                    zipp_only="zipp-only" in directives,
                    sha256=sha256_bytes(data),
                    uses_torch=bool(IMPORTS_TORCH_RE.search(data.decode("utf-8"))),
                )
            )
    return found


def split_command(command: str, posix: bool | None = None) -> list[str]:
    """Split an interpreter command such as `py -3.13` into argv.

    Windows paths keep their backslashes; surrounding quotes are removed.
    """
    if posix is None:
        posix = os.name != "nt"
    if posix:
        return shlex.split(command)
    parts = shlex.split(command, posix=False)
    return [p[1:-1] if len(p) >= 2 and p[0] == p[-1] and p[0] in "\"'" else p for p in parts]


def applicable_engines(bench: Benchmark, engines: Sequence[Engine]) -> list[Engine]:
    """CPython runs the ordinary programs; the torch reference runs only the
    zipp-only programs that import torch; Zipp runs everything."""
    usable = []
    for e in engines:
        if e.kind == ENGINE_CPYTHON and bench.zipp_only:
            continue
        if e.kind == ENGINE_TORCH and not (bench.zipp_only and bench.uses_torch):
            continue
        usable.append(e)
    return usable


def derived_source(source: str, warm: int, torch_threads: int | None) -> str:
    """The program with its timed tail (from `t0 = time.perf_counter()` on)
    repeated `warm` times in one process and, for the torch reference,
    `torch.set_num_threads` called first (`torch_threads` None: not at all;
    0: PyTorch's default left as it is). Each repetition prints its own
    checksum and `@bench-time` line."""
    text = source.replace("\r\n", "\n")
    index = text.rfind(TIMED_TAIL_MARKER)
    if index < 0:
        raise ValueError("no timed tail: the program has no line `t0 = time.perf_counter()`")
    head, tail = text[: index + 1], text[index + 1 :]
    setup = ""
    if torch_threads:
        setup = f"import torch as _zipp_bench_torch\n_zipp_bench_torch.set_num_threads({int(torch_threads)})\n"
    if warm <= 1:
        return head + setup + tail
    body = "".join("    " + line + "\n" if line.strip() else "\n" for line in tail.splitlines())
    return head + setup + f"for _zipp_bench_rep in range({int(warm)}):\n" + body


def build_schedule(
    benchmarks: Sequence[Benchmark], engines: Sequence[Engine], reps: int
) -> list[dict[str, Any]]:
    """Interleaved order: every repetition runs every benchmark on every engine.

    Benchmark order rotates by one position per repetition, and each
    benchmark's engine order rotates per repetition, so over len(engines)
    repetitions every engine runs first exactly once for every benchmark.
    """
    schedule: list[dict[str, Any]] = []
    count = len(benchmarks)
    stable_index = {b.id: i for i, b in enumerate(benchmarks)}
    for rep in range(reps):
        shift = rep % count if count else 0
        for bench in list(benchmarks[shift:]) + list(benchmarks[:shift]):
            usable = applicable_engines(bench, engines)
            if not usable:
                continue
            turn = (rep + stable_index[bench.id]) % len(usable)
            for engine in usable[turn:] + usable[:turn]:
                schedule.append(
                    {"seq": len(schedule), "rep": rep, "bench": bench.id, "engine": engine.name}
                )
    return schedule


# ---------------------------------------------------------------------------
# Processes


def normalize_output(data: bytes | str | None) -> str:
    if data is None:
        return ""
    if isinstance(data, bytes):
        data = data.decode("utf-8", errors="replace")
    return data.replace("\r\n", "\n")


def parse_work_time(stderr: str) -> float | None:
    matches = WORK_TIME_RE.findall(stderr.replace("\r\n", "\n"))
    if not matches:
        return None
    return float(matches[-1])


def parse_work_times(stderr: str) -> list[float]:
    return [float(m) for m in WORK_TIME_RE.findall(stderr.replace("\r\n", "\n"))]


def warm_and_cold(times: Sequence[float], warm: int) -> tuple[float | None, float | None]:
    """(work, cold) of one run: with `warm` > 1 the fastest of the runs after
    the first, and the first; otherwise the last line, and None."""
    if not times:
        return None, None
    if warm > 1:
        if len(times) < warm:
            return None, times[0]
        return min(times[1:]), times[0]
    return times[-1], None


def _kill_tree(proc: subprocess.Popen) -> None:
    if os.name == "nt":
        subprocess.run(
            ["taskkill", "/F", "/T", "/PID", str(proc.pid)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
    else:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except OSError:
            pass
    try:
        proc.kill()
    except OSError:
        pass


def spawn(argv: list[str], cwd: Path, env: dict[str, str], timeout: float) -> dict[str, Any]:
    """Run one process; kill its whole tree on timeout (a `py` launcher has a child)."""
    extra: dict[str, Any] = {}
    if os.name != "nt":
        extra["start_new_session"] = True
    try:
        proc = subprocess.Popen(
            argv,
            cwd=str(cwd),
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            **extra,
        )
    except OSError as exc:
        return {"returncode": None, "stdout": b"", "stderr": str(exc).encode(), "timed_out": False, "start_error": str(exc)}
    try:
        stdout, stderr = proc.communicate(timeout=timeout)
        return {"returncode": proc.returncode, "stdout": stdout, "stderr": stderr, "timed_out": False, "start_error": None}
    except subprocess.TimeoutExpired:
        _kill_tree(proc)
        try:
            stdout, stderr = proc.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            stdout, stderr = b"", b""
        return {"returncode": proc.returncode, "stdout": stdout, "stderr": stderr, "timed_out": True, "start_error": None}


def run_one(
    argv: list[str],
    cwd: Path,
    env: dict[str, str],
    timeout: float,
    spawn_fn: Callable[..., dict[str, Any]] | None = None,
    warm: int = 1,
) -> dict[str, Any]:
    """One observation: wall time around the process, work time from its
    stderr (with `warm` > 1, the fastest warm run; `cold_s` the first)."""
    spawn_fn = spawn_fn or spawn
    start = time.perf_counter()
    result = spawn_fn(argv, cwd, env, timeout)
    wall = time.perf_counter() - start
    stdout = normalize_output(result.get("stdout"))
    stderr = normalize_output(result.get("stderr"))
    times = parse_work_times(stderr)
    work, cold = warm_and_cold(times, warm)
    error = None
    if result.get("start_error"):
        error = f"could not start: {result['start_error']}"
    elif result.get("timed_out"):
        error = f"timed out after {timeout:g} s"
    elif result.get("returncode") != 0:
        error = f"exit code {result.get('returncode')}"
    elif not times:
        error = "no @bench-time line on stderr"
    elif work is None:
        error = f"{len(times)} @bench-time lines for --warm {warm}"
    return {
        "exit_code": result.get("returncode"),
        "timed_out": bool(result.get("timed_out")),
        "wall_s": wall,
        "work_s": work if error is None else None,
        "cold_s": cold if error is None else None,
        "work_times_s": times,
        "stdout": stdout,
        "stdout_sha256": sha256_bytes(stdout.encode("utf-8")),
        "stderr_tail": stderr[-STDERR_TAIL_CHARS:] if error else None,
        "error": error,
    }


def probe(argv: list[str], timeout: float = 30.0) -> tuple[int | None, str, str]:
    """Run a short metadata command (never a benchmark)."""
    try:
        proc = subprocess.run(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        return None, "", str(exc)
    return proc.returncode, normalize_output(proc.stdout), normalize_output(proc.stderr)


# ---------------------------------------------------------------------------
# Provenance


def zipp_metadata(engine: Engine, probe_fn: Callable[..., tuple[int | None, str, str]]) -> dict[str, Any]:
    executable = Path(engine.prefix[0])
    code, out, err = probe_fn([str(executable), "--version"])
    version_output = (out.strip() or err.strip()) if code == 0 else None
    identity = None
    code_json, out_json, _ = probe_fn([str(executable), "--version", "--json"])
    if code_json == 0:
        try:
            parsed = json.loads(out_json)
            identity = parsed if isinstance(parsed, dict) else None
        except ValueError:
            identity = None
    try:
        size = executable.stat().st_size
    except OSError:
        size = None
    return {
        "name": engine.name,
        "kind": engine.kind,
        "argv_prefix": list(engine.prefix),
        "executable": str(executable),
        "size": size,
        "sha256": file_sha256(executable),
        "version": version_output.splitlines()[0] if version_output else None,
        "version_output": version_output,
        "build_identity": identity,
    }


def cpython_metadata(engine: Engine, probe_fn: Callable[..., tuple[int | None, str, str]]) -> dict[str, Any]:
    code, out, err = probe_fn([*engine.prefix, "-c", CPYTHON_PROBE])
    info: dict[str, Any] | None = None
    if code == 0:
        try:
            parsed = json.loads(out.strip().splitlines()[-1])
            info = parsed if isinstance(parsed, dict) else None
        except (ValueError, IndexError):
            info = None
    executable = info.get("executable") if info else None
    return {
        "name": engine.name,
        "kind": engine.kind,
        "argv_prefix": list(engine.prefix),
        "executable": executable,
        "sha256": file_sha256(Path(executable)) if executable else None,
        "version": (info.get("version") or "").splitlines()[0] if info else None,
        "version_info": info.get("version_info") if info else None,
        "implementation": info.get("implementation") if info else None,
        "probe_error": None if info else (err.strip() or f"probe exited {code}"),
    }


def torch_metadata(engine: Engine, probe_fn: Callable[..., tuple[int | None, str, str]]) -> dict[str, Any]:
    """CPython's metadata plus the PyTorch version and default thread count."""
    meta = cpython_metadata(engine, probe_fn)
    code, out, err = probe_fn([*engine.prefix, "-c", TORCH_PROBE], timeout=120.0)
    info: dict[str, Any] | None = None
    if code == 0:
        try:
            parsed = json.loads(out.strip().splitlines()[-1])
            info = parsed if isinstance(parsed, dict) else None
        except (ValueError, IndexError):
            info = None
    meta["torch_version"] = info.get("torch") if info else None
    meta["torch_default_threads"] = info.get("threads") if info else None
    if meta["probe_error"] is None and info is None:
        meta["probe_error"] = (err.strip().splitlines() or [f"torch probe exited {code}"])[-1]
    return meta


def host_metadata() -> dict[str, Any]:
    return {
        "platform": platform.platform(),
        "system": platform.system(),
        "release": platform.release(),
        "machine": platform.machine(),
        "processor": platform.processor(),
        "node": platform.node(),
        "cpu_count": os.cpu_count(),
        "harness_python": platform.python_version(),
    }


def workspace_commit(probe_fn: Callable[..., tuple[int | None, str, str]]) -> str | None:
    code, out, _ = probe_fn(["git", "-C", str(ROOT), "rev-parse", "HEAD"])
    if code != 0:
        return None
    return out.strip() or None


def utc_now() -> dt.datetime:
    return dt.datetime.now(dt.timezone.utc)


# ---------------------------------------------------------------------------
# Statistics and validation


def median(values: Sequence[float | None]) -> float | None:
    usable = [v for v in values if v is not None]
    return statistics.median(usable) if usable else None


def minimum(values: Sequence[float | None]) -> float | None:
    usable = [v for v in values if v is not None]
    return min(usable) if usable else None


def geomean(values: Sequence[float | None]) -> float | None:
    usable = [v for v in values if v is not None and v > 0]
    if not usable:
        return None
    return math.exp(sum(math.log(v) for v in usable) / len(usable))


def ratio(numerator: float | None, denominator: float | None) -> float | None:
    if numerator is None or denominator is None or denominator <= 0 or numerator < 0:
        return None
    return numerator / denominator


def first_difference(expected: str, actual: str) -> str:
    left, right = expected.split("\n"), actual.split("\n")
    for index in range(max(len(left), len(right))):
        e = left[index] if index < len(left) else "<missing>"
        a = right[index] if index < len(right) else "<missing>"
        if e != a:
            return f"line {index + 1}: expected {e[:120]!r}, got {a[:120]!r}"
    return "outputs differ"


def validate(
    benchmarks: Sequence[Benchmark],
    observations: Sequence[dict[str, Any]],
    cpython_enabled: bool,
) -> tuple[dict[str, dict[str, Any]], list[str]]:
    """Check every run of every engine against the reference engine's stdout.

    The reference is CPython for ordinary programs and the baseline Zipp for
    zipp-only programs (or for everything under --no-cpython): its earliest
    successful run in schedule order. Failed runs are failures too.

    The torch reference engine (--torch-cpython) is compared too, but a
    difference there is recorded (`torch_mismatches`), not failed: it times
    the real PyTorch, whose float32 accumulation order differs from Zipp's,
    and a program can amplify that (see the README).
    """
    per_bench: dict[str, dict[str, Any]] = {}
    failures: list[str] = []
    for bench in benchmarks:
        rows = sorted((o for o in observations if o["bench"] == bench.id), key=lambda o: o["seq"])
        reference_engine = ENGINE_CPYTHON if cpython_enabled and not bench.zipp_only else ENGINE_ZIPP
        run_failures = []
        for row in rows:
            if row["error"] is not None:
                message = f"{bench.id} [{row['engine']} rep {row['rep'] + 1}]: {row['error']}"
                run_failures.append(message)
                failures.append(message)
        reference = next(
            (o for o in rows if o["engine"] == reference_engine and o["error"] is None), None
        )
        mismatches: list[dict[str, Any]] = []
        torch_mismatches: list[dict[str, Any]] = []
        reference_problem = None
        if reference is None:
            reference_problem = f"no successful {reference_engine} run to validate against"
        elif not reference["stdout"].strip():
            reference_problem = f"{reference_engine} printed no checksum"
        if reference_problem:
            failures.append(f"{bench.id}: {reference_problem}")
        else:
            for row in rows:
                if row["error"] is None and row["stdout"] != reference["stdout"]:
                    detail = first_difference(reference["stdout"], row["stdout"])
                    entry = {"seq": row["seq"], "engine": row["engine"], "rep": row["rep"], "detail": detail}
                    if row["engine"] == ENGINE_TORCH:
                        torch_mismatches.append(entry)
                        continue
                    mismatches.append(entry)
                    failures.append(
                        f"{bench.id} [{row['engine']} rep {row['rep'] + 1}]: checksum differs from "
                        f"{reference_engine} rep {reference['rep'] + 1}: {detail}"
                    )
        per_bench[bench.id] = {
            "validated_against": reference_engine,
            "reference_seq": reference["seq"] if reference else None,
            "reference_stdout": reference["stdout"] if reference else None,
            "mismatches": mismatches,
            "torch_mismatches": torch_mismatches,
            "run_failures": run_failures,
            "reference_problem": reference_problem,
            "valid": reference_problem is None and not mismatches and not run_failures,
        }
    return per_bench, failures


def summarize(
    benchmarks: Sequence[Benchmark],
    engines: Sequence[Engine],
    observations: Sequence[dict[str, Any]],
    validation: dict[str, dict[str, Any]],
) -> dict[str, dict[str, Any]]:
    summary: dict[str, dict[str, Any]] = {}
    for bench in benchmarks:
        engine_stats: dict[str, dict[str, Any]] = {}
        for engine in applicable_engines(bench, engines):
            rows = [
                o for o in observations
                if o["bench"] == bench.id and o["engine"] == engine.name and o["error"] is None
            ]
            item: dict[str, Any] = {}
            for metric in METRICS.values():
                values = [o[metric] for o in rows]
                item[metric] = {
                    "n": len(values),
                    "median": median(values),
                    "min": minimum(values),
                    "values": values,
                }
            colds = [o.get("cold_s") for o in rows if o.get("cold_s") is not None]
            if colds:
                item["cold_s"] = {"n": len(colds), "median": median(colds), "min": minimum(colds), "values": colds}
            item["overhead_s_median"] = median([o["wall_s"] - o["work_s"] for o in rows])
            engine_stats[engine.name] = item
        valid = validation[bench.id]["valid"]
        ratios: dict[str, dict[str, float | None]] = {}
        for metric in METRICS.values():
            ratios[metric] = {}
            for label, num, den in RATIO_PAIRS:
                if num not in engine_stats or den not in engine_stats:
                    continue
                value = ratio(engine_stats[num][metric]["median"], engine_stats[den][metric]["median"])
                ratios[metric][label] = value if valid else None
        summary[bench.id] = {
            "group": bench.group,
            "zipp_only": bench.zipp_only,
            "valid": valid,
            "validated_against": validation[bench.id]["validated_against"],
            "torch_differs": bool(validation[bench.id].get("torch_mismatches")),
            "engines": engine_stats,
            "ratios": ratios,
        }
    return summary


def group_geomeans(
    benchmarks: Sequence[Benchmark], summary: dict[str, dict[str, Any]]
) -> dict[str, dict[str, dict[str, dict[str, Any]]]]:
    """Geometric mean of each ratio per group; invalid rows are listed, not folded in."""
    result: dict[str, dict[str, dict[str, dict[str, Any]]]] = {}
    for metric in METRICS.values():
        result[metric] = {}
        for group_name in (*GROUPS, "all"):
            members = [b for b in benchmarks if group_name == "all" or b.group == group_name]
            if not members:
                continue
            per_pair: dict[str, dict[str, Any]] = {}
            for label, _, _ in RATIO_PAIRS:
                used, excluded = [], []
                for bench in members:
                    ratios = summary[bench.id]["ratios"][metric]
                    if label not in ratios:
                        continue  # not applicable, e.g. zipp/cpython for a zipp-only program
                    if ratios[label] is None:
                        excluded.append(bench.id)
                    else:
                        used.append(ratios[label])
                if used or excluded:
                    per_pair[label] = {"value": geomean(used), "count": len(used), "excluded": excluded}
            result[metric][group_name] = per_pair
    return result


# ---------------------------------------------------------------------------
# Report


def _ms(value: float | None) -> str:
    return "-" if value is None else f"{value * 1000:.1f}"


def _x(value: float | None) -> str:
    return "-" if value is None else f"{value:.2f}x"


def render_table(
    benchmarks: Sequence[Benchmark],
    engines: Sequence[Engine],
    summary: dict[str, dict[str, Any]],
    geomeans: dict[str, Any],
    metric: str,
) -> str:
    names = [e.name for e in engines]
    columns: list[tuple[str, Callable[[dict[str, Any]], str]]] = []

    def stat(engine: str, key: str) -> Callable[[dict[str, Any]], str]:
        return lambda row: _ms(row["engines"].get(engine, {}).get(metric, {}).get(key))

    for engine in names:
        columns.append((f"{engine} med", stat(engine, "median")))
        columns.append((f"{engine} min", stat(engine, "min")))
    for label, num, den in RATIO_PAIRS:
        if num in names and den in names:
            columns.append((label, lambda row, label=label: _x(row["ratios"][metric].get(label))))
    width = max([len("benchmark")] + [len(b.id) for b in benchmarks]) + 2
    header = "benchmark".ljust(width) + "".join(title.rjust(max(12, len(title) + 2)) for title, _ in columns)
    lines = [f"times in ms ({metric}); ratios are medians", header, "-" * len(header)]
    for bench in benchmarks:
        row = summary[bench.id]
        cells = "".join(render(row).rjust(max(12, len(title) + 2)) for title, render in columns)
        flags = []
        if bench.zipp_only:
            flags.append("zipp-only")
        if not row["valid"]:
            flags.append("INVALID")
        if row.get("torch_differs"):
            flags.append("torch-differs")
        lines.append(bench.id.ljust(width) + cells + ("  " + ",".join(flags) if flags else ""))
    lines.append("")
    for label, num, den in RATIO_PAIRS:
        if num not in names or den not in names:
            continue
        parts = []
        for group_name in (*GROUPS, "all"):
            entry = geomeans.get(metric, {}).get(group_name, {}).get(label)
            if not entry:
                continue
            text = f"{group_name} {_x(entry['value'])} (n={entry['count']}"
            if entry["excluded"]:
                text += f", {len(entry['excluded'])} invalid excluded"
            parts.append(text + ")")
        if parts:
            lines.append(f"geomean {label} ({metric}): " + ", ".join(parts))
    return "\n".join(lines)


def render_cold_table(
    benchmarks: Sequence[Benchmark], engines: Sequence[Engine], summary: dict[str, dict[str, Any]]
) -> str:
    """The first run of each process (`--warm`): what a program pays once."""
    names = [e.name for e in engines]
    width = max([len("benchmark")] + [len(b.id) for b in benchmarks]) + 2
    header = "benchmark".ljust(width) + "".join(f"{n} cold min".rjust(max(14, len(n) + 11)) for n in names)
    lines = ["first run in the process, ms (min over reps)", header, "-" * len(header)]
    for bench in benchmarks:
        stats = summary[bench.id]["engines"]
        cells = "".join(
            _ms(stats.get(n, {}).get("cold_s", {}).get("min")).rjust(max(14, len(n) + 11)) for n in names
        )
        lines.append(bench.id.ljust(width) + cells)
    return "\n".join(lines)


def default_report_path(started: dt.datetime, results_dir: Path = RESULTS_DIR) -> Path:
    return results_dir / f"python-bench-{started.strftime('%Y%m%dT%H%M%SZ')}.json"


def write_report(path: Path, report: dict[str, Any]) -> Path:
    """Write without clobbering: an existing name gets a numeric suffix."""
    path.parent.mkdir(parents=True, exist_ok=True)
    candidate = path
    counter = 1
    while True:
        try:
            with open(candidate, "x", encoding="utf-8", newline="\n") as handle:
                json.dump(report, handle, indent=2)
                handle.write("\n")
            return candidate
        except FileExistsError:
            candidate = path.with_name(f"{path.stem}-{counter}{path.suffix}")
            counter += 1


# ---------------------------------------------------------------------------
# CLI


def positive_int(text: str) -> int:
    value = int(text)
    if value < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return value


def positive_float(text: str) -> float:
    value = float(text)
    if not value > 0:
        raise argparse.ArgumentTypeError("must be positive")
    return value


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--zipp", required=True, help="zipp binary (engine 'zipp', the A side)")
    ap.add_argument("--zipp-b", default=None, help="second zipp binary for an A/B (engine 'zipp-b')")
    oracle = ap.add_mutually_exclusive_group()
    oracle.add_argument("--cpython", default=DEFAULT_CPYTHON, help=f"CPython command (default: {DEFAULT_CPYTHON!r})")
    oracle.add_argument("--no-cpython", action="store_true", help="skip CPython; validate against zipp A")
    ap.add_argument("--reps", type=positive_int, default=DEFAULT_REPS, help=f"repetitions (default {DEFAULT_REPS})")
    ap.add_argument("--only", default="", help="substring filter on benchmark ids such as micro/dict_get")
    ap.add_argument("--group", choices=("micro", "macro", "all"), default="all")
    ap.add_argument("--timeout", type=positive_float, default=DEFAULT_TIMEOUT_S, help="per-run timeout in seconds")
    ap.add_argument("--metric", choices=tuple(METRICS), default="work", help="metric for the table (JSON has both)")
    ap.add_argument("--json", default=None, help="report path (default target/bench-results/python-bench-<utc>.json)")
    ap.add_argument("--torch-cpython", default=None, metavar="CMD",
                    help="a CPython with PyTorch (e.g. 'py -3.11') timing the zipp-only torch programs as engine 'torch'")
    ap.add_argument("--torch-threads", type=non_negative_int, default=DEFAULT_TORCH_THREADS,
                    help=f"torch.set_num_threads for the torch engine (default {DEFAULT_TORCH_THREADS}; 0: PyTorch's default)")
    ap.add_argument("--warm", type=positive_int, default=1,
                    help="run each program's timed tail N times in one process and time the warm runs (default 1: off)")
    ap.add_argument("--suite-dir", default=str(SUITE_DIR), help=argparse.SUPPRESS)
    return ap.parse_args(argv)


def non_negative_int(text: str) -> int:
    value = int(text)
    if value < 0:
        raise argparse.ArgumentTypeError("must be 0 or more")
    return value


def build_engines(args: argparse.Namespace) -> list[Engine]:
    engines = [Engine(ENGINE_ZIPP, ENGINE_ZIPP, (str(Path(args.zipp).resolve()), "py"))]
    if args.zipp_b:
        engines.append(Engine(ENGINE_ZIPP_B, ENGINE_ZIPP, (str(Path(args.zipp_b).resolve()), "py")))
    if not args.no_cpython:
        engines.append(Engine(ENGINE_CPYTHON, ENGINE_CPYTHON, tuple(split_command(args.cpython))))
    if getattr(args, "torch_cpython", None):
        engines.append(Engine(ENGINE_TORCH, ENGINE_TORCH, tuple(split_command(args.torch_cpython))))
    return engines


def prepare_scripts(
    benchmarks: Sequence[Benchmark], engines: Sequence[Engine], warm: int, torch_threads: int, workdir: Path
) -> dict[tuple[str, str], tuple[Path, Path]]:
    """(bench id, engine) -> (script, cwd). A program runs as checked in from
    its group directory unless it needs a derived copy (`--warm`, or the
    torch engine's thread setting); a copy goes to a folder of its own under
    `workdir`, which then is that run's working directory."""
    out: dict[tuple[str, str], tuple[Path, Path]] = {}
    for bench in benchmarks:
        source = None
        for engine in applicable_engines(bench, engines):
            threads = torch_threads if engine.kind == ENGINE_TORCH else None
            if warm <= 1 and not threads:
                out[(bench.id, engine.name)] = (bench.path, bench.path.parent)
                continue
            if source is None:
                source = bench.path.read_text(encoding="utf-8")
            folder = workdir / f"{bench.group}-{bench.name}-{engine.kind}"
            folder.mkdir(parents=True, exist_ok=True)
            script = folder / bench.path.name
            script.write_text(derived_source(source, warm, threads), encoding="utf-8", newline="\n")
            out[(bench.id, engine.name)] = (script, folder)
    return out


def child_environment() -> dict[str, str]:
    return dict(os.environ, PYTHONDONTWRITEBYTECODE="1")


def main(argv: Sequence[str] | None = None) -> int:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    args = parse_args(argv)
    for option, value in (("--zipp", args.zipp), ("--zipp-b", args.zipp_b)):
        if value and not Path(value).is_file():
            print(f"{option}: no such file: {value}", file=sys.stderr)
            return 2
    suite_dir = Path(args.suite_dir)
    try:
        benchmarks = discover(suite_dir, args.group, args.only)
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2
    if not benchmarks:
        print("no benchmarks matched", file=sys.stderr)
        return 2

    engines = build_engines(args)
    started = utc_now()
    harness_sha_before = file_sha256(Path(__file__).resolve())
    engine_meta_before = []
    for engine in engines:
        if engine.kind == ENGINE_CPYTHON:
            meta = cpython_metadata(engine, probe)
            if meta["probe_error"]:
                print(
                    f"cannot run CPython command {args.cpython!r}: {meta['probe_error']} "
                    "(pass --cpython CMD or --no-cpython)",
                    file=sys.stderr,
                )
                return 2
        elif engine.kind == ENGINE_TORCH:
            meta = torch_metadata(engine, probe)
            if meta["probe_error"]:
                print(f"cannot run torch command {args.torch_cpython!r}: {meta['probe_error']}", file=sys.stderr)
                return 2
        else:
            meta = zipp_metadata(engine, probe)
        engine_meta_before.append(meta)
    commit = workspace_commit(probe)

    schedule = build_schedule(benchmarks, engines, args.reps)
    by_id = {b.id: b for b in benchmarks}
    by_engine = {e.name: e for e in engines}
    env = child_environment()
    observations: list[dict[str, Any]] = []
    workdir = Path(tempfile.mkdtemp(prefix="python-bench-"))
    try:
        scripts = prepare_scripts(benchmarks, engines, args.warm, args.torch_threads, workdir)
    except (OSError, ValueError) as exc:
        print(f"cannot prepare the programs: {exc}", file=sys.stderr)
        shutil.rmtree(workdir, ignore_errors=True)
        return 2
    print(
        f"python bench: {len(benchmarks)} programs x {args.reps} reps, engines "
        + ", ".join(e.name for e in engines)
        + f", {len(schedule)} runs"
        + (f", warm {args.warm}" if args.warm > 1 else ""),
        flush=True,
    )
    pending: list[str] = []
    for index, entry in enumerate(schedule):
        bench = by_id[entry["bench"]]
        engine = by_engine[entry["engine"]]
        script, cwd = scripts[(bench.id, engine.name)]
        argv_run = engine.argv(script.name)
        row = run_one(argv_run, cwd, env, args.timeout, warm=args.warm)
        observation = {**entry, "argv": argv_run, "cwd": str(cwd), **row}
        observations.append(observation)
        shown = row[METRICS[args.metric]]
        pending.append(f"{engine.name} {_ms(shown)}" if row["error"] is None else f"{engine.name} FAILED ({row['error']})")
        following = schedule[index + 1] if index + 1 < len(schedule) else None
        if following is None or (following["rep"], following["bench"]) != (entry["rep"], entry["bench"]):
            print(f"  rep {entry['rep'] + 1}/{args.reps}  {bench.id:<28} " + "  ".join(pending), flush=True)
            pending = []
    finished = utc_now()
    shutil.rmtree(workdir, ignore_errors=True)

    health_failures: list[str] = []
    engine_meta_after = [
        {"name": meta["name"], "sha256": file_sha256(Path(meta["executable"])) if meta.get("executable") else None}
        for meta in engine_meta_before
    ]
    for before, after in zip(engine_meta_before, engine_meta_after):
        if before["sha256"] != after["sha256"]:
            health_failures.append(f"{before['name']}: executable changed during the run")
    for bench in benchmarks:
        if file_sha256(bench.path) != bench.sha256:
            health_failures.append(f"{bench.id}: benchmark source changed during the run")
    if file_sha256(Path(__file__).resolve()) != harness_sha_before:
        health_failures.append("tools/python_bench.py changed during the run")

    validation, validation_failures = validate(benchmarks, observations, not args.no_cpython)
    summary = summarize(benchmarks, engines, observations, validation)
    geomeans = group_geomeans(benchmarks, summary)
    failures = validation_failures + health_failures

    report = {
        "schema_version": SCHEMA_VERSION,
        "tool": "tools/python_bench.py",
        "generated_at_utc": finished.isoformat(),
        "started_at_utc": started.isoformat(),
        "finished_at_utc": finished.isoformat(),
        "trusted_host_execution": True,
        "configuration": {
            "reps": args.reps,
            "group": args.group,
            "only": args.only,
            "timeout_s": args.timeout,
            "table_metric": METRICS[args.metric],
            "cpython_command": None if args.no_cpython else args.cpython,
            "torch_command": args.torch_cpython,
            "torch_threads": args.torch_threads if args.torch_cpython else None,
            "warm": args.warm,
            "warm_policy": "work_s is the fastest of runs 2..N in one process, cold_s the first" if args.warm > 1 else None,
            "suite_dir": str(suite_dir),
            "schedule_policy": "per rep: benchmark order rotated by rep; engine order per benchmark rotated by rep+index",
            "cwd_policy": "each run starts in its benchmark's group directory, so zipp's project VFS holds only that group",
            "work_time_source": "the program's own time.perf_counter() around its workload, printed as '@bench-time S' on stderr",
            "child_environment": "harness environment plus PYTHONDONTWRITEBYTECODE=1",
        },
        "host": host_metadata(),
        "workspace": {"commit": commit},
        "harness_sha256": harness_sha_before,
        "engines": engine_meta_before,
        "engines_after": engine_meta_after,
        "benchmarks": [
            {"id": b.id, "group": b.group, "path": str(b.path), "zipp_only": b.zipp_only, "sha256": b.sha256}
            for b in benchmarks
        ],
        "schedule": schedule,
        "observations": observations,
        "validation": validation,
        "summary": summary,
        "geomeans": geomeans,
        "all_valid": not failures,
        "failures": failures,
    }

    print()
    print(render_table(benchmarks, engines, summary, geomeans, METRICS[args.metric]))
    zipp_overheads = [
        summary[b.id]["engines"][ENGINE_ZIPP]["overhead_s_median"]
        for b in benchmarks
        if ENGINE_ZIPP in summary[b.id]["engines"]
    ]
    overhead = median(zipp_overheads)
    if overhead is not None and args.warm <= 1:
        print(f"zipp process overhead (wall - work), median over programs: {_ms(overhead)} ms")
    if args.warm > 1:
        print(render_cold_table(benchmarks, engines, summary))
    differing = [b.id for b in benchmarks if summary[b.id].get("torch_differs")]
    if differing:
        print("torch printed other values than zipp for: " + ", ".join(differing) + " (recorded, not failed)")
    path = Path(args.json) if args.json else default_report_path(started)
    written = write_report(path, report)
    print(f"report -> {written}")
    if failures:
        print(f"\n{len(failures)} failure(s):", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
