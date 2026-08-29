import contextlib
import importlib.util
import io
import json
import random
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


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
        self.assertEqual(
            [case.id for case in manifest.cases],
            [
                "scope-base",
                "scope-iife",
                "module-graph",
            ],
        )
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
            hostile.load_manifest(self.manifest({"schema_version": 1, "cases": cases}))

    def test_duplicate_json_keys_are_rejected(self):
        path = self.root / "manifest.json"
        path.write_text(
            '{"schema_version":1,"schema_version":1,"cases":[]}',
            encoding="utf-8",
        )
        with self.assertRaisesRegex(hostile.ManifestError, "duplicate JSON key"):
            hostile.load_manifest(path)

    def test_schema_version_is_a_strict_integer(self):
        path = self.manifest(
            {"schema_version": 1.0, "cases": self.valid_family_cases()}
        )
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
            hostile.load_manifest(self.manifest({"schema_version": 1, "cases": cases}))
        cases[0]["inputs"] = ["scope/base.js", "scope/base.js"]
        with self.assertRaisesRegex(hostile.ManifestError, "duplicate file"):
            hostile.load_manifest(self.manifest({"schema_version": 1, "cases": cases}))

    def test_family_requires_one_baseline_and_one_stressor(self):
        cases = self.valid_family_cases()
        cases[0]["variant"] = "original"
        with self.assertRaisesRegex(hostile.ManifestError, "no 'baseline'"):
            hostile.load_manifest(self.manifest({"schema_version": 1, "cases": cases}))
        cases = self.valid_family_cases()[:1]
        with self.assertRaisesRegex(hostile.ManifestError, "no stressor"):
            hostile.load_manifest(self.manifest({"schema_version": 1, "cases": cases}))

    def test_features_are_nonempty_and_unique(self):
        cases = self.valid_family_cases()
        cases[0]["features"] = ["closures", "closures"]
        with self.assertRaisesRegex(
            hostile.ManifestError, "must not contain duplicates"
        ):
            hostile.load_manifest(self.manifest({"schema_version": 1, "cases": cases}))


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
            hostile.engine_prefix("bun", ["bun", "run"], "module"),
            ["bun", "run"],
        )
        self.assertEqual(
            hostile.engine_prefix("deno", ["deno", "run"], "script"),
            ["deno", "run"],
        )
        self.assertEqual(
            hostile.engine_prefix("zipp", "zipp", "script"), ["zipp", "js"]
        )
        self.assertEqual(
            hostile.engine_prefix("zipp", "zipp", "module"), ["zipp", "mjs"]
        )

    def test_dirty_and_nonhead_provenance_require_explicit_flags(self):
        default = hostile.parse_args([])
        self.assertEqual(default.engines, "node,bun,deno,zipp")
        self.assertFalse(default.allow_dirty_engine)
        self.assertFalse(default.allow_nonhead_engine)
        allowed = hostile.parse_args(["--allow-dirty-engine", "--allow-nonhead-engine"])
        self.assertTrue(allowed.allow_dirty_engine)
        self.assertTrue(allowed.allow_nonhead_engine)

    def test_native_launcher_resolution_is_used_for_every_external_engine(self):
        calls = []

        def resolve(
            name,
            command,
            timeout,
            *,
            process_env=None,
            fresh_environment=False,
        ):
            calls.append(
                (name, command, timeout, process_env, fresh_environment)
            )
            return [f"/native/{name}", *command[1:]]

        with (
            mock.patch.object(
                hostile.core,
                "resolved_executable",
                side_effect=lambda command: Path(f"/wrapper/{command[0]}"),
            ),
            mock.patch.object(
                hostile.core, "canonical_engine_command", side_effect=resolve
            ),
        ):
            commands = hostile.resolve_engine_commands(
                hostile.CANONICAL_ENGINE_NAMES,
                node="node-wrapper",
                bun="bun-wrapper",
                deno="deno-wrapper",
                zipp="zipp-bin",
                timeout=9.0,
                process_env={"PATH": "/isolated"},
            )
        self.assertEqual(
            [name for name, _, _, _, _ in calls], ["node", "bun", "deno"]
        )
        self.assertEqual(calls[0][1], [str(Path("/wrapper/node-wrapper"))])
        self.assertEqual(calls[0][3], {"PATH": "/isolated"})
        self.assertFalse(calls[0][4])
        self.assertEqual(commands["node"], ["/native/node"])
        self.assertEqual(commands["bun"], ["/native/bun", "run"])
        self.assertEqual(commands["deno"], ["/native/deno", "run"])
        self.assertEqual(commands["zipp"], [str(Path("/wrapper/zipp-bin"))])

    def test_invalid_command_components_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "nonempty strings"):
            hostile.engine_prefix("node", ["node", ""], "script")

    def test_missing_external_launcher_fails_closed(self):
        with mock.patch.object(hostile.core, "resolved_executable", return_value=None):
            with self.assertRaisesRegex(ValueError, "node.*launcher was not found"):
                hostile.resolve_engine_commands(
                    ("node", "zipp"),
                    node="missing-node",
                    bun="bun",
                    deno="deno",
                    zipp="zipp",
                    timeout=1.0,
                )


