# Keyword calls at warm sites: the in-order form becomes a positional call
# once seen; everything else binds keywords as before.


def f(a, b, c=3, d=4):
    return (a, b, c, d)


def po(a, b, /, c=0):
    return (a, b, c)


def kwo(a, *, b=2):
    return (a, b)


def star(a, *rest, b=0, **kw):
    return (a, rest, b, sorted(kw.items()))


def gen(a, b=1):
    yield a
    yield b


results = []
for i in range(4):
    results.append(f(a=i, b=i + 1))
    results.append(f(i, b=2))
    results.append(f(i, 1, c=9))
    results.append(f(b=5, a=i))
    results.append(f(i, 2, d=8))
    results.append(f(a=1, b=2, c=3, d=i))
    results.append(po(1, 2, c=i))
    results.append(kwo(i, b=5))
    results.append(star(i, b=1, z=2))
    results.append(list(gen(a=i)))
    results.append(list(gen(i, b=7)))
print(results)
for bad in [lambda: f(a=1), lambda: f(1, a=2), lambda: f(1, 2, e=3), lambda: po(a=1, b=2),
            lambda: kwo(a=1, c=2), lambda: f(b=1)]:
    try:
        bad()
    except TypeError as e:
        print("TypeError", e)


class P:
    def __init__(self, x, y=0):
        self.x = x
        self.y = y

    def m(self, a, b=1):
        return a * b


p = P(x=1, y=2)
q = P(3, y=4)
print(p.x, p.y, q.x, q.y, p.m(a=2, b=3), p.m(2, b=5), P(y=9, x=8).y)


def redefine(a, b):
    return "first"


calls = []
for i in range(3):
    calls.append(redefine(a=1, b=2))
    if i == 1:
        def redefine(b, a):
            return "second " + str(a) + str(b)
print(calls)
g = f
for i in range(3):
    if i == 2:
        g = po
    try:
        print(g(1, 2, c=i))
    except TypeError as e:
        print("TypeError", e)
