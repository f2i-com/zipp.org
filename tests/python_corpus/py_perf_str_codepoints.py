# Code-point semantics of str operations whose fast paths ask the engine
# whether a string holds surrogate units: ASCII, non-ASCII BMP, astral
# characters, concatenated (rope) strings and long strings.

samples = [
    "",
    "plain ascii",
    "café naïve",
    "中文字符",
    "emoji \U0001F600 and \U0001F680!",
    "\U0001F600",
    "mixé\U0001F600中",
    "￿퟿",
]
long_ascii = "abcdefghij" * 5000
long_astral = ("ab\U0001F600" * 2000)
built = "".join(["x", "é", "y"]) + "z" * 100 + "\U0001F600"
samples += [long_ascii, long_astral, built, long_ascii + "\U0001F600"]

for s in samples:
    n = len(s)
    picks = [s[i] for i in (0, n // 2, n - 1)] if n else []
    print(n, picks, s[1:4], s[-3:], s[::max(1, n // 5)][:6], s.find("\U0001F600"), s.rfind("a"),
          s.count("a"), s.upper()[:8], s.strip("ae")[:5], s.split("a")[:3] if n < 60 else len(s.split("a")),
          s.startswith(s[:2]), s.endswith(s[-2:]), s.index(s[-1]) if s else -1)

words = ["b", "a", "é", "\U0001F600", "￿", "zz", "Z", "", "ab", "a\U0001F600", "a￿"]
print(sorted(words), max(words), min(words))
print([a < b for a in words[:6] for b in words[:6]])
print("\U0001F600" > "￿", "￿" < "\U0001F600", "a\U0001F600" < "a￿")
for s in samples[:9]:
    print([ord(c) for c in s][:10], list(reversed(s))[:4], s[::-1][:4])
text = "line one\nline two é\nthird \U0001F600 line\n"
for line in text.split("\n"):
    print(len(line), line.strip().split(" "), [line[i] for i in range(0, len(line), 3)])
print(len(long_ascii + long_ascii), (long_ascii + "é")[-1], (long_ascii + "é")[50000])
