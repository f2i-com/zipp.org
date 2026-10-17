#!/usr/bin/env python3
"""Merge the expansion suite's case lists into manifest.json, pinning each
workload's source by SHA-256 (of its LF-normalized bytes). Run after adding or editing a workload; the
harness checks nothing against these hashes itself, they record exactly which
bytes a published series measured.

usage: python bench/expansion/build_manifest.py [--check]
  --check  exit 1 if manifest.json is not what this would write
"""
import hashlib
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SERIES = "zipp-expansion-v1"


def main() -> int:
    cases = []
    for part in sorted(HERE.glob("cases-*.json")):
        cases.extend(json.loads(part.read_text(encoding="utf-8")))
    seen = set()
    for case in cases:
        if case["id"] in seen:
            raise SystemExit(f"duplicate case id {case['id']}")
        seen.add(case["id"])
        # LF-normalized, so a checkout with CRLF line endings hashes the same.
        source = (HERE / case["entry"]).read_bytes().replace(b"\r\n", b"\n")
        text = source.decode("utf-8")
        line = f"const SIZE = {case['sizes']['m']};"
        if len(re.findall(r"^const SIZE = .*;$", text, re.M)) != 1 or line not in text:
            raise SystemExit(f"{case['id']}: needs exactly one `{line}` line")
        case["source_sha256"] = hashlib.sha256(source).hexdigest()
    manifest = {
        "schema_version": 1,
        "series": SERIES,
        "description": (
            "Diagnostic expansion suite: workloads the canonical real13, micro5 and "
            "hostile17 suites do not reach. Never part of the canonical geomean."
        ),
        "cases": cases,
    }
    text = json.dumps(manifest, indent=2, ensure_ascii=False) + "\n"
    target = HERE / "manifest.json"
    if "--check" in sys.argv:
        return 0 if target.exists() and target.read_text(encoding="utf-8") == text else 1
    target.write_text(text, encoding="utf-8")
    print(f"{len(cases)} cases -> {target}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
