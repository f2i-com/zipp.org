# The direct entries of common str methods, warm, then on arguments they
# hand back to the full method: boxed str subclasses, tuples of prefixes,
# empty separators, non-str arguments, astral characters.


class S(str):
    pass


samples = ["  Alpha,beta,GAMMA  ", "", "x", "aé\U0001F600b,c", S(" sub,class "), "\t\n mixed   space \x0b"]
for _ in range(2):
    for s in samples:
        print(repr(s.lower()), repr(s.upper()), repr(s.strip()), s.startswith("  A"), s.endswith(("m ", "b")),
              s.find(","), s.find("\U0001F600"), s.find(""), repr(s.replace(",", ";")), s.split(","), s.split(),
              "-".join(s.split(",")), s.startswith(("x", "  ")), s.endswith(""))
for bad in [lambda: "a".split(""), lambda: "a".replace(1, "b"), lambda: "a".startswith(1), lambda: "a".find(None),
            lambda: ",".join([1, 2]), lambda: "a".lower(1), lambda: str.lower(5)]:
    try:
        bad()
    except (TypeError, ValueError) as e:
        print(type(e).__name__)
print(str.lower("ABC"), str.split("a b"), str.join(",", ["x", "y"]), ",".join(S(c) for c in "ab"), ",".join(("p", "q")))
print("a,b".split(",", 1), "a b c".split(None, 1), "abcabc".replace("b", "", 1), "abc".find("c", 1), "abc".startswith("b", 1))
print(type(S("A").lower()).__name__, type(",".join([S("a")])).__name__, S("xy").replace("x", "z"))
