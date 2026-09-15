# __hash__/__eq__ protocols of user classes in dicts and sets: equal-but-distinct keys,
# __eq__ without __hash__ (unhashable), inheriting and resetting __hash__, mutable keys, NotImplemented.
class Point:
    def __init__(self, x, y):
        self.x, self.y = x, y

    def __eq__(self, other):
        return isinstance(other, Point) and (self.x, self.y) == (other.x, other.y)

    def __hash__(self):
        return hash((self.x, self.y))

    def __repr__(self):
        return f"P({self.x},{self.y})"


d = {Point(1, 2): "a"}
d[Point(1, 2)] = "b"
d[Point(2, 1)] = "c"
print("equal-keys", d, len(d), d[Point(2, 1)], Point(1, 2) in d, Point(9, 9) in d)
s = {Point(0, 0), Point(0, 0), Point(1, 1)}
print("set", len(s), Point(1, 1) in s, sorted(s, key=lambda p: (p.x, p.y)))


class EqOnly:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return isinstance(other, EqOnly) and self.v == other.v


for thunk in [lambda: {EqOnly(1): 1}, lambda: {EqOnly(1)}, lambda: hash(EqOnly(1))]:
    try:
        thunk()
        print("no error")
    except TypeError as e:
        print("TypeError", e)


class Plain:
    pass


p1, p2 = Plain(), Plain()
print("identity-hash", {p1: 1, p2: 2}[p1], len({p1, p2, p1}), p1 == p2, p1 != p2, hash(p1) == hash(p1))


class Collide:
    calls = 0

    def __init__(self, name):
        self.name = name

    def __hash__(self):
        return 42

    def __eq__(self, other):
        Collide.calls += 1
        return isinstance(other, Collide) and self.name == other.name

    def __repr__(self):
        return self.name


cd = {}
for n in ["a", "b", "c", "a", "b"]:
    cd[Collide(n)] = n
print("same-hash", sorted(cd.values()), len(cd), Collide("c") in cd, Collide("z") in cd, Collide.calls > 0)


class Child(Point):
    pass


print("inherit-hash", {Child(1, 2): "child"}[Point(1, 2)], Child(1, 2) == Point(1, 2), hash(Child(3, 4)) == hash(Point(3, 4)))


class ChildEqKeepHash(Point):
    def __eq__(self, other):
        return super().__eq__(other)

    __hash__ = Point.__hash__


print("keep-hash", {ChildEqKeepHash(5, 5): 1}[Point(5, 5)])


class Mutable:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return isinstance(other, Mutable) and self.v == other.v


m = Mutable(1)
md = {m: "stored"}
m.v = 2
print("mutated-key", Mutable(1) in md, Mutable(2) in md, m in md, len(md), list(md.values()))
m.v = 1
print("restored-key", m in md, md[Mutable(1)])


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


print("cross-type-eq", NumLike(1) == 1, 1 == NumLike(1), {NumLike(3): "n"}.get(NumLike(3)), NumLike(4) in {NumLike(4)})
print("notimplemented", NumLike(1) == "1", NumLike(1) != "x", [NumLike(1)] == [1], NumLike(1) in [0, 1])


class Ordered:
    def __init__(self, v):
        self.v = v

    def __lt__(self, other):
        return self.v < other.v

    def __eq__(self, other):
        return self.v == other.v

    __hash__ = None


print("unhashable-explicit", Ordered.__hash__ is None, sorted([Ordered(3), Ordered(1)])[0].v, Ordered(1) in [Ordered(1)])
try:
    hash(Ordered(1))
except TypeError as e:
    print("TypeError", e)
print("builtin-hash-eq", hash(1) == hash(1.0) == hash(True), hash("a") == hash("a"), hash((1, "a")) == hash((1.0, "a")), hash(frozenset({1, 2})) == hash(frozenset({2, 1})))
groups = {}
for i in range(30):
    key = Point(i % 3, i % 2)
    groups.setdefault(key, []).append(i)
print("grouping", sorted((k.x, k.y, len(v)) for k, v in groups.items()))
seen = set()
dupes = []
for p in [Point(i % 4, 0) for i in range(10)]:
    if p in seen:
        dupes.append(p)
    seen.add(p)
print("dedupe", len(seen), dupes)
cache = {}


def memo_key(*args):
    key = tuple(args)
    if key not in cache:
        cache[key] = sum(a if isinstance(a, int) else a.x for a in args)
    return cache[key]


print("memo", memo_key(1, 2), memo_key(Point(3, 0), 1), memo_key(Point(3, 0), 1), len(cache))
