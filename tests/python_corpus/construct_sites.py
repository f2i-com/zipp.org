# Construction sites (`Name(args)`) meeting every kind of callee, in loops
# long enough to be compiled: plain classes with and without __init__,
# defaults, keyword-only parameters, __init__ returning a value, __init__
# replaced and deleted after warm-up, __new__, namedtuples, exceptions,
# CapWords functions, subclasses calling super().__init__, and the errors.
from collections import namedtuple


class NoInit:
    pass


class One:
    def __init__(self, x):
        self.x = x


class Defaults:
    def __init__(self, x, y=10, *, z=100):
        self.t = (x, y, z)


class Bad:
    def __init__(self, x):
        self.x = x
        if x == 3:
            return x


class Base:
    def __init__(self, a):
        self.a = a


class Child(Base):
    def __init__(self, a, b):
        super().__init__(a)
        self.b = b


class Swap(Base):
    def __init__(self, v):
        self.v = v


class WithNew:
    def __new__(cls, a):
        o = super().__new__(cls)
        o.n = a * 2
        return o

    def __init__(self, a):
        self.a = a


class Other:
    pass


class NewOther:
    def __new__(cls, a):
        return Other()

    def __init__(self, a):
        raise RuntimeError("not reached")


Pt = namedtuple("Pt", "x y")


def Make(v):
    return ("made", v)


def err(f):
    try:
        return f()
    except Exception as e:
        return (type(e).__name__, str(e))


def run():
    out = []
    for i in range(400):
        a = NoInit()
        b = One(i)
        c = Defaults(i)
        d = Defaults(i, -1)
        e = Child(i, -i)
        w = WithNew(i)
        p = Pt(i, -i)
        m = Make(i)
        x = ValueError(i, "v")
        k = KeyError(i)
        s = Swap(i)
        if i == 200:
            Swap.__init__ = lambda self, v: setattr(self, "v", -v)
        if i == 300:
            del Swap.__init__
        if i % 100 == 0 or i == 399:
            out.append((type(a).__name__, b.x, c.t, d.t, e.a, e.b, w.a, w.n, p, m, x.args, k.args,
                        getattr(s, "v", ("a", getattr(s, "a", None))), type(NewOther(i)).__name__))
    return out


for row in run():
    print(row)
print(err(lambda: NoInit(1)))
print(err(lambda: One()))
print(err(lambda: One(1, 2)))
print(err(lambda: Defaults()))
print(err(lambda: Defaults(1, z=2).t))
print([err(lambda: Bad(i).x) for i in range(5)])
print(err(lambda: Swap(1, 2)))
seen = []


class Counted:
    count = 0

    def __init__(self):
        Counted.count += 1
        seen.append(Counted.count)


for i in range(50):
    Counted()
print(Counted.count, seen[-3:])
