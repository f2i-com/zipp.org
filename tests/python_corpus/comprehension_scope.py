# Comprehension scoping: loop variables do not leak, class-body scope rules, closures
# capturing loop variables (late binding) and the default-argument fix, walrus in comprehensions.
x = "outer-x"
squares = [x * x for x in range(4)]
print("no-leak", x, squares)
gen = (y for y in range(3))
print("gen-no-leak", list(gen), "y" in globals())
funcs = [lambda: i for i in range(3)]
print("late-binding", [f() for f in funcs])
funcs = [lambda i=i: i for i in range(3)]
print("default-binding", [f() for f in funcs])
funcs = [(lambda j: (lambda: j))(i) for i in range(3)]
print("factory-binding", [f() for f in funcs])
adders = {}
for i in range(3):
    adders[i] = lambda v: v + i
print("loop-late-binding", [adders[k](10) for k in range(3)])


def make_multipliers():
    return [lambda v, k=k: v * k for k in range(1, 4)]


print("multipliers", [m(5) for m in make_multipliers()])


def closure_over_comprehension():
    base = 100
    return [base + n for n in range(3)], {n: base - n for n in range(2)}, list(base * n for n in range(2))


print("enclosing", closure_over_comprehension())


class Config:
    factor = 3
    names = ["a", "b"]
    upper = [n.upper() for n in names]
    pairs = [(n, i) for i, n in enumerate(names)]
    try:
        scaled = [factor * v for v in range(3)]
    except NameError as e:
        scaled = "NameError"


print("class-scope", Config.upper, Config.pairs, Config.scaled)
nested = [[(r, c) for c in range(r)] for r in range(4)]
print("nested", nested)
flat = [v for row in [[1, 2], [3], []] for v in row]
print("flatten", flat)
cond = [v for v in range(20) if v % 2 if v % 3]
print("multi-if", cond)
cross = [(a, b) for a in "xy" for b in range(a == "x", 3)]
print("dependent-inner", cross)
total = 0
values = [total := total + v for v in range(5)]
print("walrus", values, total)
if any((hit := v) > 3 for v in [1, 5, 2]):
    print("walrus-any", hit)
data = {"a": [1, 2], "b": [], "c": [3]}
print("dict-comp", {k: len(v) for k, v in data.items() if v}, {v: k for k, vs in data.items() for v in vs})
print("set-comp", sorted({c for w in ["hello", "world"] for c in w}), {n % 4 for n in range(10)})
matrix = [[1, 2, 3], [4, 5, 6]]
print("transpose", [[row[i] for row in matrix] for i in range(3)], list(map(list, zip(*matrix))))
outer_var = [10, 20]
print("outer-iterable", [v + w for v in outer_var for w in outer_var])


def shadowing(v):
    items = [v for v in range(3)]
    return v, items


print("param-shadow", shadowing("param"))


def uses_global_in_comp():
    return [len(s) for s in ["ab", "cde"]]


len_backup = len
print("global-in-comp", uses_global_in_comp())
counter = 0


def bump():
    global counter
    counter += 1
    return counter


print("side-effects", [bump() for _ in range(3)], counter, {bump(): bump() for _ in range(2)}, counter)
it = iter(range(6))
print("shared-iterator", [(a, next(it)) for a in it])
words = ["apple", "bob", "kayak", "level", "zipp"]
print("palindromes", [w for w in words if w == w[::-1]], {w: w[::-1] == w for w in words})
grid = [[(r * 3 + c) % 4 for c in range(4)] for r in range(4)]
print("grid", grid, sum(sum(row) for row in grid), [max(col) for col in zip(*grid)])
print("gen-scope", list((i, j) for i in range(2) for j in range(i)), next(v for v in range(10) if v > 6))


def generator_closure():
    result = []
    for k in range(3):
        result.append(sum(k * m for m in range(3)))
    return result


print("genexp-in-loop", generator_closure())
lambdas = [lambda: [n for n in range(k)] for k in range(3)]
print("lambda-comp-late", [f() for f in lambdas])
print("nested-lambda-comp", [(lambda q: [q * t for t in range(2)])(p) for p in range(3)])
