# `x.__class__` read natively from the instance record: instances of plain,
# derived and builtin-derived classes, primitives, None, classes and
# metaclasses, after a `__class__` assignment, and in a loop hot enough to be
# compiled.


class A:
    pass


class B(A):
    def __init__(self, v):
        self.v = v


class Size(tuple):
    pass


class Bag(list):
    pass


class Meta(type):
    pass


class WithMeta(metaclass=Meta):
    pass


class Other:
    pass


def f():
    return 1


values = [A(), B(3), Size((1, 2)), Bag([1]), 7, 2 ** 80, 1.5, "s", True, None, (1,), [2], {"k": 1}, {1}, f]
for v in values:
    print(v.__class__.__name__, v.__class__ is type(v))

print(A.__class__ is type, WithMeta.__class__ is Meta, Meta.__class__ is type)
print(B(1).__class__ is B, B(1).__class__ is A, isinstance(B(1), B(1).__class__.__mro__[1]))

o = Other()
print(o.__class__.__name__)
o.__class__ = A
print(o.__class__.__name__, o.__class__ is A)

# A loop long enough to compile, mixing receivers at one site.
counts = {}
items = [A(), B(1), Size(()), 3, "x", None, 2.5, WithMeta()]
for i in range(20000):
    c = items[i % len(items)].__class__
    counts[c.__name__] = counts.get(c.__name__, 0) + 1
print(sorted(counts.items()))
same = 0
t = Size((1, 2, 3))
for i in range(20000):
    if t.__class__ is Size:
        same += 1
print(same)
