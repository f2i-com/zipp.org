# `str % values` at literal and variable left operands, in straight-line
# code and in loops long enough to compile, over the forms the engine
# formats itself (`%d`, `%i`, `%s`, `%%`, widths, flags, tuples) and the
# ones it hands to the runtime (`%r`, `%x`, `%f`, precision, mappings,
# objects, wrong counts and kinds): CPython's text and errors.
class P:
    def __str__(self):
        return "P!"

    def __repr__(self):
        return "P()"


fmt_d = "%d-%s"
cases = [
    ("r%d", 5), ("r%d", -12), ("%5d|", 42), ("%-5d|", 42), ("%05d", -3), ("%s", "x"), ("%s", None),
    ("%s", 2.5), ("%s %s", (1, "a")), ("%%%d%%", 7), ("%i", True), ("%s", P()), ("%r", P()),
    ("%x", 255), ("%.2f", 3.14159), ("%(a)s", {"a": 1}), ("%s", (1,)), ("%s", [1, 2]), ("%d", 2 ** 70),
    ("%3s|", "é"), ("%s and %s", ("x", "y")),
]
for f, v in cases:
    print(repr(f % v))
for f, v in [("%d", "x"), ("%s %s", (1,)), ("%d", (1, 2))]:
    try:
        f % v
    except (TypeError, ValueError) as exc:
        print(type(exc).__name__, exc)
print(repr("abc" % ()))

out = []
for i in range(3000):
    out.append("r%d" % (i % 97))
    out.append(fmt_d % (i, "t"))
    out.append("%s|%5d|%-4s|" % ("k", i, "v"))
    if i % 500 == 0:
        out.append("%r" % ("q",))
        out.append("%.1f" % (i / 7))
s = "".join(out)
print(len(s), out[:6], out[-3:])
total = 0
for i in range(2000):
    v = (i, "a") if i % 2 else {"x": i}
    f = "%s-%s" if i % 2 else "%(x)d"
    total += len(f % v)
print(total)
