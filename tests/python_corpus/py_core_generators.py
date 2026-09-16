# Generator protocol: close() raises GeneratorExit in the frame, cleanup errors
# propagate, throw() keeps the return value, PEP 479, send() on a fresh generator,
# and StopIteration after exhaustion.
def worker():
    try:
        while True:
            item = yield
    except GeneratorExit:
        print('cleanup on close')
        raise
g = worker(); next(g); print("close ->", g.close())

def retclose():
    try:
        yield 1
    except GeneratorExit:
        return "rv-on-close"
g = retclose(); next(g); print("close-return", g.close())

def fin_raises():
    try:
        yield 1
    finally:
        raise ValueError("error during cleanup")
g = fin_raises(); next(g)
try:
    g.close(); print("close swallowed")
except ValueError as e:
    print("ValueError propagated:", e)

def throw_ret():
    try:
        yield 1
    except KeyError:
        return "rv"
g = throw_ret(); next(g)
try:
    g.throw(KeyError("k"))
except StopIteration as e:
    print("StopIteration value:", e.value)

def pairs(seq):
    it = iter(seq)
    for a in it:
        yield a, next(it)
try:
    print(list(pairs([1, 2, 3])))
except RuntimeError as e:
    print("RuntimeError:", e, type(e.__cause__).__name__, e.__suppress_context__)

def h():
    yield 1
    raise StopIteration
try:
    print(list(h()))
except RuntimeError as e:
    print("RuntimeError2:", e)

def inner():
    yield 5
    raise StopIteration("x")
def outer():
    yield from inner()
    yield 6
try:
    print(list(outer()))
except RuntimeError as e:
    print("RuntimeError3:", e)

def gen1():
    yield 1
g = gen1()
try:
    g.send(5)
except TypeError as e:
    print("TypeError:", e)
print("send None ok", g.send(None))

def ignores():
    try:
        yield 1
    except GeneratorExit:
        yield 2
    print("never")
g = ignores(); next(g)
try:
    g.close()
except RuntimeError as e:
    print("RuntimeError:", e)

def selfsend():
    yield g2.send(None)
g2 = selfsend()
try:
    next(g2)
except ValueError as e:
    print("ValueError", e)

# unstarted close / throw
def u():
    try:
        yield 1
    finally:
        print("u finally")
x = u(); print("unstarted close", x.close(), list(x))
x = u()
try:
    x.throw(KeyError("kk"))
except KeyError as e:
    print("unstarted throw", repr(e), list(x))

# throw StopIteration into a generator
def s():
    yield 1
    yield 2
x = s(); next(x)
try:
    x.throw(StopIteration("into"))
except RuntimeError as e:
    print("throw-stopiteration", e, repr(e.__cause__))

# contextmanager with StopIteration inside the block
from contextlib import contextmanager
@contextmanager
def cm():
    yield "v"
try:
    with cm() as v:
        raise StopIteration("block")
except StopIteration as e:
    print("cm passes StopIteration", repr(e))

@contextmanager
def cm2():
    try:
        yield "v"
    except ZeroDivisionError:
        print("cm2 handled")
with cm2():
    1 / 0
print("after cm2")

# exhausted value
def tracer():
    yield 1
    return "result"
g = tracer()
print("drain", list(g))
try:
    next(g)
except StopIteration as e:
    print("exhausted-value", e.value, e.args)
g = tracer(); next(g)
for _ in range(2):
    try:
        next(g)
    except StopIteration as e:
        print("stop", e.value)
g = tracer(); next(g)
try:
    g.send(None)
except StopIteration as e:
    print("send-stop", e.value)
try:
    g.send(None)
except StopIteration as e:
    print("send-stop2", e.value)

# a generator that closes itself is running
def closeself():
    g3.close()
    yield 1
g3 = closeself()
try:
    next(g3)
except ValueError as e:
    print("close-running", e)
