# Calls of the engine's native str methods (and hash, heapq, bisect,
# math.sqrt) from loops long enough to compile, where the native hands the
# call to the runtime's own entry (a generator to join, a tuple of affixes,
# separators of every kind), that entry raises or runs Python code, and the
# site meets other callables: the results, the errors and their tracebacks
# must be CPython's.
import bisect
import heapq
import math
import traceback


def gen(n, fail_at=-1):
    for i in range(n):
        if i == fail_at:
            raise ValueError("gen failed at %d" % i)
        yield "g%d" % i


class Up:
    def upper(self):
        return "UP"

    def split(self, sep=None):
        return ["custom", sep]


out = []
for i in range(3000):
    s = "  a,b,%d  " % i
    out.append(s.strip())
    out.append(s.strip(" ,0123456789"))
    out.append("|".join(s.split(",")))
    out.append("-".join(gen(i % 4)))
    out.append(str(s.startswith(("x", "  a"))))
    out.append(str(s.endswith(("9  ", "8  "), 0, 7)))
    out.append(s.replace(",", ";", 1))
    out.append(str(s.find("b", 4)))
    out.append(s.split(None, 1)[-1])
print(len(out), out[:9], out[-9:])

# One site, receivers changing kind: a str, a user object with the same
# method names, a str subclass, non-ASCII text.
class T(str):
    def upper(self):
        return "T-UPPER"


things = ["abc", Up(), T("t"), "élève", "x" * 3]
got = []
for i in range(2500):
    t = things[i % len(things)]
    got.append(t.upper())
    got.append(repr(t.split()))
print(got[:10], got[-10:])

# A native whose fallback raises, inside a try, every few iterations.
caught = []
for i in range(2000):
    try:
        if i % 7 == 0:
            "".join(gen(5, fail_at=i % 5))
        elif i % 7 == 1:
            "-".join(["a", i])
        elif i % 7 == 2:
            "abc".split("")
        elif i % 7 == 3:
            math.sqrt(-1.0 - i)
        elif i % 7 == 4:
            hash([i])
        elif i % 7 == 5:
            heapq.heappush([1, "a"], None)
        else:
            "abc".replace("a", i)
    except (ValueError, TypeError) as exc:
        # (replace's own message differs from CPython's in this engine.)
        if i < 14 or i > 1990:
            caught.append("%d %s: %s" % (i, type(exc).__name__, exc if i % 7 != 6 else "-"))
print("\n".join(caught))

# The traceback of a raise from a native called in a compiled loop.
def runs(n):
    acc = 0
    for i in range(n):
        acc += len("x,y".split(","))
        if i == n - 1:
            acc += len("".join(gen(3, fail_at=2)))
    return acc


try:
    runs(4000)
except ValueError as exc:
    for f in traceback.extract_tb(exc.__traceback__):
        print(f.name, f.lineno)

# hash, heapq and bisect through the same sites, then rebound.
h = []
vals = []
for i in range(3000):
    heapq.heappush(h, (i * 7919) % 1000)
    if i % 3 == 0:
        vals.append(heapq.heappop(h))
    bisect.insort(vals, i % 11)
    vals.append(hash("k%d" % (i % 5)) == hash("k" + str(i % 5)))
print(len(h), vals[:12], vals[-12:], sum(1 for v in vals if v is True))
real_hash = hash
for i in range(2000):
    if i == 1000:
        hash = lambda x: 42
    v = hash("abc")
    if i in (999, 1000, 1999):
        print(i, v == real_hash("abc"), v == 42)
hash = real_hash
print(hash("q") == real_hash("q"))
