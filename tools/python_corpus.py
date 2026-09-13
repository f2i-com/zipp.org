#!/usr/bin/env python3
"""Differential test of the Zipp Python frontend against CPython.

Every `tests/python_corpus/*.py` is run through the local CPython and through
`zipp py`; stdout must match byte for byte (a program's stderr is ignored, but
the exit status must agree: both zero or both non-zero).

    py -3 tools/python_corpus.py                 # compare (needs a zipp build)
    py -3 tools/python_corpus.py --zipp target/release/zipp.exe
    py -3 tools/python_corpus.py --write-expected  # record CPython's output as .out
    py -3 tools/python_corpus.py --only classes    # substring filter

`--write-expected` refreshes the `.out` files the Rust test
`crates/zipp-vm/tests/python_corpus.rs` pins, so CI needs no CPython.
"""
from __future__ import annotations
import argparse
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "tests" / "python_corpus"


# Library modules bundled with the frontend (crates/zipp-vm/src/frontend/python/lib)
# are importable from CPython too, so corpus programs can use them.
LIB = ROOT / "crates" / "zipp-vm" / "src" / "frontend" / "python" / "lib"


def run(cmd: list[str], cwd: Path) -> tuple[int, str, str]:
    env = dict(os.environ, PYTHONPATH=str(LIB), PYTHONDONTWRITEBYTECODE="1")
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=120, env=env)
    return p.returncode, p.stdout.replace("\r\n", "\n"), p.stderr.replace("\r\n", "\n")


def main() -> int:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--zipp", default=str(ROOT / "target" / "debug" / ("zipp.exe" if os.name == "nt" else "zipp")))
    ap.add_argument("--python", default=sys.executable)
    ap.add_argument("--write-expected", action="store_true")
    ap.add_argument("--only", default="")
    ap.add_argument("--verbose", "-v", action="store_true")
    args = ap.parse_args()
    files = sorted(p for p in CORPUS.glob("*.py") if args.only in p.name)
    if not files:
        print("no corpus files matched", file=sys.stderr)
        return 2
    failed = 0
    for path in files:
        code, out, err = run([args.python, "-X", "utf8", "-P", str(path)], CORPUS)
        if args.write_expected:
            path.with_suffix(".out").write_text(out, encoding="utf-8", newline="\n")
            path.with_suffix(".status").write_text("0\n" if code == 0 else "1\n", encoding="utf-8", newline="\n")
            print(f"  wrote {path.with_suffix('.out').name} ({len(out.splitlines())} lines, exit {code})")
            continue
        zcode, zout, zerr = run([args.zipp, "py", str(path)], CORPUS)
        ok = out == zout and (code == 0) == (zcode == 0)
        if ok:
            print(f"  ok   {path.name}")
            continue
        failed += 1
        print(f"  FAIL {path.name} (cpython exit {code}, zipp exit {zcode})")
        el, zl = out.splitlines(), zout.splitlines()
        shown = 0
        for i in range(max(len(el), len(zl))):
            e = el[i] if i < len(el) else "<missing>"
            z = zl[i] if i < len(zl) else "<missing>"
            if e != z:
                print(f"       line {i + 1}:\n         cpython: {e}\n         zipp:    {z}")
                shown += 1
                if shown >= 5 and not args.verbose:
                    print("         ...")
                    break
        if zerr.strip():
            print("       zipp stderr: " + zerr.strip().splitlines()[-1][:300])
    if args.write_expected:
        return 0
    print(f"\n{len(files) - failed} of {len(files)} corpus programs match CPython")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
