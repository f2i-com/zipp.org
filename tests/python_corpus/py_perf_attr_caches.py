# Inline attribute caches: every read and store below runs at one site many
# times, so the site is warm (served from the per-class cache tables) when
# the class changes under it. Each change must be seen at once.


def read_x(o):
    return o.x


def store_x(o, v):
    o.x = v


def bump_x(o):
    o.x += 1
    return o.x


def warm(f, *args):
    r = None
    for _ in range(5):
        r = f(*args)
    return r


class A:
    pass


a = A()
a.x = 1
print("plain", warm(read_x, a))
warm(store_x, a, 2)
print("stored", read_x(a), a.__dict__)

# A property added to the class after the instance stored the name wins
# over the instance dict, for reads and stores.
A.x = property(lambda self: "prop", lambda self, v: print("setter", v))
print("property read", read_x(a))
store_x(a, 5)
print("dict after setter", a.__dict__)
del A.x
print("property removed", read_x(a))
warm(store_x, a, 7)
print("store again", read_x(a))


# A subclass sees a base-class change.
class B(A):
    pass


b = B()
b.x = 10
print("sub", warm(read_x, b), warm(bump_x, b))
def x(self):
    return 100


A.x = property(x)
print("sub sees base property", read_x(b))
try:
    bump_x(b)
except AttributeError as e:
    print("AttributeError", e)
del A.x
print("sub after delete", warm(bump_x, b))


# A data descriptor class (__set__) on the type.
class Desc:
    def __get__(self, obj, owner=None):
        return "desc"

    def __set__(self, obj, value):
        print("desc set", value)


class C:
    pass


c = C()
c.x = 1
warm(read_x, c)
warm(store_x, c, 3)
C.x = Desc()
print("desc read", read_x(c))
store_x(c, 4)
print("dict", c.__dict__)


# A non-data descriptor does not beat the instance dict.
class NonData:
    def __get__(self, obj, owner=None):
        return "nondata"


class D:
    x = NonData()


d = D()
print("nondata no dict", read_x(d))
d.x = 9
print("nondata with dict", warm(read_x, d))


# __setattr__ installed later takes over warm stores.
class E:
    pass


e = E()
warm(store_x, e, 1)


def logging_setattr(self, name, value):
    print("setattr", name, value)
    object.__setattr__(self, name, value * 10)


E.__setattr__ = logging_setattr
store_x(e, 2)
print("after __setattr__", read_x(e))
del E.__setattr__
warm(store_x, e, 3)
print("restored", read_x(e))


# An instance attribute shadows a method; deleting it uncovers the method.
class F:
    def x(self):
        return "method"


f = F()
print("method read", warm(read_x, f)())
f.x = "shadow"
print("shadow", warm(read_x, f))
del f.x
print("unshadowed", read_x(f)())


# Class attributes read through instances, then replaced.
class G:
    x = 1


g = G()
print("class attr", warm(read_x, g))
G.x = 2
print("class attr changed", read_x(g))
g.x = 3
print("instance over class", warm(read_x, g))
del g.x
print("back to class", read_x(g))


# Slots: stores of undeclared names fail at a warm site.
class S:
    __slots__ = ("x",)


s = S()
warm(store_x, s, 4)
print("slots", warm(read_x, s))


class T:
    __slots__ = ("y",)


t = T()
try:
    store_x(t, 1)
except AttributeError as e:
    print("slots AttributeError", e)


# __class__ reassignment moves an instance to another class's tables.
class P:
    pass


class Q:
    @property
    def x(self):
        return "Q.x"


p = P()
p.x = "own"
print("before", warm(read_x, p))
p.__class__ = Q
print("after __class__", read_x(p))


# __dict__ replacement keeps warm sites working.
class H:
    pass


h = H()
h.x = 1
warm(read_x, h)
h.__dict__ = {"x": 42}
print("new dict", read_x(h))
h.__dict__["x"] = 43
print("dict write", read_x(h))


# __getattr__ answers misses only.
class M:
    def __getattr__(self, name):
        return "getattr " + name


m = M()
print("miss", warm(read_x, m))
m.x = "present"
print("hit", warm(read_x, m))


# Modules, classes, and builtin values at a warm site.
import math


class HasX:
    x = "on class"


def read_any(o):
    try:
        return o.x
    except AttributeError as e:
        return "AttributeError: " + str(e)


class WithX:
    def __init__(self):
        self.x = "inst"


objs = [WithX(), HasX, 5, "s", None, math, WithX()]
for o in objs * 3:
    print(read_any(o))


# setattr/getattr builtins and vars() agree with the inline paths.
class V:
    pass


v = V()
for i in range(4):
    store_x(v, i)
    setattr(v, "y", i * 2)
print(getattr(v, "x"), vars(v))


# Metaclass: class-level attribute reads are not instance reads.
class Meta(type):
    @property
    def x(cls):
        return "meta property"


class WithMeta(metaclass=Meta):
    pass


print("meta", warm(read_x, WithMeta))
wm = WithMeta()
wm.x = "instance of WithMeta"
print("meta instance", warm(read_x, wm))
