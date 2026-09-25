# Traceback objects (e.__traceback__, sys.exc_info()[2]) and the traceback
# module over them: frame names and line numbers as CPython records them,
# for exceptions caught, re-raised, chained, and raised from hot loops.
# Output is normalized to what does not depend on where the program runs:
# file names and the source lines under them are left out (a program run
# from memory has no file to read them from), and so are the caret lines
# CPython draws under an expression.
import os
import re
import sys
import traceback

CARET = re.compile(r"^\s*[~^]+[~^ ]*$")


def norm(text):
    out = []
    source = False
    for line in text.splitlines():
        if CARET.match(line):
            continue
        m = re.match(r'^(\s*File ")([^"]*)(".*)$', line)
        if m:
            out.append(m.group(1) + "*" + m.group(3))
            source = True
            continue
        if source and line.startswith("    "):
            continue
        source = False
        out.append(line)
    return "\n".join(out)


def chain(tb):
    out = []
    first = tb
    while tb is not None:
        # A finished frame's f_lineno is its last line; a running one's
        # (the frame that caught the exception) moves on.
        out.append((tb.tb_frame.f_code.co_name, tb.tb_lineno,
                    tb.tb_frame.f_lineno if tb.tb_next is not None and tb is not first else None))
        tb = tb.tb_next
    return out


def show(label, e):
    print(label, type(e).__name__, chain(e.__traceback__))
    for fs in traceback.extract_tb(e.__traceback__):
        print("   ", fs.lineno, fs.name)


def inner(x):
    if x > 2:
        raise ValueError("too big: %d" % x)
    return x


def middle(x):
    y = x + 1
    return inner(y)


def outer(x):
    return middle(x) * 2


try:
    outer(5)
except ValueError as e:
    show("simple", e)
    print(norm("".join(traceback.format_tb(e.__traceback__))))
    print(norm("".join(traceback.format_exception(e))))
    print(traceback.format_exception_only(e))
    t, v, tb = sys.exc_info()
    print("exc_info", t.__name__, v is e, chain(tb) == chain(e.__traceback__))
    print("exception", sys.exception() is e)
    print(norm(traceback.format_exc()))
    traceback.print_exc()
    traceback.print_tb(e.__traceback__)


def caught_here():
    try:
        inner(9)
    except ValueError as e:
        return e


show("returned", caught_here())


def bare_reraise():
    try:
        middle(7)
    except ValueError:
        x = 1
        raise


def explicit_reraise():
    try:
        middle(7)
    except ValueError as e:
        y = 2
        raise e


for f in (bare_reraise, explicit_reraise):
    try:
        f()
    except ValueError as e:
        show(f.__name__, e)


def with_finally():
    try:
        inner(8)
    finally:
        z = 3


try:
    with_finally()
except ValueError as e:
    show("finally", e)


class Ctx:
    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


def with_block():
    with Ctx():
        inner(6)


try:
    with_block()
except ValueError as e:
    show("with", e)


def cause():
    try:
        {}["missing"]
    except KeyError as k:
        raise RuntimeError("lookup failed") from k


def context():
    try:
        [][1]
    except IndexError:
        raise TypeError("while handling")


def suppressed():
    try:
        1 / 0
    except ZeroDivisionError:
        raise ValueError("clean") from None


for f in (cause, context, suppressed):
    try:
        f()
    except Exception as e:
        show(f.__name__, e)
        print(norm("".join(traceback.format_exception(e))))
        print(norm("".join(traceback.format_exception(e, chain=False))))


class Point:
    def __init__(self, x):
        self.x = x

    def check(self):
        return self.helper()

    def helper(self):
        return self.x.missing


try:
    Point(1).check()
except AttributeError as e:
    show("method", e)


def gen(n):
    for i in range(n):
        if i == 2:
            raise KeyError(i)
        yield i


try:
    list(gen(5))
except KeyError as e:
    show("generator", e)
    print(traceback.format_exception_only(e))


class MyError(Exception):
    pass


try:
    raise MyError()
except MyError as e:
    show("empty", e)
    print(traceback.format_exception_only(e))


def hot(n):
    total = 0
    for i in range(n):
        try:
            total += inner(i % 4)
        except ValueError as e:
            total += len(chain(e.__traceback__))
    return total


print("hot", hot(20000))


def deep(n):
    if n == 0:
        raise ValueError("bottom")
    return deep(n - 1)


try:
    deep(10)
except ValueError as e:
    print("deep", len(chain(e.__traceback__)))
    print(norm("".join(traceback.format_tb(e.__traceback__))))


tb = None
err = None
try:
    outer(4)
except ValueError as e:
    tb = e.__traceback__
    err = e
    print("same object", e.__traceback__ is tb)
print("frame", tb.tb_frame.f_code.co_name, tb.tb_next.tb_frame.f_back.f_code.co_name,
      tb.tb_frame.f_globals["__name__"])
te = traceback.TracebackException.from_exception(err)
print("te", te.exc_type_str, str(te), len(te.stack))
print(norm("".join(te.format())))
print([str(x) for x in traceback.StackSummary.from_list([("a.py", 1, "f", "x = 1")]).format()])


class Box:
    def __init__(self):
        self.value = 1
        self.values = [1]


def typo_cases():
    import collections
    cases = [
        lambda: valeu,
        lambda: Box().valeu,
        lambda: Box().vlaue,
        lambda: math.sqrt(2),
        lambda: collections.OrderDict(),
        lambda: Box.valeu,
        lambda: prnt("x"),
    ]
    for c in cases:
        try:
            c()
        except (NameError, AttributeError) as e:
            print(traceback.format_exception_only(e))


valeu_ok = 1
typo_cases()
