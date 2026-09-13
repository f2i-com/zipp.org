"""Evidence checks must fail if corpora drift or results hide failed executions."""
import copy
import contextlib
import io
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("dual", Path(__file__).with_name("run_test262_dual.py"))
dual = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(dual)


class ComparisonTests(unittest.TestCase):
    def setUp(self):
        self.original = dict(engine={"source": "test"}, selection=dict(sub="test", limit=0,
            include_intl402=False, no_staging=False), files=5, executions=9,
            counts={"pass": 0, "fail": 9, "skip": 0}, gate_passed=True,
            expected_failures=["original contradiction"])
        self.corrected = copy.deepcopy(self.original)
        self.corrected.update(counts={"pass": 9, "fail": 0, "skip": 0}, expected_failures=[])

    def test_original_failures_remain_visible_but_corrected_must_all_pass(self):
        self.assertTrue(dual.compare(self.original, self.corrected))
        self.corrected["counts"]["fail"] = 1
        self.assertFalse(dual.compare(self.original, self.corrected))

    def test_skips_or_expectations_cannot_make_corrected_suite_pass(self):
        self.corrected["counts"]["skip"] = 1
        self.assertFalse(dual.compare(self.original, self.corrected))
        self.corrected["counts"]["skip"] = 0
        self.corrected["expected_failures"] = ["hidden failure"]
        self.assertFalse(dual.compare(self.original, self.corrected))

    def test_different_builds_or_test_selections_are_rejected(self):
        for key, value in (("engine", {}), ("files", 4), ("executions", 8)):
            with self.subTest(key=key):
                changed = copy.deepcopy(self.corrected)
                changed[key] = value
                with self.assertRaises(ValueError):
                    dual.compare(self.original, changed)
        for report in (self.original, self.corrected):
            report["selection"]["no_staging"] = True
        with self.assertRaises(ValueError):
            dual.compare(self.original, self.corrected)


class CheckoutTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "test.js").write_bytes(b"original\n")
        self.manifest = dict(upstream_commit="pinned", corrections=[dict(path="test.js",
            original_sha256=dual.digest(self.root / "test.js"), corrected_sha256="corrected")])

    def check(self, responses, corrected=False):
        with mock.patch.object(dual, "git", side_effect=responses):
            dual.verify_tree(self.root, self.manifest, corrected)

    def test_original_requires_pinned_clean_byte_faithful_checkout(self):
        self.check(["pinned", "i/lf w/lf attr/ test.js", "", ""])
        for responses in (["wrong"], ["pinned", "i/lf w/crlf attr/ test.js"],
                          ["pinned", "i/lf w/lf attr/ test.js", "extra.js"]):
            with self.subTest(responses=responses), self.assertRaises(ValueError):
                self.check(responses)

    def test_untracked_tests_cannot_change_the_original_execution_set(self):
        with self.assertRaisesRegex(ValueError, "unexpected untracked"):
            self.check(["pinned", "i/lf w/lf attr/ test.js", "", "extra.js"])

    def test_reuse_cannot_accept_a_modified_corrected_checkout(self):
        destination = self.root / "corrected"
        destination.mkdir()
        with mock.patch.object(dual, "verify_tree", side_effect=[None, ValueError("file hash mismatch")]), \
                mock.patch.object(dual.subprocess, "run") as command:
            with self.assertRaisesRegex(ValueError, "file hash mismatch"):
                dual.prepare(self.root, destination, self.manifest, reuse=True)
            command.assert_not_called()

    def test_corrected_file_content_must_match_manifest(self):
        with self.assertRaisesRegex(ValueError, "hash mismatch"):
            self.check(["pinned", "i/lf w/lf attr/ test.js", "test.js"], corrected=True)

    def test_unlisted_changes_are_rejected_in_corrected_tree(self):
        with self.assertRaisesRegex(ValueError, "unexpected Test262 edits"):
            self.check(["pinned", "i/lf w/lf attr/ test.js", "test.js\nextra.js"], corrected=True)

    def test_dual_cli_reaches_real_runner_and_keeps_both_failed_reports(self):
        # A non-JavaScript executable deliberately fails this tiny corpus.
        # Exercise real argparse boundaries and JSON output, not a fake scorer.
        for name in ("original", "corrected"):
            tests = self.root / name / "test"
            tests.mkdir(parents=True)
            (tests / "sample.js").write_text("/*---\nflags: [noStrict]\n---*/\n0;", encoding="utf-8")
            harness = self.root / name / "harness"
            harness.mkdir()
            for include in ("assert.js", "sta.js"):
                (harness / include).write_text("", encoding="utf-8")
        output = self.root / "evidence"
        real_run = subprocess.run
        diagnostics = []
        def invoke(*args, **kwargs):
            result = real_run(*args, capture_output=True, **kwargs)
            diagnostics.append(result.stderr.decode("utf-8", "replace"))
            return result
        with mock.patch.object(dual, "prepare"), mock.patch.object(dual, "verify_tree"), \
                mock.patch.object(dual.subprocess, "run", side_effect=invoke), \
                contextlib.redirect_stdout(io.StringIO()):
            result = dual.main(["--t262", str(self.root / "original"),
                "--corrected-tree", str(self.root / "corrected"), "--zipp", sys.executable,
                "--jobs", "1", "--timeout", "5", "--output-dir", str(output)])
        self.assertEqual(result, 1)
        report = json.loads((output / "comparison.json").read_text())
        self.assertEqual(report["runner_errors"], [], "\n".join(diagnostics))
        self.assertEqual(set(report["runs"]), {"upstream", "corrected"})
        self.assertFalse(report["gate_passed"])


if __name__ == "__main__":
    unittest.main()