class MeasurementTests(unittest.TestCase):
    def test_four_engine_schedule_balances_every_position(self):
        case = synthetic_case("one")

        def runner(cmd, path, *, timeout):
            return process_result(0.1, b"" if "empty" in Path(path).name else b"ok\n")

        with contextlib.redirect_stderr(io.StringIO()):
            result = hostile.run_measurements(
                (case,),
                node="node-bin",
                bun=["bun-bin", "run"],
                deno=["deno-bin", "run"],
                zipp="zipp-bin",
                reps=8,
                seed=0x1234,
                runner=runner,
            )
        positions = {engine: [0, 0, 0, 0] for engine in hostile.CANONICAL_ENGINE_NAMES}
        for schedule in result["schedules"]:
            for position, engine in enumerate(schedule["engine_order"]):
                positions[engine][position] += 1
        self.assertEqual(
            positions,
            {engine: [2, 2, 2, 2] for engine in hostile.CANONICAL_ENGINE_NAMES},
        )

    def test_schedule_commands_samples_and_exact_output(self):
        cases = (
            synthetic_case("script"),
            synthetic_case("module", category="modules", goal="module"),
        )
        calls = []

        def runner(cmd, path, *, timeout):
            calls.append((tuple(cmd), Path(path), timeout))
            startup = Path(path).name.startswith("zipp-hostile-empty-")
            engine = {
                "node-bin": "node",
                "bun-bin": "bun",
                "deno-bin": "deno",
                "zipp-bin": "zipp",
            }[cmd[0]]
            if startup:
                return process_result(
                    {"node": 0.04, "bun": 0.03, "deno": 0.05, "zipp": 0.02}[engine],
                    b"",
                )
            output = f"{Path(path).stem}:ok\n".encode()
            return process_result(
                {"node": 0.25, "bun": 0.22, "deno": 0.27, "zipp": 0.20}[engine],
                output,
            )

        with contextlib.redirect_stderr(io.StringIO()):
            result = hostile.run_measurements(
                cases,
                node="node-bin",
                bun=["bun-bin", "run"],
                deno=["deno-bin", "run"],
                zipp="zipp-bin",
                reps=2,
                seed=7,
                runner=runner,
            )
        self.assertTrue(result["all_correct"])
        self.assertEqual(result["engine_names"], ["node", "bun", "deno", "zipp"])
        for schedule in result["schedules"]:
            self.assertEqual(set(schedule["engine_order"]), set(result["engine_names"]))
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
        self.assertTrue(any(cmd == ("bun-bin", "run") for cmd, _, _ in calls))
        self.assertTrue(any(cmd == ("deno-bin", "run") for cmd, _, _ in calls))
        self.assertTrue(
            any(path.suffix == ".mjs" and "empty" in path.name for _, path, _ in calls)
        )
        pair_ids = [
            item["pair_id"]
            for item in result["observations"]
            if item["case"] == "script" and item["rep"] == 0
        ]
        self.assertEqual(pair_ids, ["script:0"] * 4)

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
            return process_result(
                0.1, b"first\n" if zipp_full_runs == 1 else b"second\n"
            )

        with contextlib.redirect_stderr(io.StringIO()):
            result = hostile.run_measurements(
                (case,),
                node="node-bin",
                zipp="zipp-bin",
                engine_names=("node", "zipp"),
                reps=2,
                seed=1,
                runner=runner,
            )
        self.assertFalse(result["all_correct"])
        self.assertTrue(
            any(
                "not reproducible" in failure
                for failure in result["correctness_failures"]
            )
        )
        self.assertTrue(
            any(
                "differs from node" in failure
                for failure in result["correctness_failures"]
            )
        )

    def test_node_is_correctness_baseline_for_all_competitors(self):
        case = synthetic_case("one")

        def runner(cmd, path, *, timeout):
            if Path(path).name.startswith("zipp-hostile-empty-"):
                return process_result(0.01, b"")
            engine = cmd[0].removesuffix("-bin")
            return process_result(0.1, f"{engine}\n".encode())

        with contextlib.redirect_stderr(io.StringIO()):
            result = hostile.run_measurements(
                (case,),
                node="node-bin",
                bun=["bun-bin", "run"],
                deno=["deno-bin", "run"],
                zipp="zipp-bin",
                reps=1,
                seed=1,
                runner=runner,
            )
        self.assertFalse(result["all_correct"])
        for engine in ("bun", "deno", "zipp"):
            self.assertIn(
                f"{engine} output differs from node on one",
                result["correctness_failures"],
            )

    def test_every_measurement_uses_the_fail_closed_base_environment(self):
        case = synthetic_case("one")
        process_env = {"PATH": "/isolated", "HOME": "/isolated/home"}
        seen = []

        def runner(cmd, path, *, timeout, base_env):
            seen.append(base_env)
            return process_result(0.1, b"" if "empty" in Path(path).name else b"ok\n")

        with contextlib.redirect_stderr(io.StringIO()):
            result = hostile.run_measurements(
                (case,),
                node="node-bin",
                zipp="zipp-bin",
                engine_names=("node", "zipp"),
                reps=1,
                seed=1,
                process_env=process_env,
                runner=runner,
            )
        self.assertTrue(result["all_correct"])
        self.assertEqual(seen, [process_env] * 4)

    def test_every_measurement_requests_a_fresh_process_environment(self):
        case = synthetic_case("one")
        seen = []

        def runner(cmd, path, *, timeout, fresh_environment):
            seen.append(fresh_environment)
            return process_result(
                0.1, b"" if "empty" in Path(path).name else b"ok\n"
            )

        with contextlib.redirect_stderr(io.StringIO()):
            result = hostile.run_measurements(
                (case,),
                node="node-bin",
                zipp="zipp-bin",
                engine_names=("node", "zipp"),
                reps=1,
                seed=1,
                fresh_environment=True,
                runner=runner,
            )
        self.assertTrue(result["all_correct"])
        self.assertEqual(seen, [True] * 4)

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
                engine_names=("node", "zipp"),
                reps=1,
                seed=1,
                runner=runner,
            )
        self.assertTrue(any("timed out" in item for item in result["health_failures"]))
        self.assertIsNone(
            hostile.summarize((case,), result, seed=1, bootstrap_samples=10)
        )


