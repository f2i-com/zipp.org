#!/usr/bin/env python3
"""Apply ZIPP's storage patch after updating regress's generated Unicode tables.

The original interval literals remain the source of truth. Rust const-evaluates
their gap/width varints, so regeneration needs no downloaded Unicode files.
Also emits exhaustive decoder round-trip tests against those original literals.
"""
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1] / "crates/regress-fork/src"
PATTERN = re.compile(r"((?:pub\(crate\) )?const (\w+)): (?:\[Interval; \d+\]\s*=\s*\[|PackedIntervals\s*=\s*(?:packed|direct)_intervals!\[)(.*?)\];", re.S)

for name in ("unicodetables.rs", "unicodetables_unknown.rs"):
    path = ROOT / name
    source = path.read_text(encoding="utf-8")
    source = source.split("\n// ZIPP packed-table round-trip tests.\n")[0]
    tests = []
    def replace(match):
        decl, ident, body = match.groups()
        tests.append(f"    assert_eq!({ident}.to_vec(), {ident}.original, \"{ident}\");")
        encoding = "direct" if ident in ("ID_START", "ID_CONTINUE") else "packed"
        return f"{decl}: PackedIntervals = {encoding}_intervals![{body}];"
    source, count = PATTERN.subn(replace, source)
    assert count, name
    imp = "use crate::packed_intervals::{PackedIntervals, packed_intervals};\n"
    if imp not in source:
        source = source.replace("use crate::codepointset::Interval;\n", "use crate::codepointset::Interval;\n" + imp)
    if name == "unicodetables.rs" and "use crate::packed_intervals::direct_intervals;" not in source:
        source = source.replace(imp, imp + "use crate::packed_intervals::direct_intervals;\n")
    source = source.replace("-> &'static [Interval]", "-> &'static PackedIntervals")
    source += "\n// ZIPP packed-table round-trip tests.\n#[cfg(test)]\n#[test]\nfn all_packed_intervals_match_generated_source() {\n" + "\n".join(tests) + "\n}\n"
    path.write_text(source, encoding="utf-8", newline="\n")
    print(f"{name}: {count} tables")
