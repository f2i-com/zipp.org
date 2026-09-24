# Calls through the positional call sites: plain functions (with defaults,
# every count up to seven), lambdas, generator functions, builtins (first and
# later calls), str methods, native entries (hash, math.sqrt, heapq,
# bisect), bound methods, and class construction every way a class can be
# called: no __init__, __init__ with 0..6 arguments and defaults, __init__
# returning a value, __init__ replaced after instances exist, __new__,
# a metaclass __call__, subclasses of builtins and of exceptions, keyword
# construction, and construction from inside __init__ (recursion).
import bisect
import heapq
import math


def f0():
    return "f0"


def f1(a):
    return ("f1", a)


def f7(a, b, c, d, e, f, g):
    return a + b + c + d + e + f + g


def fdef(a, b=10, c=20):
    return a + b + c


def gen(n):
    for i in range(n):
        yield i * i


add = lambda x, y: x + y
print(f0(), f1(1), f7(1, 2, 3, 4, 5, 6, 7), fdef(1), fdef(1, 2), fdef(1, 2, 3), add(2, 3), list(gen(4)))
print(abs(-3), abs(-4), min(3, 1), max(3, 1), len("abcd"), round(2.5), divmod(7, 2), pow(2, 5), repr("x"), str(12), int("42"), float("1.5"))
print("AbC".lower(), "AbC".upper(), "  x ".strip(), "a,b".split(","), "xax".strip("x"), "abc".replace("b", "B"), "abc".find("c"))
print(hash(7) == hash(7), math.sqrt(16.0), math.sqrt(2) ** 2 > 1.99)
h = []
for v in [5, 1, 4, 2, 3]:
    heapq.heappush(h, v)
print([heapq.heappop(h) for _ in range(5)])
arr = [1, 3, 5]
bisect.insort_left(arr, 4)
print(arr, bisect.bisect_left(arr, 4), bisect.bisect_right(arr, 4))
print(list(), dict(), tuple([1, 2]), list(range(3)), dict(a=1), set([1, 1]), frozenset(), bool(0), type(3).__name__)


class Empty:
    pass


class P0:
    def __init__(self):
        self.tag = "p0"


class P2:
    def __init__(self, a, b=5):
        self.a = a
        self.b = b


class P6:
    def __init__(self, a, b, c, d, e, f):
        self.s = a + b + c + d + e + f


class Bad:
    def __init__(self):
        return 3


class Swapped:
    def __init__(self, x):
        self.x = x


class WithNew:
    def __new__(cls, x):
        obj = super().__new__(cls)
        obj.made = "new"
        return obj

    def __init__(self, x):
        self.x = x


class Meta(type):
    def __call__(cls, *args):
        return ("meta", cls.__name__, args)


class Metad(metaclass=Meta):
    def __init__(self, x):
        self.x = x


class MyList(list):
    def __init__(self, items, tag):
        super().__init__(items)
        self.tag = tag


class MyErr(ValueError):
    pass


class Node:
    def __init__(self, depth):
        self.depth = depth
        self.child = Node(depth - 1) if depth > 0 else None


e = Empty()
print(type(e).__name__, P0().tag, P2(1).a, P2(1).b, P2(1, 2).b, P6(1, 2, 3, 4, 5, 6).s)
try:
    Bad()
except TypeError as err:
    print("TypeError:", err)
first = Swapped(1)


def new_init(self, x):
    self.x = x * 100


Swapped.__init__ = new_init
print(first.x, Swapped(2).x)
del Swapped.__init__
try:
    Swapped(3)
except TypeError as err:
    print("TypeError:", err)
print(Swapped().__class__.__name__)
w = WithNew(5)
print(w.made, w.x, Metad(1, 2))
ml = MyList([1, 2], "t")
print(ml, ml.tag, len(ml), isinstance(ml, list))
err = MyErr("bad", 2)
print(repr(err), err.args, isinstance(err, ValueError))
n = Node(4)
depth = 0
while n is not None:
    depth += 1
    n = n.child
print("depth", depth, P2(a=7).a, P2(1, b=9).b)
try:
    P2()
except TypeError as err:
    print("TypeError:", err)
try:
    P0(1)
except TypeError as err:
    print("TypeError:", err)
try:
    Empty(1)
except TypeError as err:
    print("TypeError:", err)


class Counter:
    count = 0

    def __init__(self):
        Counter.count += 1
        self.n = Counter.count


made = [Counter() for _ in range(5)]
print([c.n for c in made], Counter.count)


class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def moved(self, dx):
        return Point(self.x + dx, self.y)


p = Point(0, 0)
for i in range(100):
    p = p.moved(1)
print(p.x, p.y, vars(p))
ctors = [Empty, P0, Counter]
print([type(c()).__name__ for c in ctors], [f(3) for f in (f1, abs, str, P2)][:3])


# Builtin names read through the module globals (hashed once they are many),
# then shadowed at module level, then unshadowed.
def lengths(xs):
    return [len(x) for x in xs]


words = ["a", "bb", "ccc"]
print(lengths(words))
len = lambda x: -1  # noqa: E731
print(lengths(words))
del len
print(lengths(words), max(3, 9), min([4, 2]), abs(-2))


# super() method calls: single and multiple inheritance, a method found
# further up the MRO, super().__init__, and a class attribute reached
# through super.
class Base:
    tag = "base"

    def __init__(self, v):
        self.v = v

    def scale(self, a):
        return a * 2

    def describe(self):
        return "Base(%s)" % self.v


class Left(Base):
    def scale(self, a):
        return super().scale(a) + 1

    def describe(self):
        return "Left>" + super().describe()


class Right(Base):
    def scale(self, a):
        return super().scale(a) * 10

    def describe(self):
        return "Right>" + super().describe()


class Both(Left, Right):
    def __init__(self, v):
        super().__init__(v * 2)

    def scale(self, a):
        return super().scale(a) - 1

    def describe(self):
        return "Both>" + super().describe() + ":" + super().tag


objs = [Base(1), Left(2), Right(3), Both(4)]
print([o.scale(5) for o in objs], [o.describe() for o in objs], [c.__name__ for c in Both.__mro__])
total = 0
for i in range(200):
    total += objs[i % 4].scale(i)
print(total)
