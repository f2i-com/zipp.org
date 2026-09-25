# Keys with a __hash__ / __eq__ of their own: equal to native keys through
# either side, colliding, mutating the dict from __eq__, raising, and the
# number of __hash__ / __eq__ calls a lookup makes.
calls = {"hash": 0, "eq": 0}


class K:
    def __init__(self, v, h=None):
        self.v = v
        self.h = v if h is None else h

    def __hash__(self):
        calls["hash"] += 1
        return hash(self.h)

    def __eq__(self, other):
        calls["eq"] += 1
        if isinstance(other, K):
            return self.v == other.v
        return self.v == other

    def __repr__(self):
        return "K(%r)" % (self.v,)


def counted(label, f):
    calls["hash"] = calls["eq"] = 0
    r = f()
    print(label, r, calls["hash"], calls["eq"])


d = {K(1): "k1", K(2): "k2"}
counted("get-same", lambda: d[K(1)])
counted("get-miss", lambda: d.get(K(3)))
counted("contains", lambda: K(2) in d)
counted("int-finds-user", lambda: d.get(1))
counted("set-existing", lambda: d.__setitem__(K(1), "k1b"))
print("after", d)
d[1] = "int-one"
print("int-merges", d, len(d))
d[K(9, h=1)] = "collides"
counted("collision-get", lambda: d[K(9, h=1)])
print("collision", len(d), list(d.values()))

n = {5: "five", "s": "str", (1, 2): "tuple"}
counted("user-finds-int", lambda: n[K(5)])
counted("user-finds-tuple", lambda: n.get(K((1, 2))))
del n[K(5)]
print("user-deletes-int", n)

# A __hash__ the dict cannot use.
class NoHash:
    __hash__ = None
try:
    {NoHash(): 1}
except TypeError as e:
    print("nohash", e)
class EqOnly:
    def __eq__(self, other):
        return True
try:
    {EqOnly(): 1}
except TypeError as e:
    print("eqonly", e)

# Raising from __hash__ and from __eq__ leaves the dict unchanged.
class Bad:
    def __init__(self, where):
        self.where = where
    def __hash__(self):
        if self.where == "hash":
            raise ValueError("bad hash")
        return 7
    def __eq__(self, other):
        if self.where == "eq":
            raise ValueError("bad eq")
        return self is other
b = {Bad("ok"): 1}
for where in ("hash", "eq"):
    try:
        b[Bad(where)] = 2
    except ValueError as e:
        print("raises", where, e, len(b))
    try:
        print(Bad(where) in b)
    except ValueError as e:
        print("raises-in", where, e)

# An __eq__ that changes the dict while it is being looked in.
class Mutator:
    def __init__(self, target, v):
        self.target = target
        self.v = v
    def __hash__(self):
        return 42
    def __eq__(self, other):
        if isinstance(other, Mutator):
            if self.target.pop("junk", None) is not None:
                self.target["junk2"] = 1
            return self.v == other.v
        return NotImplemented
m = {}
m["junk"] = 0
m[Mutator(m, 1)] = "a"
m[Mutator(m, 2)] = "b"
print("mutator-get", m.get(Mutator(m, 2)), m.get(Mutator(m, 3)), sorted(k for k in m if isinstance(k, str)), len(m))

# Sets of such keys.
s = {K(1), K(2), 3}
print("set", len(s), K(3) in s, 1 in s, 2.0 in s, K(4) in s)
s.add(K(3))
s.discard(K(1))
print("set-after", len(s), sorted(x.v if isinstance(x, K) else x for x in s))
s2 = {K(2), 5}
print("set-ops", len(s | s2), len(s & s2), len(s - s2), len(s ^ s2), s2 <= s | s2)


class Point:
    __slots__ = ("x", "y")
    def __init__(self, x, y):
        self.x, self.y = x, y
    def __eq__(self, o):
        return isinstance(o, Point) and (self.x, self.y) == (o.x, o.y)
    def __hash__(self):
        return hash((self.x, self.y))
    def __repr__(self):
        return "P(%d,%d)" % (self.x, self.y)
grid = {Point(x, y): x * y for x in range(5) for y in range(5)}
print("points", len(grid), grid[Point(3, 4)], Point(9, 9) in grid, sum(grid[Point(i, i)] for i in range(5)))
seen = set()
for i in range(20):
    seen.add(Point(i % 3, i % 4))
print("point-set", len(seen), Point(2, 3) in seen)

# Identity-hashed objects.
class Node:
    pass
nodes = [Node() for _ in range(50)]
visited = {}
for i, nd in enumerate(nodes):
    visited[nd] = i
print("identity", len(visited), visited[nodes[17]], nodes[3] in visited, Node() in visited)
del visited[nodes[0]]
print("identity-del", len(visited), nodes[0] in visited)
