# Class attribute changes reach every subclass's cached lookups (per-class
# invalidation): a counter on a base with subclasses, diamonds, shadowing and
# unshadowing at a middle class, late __init__/__new__ on a base, misses that
# later resolve, metaclass setattr paths, and classes made in a loop.
class Shape:
    count = 0
    def __init__(self):
        Shape.count += 1
    def area(self):
        return 0
class Circle(Shape):
    def area(self):
        return 3
class Square(Shape):
    pass
shapes = [Circle() if i % 2 else Square() for i in range(6)]
print(Shape.count, Circle.count, Square.count, [s.area() for s in shapes])
for i in range(3):
    Shape.count = 10 * i
    print(Circle().count, Square.count, shapes[0].count, end=" | ")
print()

class Top:
    def who(self):
        return "top"
class Left(Top):
    pass
class Right(Top):
    pass
class Bottom(Left, Right):
    pass
b = Bottom()
print(b.who(), Left().who(), Right().who())
Top.who = lambda self: "top2"
print(b.who(), Left().who(), Right().who())
Right.who = lambda self: "right"
print(b.who(), Left().who(), Right().who(), Bottom.__mro__[2].__name__)
del Right.who
print(b.who(), Right().who())
Left.who = lambda self: "left"
print(b.who(), Right().who())

class A:
    tag = "a"
class B(A):
    pass
class C(B):
    pass
class D(A):
    pass
c, d = C(), D()
print(c.tag, d.tag)
B.tag = "b"
print(c.tag, d.tag, A.tag)
del B.tag
print(c.tag, d.tag)
print(hasattr(c, "late"), hasattr(d, "late"), getattr(c, "late", "none"))
A.late = "now"
print(hasattr(c, "late"), d.late, C.late)
del A.late
print(hasattr(c, "late"), getattr(d, "late", "gone"))

class P:
    def __init__(self, x):
        self.x = x
class Q(P):
    pass
class R2(Q):
    pass
print(R2(1).x, Q(2).x)
P.__init__ = lambda self, x: setattr(self, "x", x * 100)
print(R2(1).x, Q(2).x, P(3).x)
Q.__init__ = lambda self, x: setattr(self, "x", -x)
print(R2(1).x, Q(2).x, P(3).x)
del Q.__init__
print(R2(1).x, Q(2).x)
def made(cls, *args):
    o = object.__new__(cls)
    o.via_new = cls.__name__
    return o
P.__new__ = staticmethod(made)
r = R2(4)
print(r.x, r.via_new, Q(5).via_new)

class Meta(type):
    def __setattr__(cls, name, value):
        super().__setattr__(name, value + 1 if isinstance(value, int) else value)
class M(metaclass=Meta):
    v = 1
class MSub(M):
    pass
ms = MSub()
print(ms.v)
Meta.__setattr__(M, "v", 5)
print(ms.v, M.v)
type.__setattr__(M, "v", 7)
print(ms.v, MSub.v)
type.__delattr__(M, "v")
print(hasattr(ms, "v"), hasattr(MSub, "v"))

class Root:
    value = 0
made_classes = [type("K%d" % i, (Root,), {}) for i in range(40)]
objs = [k() for k in made_classes]
print(sum(o.value for o in objs))
Root.value = 2
print(sum(o.value for o in objs))
made_classes = None
objs = None
kept = type("Kept", (Root,), {})()
Root.value = 3
print(kept.value, Root.value)
