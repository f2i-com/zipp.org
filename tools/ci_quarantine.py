#!/usr/bin/env python3
"""The CI test quarantine, made accountable.

Reads ``.github/ci-quarantine.json`` — one entry per EXACTLY named test, with
its binary, owner, reason, last observation and a review-by date — and:

* ``--print``      prints the inventory (what CI is about to skip, and why);
* ``--emit-skips`` prints the ``--skip NAME`` arguments for ``cargo test``;
* ``--check``      exits non-zero when an entry has passed its review date,
                   names a test that no longer exists, or is malformed.

CI runs all three, so a skipped test is always visible in the log, an expired
entry fails the run until someone reviews it, and a new test that merely
resembles a quarantined one is never skipped by accident (the previous
prefix-based ``--skip msplit_mechanism_`` would have swallowed any future
``msplit_mechanism_*`` regression silently: the 11 September 2026 audit's
ZIPP-15).
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
MANIFEST = ROOT / ".github" / "ci-quarantine.json"
TESTS_DIR = ROOT / "crates" / "zipp-vm" / "tests"
REQUIRED = ("binary", "test", "owner", "reason", "observed", "last_reviewed", "review_by")


def load() -> list[dict]:
    data = json.loads(MANIFEST.read_text(encoding="utf-8"))
    entries = data.get("entries")
    if not isinstance(entries, list):
        raise SystemExit(f"{MANIFEST}: 'entries' must be a list")
    return entries


def check(entries: list[dict], today: dt.date) -> list[str]:
    problems: list[str] = []
    seen: set[tuple[str, str]] = set()
    for i, entry in enumerate(entries):
        for key in REQUIRED:
            if not isinstance(entry.get(key), str) or not entry[key].strip():
                problems.append(f"entry {i}: missing or empty '{key}'")
        if problems and problems[-1].startswith(f"entry {i}:"):
            continue
        test, binary = entry["test"], entry["binary"]
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", test):
            problems.append(f"{binary}::{test}: test names must be exact identifiers, not patterns")
        if (binary, test) in seen:
            problems.append(f"{binary}::{test}: listed twice")
        seen.add((binary, test))
        source = TESTS_DIR / f"{binary}.rs"
        if not source.exists():
            problems.append(f"{binary}::{test}: no test binary {source.relative_to(ROOT)}")
        elif not re.search(rf"\bfn\s+{re.escape(test)}\s*\(", source.read_text(encoding="utf-8")):
            problems.append(f"{binary}::{test}: no such test in {source.relative_to(ROOT)} (stale entry)")
        try:
            review_by = dt.date.fromisoformat(entry["review_by"])
        except ValueError:
            problems.append(f"{binary}::{test}: review_by is not an ISO date")
            continue
        if review_by < today:
            problems.append(
                f"{binary}::{test}: quarantine expired on {review_by} (owner: {entry['owner']}); "
                "fix the test and remove the entry, or record a fresh observation and a new review_by"
            )
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--print", action="store_true", help="print the inventory")
    parser.add_argument("--emit-skips", action="store_true", help="print cargo test --skip arguments")
    parser.add_argument("--check", action="store_true", help="fail on expired, stale or malformed entries")
    parser.add_argument("--today", default=None, help="override today's date (ISO) for testing")
    args = parser.parse_args()
    entries = load()
    today = dt.date.fromisoformat(args.today) if args.today else dt.date.today()
    status = 0
    if args.check:
        problems = check(entries, today)
        for problem in problems:
            print(f"quarantine: {problem}", file=sys.stderr)
        if problems:
            status = 1
    if args.print:
        print(f"{len(entries)} quarantined test(s) (exact names; see .github/ci-quarantine.json):")
        for entry in entries:
            print(
                f"  {entry.get('binary')}::{entry.get('test')}  review by {entry.get('review_by')}  "
                f"[{entry.get('owner')}] {entry.get('reason')}"
            )
    if args.emit_skips:
        print(" ".join(f"--skip {entry['test']}" for entry in entries))
    return status


if __name__ == "__main__":
    raise SystemExit(main())
