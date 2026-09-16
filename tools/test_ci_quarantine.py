"""Regression coverage for the CI test quarantine's exact-name guarantee."""
import contextlib
import datetime as dt
import importlib.util
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SPEC = importlib.util.spec_from_file_location(
    "ci_quarantine", Path(__file__).with_name("ci_quarantine.py"))
quarantine = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(quarantine)

TODAY = dt.date(2026, 9, 15)


def entry(test, binary="sample", review_by="2026-12-31"):
    return {"binary": binary, "test": test, "owner": "engine", "reason": "planner",
            "observed": "run 1", "last_reviewed": "2026-09-11", "review_by": review_by}


class EmitSkipsTests(unittest.TestCase):
    def test_skips_are_exact(self):
        # Without --exact, libtest treats `--skip sroa_mechanism_in_function`
        # as a substring and would also hide `sroa_mechanism_in_function_v2`.
        args = quarantine.emit_skips([entry("alpha"), entry("beta")]).split()
        self.assertEqual(args, ["--exact", "--skip", "alpha", "--skip", "beta"])

    def test_command_line_prints_exact_skips(self):
        out = io.StringIO()
        with mock.patch.object(quarantine, "load", return_value=[entry("alpha")]), \
             mock.patch.object(sys, "argv", ["ci_quarantine.py", "--emit-skips"]), \
             contextlib.redirect_stdout(out):
            self.assertEqual(quarantine.main(), 0)
        self.assertEqual(out.getvalue().split(), ["--exact", "--skip", "alpha"])

    def test_checked_in_manifest_emits_exact_skips(self):
        args = quarantine.emit_skips(quarantine.load()).split()
        self.assertEqual(args[0], "--exact")
        self.assertEqual(args.count("--exact"), 1)


class CheckTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.crates = Path(self.temp.name) / "crates"
        self.vm_tests = self.crates / "zipp-vm" / "tests"
        self.vm_tests.mkdir(parents=True)
        (self.vm_tests / "sample.rs").write_text(
            "#[test]\nfn mechanism_in_function() {}\n\n"
            "mod nested {\n    #[test]\n    fn mechanism_in_module() {}\n}\n",
            encoding="utf-8")

    def check(self, *entries):
        return quarantine.check(list(entries), TODAY, self.crates)

    def test_top_level_test_is_accepted(self):
        self.assertEqual(self.check(entry("mechanism_in_function")), [])

    def test_longer_name_elsewhere_is_not_a_collision(self):
        (self.vm_tests / "other.rs").write_text(
            "#[test]\nfn mechanism_in_function_v2() {}\n", encoding="utf-8")
        self.assertEqual(self.check(entry("mechanism_in_function")), [])

    def test_nested_test_cannot_be_matched_exactly(self):
        problems = self.check(entry("mechanism_in_module"))
        self.assertEqual(len(problems), 1)
        self.assertIn("not a top-level test function", problems[0])

    def test_same_name_in_another_binary_is_rejected(self):
        # The skip list reaches every binary of `cargo test --workspace`.
        (self.vm_tests / "other.rs").write_text(
            "#[test]\nfn mechanism_in_function() {}\n", encoding="utf-8")
        cli_tests = self.crates / "zipp-cli" / "tests"
        cli_tests.mkdir(parents=True)
        (cli_tests / "cli.rs").write_text(
            "#[test]\nfn mechanism_in_function() {}\n", encoding="utf-8")
        problems = self.check(entry("mechanism_in_function"))
        self.assertEqual(len(problems), 2)
        self.assertTrue(all("would be skipped too" in p for p in problems))
        self.assertIn("other.rs", problems[0] + problems[1])
        self.assertIn("cli.rs", problems[0] + problems[1])

    def test_stale_pattern_and_expired_entries_are_rejected(self):
        problems = self.check(entry("missing_test"), entry("mechanism_*"),
                              entry("mechanism_in_function", review_by="2026-09-14"))
        self.assertEqual(len(problems), 3)
        self.assertIn("stale entry", problems[0])
        self.assertIn("exact identifiers", problems[1])
        self.assertIn("expired", problems[2])

    def test_checked_in_manifest_is_valid_until_its_review_date(self):
        self.assertEqual(quarantine.check(quarantine.load(), TODAY), [])


if __name__ == "__main__":
    unittest.main()
