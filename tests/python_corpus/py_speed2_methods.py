# Method and attribute resolution through the inline paths: instance-dict
# shadowing, class changes after first use, builtin-type methods, subclasses
# of builtins, modules (and their globals changing), properties, descriptors,
# __getattr__/__setattr__/__getattribute__, and generator steps via methods.
import math


class Counter:
    def __init__(self):
        self.value = 0

    def bump(self, d):
        self.value += d
        return self.value


c = Counter()
out = []
for i in range(5):
    out.append(c.bump(i))
    if i == 2:
        c.bump = lambda d: "shadowed %d" % d
print(out, c.bump(1), Counter.bump(c, 10), c.value)
del c.bump
print(c.bump(1))
Counter.bump = lambda self, d: "replaced %d" % d
print(c.bump(7))
del Counter.bump
try:
    c.bump(1)
except AttributeError as e:
    print("AttributeError:", e)


class Base:
    def who(self):
        return "base"

    def greet(self, x):
        return "hi %s from %s" % (x, self.who())


class Child(Base):
    def who(self):
        return "child"


objs = [Base(), Child(), Base(), Child()]
print([o.greet(i) for i, o in enumerate(objs)])
Child.who = lambda self: "patched"
print([o.greet(0) for o in objs])


class MyList(list):
    def append(self, x):
        super().append(x * 10)


ml = MyList()
pl = []
for i in range(3):
    ml.append(i)
    pl.append(i)
print(ml, pl, len(ml))
d = {"a": 1}
print(d.get("a"), d.get("b"), d.get("b", 5), d.get("a", 5))


class MyDict(dict):
    def get(self, k, default=None):
        return ("mine", k, default)


print(MyDict(a=1).get("a"), MyDict().get("z", 3))
s = "Hello World"
print(s.lower(), s.upper(), s.split(), s.split("o"), s.startswith("He"), s.find("o"), s.replace("o", "0"), "-".join(["a", s]))


class MyStr(str):
    def lower(self):
        return "custom"


print(MyStr("ABC").lower(), MyStr("ABC").upper())
print(math.sqrt(16.0), math.floor(2.5), math.sqrt(2))
saved = math.sqrt
math.sqrt = lambda x: "fake %s" % x
print(math.sqrt(9))
math.sqrt = saved
print(math.sqrt(9))


class Shape:
    sides = 0

    def __init__(self, n):
        self.n = n

    @property
    def area(self):
        return self.n * 2

    @staticmethod
    def kind():
        return "shape"

    @classmethod
    def make(cls, n):
        return cls(n)

    def __getattr__(self, name):
        return "missing:" + name


sh = Shape.make(3)
print(sh.area, sh.kind(), sh.sides, sh.n, sh.nope, Shape.kind())
sh.sides = 5
print(sh.sides, Shape.sides)
sh.area_cache = 1
print(sh.area_cache)


class Guarded:
    def __init__(self):
        object.__setattr__(self, "log", [])

    def __setattr__(self, name, value):
        self.log.append(name)
        object.__setattr__(self, name, value * 2)


g = Guarded()
for i in range(3):
    g.x = i
print(g.x, g.log)


class Callable:
    def __call__(self, v):
        return "called %s" % v


class Holder:
    tool = Callable()

    def run(self):
        return self.tool(3)


print(Holder().run())


class P:
    def __init__(self):
        self.x = 1
        self.y = 2


p = P()
total = 0
for i in range(100):
    p.x = p.x + p.y
    total += p.x
print(total, p.x)
del p.x
try:
    print(p.x)
except AttributeError as e:
    print("AttributeError:", e)
p.x = "back"
print(p.x, vars(p))
P.y = property(lambda self: "prop-y")
q = P.__new__(P)
print(q.y)


def gen():
    yield from range(3)


it = gen()
print(it.__next__(), next(it), list(it))


class Temp:
    def __init__(self):
        self._c = 0
        self.log = []

    @property
    def c(self):
        return self._c

    @c.setter
    def c(self, v):
        self.log.append(v)
        self._c = v

    @property
    def only(self):
        raise AttributeError("no only")

    def __getattr__(self, name):
        return "fallback:" + name


t = Temp()
for i in range(4):
    t.c = t.c + i
print(t.c, t.log, t.missing_attr)
try:
    t.only = 5
except AttributeError as e:
    print("AttributeError")


class Plain(Temp):
    c = 99


pl = Plain()
print(pl.c)
pl.c = 7
print(pl.c, pl.__dict__.get("c"), pl.log)
Temp.c = property(lambda self: "replaced")
print(t.c, [t.c for _ in range(2)])
del Temp.c
print(t.c)
Temp.c = 3
print(t.c)
t.c = 4
print(t.c, Temp.c)
print(len([1, 2]), len((1,)), len({"a": 1}), len({1, 2, 3}), len(set()), len({}), len(""), len("\u00e9\U0001F600"), len(frozenset([1, 2])))
d = {}
for i in range(50):
    d[i] = i
    d[str(i)] = i
print(len(d))
del d[3]
print(len(d))
s = set()
for i in range(10):
    s.add(i % 4)
print(len(s))
d = {1: "one", 2: "two", "s": "str", 2 ** 60: "big", 2 ** 70: "huge", -5: "neg", (1, 2): "tup", 0: "zero"}
probe = [1, 2, 3, True, False, 1.0, 2.5, "s", "t", 2 ** 60, 2 ** 70, 2 ** 70 + 1, -5, (1, 2), (2, 1), 0, -0.0, None]
print([d.get(k) for k in probe], [d.get(k, "dflt") for k in probe])
sd = {"a": 1, "b": None}
print([sd.get(k, 9) for k in ("a", "b", "c", 1, None)], sd.get("b"), sd.get("zz"))
cnt = {}
for w in "the cat and the hat and the bat".split():
    cnt[w] = cnt.get(w, 0) + 1
ic = {}
for i in range(40):
    ic[i % 7] = ic.get(i % 7, 0) + i
print(sorted(cnt.items()), sorted(ic.items()))
try:
    d.get([1])
except TypeError as e:
    print("TypeError unhashable")


class D(dict):
    def get(self, k, default=None):
        return ("sub", k, default)


print(D(a=1).get("a"), D().get(1, 2))
