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

This is a trusted developer benchmark harness, not an untrusted-code sandbox:
it deliberately measures engines with their production JITs. External benchmark
directories therefore require an explicit opt-in; use ``zipp sandbox`` for
ordinary untrusted application scripts.
"""

from __future__ import annotations

import argparse
import atexit
import contextlib
import datetime as dt
import hashlib
import importlib.util
import json
import math
import os
import platform
import random
import re
import signal
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path, PurePosixPath
from typing import Any, Callable, Iterable, Iterator


SCHEMA_VERSION = 2
DEFAULT_SEED = 0x5A17_2026
BOOTSTRAP_SAMPLES = 10_000
MIN_PUBLISHABLE_REPS = 15
ROOT = Path(__file__).resolve().parent.parent
BENCH_DIR = ROOT / "bench" / "real"
BENCHMARK_INPUT_STAGING_POLICY = (
    "zipp-benchmark-input-stage-v1;exclusive-private-tree;plain-files;"
    "read-once-with-identity-check;readonly;execute-staged;live-recheck"
)

_STAGE_HELPER_PATH = Path(__file__).with_name("pgo_training.py")
_STAGE_HELPER_SPEC = importlib.util.spec_from_file_location(
    "zipp_benchmark_input_stage", _STAGE_HELPER_PATH
)
if _STAGE_HELPER_SPEC is None or _STAGE_HELPER_SPEC.loader is None:
    raise RuntimeError(f"cannot import input-staging helpers from {_STAGE_HELPER_PATH}")
_stage_helper = importlib.util.module_from_spec(_STAGE_HELPER_SPEC)
sys.modules[_STAGE_HELPER_SPEC.name] = _stage_helper
_STAGE_HELPER_SPEC.loader.exec_module(_stage_helper)

_RECORDED_ENV_PREFIXES = (
    "ZIPP_",
    "RUST",
    "MIMALLOC_",
    "NODE_",
    "DENO_",
    "BUN_",
    "LD_",
    "DYLD_",
    "MALLOC_",
    "JEMALLOC_",
    "TCMALLOC_",
    "ASAN_",
    "LSAN_",
    "MSAN_",
    "TSAN_",
    "UBSAN_",
)
_PUBLIC_CONTROL_ENV_KEYS = frozenset(
    {
        "RUST_BACKTRACE",
        "RUST_TEST_THREADS",
        "GLIBC_TUNABLES",
        "LLVM_PROFILE_FILE",
        "MALLOC_CONF",
        "MALLOC_CHECK_",
        "MALLOC_PERTURB_",
        "UV_THREADPOOL_SIZE",
        "__COMPAT_LAYER",
        "ZIPP_ASYNCSTATS",
        "ZIPP_BUILTINSTATS",
        "ZIPP_CALLLOG",
        "ZIPP_GC_STRESS",
        "ZIPP_GCSTATS",
        "ZIPP_ICSTATS",
        "ZIPP_JIT_THRESHOLD",
        "ZIPP_JITDECLINE",
        "ZIPP_JITDUMP",
        "ZIPP_JITLOG",
        "ZIPP_NO_COMPUTED_CALL_DENSE",
        "ZIPP_NO_MODULE_JIT",
        "ZIPP_NO_NURSERY_ADAPT",
        "ZIPP_NO_POLY_CALL_FALLBACK",
        "ZIPP_NO_STATIC_KEY_PLANS",
        "ZIPP_NO_STATIC_RECORD_FACTORY",
        "ZIPP_NO_TIERC_PLANNED_APPEND_PROBE",
        "ZIPP_NOJIT",
        "ZIPP_NURSERY",
        "ZIPP_NURSERY_MAX_MINORS",
        "ZIPP_NURSERY_VERIFY",
        "ZIPP_NURSERY_YOUNG_BUDGET",
        "ZIPP_PROF",
        "ZIPP_RX_JIT_THRESHOLD",
        "ZIPP_RXSTATS",
        "ZIPP_SHAPE_VERIFY",
        "ZIPP_SHAPESTATS",
        "ZIPP_STATIC_KEY_STATS",
        "ZIPP_STATIC_RECORD_STATS",
        "ZIPP_TRACE_CALLS",
        "ZIPP_VM_DUMP",
    }
)
_SENSITIVE_ENV_COMPONENTS = frozenset(
    {
        "APIKEY",
        "AUTH",
        "AUTHORIZATION",
        "COOKIE",
        "COOKIES",
        "CREDENTIAL",
        "CREDENTIALS",
        "KEY",
        "PASSWORD",
        "PASSWORDS",
        "PASSWD",
        "PRIVATE",
        "SECRET",
        "SECRETS",
        "SESSION",
        "SESSIONS",
        "TOKEN",
        "TOKENS",
    }
)
_PUBLIC_CONTROL_VALUE = re.compile(
    r"(?:[-+]?\d+(?:\.\d+)?|true|false|yes|no|on|off|auto|default|full|short)",
    re.IGNORECASE,
)
_REDACTED_ENV_VALUE = "<redacted>"

# The RETAINED TEN. This list is the historical series and must not change: every
# geomean in README.md and PERF_ROADMAP.md is comparable only because these ten
# programs, in this form, have been the headline since the series started.
HEADLINE_BENCHES = (
    "parse-large-js",
    "json-large",
    "markdown-render",
    "map-set-heavy",
    "typedarray-math",
    "regex-log-scan",
    "class-prototype-hot",
    "async-promise-chain",
    "polymorphic-objects",
    "sparse-array",
)

# M0.3 diagnostic siblings -- deliberately OUT of series. They exist to expose a
# specific mechanism (>8 same-shape receivers, computed-key churn, sparse growth)
# and are far slower than the headline rows, so folding them in inflates the
# geomean by ~0.43x. bench.py used to compute one 13-row number and leave the
# split to whoever remembered to pass --benches; now both are in the artifact.
DIAGNOSTIC_BENCHES = (
    "property-ic-shapes",
    "polymorphic-objects-v2",
    "sparse-array-v2",
)

CANONICAL_ENGINE_NAMES = ("node", "bun", "deno", "zipp")
PGO_TRAINING_INPUTS = (
    "bench/pgo-training/runtime-mix.js",
    "bench/pgo-training/text-data-mix.js",
    "bench/pgo-training/csv-tuple-mix.js",
    "bench/pgo-training/template-uri-mix.js",
    "bench/pgo-training/async-dag-mix.js",
    "bench/pgo-training/memory-shapes-mix.js",
    "bench/pgo-training/dictionary-mix.js",
)
PGO_CORPUS_VALIDATOR = "tools/pgo_corpus.py"
PGO_TRAINING_RUNNER = "tools/pgo_training.py"
PGO_EXPECTED_OUTPUT_MANIFEST = "bench/pgo-training/expected-output.json"
PGO_SIMILARITY_POLICY = (
    "zipp-pgo-structural-similarity-v1;normalized-js-tokens;10gram;"
    "ngram-evidence>=16;"
    "function-containment<0.78;whole-containment<0.66;"
    "window=96/24@0.82;absolute-run<72;short-run<36-or-0.90;"
    "training-source=ascii-lf;training-template-literal=deny;"
    "training-unicode-escape=deny;training-html-comment=deny;"
    "training-hashbang=deny;training-fnv1a=deny;"
    "training-distinctive-numbers=disjoint;training-numeric-tuples=disjoint;"
    "training-cooked-strings+regex-bodies=disjoint;"
    "training-ambiguous-slash=deny;private-id=atomic"
)
PGO_RUNNER_POLICY = (
    "zipp-pgo-runner-v1;exclusive-readonly-stage;external-code-off;"
    "timeout=30s;stdout<=4096;combined-output<=8192;output=manifest;"
    "one-profraw-per-input;explicit-hashed-profile-merge;atomic-publish"
)
PGO_RECIPE_VERSION = (
    "zipp-pgo-training-recipe-v7-immutable-source-staged-bounded-external-code-off"
)
PGO_EXCLUDED_INPUTS_LABEL = "excluded-publication-inputs"
PGO_RECIPE_COMMAND = (
    "build both Cargo stages from one private read-only clean-HEAD source snapshot; "
    "stage ordered corpus and scored provenance into an exclusive read-only tree; "
    "validate staged bytes; run each ordered training input once as zipp js "
    "--pgo-training STAGED_INPUT under zipp-pgo-training-env-allowlist-v1; "
    "enforce timeout, output caps, exact manifest stdout, and one explicitly "
    "hashed profraw per input; merge only enumerated profiles"
)
PGO_BUILD_RECIPE_VERSION = "zipp-pgo-build-recipe-v2"
PGO_BUILD_CONTRACT = (
    "zipp-pgo-build-v2;cargo=build --locked --release "
    "--target=x86_64-pc-windows-msvc --package=zipp-cli --bin=zipp "
    "--no-default-features;profile=opt-level=3,lto=fat,codegen-units=1,"
    "panic=abort,incremental=false,debug=false,strip=none,"
    "debug-assertions=false,overflow-checks=false;rustflags="
    "target-cpu=x86-64,linker-flavor=lld-link,profile-use=<verified-profile>;"
    "linker=selected-rustc-rust-lld;cc-rs=target-specific-selected-msvc-cl+lib;"
    "source=private-readonly-clean-head-snapshot-v1;cargo-config=controlled-cwd+"
    "no-home-config;target-dir=fresh;sdk=validated-environment-paths-not-byte-"
    "manifested;env=allowlist-v2"
)
PGO_BUILD_ENVIRONMENT_POLICY = "zipp-pgo-build-env-allowlist-v2"
PGO_CANONICAL_TARGET = "x86_64-pc-windows-msvc"
PGO_CANONICAL_RUSTFLAGS = (
    "-Cprofile-use=<redacted-path> -Ctarget-cpu=x86-64 "
    "-Clinker-flavor=lld-link"
)
PGO_BUILD_DEFINITION_FILES = (
    "Cargo.toml",
    "Cargo.lock",
    "crates/zipp-cli/Cargo.toml",
    "crates/zipp-cli/build.rs",
    "crates/zipp-vm/Cargo.toml",
    "crates/regress-fork/Cargo.toml",
)
BENCHMARK_ENVIRONMENT_POLICY_VERSION = 2
DESCRIPTIVE_BOOTSTRAP_METHOD = (
    "percentile bootstrap estimate; descriptive only, not a hypothesis test"
)


def canonical_benchmark_environment_descriptor() -> dict[str, Any]:
    """Describe the exact, fail-closed environment supplied to every engine.

    The temporary directory itself is intentionally represented symbolically:
    its random name is measurement bookkeeping, not a benchmark control.  No
    ambient variable outside the small OS bootstrap below reaches a child.
    """

    descriptor: dict[str, Any] = {
        "version": BENCHMARK_ENVIRONMENT_POLICY_VERSION,
        "inherit": "none",
        "platform": "windows" if os.name == "nt" else "posix",
        "fixed": {"LANG": "C", "LC_ALL": "C", "TZ": "UTC"},
        "isolated": [
            "HOME",
            "TMP",
            "TEMP",
            "TMPDIR",
            "USERPROFILE",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
        ],
        "explicit_engine_overlays_only": True,
        "lifecycle": "fresh isolated root per child process",
    }
    if os.name == "nt":
        descriptor["isolated"].extend(["APPDATA", "LOCALAPPDATA"])
        descriptor["os_bootstrap"] = ["SystemRoot", "WINDIR"]
        descriptor["path"] = "%SystemRoot%\\System32"
        descriptor["windows_command_bootstrap"] = {
            "ComSpec": "%SystemRoot%\\System32\\cmd.exe",
            "PATHEXT": ".COM;.EXE;.BAT;.CMD",
        }
    else:
        descriptor["os_bootstrap"] = []
        descriptor["path"] = "/usr/bin:/bin"
    return descriptor


def canonical_benchmark_environment(
    isolated_root: Path,
    *,
    host_environment: dict[str, str] | None = None,
) -> dict[str, str]:
    """Build an allowlisted child environment with isolated home/cache/temp.

    This is deliberately constructed from scratch.  Prefix blacklists are
    intrinsically fail-open when a runtime adds a new control variable.
    """

    root = isolated_root.resolve()
    home = root / "home"
    temporary = root / "tmp"
    cache = root / "cache"
    config = root / "config"
    data = root / "data"
    for directory in (home, temporary, cache, config, data):
        directory.mkdir(parents=True, exist_ok=True)

    environment = {
        "LANG": "C",
        "LC_ALL": "C",
        "TZ": "UTC",
        "HOME": str(home),
        "USERPROFILE": str(home),
        "TMP": str(temporary),
        "TEMP": str(temporary),
        "TMPDIR": str(temporary),
        "XDG_CACHE_HOME": str(cache),
        "XDG_CONFIG_HOME": str(config),
        "XDG_DATA_HOME": str(data),
    }
    if os.name == "nt":
        source = host_environment if host_environment is not None else dict(os.environ)
        system_root = source.get("SystemRoot") or source.get("WINDIR") or r"C:\Windows"
        environment.update(
            {
                # These two values are OS bootstrap data, not a general
                # inheritance channel.  Engine commands themselves are absolute.
                "SystemRoot": system_root,
                "WINDIR": system_root,
                "PATH": str(Path(system_root) / "System32"),
                "ComSpec": str(Path(system_root) / "System32" / "cmd.exe"),
                "PATHEXT": ".COM;.EXE;.BAT;.CMD",
                "APPDATA": str(data / "appdata"),
                "LOCALAPPDATA": str(cache / "localappdata"),
            }
        )
        Path(environment["APPDATA"]).mkdir(parents=True, exist_ok=True)
        Path(environment["LOCALAPPDATA"]).mkdir(parents=True, exist_ok=True)
    else:
        environment["PATH"] = "/usr/bin:/bin"
    return environment


@contextlib.contextmanager
def benchmark_process_environment(
    *,
    process_env: dict[str, str] | None = None,
    fresh_environment: bool = False,
    prefix: str = "zipp-benchmark-process-",
) -> Iterator[dict[str, str] | None]:
    """Yield one child environment, cleaning a fresh isolated root afterwards.

    Environment-directory creation happens before the caller starts any timing.
    A fresh root is never shared with launcher resolution, metadata probes, an
    empty launch, another engine, or another repetition, so disk/code caches
    cannot turn later nominally-cold observations into warm ones.
    """

    if fresh_environment and process_env is not None:
        raise ValueError("fresh_environment cannot be combined with process_env")
    if not fresh_environment:
        yield process_env
        return
    with tempfile.TemporaryDirectory(prefix=prefix) as directory:
        yield canonical_benchmark_environment(Path(directory))


def discover_benches(bench_dir: Path = BENCH_DIR) -> list[str]:
    return sorted(path.stem for path in bench_dir.glob("*.js"))


def classify_benches(benches: list[str]) -> dict[str, list[str]]:
    """Split a bench list into the retained-ten series and everything else.

    Returns the three lists the artifact records. `unclassified` is not an error
    -- a new benchmark simply is not in the historical series until someone adds
    it to `HEADLINE_BENCHES` on purpose.
    """
    return {
        "headline_benches": [b for b in benches if b in HEADLINE_BENCHES],
        "diagnostic_benches": [b for b in benches if b in DIAGNOSTIC_BENCHES],
        "unclassified_benches": [
            b
            for b in benches
            if b not in HEADLINE_BENCHES and b not in DIAGNOSTIC_BENCHES
        ],
    }


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
    alpha: float = 0.05,
) -> tuple[float, float]:
    """Return a deterministic paired-bootstrap interval for a median ratio."""
    if not ratios:
        raise ValueError("bootstrap requires at least one ratio")
    if not 0.0 < alpha < 1.0:
        raise ValueError("bootstrap alpha must be between zero and one")
    if len(ratios) == 1:
        return ratios[0], ratios[0]
    rng = random.Random(seed)
    size = len(ratios)
    medians = [
        statistics.median(ratios[rng.randrange(size)] for _ in range(size))
        for _ in range(samples)
    ]
    return percentile(medians, alpha / 2.0), percentile(medians, 1.0 - alpha / 2.0)


def exact_one_sided_sign_test(
    numerators: list[float], denominators: list[float]
) -> dict[str, Any] | None:
    """Test H0: P(numerator < denominator) <= 0.5 exactly.

    Each paired observation is one Bernoulli trial. Equal timings count as
    non-wins, which is conservative and keeps the tested event identical to the
    strict "faster" claim. The binomial upper tail at p=0.5 is the worst case
    under the composite null, so the returned p-value is finite-sample exact.
    """

    if len(numerators) != len(denominators) or not numerators:
        return None
    if any(
        not math.isfinite(numerator) or not math.isfinite(denominator)
        for numerator, denominator in zip(numerators, denominators)
    ):
        return None
    trials = len(numerators)
    wins = sum(
        numerator < denominator
        for numerator, denominator in zip(numerators, denominators)
    )
    ties = sum(
        numerator == denominator
        for numerator, denominator in zip(numerators, denominators)
    )
    tail = sum(math.comb(trials, successes) for successes in range(wins, trials + 1))
    return {
        "strict_wins": wins,
        "trials": trials,
        "ties": ties,
        "one_sided_pvalue": tail / (2**trials),
        "null": "P(numerator < denominator) <= 0.5; ties are non-wins",
    }


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
    """Return a deterministic, position-balanced engine order.

    Two-engine A/Bs retain their exact AB/BA alternation.  For larger tables a
    seeded base order is rotated once per repetition, so every engine occupies
    every position exactly once in each ``len(engines)``-rep block.  Alternate
    blocks reverse the rotations; over two blocks that also balances which side
    of every engine pair runs first.  An incomplete final block can differ by at
    most one position exposure, rather than the several-position skew produced
    by independently shuffling every repetition.
    """
    order = list(engines)
    if len(order) < 2:
        return order
    if len(order) == 2:
        if rep % 2:
            order.reverse()
        return order
    random.Random(seed).shuffle(order)
    block, offset = divmod(rep, len(order))
    order = order[offset:] + order[:offset]
    if block % 2:
        order.reverse()
    return order


def run_once(
    cmd: list[str],
    path: Path,
    *,
    timeout: float,
    env: dict[str, str] | None = None,
    base_env: dict[str, str] | None = None,
    fresh_environment: bool = False,
) -> dict[str, Any]:
    with benchmark_process_environment(
        process_env=base_env,
        fresh_environment=fresh_environment,
    ) as selected_env:
        child_env = dict(os.environ) if selected_env is None else dict(selected_env)
        if env:
            child_env.update(env)
        # Constructing the isolated directory and environment is deliberately
        # outside the timed region.
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


class GitCommitRecipeSource:
    """Read canonical recipe bytes directly from one immutable Git commit.

    A clean checkout is allowed to materialize text files with platform EOLs.
    The PGO builder deliberately checks out its private source snapshot with
    ``core.autocrlf=false``, so independent recipe verification must use the
    commit's blob bytes rather than the caller's possibly-CRLF worktree bytes.
    """

    def __init__(self, root: Path, commit: str) -> None:
        if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", commit):
            raise ValueError("canonical recipe commit is not a full object id")
        self.root = root.resolve(strict=True)
        self.commit = commit
        self._entries: dict[str, tuple[str, str, str]] | None = None

    def entries(self) -> dict[str, tuple[str, str, str]]:
        if self._entries is not None:
            return self._entries
        probe = subprocess.run(
            ["git", "ls-tree", "-r", "-z", self.commit],
            cwd=self.root,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=20,
            check=False,
        )
        if probe.returncode != 0:
            raise OSError("could not enumerate canonical recipe commit")
        parsed: dict[str, tuple[str, str, str]] = {}
        for item in probe.stdout.split(b"\0"):
            if not item:
                continue
            try:
                metadata, raw_path = item.split(b"\t", 1)
                raw_mode, raw_kind, raw_oid = metadata.split(b" ", 2)
                relative = raw_path.decode("utf-8", errors="surrogateescape")
                mode = raw_mode.decode("ascii", errors="strict")
                kind = raw_kind.decode("ascii", errors="strict")
                oid = raw_oid.decode("ascii", errors="strict")
            except (ValueError, UnicodeError) as exc:
                raise OSError("could not parse canonical recipe tree") from exc
            if relative in parsed:
                raise OSError("duplicate path in canonical recipe tree")
            parsed[relative] = (mode, kind, oid)
        if not parsed:
            raise OSError("canonical recipe commit has no files")
        self._entries = parsed
        return parsed

    def read_bytes(self, relative: str) -> bytes | None:
        entry = self.entries().get(relative)
        if entry is None:
            return None
        mode, kind, oid = entry
        if kind != "blob" or mode not in {"100644", "100755"}:
            return None
        probe = subprocess.run(
            ["git", "cat-file", "blob", oid],
            cwd=self.root,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=20,
            check=False,
        )
        return probe.stdout if probe.returncode == 0 else None

    def digest(self, relative: str) -> str | None:
        contents = self.read_bytes(relative)
        return hashlib.sha256(contents).hexdigest() if contents is not None else None

    def snapshot_digest(self) -> str | None:
        """Mirror ``pgo.sh``'s private-clone repository snapshot digest."""

        digest = hashlib.sha256(b"zipp-pgo-repository-snapshot-v1\0")
        try:
            entries = self.entries()
            for relative in sorted(
                entries,
                key=lambda item: item.encode("utf-8", errors="surrogateescape"),
            ):
                contents = self.read_bytes(relative)
                if contents is None:
                    return None
                digest.update(relative.encode("utf-8", errors="surrogateescape"))
                digest.update(b"\0file\0")
                digest.update(hashlib.sha256(contents).digest())
                digest.update(b"\0")
            return digest.hexdigest()
        except (OSError, UnicodeError):
            return None


