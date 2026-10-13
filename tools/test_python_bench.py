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
TORCH = pb.Engine("torch", "torch", ("py", "-3.11"))

TORCH_PROGRAM = '''# zipp-bench: zipp-only
"""doc"""
import sys
import time

import torch


def bench():
    return torch.ones(2).sum().item()


t0 = time.perf_counter()
result = bench()
elapsed = time.perf_counter() - t0
print("tiny", "%.1f" % result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
'''


def torch_bench(bench_id="macro/t"):
    group, name = bench_id.split("/")
    return pb.Benchmark(bench_id, group, name, Path(f"/suite/{bench_id}.py"), True, "0" * 64, uses_torch=True)


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
        self.assertIsNone(args.torch_cpython)
        self.assertEqual(args.torch_threads, 1)
        self.assertEqual(args.warm, 1)
        self.assertNotIn("torch", [e.name for e in pb.build_engines(args)])

    def test_torch_reference_and_warm_options(self):
        args = pb.parse_args(["--zipp", "a", "--no-cpython", "--torch-cpython", "py -3.11", "--torch-threads", "0", "--warm", "5"])
        engines = pb.build_engines(args)
        self.assertEqual([(e.name, e.kind) for e in engines], [("zipp", "zipp"), ("torch", "torch")])
        self.assertEqual(engines[1].prefix, ("py", "-3.11"))
        self.assertEqual((args.torch_threads, args.warm), (0, 5))
        for bad in (["--warm", "0"], ["--torch-threads", "-1"]):
            with self.subTest(bad=bad), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                pb.parse_args(["--zipp", "a", *bad])

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

    def test_warm_runs_time_the_fastest_after_the_first(self):
        times = pb.parse_work_times("@bench-time 0.9\r\n@bench-time 0.3\n@bench-time 0.2\n@bench-time 0.25\n")
        self.assertEqual(times, [0.9, 0.3, 0.2, 0.25])
        self.assertEqual(pb.warm_and_cold(times, 4), (0.2, 0.9))
        self.assertEqual(pb.warm_and_cold(times, 1), (0.25, None))
        self.assertEqual(pb.warm_and_cold([0.9], 3), (None, 0.9))
        self.assertEqual(pb.warm_and_cold([], 3), (None, None))


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
        torch_rows = {"macro/torch_" + n for n in ("cnn", "dataloader", "matmul", "matmul_big", "mlp", "rnn", "small_ops", "transformer")}
        zipp_only = {b.id for b in benches if b.zipp_only}
        self.assertEqual(zipp_only, torch_rows)
        self.assertEqual({b.id for b in benches if b.uses_torch}, torch_rows)
        for b in benches:
            source = b.path.read_text(encoding="utf-8")
            with self.subTest(bench=b.id):
                self.assertIn('print("@bench-time %.6f" % elapsed, file=sys.stderr)', source)
                self.assertIn(f'print("{b.name}"', source)
                self.assertNotIn("import random", source)
                self.assertEqual(source.encode("utf-8"), source.encode("ascii", errors="replace"))
                # --warm repeats the program from this line on.
                self.assertIn(pb.TIMED_TAIL_MARKER, source.replace("\r\n", "\n"))
                pb.derived_source(source, 3, 1)
        for group in pb.GROUPS:
            with self.subTest(group=group):
                strays = [p.name for p in (pb.SUITE_DIR / group).iterdir() if p.suffix != ".py" and p.name != "__pycache__"]
                self.assertEqual(strays, [], "group directories are zipp's project VFS; keep them .py-only")

    def test_group_and_only_filters(self):
        self.assertTrue(all(b.group == "macro" for b in pb.discover(pb.SUITE_DIR, "macro")))
        only = pb.discover(pb.SUITE_DIR, "all", "dict_")
        self.assertEqual([b.id for b in only], ["micro/dict_get", "micro/dict_str_key"])


