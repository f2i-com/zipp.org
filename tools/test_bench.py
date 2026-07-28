import argparse
import contextlib
import importlib.util
import io
import json
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

    def test_bootstrap_is_deterministic_and_contains_point_estimate(self):
        ratios = [0.8, 0.9, 1.0, 1.1, 1.2]
        first = bench.bootstrap_median_ci(ratios, seed=7, samples=500)
        second = bench.bootstrap_median_ci(ratios, seed=7, samples=500)
        self.assertEqual(first, second)
        self.assertLessEqual(first[0], 1.0)
        self.assertGreaterEqual(first[1], 1.0)

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
        self.assertFalse(written["observations"][0]["valid_for_stats"])
        self.assertTrue(written["observations"][1]["valid_for_stats"])


if __name__ == "__main__":
    unittest.main()
