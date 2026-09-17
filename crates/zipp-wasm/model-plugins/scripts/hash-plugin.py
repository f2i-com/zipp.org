#!/usr/bin/env python3
"""Rewrite a plugin manifest's source digests.

A plugin's `sources` map pins every Python file by sha256, and the registry
refuses to install one that does not match. That is the point -- but it means
editing a plugin without rerunning this leaves a manifest that installs
nowhere, and the failure arrives as a digest mismatch rather than as anything
about the edit.

    python scripts/hash-plugin.py plugins/qwen3
"""
import hashlib
import json
import pathlib
import sys


def rehash(directory):
    manifest_path = directory / "plugin.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    changed = []
    for name in sorted(manifest["sources"]):
        digest = hashlib.sha256((directory / name).read_bytes()).hexdigest()
        if manifest["sources"][name] != digest:
            changed.append(name)
        manifest["sources"][name] = digest
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8", newline="\n")
    return changed


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    for argument in sys.argv[1:]:
        directory = pathlib.Path(argument)
        changed = rehash(directory)
        print(f"{directory}: {'updated ' + ', '.join(changed) if changed else 'already current'}")
