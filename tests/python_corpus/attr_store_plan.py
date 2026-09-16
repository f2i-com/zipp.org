# Attribute reads and stores under the per-class caches: class attributes
# of every kind, data and non-data descriptors, __setattr__ added and removed
# after instances exist, __slots__, properties through their direct entry,
# methods rebound on the class after calls, and builtin methods called with
# the wrong number of arguments.


class Plain:
    count = 0
    nothing = None
    label = "cls"

    def method(self):
        return "method"


p = Plain()
p.x = 1
p.count = 10          # shadows a primitive class attribute
p.nothing = "set"     # shadows a None class attribute
p.method = lambda: "instance"  # shadows a plain function
print(p.x, p.count, Plain.count, p.nothing, Plain.nothing, p.method(), Plain().method())
print(sorted(vars(p)))


class Desc:
    def __get__(self, obj, owner):
        return "desc-get" if obj is not None else "desc-class"

    def __set__(self, obj, value):
        obj.__dict__["_d"] = value * 2


class NonData:
    def __get__(self, obj, owner):
        return "nondata"


class WithDescs:
    d = Desc()
    n = NonData()


w = WithDescs()
w.d = 21
w.n = "shadowed"
print(w.d, w._d, w.n, WithDescs.d, WithDescs.n)


class Prop:
    def __init__(self):
        self._v = 1

    @property
    def v(self):
        return self._v

    @v.setter
    def v(self, value):
        self._v = value + 100

    @property
    def ro(self):
        return "ro"


q = Prop()
q.v = 5
print(q.v, q._v)
try:
    q.ro = 1
except AttributeError as e:
    print("AttributeError:", e)


# Property getters of every callable shape.
def getter_with_default(self, extra=7):
    return ("default", extra)


class Kinds:
    a = property(getter_with_default)
    b = property(lambda self, *rest: ("star", rest))
    c = property(repr)
    d = property(str)

    def __repr__(self):
        return "<Kinds>"


k = Kinds()
print(k.a, k.b, k.c, k.d)


# __setattr__ added after instances stored plainly, then removed again.
class Late:
    pass


late = Late()
late.a = 1


def strict_setattr(self, name, value):
    object.__setattr__(self, name, value * 10)


Late.__setattr__ = strict_setattr
late.a = 2
late.b = 3
print(late.a, late.b)
del Late.__setattr__
late.a = 4
print(late.a)


class Sub(Late):
    pass


sub = Sub()
sub.a = 1
Late.__setattr__ = strict_setattr
sub.a = 2
print(sub.a)
del Late.__setattr__


class Slotted:
    __slots__ = ("x", "y")


s = Slotted()
s.x = 1
try:
    s.z = 2
except AttributeError as e:
    print("AttributeError:", e)
print(s.x)


# A method rebound on the class after calls goes through the new one.
class Counter:
    def bump(self, d):
        return d + 1


c = Counter()
print(c.bump(1))
Counter.bump = lambda self, d: d + 100
print(c.bump(1))
del Counter.bump
try:
    c.bump(1)
except AttributeError as e:
    print("AttributeError:", e)


# A class attribute added after a cached miss is found at the next read.
class Grow:
    pass


g = Grow()
print(hasattr(g, "later"))
Grow.later = "there"
print(g.later)
g.later = "own"
print(g.later, Grow.later)
del g.later
print(g.later)


# Builtin methods through the direct call path: arity errors are the same.
for expr in (
    lambda: [].append(),
    lambda: [].append(1, 2),
    lambda: "a".upper(1),
    lambda: {}.get(),
    lambda: [1].pop(0, 1),
):
    try:
        expr()
    except TypeError as e:
        # The wording differs from CPython's today; the type is the contract.
        print(type(e).__name__)
print("a,b,c".split(","), "a,b,c".split(",", 1), [3, 1, 2].pop(), sorted([3, 1, 2]))

# Method call on an instance whose dict shadows the method.
class Callable:
    def __init__(self):
        self.go = lambda: "instance go"

    def go(self):
        return "class go"


print(Callable().go())


# Exceptions carry a dict too.
e = ValueError("boom")
e.extra = 1
print(e.extra, e.args)


# int/str subclasses store attributes beside their value.
class Named(int):
    pass


n = Named(5)
n.name = "five"
print(n + 1, n.name)
