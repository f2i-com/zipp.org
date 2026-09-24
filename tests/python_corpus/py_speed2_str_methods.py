# str methods through the native entries: strip/lstrip/rstrip with
# characters, split with and without a separator, replace, find, join,
# lower/upper, and str % formatting -- ASCII, non-ASCII, astral and empty
# strs, str subclasses, bad arguments, and the results' types.
words = ["kamizu,", "..dor!", "", "?", "plain", "caf\u00e9;", "\u00e9t\u00e9.", "\U0001F600x\U0001F600", "  spaced  ", "a.b.c"]
for w in words:
    print(repr(w.strip(".,;!?")), repr(w.lstrip(".,;!?")), repr(w.rstrip(".,;!?")), repr(w.strip("")), repr(w.strip("\U0001F600")), repr(w.strip("\u00e9")))
    print(w.split(), w.split("."), w.split("\U0001F600"), w.split("a", 1), w.rsplit(".", 1))
    print(repr(w.replace(".", "-")), repr(w.replace("\u00e9", "E")), repr(w.replace("zz", "y")), repr(w.replace("a", "")), repr(w.replace("", "_")))
    print(w.find("a"), w.find("\U0001F600"), w.find("x"), w.find(""), w.find("zz"), w.find("\u00e9"), w.lower(), w.upper(), w.upper().lower())
print(" a\tb\nc\x0bd\x0ce\rf  ".split(), "\u00a0x\u2003y\u3000".split())
print("-".join([]), "-".join(["a"]), "-".join(["a", "b", "c"]), "".join(("x", "y")), "\u00e9".join(["\U0001F600", "z"]), ", ".join(str(i) for i in range(3)))
try:
    "-".join(["a", 1])
except TypeError as e:
    print("TypeError:", e)
try:
    "a".split("")
except ValueError as e:
    print("ValueError:", e)
for bad in (None, 5):
    try:
        "abc".strip(bad) if bad is None else "abc".strip(bad)
        print("strip", bad, "ok")
    except TypeError as e:
        print("TypeError", type(bad).__name__)
    try:
        "abc".find(bad)
    except TypeError as e:
        print("TypeError find", type(bad).__name__)


class S(str):
    pass


s = S("  Mixed Case,.  ")
print(repr(s.strip()), repr(s.strip(" ,.")), s.split(), s.lower(), s.upper(), s.find("C"), s.replace("e", "3"), type(s.strip(" ")).__name__, "+".join([s, "t"]))
print("MiXeD \u00c9\u00e9".lower(), "MiXeD \u00c9\u00e9".upper(), "stra\u00dfe".upper(), "\u0130".lower() == "i\u0307")
big = "ab" * 50000
print(len(big.replace("a", "xyz")), len(big.split("b")), big.find("ba"), len(big.strip("ab")), len(" ".join([big, big])))
print("user%d" % 7, "r%d" % -3, "%s-%s" % ("a", 1), "%5d|%-5d|%05d|%-05d" % (42, 42, -42, 42), "%3s|%-3s|%s" % ("x", "y", None), "%s %s %s" % (True, 1.5, 1e20), "100%%" % ())
print("%d" % True, "%i" % 10 ** 30, "%s" % (10 ** 25,), "%s" % "\u00e9\U0001F600", "%4s|" % "\u00e9", "%s" % [1, 2], "%s" % (1,), "%r" % "q", "%x" % 255, "%.2f" % 2.5)
for fmt, args in (("%d", "x"), ("%d %d", (1,)), ("%s", (1, 2)), ("%(a)s", {"a": 5})):
    try:
        print(repr(fmt % args))
    except (TypeError, ValueError, KeyError) as e:
        print(type(e).__name__)
import math
ws = ["", "a", "hello", "caf" + chr(0xe9), chr(0x1F600) + "x", "x" * 1000, "zipp" * 7]
print([hash(w) == hash(w[:]) for w in ws], len({hash(w) for w in ws}), {w: i for i, w in enumerate(ws)}[ws[3]])
print(math.sqrt(2.0), math.sqrt(0.0), math.sqrt(-0.0), math.sqrt(4), math.sqrt(True), math.sqrt(float("inf")), math.sqrt(float("nan")), math.sqrt(10 ** 30))
for bad in (-1.0, -1, "x", None):
    try:
        math.sqrt(bad)
    except (ValueError, TypeError) as e:
        print(type(e).__name__)
