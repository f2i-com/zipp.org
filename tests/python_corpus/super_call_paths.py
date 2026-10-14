# super().name(...) at warm sites: a plain function found by the cached hit
# (the receiver goes first), static and class methods found by the full
# lookup (no receiver, or the class), alternating at one site, a super() call
# inside another's arguments, and `__class__` read beside super().


class Base:
    def scale(self, x):
        return x * 2

    @staticmethod
    def st(x):
        return ("st", x)

    @classmethod
    def cm(cls, x):
        return (cls.__name__, x)

    def pair(self, a, b):
        return (type(self).__name__, a, b)

    def kind(self):
        return "base"


class Derived(Base):
    def scale(self, x):
        return super().scale(x) + 1

    def st_loop(self, n):
        out = []
        for i in range(n):
            out.append(super().st(i))
        return out

    def cm_loop(self, n):
        out = []
        for i in range(n):
            out.append(super().cm(i))
        return out

    def nested(self, x):
        return super().pair(super().scale(x), super().st(x))

    def nested_static_outer(self, x):
        return super().st(super().scale(x))

    def with_class(self):
        return __class__.__name__, super().kind()

    def kind(self):
        return "derived"


class Flip(Base):
    def __init__(self, which):
        self.which = which

    def call(self, x):
        return super().pick(x)


class StaticPick:
    @staticmethod
    def pick(x):
        return ("static pick", x)


class PlainPick:
    def pick(self, x):
        return ("plain pick", type(self).__name__, x)


class A(Flip, StaticPick):
    pass


class B(Flip, PlainPick):
    pass


d = Derived()
total = 0
for i in range(300):
    total += d.scale(i)
print("scale", total)
print("st", d.st_loop(4)[-2:], len(d.st_loop(200)))
print("cm", d.cm_loop(3), len(d.cm_loop(200)))
print("nested", [d.nested(i) for i in range(3)])
print("nested static outer", [d.nested_static_outer(i) for i in range(3)])
for i in range(150):
    d.nested(i)
    d.nested_static_outer(i)
print("after warm", d.nested(7), d.nested_static_outer(7))
print("with class", d.with_class())

# One code object reached through two classes: `pick` is a static method on
# one MRO and a plain function on the other, at the same site.
a, b = A(1), B(2)
seen = []
for i in range(200):
    r = (a if i % 3 else b).call(i)
    if i < 6 or i > 196:
        seen.append(r)
print("flip", seen)

