import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


HARNESS_PATH = Path(__file__).with_name("bench_peak_rss.py")
SPEC = importlib.util.spec_from_file_location("zipp_bench_peak_rss", HARNESS_PATH)
assert SPEC and SPEC.loader
rss = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = rss
SPEC.loader.exec_module(rss)


class PeakRssHarnessTests(unittest.TestCase):
    def test_generated_case_is_deterministic_and_self_describing(self):
        first = rss.generated_case(5, 123)
        second = rss.generated_case(5, 123)
        self.assertEqual(first, second)
        self.assertEqual(first["source_sha256"], rss.sha256_bytes(first["source"].encode()))
        self.assertEqual(
            first["expected_stdout"],
            b"zipp-peak-rss 5 123 620\n",
        )
        self.assertIn("globalThis.__zippPeakRssRetained", first["source"])

    def test_zero_key_case_keeps_objects_observable(self):
        case = rss.generated_case(0, 10)
        self.assertIn("retained[0] === probe", case["source"])
        self.assertEqual(case["expected_stdout"], b"zipp-peak-rss 0 10 0\n")

    def test_case_rotations_balance_every_position(self):
        case_ids = ["a", "b", "c", "d"]
        orders = [
            rss.case_order_for_rep(case_ids, rep, seed=17)
            for rep in range(len(case_ids))
        ]
        for position in range(len(case_ids)):
            self.assertEqual(
                {order[position] for order in orders},
                set(case_ids),
            )

    def test_engine_order_is_counterbalanced(self):
        engines = [("baseline", ["old"]), ("candidate", ["new"])]
        for stable_case_index in range(6):
            orders = [
                [
                    name
                    for name, _ in rss.engine_order_for_case(
                        engines, rep, stable_case_index
                    )
                ]
                for rep in range(6)
            ]
            self.assertEqual(
                sum(order[0] == "baseline" for order in orders),
                3,
            )
            self.assertEqual(
                sum(order[0] == "candidate" for order in orders),
                3,
            )

    def test_summary_applies_max_of_absolute_and_relative_gate(self):
        observations = []
        for rep, baseline, candidate in ((0, 100, 108), (1, 100, 108)):
            observations.extend(
                [
                    {
                        "rep": rep,
                        "case": "small",
                        "engine": "baseline",
                        "valid": True,
                        "peak_rss_bytes": baseline,
                    },
                    {
                        "rep": rep,
                        "case": "small",
                        "engine": "candidate",
                        "valid": True,
                        "peak_rss_bytes": candidate,
                    },
                ]
            )
        summary, failures = rss.summarize(
            observations,
            case_ids=["small"],
            reps=2,
            absolute_gate_bytes=5,
            relative_gate=0.10,
        )
        self.assertEqual(failures, [])
        self.assertTrue(summary["small"]["gate_passed"])
        self.assertEqual(summary["small"]["allowed_delta_bytes"], 10)

    def test_summary_uses_paired_deltas_not_marginal_medians(self):
        observations = []
        for rep, baseline, candidate in (
            (0, 1, 1001),
            (1, 2, 3),
            (2, 100, 101),
        ):
            for engine, peak in (("baseline", baseline), ("candidate", candidate)):
                observations.append(
                    {
                        "rep": rep,
                        "case": "crossing",
                        "engine": engine,
                        "valid": True,
                        "peak_rss_bytes": peak,
                    }
                )
        summary, failures = rss.summarize(
            observations,
            case_ids=["crossing"],
            reps=3,
            absolute_gate_bytes=10,
            relative_gate=0.0,
        )
        item = summary["crossing"]
        self.assertEqual(failures, [])
        self.assertEqual(item["median_paired_delta_bytes"], 1)
        self.assertEqual(item["marginal_median_delta_bytes"], 99)
        self.assertTrue(item["gate_passed"])

    def test_default_gate_detects_sixteen_bytes_per_default_object(self):
        baseline = 128 * rss.MIB
        fixed_growth = 16 * rss.DEFAULT_OBJECTS
        observations = []
        for rep in range(2):
            for engine, peak in (
                ("baseline", baseline),
                ("candidate", baseline + fixed_growth),
            ):
                observations.append(
                    {
                        "rep": rep,
                        "case": "layout",
                        "engine": engine,
                        "valid": True,
                        "peak_rss_bytes": peak,
                    }
                )
        summary, failures = rss.summarize(
            observations,
            case_ids=["layout"],
            reps=2,
            absolute_gate_bytes=int(rss.DEFAULT_ABSOLUTE_GATE_MIB * rss.MIB),
            relative_gate=rss.DEFAULT_RELATIVE_GATE_PERCENT / 100.0,
        )
        self.assertFalse(summary["layout"]["gate_passed"])
        self.assertTrue(failures)
        sensitivity = rss.gate_sensitivity(
            rss.DEFAULT_OBJECTS,
            int(rss.DEFAULT_ABSOLUTE_GATE_MIB * rss.MIB),
            rss.DEFAULT_RELATIVE_GATE_PERCENT / 100.0,
        )
        self.assertEqual(sensitivity["fixed_layout_probe_total_bytes"], 4_000_000)
        self.assertTrue(sensitivity["probe_exceeds_absolute_gate"])

    def test_run_peak_once_checks_exact_output_and_collects_peak(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "child.py"
            script.write_text(
                "payload = bytearray(2 * 1024 * 1024)\n"
                "print('peak-child-ok')\n",
                encoding="utf-8",
            )
            expected = b"peak-child-ok" + os.linesep.encode()
            result = rss.run_peak_once(
                [sys.executable, str(script)],
                expected_stdout=expected,
                timeout=10.0,
            )
        self.assertTrue(result["valid"], result)
        self.assertGreater(result["peak_rss_bytes"], 0)
        self.assertTrue(result["stdout_exact"])
        self.assertTrue(result["stderr_empty"])

    def test_child_environment_strips_ambient_controls_and_applies_side(self):
        ambient = {
            "PATH": "loader-path",
            "SystemRoot": r"C:\\Windows",
            "TEMP": "temporary",
            "UNRELATED": "preserved",
            "ZIPP_NOJIT": "1",
            "rust_log": "trace",
            "MIMALLOC_VERBOSE": "1",
            "NODE_OPTIONS": "--jitless",
            "DENO_DIR": "private-path",
            "BUN_RUNTIME_TRANSPILER_CACHE_PATH": "private-path",
        }
        explicit = {"ZIPP_NOJIT": "0", "ZIPP_RSS_SIDE": "candidate"}
        cleaned = rss.clean_child_environment(ambient, explicit)
        self.assertEqual(
            cleaned,
            {
                "PATH": "loader-path",
                "SystemRoot": r"C:\\Windows",
                "TEMP": "temporary",
                "UNRELATED": "preserved",
                "ZIPP_NOJIT": "0",
                "ZIPP_RSS_SIDE": "candidate",
            },
        )

    def test_run_peak_once_uses_the_clean_environment_policy(self):
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / "environment_child.py"
            script.write_text(
                "import os\n"
                "print(os.environ.get('ZIPP_AMBIENT', 'missing'), "
                "os.environ.get('ZIPP_SIDE', 'missing'), 'PATH' in os.environ)\n",
                encoding="utf-8",
            )
            expected = b"missing explicit True" + os.linesep.encode()
            with mock.patch.dict(
                rss.os.environ,
                {"ZIPP_AMBIENT": "must-be-removed"},
                clear=False,
            ):
                result = rss.run_peak_once(
                    [sys.executable, str(script)],
                    expected_stdout=expected,
                    timeout=10.0,
                    env={"ZIPP_SIDE": "explicit"},
                )
        self.assertTrue(result["valid"], result)

    def test_failed_posix_reap_marks_process_to_keep_context_exit_bounded(self):
        process = mock.Mock(pid=12345, returncode=None)
        with (
            mock.patch.object(rss, "_signal_posix_tree") as signal_tree,
            mock.patch.object(
                rss,
                "_bounded_wait4",
                side_effect=OSError("synthetic wait4 failure"),
            ),
        ):
            returncode = rss._bounded_posix_kill_and_reap(process, timeout=0.01)
        signal_tree.assert_called_once_with(process)
        self.assertIsNone(returncode)
        self.assertEqual(process.returncode, -rss.POSIX_SIGKILL)

    def test_environment_recording_exactly_reuses_bench_redaction(self):
        with mock.patch.dict(
            rss.os.environ,
            {
                "ZIPP_NOJIT": "1",
                "ZIPP_API_TOKEN": "secret",
                "ZIPP_UNKNOWN_CONTROL": "1",
                "RUST_LOG": "zipp=trace",
                "UNRELATED_SECRET": "ignored",
            },
            clear=True,
        ):
            self.assertEqual(
                rss.core.relevant_environment(),
                {
                    "RUST_LOG": "<redacted>",
                    "ZIPP_API_TOKEN": "<redacted>",
                    "ZIPP_NOJIT": "1",
                    "ZIPP_UNKNOWN_CONTROL": "<redacted>",
                },
            )

    def test_explicit_side_environment_is_redacted_before_serialization(self):
        explicit = {
            "ZIPP_NOJIT": "1",
            "zipp_api_token": "secret",
            "ZIPP_PRIVATE_PATH": r"C:\\private\\source",
            "UNRELATED_SECRET": "not-in-artifact-namespace",
        }
        self.assertEqual(
            rss.core.recorded_environment(explicit),
            {
                "ZIPP_NOJIT": "1",
                "ZIPP_PRIVATE_PATH": "<redacted>",
                "zipp_api_token": "<redacted>",
            },
        )

    def test_ab_env_parsing_is_causal_and_exact(self):
        args = rss.build_parser().parse_args(
            [
                "--ab",
                "old",
                "new",
                "--ab-env",
                "ZIPP_NOJIT=1,ZIPP_VALUE=two=parts",
                "-",
            ]
        )
        self.assertEqual(
            args.ab_env,
            [
                {"ZIPP_NOJIT": "1", "ZIPP_VALUE": "two=parts"},
                {},
            ],
        )
        # The shared A/B guard permits one binary only when the explicit side
        # environments actually differ.
        rss.core.reject_identical_ab_binaries(
            [sys.executable, sys.executable],
            tuple(args.ab_env),
            allow=False,
        )
        with self.assertRaisesRegex(SystemExit, "same executable"):
            rss.core.reject_identical_ab_binaries(
                [sys.executable, sys.executable],
                ({"ZIPP_NOJIT": "1"}, {"ZIPP_NOJIT": "1"}),
                allow=False,
            )

    def test_balance_assessment_names_incomplete_schedules(self):
        exact = rss.balance_assessment(6, 6)
        self.assertTrue(exact["engine_order_exact"])
        self.assertTrue(exact["case_position_exact"])
        incomplete = rss.balance_assessment(5, 6)
        self.assertFalse(incomplete["engine_order_exact"])
        self.assertFalse(incomplete["case_position_exact"])

    def test_main_rejects_nonfinite_measurement_arguments(self):
        cases = (
            ("--timeout", "nan", "--timeout must be positive"),
            ("--timeout", "inf", "--timeout must be positive"),
            ("--absolute-gate-mib", "nan", "gate thresholds"),
            ("--relative-gate-percent", "inf", "gate thresholds"),
        )
        for option, value, message in cases:
            with self.subTest(option=option, value=value):
                with self.assertRaisesRegex(SystemExit, message):
                    rss.main(["--ab", "missing-old", "missing-new", option, value])

    def test_main_refuses_to_overwrite_before_resolving_engines(self):
        with tempfile.TemporaryDirectory() as directory:
            result = Path(directory) / "existing.json"
            result.write_text('{"preserved": true}\n', encoding="utf-8")
            with self.assertRaisesRegex(SystemExit, "refusing to overwrite"):
                rss.main(
                    [
                        "--ab",
                        "missing-old",
                        "missing-new",
                        "--json",
                        str(result),
                    ]
                )
            self.assertEqual(result.read_text(encoding="utf-8"), '{"preserved": true}\n')


if __name__ == "__main__":
    unittest.main()
