#!/usr/bin/env python3
"""Run the pinned upstream corpus and a separately documented corrected corpus.

No tests are skipped or removed. Raw runner reports are retained unchanged;
comparison.json labels each corpus and records patch and executable hashes.
The corrected checkout is new unless verified reuse is requested explicitly.
Nothing is pushed or committed.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys

TOOLS = Path(__file__).resolve().parent
MANIFEST = TOOLS / "test262-corrections" / "manifest.json"


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args]).decode("utf-8").strip()


def load_manifest():
    data = json.loads(MANIFEST.read_text(encoding="utf-8"))
    if data["version"] != 1:
        raise ValueError("unsupported corrections manifest version")
    return data


def verify_tree(root, manifest, corrected=False):
    """Reject revision drift, unlisted edits and checkout newline conversion."""
    if git(root, "rev-parse", "HEAD") != manifest["upstream_commit"]:
        raise ValueError("Test262 checkout does not match the pinned revision")
    # core.autocrlf can conceal changed fixture bytes from git diff. Imports of
    # bytes must observe the upstream blob, including its original terminators.
    for line in git(root, "ls-files", "--eol").splitlines():
        columns = line.split()
        if len(columns) >= 2 and columns[0][2:] != columns[1][2:]:
            raise ValueError("Test262 checkout changed file bytes: " + line)
    changed = set(git(root, "diff", "HEAD", "--name-only").splitlines())
    wanted = {c["path"] for c in manifest["corrections"]} if corrected else set()
    if changed != wanted:
        raise ValueError(f"unexpected Test262 edits: expected {sorted(wanted)}, got {sorted(changed)}")
    key = "corrected_sha256" if corrected else "original_sha256"
    for correction in manifest["corrections"]:
        if digest(Path(root) / correction["path"]) != correction[key]:
            raise ValueError("Test262 file hash mismatch: " + correction["path"])
    extra = git(root, "ls-files", "--others")
    if extra:
        raise ValueError("unexpected untracked Test262 files: " + extra)


def prepare(source, destination, manifest, reuse=False):
    source, destination = Path(source).resolve(), Path(destination).resolve()
    verify_tree(source, manifest)
    if destination.exists():
        if reuse:
            verify_tree(destination, manifest, corrected=True)
            return
        raise ValueError("corrected checkout must be a new directory: " + str(destination))
    # A local clone preserves the original tree and avoids fetching a moving ref.
    subprocess.run(["git", "clone", "--config", "core.autocrlf=false", "--no-hardlinks",
                    str(source), str(destination)], check=True)
    verify_tree(destination, manifest)
    patches = [str(MANIFEST.parent / c["patch"]) for c in manifest["corrections"]]
    subprocess.run(["git", "-C", str(destination), "apply", "--check", *patches], check=True)
    subprocess.run(["git", "-C", str(destination), "apply", *patches], check=True)
    verify_tree(destination, manifest, corrected=True)


def compare(original, corrected):
    if original["engine"] != corrected["engine"]:
        raise ValueError("the two runs used different engine builds")
    for key in ("selection", "files", "executions"):
        if original[key] != corrected[key]:
            raise ValueError("the two runs differ in " + key)
    selection = original["selection"]
    if (selection["sub"] != "test" or selection["limit"] or
            selection["include_intl402"] or selection["no_staging"]):
        raise ValueError("dual report requires the full core suite, including staging")
    if original["executions"] <= 0:
        raise ValueError("empty Test262 run")
    return (original["gate_passed"] and corrected["gate_passed"] and
            original["counts"]["skip"] == 0 and corrected["counts"]["skip"] == 0 and
            corrected["counts"]["fail"] == 0 and not corrected["expected_failures"])


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--t262", required=True)
    parser.add_argument("--corrected-tree", required=True)
    parser.add_argument("--zipp", required=True)
    parser.add_argument("--output-dir", default="target/test262-evidence")
    parser.add_argument("--jobs", type=int, default=2)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--reuse-corrected", action="store_true", help="reuse only an existing checkout that passes all correction hash checks")
    args = parser.parse_args(argv)
    if args.jobs <= 0 or args.timeout <= 0:
        parser.error("jobs and timeout must be positive integers")
    manifest = load_manifest()
    prepare(args.t262, args.corrected_tree, manifest, reuse=args.reuse_corrected)
    if args.prepare_only:
        return 0
    output = Path(args.output_dir).resolve()
    output.mkdir(parents=True, exist_ok=True)
    if any((output / name).exists() for name in ("upstream.json", "corrected.json", "comparison.json")):
        raise ValueError("use a fresh output directory to avoid stale evidence")
    binary = Path(args.zipp).resolve()
    binary_hash = digest(binary)
    reports, codes, errors = {}, [], []
    for name, tree in (("upstream", args.t262), ("corrected", args.corrected_tree)):
        command = [sys.executable, str(TOOLS / "run_test262.py"), "--t262", str(tree),
                   "--zipp", str(binary), "--jobs", str(args.jobs), "--timeout", str(args.timeout),
                   "--dump-fails", str(output / (name + "-failures.txt")),
                   "--json", str(output / (name + ".json"))]
        if name == "upstream":
            command += ["--expected-failures", str(TOOLS / "test262-expected-failures.txt")]
        # Run both even when one fails, retaining evidence of the discrepancy.
        codes.append(subprocess.run(command).returncode)
        verify_tree(tree, manifest, corrected=(name == "corrected"))
        if digest(binary) != binary_hash:
            raise ValueError("engine binary changed during validation")
        report_path = output / (name + ".json")
        if not report_path.exists():
            errors.append(f"{name} runner exited {codes[-1]} without producing a report")
        else:
            reports[name] = json.loads(report_path.read_text(encoding="utf-8"))
    success = not errors and compare(reports["upstream"], reports["corrected"]) and codes == [0, 0]
    evidence = {
        "schema_version": 1, "gate_passed": success, "runner_errors": errors,
        "engine_sha256": binary_hash, "upstream_commit": manifest["upstream_commit"],
        "manifest_sha256": digest(MANIFEST),
        "corrections": [{**c, "patch_sha256": digest(MANIFEST.parent / c["patch"])}
                        for c in manifest["corrections"]],
        "runs": {name: {"profile": "unmodified-upstream" if name == "upstream" else manifest["profile"],
                        "report": name + ".json", "report_sha256": digest(output / (name + ".json")),
                        "counts": report["counts"], "executions": report["executions"],
                        "gate_passed": report["gate_passed"]}
                 for name, report in reports.items()},
    }
    (output / "comparison.json").write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
    print("Dual-suite gate:", "PASS" if success else "FAIL", flush=True)
    return 0 if success else 1


if __name__ == "__main__":
    sys.exit(main())
