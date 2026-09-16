# Equality, ordering and hashing across types: None against a user __eq__,
# bytes ordering, sort error operands, subclass-first reflected operators,
# 3-argument pow on user types, and dict/set keys equal to ints through a
# user __hash__ and reflected __eq__.


class EqAll:
    def __eq__(self, other):
        return True

    def __ne__(self, other):
        return False


e = EqAll()
print("eq-none", [e] == [None], [None] == [e], e in [None], None in [e], (e,) == (None,), [None].count(e), [None, e].index(None))
print("eq-other", [e] == [5], 5 in [e], e in [5], [e].index(5), (1, e) == (1, "x"), e == None, None == e, None != e)

# bytes and bytearray order lexicographically by byte value.
for thunk in (lambda: b"a" < b"b", lambda: b"abc" < b"abd", lambda: b"ab" < b"abc", lambda: b"\xff" > b"\x00",
              lambda: bytearray(b"a") <= b"a", lambda: sorted([b"b", b"a", b"ab", b""]), lambda: max(b"xy", b"xz"),
              lambda: b"b" >= bytearray(b"ab"), lambda: min([bytearray(b"z"), b"y"]), lambda: b"a" < "a"):
    try:
        print("bytes-order", thunk())
    except TypeError as e2:
        print("TypeError", e2)

# The TypeError from sorting incomparable items names the operands in CPython's comparison order.
for data in ([1, "a", 2], [3, None, 1], ["x", 2.5], [(1, 2), (1, "a")], [[1], [None]]):
    try:
        sorted(data)
    except TypeError as e2:
        print("sort-error", e2)
for data in ([2, "b"], ["a", 1, 0.5]):
    try:
        sorted(data, reverse=True)
    except TypeError as e2:
        print("sort-error-rev", e2)
print("sort-stable", sorted([(1, "b"), (0, "z"), (1, "a"), (0, "y")], key=lambda p: p[0]),
      sorted([3, 1, 2], reverse=True), sorted(["b", "A", "a", "B"], key=str.lower, reverse=True))


# If the right operand's type is a subclass that overrides the reflected method, it is tried first.
class Base:
    def __add__(self, other):
        return "Base.__add__"

    def __radd__(self, other):
        return "Base.__radd__"

    def __lt__(self, other):
        return "Base.__lt__"

    def __gt__(self, other):
        return "Base.__gt__"


class Derived(Base):
    def __radd__(self, other):
        return "Derived.__radd__"

    def __gt__(self, other):
        return "Derived.__gt__"


class Plain(Base):
    pass


print("subclass-reflected-first", Base() + Derived(), Base() < Derived(), Derived() + Base(), Base() + Plain(), Base() < Plain())


class Money:
    def __init__(self, v):
        self.v = v

    def __add__(self, other):
        return Money(self.v + (other.v if isinstance(other, Money) else other))

    __radd__ = __add__

    def __repr__(self):
        return "Money(%r)" % (self.v,)


print("radd-sum", sum([Money(1), Money(2)]), 5 + Money(1), Money(1) + 2.5)


# pow(x, y, mod) dispatches to a user __pow__ with the modulus.
class V:
    def __init__(self, x):
        self.x = x

    def __pow__(self, other, mod=None):
        return ("pow", self.x, other, mod)

    def __rpow__(self, other):
        return ("rpow", other, self.x)


print("pow3", pow(V(7), 2, 5), pow(V(7), 2), V(3) ** 4, 2 ** V(3), pow(2, V(5)))
try:
    pow(V(1), 2, None)
    print("pow3-none", pow(V(1), 2, None))
except TypeError as e2:
    print("TypeError", e2)


# Dict and set lookup with a user key that equals a builtin key through a reflected __eq__.
class NumLike:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        if isinstance(other, (int, float)):
            return self.v == other
        if isinstance(other, NumLike):
            return self.v == other.v
        return NotImplemented

    def __hash__(self):
        return hash(self.v)


print("cross-type-key", {1: "int-one"}.get(NumLike(1)), NumLike(2) in {2, 3}, {NumLike(3): "n"}.get(3), 3 in {NumLike(3)}, {NumLike(4): 1} == {4: 1})
d = {NumLike(5): "user"}
d[5] = "int"
d[5.0] = "float"
print("cross-type-merge", len(d), list(d.values()), d[NumLike(5)])
s = {1, 2.0, NumLike(3)}
print("cross-type-set", 1.0 in s, NumLike(2) in s, 3 in s, len(s | {3, 3.0}), len({2**70, NumLike(2**70)}))

# hash() agrees with equality across int, float, bool and big values.
print("hash-eq", hash(1) == hash(1.0) == hash(True), hash(-1), hash(-1.0), hash(2**61 - 1), hash(2**61), hash(-(2**61)), hash(2**100))
print("hash-float", hash(0.5), hash(-0.5), hash(1.5), hash(1e300), hash(float("inf")), hash(-float("inf")), hash(0.0) == hash(-0.0), hash(2.0**70) == hash(2**70))
print("hash-tuple", hash(()), hash((1, 2)), hash((1, (2, 3))), hash((1.5, -1)), hash((True, None)) == hash((1, None)), hash((2**64,)))


class Pair(tuple):
    pass


print("hash-subclass", hash(Pair((1, 2))) == hash((1, 2)), {Pair((1, 2)): "p"}[(1, 2)], hash(NumLike(7)) == hash(7))


# Keys that share a hash bucket still iterate in insertion order.
class Tagged:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __repr__(self):
        return "Tagged(%d)" % self.v


powers = {2**i: i for i in range(64)}
print("bucket-order", list(powers.values()) == list(range(64)), list(powers)[60:], list({1: "a", "x": "b", 2**61: "c", 2.5: "d"}))
mixed = {5: "int", "s": "str", Tagged(5): "tagged", 6: "six"}
del mixed["s"]
mixed["s"] = "again"
print("bucket-order-user", list(mixed.values()), list(mixed.items())[1:3], mixed.popitem(), len(mixed))
