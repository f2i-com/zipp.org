# Calls through positional entries, inline method caches and inline
# subscripts: each site below runs warm (many times) and then meets the
# cases the fast paths must hand back to the general protocol.


def rep(f, *args):
    r = None
    for _ in range(4):
        r = f(*args)
    return r


# ---- positional entries and defaults -------------------------------------
def add(a, b):
    return a + b


def inc(x, step=1, *, scale=1):
    return (x + step) * scale


def pos_defaults(a, b=2, c=[]):
    c.append(a)
    return a, b, len(c)


def closure_default(a, b=10):
    def inner():
        return a + b
    return inner()


def gen_default(n, start=0):
    for i in range(start, n):
        yield i


def many(a, b, c, d, e, f, g, h, i, j, k, l, m=13, n=14):
    return a + b + c + d + e + f + g + h + i + j + k + l + m + n


def many_gen(a, b, c, d, e, f, g, h, i, j, k, l, m, n=14):
    yield a + n
    yield m


def star(*args, **kwargs):
    return args, sorted(kwargs.items())


def kwonly(a, *, b):
    return a * b


total = 0
for i in range(20):
    total += add(i, 1) + inc(i) + inc(i, 2) + inc(i, 3, scale=2) + closure_default(i) + closure_default(i, 1)
    total += sum(gen_default(i)) + sum(gen_default(i, 2))
    total += many(*range(12)) + many(*range(14))
    total += sum(many_gen(*range(13))) + sum(many_gen(*range(14)))
print("total", total)
print(pos_defaults(1), pos_defaults(2, 3), pos_defaults(4, 5, [9]))
print(star(), star(1, 2), star(1, x=2), kwonly(3, b=4))
lam = lambda x, y=5: x * y
print(rep(lam, 2), rep(lam, 2, 3))

for bad in [lambda: add(1), lambda: add(1, 2, 3), lambda: inc(), lambda: inc(1, 2, 3),
            lambda: kwonly(1), lambda: kwonly(1, 2), lambda: many(1), lambda: gen_default(),
            lambda: lam(), lambda: lam(1, 2, 3), lambda: add(1, c=2), lambda: star(**{1: 2})]:
    try:
        bad()
    except TypeError as e:
        print("TypeError:", e)

# Keyword calls with distinct names build the record inline.
def kw(a, b, c=3):
    return a - b + c


print(rep(kw, 1, 2), kw(a=5, b=1), kw(b=1, a=5), kw(1, c=10, b=2))
try:
    kw(1, a=2)
except TypeError as e:
    print("TypeError:", e)
opts = {"b": 7}
print(kw(1, **opts), kw(a=1, **opts))
try:
    kw(1, b=2, **opts)
except TypeError:
    print("TypeError")


# Recursion through the direct entries.
def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)


def depth(n):
    return 0 if n == 0 else 1 + depth(n - 1)


print(fib(18), depth(500))


# ---- method calls -----------------------------------------------------------
class Counter:
    def __init__(self):
        self.value = 0

    def bump(self, d=1):
        self.value += d
        return self.value

    def twice(self, a, b):
        return (a, b, self.value)


def call_bump(c, *args):
    return c.bump(*args) if args else c.bump()


c = Counter()
for _ in range(5):
    c.bump(2)
    c.bump()
print("bump", c.value, c.twice(1, 2))
# An instance attribute shadows the method at a warm site.
c.bump = lambda d=1: "shadowed %d" % d
print(c.bump(), c.bump(4))
del c.bump
print(c.bump(1))
# Replacing the method on the class.
Counter.bump = lambda self, d=1: "class replaced %d" % d
print(c.bump(), c.bump(9))


class Sub(Counter):
    def bump(self, d=1):
        return "sub " + str(super().bump(d))


s = Sub()
for _ in range(3):
    r = s.bump(5)
print(r)


class Tools:
    @staticmethod
    def st(x):
        return x * 2

    @classmethod
    def cm(cls, x):
        return cls.__name__ + str(x)

    @property
    def prop(self):
        return lambda y: y + 1

    def __call__(self, x):
        return x - 1


t = Tools()
for _ in range(3):
    out = (t.st(3), t.cm(4), t.prop(5), Tools.st(6), Tools.cm(7), t(8))
print(out)
m = t.cm
print(m(1), rep(m, 2))

# Builtin methods on builtin containers and str, warm, then on subclasses.
items = []
d = {}
for i in range(6):
    items.append(i)
    d.setdefault(i % 3, []).append(i)
    items.extend([i])
print(items, d, d.get(1), d.get(9), d.get(9, "dflt"), "a,b".split(","), " x ".strip(), "ab".upper())


class MyList(list):
    def append(self, x):
        super().append(x * 10)


class MyStr(str):
    pass


ml = MyList()
for i in range(3):
    ml.append(i)
ms = MyStr("Hello")
print(ml, ms.upper(), ms.lower(), ms.startswith("He"), ms.split("l"), len(ms))
for obj in [[1, 2], (1, 2), "12", {"1": 2}]:
    try:
        print(obj.index("1") if isinstance(obj, str) else obj.count(1) if not isinstance(obj, dict) else obj.keys())
    except Exception as e:
        print(type(e).__name__, e)
for bad in [None, 5, 2.5]:
    try:
        bad.append(1)
    except AttributeError as e:
        print("AttributeError:", e)
for args in [(), (1, 2)]:
    try:
        [].append(*args)
    except TypeError:
        print("TypeError", len(args))

# Module functions.
import math
print(sum(math.sqrt(x) for x in range(5)), math.floor(2.5), rep(math.gcd, 12, 18))

# ---- subscripts ----------------------------------------------------------------
a = [10, 20, 30]
tup = (1, 2, 3)
for i in range(3):
    a[i] = a[i] + tup[i]
print(a, a[-1], tup[-2], a[True], tup[False])
for bad in [lambda: a[3], lambda: a[-4], lambda: tup[5], lambda: a[1.0], lambda: a["x"], lambda: {}["k"], lambda: {1: 2}[2]]:
    try:
        bad()
    except (IndexError, TypeError, KeyError) as e:
        print(type(e).__name__, e)
try:
    a[3] = 1
except IndexError as e:
    print("IndexError", e)
a[-1] = 99
print(a)
big = 10 ** 30
try:
    a[big]
except IndexError:
    print("IndexError")
ds = {"a": 1}
for k in ["a", "b", "a"]:
    ds[k] = ds.get(k, 0) + 1
    ds[k] += 1
print(ds, ds["a"])
mixed = {1: "one", "1": "str one"}
print(mixed["1"], mixed[1], mixed[True])
mixed["x"] = 5
print(mixed)


class D(dict):
    def __getitem__(self, k):
        return "D:" + str(k)


class L(list):
    def __getitem__(self, i):
        return "L:" + str(i)


dd = D(a=1)
ll = L([1, 2])
print(dd["a"], ll[0], dd.get("a"), len(ll))


class Obj:
    pass


o = Obj()
o.x = 1
od = o.__dict__
od["y"] = 2
print(od["x"], od["y"], o.y, len(od))
print(len([1, 2]), len((1,)), len("héllo"), len({"a": 1}), len(range(5)), len(b"ab"))


class HasLen:
    def __len__(self):
        return 7


print(len(HasLen()))
len = lambda x: "shadowed"
print(len([1]))
del len
print(len([1, 2, 3]))
