# f-strings: no spec, conversions, nested and computed specs, fill/align/sign/width,
# __format__/__str__/__repr__ dispatch, debugging `=`, mixed literal text.
class Money:
    def __init__(self, cents):
        self.cents = cents

    def __format__(self, spec):
        if spec == "":
            return f"${self.cents / 100:.2f}"
        return f"${self.cents / 100:{spec}}"

    def __str__(self):
        return "Money-str"

    def __repr__(self):
        return "Money-repr"


class OnlyRepr:
    def __repr__(self):
        return "<only repr>"


class OnlyStr:
    def __str__(self):
        return "only-str"


m = Money(12345)
print(f"{m}|{m!s}|{m!r}|{m:>12.1f}|{m:,.3f}")
print(f"{OnlyRepr()}|{OnlyRepr()!s}|{OnlyStr()}|{OnlyStr()!s}")
values = [0, -7, 42, 3.5, -0.25, "txt", "", None, True, False, [1, "a"], (1,), {"k": 2}, {3}, b"by", 10**20, 1e-9, range(3)]
for v in values:
    print(f"plain:{v} str:{v!s} repr:{v!r} ascii:{v!a} len-{len(str(v))}")
for v in [0, 1, -1, 255, 3.14159, -2.5, True]:
    print(f"[{v:>8}] [{v:<8}] [{v:^8}] [{v:*^9}] [{v:+}] [{v: }] [{v:08}] [{v:=+8}]")
for s in ["", "a", "héllo", "日本"]:
    print(f"[{s:>6}] [{s:<6}] [{s:^6}] [{s:.>6}] [{s:.2}] [{s!r:>10}]")
width, prec = 10, 3
for v in [1 / 3, 1234.5678, -0.001]:
    print(f"{v:{width}.{prec}f}|{v:<{width}.{prec + 2}e}|{v:^{width * 2}.{prec}g}|{v:{'>'}{width}}")
fill, align = "#", "^"
print(f"{'mid':{fill}{align}{width}}", f"{42:{fill}>{width}d}", f"{255:{'#x'}}", f"{255:#{'b'}}")
x, y = 3, 4
name = "val"
print(f"{x=}", f"{x + y=}", f"{name=!s}", f"{name=:>6}", f"{x * y = }", f"{[x, y]=}")
print(f"{'quoted'}", f'{"double"}', f"{'nested ' f'{x}'}", f"{{braces}} {{{x}}}", f"{x!r:>4}")
print(f"{x if x > y else y}", f"{(lambda q: q * 2)(x)}", f"{', '.join(str(i) for i in range(4))}", f"{ {'a': 1}['a'] }")
print(f"{3.0}", f"{1e16}", f"{1.5e-7}", f"{2**70}", f"{-0.0}", f"{float('inf')}", f"{0.1 + 0.2}")
rows = [("apple", 3, 0.5), ("kiwi", 12, 0.25), ("fig", 100, 2.125)]
for item, qty, price in rows:
    print(f"{item:<8}|{qty:>5d}|{price:>7.3f}|{qty * price:>9,.2f}|")
total = sum(q * p for _, q, p in rows)
print(f"{'TOTAL':<8}|{sum(q for _, q, _ in rows):>5}|{'':>7}|{total:>9,.2f}|")
parts = []
for i in range(12):
    parts.append(f"{i:02d}:{i * i:>3}:{i / 7:.2f}")
print(" ".join(parts))
print(f"{12345678:,}", f"{12345678:_}", f"{1234.5:,.1f}", f"{0.000123:.2%}", f"{123:o}", f"{123:X}", f"{-123:#o}", f"{65:c}")
print(f"{'left':<{8}}|", f"{'right':>{4 + 4}}|", "|".join(f"{c:^3}" for c in "abc"))
nested = {"user": {"name": "ada", "langs": ["py", "rs"]}}
print(f"{nested['user']['name'].title()} knows {len(nested['user']['langs'])}: {', '.join(nested['user']['langs'])}")
multi = f"""line1 {x}
line2 {y!r}"""
print(multi)
print(format(m, ">10.2f"), format(OnlyStr()), format(7, ""), "{:>5}|{!r}|{:^5}".format("a", "b", 1), "{0:{1}}".format(3.14159, ".2f"))
