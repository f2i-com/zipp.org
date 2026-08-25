import contextlib
import importlib.util
import io
import json
import random
import sys
import tempfile
import unittest
from pathlib import Path


HARNESS_PATH = Path(__file__).with_name("bench_hostile.py")
SPEC = importlib.util.spec_from_file_location("zipp_bench_hostile", HARNESS_PATH)
assert SPEC and SPEC.loader
hostile = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = hostile
SPEC.loader.exec_module(hostile)


def process_result(
    elapsed,
    stdout=b"ok\n",
    *,
    returncode=0,
    timed_out=False,
    spawn_error=False,
    stderr=b"",
):
    return {
        "elapsed_s": elapsed,
        "stdout": stdout,
        "stderr": stderr,
        "returncode": returncode,
        "timed_out": timed_out,
        "spawn_error": spawn_error,
    }


def synthetic_case(
    case_id,
    *,
    category="scope",
    goal="script",
    family=None,
    variant=None,
):
    suffix = ".mjs" if goal == "module" else ".js"
    path = Path(f"/{case_id}{suffix}")
    return hostile.Case(
        id=case_id,
        entry=path,
        entry_rel=path.name,
        category=category,
        goal=goal,
        family=family,
        variant=variant,
        inputs=(path,),
        input_rels=(path.name,),
        timeout_s=30.0,
        features=(),
        description=None,
    )


class ManifestFixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def source(self, relative, text="console.log('ok');\n"):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
        return path

    def manifest(self, value):
        path = self.root / "manifest.json"
        path.write_text(json.dumps(value), encoding="utf-8")
        return path

    def valid_family_cases(self):
        self.source("scope/base.js")
        self.source("scope/iife.js")
        return [
            {
                "id": "scope-base",
                "entry": "scope/base.js",
                "category": "scope",
                "goal": "script",
                "family": "scope-form",
                "variant": "baseline",
            },
            {
                "id": "scope-iife",
                "entry": "scope/iife.js",
                "category": "scope",
                "goal": "script",
                "family": "scope-form",
                "variant": "iife",
            },
        ]


