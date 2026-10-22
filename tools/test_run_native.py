"""The diagnostic comparison runner's Zipp lanes (bench/comparison/run_native.py):
each lane's tiers are set in the child's own environment, whatever the runner
inherits, and the strict interpreter lane turns off both native tiers."""
import importlib.util
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock


RUNNER_PATH = Path(__file__).resolve().parents[1] / "bench" / "comparison" / "run_native.py"
SPEC = importlib.util.spec_from_file_location("zipp_run_native", RUNNER_PATH)
assert SPEC and SPEC.loader
run_native = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = run_native
SPEC.loader.exec_module(run_native)

# A fake engine child: prints the Zipp controls it was started with.
PRINT_ZIPP_ENV = (
    "import json, os; "
    "print(json.dumps({k: v for k, v in os.environ.items() if k.upper().startswith('ZIPP_')}, sort_keys=True))"
)


def child_zipp_environment(lane: str) -> dict:
    engine = run_native.make_engine_for(lane, Path(sys.executable))
    result = subprocess.run(
        [sys.executable, "-c", PRINT_ZIPP_ENV],
        env=run_native.controlled_environment(engine.environment),
        capture_output=True,
        text=True,
        check=True,
    )
    return json.loads(result.stdout)


class ZippLaneTests(unittest.TestCase):
    def test_strict_interpreter_lane_turns_off_both_native_tiers(self):
        engine = run_native.make_engine_for("zipp-interp", Path("zipp"))
        self.assertEqual(engine.environment, {"ZIPP_NOJIT": "1", "ZIPP_NO_RX_JIT": "1"})
        self.assertEqual(
            run_native.ZIPP_LANES["zipp-interp"]["tiers"],
            {"js_jit": False, "regex_jit": False},
        )

    def test_every_lane_states_the_tiers_its_switches_select(self):
        for lane, spec in run_native.ZIPP_LANES.items():
            with self.subTest(lane=lane):
                environment = run_native.make_engine_for(lane, Path("zipp")).environment
                self.assertEqual(spec["tiers"]["js_jit"], "ZIPP_NOJIT" not in environment)
                self.assertEqual(spec["tiers"]["regex_jit"], "ZIPP_NO_RX_JIT" not in environment)
                self.assertEqual(run_native.make_engine_for(lane, Path("zipp")).command[1:], ("js",))

    def test_a_lane_child_gets_its_own_tiers_whatever_the_runner_inherits(self):
        hostile = {"ZIPP_NOJIT": "0", "ZIPP_NO_RX_JIT": "0", "ZIPP_JIT_LARGE": "1", "zipp_nojit": "1"}
        with mock.patch.dict(os.environ, hostile):
            self.assertEqual(
                child_zipp_environment("zipp-interp"),
                {"ZIPP_NOJIT": "1", "ZIPP_NO_RX_JIT": "1"},
            )
            self.assertEqual(child_zipp_environment("zipp-jit"), {})
            self.assertEqual(child_zipp_environment("zipp-jit-norx"), {"ZIPP_NO_RX_JIT": "1"})
            self.assertEqual(child_zipp_environment("zipp-interp-rxjit"), {"ZIPP_NOJIT": "1"})

    def test_an_exported_switch_alone_does_not_reach_a_lane(self):
        # Exporting ZIPP_NO_RX_JIT around the runner was never a way to make
        # the old `zipp-interp` strict: the child environment drops it.
        with mock.patch.dict(os.environ, {"ZIPP_NO_RX_JIT": "1"}):
            self.assertEqual(child_zipp_environment("zipp-interp-rxjit"), {"ZIPP_NOJIT": "1"})


if __name__ == "__main__":
    unittest.main()
