# Where a zero-argument super() finds its receiver: in a method whose first
# parameter a nested scope captures (a lambda or a nested function using
# `self`), it takes the cell's contents; in a list, set or dict
# comprehension (run in the method's own code since Python 3.12), the
# method's first parameter. Both as CPython does.


class Base:
    def __init__(self):
        self.x = 1

    def scale(self, n):
        return n * 2

    @staticmethod
    def st(n):
        return ("st", n)

    def name(self):
        return "Base"


class Callbacks(Base):
    def __init__(self):
        super().__init__()
        self.cb = lambda: self.x + 1

    def scale(self, n):
        helper = lambda m: self.x + m
        return helper(super().scale(n))

    def via_inner(self, n):
        def inner():
            return self.x * 100
        return inner() + super().scale(n), super().st(n)

    def plain_super(self):
        get = lambda: self
        return super().name(), type(super()).__name__, get() is self

    def reassigned(self, other):
        keep = lambda: self
        self = other
        return super().name(), keep() is other


class Comprehensions(Base):
    def scale(self, n):
        return -n

    def listed(self, n):
        return [super().scale(i) for i in range(n)]

    def as_set(self, n):
        return sorted({super().scale(i % 3) for i in range(n)})

    def as_dict(self, n):
        return {i: super().scale(i) for i in range(n)}

    def filtered(self, n):
        return [i for i in range(n) if super().scale(i) > 4]

    def nested(self, n):
        return [[super().scale(i * j) for j in range(2)] for i in range(n)]

    def with_lambda(self, n):
        return [super().scale(i) + (lambda: self.x)() for i in range(n)]

    def plain(self, n):
        return [type(super()).__name__ for _ in range(n)]

    def static(self, n):
        return [super().st(i) for i in range(n)]

    def own_scale(self, n):
        return [self.scale(i) for i in range(n)]


c = Callbacks()
print(c.cb(), c.scale(5), c.via_inner(3), c.plain_super())
total = 0
for i in range(200):
    total += c.scale(i) + c.via_inner(i)[0]
print(total)
other = Callbacks()
print(c.reassigned(other))

k = Comprehensions()
print(k.listed(4), k.as_set(9), k.as_dict(3), k.filtered(5))
print(k.nested(3), k.with_lambda(3), k.plain(2), k.static(2), k.own_scale(3))
acc = 0
for i in range(100):
    acc += sum(k.listed(5)) + len(k.filtered(i % 7))
print(acc)
