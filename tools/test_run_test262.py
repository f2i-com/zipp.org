"""Regression coverage for Test262 scoring and its command-line gate."""
import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SPEC = importlib.util.spec_from_file_location(
    "run_test262", Path(__file__).with_name("run_test262.py"))
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class ClassificationTests(unittest.TestCase):
    def negative(self, phase="parse", kind="SyntaxError"):
        return {"flags": [], "negative": {"phase": phase, "type": kind}}

    def test_negative_requires_language_error_exit(self):
        for code in (0, 101, -11, 3221225477):
            with self.subTest(code=code):
                self.assertEqual(runner.classify(
                    self.negative(), code, "", "zipp: SyntaxError: invalid token")[0], "FAIL")

    def test_error_type_is_not_matched_in_another_error_message(self):
        for message in ("zipp: TypeError: expected SyntaxError",
                        "zipp: NotSyntaxError: invalid token",
                        "zipp: syntaxerror: invalid token",
                        "zipp: cannot read 'SyntaxError.js': missing",
                        "diagnostic printed by test: SyntaxError"):
            with self.subTest(message=message):
                self.assertEqual(runner.classify(
                    self.negative(), 1, "", message)[0], "FAIL")

    def test_typed_error_and_known_legacy_compile_error(self):
        for message in ("SyntaxError: invalid token", *runner.LEGACY_SYNTAX_ERRORS):
            with self.subTest(message=message):
                self.assertEqual(runner.classify(
                    self.negative(), 1, "", "zipp: " + message), ("PASS", None))
        self.assertEqual(runner.classify(
            self.negative("runtime"), 1, "",
            "zipp: `await` is only valid inside an async function")[0], "FAIL")

    def test_body_execution_cannot_satisfy_early_error(self):
        self.assertEqual(runner.classify(self.negative(), 1,
            "Test262Error: should not be evaluated", "zipp: SyntaxError: wrong phase")[0], "FAIL")

    def test_runtime_negative_and_async_completion(self):
        self.assertEqual(runner.classify(self.negative("runtime", "TypeError"),
            1, "", "zipp: TypeError: bad receiver"), ("PASS", None))
        meta = {"flags": ["async"], "negative": None}
        self.assertEqual(runner.classify(meta, 0, "Test262:AsyncTestComplete", ""),
                         ("PASS", None))
        for code, out in ((0, "Test262:AsyncTestFailure: bad\nTest262:AsyncTestComplete"),
                          (1, "Test262:AsyncTestComplete"), (0, "")):
            self.assertEqual(runner.classify(meta, code, out, "")[0], "FAIL")


class LostFailureTests(unittest.TestCase):
    """A failure after the synchronous part exits 0; only the engine's
    ZIPP_REPORT_UNHANDLED lines reveal it."""
    positive = {"flags": [], "negative": None}
    asynchronous = {"flags": ["async"], "negative": None}

    def test_unhandled_assertion_in_promise_job_fails(self):
        err = runner.PROMISE_JOB_EXCEPTION + "Test262Error: deferred assertion Expected SameValue(1, 2)\n"
        verdict, sig = runner.classify(self.positive, 0, "", err)
        self.assertEqual(verdict, "FAIL")
        self.assertIn("Test262Error: deferred assertion", sig)

    def test_other_job_exceptions_stay_passes(self):
        # Test262 scores only its own harness assertions, so a TypeError
        # thrown inside an unobserved reaction is not a failure. An ordinary
        # `reject(value)` never reaches this function: the engine reports only
        # jobs that THREW, because built-ins/Promise/all/iter-step-err-no-
        # close.js and about fifteen siblings reject with a `Test262Error` on
        # purpose and never handle it.
        for reason in ("TypeError: boom", "3", "Error: Test262"):
            with self.subTest(reason=reason):
                self.assertEqual(runner.classify(
                    self.positive, 0, "", runner.PROMISE_JOB_EXCEPTION + reason),
                    ("PASS", None))

    def test_any_timer_exception_fails(self):
        for reason in ("TypeError: in-timer", "Test262Error: late"):
            with self.subTest(reason=reason):
                self.assertEqual(runner.classify(
                    self.positive, 0, "", runner.TIMER_EXCEPTION + reason)[0], "FAIL")

    def test_async_verdict_stays_with_done(self):
        # INTERPRETING.md makes the printed string the verdict for
        # `flags: [async]`, so a report line after Test262:AsyncTestComplete is
        # diagnostic only. staging/explicit-resource-management/await-using-in-
        # async-generator-body.js loses an assertion exactly this way and Node
        # 24 loses the same one, so it is an upstream test bug rather than a
        # gate failure (and the corrected tree has no patch for that file).
        out = "Test262:AsyncTestComplete\n"
        self.assertEqual(runner.classify(self.asynchronous, 0, out, ""), ("PASS", None))
        for reason in ("Test262Error: after done", "TypeError: x"):
            with self.subTest(reason=reason):
                self.assertEqual(runner.classify(
                    self.asynchronous, 0, out, runner.PROMISE_JOB_EXCEPTION + reason),
                    ("PASS", None))
        self.assertEqual(runner.classify(
            self.asynchronous, 0, out, runner.TIMER_EXCEPTION + "TypeError: t"),
            ("PASS", None))

    def test_text_printed_by_a_test_is_not_a_report(self):
        # Only the engine's own report lines count, not a test's output.
        self.assertEqual(runner.classify(
            self.positive, 0, "Test262Error: printed", "Test262Error: printed"), ("PASS", None))

    def test_report_lines_do_not_replace_the_error_diagnostic(self):
        # The CLI prints report lines before its final `zipp: ` error line.
        err = runner.PROMISE_JOB_EXCEPTION + "TypeError: other\nzipp: SyntaxError: bad"
        self.assertEqual(runner.classify(
            {"flags": [], "negative": {"phase": "parse", "type": "SyntaxError"}}, 1, "", err),
            ("PASS", None))

    def test_run_one_asks_the_engine_for_the_report(self):
        with tempfile.TemporaryDirectory() as temp:
            test = Path(temp) / "deferred.js"
            test.write_bytes(b"/*---\nflags: [raw]\n---*/\nPromise.resolve();\n")
            args = mock.Mock(t262=temp, zipp="zipp", timeout=5)
            completed = mock.Mock(returncode=0, stdout="",
                                  stderr=runner.PROMISE_JOB_EXCEPTION + "Test262Error: lost\n")
            with mock.patch.object(runner.subprocess, "run", return_value=completed) as run:
                verdict, _, _ = runner.run_one(args, None, (str(test), "sloppy"))
            self.assertEqual(run.call_args.kwargs["env"]["ZIPP_REPORT_UNHANDLED"], "1")
            self.assertEqual(verdict, "FAIL")


class RunnerGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.tests = self.root / "test" / "language"
        self.tests.mkdir(parents=True)
        (self.tests / "sample.js").write_text("/*---\nflags: [noStrict]\n---*/\n0;",
                                            encoding="utf-8")
        self.report = self.root / "results.json"
        self.manifest = self.root / "expected.txt"

    def invoke(self, verdict="PASS", extra=()):
        argv = ["run_test262.py", "--t262", str(self.root), "--zipp", sys.executable,
                "--json", str(self.report), *extra]
        def outcome(args, harness, job):
            return verdict, None if verdict == "PASS" else "test failure", job
        with mock.patch.object(sys, "argv", argv), \
             mock.patch.object(runner, "run_one", side_effect=outcome), \
             mock.patch.object(runner, "report_engine_identity", return_value={"commit": "engine"}), \
             mock.patch.object(runner, "report_corpus_identity", return_value={"commit": "corpus"}), \
             contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            return runner.main()

    def result(self):
        return json.loads(self.report.read_text(encoding="utf-8"))

    def test_unexpected_failure_sets_exit_code_and_json(self):
        self.assertEqual(self.invoke("FAIL"), 1)
        result = self.result()
        self.assertEqual(result["unexpected_failures"], ["language/sample.js [sloppy]"])
        self.assertEqual(result["counts"], {"pass": 0, "fail": 1, "skip": 0})
        self.assertFalse(result["gate_passed"])
        self.assertEqual(result["engine"]["commit"], "engine")
        self.assertEqual(result["corpus"]["commit"], "corpus")

    def test_exact_expectations_pass_but_stale_entries_fail(self):
        self.manifest.write_text("language/sample.js [sloppy]\n", encoding="utf-8")
        args = ("--expected-failures", str(self.manifest))
        self.assertEqual(self.invoke("FAIL", args), 0)
        self.assertTrue(self.result()["gate_passed"])
        self.assertEqual(self.invoke("PASS", args), 1)
        self.assertEqual(self.result()["stale_expectations"], ["language/sample.js [sloppy]"])

    def test_skip_never_passes_gate(self):
        self.assertEqual(self.invoke("SKIP"), 1)
        self.assertEqual(self.result()["skipped"],
                         [{"id": "language/sample.js [sloppy]", "reason": "test failure"}])

    def test_single_file_selection_and_fixture_filter(self):
        (self.tests / "helper_FIXTURE_extra.js").write_text("throw 1;", encoding="utf-8")
        (self.tests / ".zipptmp-leftover.js").write_text("throw 1;", encoding="utf-8")
        self.assertEqual(self.invoke(), 0)
        self.assertEqual(self.result()["files"], 1)
        self.assertEqual(self.invoke(extra=("--sub", "test/language/sample.js")), 0)
        self.assertEqual(self.result()["executions"], 1)

    def test_empty_selection_is_a_usage_error(self):
        (self.root / "empty").mkdir()
        with self.assertRaises(SystemExit) as exit_info:
            self.invoke(extra=("--sub", "empty"))
        self.assertEqual(exit_info.exception.code, 2)

    def test_invalid_or_duplicate_manifest_is_rejected(self):
        for content in ("not a failure ID\n", "language/sample.js [sloppy]\n" * 2):
            self.manifest.write_text(content, encoding="utf-8")
            with self.subTest(content=content), self.assertRaises(SystemExit) as exit_info:
                self.invoke(extra=("--expected-failures", str(self.manifest)))
            self.assertEqual(exit_info.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
