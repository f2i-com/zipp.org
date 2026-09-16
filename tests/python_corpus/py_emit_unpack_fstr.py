# Unpacking without an iterator (literal right-hand sides, exact tuples and
# lists read in place) and f-string values formatted inline; every other
# shape keeps the iterator protocol and the runtime's formatting.

a, b, c = 1, 2, 3
a, b = b, a
print("swap", a, b)
a, b, c = c, a, b
print("rotate", a, b, c)
(a, b), c = (c, a), b
print("nested", a, b, c)
[a, b] = [b, a]
print("list literal", a, b)
lst = [10, 20, 30]
lst[0], lst[2] = lst[2], lst[0]
print("subscripts", lst)
lst = [1, 2]
lst[1], lst[0] = lst
print("targets inside the source", lst)
lst = [1, 2]
i, lst[i] = 1, 9
print("left to right", i, lst)


class P:
    pass


p = P()
p.x, p.y = 3, 4
p.x, p.y = p.y, p.x
print("attributes", p.x, p.y)


def locals_swap(m, n):
    m, n = n, m
    k, (m, n) = m + n, (n, m)
    return m, n, k


print("locals", locals_swap(1, 2))
x = y = 0
x, y = y + 1, x + 2
print("uses old values", x, y)
first, *rest = 1, 2, 3
*init, last = [4, 5, 6]
print("starred", first, rest, init, last)
t = (7, 8)
u, v = t
w, z = [9, 10]
print("tuple and list", u, v, w, z)
s1, s2 = "ab"
d1, d2 = {"k": 1, "j": 2}
r1, r2, r3 = range(3)
g1, g2 = (n * n for n in (3, 4))
print("iterables", s1, s2, d1, d2, r1, r2, r3, g1, g2)
st = sorted([e for e in {5, 6}])
q1, q2 = st
print("from a list", q1, q2)


class Pair:
    def __iter__(self):
        yield "left"
        yield "right"


o1, o2 = Pair()
print("user iterable", o1, o2)
for bad in [(1, 2, 3), [1], "abc", 5, None, (1,)]:
    try:
        e1, e2 = bad
        print("unpacked", e1, e2)
    except (ValueError, TypeError) as e:
        print(type(e).__name__)
pairs = [(1, "a"), (2, "b"), [3, "c"]]
seen = []
for num, ch in pairs:
    seen.append(ch * num)
for k, v in {"x": 1, "y": 2}.items():
    seen.append(k + str(v))
for idx, (m, n) in enumerate([(1, 2), (3, 4)]):
    seen.append(idx + m + n)
print("loop targets", seen)
print("comprehension targets", [m * n for m, n in [(1, 2), (3, 4)]], {k: v for v, k in [(1, "a")]})

# ---- f-strings ---------------------------------------------------------------
n = 42
neg = -7
big = 2**100
name = "zipp"
pi = 3.14159
flag = True
nothing = None
emoji = "\U0001f600"


class Fmt:
    def __format__(self, spec):
        return "<" + spec + ">"

    def __str__(self):
        return "Fmt-str"

    def __repr__(self):
        return "Fmt-repr"


print(f"{n}", f"{neg}", f"{big}", f"{name}", f"{pi}", f"{flag}", f"{nothing}")
print(f"n={n} neg={neg} name={name}!", f"{n}{name}{n}", f"", f"plain")
print(f"{Fmt()}", f"{Fmt():spec}", f"{Fmt()!s}", f"{Fmt()!r}", f"{name!r}", f"{n!r}")
print(f"{n:5}|{n:<5}|{n:x}|{pi:.2f}|{name:>8}|{big:,}", f"{2.0}", f"{1e20}", f"{-0.0}")
print(f"{[1, 'a']}", f"{(1,)}", f"{ {'k': n} }", f"{[name]!s}")
print(f"{n + 1}", f"{'nested ' + f'{n}'}", f"{n if flag else name}", f"{emoji}")
width = 6
print(f"{name:{width}}|", f"{pi:{width}.{2}f}|", f"{n=}", f"{name = }")
parts = [f"item{i}" for i in range(4)]
print(parts, "".join(f"{c}{i}" for i, c in enumerate("abc")))
