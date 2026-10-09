"""The native str methods (strip/lstrip/rstrip with characters, split, replace,
find, join, lower, upper): every op the engine serves natively reads its
receiver and arguments where they live, so these cover the edges around each:
empty and absent matches, overlapping and multi-byte text, ropes, the
separator among the parts, non-str parts, and a str subclass."""
out = []


def show(tag, v):
    out.append(tag + "=" + repr(v))


texts = ["", "a", "  Alpha,beta,GAMMA,delta  ", "xxhixx", "aaa", "café ☃ snow", "A\U0001F600b", "\t\n mixed \r\n", "ab" * 40, "MiXeD Case 123"]
for i, t in enumerate(texts):
    show("strip%d" % i, [t.strip("x"), t.lstrip("x a"), t.rstrip(" \n"), t.strip(""), t.strip("☃w")])
    show("split%d" % i, [t.split(), t.split(","), t.split("a"), t.split("aa"), t.split("é")])
    show("repl%d" % i, [t.replace("a", "b"), t.replace("aa", "a"), t.replace("zz", "y"), t.replace("a", ""), t.replace("é", "e"), t.replace(",", ", ")])
    show("find%d" % i, [t.find("a"), t.find(""), t.find("zz"), t.find("☃"), t.find("b"), t.find(t), t.find(t + "x")])
    show("case%d" % i, [t.lower(), t.upper()])

# identity when nothing changes
s = "nochange"
show("same", [s.strip("z") is s or s.strip("z") == s, s.replace("q", "w") == s, s.lower() == s, "UP".upper() == "UP"])

# join: lists, tuples, the separator among the parts, one part, none, non-ASCII, ropes
parts = ["a", "bb", "", "café", "\U0001F600"]
rope = ""
for i in range(30):
    rope += "ab"
show("join", ["-".join(parts), "".join(parts), ", ".join(tuple(parts)), "x".join([]), "x".join(["only"]),
              "☃".join(["a", "b", "c"]), rope.join(["<", ">", "!"]), "-".join([rope, rope])])
sep = "+"
show("joinself", sep.join([sep, sep, sep]))
for bad in ([1, "a"], ["a", None], ["a", b"b"]):
    try:
        "-".join(bad)
        show("joinbad", "no error")
    except TypeError as e:
        show("joinbad", str(e))
show("joingen", "-".join(str(i) for i in range(5)))
show("joinstr", "-".join("abc"))

# a str subclass and a non-str receiver argument take the ordinary path
class S(str):
    def strip(self, chars=None):
        return "custom"

show("sub", [S("  x  ").strip(), S("a,b").split(","), S("AbC").lower(), "-".join([S("p"), "q"])])
for call in (lambda: "abc".find(1), lambda: "abc".replace("a", 1), lambda: "abc".split(""), lambda: "abc".strip(1)):
    try:
        show("bad", call())
    except (TypeError, ValueError) as e:
        show("bad", type(e).__name__)

# hot loop over the same operations, as the benchmark does
line = "  Alpha,beta,GAMMA,delta  "
total = 0
last = ""
for i in range(3000):
    fields = line.strip().split(",")
    lowered = fields[2].lower()
    if fields[0].startswith("Al"):
        total += len(lowered)
    last = "-".join([fields[1].upper(), lowered.replace("m", "n")])
    total += last.find("GAN") + 1
show("loop", [total, last])
print("\n".join(out))
