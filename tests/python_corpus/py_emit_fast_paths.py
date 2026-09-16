# Inline fast paths for arithmetic, comparisons, identity and branches:
# every operand type the guards admit, every one they turn away, and the
# edge values (signed zeros, NaN, infinities, big ints, bools) on both sides.
import math

log = []


def t(tag, value):
    log.append(tag)
    return value


def flush(label):
    print(label, " ".join(str(x) for x in log))
    log.clear()


nan = float("nan")
inf = float("inf")
zero = 0.0
big = 10**20


class Num:
    def __init__(self, v):
        self.v = v

    def __add__(self, o):
        return Num(self.v + (o.v if isinstance(o, Num) else o))

    def __radd__(self, o):
        return Num(o + self.v)

    def __sub__(self, o):
        return Num(self.v - (o.v if isinstance(o, Num) else o))

    def __neg__(self):
        return Num(-self.v)

    def __invert__(self):
        return Num(~self.v)

    def __lt__(self, o):
        return Num(self.v < o.v)

    def __gt__(self, o):
        return self.v > o.v

    def __eq__(self, o):
        return isinstance(o, Num) and self.v == o.v

    def __bool__(self):
        return self.v != 0

    def __repr__(self):
        return "Num(%r)" % (self.v,)


# ---- arithmetic -------------------------------------------------------------
ints = [0, 1, -7, 3, big, -big, True, False]
floats = [0.0, -0.0, 1.5, -2.25, 2.0, inf, -inf, nan, 1e308]
for a in ints:
    print("int", a, a + 1, a - 1, a + 3, a - 100, a * 3, a + 2.5)
for a in floats:
    print("float", a, a + 1, a - 1, a + 0.5, a * 2.0, a - a, -a)
