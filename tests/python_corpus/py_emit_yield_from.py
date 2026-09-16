# `yield from` delegation (PEP 380): send, throw and close reach the
# subiterator; its return value is the expression's value.


def averager():
    total = 0.0
    count = 0
    while True:
        x = yield (total / count if count else None)
        if x is None:
            return (count, total)
        total += x
        count += 1


def delegate(log):
    result = yield from averager()
    log.append(result)
    yield "after"


log = []
g = delegate(log)
print("prime", next(g))
print("send", g.send(10), g.send(20), g.send(45))
print("finish", g.send(None), log)


# throw() is raised inside the subgenerator, which may handle it.
def inner_catch():
    try:
        yield "a"
    except ValueError as e:
        yield "inner caught " + str(e)
    yield "b"


def outer_throw():
    yield from inner_catch()
    yield "outer done"


ot = outer_throw()
print("throw", next(ot), ot.throw(ValueError("boom")), next(ot), next(ot))


# An exception the subgenerator does not handle leaves through the delegator.
def inner_plain():
    yield 1
    yield 2


def outer_plain():
    try:
        yield from inner_plain()
    except KeyError as e:
        yield "outer caught " + repr(e)


op = outer_plain()
print("unhandled", next(op), op.throw(KeyError("k")))


# close() closes the subgenerator (its finally runs) before the delegator's.
def inner_fin(name):
    try:
        yield name + "-1"
        yield name + "-2"
    finally:
        print("  inner finally", name)


def outer_fin():
    try:
        yield from inner_fin("x")
    finally:
        print("  outer finally")


of = outer_fin()
print("close", next(of))
of.close()
print("closed")


# A subgenerator that finishes after a throw ends the delegation.
def inner_return_on_throw():
    try:
        yield "waiting"
    except RuntimeError:
        return "recovered"


def outer_return():
    yield from inner_return_on_throw()
    yield "delegation over"


orr = outer_return()
print("throw-return", next(orr), orr.throw(RuntimeError("r")))


# A non-generator iterator with send/throw/close methods.
class Echo:
    def __init__(self):
        self.n = 0

    def __iter__(self):
        return self

    def __next__(self):
        self.n += 1
        if self.n > 3:
            raise StopIteration("echo-done")
        return self.n

    def send(self, v):
        print("  Echo.send", v)
        return self.__next__()

    def throw(self, exc):
        print("  Echo.throw", type(exc).__name__)
        return 99

    def close(self):
        print("  Echo.close")


def use_echo():
    r = yield from Echo()
    yield "echo result " + r


ue = use_echo()
print("echo", next(ue), ue.send("s"), ue.throw(ValueError("v")), next(ue), next(ue))
ue = use_echo()
next(ue)
ue.close()


# Plain iterables: send() needs a send method, throw() re-raises at the delegator.
def over_list():
    r = yield from [1, 2]
    yield r


ol = over_list()
print("list", next(ol))
try:
    ol.send(5)
except AttributeError as e:
    print("AttributeError", e)
ol = over_list()
next(ol)
try:
    ol.throw(IndexError("i"))
except IndexError as e:
    print("IndexError", e)


def chain(*its):
    for it in its:
        yield from it


print("chain", list(chain(range(3), "ab", (x * x for x in range(3)))))


def empty():
    return (yield from ())


try:
    next(empty())
except StopIteration as e:
    print("empty value", e.value)


def twice():
    def sub():
        yield 1
        return "r"
    a = yield from sub()
    b = yield from sub()
    return (a, b)


t = twice()
print("twice", next(t), next(t))
try:
    next(t)
except StopIteration as e:
    print("twice value", e.value)


# `x = yield` resumed by next() binds None.
def plain_yield():
    x = yield 1
    print("  resumed with", x, x is None)
    y = yield 2
    print("  again", y is None)


py = plain_yield()
next(py)
next(py)
for _ in py:
    pass
