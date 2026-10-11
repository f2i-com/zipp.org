# f-strings whose parts are ints: every shape must give a str, with the
# int's decimal digits, wherever the int stands among the parts.
a, b, neg, zero = 1, 2, -7, 0
big = 2 ** 46 + 5
huge = -(2 ** 70) + 3
print(repr(f"{a}"), type(f"{a}").__name__, len(f"{a}"), f"{a}" == "1")
print(repr(f"{a}{b}"), repr(f"{neg}{b}"), repr(f"{a}{b}{neg}"))
print(repr(f"{a}x"), repr(f"x{a}"), repr(f"x{a}{b}"), repr(f"{a}x{b}"))
print(repr(f"{neg}"), repr(f"[{neg}]"), repr(f"{zero}|{zero}"))
print(repr(f"{big}"), repr(f"<{big}>"), repr(f"{huge}{big}"), repr(f"={huge}"))
print(repr(f"{neg}x"), repr(f"{huge}x"), repr(f"{big}x{neg}"), repr(f"{zero}x"), repr(f"{neg}{neg}x"))
print(repr(f"{True}{a}"), repr(f"{a}{True}"), repr(f"{1.5}{a}"), repr(f"{a}{2.0}"))
print(repr(f"{a!r}{b}"), repr(f"{a:>3}{b}"), repr(f"{a}{b:03d}"))
width = 6
print(repr(f"{'ab':{width}}"), repr(f"{a:{width}}|"), repr(f"{a}{b:{width}}"))
print(f"{a}" + "x", "x" + f"{b}", f"{a}{b}" * 2)
s = "s"
print(repr(f"{s}{a}"), repr(f"{a}{s}"), repr(f"{s}{a}{s}"))
parts = []
for i in range(-3, 1200):
    parts.append(f"item{i}")
    parts.append(f"{i}")
    parts.append(f"{i}{i}")
print(len("".join(parts)), parts[0], parts[1], parts[2], parts[-3], parts[-2], parts[-1])
print(all(type(p) is str for p in parts))
