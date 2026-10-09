#!/usr/bin/env python3
"""Apply ZIPP's storage patch after updating regress's generated Unicode tables.

The original interval literals remain the source of truth. Rust const-evaluates
their gap/width varints, so regeneration needs no downloaded Unicode files.
Also emits exhaustive decoder round-trip tests against those original literals.

The properties of strings (`RGI_Emoji` and the six sets it is made of) are
generated as slices of slices. Their literals stay too, as constants that
only const evaluation reads: the six sets are packed into one pool
(`packed_strings`), and the `RGI_Emoji` literal, which repeats the six in
order, is checked to do exactly that and dropped.
"""
from pathlib import Path
import re

# The sets `RGI_Emoji` is the concatenation of, in its order.
STRING_SETS = [
    ("BASIC_EMOJI", "BasicEmoji"),
    ("EMOJI_KEYCAP_SEQUENCE", "EmojiKeycapSequence"),
    ("RGI_EMOJI_FLAG_SEQUENCE", "RGIEmojiFlagSequence"),
    ("RGI_EMOJI_MODIFIER_SEQUENCE", "RGIEmojiModifierSequence"),
    ("RGI_EMOJI_TAG_SEQUENCE", "RGIEmojiTagSequence"),
    ("RGI_EMOJI_ZWJ_SEQUENCE", "RGIEmojiZWJSequence"),
]
STRING_TABLE = re.compile(r"(?:static|const) (\w+): (?:&\[&\[u32\]; \d+\]|Generated) = &\[\n(.*?)\n\];\n", re.S)
STRING_GETTER = re.compile(r"\n/// Return the code point ranges of the '\w+' Unicode property\.\npub\(crate\) fn \w+_sets\(\) -> &'static \[&'static \[u32\]\] \{\n    \w+\.as_slice\(\)\n\}\n")
STRING_LOOKUP = re.compile(r"pub\(crate\) fn string_property_sets\(value: &UnicodeStringProperty\) -> .*?\n\}\n", re.S)
STRING_POOL = """
// The six sets in `RGI_Emoji`'s order, packed by const evaluation: the
// literals above are read by nothing else.
const STRING_SETS: [Generated; %(count)d] = [
%(names)s
];
const STRING_STARTS: [(usize, usize); %(count)d + 1] = packed_strings::set_starts(&STRING_SETS);
static STRING_LENGTHS: [u8; packed_strings::count_strings(&STRING_SETS)] =
    packed_strings::pack_lengths(&STRING_SETS);
static STRING_CODE_POINTS: [u8; packed_strings::count_code_points(&STRING_SETS) * 3] =
    packed_strings::pack_code_points(&STRING_SETS);
"""
STRING_LOOKUP_NEW = """pub(crate) fn string_property_sets(value: &UnicodeStringProperty) -> StringSet {
    use UnicodeStringProperty::*;
    // A property is a run of the sets; `RGI_Emoji` is all of them.
    let (first, last) = match value {
%(arms)s
        RGIEmoji => (0, %(count)d),
    };
    StringSet::new(
        &STRING_LENGTHS,
        &STRING_CODE_POINTS,
        STRING_STARTS[first],
        STRING_STARTS[last],
    )
}
"""


def string_rows(body):
    return [tuple(int(x) for x in row.split(",") if x.strip()) for row in re.findall(r"&\[([^\]]*)\]", body)]


