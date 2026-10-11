# One call site meeting a callee that changes under it: new defaults, a
# deleted __defaults__, the name rebound, closures of one code object over
# different cells, a method replaced on the class, a generator function
# and a builtin at the same site, and recursion past the limit.
import sys


def inc(a, b=1):
    return a + b


def run_inc(n):
    out = []
    for i in range(n):
        out.append(inc(i))
        if i == 3:
            inc.__defaults__ = (10,)
        if i == 6:
            inc.__defaults__ = None
            try:
                inc(i)
            except TypeError as e:
                out.append(str(e))
            inc.__defaults__ = (100,)
    return out


print(run_inc(9))


def three(a, b=2, c=3):
    return (a, b, c)


def run_three():
    out = []
    for i in range(6):
        out.append(three(i))
        out.append(three(i, -1))
        if i == 2:
            three.__defaults__ = ("x", "y")
        if i == 4:
            del three.__defaults__
            try:
                three(i)
            except TypeError as e:
                out.append(str(e))
            three.__defaults__ = (7, 8)
    return out


print(run_three())


def f(x):
    return x + 1


def g(x):
    return x * 2


def run_rebind():
    global f
    out = []
    for i in range(8):
        out.append(f(i))
        if i == 3:
            f = g
    return out


print(run_rebind())


def make(k):
    def add(x):
        return x + k
    return add


def run_closures():
    fns = [make(1), make(10), make(100)]
    total = 0
    for i in range(30):
        total += fns[i % 3](i)
    return total


print(run_closures())


class C:
    def m(self, x):
        return x + 1


def run_method():
    c = C()
    out = []
    for i in range(6):
        out.append(c.m(i))
        if i == 2:
            C.m = lambda self, x: x * 100
        if i == 4:
            c.m = lambda x: -x
    return out


print(run_method())


def gen(n):
    yield n
    yield n + 1


def run_mixed():
    out = []
    for fn in (inc, gen, len, inc, abs, gen, f):
        r = fn(5) if fn is not len else fn("abc")
        out.append(list(r) if fn is gen else r)
    return out


print(run_mixed())


def depth(n):
    return depth(n + 1)


try:
    depth(0)
except RecursionError as e:
    print("RecursionError", type(e).__name__)


def kw(a, b=2, *, c=3):
    return a + b + c


def run_kw():
    return [kw(1), kw(1, 1), kw(1, c=0), kw(2, b=0, c=0)]


print(run_kw())
