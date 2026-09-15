# Exception control flow: raise/catch inside hot loops, finally overriding return,
# break/continue through finally, chaining (__cause__, __context__, suppression), re-raise.
def parse_all(items):
    good, bad = [], 0
    for it in items:
        try:
            good.append(int(it))
        except ValueError:
            bad += 1
        except TypeError:
            bad += 100
        else:
            good[-1] *= 2
        finally:
            good.append("|")
    return good, bad


print("loop-catch", parse_all(["1", "x", None, "4", "", "-3"]))
hits = 0
for i in range(200):
    try:
        if i % 7 == 0:
            raise KeyError(i)
        if i % 11 == 0:
            raise IndexError(i)
        hits += 1
    except LookupError as e:
        hits -= 1 if isinstance(e, KeyError) else 2
print("hot-raise", hits)


def finally_overrides():
    try:
        return "try"
    finally:
        return "finally"


def finally_swallows():
    try:
        raise ValueError("lost")
    finally:
        return "swallowed"


def finally_side_effect():
    x = ["try"]
    try:
        return x
    finally:
        x.append("finally-mutated")


def nested_finally():
    out = []
    try:
        try:
            out.append("inner-try")
            return out
        finally:
            out.append("inner-finally")
    finally:
        out.append("outer-finally")


print("finally-return", finally_overrides(), finally_swallows(), finally_side_effect(), nested_finally())


def break_in_finally():
    for i in range(3):
        try:
            if i >= 1:
                raise RuntimeError(i)
        finally:
            if i == 1:
                break
    return i


def continue_in_finally():
    seen = []
    for i in range(4):
        try:
            if i % 2:
                raise ValueError(i)
            seen.append(i)
        finally:
            seen.append(-i)
            continue
    return seen


print("finally-break-continue", break_in_finally(), continue_in_finally())


def while_try(n):
    log = []
    while True:
        try:
            n -= 1
            if n == 0:
                return log
            if n % 2:
                continue
            log.append(n)
        finally:
            log.append("f")


print("while-try", while_try(6))


class AppError(Exception):
    pass


def layer1():
    raise KeyError("root")


def layer2():
    try:
        layer1()
    except KeyError as e:
        raise AppError("wrapped") from e


def layer3():
    try:
        layer2()
    except AppError as e:
        raise RuntimeError("outer")


try:
    layer3()
except RuntimeError as e:
    ctx = e.__context__
    print("chain", repr(e), repr(ctx), repr(ctx.__cause__), e.__cause__, e.__suppress_context__, ctx.__suppress_context__)
try:
    try:
        raise ValueError("a")
    except ValueError:
        raise TypeError("b") from None
except TypeError as e:
    print("from-none", repr(e.__context__), e.__cause__, e.__suppress_context__)
try:
    try:
        raise ZeroDivisionError("explicit")
    except ZeroDivisionError as z:
        try:
            raise IndexError("inner")
        except IndexError as ie:
            raise
except IndexError as e:
    print("implicit-context", type(e.__context__).__name__, e.__cause__ is None)


def reraise_after_handling():
    try:
        raise ValueError("original")
    except ValueError:
        try:
            raise TypeError("inner handled")
        except TypeError:
            pass
        raise


try:
    reraise_after_handling()
except ValueError as e:
    print("bare-reraise", e)
err = None
try:
    raise OSError("bound")
except OSError as caught:
    err = caught
try:
    caught
except NameError:
    print("except-name-cleared", repr(err))
e1 = ValueError("x")
try:
    raise e1
except ValueError as got:
    print("same-object", got is e1)


def gen_with_finally():
    try:
        yield 1
        yield 2
    finally:
        print("  generator finally")


for v in gen_with_finally():
    pass
gen = gen_with_finally()
next(gen)
gen.close()
print("gen-closed")


def exc_in_except():
    try:
        raise KeyError("first")
    except KeyError:
        raise KeyError("second")


try:
    exc_in_except()
except KeyError as e:
    print("exc-in-except", e, repr(e.__context__))
results = []
for exc_type in [ValueError, TypeError, KeyError, ZeroDivisionError, StopIteration, Exception]:
    try:
        try:
            raise exc_type("x")
        except (ValueError, TypeError) as e:
            results.append("inner:" + type(e).__name__)
            raise
        except ArithmeticError:
            results.append("arith")
    except Exception as outer:
        results.append("outer:" + type(outer).__name__)
print("dispatch", results)
try:
    raise Exception("args", 1, None)
except Exception as e:
    print("args", e.args, str(e), repr(e))
print("exc-attrs", ValueError().args, str(ValueError()), repr(KeyError("k")), str(KeyError("k")), str(KeyError()), IndexError("i").args)


class WithNotes(Exception):
    def __str__(self):
        return f"custom({', '.join(map(str, self.args))})"


try:
    raise WithNotes(1, 2)
except WithNotes as e:
    print("custom-str", e, repr(e), e.args)
total = 0
for i in range(50):
    try:
        try:
            if i % 3 == 0:
                raise ValueError
        finally:
            total += 1
    except ValueError:
        total += 10
print("nested-hot", total)
