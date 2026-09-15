# Instance and class attributes: lookup order, shadowing, class mutation after
# instances exist, monkey-patching methods on the class and on one instance.
class Base:
    kind = "base"
    shared = []
    counter = 0

    def __init__(self, name):
        self.name = name
        Base.counter += 1

    def describe(self):
        return f"{self.name}:{self.kind}"


class Child(Base):
    kind = "child"


a = Base("a")
b = Base("b")
c = Child("c")
print("lookup", a.kind, c.kind, a.describe(), c.describe(), Base.counter, Child.counter, c.counter)

a.kind = "instance"
print("shadow", a.kind, b.kind, Base.kind, a.describe(), b.describe(), "kind" in a.__dict__, "kind" in b.__dict__)
del a.kind
print("unshadow", a.kind, a.describe(), "kind" in a.__dict__)

Base.kind = "mutated"
print("class-mutate", a.kind, b.kind, c.kind, Child.kind)
del Child.kind
print("delete-subclass-attr", c.kind, c.describe())
Child.kind = "child-again"
print("readd", c.kind, a.kind)

a.shared.append("from-a")
c.shared.append("from-c")
print("shared-mutable", b.shared, Base.shared is c.shared)
a.shared = ["own"]
print("rebind-mutable", a.shared, b.shared)

for i in range(5):
    Base.counter = Base.counter + i
    b.counter = i * 10
print("loop-mutate", Base.counter, b.counter, a.counter, c.counter)


def shout(self):
    return self.name.upper() + "!"


Base.shout = shout
print("patch-class", a.shout(), c.shout(), hasattr(b, "shout"))
original = Base.describe
Base.describe = lambda self: "patched " + original(self)
print("patch-method", a.describe(), c.describe())
Base.describe = original
print("restore-method", a.describe())

b.describe = lambda: "instance-level"
print("patch-instance", b.describe(), a.describe(), Base.describe(b))
del b.describe
print("unpatch-instance", b.describe())


class Counter:
    total = 0

    def incr(self):
        self.total += 1
        return self.total


k1, k2 = Counter(), Counter()
print("augassign-attr", k1.incr(), k1.incr(), k2.incr(), Counter.total, k1.__dict__, k2.__dict__)
Counter.total = 100
print("after-class-set", k1.total, k2.total, Counter().total, Counter().incr())


class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y


pts = [Point(i, i * i) for i in range(6)]
total = 0
for p in pts:
    p.x += 1
    total += p.x * p.y
    p.z = p.x - p.y
print("attr-loop", total, [p.z for p in pts], sorted(vars(pts[2]).items()))
setattr(pts[0], "dynamic", 5)
print("setattr", getattr(pts[0], "dynamic"), getattr(pts[1], "dynamic", "missing"), hasattr(pts[0], "x"), hasattr(pts[0], "w"))
delattr(pts[0], "dynamic")
print("delattr", hasattr(pts[0], "dynamic"))
try:
    pts[0].nope
except AttributeError as e:
    print("AttributeError", e)
try:
    del pts[0].nope
except AttributeError as e:
    print("AttributeError", e)
try:
    Point.nope
except AttributeError as e:
    print("AttributeError", e)


class Evolving:
    pass


e = Evolving()
Evolving.later = "class attr added later"
e.__dict__["direct"] = "via __dict__"
print("evolving", e.later, e.direct, sorted(e.__dict__))
Evolving.method = lambda self, x: (self.direct, x)
print("added-method", e.method(3))


class Parent:
    def who(self):
        return "parent"


class Kid(Parent):
    pass


kid = Kid()
print("inherit", kid.who())
Parent.who = lambda self: "patched-parent"
print("inherit-patched", kid.who())
Kid.who = lambda self: "kid-own"
print("override-later", kid.who(), Parent().who())
del Kid.who
print("override-removed", kid.who())
print("class-attrs", Base.__name__, Child.__bases__[0].__name__, Child.__mro__[1].__name__, "kind" in Base.__dict__, "describe" in Child.__dict__)
print("instance-class", a.__class__ is Base, c.__class__.__name__, type(c).kind, isinstance(c, Base))
