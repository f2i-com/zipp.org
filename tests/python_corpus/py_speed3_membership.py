# `in` and `not in` over every container the native test answers and the
# ones it leaves to the runtime: strs (ASCII, non-ASCII, empty), lists and
# tuples of mixed plain values (int/float/bool equality, None, NaN), lists
# holding objects with __eq__, dicts in str mode and bucket mode (ints,
# floats, bools, None, tuples, big ints, NUL-led strs), sets and frozensets,
# subclasses, ranges, dict views, generators, user __contains__, and the
# TypeErrors (a non-str in a str, an unhashable key in a dict or set).


class Loose:
    def __eq__(self, other):
        return other == "anything" or other == 7

    __hash__ = None


class Counted:
    calls = 0

    def __eq__(self, other):
        Counted.calls += 1
        return False

    def __hash__(self):
        return 1


class Box:
    def __init__(self, items):
        self.items = items

    def __contains__(self, x):
        return x in self.items


class MyList(list):
    def __contains__(self, x):
        return x == "magic"


class MyDict(dict):
    pass


nan = float("nan")
checks = []
for needle, hay in [("b", "abc"), ("bc", "abc"), ("", "abc"), ("x", ""), ("é", "café"), ("fé", "café"), ("abcd", "abc"), ("😀", "a😀b")]:
    checks.append((needle in hay, needle not in hay))
print(checks)
seq = [1, 2.5, "s", None, True, (1, 2)]
tests = [1, 1.0, 2, 2.5, "s", "S", None, True, False, 0, (1, 2), (2, 1), nan, 10 ** 30]
print([t in seq for t in tests], [t in tuple(seq) for t in tests])
print([t in [0.0, 1] for t in (False, True, 0, 1, -0.0, 2)], nan in [1.5], 2 ** 60 in [2 ** 60], 2 ** 70 in [2 ** 70, 1])
Counted.calls = 0
print(5 in [1, Counted(), 5], 5 in [5, Counted()], Counted.calls)
print("anything" in [Loose()], 7 in [1, Loose()], 8 in [Loose()])
d = {"a": 1, "b": 2}
print(["a" in d, "c" in d, 1 in d, None in d, 1.5 in d, (1,) in d, "a" not in d])
mixed = {1: "i", 2.5: "f", "s": "str", None: "n", (1, 2): "t", True: "b", "\0nul": "z", 2 ** 60: "big", -3: "neg"}
print([k in mixed for k in (1, 1.0, True, 2.5, "s", None, (1, 2), (2, 1), "\0nul", "\0", 2 ** 60, 2 ** 60 + 1, -3, -3.0, 0, False, nan)])
s = {1, 2.0, "x", None, (3, 4), frozenset([1])}
print([k in s for k in (1, 1.0, True, 2, "x", "y", None, (3, 4), frozenset([1]), 0, nan)])
fs = frozenset(["p", 1])
print("p" in fs, 1 in fs, 1.0 in fs, "q" in fs)
print(3 in range(5), 5 in range(5), 2.0 in range(5), 4 in range(0, 10, 2), 3 in range(0, 10, 2))
print("a" in d.keys(), 1 in d.values(), ("a", 1) in d.items(), 3 in (x for x in range(5)), 9 in iter([1, 2]))
print(2 in Box([1, 2]), "magic" in MyList([1]), 1 in MyList([1]), "a" in MyDict(a=1), "b" in MyDict(a=1))
errors = []
for needle, hay in [(1, "abc"), (None, "abc"), ([], {}), ({}, {"a": 1}), ([1], {1}), ([], {"a": 1})]:
    try:
        needle in hay
        errors.append("no error")
    except TypeError as e:
        errors.append(str(e))
print(errors)
words = ["alpha", "beta", "gamma", "beta"]
seen = set()
counts = {}
dup = 0
for w in words * 50:
    if w in seen:
        dup += 1
    else:
        seen.add(w)
    if w not in counts:
        counts[w] = 0
    counts[w] += 1
print(dup, sorted(counts.items()))
grid = {(x, y): x * y for x in range(5) for y in range(5)}
print(sum(1 for x in range(7) for y in range(7) if (x, y) in grid))
nums = {i: i * i for i in range(100)}
print(sum(1 for i in range(-10, 150) if i in nums), sum(1 for i in range(-10, 150) if float(i) in nums))