class ManifestValidationTests(ManifestFixture):
    def test_nested_scripts_modules_features_descriptions_and_inputs(self):
        cases = self.valid_family_cases()
        cases[0]["features"] = ["functions", "top-level-var"]
        cases[0]["description"] = "Baseline program"
        self.source("modules/entry.mjs")
        self.source("modules/dep.mjs", "export const n = 1;\n")
        cases.append(
            {
                "id": "module-graph",
                "entry": "modules/entry.mjs",
                "inputs": ["modules/entry.mjs", "modules/dep.mjs"],
                "category": "modules",
                "goal": "module",
                "features": ["modules", "live-bindings"],
            }
        )
        manifest = hostile.load_manifest(
            self.manifest(
                {
                    "schema_version": 1,
                    "description": "Hostile corpus",
                    "cases": cases,
                }
            )
        )
        self.assertEqual(manifest.description, "Hostile corpus")
        self.assertEqual([case.id for case in manifest.cases], [
            "scope-base",
            "scope-iife",
            "module-graph",
        ])
        self.assertEqual(manifest.cases[0].features, ("functions", "top-level-var"))
        self.assertEqual(manifest.cases[0].description, "Baseline program")
        self.assertEqual(manifest.cases[2].goal, "module")
        self.assertEqual(
            manifest.cases[2].input_rels,
            ("modules/entry.mjs", "modules/dep.mjs"),
        )

    def test_goal_is_inferred_from_entry_when_omitted(self):
        self.source("plain.js")
        self.source("mod.mjs")
        manifest = hostile.load_manifest(
            self.manifest(
                {
                    "schema_version": 1,
                    "cases": [
                        {"id": "plain", "entry": "plain.js", "category": "a"},
                        {"id": "mod", "entry": "mod.mjs", "category": "b"},
                    ],
                }
            )
        )
        self.assertEqual([case.goal for case in manifest.cases], ["script", "module"])

    def test_unknown_fields_are_rejected(self):
        cases = self.valid_family_cases()
        cases[0]["mode"] = "cold"
        with self.assertRaisesRegex(hostile.ManifestError, "unknown field.*mode"):
            hostile.load_manifest(
                self.manifest({"schema_version": 1, "cases": cases})
            )

    def test_duplicate_json_keys_are_rejected(self):
        path = self.root / "manifest.json"
        path.write_text(
            '{"schema_version":1,"schema_version":1,"cases":[]}',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(hostile.ManifestError, "duplicate JSON key"):
            hostile.load_manifest(path)

    def test_schema_version_is_a_strict_integer(self):
        path = self.manifest({"schema_version": 1.0, "cases": self.valid_family_cases()})
        with self.assertRaisesRegex(hostile.ManifestError, "schema_version"):
            hostile.load_manifest(path)

    def test_paths_must_exist_and_stay_inside_root(self):
        outside = self.root.parent / "outside-hostile.js"
        outside.write_text("console.log('outside')", encoding="utf-8")
        try:
            value = {
                "schema_version": 1,
                "cases": [
                    {
                        "id": "escape",
                        "entry": "../outside-hostile.js",
                        "category": "bad",
                    }
                ],
            }
            with self.assertRaisesRegex(hostile.ManifestError, "normalized relative"):
                hostile.load_manifest(self.manifest(value))
            value["cases"][0]["entry"] = "scope//bad.js"
            with self.assertRaisesRegex(hostile.ManifestError, "normalized relative"):
                hostile.load_manifest(self.manifest(value))
            value["cases"][0]["entry"] = "missing.js"
            with self.assertRaisesRegex(hostile.ManifestError, "readable file"):
                hostile.load_manifest(self.manifest(value))
        finally:
            outside.unlink(missing_ok=True)

    def test_entry_suffix_must_match_goal(self):
        self.source("wrong.js")
        value = {
            "schema_version": 1,
            "cases": [
                {
                    "id": "wrong",
                    "entry": "wrong.js",
                    "category": "modules",
                    "goal": "module",
                }
            ],
        }
        with self.assertRaisesRegex(hostile.ManifestError, "must end in '.mjs'"):
            hostile.load_manifest(self.manifest(value))

    def test_inputs_must_be_unique_and_include_entry(self):
        cases = self.valid_family_cases()
        self.source("fixture.json", "{}")
        cases[0]["inputs"] = ["fixture.json"]
        with self.assertRaisesRegex(hostile.ManifestError, "must include its entry"):
            hostile.load_manifest(
                self.manifest({"schema_version": 1, "cases": cases})
            )
        cases[0]["inputs"] = ["scope/base.js", "scope/base.js"]
        with self.assertRaisesRegex(hostile.ManifestError, "duplicate file"):
            hostile.load_manifest(
                self.manifest({"schema_version": 1, "cases": cases})
            )

    def test_family_requires_one_baseline_and_one_stressor(self):
        cases = self.valid_family_cases()
        cases[0]["variant"] = "original"
        with self.assertRaisesRegex(hostile.ManifestError, "no 'baseline'"):
            hostile.load_manifest(
                self.manifest({"schema_version": 1, "cases": cases})
            )
        cases = self.valid_family_cases()[:1]
        with self.assertRaisesRegex(hostile.ManifestError, "no stressor"):
            hostile.load_manifest(
                self.manifest({"schema_version": 1, "cases": cases})
            )

    def test_features_are_nonempty_and_unique(self):
        cases = self.valid_family_cases()
        cases[0]["features"] = ["closures", "closures"]
        with self.assertRaisesRegex(hostile.ManifestError, "must not contain duplicates"):
            hostile.load_manifest(
                self.manifest({"schema_version": 1, "cases": cases})
            )


class SelectionTests(ManifestFixture):
    def test_filters_are_conjunctive_and_manifest_order_is_preserved(self):
        cases = self.valid_family_cases()
        cases[0]["features"] = ["functions", "control"]
        cases[1]["features"] = ["functions", "iife"]
        manifest = hostile.load_manifest(
            self.manifest({"schema_version": 1, "cases": cases})
        )
        selected = hostile.select_cases(
            manifest,
            categories=("scope",),
            families=("scope-form",),
            features=("functions",),
        )
        self.assertEqual([case.id for case in selected], ["scope-base", "scope-iife"])
        selected = hostile.select_cases(manifest, features=("iife",))
        self.assertEqual([case.id for case in selected], ["scope-iife"])

    def test_unknown_and_empty_filters_fail(self):
        cases = self.valid_family_cases()
        cases[1]["features"] = ["iife"]
        manifest = hostile.load_manifest(
            self.manifest({"schema_version": 1, "cases": cases})
        )
        with self.assertRaisesRegex(ValueError, "unknown category"):
            hostile.select_cases(manifest, categories=("nope",))
        with self.assertRaisesRegex(ValueError, "selected no benchmark"):
            hostile.select_cases(
                manifest,
                case_ids=("scope-base",),
                features=("iife",),
            )

    def test_csv_validation(self):
        self.assertEqual(hostile.parse_csv("a,b", "x"), ("a", "b"))
        with self.assertRaisesRegex(ValueError, "nonempty"):
            hostile.parse_csv("a,", "x")
        with self.assertRaisesRegex(ValueError, "duplicates"):
            hostile.parse_csv("a,a", "x")


class CommandTests(unittest.TestCase):
    def test_engine_prefixes_are_goal_aware(self):
        self.assertEqual(hostile.engine_prefix("node", "node", "script"), ["node"])
        self.assertEqual(hostile.engine_prefix("node", "node", "module"), ["node"])
        self.assertEqual(
            hostile.engine_prefix("zipp", "zipp", "script"), ["zipp", "js"]
        )
        self.assertEqual(
            hostile.engine_prefix("zipp", "zipp", "module"), ["zipp", "mjs"]
        )

    def test_dirty_and_nonhead_provenance_require_explicit_flags(self):
        default = hostile.parse_args([])
        self.assertFalse(default.allow_dirty_engine)
        self.assertFalse(default.allow_nonhead_engine)
        allowed = hostile.parse_args(
            ["--allow-dirty-engine", "--allow-nonhead-engine"]
        )
        self.assertTrue(allowed.allow_dirty_engine)
        self.assertTrue(allowed.allow_nonhead_engine)


class MeasurementTests(unittest.TestCase):
    def test_schedule_commands_samples_and_exact_output(self):
        cases = (
            synthetic_case("script"),
            synthetic_case("module", category="modules", goal="module"),
        )
        calls = []

        def runner(cmd, path, *, timeout):
            calls.append((tuple(cmd), Path(path), timeout))
            startup = Path(path).name.startswith("zipp-hostile-empty-")
            engine = "zipp" if cmd[0] == "zipp-bin" else "node"
            if startup:
                return process_result(0.02 if engine == "zipp" else 0.04, b"")
            output = f"{Path(path).stem}:ok\n".encode()
            return process_result(0.20 if engine == "zipp" else 0.25, output)

        with contextlib.redirect_stderr(io.StringIO()):
            result = hostile.run_measurements(
                cases,
                node="node-bin",
                zipp="zipp-bin",
                reps=2,
                seed=7,
                runner=runner,
            )
        self.assertTrue(result["all_correct"])
        self.assertEqual(result["schedules"][0]["engine_order"], ["node", "zipp"])
        self.assertEqual(result["schedules"][1]["engine_order"], ["zipp", "node"])
        expected_order = list(cases)
        random.Random(7).shuffle(expected_order)
        self.assertEqual(
            result["schedules"][0]["case_order"],
            [case.id for case in expected_order],
        )
        for sample in result["samples"]["adjusted"]["zipp"]["script"]:
            self.assertAlmostEqual(sample, 0.18)
        self.assertTrue(any(cmd == ("zipp-bin", "js") for cmd, _, _ in calls))
        self.assertTrue(any(cmd == ("zipp-bin", "mjs") for cmd, _, _ in calls))
        self.assertTrue(any(path.suffix == ".mjs" and "empty" in path.name for _, path, _ in calls))

    def test_cross_engine_and_repeatability_mismatches_fail(self):
        case = synthetic_case("one")
        zipp_full_runs = 0

        def runner(cmd, path, *, timeout):
            nonlocal zipp_full_runs
            if Path(path).name.startswith("zipp-hostile-empty-"):
                return process_result(0.01, b"")
            if cmd[0] == "node-bin":
                return process_result(0.1, b"node\n")
            zipp_full_runs += 1
            return process_result(0.1, b"first\n" if zipp_full_runs == 1 else b"second\n")

        with contextlib.redirect_stderr(io.StringIO()):
            result = hostile.run_measurements(
                (case,),
                node="node-bin",
                zipp="zipp-bin",
                reps=2,
                seed=1,
                runner=runner,
            )
        self.assertFalse(result["all_correct"])
        self.assertTrue(
            any("not reproducible" in failure for failure in result["correctness_failures"])
        )
        self.assertTrue(
            any("differs from node" in failure for failure in result["correctness_failures"])
        )

    def test_process_failure_is_recorded_and_suppresses_summary(self):
        case = synthetic_case("one")

        def runner(cmd, path, *, timeout):
            if Path(path).name.startswith("zipp-hostile-empty-"):
                return process_result(0.01, b"")
            if cmd[0] == "zipp-bin":
                return process_result(
                    1.0,
                    b"",
                    returncode=None,
                    timed_out=True,
                )
            return process_result(0.1, b"ok\n")

        with contextlib.redirect_stderr(io.StringIO()):
            result = hostile.run_measurements(
                (case,),
                node="node-bin",
                zipp="zipp-bin",
                reps=1,
                seed=1,
                runner=runner,
            )
        self.assertTrue(any("timed out" in item for item in result["health_failures"]))
        self.assertIsNone(
            hostile.summarize((case,), result, seed=1, bootstrap_samples=10)
        )


class SummaryTests(unittest.TestCase):
    def test_category_geomeans_and_family_degradation(self):
        cases = (
            synthetic_case("base", family="scope-form", variant="baseline"),
            synthetic_case("iife", family="scope-form", variant="iife"),
            synthetic_case("other", category="application"),
        )
        samples = {
            metric: {
                "node": {
                    "base": [1.0, 1.0],
                    "iife": [2.0, 2.0],
                    "other": [2.0, 2.0],
                },
                "zipp": {
                    "base": [2.0, 2.0],
                    "iife": [6.0, 6.0],
                    "other": [1.0, 1.0],
                },
            }
            for metric in ("startup", "cold", "adjusted")
        }
        measurement = {
            "samples": samples,
            "health_failures": [],
            "correctness_failures": [],
            "all_correct": True,
        }
        summary = hostile.summarize(
            cases, measurement, seed=123, bootstrap_samples=20
        )
        assert summary is not None
        self.assertEqual(
            set(summary["suite_summaries"]["cold"]["categories"]),
            {"scope", "application"},
        )
        balanced = summary["suite_summaries"]["cold"]["category_balanced"]
        self.assertIsNotNone(balanced)
        # scope=sqrt(2*3), application=0.5; categories receive equal weight.
        self.assertAlmostEqual(
            balanced["geomean_paired_ratio"],
            ((2.0 * 3.0) ** 0.5 * 0.5) ** 0.5,
        )
        self.assertNotAlmostEqual(
            balanced["geomean_paired_ratio"],
            summary["suite_summaries"]["cold"]["overall"][
                "geomean_paired_ratio"
            ],
        )
        degradation = summary["degradations"][0]["metrics"]["cold"]
        self.assertEqual(
            degradation["engine_stressor_over_baseline"]["node"]["paired_ratio"],
            2.0,
        )
        self.assertEqual(
            degradation["engine_stressor_over_baseline"]["zipp"]["paired_ratio"],
            3.0,
        )
        self.assertEqual(
            degradation["relative_parity_ratio"]["paired_ratio"], 1.5
        )

    def test_nonpositive_adjusted_samples_are_reported_not_crashed(self):
        case = synthetic_case("one")
        samples = {
            "startup": {"node": {"one": [0.1]}, "zipp": {"one": [0.1]}},
            "cold": {"node": {"one": [0.2]}, "zipp": {"one": [0.2]}},
            "adjusted": {"node": {"one": [0.1]}, "zipp": {"one": [-0.01]}},
        }
        summary = hostile.summarize(
            (case,),
            {
                "samples": samples,
                "health_failures": [],
                "correctness_failures": [],
                "all_correct": True,
            },
            seed=1,
            bootstrap_samples=10,
        )
        assert summary is not None
        detail = summary["case_summaries"]["one"]["metrics"]["adjusted"]
        self.assertIsNone(detail["paired_ratio"])
        self.assertEqual(detail["nonpositive_pairs"], 1)

    def test_digest_drift_names_manifest_and_changed_inputs(self):
        self.assertEqual(
            hostile.digest_drift(
                "manifest-a",
                "manifest-b",
                {"a.js": "one", "stable.js": "same"},
                {"a.js": "two", "stable.js": "same"},
            ),
            [
                "manifest changed during run (manifest-a -> manifest-b)",
                "input a.js changed during run (one -> two)",
            ],
        )

    def test_digest_drift_names_imported_harness_changes(self):
        self.assertEqual(
            hostile.digest_drift(
                "same",
                "same",
                {"a.js": "same"},
                {"a.js": "same"},
                harnesses_before={"tools/bench.py": "old"},
                harnesses_after={"tools/bench.py": "new"},
            ),
            ["harness tools/bench.py changed during run (old -> new)"],
        )

    def test_publishable_requires_clean_provenance_sources_engine_and_output(self):
        self.assertTrue(
            hostile.artifact_publishable(
                [], [], [], all_correct=True, publication_reasons=[]
            )
        )
        self.assertFalse(
            hostile.artifact_publishable(
                ["dirty"], [], [], all_correct=True, publication_reasons=[]
            )
        )
        self.assertFalse(
            hostile.artifact_publishable(
                [], ["engine drift"], [], all_correct=True, publication_reasons=[]
            )
        )
        self.assertFalse(
            hostile.artifact_publishable(
                [], [], ["source drift"], all_correct=True, publication_reasons=[]
            )
        )
        self.assertFalse(
            hostile.artifact_publishable(
                [], [], [], all_correct=False, publication_reasons=[]
            )
        )
        self.assertFalse(
            hostile.artifact_publishable(
                [],
                [],
                [],
                all_correct=True,
                publication_reasons=["filtered corpus"],
            )
        )

    def test_publication_policy_requires_canonical_full_well_sampled_run(self):
        self.assertEqual(
            hostile.publication_policy_reasons(
                hostile.DEFAULT_MANIFEST,
                filtered=False,
                reps=hostile.DEFAULT_REPS,
                bootstrap_samples=hostile.core.BOOTSTRAP_SAMPLES,
                source_reason=None,
                environment={},
            ),
            [],
        )
        reasons = hostile.publication_policy_reasons(
            hostile.DEFAULT_MANIFEST.with_name("alternate.json"),
            filtered=True,
            reps=1,
            bootstrap_samples=1,
            source_reason="manifest, harness, or declared inputs differ from HEAD",
            environment={"NODE_OPTIONS": "<redacted>"},
        )
        self.assertEqual(len(reasons), 6)
        self.assertIn("alternate manifest", reasons[0])
        self.assertIn("filtered corpus", reasons[1])
        self.assertIn("repetitions", reasons[2])
        self.assertIn("bootstrap samples", reasons[3])
        self.assertIn("differ from HEAD", reasons[4])
        self.assertIn("environment", reasons[5])


if __name__ == "__main__":
    unittest.main()