for a in [7, -7, 8, -8, big]:
    for b in [2, -2, 3, -3]:
        print("div", a, b, a // b, a % b, a / b, a & b, a | b, a ^ b, ~a)
for a in [1.0, -1.0, 0.0, -0.0, inf, nan, 5.5]:
    for b in [2.0, -0.5, inf, -inf, nan, 3.0]:
        print("fdiv", a, b, a / b, a + b, a - b, a * b if a != 0 else 0.0)
for a, b in [(1.0, 0.0), (1.0, -0.0), (0.0, 0.0), (nan, 0.0), (1, 0.0), (1.5, 0)]:
    try:
        print(a / b)
    except ZeroDivisionError as e:
        print("ZeroDivisionError", e)
print(-zero, -(-zero), -(0.0), -0.0, 0.0 - 0.0, -0.0 + 0.0, -0.0 - 0.0)
print(math.copysign(1.0, -zero), math.copysign(1.0, -0.0 + -0.0))
nz = -0.0
print(repr(nz - 0), repr(nz + 0), repr(nz - -0), repr(nz + -0), repr(zero - 0), big - 0, True - 0, 7 + 0)
nz2 = -0.0
nz2 -= 0
nz3 = -0.0
nz3 += 0
nz4 = -0.0
nz4 -= -0
print(repr(nz2), repr(nz3), repr(nz4))
print(2.0 + 1, 3 + 2.0, 1.5 - 2, 2**53 + 1.0, big + 0.5, 7 / 2, -7 / 2, 1 / 3)
print(True + 1, True - 1, True + True, -True, ~True, -False, True * 3)
print("ab" + "cd", "" + "", "x" * 3, 3 * "y", [1] + [2], (1,) + (2,))
s = "a"
for i in range(5):
    s += "b"
    s = s + str(i)
print(s, -(-5), ~5, ~-1, -(big), ~big, -(2**64))
x = 10
x += 1
x -= 3
x += -2
x -= -4
y = 1.5
y += 1
y -= 2
z = True
z += 1
print(x, y, z, Num(1) + 2, 2 + Num(1), Num(5) - 1, -Num(3), ~Num(3))
for bad in [("a", 1), (1, "a"), (None, 1), ([1], 1)]:
    try:
        print(bad[0] + bad[1])
    except TypeError as e:
        print("TypeError", e)
try:
    print(-"s")
except TypeError as e:
    print("TypeError", e)
try:
    print(~1.5)
except TypeError as e:
    print("TypeError", e)
print(1e308 * 10, -1e308 * 10, 1e308 + 1e308, inf - inf, inf * 0.5)

# ---- comparisons ------------------------------------------------------------
pairs = [(1, 2), (2, 1), (2, 2), (1.5, 2.5), (2.5, 1.5), (nan, 1.0), (1.0, nan),
         (nan, nan), (0.0, -0.0), (inf, big), (1, 1.0), (1, 1.5), (big, big + 1),
         ("a", "b"), ("b", "a"), ("a", "a"), (True, 1), (False, 0.0)]
for a, b in pairs:
    print("cmp", repr(a), repr(b), a < b, a <= b, a > b, a >= b, a == b, a != b)
    flags = []
    if a < b:
        flags.append("lt")
    if a <= b:
        flags.append("le")
    if a > b:
        flags.append("gt")
    if a >= b:
        flags.append("ge")
    if a == b:
        flags.append("eq")
    if a != b:
        flags.append("ne")
    if not a < b:
        flags.append("!lt")
    if not a >= b:
        flags.append("!ge")
    print("   branch", " ".join(flags))
for a, b in [("\uffff", "\U0001f600"), ("\U0001f600", "\uffff"), ("abc", "abd")]:
    print("str order", a < b, a > b, a <= b, a >= b)
    if a < b:
        print("   lt")
    else:
        print("   not lt")
print(Num(1) < Num(2), Num(2) > Num(1), Num(1) == Num(1), Num(1) != Num(2))
if Num(1) < Num(2):
    print("Num lt truthy")
if Num(2) < Num(1):
    print("never")
else:
    print("Num lt falsy")
for a, b in [(1, 1), (None, None), (nan, nan), (0.0, -0.0), ("s", "s"), (big, big)]:
    print("is", a is b, a is not b)
obj = [1]
alias = obj
print(obj is alias, obj is [1], obj is not alias, None is None, None is not obj)
for v in [None, 0, "", [], False, True, 0.0, nan]:
    print("singleton", repr(v), v is None, v is not None, v is True, v is False, v is not False)
    if v is None:
        print("   none branch")
    elif v is not True:
        print("   not-true branch")

# ---- branches: and / or / not, evaluation order ------------------------------
for a in [0, 1, 2]:
    for b in [0, 1]:
        if t("a", a) and t("b", b):
            log.append("and-true")
        if t("a", a) or t("b", b):
            log.append("or-true")
        if not (t("a", a) and t("b", b)):
            log.append("nand")
        if not (t("a", a) or t("b", b)):
            log.append("nor")
        if t("a", a) > 0 and t("b", b) == 1 or t("c", a) == 0:
            log.append("mixed")
        if (t("a", a) and not t("b", b)) or (t("c", a) < 2 and t("d", b) >= 0):
            log.append("nested")
        flush("bool %d %d" % (a, b))
n = 0
while n < 5 and n != 3:
    n += 1
print("while", n)
n = 10
while not n <= 7:
    n -= 1
print("while-not", n)
k = 0.0
while k < 2.5:
    k += 0.5
print("while-float", k)
values = [3, 1.5, "x", None, 0, nan]
print([v for v in values if v is not None and v == v])
print([v for v in values if not isinstance(v, str) and v is not None and v != 0])
print(["hi" if v else "lo" for v in values], [1 if v is None else 2 for v in values])
i = 0
data = [5, 6, 7]
while 0 <= i < len(data):
    i += 2
print("chain-branch", i)
if 1 < 2 < 3 > 2 >= 2 == 2 != 3:
    print("long chain true")
if (m := len(data)) > 2 and m < 10:
    print("walrus in branch", m)
if not (q := 0):
    print("walrus not", q)
if [] or {} or 0 or "" or None:
    print("never")
else:
    print("all falsy")
if Num(0) or Num(3):
    print("Num truth")
if 3 in data or 5 in data:
    print("membership")
if 9 not in data and "z" not in "abc":
    print("non-membership")
print(1 if nan else 0, 1 if 0.0 else 0, 1 if -0.0 else 0, 1 if big else 0)