def pack_string_sets(source):
    """The string-property tables as constants packed into one pool."""
    tables = {m.group(1): m for m in STRING_TABLE.finditer(source)}
    names = [name for name, _ in STRING_SETS]
    assert all(name in tables for name in names), sorted(tables)
    if "RGI_EMOJI" in tables:
        whole = [row for name in names for row in string_rows(tables[name].group(2))]
        assert string_rows(tables["RGI_EMOJI"].group(2)) == whole, "RGI_Emoji is no longer the six sets in order"
        source = source.replace(tables["RGI_EMOJI"].group(0), "")
    for name in names:
        source = re.sub(r"static %s: &\[&\[u32\]; \d+\] = &\[" % name, "const %s: Generated = &[" % name, source)
    source = STRING_GETTER.sub("", source)
    if "const STRING_SETS:" not in source:
        pool = STRING_POOL % {"count": len(names), "names": "\n".join("    %s," % name for name in names)}
        source = source.replace("\n#[derive(Debug, Clone, Copy)]\npub enum UnicodeStringProperty {", pool + "\n#[derive(Debug, Clone, Copy)]\npub enum UnicodeStringProperty {")
    arms = "\n".join("        %s => (%d, %d)," % (variant, i, i + 1) for i, (_, variant) in enumerate(STRING_SETS))
    source, count = STRING_LOOKUP.subn(lambda _: STRING_LOOKUP_NEW % {"arms": arms, "count": len(names)}, source)
    assert count == 1
    imp = "use crate::packed_strings::{self, Generated, StringSet};\n"
    if imp not in source:
        source = source.replace("use crate::unicode::FoldRange;\n", imp + "use crate::unicode::FoldRange;\n")
    checks = ["    let mut all: Vec<Vec<u32>> = Vec::new();"]
    for name, variant in STRING_SETS:
        checks.append(f"    let generated: Vec<Vec<u32>> = {name}.iter().map(|s| s.to_vec()).collect();")
        checks.append(f"    let set = string_property_sets(&UnicodeStringProperty::{variant});")
        checks.append(f"    assert_eq!(set.iter().map(|s| s.to_vec()).collect::<Vec<_>>(), generated, \"{name}\");")
        checks.append("    assert_eq!(set.len(), generated.len());")
        checks.append("    assert_eq!(set.code_point_count(), generated.iter().map(Vec::len).sum::<usize>());")
        checks.append("    all.extend(generated);")
    checks.append("    let whole = string_property_sets(&UnicodeStringProperty::RGIEmoji);")
    checks.append("    assert_eq!(whole.iter().map(|s| s.to_vec()).collect::<Vec<_>>(), all, \"RGI_EMOJI\");")
    checks.append("    assert_eq!((whole.len(), whole.code_point_count()), (all.len(), all.iter().map(Vec::len).sum()));")
    test = "\n#[cfg(test)]\n#[test]\nfn all_packed_string_sets_match_generated_source() {\n" + "\n".join(checks) + "\n}\n"
    return source, test

ROOT = Path(__file__).resolve().parents[1] / "crates/regress-fork/src"
PATTERN = re.compile(r"((?:pub\(crate\) )?(?:const|static) (\w+)): (?:\[Interval; \d+\]\s*=\s*\[|PackedIntervals\s*=\s*(?:packed|direct)_intervals!\[)(.*?)\];", re.S)

for name in ("unicodetables.rs", "unicodetables_unknown.rs"):
    path = ROOT / name
    source = path.read_text(encoding="utf-8")
    source = source.split("\n// ZIPP packed-table round-trip tests.\n")[0]
    tests = []
    def replace(match):
        decl, ident, body = match.groups()
        tests.append(f"    assert_eq!({ident}.to_vec(), {ident}.original, \"{ident}\");")
        encoding = "direct" if ident in ("ID_START", "ID_CONTINUE") else "packed"
        if encoding == "direct":
            # One copy: a `const` is instantiated again in every crate its
            # accessor is inlined into, and these two were in the engine too.
            decl = decl.replace("const ", "static ")
        return f"{decl}: PackedIntervals = {encoding}_intervals![{body}];"
    source, count = PATTERN.subn(replace, source)
    assert count, name
    imp = "use crate::packed_intervals::{PackedIntervals, packed_intervals};\n"
    if imp not in source:
        source = source.replace("use crate::codepointset::Interval;\n", "use crate::codepointset::Interval;\n" + imp)
    if name == "unicodetables.rs" and "use crate::packed_intervals::direct_intervals;" not in source:
        source = source.replace(imp, imp + "use crate::packed_intervals::direct_intervals;\n")
    source = source.replace("-> &'static [Interval]", "-> &'static PackedIntervals")
    string_test = ""
    if name == "unicodetables.rs":
        source, string_test = pack_string_sets(source)
    source += "\n// ZIPP packed-table round-trip tests.\n#[cfg(test)]\n#[test]\nfn all_packed_intervals_match_generated_source() {\n" + "\n".join(tests) + "\n}\n" + string_test
    path.write_text(source, encoding="utf-8", newline="\n")
    print(f"{name}: {count} tables")
