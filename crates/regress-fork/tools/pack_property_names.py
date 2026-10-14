#!/usr/bin/env python3
"""Replace the generated property-name `match`es in src/unicodetables.rs with tables.

The generator writes, for each kind of property (binary properties, General
Category values, Script values, properties of strings), an enum, a function
that maps a name to it with a `match` over string literals, and a function
that maps it to its code point table with another `match`. Compiled, the
name matches are a few kilobytes of comparisons each. This rewrites each kind
as a byte string of names (each a length byte and the name, a zero byte after
each value's names) and an array of tables in the same order;
`unicode::lookup_property_name` finds the value. Run it from the repository
root after tools/pack_regex_tables.py, then derive_property_tables.py.
"""
from pathlib import Path
import re

import sys
PATH = Path(sys.argv[1] if len(sys.argv) > 1 else "crates/regress-fork/src/unicodetables.rs")


def arms(body, value_pattern):
    """The (aliases, value) pairs of a `match` body, in order."""
    out = []
    for line in body.splitlines():
        line = line.strip()
        if not line or line.startswith("_ =>") or line.startswith("//"):
            continue
        m = re.match(r'^((?:"[^"]+"(?: \| )?)+) => ' + value_pattern + r',$', line)
        assert m, line
        out.append((re.findall(r'"([^"]+)"', m.group(1)), m.group(2)))
    return out


def take(source, pattern):
    m = re.search(pattern, source, re.S)
    assert m, pattern
    return m


def names_const(name, pairs, doc):
    # Each name is its length byte and its bytes; a zero byte ends a value.
    # A lookup skips a name it does not match by its length alone.
    text = "\\0".join("".join(f"\\x{len(a):02X}{a}" for a in aliases) for aliases, _ in pairs)
    return f'/// {doc}\n/// Each value is its names, each a length byte and the name, then a zero byte.\nconst {name}: &[u8] = b"{text}";\n'


def tables(name, values, mapping, extra_doc=""):
    rows = "\n".join(f"    {mapping[v]}," for v in values)
    return f"{extra_doc}static {name}: [&PackedIntervals; {len(values)}] = [\n{rows}\n];\n"


