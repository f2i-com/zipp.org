# Generators driven every way the runtime steps them: for loops, next()
# with and without a default, list/sum/zip/enumerate, send/throw/close,
# return values (StopIteration.value, yield from results), exceptions
# escaping the body, yields inside try/except/finally (the current exception
# saved and restored across a yield), nested and recursive generators,
# generator expressions, a generator re-entered while running, exhausted
# generators stepped again, and early exit from a for loop.
import sys


def count(n):
    for i in range(n):
        yield i


def with_return(n):
    total = 0
    for i in range(n):
        total += i
        yield i
    return "total=%d" % total


def delegating(n):
    got = yield from with_return(n)
    yield "delegated:" + got
    got2 = yield from count(2)
    yield "second:%r" % (got2,)


def echo():
    received = []
    try:
        while True:
            value = yield len(received)
            if value == "stop":
                return received
            received.append(value)
    finally:
        received.append("closed")


def raises_inside(n):
    for i in range(n):
        if i == 2:
            raise ValueError("boom at %d" % i)
        yield i


def handler_state():
    try:
        raise KeyError("outer")
    except KeyError:
        yield sys.exc_info()[0].__name__
        yield repr(sys.exc_info()[1])
    yield sys.exc_info()[0]


def catches_thrown():
    while True:
        try:
            yield "waiting"
        except ValueError as e:
            yield "caught %s" % e


def tree(depth):
    if depth == 0:
        yield "leaf"
        return
    yield "node%d" % depth
    yield from tree(depth - 1)
    yield from tree(depth - 1)


print(list(count(5)), sum(count(100)), list(zip(count(3), count(10))), list(enumerate(count(3), 1)))
g = count(3)
print(next(g), next(g), next(g), next(g, "default"), next(g, None))
try:
    next(g)
except StopIteration as e:
    print("stop", e.value, e.args)
g = with_return(4)
print(list(g))
g = with_return(3)
out = []
while True:
    try:
        out.append(next(g))
    except StopIteration as e:
        out.append(e.value)
        break
print(out)
print(list(delegating(3)))
e = echo()
print(next(e), e.send("a"), e.send("b"))
try:
    e.send("stop")
except StopIteration as s:
    print("returned", s.value)
e = echo()
next(e)
e.send(1)
e.close()
print("closed ok", list(e))
g = raises_inside(5)
got = []
try:
    for v in g:
        got.append(v)
except ValueError as err:
    got.append(str(err))
print(got, list(g))
h = handler_state()
print(next(h), next(h), next(h), sys.exc_info()[0])
c = catches_thrown()
print(next(c), c.throw(ValueError("v1")), next(c), c.throw(ValueError("v2")))
try:
    c.throw(TypeError("t"))
except TypeError as err:
    print("propagated", err)
print(list(tree(3)))
print(sum(x * x for x in range(10)), list(x for x in "abc" if x != "b"), max((len(w), w) for w in ["aa", "b", "ccc"]))


def reenter():
    yield next(me)


me = reenter()
try:
    next(me)
except ValueError as err:
    print("running:", err)
first = []
for v in count(10):
    if v == 3:
        break
    first.append(v)
print(first)
gen = count(2)
print(list(gen), list(gen), next(gen, "exhausted"))


def pipeline(n):
    evens = (x for x in count(n) if x % 2 == 0)
    squares = (x * x for x in evens)
    return sum(squares)


print([pipeline(n) for n in range(8)])


def fib_gen():
    a, b = 0, 1
    while True:
        yield a
        a, b = b, a + b


fg = fib_gen()
print([next(fg) for _ in range(20)])
total = 0
for k in range(50):
    for v in count(40):
        total += v
print(total)


def gen_finally(log):
    try:
        yield 1
        yield 2
    finally:
        log.append("finally")


log = []
gf = gen_finally(log)
for v in gf:
    log.append(v)
    break
gf.close()
print(log)
