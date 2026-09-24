# heapq pushes and pops (ints, floats, strs, tuples, mixed and unorderable
# items, an empty heap, a list subclass), key functions of every kind for
# sorted/min/max/map/filter, and tuple and list comparisons.
import heapq
import bisect

state = 17
def lcg():
    global state
    state = (state * 1103515245 + 12345) % 2147483648
    return state


h = []
for i in range(200):
    heapq.heappush(h, lcg() % 1000)
out = [heapq.heappop(h) for _ in range(len(h))]
print(out == sorted(out), out[:10], out[-5:])
h = []
for i in range(100):
    heapq.heappush(h, (lcg() % 10, "s%d" % (lcg() % 7), i))
res = []
while h:
    res.append(heapq.heappop(h))
print(res == sorted(res), res[:5])
h = [5.5, 1, 3.25, True, 0, -2, 1.0, 2 ** 70, -(2 ** 65)]
heapq.heapify(h)
print([heapq.heappop(h) for _ in range(len(h))])
h = ["b", "\u00e9", "a", "\U0001F600", "ab", ""]
heapq.heapify(h)
print([heapq.heappop(h) for _ in range(len(h))])
h = [(1, (2, 3)), (1, (2, 2)), (0, (9,)), (1, (2,))]
heapq.heapify(h)
print([heapq.heappop(h) for _ in range(len(h))])
try:
    heapq.heappop([])
except IndexError as e:
    print("IndexError", e)
h = [1]
try:
    heapq.heappush(h, "x")
except TypeError as e:
    print("TypeError")
print(h)
h = [(1, None)]
try:
    heapq.heappush(h, (1, None))
except TypeError as e:
    print("TypeError tuple")
print(len(h))


class Item:
    def __init__(self, k):
        self.k = k

    def __lt__(self, other):
        return self.k > other.k


h = []
for k in [3, 1, 4, 1, 5]:
    heapq.heappush(h, Item(k))
print([heapq.heappop(h).k for _ in range(5)])


class MyList(list):
    pass


ml = MyList([3, 1, 2])
heapq.heapify(ml)
heapq.heappush(ml, 0)
print(heapq.heappop(ml), list(ml), type(ml).__name__)
w = []
for v in [5, 1, 4, 1, 5, 9, 2, 6]:
    bisect.insort(w, v)
print(w, bisect.bisect_left(w, 5), bisect.bisect_right(w, 5), bisect.bisect(w, 0))
data = [lcg() % 100 for _ in range(50)]
print(sorted(data, key=lambda v: (v % 10, -v))[:8], sorted(data, key=str)[:5], sorted(data, key=abs, reverse=True)[:3])
print(min(data, key=lambda v: -v), max(data, key=lambda v: (v % 7, v)), min(["bb", "a", "ccc"], key=len), max("hello", key=ord))
print(list(map(lambda v: v * 2, data[:5])), list(map(str, data[:3])), list(map(pow, [2, 3], [3, 2])), list(filter(lambda v: v % 2, data[:10])), list(filter(None, [0, 1, "", "x"])))


def keyf(v, bonus=0):
    return -v + bonus


print(sorted([3, 1, 2], key=keyf), sorted([(1, "b"), (1, "a"), (0, "z")]), (1, 2) < (1, 3), [1, (2, 3)] < [1, (2, 4)], (1, "a") < (1, "b"), (1,) < (1, 0), [] < [0])
try:
    sorted([(1, "a"), (1, 2)])
except TypeError as e:
    print("TypeError sort")
print(sorted([(1, float("nan")), (0, 1.0)]), sorted([[2, 1], [1, 2]]), sorted([(2,), (1, 5)], key=lambda t: t[0]))
