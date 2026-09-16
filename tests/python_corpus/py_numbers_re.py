# re fidelity: group spans from the real match positions, unmatched groups,
# code-point offsets past astral characters, fullmatch backtracking, `$`
# before a trailing newline, inline and scoped flags, Unicode classes,
# brace and bracket literals, split/sub empty-match rules, pos/endpos, and a
# callback that runs its own pattern.
import re

m = re.search(r"(a)b(a)", "aba")
print("group-span", m.span(1), m.span(2), m.start(2), m.end(2), re.search(r"(a)(a)", "xaa").span(2))
m = re.search(r"(a)|(b)", "b")
print("unmatched", m.span(1), m.start(1), m.end(1), m.group(1), m.groups(), m.groups("-"), m.lastindex, m.span(2))
m = re.search(r"(b)?c", "ac")
print("optional", m.start(1), m.span(), m.group(0), m.regs)
s = "\U0001F600 hello \U0001F600 world"
m = re.search("hello", s)
print("astral", m.span(), s[m.start():m.end()], re.search("b", "\U0001F600b").start(), [x.span() for x in re.finditer(r"\w+", s)])
print("astral-sub", re.sub("", "-", "a\U0001F600"), re.split("", "\U0001F600x"), re.findall(".", "a\U0001F600b"))
print("repr", re.search(r"l+", "hello"), re.match("x", "y"), re.search("o", s))

# fullmatch tries the other alternatives; match anchors at pos only.
print("fullmatch", re.fullmatch(r"a|ab", "ab"), re.fullmatch(r"a*?", "aaa"), re.fullmatch("a", "ab"), re.compile(r"\d+").fullmatch("x123y", 1, 4))
p = re.compile(r"(?:a|b)c")
print("match-pos", p.match("xac", 1), p.match("xac"), p.search("xxac", 3), p.search("acxac", 1, 4), p.match("ac", 0, 1))

# `$` matches before a trailing newline; \A and \Z are the string ends even under MULTILINE.
print("dollar", re.search(r"foo$", "foo\n"), re.match(r"\d+$", "123\n") is not None, re.findall(r"^\w+$", "a\nb\n", re.M), re.search(r"a\Z", "a\n"))
print("anchors", re.findall(r"\A\w", "a\nb", re.M), re.findall(r"^\w", "a\nb\rc", re.M), re.sub(r"$", "!", "x\ny\n"), re.sub(r"(?m)$", "!", "x\ny"))

# Inline global flags and scoped flag groups.
print("inline", re.search(r"(?i)HELLO", "say hello").span(), re.findall(r"(?s)a.b", "a\nb"), re.findall(r"(?x) a \s b  # comment", "a b"), re.compile("(?i)x").flags)
print("scoped", re.findall(r"(?i:a)b", "Ab AB ab"), re.findall(r"a(?s:.)b.", "a\nbca\nb\n"), re.findall(r"(?-i:a)b", "ab Ab aB", re.I))
print("flags", re.compile("a").flags, re.compile("a", re.I | re.M).flags, re.compile("a", re.A).flags, re.compile(r"a", re.I | re.S))

# \w, \d, \s and \b are Unicode-aware for str patterns unless re.ASCII.
print("unicode", re.findall(r"\w+", "na\xefve caf\xe9 x_1"), re.findall(r"\w+", "na\xefve caf\xe9", re.A), re.findall(r"\d", "4٤"), re.findall(r"\b\w", "\xe9t\xe9 \xe0 x"))
print("spaces", re.split(r"\s+", "a\xa0b\x1cc\ufeffd"), re.findall(r"[\s]", "a b\tc"), re.findall(r"[\w-]+", "ab-c d\xe9"))

# Braces and brackets that are not syntax are literals; a leading ']' is in the set.
print("literals", re.findall(r"[]a]", "]a"), re.findall(r"a{b", "a{b"), re.findall(r"x{,2}", "xxx"), re.findall(r"a{2}", "aaaa"), re.findall(r"[^]]", "]x]"), re.findall(r"\-\#\&", "-#&"))
print("verbose-class", re.findall(r"[ ]x", " x", re.X), re.findall(r"[#]\d # digits", "#1 #2", re.X))
print("escape", re.escape("a.b*c"), re.escape("x-y z#"), re.escape("\xe9!"), re.search(re.escape("1+1=2"), "is 1+1=2?").group())

# Named groups, backreferences, groupdict, lastgroup, expand.
m = re.match(r"(?P<first>\w+) (?P<last>\w+)", "Jane Doe")
print("named", m.group("first"), m["last"], m.groupdict(), m.span("last"), m.lastgroup, m.lastindex, m.expand(r"\g<last>, \1"), re.compile(r"(?P<x>a)(b)").groupindex)
print("backref", re.findall(r"(\w)\1", "aabbcd"), re.sub(r"(?P<w>\w+) (?P=w)", r"\g<w>", "the the cat"), re.search(r"((a)b)", "ab").lastindex)

# split and sub follow the 3.7+ empty-match rules.
print("split", re.split(r"x*", "axbc"), re.split(r"\b", "a b"), re.split(r"(,)", "a,b"), re.split(",", "a,b,c", 1), re.split(",", "a,b,c", maxsplit=1), re.split(r"\d", "a1b2c3", flags=re.I))
print("sub", re.sub("x*", "-", "abxd"), re.subn("a", "b", "aaa", 2), re.sub(r"(\w)(\d)", r"\2\1", "a1b2"), re.sub("a", r"\n", "bab") == "b\nb", re.sub(r"(a)|b", r"[\1]", "ab"))
for pat, rep in [(r"(a)", r"\2"), (r"a", r"\q")]:
    try:
        re.sub(pat, rep, "a")
    except (re.error, IndexError) as e:
        print("sub-error", pat)

# A replacement callable may run the same pattern again.
env = {"a": "{b}", "b": "B", "c": "C"}


def expand(text):
    return re.sub(r"\{(\w+)\}", lambda m: expand(env[m.group(1)]), text)


word = re.compile(r"\w+")
print("reentrant", expand("x {a} y {c} z"), word.sub(lambda m: str(len(word.findall("a b c"))) + m.group(), "hi yo"),
      word.sub(lambda m: word.sub("*", m.group()), "ab cd"))

# Group offsets of matches found by a scan, a callback, a cut window, a
# lookbehind, pos-anchored match and fullmatch.
kv = re.compile(r"(\w)=(\d+)?")
print("scan-spans", [(m.span(1), m.span(2), m.lastindex, m.regs) for m in kv.finditer("a=1 b= c=22", 2, 10)])
print("sub-spans", kv.sub(lambda m: "%s%s%s" % (m.span(), m.start(2), m.end(1)), "a=1 b= c=22"))
print("lookbehind", [m.span(1) for m in re.finditer(r"(?<=\$)(\d+)", "$1 x2 $33")], kv.match("zz c=5", 3).span(2), kv.fullmatch("q=77").span(2))
for bad in ["(", "a)", "[a", "a**", "(?<n>x)"]:
    try:
        re.compile(bad)
        print("compiled", bad)
    except re.error:
        print("re.error", bad)
