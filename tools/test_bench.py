import argparse
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


BENCH_PATH = Path(__file__).with_name("bench.py")
SPEC = importlib.util.spec_from_file_location("zipp_bench", BENCH_PATH)
assert SPEC and SPEC.loader
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)

PGO_CORPUS_PATH = Path(__file__).with_name("pgo_corpus.py")
PGO_CORPUS_SPEC = importlib.util.spec_from_file_location(
    "zipp_pgo_corpus", PGO_CORPUS_PATH
)
assert PGO_CORPUS_SPEC and PGO_CORPUS_SPEC.loader
pgo_corpus = importlib.util.module_from_spec(PGO_CORPUS_SPEC)
sys.modules[PGO_CORPUS_SPEC.name] = pgo_corpus
PGO_CORPUS_SPEC.loader.exec_module(pgo_corpus)

PGO_TRAINING_PATH = Path(__file__).with_name("pgo_training.py")
PGO_TRAINING_SPEC = importlib.util.spec_from_file_location(
    "zipp_pgo_training", PGO_TRAINING_PATH
)
assert PGO_TRAINING_SPEC and PGO_TRAINING_SPEC.loader
pgo_training = importlib.util.module_from_spec(PGO_TRAINING_SPEC)
sys.modules[PGO_TRAINING_SPEC.name] = pgo_training
PGO_TRAINING_SPEC.loader.exec_module(pgo_training)


def v2_observation(
    engine,
    rep,
    *,
    startup,
    cold,
    digest="00" * 32,
    valid=True,
):
    return {
        "engine": engine,
        "bench": "one",
        "rep": rep,
        "startup_s": startup,
        "cold_s": cold,
        "startup_returncode": 0 if valid else 1,
        "returncode": 0,
        "startup_timed_out": False,
        "timed_out": False,
        "startup_stdout_bytes": 0,
        "stdout_bytes": 2,
        "stdout_sha256": digest,
        "valid_for_stats": valid,
    }


def v2_result(observations, *, reps=2, metric="cold"):
    return {
        "schema_version": 2,
        "reps": reps,
        "benches": ["one"],
        "baseline": "old",
        "engines": [{"name": "old"}, {"name": "new"}],
        "observations": observations,
        "seed": 123,
        "bootstrap_samples": 77,
        "headline_metric": metric,
        "all_correct": True,
        "health_failures": [],
        "correctness_failures": [],
        "failures": [],
    }