class MainIntegrityTests(ManifestFixture):
    def test_artifact_records_repository_head_drift_and_isolated_environment(self):
        manifest = self.manifest(
            {"schema_version": 1, "cases": self.valid_family_cases()}
        )
        output = self.root / "result.json"

        def metadata(
            name,
            command,
            timeout,
            *,
            process_env=None,
            fresh_environment=False,
        ):
            return {
                "name": name,
                "argv": command,
                "sha256": f"sha-{name}",
                "build_identity": None,
                "fresh_environment_seen": fresh_environment,
            }

        samples = {
            metric: {
                engine: {case_id: [0.1] for case_id in ("scope-base", "scope-iife")}
                for engine in ("node", "zipp")
            }
            for metric in ("startup", "cold", "adjusted")
        }
        measurement = {
            "samples": samples,
            "outputs": {},
            "observations": [],
            "schedules": [],
            "health_failures": [],
            "correctness_failures": [],
            "engine_names": ["node", "zipp"],
            "all_correct": True,
        }
        with (
            mock.patch.object(
                hostile.core, "git_paths_match_head", return_value=(True, None)
            ),
            mock.patch.object(
                hostile.core,
                "git_repository_matches_head",
                side_effect=[(True, None), (False, "repository became dirty")],
            ) as repository_probe,
            mock.patch.object(hostile.core, "relevant_environment", return_value={}),
            mock.patch.object(
                hostile.core, "git_revision", side_effect=["head-a", "head-b"]
            ),
            mock.patch.object(
                hostile,
                "resolve_engine_commands",
                return_value={"node": ["node-native"], "zipp": ["zipp-native"]},
            ),
            mock.patch.object(hostile.core, "engine_metadata", side_effect=metadata),
            mock.patch.object(
                hostile.core, "provenance_assessment", return_value=([], [])
            ),
            mock.patch.object(hostile.core, "engine_drift", return_value=[]),
            mock.patch.object(hostile, "run_measurements", return_value=measurement),
            mock.patch.object(hostile, "summarize", return_value={}),
            mock.patch.object(hostile, "print_report"),
            mock.patch.object(hostile.core, "power_mode", return_value=None),
        ):
            with contextlib.redirect_stderr(io.StringIO()):
                status = hostile.main(
                    [
                        "--manifest",
                        str(manifest),
                        "--engines",
                        "node,zipp",
                        "--reps",
                        "1",
                        "--bootstrap-samples",
                        "1",
                        "--json",
                        str(output),
                    ]
                )

        self.assertEqual(status, 1)
        self.assertEqual(repository_probe.call_count, 2)
        artifact = json.loads(output.read_text(encoding="utf-8"))
        self.assertTrue(artifact["repository_head_before"])
        self.assertFalse(artifact["repository_head_after"])
        self.assertEqual(artifact["workspace_commit_before"], "head-a")
        self.assertEqual(artifact["workspace_commit_after"], "head-b")
        self.assertFalse(artifact["publishable"])
        self.assertEqual(artifact["benchmark_environment_policy"]["inherit"], "none")
        self.assertTrue(
            all(
                engine["fresh_environment_seen"]
                for engine in artifact["engines_before"]
            )
        )
        self.assertTrue(
            all(
                engine["fresh_environment_seen"]
                for engine in artifact["engines_after"]
            )
        )
        self.assertIn("repository became dirty", artifact["publication_reasons"])
        self.assertTrue(
            any(
                "workspace HEAD changed during run" in failure
                for failure in artifact["health_failures"]
            )
        )