class DerivedProgramTests(unittest.TestCase):
    def test_warm_repeats_the_timed_tail_and_threads_are_set_first(self):
        text = pb.derived_source(TORCH_PROGRAM.replace("\n", "\r\n"), 3, 1)
        head, _, loop = text.partition("for _zipp_bench_rep in range(3):\n")
        self.assertTrue(loop, text)
        self.assertIn("def bench():", head)
        self.assertTrue(head.endswith("import torch as _zipp_bench_torch\n_zipp_bench_torch.set_num_threads(1)\n"))
        self.assertIn("    t0 = time.perf_counter()\n    result = bench()\n", loop)
        self.assertIn('    print("@bench-time %.6f" % elapsed, file=sys.stderr)\n', loop)
        self.assertNotIn("\r", text)
        namespace = {}
        out = io.StringIO()
        with contextlib.redirect_stderr(io.StringIO()) as err, contextlib.redirect_stdout(out):
            fake_torch = mock.MagicMock()
            fake_torch.ones.return_value.sum.return_value.item.return_value = 2.0
            with mock.patch.dict(sys.modules, {"torch": fake_torch}):
                exec(compile(text, "derived", "exec"), namespace)
        self.assertEqual(out.getvalue(), "tiny 2.0\n" * 3)
        self.assertEqual(len(pb.parse_work_times(err.getvalue())), 3)
        fake_torch.set_num_threads.assert_called_once_with(1)

    def test_single_run_without_threads_is_the_program_itself(self):
        self.assertEqual(pb.derived_source(TORCH_PROGRAM, 1, None), TORCH_PROGRAM)
        self.assertEqual(pb.derived_source(TORCH_PROGRAM, 1, 0), TORCH_PROGRAM)
        with self.assertRaises(ValueError):
            pb.derived_source("print(1)\n", 2, None)

    def test_prepare_scripts_copies_only_what_must_change(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "macro").mkdir()
            path = root / "macro" / "t.py"
            path.write_text(TORCH_PROGRAM, encoding="utf-8")
            b = pb.Benchmark("macro/t", "macro", "t", path, True, "0" * 64, uses_torch=True)
            plain = pb.prepare_scripts([b], [ZIPP, TORCH], 1, 1, root / "work")
            self.assertEqual(plain[("macro/t", "zipp")], (path, path.parent))
            script, cwd = plain[("macro/t", "torch")]
            self.assertNotEqual(cwd, path.parent)
            self.assertEqual(script.parent, cwd)
            self.assertEqual(script.name, "t.py")
            self.assertIn("set_num_threads(1)", script.read_text(encoding="utf-8"))
            warm = pb.prepare_scripts([b], [ZIPP, TORCH], 4, 0, root / "work2")
            zscript, zcwd = warm[("macro/t", "zipp")]
            self.assertNotEqual(zcwd, path.parent)
            ztext = zscript.read_text(encoding="utf-8")
            self.assertIn("range(4)", ztext)
            self.assertNotIn("set_num_threads", ztext)
            self.assertNotIn("set_num_threads", warm[("macro/t", "torch")][0].read_text(encoding="utf-8"))


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

    def test_torch_reference_runs_only_the_zipp_only_torch_programs(self):
        plain_zipp_only = bench("macro/z", zipp_only=True)
        benches = [bench("micro/a"), plain_zipp_only, torch_bench()]
        engines = [ZIPP, CPYTHON, TORCH]
        self.assertEqual([e.name for e in pb.applicable_engines(benches[0], engines)], ["zipp", "cpython"])
        self.assertEqual([e.name for e in pb.applicable_engines(plain_zipp_only, engines)], ["zipp"])
        self.assertEqual([e.name for e in pb.applicable_engines(benches[2], engines)], ["zipp", "torch"])
        pairs = {(e["bench"], e["engine"]) for e in pb.build_schedule(benches, engines, 2)}
        self.assertEqual({p for p in pairs if p[1] == "torch"}, {("macro/t", "torch")})

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

    def test_torch_reference_difference_is_recorded_not_failed(self):
        benches = [torch_bench()]
        obs = [
            observation(0, "macro/t", "zipp", 0, stdout="t 1.0\n"),
            observation(1, "macro/t", "torch", 0, stdout="t 1.1\n"),
            observation(2, "macro/t", "zipp-b", 0, stdout="t 1.0\n"),
        ]
        per, failures = pb.validate(benches, obs, cpython_enabled=False)
        self.assertEqual(failures, [])
        self.assertTrue(per["macro/t"]["valid"])
        self.assertEqual([m["engine"] for m in per["macro/t"]["torch_mismatches"]], ["torch"])
        summary = pb.summarize(benches, [ZIPP, ZIPP_B, TORCH], obs, per)
        self.assertTrue(summary["macro/t"]["torch_differs"])
        self.assertAlmostEqual(summary["macro/t"]["ratios"]["work_s"]["zipp/torch"], 1.0)
        table = pb.render_table(benches, [ZIPP, ZIPP_B, TORCH], summary, pb.group_geomeans(benches, summary), "work_s")
        self.assertIn("torch-differs", table)
        self.assertIn("geomean zipp/torch (work_s): macro 1.00x (n=1)", table)

    def test_a_failed_torch_run_is_a_failure(self):
        benches = [torch_bench()]
        obs = [observation(0, "macro/t", "zipp", 0), observation(1, "macro/t", "torch", 0, stdout="", error="exit code 1")]
        per, failures = pb.validate(benches, obs, cpython_enabled=False)
        self.assertFalse(per["macro/t"]["valid"])
        self.assertTrue(any("torch rep 1" in f for f in failures))

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

    def test_warm_run_keeps_the_fastest_warm_time_and_the_cold_one(self):
        fake = mock.Mock(return_value={
            "returncode": 0, "stdout": b"t 1\nt 1\nt 1\n", "stderr": b"@bench-time 0.8\n@bench-time 0.12\n@bench-time 0.1\n",
            "timed_out": False, "start_error": None,
        })
        row = pb.run_one(["py", "t.py"], Path("."), {}, 10.0, spawn_fn=fake, warm=3)
        self.assertIsNone(row["error"])
        self.assertEqual((row["work_s"], row["cold_s"]), (0.1, 0.8))
        self.assertEqual(row["work_times_s"], [0.8, 0.12, 0.1])
        short = mock.Mock(return_value={"returncode": 0, "stdout": b"t 1\n", "stderr": b"@bench-time 0.8\n",
                                        "timed_out": False, "start_error": None})
        row = pb.run_one(["py", "t.py"], Path("."), {}, 10.0, spawn_fn=short, warm=3)
        self.assertIn("1 @bench-time lines for --warm 3", row["error"])
        self.assertIsNone(row["work_s"])

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

    def test_torch_reference_with_warm_runs(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            suite = self.make_suite(root)
            (suite / "macro" / "torch_tiny.py").write_text(TORCH_PROGRAM, encoding="utf-8")
            zipp = root / "zipp.exe"
            zipp.write_bytes(b"fake zipp binary")

            def fake_probe(argv_probe, timeout=30.0):
                if argv_probe[-1] in ("--version", "--json"):
                    return 0, json.dumps({"name": "zipp"}) if argv_probe[-1] == "--json" else "zipp 0.0.23\n", ""
                if "-c" in argv_probe and "torch" in argv_probe[-1]:
                    return 0, json.dumps({"torch": "2.11.0", "threads": 16}), ""
                if "-c" in argv_probe:
                    return 0, json.dumps({"version": "3.11.9", "version_info": [3, 11, 9], "implementation": "cpython",
                                          "executable": None}), ""
                if argv_probe[0] == "git":
                    return 0, "abc\n", ""
                raise AssertionError(argv_probe)

            calls = []

            def fake_spawn(argv_run, cwd, env, timeout):
                script = Path(cwd) / argv_run[-1]
                calls.append((argv_run[0], argv_run[-1], script.read_text(encoding="utf-8")))
                torch_run = argv_run[0] == "py"
                name = Path(argv_run[-1]).stem
                stdout = ("%s 1\n" % name) * 2 if name != "torch_tiny" else ("tiny 2.0\n" * 2)
                stderr = b"@bench-time 0.900000\n@bench-time 0.100000\n" if torch_run else b"@bench-time 0.400000\n@bench-time 0.300000\n"
                return {"returncode": 0, "stdout": stdout.encode(), "stderr": stderr, "timed_out": False, "start_error": None}

            report_path = root / "out" / "r.json"
            argv = ["--zipp", str(zipp), "--no-cpython", "--torch-cpython", "py -3.11", "--warm", "2",
                    "--suite-dir", str(suite), "--json", str(report_path), "--reps", "1", "--only", "torch"]
            with mock.patch.object(pb, "probe", side_effect=fake_probe), \
                    mock.patch.object(pb, "spawn", side_effect=fake_spawn), \
                    contextlib.redirect_stdout(io.StringIO()) as out, \
                    contextlib.redirect_stderr(io.StringIO()) as err:
                code = pb.main(argv)
            self.assertEqual(code, 0, err.getvalue())
            report = json.loads(report_path.read_text(encoding="utf-8"))
        torch_calls = [c for c in calls if c[0] == "py"]
        self.assertEqual([c[1] for c in torch_calls], ["torch_tiny.py"])
        self.assertIn("set_num_threads(1)", torch_calls[0][2])
        self.assertTrue(all("range(2)" in c[2] for c in calls if c[1] == "torch_tiny.py"))
        tiny = report["summary"]["macro/torch_tiny"]
        self.assertAlmostEqual(tiny["engines"]["torch"]["work_s"]["min"], 0.1)
        self.assertAlmostEqual(tiny["engines"]["torch"]["cold_s"]["min"], 0.9)
        self.assertAlmostEqual(tiny["engines"]["zipp"]["work_s"]["min"], 0.3)
        self.assertAlmostEqual(tiny["ratios"]["work_s"]["zipp/torch"], 3.0)
        self.assertEqual(report["configuration"]["torch_threads"], 1)
        self.assertEqual(report["configuration"]["warm"], 2)
        self.assertEqual(report["engines"][-1]["torch_version"], "2.11.0")
        self.assertIn("first run in the process", out.getvalue())
        self.assertNotIn("macro/tensor", {o["bench"] for o in report["observations"] if o["engine"] == "torch"})

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
