#!/usr/bin/env python3
"""The root workspace's tests, split for parallel CI jobs.

``ci_test_shard.py K N`` prints the ``cargo test`` target arguments for shard
``K`` (0-based) of ``N``: every integration test target of every workspace
member goes to exactly one shard, in name order, round robin, and shard 0 also
takes the unit tests and builds the examples (``--lib --bins --examples``). Doc
tests are not a target selection cargo can mix with these; the job that runs
shard 0 runs them as ``cargo test --workspace --doc``.

The caller keeps ``--workspace`` and selects targets by name, so Cargo unifies
features exactly as an unsharded ``cargo test --workspace`` does: ``-p
zipp-vm`` on its own would build the engine without the features the CLI
turns on in it. A name that two members both use (an ``audit_*`` file, say)
selects both, in the same shard.

A target with ``required-features`` is selected only when the workspace's
resolved features for its package include them all, as an unsharded run
builds it only then (cargo refuses a target named on the command line whose
features are off). The resolution is the one ``cargo metadata`` reports for
the whole workspace with default features, the set ``cargo test
--workspace`` builds.

``--check N`` exits non-zero unless the ``N`` shards together select every
such target exactly once, so a new test file cannot fall between them.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys


def test_targets(metadata: dict) -> list[str]:
    """The distinct names of the integration test targets an unsharded run builds, sorted."""
    members = set(metadata["workspace_members"])
    enabled = {n["id"]: set(n["features"]) for n in metadata["resolve"]["nodes"]}
    return sorted({
        t["name"]
        for p in metadata["packages"] if p["id"] in members
        for t in p["targets"]
        if "test" in t["kind"] and set(t.get("required-features", [])) <= enabled.get(p["id"], set())
    })


def shard_args(names: list[str], k: int, n: int) -> list[str]:
    if not 0 <= k < n:
        raise ValueError(f"shard {k} is not in 0..{n - 1}")
    args = ["--lib", "--bins", "--examples"] if k == 0 else []
    return args + [f"--test={name}" for i, name in enumerate(names) if i % n == k]


def check(names: list[str], n: int) -> list[str]:
    """Problems with the split: names selected by no shard or by more than one."""
    seen: dict[str, int] = {}
    for k in range(n):
        for arg in shard_args(names, k, n):
            if arg.startswith("--test="):
                seen[arg[len("--test="):]] = seen.get(arg[len("--test="):], 0) + 1
    return [f"{name} is in {seen.get(name, 0)} shards" for name in names if seen.get(name, 0) != 1]


def workspace_metadata() -> dict:
    cargo = os.environ.get("CARGO", "cargo")
    out = subprocess.check_output([cargo, "metadata", "--format-version", "1", "--locked"])
    return json.loads(out)


def main(argv: list[str]) -> int:
    if len(argv) == 2 and argv[0] == "--check":
        problems = check(test_targets(workspace_metadata()), int(argv[1]))
        for problem in problems:
            print(problem, file=sys.stderr)
        return 1 if problems else 0
    if len(argv) != 2:
        print("usage: ci_test_shard.py K N | --check N", file=sys.stderr)
        return 2
    print(" ".join(shard_args(test_targets(workspace_metadata()), int(argv[0]), int(argv[1]))))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
