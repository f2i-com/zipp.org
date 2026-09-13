# Functions: defaults, keywords, *args/**kwargs, closures, decorators, lambdas, generators.
def f(a, b=2, *args, c=3, **kwargs):
    return (a, b, args, c, sorted(kwargs.items()))


print(f(1))
print(f(1, 5, 6, 7, c=8, d=9, e=10))
print(f(*[1, 2, 3], **{"c": 4, "z": 5}))
print(f(b=20, a=10))


def g(x, /, y, *, z):
    return x + y + z


print(g(1, 2, z=3), g(1, y=2, z=3))


def counter():
    count = 0

    def inc(by=1):
        nonlocal count
        count += by
        return count

    def get():
        return count

    return inc, get


inc, get = counter()
inc()
inc(5)
print(get(), inc(), get())


def make_adders():
    return [lambda x, i=i: x + i for i in range(3)]


print([add(10) for add in make_adders()])


def late_binding():
    fs = []
    for i in range(3):
        fs.append(lambda: i)
    return [fn() for fn in fs]


print(late_binding())

total = 0


def bump():
    global total
    total += 1
    return total


bump()
bump()
print(total)


def trace(fn):
    def wrapper(*args, **kwargs):
        result = fn(*args, **kwargs)
        print(f"{fn.__name__}{args} -> {result}")
        return result
    wrapper.__name__ = fn.__name__
    return wrapper


def repeat(n):
    def deco(fn):
        def wrapper(*a):
            return [fn(*a) for _ in range(n)]
        return wrapper
    return deco


@trace
def square(x):
    return x * x


@repeat(3)
def hello(name):
    return "hi " + name


print(square(4), hello("bob"))
print(square.__name__)


def fib():
    a, b = 0, 1
    while True:
        yield a
        a, b = b, a + b


gen = fib()
print([next(gen) for _ in range(10)])


def countdown(n):
    while n > 0:
        yield n
        n -= 1
    return "liftoff"


print(list(countdown(3)), sum(countdown(4)))


def chain(*its):
    for it in its:
        yield from it


print(list(chain([1, 2], "ab", range(2))))


def echo():
    received = []
    while True:
        v = yield len(received)
        if v is None:
            break
        received.append(v)
    return received


e = echo()
print(next(e), e.send("a"), e.send("b"))
try:
    e.send(None)
except StopIteration as stop:
    print("returned", stop.value)

squares = (x * x for x in range(5))
print(next(squares), list(squares), list(squares))
print(sum(x for x in range(10) if x % 2), max(len(w) for w in ["a", "abc", "ab"]))


def apply(fn, *values, **opts):
    return [fn(v, **opts) for v in values]


print(apply(lambda v, base=10: v * base, 1, 2, 3, base=5))
print((lambda: 42)(), (lambda *a, **k: (a, sorted(k)))(1, 2, x=3))


def outer():
    x = "outer"

    def middle():
        def inner():
            return x
        return inner
    return middle()()


print(outer())


def recursive(n):
    return 1 if n <= 1 else n * recursive(n - 1)


print(recursive(10), recursive(25))


def default_mutable(item, acc=[]):
    acc.append(item)
    return acc


print(default_mutable(1), default_mutable(2))
print(f.__name__, f.__defaults__, (lambda: 0).__name__, square.__name__)


def kw_only(*, a, b=2):
    return a, b


print(kw_only(a=1), kw_only(b=5, a=4))
try:
    kw_only(1)
except TypeError as err:
    print("TypeError:", err)
try:
    f()
except TypeError as err:
    print("TypeError:", err)
try:
    f(1, 2, 3, b=4)
except TypeError as err:
    print("TypeError:", err)
try:
    g(1, 2, 3)
except TypeError as err:
    print("TypeError:", err)


def documented():
    """Docs here."""
    return None


print(documented.__doc__, documented())
print(list(map(str, range(3))), list(filter(None, [0, 1, "", "x", None, []])), list(zip("ab", [1, 2], (True, False))))
print(sorted(["bb", "a", "ccc"], key=len, reverse=True), sorted([3, 1, 2]), min([4, 2, 8]), max("abc"), min(5, 3, 9), max([], default="empty"))
print(list(enumerate("ab", 1)), list(reversed([1, 2, 3])), any([0, "", 3]), all([1, "x"]), all([]))
