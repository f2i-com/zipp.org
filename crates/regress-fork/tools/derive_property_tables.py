#!/usr/bin/env python3
"""Store the property tables that are unions of others as those unions.

The General_Category groups are unions of their categories (L = Lu Ll Lt Lm
Lo, and so on), and Assigned is the complement of Unassigned (Cn); the
generator writes each as a table of its own. This keeps their interval
literals, for the round-trip test and the interval count the sandbox's
expansion limit charges, but stores each as a `derived_intervals!`: the
tables it is the union of, and whether to complement it. Their bytes leave
the artifact; a regex that uses one builds it when it is compiled.

All property tables become `static`, so a table a union names is the one in
the artifact and not a copy, and ID_Start and ID_Continue, which
tools/pack_regex_tables.py keeps directly indexed, are packed like the
rest. Run from the repository root after pack_property_names.py, then
rustfmt the file.
"""
from pathlib import Path
import re
import sys

PATH = Path(sys.argv[1] if len(sys.argv) > 1 else "crates/regress-fork/src/unicodetables.rs")

# table: (the tables it is the union of, complement the union)
DERIVED = {
    "LETTER": (["UPPERCASE_LETTER", "LOWERCASE_LETTER", "TITLECASE_LETTER", "MODIFIER_LETTER", "OTHER_LETTER"], False),
    "CASED_LETTER": (["UPPERCASE_LETTER", "LOWERCASE_LETTER", "TITLECASE_LETTER"], False),
    "MARK": (["NONSPACING_MARK", "SPACING_MARK", "ENCLOSING_MARK"], False),
    "NUMBER": (["DECIMAL_NUMBER", "LETTER_NUMBER", "OTHER_NUMBER"], False),
    "PUNCTUATION": (["CONNECTOR_PUNCTUATION", "DASH_PUNCTUATION", "OPEN_PUNCTUATION", "CLOSE_PUNCTUATION",
                     "INITIAL_PUNCTUATION", "FINAL_PUNCTUATION", "OTHER_PUNCTUATION"], False),
    "SYMBOL": (["MATH_SYMBOL", "CURRENCY_SYMBOL", "MODIFIER_SYMBOL", "OTHER_SYMBOL"], False),
    "SEPARATOR": (["SPACE_SEPARATOR", "LINE_SEPARATOR", "PARAGRAPH_SEPARATOR"], False),
    "OTHER": (["CONTROL", "FORMAT", "SURROGATE", "PRIVATE_USE", "UNASSIGNED"], False),
    "ASSIGNED": (["UNASSIGNED"], True),
}


def main():
    source = PATH.read_text(encoding="utf-8")
    source = re.sub(r"^const (\w+): PackedIntervals =", r"static \1: PackedIntervals =", source, flags=re.M)
    # ID_Start and ID_Continue are packed like the others: the lookups the
    # lexer and group names make decode them once (unicode::is_id_start_cp).
    source = source.replace("PackedIntervals = direct_intervals![", "PackedIntervals = packed_intervals![")
    source = source.replace("use crate::packed_intervals::direct_intervals;\n", "")
    for name, (parts, complement) in DERIVED.items():
        pattern = re.compile(r"static %s: PackedIntervals = packed_intervals!\[\n(.*?)\n\];\n" % name, re.S)
        m = pattern.search(source)
        assert m, name
        refs = ", ".join("&%s" % p for p in parts)
        source = source.replace(
            m.group(0),
            "static %s: PackedIntervals = derived_intervals!(\n    [%s],\n    %s,\n%s\n);\n"
            % (name, refs, "true" if complement else "false", m.group(1)),
        )
    if "derived_intervals" not in source.split("\n", 8)[3] + source.split("\n", 8)[4]:
        source = source.replace(
            "use crate::packed_intervals::{PackedIntervals, packed_intervals};\n",
            "use crate::packed_intervals::{PackedIntervals, derived_intervals, packed_intervals};\n",
            1,
        )
    PATH.write_text(source, encoding="utf-8", newline="\n")
    print("derived", len(DERIVED))


if __name__ == "__main__":
    main()
