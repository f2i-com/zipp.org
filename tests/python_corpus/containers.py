# Containers: list, tuple, dict, set, slicing, comprehensions, unpacking.
xs = [5, 3, 8, 1, 9, 2]
print(xs[0], xs[-1], xs[1:4], xs[::2], xs[::-1], xs[-3:], xs[:2], xs[10:], xs[2:2])
xs.sort()
print(xs, xs.index(8), xs.count(3), 8 in xs, 42 in xs)
xs.append(7)
xs.insert(0, 0)
xs.extend([10, 11])
print(xs, xs.pop(), xs.pop(0), xs)
xs.remove(7)
xs.reverse()
print(xs, len(xs), xs * 2, [0] * 3, [] + [1], [1, 2] == [1, 2], [1, 2] < [1, 3], [1] < [1, 0])
ys = xs[:]
ys[0] = "changed"
print(xs[0], ys[0], xs is ys, xs == xs[:])
xs[1:3] = ["a", "b", "c"]
print(xs)
del xs[0]
del xs[1:3]
print(xs)
xs[::2] = [0, 0, 0] if len(xs[::2]) == 3 else xs[::2]
print(xs, xs.copy() == xs, sorted(xs, key=str))
nested = [[i * j for j in range(3)] for i in range(3)]
print(nested, [row[1] for row in nested], sum(sum(r) for r in nested))
flat = [v for row in nested for v in row if v]
print(flat)

t = (1, 2, 3)
a, b, c = t
print(a, b, c, t[1:], t + (4,), t * 2, len(t), t.index(2), t.count(1), (1,) == (1,), () == (), tuple([1, 2]))
first, *rest = [1, 2, 3, 4]
*init, last = [1, 2, 3, 4]
head, *mid, tail = "abcde"
print(first, rest, init, last, head, mid, tail)
(p, q), r = (1, 2), 3
print(p, q, r)
a, b = b, a
print(a, b)

d = {"one": 1, "two": 2}
d["three"] = 3
print(d, len(d), d["one"], d.get("four"), d.get("four", 4), "one" in d, "four" not in d)
print(list(d.keys()), list(d.values()), list(d.items()), sorted(d))
d.update({"four": 4}, five=5)
print(d.pop("one"), d.pop("zero", None), d.setdefault("six", 6), d.setdefault("six", 60), d)
del d["two"]
print(d, d.popitem(), d)
for k, v in d.items():
    print(k, v, end="; ")
print()
counts = {}
for ch in "mississippi":
    counts[ch] = counts.get(ch, 0) + 1
print(counts, sorted(counts.items(), key=lambda kv: (-kv[1], kv[0])))
squares = {n: n * n for n in range(5)}
print(squares, {v: k for k, v in squares.items()}, dict(zip("abc", range(3))), dict([("x", 1)]), dict(a=1, b=2))
print({1: "a", 1.0: "b", True: "c"}, {(1, 2): "tuple key"}[(1, 2)], {} == {}, {"a": 1} == {"a": 1.0})
merged = {**squares, **{"extra": True}}
print(merged, {"a": 1} | {"b": 2})
try:
    d["missing"]
except KeyError as e:
    print("KeyError", e)

s = {3, 1, 2, 3}
s.add(4)
s.discard(10)
s.remove(1)
print(sorted(s), len(s), 2 in s, 1 in s)
a_set, b_set = {1, 2, 3}, {2, 3, 4}
print(sorted(a_set | b_set), sorted(a_set & b_set), sorted(a_set - b_set), sorted(a_set ^ b_set))
print(a_set <= {1, 2, 3, 4}, a_set < a_set, a_set == {3, 2, 1}, a_set.isdisjoint({9}), sorted(set("hello")))
fs = frozenset([1, 2])
print(fs == {1, 2}, hash(fs) == hash(frozenset([2, 1])), {fs: "ok"}[frozenset([1, 2])], sorted({x % 3 for x in range(10)}))
try:
    {[1]: 2}
except TypeError as e:
    print("TypeError:", e)

r = range(2, 20, 3)
print(list(r), len(r), r[1], r[-1], 8 in r, 9 in r, list(range(5, 0, -2)), list(r[1:3]), range(3) == range(0, 3))
print(list(zip(*[(1, "a"), (2, "b")])), list(map(lambda x, y: x + y, [1, 2], [10, 20])))
matrix = [[1, 2, 3], [4, 5, 6]]
print([list(col) for col in zip(*matrix)])
words = "the quick brown fox".split()
print(sorted(words, key=len), sorted(words, key=lambda w: w[-1]), " ".join(reversed(words)), max(words, key=len))
print([i for i in range(20) if i % 3 == 0 if i % 2 == 0], [(x, y) for x in range(2) for y in range(2)])
stack, queue = [], []
stack.append(1); stack.append(2); queue.insert(0, 1); queue.insert(0, 2)
print(stack.pop(), queue.pop(), stack, queue)
print(list("abc"), list(range(3)), list({"k": 1}), list((1, 2)), tuple("ab"), set([1, 1]) == {1})
print(sum([1, 2, 3]), sum([0.5, 0.25]), sum([[1], [2]], []), sum(range(101)), len([]), len(""), len({}), len(range(0)))
print([1, 2, 3][True], (1, 2)[False], "abc"[1], "abc"[-1], "hello"[1:4], "hello"[::-1], "hello"[::2])
big = list(range(10))
print(big[2:8:2], big[8:2:-2], big[-1:-4:-1], big[5:], big[:-5])
print(list(reversed(range(4))), sorted({3: 1, 1: 2}), sorted("bca"), sorted([(2, "b"), (1, "z"), (1, "a")]))
