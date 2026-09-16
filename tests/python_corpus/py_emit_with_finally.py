# `with` binds its target inside the protected region, and an exception
# raised in a `finally` body or in `__exit__` while another is propagating
# gets that exception as its __context__.


class CM:
    def __init__(self, name, value=None, suppress=False):
        self.name = name
        self.value = value
        self.suppress = suppress

    def __enter__(self):
        print("enter", self.name)
        return self if self.value is None else self.value

    def __exit__(self, t, v, tb):
        print("exit", self.name, t.__name__ if t else None)
        return self.suppress


try:
    with CM("unpack") as (p, q):
        print("body")
except TypeError:
    print("TypeError reached the caller")

with CM("suppressed", value=5, suppress=True) as [a, b]:
    print("not reached")
print("after suppressed")


class Holder:
    def __setattr__(self, name, value):
        raise ValueError("no " + name)


h = Holder()
try:
    with CM("attr") as h.slot:
        pass
except ValueError as e:
    print("ValueError", e)

with CM("outer"), CM("inner", value=(1, 2)) as (x, y):
    print("both", x, y)


def unbound_after_suppress():
    with CM("inner-suppress", value=7, suppress=True) as (m, n):
        pass
    try:
        print(m)
    except UnboundLocalError:
        print("target unbound after suppression")


unbound_after_suppress()

try:
    try:
        raise KeyError(1)
    finally:
        try:
            raise ValueError(2)
        except ValueError as v:
            print("context in finally", repr(v.__context__))
except KeyError:
    print("KeyError continues")

try:
    try:
        raise KeyError("first")
    finally:
        raise ValueError("second")
except ValueError as v:
    print("direct", repr(v), repr(v.__context__))

try:
    try:
        pass
    finally:
        raise ValueError("clean")
except ValueError as v:
    print("no pending", repr(v.__context__))


def return_in_finally():
    try:
        raise KeyError("dropped")
    finally:
        return "finally wins"


print(return_in_finally())


def break_in_finally():
    for i in range(3):
        try:
            raise KeyError(i)
        finally:
            if i == 0:
                break
    try:
        raise ValueError("later")
    except ValueError as e:
        return repr(e.__context__)


print("break", break_in_finally())


class BadExit:
    def __enter__(self):
        return self

    def __exit__(self, t, v, tb):
        raise KeyError("from exit")


try:
    with BadExit():
        raise ValueError("body")
except KeyError as k:
    print("exit raised", repr(k), repr(k.__context__))

try:
    with BadExit():
        pass
except KeyError as k:
    print("exit raised cleanly", repr(k.__context__))
