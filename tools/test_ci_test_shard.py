"""The workspace test split puts every test target in exactly one shard."""
import importlib.util
import unittest
from pathlib import Path


SPEC = importlib.util.spec_from_file_location(
    "ci_test_shard", Path(__file__).with_name("ci_test_shard.py"))
shard = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(shard)


def metadata(*packages):
    """Workspace members (name, enabled features, targets); a target is (name, kind[, required])."""
    return {
        "workspace_members": [name for name, _, _ in packages],
        "resolve": {"nodes": [{"id": name, "features": features} for name, features, _ in packages]},
        "packages": [
            {"id": name, "name": name, "targets": [
                {"name": t[0], "kind": [t[1]], **({"required-features": t[2]} if len(t) > 2 else {})}
                for t in targets]}
            for name, _, targets in packages
        ],
    }


META = metadata(
    ("zipp-vm", ["jit"], [("zipp_vm", "lib"), ("b_test", "test"), ("a_test", "test"), ("audit_x", "test")]),
    ("zipp-cli", [], [("zipp", "bin"), ("audit_x", "test"), ("c_test", "test")]),
    ("zipp-regress", ["rx-jit"], [("rx_on", "test", ["rx-jit"]), ("bounded", "test", ["bounded-backtracking"])]),
)


class ShardTests(unittest.TestCase):
    def test_targets_are_distinct_sorted_test_names(self):
        self.assertEqual(shard.test_targets(META), ["a_test", "audit_x", "b_test", "c_test", "rx_on"])

    def test_a_target_whose_features_are_off_is_left_out(self):
        self.assertNotIn("bounded", shard.test_targets(META))

    def test_round_robin_and_unit_tests_in_shard_zero(self):
        names = shard.test_targets(META)
        self.assertEqual(shard.shard_args(names, 0, 2),
                         ["--lib", "--bins", "--examples", "--test=a_test", "--test=b_test", "--test=rx_on"])
        self.assertEqual(shard.shard_args(names, 1, 2), ["--test=audit_x", "--test=c_test"])

    def test_every_target_in_exactly_one_shard(self):
        names = [f"t{i:03}" for i in range(355)]
        for n in (1, 2, 3, 5):
            self.assertEqual(shard.check(names, n), [])

    def test_shard_out_of_range_is_refused(self):
        with self.assertRaises(ValueError):
            shard.shard_args(["a"], 3, 3)


if __name__ == "__main__":
    unittest.main()