def canonical_recipe_source_for_identity(
    identity: dict[str, Any], *, root: Path = ROOT
) -> tuple[GitCommitRecipeSource | None, str | None]:
    """Return Git-blob recipe bytes only inside an exact clean-HEAD envelope."""

    commit = identity.get("commit")
    if (
        not isinstance(commit, str)
        or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", commit)
        or identity.get("dirty") is not False
    ):
        return None, "PGO recipe source is not an exact clean commit"
    try:
        head = subprocess.run(
            ["git", "rev-parse", "--verify", "HEAD"],
            cwd=root,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=5,
            check=False,
            text=True,
        )
        if head.returncode != 0 or head.stdout.strip() != commit:
            return None, "PGO recipe source commit does not match workspace HEAD"
        matches, reason = git_repository_matches_head(root=root)
        if not matches:
            return None, reason or "PGO recipe source does not match clean HEAD"
        source = GitCommitRecipeSource(root, commit)
        source.entries()
        return source, None
    except (OSError, subprocess.TimeoutExpired, ValueError):
        return None, "could not read canonical PGO recipe bytes from Git"


class ImmutableInputStage:
    """Private read-only snapshot used for every timed benchmark launch."""

    def __init__(self, files: dict[str, Path], *, prefix: str) -> None:
        self.root = Path(tempfile.mkdtemp(prefix=prefix))
        self._closed = False
        try:
            _stage_helper.stage_named_files(
                destination=self.root,
                files=files,
                verbose=False,
            )
        except Exception:
            try:
                _stage_helper.remove_plain_tree(self.root)
            except Exception:
                pass
            raise
        atexit.register(self.close)

    def path(self, relative: str) -> Path:
        pure = PurePosixPath(relative)
        if (
            pure.is_absolute()
            or "\\" in relative
            or any(part in ("", ".", "..") for part in pure.parts)
            or relative != pure.as_posix()
        ):
            raise ValueError(f"invalid staged input name: {relative!r}")
        return self.root.joinpath(*pure.parts)

    def digests(self, names: Iterable[str]) -> dict[str, str | None]:
        return {name: file_digest(self.path(name)) for name in names}

    def close(self) -> None:
        if self._closed:
            return
        _stage_helper.remove_plain_tree(self.root)
        self._closed = True


