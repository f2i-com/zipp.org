# Exceptions: try/except/else/finally, raise from, custom hierarchies, with.
def risky(n):
    if n == 0:
        raise ValueError("zero is not allowed")
    if n < 0:
        raise KeyError(n)
    return 10 // n


for n in (5, 0, -1, 3):
    try:
        r = risky(n)
    except ValueError as e:
        print("ValueError:", e, e.args, repr(e))
    except (KeyError, IndexError) as e:
        print("lookup:", type(e).__name__, str(e), e.args[0])
    else:
        print("ok", r)
    finally:
        print("done", n)


class AppError(Exception):
    def __init__(self, message, code=1):
        super().__init__(message)
        self.code = code

    def __str__(self):
        return f"[{self.code}] {self.args[0]}"


class NotFound(AppError):
    pass


def lookup(key):
    try:
        return {"a": 1}[key]
    except KeyError as e:
        raise NotFound(f"missing {key}", 404) from e


try:
    lookup("b")
except AppError as e:
    print(type(e).__name__, e, e.code, isinstance(e, Exception), type(e.__cause__).__name__, e.__cause__.args)

try:
    try:
        1 / 0
    except ZeroDivisionError:
        raise RuntimeError("wrapped")
except RuntimeError as e:
    print(e, type(e.__context__).__name__)


def f():
    try:
        return "try"
    finally:
        print("finally runs before return")


print(f())


def g():
    for i in range(5):
        try:
            if i == 1:
                continue
            if i == 3:
                break
            print("body", i)
        finally:
            print("cleanup", i)
    return "end"


print(g())


def h():
    try:
        try:
            raise ValueError("inner")
        finally:
            print("inner finally")
    except ValueError as e:
        print("caught", e)
        return 1
    finally:
        print("outer finally")


print(h())

try:
    raise
except RuntimeError as e:
    print("bare raise outside handler:", e)

try:
    try:
        raise TypeError("first")
    except TypeError:
        raise
except TypeError as e:
    print("re-raised", e)

try:
    [1, 2][5]
except IndexError as e:
    print("IndexError:", e)
try:
    {}["k"]
except KeyError as e:
    print("KeyError:", e, repr(e))
try:
    int("x")
except ValueError as e:
    print("ValueError:", e)
try:
    None.attr
except AttributeError as e:
    print("AttributeError:", e)
try:
    undefined_name
except NameError as e:
    print("NameError:", e)
try:
    "a" + 1
except TypeError as e:
    print("TypeError:", e)
try:
    len(5)
except TypeError as e:
    print("TypeError:", e)
try:
    (lambda: 1)(2)
except TypeError as e:
    print("TypeError:", e)


def locals_check():
    try:
        print(late)
    except UnboundLocalError as e:
        print("UnboundLocalError:", e)
    late = 1


locals_check()


class Resource:
    def __init__(self, name, fail=False):
        self.name = name
        self.fail = fail

    def __enter__(self):
        print("enter", self.name)
        return self.name

    def __exit__(self, t, v, tb):
        print("exit", self.name, t.__name__ if t else None)
        return self.fail


with Resource("a") as ra, Resource("b") as rb:
    print("inside", ra, rb)

with Resource("swallow", fail=True):
    raise ValueError("swallowed")
print("after swallow")

try:
    with Resource("propagate"):
        raise ValueError("propagated")
except ValueError as e:
    print("caught", e)


def returns_in_with():
    with Resource("ret"):
        return "returned"


print(returns_in_with())

assert 1 + 1 == 2
try:
    assert 1 + 1 == 3, "math is broken"
except AssertionError as e:
    print("AssertionError:", e)

try:
    raise StopIteration
except StopIteration:
    print("StopIteration caught")

results = []
for value in ["1", "x", "3"]:
    try:
        results.append(int(value))
    except ValueError:
        results.append(None)
print(results)
print(issubclass(KeyError, LookupError), issubclass(ZeroDivisionError, ArithmeticError), issubclass(Exception, BaseException))
e = ValueError("a", 1)
print(e.args, str(e), repr(e))
