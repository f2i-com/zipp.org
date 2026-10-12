# Exceptions leaving frames on their way to a handler: the traceback each
# collects (frames, lines, the catching frame first), through plain calls,
# methods, properties, generators, finally blocks, with blocks, re-raises
# from the frame that caught them, raising a caught exception anew in
# another frame, deep recursion, library code, and the same in loops long
# enough to be compiled.
import json
import sys
import traceback


def chain(tb):
    out = []
    while tb is not None:
        out.append((tb.tb_frame.f_code.co_name, tb.tb_lineno))
        tb = tb.tb_next
    return out


def show(label, e):
    print(label, type(e).__name__, e.args[:1], chain(e.__traceback__),
          [(f.name, f.lineno) for f in traceback.extract_tb(e.__traceback__)])


def leaf(v):
    if v < 0:
        raise ValueError(v)
    return v


def mid(v):
    return leaf(v) + 1


def top(v):
    try:
        return mid(v)
    except ValueError as e:
        show("top", e)
        return None


top(-1)


class Acct:
    def __init__(self):
        self._c = 0

    @property
    def bal(self):
        return self._c

    @bal.setter
    def bal(self, v):
        if v < 0:
            raise ArithmeticError("neg")
        self._c = v

    def take(self, n):
        self.bal = self.bal - n


def via_property():
    a = Acct()
    try:
        a.take(5)
    except ArithmeticError as e:
        show("property", e)


via_property()


def gen(n):
    yield 1
    leaf(n)
    yield 2


def via_gen():
    try:
        for _ in gen(-3):
            pass
    except ValueError as e:
        show("generator", e)


via_gen()


def with_finally(v):
    try:
        return mid(v)
    finally:
        pass


def via_finally():
    try:
        with_finally(-4)
    except ValueError as e:
        show("finally", e)


via_finally()


class CM:
    def __enter__(self):
        return self

    def __exit__(self, t, v, tb):
        return False


def with_block(v):
    with CM():
        return mid(v)


def via_with():
    try:
        with_block(-5)
    except ValueError as e:
        show("with", e)


via_with()


def catch_and_reraise(v):
    try:
        mid(v)
    except ValueError:
        raise


def via_reraise():
    try:
        catch_and_reraise(-6)
    except ValueError as e:
        show("reraise", e)


via_reraise()


def catch_and_raise_anew(v):
    try:
        mid(v)
    except ValueError as e:
        err = e
    raise err


def via_raise_anew():
    try:
        catch_and_raise_anew(-7)
    except ValueError as e:
        show("raise anew", e)


via_raise_anew()


def chained(v):
    try:
        mid(v)
    except ValueError as e:
        raise KeyError("k") from e


def via_chain():
    try:
        chained(-8)
    except KeyError as e:
        show("chained", e)
        show("cause", e.__cause__)


via_chain()


def deep(n):
    if n == 0:
        raise ValueError("bottom")
    return deep(n - 1)


try:
    deep(30)
except ValueError as e:
    tb = chain(e.__traceback__)
    print("deep", len(tb), tb[:2], tb[-2:])


def recurse(n):
    return recurse(n + 1)


try:
    recurse(0)
except RecursionError as e:
    tb = chain(e.__traceback__)
    print("recursion", type(e).__name__, len(tb) > 50, tb[:2], tb[-1])

try:
    json.loads("{bad")
except ValueError as e:
    names = [n for n, _ in chain(e.__traceback__)]
    print("library", type(e).__name__, names[0])


def hot(n):
    failed = 0
    last = None
    for i in range(n):
        try:
            mid(-i if i % 2 else i)
        except ValueError as e:
            failed += 1
            last = e
    return failed, chain(last.__traceback__), last.args


print(hot(4000))


def hot_property(n):
    a = Acct()
    a.bal = 100
    failed = 0
    for i in range(n):
        try:
            a.take(i % 7)
        except ArithmeticError as e:
            failed += 1
            a.bal = 100
            tb = chain(e.__traceback__)
    return failed, a.bal, tb


print(hot_property(3000))
try:
    top(-1) or mid(-2)
except ValueError:
    print("".join(traceback.format_exception(*sys.exc_info())).count("File"))
