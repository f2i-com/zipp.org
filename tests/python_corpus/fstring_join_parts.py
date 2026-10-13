# f-strings of one to fourteen parts (strs, ints of every size, and values
# formatted by the runtime), joined in straight-line code and in loops long
# enough to compile: the text, its type and length must be CPython's.
a, b, neg, zero = 1, 2, -7, 0
big = 2 ** 46 + 5
wide = -(2 ** 70) + 3
huge = 3 ** 90
s, e, accent, wide_ch, astral = "s", "", "café", "中文", "\U0001f600"


class Pt:
    def __init__(self, x):
        self.x = x

    def __repr__(self):
        return "Pt(%d)" % self.x

    def __format__(self, spec):
        return "<pt %d %s>" % (self.x, spec)


class S(str):
    pass


p = Pt(4)
sub = S("sub")
print(repr(f"{s}"), repr(f"{a}"), repr(f"{e}"), repr(f"{e}{e}"), repr(f"x{e}y"))
print(repr(f"{a}{b}"), repr(f"{neg}{zero}"), repr(f"{big}{wide}"), repr(f"{huge}"), repr(f"{huge}|{neg}"))
print(repr(f"{a}{b}{a}{b}{a}{b}{a}{b}{a}{b}{a}{b}"), repr(f"{a}-{b}-{a}-{b}-{a}-{b}-{a}"))
print(repr(f"{a}{b}{a}{b}{a}{b}{a}{b}{a}{b}{a}{b}{a}"), repr(f"{a}{b}{a}{b}{a}{b}{a}{b}{a}{b}{a}{b}{a}{b}"))
print(repr(f"{accent}{a}"), repr(f"{a}{wide_ch}{b}"), repr(f"{astral}{astral}"), len(f"{astral}{a}{accent}"))
print(repr(f"{True}{a}"), repr(f"{a}{False}"), repr(f"{1.5}{a}"), repr(f"{a}{-0.0}"), repr(f"{None}{a}"))
print(repr(f"{p}"), repr(f"{p}{a}"), repr(f"{p!r}{a}"), repr(f"{p:>3}{a}"), repr(f"{a}{p:x}"))
print(repr(f"{sub}"), type(f"{sub}").__name__, repr(f"{sub}{a}"), type(f"{sub}{a}").__name__)
print(repr(f"{a!r}{s!r}"), repr(f"{s!a}{accent!a}"), repr(f"{a:>4}{b:<3}|"), repr(f"{big:,}{huge:x}"))
print(repr(f"{[a, b]}{(a,)}"), repr(f"{ {a: b} }{a}"), repr(f"{a + b}{a * neg}"))
hi, lo = chr(0xD800), chr(0xDC00)
# (A lone surrogate's neighbours are ints here: two adjacent ones, and the
# length of a str holding two, read differently in this engine before.)
lone = f"{hi}{a}"
print(len(lone), [ord(c) for c in lone], f"{hi}{lo}" == hi + lo)
n = 12
print(repr(f"{n}{'x' * 3}{n:{n}}"), repr(f"{f'{a}{b}'}{f'{neg}'}"))
print(type(f"{a}{b}").__name__, type(f"{a}").__name__, f"{a}{b}" == "12", f"{a}" + f"{b}" == "12")

# Loops long enough to compile, the parts changing kind as they go.
out = []
for i in range(-50, 3000):
    out.append(f"item{i}")
    out.append(f"{i}")
    out.append(f"{i}{i}")
    out.append(f"<{i}|{s}|{i * big}>")
print(len("".join(out)), out[0], out[1], out[2], out[3], out[-4], out[-3], out[-2], out[-1])
print(all(type(t) is str for t in out))
values = [1, -2, "s", 2.5, True, None, big, wide, huge, accent, astral, p, sub, (1, 2), [3], "été", 0, -0.0]
seen = []
for i in range(4000):
    v = values[i % len(values)]
    w = values[(i * 7) % len(values)]
    t = f"{v}:{w}|{i}"
    if i % 397 == 0 or i >= 3990:
        seen.append(t)
    if type(t) is not str:
        print("not a str", i)
print(seen)
for i in range(3000):
    t = f"{hi if i % 2 else lo}{i}{lo if i % 3 else hi}"
    if i >= 2994:
        print([ord(c) for c in t])
total = 0
for i in range(5000):
    t = f"{i}{i + 1}{i + 2}{i + 3}{i + 4}{i + 5}{i + 6}{i + 7}{i + 8}{i + 9}{i + 10}{i + 11}"
    u = f"{i}{i + 1}{i + 2}{i + 3}{i + 4}{i + 5}{i + 6}{i + 7}{i + 8}{i + 9}{i + 10}{i + 11}{i + 12}"
    total += len(t) + len(u)
print(total, t, u)
