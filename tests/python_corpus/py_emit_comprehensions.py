# Comprehensions compiled inline in the enclosing code (PEP 709): their loop
# names stay private, shared names read and write the enclosing scope, and
# the shapes that keep a separate code object (generator expressions,
# nested scopes, class bodies) behave the same.
x = "module x"
print([x for x in range(3)], x)
print({x: x + x for x in "ab"}, {x for x in (1, 1, 2)}, x)
total = 0
print([total := total + v for v in [1, 2, 3]], total)


def shadowing():
    x = "outer"
    ys = [x for x in range(2)]
    zs = [[x, y] for x in "ab" for y in range(2) if y or x == "a"]
    return x, ys, zs


print(shadowing())


def reads_enclosing(k):
    scale = 10
    return [v * scale + k for v in range(3)], {v: k for v in (k, k + 1)}


print(reads_enclosing(5))


def walrus_into_function():
    found = None
    hits = [found := v for v in range(10) if v % 4 == 3]
    return hits, found


print(walrus_into_function())


def grandparent():
    base = 100

    def parent(n):
        return [base + i for i in range(n)]

    return parent(3)


print(grandparent())


def nested_scopes():
    fs = [lambda: i for i in range(3)]
    gs = [lambda i=i: i for i in range(3)]
    rows = [[i * j for j in range(3)] for i in range(3)]
    gen = list(i * 2 for i in range(3))
    return [f() for f in fs], [g() for g in gs], rows, gen


print(nested_scopes())


class Table:
    size = 3
    cells = [c for c in range(size)]
    squares = {c: c * c for c in range(3)}


print(Table.cells, Table.squares)


def errors():
    out = []
    try:
        [1 // v for v in [1, 0]]
    except ZeroDivisionError as e:
        out.append("ZeroDivisionError")
    try:
        [v for v in 5]
    except TypeError as e:
        out.append(str(e))
    try:
        {[v]: v for v in range(2)}
    except TypeError as e:
        out.append("unhashable")
    return out


print(errors())


def generator_with_comprehension():
    for n in range(3):
        yield [n * m for m in range(n + 1)]


print(list(generator_with_comprehension()))


def repeated(n):
    t = 0
    for i in range(n):
        t += len([c for c in (i, i + 1, i + 2) if c % 2])
    return t


print(repeated(100))


def unpacking_targets():
    pairs = [(1, "a"), (2, "b")]
    return [f"{n}{s}" for n, s in pairs], {s: n for n, s in pairs}, [a + b for (a, b) in pairs[:0]]


print(unpacking_targets())


def after_loop_var():
    v = "kept"
    [v for v in range(5)]
    return v


print(after_loop_var())


def comprehension_in_default(values=[w * 2 for w in range(3)]):
    return values


print(comprehension_in_default())
items = ["b", "a", "c"]
print(sorted([s.upper() for s in items]), "".join([s for s in items if s != "a"]))
print([[y for y in range(x)] for x in range(4)])


def walrus_alias():
    x = 0
    pairs = [(x, x := x + 1) for _ in range(3)]
    y = 1
    total = y + [y := 10 for _ in range(1)][0] + y
    return pairs, x, total


print(walrus_alias())


def maybe_bound(flag):
    if flag:
        v = 5
    try:
        return [v + i for i in range(2)]
    except NameError as e:
        return type(e).__name__


print(maybe_bound(True), maybe_bound(False))


def deleted_outer():
    w = 3
    del w
    try:
        return [w for _ in range(1)]
    except NameError as e:
        return type(e).__name__


print(deleted_outer())


def outer_and_closure():
    k = 2
    f = lambda: k
    return [k * i for i in range(3)], f()


print(outer_and_closure())


def loop_rebinds():
    acc = []
    for n in range(3):
        acc.append([n + j for j in range(n)])
    return acc, n


print(loop_rebinds())
