#!/usr/bin/env python3
"""Counterbalanced Node-vs-Zipp harness for the hostile benchmark corpus.

The retained ``bench/real`` series is intentionally frozen.  This harness is
separate because the hostile corpus has different requirements: nested entry
paths, scripts and modules, explicit baseline/stressor families, and complete
declared-input hashing.  Workloads that model a warm server remain ordinary long-lived
programs for now; every observation is still one fresh engine process.

The manifest lives at ``bench/hostile/manifest.json`` by default::

    {
      "schema_version": 1,
      "cases": [
        {
          "id": "scope-original",
          "entry": "scope/original.js",
          "category": "scope",
          "goal": "script",
          "family": "scope-shape",
          "variant": "baseline",
          "features": ["functions", "top-level-var"],
          "description": "Control for scope-sensitive compilation"
        },
        {
          "id": "scope-iife",
          "entry": "scope/iife.js",
          "category": "scope",
          "goal": "script",
          "family": "scope-shape",
          "variant": "iife",
          "features": ["functions", "iife"]
        }
      ]
    }

``inputs`` defaults to the entry.  Module graphs and vendored libraries must
explicitly list every source/fixture they consume so an artifact identifies the
complete program rather than only its entry point; the trusted runner does not
attempt to discover arbitrary static or dynamic JavaScript imports.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import importlib.util
import json
import math
import os
import platform
import random
import statistics
import sys
import tempfile
from dataclasses import asdict, dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Callable, Iterable


CORE_PATH = Path(__file__).with_name("bench.py")
CORE_SPEC = importlib.util.spec_from_file_location("zipp_bench_core", CORE_PATH)
if CORE_SPEC is None or CORE_SPEC.loader is None:  # pragma: no cover - import invariant
    raise RuntimeError(f"cannot import benchmark helpers from {CORE_PATH}")
core = importlib.util.module_from_spec(CORE_SPEC)
CORE_SPEC.loader.exec_module(core)


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_MANIFEST = ROOT / "bench" / "hostile" / "manifest.json"
MANIFEST_SCHEMA_VERSION = 1
ARTIFACT_SCHEMA_VERSION = 1
DEFAULT_TIMEOUT_S = 300.0
DEFAULT_REPS = 15

ROOT_KEYS = frozenset({"schema_version", "cases", "description"})
CASE_KEYS = frozenset(
    {
        "id",
        "entry",
        "category",
        "goal",
        "family",
        "variant",
        "inputs",
        "timeout_s",
        "features",
        "description",
    }
)


class ManifestError(ValueError):
    """The hostile benchmark manifest is malformed or unsafe."""


@dataclass(frozen=True)
class Case:
    id: str
    entry: Path
    entry_rel: str
    category: str
    goal: str
    family: str | None
    variant: str | None
    inputs: tuple[Path, ...]
    input_rels: tuple[str, ...]
    timeout_s: float
    features: tuple[str, ...]
    description: str | None


@dataclass(frozen=True)
class Manifest:
    path: Path
    root: Path
    cases: tuple[Case, ...]
    description: str | None


def _strict_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ManifestError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def _invalid_json_constant(value: str) -> Any:
    raise ManifestError(f"invalid JSON numeric constant {value!r}")


def _expect_object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ManifestError(f"{label} must be an object")
    return value


def _check_keys(value: dict[str, Any], allowed: frozenset[str], label: str) -> None:
    unknown = sorted(set(value) - allowed)
    if unknown:
        raise ManifestError(f"{label} has unknown field(s): {', '.join(unknown)}")


def _nonempty_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ManifestError(f"{label} must be a nonempty string")
    if value != value.strip():
        raise ManifestError(f"{label} must not have leading or trailing whitespace")
    if any(ord(ch) < 0x20 for ch in value):
        raise ManifestError(f"{label} must not contain control characters")
    return value


def _filter_token(value: Any, label: str) -> str:
    result = _nonempty_string(value, label)
    if "," in result:
        raise ManifestError(f"{label} must not contain ','")
    return result


def _optional_string(value: Any, label: str) -> str | None:
    if value is None:
        return None
    return _nonempty_string(value, label)


def _portable_relative_path(value: Any, label: str) -> tuple[str, PurePosixPath]:
    raw = _nonempty_string(value, label)
    if "\\" in raw:
        raise ManifestError(f"{label} must use portable '/' separators")
    pure = PurePosixPath(raw)
    if (
        pure.is_absolute()
        or raw.startswith("//")
        or (len(raw) >= 2 and raw[0].isalpha() and raw[1] == ":")
        or any(part in ("", ".", "..") for part in pure.parts)
        or raw != pure.as_posix()
    ):
        raise ManifestError(f"{label} must be a normalized relative path")
    return pure.as_posix(), pure


def _resolve_input(root: Path, value: Any, label: str) -> tuple[str, Path]:
    rel, pure = _portable_relative_path(value, label)
    try:
        resolved = root.joinpath(*pure.parts).resolve(strict=True)
    except OSError as exc:
        raise ManifestError(f"{label} does not name a readable file: {rel}") from exc
    try:
        resolved.relative_to(root)
    except ValueError as exc:
        raise ManifestError(f"{label} escapes the manifest directory: {rel}") from exc
    if not resolved.is_file():
        raise ManifestError(f"{label} is not a file: {rel}")
    return rel, resolved


def _string_list(value: Any, label: str) -> tuple[str, ...]:
    if not isinstance(value, list) or not value:
        raise ManifestError(f"{label} must be a nonempty array")
    items = tuple(_nonempty_string(item, f"{label} item") for item in value)
    if len(set(items)) != len(items):
        raise ManifestError(f"{label} must not contain duplicates")
    return items


def load_manifest(path: Path | str = DEFAULT_MANIFEST) -> Manifest:
    """Load and fully validate a hostile-corpus manifest.

    Every manifest-declared path is confined to the manifest directory after
    canonicalization, so a checked-in symlink cannot silently redirect a
    declared entry or input to an external file. Module dependencies remain a
    trusted, manually enumerated input set.
    """

    manifest_path = Path(path).resolve()
    try:
        with manifest_path.open(encoding="utf-8") as handle:
            raw = json.load(
                handle,
                object_pairs_hook=_strict_object,
                parse_constant=_invalid_json_constant,
            )
    except ManifestError:
        raise
    except (OSError, json.JSONDecodeError) as exc:
        raise ManifestError(f"cannot read manifest {manifest_path}: {exc}") from exc

    root_obj = _expect_object(raw, "manifest root")
    _check_keys(root_obj, ROOT_KEYS, "manifest root")
    schema = root_obj.get("schema_version")
    if (
        isinstance(schema, bool)
        or not isinstance(schema, int)
        or schema != MANIFEST_SCHEMA_VERSION
    ):
        raise ManifestError(
            f"manifest schema_version must be {MANIFEST_SCHEMA_VERSION}"
        )
    description = _optional_string(root_obj.get("description"), "manifest description")
    raw_cases = root_obj.get("cases")
    if not isinstance(raw_cases, list) or not raw_cases:
        raise ManifestError("manifest cases must be a nonempty array")

    manifest_root = manifest_path.parent.resolve(strict=True)
    cases: list[Case] = []
    ids: set[str] = set()
    family_variants: dict[str, set[str]] = {}

    for index, raw_case in enumerate(raw_cases):
        label = f"case[{index}]"
        item = _expect_object(raw_case, label)
        _check_keys(item, CASE_KEYS, label)
        for required in ("id", "entry", "category"):
            if required not in item:
                raise ManifestError(f"{label} is missing required field {required!r}")

        case_id = _filter_token(item["id"], f"{label}.id")
        if case_id in ids:
            raise ManifestError(f"duplicate case id {case_id!r}")
        ids.add(case_id)

        category = _filter_token(item["category"], f"{label}.category")
        entry_rel, entry = _resolve_input(manifest_root, item["entry"], f"{label}.entry")
        inferred_goal = "module" if entry.suffix == ".mjs" else "script"
        goal = item.get("goal", inferred_goal)
        if goal not in ("script", "module"):
            raise ManifestError(f"{label}.goal must be 'script' or 'module'")
        required_suffix = ".mjs" if goal == "module" else ".js"
        if entry.suffix != required_suffix:
            raise ManifestError(
                f"{label}.entry must end in {required_suffix!r} for goal {goal!r}"
            )

        family = _optional_string(item.get("family"), f"{label}.family")
        variant = _optional_string(item.get("variant"), f"{label}.variant")
        if family is not None:
            family = _filter_token(family, f"{label}.family")
        if (family is None) != (variant is None):
            raise ManifestError(f"{label}.family and .variant must be provided together")
        if family is not None and variant is not None:
            variants = family_variants.setdefault(family, set())
            if variant in variants:
                raise ManifestError(
                    f"family {family!r} has duplicate variant {variant!r}"
                )
            variants.add(variant)

        raw_inputs = item.get("inputs", [entry_rel])
        if not isinstance(raw_inputs, list) or not raw_inputs:
            raise ManifestError(f"{label}.inputs must be a nonempty array")
        input_rels: list[str] = []
        inputs: list[Path] = []
        resolved_seen: set[Path] = set()
        for input_index, raw_input in enumerate(raw_inputs):
            rel, resolved = _resolve_input(
                manifest_root,
                raw_input,
                f"{label}.inputs[{input_index}]",
            )
            if resolved in resolved_seen:
                raise ManifestError(f"{label}.inputs contains duplicate file {rel!r}")
            resolved_seen.add(resolved)
            input_rels.append(rel)
            inputs.append(resolved)
        if entry not in resolved_seen:
            raise ManifestError(f"{label}.inputs must include its entry {entry_rel!r}")

        timeout = item.get("timeout_s", DEFAULT_TIMEOUT_S)
        if isinstance(timeout, bool) or not isinstance(timeout, (int, float)):
            raise ManifestError(f"{label}.timeout_s must be numeric")
        timeout = float(timeout)
        if not math.isfinite(timeout) or timeout <= 0:
            raise ManifestError(f"{label}.timeout_s must be finite and positive")

        features = (
            tuple(
                _filter_token(feature, f"{label}.features item")
                for feature in _string_list(item["features"], f"{label}.features")
            )
            if "features" in item
            else ()
        )
        case_description = _optional_string(
            item.get("description"), f"{label}.description"
        )
        cases.append(
            Case(
                id=case_id,
                entry=entry,
                entry_rel=entry_rel,
                category=category,
                goal=goal,
                family=family,
                variant=variant,
                inputs=tuple(inputs),
                input_rels=tuple(input_rels),
                timeout_s=timeout,
                features=features,
                description=case_description,
            )
        )

    for family, variants in family_variants.items():
        if "baseline" not in variants:
            raise ManifestError(f"family {family!r} has no 'baseline' variant")
        if len(variants) < 2:
            raise ManifestError(f"family {family!r} has no stressor variant")

    return Manifest(
        path=manifest_path,
        root=manifest_root,
        cases=tuple(cases),
        description=description,
    )


def parse_csv(value: str | None, label: str) -> tuple[str, ...]:
    if value is None:
        return ()
    items = tuple(part.strip() for part in value.split(","))
    if not items or any(not item for item in items):
        raise ValueError(f"{label} must contain nonempty comma-separated values")
    if len(set(items)) != len(items):
        raise ValueError(f"{label} contains duplicates")
    return items


def select_cases(
    manifest: Manifest,
    *,
    case_ids: Iterable[str] = (),
    categories: Iterable[str] = (),
    families: Iterable[str] = (),
    features: Iterable[str] = (),
) -> tuple[Case, ...]:
    """Filter in manifest order; combined filter dimensions are conjunctive."""

    requested_ids = set(case_ids)
    requested_categories = set(categories)
    requested_families = set(families)
    requested_features = set(features)

    known_ids = {case.id for case in manifest.cases}
    known_categories = {case.category for case in manifest.cases}
    known_families = {case.family for case in manifest.cases if case.family is not None}
    known_features = {feature for case in manifest.cases for feature in case.features}
    for requested, known, label in (
        (requested_ids, known_ids, "case"),
        (requested_categories, known_categories, "category"),
        (requested_families, known_families, "family"),
        (requested_features, known_features, "feature"),
    ):
        unknown = sorted(requested - known)
        if unknown:
            raise ValueError(f"unknown {label}(s): {', '.join(unknown)}")

    selected = tuple(
        case
        for case in manifest.cases
        if (not requested_ids or case.id in requested_ids)
        and (not requested_categories or case.category in requested_categories)
        and (not requested_families or case.family in requested_families)
        and (not requested_features or requested_features.issubset(case.features))
    )
    if not selected:
        raise ValueError("filters selected no benchmark cases")
    return selected


def engine_prefix(engine: str, executable: str, goal: str) -> list[str]:
    if engine == "node":
        return [executable]
    if engine == "zipp":
        return [executable, "mjs" if goal == "module" else "js"]
    raise ValueError(f"unknown engine {engine!r}")


def _temporary_empty(suffix: str) -> Path:
    handle = tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        suffix=suffix,
        prefix="zipp-hostile-empty-",
        delete=False,
    )
    try:
        handle.write("\n")
        return Path(handle.name)
    finally:
        handle.close()


def _result_failure(
    result: dict[str, Any], engine: str, case: Case, rep: int, phase: str
) -> str | None:
    if result.get("timed_out", False):
        return f"{engine} {phase} timed out on {case.id}, rep {rep + 1}"
    if result.get("spawn_error", False):
        return (
            f"{engine} {phase} could not launch on {case.id}, rep {rep + 1}: "
            f"{core.decode_stderr(result.get('stderr', b''))}"
        )
    if result.get("returncode") != 0:
        return (
            f"{engine} {phase} exited {result.get('returncode')} on {case.id}, "
            f"rep {rep + 1}: {core.decode_stderr(result.get('stderr', b''))}"
        )
    return None


Runner = Callable[..., dict[str, Any]]


def run_measurements(
    cases: tuple[Case, ...],
    *,
    node: str,
    zipp: str,
    reps: int,
    seed: int,
    timeout_override: float | None = None,
    runner: Runner = core.run_once,
) -> dict[str, Any]:
    """Run cold observations and return raw samples plus correctness state."""

    engines = [("node", [node]), ("zipp", [zipp])]
    engine_names = [name for name, _ in engines]
    samples = {
        metric: {
            engine: {case.id: [] for case in cases} for engine in engine_names
        }
        for metric in ("startup", "cold", "adjusted")
    }
    outputs: dict[str, dict[str, bytes]] = {name: {} for name in engine_names}
    observations: list[dict[str, Any]] = []
    schedules: list[dict[str, Any]] = []
    health_failures: list[str] = []
    correctness_failures: list[str] = []
    empty = {"script": _temporary_empty(".js"), "module": _temporary_empty(".mjs")}

    try:
        for rep in range(reps):
            case_order = list(cases)
            random.Random(seed + rep).shuffle(case_order)
            ordered_engines = core.engine_order_for_rep(engines, rep, seed)
            schedules.append(
                {
                    "rep": rep,
                    "case_order": [case.id for case in case_order],
                    "engine_order": [name for name, _ in ordered_engines],
                }
            )
            for case_position, case in enumerate(case_order):
                timeout = timeout_override or case.timeout_s
                for engine_position, (engine, raw_cmd) in enumerate(ordered_engines):
                    prefix = engine_prefix(engine, raw_cmd[0], case.goal)
                    startup_result = runner(
                        prefix,
                        empty[case.goal],
                        timeout=timeout,
                    )
                    full_result = runner(
                        prefix,
                        case.entry,
                        timeout=timeout,
                    )
                    startup_s = float(startup_result["elapsed_s"])
                    cold_s = float(full_result["elapsed_s"])
                    adjusted_s = cold_s - startup_s
                    startup_failure = _result_failure(
                        startup_result, engine, case, rep, "startup"
                    )
                    full_failure = _result_failure(full_result, engine, case, rep, "run")
                    healthy = startup_failure is None and full_failure is None
                    if startup_failure:
                        health_failures.append(startup_failure)
                    if full_failure:
                        health_failures.append(full_failure)

                    stdout = full_result.get("stdout", b"")
                    observation: dict[str, Any] = {
                        "rep": rep,
                        "case": case.id,
                        "case_position": case_position,
                        "engine": engine,
                        "engine_position": engine_position,
                        "goal": case.goal,
                        "startup_argv": prefix + [str(empty[case.goal])],
                        "argv": prefix + [str(case.entry)],
                        "timeout_s": timeout,
                        "startup_s": startup_s,
                        "cold_s": cold_s,
                        "adjusted_s": adjusted_s,
                        "startup_returncode": startup_result.get("returncode"),
                        "returncode": full_result.get("returncode"),
                        "startup_timed_out": startup_result.get("timed_out", False),
                        "timed_out": full_result.get("timed_out", False),
                        "startup_spawn_error": startup_result.get("spawn_error", False),
                        "spawn_error": full_result.get("spawn_error", False),
                        "startup_stdout_bytes": len(startup_result.get("stdout", b"")),
                        "stdout_bytes": len(stdout),
                        "stdout_sha256": hashlib.sha256(stdout).hexdigest(),
                        "valid_for_stats": healthy,
                    }
                    if startup_result.get("stderr"):
                        observation["startup_stderr"] = core.decode_stderr(
                            startup_result["stderr"]
                        )
                    if full_result.get("stderr"):
                        observation["stderr"] = core.decode_stderr(full_result["stderr"])
                    observations.append(observation)

                    if healthy:
                        samples["startup"][engine][case.id].append(startup_s)
                        samples["cold"][engine][case.id].append(cold_s)
                        samples["adjusted"][engine][case.id].append(adjusted_s)

                    if full_failure is None:
                        if not stdout:
                            correctness_failures.append(
                                f"{engine} produced empty output on {case.id}, rep {rep + 1}"
                            )
                        previous = outputs[engine].get(case.id)
                        if previous is None:
                            outputs[engine][case.id] = stdout
                        elif previous != stdout:
                            correctness_failures.append(
                                f"{engine} output not reproducible on {case.id}, rep {rep + 1}"
                            )
            print(f"  rep {rep + 1}/{reps} done", file=sys.stderr)
    finally:
        for path in empty.values():
            try:
                path.unlink()
            except OSError:
                pass

    for case in cases:
        node_output = outputs["node"].get(case.id)
        zipp_output = outputs["zipp"].get(case.id)
        if node_output is not None and zipp_output is not None and node_output != zipp_output:
            correctness_failures.append(f"zipp output differs from node on {case.id}")

    health_failures = list(dict.fromkeys(health_failures))
    correctness_failures = list(dict.fromkeys(correctness_failures))
    return {
        "samples": samples,
        "outputs": outputs,
        "observations": observations,
        "schedules": schedules,
        "health_failures": health_failures,
        "correctness_failures": correctness_failures,
        "all_correct": not health_failures and not correctness_failures,
    }


def _ratio_detail(
    numerators: list[float],
    denominators: list[float],
    *,
    seed: int,
    bootstrap_samples: int,
) -> dict[str, Any]:
    if len(numerators) != len(denominators) or not numerators:
        return {"paired_ratio": None, "ci95": None, "nonpositive_pairs": 0}
    nonpositive = sum(
        numerator <= 0 or denominator <= 0
        for numerator, denominator in zip(numerators, denominators)
    )
    if nonpositive:
        return {
            "paired_ratio": None,
            "ci95": None,
            "nonpositive_pairs": nonpositive,
        }
    ratios = core.paired_ratios(numerators, denominators)
    low, high = core.bootstrap_median_ci(
        ratios,
        seed=seed,
        samples=bootstrap_samples,
    )
    return {
        "paired_ratio": statistics.median(ratios),
        "ci95": [low, high],
        "nonpositive_pairs": 0,
    }


def degradation_summaries(
    cases: tuple[Case, ...],
    samples: dict[str, dict[str, dict[str, list[float]]]],
    *,
    seed: int,
    bootstrap_samples: int,
) -> list[dict[str, Any]]:
    """Compare every measured stressor with its family's measured baseline.

    ``relative_parity_ratio`` is ``(zipp/node stressor) / (zipp/node baseline)``.
    Above one means the stressor hurts Zipp more than it hurts Node.
    """

    by_family: dict[str, list[Case]] = {}
    for case in cases:
        if case.family is not None:
            by_family.setdefault(case.family, []).append(case)

    result: list[dict[str, Any]] = []
    for family, members in by_family.items():
        baseline = next((case for case in members if case.variant == "baseline"), None)
        stressors = [case for case in members if case.variant != "baseline"]
        if baseline is None or not stressors:
            result.append(
                {
                    "family": family,
                    "available": False,
                    "reason": "selected cases do not include baseline and stressor",
                }
            )
            continue
        for stressor in stressors:
            metrics: dict[str, Any] = {}
            for metric in ("cold", "adjusted"):
                engine_ratios = {
                    engine: _ratio_detail(
                        samples[metric][engine][stressor.id],
                        samples[metric][engine][baseline.id],
                        seed=core.derived_seed(
                            seed, "degradation", family, stressor.id, metric, engine
                        ),
                        bootstrap_samples=bootstrap_samples,
                    )
                    for engine in ("node", "zipp")
                }
                node_base = samples[metric]["node"][baseline.id]
                zipp_base = samples[metric]["zipp"][baseline.id]
                node_stress = samples[metric]["node"][stressor.id]
                zipp_stress = samples[metric]["zipp"][stressor.id]
                relative: list[float] = []
                if len({len(node_base), len(zipp_base), len(node_stress), len(zipp_stress)}) == 1:
                    for nb, zb, ns, zs in zip(
                        node_base, zipp_base, node_stress, zipp_stress
                    ):
                        if min(nb, zb, ns, zs) <= 0:
                            relative = []
                            break
                        relative.append((zs / ns) / (zb / nb))
                relative_detail = _ratio_detail(
                    relative,
                    [1.0] * len(relative),
                    seed=core.derived_seed(
                        seed, "relative-degradation", family, stressor.id, metric
                    ),
                    bootstrap_samples=bootstrap_samples,
                )
                metrics[metric] = {
                    "engine_stressor_over_baseline": engine_ratios,
                    "relative_parity_ratio": relative_detail,
                }
            result.append(
                {
                    "family": family,
                    "available": True,
                    "baseline": baseline.id,
                    "stressor": stressor.id,
                    "variant": stressor.variant,
                    "metrics": metrics,
                }
            )
    return result


def category_balanced_geomean(
    cases: tuple[Case, ...],
    samples: dict[str, dict[str, list[float]]],
    *,
    seed: int,
    bootstrap_samples: int,
) -> dict[str, Any] | None:
    """Give every category equal weight while preserving paired repetitions.

    The ordinary suite geomean gives every row equal weight.  That is useful,
    but an evolving corpus can accidentally move the headline merely by adding
    several variants to one category.  This companion metric first takes the
    row geomean inside each category, then the geomean across categories.
    """

    by_category: dict[str, list[str]] = {}
    for case in cases:
        by_category.setdefault(case.category, []).append(case.id)
    if not by_category:
        return None

    ratios: dict[str, list[float]] = {}
    reps: int | None = None
    for case in cases:
        paired = list(zip(samples["zipp"][case.id], samples["node"][case.id]))
        if not paired or any(numerator <= 0 or denominator <= 0 for numerator, denominator in paired):
            return None
        row = [numerator / denominator for numerator, denominator in paired]
        if reps is None:
            reps = len(row)
        elif len(row) != reps:
            return None
        ratios[case.id] = row

    assert reps is not None

    def estimate(indexes: list[int] | None = None) -> float:
        category_points = []
        for rows in by_category.values():
            row_points = []
            for case_id in rows:
                values = ratios[case_id]
                selected = values if indexes is None else [values[index] for index in indexes]
                row_points.append(statistics.median(selected))
            category_points.append(core.geometric_mean(row_points))
        return core.geometric_mean(category_points)

    point = estimate()
    if reps == 1:
        ci = [point, point]
    else:
        rng = random.Random(seed)
        estimates = [
            estimate([rng.randrange(reps) for _ in range(reps)])
            for _ in range(bootstrap_samples)
        ]
        ci = [core.percentile(estimates, 0.025), core.percentile(estimates, 0.975)]
    return {
        "categories": {category: rows for category, rows in by_category.items()},
        "geomean_paired_ratio": point,
        "ci95": ci,
    }


def summarize(
    cases: tuple[Case, ...],
    measurement: dict[str, Any],
    *,
    seed: int,
    bootstrap_samples: int,
) -> dict[str, Any] | None:
    if measurement["health_failures"]:
        return None
    samples = measurement["samples"]
    ids = [case.id for case in cases]
    categories = sorted({case.category for case in cases})
    case_summaries: dict[str, Any] = {}
    for case in cases:
        case_summaries[case.id] = {
            "category": case.category,
            "family": case.family,
            "variant": case.variant,
            "metrics": {
                metric: core.metric_summary(
                    samples[metric],
                    ["node", "zipp"],
                    case.id,
                    "node",
                    "zipp",
                    seed=core.derived_seed(seed, "case", case.id, metric),
                    bootstrap_samples=bootstrap_samples,
                )
                for metric in ("startup", "cold", "adjusted")
            },
        }

    suite_summaries: dict[str, Any] = {}
    for metric in ("startup", "cold", "adjusted"):
        suite_summaries[metric] = {
            "overall": core.subset_geomean(
                samples[metric],
                ids,
                "node",
                "zipp",
                seed=core.derived_seed(seed, "overall", metric),
                bootstrap_samples=bootstrap_samples,
            ),
            "category_balanced": category_balanced_geomean(
                cases,
                samples[metric],
                seed=core.derived_seed(seed, "category-balanced", metric),
                bootstrap_samples=bootstrap_samples,
            ),
            "categories": {
                category: core.subset_geomean(
                    samples[metric],
                    [case.id for case in cases if case.category == category],
                    "node",
                    "zipp",
                    seed=core.derived_seed(seed, "category", category, metric),
                    bootstrap_samples=bootstrap_samples,
                )
                for category in categories
            },
        }

    return {
        "case_summaries": case_summaries,
        "suite_summaries": suite_summaries,
        "degradations": degradation_summaries(
            cases,
            samples,
            seed=seed,
            bootstrap_samples=bootstrap_samples,
        ),
    }


def _format_ratio(detail: dict[str, Any] | None) -> str:
    if not detail or detail.get("paired_ratio") is None:
        return "n/a"
    ci = detail.get("paired_ratio_ci95", detail.get("ci95"))
    span = f" [{ci[0]:.3f},{ci[1]:.3f}]" if ci else ""
    return f"{detail['paired_ratio']:.3f}x{span}"


def print_report(
    cases: tuple[Case, ...],
    summary: dict[str, Any] | None,
    measurement: dict[str, Any],
) -> None:
    if summary is None:
        print("statistics unavailable because one or more processes failed")
    else:
        for metric in ("cold", "adjusted"):
            print(f"\nmetric={metric}; paired medians; zipp/node")
            print(f"{'case':<30}{'node':>11}{'zipp':>11}  ratio [95% CI]")
            for case in cases:
                detail = summary["case_summaries"][case.id]["metrics"][metric]
                medians = detail["median_ms"]
                print(
                    f"{case.id:<30}{medians['node']:>9.1f}ms"
                    f"{medians['zipp']:>9.1f}ms  {_format_ratio(detail)}"
                )
            overall = summary["suite_summaries"][metric]["overall"]
            if overall:
                ci = overall.get("ci95")
                span = f" [{ci[0]:.3f},{ci[1]:.3f}]" if ci else ""
                print(
                    f"geomean[overall] {overall['geomean_paired_ratio']:.4f}x{span}"
                )
            balanced = summary["suite_summaries"][metric]["category_balanced"]
            if balanced:
                ci = balanced.get("ci95")
                span = f" [{ci[0]:.3f},{ci[1]:.3f}]" if ci else ""
                print(
                    "geomean[category-balanced] "
                    f"{balanced['geomean_paired_ratio']:.4f}x{span}"
                )
            for category, detail in summary["suite_summaries"][metric][
                "categories"
            ].items():
                if detail:
                    print(
                        f"geomean[{category}] "
                        f"{detail['geomean_paired_ratio']:.4f}x"
                    )

        available = [row for row in summary["degradations"] if row["available"]]
        if available:
            for metric in ("cold", "adjusted"):
                print(
                    f"\nfamily degradation metric={metric} "
                    "(stressor/baseline; relative >1 hurts Zipp more)"
                )
                for row in available:
                    detail = row["metrics"][metric]
                    engine = detail["engine_stressor_over_baseline"]
                    relative = detail["relative_parity_ratio"]
                    print(
                        f"{row['family']}: {row['stressor']} / {row['baseline']}  "
                        f"node={_format_ratio(engine['node'])}  "
                        f"zipp={_format_ratio(engine['zipp'])}  "
                        f"relative={_format_ratio(relative)}"
                    )

    for failure in measurement["health_failures"] + measurement["correctness_failures"]:
        print(f"  FAIL: {failure}")
    print(
        f"ALL_CORRECT={'1' if measurement['all_correct'] else '0'}  "
        "(exact bytes, no normalisation)"
    )


def case_to_json(case: Case) -> dict[str, Any]:
    data = asdict(case)
    data["entry"] = str(case.entry)
    data["inputs"] = [str(path) for path in case.inputs]
    data["input_rels"] = list(case.input_rels)
    data["features"] = list(case.features)
    return data


def input_digests(cases: tuple[Case, ...]) -> dict[str, str | None]:
    digests: dict[str, str | None] = {}
    for case in cases:
        for rel, path in zip(case.input_rels, case.inputs):
            digest = core.file_digest(path)
            previous = digests.get(rel)
            if previous is not None and previous != digest:
                raise ValueError(f"input path {rel!r} resolved inconsistently")
            digests[rel] = digest
    return dict(sorted(digests.items()))


def harness_digests() -> dict[str, str | None]:
    paths = (Path(__file__).resolve(), CORE_PATH.resolve())
    return {
        path.relative_to(ROOT).as_posix(): core.file_digest(path)
        for path in paths
    }


def digest_drift(
    manifest_before: str | None,
    manifest_after: str | None,
    inputs_before: dict[str, str | None],
    inputs_after: dict[str, str | None],
    *,
    harnesses_before: dict[str, str | None] | None = None,
    harnesses_after: dict[str, str | None] | None = None,
) -> list[str]:
    drift: list[str] = []
    if manifest_before != manifest_after:
        drift.append(
            f"manifest changed during run ({manifest_before} -> {manifest_after})"
        )
    for path in sorted(set(inputs_before) | set(inputs_after)):
        before = inputs_before.get(path)
        after = inputs_after.get(path)
        if before != after:
            drift.append(f"input {path} changed during run ({before} -> {after})")
    before_harnesses = harnesses_before or {}
    after_harnesses = harnesses_after or {}
    for path in sorted(set(before_harnesses) | set(after_harnesses)):
        before = before_harnesses.get(path)
        after = after_harnesses.get(path)
        if before != after:
            drift.append(f"harness {path} changed during run ({before} -> {after})")
    return drift


def artifact_publishable(
    provenance_reasons: list[str],
    engine_drift: list[str],
    source_drift: list[str],
    *,
    all_correct: bool,
    publication_reasons: list[str],
) -> bool:
    return (
        all_correct
        and not provenance_reasons
        and not engine_drift
        and not source_drift
        and not publication_reasons
    )


def publication_policy_reasons(
    manifest_path: Path,
    *,
    filtered: bool,
    reps: int,
    bootstrap_samples: int,
    source_reason: str | None,
    environment: dict[str, str],
) -> list[str]:
    """Name non-canonical choices that make a run diagnostic-only."""

    reasons: list[str] = []
    if manifest_path.resolve() != DEFAULT_MANIFEST.resolve():
        reasons.append("alternate manifest (canonical hostile manifest required)")
    if filtered:
        reasons.append("filtered corpus (all default cases required)")
    if reps < DEFAULT_REPS:
        reasons.append(
            f"only {reps} repetitions (at least {DEFAULT_REPS} required)"
        )
    if bootstrap_samples < core.BOOTSTRAP_SAMPLES:
        reasons.append(
            f"only {bootstrap_samples} bootstrap samples "
            f"(at least {core.BOOTSTRAP_SAMPLES} required)"
        )
    if source_reason is not None:
        reasons.append(source_reason)
    if environment:
        reasons.append(
            "benchmark-affecting environment variables are set "
            "(canonical publication requires a clean inherited environment)"
        )
    return reasons


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    parser.add_argument("--cases", help="comma-separated case ids")
    parser.add_argument("--categories", help="comma-separated categories")
    parser.add_argument("--families", help="comma-separated stressor families")
    parser.add_argument(
        "--features",
        help="comma-separated features; selected cases must contain all of them",
    )
    parser.add_argument("--list", action="store_true", help="list selected cases and exit")
    parser.add_argument("--reps", type=int, default=DEFAULT_REPS)
    parser.add_argument(
        "--seed",
        type=lambda value: int(value, 0),
        default=core.DEFAULT_SEED,
    )
    parser.add_argument(
        "--bootstrap-samples", type=int, default=core.BOOTSTRAP_SAMPLES
    )
    parser.add_argument(
        "--timeout",
        type=float,
        help="override every case timeout (seconds)",
    )
    parser.add_argument("--node", default="node")
    parser.add_argument(
        "--zipp",
        default=str(
            ROOT
            / "target"
            / "release"
            / ("zipp.exe" if os.name == "nt" else "zipp")
        ),
    )
    parser.add_argument("--json", help="write a raw hostile-benchmark artifact")
    parser.add_argument(
        "--overwrite-json",
        action="store_true",
        help="atomically replace an existing --json artifact",
    )
    parser.add_argument(
        "--allow-dirty-engine",
        action="store_true",
        help=(
            "measure a binary built from a dirty tree and mark the result "
            "publishable:false"
        ),
    )
    parser.add_argument(
        "--allow-nonhead-engine",
        action="store_true",
        help=(
            "measure a binary built from a commit other than workspace HEAD "
            "and mark the result publishable:false"
        ),
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.reps < 1:
        raise SystemExit("--reps must be positive")
    if args.bootstrap_samples < 1:
        raise SystemExit("--bootstrap-samples must be positive")
    if args.timeout is not None and (
        not math.isfinite(args.timeout) or args.timeout <= 0
    ):
        raise SystemExit("--timeout must be finite and positive")

    try:
        manifest = load_manifest(args.manifest)
        cases = select_cases(
            manifest,
            case_ids=parse_csv(args.cases, "--cases"),
            categories=parse_csv(args.categories, "--categories"),
            families=parse_csv(args.families, "--families"),
            features=parse_csv(args.features, "--features"),
        )
    except (ManifestError, ValueError) as exc:
        raise SystemExit(str(exc)) from exc

    publication_paths = {
        manifest.path,
        Path(__file__).resolve(),
        CORE_PATH.resolve(),
        *(
            manifest.root / Path(rel)
            for case in cases
            for rel in case.input_rels
        ),
    }
    publication_sources_head_before, publication_source_reason = (
        core.git_paths_match_head(publication_paths)
    )
    environment = core.relevant_environment()
    publication_reasons = publication_policy_reasons(
        manifest.path,
        filtered=any(
            selector is not None
            for selector in (args.cases, args.categories, args.families, args.features)
        ),
        reps=args.reps,
        bootstrap_samples=args.bootstrap_samples,
        source_reason=publication_source_reason,
        environment=environment,
    )

    if args.list:
        for case in cases:
            family = (
                f" family={case.family}/{case.variant}" if case.family is not None else ""
            )
            features = f" features={','.join(case.features)}" if case.features else ""
            print(
                f"{case.id}\t{case.category}\t{case.goal}\t{case.entry_rel}"
                f"{family}{features}"
            )
        return 0

    if publication_reasons:
        print("publication policy (this run is diagnostic-only):", file=sys.stderr)
        for reason in publication_reasons:
            print(f"  {reason}", file=sys.stderr)

    json_path = Path(args.json).resolve() if args.json else None
    if json_path and json_path.exists() and not args.overwrite_json:
        raise SystemExit(
            f"refusing to overwrite existing result: {json_path} "
            "(pass --overwrite-json to replace it)"
        )

    workspace_commit = core.git_revision()
    manifest_hash_before = core.file_digest(manifest.path)
    input_hashes_before = input_digests(cases)
    harness_hashes_before = harness_digests()
    engines_before = [
        core.engine_metadata("node", [args.node], args.timeout or DEFAULT_TIMEOUT_S),
        core.engine_metadata("zipp", [args.zipp], args.timeout or DEFAULT_TIMEOUT_S),
    ]
    provenance_reasons, uncovered_provenance_reasons = core.provenance_assessment(
        engines_before,
        workspace_commit,
        is_ab=False,
        allow_dirty=args.allow_dirty_engine,
        allow_nonhead=args.allow_nonhead_engine,
    )
    if provenance_reasons:
        print("engine provenance:", file=sys.stderr)
        for reason in provenance_reasons:
            print(f"  {reason}", file=sys.stderr)
        if core.provenance_is_fatal(
            uncovered_provenance_reasons,
            is_ab=False,
        ):
            raise SystemExit(
                "refusing to measure: rebuild the release binary from the "
                "clean HEAD, or pass --allow-dirty-engine / "
                "--allow-nonhead-engine for an explicitly UNPUBLISHABLE run"
            )
        print("  recorded; this artifact is publishable:false", file=sys.stderr)
    measurement = run_measurements(
        cases,
        node=args.node,
        zipp=args.zipp,
        reps=args.reps,
        seed=args.seed,
        timeout_override=args.timeout,
    )
    engines_after = [
        core.engine_metadata("node", [args.node], args.timeout or DEFAULT_TIMEOUT_S),
        core.engine_metadata("zipp", [args.zipp], args.timeout or DEFAULT_TIMEOUT_S),
    ]
    drift = core.engine_drift(engines_before, engines_after)
    for reason in drift:
        measurement["health_failures"].append(f"engine changed during run: {reason}")
    manifest_hash_after = core.file_digest(manifest.path)
    input_hashes_after = input_digests(cases)
    harness_hashes_after = harness_digests()
    source_drift = digest_drift(
        manifest_hash_before,
        manifest_hash_after,
        input_hashes_before,
        input_hashes_after,
        harnesses_before=harness_hashes_before,
        harnesses_after=harness_hashes_after,
    )
    measurement["health_failures"].extend(source_drift)
    if drift or source_drift:
        measurement["all_correct"] = False
    publication_sources_head_after, publication_source_reason_after = (
        core.git_paths_match_head(publication_paths)
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

    summary = summarize(
        cases,
        measurement,
        seed=args.seed,
        bootstrap_samples=args.bootstrap_samples,
    )
    print_report(cases, summary, measurement)

    if json_path:
        artifact = {
            "schema_version": ARTIFACT_SCHEMA_VERSION,
            "created_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
            "workspace_commit": workspace_commit,
            "publishable": artifact_publishable(
                provenance_reasons,
                drift,
                source_drift,
                all_correct=measurement["all_correct"],
                publication_reasons=publication_reasons,
            ),
            "provenance_reasons": provenance_reasons,
            "publication_reasons": publication_reasons,
            "publication_sources_head_before": publication_sources_head_before,
            "publication_sources_head_after": publication_sources_head_after,
            "manifest": str(manifest.path),
            "manifest_sha256_before": manifest_hash_before,
            "manifest_sha256_after": manifest_hash_after,
            "harness_sha256_before": harness_hashes_before,
            "harness_sha256_after": harness_hashes_after,
            "input_sha256_before": input_hashes_before,
            "input_sha256_after": input_hashes_after,
            "source_drift": source_drift,
            "cases": [case_to_json(case) for case in cases],
            "selection": {
                "cases": parse_csv(args.cases, "--cases"),
                "categories": parse_csv(args.categories, "--categories"),
                "families": parse_csv(args.families, "--families"),
                "features": parse_csv(args.features, "--features"),
            },
            "reps": args.reps,
            "seed": args.seed,
            "bootstrap_samples": args.bootstrap_samples,
            "timeout_override_s": args.timeout,
            "engines_before": engines_before,
            "engines_after": engines_after,
            "engine_drift": drift,
            "host": {
                "platform": platform.platform(),
                "machine": platform.machine(),
                "python": platform.python_version(),
                "power_mode": core.power_mode(),
            },
            "environment": environment,
            "schedules": measurement["schedules"],
            "observations": measurement["observations"],
            "all_correct": measurement["all_correct"],
            "health_failures": measurement["health_failures"],
            "correctness_failures": measurement["correctness_failures"],
            "summary": summary,
        }
        core.write_json_result(json_path, artifact, overwrite=args.overwrite_json)

    return 0 if measurement["all_correct"] and summary is not None else 1


if __name__ == "__main__":
    raise SystemExit(main())
