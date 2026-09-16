# Truth testing of every builtin type and of user classes with __bool__/__len__,
# in if/while/not/and/or/conditional expressions and builtins that test truth.
from collections import deque, OrderedDict, Counter, defaultdict


class Empty:
    pass


class B:
    def __init__(self, v):
        self.v = v

    def __bool__(self):
        print("  __bool__", self.v)
        return self.v


class L:
    def __init__(self, n):
        self.n = n

    def __len__(self):
        print("  __len__", self.n)
        return self.n


class Both:
    def __bool__(self):
        return False

    def __len__(self):
        return 5


values = [None, True, False, 0, 1, -1, 2**64, -2**127, 0.0, -0.0, 1e-300, float("nan"), float("inf"),
          "", " ", "0", "False", b"", b"\x00", [], [0], [[]], (), (None,), {}, {0: 0}, set(), {0}, frozenset(),
          frozenset([0]), range(0), range(1), range(5, 5), range(5, 0, -1), bytearray(), bytearray(b"a"),
          deque(), deque([0]), OrderedDict(), Counter(), Counter("a"), defaultdict(int), Empty(),
          len, print, Empty, type, object(), iter([]), slice(0), 0]
for v in values:
    t = type(v).__name__
    r = []
    r.append(bool(v))
    r.append(not v)
    r.append(True if v else False)
    r.append(v and 1)
    if type(v) in (int, float, str, bytes, bool) or v is None:
        r.append(v or "dflt")
    n = 0
    while v:
        n += 1
        break
    r.append(n)
    print("truth", t, r)

for obj in [B(True), B(False), L(0), L(3), Both()]:
    print("user", type(obj).__name__, bool(obj), not obj, "yes" if obj else "no")
    if obj:
        print("  branch taken")
    x = obj and "and-right"
    y = obj or "or-right"
    print("  and/or", x if isinstance(x, str) else type(x).__name__, y if isinstance(y, str) else type(y).__name__)

print("any/all", any([B(False), B(True), B(True)]), all([L(1), L(0), L(2)]))
print("filter", [type(o).__name__ for o in filter(None, [L(0), L(2), B(False), B(True)])])


class BadBool:
    def __bool__(self):
        return 1


class RaisingBool:
    def __bool__(self):
        raise ValueError("no truth")


for bad in [BadBool(), RaisingBool()]:
    try:
        if bad:
            pass
        print("no error")
    except (TypeError, ValueError) as e:
        print(type(e).__name__, e)

count = 0
items = [0, "", None, [], 7, "x", [1], 0.0, 3.5]
for it in items:
    if it:
        count += 1
    elif it is None:
        count += 100
    elif not it:
        count += 10
print("count", count)
print("short-circuit", 0 or [] or "" or None, 1 and "a" and [2] and (3,), (0 and 1) or (2 and 0) or "end", None or 0 or False)
print("chained-not", not not 0, not not [0], not (1 and 0), (not 1) == False, [not x for x in (0, 1, "", "a")])
print("cond-expr", [("t" if x else "f") for x in values[:20]])
k = 5
while k:
    k -= 2 if k > 1 else 1
print("while-int", k)
s = "abc"
while s:
    s = s[1:]
print("while-str", repr(s))
lst = [1, 2, 3]
while lst:
    lst.pop()
print("while-list", lst)
