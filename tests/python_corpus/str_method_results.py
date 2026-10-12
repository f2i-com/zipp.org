# The strs the native str methods make (strip, split, replace, join, lower,
# upper) from ASCII and non-ASCII operands in every combination, each then
# measured, indexed from both ends, sliced, compared, hashed into a dict and
# searched: a result must behave as the same text built any other way.
def probe(s):
    mid = len(s) // 2
    return (s, len(s), s[:1], s[-1:], s[mid:mid + 2], s[::-1][:3], s == "".join(list(s)), s.find("é"), s.isascii())


ASCII = ["  pad me  ", "a,b,,c", "MiXeD Case", "xxhixx", "", "   ", "no-change", "a" * 40 + ",b"]
OTHER = ["  café  ", "é,b,ü", "ÉCOLE mixte", "ééhié", "é", " 　x　 ", "naïve,\U0001f600,z"]
SEPS = [",", "é", "x", ", ", "\U0001f600"]

rows = 0
seen = {}
for group in (ASCII, OTHER):
    for s in group:
        for r in (s.strip(), s.lstrip(), s.rstrip(), s.strip("x "), s.strip("é "), s.lower(), s.upper()):
            print("one", probe(r))
            seen[r] = seen.get(r, 0) + 1
            rows += 1
        print("split", [probe(p)[:5] for p in s.split()])
        for sep in SEPS:
            parts = s.split(sep)
            joined = sep.join(parts)
            print("sep", repr(sep), [probe(p)[:5] for p in parts], joined == s, probe(joined)[1:4])
            print("join", probe("-".join(parts)), probe(sep.join(["ab", "cd", s])))
            for old, new in ((sep, "-"), ("a", sep), (sep, sep + sep), ("b", ""), ("zz", sep)):
                print("replace", repr(old), repr(new), probe(s.replace(old, new)))
                rows += 1

# The same in loops long enough to compile, the operands alternating.
total = 0
last = None
texts = ASCII + OTHER
for i in range(3000):
    s = texts[i % len(texts)]
    sep = SEPS[i % len(SEPS)]
    t = sep.join([s.strip().upper(), s.lower().replace("a", sep)])
    parts = t.split(sep)
    total += len(t) + len(parts) + len(parts[-1]) + (t[-1:] == parts[-1][-1:]) + t.count("é")
    last = probe(t)
print("loop", total, last)
print("seen", len(seen), sorted(seen.items())[:6], rows)
