# Assigning a function's __defaults__ / __kwdefaults__ (and del) changes
# how later calls bind, including calls from hot loops and methods.


def add(a, b=10, c=20):
    return a * 10000 + b * 100 + c


def show(label, f, *calls):
    out = []
    for args, kw in calls:
        try:
            out.append(f(*args, **kw))
        except TypeError as e:
            out.append("TypeError: " + str(e))
    print(label, out, f.__defaults__, f.__kwdefaults__)


C = [((1,), {}), ((1, 2), {}), ((1, 2, 3), {}), ((), {}), ((1,), {"c": 5}), ((1,), {"b": 4, "c": 5})]
show("orig", add, *C)
add.__defaults__ = (7, 8)          # same count
show("same", add, *C)
add.__defaults__ = (3,)            # fewer
show("fewer", add, *C)
add.__defaults__ = (4, 5, 6)       # more: a gets one too
show("more", add, *C)
add.__defaults__ = None
show("none", add, *C)
add.__defaults__ = (1, 2)          # back to the original count
show("back", add, *C)
del add.__defaults__
show("deleted", add, *C)
try:
    add.__defaults__ = [1, 2]
except TypeError as e:
    print("TypeError", e)
add.__defaults__ = (10, 20)


def loop(n, f):
    t = 0
    for i in range(n):
        if i == n // 3:
            f.__defaults__ = (1, 1)
        if i == 2 * n // 3:
            f.__defaults__ = (2,)
        t += f(i, 5)
        t += f(i, 5, 6)
        if i < 2 * n // 3:
            t += f(i)
        t = t % 1000003
    return t


print("loop", loop(30000, add))
add.__defaults__ = (10, 20)


def kw(a, *, k=5, m=6):
    return a + k * 10 + m * 100


print("kw", kw(1), kw.__kwdefaults__)
kw.__kwdefaults__ = {"k": 1, "m": 2}
print("kw", kw(1), kw(1, k=3), kw.__kwdefaults__)
kw.__kwdefaults__ = {"m": 9}
try:
    kw(1)
except TypeError as e:
    print("TypeError", e)
print("kw", kw(1, k=0))
kw.__kwdefaults__ = None
print("kw", kw.__kwdefaults__)
try:
    kw(1)
except TypeError as e:
    print("TypeError", e)
try:
    kw.__kwdefaults__ = 3
except TypeError as e:
    print("TypeError", e)


class P:
    def __init__(self, x=1, y=2):
        self.v = x * 10 + y

    def m(self, d=3):
        return self.v + d


def make(n):
    t = 0
    for i in range(n):
        if i == n // 2:
            P.__init__.__defaults__ = (5, 6)
            P.m.__defaults__ = (100,)
        p = P()
        q = P(i % 7)
        t += p.v + q.v + p.m() + q.m(1)
    return t


print("class", make(20000), P().v, P(1).v, P().m())
P.__init__.__defaults__ = (7,)
try:
    P()
except TypeError as e:
    print("TypeError", e)
print("class", P(1).v)


def gen(a, b=2):
    yield a
    yield b


print("gen", list(gen(1)))
gen.__defaults__ = (9,)
print("gen", list(gen(1)))


def outer():
    def inner(x, y=1):
        return x + y
    return inner


f = outer()
print("closure", f(1))
f.__defaults__ = (50,)
print("closure", f(1), outer()(1))
lam = lambda x, y=3: x * y
lam.__defaults__ = (4,)
print("lambda", lam(2), [lam(i) for i in range(3)])
