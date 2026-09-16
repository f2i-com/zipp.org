import contextlib
import importlib.util
import io
import json
import math
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


HARNESS_PATH = Path(__file__).with_name("python_bench.py")
SPEC = importlib.util.spec_from_file_location("zipp_python_bench", HARNESS_PATH)
assert SPEC and SPEC.loader
pb = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = pb
SPEC.loader.exec_module(pb)


def bench(bench_id, zipp_only=False):
    group, name = bench_id.split("/")
    return pb.Benchmark(bench_id, group, name, Path(f"/suite/{bench_id}.py"), zipp_only, "0" * 64)


ZIPP = pb.Engine("zipp", "zipp", ("/bin/zipp-a", "py"))
ZIPP_B = pb.Engine("zipp-b", "zipp", ("/bin/zipp-b", "py"))
CPYTHON = pb.Engine("cpython", "cpython", ("py", "-3.13"))


def observation(seq, bench_id, engine, rep, stdout="ok 1\n", work=0.5, wall=0.6, error=None):
    return {
        "seq": seq,
        "rep": rep,
        "bench": bench_id,
        "engine": engine,
        "stdout": stdout,
        "work_s": None if error else work,
        "wall_s": wall,
        "error": error,
    }


class ArgumentParsingTests(unittest.TestCase):
    def test_zipp_is_required(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            pb.parse_args([])

    def test_defaults(self):
        args = pb.parse_args(["--zipp", "zipp.exe"])
        self.assertEqual(args.zipp, "zipp.exe")
        self.assertIsNone(args.zipp_b)
        self.assertEqual(args.cpython, pb.DEFAULT_CPYTHON)
        self.assertFalse(args.no_cpython)
        self.assertEqual(args.reps, 5)
        self.assertEqual(args.group, "all")
        self.assertEqual(args.only, "")
        self.assertEqual(args.metric, "work")
        self.assertGreater(args.timeout, 0)
        self.assertIsNone(args.json)

    def test_ab_and_filters(self):
        args = pb.parse_args(
            ["--zipp", "a", "--zipp-b", "b", "--reps", "3", "--group", "macro", "--only", "torch",
             "--timeout", "30", "--metric", "wall", "--cpython", "python3.13 -X utf8"]
        )
        self.assertEqual((args.zipp, args.zipp_b, args.reps), ("a", "b", 3))
        self.assertEqual((args.group, args.only, args.timeout, args.metric), ("macro", "torch", 30.0, "wall"))
        engines = pb.build_engines(args)
        self.assertEqual([e.name for e in engines], ["zipp", "zipp-b", "cpython"])
        self.assertEqual(engines[2].prefix[-2:], ("-X", "utf8"))
        self.assertEqual(engines[0].prefix[-1], "py")

    def test_no_cpython_drops_the_engine_and_conflicts_with_cpython(self):
        args = pb.parse_args(["--zipp", "a", "--no-cpython"])
        self.assertEqual([e.name for e in pb.build_engines(args)], ["zipp"])
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            pb.parse_args(["--zipp", "a", "--no-cpython", "--cpython", "python3"])

    def test_rejects_bad_values(self):
        for bad in (["--reps", "0"], ["--timeout", "0"], ["--group", "huge"], ["--metric", "cpu"]):
            with self.subTest(bad=bad), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                pb.parse_args(["--zipp", "a", *bad])

    def test_split_command(self):
        self.assertEqual(pb.split_command("py -3.13", posix=True), ["py", "-3.13"])
        self.assertEqual(
            pb.split_command('"C:\\Program Files\\Python313\\python.exe" -X utf8', posix=False),
            ["C:\\Program Files\\Python313\\python.exe", "-X", "utf8"],
        )


class StatisticsTests(unittest.TestCase):
    def test_median_and_minimum_ignore_missing_values(self):
        self.assertEqual(pb.median([3.0, None, 1.0, 2.0]), 2.0)
        self.assertEqual(pb.median([4.0, 1.0]), 2.5)
        self.assertEqual(pb.minimum([None, 3.0, 2.0]), 2.0)
        self.assertIsNone(pb.median([]))
        self.assertIsNone(pb.minimum([None]))

    def test_geomean(self):
        self.assertAlmostEqual(pb.geomean([1.0, 4.0]), 2.0)
        self.assertAlmostEqual(pb.geomean([2.0, 8.0, None]), 4.0)
        self.assertAlmostEqual(pb.geomean([10.0]), 10.0)
        self.assertIsNone(pb.geomean([]))
        self.assertIsNone(pb.geomean([None, 0.0]))

    def test_ratio(self):
        self.assertAlmostEqual(pb.ratio(3.0, 1.5), 2.0)
        self.assertAlmostEqual(pb.ratio(0.25, 1.0), 0.25)
        self.assertIsNone(pb.ratio(1.0, 0.0))
        self.assertIsNone(pb.ratio(None, 1.0))
        self.assertIsNone(pb.ratio(1.0, None))

    def test_parse_work_time_takes_the_last_line(self):
        self.assertEqual(pb.parse_work_time("noise\r\n@bench-time 0.5\r\n@bench-time 0.250000\n"), 0.25)
        self.assertEqual(pb.parse_work_time("@bench-time 1e-3\n"), 0.001)
        self.assertIsNone(pb.parse_work_time("Traceback ...\n"))
        self.assertIsNone(pb.parse_work_time("x @bench-time 0.5\n"))


class DiscoveryTests(unittest.TestCase):
    def test_directives(self):
        self.assertEqual(pb.parse_directives("# zipp-bench: zipp-only\n\"\"\"doc\"\"\"\n"), {"zipp-only"})
        self.assertEqual(pb.parse_directives("#!/usr/bin/env python3\n# zipp-bench: zipp-only\nx = 1\n"), {"zipp-only"})
        self.assertEqual(pb.parse_directives("x = 1\n# zipp-bench: zipp-only\n"), frozenset())
        with self.assertRaises(ValueError):
            pb.parse_directives("# zipp-bench: zipp-onyl\n")

    def test_checked_in_suite_is_well_formed(self):
        benches = pb.discover(pb.SUITE_DIR)
        ids = [b.id for b in benches]
        self.assertEqual(len(ids), len(set(ids)))
        micro = [b for b in benches if b.group == "micro"]
        macro = [b for b in benches if b.group == "macro"]
        self.assertGreaterEqual(len(micro), 25)
        self.assertGreaterEqual(len(macro), 8)
        zipp_only = {b.id for b in benches if b.zipp_only}
        self.assertEqual(zipp_only, {"macro/torch_matmul", "macro/torch_mlp"})
        for b in benches:
            source = b.path.read_text(encoding="utf-8")
            with self.subTest(bench=b.id):
                self.assertIn('print("@bench-time %.6f" % elapsed, file=sys.stderr)', source)
                self.assertIn(f'print("{b.name}"', source)
                self.assertNotIn("import random", source)
                self.assertEqual(source.encode("utf-8"), source.encode("ascii", errors="replace"))
        for group in pb.GROUPS:
            with self.subTest(group=group):
                strays = [p.name for p in (pb.SUITE_DIR / group).iterdir() if p.suffix != ".py" and p.name != "__pycache__"]
                self.assertEqual(strays, [], "group directories are zipp's project VFS; keep them .py-only")

    def test_group_and_only_filters(self):
        self.assertTrue(all(b.group == "macro" for b in pb.discover(pb.SUITE_DIR, "macro")))
        only = pb.discover(pb.SUITE_DIR, "all", "dict_")
        self.assertEqual([b.id for b in only], ["micro/dict_get", "micro/dict_str_key"])


class ScheduleTests(unittest.TestCase):
    def test_every_rep_runs_every_pair_once_and_skips_cpython_for_zipp_only(self):
        benches = [bench("micro/a"), bench("micro/b"), bench("macro/t", zipp_only=True)]
        engines = [ZIPP, ZIPP_B, CPYTHON]
        schedule = pb.build_schedule(benches, engines, 3)
        self.assertEqual([e["seq"] for e in schedule], list(range(len(schedule))))
        for rep in range(3):
            pairs = [(e["bench"], e["engine"]) for e in schedule if e["rep"] == rep]
            self.assertEqual(len(pairs), len(set(pairs)))
            self.assertEqual(
                set(pairs),
                {(b, e) for b in ("micro/a", "micro/b") for e in ("zipp", "zipp-b", "cpython")}
                | {("macro/t", "zipp"), ("macro/t", "zipp-b")},
            )
        reps_in_order = [e["rep"] for e in schedule]
        self.assertEqual(reps_in_order, sorted(reps_in_order), "repetitions are not interleaved across each other")

    def test_engine_order_rotates_so_each_engine_leads_equally(self):
        benches = [bench("micro/a"), bench("micro/b")]
        engines = [ZIPP, CPYTHON]
        schedule = pb.build_schedule(benches, engines, 4)
        for bench_id in ("micro/a", "micro/b"):
            leaders = []
            for rep in range(4):
                rows = [e for e in schedule if e["rep"] == rep and e["bench"] == bench_id]
                leaders.append(rows[0]["engine"])
            with self.subTest(bench=bench_id):
                self.assertEqual(leaders.count("zipp"), 2)
                self.assertEqual(leaders.count("cpython"), 2)


class ValidationTests(unittest.TestCase):
    def test_all_equal_is_valid(self):
        benches = [bench("micro/a")]
        obs = [
            observation(0, "micro/a", "zipp", 0),
            observation(1, "micro/a", "cpython", 0),
            observation(2, "micro/a", "cpython", 1),
            observation(3, "micro/a", "zipp", 1),
        ]
        per, failures = pb.validate(benches, obs, cpython_enabled=True)
        self.assertEqual(failures, [])
        self.assertTrue(per["micro/a"]["valid"])
        self.assertEqual(per["micro/a"]["validated_against"], "cpython")
        self.assertEqual(per["micro/a"]["reference_seq"], 1)

    def test_zipp_checksum_mismatch_against_cpython_is_reported(self):
        benches = [bench("micro/a")]
        obs = [
            observation(0, "micro/a", "zipp", 0, stdout="a 1\nb 2\n"),
            observation(1, "micro/a", "cpython", 0, stdout="a 1\nb 3\n"),
        ]
        per, failures = pb.validate(benches, obs, cpython_enabled=True)
        self.assertFalse(per["micro/a"]["valid"])
        self.assertEqual(len(failures), 1)
        self.assertIn("micro/a [zipp rep 1]", failures[0])
        self.assertIn("line 2", failures[0])
        self.assertEqual(per["micro/a"]["mismatches"][0]["engine"], "zipp")

    def test_zipp_only_validates_zipp_b_against_zipp_a(self):
        benches = [bench("macro/t", zipp_only=True)]
        obs = [
            observation(0, "macro/t", "zipp-b", 0, stdout="t 2\n"),
            observation(1, "macro/t", "zipp", 0, stdout="t 1\n"),
        ]
        per, failures = pb.validate(benches, obs, cpython_enabled=True)
        self.assertEqual(per["macro/t"]["validated_against"], "zipp")
        self.assertFalse(per["macro/t"]["valid"])
        self.assertIn("zipp-b rep 1", failures[0])

    def test_rep_to_rep_drift_on_the_reference_engine_is_a_mismatch(self):
        benches = [bench("micro/a")]
        obs = [observation(0, "micro/a", "zipp", 0, stdout="x 1\n"), observation(1, "micro/a", "zipp", 1, stdout="x 2\n")]
        per, failures = pb.validate(benches, obs, cpython_enabled=False)
        self.assertEqual(per["micro/a"]["validated_against"], "zipp")
        self.assertFalse(per["micro/a"]["valid"])
        self.assertEqual(len(failures), 1)

    def test_failed_runs_and_missing_reference(self):
        benches = [bench("micro/a")]
        obs = [
            observation(0, "micro/a", "zipp", 0),
            observation(1, "micro/a", "cpython", 0, stdout="", error="exit code 1"),
        ]
        per, failures = pb.validate(benches, obs, cpython_enabled=True)
        self.assertFalse(per["micro/a"]["valid"])
        self.assertTrue(any("exit code 1" in f for f in failures))
        self.assertTrue(any("no successful cpython run" in f for f in failures))

    def test_empty_reference_output_is_not_a_checksum(self):
        benches = [bench("micro/a")]
        obs = [observation(0, "micro/a", "zipp", 0, stdout=""), observation(1, "micro/a", "cpython", 0, stdout="\n")]
        per, failures = pb.validate(benches, obs, cpython_enabled=True)
        self.assertFalse(per["micro/a"]["valid"])
        self.assertIn("printed no checksum", failures[0])


class SummaryTests(unittest.TestCase):
    def test_ratios_and_group_geomeans(self):
        benches = [bench("micro/a"), bench("micro/b"), bench("macro/t", zipp_only=True)]
        engines = [ZIPP, ZIPP_B, CPYTHON]
        obs = []
        work = {
            ("micro/a", "zipp"): [2.0, 4.0, 3.0], ("micro/a", "zipp-b"): [1.5, 1.5, 1.5], ("micro/a", "cpython"): [0.1, 0.1, 0.1],
            ("micro/b", "zipp"): [8.0, 8.0, 8.0], ("micro/b", "zipp-b"): [8.0, 8.0, 8.0], ("micro/b", "cpython"): [0.2, 0.2, 0.2],
            ("macro/t", "zipp"): [1.0, 1.0, 1.0], ("macro/t", "zipp-b"): [0.5, 0.5, 0.5],
        }
        for (bench_id, engine), values in work.items():
            for rep, value in enumerate(values):
                obs.append(observation(len(obs), bench_id, engine, rep, work=value, wall=value + 0.05))
        validation, failures = pb.validate(benches, obs, cpython_enabled=True)
        self.assertEqual(failures, [])
        summary = pb.summarize(benches, engines, obs, validation)
        a = summary["micro/a"]
        self.assertEqual(a["engines"]["zipp"]["work_s"]["median"], 3.0)
        self.assertEqual(a["engines"]["zipp"]["work_s"]["min"], 2.0)
        self.assertAlmostEqual(a["engines"]["zipp"]["overhead_s_median"], 0.05)
        self.assertAlmostEqual(a["ratios"]["work_s"]["zipp/cpython"], 30.0)
        self.assertAlmostEqual(a["ratios"]["work_s"]["zipp-b/zipp"], 0.5)
        self.assertAlmostEqual(a["ratios"]["work_s"]["zipp-b/cpython"], 15.0)
        self.assertNotIn("zipp/cpython", summary["macro/t"]["ratios"]["work_s"])

        geo = pb.group_geomeans(benches, summary)["work_s"]
        self.assertAlmostEqual(geo["micro"]["zipp/cpython"]["value"], math.sqrt(30.0 * 40.0))
        self.assertEqual(geo["micro"]["zipp/cpython"]["count"], 2)
        self.assertNotIn("zipp/cpython", geo["macro"])
        self.assertAlmostEqual(geo["macro"]["zipp-b/zipp"]["value"], 0.5)
        self.assertAlmostEqual(geo["all"]["zipp-b/zipp"]["value"], (0.5 * 1.0 * 0.5) ** (1 / 3))
        self.assertEqual(geo["all"]["zipp-b/zipp"]["count"], 3)

    def test_invalid_rows_are_excluded_from_geomeans(self):
        benches = [bench("micro/a"), bench("micro/b")]
        obs = [
            observation(0, "micro/a", "zipp", 0, work=2.0), observation(1, "micro/a", "cpython", 0, work=1.0),
            observation(2, "micro/b", "zipp", 0, work=9.0, stdout="bad\n"), observation(3, "micro/b", "cpython", 0, work=1.0),
        ]
        validation, failures = pb.validate(benches, obs, cpython_enabled=True)
        self.assertEqual(len(failures), 1)
        summary = pb.summarize(benches, [ZIPP, CPYTHON], obs, validation)
        self.assertIsNone(summary["micro/b"]["ratios"]["work_s"]["zipp/cpython"])
        geo = pb.group_geomeans(benches, summary)["work_s"]["micro"]["zipp/cpython"]
        self.assertAlmostEqual(geo["value"], 2.0)
        self.assertEqual(geo["excluded"], ["micro/b"])
        table = pb.render_table(benches, [ZIPP, CPYTHON], summary, pb.group_geomeans(benches, summary), "work_s")
        self.assertIn("INVALID", table)
        self.assertIn("geomean zipp/cpython (work_s): micro 2.00x (n=1, 1 invalid excluded)", table)


class RunOneTests(unittest.TestCase):
    def test_success_parses_work_time_and_normalizes_output(self):
        fake = mock.Mock(return_value={
            "returncode": 0, "stdout": b"sum 3\r\n", "stderr": b"@bench-time 0.125000\r\n",
            "timed_out": False, "start_error": None,
        })
        row = pb.run_one(["zipp", "py", "a.py"], Path("."), {}, 10.0, spawn_fn=fake)
        fake.assert_called_once()
        self.assertIsNone(row["error"])
        self.assertEqual(row["stdout"], "sum 3\n")
        self.assertEqual(row["work_s"], 0.125)
        self.assertGreaterEqual(row["wall_s"], 0.0)
        self.assertIsNone(row["stderr_tail"])

    def test_failures(self):
        cases = [
            ({"returncode": 1, "stdout": b"", "stderr": b"Traceback\n", "timed_out": False, "start_error": None}, "exit code 1"),
            ({"returncode": 0, "stdout": b"x\n", "stderr": b"", "timed_out": False, "start_error": None}, "no @bench-time"),
            ({"returncode": None, "stdout": b"", "stderr": b"", "timed_out": True, "start_error": None}, "timed out"),
            ({"returncode": None, "stdout": b"", "stderr": b"", "timed_out": False, "start_error": "not found"}, "could not start"),
        ]
        for result, expected in cases:
            with self.subTest(expected=expected):
                row = pb.run_one(["x"], Path("."), {}, 5.0, spawn_fn=lambda *a, result=result: result)
                self.assertIn(expected, row["error"])
                self.assertIsNone(row["work_s"])

    def test_spawn_kills_the_tree_on_timeout(self):
        proc = mock.Mock()
        proc.pid = 1234
        proc.returncode = -9
        proc.communicate.side_effect = [subprocess.TimeoutExpired(["x"], 1.0), (b"partial", b"")]
        with mock.patch.object(pb.subprocess, "Popen", return_value=proc), \
                mock.patch.object(pb, "_kill_tree") as kill:
            result = pb.spawn(["x"], Path("."), {}, 1.0)
        kill.assert_called_once_with(proc)
        self.assertTrue(result["timed_out"])
        self.assertEqual(result["stdout"], b"partial")


class MainTests(unittest.TestCase):
    def make_suite(self, root: Path) -> Path:
        suite = root / "suite"
        (suite / "micro").mkdir(parents=True)
        (suite / "macro").mkdir(parents=True)
        (suite / "micro" / "loop.py").write_text('print("loop", 1)\n', encoding="utf-8")
        (suite / "macro" / "tensor.py").write_text('# zipp-bench: zipp-only\nprint("tensor", 2)\n', encoding="utf-8")
        return suite

    def run_main(self, root: Path, argv, outputs):
        zipp = root / "zipp.exe"
        zipp.write_bytes(b"fake zipp binary")

        def fake_probe(argv_probe, timeout=30.0):
            if argv_probe[-1] == "--version":
                return 0, "zipp 0.0.18\nsource: abc\n", ""
            if argv_probe[-1] == "--json":
                return 0, json.dumps({"name": "zipp", "version": "0.0.18", "dirty": True}), ""
            if "-c" in argv_probe:
                return 0, json.dumps({
                    "version": "3.13.14 (main)", "version_info": [3, 13, 14, "final", 0],
                    "implementation": "cpython", "executable": None, "platform": "test",
                }), ""
            if argv_probe[0] == "git":
                return 0, "39304730\n", ""
            raise AssertionError(argv_probe)

        calls = []

        def fake_spawn(argv_run, cwd, env, timeout):
            calls.append((list(argv_run), Path(cwd).name))
            engine = "cpython" if argv_run[0] == "py" else "zipp"
            stdout = outputs[(engine, argv_run[-1])]
            return {"returncode": 0, "stdout": stdout.encode(), "stderr": b"@bench-time 0.200000\n",
                    "timed_out": False, "start_error": None}

        report = root / "out" / "report.json"
        full_argv = ["--zipp", str(zipp), "--cpython", "py -3.13", "--suite-dir", str(self.make_suite(root)),
                     "--json", str(report), *argv]
        with mock.patch.object(pb, "probe", side_effect=fake_probe), \
                mock.patch.object(pb, "spawn", side_effect=fake_spawn), \
                contextlib.redirect_stdout(io.StringIO()) as out, \
                contextlib.redirect_stderr(io.StringIO()) as err:
            code = pb.main(full_argv)
        return code, report, calls, out.getvalue(), err.getvalue()

    def test_report_shape_and_success(self):
        outputs = {("zipp", "loop.py"): "loop 1\n", ("cpython", "loop.py"): "loop 1\n", ("zipp", "tensor.py"): "tensor 2\n"}
        with tempfile.TemporaryDirectory() as tmp:
            code, report_path, calls, out, err = self.run_main(Path(tmp), ["--reps", "2"], outputs)
            self.assertEqual(code, 0, err)
            report = json.loads(report_path.read_text(encoding="utf-8"))
        self.assertEqual(len(calls), 6)  # (loop: zipp + cpython, tensor: zipp) x 2 reps
        self.assertEqual({cwd for _, cwd in calls}, {"micro", "macro"})
        self.assertNotIn(("py", "tensor.py"), {(a[0], a[-1]) for a, _ in calls})
        for key in ("schema_version", "generated_at_utc", "started_at_utc", "configuration", "host", "workspace",
                    "harness_sha256", "engines", "benchmarks", "schedule", "observations", "validation",
                    "summary", "geomeans", "all_valid", "failures"):
            self.assertIn(key, report)
        self.assertTrue(report["all_valid"])
        self.assertEqual(report["failures"], [])
        self.assertEqual(report["configuration"]["reps"], 2)
        zipp_meta = report["engines"][0]
        self.assertEqual(zipp_meta["name"], "zipp")
        self.assertEqual(len(zipp_meta["sha256"]), 64)
        self.assertEqual(zipp_meta["version"], "zipp 0.0.18")
        self.assertTrue(zipp_meta["build_identity"]["dirty"])
        self.assertEqual(report["engines"][1]["version_info"][:2], [3, 13])
        self.assertEqual(report["workspace"]["commit"], "39304730")
        self.assertEqual(len(report["observations"]), 6)
        loop = report["summary"]["micro/loop"]
        self.assertEqual(loop["validated_against"], "cpython")
        self.assertAlmostEqual(loop["engines"]["zipp"]["work_s"]["median"], 0.2)
        self.assertAlmostEqual(loop["ratios"]["work_s"]["zipp/cpython"], 1.0)
        self.assertEqual(report["summary"]["macro/tensor"]["validated_against"], "zipp")
        self.assertAlmostEqual(report["geomeans"]["work_s"]["micro"]["zipp/cpython"]["value"], 1.0)
        self.assertIn("geomean zipp/cpython (work_s): micro 1.00x (n=1)", out)

    def test_checksum_mismatch_exits_non_zero_and_is_recorded(self):
        outputs = {("zipp", "loop.py"): "loop 7\n", ("cpython", "loop.py"): "loop 1\n", ("zipp", "tensor.py"): "tensor 2\n"}
        with tempfile.TemporaryDirectory() as tmp:
            code, report_path, _, _, err = self.run_main(Path(tmp), ["--reps", "1"], outputs)
            report = json.loads(report_path.read_text(encoding="utf-8"))
        self.assertEqual(code, 1)
        self.assertFalse(report["all_valid"])
        self.assertIn("checksum differs", err)
        self.assertFalse(report["summary"]["micro/loop"]["valid"])
        self.assertIsNone(report["summary"]["micro/loop"]["ratios"]["work_s"]["zipp/cpython"])

    def test_missing_binary_and_empty_selection_are_usage_errors(self):
        with tempfile.TemporaryDirectory() as tmp, contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(pb.main(["--zipp", str(Path(tmp) / "missing.exe")]), 2)
            zipp = Path(tmp) / "zipp.exe"
            zipp.write_bytes(b"x")
            self.assertEqual(pb.main(["--zipp", str(zipp), "--only", "no-such-benchmark"]), 2)

    def test_report_path_never_clobbers(self):
        started = pb.dt.datetime(2026, 9, 15, 1, 2, 3, tzinfo=pb.dt.timezone.utc)
        with tempfile.TemporaryDirectory() as tmp:
            path = pb.default_report_path(started, Path(tmp) / "bench-results")
            self.assertEqual(path.name, "python-bench-20260915T010203Z.json")
            first = pb.write_report(path, {"a": 1})
            second = pb.write_report(path, {"a": 2})
            self.assertEqual(first, path)
            self.assertNotEqual(second, path)
            self.assertEqual(json.loads(first.read_text(encoding="utf-8")), {"a": 1})


if __name__ == "__main__":
    unittest.main()
