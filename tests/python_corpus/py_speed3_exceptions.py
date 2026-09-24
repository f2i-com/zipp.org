# Missing keys and the exceptions the runtime makes: exact dicts with str,
# int, float, bool, tuple and None keys (hits and misses), dict subclasses
# with and without __missing__, KeyError args/str/repr, __context__ of an
# exception raised while another is handled (nested handlers, finally,
# generators), exceptions made with no, one and several args, attributes set
# on caught exceptions, and runtime errors (IndexError, ZeroDivisionError,
# AttributeError, TypeError) caught in loops.


def lookup(d, k):
    try:
        return "hit:%r" % (d[k],)
    except KeyError as e:
        return "miss:%r:%r:%s:%r" % (e.args, e, e, type(e).__name__)


words = {"a": 1, "b": 2}
ints = {1: "one", 2: "two", -3: "minus", 2 ** 60: "big"}
mixed = {1.5: "f", True: "t", None: "n", (1, 2): "tup", "s": "str"}
for k in ["a", "b", "c", "", "A"]:
    print(lookup(words, k))
for k in [1, 2, 3, -3, 2 ** 60, 2 ** 60 + 1, 1.0, True, 0]:
    print(lookup(ints, k))
for k in [1.5, 1, True, None, (1, 2), (2, 1), "s", "t", 0.0]:
    print(lookup(mixed, k))


class Defaulting(dict):
    def __missing__(self, key):
        return "default-%s" % (key,)


class Plain(dict):
    pass


class Loud(dict):
    def __missing__(self, key):
        raise LookupError("no %s here" % (key,))


dd = Defaulting(x=1)
print(dd["x"], dd["y"], dd[3], "y" in dd)
pd = Plain(x=1)
print(lookup(pd, "x"), lookup(pd, "y"))
try:
    Loud()["q"]
except LookupError as e:
    print(type(e).__name__, e, e.__context__)


def context_chain():
    out = []
    try:
        try:
            {}["first"]
        except KeyError:
            {}["second"]
    except KeyError as e:
        out.append((e.args, e.__context__.args, e.__context__.__context__, e.__cause__, e.__suppress_context__))
    try:
        try:
            raise ValueError("v")
        except ValueError:
            try:
                {}["inner"]
            except KeyError as inner:
                out.append(("inner ctx", inner.__context__.args))
            {}["after"]
    except KeyError as e:
        out.append(("after ctx", e.__context__.args))
    try:
        try:
            pass
        finally:
            {}["in-finally"]
    except KeyError as e:
        out.append(("finally ctx", e.__context__))
    try:
        try:
            raise TypeError("t")
        finally:
            {}["finally-propagating"]
    except KeyError as e:
        out.append(("finally-propagating ctx", repr(e.__context__)))
    try:
        {}["fresh"]
    except KeyError as e:
        out.append(("fresh ctx", e.__context__))
    return out


for item in context_chain():
    print(item)


def gen_keys(d, keys):
    for k in keys:
        try:
            yield d[k]
        except KeyError as e:
            yield "gen-miss %r ctx=%r" % (e.args, e.__context__)


print(list(gen_keys({"a": 1}, ["a", "b", "a", "c"])))
count = 0
d = {"present": 1}
for i in range(300):
    try:
        d["absent"]
    except KeyError:
        count += 1
    except Exception:
        count -= 1000
print("caught", count)
errs = []
for i in range(6):
    try:
        if i == 0:
            [][i]
        elif i == 1:
            1 // (i - 1)
        elif i == 2:
            None.attr
        elif i == 3:
            "a" + 1
        elif i == 4:
            {}[i]
        else:
            {"k": 1}["k"] + None
    except (IndexError, ZeroDivisionError, AttributeError, TypeError, KeyError) as e:
        errs.append((type(e).__name__, str(e), len(e.args)))
print(errs)
try:
    {}["k"]
except KeyError as e:
    e.note = "attached"
    print(e.note, e.args, str(e), repr(e))
print(repr(KeyError()), repr(KeyError(1, 2)), str(KeyError(1, 2)), KeyError("x").args)
print(repr(Exception()), str(Exception("a", "b")), repr(ValueError("v")))
e1 = KeyError("same")
try:
    raise e1
except KeyError as e:
    print(e is e1, e.__context__)


def nested(n):
    if n == 0:
        return {}["deep"]
    return nested(n - 1)


try:
    nested(5)
except KeyError as e:
    print("deep", e.args)
try:
    try:
        {}["x"]
    except KeyError:
        raise
except KeyError as e:
    print("reraise", e.args, e.__context__)
try:
    try:
        {}["x"]
    except KeyError as k:
        raise RuntimeError("wrapped") from k
except RuntimeError as e:
    print(repr(e.__cause__), repr(e.__context__), e.__suppress_context__)
m = {}
for i in range(5):
    m[i * 2] = i
print([m.get(i, "-") for i in range(10)], [lookup(m, i) for i in (4, 5)])
