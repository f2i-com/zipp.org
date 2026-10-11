# heapq and bisect compare their items as Python's `<` does. For tuples that
# is lazy: the first pair of items that differ decides, and nothing after it
# is looked at. So a tuple may carry an item that cannot be ordered at all,
# as long as an earlier item settles every comparison; when one does not,
# the error is Python's, and a push leaves the list as CPython leaves it.
# Every heap popped from here holds items no two of which compare equal.
import bisect
import heapq


class Weird:
    """Has no ordering; equal only to itself."""

    def __init__(self, tag):
        self.tag = tag

    def __repr__(self):
        return "Weird(%s)" % self.tag


class Ordered:
    """Orders by key, and counts the comparisons asked of it while armed."""

    asked = 0
    armed = False

    def __init__(self, key):
        self.key = key

    def __lt__(self, other):
        if Ordered.armed:
            Ordered.asked += 1
        return self.key < other.key

    def __repr__(self):
        return "Ordered(%s)" % self.key


def drain(heap):
    out = []
    while heap:
        out.append(heapq.heappop(heap))
    return out


def attempt(label, call):
    try:
        print(label, call())
    except TypeError:
        print(label, "TypeError")


print("== tuples of ints, strs, floats and bools")
heap = []
for item in [(3, "c", 1.5), (1, "b", 2.5), (2, "a", 0.5), (1, "a", 9.0), (1, "b", 1.0), (2, "a", 0.25), (0, "z", -1.0)]:
    heapq.heappush(heap, item)
    print(heap)
print(drain(heap))

heap = []
for item in [(1, True, "t"), (1, False, "f"), (0, 2, "z"), (1, 1, "a"), (True, 0, "b"), (1.0, -1, "m"), (0, 2.5, "h"), (False, 2, "a")]:
    heapq.heappush(heap, item)
    print(heap)
print(drain(heap))

print("== the first items decide: nothing after them is compared")
w = [Weird(i) for i in range(6)]
heap = []
for i in (4, 1, 5, 0, 3, 2):
    heapq.heappush(heap, (i, w[i]))
    print(heap)
print([heapq.heappop(heap) for _ in range(3)])
print(heap)
print(drain(heap))

print("== a push that ties on the first items reaches the item with no order")
heap = [(1, w[1]), (2, w[2]), (3, w[3])]
attempt("push tie at the root", lambda: heapq.heappush(heap, (1, w[0])))
print(heap)
heap = [(1, w[1]), (2, w[2]), (3, w[3])]
attempt("push tie at a leaf", lambda: heapq.heappush(heap, (2, w[0])))
print(heap)
heap = [(1, w[1]), (2, w[2]), (3, w[3]), (5, w[5])]
attempt("push below, then tie", lambda: heapq.heappush(heap, (1, w[4])))
print(heap)
heap = [(1, w[1]), (2, w[2]), (3, w[3])]
attempt("push with no tie", lambda: heapq.heappush(heap, (0, w[0])))
print(heap)

print("== the same object on both sides is equal to itself")
heap = []
for item in [(1, w[0], 3), (1, w[0], 1), (1, w[0], 2), (0, w[5], 9)]:
    heapq.heappush(heap, item)
    print(heap)
print(drain(heap))

print("== None orders against nothing, but equals itself")
heap = []
for item in [(2, None, "b"), (1, None, "c"), (2, None, "a"), (1, None, "a")]:
    heapq.heappush(heap, item)
    print(heap)
print(drain(heap))
heap = [(1, None)]
attempt("None against int", lambda: heapq.heappush(heap, (1, 0)))
print(heap)
heap = [None]
attempt("None against None", lambda: heapq.heappush(heap, None))
print(heap)
heap = [(None,), (None, 1)]
attempt("shorter tuple first", lambda: heapq.heappush(heap, (None, 0)))
print(heap)

print("== NaN decides nothing on a push")
nan = float("nan")
heap = []
for i, x in enumerate([2.0, nan, 1.0, nan, 0.5]):
    heapq.heappush(heap, (x, i))
    print([t[1] for t in heap])
heap = []
for i, x in enumerate([3, 1, 2, 1, 0]):
    heapq.heappush(heap, (x, nan, i))
    print([(t[0], t[2]) for t in heap])
heap = []
for x in [2.0, nan, 1.0, nan, 0.5, 3.0]:
    heapq.heappush(heap, x)
    print([("nan" if v != v else v) for v in heap])

print("== mixed kinds inside tuples")
heap = []
for item in [(1, 2), (1, 2.5), (1, 2 ** 70), (1, -(2 ** 70)), (1, 1.5), (True, 2 ** 200), (1, -1e300), (1, -(2 ** 200)), (1, 1e300), (1, 2 ** 70 + 1)]:
    heapq.heappush(heap, item)