def main():
    source = PATH.read_text(encoding="utf-8")
    ranges_fn = {}
    for m in re.finditer(r"pub\(crate\) fn (\w+_ranges)\(\) -> &'static PackedIntervals \{\n    (&[A-Z0-9_]+)\n\}\n", source):
        ranges_fn[m.group(1)] = m.group(2)

    # Binary properties.
    m = take(source, r"pub fn unicode_property_binary_from_str\(s: &str\) -> Option<UnicodePropertyBinary> \{\n    use UnicodePropertyBinary::\*;\n    match s \{\n(.*?)\n    \}\n\}\n")
    binary = arms(m.group(1), r"Some\((\w+)\)")
    source = source.replace(m.group(0), "")
    m = take(source, r"pub\(crate\) fn binary_property_ranges\(value: &UnicodePropertyBinary\) -> &'static PackedIntervals \{\n    use UnicodePropertyBinary::\*;\n    match value \{\n(.*?)\n    \}\n\}\n")
    binary_map = {v: ranges_fn[f] for v, f in re.findall(r"(\w+) => (\w+)\(\),", m.group(1))}
    source = source.replace(m.group(0),
        names_const("BINARY_PROPERTY_NAMES", binary, "The binary properties `\\p{...}` names: line i selects `BINARY_PROPERTIES[i]`.")
        + "\n" + tables("BINARY_PROPERTIES", [v for _, v in binary], binary_map) + "\n"
        + "/// The table of the binary property `name`.\n"
        + "pub(crate) fn binary_property_from_str(name: &str) -> Option<&'static PackedIntervals> {\n"
        + "    Some(BINARY_PROPERTIES[lookup_property_name(BINARY_PROPERTY_NAMES, name)?])\n}\n")
    source = re.sub(r"\n#\[derive\(Debug, Clone, Copy\)\]\npub enum UnicodePropertyBinary \{\n.*?\n\}\n", "\n", source, count=1, flags=re.S)

    # General Category.
    m = take(source, r"pub fn unicode_property_value_general_category_from_str\(\n    s: &str,\n\) -> Option<UnicodePropertyValueGeneralCategory> \{\n    use UnicodePropertyValueGeneralCategory::\*;\n    match s \{\n(.*?)\n    \}\n\}\n")
    gc = arms(m.group(1), r"Some\((\w+)\)")
    source = source.replace(m.group(0), "")
    m = take(source, r"pub\(crate\) fn general_category_property_value_ranges\(\n    value: &UnicodePropertyValueGeneralCategory,\n\) -> &'static PackedIntervals \{\n    use UnicodePropertyValueGeneralCategory::\*;\n    match value \{\n(.*?)\n    \}\n\}\n")
    gc_map = {v: ranges_fn[f] for v, f in re.findall(r"(\w+) => (\w+)\(\),", m.group(1))}
    source = source.replace(m.group(0),
        names_const("GENERAL_CATEGORY_NAMES", gc, "General_Category value names: line i selects `GENERAL_CATEGORIES[i]`.")
        + "\n" + tables("GENERAL_CATEGORIES", [v for _, v in gc], gc_map) + "\n"
        + "/// The table of the General_Category value `name`.\n"
        + "pub(crate) fn general_category_from_str(name: &str) -> Option<&'static PackedIntervals> {\n"
        + "    Some(GENERAL_CATEGORIES[lookup_property_name(GENERAL_CATEGORY_NAMES, name)?])\n}\n")
    source = re.sub(r"\n#\[derive\(Debug, Clone, Copy\)\]\npub enum UnicodePropertyValueGeneralCategory \{\n.*?\n\}\n", "\n", source, count=1, flags=re.S)

    # Script and Script_Extensions.
    m = take(source, r"pub\(crate\) fn unicode_property_value_script_from_str\(\n    s: &str,\n\) -> Option<UnicodePropertyValueScript> \{\n    use UnicodePropertyValueScript::\*;\n    match s \{\n(.*?)\n    \}\n\}\n")
    sc = arms(m.group(1), r"Some\((\w+)\)")
    source = source.replace(m.group(0), "")
    m = take(source, r"pub\(crate\) fn script_value_ranges\(value: &UnicodePropertyValueScript\) -> &'static PackedIntervals \{\n    use UnicodePropertyValueScript::\*;\n    match value \{\n(.*?)\n    \}\n\}\n")
    sc_map = dict(re.findall(r"(\w+) => (&[A-Z0-9_]+),", m.group(1)))
    source = source.replace(m.group(0), "")
    m = take(source, r"pub\(crate\) fn script_extensions_value_ranges\(\n    value: &UnicodePropertyValueScript,\n\) -> &'static PackedIntervals \{\n    use UnicodePropertyValueScript::\*;\n    match value \{\n(.*?)\n    \}\n\}\n")
    scx_map = dict(re.findall(r"(\w+) => (&[A-Z0-9_]+),", m.group(1)))
    values = [v for _, v in sc]
    assert set(values) == set(sc_map) == set(scx_map)
    source = source.replace(m.group(0),
        names_const("SCRIPT_NAMES", sc, "Script value names: line i selects `SCRIPTS[i]` and `SCRIPT_EXTENSIONS[i]`.")
        + "\n" + tables("SCRIPTS", values, sc_map) + "\n" + tables("SCRIPT_EXTENSIONS", values, scx_map) + "\n"
        + "/// The table of the Script value `name`, or of its Script_Extensions.\n"
        + "pub(crate) fn script_from_str(name: &str, extensions: bool) -> Option<&'static PackedIntervals> {\n"
        + "    let index = lookup_property_name(SCRIPT_NAMES, name)?;\n"
        + "    Some(if extensions { SCRIPT_EXTENSIONS[index] } else { SCRIPTS[index] })\n}\n")
    source = re.sub(r"\n#\[derive\(Debug, Clone, Copy\)\]\npub enum UnicodePropertyValueScript \{\n.*?\n\}\n", "\n", source, count=1, flags=re.S)

    # Properties of strings: the sets are runs of STRING_STARTS.
    m = take(source, r"pub fn unicode_string_property_from_str\(s: &str\) -> Option<UnicodeStringProperty> \{\n    use UnicodeStringProperty::\*;\n    match s \{\n(.*?)\n    \}\n\}\n")
    strings = arms(m.group(1), r"Some\((\w+)\)")
    source = source.replace(m.group(0), "")
    m = take(source, r"pub\(crate\) fn string_property_sets\(value: &UnicodeStringProperty\) -> StringSet \{\n    use UnicodeStringProperty::\*;\n    // A property is a run of the sets; `RGI_Emoji` is all of them.\n    let \(first, last\) = match value \{\n(.*?)\n    \};\n")
    runs = dict(re.findall(r"(\w+) => (\(\d+, \d+\)),", m.group(1)))
    rows = "\n".join(f"    {runs[v]}," for _, v in strings)
    source = source.replace(m.group(0),
        names_const("STRING_PROPERTY_NAMES", strings, "Property-of-strings names: line i selects the run `STRING_PROPERTY_RUNS[i]` of the sets.")
        + f"\n/// Each property of strings as a run of the sets; `RGI_Emoji` is all of them.\nconst STRING_PROPERTY_RUNS: [(usize, usize); {len(strings)}] = [\n{rows}\n];\n\n"
        + "/// The strings of the property of strings `name`.\n"
        + "pub(crate) fn string_property_from_str(name: &str) -> Option<StringSet> {\n"
        + "    let (first, last) = STRING_PROPERTY_RUNS[lookup_property_name(STRING_PROPERTY_NAMES, name)?];\n")
    source = re.sub(r"\n#\[derive\(Debug, Clone, Copy\)\]\npub enum UnicodeStringProperty \{\n.*?\n\}\n", "\n", source, count=1, flags=re.S)
    source, n = re.subn(r"(pub\(crate\) fn string_property_from_str\(name: &str\) -> Option<StringSet> \{\n.*?\n)    StringSet::new\((.*?)\n    \)\n\}\n",
                        lambda m: f"{m.group(1)}    Some(StringSet::new({m.group(2)}\n    ))\n}}\n", source, count=1, flags=re.S)
    assert n == 1

    # The round-trip tests ask for the sets by name now.
    for aliases, variant in strings:
        source = source.replace(f"string_property_sets(&UnicodeStringProperty::{variant})",
                                f'string_property_from_str("{aliases[0]}").unwrap()')
    assert "UnicodeStringProperty" not in source and "UnicodePropertyBinary" not in source

    # The tables are referenced directly; drop the accessors nothing else
    # calls (the identifier properties keep theirs: the lexer and group
    # names use them).
    keep = {"id_start_ranges", "id_continue_ranges"}
    def drop(m):
        return m.group(0) if m.group(1) in keep else ""
    source = re.sub(r"\n/// Return the code point ranges of the '[^']+' Unicode property\.\npub\(crate\) fn (\w+_ranges)\(\) -> &'static PackedIntervals \{\n    &[A-Z0-9_]+\n\}\n", drop, source)

    source = source.replace("use crate::unicode::FoldRange;\n", "use crate::unicode::{FoldRange, lookup_property_name};\n", 1)
    source = re.sub(r"\n{3,}", "\n\n", source)
    PATH.write_text(source, encoding="utf-8", newline="\n")
    print(f"binary {len(binary)}, general category {len(gc)}, script {len(sc)}, strings {len(strings)}")


if __name__ == "__main__":
    main()
