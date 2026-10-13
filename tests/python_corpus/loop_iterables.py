# `for` loops entered thousands of times over iterables of every kind:
# exact lists, tuples and range objects (indexed in place), subclasses with
# their own `__iter__`, dicts, strs, sets, generators, user iterables, a
# list grown while it is walked, and a non-iterable: CPython's results.
class RevList(list):
    def __iter__(self):
        return iter(self[::-1])


class Tup(tuple):
    def __iter__(self):
        yield "custom"


class Count:
    def __init__(self, n):
        self.n = n

    def __iter__(self):
        return iter(range(self.n))


def gen(n):
    yield from range(n)


iterables = [[1, 2, 3], (4, 5), range(3), range(10, 0, -3), RevList([1, 2, 3]), Tup((9, 9)),
             {"a": 1, "b": 2}, "xyz", {7}, Count(2), [], (), range(0)]
total = 0
seen = []
for i in range(3000):
    it = iterables[i % len(iterables)]
    acc = []
    for x in it:
        acc.append(x)
    total += len(acc)
    if i < len(iterables):
        seen.append(acc)
    for y in gen(i % 4):
        total += y
print(total, seen)

grow = [1]
for x in grow:
    if len(grow) < 50:
        grow.append(x + 1)
print(len(grow), grow[-1])

nested = 0
rows = [[i, i + 1, i + 2] for i in range(40)]
for k in range(200):
    for row in rows:
        for v in row:
            nested += v
print(nested)

for bad in (None, 5, 2.5):
    try:
        for z in bad:
            pass
    except TypeError as exc:
        print(type(exc).__name__, exc)