print(drain(heap))
heap = [(1, "a")]
attempt("str against int", lambda: heapq.heappush(heap, (1, 1)))
print(heap)
heap = [(1, (2, 3))]
attempt("tuple against int", lambda: heapq.heappush(heap, (1, 2)))
print(heap)
heap = [(1, [2, 3])]
attempt("list against list", lambda: heapq.heappush(heap, (1, [2, 2])))
print(heap)
heap = ["b", "a" * 3, "ab"]
heapq.heapify(heap)
for s in ["a", "", "abc", "caf" + chr(0xE9), chr(0x1F600), "b" + chr(0x1F600), "z"]:
    heapq.heappush(heap, s)
print([ascii(s) for s in drain(heap)])

print("== lengths and nesting")
heap = []
for item in [(1, 2, 3), (1, 2), (1,), (), (1, 2, 3, 0), (0, 9, 9)]:
    heapq.heappush(heap, item)
    print(heap)
print(drain(heap))
heap = []
for item in [((1, 2), 0), ((1,), 5), ((1, 2), -1), ((0, 9), 9), ((), 1), ((1, 2, 0), 0)]:
    heapq.heappush(heap, item)
    print(heap)
print(drain(heap))
heap = [((1, 2), 0)]
attempt("tuple against int, nested", lambda: heapq.heappush(heap, (1, 2)))
print(heap)
heap = []
for item in [((1, ("a", 2.5)), 1), ((1, ("a", 2.0)), 2), ((1, ("a",)), 3), ((0, ("z", 9)), 4), ((1, ("a", 2.5)), 0)]:
    heapq.heappush(heap, item)
print(drain(heap))


def nest(value, levels):
    for _ in range(levels):
        value = (value, 0)
    return value


def innermost(value):
    while isinstance(value, tuple):
        value = value[0]
    return value


for levels in (3, 8, 9, 12):
    heap = []
    for v in (2, 0, 3, 1):
        heapq.heappush(heap, nest((v,), levels))
    print(levels, [innermost(t) for t in heap], [innermost(t) for t in drain(heap)])

print("== items with their own __lt__")
heap = []
for k in (5, 3, 8, 1, 9, 2):
    heapq.heappush(heap, (0, Ordered(k)))
    print(heap)
print(drain(heap))
# First items that differ: the Ordered items are never asked.
Ordered.armed = True
heap = []
for k in (5, 3, 8, 1, 9, 2):
    heapq.heappush(heap, (k, Ordered(k)))
print(drain(heap), Ordered.asked)
Ordered.armed = False

print("== bisect on tuples")
rows = [(1, "a"), (1, "b"), (2, "a"), (2, "a"), (3, None), (4, w[0])]
for probe in [(0,), (1, "a"), (1, "b"), (1, "c"), (2,), (2, "a"), (2, "a", 0), (2, "b"), (5,)]:
    print(probe, bisect.bisect_left(rows, probe), bisect.bisect_right(rows, probe))
attempt("bisect None", lambda: bisect.bisect_left(rows, (3, 1)))
attempt("bisect none tie", lambda: bisect.bisect_left(rows, (3, None)))
attempt("bisect weird", lambda: bisect.bisect_right(rows, (4, w[1])))
attempt("bisect same weird", lambda: bisect.bisect_right(rows, (4, w[0])))
table = [(1, "a"), (3, "c")]
bisect.insort(table, (2, "b"))
bisect.insort_left(table, (3, "c"))
bisect.insort_right(table, (1, "a", 0))
attempt("insort weird", lambda: bisect.insort(table, (3, w[0])))
print(table)
print(bisect.bisect((1, 2.5, 4), 2.5), bisect.bisect_left(("a", "b", "d"), "c"), bisect.bisect([(1,), (1, 0), (2,)], (1, -1)))


def merge(chunks):
    # The k-way merge of tools/python_bench's sort_heapq, small.
    heap = [(chunk[0], index, 0) for index, chunk in enumerate(chunks) if chunk]
    heapq.heapify(heap)
    out = []
    while heap:
        value, index, pos = heapq.heappop(heap)
        out.append(value)
        pos += 1
        if pos < len(chunks[index]):
            heapq.heappush(heap, (chunks[index][pos], index, pos))
    return out


chunks = [sorted((i * 7919 + k * 31) % 1000 for i in range(40)) for k in range(6)]
merged = merge(chunks)
print(merged == sorted(merged), len(merged), sum(merged), merged[:5], merged[-5:])
