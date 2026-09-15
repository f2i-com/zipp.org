# Operators on user classes and subclasses of int/float-free types: forward, reflected and
# in-place dunders, NotImplemented fallbacks, subclass-reflected priority, unary and bitwise dunders.
class V:
    def __init__(self, x):
        self.x = x

    def __repr__(self):
        return f"V({self.x})"

    def _coerce(self, other):
        if isinstance(other, V):
            return other.x
        if isinstance(other, (int, float)):
            return other
        return None

    def __add__(self, other):
        o = self._coerce(other)
        return NotImplemented if o is None else V(self.x + o)

    def __radd__(self, other):
        o = self._coerce(other)
        return NotImplemented if o is None else V(o + self.x)

    def __sub__(self, other):
        o = self._coerce(other)
        return NotImplemented if o is None else V(self.x - o)

    def __rsub__(self, other):
        o = self._coerce(other)
        return NotImplemented if o is None else V(o - self.x)

    def __mul__(self, other):
        o = self._coerce(other)
        return NotImplemented if o is None else V(self.x * o)

    __rmul__ = __mul__

    def __truediv__(self, other):
        return V(self.x / self._coerce(other))

    def __rtruediv__(self, other):
        return V(self._coerce(other) / self.x)

    def __floordiv__(self, other):
        return V(self.x // self._coerce(other))

    def __mod__(self, other):
        return V(self.x % self._coerce(other))

    def __pow__(self, other, mod=None):
        return V(pow(self.x, self._coerce(other), mod))

    def __rpow__(self, other):
        return V(other ** self.x)

    def __matmul__(self, other):
        return f"matmul({self.x},{other})"

    def __neg__(self):
        return V(-self.x)

    def __pos__(self):
        return V(abs(self.x))

    def __invert__(self):
        return V(~self.x)

    def __abs__(self):
        return abs(self.x)

    def __and__(self, other):
        return V(self.x & self._coerce(other))

    def __or__(self, other):
        return V(self.x | self._coerce(other))

    def __xor__(self, other):
        return V(self.x ^ self._coerce(other))

    def __lshift__(self, other):
        return V(self.x << other)

    def __rshift__(self, other):
        return V(self.x >> other)

    def __rand__(self, other):
        return V(other & self.x)

    def __ror__(self, other):
        return V(other | self.x)

    def __eq__(self, other):
        o = self._coerce(other)
        return NotImplemented if o is None else self.x == o

    def __lt__(self, other):
        o = self._coerce(other)
        return NotImplemented if o is None else self.x < o

    def __le__(self, other):
        o = self._coerce(other)
        return NotImplemented if o is None else self.x <= o

    def __gt__(self, other):
        o = self._coerce(other)
        return NotImplemented if o is None else self.x > o

    __hash__ = None


a, b = V(7), V(3)
print("forward", a + b, a - b, a * b, a / b, a // b, a % b, a ** b, a @ b, pow(a, 2))
print("mixed", a + 1, 1 + a, a - 1, 10 - a, 2 * a, a * 2.5, 21 / a, 2 ** b, 1.5 + a)
print("unary", -a, +V(-4), ~a, abs(V(-9)))
print("bitwise", V(12) & 10, V(12) | 3, V(12) ^ 5, V(1) << 4, V(256) >> 3, 6 & V(3), 8 | V(1))
print("compare", a == 7, 7 == a, a < b, b < a, a <= 7, 3 < a, 8 > a, a > 2, a >= b, a != b, a != 7)
for thunk in [lambda: a + "s", lambda: "s" + a, lambda: a < "s", lambda: [] - a]:
    try:
        print("no error", thunk())
    except TypeError as e:
        print("TypeError", e)
c = V(1)
c += 5
c -= V(2)
c *= 3
c **= 2
c //= 5
print("inplace-fallback", c)


class Acc:
    def __init__(self):
        self.items = []

    def __iadd__(self, other):
        self.items.append(other)
        return self

    def __add__(self, other):
        return "plain add"


acc = Acc()
alias = acc
acc += 1
acc += 2
print("iadd", acc is alias, acc.items, acc + 3)


class Base:
    def __add__(self, other):
        return "Base.__add__"

    def __radd__(self, other):
        return "Base.__radd__"


class Derived(Base):
    def __radd__(self, other):
        return "Derived.__radd__"


print("reflected", Derived() + Base(), 1 + Derived(), Base() + Base(), "x" + Base())


class OnlyR:
    def __rsub__(self, other):
        return f"rsub({other})"

    def __rmul__(self, other):
        return f"rmul({other})"


print("only-reflected", 5 - OnlyR(), [1] * OnlyR() if False else "skip", 2.5 * OnlyR(), "s" * OnlyR())
print("sum-dunder", sum([V(1), V(2), V(3)]), sum([V(1)], V(10)))
vals = [V(i) for i in range(10)]
total = V(0)
for v in vals:
    total = total + v * 2 - 1
print("hot-loop", total, max(vals, key=lambda q: q.x % 7), sorted(vals, key=lambda q: -q.x)[:2])


class Matrix:
    def __init__(self, rows):
        self.rows = rows

    def __matmul__(self, other):
        cols = list(zip(*other.rows))
        return Matrix([[sum(x * y for x, y in zip(r, c)) for c in cols] for r in self.rows])

    def __imatmul__(self, other):
        self.rows = (self @ other).rows
        return self

    def __repr__(self):
        return f"Matrix({self.rows})"


m = Matrix([[1, 2], [3, 4]])
print("matmul", m @ m)
m @= Matrix([[0, 1], [1, 0]])
print("imatmul", m)


class Index:
    def __init__(self, i):
        self.i = i

    def __index__(self):
        return self.i


print("index-dunder", [10, 20, 30][Index(1)], "abc"[Index(-1)], bin(Index(5)), list(range(Index(3))), [1, 2, 3, 4][Index(1):Index(3)], 2 ** Index(3) if False else "skip")


class Len:
    def __len__(self):
        return 3

    def __contains__(self, item):
        return item == "yes"

    def __getitem__(self, i):
        if i >= 3:
            raise IndexError
        return i * 10


print("container-dunders", len(Len()), "yes" in Len(), "no" in Len(), "no" not in Len(), list(Len()), 20 in iter(Len()))
print("bool-int-arith", True + V(1), V(2) * True, V(5) - False)
