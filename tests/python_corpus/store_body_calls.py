# Functions whose body only stores their parameters into attributes of the
# first one (the commonest `__init__`, and setter-like methods), called in
# straight-line code and in loops long enough to compile, while the
# instances, their layouts and the classes change under them: every
# attribute, every error and every traceback must be CPython's.
import traceback


class Vec:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def move(self, x, y):
        self.x = x
        self.y = y

    def __repr__(self):
        return "Vec(%r, %r)" % (self.x, self.y)


class Vec3(Vec):
    def __init__(self, x, y, z):
        super().__init__(x, y)
        self.z = z


class Swapped:
    def __init__(self, a, b):
        self.b = b
        self.a = a


class Same:
    def __init__(self, a):
        self.a = a
        self.a = a


class Self:
    def __init__(self, other):
        self.me = self
        self.other = other


class Defaulted:
    def __init__(self, x, y=7):
        self.x = x
        self.y = y


class Many:
    def __init__(self, a, b, c, d, e, f, g, h):
        self.a = a
        self.b = b
        self.c = c
        self.d = d
        self.e = e
        self.f = f
        self.g = g
        self.h = h


class TooMany:
    def __init__(self, a, b, c, d, e, f, g, h, i):
        self.a = a
        self.b = b
        self.c = c
        self.d = d
        self.e = e
        self.f = f
        self.g = g
        self.h = h
        self.i = i


class Logged:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def __setattr__(self, name, value):
        log.append(name)
        object.__setattr__(self, name, value)


class Guarded:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    @property
    def y(self):
        return self._y

    @y.setter
    def y(self, value):
        if value < 0:
            raise ValueError("negative y")
        self._y = value


class Slotted:
    __slots__ = ("x", "y")

    def __init__(self, x, y):
        self.x = x
        self.y = y


log = []
total = 0
keep = []
for i in range(4000):
    v = Vec(i, [i])
    w = Vec3(i, i + 1, str(i))
    s = Swapped(i, -i)
    m = Same(i)
    me = Self(v)
    d = Defaulted(i) if i % 2 else Defaulted(i, i)
    many = Many(i, 1, 2, 3, 4, 5, 6, 7)
    tm = TooMany(i, 1, 2, 3, 4, 5, 6, 7, 8)
    lg = Logged(i, i)
    sl = Slotted(i, -i)
    v.move(v.y, v.x)
    total += v.y + v.x[0] + w.x + w.y + int(w.z) + s.a + s.b + m.a + me.other.y + d.y
    total += many.a + many.h + tm.i + lg.x + sl.x + sl.y
    if i % 997 == 0:
        keep.append((v, w, s, d, many, tm))
print("total", total, len(log), log[:4])
print([sorted(vars(o)) for o in (v, w, s, m, d, many, tm)])
print(list(vars(s)), list(vars(w)), list(vars(Self(1)))[:2])
print(keep[1][0], keep[2][1].z, keep[3][2].a, keep[-1][4].e)
print(me.me is me, me.other is v)

# A store body as a method: the same object restored again and again, and
# objects whose attribute order differs (the stores replace, not append).
p = Vec(0, 0)
q = Vec(0, 0)
del q.x
q.x = 5
for i in range(3000):
    p.move(i, i * 2)
    q.move(i * 3, i)
print(p, q, list(vars(q)))

# The class changes mid-loop: a property for one of the names, then gone.
g_total = 0
for i in range(3000):
    if i == 1000:
        Vec.y = property(lambda self: self.__dict__["y"] * 100, lambda self, v: self.__dict__.__setitem__("y", v + 1))
    if i == 2000:
        del Vec.y
    v = Vec(i, i)
    g_total += v.y
print("changed", g_total)

# `__init__` replaced on the class, and called directly on a live instance.
def other_init(self, a, b):
    self.a = b
    self.b = a


h_total = 0
for i in range(3000):
    if i == 1500:
        Swapped.__init__ = other_init
    t = Swapped(i, 1)
    h_total += t.a * 2 + t.b
    p.__init__(i, -i)
print("replaced", h_total, p, list(vars(Swapped(1, 2))))

# A setter that raises from inside a store body's call, in a loop.
errors = 0
for i in range(3000):
    try:
        Guarded(i, i % 50 - 1)
    except ValueError:
        errors += 1
print("guarded", errors, Guarded(1, 2).y)
try:
    Guarded(1, -5)
except ValueError as exc:
    for f in traceback.extract_tb(exc.__traceback__):
        print(f.name, f.lineno)

# Wrong argument counts still raise as CPython raises them.
for args in [(), (1,), (1, 2, 3)]:
    try:
        Vec(*args)
    except TypeError as e:
        print(type(e).__name__, str(e).split("(")[0][:30])
try:
    for i in range(3000):
        Vec(i, i, i) if i == 2999 else Vec(i, i)
except TypeError as e:
    print("late", type(e).__name__)
