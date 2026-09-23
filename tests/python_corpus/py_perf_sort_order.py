# Sorting, heaps and bisection over plain keys (ordered by the engine) and
# over keys the general protocol must order: results and tie order.
import heapq
import bisect

big = 2 ** 130
keys = [3, -1, 2.5, True, False, 0, -0.0, 0.0, big, -big, 2 ** 64, 1e300, -1e300, 7, 3.0, 2 ** 53 + 1, float(2 ** 53)]
print(sorted(keys))
print(sorted(keys, reverse=True))
print(sorted(range(20), key=lambda v: v % 3))
print(sorted(range(20), key=lambda v: v % 3, reverse=True))
words = ["pear", "Apple", "apple", "bé", "be", "\U0001F600", "￿", "", "a" * 3, "aa"]
print(sorted(words), sorted(words, key=len), sorted(words, key=str.lower, reverse=True))
pairs = [(w[:1], len(w), w) for w in words]
print(sorted(pairs))
nested = [((i % 3, "x" * (i % 2)), -i) for i in range(10)]
print(sorted(nested), max(nested), min(nested))
with_none = [(1, None), (0, None), (1, None)]
print(sorted(with_none, key=lambda t: t[0]))
try:
    sorted([(1, None), (1, 2)])
except TypeError as e:
    print("TypeError", e)
try:
    sorted([1, "a"])
except TypeError as e:
    print("TypeError", e)
try:
    sorted([(1, "a"), (1, 2)])
except TypeError as e:
    print("TypeError", e)
nan = float("nan")
print(len(sorted([3.0, nan, 1.0, nan, 2.0])), sorted([(1, 2), (1, 1), (0, 5)], key=lambda t: (t[0], -t[1])))
lists = [[2, 1], [1, 5], [1, 2, 3], []]
print(sorted(lists), sorted(lists, reverse=True))
mixed_len = [(1,), (1, 0), (), (0, 9, 9)]
print(sorted(mixed_len))


class Box:
    def __init__(self, v):
        self.v = v

    def __lt__(self, other):
        return self.v < other.v

    def __repr__(self):
        return "Box(%r)" % self.v


print(sorted([Box(3), Box(1), Box(2)]), sorted([(1, Box(2)), (1, Box(1)), (0, Box(9))]))

h = []
for i, w in enumerate(words * 2):
    heapq.heappush(h, (len(w), w, i))
out = [heapq.heappop(h) for _ in range(len(h))]
print(out[:6], out[-3:])
h = [(5, "e"), (1, "a"), (3, "c"), (2, "b"), (4, "d")]
heapq.heapify(h)
print(h, heapq.heappushpop(h, (0, "z")), heapq.nsmallest(2, h), heapq.nlargest(2, h))
grid = sorted((x * 7) % 11 for x in range(30))
for probe in [-1, 0, 5, 5.5, 10, 11, True]:
    print(probe, bisect.bisect_left(grid, probe), bisect.bisect_right(grid, probe))
ins = []
for v in [5, 1, 4, 1, 5, 9, 2, 6, 5, 3]:
    bisect.insort(ins, v)
print(ins)
tuples = []
for v in [(2, "b"), (1, "z"), (2, "a"), (1, "a")]:
    bisect.insort(tuples, v)
print(tuples, bisect.bisect(tuples, (2, "a")))
print(sorted({"b": 1, "a": 2}.items()), sorted(set([3, 1, 2])), sorted("hello"))
flags = [1, True, 0, False, 1.0, 0.0, -0.0, True, 1]
print(sorted(flags), [type(x).__name__ for x in sorted(flags)], sorted(flags, reverse=True))
counts = {"a": 2, "b": 2.0, "c": True, "d": 1, "e": 1.5}
print(sorted(counts.items(), key=lambda kv: (-kv[1], kv[0])))
print(sorted([(1.0, "b"), (1, "a"), (True, "c"), (0.5, "z")]))