def pgo_publication_input_paths(
    *,
    root: Path = ROOT,
    source: GitCommitRecipeSource | None = None,
) -> list[str] | None:
    """Derive every non-training benchmark/provenance input bound by PGO."""

    if source is not None:
        try:
            entries = source.entries()
            paths = {
                relative
                for relative, (mode, kind, _) in entries.items()
                if relative.startswith("bench/")
                and not relative.startswith("bench/pgo-training/")
                and PurePosixPath(relative).suffix.lower() in (".js", ".mjs", ".cjs")
                and kind == "blob"
                and mode in {"100644", "100755"}
            }
            if not paths:
                return None
            manifest_relative = "bench/hostile/manifest.json"
            manifest_bytes = source.read_bytes(manifest_relative)
            if manifest_bytes is None:
                return None
            manifest = json.loads(manifest_bytes.decode("utf-8"))
            if (
                not isinstance(manifest, dict)
                or type(manifest.get("schema_version")) is not int
                or manifest.get("schema_version") != 1
                or not isinstance(manifest.get("cases"), list)
                or not manifest["cases"]
            ):
                return None

            def resolve_manifest_input(value: Any) -> str | None:
                if (
                    not isinstance(value, str)
                    or not value
                    or value != value.strip()
                    or "\\" in value
                    or any(ord(character) < 0x20 for character in value)
                ):
                    return None
                pure = PurePosixPath(value)
                if (
                    pure.is_absolute()
                    or value.startswith("//")
                    or (len(value) >= 2 and value[0].isalpha() and value[1] == ":")
                    or any(part in ("", ".", "..") for part in pure.parts)
                    or value != pure.as_posix()
                ):
                    return None
                relative = (PurePosixPath("bench/hostile") / pure).as_posix()
                entry = entries.get(relative)
                if entry is None:
                    return None
                mode, kind, _ = entry
                return (
                    relative
                    if kind == "blob" and mode in {"100644", "100755"}
                    else None
                )

            for case in manifest["cases"]:
                if not isinstance(case, dict):
                    return None
                entry = resolve_manifest_input(case.get("entry"))
                if entry is None:
                    return None
                raw_inputs = case.get("inputs", [case.get("entry")])
                if not isinstance(raw_inputs, list) or not raw_inputs:
                    return None
                case_inputs: set[str] = set()
                for raw_input in raw_inputs:
                    resolved = resolve_manifest_input(raw_input)
                    if resolved is None or resolved in case_inputs:
                        return None
                    case_inputs.add(resolved)
                if entry not in case_inputs:
                    return None
                paths.update(case_inputs)
            return sorted(paths) if paths else None
        except (OSError, UnicodeError, ValueError, json.JSONDecodeError):
            return None

    try:
        resolved_root = root.resolve(strict=True)
        bench_root = (resolved_root / "bench").resolve(strict=True)
        listed = subprocess.run(
            ["git", "-C", str(resolved_root), "ls-files", "-z", "--", "bench"],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        ).stdout
        tracked_names = [name for name in listed.split(b"\0") if name]
        if not tracked_names:
            return None
        paths: set[Path] = set()
        for encoded_name in tracked_names:
            relative = os.fsdecode(encoded_name)
            pure = PurePosixPath(relative)
            if (
                len(pure.parts) < 2
                or pure.parts[0] != "bench"
                or pure.parts[1] == "pgo-training"
                or pure.suffix.lower() not in (".js", ".mjs", ".cjs")
            ):
                continue
            path = resolved_root.joinpath(*pure.parts)
            resolved = path.resolve(strict=True)
            resolved.relative_to(bench_root)
            if path.is_symlink() or not resolved.is_file():
                return None
            paths.add(resolved)
        if not paths:
            return None

        manifest_path = resolved_root / "bench" / "hostile" / "manifest.json"
        manifest_root = manifest_path.parent.resolve(strict=True)
        with manifest_path.open(encoding="utf-8") as handle:
            manifest = json.load(handle)
        if (
            not isinstance(manifest, dict)
            or manifest.get("schema_version") != 1
            or isinstance(manifest.get("schema_version"), bool)
            or not isinstance(manifest.get("cases"), list)
            or not manifest["cases"]
        ):
            return None

        def resolve_manifest_input(value: Any) -> Path | None:
            if (
                not isinstance(value, str)
                or not value
                or value != value.strip()
                or "\\" in value
                or any(ord(character) < 0x20 for character in value)
            ):
                return None
            pure = PurePosixPath(value)
            if (
                pure.is_absolute()
                or value.startswith("//")
                or (len(value) >= 2 and value[0].isalpha() and value[1] == ":")
                or any(part in ("", ".", "..") for part in pure.parts)
                or value != pure.as_posix()
            ):
                return None
            resolved = manifest_root.joinpath(*pure.parts).resolve(strict=True)
            resolved.relative_to(manifest_root)
            return resolved if resolved.is_file() else None

        for case in manifest["cases"]:
            if not isinstance(case, dict):
                return None
            entry = resolve_manifest_input(case.get("entry"))
            if entry is None:
                return None
            raw_inputs = case.get("inputs", [case.get("entry")])
            if not isinstance(raw_inputs, list) or not raw_inputs:
                return None
            case_inputs: set[Path] = set()
            for raw_input in raw_inputs:
                resolved = resolve_manifest_input(raw_input)
                if resolved is None or resolved in case_inputs:
                    return None
                case_inputs.add(resolved)
            if entry not in case_inputs:
                return None
            paths.update(case_inputs)

        relative_paths = sorted(
            path.relative_to(resolved_root).as_posix() for path in paths
        )
        return relative_paths if relative_paths else None
    except (OSError, ValueError, json.JSONDecodeError):
        return None


def pgo_training_recipe_digest(
    *,
    root: Path = ROOT,
    source: GitCommitRecipeSource | None = None,
) -> str | None:
    """Recompute the structural-similarity-guarded recipe used by ``pgo.sh``."""

    recipe = hashlib.sha256()

    def add(value: str) -> None:
        recipe.update(value.encode("utf-8"))
        recipe.update(b"\0")

    def digest(relative: str) -> str | None:
        return (
            source.digest(relative)
            if source is not None
            else file_digest(root / Path(relative))
        )

    script_digest = digest("tools/pgo.sh")
    if script_digest is None:
        return None
    add(PGO_RECIPE_VERSION)
    add(PGO_RECIPE_COMMAND)
    add("tools/pgo.sh")
    add(script_digest)
    validator_digest = digest(PGO_CORPUS_VALIDATOR)
    if validator_digest is None:
        return None
    add(PGO_SIMILARITY_POLICY)
    add(PGO_CORPUS_VALIDATOR)
    add(validator_digest)
    runner_digest = digest(PGO_TRAINING_RUNNER)
    manifest_digest = digest(PGO_EXPECTED_OUTPUT_MANIFEST)
    if runner_digest is None or manifest_digest is None:
        return None
    add(PGO_RUNNER_POLICY)
    add(PGO_TRAINING_RUNNER)
    add(runner_digest)
    add(PGO_EXPECTED_OUTPUT_MANIFEST)
    add(manifest_digest)
    for relative in PGO_TRAINING_INPUTS:
        input_digest = digest(relative)
        if input_digest is None:
            return None
        add(relative)
        add(input_digest)
    manifest_relative = "bench/hostile/manifest.json"
    hostile_manifest_digest = digest(manifest_relative)
    publication_inputs = pgo_publication_input_paths(root=root, source=source)
    if hostile_manifest_digest is None or publication_inputs is None:
        return None
    add(manifest_relative)
    add(hostile_manifest_digest)
    add(PGO_EXCLUDED_INPUTS_LABEL)
    for relative in publication_inputs:
        input_digest = digest(relative)
        if input_digest is None:
            return None
        add(relative)
        add(input_digest)
    return recipe.hexdigest()


def pgo_build_recipe_digest(
    identity: dict[str, Any],
    *,
    root: Path = ROOT,
    source: GitCommitRecipeSource | None = None,
) -> str | None:
    """Recompute the canonical build recipe stamped by ``pgo.sh``."""

    required_text = {
        "pgo_training_recipe_sha256": identity.get(
            "pgo_training_recipe_sha256"
        ),
        "pgo_profile_sha256": identity.get("pgo_profile_sha256"),
        "pgo_cargo_identity": identity.get("pgo_cargo_identity"),
        "pgo_cargo_sha256": identity.get("pgo_cargo_sha256"),
        "rustc": identity.get("rustc"),
        "pgo_rustc_sha256": identity.get("pgo_rustc_sha256"),
        "pgo_linker_identity": identity.get("pgo_linker_identity"),
        "pgo_linker_sha256": identity.get("pgo_linker_sha256"),
        "pgo_build_environment_policy": identity.get(
            "pgo_build_environment_policy"
        ),
        "pgo_build_environment_sha256": identity.get(
            "pgo_build_environment_sha256"
        ),
        "pgo_source_snapshot_sha256": identity.get("pgo_source_snapshot_sha256"),
        "pgo_msvc_cl_identity": identity.get("pgo_msvc_cl_identity"),
        "pgo_msvc_cl_sha256": identity.get("pgo_msvc_cl_sha256"),
        "pgo_msvc_lib_identity": identity.get("pgo_msvc_lib_identity"),
        "pgo_msvc_lib_sha256": identity.get("pgo_msvc_lib_sha256"),
    }
    if any(not isinstance(value, str) or not value for value in required_text.values()):
        return None

    recipe = hashlib.sha256()

    def add(value: str) -> None:
        recipe.update(value.encode("utf-8"))
        recipe.update(b"\0")

    def digest(relative: str) -> str | None:
        return (
            source.digest(relative)
            if source is not None
            else file_digest(root / Path(relative))
        )

    script_digest = digest("tools/pgo.sh")
    if script_digest is None:
        return None
    add(PGO_BUILD_RECIPE_VERSION)
    add(PGO_BUILD_CONTRACT)
    add("tools/pgo.sh")
    add(script_digest)
    for label, field in (
        ("pgo-training-recipe-sha256", "pgo_training_recipe_sha256"),
        ("pgo-profile-sha256", "pgo_profile_sha256"),
        ("cargo-identity", "pgo_cargo_identity"),
        ("cargo-sha256", "pgo_cargo_sha256"),
        ("rustc-identity", "rustc"),
        ("rustc-sha256", "pgo_rustc_sha256"),
        ("linker-identity", "pgo_linker_identity"),
        ("linker-sha256", "pgo_linker_sha256"),
        ("msvc-cl-identity", "pgo_msvc_cl_identity"),
        ("msvc-cl-sha256", "pgo_msvc_cl_sha256"),
        ("msvc-lib-identity", "pgo_msvc_lib_identity"),
        ("msvc-lib-sha256", "pgo_msvc_lib_sha256"),
        ("source-snapshot-sha256", "pgo_source_snapshot_sha256"),
        ("build-environment-policy", "pgo_build_environment_policy"),
        ("build-environment-sha256", "pgo_build_environment_sha256"),
    ):
        add(label)
        add(required_text[field])
    for relative in PGO_BUILD_DEFINITION_FILES:
        definition_digest = digest(relative)
        if definition_digest is None:
            return None
        add(relative)
        add(definition_digest)
    return recipe.hexdigest()


def resolved_executable(cmd: list[str]) -> Path | None:
    raw = Path(cmd[0])
    if raw.is_file():
        return raw.resolve()
    found = shutil.which(cmd[0])
    return Path(found).resolve() if found else None


def canonical_engine_command(
    name: str,
    cmd: list[str],
    timeout: float,
    *,
    process_env: dict[str, str] | None = None,
    fresh_environment: bool = False,
) -> list[str]:
    """Resolve a canonical engine launcher to the native process it executes.

    Package-manager shims are mutable indirection: hashing `bun.cmd` while its
    `bun.exe` changes would make the before/after drift proof meaningless. Ask
    each external engine for its own executable path, validate it, and execute
    that file directly for both measurement and metadata probes.
    """

    probes = {
        "node": ["-p", "process.execPath"],
        "bun": ["-e", "console.log(process.execPath)"],
        "deno": ["eval", "console.log(Deno.execPath())"],
    }
    probe_args = probes.get(name)
    if probe_args is None:
        return cmd
    try:
        with benchmark_process_environment(
            process_env=process_env,
            fresh_environment=fresh_environment,
            prefix="zipp-benchmark-resolve-",
        ) as child_env:
            probe = subprocess.run(
                [cmd[0], *probe_args],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                env=child_env,
                timeout=min(timeout, 10.0),
                check=False,
            )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ValueError(f"{name}: cannot resolve native engine executable") from exc
    lines = probe.stdout.decode("utf-8", errors="replace").strip().splitlines()
    if probe.returncode != 0 or len(lines) != 1:
        raise ValueError(f"{name}: invalid native executable probe")
    raw_target = Path(lines[0].strip())
    if not raw_target.is_absolute():
        raise ValueError(f"{name}: engine reported a non-absolute executable path")
    target = raw_target.resolve()
    if not target.is_file():
        raise ValueError(f"{name}: reported native executable does not exist: {target}")
    return [str(target), *cmd[1:]]