class SummaryTests(unittest.TestCase):
    def test_exact_sign_gate_uses_bonferroni_threshold(self):
        fourteen = hostile._bonferroni_sign_test(
            [0.8] * 14 + [1.2], [1.0] * 15, hypothesis_count=51
        )
        thirteen = hostile._bonferroni_sign_test(
            [0.8] * 13 + [1.2] * 2, [1.0] * 15, hypothesis_count=51
        )
        assert fourteen is not None and thirteen is not None
        self.assertEqual(fourteen["bonferroni_alpha"], 0.05 / 51)
        self.assertLessEqual(
            fourteen["one_sided_pvalue"], fourteen["bonferroni_alpha"]
        )
        self.assertGreater(
            thirteen["one_sided_pvalue"], thirteen["bonferroni_alpha"]
        )

    def test_four_engine_ratios_are_rep_paired_and_gate_all_three(self):
        case = synthetic_case("one")
        samples = {
            metric: {
                "node": {"one": [1.0, 100.0] * 8},
                "bun": {"one": [2.0, 200.0] * 8},
                "deno": {"one": [4.0, 400.0] * 8},
                "zipp": {"one": [0.5, 20.0] * 8},
            }
            for metric in ("startup", "cold", "adjusted")
        }
        summary = hostile.summarize(
            (case,),
            {
                "samples": samples,
                "engine_names": list(hostile.CANONICAL_ENGINE_NAMES),
                "health_failures": [],
                "correctness_failures": [],
                "all_correct": True,
            },
            seed=7,
            bootstrap_samples=200,
        )
        assert summary is not None
        detail = summary["case_summaries"]["one"]["metrics"]["cold"]
        # Same-repetition ratios are [0.5, 0.2], whose median is 0.35.
        # Dividing independent medians would produce a different answer.
        self.assertAlmostEqual(detail["zipp_vs"]["node"]["paired_ratio"], 0.35)
        self.assertAlmostEqual(detail["zipp_vs"]["bun"]["paired_ratio"], 0.175)
        self.assertAlmostEqual(detail["zipp_vs"]["deno"]["paired_ratio"], 0.0875)
        sign = detail["zipp_vs"]["node"]["exact_sign_test"]
        self.assertEqual(sign["hypothesis_count"], 3)
        self.assertEqual(sign["strict_wins"], 16)
        self.assertTrue(sign["rejects"])
        self.assertTrue(detail["zipp_faster_than_all"]["point_estimate"])
        self.assertTrue(detail["zipp_faster_than_all"]["ci95"])
        suite_gate = summary["suite_summaries"]["cold"]["zipp_faster_than_all"]
        self.assertTrue(suite_gate["point_estimate"])
        self.assertTrue(suite_gate["descriptive_bootstrap_ci95_conjunction"])
        rows_gate = summary["suite_summaries"]["cold"]["all_rows_faster_than_all"]
        self.assertTrue(rows_gate["point_estimate"])
        self.assertTrue(rows_gate["ci95"])
        self.assertEqual(rows_gate["failing_ci95_rows"], [])

    def test_faster_than_all_gate_is_fail_closed(self):
        favorable = {
            name: {
                "paired_ratio": 0.8,
                "paired_ratio_ci95": [0.7, 0.9],
                "exact_sign_test": {
                    "one_sided_pvalue": 0.0001,
                    "bonferroni_alpha": 0.001,
                    "rejects": True,
                },
            }
            for name in hostile.COMPARISON_ENGINE_NAMES
        }
        gate = hostile._faster_than_all_gate(favorable)
        self.assertTrue(gate["individual_ci95"])
        self.assertTrue(gate["familywise95"])
        self.assertTrue(gate["ci95"])
        subset = {"node": favorable["node"], "bun": favorable["bun"]}
        self.assertFalse(hostile._faster_than_all_gate(subset)["complete"])
        self.assertFalse(hostile._faster_than_all_gate(subset)["ci95"])
        favorable["deno"]["paired_ratio_ci95"] = [0.7, 1.0]
        gate = hostile._faster_than_all_gate(favorable)
        self.assertFalse(gate["individual_ci95"])
        self.assertTrue(gate["familywise95"])
        favorable["deno"]["paired_ratio_ci95"] = [0.7, 0.9]
        favorable["deno"]["exact_sign_test"]["rejects"] = False
        gate = hostile._faster_than_all_gate(favorable)
        self.assertTrue(gate["individual_ci95"])
        self.assertFalse(gate["familywise95"])
        self.assertFalse(gate["ci95"])
        favorable["deno"]["exact_sign_test"]["rejects"] = True
        incorrect = hostile._faster_than_all_gate(favorable, all_correct=False)
        self.assertFalse(incorrect["point_estimate"])
        self.assertFalse(incorrect["ci95"])

    def test_all_rows_gate_cannot_be_hidden_by_a_winning_geomean(self):
        cases = (synthetic_case("fast"), synthetic_case("slow"))
        samples = {
            metric: {
                name: {
                    "fast": [0.1 if name == "zipp" else 1.0],
                    "slow": [1.1 if name == "zipp" else 1.0],
                }
                for name in hostile.CANONICAL_ENGINE_NAMES
            }
            for metric in ("startup", "cold", "adjusted")
        }
        summary = hostile.summarize(
            cases,
            {
                "samples": samples,
                "engine_names": list(hostile.CANONICAL_ENGINE_NAMES),
                "health_failures": [],
                "correctness_failures": [],
                "all_correct": True,
            },
            seed=1,
            bootstrap_samples=10,
        )
        assert summary is not None
        suite = summary["suite_summaries"]["cold"]
        self.assertTrue(suite["zipp_faster_than_all"]["point_estimate"])
        self.assertFalse(suite["all_rows_faster_than_all"]["point_estimate"])
        self.assertEqual(
            suite["all_rows_faster_than_all"]["failing_point_rows"], ["slow"]
        )

    def test_summary_rejects_unpaired_engine_sample_counts(self):
        case = synthetic_case("one")
        samples = {
            metric: {
                "node": {"one": [1.0, 2.0]},
                "bun": {"one": [1.0, 2.0]},
                "deno": {"one": [1.0]},
                "zipp": {"one": [1.0, 2.0]},
            }
            for metric in ("startup", "cold", "adjusted")
        }
        self.assertIsNone(
            hostile.summarize(
                (case,),
                {
                    "samples": samples,
                    "engine_names": list(hostile.CANONICAL_ENGINE_NAMES),
                    "health_failures": [],
                    "correctness_failures": [],
                    "all_correct": True,
                },
                seed=1,
                bootstrap_samples=10,
            )
        )

    def test_report_names_every_engine_and_explicit_ci_gate(self):
        case = synthetic_case("one")
        samples = {
            metric: {
                name: {"one": [0.5 if name == "zipp" else 1.0] * 15}
                for name in hostile.CANONICAL_ENGINE_NAMES
            }
            for metric in ("startup", "cold", "adjusted")
        }
        measurement = {
            "samples": samples,
            "engine_names": list(hostile.CANONICAL_ENGINE_NAMES),
            "health_failures": [],
            "correctness_failures": [],
            "all_correct": True,
        }
        summary = hostile.summarize((case,), measurement, seed=1, bootstrap_samples=10)
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            hostile.print_report((case,), summary, measurement)
        rendered = output.getvalue()
        for name in hostile.CANONICAL_ENGINE_NAMES:
            self.assertIn(name, rendered)
        self.assertIn("zipp/node=", rendered)
        self.assertIn("zipp/bun=", rendered)
        self.assertIn("zipp/deno=", rendered)
        self.assertIn(
            "ZIPP_FASTER_THAN_NODE_BUN_DENO_ALL_ROWS_EXACT_SIGN_BONFERRONI95=1",
            rendered,
        )

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
        summary = hostile.summarize(cases, measurement, seed=123, bootstrap_samples=20)
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
            summary["suite_summaries"]["cold"]["overall"]["geomean_paired_ratio"],
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
        self.assertEqual(degradation["relative_parity_ratio"]["paired_ratio"], 1.5)

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

    def test_harness_digest_binds_input_staging_helper(self):
        digests = hostile.harness_digests()
        self.assertIn("tools/pgo_training.py", digests)
        self.assertEqual(
            digests["tools/pgo_training.py"],
            hostile.core.file_digest(hostile.STAGE_HELPER_PATH),
        )

    def test_publishable_requires_clean_provenance_sources_engine_and_output(self):
        policy = hostile.core.canonical_benchmark_environment_descriptor()

        def publishable(provenance, engine, source, *, correct=True, reasons=()):
            return hostile.artifact_publishable(
                provenance,
                engine,
                source,
                all_correct=correct,
                publication_reasons=list(reasons),
                benchmark_environment_policy=policy,
                publication_sources_head_before=True,
                publication_sources_head_after=True,
                repository_head_before=True,
                repository_head_after=True,
            )

        self.assertTrue(publishable([], [], []))
        self.assertFalse(publishable(["dirty"], [], []))
        self.assertFalse(publishable([], ["engine drift"], []))
        self.assertFalse(publishable([], [], ["source drift"]))
        self.assertFalse(publishable([], [], [], correct=False))
        self.assertEqual(policy["inherit"], "none")
        self.assertFalse(publishable([], [], [], reasons=["filtered corpus"]))
        tampered_policy = {**policy, "inherit": "ambient"}
        self.assertFalse(
            hostile.artifact_publishable(
                [],
                [],
                [],
                all_correct=True,
                publication_reasons=[],
                benchmark_environment_policy=tampered_policy,
                publication_sources_head_before=True,
                publication_sources_head_after=True,
                repository_head_before=True,
                repository_head_after=True,
            )
        )
        self.assertFalse(
            hostile.artifact_publishable(
                [],
                [],
                [],
                all_correct=True,
                publication_reasons=[],
                benchmark_environment_policy=policy,
                publication_sources_head_before=True,
                publication_sources_head_after=True,
                repository_head_before=True,
                repository_head_after=False,
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

        engine_reasons = hostile.publication_policy_reasons(
            hostile.DEFAULT_MANIFEST,
            filtered=False,
            engine_names=("node", "zipp"),
            reps=hostile.DEFAULT_REPS,
            bootstrap_samples=hostile.core.BOOTSTRAP_SAMPLES,
            source_reason=None,
            environment={},
        )
        self.assertEqual(len(engine_reasons), 1)
        self.assertIn("exact engine order node,bun,deno,zipp", engine_reasons[0])


class ImmutableHostileInputStageTests(unittest.TestCase):
    def test_stage_preserves_module_graph_through_transient_live_edit_restore(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            hostile_root = root / "bench" / "hostile"
            graph = hostile_root / "graph"
            graph.mkdir(parents=True)
            entry = graph / "entry.mjs"
            dependency = graph / "dependency.mjs"
            entry_bytes = b'import { value } from "./dependency.mjs"; console.log(value);\n'
            dependency_bytes = b"export const value = 7;\n"
            entry.write_bytes(entry_bytes)
            dependency.write_bytes(dependency_bytes)
            manifest_path = hostile_root / "manifest.json"
            manifest_path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "cases": [
                            {
                                "id": "graph",
                                "entry": "graph/entry.mjs",
                                "goal": "module",
                                "category": "module",
                                "inputs": [
                                    "graph/entry.mjs",
                                    "graph/dependency.mjs",
                                ],
                            }
                        ],
                    }
                ),
                encoding="ascii",
            )
            stage = hostile.core.ImmutableInputStage(
                {
                    "bench/hostile/manifest.json": manifest_path,
                    "bench/hostile/graph/entry.mjs": entry,
                    "bench/hostile/graph/dependency.mjs": dependency,
                },
                prefix="zipp-hostile-stage-test-",
            )
            try:
                staged_manifest = hostile.load_manifest(
                    stage.path("bench/hostile/manifest.json")
                )
                staged_case = staged_manifest.cases[0]
                entry.write_bytes(b"throw new Error('transient');\n")
                dependency.write_bytes(b"export const value = 99;\n")
                self.assertEqual(staged_case.entry.read_bytes(), entry_bytes)
                self.assertEqual(staged_case.inputs[1].read_bytes(), dependency_bytes)
                entry.write_bytes(entry_bytes)
                dependency.write_bytes(dependency_bytes)
                self.assertEqual(
                    hostile.input_digests((staged_case,)),
                    hostile.live_input_digests(hostile_root, (staged_case,)),
                )
            finally:
                stage.close()


if __name__ == "__main__":
    unittest.main()
