# for-loop targets: enumerate, zip (including strict), nested unpacking targets,
# attribute and subscript targets, reversed/sorted/map/filter chains, iterator protocols.
names = ["ann", "bob", "cy"]
ages = [31, 25, 40]
cities = ("oslo", "rome")
for i, name in enumerate(names):
    print("enumerate", i, name)
for i, name in enumerate(names, start=10):
    print("enumerate-start", i, name)
for name, age in zip(names, ages):
    print("zip", name, age)
print("zip-short", list(zip(names, cities)), list(zip()), list(zip(names)))
try:
    list(zip(names, cities, strict=True))
except ValueError as e:
    print("ValueError", e)
try:
    list(zip(cities, names, strict=True))
except ValueError as e:
    print("ValueError", e)
for idx, (name, age) in enumerate(zip(names, ages)):
    print("enum-zip", idx, name, age)
for (i, name), age in zip(enumerate(names), ages):
    print("zip-enum", i, name, age)
pairs = [((1, 2), [3, 4]), ((5, 6), [7, 8])]
for (a, b), [c, d] in pairs:
    print("nested-target", a + b + c + d)
records = {"x": (1, (2, 3)), "y": (4, (5, 6))}
for key, (p, (q, r)) in records.items():
    print("dict-nested", key, p, q, r)


class Holder:
    pass


h = Holder()
for h.value in range(3):
    pass
print("attr-target", h.value)
slots = [None, None, None]
for i, slots[i] in enumerate("xyz"):
    pass
print("subscript-target", slots)
dd = {}
for k, dd[k] in [("a", 1), ("b", 2)]:
    pass
print("dict-subscript-target", dd)
for i in range(3):
    pass
print("loop-var-after", i)
for never in []:
    pass
try:
    never
except NameError as e:
    print("NameError", e)
for x in reversed(names):
    print("reversed", x)
for x in sorted(names, key=len, reverse=True):
    print("sorted", x)
print("map-filter", list(map(str.upper, filter(lambda s: "b" not in s, names))))
print("map-multi", list(map(lambda a, b: a * b, [1, 2, 3], [4, 5])), list(map(pow, [2, 3], [3, 2])))
squares = map(lambda v: v * v, range(5))
print("map-lazy", next(squares), next(squares), list(squares), list(squares))
en = enumerate("ab")
print("enum-iter", next(en), list(en), list(en))
z = zip([1, 2, 3], "ab")
print("zip-iter", next(z), list(z))
it = iter(range(10))
for x in it:
    if x == 3:
        break
print("resume-iter", list(it))
it = iter([1, 2, 3, 4, 5, 6])
print("pairwise-manual", list(zip(it, it)))
total = 0
for row_i, row in enumerate([[1, 2], [3, 4], [5, 6]]):
    for col_i, v in enumerate(row):
        total += row_i * 10 + col_i * v
print("nested-enum", total)
for x in "ab":
    for y in range(2):
        if y:
            continue
        print("nested-continue", x, y)
else_hits = []
for n in range(2, 12):
    for f in range(2, n):
        if n % f == 0:
            break
    else:
        else_hits.append(n)
print("for-else-primes", else_hits)


class Countdown:
    def __init__(self, n):
        self.n = n

    def __iter__(self):
        return self

    def __next__(self):
        if self.n == 0:
            raise StopIteration
        self.n -= 1
        return self.n


class Seq:
    def __getitem__(self, i):
        if i >= 4:
            raise IndexError(i)
        return chr(97 + i)


print("protocols", list(Countdown(3)), [c for c in Seq()], list(enumerate(Seq())), dict(zip(Seq(), Countdown(4))))
for i, (a, b) in enumerate(zip(Countdown(3), Seq())):
    print("proto-mix", i, a, b)
print("iter-builtins", sum(x for x, _ in zip(range(100), range(50))), max(enumerate("hello"), key=lambda t: t[1]), min(zip("ba", [2, 1])))
print("dict-iter", [k for k in {"b": 1, "a": 2}], list({"x": 1}.items()), [(k, v) for k, v in sorted({"q": 2, "p": 1}.items())])
print("range-forms", list(range(-3, 3, 2)), list(range(10, 0, -3)), list(range(0)), len(range(0, 100, 7)), range(0, 10, 2)[-1], list(range(5))[::-2])
words = "one two three two one".split()
positions = {}
for pos, w in enumerate(words):
    positions.setdefault(w, []).append(pos)
print("index-build", positions)
