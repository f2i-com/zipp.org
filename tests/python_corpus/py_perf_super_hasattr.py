# super().method(...) resolved without a super object, and the per-class
# cache of names nothing on the type answers (hasattr/getattr misses), each
# at warm sites and then after the class changes.


class Base:
    def scale(self, x):
        return x * 2

    def who(self):
        return "Base.who " + type(self).__name__

    @classmethod
    def make(cls, v):
        return (cls.__name__, v)

    @staticmethod
    def static(v):
        return ("static", v)

    @property
    def prop(self):
        return "Base.prop"

    value = 10


class Derived(Base):
    def scale(self, x):
        return super().scale(x) + 1

    def who(self):
        return "Derived>" + super().who()

    @classmethod
    def make(cls, v):
        return super().make(v * 10)

    def calls(self):
        return super().static(1), super().prop, super().value

    def missing(self):
        return super().nothing()


class Again(Derived):
    def scale(self, x):
        return super().scale(x) * 100


d = Derived()
a = Again()
total = 0
for i in range(5):
    total += d.scale(i) + a.scale(i)
print(total, d.who(), a.who(), Derived.make(3), a.make(4), d.calls())
try:
    d.missing()
except AttributeError as e:
    print("AttributeError", e)
Base.scale = lambda self, x: x * 3
print(d.scale(2), a.scale(2))


def replacement(self):
    return "replaced who"


Base.who = replacement
print(d.who())


class Two(Base):
    def m(self, *args):
        return super().scale(*args)

    def k(self, x):
        return super().scale(x=x) if False else super().scale(x)


print(Two().m(5), Two().k(6))


# ---- hasattr / getattr misses -------------------------------------------------
class Thing:
    def __init__(self):
        self.present = 1


def probe(o, name):
    out = []
    for _ in range(3):
        out.append(hasattr(o, name))
    return out, getattr(o, name, "default")


t = Thing()
print(probe(t, "absent"), probe(t, "present"))
t.absent = "now present"
print(probe(t, "absent"))
del t.absent
print(probe(t, "absent"))
Thing.absent = "class attr"
print(probe(t, "absent"))
del Thing.absent
print(probe(t, "absent"))


def ga(self, name):
    return "via __getattr__ " + name


Thing.__getattr__ = ga
print(probe(t, "absent"))
del Thing.__getattr__
print(probe(t, "absent"))


class Sub(Thing):
    pass


s = Sub()
print(probe(s, "zzz"))
Thing.zzz = property(lambda self: "prop zzz")
print(probe(s, "zzz"))
Sub.zzz = 5
print(probe(s, "zzz"))


class Err(Exception):
    pass


e = Err("x")
print(probe(e, "args"), probe(e, "nope"), probe(e, "__cause__"))
