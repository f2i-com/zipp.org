# Functions that only read globals (module names and builtins), called
# from loops long enough to compile and recursively, while the globals they
# read are rebound, deleted and restored and a builtin is shadowed; beside
# functions that store globals, define nested functions, generators that
# read globals across their yields, methods and lambdas: the values and the
# errors must be CPython's.
SCALE = 3
names = ["a", "b"]


def scaled(x):
    return x * SCALE + len(names)


def fib(n):
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)


def counter():
    global SCALE
    SCALE += 1
    return SCALE


def outer(x):
    def inner(y):
        return y + SCALE
    return inner(x)


def gen(n):
    for i in range(n):
        yield i * SCALE
        yield len(names)


class K:
    factor = 2

    def m(self, x):
        return x * SCALE * self.factor + abs(-x)


lam = lambda v: v + SCALE + max(names and [1, 2])

total = 0
k = K()
for i in range(3000):
    total += scaled(i) + outer(i) + k.m(i) + lam(i)
    if i == 1000:
        SCALE = 5
    if i == 2000:
        names = names + ["c"]
print(total, fib(20), SCALE)

print(sum(gen(10)))
g = gen(4)
first = [next(g), next(g)]
SCALE = 100
print(first, list(g))
SCALE = 3

# A global deleted, then read: NameError from the function that reads it.
def reads_missing():
    return MISSING + 1


MISSING = 1
caught = 0
for i in range(2000):
    if i == 1500:
        del MISSING
    try:
        reads_missing()
    except NameError as exc:
        caught += 1
        if caught == 1:
            print(i, exc)
print(caught)


# A builtin shadowed by a module global, then the global deleted.
def uses_len(s):
    return len(s)


out = []
for i in range(3000):
    if i == 1000:
        len = lambda s: -1
    if i == 2000:
        del len
    out.append(uses_len("abc"))
print(out[999], out[1000], out[1999], out[2000], sum(out))

# A generator with no loop reads its globals lazily, across a suspension.
def steps():
    yield SCALE
    yield len(names)
    yield SCALE * 2


g2 = steps()
first = next(g2)
SCALE = 7
rest = [next(g2), next(g2)]
print(first, rest)
SCALE = 3
acc = 0
for i in range(3000):
    if i == 1500:
        SCALE = 4
    for v in steps():
        acc += v
print(acc)
SCALE = 3

counts = [counter() for _ in range(5)]
print(counts, SCALE)
print([scaled(i) for i in range(3)], outer(1), K().m(2), lam(1))
