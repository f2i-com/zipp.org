# Class attribute changes after lookups and constructions were cached: a
# counter on a leaf class, patched and deleted base methods, a replaced
# __init__/__new__, metaclass attributes, and construction edge cases.
class Counter:
    count = 0
    def bump(self):
        Counter.count += 1
        return Counter.count
c = Counter()
print([c.bump() for _ in range(3)], Counter.count, c.count)
class Base:
    def m(self): return "base"
class Child(Base):
    pass
ch = Child()
print(ch.m())
Base.m = lambda self: "patched"
print(ch.m(), Child().m())
del Base.m
print(hasattr(ch, "m"), hasattr(Child, "m"))
class P:
    def __init__(self, x): self.x = x
p = P(1)
print(p.x)
def init2(self, x, y=10): self.x = x + y
P.__init__ = init2
print(P(1).x, P(1, 2).x)
class Q(P):
    pass
print(Q(5).x)
P.__init__ = lambda self, x: setattr(self, "x", -x)
print(Q(5).x, P(5).x)
del P.__init__
try:
    P(5)
except TypeError as e:
    print("TypeError", e)
print(type(P()).__name__)
class R:
    pass
print(type(R()).__name__)
def new(cls, *a):
    o = object.__new__(cls)
    o.made = True
    return o
R.__new__ = staticmethod(new)
r = R()
print(r.made)
class Meta(type):
    tag = "m1"
class WithMeta(metaclass=Meta):
    pass
print(WithMeta.tag)
Meta.tag = "m2"
print(WithMeta.tag)
class Leaf:
    v = 1
x = Leaf()
for i in range(3):
    Leaf.v = i
    print(x.v, end=" ")
print()
class L2(Leaf):
    pass
Leaf.v = 99
print(L2.v, L2().v)
class K:
    __slots__ = ("a",)
    def __init__(self): self.a = 1
print(K().a)
class E(Exception):
    def __init__(self, msg, code):
        super().__init__(msg)
        self.code = code
try:
    raise E("bad", 3)
except E as e:
    print(e, e.code, e.args)
class NoInit:
    pass
try:
    NoInit(1)
except TypeError as e:
    print("TypeError", e)
class KW:
    def __init__(self, a, *, b=2): self.s = a + b
print(KW(1).s, KW(1, b=5).s)
class Lst(list):
    def __init__(self, *a):
        super().__init__(*a)
        self.extra = 1
print(Lst([1, 2]), Lst([1]).extra)
class NoneInit:
    __init__ = None
try:
    NoneInit()
except TypeError as e:
    print("TypeError", e)
