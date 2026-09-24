# Generator protocol through every consumer: for loops, next(), list(),
# sum(), zip(), send/throw/close, yield from and return values, PEP 479,
# re-entrancy, and the exception state a generator keeps across yields.
import sys


def count(n):
    for i in range(n):
        yield i


def with_return(n):
    for i in range(n):
        yield i * i
    return "done-%d" % n


def returns_none():
    yield 1
    return


def delegator():
    r = yield from with_return(3)
    yield r
    r2 = yield from returns_none()
    yield r2


print(list(count(5)), sum(count(10)), list(zip(count(3), count(4))))
print(list(delegator()))
total = 0
for v in count(1000):
    total += v
print(total)
for v in count(10):
    if v == 3:
        break
print("broke at", v)

g = with_return(2)
print(next(g), next(g))
try:
    next(g)
except StopIteration as e:
    print("stop value", e.value, e.args)
try:
    next(g)
except StopIteration as e:
    print("after stop", e.value, e.args)
print(next(g, "dflt"), next(count(0), "empty"))

# An exhausted generator stays exhausted in a for loop.
g = count(2)
print(list(g), list(g))
for x in g:
    print("never")


def echo():
    received = []
    try:
        while True:
            x = yield len(received)
            received.append(x)
    except GeneratorExit:
        print("closing with", received)
        raise


e = echo()
print(next(e), e.send("a"), e.send("b"))
e.close()
e.close()
try:
    e.send("c")
except StopIteration:
    print("send after close: StopIteration")

e2 = echo()
try:
    e2.send("x")
except TypeError as ex:
    print("TypeError:", ex)


def catcher():
    while True:
        try:
            yield "ready"
        except ValueError as ex:
            print("caught inside", ex)
            yield "recovered"


c = catcher()
print(next(c), c.throw(ValueError("boom")), next(c))
try:
    c.throw(KeyError("k"))
except KeyError as ex:
    print("propagated", repr(ex))
print(next(c, "finished"))


def leaks_stop():
    yield 1
    raise StopIteration("inner")


try:
    for v in leaks_stop():
        print("got", v)
except RuntimeError as ex:
    print("RuntimeError:", ex, "| cause:", repr(ex.__cause__), "| suppress:", ex.__suppress_context__)


def raises_mid(n):
    for i in range(n):
        if i == 2:
            raise ValueError("mid %d" % i)
        yield i


seen = []
try:
    for v in raises_mid(5):
        seen.append(v)
except ValueError as ex:
    print("seen", seen, "error", ex)
g = raises_mid(5)
print(next(g), next(g))
try:
    next(g)
except ValueError as ex:
    print("next raised", ex)
print(next(g, "exhausted after error"))


def reentrant():
    yield next(me)


me = reentrant()
try:
    next(me)
except ValueError as ex:
    print("ValueError:", ex)


def keeps_exception():
    try:
        raise ValueError("handled")
    except ValueError:
        yield sys.exc_info()[0].__name__
        yield sys.exc_info()[0].__name__
        raise KeyError("later")


k = keeps_exception()
print(next(k), sys.exc_info()[0])
print(next(k), sys.exc_info()[0])
try:
    next(k)
except KeyError as ex:
    print("context", repr(ex.__context__))

try:
    raise IndexError("outer")
except IndexError:
    k2 = keeps_exception()
    print(next(k2), sys.exc_info()[0].__name__)
    try:
        next(k2)
        next(k2)
    except KeyError as ex:
        print("context2", repr(ex.__context__), repr(ex.__context__.__context__))


def fails_in_handler():
    try:
        yield 1
    finally:
        print("finally runs")


f = fails_in_handler()
next(f)
f.close()
f2 = fails_in_handler()
print(list(f2))


def nested(depth):
    if depth == 0:
        yield "leaf"
        return
    for v in nested(depth - 1):
        yield v + str(depth)


print(list(nested(4)))
gen_exp = (x * 2 for x in count(4))
print(next(gen_exp), list(gen_exp), list(gen_exp))
print(sorted(count(5), reverse=True), max(count(7)), min(count(3)), "".join(str(i) for i in count(6)))
print(dict(zip(count(3), "abc")), tuple(enumerate(count(2))), any(x > 3 for x in count(5)), all(x < 3 for x in count(5)))


def yields_none():
    yield
    yield None


print(list(yields_none()), [x for x in yields_none()])
it = iter(count(3))
print(it is iter(it), next(it), list(it))
