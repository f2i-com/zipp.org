# Loops that count or index in place (exact lists, tuples and ranges) and
# the iterables that must keep the iterator protocol: mutation during the
# loop, shadowed `range`, odd range arguments, subclasses, targets that are
# not plain names, and break / continue / else in every mode.


def show(label, value):
    print(label, value)


# ---- range --------------------------------------------------------------------
show("r1", [i for i in range(5)])
out = []
for i in range(2, 11, 3):
    out.append(i)
for i in range(10, 0, -3):
    out.append(i)
for i in range(5, 5):
    out.append("never")
for i in range(-3):
    out.append("never")
show("r2", out)
step = -2
start, stop = 7, -4
out = []
for i in range(start, stop, step):
    out.append(i)
step = 4
for i in range(0, 10, step):
    out.append(i)
show("r3", out)
show("r4", [i for i in range(True, 4)] + [i for i in range(3, False, -1)])
big = 10**20
show("r5", [i - big for i in range(big, big + 3)])
try:
    for i in range(0, 5, 0):
        pass
except ValueError as e:
    show("zero step", e)
try:
    for i in range(1.5):
        pass
except TypeError as e:
    show("float arg", type(e).__name__)


class Index:
    def __init__(self, n):
        self.n = n

    def __index__(self):
        return self.n


show("index arg", [i for i in range(Index(3))] + list(range(Index(1), Index(4))))


def shadowed():
    range = lambda n: ["shadow"] * n
    out = []
    for i in range(2):
        out.append(i)
    return out


show("shadowed", shadowed())
r = range(3, 12, 4)
show("range object", [i for i in r] + [i for i in range(9, 0, -4)])
for i in r:
    pass
show("after", i)
total = 0
for i in range(10):
    if i == 7:
        break
    if i % 2:
        continue
    total += i
else:
    total = -1
show("break", (total, i))
for i in range(3):
    pass
else:
    show("else ran", i)


class Box:
    pass


b = Box()
d = {}
for b.attr in range(3):
    pass
for d["k"] in range(4, 6):
    pass
show("targets", (b.attr, d))
for i, j in [(1, 2), (3, 4)]:
    pass
show("unpacked", (i, j))

# ---- lists and tuples -----------------------------------------------------------
items = [1, 2, 3]
seen = []
for x in items:
    seen.append(x)
    if x < 3:
        items.append(x * 10)
show("append during", (seen, items))
items = [3, 1, 2]
seen = []
for x in items:
    seen.append(x)
    if len(seen) == 1:
        items.sort()
show("sort during", seen)
items = [0, 1, 2, 3, 4, 5]
seen = []
for x in items:
    seen.append(x)
    if x == 1:
        del items[::2]
show("del slice during", (seen, items))
items = [1, 2, 3, 4]
seen = []
for x in items:
    seen.append(x)
    items.pop()
show("pop during", seen)
items = [1, 2]
seen = []
for x in items:
    seen.append(x)
    if x == 2:
        items[:] = [7, 8, 9]
show("slice assign during", seen)
seen = []
for x in (4, 5, 6):
    seen.append(x)
for x, in [(7,), (8,)]:
    seen.append(x)
for x in []:
    seen.append("never")
for x in ():
    seen.append("never")
show("tuples", seen)


class Reversed(list):
    def __iter__(self):
        for i in range(len(self) - 1, -1, -1):
            yield self[i]


class Plain(list):
    pass


show("subclasses", ([x for x in Reversed([1, 2, 3])], [x for x in Plain([4, 5])]))
seen = []
for x in [1, 2, 3, 4, 5, 6]:
    if x == 2:
        continue
    if x == 5:
        break
    seen.append(x)
else:
    seen.append("else")
show("list break", seen)
for x in [1, 2]:
    pass
else:
    show("list else", x)

# ---- other iterables and comprehensions ---------------------------------------
show("str", [c for c in "h\xe9llo"] + [c for c in "\U0001f600a"])
show("dict", [k for k in {"a": 1, "b": 2}] + [v for k, v in {"c": 3}.items()])
show("set", sorted(x for x in {3, 1, 2}))
show("gen", [x * 2 for x in (y for y in range(4))])
show("nested", [(i, j) for i in range(3) for j in range(i) if (i + j) % 2])
show("nested lists", [x for row in [[1, 2], [3], []] for x in row])
show("cond", [x for x in range(20) if x % 3 == 0 if x % 2 == 0])
show("dictcomp", {i: [j for j in range(i)] for i in range(4)})
show("setcomp", sorted({i % 3 for i in [5, 6, 7, 8]}))
lst = [1, 2, 3]
g = (x for x in lst)
lst.append(4)
show("lazy genexpr", list(g))
g = (x for x in range(3))
show("genexpr range", (next(g), list(g)))
t = (5, 6)
show("tuple comp", [x + 1 for x in t])
show("empty comps", ([x for x in []], [x for x in range(0)], [x for x in ()]))


def mutating_comp():
    data = [1, 2, 3]
    return [data.append(9) or x for x in data if len(data) < 6]


show("mutating comp", mutating_comp())


class Iterable:
    def __iter__(self):
        yield from (10, 20)


show("user iterable", [x for x in Iterable()] + list(Iterable()))
try:
    for x in 5:
        pass
except TypeError as e:
    show("not iterable", e)
try:
    [x for x in None]
except TypeError as e:
    show("comp not iterable", e)
acc = []
for i in range(3):
    for j in range(i, 3):
        for k in [i, j]:
            acc.append(i * 100 + j * 10 + k)
show("triple", acc)
show("sum range", sum(i * i for i in range(100)))
