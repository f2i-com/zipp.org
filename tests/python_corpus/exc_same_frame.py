# Exceptions raised and caught in the same frame: the traceback each one
# carries (frames and lines), its __context__, the handler that runs, and
# the same shapes repeated in loops long enough to be compiled.
import sys
import traceback


def chain(tb):
    out = []
    while tb is not None:
        out.append((tb.tb_frame.f_code.co_name, tb.tb_lineno))
        tb = tb.tb_next
    return out


def show(label, e):
    ctx = e.__context__
    print(label, type(e).__name__, e.args, chain(e.__traceback__),
          type(ctx).__name__ if ctx is not None else None)


def raise_in_try():
    try:
        raise ValueError("a")
    except ValueError as e:
        show("raise_in_try", e)


def miss_in_try():
    d = {"k": 1}
    try:
        d["absent"]
    except KeyError as e:
        show("miss_in_try", e)


def bare_reraise():
    try:
        raise TypeError("b")
    except TypeError:
        raise


def raise_in_except():
    try:
        raise ValueError("c")
    except ValueError:
        raise KeyError("d")


def finally_then_raise():
    try:
        try:
            raise ValueError("e")
        finally:
            print("finally ran")
    except ValueError as e:
        show("finally_then_raise", e)


def raise_in_finally_body():
    try:
        try:
            pass
        finally:
            raise IndexError("f")
    except IndexError as e:
        show("raise_in_finally_body", e)


def nested_try():
    try:
        try:
            raise ValueError("g")
        except KeyError:
            print("wrong handler")
    except ValueError as e:
        show("nested_try", e)


def inner_with_handler():
    class CM:
        def __enter__(self):
            return self

        def __exit__(self, t, v, tb):
            print("exit sees", t.__name__ if t else None)
            return True

    try:
        with CM():
            raise ValueError("h")
        print("with swallowed it")
    except ValueError:
        print("not reached")


def handler_raises_again():
    try:
        raise ValueError("i")
    except ValueError:
        try:
            raise KeyError("j")
        except KeyError as e:
            show("handler_raises_again", e)


def no_match_propagates():
    try:
        raise ValueError("k")
    except KeyError:
        print("wrong")


for f in (raise_in_try, miss_in_try, finally_then_raise, raise_in_finally_body,
          nested_try, inner_with_handler, handler_raises_again):
    f()
for f in (bare_reraise, raise_in_except, no_match_propagates):
    try:
        f()
    except Exception as e:
        show(f.__name__, e)
print(sys.exc_info())


def hot(n):
    d = {"present": 1}
    caught = 0
    last = None
    for i in range(n):
        try:
            d["absent"]
        except KeyError as e:
            caught += 1
            last = e
        try:
            if i % 3 == 0:
                raise ValueError(i)
            caught += 10
        except ValueError as e:
            caught += 100
            last2 = e
    return caught, chain(last.__traceback__), last.args, last2.args, chain(last2.__traceback__)


print(hot(5000))


def hot_reraise(n):
    count = 0
    tb = None
    for i in range(n):
        try:
            try:
                raise ValueError(i)
            except ValueError:
                count += 1
                if i % 1000 == 999:
                    raise
        except ValueError as e:
            count += 1000
            tb = chain(e.__traceback__)
    return count, tb


print(hot_reraise(4000))
try:
    hot_reraise(10)
    {}["x"]
except KeyError as e:
    print("outer", "".join(traceback.format_exception_only(e)).strip(), [n for n, _ in chain(e.__traceback__)])