def engine_metadata(
    name: str,
    cmd: list[str],
    timeout: float,
    *,
    process_env: dict[str, str] | None = None,
    fresh_environment: bool = False,
) -> dict[str, Any]:
    executable = resolved_executable(cmd)
    stat = executable.stat() if executable and executable.is_file() else None
    version = None
    version_probe_error = None
    if executable:
        try:
            with benchmark_process_environment(
                process_env=process_env,
                fresh_environment=fresh_environment,
                prefix="zipp-benchmark-version-",
            ) as child_env:
                probe = subprocess.run(
                    [str(executable), "--version"],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    env=child_env,
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
        "build_identity": build_identity(
            executable,
            timeout,
            process_env=process_env,
            fresh_environment=fresh_environment,
        ),
    }


def build_identity(
    executable: Path | None,
    timeout: float,
    *,
    process_env: dict[str, str] | None = None,
    fresh_environment: bool = False,
) -> dict[str, Any] | None:
    """`zipp --version --json`, parsed. ``None`` for an engine without it (node)."""
    if not executable:
        return None
    try:
        with benchmark_process_environment(
            process_env=process_env,
            fresh_environment=fresh_environment,
            prefix="zipp-benchmark-identity-",
        ) as child_env:
            probe = subprocess.run(
                [str(executable), "--version", "--json"],
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                env=child_env,
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


def source_identity(identity: dict[str, Any] | None) -> str | None:
    """The one string that names the SOURCE a binary was built from.

    `commit` alone is not it: a dirty build's commit is the parent it was based
    on, which is exactly how an artifact came to be named for a commit it was not
    measuring. The `+dirty.<digest>` suffix is what makes the two distinguishable.
    """
    if not isinstance(identity, dict):
        return None
    # `zipp --version --json` composes this itself; prefer the binary's own
    # answer over reconstructing it, so the two can never drift apart.
    reported = identity.get("source")
    if isinstance(reported, str) and reported:
        return reported
    commit = identity.get("commit")
    if not isinstance(commit, str) or not commit:
        return None
    if identity.get("dirty"):
        digest = identity.get("diff_digest")
        return f"{commit}+dirty.{digest}" if digest else f"{commit}+dirty"
    return commit


def check_engine_provenance(
    engines_meta: list[dict[str, Any]],
    workspace_commit: str | None,
    *,
    is_ab: bool,
    allow_dirty: bool,
    allow_nonhead: bool,
    ab_sides_distinguished: bool = False,
) -> list[str]:
    """Reasons this run's engines cannot back a PUBLISHED number.

    Two different questions, deliberately answered differently:

    A HEADLINE capture (Node vs zipp, the thing README quotes) claims to measure
    a commit, so it has to be measuring that commit: identity present, tree not
    dirty, and the engine's commit equal to the workspace HEAD. Failing these is
    fatal unless explicitly overridden, because the artifact's whole value is the
    attribution.

    An A/B compares two builds that by construction cannot both be HEAD, and the
    ablation idiom this repo uses most is ONE binary with two ``--ab-env`` sides,
    where both sides report the SAME source on purpose. So an A/B is never
    blocked here; its reasons only mark the artifact unpublishable, which it
    already is -- an A/B measures a delta, not a headline. The failure an A/B
    does need to catch (a rebuild that silently did not happen) is caught by
    ``reject_identical_ab_binaries`` on the binary HASH, before measurement, and
    is repeated here on the reported SOURCE for the case where the two binaries
    differ but were built from the same tree.
    """
    reasons: list[str] = []
    identified = [
        (m["name"], source_identity(m.get("build_identity")), m.get("build_identity"))
        for m in engines_meta
        if m.get("build_identity") is not None
    ]
    # An engine with no `--version --json` at all (node, bun, deno) is not a zipp
    # build and is out of scope -- its version string is all the provenance it
    # has. But if NOTHING under measurement identifies its source, the artifact
    # names a commit it cannot support.
    if not identified:
        reasons.append(
            "no engine reported a build identity (`--version --json`); "
            "nothing identifies the source under measurement"
        )
        return reasons
    for name, source, identity in identified:
        if source is None:
            reasons.append(f"{name}: build identity present but has no commit")
            continue
        if identity.get("dirty") and not allow_dirty:
            reasons.append(
                f"{name}: built from a DIRTY tree ({source}); the commit it "
                "names is the parent it was based on, not the code that ran"
            )
        if is_ab:
            continue
        if workspace_commit is None:
            reasons.append(
                f"{name}: workspace HEAD is unknown, so the engine's commit "
                "cannot be checked against it"
            )
        elif identity.get("commit") != workspace_commit and not allow_nonhead:
            reasons.append(
                f"{name}: built from {identity.get('commit')} but the workspace "
                f"is at {workspace_commit}"
            )
    if is_ab and len(identified) == 2 and not ab_sides_distinguished:
        (name_a, src_a, _), (name_b, src_b, _) = identified
        if src_a is not None and src_a == src_b:
            reasons.append(
                f"--ab sides {name_a} and {name_b} report the SAME source "
                f"({src_a}); one of them was not rebuilt"
            )
    return reasons


def pgo_build_reasons(
    engines_meta: list[dict[str, Any]],
    *,
    require_pgo: bool,
    source_resolver: Callable[
        [dict[str, Any]], tuple[GitCommitRecipeSource | None, str | None]
    ]
    | None = None,
) -> list[str]:
    """Reject incomplete or contradictory PGO identities for publication.

    A non-PGO binary remains useful for diagnostics, so these are publication
    reasons rather than fatal source-attribution errors.  A claimed PGO build,
    however, must bind both the exact merged profile and its independent training
    recipe into the binary that was measured.  The v2 build contract also binds
    target, features, optimization/profile settings, exact codegen flags,
    Rust and MSVC tool identities, an immutable source snapshot, the controlled
    build environment, and every Cargo definition file that can alter the
    resulting executable.
    """

    reasons: list[str] = []
    resolve_source = source_resolver or canonical_recipe_source_for_identity
    canonical_training_recipes: dict[str, str | None] = {}
    canonical_sources: dict[
        str, tuple[GitCommitRecipeSource | None, str | None]
    ] = {}
    saw_build_identity = False

    def valid_digest(value: Any) -> bool:
        return isinstance(value, str) and bool(re.fullmatch(r"[0-9a-f]{64}", value))

    for metadata in engines_meta:
        identity = metadata.get("build_identity")
        if not isinstance(identity, dict):
            continue
        if identity.get("name") != "zipp" and not any(
            field in identity
            for field in (
                "pgo_profile_sha256",
                "pgo_training_recipe_sha256",
                "rustflags_source",
            )
        ):
            continue
        saw_build_identity = True
        name = metadata["name"]
        rustflags = identity.get("rustflags")
        rustflags_source = identity.get("rustflags_source")
        training_recipe_hash = identity.get("pgo_training_recipe_sha256")
        build_recipe_hash = identity.get("pgo_build_recipe_sha256")
        uses_profile = isinstance(rustflags, str) and "profile-use=" in rustflags
        claims_pgo = uses_profile or any(
            identity.get(field)
            for field in (
                "pgo_profile_sha256",
                "pgo_training_recipe_sha256",
                "pgo_build_recipe_sha256",
                "pgo_build_contract",
            )
        )
        if not claims_pgo:
            if require_pgo:
                reasons.append(
                    f"{name}: publication protocol requires a provenance-stamped "
                    "canonical PGO build"
                )
            continue
        commit = identity.get("commit")
        source_key = (
            f"{commit}:{identity.get('dirty')!r}"
            if isinstance(commit, str)
            else f"missing:{identity.get('dirty')!r}"
        )
        if source_key not in canonical_sources:
            canonical_sources[source_key] = resolve_source(identity)
        recipe_source, recipe_source_reason = canonical_sources[source_key]
        if recipe_source is None:
            reasons.append(
                f"{name}: cannot verify canonical Git-blob PGO recipe bytes: "
                f"{recipe_source_reason or 'source unavailable'}"
            )
        if not uses_profile:
            reasons.append(f"{name}: PGO hashes are present without profile-use")
        if rustflags_source != "CARGO_ENCODED_RUSTFLAGS":
            reasons.append(
                f"{name}: PGO profile-use was not reported from "
                "CARGO_ENCODED_RUSTFLAGS"
            )
        if rustflags != PGO_CANONICAL_RUSTFLAGS:
            reasons.append(
                f"{name}: noncanonical PGO rustflags (extra, missing, or reordered "
                "codegen flags)"
            )
        for field, expected, label in (
            ("target", PGO_CANONICAL_TARGET, "target"),
            ("profile", "release", "Cargo profile"),
            ("opt_level", "3", "optimization level"),
            ("features", "", "CLI feature set"),
            ("pgo_build_contract", PGO_BUILD_CONTRACT, "PGO build contract"),
            (
                "pgo_build_environment_policy",
                PGO_BUILD_ENVIRONMENT_POLICY,
                "PGO build environment policy",
            ),
        ):
            if identity.get(field) != expected:
                reasons.append(f"{name}: noncanonical {label}")
        if identity.get("jit") is not True:
            reasons.append(f"{name}: canonical publication build must enable the JIT")

        for field, label in (
            ("pgo_profile_sha256", "profile hash"),
            ("pgo_training_recipe_sha256", "training recipe hash"),
            ("pgo_build_recipe_sha256", "build recipe hash"),
            ("pgo_build_environment_sha256", "build environment hash"),
            ("pgo_cargo_sha256", "Cargo executable hash"),
            ("pgo_rustc_sha256", "rustc executable hash"),
            ("pgo_linker_sha256", "linker executable hash"),
            ("pgo_msvc_cl_sha256", "MSVC compiler executable hash"),
            ("pgo_msvc_lib_sha256", "MSVC librarian executable hash"),
            ("pgo_source_snapshot_sha256", "source snapshot hash"),
        ):
            if not valid_digest(identity.get(field)):
                reasons.append(f"{name}: PGO build has no valid lowercase {label}")
        for field, label in (
            ("rustc", "rustc identity"),
            ("pgo_cargo_identity", "Cargo identity"),
            ("pgo_linker_identity", "linker identity"),
            ("pgo_msvc_cl_identity", "MSVC compiler identity"),
            ("pgo_msvc_lib_identity", "MSVC librarian identity"),
        ):
            value = identity.get(field)
            if not isinstance(value, str) or not value:
                reasons.append(f"{name}: PGO build has no valid {label}")

        if valid_digest(training_recipe_hash):
            if recipe_source is not None and source_key not in canonical_training_recipes:
                canonical_training_recipes[source_key] = pgo_training_recipe_digest(
                    source=recipe_source
                )
            canonical_training_recipe = canonical_training_recipes.get(source_key)
            if recipe_source is None:
                pass
            elif canonical_training_recipe is None:
                reasons.append(
                    f"{name}: canonical Git-blob PGO training recipe could not be hashed"
                )
            elif training_recipe_hash != canonical_training_recipe:
                reasons.append(
                    f"{name}: PGO training recipe hash does not match the current "
                    "structural-similarity-guarded canonical recipe"
                )

        expected_source_snapshot = (
            recipe_source.snapshot_digest() if recipe_source is not None else None
        )
        if recipe_source is None:
            pass
        elif expected_source_snapshot is None:
            reasons.append(
                f"{name}: canonical Git-blob source snapshot could not be hashed"
            )
        elif identity.get("pgo_source_snapshot_sha256") != expected_source_snapshot:
            reasons.append(
                f"{name}: PGO source snapshot hash does not match the canonical "
                "Git commit bytes"
            )

        recomputed_build_recipe = (
            pgo_build_recipe_digest(identity, source=recipe_source)
            if recipe_source is not None
            else None
        )
        if recipe_source is None:
            pass
        elif recomputed_build_recipe is None:
            reasons.append(
                f"{name}: current canonical PGO build recipe could not be reconstructed"
            )
        elif build_recipe_hash != recomputed_build_recipe:
            reasons.append(
                f"{name}: PGO build recipe hash does not match the canonical build "
                "contract and current build definitions"
            )
    if require_pgo and not saw_build_identity:
        reasons.append(
            "stored engines do not include the build identity required for PGO provenance"
        )
    return reasons


def provenance_is_fatal(reasons: list[str], *, is_ab: bool) -> bool:
    """Whether `reasons` should stop the run rather than just mark the artifact.

    Only a headline capture is stopped. See `check_engine_provenance`.
    """
    return bool(reasons) and not is_ab


def provenance_assessment(
    engines_meta: list[dict[str, Any]],
    workspace_commit: str | None,
    *,
    is_ab: bool,
    allow_dirty: bool,
    allow_nonhead: bool,
    ab_sides_distinguished: bool = False,
) -> tuple[list[str], list[str]]:
    """Return ``(recorded violations, violations not covered by overrides)``.

    An override permits a directional run; it must never erase the reason from
    the artifact or make that artifact publishable. Keeping the two views
    separate also prevents ``--allow-dirty-engine`` from accidentally covering
    an unrelated non-HEAD binary (and vice versa).
    """

    recorded = check_engine_provenance(
        engines_meta,
        workspace_commit,
        is_ab=is_ab,
        allow_dirty=False,
        allow_nonhead=False,
        ab_sides_distinguished=ab_sides_distinguished,
    )
    recorded.extend(pgo_build_reasons(engines_meta, require_pgo=not is_ab))
    uncovered = check_engine_provenance(
        engines_meta,
        workspace_commit,
        is_ab=is_ab,
        allow_dirty=allow_dirty,
        allow_nonhead=allow_nonhead,
        ab_sides_distinguished=ab_sides_distinguished,
    )
    return recorded, uncovered


def engine_drift(
    before: list[dict[str, Any]], after: list[dict[str, Any]]
) -> list[str]:
    """Engines whose binary or reported source CHANGED during the run.

    A rebuild landing mid-measurement silently mixes two engines into one column.
    Cheap to detect (one stat + one `--version --json` per engine) and otherwise
    invisible.
    """
    drift: list[str] = []
    for old_meta, new_meta in zip(before, after):
        name = old_meta["name"]
        if old_meta.get("sha256") != new_meta.get("sha256"):
            drift.append(
                f"{name}: executable sha256 changed during the run "
                f"({old_meta.get('sha256')} -> {new_meta.get('sha256')})"
            )
        old_src = source_identity(old_meta.get("build_identity"))
        new_src = source_identity(new_meta.get("build_identity"))
        if old_src != new_src:
            drift.append(
                f"{name}: build identity changed during the run "
                f"({old_src} -> {new_src})"
            )
    return drift


def harness_digest() -> dict[str, Any]:
    """Hash the harness itself, so an artifact records how it was measured."""
    return {
        "bench_py_sha256": file_digest(Path(__file__).resolve()),
        "run_real_sh_sha256": file_digest(ROOT / "bench" / "run_real.sh"),
        "pgo_training_py_sha256": file_digest(_STAGE_HELPER_PATH.resolve()),
    }


def bench_input_digests(bench_dir: Path, benches: list[str]) -> dict[str, str | None]:
    """Hash every benchmark program actually run.

    A row's number means nothing without the program that produced it, and these
    files do change (B67 corrected three of them).
    """
    return {b: file_digest(bench_dir / f"{b}.js") for b in benches}


def digest_mapping_drift(
    before: dict[str, Any], after: dict[str, Any], *, kind: str
) -> list[str]:
    """Name harness/input files whose bytes changed during measurement."""

    drift: list[str] = []
    for name in sorted(set(before) | set(after)):
        old_digest = before.get(name)
        new_digest = after.get(name)
        if old_digest != new_digest:
            drift.append(
                f"{kind} {name} changed during run "
                f"({old_digest} -> {new_digest})"
            )
    return drift


def publication_policy_reasons(
    *,
    is_ab: bool,
    canonical_inputs: bool,
    engine_names: list[str],
    baseline: str,
    metric: str,
    historical: bool,
    reps: int,
    bootstrap_samples: int,
    source_reason: str | None,
    environment: dict[str, str],
) -> list[str]:
    """Name choices that make a real-suite artifact diagnostic-only."""

    reasons: list[str] = []
    if is_ab:
        reasons.append("A/B comparison (headline publication requires an engine table)")
    if not canonical_inputs:
        reasons.append("alternate or filtered benchmark corpus (all default rows required)")
    if tuple(engine_names) != CANONICAL_ENGINE_NAMES:
        reasons.append(
            "noncanonical engine table (publication requires node, bun, deno, and zipp)"
        )
    if baseline != "node":
        reasons.append("noncanonical baseline (publication requires node)")
    if metric != "cold":
        reasons.append("noncanonical headline metric (publication requires cold wall time)")
    if historical:
        reasons.append("historical report mode (publication requires the modern report)")
    if reps < MIN_PUBLISHABLE_REPS:
        reasons.append(
            f"only {reps} repetitions (at least {MIN_PUBLISHABLE_REPS} required)"
        )
    if bootstrap_samples < BOOTSTRAP_SAMPLES:
        reasons.append(
            f"only {bootstrap_samples} bootstrap samples "
            f"(at least {BOOTSTRAP_SAMPLES} required)"
        )
    if source_reason is not None:
        reasons.append(source_reason)
    if environment:
        reasons.append(
            "benchmark-affecting environment variables are set "
            "(canonical publication requires a clean inherited environment)"
        )
    return reasons


def artifact_publishable(
    provenance_reasons: list[str],
    engine_drift_reasons: list[str],
    source_drift: list[str],
    *,
    all_correct: bool,
    publication_reasons: list[str],
) -> bool:
    """Fail closed unless both measurements and publication inputs are sound."""

    return (
        all_correct
        and not provenance_reasons
        and not engine_drift_reasons
        and not source_drift
        and not publication_reasons
    )


def sample_completeness_failures(
    cold: dict[str, dict[str, list[float]]],
    startup: dict[str, dict[str, list[float]]],
    adjusted: dict[str, dict[str, list[float]]],
    engine_names: list[str],
    benches: list[str],
    reps: int,
) -> list[str]:
    """Require one valid, still-paired sample per rep/engine/benchmark."""

    failures: list[str] = []
    for name in engine_names:
        for bench in benches:
            counts = {
                "cold": len(cold[name][bench]),
                "startup": len(startup[name][bench]),
                "adjusted": len(adjusted[name][bench]),
            }
            if any(count != reps for count in counts.values()):
                failures.append(
                    f"incomplete samples for {name}/{bench}: expected {reps}, "
                    + ", ".join(f"{metric}={count}" for metric, count in counts.items())
                )
    return failures


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


def git_paths_match_head(
    paths: Iterable[Path], *, root: Path = ROOT
) -> tuple[bool, str | None]:
    """Require publication bytes to equal regular-file blobs stored in HEAD."""

    resolved_root = root.resolve()
    relative_paths: set[str] = set()
    try:
        for path in paths:
            candidate = path if path.is_absolute() else resolved_root / path
            absolute_path = Path(os.path.abspath(candidate))
            relative_paths.add(
                absolute_path.relative_to(resolved_root).as_posix()
            )
    except (OSError, ValueError):
        return False, "publication source is outside the repository"
    if not relative_paths:
        return False, "publication source set is empty"

    pathspec = sorted(relative_paths)
    try:
        tree_probe = subprocess.run(
            ["git", "ls-tree", "-r", "-z", "HEAD", "--", *pathspec],
            cwd=resolved_root,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=10,
            check=False,
        )
        if tree_probe.returncode != 0:
            return False, "could not verify publication sources against HEAD"
        head_entries: dict[str, tuple[str, str, str]] = {}
        for item in tree_probe.stdout.split(b"\0"):
            if not item:
                continue
            try:
                metadata, raw_path = item.split(b"\t", 1)
                raw_mode, raw_kind, raw_oid = metadata.split(b" ", 2)
            except ValueError:
                return False, "could not parse publication sources from HEAD"
            rel = raw_path.decode("utf-8", errors="surrogateescape")
            head_entries[rel] = (
                raw_mode.decode("ascii", errors="strict"),
                raw_kind.decode("ascii", errors="strict"),
                raw_oid.decode("ascii", errors="strict"),
            )
        if not relative_paths.issubset(head_entries):
            return False, "publication sources include untracked files"

        for rel in pathspec:
            mode, kind, oid = head_entries[rel]
            if kind != "blob" or mode not in {"100644", "100755"}:
                return False, "publication source in HEAD is not a regular file"
            working_path = resolved_root / Path(rel)
            if working_path.is_symlink() or not working_path.is_file():
                return False, "working publication source is not a regular file"
            # Read and clean-filter the actual working file, independently of
            # the index. This honors checked-out EOL/attribute normalization
            # while defeating assume-unchanged and skip-worktree flags.
            hash_probe = subprocess.run(
                ["git", "hash-object", f"--path={rel}", "--", rel],
                cwd=resolved_root,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                timeout=10,
                check=False,
            )
            if hash_probe.returncode != 0:
                return False, "could not hash a working publication source"
            if hash_probe.stdout.decode("ascii", errors="strict").strip() != oid:
                return False, "manifest, harness, or declared inputs differ from HEAD"
        return True, None
    except (OSError, subprocess.TimeoutExpired, UnicodeError):
        return False, "could not verify publication sources against HEAD"


def git_repository_matches_head(*, root: Path = ROOT) -> tuple[bool, str | None]:
    """Require the complete checkout and index to match regular files in HEAD.

    The fast status check catches staged, tracked, deleted, and untracked files.
    A second index-flag check rejects skip-worktree/assume-unchanged hiding, then
    ``git_paths_match_head`` hashes every tracked working file independently of
    the index's cached stat data.  This is intentionally strict: publication
    must attribute an engine binary to all source bytes in its checkout.
    """

    resolved_root = root.resolve()
    try:
        status = subprocess.run(
            [
                "git",
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
            cwd=resolved_root,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=20,
            check=False,
        )
        if status.returncode != 0:
            return False, "could not verify the repository worktree against HEAD"
        if status.stdout:
            return (
                False,
                "repository worktree/index contains tracked changes or untracked files",
            )

        tracked = subprocess.run(
            ["git", "ls-files", "-v", "-z"],
            cwd=resolved_root,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=20,
            check=False,
        )
        if tracked.returncode != 0:
            return False, "could not enumerate repository files for publication"
        paths: list[Path] = []
        for item in tracked.stdout.split(b"\0"):
            if not item:
                continue
            if len(item) < 3 or item[1:2] != b" ":
                return False, "could not parse repository files for publication"
            tag = item[:1]
            if tag != b"H":
                return (
                    False,
                    "repository index uses hidden or nonordinary tracked-file flags",
                )
            paths.append(
                resolved_root
                / Path(item[2:].decode("utf-8", errors="surrogateescape"))
            )
        if not paths:
            return False, "repository has no tracked publication source files"
        matches, reason = git_paths_match_head(paths, root=resolved_root)
        if not matches:
            return False, reason or "repository files differ from HEAD"
        return True, None
    except (OSError, subprocess.TimeoutExpired, UnicodeError):
        return False, "could not verify the repository worktree against HEAD"


def replay_current_source_reasons(
    replay: dict[str, Any], benches: list[str]
) -> list[str]:
    """Bind README replay to the current checkout and canonical input bytes.

    A stored result can remain useful as a diagnostic after the repository moves,
    but it must not rewrite the live README for a different commit, harness, or
    benchmark corpus.  Revalidate the saved before/after envelope rather than
    trusting the artifact's historical ``publishable`` boolean.
    """

    reasons: list[str] = []
    canonical_benches = discover_benches(BENCH_DIR)
    if benches != canonical_benches:
        return [
            "stored artifact does not contain the current canonical benchmark set"
        ]

    stored_source = replay.get("workspace_source")
    stored_source_before = replay.get("workspace_source_before")
    stored_source_after = replay.get("workspace_source_after")
    current_source = git_revision()
    if not isinstance(stored_source, str) or not stored_source:
        reasons.append("stored artifact has no valid workspace source identity")
    elif stored_source_before != stored_source or stored_source_after != stored_source:
        reasons.append("stored workspace HEAD changed during the benchmark run")
    elif current_source is None:
        reasons.append("could not resolve the current HEAD for replay publication")
    elif current_source != stored_source:
        reasons.append(
            "current HEAD differs from the stored benchmark source "
            f"({current_source} != {stored_source})"
        )

    publication_paths = {
        Path(__file__).resolve(),
        _STAGE_HELPER_PATH.resolve(),
        ROOT / "bench" / "run_real.sh",
        *(BENCH_DIR / f"{bench}.js" for bench in benches),
    }
    sources_match, source_reason = git_paths_match_head(publication_paths)
    if not sources_match:
        reasons.append(
            source_reason
            or "current publication sources could not be verified against HEAD"
        )
    repository_matches, repository_reason = git_repository_matches_head()
    if not repository_matches:
        reasons.append(
            repository_reason
            or "current repository worktree could not be verified against HEAD"
        )

    stored_harness_before = replay.get("harness_sha256_before")
    stored_harness_after = replay.get("harness_sha256_after")
    if not isinstance(stored_harness_before, dict) or not isinstance(
        stored_harness_after, dict
    ):
        reasons.append("stored artifact has invalid harness digest metadata")
    elif stored_harness_before != stored_harness_after:
        reasons.append("stored harness bytes changed during the benchmark run")
    elif harness_digest() != stored_harness_after:
        reasons.append("current harness bytes differ from the stored artifact")

    stored_inputs_before = replay.get("bench_input_sha256_before")
    stored_inputs_after = replay.get("bench_input_sha256_after")
    if not isinstance(stored_inputs_before, dict) or not isinstance(
        stored_inputs_after, dict
    ):
        reasons.append("stored artifact has invalid benchmark input digest metadata")
    elif stored_inputs_before != stored_inputs_after:
        reasons.append("stored benchmark input bytes changed during the run")
    elif bench_input_digests(BENCH_DIR, benches) != stored_inputs_after:
        reasons.append("current benchmark input bytes differ from the stored artifact")

    engine_names = replay.get("engine_names", [])
    expected_engines = set(engine_names) if isinstance(engine_names, list) else set()
    source_before = replay.get("engine_source_before")
    source_after = replay.get("engine_source_after")
    if (
        not isinstance(source_before, dict)
        or not isinstance(source_after, dict)
        or set(source_before) != expected_engines
        or set(source_after) != expected_engines
    ):
        reasons.append("stored artifact has incomplete engine source metadata")
    elif source_before != source_after:
        reasons.append("stored engine sources changed during the benchmark run")
    elif isinstance(stored_source, str) and source_after.get("zipp") != stored_source:
        reasons.append("stored Zipp engine source does not match the workspace source")

    binary_before = replay.get("engine_binary_sha_before")
    binary_after = replay.get("engine_binary_sha_after")
    if (
        not isinstance(binary_before, dict)
        or not isinstance(binary_after, dict)
        or set(binary_before) != expected_engines
        or set(binary_after) != expected_engines
    ):
        reasons.append("stored artifact has incomplete engine binary metadata")
    elif binary_before != binary_after:
        reasons.append("stored engine binaries changed during the benchmark run")
    elif any(
        not isinstance(digest, str)
        or len(digest) != 64
        or any(ch not in "0123456789abcdefABCDEF" for ch in digest)
        for digest in binary_after.values()
    ):
        reasons.append("stored artifact has invalid engine binary digests")

    engines_meta = replay.get("engines_meta")
    metadata_by_name = (
        {metadata.get("name"): metadata for metadata in engines_meta}
        if isinstance(engines_meta, list)
        and all(isinstance(metadata, dict) for metadata in engines_meta)
        else {}
    )
    if set(metadata_by_name) != expected_engines:
        reasons.append("stored artifact has incomplete engine metadata")
    else:
        metadata_sources = {
            name: source_identity(metadata.get("build_identity"))
            for name, metadata in metadata_by_name.items()
        }
        metadata_binaries = {
            name: metadata.get("sha256")
            for name, metadata in metadata_by_name.items()
        }
        if isinstance(source_after, dict) and metadata_sources != source_after:
            reasons.append(
                "stored engine metadata contradicts the engine source envelope"
            )
        if isinstance(binary_after, dict) and metadata_binaries != binary_after:
            reasons.append(
                "stored engine metadata contradicts the engine binary envelope"
            )

        zipp_metadata = metadata_by_name.get("zipp")
        zipp_identity = (
            zipp_metadata.get("build_identity")
            if isinstance(zipp_metadata, dict)
            else None
        )
        if not isinstance(zipp_identity, dict) or zipp_identity.get("name") != "zipp":
            reasons.append("stored Zipp metadata has no valid Zipp build identity")
        else:
            reasons.extend(
                check_engine_provenance(
                    [zipp_metadata],
                    current_source,
                    is_ab=False,
                    allow_dirty=False,
                    allow_nonhead=False,
                )
            )

    if replay.get("publication_sources_head_before") is not True or replay.get(
        "publication_sources_head_after"
    ) is not True:
        reasons.append("stored publication sources were not clean against HEAD")
    if replay.get("repository_head_before") is not True or replay.get(
        "repository_head_after"
    ) is not True:
        reasons.append("stored repository worktree was not clean against HEAD")

    stored_environment_policy = replay.get("benchmark_environment_policy")
    current_environment_policy = canonical_benchmark_environment_descriptor()
    if stored_environment_policy != current_environment_policy:
        reasons.append(
            "stored benchmark child-environment policy is missing or noncanonical"
        )
    if replay.get("benchmark_input_staging_policy") != BENCHMARK_INPUT_STAGING_POLICY:
        reasons.append(
            "stored benchmark input-staging policy is missing or noncanonical"
        )

    return reasons


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


def recorded_environment(environment: dict[str, str]) -> dict[str, str]:
    """Return benchmark controls without copying credentials into artifacts.

    Only explicitly audited numeric/boolean controls are safe and useful to
    retain. Every unknown key is represented only as redacted; broad runtime
    prefixes must never serialize auth tokens, private paths, or arbitrary
    option payloads into a potentially public JSON result.
    """

    recorded: dict[str, str] = {}
    for key, value in sorted(environment.items()):
        upper_key = key.upper()
        if (
            not upper_key.startswith(_RECORDED_ENV_PREFIXES)
            and upper_key not in _PUBLIC_CONTROL_ENV_KEYS
        ):
            continue
        components = tuple(
            component
            for component in re.split(r"[^A-Z0-9]+", upper_key)
            if component
        )
        sensitive_name = any(
            component in _SENSITIVE_ENV_COMPONENTS
            or component.endswith("TOKEN")
            or component.endswith("SECRET")
            or component.endswith("PASSWORD")
            for component in components
        )
        public_control = (
            upper_key in _PUBLIC_CONTROL_ENV_KEYS
            and _PUBLIC_CONTROL_VALUE.fullmatch(value) is not None
        )
        recorded[key] = (
            value if public_control and not sensitive_name else _REDACTED_ENV_VALUE
        )
    return recorded


def relevant_environment() -> dict[str, str]:
    """Return the current process's safely recordable benchmark controls."""

    return recorded_environment(dict(os.environ))


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
            "paired_ratio_ci95_method": DESCRIPTIVE_BOOTSTRAP_METHOD,
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
        "paired_ratio_ci95_method": DESCRIPTIVE_BOOTSTRAP_METHOD,
        "nonpositive_pairs": 0,
    }


def all_competitor_comparison_summary(
    samples: dict[str, dict[str, list[float]]],
    benches: list[str],
    engine_names: list[str],
    target: str,
    *,
    seed: int,
    bootstrap_samples: int,
) -> dict[str, Any]:
    """Evaluate the literal target-vs-every-engine, every-row criterion.

    A median paired ratio below one is required as the effect-direction point
    estimate. Statistical evidence comes from an exact one-sided paired sign
    test of H0: strict per-pair win probability <= 0.5. Bonferroni correction
    covers the complete row-by-competitor family. Percentile-bootstrap ratio
    intervals remain descriptive estimates and never decide this gate.
    """

    competitors = [name for name in engine_names if name != target]
    total = len(benches) * len(competitors)
    adjusted_alpha = 0.05 / total if total else 0.05
    rows: dict[str, dict[str, Any]] = {}
    point_wins = 0
    descriptive_interval_wins = 0
    proven_wins = 0
    unavailable = 0
    for bench in benches:
        rows[bench] = {}
        for competitor in competitors:
            paired = list(zip(samples[target][bench], samples[competitor][bench]))
            sign_test = exact_one_sided_sign_test(
                samples[target][bench], samples[competitor][bench]
            )
            nonpositive = sum(num <= 0 or den <= 0 for num, den in paired)
            if nonpositive or sign_test is None:
                unavailable += 1
                rows[bench][competitor] = {
                    "paired_ratio": None,
                    "paired_ratio_ci95": None,
                    "paired_ratio_ci95_method": DESCRIPTIVE_BOOTSTRAP_METHOD,
                    "median_faster": False,
                    "descriptive_interval_below_one": False,
                    "statistically_faster": False,
                    "nonpositive_pairs": nonpositive,
                    "exact_sign_test": (
                        None
                        if sign_test is None
                        else {
                            **sign_test,
                            "bonferroni_alpha": adjusted_alpha,
                            "rejects": False,
                        }
                    ),
                }
                continue
            ratios = paired_ratios(
                samples[target][bench], samples[competitor][bench]
            )
            ratio = statistics.median(ratios)
            ci_low, ci_high = bootstrap_median_ci(
                ratios,
                seed=derived_seed(seed, bench, target, competitor),
                samples=bootstrap_samples,
            )
            median_faster = ratio < 1.0
            descriptive_interval_faster = ci_high < 1.0
            statistically_faster = (
                median_faster
                and sign_test["one_sided_pvalue"] <= adjusted_alpha
            )
            point_wins += int(median_faster)
            descriptive_interval_wins += int(descriptive_interval_faster)
            proven_wins += int(statistically_faster)
            rows[bench][competitor] = {
                "paired_ratio": ratio,
                "paired_ratio_ci95": [ci_low, ci_high],
                "paired_ratio_ci95_method": DESCRIPTIVE_BOOTSTRAP_METHOD,
                "median_faster": median_faster,
                "descriptive_interval_below_one": descriptive_interval_faster,
                "statistically_faster": statistically_faster,
                "nonpositive_pairs": 0,
                "exact_sign_test": {
                    **sign_test,
                    "bonferroni_alpha": adjusted_alpha,
                    "rejects": statistically_faster,
                },
            }

    return {
        "target": target,
        "competitors": competitors,
        "bench_count": len(benches),
        "comparison_count": total,
        "point_estimate_wins": point_wins,
        "descriptive_bootstrap_95pct_interval_wins": descriptive_interval_wins,
        "statistically_proven_wins": proven_wins,
        "unavailable_comparisons": unavailable,
        "median_faster_on_every_row": total > 0 and point_wins == total,
        "statistically_faster_on_every_row": total > 0 and proven_wins == total,
        "familywise_alpha": 0.05,
        "multiple_comparison_method": (
            "Bonferroni-adjusted exact one-sided paired sign test"
        ),
        "null_hypothesis": "strict paired win probability <= 0.5",
        "per_comparison_alpha": adjusted_alpha,
        "bootstrap_intervals": DESCRIPTIVE_BOOTSTRAP_METHOD,
        "rows": rows,
    }


def subset_geomean(
    samples: dict[str, dict[str, list[float]]],
    benches: list[str],
    baseline: str,
    compare_name: str,
    *,
    seed: int,
    bootstrap_samples: int,
) -> dict[str, Any] | None:
    """Paired geomean + cluster-bootstrap CI over an explicit subset of rows.

    The artifact carries one of these per ROW SET rather than one number for
    whatever happened to be measured. A default `bench.py` run globs all 13
    programs and used to print a single geomean that is not comparable to the
    retained ten -- the three diagnostics are 3.5-5.5x rows and inflate it by
    roughly 0.43x. Nothing about the split should depend on a person remembering
    to pass --benches.
    """
    rows = [b for b in benches if b in samples[compare_name]]
    if not rows:
        return None
    ratios_by_bench = []
    for bench in rows:
        pairs = list(zip(samples[compare_name][bench], samples[baseline][bench]))
        if not pairs or any(num <= 0 or den <= 0 for num, den in pairs):
            return None
        ratios_by_bench.append(
            paired_ratios(samples[compare_name][bench], samples[baseline][bench])
        )
    point = geometric_mean(
        statistics.median(ratios) for ratios in ratios_by_bench
    )
    try:
        ci_low, ci_high = bootstrap_geomean_of_medians_ci(
            ratios_by_bench, seed=seed, samples=bootstrap_samples
        )
    except ValueError:
        return {
            "benches": rows,
            "geomean_paired_ratio": point,
            "ci95": None,
            "ci95_method": DESCRIPTIVE_BOOTSTRAP_METHOD,
        }
    return {
        "benches": rows,
        "geomean_paired_ratio": point,
        "ci95": [ci_low, ci_high],
        "ci95_method": DESCRIPTIVE_BOOTSTRAP_METHOD,
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
        engines_meta = [
            {"name": name, "build_identity": None} for name in engine_names
        ]
    else:
        engine_names = [
            engine.get("name") if isinstance(engine, dict) else engine
            for engine in raw_engines
        ]
        engines_meta = [
            dict(engine) if isinstance(engine, dict) else {"name": engine}
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

    # Schema v2 requires every observation tuple above, but unhealthy tuples are
    # deliberately excluded from the numeric tables.  Schema v1 has no tuple
    # records at all.  In both cases make the final table cardinality explicit:
    # a shortened column must never be mistaken for a complete paired result.
    health_failures.extend(
        sample_completeness_failures(
            cold,
            startup,
            adjusted,
            engine_names,
            benches,
            reps,
        )
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
    stored_publishable = data.get("publishable", False)
    if not isinstance(stored_publishable, bool):
        raise ValueError("benchmark result publishable must be boolean")
    publication_fields: dict[str, list[str]] = {}
    for field in (
        "provenance_reasons",
        "publication_reasons",
        "engine_drift",
        "source_drift",
    ):
        values = data.get(field, [])
        if (
            not isinstance(values, list)
            or any(not isinstance(value, str) for value in values)
        ):
            raise ValueError(f"benchmark result {field} must be an array of strings")
        publication_fields[field] = values
    replay_publishable = (
        stored_publishable
        and stored_all_correct
        and not failures
        and not health_failures
        and not correctness_failures
        and not any(publication_fields.values())
    )
    publication_metadata_complete = schema == SCHEMA_VERSION and all(
        field in data
        for field in (
            "publishable",
            "workspace_source",
            "workspace_source_before",
            "workspace_source_after",
            "provenance_reasons",
            "publication_reasons",
            "engine_drift",
            "source_drift",
            "engine_source_before",
            "engine_source_after",
            "engine_binary_sha_before",
            "engine_binary_sha_after",
            "publication_sources_head_before",
            "publication_sources_head_after",
            "repository_head_before",
            "repository_head_after",
            "harness_sha256_before",
            "harness_sha256_after",
            "bench_input_sha256_before",
            "bench_input_sha256_after",
            "benchmark_environment_policy",
            "benchmark_input_staging_policy",
        )
    ) and all(
        isinstance(engine, dict) for engine in raw_engines
    ) and data.get("benchmark_input_staging_policy") == BENCHMARK_INPUT_STAGING_POLICY
    return {
        "schema_version": schema,
        "reps": reps,
        "benches": benches,
        "engine_names": engine_names,
        "engines_meta": engines_meta,
        "baseline": baseline,
        "seed": stored_seed,
        "bootstrap_samples": stored_bootstrap,
        "headline_metric": stored_metric,
        "publishable": replay_publishable,
        "publication_metadata_complete": publication_metadata_complete,
        "workspace_source": data.get("workspace_source"),
        "workspace_source_before": data.get("workspace_source_before"),
        "workspace_source_after": data.get("workspace_source_after"),
        "engine_source_before": data.get("engine_source_before"),
        "engine_source_after": data.get("engine_source_after"),
        "engine_binary_sha_before": data.get("engine_binary_sha_before"),
        "engine_binary_sha_after": data.get("engine_binary_sha_after"),
        "publication_sources_head_before": data.get(
            "publication_sources_head_before"
        ),
        "publication_sources_head_after": data.get(
            "publication_sources_head_after"
        ),
        "repository_head_before": data.get("repository_head_before"),
        "repository_head_after": data.get("repository_head_after"),
        "harness_sha256_before": data.get("harness_sha256_before"),
        "harness_sha256_after": data.get("harness_sha256_after"),
        "bench_input_sha256_before": data.get("bench_input_sha256_before"),
        "bench_input_sha256_after": data.get("bench_input_sha256_after"),
        "benchmark_environment_policy": data.get("benchmark_environment_policy"),
        "benchmark_input_staging_policy": data.get("benchmark_input_staging_policy"),
        **publication_fields,
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
    """Publish a fully written result atomically, never overwriting by default."""
    path.parent.mkdir(parents=True, exist_ok=True)

    def dump(handle: Any) -> None:
        json.dump(data, handle, indent=2)
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())

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
        if overwrite:
            os.replace(temporary, path)
        else:
            # Linking a fully fsynced temporary file publishes it under the
            # final name without the overwrite race of exists()+replace().
            # It also leaves no partial final artifact if serialization fails.
            os.link(temporary, path)
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

    all_engine_criterion = (
        None
        if ab
        else all_competitor_comparison_summary(
            sources[metric],
            benches,
            engine_names,
            compare_name,
            seed=derived_seed(seed, "all-competitors", metric),
            bootstrap_samples=bootstrap_samples,
        )
    )

    width = max(len(bench) for bench in benches) + 2
    header = f"{'bench':<{width}}" + "".join(
        f"{name:>12}" for name in engine_names
    )
    header += f"{'paired':>11}{'boot 95% CI':>19}"
    print(
        f"metric={metric}; times are medians; ratios use paired observations; "
        "95% percentile-bootstrap intervals are descriptive"
    )
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
            "target_vs_competitors": (
                all_engine_criterion["rows"][bench]
                if all_engine_criterion is not None
                else None
            ),
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
            f"({(headline - 1) * 100:+.2f}%), descriptive bootstrap 95% CI "
            f"[{(headline_ci_low - 1) * 100:+.2f}%, "
            f"{(headline_ci_high - 1) * 100:+.2f}%]"
        )
    else:
        print(
            f"geomean paired ratio: {headline:.2f}x {compare_name}/{baseline}, "
            "descriptive bootstrap 95% CI "
            f"[{headline_ci_low:.2f}x, {headline_ci_high:.2f}x]"
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

    if all_engine_criterion is not None:
        proven = all_engine_criterion["statistically_faster_on_every_row"]
        point_wins = all_engine_criterion["point_estimate_wins"]
        proven_wins = all_engine_criterion["statistically_proven_wins"]
        total = all_engine_criterion["comparison_count"]
        status = "PROVEN" if proven else "NOT PROVEN"
        print(
            f"FASTER_THAN_EVERY_ENGINE_ON_EVERY_ROW={int(proven)} ({status}; "
            f"median wins {point_wins}/{total}, exact-sign-test wins "
            f"{proven_wins}/{total}, Bonferroni family alpha=0.05)"
        )
        for bench in benches:
            for competitor in all_engine_criterion["competitors"]:
                detail = all_engine_criterion["rows"][bench][competitor]
                if detail["statistically_faster"]:
                    continue
                ratio = detail["paired_ratio"]
                ci = detail["paired_ratio_ci95"]
                sign_test = detail["exact_sign_test"]
                if ratio is None or ci is None or sign_test is None:
                    rendered = "unavailable"
                else:
                    rendered = (
                        f"{ratio:.3f}x descriptive-bootstrap "
                        f"[{ci[0]:.3f}, {ci[1]:.3f}]; strict wins "
                        f"{sign_test['strict_wins']}/{sign_test['trials']}; "
                        f"exact p={sign_test['one_sided_pvalue']:.6g}, "
                        f"threshold={sign_test['bonferroni_alpha']:.6g}"
                    )
                print(
                    f"  unproven: {bench} {compare_name}/{competitor} {rendered}"
                )

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
            f"{compare_name}/{baseline} ({metric} wall time; descriptive "
            "bootstrap 95% CI "
            f"{headline_ci_low:.2f}×–{headline_ci_high:.2f}×)** on "
            f"{len(readme_rows)} benchmarks, {reps} paired observations with "
            "recorded order:"
        )
        print()
        print(
            f"| bench | {baseline} | {compare_name} | "
            "paired ratio (descriptive bootstrap 95% CI) |"
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
        "geomean_paired_ratio_ci95_method": DESCRIPTIVE_BOOTSTRAP_METHOD,
        "metric_geomean_paired_ratio": metric_headlines,
        "startup_median_ms": startup_ms,
        "all_engine_criterion": all_engine_criterion,
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
    parser.add_argument(
        "--allow-external-bench-dir",
        action="store_true",
        help="run trusted benchmark sources outside this workspace",
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
    parser.add_argument(
        "--allow-dirty-engine",
        action="store_true",
        help=(
            "measure a binary built from a dirty tree. The artifact is marked "
            "publishable:false -- a dirty build's reported commit is the parent "
            "it was based on, so the number cannot be attributed to any commit"
        ),
    )
    parser.add_argument(
        "--allow-nonhead-engine",
        action="store_true",
        help=(
            "measure a binary built from a commit other than the workspace HEAD. "
            "The artifact is marked publishable:false"
        ),
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
        replay_policy_reasons = publication_policy_reasons(
            is_ab=compare_name == "new",
            canonical_inputs=(
                args.benches is None and benches == discover_benches(BENCH_DIR)
            ),
            engine_names=engine_names,
            baseline=baseline,
            metric=metric,
            historical=args.historical,
            reps=replay["reps"],
            bootstrap_samples=bootstrap_samples,
            source_reason=None,
            environment=relevant_environment() if args.readme else {},
        )
        replay_current_reasons = (
            replay_current_source_reasons(replay, benches) if args.readme else []
        )
        replay_publication_reasons = list(
            dict.fromkeys(
                [
                    *replay["provenance_reasons"],
                    *replay["publication_reasons"],
                    *replay["engine_drift"],
                    *replay["source_drift"],
                    *pgo_build_reasons(replay["engines_meta"], require_pgo=True),
                    *replay_policy_reasons,
                    *replay_current_reasons,
                ]
            )
        )
        if not replay["publishable"]:
            replay_publication_reasons.insert(
                0, "stored artifact is marked publishable:false"
            )
        if not replay["publication_metadata_complete"]:
            replay_publication_reasons.insert(
                0, "stored artifact lacks the current publication provenance envelope"
            )
        if args.readme and replay_publication_reasons:
            rendered = "\n  ".join(dict.fromkeys(replay_publication_reasons))
            raise SystemExit(
                "refusing --readme for an unpublishable benchmark artifact:\n  "
                + rendered
            )
        if replay["schema_version"] == 1 and not args.historical:
            raise SystemExit(
                "schema-v1 replay requires --historical; paired modern "
                "statistics require schema-v2 observations"
            )
        replay_summary = None
        replay_stats_ready = replay["all_correct"] and not replay["failures"]
        if not replay_stats_ready:
            print(
                "statistics unavailable: failed, incorrect, or incomplete "
                "observations cannot support paired statistics"
            )
            print("ALL_CORRECT=0  (result failed integrity checks)")
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

    # Every child gets a separate allowlisted home/cache/temp tree. Creating
    # and removing that tree is outside the measured interval.
    benchmark_environment_policy = canonical_benchmark_environment_descriptor()

    bench_dir = Path(args.bench_dir).resolve()
    if not bench_dir.is_dir():
        raise SystemExit(f"benchmark directory does not exist: {bench_dir}")
    try:
        bench_dir.relative_to(ROOT)
        external_bench_dir = False
    except ValueError:
        external_bench_dir = True
    if external_bench_dir and not getattr(args, "allow_external_bench_dir", False):
        raise SystemExit(
            "external --bench-dir sources run with engine host permissions; "
            "pass --allow-external-bench-dir only for reviewed code"
        )
    available_benches = discover_benches(bench_dir)
    benches = args.benches.split(",") if args.benches else available_benches
    if not benches or any(not bench for bench in benches):
        raise SystemExit("benchmark subset must contain nonempty names")
    if len(set(benches)) != len(benches):
        raise SystemExit("benchmark subset contains duplicate names")
    unknown = sorted(set(benches) - set(available_benches))
    if unknown:
        raise SystemExit(f"unknown benchmark(s): {', '.join(unknown)}")

    stage_files = {
        "tools/bench.py": Path(__file__).resolve(),
        "tools/pgo_training.py": _STAGE_HELPER_PATH.resolve(),
        "bench/run_real.sh": ROOT / "bench" / "run_real.sh",
        **{
            f"inputs/{bench}.js": bench_dir / f"{bench}.js"
            for bench in benches
        },
    }
    try:
        input_stage = ImmutableInputStage(
            stage_files, prefix="zipp-real-inputs-"
        )
    except (OSError, ValueError) as exc:
        raise SystemExit(f"cannot stage immutable benchmark inputs: {exc}") from exc
    staged_bench_dir = input_stage.path("inputs")
    harness_hashes_before = {
        "bench_py_sha256": file_digest(input_stage.path("tools/bench.py")),
        "run_real_sh_sha256": file_digest(
            input_stage.path("bench/run_real.sh")
        ),
        "pgo_training_py_sha256": file_digest(
            input_stage.path("tools/pgo_training.py")
        ),
    }
    input_hashes_before = {
        bench: file_digest(input_stage.path(f"inputs/{bench}.js"))
        for bench in benches
    }

    if args.ab:
        old_executable = resolved_executable([args.ab[0]])
        new_executable = resolved_executable([args.ab[1]])
        engines = [
            ("old", [str(old_executable) if old_executable else args.ab[0], "js"]),
            ("new", [str(new_executable) if new_executable else args.ab[1], "js"]),
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
            ("deno", ["deno", "run"]),
        ):
            if name not in requested_engines:
                continue
            executable = shutil.which(cmd[0])
            if executable:
                try:
                    canonical_cmd = canonical_engine_command(
                        name,
                        [executable] + cmd[1:],
                        args.timeout,
                        fresh_environment=True,
                    )
                except ValueError as exc:
                    raise SystemExit(str(exc)) from exc
                engines.append((name, canonical_cmd))
        if "zipp" in requested_engines:
            zipp_executable = resolved_executable([args.zipp])
            if zipp_executable is None:
                raise SystemExit(f"zipp executable does not exist: {args.zipp}")
            engines.append(("zipp", [str(zipp_executable), "js"]))
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

    canonical_inputs = (
        bench_dir == BENCH_DIR.resolve()
        and args.benches is None
        and benches == discover_benches(BENCH_DIR)
    )
    publication_paths = {
        Path(__file__).resolve(),
        _STAGE_HELPER_PATH.resolve(),
        ROOT / "bench" / "run_real.sh",
        *(bench_dir / f"{bench}.js" for bench in benches),
    }
    publication_sources_head_before, publication_source_reason = (
        git_paths_match_head(publication_paths)
    )
    repository_head_before, repository_head_reason = git_repository_matches_head()
    environment = relevant_environment()
    publication_reasons = publication_policy_reasons(
        is_ab=bool(args.ab),
        canonical_inputs=canonical_inputs,
        engine_names=engine_names,
        baseline=baseline,
        metric=args.metric,
        historical=args.historical,
        reps=args.reps,
        bootstrap_samples=args.bootstrap_samples,
        source_reason=publication_source_reason,
        environment=environment,
    )
    if (
        repository_head_reason is not None
        and repository_head_reason not in publication_reasons
    ):
        publication_reasons.append(repository_head_reason)
    if publication_reasons:
        print("publication policy (this run is diagnostic-only):", file=sys.stderr)
        for reason in publication_reasons:
            print(f"  {reason}", file=sys.stderr)

    # Provenance BEFORE the first measurement. The harness used to collect this
    # only at the end, inside `if json_path:` -- so a run without --json recorded
    # nothing, and a run with it could not tell whether the binary it had just
    # spent twenty minutes measuring was the one it was about to name.
    workspace_commit = git_revision()
    engines_meta_before = [
        {
            **engine_metadata(
                name,
                cmd,
                args.timeout,
                fresh_environment=True,
            ),
            "environment": engine_env[name],
        }
        for name, cmd in engines
    ]
    ab_sides_distinguished = bool(args.allow_aa) or (
        args.ab_env is not None and args.ab_env[0] != args.ab_env[1]
    )
    provenance_reasons, uncovered_provenance_reasons = provenance_assessment(
        engines_meta_before,
        workspace_commit,
        is_ab=bool(args.ab),
        allow_dirty=getattr(args, "allow_dirty_engine", False),
        allow_nonhead=getattr(args, "allow_nonhead_engine", False),
        ab_sides_distinguished=ab_sides_distinguished,
    )
    if provenance_reasons:
        print("engine provenance:", file=sys.stderr)
        for reason in provenance_reasons:
            print(f"  {reason}", file=sys.stderr)
        if provenance_is_fatal(
            uncovered_provenance_reasons,
            is_ab=bool(args.ab),
        ):
            raise SystemExit(
                "refusing to measure: the engine cannot be attributed to a "
                "commit (see above).\n"
                "Rebuild from a clean checkout of the tree you mean to measure, "
                "or pass --allow-dirty-engine / --allow-nonhead-engine to record "
                "an explicitly UNPUBLISHABLE artifact."
            )
        print(
            "  recorded; this artifact is publishable:false",
            file=sys.stderr,
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
                bench_path = staged_bench_dir / f"{bench}.js"
                for engine_position, (name, cmd) in enumerate(ordered_engines):
                    launch = run_once(
                        cmd,
                        empty,
                        timeout=args.timeout,
                        env=engine_env[name],
                        fresh_environment=True,
                    )
                    full = run_once(
                        cmd,
                        bench_path,
                        timeout=args.timeout,
                        env=engine_env[name],
                        fresh_environment=True,
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

    # Re-probe. A rebuild that lands mid-run replaces the binary under
    # measurement without changing anything the harness would otherwise notice.
    engines_meta_after = [
        {
            **engine_metadata(
                name,
                cmd,
                args.timeout,
                fresh_environment=True,
            ),
            "environment": engine_env[name],
        }
        for name, cmd in engines
    ]
    drift_reasons = engine_drift(engines_meta_before, engines_meta_after)
    for reason in drift_reasons:
        health_failures.append(f"engine changed during the run: {reason}")

    harness_hashes_after = harness_digest()
    input_hashes_after = bench_input_digests(bench_dir, benches)
    staged_harness_hashes_after = {
        "bench_py_sha256": file_digest(input_stage.path("tools/bench.py")),
        "run_real_sh_sha256": file_digest(
            input_stage.path("bench/run_real.sh")
        ),
        "pgo_training_py_sha256": file_digest(
            input_stage.path("tools/pgo_training.py")
        ),
    }
    staged_input_hashes_after = {
        bench: file_digest(input_stage.path(f"inputs/{bench}.js"))
        for bench in benches
    }
    workspace_commit_after = git_revision()
    source_drift = [
        *digest_mapping_drift(
            harness_hashes_before,
            harness_hashes_after,
            kind="harness",
        ),
        *digest_mapping_drift(
            input_hashes_before,
            input_hashes_after,
            kind="benchmark input",
        ),
        *digest_mapping_drift(
            harness_hashes_before,
            staged_harness_hashes_after,
            kind="staged harness",
        ),
        *digest_mapping_drift(
            input_hashes_before,
            staged_input_hashes_after,
            kind="staged benchmark input",
        ),
    ]
    if workspace_commit_after != workspace_commit:
        source_drift.append(
            "workspace HEAD changed during run "
            f"({workspace_commit} -> {workspace_commit_after})"
        )
    health_failures.extend(source_drift)

    publication_sources_head_after, publication_source_reason_after = (
        git_paths_match_head(publication_paths)
    )
    repository_head_after, repository_head_reason_after = (
        git_repository_matches_head()
    )
    if (
        publication_source_reason_after is not None
        and publication_source_reason_after not in publication_reasons
    ):
        publication_reasons.append(publication_source_reason_after)
        print(
            "publication policy changed during the run: "
            f"{publication_source_reason_after}",
            file=sys.stderr,
        )
    if (
        repository_head_reason_after is not None
        and repository_head_reason_after not in publication_reasons
    ):
        publication_reasons.append(repository_head_reason_after)
        print(
            "publication policy changed during the run: "
            f"{repository_head_reason_after}",
            file=sys.stderr,
        )

    health_failures.extend(
        sample_completeness_failures(
            cold,
            startup,
            adjusted,
            engine_names,
            benches,
            args.reps,
        )
    )
    for bench in benches:
        reference = outputs[baseline].get(bench)
        for name in engine_names:
            if name != baseline and outputs[name].get(bench) != reference:
                correctness_failures.append(
                    f"{name} output differs from {baseline} on {bench}"
                )

    health_failures = list(dict.fromkeys(health_failures))
    correctness_failures = list(dict.fromkeys(correctness_failures))
    failures.extend(health_failures)
    failures.extend(correctness_failures)
    all_correct = not health_failures and not correctness_failures

    readme_allowed = artifact_publishable(
        provenance_reasons,
        drift_reasons,
        source_drift,
        all_correct=all_correct,
        publication_reasons=publication_reasons,
    )
    readme_refused = args.readme and not readme_allowed
    if readme_refused:
        print(
            "README-ready output suppressed: this run is not publishable",
            file=sys.stderr,
        )

    summary: dict[str, Any] | None = None
    if not all_correct:
        print(
            "statistics unavailable: failed, incorrect, or incomplete "
            "observations cannot support paired statistics"
        )
        print("ALL_CORRECT=0  (measurement failed integrity checks)")
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
            summary = None
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
                readme=args.readme and readme_allowed,
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

    report_integrity = all_correct and summary is not None and not unique_failures
    row_sets = classify_benches(benches)
    row_set_summaries: dict[str, Any] = {}
    if report_integrity:
        metric_source = {"cold": cold, "startup": startup, "adjusted": adjusted}[
            args.metric if args.metric in ("cold", "adjusted") else "cold"
        ]
        row_set_summaries = {
            set_name: subset_geomean(
                metric_source,
                set_benches,
                baseline,
                compare_name,
                seed=derived_seed(args.seed, set_name),
                bootstrap_samples=args.bootstrap_samples,
            )
            for set_name, set_benches in (
                ("headline", row_sets["headline_benches"]),
                ("diagnostic", row_sets["diagnostic_benches"]),
                ("all_measured", benches),
            )
            if set_benches
        }
        for set_name, detail in row_set_summaries.items():
            if detail is None:
                continue
            ci = detail["ci95"]
            span = (
                f" [{ci[0]:.3f}, {ci[1]:.3f}]" if ci else ""
            )
            print(
                f"geomean[{set_name}] {detail['geomean_paired_ratio']:.4f}x"
                f"{span}  ({len(detail['benches'])} rows)"
            )

    if json_path:
        # The artifact's own account of what it measured. `workspace_source` is
        # the TREE; `engine_source_before/after` is what the BINARY says it was
        # built from. These are different questions and the harness used to
        # answer only the first, which is how bench/head_clean_2a616f5.json came
        # to be named for a commit its engine had never seen.
        publishable = artifact_publishable(
            provenance_reasons,
            drift_reasons,
            source_drift,
            all_correct=report_integrity,
            publication_reasons=publication_reasons,
        )
        metadata = {
            "schema_version": SCHEMA_VERSION,
            "created_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
            "git_commit": workspace_commit,
            "workspace_source": workspace_commit,
            "workspace_source_before": workspace_commit,
            "workspace_source_after": workspace_commit_after,
            "engine_source_before": {
                m["name"]: source_identity(m.get("build_identity"))
                for m in engines_meta_before
            },
            "engine_source_after": {
                m["name"]: source_identity(m.get("build_identity"))
                for m in engines_meta_after
            },
            "engine_binary_sha_before": {
                m["name"]: m.get("sha256") for m in engines_meta_before
            },
            "engine_binary_sha_after": {
                m["name"]: m.get("sha256") for m in engines_meta_after
            },
            "publishable": publishable,
            "provenance_reasons": provenance_reasons,
            "publication_reasons": publication_reasons,
            "publication_sources_head_before": publication_sources_head_before,
            "publication_sources_head_after": publication_sources_head_after,
            "repository_head_before": repository_head_before,
            "repository_head_after": repository_head_after,
            "engine_drift": drift_reasons,
            "source_drift": source_drift,
            "harness": harness_hashes_after,
            "harness_sha256_before": harness_hashes_before,
            "harness_sha256_after": harness_hashes_after,
            "bench_input_sha256": input_hashes_after,
            "bench_input_sha256_before": input_hashes_before,
            "bench_input_sha256_after": input_hashes_after,
            "benchmark_input_staging_policy": BENCHMARK_INPUT_STAGING_POLICY,
            **row_sets,
            "row_set_summaries": row_set_summaries,
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
            "engines": engines_meta_after,
            "host": {
                "platform": platform.platform(),
                "machine": platform.machine(),
                "processor": platform.processor(),
                "python": platform.python_version(),
                "power_mode": power_mode(),
            },
            "environment": environment,
            "benchmark_environment_policy": benchmark_environment_policy,
            "schedule": schedules,
            "observations": observations,
            "summary": summary,
            "all_correct": all_correct,
            "health_failures": health_failures,
            "correctness_failures": correctness_failures,
            "failures": unique_failures,
        }
        write_json_result(
            json_path,
            metadata,
            overwrite=args.overwrite_json,
        )
        print(f"raw observations -> {json_path}")

    return 0 if all_correct and not unique_failures and not readme_refused else 1


if __name__ == "__main__":
    sys.exit(main())
