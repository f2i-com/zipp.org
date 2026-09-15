# Rich comparisons across builtin types: str, bytes, tuple, list lexicographic order,
# mixed int/float/bool, set inclusion, chained comparisons, sort stability and errors.
pairs = [(1, 2), (2, 1), (1, 1), (1, 1.0), (True, 1), (False, 0.0), (-1, -1.5), (2**64, 2.0**64), (3, 2.9999999)]
for a, b in pairs:
    print("num", a, b, a < b, a <= b, a == b, a != b, a > b, a >= b)
strs = [("a", "b"), ("abc", "abd"), ("abc", "ab"), ("", "a"), ("Z", "a"), ("é", "z"), ("10", "9"), ("a", "a")]
for a, b in strs:
    print("str", repr(a), repr(b), a < b, a <= b, a == b, a > b, a >= b)
print("bytes", b"a" == b"a", b"abc" != b"abd", b"" == b"", b"x" == bytearray(b"x"), b"a" == "a")
seqs = [((1, 2), (1, 3)), ((1, 2), (1, 2, 0)), ((), ()), ((2,), (1, 9)), ([1, "a"], [1, "b"]), ([1, [2, 3]], [1, [2, 4]]), ([0.5], [0]), (("a", 1), ("a", 1.0))]
for a, b in seqs:
    print("seq", a, b, a < b, a <= b, a == b, a > b)
print("list-tuple-eq", [1, 2] == (1, 2), (1, 2) != [1, 2], [] == (), list((1, 2)) == [1, 2])
for thunk in [lambda: (1, 2) < [1, 2], lambda: "a" < 1, lambda: None < 1, lambda: [1, "a"] < [1, 2], lambda: {1} < [1], lambda: {} < {}]:
    try:
        print("no error", thunk())
    except TypeError as e:
        print("TypeError", e)
print("sets", {1} < {1, 2}, {1, 2} <= {1, 2}, {1, 2} < {1, 2}, {1, 3} > {1}, {1} == frozenset([1]), {2} >= {1}, {1, 2} != {2, 1})
print("dicts", {"a": 1} == {"a": 1}, {"a": 1} != {"a": 2}, {1: 1} == {1.0: 1.0}, {} == {})
print("none-bool", None == None, None != 0, None == False, True == 1 == 1.0, False < True, True > 0.5, [None] == [None])
x = 5
print("chained", 1 < x < 10, 1 < x > 3, x == 5 == 5.0, 0 <= x <= 4, 1 < 2 < 3 < 4 < 5 < x, "a" < "b" < "c", 1 < x != 6, 3 > 2 > 1 == True)
print("identity-vs-eq", [1] == [1], [1] is [1], (x := [1]) is x, "x" * 2 == "xx")
records = [("bob", 25, 1.5), ("amy", 25, 2.5), ("cid", 19, 1.5), ("amy", 19, 9.0), ("bob", 25, 0.5)]
print("sort-tuples", sorted(records))
print("sort-key", sorted(records, key=lambda r: r[1]), sorted(records, key=lambda r: (-r[1], r[0])))
print("sort-stable", [r[0] for r in sorted(records, key=lambda r: r[2])], [r[2] for r in sorted(records, key=lambda r: r[0], reverse=True)])
print("sort-mixed-num", sorted([3, 1.5, True, -2, 0.0, 2**70, -0.5]), min(1, 1.0), max(1.0, True), sorted([1, True, 1.0]))
print("sort-strs", sorted(["b", "B", "a", "A", "_", "1", "é", "aa"]), sorted(["b", "B", "a"], key=str.lower), max(["x", "xy", "w"]))
print("sort-lists", sorted([[2], [1, 5], [1], [], [1, 4, 0]]), sorted([(1, "b"), (1, "a"), (0, "z")], reverse=True))
try:
    sorted(["a", 1])
except TypeError as e:
    print("sort-error", type(e).__name__)


class Rank:
    def __init__(self, n):
        self.n = n

    def __lt__(self, other):
        return self.n < other.n

    def __repr__(self):
        return f"R{self.n}"


print("user-lt-only", sorted([Rank(3), Rank(1), Rank(2)]), max([Rank(1), Rank(5)], key=lambda r: r.n), min(Rank(4), Rank(2)), Rank(1) > Rank(0))
try:
    Rank(1) <= Rank(2)
except TypeError as e:
    print("TypeError", e)
count = 0
for i in range(-50, 50):
    f = i / 4
    if f < i:
        count += 1
    if i <= f:
        count += 2
    if f == i:
        count += 4
    if -3 < i < 3 and i != 0:
        count += 8
print("hot-compare", count)
words = ["pear", "apple", "fig", "banana", "kiwi"]
best = words[0]
for w in words:
    if (len(w), w) > (len(best), best):
        best = w
print("tuple-compare-loop", best, min(words, key=lambda w: (len(w), w)))
print("in-ops", 1 in [1.0], 1.0 in {1}, "a" in "cat", (1, 2) in [(1, 2)], [1] in [[1]], None in [0, False, None], 2 in range(0, 4, 2), 2.0 in range(3))
