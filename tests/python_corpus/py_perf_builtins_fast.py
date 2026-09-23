# Builtin fast paths: dict.get and list.append at warm call sites, the
# sort comparator's int/str/tuple shortcuts, str hashing, and isinstance on
# exact types. Each must agree with the general protocol it stands in for.


class Key:
    def __init__(self, v):
        self.v = v

    def __hash__(self):
        return hash(self.v)

    def __eq__(self, other):
        return self.v == (other.v if isinstance(other, Key) else other)


def lookups(d, keys, default=None):
    out = []
    for k in keys:
        out.append(d.get(k))
        out.append(d.get(k, default))
    return out


str_dict = {"a": 1, "b": 2, "\0x": 3, "": 4}
int_dict = {1: "one", 2: "two", -5: "minus five", 2 ** 60: "big", 10 ** 30: "huge"}
mixed = {1: "int one", 2.5: "float", "s": "str", (1, 2): "tuple", None: "none"}
mixed[True] = "true replaces 1"
user = {Key(7): "seven", 8: "eight"}
for _ in range(3):
    print(lookups(str_dict, ["a", "z", "\0x", "", 1], "dflt"))
    print(lookups(int_dict, [1, 3, -5, 2 ** 60, 2 ** 60 + 1, 10 ** 30, True, 2.0, "1"], 0))
    print(lookups(mixed, [1, 1.0, True, 2.5, "s", (1, 2), None, 0], "-"))
    print(lookups(user, [7, 8, Key(8), 7.0, 9]))
try:
    str_dict.get([1])
except TypeError as e:
    print("TypeError", e)
try:
    int_dict.get({})
except TypeError as e:
    print("TypeError", e)


class DD(dict):
    def get(self, k, d=None):
        return "override"


class DMissing(dict):
    def __missing__(self, k):
        return "missing"


for d in [DD(a=1), DMissing(a=1)]:
    print(d.get("a"), d.get("b"), d.get("b", 5))
print({}.get("x"), {}.get(1, 2))
obj_dict = type("O", (), {})()
obj_dict.q = 1
print(obj_dict.__dict__.get("q"), obj_dict.__dict__.get("r", "no"))

items = []
for i in range(5):
    items.append(i)
    items.append([i])
print(items)


class AL(list):
    pass


al = AL()
for i in range(3):
    al.append(i * 2)
print(al, type(al).__name__)
append = items.append
append("bound")
print(items[-1], len(items))

# Sorting: ints, strs, tuples, mixed, keys, reverse, stability.
import random

rng = random.Random(5)
ints = [rng.randrange(-50, 50) for _ in range(200)]
bigs = [rng.randrange(-2 ** 70, 2 ** 70) for _ in range(100)] + [0, 2 ** 70, -2 ** 70]
strs = ["b", "a", "", "ab", "é", "\U0001F600", "z", "￿", "Z", "a"] * 10
tuples = [(i % 7, str(i % 5), -i) for i in range(150)]
mix = [1, 2.5, True, -3, 0.0, 10 ** 20, False]
print(sorted(ints)[:10], sorted(ints, reverse=True)[:5])
print(sorted(bigs)[:3] == sorted(bigs, key=lambda x: x)[:3], sorted(bigs)[-1])
print(sorted(strs)[:12], sorted(strs, reverse=True)[:4])
print(sorted(tuples)[:5], sorted(tuples, key=lambda t: (t[1], t[0]))[:5])
print(sorted(mix), sorted(mix, reverse=True))
print(sorted([(1, "b"), (1, "a"), (0, "z"), (1, 2.0)] if False else [(1, "b"), (1, "a"), (0, "z")]))
pairs = [(i % 3, i) for i in range(30)]
print(sorted(pairs, key=lambda p: p[0])[:12])
words = "the quick brown fox jumps over the lazy dog the end".split()
counts = {}
for w in words:
    counts[w] = counts.get(w, 0) + 1
print(sorted(counts.items(), key=lambda kv: (-kv[1], kv[0])))
try:
    sorted([1, "a", 2])
except TypeError as e:
    print("TypeError", e)
try:
    sorted([(1, 2), (1, "a")])
except TypeError as e:
    print("TypeError", e)
nan = float("nan")
print(sorted([3.0, nan, 1.0])[1:] if False else len(sorted([3.0, nan, 1.0])))
print((1, 2) < (1, 3), (1, "a") < (1, "b"), [1, 2] < [1, 2, 0], (2,) < (1, 5), ("a", 1) < ("a", 1))


class Rev:
    def __init__(self, v):
        self.v = v

    def __lt__(self, other):
        return self.v > other.v

    def __repr__(self):
        return "R%d" % self.v


print(sorted([Rev(1), Rev(3), Rev(2)]), sorted([(1, Rev(1)), (1, Rev(2))]))

# Hashing strs repeatedly gives the same value; set/dict membership agrees.
s1 = "hello world"
s2 = "".join(["hello", " ", "world"])
print(hash(s1) == hash(s2), len({s1, s2}), hash("") == hash(""), s1 in {s2: 1})

# isinstance on exact types and the special cases.
for v in [1, True, 2.0, "s", None, [1], (1,), {1: 2}, b"x", bytearray(b"y"), AL()]:
    print(type(v).__name__, isinstance(v, int), isinstance(v, bool), isinstance(v, float), isinstance(v, str),
          isinstance(v, list), isinstance(v, bytes), isinstance(v, bytearray), isinstance(v, (int, str)), isinstance(v, object))
