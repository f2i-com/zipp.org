# Unpacking: tuple/list targets, swaps and rotations, starred targets, nested targets,
# unpacking from arbitrary iterables, attribute/subscript targets, and error messages.
a, b = 1, 2
a, b = b, a
print("swap", a, b)
a, b, c = 1, 2, 3
a, b, c = c, a, b
print("rotate", a, b, c)
x = [10, 20, 30]
x[0], x[2] = x[2], x[0]
print("swap-subscript", x)
i = 0
i, x[i] = 2, "set"
print("target-left-to-right", i, x)


class P:
    pass


p = P()
p.u, p.v = "u", "v"
p.u, p.v = p.v, p.u
print("swap-attr", p.u, p.v)
(a, b), (c, d) = (1, 2), [3, 4]
print("nested", a, b, c, d)
[a, [b, (c, d)]] = [5, (6, [7, 8])]
print("nested-list-target", a, b, c, d)
first, *rest = range(5)
*init, last = "hello"
head, *middle, tail = [1, 2]
lone, *empty = (9,)
print("starred", first, rest, init, last, head, middle, tail, lone, empty, type(rest).__name__)
a, *b, c, d = "abcdef"
print("starred-mid", a, b, c, d)
(a, *b), c = "xyz", 1
print("starred-nested", a, b, c)
for k, *vs in [(1, 2, 3), (4,), (5, 6)]:
    print("starred-for", k, vs)
a, b = "hi"
c, d = {"k1": 1, "k2": 2}
e, f = {5, 6} if False else (5, 6)
g, h = range(2)
print("iterables", a, b, c, d, e, f, g, h)


def gen():
    yield "g1"
    yield "g2"


a, b = gen()
print("generator", a, b)
m = n = o = [0]
m += [1]
print("chained-assign", m, n, o, m is o)
a = b = 5
b += 1
print("chained-int", a, b)
t = 1, 2, 3
u = 1,
print("implicit-tuple", t, u, type(u).__name__)
d = {}
d["a"], d["b"] = divmod(17, 5)
print("dict-targets", d)
values = [(1, "one"), (2, "two")]
(n1, s1), (n2, s2) = values
print("pairs", n1 + n2, s1 + s2)

cases = [
    ("too-many", lambda: [(a, b) for a, b in [(1, 2, 3)]]),
    ("too-few", lambda: [(a, b, c) for a, b, c in [(1, 2)]]),
    ("starred-too-few", lambda: [a for a, b, *c in [(1,)]]),
    ("string-too-many", lambda: [a for a, b in ["abc"]]),
    ("empty", lambda: [a for (a,) in [()]]),
    ("dict-too-many", lambda: [a for a, b in [{1: 1, 2: 2, 3: 3}]]),
]
for name, thunk in cases:
    try:
        thunk()
        print(name, "no error")
    except (ValueError, TypeError) as e:
        print(name, type(e).__name__, e)


def unpack3(seq):
    x, y, z = seq
    return x + y + z


for seq in [(1, 2, 3), [4, 5, 6], "abc", range(3)]:
    print("func-unpack", unpack3(seq))
for bad in [(1, 2), [1, 2, 3, 4], "ab", "abcd"]:
    try:
        unpack3(bad)
    except (ValueError, TypeError) as e:
        print("func-unpack-error", type(e).__name__, e)
total = 0
pts = [(i, i * 2) for i in range(50)]
for px, py in pts:
    px, py = py, px
    total += px - py
print("hot-swap", total)
fib = []
a, b = 0, 1
for _ in range(30):
    fib.append(a)
    a, b = b, a + b
print("fib-unpack", fib[-3:], a)
matrix = [[1, 2, 3], [4, 5, 6], [7, 8, 9]]
(r0c0, _, _), (_, r1c1, _), (_, _, r2c2) = matrix
print("diag", r0c0, r1c1, r2c2, _)
print("star-expr", [*range(3), *"ab"], (*[1, 2],), {*"aa", *"b"} == {"a", "b"}, {**{"x": 1}, "y": 2}, [*[], *()])
print("print-star", *[1, 2, 3], sep="|")
