# Generators: lazy evaluation order, send/throw/close, yield from with return values,
# generator expressions consumed by builtins, infinite generators with islice, pipelines.
from itertools import islice, count


def tracer(n):
    print("  start")
    for i in range(n):
        print("  yield", i)
        yield i
    print("  end")
    return "result"


g = tracer(2)
print("created, nothing ran")
print("first", next(g))
print("second", next(g))
try:
    next(g)
except StopIteration as e:
    print("stop-value", e.value)
print("rest", list(tracer(1)), list(g))


def accumulator():
    total = 0
    while True:
        value = yield total
        if value is None:
            return total
        total += value


acc = accumulator()
print("send", next(acc), acc.send(5), acc.send(10), acc.send(-3))
try:
    acc.send(None)
except StopIteration as e:
    print("send-return", e.value)


def resilient():
    while True:
        try:
            yield "ok"
        except ValueError as e:
            yield f"handled {e}"


r = resilient()
print("throw", next(r), r.throw(ValueError("bad")), next(r), next(r))
try:
    r.throw(KeyError("unhandled"))
except KeyError as e:
    print("throw-propagates", repr(e))


def closer():
    try:
        yield 1
        yield 2
    finally:
        print("  cleanup")


c = closer()
next(c)
c.close()
c.close()
print("closed", list(c))


def inner():
    yield "inner-1"
    yield "inner-2"
    return "inner-return"


def outer():
    result = yield from inner()
    yield f"outer saw {result}"
    yield from [10, 20]
    yield from (ch for ch in "ab")


o = outer()
print("yield-from", next(o), next(o), next(o), list(o))


def flatten(items):
    for it in items:
        if isinstance(it, (list, tuple)):
            yield from flatten(it)
        else:
            yield it


print("recursive-yield-from", list(flatten([1, [2, (3, [4, 5]), []], 6])))


def naturals():
    n = 0
    while True:
        yield n
        n += 1


print("infinite", list(islice(naturals(), 5)), list(islice((x * x for x in count(3)), 3)))
evens = (n for n in naturals() if n % 2 == 0)
print("infinite-filter", [next(evens) for _ in range(4)])
print("builtins", sum(x for x in range(10)), max(x % 7 for x in range(20)), min((x for x in [3, 1, 2]), default=0), any(x > 8 for x in range(10)), all(x < 5 for x in range(10)))
print("builtins2", sorted(x for x in "banana"), set(x % 3 for x in range(10)), dict((x, x * x) for x in range(3)), tuple(x for x in "ab"), "".join(c.upper() for c in "abc"))
print("builtins3", list(enumerate(x for x in "xy")), list(zip((i for i in range(3)), "abc")), list(map(str, (i for i in range(3)))), sum((i for i in range(4)), 100))
it = (x for x in range(3))
print("single-use", list(it), list(it), sum(x for x in []))
data = [5, 3, 8, 1]
gen = (x * 2 for x in data)
data.append(100)
print("late-source", list(gen))
data = [5, 3]
gen = (x for x in data)
data = [0]
print("bound-iterable", list(gen))


def pipeline(lines):
    stripped = (ln.strip() for ln in lines)
    nonempty = (ln for ln in stripped if ln)
    parsed = (ln.split("=", 1) for ln in nonempty if "=" in ln)
    return {k.strip(): v.strip() for k, v in parsed}


print("pipeline", pipeline(["a = 1", "", "  b=2 ", "junk", "c = x = y"]))


def counter_gen():
    count = 0
    while count < 3:
        count += 1
        yield count


gens = [counter_gen() for _ in range(3)]
print("interleave", [next(g) for g in gens], [next(g) for g in gens], [list(g) for g in gens])


def gen_state():
    yield 1


gs = gen_state()
print("gen-type", type(gs).__name__, iter(gs) is gs, hasattr(gs, "send"))


def fib_gen(limit):
    a, b = 0, 1
    while a < limit:
        yield a
        a, b = b, a + b


print("fib", list(fib_gen(1000)), sum(fib_gen(10**6)))


def windows(seq, n):
    for i in range(len(seq) - n + 1):
        yield seq[i:i + n]


print("windows", list(windows([1, 2, 3, 4, 5], 3)), [sum(w) for w in windows(list(range(10)), 4)])


def gen_raises():
    yield 1
    raise ValueError("inside generator")


try:
    for v in gen_raises():
        print("got", v)
except ValueError as e:
    print("gen-exception", e)


def stop_inside():
    yield 1
    return
    yield 2


print("early-return", list(stop_inside()))


def lazy_chain(*gens):
    for g in gens:
        yield from g


print("lazy-chain", list(lazy_chain(range(2), iter("ab"), (i for i in [9]))))
squares = {n: (lambda m: (m * m for _ in range(2)))(n) for n in range(3)}
print("gen-in-dict", {k: list(v) for k, v in squares.items()})