class BenchMathTests(unittest.TestCase):
    def test_percentile_interpolates(self):
        self.assertEqual(bench.percentile([0.0, 10.0], 0.25), 2.5)

    def test_geomean(self):
        self.assertAlmostEqual(bench.geometric_mean([1.0, 4.0]), 2.0)

    def test_canonical_engine_command_bypasses_mutable_wrapper(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            wrapper = root / "bun.cmd"
            target = root / "node_modules" / "bun" / "bin" / "bun.exe"
            target.parent.mkdir(parents=True)
            wrapper.write_text("@echo off\n", encoding="utf-8")
            target.write_bytes(b"native-bun")
            completed = subprocess.CompletedProcess(
                [str(wrapper)], 0, stdout=(str(target) + "\n").encode(), stderr=b""
            )
            child_env = {"ONLY": "canonical"}
            with mock.patch.object(
                bench.subprocess, "run", return_value=completed
            ) as run:
                command = bench.canonical_engine_command(
                    "bun",
                    [str(wrapper), "run"],
                    1.0,
                    process_env=child_env,
                )
        self.assertEqual(command, [str(target.resolve()), "run"])
        self.assertEqual(run.call_args.kwargs["env"], child_env)

    def test_canonical_engine_command_fails_closed_on_ambiguous_probe(self):
        completed = subprocess.CompletedProcess(
            ["deno"], 0, stdout=b"one\ntwo\n", stderr=b""
        )
        with mock.patch.object(bench.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(ValueError, "invalid native executable probe"):
                bench.canonical_engine_command("deno", ["deno", "run"], 1.0)

    def test_launcher_resolution_probe_gets_its_own_cleaned_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "node.exe"
            target.write_bytes(b"node")
            completed = subprocess.CompletedProcess(
                ["node"], 0, stdout=(str(target) + "\n").encode(), stderr=b""
            )
            with mock.patch.object(
                bench.subprocess, "run", return_value=completed
            ) as run:
                bench.canonical_engine_command(
                    "node", ["node"], 1.0, fresh_environment=True
                )
            home = Path(run.call_args.kwargs["env"]["HOME"])
        self.assertFalse(home.exists())

    def test_metadata_subprocesses_use_distinct_fresh_environments(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = Path(directory) / "zipp.exe"
            executable.write_bytes(b"zipp")
            probes = [
                subprocess.CompletedProcess([], 0, stdout=b"zipp 1\n", stderr=b""),
                subprocess.CompletedProcess([], 0, stdout=b"{}\n", stderr=b""),
            ]
            with mock.patch.object(
                bench.subprocess, "run", side_effect=probes
            ) as run:
                bench.engine_metadata(
                    "zipp",
                    [str(executable)],
                    1.0,
                    fresh_environment=True,
                )
            homes = [Path(call.kwargs["env"]["HOME"]) for call in run.call_args_list]
        self.assertEqual(len(homes), 2)
        self.assertNotEqual(homes[0], homes[1])
        self.assertTrue(all(not home.exists() for home in homes))

    def test_paired_ratios_keep_pairing(self):
        self.assertEqual(bench.paired_ratios([4.0, 9.0], [2.0, 3.0]), [2.0, 3.0])

    def test_paired_ratios_reject_nonpositive_samples(self):
        with self.assertRaisesRegex(ValueError, "positive"):
            bench.paired_ratios([1.0, 0.0], [1.0, 2.0])
        with self.assertRaisesRegex(ValueError, "positive"):
            bench.paired_ratios([1.0, 2.0], [1.0, -2.0])

    def test_two_engine_order_is_counterbalanced(self):
        engines = [("old", ["old"]), ("new", ["new"])]
        self.assertEqual(
            [name for name, _ in bench.engine_order_for_rep(engines, 0, 1)],
            ["old", "new"],
        )
        self.assertEqual(
            [name for name, _ in bench.engine_order_for_rep(engines, 1, 1)],
            ["new", "old"],
        )

    def test_four_engine_order_is_deterministic_and_balanced(self):
        engines = [(name, [name]) for name in ("node", "bun", "deno", "zipp")]

        def schedule(reps):
            return [
                [name for name, _ in bench.engine_order_for_rep(engines, rep, 17)]
                for rep in range(reps)
            ]

        first_block = schedule(4)
        self.assertEqual(first_block, schedule(4))
        for position in range(4):
            self.assertEqual(
                {order[position] for order in first_block},
                {name for name, _ in engines},
            )

        two_blocks = schedule(8)
        names = [name for name, _ in engines]
        for index, left in enumerate(names):
            for right in names[index + 1 :]:
                left_first = sum(
                    order.index(left) < order.index(right) for order in two_blocks
                )
                self.assertEqual(left_first, 4, (left, right, two_blocks))

        publishable_length = schedule(bench.MIN_PUBLISHABLE_REPS)
        for name in names:
            exposures = [
                sum(order[position] == name for order in publishable_length)
                for position in range(4)
            ]
            self.assertLessEqual(max(exposures) - min(exposures), 1)
        for index, left in enumerate(names):
            for right in names[index + 1 :]:
                left_first = sum(
                    order.index(left) < order.index(right)
                    for order in publishable_length
                )
                self.assertIn(left_first, (7, 8))

    def test_parse_env_assignments(self):
        self.assertEqual(
            bench.parse_env_assignments("A=1,B=two=parts"),
            {"A": "1", "B": "two=parts"},
        )
        self.assertEqual(bench.parse_env_assignments("-"), {})

    def test_run_once_times_out_and_cleans_up(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "sleep.py"
            script.write_text(
                "import time\ntime.sleep(5)\n",
                encoding="utf-8",
            )
            result = bench.run_once(
                [sys.executable],
                script,
                timeout=0.05,
            )
        self.assertTrue(result["timed_out"])
        self.assertFalse(result["spawn_error"])
        self.assertIsNone(result["returncode"])
        self.assertLess(result["elapsed_s"], 2.0)

    def test_run_once_can_replace_the_ambient_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "environment.py"
            script.write_text(
                "import json, os\nprint(json.dumps(dict(os.environ)))\n",
                encoding="utf-8",
            )
            with mock.patch.dict(
                bench.os.environ,
                {"AMBIENT_FUTURE_RUNTIME_KNOB": "hostile"},
                clear=True,
            ):
                result = bench.run_once(
                    [sys.executable],
                    script,
                    timeout=2.0,
                    base_env={"CANONICAL_ONLY": "yes"},
                    env={"EXPLICIT_OVERLAY": "yes"},
                )
        self.assertEqual(result["returncode"], 0, result["stderr"])
        child = json.loads(result["stdout"])
        self.assertEqual(child["CANONICAL_ONLY"], "yes")
        self.assertEqual(child["EXPLICIT_OVERLAY"], "yes")
        self.assertNotIn("AMBIENT_FUTURE_RUNTIME_KNOB", child)

    def test_run_once_uses_and_cleans_a_fresh_root_per_process(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "home.py"
            script.write_text(
                "import os\nprint(os.environ['HOME'])\n",
                encoding="utf-8",
            )
            first = bench.run_once(
                [sys.executable], script, timeout=2.0, fresh_environment=True
            )
            second = bench.run_once(
                [sys.executable], script, timeout=2.0, fresh_environment=True
            )
        homes = [
            Path(result["stdout"].decode("utf-8").strip())
            for result in (first, second)
        ]
        self.assertNotEqual(homes[0], homes[1])
        self.assertTrue(all(not home.exists() for home in homes))
        self.assertEqual(
            bench.canonical_benchmark_environment_descriptor()["lifecycle"],
            "fresh isolated root per child process",
        )

    def test_canonical_child_environment_is_allowlisted_and_isolated(self):
        with tempfile.TemporaryDirectory() as directory:
            child = bench.canonical_benchmark_environment(
                Path(directory),
                host_environment={
                    "SystemRoot": r"C:\Windows",
                    "LD_PRELOAD": "injected",
                    "NODE_OPTIONS": "--require=evil",
                    "FUTURE_ENGINE_KNOB": "evil",
                    "PATH": "host-search-path",
                },
            )
        self.assertEqual(child["LANG"], "C")
        self.assertEqual(child["LC_ALL"], "C")
        self.assertEqual(child["TZ"], "UTC")
        self.assertNotIn("LD_PRELOAD", child)
        self.assertNotIn("NODE_OPTIONS", child)
        self.assertNotIn("FUTURE_ENGINE_KNOB", child)
        self.assertNotEqual(child["PATH"], "host-search-path")
        self.assertEqual(
            bench.canonical_benchmark_environment_descriptor()["inherit"],
            "none",
        )

    def test_bootstrap_is_deterministic_and_contains_point_estimate(self):
        ratios = [0.8, 0.9, 1.0, 1.1, 1.2]
        first = bench.bootstrap_median_ci(ratios, seed=7, samples=500)
        second = bench.bootstrap_median_ci(ratios, seed=7, samples=500)
        self.assertEqual(first, second)
        self.assertLessEqual(first[0], 1.0)
        self.assertGreaterEqual(first[1], 1.0)

    def test_exact_sign_test_uses_strict_wins_and_finite_sample_tail(self):
        fourteen = bench.exact_one_sided_sign_test(
            [0.5] * 14 + [1.5], [1.0] * 15
        )
        thirteen = bench.exact_one_sided_sign_test(
            [0.5] * 13 + [1.5, 1.0], [1.0] * 15
        )
        assert fourteen is not None and thirteen is not None
        self.assertEqual(fourteen["strict_wins"], 14)
        self.assertEqual(fourteen["one_sided_pvalue"], 16 / 32768)
        self.assertLessEqual(fourteen["one_sided_pvalue"], 0.05 / 51)
        self.assertEqual(thirteen["strict_wins"], 13)
        self.assertEqual(thirteen["ties"], 1)
        self.assertEqual(thirteen["one_sided_pvalue"], 121 / 32768)
        self.assertGreater(thirteen["one_sided_pvalue"], 0.05 / 51)

    def test_suite_bootstrap_is_deterministic(self):
        ratios = [
            [0.9, 1.0, 1.1, 1.0, 0.95],
            [1.1, 1.0, 0.9, 1.0, 1.05],
        ]
        first = bench.bootstrap_geomean_of_medians_ci(
            ratios,
            seed=9,
            samples=500,
        )
        second = bench.bootstrap_geomean_of_medians_ci(
            ratios,
            seed=9,
            samples=500,
        )
        self.assertEqual(first, second)
        self.assertLessEqual(first[0], 1.0)
        self.assertGreaterEqual(first[1], 1.0)

    def test_validate_comparison_rejects_bad_subsets(self):
        with self.assertRaisesRegex(ValueError, "comparison target"):
            bench.validate_comparison(["node", "bun"], "node", "zipp")
        with self.assertRaisesRegex(ValueError, "must be different"):
            bench.validate_comparison(["node", "zipp"], "zipp", "zipp")
        with self.assertRaisesRegex(ValueError, "duplicate"):
            bench.validate_comparison(["node", "zipp", "zipp"], "node", "zipp")

    def test_metric_summary_marks_nonpositive_adjusted_ratio_unavailable(self):
        samples = {
            "old": {"one": [0.2, -0.1]},
            "new": {"one": [0.1, 0.3]},
        }
        result = bench.metric_summary(
            samples,
            ["old", "new"],
            "one",
            "old",
            "new",
            seed=1,
            bootstrap_samples=25,
        )
        self.assertIsNone(result["paired_ratio"])
        self.assertIsNone(result["paired_ratio_ci95"])
        self.assertEqual(result["nonpositive_pairs"], 1)


class BenchResultTests(unittest.TestCase):
    def test_schema_v1_reader_preserves_signed_adjusted_samples(self):
        data = {
            "reps": 2,
            "benches": ["one"],
            "engines": ["old", "new"],
            "startup_s": {
                "old": [0.2, 0.3],
                "new": [0.1, 0.1],
            },
            "samples": {
                "old": {"one": [0.1, 0.5]},
                "new": {"one": [0.2, 0.3]},
            },
            "all_correct": True,
            "failures": [],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "v1.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            result = bench.load_result(path)

        self.assertEqual(result["schema_version"], 1)
        self.assertEqual(result["baseline"], "old")
        for actual, expected in zip(
            result["adjusted"]["old"]["one"],
            [-0.1, 0.2],
        ):
            self.assertAlmostEqual(actual, expected)
        for actual, expected in zip(
            result["adjusted"]["new"]["one"],
            [0.1, 0.2],
        ):
            self.assertAlmostEqual(actual, expected)
        self.assertTrue(result["all_correct"])
        self.assertFalse(result["has_health_failures"])

    def test_schema_v1_reader_marks_short_columns_incomplete(self):
        result = bench.normalize_result_data(
            {
                "reps": 2,
                "benches": ["one"],
                "engines": ["old", "new"],
                "startup_s": {"old": [0.1], "new": [0.1]},
                "samples": {
                    "old": {"one": [1.0]},
                    "new": {"one": [0.8]},
                },
                "all_correct": True,
                "failures": [],
            }
        )

        self.assertTrue(result["has_health_failures"])
        self.assertFalse(result["all_correct"])
        self.assertIn("incomplete samples", "\n".join(result["failures"]))

    def test_schema_v2_reader_excludes_invalid_observations(self):
        def observation(engine, rep, startup, cold, valid):
            return {
                "engine": engine,
                "bench": "one",
                "rep": rep,
                "startup_s": startup,
                "cold_s": cold,
                "startup_returncode": 0 if valid else 1,
                "returncode": 0,
                "startup_timed_out": False,
                "timed_out": False,
                "stdout_sha256": "00" * 32,
                "valid_for_stats": valid,
            }

        result = bench.normalize_result_data(
            {
                "schema_version": 2,
                "reps": 2,
                "benches": ["one"],
                "baseline": "old",
                "engines": [{"name": "old"}, {"name": "new"}],
                "observations": [
                    observation("old", 0, 0.1, 1.0, True),
                    observation("new", 0, 0.1, 0.8, True),
                    observation("old", 1, 0.2, 99.0, False),
                    observation("new", 1, 0.1, 0.9, True),
                ],
                "all_correct": True,
                "failures": [],
            }
        )

        self.assertEqual(result["cold"]["old"]["one"], [1.0])
        self.assertEqual(result["adjusted"]["old"]["one"], [0.9])
        self.assertNotIn(99.0, result["cold"]["old"]["one"])
        self.assertTrue(result["has_health_failures"])
        self.assertFalse(result["all_correct"])
        self.assertRegex(result["failures"][0], "invalid observation")

    def test_schema_v2_reader_pairs_by_rep_not_json_order(self):
        observations = [
            v2_observation("new", 1, startup=0.1, cold=5.0),
            v2_observation("old", 0, startup=0.1, cold=1.0),
            v2_observation("new", 0, startup=0.1, cold=2.0),
            v2_observation("old", 1, startup=0.1, cold=10.0),
        ]
        result = bench.normalize_result_data(v2_result(observations))

        self.assertEqual(result["cold"]["old"]["one"], [1.0, 10.0])
        self.assertEqual(result["cold"]["new"]["one"], [2.0, 5.0])
        self.assertEqual(
            bench.paired_ratios(
                result["cold"]["new"]["one"],
                result["cold"]["old"]["one"],
            ),
            [2.0, 0.5],
        )

    def test_schema_v2_reader_rejects_nonfinite_and_malformed_fields(self):
        base = [
            v2_observation("old", 0, startup=0.1, cold=1.0),
            v2_observation("new", 0, startup=0.1, cold=0.8),
        ]
        cases = [
            ("cold_s", float("nan"), "finite"),
            ("startup_s", float("inf"), "finite"),
            ("stdout_bytes", -1, "nonnegative integer"),
            ("rep", 0.5, "repetition"),
        ]
        for field, value, message in cases:
            with self.subTest(field=field):
                observations = [dict(item) for item in base]
                observations[0][field] = value
                with self.assertRaisesRegex(ValueError, message):
                    bench.normalize_result_data(
                        v2_result(observations, reps=1)
                    )

    def test_schema_v2_reader_detects_health_and_output_contradictions(self):
        health = [
            v2_observation("old", 0, startup=0.1, cold=1.0),
            v2_observation("new", 0, startup=0.1, cold=0.8),
        ]
        health[0]["startup_returncode"] = 1
        health_result = bench.normalize_result_data(
            v2_result(health, reps=1)
        )
        self.assertTrue(health_result["has_health_failures"])
        self.assertFalse(health_result["all_correct"])
        self.assertRegex(
            "\n".join(health_result["failures"]),
            "contradictory health marker",
        )

        output = [
            v2_observation(
                "old",
                0,
                startup=0.1,
                cold=1.0,
                digest="11" * 32,
            ),
            v2_observation(
                "new",
                0,
                startup=0.1,
                cold=0.8,
                digest="22" * 32,
            ),
        ]
        output_result = bench.normalize_result_data(
            v2_result(output, reps=1)
        )
        self.assertTrue(output_result["has_correctness_failures"])
        self.assertFalse(output_result["all_correct"])
        self.assertRegex(
            "\n".join(output_result["failures"]),
            "output differs",
        )

    def test_historical_report_keeps_negative_adjusted_median(self):
        cold = {
            "old": {"one": [0.05, 0.06]},
            "new": {"one": [0.2, 0.21]},
        }
        startup = {
            "old": {"one": [0.1, 0.1]},
            "new": {"one": [0.1, 0.1]},
        }
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            summary = bench.print_historical_report(
                benches=["one"],
                engine_names=["old", "new"],
                baseline="old",
                compare_name="new",
                cold=cold,
                startup=startup,
                all_correct=True,
                ab=True,
            )

        self.assertIsNone(summary["geomean_adjusted_ratio"])
        self.assertAlmostEqual(summary["rows"]["one"]["median_ms"]["old"], -45.0)
        self.assertIn("n/a", output.getvalue())
        self.assertIn("nonpositive", output.getvalue())

    def test_modern_report_uses_dynamic_readme_labels_and_preserves_phases(self):
        cold = {
            "bun": {"one": [0.4, 0.42]},
            "zipp": {"one": [0.2, 0.21]},
        }
        startup = {
            "bun": {"one": [0.04, 0.05]},
            "zipp": {"one": [0.01, 0.02]},
        }
        adjusted = {
            name: {
                "one": [
                    run - launch
                    for run, launch in zip(cold[name]["one"], startup[name]["one"])
                ]
            }
            for name in ("bun", "zipp")
        }
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            summary = bench.print_modern_report(
                benches=["one"],
                engine_names=["bun", "zipp"],
                baseline="bun",
                compare_name="zipp",
                cold=cold,
                startup=startup,
                adjusted=adjusted,
                metric="cold",
                seed=1,
                bootstrap_samples=50,
                all_correct=True,
                ab=False,
                readme=True,
                reps=2,
            )

        rendered = output.getvalue()
        self.assertIn("| bench | bun | zipp |", rendered)
        self.assertNotIn("| bench | node |", rendered)
        self.assertIn("95% CI", rendered)
        self.assertIn("geomean_paired_ratio_ci95", summary)
        self.assertEqual(
            set(summary["rows"]["one"]["metrics"]),
            {"cold", "startup", "adjusted"},
        )
        self.assertEqual(
            set(summary["metric_geomean_paired_ratio"]),
            {"cold", "startup", "adjusted"},
        )

    def test_all_engine_criterion_uses_every_paired_competitor_ci(self):
        names = ["node", "bun", "deno", "zipp"]
        cold = {
            "node": {"one": [1.0] * 15},
            "bun": {"one": [0.9] * 15},
            "deno": {"one": [0.4] * 15},
            # Faster than Node and Bun, but slower than Deno.
            "zipp": {"one": [0.5] * 15},
        }
        startup = {name: {"one": [0.01] * 15} for name in names}
        adjusted = {
            name: {
                "one": [
                    run - launch
                    for run, launch in zip(cold[name]["one"], startup[name]["one"])
                ]
            }
            for name in names
        }
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            summary = bench.print_modern_report(
                benches=["one"],
                engine_names=names,
                baseline="node",
                compare_name="zipp",
                cold=cold,
                startup=startup,
                adjusted=adjusted,
                metric="cold",
                seed=1,
                bootstrap_samples=50,
                all_correct=True,
                ab=False,
                readme=False,
                reps=15,
            )

        criterion = summary["all_engine_criterion"]
        self.assertEqual(criterion["comparison_count"], 3)
        self.assertEqual(criterion["point_estimate_wins"], 2)
        self.assertEqual(
            criterion["descriptive_bootstrap_95pct_interval_wins"], 2
        )
        self.assertEqual(criterion["statistically_proven_wins"], 2)
        self.assertEqual(
            criterion["multiple_comparison_method"],
            "Bonferroni-adjusted exact one-sided paired sign test",
        )
        self.assertAlmostEqual(criterion["per_comparison_alpha"], 0.05 / 3)
        self.assertFalse(criterion["median_faster_on_every_row"])
        self.assertFalse(criterion["statistically_faster_on_every_row"])
        self.assertGreater(
            criterion["rows"]["one"]["deno"]["paired_ratio"], 1.0
        )
        self.assertEqual(
            criterion["rows"]["one"]["node"]["exact_sign_test"]["strict_wins"],
            15,
        )
        self.assertIn("FASTER_THAN_EVERY_ENGINE_ON_EVERY_ROW=0", output.getvalue())
        self.assertIn("unproven: one zipp/deno", output.getvalue())

    def test_all_engine_claim_uses_exact_sign_test_not_descriptive_intervals(self):
        samples = {
            "node": {"one": [1.0] * 15},
            "bun": {"one": [1.0] * 15},
            "zipp": {"one": [0.8] * 11 + [1.2] * 4},
        }

        def interval(_ratios, *, seed, samples, alpha=0.05):
            del seed, samples
            return (0.7, 0.9) if alpha == 0.05 else (0.6, 1.01)

        with mock.patch.object(bench, "bootstrap_median_ci", side_effect=interval):
            result = bench.all_competitor_comparison_summary(
                samples,
                ["one"],
                ["node", "bun", "zipp"],
                "zipp",
                seed=1,
                bootstrap_samples=50,
            )

        self.assertEqual(
            result["descriptive_bootstrap_95pct_interval_wins"], 2
        )
        self.assertEqual(result["statistically_proven_wins"], 0)
        self.assertFalse(result["statistically_faster_on_every_row"])

    def test_adjusted_report_errors_before_printing_partial_table(self):
        samples = {
            "old": {"one": [0.1, -0.1]},
            "new": {"one": [0.2, 0.3]},
        }
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            with self.assertRaisesRegex(ValueError, "nonpositive"):
                bench.print_modern_report(
                    benches=["one"],
                    engine_names=["old", "new"],
                    baseline="old",
                    compare_name="new",
                    cold={
                        "old": {"one": [1.0, 1.1]},
                        "new": {"one": [0.8, 0.9]},
                    },
                    startup={
                        "old": {"one": [0.1, 0.1]},
                        "new": {"one": [0.1, 0.1]},
                    },
                    adjusted=samples,
                    metric="adjusted",
                    seed=1,
                    bootstrap_samples=25,
                    all_correct=True,
                    ab=True,
                    readme=False,
                    reps=2,
                )
        self.assertEqual(output.getvalue(), "")

    def test_json_overwrite_is_explicit_and_atomic(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "result.json"
            path.write_text('{"old": true}\n', encoding="utf-8")
            with self.assertRaises(FileExistsError):
                bench.write_json_result(path, {"new": True}, overwrite=False)

            bench.write_json_result(path, {"new": True}, overwrite=True)
            self.assertEqual(
                json.loads(path.read_text(encoding="utf-8")),
                {"new": True},
            )

            path.write_text('{"preserved": true}\n', encoding="utf-8")
            with (
                mock.patch.object(
                    bench.json,
                    "dump",
                    side_effect=RuntimeError("interrupted"),
                ),
                self.assertRaisesRegex(RuntimeError, "interrupted"),
            ):
                bench.write_json_result(path, {"lost": True}, overwrite=True)
            self.assertEqual(
                json.loads(path.read_text(encoding="utf-8")),
                {"preserved": True},
            )

            new_path = Path(directory) / "interrupted-first-write.json"
            with (
                mock.patch.object(
                    bench.json,
                    "dump",
                    side_effect=RuntimeError("interrupted"),
                ),
                self.assertRaisesRegex(RuntimeError, "interrupted"),
            ):
                bench.write_json_result(new_path, {"partial": True}, overwrite=False)
            self.assertFalse(new_path.exists())
            self.assertEqual(list(Path(directory).glob(".*.tmp")), [])

    def test_relevant_environment_never_serializes_credentials_or_paths(self):
        with mock.patch.dict(
            bench.os.environ,
            {
                "ZIPP_NOJIT": "1",
                "ZIPP_NO_TIERC_PLANNED_APPEND_PROBE": "1",
                "ZIPP_API_TOKEN": "zipp-secret",
                "ZIPP_PRIVATEKEY": "123456789",
                "ZIPP_GITHUB_PAT": "123456",
                "ZIPP_PIN": "123456",
                "ZIPP_UNKNOWN_CONTROL": "1",
                "MIMALLOC_LICENSE": "424242",
                "NODE_AUTH_TOKEN": "node-secret",
                "DENO_AUTH_TOKENS": "example.test=deno-secret",
                "RUST_BACKTRACE": "full",
                "RUST_LOG": "zipp=debug",
                "RUSTUP_HOME": r"C:\\Users\\private\\.rustup",
                "UNRELATED_SECRET": "not-in-scope",
            },
            clear=True,
        ):
            self.assertEqual(
                bench.relevant_environment(),
                {
                    "DENO_AUTH_TOKENS": "<redacted>",
                    "MIMALLOC_LICENSE": "<redacted>",
                    "NODE_AUTH_TOKEN": "<redacted>",
                    "RUSTUP_HOME": "<redacted>",
                    "RUST_BACKTRACE": "full",
                    "RUST_LOG": "<redacted>",
                    "ZIPP_API_TOKEN": "<redacted>",
                    "ZIPP_GITHUB_PAT": "<redacted>",
                    "ZIPP_NOJIT": "1",
                    "ZIPP_NO_TIERC_PLANNED_APPEND_PROBE": "1",
                    "ZIPP_PIN": "<redacted>",
                    "ZIPP_PRIVATEKEY": "<redacted>",
                    "ZIPP_UNKNOWN_CONTROL": "<redacted>",
                },
            )

    def test_loader_and_runtime_controls_make_publication_noncanonical(self):
        with mock.patch.dict(
            bench.os.environ,
            {
                "PATH": "/ordinary/search/path",
                "LD_PRELOAD": "/tmp/injected.so",
                "LD_LIBRARY_PATH": "/tmp/libraries",
                "GLIBC_TUNABLES": "glibc.malloc.tcache_count=1",
                "UV_THREADPOOL_SIZE": "64",
            },
            clear=True,
        ):
            environment = bench.relevant_environment()

        self.assertNotIn("PATH", environment)
        self.assertEqual(environment["LD_PRELOAD"], "<redacted>")
        self.assertEqual(environment["LD_LIBRARY_PATH"], "<redacted>")
        self.assertEqual(environment["GLIBC_TUNABLES"], "<redacted>")
        self.assertEqual(environment["UV_THREADPOOL_SIZE"], "64")
        reasons = bench.publication_policy_reasons(
            is_ab=False,
            canonical_inputs=True,
            engine_names=list(bench.CANONICAL_ENGINE_NAMES),
            baseline="node",
            metric="cold",
            historical=False,
            reps=bench.MIN_PUBLISHABLE_REPS,
            bootstrap_samples=bench.BOOTSTRAP_SAMPLES,
            source_reason=None,
            environment=environment,
        )
        self.assertTrue(any("environment variables" in reason for reason in reasons))

    def test_publication_policy_fails_closed(self):
        self.assertEqual(
            bench.publication_policy_reasons(
                is_ab=False,
                canonical_inputs=True,
                engine_names=list(bench.CANONICAL_ENGINE_NAMES),
                baseline="node",
                metric="cold",
                historical=False,
                reps=bench.MIN_PUBLISHABLE_REPS,
                bootstrap_samples=bench.BOOTSTRAP_SAMPLES,
                source_reason=None,
                environment={},
            ),
            [],
        )
        reasons = bench.publication_policy_reasons(
            is_ab=True,
            canonical_inputs=False,
            engine_names=["node", "zipp"],
            baseline="bun",
            metric="adjusted",
            historical=True,
            reps=bench.MIN_PUBLISHABLE_REPS - 1,
            bootstrap_samples=bench.BOOTSTRAP_SAMPLES - 1,
            source_reason="harness differs from HEAD",
            environment={"ZIPP_NOJIT": "1"},
        )
        rendered = "\n".join(reasons)
        for expected in (
            "A/B comparison",
            "alternate or filtered",
            "engine table",
            "baseline",
            "headline metric",
            "historical report",
            "repetitions",
            "bootstrap samples",
            "harness differs from HEAD",
            "environment variables",
        ):
            self.assertIn(expected, rendered)

        self.assertTrue(
            bench.artifact_publishable(
                [], [], [], all_correct=True, publication_reasons=[]
            )
        )
        for provenance, engine_drift, source_drift, all_correct, policy in (
            (["dirty engine"], [], [], True, []),
            ([], ["engine drift"], [], True, []),
            ([], [], ["input drift"], True, []),
            ([], [], [], False, []),
            ([], [], [], True, ["too few repetitions"]),
        ):
            self.assertFalse(
                bench.artifact_publishable(
                    provenance,
                    engine_drift,
                    source_drift,
                    all_correct=all_correct,
                    publication_reasons=policy,
                )
            )

    def test_digest_drift_names_changed_inputs(self):
        self.assertEqual(
            bench.digest_mapping_drift(
                {"one.js": "before"},
                {"one.js": "after"},
                kind="benchmark input",
            ),
            ["benchmark input one.js changed during run (before -> after)"],
        )

    def test_git_publication_pathspecs_are_split_into_bounded_batches(self):
        paths = [f"directory-{'x' * 180}/{index:04d}.js" for index in range(257)]
        batches = list(bench._git_pathspec_batches(paths))

        self.assertGreater(len(batches), 1)
        self.assertEqual([item for batch in batches for item in batch], paths)
        for batch in batches:
            self.assertLessEqual(len(batch), bench._GIT_PATHSPEC_BATCH_MAX_ARGS)
            self.assertLessEqual(
                sum(len(item) + 3 for item in batch),
                bench._GIT_PATHSPEC_BATCH_MAX_CHARS,
            )

    def test_publication_paths_must_be_tracked_and_clean_against_head(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def git(*args):
                return subprocess.run(
                    ["git", *args],
                    cwd=root,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    check=True,
                )

            git("init", "--quiet")
            git("config", "user.email", "bench-test@example.invalid")
            git("config", "user.name", "Benchmark Test")
            tracked = root / "tracked.js"
            tracked.write_text("print(1);\n", encoding="utf-8")
            git("add", "tracked.js")
            git("commit", "--quiet", "-m", "fixture")

            self.assertEqual(
                bench.git_repository_matches_head(root=root),
                (True, None),
            )
            self.assertEqual(
                bench.git_paths_match_head([tracked], root=root),
                (True, None),
            )
            tracked.write_text("print(2);\n", encoding="utf-8")
            repository_clean, repository_reason = (
                bench.git_repository_matches_head(root=root)
            )
            self.assertFalse(repository_clean)
            self.assertIn("tracked changes", repository_reason)
            clean, reason = bench.git_paths_match_head([tracked], root=root)
            self.assertFalse(clean)
            self.assertIn("differ from HEAD", reason)

            tracked.write_text("print(1);\n", encoding="utf-8")
            git("update-index", "--assume-unchanged", "tracked.js")
            tracked.write_text("print(4);\n", encoding="utf-8")
            repository_clean, repository_reason = (
                bench.git_repository_matches_head(root=root)
            )
            self.assertFalse(repository_clean)
            self.assertIn("hidden", repository_reason)
            clean, reason = bench.git_paths_match_head([tracked], root=root)
            self.assertFalse(clean)
            self.assertIn("differ from HEAD", reason)

            git("update-index", "--no-assume-unchanged", "tracked.js")
            tracked.write_text("print(1);\n", encoding="utf-8")
            git("update-index", "--skip-worktree", "tracked.js")
            tracked.write_text("print(5);\n", encoding="utf-8")
            clean, reason = bench.git_paths_match_head([tracked], root=root)
            self.assertFalse(clean)
            self.assertIn("differ from HEAD", reason)

            untracked = root / "untracked.js"
            untracked.write_text("print(3);\n", encoding="utf-8")
            clean, reason = bench.git_paths_match_head([untracked], root=root)
            self.assertFalse(clean)
            self.assertIn("untracked", reason)

    def test_replay_publication_is_bound_to_current_head_harness_and_inputs(self):
        engines = ["node", "bun", "deno", "zipp"]
        source = "a" * 40
        harness = {"bench_py_sha256": "b" * 64, "run_real_sh_sha256": "c" * 64}
        inputs = {"one": "d" * 64}
        zipp_identity = {
            "name": "zipp",
            "source": source,
            "commit": source,
            "dirty": False,
        }
        replay = {
            "engine_names": engines,
            "engines_meta": [
                {
                    "name": name,
                    "sha256": "e" * 64,
                    "build_identity": zipp_identity if name == "zipp" else None,
                }
                for name in engines
            ],
            "workspace_source": source,
            "workspace_source_before": source,
            "workspace_source_after": source,
            "engine_source_before": {
                "node": None,
                "bun": None,
                "deno": None,
                "zipp": source,
            },
            "engine_source_after": {
                "node": None,
                "bun": None,
                "deno": None,
                "zipp": source,
            },
            "engine_binary_sha_before": {name: "e" * 64 for name in engines},
            "engine_binary_sha_after": {name: "e" * 64 for name in engines},
            "publication_sources_head_before": True,
            "publication_sources_head_after": True,
            "repository_head_before": True,
            "repository_head_after": True,
            "benchmark_environment_policy": (
                bench.canonical_benchmark_environment_descriptor()
            ),
            "benchmark_input_staging_policy": bench.BENCHMARK_INPUT_STAGING_POLICY,
            "harness_sha256_before": harness,
            "harness_sha256_after": harness,
            "bench_input_sha256_before": inputs,
            "bench_input_sha256_after": inputs,
        }
        with (
            mock.patch.object(bench, "discover_benches", return_value=["one"]),
            mock.patch.object(bench, "git_revision", return_value=source) as head,
            mock.patch.object(
                bench, "git_paths_match_head", return_value=(True, None)
            ) as matched_paths,
            mock.patch.object(
                bench, "git_repository_matches_head", return_value=(True, None)
            ),
            mock.patch.object(bench, "harness_digest", return_value=harness) as live_harness,
            mock.patch.object(bench, "bench_input_digests", return_value=inputs),
        ):
            self.assertEqual(
                bench.replay_current_source_reasons(replay, ["one"]), []
            )
            replay_paths = set(matched_paths.call_args.args[0])
            self.assertIn(PGO_TRAINING_PATH.resolve(), replay_paths)

            saved_policy = replay.pop("benchmark_environment_policy")
            self.assertIn(
                "child-environment policy",
                "\n".join(bench.replay_current_source_reasons(replay, ["one"])),
            )
            replay["benchmark_environment_policy"] = saved_policy

            saved_staging_policy = replay.pop("benchmark_input_staging_policy")
            self.assertIn(
                "input-staging policy",
                "\n".join(bench.replay_current_source_reasons(replay, ["one"])),
            )
            replay["benchmark_input_staging_policy"] = "noncanonical"
            self.assertIn(
                "input-staging policy",
                "\n".join(bench.replay_current_source_reasons(replay, ["one"])),
            )
            replay["benchmark_input_staging_policy"] = saved_staging_policy

            head.return_value = "f" * 40
            live_harness.return_value = {**harness, "bench_py_sha256": "0" * 64}
            rendered = "\n".join(
                bench.replay_current_source_reasons(replay, ["one"])
            )

        self.assertIn("current HEAD differs", rendered)
        self.assertIn("current harness bytes differ", rendered)

    def test_replay_rejects_a_contradictory_engine_metadata_envelope(self):
        engines = ["node", "bun", "deno", "zipp"]
        source = "a" * 40
        digest = "e" * 64
        sources = {name: source if name == "zipp" else None for name in engines}
        binaries = {name: digest for name in engines}
        replay = {
            "engine_names": engines,
            "engines_meta": [
                {
                    "name": name,
                    "sha256": "0" * 64 if name == "zipp" else digest,
                    "build_identity": {
                        "name": "zipp",
                        "source": source,
                        "commit": source,
                        "dirty": True,
                    }
                    if name == "zipp"
                    else None,
                }
                for name in engines
            ],
            "workspace_source": source,
            "workspace_source_before": source,
            "workspace_source_after": source,
            "engine_source_before": sources,
            "engine_source_after": sources,
            "engine_binary_sha_before": binaries,
            "engine_binary_sha_after": binaries,
            "publication_sources_head_before": True,
            "publication_sources_head_after": True,
            "repository_head_before": True,
            "repository_head_after": True,
            "benchmark_environment_policy": (
                bench.canonical_benchmark_environment_descriptor()
            ),
            "benchmark_input_staging_policy": bench.BENCHMARK_INPUT_STAGING_POLICY,
            "harness_sha256_before": {},
            "harness_sha256_after": {},
            "bench_input_sha256_before": {},
            "bench_input_sha256_after": {},
        }
        with (
            mock.patch.object(bench, "discover_benches", return_value=["one"]),
            mock.patch.object(bench, "git_revision", return_value=source),
            mock.patch.object(bench, "git_paths_match_head", return_value=(True, None)),
            mock.patch.object(
                bench, "git_repository_matches_head", return_value=(True, None)
            ),
            mock.patch.object(bench, "harness_digest", return_value={}),
            mock.patch.object(bench, "bench_input_digests", return_value={}),
        ):
            rendered = "\n".join(
                bench.replay_current_source_reasons(replay, ["one"])
            )

        self.assertIn("binary envelope", rendered)
        self.assertIn("DIRTY tree", rendered)

    def test_fresh_canonical_artifact_can_replay_readme_output(self):
        engines = list(bench.CANONICAL_ENGINE_NAMES)
        source = "a" * 40
        binary_digest = "e" * 64
        harness = {"bench_py_sha256": "b" * 64, "run_real_sh_sha256": "c" * 64}
        inputs = {"one": "d" * 64}
        recipe = bench.pgo_training_recipe_digest()
        self.assertIsNotNone(recipe)
        observations = [
            v2_observation(
                name,
                rep,
                startup=0.01,
                cold=0.10 + engines.index(name) * 0.01,
            )
            for rep in range(bench.MIN_PUBLISHABLE_REPS)
            for name in engines
        ]
        zipp_identity = engine_meta("zipp", commit=source)["build_identity"]
        zipp_identity["source"] = source
        sources = {name: source if name == "zipp" else None for name in engines}
        binaries = {name: binary_digest for name in engines}
        data = {
            "schema_version": bench.SCHEMA_VERSION,
            "reps": bench.MIN_PUBLISHABLE_REPS,
            "benches": ["one"],
            "baseline": "node",
            "engines": [
                {
                    "name": name,
                    "sha256": binary_digest,
                    "build_identity": zipp_identity if name == "zipp" else None,
                }
                for name in engines
            ],
            "observations": observations,
            "seed": 123,
            "bootstrap_samples": bench.BOOTSTRAP_SAMPLES,
            "headline_metric": "cold",
            "all_correct": True,
            "health_failures": [],
            "correctness_failures": [],
            "failures": [],
            "publishable": True,
            "provenance_reasons": [],
            "publication_reasons": [],
            "engine_drift": [],
            "source_drift": [],
            "workspace_source": source,
            "workspace_source_before": source,
            "workspace_source_after": source,
            "engine_source_before": sources,
            "engine_source_after": sources,
            "engine_binary_sha_before": binaries,
            "engine_binary_sha_after": binaries,
            "publication_sources_head_before": True,
            "publication_sources_head_after": True,
            "repository_head_before": True,
            "repository_head_after": True,
            "benchmark_environment_policy": (
                bench.canonical_benchmark_environment_descriptor()
            ),
            "benchmark_input_staging_policy": bench.BENCHMARK_INPUT_STAGING_POLICY,
            "harness_sha256_before": harness,
            "harness_sha256_after": harness,
            "bench_input_sha256_before": inputs,
            "bench_input_sha256_after": inputs,
        }
        self.assertTrue(
            bench.normalize_result_data(data)["publication_metadata_complete"]
        )
        missing_policy = dict(data)
        missing_policy.pop("benchmark_input_staging_policy")
        self.assertFalse(
            bench.normalize_result_data(missing_policy)[
                "publication_metadata_complete"
            ]
        )
        wrong_policy = dict(data)
        wrong_policy["benchmark_input_staging_policy"] = "legacy-live-inputs"
        normalized_wrong = bench.normalize_result_data(wrong_policy)
        self.assertFalse(normalized_wrong["publication_metadata_complete"])
        self.assertEqual(
            normalized_wrong["benchmark_input_staging_policy"],
            "legacy-live-inputs",
        )
        args = argparse.Namespace(
            reps=bench.MIN_PUBLISHABLE_REPS,
            benches=None,
            bench_dir="unused",
            json=None,
            read_json=None,
            overwrite_json=False,
            readme=True,
            metric=None,
            historical=False,
            seed=None,
            timeout=1.0,
            bootstrap_samples=None,
            zipp="unused",
            ab=None,
            ab_env=None,
            allow_aa=False,
            baseline="node",
            engines="node,bun,deno,zipp",
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fresh.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            args.read_json = str(path)
            with (
                mock.patch.object(bench, "parse_args", return_value=args),
                mock.patch.object(bench, "discover_benches", return_value=["one"]),
                mock.patch.object(bench, "git_revision", return_value=source),
                mock.patch.object(
                    bench, "git_paths_match_head", return_value=(True, None)
                ),
                mock.patch.object(
                    bench, "git_repository_matches_head", return_value=(True, None)
                ),
                mock.patch.object(bench, "harness_digest", return_value=harness),
                mock.patch.object(bench, "bench_input_digests", return_value=inputs),
                mock.patch.object(bench, "relevant_environment", return_value={}),
                mock.patch.object(
                    bench,
                    "canonical_recipe_source_for_identity",
                    side_effect=synthetic_recipe_source_resolver,
                ),
                mock.patch.object(
                    bench,
                    "print_modern_report",
                    return_value={"geomean_paired_ratio": 1.0},
                ) as report,
                contextlib.redirect_stdout(io.StringIO()),
            ):
                returncode = bench.main()

        self.assertEqual(returncode, 0)
        self.assertTrue(report.call_args.kwargs["readme"])

    def test_pgo_commands_write_results_under_ignored_target(self):
        script = (bench.ROOT / "tools" / "pgo.sh").read_text(encoding="utf-8")
        self.assertIn("--json target/bench-results/pgo-real-", script)
        self.assertIn("--json target/bench-results/pgo-hostile-", script)
        self.assertNotIn("--json bench/pgo-real-", script)
        self.assertIn("/target", (bench.ROOT / ".gitignore").read_text(encoding="utf-8"))

    def test_harness_digest_binds_input_staging_helper(self):
        digests = bench.harness_digest()
        self.assertEqual(
            digests["pgo_training_py_sha256"],
            bench.file_digest(PGO_TRAINING_PATH),
        )

    def test_modern_v2_replay_uses_stored_analysis_metadata(self):
        observations = [
            v2_observation("old", 0, startup=0.1, cold=1.0),
            v2_observation("new", 0, startup=0.1, cold=0.8),
        ]
        data = v2_result(observations, reps=1, metric="adjusted")
        args = argparse.Namespace(
            reps=15,
            benches=None,
            bench_dir="unused",
            json=None,
            read_json=None,
            overwrite_json=False,
            readme=False,
            metric=None,
            historical=False,
            seed=None,
            timeout=1.0,
            bootstrap_samples=None,
            zipp="unused",
            ab=None,
            ab_env=None,
            allow_aa=False,
            baseline="node",
            engines="node,zipp",
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "result.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            args.read_json = str(path)
            with (
                mock.patch.object(bench, "parse_args", return_value=args),
                mock.patch.object(
                    bench,
                    "print_modern_report",
                    return_value={"geomean_paired_ratio": 0.8},
                ) as report,
                contextlib.redirect_stdout(io.StringIO()),
            ):
                returncode = bench.main()

        self.assertEqual(returncode, 0)
        self.assertEqual(report.call_args.kwargs["metric"], "adjusted")
        self.assertEqual(report.call_args.kwargs["seed"], 123)
        self.assertEqual(report.call_args.kwargs["bootstrap_samples"], 77)

    def test_unpublishable_replay_refuses_readme_output(self):
        observations = [
            v2_observation("old", 0, startup=0.1, cold=1.0),
            v2_observation("new", 0, startup=0.1, cold=0.8),
        ]
        data = v2_result(observations, reps=1)
        data["publishable"] = False
        args = argparse.Namespace(
            reps=15,
            benches=None,
            bench_dir="unused",
            json=None,
            read_json=None,
            overwrite_json=False,
            readme=True,
            metric=None,
            historical=False,
            seed=None,
            timeout=1.0,
            bootstrap_samples=None,
            zipp="unused",
            ab=None,
            ab_env=None,
            allow_aa=False,
            baseline="node",
            engines="node,zipp",
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "diagnostic.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            args.read_json = str(path)
            with (
                mock.patch.object(bench, "parse_args", return_value=args),
                mock.patch.object(bench, "print_modern_report") as report,
            ):
                with self.assertRaisesRegex(SystemExit, "publishable:false"):
                    bench.main()
        report.assert_not_called()

    def test_legacy_publishable_replay_cannot_bypass_current_pgo_protocol(self):
        observations = [
            v2_observation("old", 0, startup=0.1, cold=1.0),
            v2_observation("new", 0, startup=0.1, cold=0.8),
        ]
        data = v2_result(observations, reps=1)
        data["publishable"] = True
        args = argparse.Namespace(
            reps=15,
            benches=None,
            bench_dir="unused",
            json=None,
            read_json=None,
            overwrite_json=False,
            readme=True,
            metric=None,
            historical=False,
            seed=None,
            timeout=1.0,
            bootstrap_samples=None,
            zipp="unused",
            ab=None,
            ab_env=None,
            allow_aa=False,
            baseline="node",
            engines="node,zipp",
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "legacy-publishable.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            args.read_json = str(path)
            with mock.patch.object(bench, "parse_args", return_value=args):
                with self.assertRaises(SystemExit) as raised:
                    bench.main()
        message = str(raised.exception)
        self.assertIn("publication provenance envelope", message)
        self.assertIn("PGO provenance", message)

    def test_failed_v2_replay_never_calls_summary_report(self):
        observations = [
            v2_observation(
                "old", 0, startup=0.1, cold=1.0, digest="11" * 32
            ),
            v2_observation(
                "new", 0, startup=0.1, cold=0.8, digest="22" * 32
            ),
        ]
        data = v2_result(observations, reps=1)
        args = argparse.Namespace(
            reps=15,
            benches=None,
            bench_dir="unused",
            json=None,
            read_json=None,
            overwrite_json=False,
            readme=False,
            metric=None,
            historical=False,
            seed=None,
            timeout=1.0,
            bootstrap_samples=None,
            zipp="unused",
            ab=None,
            ab_env=None,
            allow_aa=False,
            baseline="node",
            engines="node,zipp",
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "failed.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            args.read_json = str(path)
            output = io.StringIO()
            with (
                mock.patch.object(bench, "parse_args", return_value=args),
                mock.patch.object(bench, "print_modern_report") as report,
                contextlib.redirect_stdout(output),
            ):
                returncode = bench.main()

        self.assertEqual(returncode, 1)
        report.assert_not_called()
        self.assertIn("statistics unavailable", output.getvalue())

    def test_health_failure_aborts_stats_and_writes_failed_v2_result(self):
        def result(returncode=0, stdout=b"ok"):
            return {
                "elapsed_s": 0.1,
                "stdout": stdout,
                "stderr": b"bad" if returncode else b"",
                "returncode": returncode,
                "timed_out": False,
            }

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bench_dir = root / "benches"
            bench_dir.mkdir()
            (bench_dir / "one.js").write_text("print(1)\n", encoding="utf-8")
            json_path = root / "result.json"
            args = argparse.Namespace(
                reps=1,
                benches=None,
                bench_dir=str(bench_dir),
                allow_external_bench_dir=True,
                json=str(json_path),
                read_json=None,
                overwrite_json=False,
                readme=False,
                metric="cold",
                historical=False,
                seed=1,
                timeout=1.0,
                bootstrap_samples=25,
                zipp="unused",
                ab=["old", "new"],
                ab_env=None,
                allow_aa=False,
                baseline="node",
                engines="node,zipp",
            )
            side_effect = [
                result(returncode=1),
                result(),
                result(),
                result(),
            ]
            output = io.StringIO()
            with (
                mock.patch.object(bench, "parse_args", return_value=args),
                mock.patch.object(bench, "run_once", side_effect=side_effect),
                contextlib.redirect_stdout(output),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                returncode = bench.main()
            written = json.loads(json_path.read_text(encoding="utf-8"))

        self.assertEqual(returncode, 1)
        self.assertIn("statistics unavailable", output.getvalue())
        self.assertIn("ALL_CORRECT=0", output.getvalue())
        self.assertFalse(written["all_correct"])
        self.assertIsNone(written["summary"])
        self.assertFalse(written["publishable"])
        self.assertEqual(written["row_set_summaries"], {})
        self.assertFalse(written["observations"][0]["valid_for_stats"])
        self.assertTrue(written["observations"][1]["valid_for_stats"])

    def test_harness_drift_invalidates_measurement_and_summary(self):
        def result():
            return {
                "elapsed_s": 0.1,
                "stdout": b"ok",
                "stderr": b"",
                "returncode": 0,
                "timed_out": False,
            }

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bench_dir = root / "benches"
            bench_dir.mkdir()
            (bench_dir / "one.js").write_text("print(1)\n", encoding="utf-8")
            json_path = root / "result.json"
            args = argparse.Namespace(
                reps=1,
                benches=None,
                bench_dir=str(bench_dir),
                allow_external_bench_dir=True,
                json=str(json_path),
                read_json=None,
                overwrite_json=False,
                readme=False,
                metric="cold",
                historical=False,
                seed=1,
                timeout=1.0,
                bootstrap_samples=25,
                zipp="unused",
                ab=["old", "new"],
                ab_env=None,
                allow_aa=False,
                baseline="node",
                engines="node,zipp",
            )
            with (
                mock.patch.object(bench, "parse_args", return_value=args),
                mock.patch.object(bench, "run_once", side_effect=[result()] * 4),
                mock.patch.object(
                    bench,
                    "harness_digest",
                    side_effect=[
                        {"bench_py_sha256": "before"},
                        {"bench_py_sha256": "after"},
                    ],
                ),
                mock.patch.object(
                    bench, "git_paths_match_head", return_value=(True, None)
                ),
                mock.patch.object(
                    bench,
                    "git_repository_matches_head",
                    side_effect=[
                        (True, None),
                        (
                            False,
                            "repository worktree/index contains tracked changes or untracked files",
                        ),
                    ],
                ),
                mock.patch.object(bench, "print_modern_report") as report,
                contextlib.redirect_stdout(io.StringIO()),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                returncode = bench.main()
            written = json.loads(json_path.read_text(encoding="utf-8"))

        self.assertEqual(returncode, 1)
        report.assert_not_called()
        self.assertFalse(written["all_correct"])
        self.assertFalse(written["publishable"])
        self.assertIsNone(written["summary"])
        self.assertEqual(written["row_set_summaries"], {})
        self.assertIn("harness", "\n".join(written["source_drift"]))
        self.assertTrue(written["repository_head_before"])
        self.assertFalse(written["repository_head_after"])
        self.assertIn("repository worktree", "\n".join(written["publication_reasons"]))

    def test_live_unpublishable_readme_request_exits_nonzero(self):
        successful = {
            "elapsed_s": 0.1,
            "stdout": b"ok",
            "stderr": b"",
            "returncode": 0,
            "timed_out": False,
            "spawn_error": False,
        }
        with tempfile.TemporaryDirectory() as directory:
            bench_dir = Path(directory)
            (bench_dir / "one.js").write_text("print(1)\n", encoding="utf-8")
            args = argparse.Namespace(
                reps=1,
                benches=None,
                bench_dir=str(bench_dir),
                allow_external_bench_dir=True,
                json=None,
                read_json=None,
                overwrite_json=False,
                readme=True,
                metric="cold",
                historical=False,
                seed=1,
                timeout=1.0,
                bootstrap_samples=25,
                zipp="unused",
                ab=["old", "new"],
                ab_env=None,
                allow_aa=False,
                baseline="node",
                engines="node,zipp",
                allow_dirty_engine=False,
                allow_nonhead_engine=False,
            )
            with (
                mock.patch.object(bench, "parse_args", return_value=args),
                mock.patch.object(bench, "run_once", side_effect=[successful] * 4),
                mock.patch.object(
                    bench,
                    "print_modern_report",
                    return_value={"geomean_paired_ratio": 1.0},
                ) as report,
                contextlib.redirect_stdout(io.StringIO()),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                returncode = bench.main()

        self.assertEqual(returncode, 1)
        self.assertFalse(report.call_args.kwargs["readme"])


class AbBinaryGuardTests(unittest.TestCase):
    """`--ab` must refuse two byte-identical executables.

    The failure this prevents is silent: a `git stash`/rebuild cycle that forgets
    the final rebuild leaves both sides on one binary, every gate "passes"
    because it compares a build to itself, and only a ratio that fails to move
    gives it away. See PERF_ROADMAP B61.
    """

    @staticmethod
    def _write(path: Path, body: bytes) -> str:
        path.write_bytes(body)
        return str(path)

    def test_identical_binaries_abort_before_measuring(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            a = self._write(root / "a.exe", b"same-bytes")
            b = self._write(root / "b.exe", b"same-bytes")
            with self.assertRaises(SystemExit) as caught:
                bench.reject_identical_ab_binaries([a, b], ({}, {}), allow=False)
            message = str(caught.exception)
            self.assertIn("same executable", message)
            self.assertIn("--allow-aa", message)

    def test_same_path_twice_aborts(self):
        # The literal shape of the mistake: one path passed for both sides.
        with tempfile.TemporaryDirectory() as directory:
            exe = self._write(Path(directory) / "zipp.exe", b"one")
            with self.assertRaises(SystemExit):
                bench.reject_identical_ab_binaries([exe, exe], ({}, {}), allow=False)

    def test_allow_aa_permits_a_deliberate_calibration(self):
        with tempfile.TemporaryDirectory() as directory:
            exe = self._write(Path(directory) / "zipp.exe", b"one")
            bench.reject_identical_ab_binaries([exe, exe], ({}, {}), allow=True)

    def test_differing_ab_env_permits_one_binary(self):
        # The ablation-pricing idiom: same binary, behaviour switched by env, so
        # the two sides are genuinely different runs.
        with tempfile.TemporaryDirectory() as directory:
            exe = self._write(Path(directory) / "zipp.exe", b"one")
            bench.reject_identical_ab_binaries(
                [exe, exe], ({}, {"ZIPP_NOJIT": "1"}), allow=False
            )

    def test_different_binaries_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            a = self._write(root / "a.exe", b"old-build")
            b = self._write(root / "b.exe", b"new-build")
            bench.reject_identical_ab_binaries([a, b], ({}, {}), allow=False)

    def test_unresolvable_executable_does_not_abort(self):
        # Not this guard's job to report a missing file; the run fails later with
        # a clearer error. Guarding here would turn a typo into a confusing
        # "same executable" message.
        bench.reject_identical_ab_binaries(
            ["definitely-not-on-path-xyz", "also-not-here-xyz"], ({}, {}), allow=False
        )


def engine_meta(name, *, commit, dirty=False, diff="d1", sha="ab" * 32):
    """A synthetic `engine_metadata` row.

    Real fake-engine executables would have to be `.exe`/`.bat` on Windows and
    plain scripts elsewhere; the decision logic under test never touches the
    filesystem, so the probe results are injected directly and `engine_metadata`
    itself is exercised through `main()` with a mock.
    """
    identity = {
        "name": "zipp",
        "commit": commit,
        "dirty": dirty,
        "rustc": "rustc synthetic -vV identity",
        "target": bench.PGO_CANONICAL_TARGET,
        "profile": "release",
        "opt_level": "3",
        "features": "",
        "jit": True,
        "rustflags": bench.PGO_CANONICAL_RUSTFLAGS,
        "rustflags_source": "CARGO_ENCODED_RUSTFLAGS",
        "pgo_profile_sha256": "12" * 32,
        "pgo_training_recipe_sha256": bench.pgo_training_recipe_digest()
        or "34" * 32,
        "pgo_build_contract": bench.PGO_BUILD_CONTRACT,
        "pgo_build_environment_policy": bench.PGO_BUILD_ENVIRONMENT_POLICY,
        "pgo_build_environment_sha256": "45" * 32,
        "pgo_cargo_identity": "cargo synthetic identity",
        "pgo_cargo_sha256": "56" * 32,
        "pgo_rustc_sha256": "67" * 32,
        "pgo_linker_identity": "rust-lld synthetic identity",
        "pgo_linker_sha256": "78" * 32,
        "pgo_msvc_cl_identity": "Microsoft C/C++ synthetic identity",
        "pgo_msvc_cl_sha256": "9a" * 32,
        "pgo_msvc_lib_identity": "Microsoft Library Manager synthetic identity",
        "pgo_msvc_lib_sha256": "ab" * 32,
        "pgo_source_snapshot_sha256": "bc" * 32,
    }
    identity["pgo_build_recipe_sha256"] = (
        bench.pgo_build_recipe_digest(identity) or "89" * 32
    )
    if dirty:
        identity["diff_digest"] = diff
    return {"name": name, "sha256": sha, "build_identity": identity}


class WorkingTreeRecipeSource:
    """Duck-typed recipe source for synthetic provenance unit identities."""

    def __init__(self, root: Path) -> None:
        self.root = root

    def entries(self):
        listed = subprocess.run(
            ["git", "-C", str(self.root), "ls-files", "-z"],
            check=True,
            stdout=subprocess.PIPE,
        ).stdout
        return {
            os.fsdecode(name).replace("\\", "/"): ("100644", "blob", "synthetic")
            for name in listed.split(b"\0")
            if name
        }

    def read_bytes(self, relative):
        path = self.root / Path(relative)
        return path.read_bytes() if path.is_file() else None

    def digest(self, relative):
        contents = self.read_bytes(relative)
        return hashlib.sha256(contents).hexdigest() if contents is not None else None

    def snapshot_digest(self):
        return "bc" * 32


def synthetic_recipe_source_resolver(_identity):
    return WorkingTreeRecipeSource(bench.ROOT), None


class ProvenanceTests(unittest.TestCase):
    """The artifact must not be able to name a commit it did not measure.

    `bench/head_clean_2a616f5.json` is named for 2a616f5 and its engine reports
    `cdda4e8 + dirty:true`. The harness recorded the workspace HEAD and the
    engine's build identity from two independent sources, after measurement, and
    never compared them.
    """

    HEAD = "a" * 40
    OTHER = "b" * 40

    def test_pgo_build_contract_pins_the_windows_main_stack(self):
        self.assertIn(
            ";pe-stack=reserve-268435456,commit-4096;",
            bench.PGO_BUILD_CONTRACT,
        )
        build_rs = (bench.ROOT / "crates/zipp-cli/build.rs").read_text(
            encoding="utf-8"
        )
        pgo_sh = (bench.ROOT / "tools/pgo.sh").read_text(encoding="utf-8")
        build_match = re.search(
            r'^const PGO_BUILD_CONTRACT: &str = "([^"]+)";$',
            build_rs,
            re.MULTILINE,
        )
        pgo_match = re.search(
            r"^PGO_BUILD_CONTRACT='([^']+)'$",
            pgo_sh,
            re.MULTILINE,
        )
        self.assertIsNotNone(build_match)
        self.assertIsNotNone(pgo_match)
        self.assertEqual(build_match.group(1), bench.PGO_BUILD_CONTRACT)
        self.assertEqual(pgo_match.group(1), bench.PGO_BUILD_CONTRACT)

    def test_source_identity_distinguishes_dirty_from_its_parent(self):
        clean = {"commit": self.HEAD, "dirty": False}
        dirty = {"commit": self.HEAD, "dirty": True, "diff_digest": "beef"}
        self.assertEqual(bench.source_identity(clean), self.HEAD)
        self.assertEqual(bench.source_identity(dirty), f"{self.HEAD}+dirty.beef")
        self.assertNotEqual(
            bench.source_identity(clean), bench.source_identity(dirty)
        )
        self.assertIsNone(bench.source_identity(None))
        self.assertIsNone(bench.source_identity({"dirty": False}))

    def test_clean_head_engine_has_no_reasons(self):
        reasons = bench.check_engine_provenance(
            [engine_meta("zipp", commit=self.HEAD)],
            self.HEAD,
            is_ab=False,
            allow_dirty=False,
            allow_nonhead=False,
        )
        self.assertEqual(reasons, [])

    def test_pgo_identity_is_fail_closed_for_publication(self):
        metadata = engine_meta("zipp", commit=self.HEAD)
        identity = metadata["build_identity"]
        identity["rustflags_source"] = "RUSTFLAGS"
        identity["pgo_profile_sha256"] = ""
        identity["pgo_training_recipe_sha256"] = "not-a-digest"
        reasons = bench.pgo_build_reasons(
            [metadata],
            require_pgo=True,
            source_resolver=synthetic_recipe_source_resolver,
        )
        rendered = "\n".join(reasons)
        self.assertIn("CARGO_ENCODED_RUSTFLAGS", rendered)
        self.assertIn("profile hash", rendered)
        self.assertIn("training recipe hash", rendered)

        identity["rustflags"] = ""
        reasons = bench.pgo_build_reasons(
            [metadata],
            require_pgo=True,
            source_resolver=synthetic_recipe_source_resolver,
        )
        self.assertTrue(any("without profile-use" in reason for reason in reasons))

        identity["pgo_profile_sha256"] = ""
        identity["pgo_training_recipe_sha256"] = ""
        reasons = bench.pgo_build_reasons(
            [metadata],
            require_pgo=True,
            source_resolver=synthetic_recipe_source_resolver,
        )
        self.assertTrue(any("without profile-use" in reason for reason in reasons))

    def test_pgo_recipe_must_match_current_source_disjoint_recipe(self):
        metadata = engine_meta("zipp", commit=self.HEAD)
        expected = "ab" * 32
        metadata["build_identity"]["pgo_training_recipe_sha256"] = "cd" * 32
        with mock.patch.object(
            bench, "pgo_training_recipe_digest", return_value=expected
        ):
            reasons = bench.pgo_build_reasons(
                [metadata],
                require_pgo=True,
                source_resolver=synthetic_recipe_source_resolver,
            )
            self.assertTrue(any("does not match" in reason for reason in reasons))

            metadata["build_identity"]["pgo_training_recipe_sha256"] = expected
            reasons = bench.pgo_build_reasons(
                [metadata],
                require_pgo=True,
                source_resolver=synthetic_recipe_source_resolver,
            )
            self.assertFalse(any("training recipe" in reason for reason in reasons))

    def test_pgo_publication_rejects_noncanonical_build_envelope(self):
        metadata = engine_meta("zipp", commit=self.HEAD)
        identity = metadata["build_identity"]
        self.assertEqual(
            bench.pgo_build_reasons(
                [metadata],
                require_pgo=True,
                source_resolver=synthetic_recipe_source_resolver,
            ),
            [],
        )
        baseline = dict(identity)
        for field, bad_value, expected in (
            ("target", "x86_64-unknown-linux-gnu", "target"),
            ("profile", "debug", "Cargo profile"),
            ("opt_level", "2", "optimization level"),
            ("features", "secure-allocator", "feature set"),
            ("jit", False, "enable the JIT"),
            (
                "rustflags",
                bench.PGO_CANONICAL_RUSTFLAGS + " -Ctarget-cpu=native",
                "rustflags",
            ),
            ("pgo_build_contract", "self-asserted", "build contract"),
            (
                "pgo_build_environment_policy",
                "inherit-everything",
                "environment policy",
            ),
        ):
            identity[field] = bad_value
            rendered = "\n".join(
                bench.pgo_build_reasons(
                    [metadata],
                    require_pgo=True,
                    source_resolver=synthetic_recipe_source_resolver,
                )
            )
            self.assertIn(expected, rendered, field)
            identity.clear()
            identity.update(baseline)

        identity["pgo_cargo_identity"] = "different cargo"
        rendered = "\n".join(
            bench.pgo_build_reasons(
                [metadata],
                require_pgo=True,
                source_resolver=synthetic_recipe_source_resolver,
            )
        )
        self.assertIn("build recipe hash", rendered)
        identity.clear()
        identity.update(baseline)
        for field in (
            "pgo_msvc_cl_identity",
            "pgo_msvc_cl_sha256",
            "pgo_msvc_lib_identity",
            "pgo_msvc_lib_sha256",
            "pgo_source_snapshot_sha256",
        ):
            identity[field] = "different" if field.endswith("identity") else "cd" * 32
            rendered = "\n".join(
                bench.pgo_build_reasons(
                    [metadata],
                    require_pgo=True,
                    source_resolver=synthetic_recipe_source_resolver,
                )
            )
            self.assertIn("build recipe hash", rendered, field)
            identity.clear()
            identity.update(baseline)

    def test_pgo_recipe_calculator_commits_ordered_names_and_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tools").mkdir()
            (root / "tools" / "pgo.sh").write_bytes(b"recipe-script")
            (root / bench.PGO_CORPUS_VALIDATOR).write_bytes(b"corpus-validator")
            (root / bench.PGO_TRAINING_RUNNER).write_bytes(b"training-runner")
            for index, relative in enumerate(bench.PGO_TRAINING_INPUTS):
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(f"input-{index}".encode())
            expected_output_bytes = b'{"schema_version":1,"cases":[]}'
            (root / bench.PGO_EXPECTED_OUTPUT_MANIFEST).write_bytes(
                expected_output_bytes
            )
            real_input = root / "bench" / "real" / "one.js"
            real_input.parent.mkdir(parents=True)
            real_input.write_bytes(b"real-input")
            common_input = root / "bench" / "common.cjs"
            common_input.write_bytes(b"common-input")
            hostile_input = root / "bench" / "hostile" / "input.js"
            hostile_input.parent.mkdir(parents=True)
            hostile_input.write_bytes(b"hostile-input")
            manifest_bytes = json.dumps(
                {
                    "schema_version": 1,
                    "cases": [
                        {
                            "id": "one",
                            "entry": "input.js",
                            "category": "test",
                        }
                    ],
                },
                sort_keys=True,
            ).encode()
            (root / "bench" / "hostile" / "manifest.json").write_bytes(
                manifest_bytes
            )
            subprocess.run(
                ["git", "init", "--quiet"], cwd=root, check=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE
            )
            subprocess.run(
                ["git", "add", "bench"], cwd=root, check=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE
            )

            expected = hashlib.sha256()
            for value in (
                bench.PGO_RECIPE_VERSION,
                bench.PGO_RECIPE_COMMAND,
                "tools/pgo.sh",
                hashlib.sha256(b"recipe-script").hexdigest(),
                bench.PGO_SIMILARITY_POLICY,
                bench.PGO_CORPUS_VALIDATOR,
                hashlib.sha256(b"corpus-validator").hexdigest(),
                bench.PGO_RUNNER_POLICY,
                bench.PGO_TRAINING_RUNNER,
                hashlib.sha256(b"training-runner").hexdigest(),
                bench.PGO_EXPECTED_OUTPUT_MANIFEST,
                hashlib.sha256(expected_output_bytes).hexdigest(),
            ):
                expected.update(value.encode())
                expected.update(b"\0")
            for index, relative in enumerate(bench.PGO_TRAINING_INPUTS):
                for value in (
                    relative,
                    hashlib.sha256(f"input-{index}".encode()).hexdigest(),
                ):
                    expected.update(value.encode())
                    expected.update(b"\0")
            for value in (
                "bench/hostile/manifest.json",
                hashlib.sha256(manifest_bytes).hexdigest(),
                bench.PGO_EXCLUDED_INPUTS_LABEL,
                "bench/common.cjs",
                hashlib.sha256(b"common-input").hexdigest(),
                "bench/hostile/input.js",
                hashlib.sha256(b"hostile-input").hexdigest(),
                "bench/real/one.js",
                hashlib.sha256(b"real-input").hexdigest(),
            ):
                expected.update(value.encode())
                expected.update(b"\0")

            self.assertEqual(
                bench.pgo_training_recipe_digest(root=root), expected.hexdigest()
            )

    def test_clean_crlf_checkout_uses_lf_git_blob_recipe_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = base / "source"
            clone = base / "private-clone"
            windows_checkout = base / "windows-checkout"
            root.mkdir()
            subprocess.run(
                ["git", "init", "--quiet"], cwd=root, check=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE
            )
            (root / ".gitattributes").write_bytes(
                b"*.sh text eol=lf\nbench/pgo-training/** text eol=lf\n"
            )
            (root / "tools").mkdir()
            (root / "tools" / "pgo.sh").write_bytes(b"recipe\nscript\n")
            (root / bench.PGO_CORPUS_VALIDATOR).write_bytes(b"validator\n")
            (root / bench.PGO_TRAINING_RUNNER).write_bytes(b"runner\n")
            for index, relative in enumerate(bench.PGO_TRAINING_INPUTS):
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(f"input-{index}\n".encode())
            (root / bench.PGO_EXPECTED_OUTPUT_MANIFEST).write_bytes(
                b'{"schema_version":1,"cases":[]}\n'
            )
            hostile = root / "bench" / "hostile"
            hostile.mkdir(parents=True, exist_ok=True)
            (hostile / "input.js").write_bytes(b"print(1);\n")
            (hostile / "manifest.json").write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "cases": [
                            {
                                "id": "one",
                                "entry": "input.js",
                                "category": "test",
                            }
                        ],
                    },
                    sort_keys=True,
                )
                + "\n",
                encoding="utf-8",
                newline="\n",
            )
            subprocess.run(
                ["git", "add", "."], cwd=root, check=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE
            )
            subprocess.run(
                [
                    "git",
                    "-c",
                    "user.name=PGO Test",
                    "-c",
                    "user.email=pgo@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "fixture",
                ],
                cwd=root,
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            commit = subprocess.run(
                ["git", "rev-parse", "HEAD"], cwd=root, check=True,
                stdout=subprocess.PIPE, text=True
            ).stdout.strip()
            subprocess.run(
                [
                    "git",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "clone",
                    "--quiet",
                    "--no-hardlinks",
                    "--no-checkout",
                    "--",
                    str(root),
                    str(clone),
                ],
                check=True,
            )
            subprocess.run(
                [
                    "git",
                    "-C",
                    str(clone),
                    "-c",
                    "core.autocrlf=false",
                    "checkout",
                    "--quiet",
                    "--detach",
                    "--force",
                    commit,
                ],
                check=True,
            )
            canonical_recipe = bench.pgo_training_recipe_digest(root=clone)
            self.assertIsNotNone(canonical_recipe)

            # This is a legitimate clean Windows checkout: Git's clean filter
            # maps CRLF back to the exact LF blob, while raw-byte hashing differs.
            subprocess.run(
                [
                    "git",
                    "-c",
                    "core.autocrlf=true",
                    "clone",
                    "--quiet",
                    "--no-hardlinks",
                    "--",
                    str(root),
                    str(windows_checkout),
                ],
                check=True,
            )
            subprocess.run(
                ["git", "config", "core.autocrlf", "true"],
                cwd=windows_checkout,
                check=True,
            )
            validator = windows_checkout / bench.PGO_CORPUS_VALIDATOR
            self.assertIn(b"\r\n", validator.read_bytes())
            status = subprocess.run(
                ["git", "status", "--porcelain=v1"],
                cwd=windows_checkout,
                check=True,
                stdout=subprocess.PIPE
            ).stdout
            work_oid = subprocess.run(
                [
                    "git",
                    "hash-object",
                    f"--path={bench.PGO_CORPUS_VALIDATOR}",
                    "--",
                    bench.PGO_CORPUS_VALIDATOR,
                ],
                cwd=windows_checkout,
                check=True,
                stdout=subprocess.PIPE,
            ).stdout.strip()
            head_oid = subprocess.run(
                ["git", "rev-parse", f"HEAD:{bench.PGO_CORPUS_VALIDATOR}"],
                cwd=windows_checkout,
                check=True,
                stdout=subprocess.PIPE,
            ).stdout.strip()
            self.assertEqual(work_oid, head_oid)
            self.assertEqual(status, b"")
            source, reason = bench.canonical_recipe_source_for_identity(
                {"commit": commit, "dirty": False}, root=windows_checkout
            )
            self.assertIsNone(reason)
            self.assertIsNotNone(source)
            assert source is not None
            self.assertNotEqual(
                bench.file_digest(validator),
                source.digest(bench.PGO_CORPUS_VALIDATOR),
            )
            self.assertNotEqual(
                bench.pgo_training_recipe_digest(root=windows_checkout),
                canonical_recipe,
            )
            self.assertEqual(
                bench.pgo_training_recipe_digest(
                    root=windows_checkout, source=source
                ),
                canonical_recipe,
            )
            self.assertEqual(
                source.snapshot_digest(),
                bench.GitCommitRecipeSource(clone, commit).snapshot_digest(),
            )

    def test_pgo_script_and_independent_recipe_use_the_same_training_inputs(self):
        script = BENCH_PATH.with_name("pgo.sh").read_text(encoding="utf-8")
        runner = PGO_TRAINING_PATH.read_text(encoding="utf-8")

        def shell_array(name):
            lines = script.splitlines()
            start = lines.index(f"{name}=(") + 1
            values = []
            for line in lines[start:]:
                value = line.strip()
                if value == ")":
                    return tuple(values)
                if value and not value.startswith("#"):
                    values.append(value.strip("'\""))
            self.fail(f"unterminated {name} in tools/pgo.sh")

        declared = shell_array("PGO_INPUTS")
        self.assertEqual(declared, bench.PGO_TRAINING_INPUTS)
        self.assertEqual(len(declared), 7)
        self.assertEqual(len(declared), len(set(declared)))
        self.assertEqual(
            declared.count("bench/pgo-training/dictionary-mix.js"), 1
        )
        for relative in (
            "bench/pgo-training/csv-tuple-mix.js",
            "bench/pgo-training/template-uri-mix.js",
            "bench/pgo-training/async-dag-mix.js",
        ):
            self.assertEqual(declared.count(relative), 1)
        self.assertNotIn("bench/pgo-training/text-pipelines-mix.js", declared)
        self.assertNotIn("bench/pgo-training/async-mix.js", declared)
        self.assertIn(f"PGO_RECIPE_VERSION='{bench.PGO_RECIPE_VERSION}'", script)
        self.assertIn(f"PGO_RECIPE_COMMAND='{bench.PGO_RECIPE_COMMAND}'", script)
        self.assertIn("printf '%s\\0' \"$PGO_RECIPE_VERSION\"", script)
        self.assertIn("printf '%s\\0' \"$PGO_RECIPE_COMMAND\"", script)
        self.assertIn(
            f"printf '%s\\0' '{bench.PGO_EXCLUDED_INPUTS_LABEL}'", script
        )
        self.assertIn(
            f"PGO_SIMILARITY_POLICY='{bench.PGO_SIMILARITY_POLICY}'", script
        )
        self.assertIn(f"PGO_RUNNER_POLICY='{bench.PGO_RUNNER_POLICY}'", script)
        self.assertIn(f"CORPUS_VALIDATOR={bench.PGO_CORPUS_VALIDATOR}", script)
        self.assertIn(f"TRAINING_RUNNER={bench.PGO_TRAINING_RUNNER}", script)
        self.assertIn(
            f"EXPECTED_OUTPUT_MANIFEST={bench.PGO_EXPECTED_OUTPUT_MANIFEST}", script
        )
        self.assertIn('printf \'%s\\0\' "$PGO_SIMILARITY_POLICY"', script)
        self.assertIn('printf \'%s\\0\' "$PGO_RUNNER_POLICY"', script)
        self.assertIn('(str(binary), "js", "--pgo-training", str(staged))', runner)
        self.assertIn("verify-profiles", script)
        self.assertNotIn('merge -o "$PROFDATA" "$PGODIR"', script)
        self.assertIn("clone --quiet --no-hardlinks --no-checkout", script)
        self.assertIn('ROOT="$SOURCE_ROOT"', script)
        self.assertIn('--manifest-path "$ROOT/Cargo.toml"', script)
        self.assertNotIn('--manifest-path "$CHECKOUT_ROOT/Cargo.toml"', script)
        self.assertIn('SOURCE_SNAPSHOT_SHA256="$(repository_snapshot_sha256 "$ROOT")"', script)
        self.assertIn('PROFILE_SNAPSHOT_DIR="$CHECKOUT_ROOT/target/pgo-profiles"', script)
        self.assertIn('verify_source_and_tools_unchanged\nmkdir -p "$(dirname "$PUBLISHED_BIN")"', script)
        for target_variable in (
            "CC_x86_64_pc_windows_msvc",
            "CXX_x86_64_pc_windows_msvc",
            "AR_x86_64_pc_windows_msvc",
        ):
            self.assertIn(target_variable, script)
        for tool_label in (
            "msvc-cl-identity",
            "msvc-cl-sha256",
            "msvc-lib-identity",
            "msvc-lib-sha256",
            "source-snapshot-sha256",
        ):
            self.assertIn(tool_label, script)

    def test_pgo_output_manifest_is_exact_and_complete(self):
        cases = pgo_training.load_expected_manifest(
            bench.ROOT / bench.PGO_EXPECTED_OUTPUT_MANIFEST
        )
        self.assertEqual(tuple(case.path for case in cases), bench.PGO_TRAINING_INPUTS)
        self.assertTrue(all(case.stdout for case in cases))
        self.assertTrue(all(case.stderr == b"" for case in cases))
        self.assertEqual(pgo_training.POLICY_ID, bench.PGO_RUNNER_POLICY)
        for legacy in ("fib", "loop", "array", "string", "object", "sort"):
            self.assertNotIn(f"bench/{legacy}.js", bench.PGO_TRAINING_INPUTS)
            self.assertNotIn(
                f"bench/pgo-training/{legacy}-micro.js", bench.PGO_TRAINING_INPUTS
            )

    def test_pgo_output_manifest_rejects_duplicate_keys_and_boolean_schema(self):
        valid_case = '{"path":"bench/train.js","stdout":"ok\\n","stderr":""}'
        invalid_manifests = {
            "duplicate root key": (
                '{"schema_version":1,"schema_version":1,"cases":['
                + valid_case
                + "]}"
            ),
            "duplicate case key": (
                '{"schema_version":1,"cases":[{"path":"bench/train.js",'
                '"path":"bench/other.js","stdout":"ok\\n","stderr":""}]}'
            ),
            "boolean schema": (
                '{"schema_version":true,"cases":[' + valid_case + "]}"
            ),
        }
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "expected.json"
            for label, content in invalid_manifests.items():
                with self.subTest(label=label):
                    manifest.write_text(content, encoding="ascii")
                    with self.assertRaises(pgo_training.TrainingError):
                        pgo_training.load_expected_manifest(manifest)

    def test_pgo_runner_enforces_timeout_and_output_cap(self):
        with mock.patch.object(pgo_training, "TIMEOUT_SECONDS", 0.05):
            result = pgo_training.run_bounded(
                (sys.executable, "-c", "import time; time.sleep(2)"),
                environment=dict(os.environ),
            )
        self.assertTrue(result.timed_out)

        result = pgo_training.run_bounded(
            (sys.executable, "-c", "import sys; sys.stdout.write('x' * 9000)"),
            environment=dict(os.environ),
        )
        self.assertTrue(result.output_exceeded)
        self.assertGreater(len(result.stdout) + len(result.stderr), 8192)

    def test_pgo_runner_stages_immutable_bytes_and_refuses_symlink_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            root = base / "root"
            root.mkdir()
            source = root / "bench/train.js"
            source.parent.mkdir()
            source.write_bytes(b"console.log(1);\n")
            destination = base / "stage"
            destination.mkdir()
            pgo_training.stage_files(
                root=root, destination=destination, paths=("bench/train.js",)
            )
            source.write_bytes(b"console.log(2);\n")
            self.assertEqual(
                (destination / "bench/train.js").read_bytes(), b"console.log(1);\n"
            )
            pgo_training.remove_plain_tree(destination)
            self.assertFalse(destination.exists())

            victim = base / "victim"
            victim.mkdir()
            marker = victim / "keep.txt"
            marker.write_text("keep", encoding="ascii")
            link = base / "linked-stage"
            try:
                link.symlink_to(victim, target_is_directory=True)
            except OSError as exc:
                self.skipTest(f"directory symlinks unavailable: {exc}")
            with self.assertRaises(pgo_training.TrainingError):
                pgo_training.remove_plain_tree(link)
            self.assertEqual(marker.read_text(encoding="ascii"), "keep")

    def test_benchmark_input_stage_survives_transient_live_edit_restore(self):
        with tempfile.TemporaryDirectory() as directory:
            live = Path(directory) / "row.js"
            original = b"console.log('original');\n"
            live.write_bytes(original)
            stage = bench.ImmutableInputStage(
                {"inputs/row.js": live}, prefix="zipp-stage-test-"
            )
            staged = stage.path("inputs/row.js")
            try:
                live.write_bytes(b"console.log('transient');\n")
                self.assertEqual(staged.read_bytes(), original)
                live.write_bytes(original)
                self.assertEqual(file_digest := bench.file_digest(staged), bench.file_digest(live))
                self.assertIsNotNone(file_digest)
            finally:
                stage.close()
            self.assertFalse(stage.root.exists())

    def test_pgo_atomic_publish_rejects_redirected_parent(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            source = base / "source.bin"
            source.write_bytes(b"trusted")
            victim = base / "victim"
            victim.mkdir()
            link = base / "publish-parent"
            try:
                link.symlink_to(victim, target_is_directory=True)
            except OSError as exc:
                self.skipTest(f"directory symlinks unavailable: {exc}")
            with self.assertRaises(pgo_training.TrainingError):
                pgo_training.publish_atomic(
                    source=source,
                    destination=link / "published.bin",
                    readonly=False,
                    reuse_identical=False,
                )
            self.assertFalse((victim / "published.bin").exists())

    def test_pgo_runner_hashes_and_enumerates_one_profile_per_input(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            inputs = ("bench/one.js", "bench/two.js")
            for relative in inputs:
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"console.log('ok');\n")
            manifest = root / "expected.json"
            manifest.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "cases": [
                            {"path": path, "stdout": "ok\n", "stderr": ""}
                            for path in inputs
                        ],
                    }
                ),
                encoding="utf-8",
            )
            binary = root / "zipp"
            binary.write_bytes(b"placeholder")
            profile_dir = root / "profiles"
            profile_dir.mkdir()
            profile_list = root / "profile-list"
            invocation = 0

            def fake_run(_command, *, environment):
                nonlocal invocation
                name = f"input-{invocation:02d}-123-main.profraw"
                invocation += 1
                (profile_dir / name).write_bytes(name.encode("ascii"))
                self.assertIn("LLVM_PROFILE_FILE", environment)
                return pgo_training.ProcessResult(0, b"ok\n", b"", False, False)

            with mock.patch.object(pgo_training, "run_bounded", side_effect=fake_run):
                pgo_training.run_training(
                    root=root,
                    binary=binary,
                    manifest=manifest,
                    profile_dir=profile_dir,
                    profile_list=profile_list,
                    inputs=inputs,
                )
            verified = pgo_training.verify_profile_list(
                profile_dir=profile_dir, profile_list=profile_list
            )
            self.assertEqual(len(verified), len(inputs))
            (profile_dir / "extra.profraw").write_bytes(b"injected")
            with self.assertRaisesRegex(pgo_training.TrainingError, "differs"):
                pgo_training.verify_profile_list(
                    profile_dir=profile_dir, profile_list=profile_list
                )

    def test_replacement_training_inputs_are_bounded_and_self_contained(self):
        specifications = {
            "bench/pgo-training/async-dag-mix.js": (
                8_000,
                ((r"var SHARDS = (\d+);", 256), (r"var WIDTH = (\d+);", 128)),
                'var EXPECTED = "async-dag=192:92:2743580189";',
            ),
            "bench/pgo-training/csv-tuple-mix.js": (
                8_000,
                (
                    (r"var ROWS = (\d+);", 20_000),
                    (r"var COLUMNS = (\d+);", 8),
                    (r"pass < (\d+)", 5),
                ),
                'var EXPECTED = "csv-tuples=960995:70000:876997:158174722";',
            ),
            "bench/pgo-training/template-uri-mix.js": (
                8_000,
                ((r"var REQUESTS = (\d+);", 50_000),),
                'var EXPECTED = "template-uri=36000:1810247:106620:2787392704";',
            ),
        }
        for relative, (byte_limit, bounds, expected) in specifications.items():
            with self.subTest(relative=relative):
                self.assertEqual(bench.PGO_TRAINING_INPUTS.count(relative), 1)
                source = (bench.ROOT / relative).read_text(encoding="utf-8")
                self.assertLess(len(source.encode("utf-8")), byte_limit)
                self.assertNotRegex(
                    source,
                    r"(?<![A-Za-z0-9_$])(import|require|eval|Function)"
                    r"(?![A-Za-z0-9_$])",
                )
                for host_api in ("Deno.", "Bun.", "process.", "fetch("):
                    self.assertNotIn(host_api, source)
                for declaration, maximum in bounds:
                    match = re.search(declaration, source)
                    self.assertIsNotNone(match, declaration)
                    self.assertLessEqual(int(match.group(1)), maximum)
                self.assertIn(expected, source)

    def test_current_pgo_corpus_passes_structural_anti_leakage_policy(self):
        scored = bench.pgo_publication_input_paths()
        self.assertIsNotNone(scored)
        self.assertEqual(pgo_corpus.POLICY_ID, bench.PGO_SIMILARITY_POLICY)
        report = pgo_corpus.validate_corpus(
            root=bench.ROOT,
            training_paths=bench.PGO_TRAINING_INPUTS,
            scored_paths=scored,
        )
        self.assertEqual(report.training_count, len(bench.PGO_TRAINING_INPUTS))
        self.assertEqual(report.scored_count, len(scored))
        self.assertGreater(report.compared_unit_pairs, 0)
        self.assertIsNotNone(report.maximum)

    def test_pgo_holdout_covers_every_tracked_nontraining_benchmark(self):
        scored = bench.pgo_publication_input_paths()
        self.assertIsNotNone(scored)
        scored_set = set(scored)
        listed = subprocess.run(
            ["git", "-C", str(bench.ROOT), "ls-files", "-z", "--", "bench"],
            check=True,
            stdout=subprocess.PIPE,
        ).stdout
        expected = {
            os.fsdecode(name).replace("\\", "/")
            for name in listed.split(b"\0")
            if name
            and Path(os.fsdecode(name)).suffix.lower() in (".js", ".mjs", ".cjs")
            and not os.fsdecode(name).replace("\\", "/").startswith(
                "bench/pgo-training/"
            )
        }
        self.assertTrue(expected)
        self.assertTrue(expected <= scored_set, sorted(expected - scored_set))
        self.assertFalse(
            any(path.startswith("bench/pgo-training/") for path in scored_set)
        )
        for legacy in ("fib", "loop", "array", "string", "object", "sort"):
            self.assertIn(f"bench/{legacy}.js", scored_set)

    def test_diluted_exact_class_hierarchy_is_rejected(self):
        scored_source = (bench.ROOT / "bench/real/class-prototype-hot.js").read_text(
            encoding="utf-8"
        )
        hierarchy = scored_source[
            scored_source.index("class Shape") : scored_source.index("var objs")
        ]
        self.assertEqual(len(pgo_corpus.normalized_js_tokens(hierarchy)), 240)
        padding = "\n".join(
            (bench.ROOT / relative).read_text(encoding="utf-8")
            for relative in (
                "bench/pgo-training/csv-tuple-mix.js",
                "bench/pgo-training/async-dag-mix.js",
            )
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            training = root / "bench/pgo-training/diluted.js"
            scored = root / "bench/real/class-prototype-hot.js"
            training.parent.mkdir(parents=True)
            scored.parent.mkdir(parents=True)
            training.write_text(hierarchy + padding, encoding="ascii", newline="\n")
            scored.write_text(scored_source, encoding="utf-8", newline="\n")
            with self.assertRaisesRegex(
                pgo_corpus.CorpusValidationError, "structural PGO clone rejected"
            ):
                pgo_corpus.validate_corpus(
                    root=root,
                    training_paths=("bench/pgo-training/diluted.js",),
                    scored_paths=("bench/real/class-prototype-hot.js",),
                )

    def test_ambiguous_slash_cannot_swallow_scored_clone(self):
        scored = (bench.ROOT / "bench/real/class-prototype-hot.js").read_text(
            encoding="utf-8"
        )
        one_line = re.sub(r"//[^\n]*", "", scored).replace("\n", " ")
        disguised = (
            "var quotient = function () {} / 1; "
            + one_line
            + "; var tail = 2 / 3;"
        )
        tokens = pgo_corpus.normalized_js_tokens(disguised)
        self.assertIn("class", tokens)
        self.assertGreater(len(tokens), 600)
        self.assertTrue(
            any(
                finding.violates
                for finding in pgo_corpus.compare_sources(
                    "bench/pgo-training/disguised.js",
                    disguised,
                    "bench/real/class-prototype-hot.js",
                    scored,
                )
            )
        )

    def test_ambiguous_slash_policy_rejects_before_payload_scan(self):
        scored = (bench.ROOT / "bench/real/class-prototype-hot.js").read_text(
            encoding="utf-8"
        )
        one_line = re.sub(r"//[^\n]*", "", scored).replace("\n", " ")
        disguised_sources = {
            "keyword property call": (
                "var quotient={if:function(x){return x}}.if(2)/1; "
                + one_line
                + ";var tail=2/3;"
            ),
            "block then regex": (
                'if (false) {} /[/*]/.test("x");\n'
                + scored
                + "\n; /* marker */"
            ),
            "debugger ASI": (
                'debugger\n/[/*]/.test("x");\n' + scored + "\n; /* marker */"
            ),
            "break ASI": (
                'while(true){break\n/[/*]/.test("x");\n'
                + scored
                + "\n} /* marker */"
            ),
            "continue ASI": (
                'while(true){continue\n/[/*]/.test("x");\n'
                + scored
                + "\n} /* marker */"
            ),
            "contextual of identifier": (
                "var of=8; of/1; " + one_line + ";var tail=2/3;"
            ),
            "contextual await identifier": (
                "var await=8; await/1; " + one_line + ";var tail=2/3;"
            ),
            "contextual yield identifier": (
                "var yield=8; yield/1; " + one_line + ";var tail=2/3;"
            ),
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            training = root / "bench/pgo-training/disguised.js"
            scored_path = root / "bench/real/class-prototype-hot.js"
            training.parent.mkdir(parents=True)
            scored_path.parent.mkdir(parents=True)
            scored_path.write_text(scored, encoding="ascii", newline="\n")
            for label, disguised in disguised_sources.items():
                with self.subTest(label=label):
                    with self.assertRaisesRegex(
                        pgo_corpus.CorpusValidationError,
                        "ambiguous regex/division slash",
                    ):
                        pgo_corpus.normalized_js_tokens(
                            disguised, reject_ambiguous_slash=True
                        )
                    training.write_text(disguised, encoding="ascii", newline="\n")
                    with self.assertRaises(pgo_corpus.CorpusValidationError):
                        pgo_corpus.validate_corpus(
                            root=root,
                            training_paths=("bench/pgo-training/disguised.js",),
                            scored_paths=("bench/real/class-prototype-hot.js",),
                        )

    def test_property_keyword_division_does_not_open_regex(self):
        scored = (bench.ROOT / "bench/real/class-prototype-hot.js").read_text(
            encoding="utf-8"
        )
        one_line = re.sub(r"//[^\n]*", "", scored).replace("\n", " ")
        disguised = (
            "var quotient={return:8}.return/1; "
            + one_line
            + ";var tail=2/3;"
        )
        tokens = pgo_corpus.normalized_js_tokens(
            disguised, reject_ambiguous_slash=True
        )
        self.assertIn("class", tokens)
        self.assertGreater(len(tokens), 600)
        self.assertTrue(
            any(
                finding.violates
                for finding in pgo_corpus.compare_sources(
                    "bench/pgo-training/disguised.js",
                    disguised,
                    "bench/real/class-prototype-hot.js",
                    scored,
                )
            )
        )

    def test_private_keyword_names_are_atomic_before_division(self):
        scored = (bench.ROOT / "bench/real/class-prototype-hot.js").read_text(
            encoding="utf-8"
        )
        one_line = re.sub(r"//[^\n]*", "", scored).replace("\n", " ")
        prefixes = {
            "private regex-prefix keyword": (
                "class Carrier{#return=8;quotient(){return this.#return/1;}}"
                "var quotient=new Carrier().quotient(); ",
                False,
            ),
            "private control-head keyword": (
                "class Carrier{#if(x){return x}quotient(){return this.#if(2)/1;}}"
                "var quotient=new Carrier().quotient(); ",
                True,
            ),
        }
        for label, (prefix, rejects_ambiguous_call) in prefixes.items():
            with self.subTest(label=label):
                disguised = prefix + one_line + ";var tail=2/3;"
                tokens = pgo_corpus.normalized_js_tokens(disguised)
                self.assertIn("PRIVATE_ID", tokens)
                self.assertIn("class", tokens)
                self.assertGreater(len(tokens), 600)
                if rejects_ambiguous_call:
                    with self.assertRaisesRegex(
                        pgo_corpus.CorpusValidationError,
                        "ambiguous regex/division slash",
                    ):
                        pgo_corpus.normalized_js_tokens(
                            disguised, reject_ambiguous_slash=True
                        )
                    continue
                strict_tokens = pgo_corpus.normalized_js_tokens(
                    disguised, reject_ambiguous_slash=True
                )
                self.assertEqual(strict_tokens, tokens)
                self.assertTrue(
                    any(
                        finding.violates
                        for finding in pgo_corpus.compare_sources(
                            "bench/pgo-training/disguised.js",
                            disguised,
                            "bench/real/class-prototype-hot.js",
                            scored,
                        )
                    )
                )

    def test_true_control_regex_payload_cannot_hide_following_clone(self):
        scored = (bench.ROOT / "bench/real/class-prototype-hot.js").read_text(
            encoding="utf-8"
        )
        disguised = (
            'if (true) /[/*]/.test("x");\n'
            + scored
            + "\n; /* marker */"
        )
        tokens = pgo_corpus.normalized_js_tokens(
            disguised, reject_ambiguous_slash=True
        )
        self.assertEqual(tokens.count("REGEX"), 1)
        self.assertIn("class", tokens)
        self.assertGreater(len(tokens), 600)
        self.assertTrue(
            any(
                finding.violates
                for finding in pgo_corpus.compare_sources(
                    "bench/pgo-training/disguised.js",
                    disguised,
                    "bench/real/class-prototype-hot.js",
                    scored,
                )
            )
        )

    def test_training_source_spelling_policy_rejects_lexer_evasions(self):
        cases = {
            "unicode escape": (r"var \u0066old = 1;\n", "raw Unicode escape"),
            "combining identifier": ("var a\u0301 = 1;\n", "ASCII source spelling"),
            "zwnj identifier": ("var a\u200c = 1;\n", "ASCII source spelling"),
            "other id start": ("var \u2118name = 1;\n", "ASCII source spelling"),
            "feff whitespace": ("var\ufeffvalue\ufeff=\ufeff1;\n", "ASCII source spelling"),
            "carriage return": ("// filler\rvar value = 1;\n", "LF-only"),
            "line separator": ("// filler\u2028var value = 1;\n", "ASCII source spelling"),
            "paragraph separator": ("// filler\u2029var value = 1;\n", "ASCII source spelling"),
            "html open comment": ("<!-- filler\nvar value = 1;\n", "HTML comment"),
            "html close comment": ("--> filler\nvar value = 1;\n", "HTML comment"),
            "hashbang": ("#!/usr/bin/env zipp\nvar value = 1;\n", "hashbang"),
            "template literal": ("var value = `distinct-token`;\n", "template literal"),
            "fnv decimal multiplier": (
                "var hash = Math.imul(7, 16777619);\n",
                "FNV-1a checksum constant",
            ),
            "fnv hexadecimal offset": (
                "var hash = 0x811C9DC5;\n",
                "FNV-1a checksum constant",
            ),
        }
        for label, (training_source, message) in cases.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                training = root / "bench/pgo-training/train.js"
                scored = root / "bench/real/score.js"
                training.parent.mkdir(parents=True)
                scored.parent.mkdir(parents=True)
                training.write_text(training_source, encoding="utf-8", newline="")
                scored.write_text("console.log(2);\n", encoding="ascii", newline="\n")
                with self.assertRaisesRegex(pgo_corpus.CorpusValidationError, message):
                    pgo_corpus.validate_corpus(
                        root=root,
                        training_paths=("bench/pgo-training/train.js",),
                        scored_paths=("bench/real/score.js",),
                    )

    def test_fail_closed_spelling_rejects_semantic_clone_transformations(self):
        scored_source = (bench.ROOT / "bench/real/class-prototype-hot.js").read_text(
            encoding="utf-8"
        )
        transformations = {
            "unicode identifiers": scored_source.replace("Shape", r"\u0053hape"),
            "feff separators": scored_source.replace(" ", "\ufeff"),
            "annex-b line injection": "\n".join(
                "<!-- independent filler\n" + line
                for line in scored_source.splitlines()
            ),
        }
        for label, transformed in transformations.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                training = root / "bench/pgo-training/transformed.js"
                scored = root / "bench/real/class-prototype-hot.js"
                training.parent.mkdir(parents=True)
                scored.parent.mkdir(parents=True)
                training.write_text(transformed, encoding="utf-8", newline="")
                scored.write_text(scored_source, encoding="utf-8", newline="\n")
                with self.assertRaises(pgo_corpus.CorpusValidationError):
                    pgo_corpus.validate_corpus(
                        root=root,
                        training_paths=("bench/pgo-training/transformed.js",),
                        scored_paths=("bench/real/class-prototype-hot.js",),
                    )

    def test_distinctive_numeric_literals_use_canonical_values(self):
        training_spellings = (
            "2654435761e0",
            "2654435761.0",
            "2654435761..toString()",
        )
        for spelling in training_spellings:
            with self.subTest(spelling=spelling), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                training = root / "bench/pgo-training/train.js"
                scored = root / "bench/real/score.js"
                training.parent.mkdir(parents=True)
                scored.parent.mkdir(parents=True)
                training.write_text(
                    f"var trainingValue = {spelling};\n",
                    encoding="ascii",
                    newline="\n",
                )
                scored.write_text(
                    "var scoredValue = 2654435761;\n",
                    encoding="ascii",
                    newline="\n",
                )
                with self.assertRaisesRegex(
                    pgo_corpus.CorpusValidationError,
                    "distinctive numeric literal reused",
                ):
                    pgo_corpus.validate_corpus(
                        root=root,
                        training_paths=("bench/pgo-training/train.js",),
                        scored_paths=("bench/real/score.js",),
                    )

        tokens = pgo_corpus.normalized_js_tokens(
            "var quotient = 1 / 2;",
            reject_ambiguous_slash=True,
            preserve_numbers=True,
        )
        self.assertIn("/", tokens)

    def test_literal_sensitive_guard_rejects_small_numeric_kernels(self):
        cases = {
            "xorshift": (
                "var a=1; a ^= a << 13; a ^= a >>> 17; a ^= a << 5;\n",
                "var b=2; b ^= b << 13; b ^= b >>> 17; b ^= b << 5;\n",
            ),
            "rotate": (
                "var a=1; a=(a << 7) | (a >>> 25);\n",
                "var b=2; b=(b << 7) | (b >>> 25);\n",
            ),
        }
        for label, (training_source, scored_source) in cases.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                training = root / "bench/pgo-training/train.js"
                scored = root / "bench/real/score.js"
                training.parent.mkdir(parents=True)
                scored.parent.mkdir(parents=True)
                training.write_text(training_source, encoding="ascii", newline="\n")
                scored.write_text(scored_source, encoding="ascii", newline="\n")
                with self.assertRaisesRegex(
                    pgo_corpus.CorpusValidationError,
                    "ordered numeric operator tuple reused",
                ):
                    pgo_corpus.validate_corpus(
                        root=root,
                        training_paths=("bench/pgo-training/train.js",),
                        scored_paths=("bench/real/score.js",),
                    )

    def test_literal_sensitive_guard_decodes_strings_and_checks_regex(self):
        cases = {
            "cooked string": (
                r'var a = "distinct\x2dtoken";' + "\n",
                'var b = "distinct-token";\n',
                "cooked string literal reused",
            ),
            "regex body": (
                r"var a = /ab+c[0-9]/;" + "\n",
                r"var b = /ab+c[0-9]/;" + "\n",
                "regular-expression body reused",
            ),
        }
        for label, (training_source, scored_source, message) in cases.items():
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                training = root / "bench/pgo-training/train.js"
                scored = root / "bench/real/score.js"
                training.parent.mkdir(parents=True)
                scored.parent.mkdir(parents=True)
                training.write_text(training_source, encoding="ascii", newline="\n")
                scored.write_text(scored_source, encoding="ascii", newline="\n")
                with self.assertRaisesRegex(pgo_corpus.CorpusValidationError, message):
                    pgo_corpus.validate_corpus(
                        root=root,
                        training_paths=("bench/pgo-training/train.js",),
                        scored_paths=("bench/real/score.js",),
                    )

        tokens = pgo_corpus.normalized_js_tokens(
            "var a = 'distinct-token' / 2; var b = /ab+c/ / 2;",
            reject_ambiguous_slash=True,
            preserve_numbers=True,
            preserve_literals=True,
        )
        self.assertEqual(tokens.count("/"), 2)

    def test_all_ecmascript_line_terminators_end_line_comments(self):
        for terminator in ("\n", "\r", "\u2028", "\u2029"):
            with self.subTest(codepoint=ord(terminator)):
                tokens = pgo_corpus.normalized_js_tokens(
                    "// hidden" + terminator + "var visible = 1;"
                )
                self.assertIn("var", tokens)
                self.assertIn("=", tokens)

    def test_structural_anti_leakage_rejects_renamed_resized_copy(self):
        scored_source = """
function fold(values) {
  var total = 0;
  for (var index = 0; index < values.length; index++) {
    var value = values[index];
    if ((value & 1) === 0) {
      total = Math.imul(total ^ value, 16777617) >>> 0;
    } else {
      total = (total + value * 3) ^ (total >>> 7);
    }
  }
  return total >>> 0;
}
var values = [];
for (var index = 0; index < 8000; index++) values.push(index * 5);
console.log(fold(values));
"""
        training_source = """
function combine(entries) {
  var checksum = 0;
  for (var cursor = 0; cursor < entries.length; cursor++) {
    var item = entries[cursor];
    if ((item & 1) === 0) {
      checksum = Math.imul(checksum ^ item, 2166136263) >>> 0;
    } else {
      checksum = (checksum + item * 19) ^ (checksum >>> 11);
    }
  }
  return checksum >>> 0;
}
var entries = [];
for (var cursor = 0; cursor < 12000; cursor++) entries.push(cursor * 13);
console.log(combine(entries));
"""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            training = root / "bench" / "pgo-training" / "copy.js"
            scored = root / "bench" / "real" / "score.js"
            training.parent.mkdir(parents=True)
            scored.parent.mkdir(parents=True)
            training.write_text(training_source, encoding="utf-8", newline="\n")
            scored.write_text(scored_source, encoding="utf-8", newline="\n")
            with self.assertRaisesRegex(
                pgo_corpus.CorpusValidationError,
                "structural PGO clone rejected",
            ):
                pgo_corpus.validate_corpus(
                    root=root,
                    training_paths=("bench/pgo-training/copy.js",),
                    scored_paths=("bench/real/score.js",),
                )

    def test_structural_tokenizer_normalizes_regex_literal_contents(self):
        tokens = pgo_corpus.normalized_js_tokens(
            'var match = /[}"\'\\/]+/gi.exec(line); '
            "var ratio = left / right / 2;"
        )
        self.assertEqual(tokens.count("REGEX"), 1)
        self.assertEqual(tokens.count("/"), 2)
        control_tokens = pgo_corpus.normalized_js_tokens(
            "if (ready) /[}]/.test(text);"
        )
        self.assertEqual(control_tokens.count("REGEX"), 1)
        with self.assertRaisesRegex(
            pgo_corpus.CorpusValidationError,
            "unterminated regular expression literal",
        ):
            pgo_corpus.normalized_js_tokens("var bad = /unterminated\n")

    def test_template_interpolation_cannot_hide_executable_structure(self):
        scored_source = """
function aggregate(values) {
  var total = 0;
  for (var index = 0; index < values.length; index++) {
    var value = values[index];
    if ((value & 3) === 0) total = Math.imul(total ^ value, 16777619);
    else total = (total + value * 7) ^ (total >>> 9);
  }
  return total >>> 0;
}
"""
        hidden_source = """
var rendered = `${(function combine(entries) {
  var checksum = 0;
  for (var cursor = 0; cursor < entries.length; cursor++) {
    var item = entries[cursor];
    if ((item & 3) === 0) checksum = Math.imul(checksum ^ item, 2166136261);
    else checksum = (checksum + item * 19) ^ (checksum >>> 11);
  }
  return checksum >>> 0;
})(items)}`;
"""
        tokens = pgo_corpus.normalized_js_tokens(hidden_source)
        self.assertIn("${", tokens)
        self.assertIn("function", tokens)
        self.assertIn("for", tokens)
        self.assertTrue(
            any(
                finding.violates
                for finding in pgo_corpus.compare_sources(
                    "bench/pgo-training/hidden.js",
                    hidden_source,
                    "bench/real/scored.js",
                    scored_source,
                )
            )
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            training = root / "bench" / "pgo-training" / "hidden.js"
            scored = root / "bench" / "real" / "scored.js"
            training.parent.mkdir(parents=True)
            scored.parent.mkdir(parents=True)
            training.write_text(hidden_source, encoding="utf-8", newline="\n")
            scored.write_text(scored_source, encoding="utf-8", newline="\n")
            with self.assertRaisesRegex(
                pgo_corpus.CorpusValidationError,
                "template literal syntax",
            ):
                pgo_corpus.validate_corpus(
                    root=root,
                    training_paths=("bench/pgo-training/hidden.js",),
                    scored_paths=("bench/real/scored.js",),
                )

    def test_corpus_validator_preserves_exact_and_dependency_guards(self):
        cases = (
            (
                "module dependency",
                "// require is rejected even in comments\nconsole.log(1);\n",
                "console.log(2);\n",
                "not self-contained",
            ),
            (
                "dynamic code",
                'var make = Function("return 1"); console.log(make());\n',
                "console.log(2);\n",
                "dynamic code evaluation",
            ),
            (
                "identical bytes",
                "console.log(42);\n",
                "console.log(42);\n",
                "duplicates scored input bytes",
            ),
        )
        for label, training_source, scored_source, message in cases:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                training = root / "bench" / "pgo-training" / "train.js"
                scored = root / "bench" / "real" / "score.js"
                training.parent.mkdir(parents=True)
                scored.parent.mkdir(parents=True)
                training.write_text(training_source, encoding="utf-8", newline="\n")
                scored.write_text(scored_source, encoding="utf-8", newline="\n")
                with self.assertRaisesRegex(
                    pgo_corpus.CorpusValidationError, message
                ):
                    pgo_corpus.validate_corpus(
                        root=root,
                        training_paths=("bench/pgo-training/train.js",),
                        scored_paths=("bench/real/score.js",),
                    )

    def test_dirty_engine_is_a_reason_and_is_fatal_for_a_headline(self):
        reasons = bench.check_engine_provenance(
            [engine_meta("zipp", commit=self.HEAD, dirty=True)],
            self.HEAD,
            is_ab=False,
            allow_dirty=False,
            allow_nonhead=False,
        )
        self.assertTrue(any("DIRTY" in reason for reason in reasons))
        self.assertTrue(
            bench.provenance_is_fatal(reasons, is_ab=False)
        )

    def test_nonhead_engine_is_a_reason(self):
        reasons = bench.check_engine_provenance(
            [engine_meta("zipp", commit=self.OTHER)],
            self.HEAD,
            is_ab=False,
            allow_dirty=False,
            allow_nonhead=False,
        )
        self.assertEqual(len(reasons), 1)
        self.assertIn(self.OTHER, reasons[0])
        self.assertIn(self.HEAD, reasons[0])

    def test_the_exact_head_clean_artifact_failure_is_caught(self):
        # git_commit 2a616f5..., engine cdda4e8... + dirty. Both rules fire.
        reasons = bench.check_engine_provenance(
            [engine_meta("zipp", commit="cdda4e8", dirty=True, diff="a8cbe062")],
            "2a616f5",
            is_ab=False,
            allow_dirty=False,
            allow_nonhead=False,
        )
        self.assertEqual(len(reasons), 2)
        self.assertTrue(
            bench.provenance_is_fatal(reasons, is_ab=False)
        )

    def test_overrides_downgrade_fatal_to_recorded(self):
        meta = [engine_meta("zipp", commit=self.OTHER, dirty=True)]
        reasons = bench.check_engine_provenance(
            meta, self.HEAD, is_ab=False, allow_dirty=True, allow_nonhead=True
        )
        self.assertEqual(reasons, [])
        # An override that does not cover the reason still stops the run.
        partial = bench.check_engine_provenance(
            meta, self.HEAD, is_ab=False, allow_dirty=True, allow_nonhead=False
        )
        self.assertEqual(len(partial), 1)
        self.assertTrue(bench.provenance_is_fatal(partial, is_ab=False))

    def test_assessment_records_covered_reasons_and_keeps_partial_uncovered(self):
        meta = [engine_meta("zipp", commit=self.OTHER, dirty=True)]
        recorded, uncovered = bench.provenance_assessment(
            meta,
            self.HEAD,
            is_ab=False,
            allow_dirty=True,
            allow_nonhead=True,
        )
        self.assertEqual(len(recorded), 3)
        self.assertEqual(uncovered, [])

        recorded, uncovered = bench.provenance_assessment(
            meta,
            self.HEAD,
            is_ab=False,
            allow_dirty=True,
            allow_nonhead=False,
        )
        self.assertEqual(len(recorded), 3)
        self.assertEqual(len(uncovered), 1)
        self.assertIn(self.OTHER, uncovered[0])

    def test_missing_identity_is_a_reason(self):
        reasons = bench.check_engine_provenance(
            [{"name": "node", "sha256": "cd" * 32, "build_identity": None}],
            self.HEAD,
            is_ab=False,
            allow_dirty=False,
            allow_nonhead=False,
        )
        self.assertEqual(len(reasons), 1)
        self.assertIn("build identity", reasons[0])

    def test_ab_of_two_commits_is_allowed_and_never_fatal(self):
        # The point of an A/B: two builds, neither of which can be HEAD.
        reasons = bench.check_engine_provenance(
            [
                engine_meta("old", commit=self.OTHER, sha="11" * 32),
                engine_meta("new", commit=self.HEAD, sha="22" * 32),
            ],
            self.HEAD,
            is_ab=True,
            allow_dirty=False,
            allow_nonhead=False,
        )
        self.assertEqual(reasons, [])
        self.assertFalse(bench.provenance_is_fatal(["x"], is_ab=True))

    def test_ab_sides_reporting_one_source_is_a_reason(self):
        reasons = bench.check_engine_provenance(
            [
                engine_meta("old", commit=self.HEAD, sha="11" * 32),
                engine_meta("new", commit=self.HEAD, sha="22" * 32),
            ],
            self.HEAD,
            is_ab=True,
            allow_dirty=False,
            allow_nonhead=False,
        )
        self.assertEqual(len(reasons), 1)
        self.assertIn("SAME source", reasons[0])

    def test_ab_env_ablation_on_one_binary_is_not_a_reason(self):
        # The idiom this repo measures with: ONE binary, two --ab-env sides. Both
        # sides report the same source BY CONSTRUCTION, and flagging it would
        # reject the protocol rather than a mistake.
        reasons = bench.check_engine_provenance(
            [
                engine_meta("old", commit=self.HEAD, sha="11" * 32),
                engine_meta("new", commit=self.HEAD, sha="11" * 32),
            ],
            self.HEAD,
            is_ab=True,
            allow_dirty=False,
            allow_nonhead=False,
            ab_sides_distinguished=True,
        )
        self.assertEqual(reasons, [])

    def test_binary_change_during_the_run_is_drift(self):
        before = [engine_meta("zipp", commit=self.HEAD, sha="11" * 32)]
        after = [engine_meta("zipp", commit=self.HEAD, sha="22" * 32)]
        drift = bench.engine_drift(before, after)
        self.assertEqual(len(drift), 1)
        self.assertIn("sha256 changed", drift[0])

    def test_identity_change_during_the_run_is_drift(self):
        before = [engine_meta("zipp", commit=self.HEAD, sha="11" * 32)]
        after = [engine_meta("zipp", commit=self.OTHER, sha="11" * 32)]
        drift = bench.engine_drift(before, after)
        self.assertEqual(len(drift), 1)
        self.assertIn("build identity changed", drift[0])

    def test_a_stable_engine_shows_no_drift(self):
        before = [engine_meta("zipp", commit=self.HEAD)]
        self.assertEqual(bench.engine_drift(before, list(before)), [])


class RowSetTests(unittest.TestCase):
    """Headline vs diagnostic must live in the harness, not in a person's head.

    A default run globs all 13 programs; the three diagnostics are 3.5-5.5x rows
    and inflate the geomean by roughly 0.43x, so a 13-row number silently is not
    the retained series.
    """

    def test_classification_splits_the_retained_ten(self):
        sets = bench.classify_benches(bench.discover_benches())
        self.assertEqual(len(sets["headline_benches"]), 10)
        self.assertEqual(len(sets["diagnostic_benches"]), 3)
        self.assertEqual(sets["unclassified_benches"], [])
        self.assertNotIn("sparse-array-v2", sets["headline_benches"])

    def test_headline_list_is_the_documented_ten(self):
        self.assertEqual(len(set(bench.HEADLINE_BENCHES)), 10)
        self.assertFalse(
            set(bench.HEADLINE_BENCHES) & set(bench.DIAGNOSTIC_BENCHES)
        )

    def test_a_new_benchmark_is_unclassified_not_headline(self):
        sets = bench.classify_benches(["json-large", "brand-new-row"])
        self.assertEqual(sets["headline_benches"], ["json-large"])
        self.assertEqual(sets["unclassified_benches"], ["brand-new-row"])

    def test_subset_geomean_uses_only_its_rows(self):
        samples = {
            "zipp": {"fast": [1.0, 1.0], "slow": [4.0, 4.0]},
            "node": {"fast": [1.0, 1.0], "slow": [1.0, 1.0]},
        }
        fast_only = bench.subset_geomean(
            samples, ["fast"], "node", "zipp", seed=1, bootstrap_samples=32
        )
        both = bench.subset_geomean(
            samples, ["fast", "slow"], "node", "zipp", seed=1, bootstrap_samples=32
        )
        self.assertAlmostEqual(fast_only["geomean_paired_ratio"], 1.0)
        self.assertAlmostEqual(both["geomean_paired_ratio"], 2.0)
        self.assertEqual(fast_only["benches"], ["fast"])


if __name__ == "__main__":
    unittest.main()
