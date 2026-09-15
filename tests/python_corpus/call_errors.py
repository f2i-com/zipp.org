# Argument binding errors and their messages, for functions, methods, lambdas,
# constructors and builtins; calling non-callables.
def pos(a, b, c):
    return (a, b, c)


def dflt(a, b=1):
    return (a, b)


def kwo(a, *, k):
    return (a, k)


def kwo2(*, x, y, z=0):
    return (x, y, z)


def posonly(a, b, /, c):
    return (a, b, c)


def noargs():
    return None


def varargs(a, *args):
    return (a, args)


def varkw(a, **kw):
    return (a, kw)


class K:
    def m(self, x):
        return x

    @staticmethod
    def s(x):
        return x

    @classmethod
    def c(cls, x):
        return x

    def __init__(self, v):
        self.v = v


cases = [
    ("missing-1", lambda: pos(1, 2)),
    ("missing-2", lambda: pos(1)),
    ("missing-3", lambda: pos()),
    ("too-many", lambda: pos(1, 2, 3, 4)),
    ("too-many-none", lambda: noargs(1)),
    ("dup-kw", lambda: pos(1, 2, 3, a=4)),
    ("dup-kw-2", lambda: dflt(1, a=2)),
    ("unexpected-kw", lambda: pos(1, 2, 3, d=4)),
    ("unexpected-kw-none", lambda: noargs(x=1)),
    ("kwo-missing", lambda: kwo(1)),
    ("kwo-missing-2", lambda: kwo2()),
    ("kwo-missing-1", lambda: kwo2(x=1)),
    ("kwo-positional", lambda: kwo(1, 2)),
    ("kwo2-positional", lambda: kwo2(1, 2)),
    ("posonly-missing", lambda: posonly(1)),
    ("varargs-missing", lambda: varargs()),
    ("varkw-positional", lambda: varkw(1, 2)),
    ("varkw-dup", lambda: varkw(1, a=2)),
    ("call-int", lambda: 5()),
    ("call-str", lambda: "abc"(1)),
    ("call-none", lambda: None()),
    ("call-list", lambda: [1, 2](0)),
    ("call-instance", lambda: K(1)()),
    ("builtin-len", lambda: len()),
    ("builtin-len-2", lambda: len([], [])),
    ("builtin-abs", lambda: abs("x")),
    ("builtin-int", lambda: int([])),
    ("object-init", lambda: object(1)),
]
for name, thunk in cases:
    try:
        thunk()
        print(name, "no error")
    except TypeError as e:
        print(name, "TypeError:", e)

errors = 0
for i in range(20):
    try:
        if i % 3 == 0:
            pos(i)
        elif i % 3 == 1:
            kwo(i)
        else:
            dflt(i, i, i)
    except TypeError:
        errors += 1
print("loop-errors", errors)


def safe_call(fn, *args, **kwargs):
    try:
        return ("ok", fn(*args, **kwargs))
    except TypeError as e:
        return ("err", str(e).split("(")[0])


print("safe", safe_call(pos, 1, 2, 3), safe_call(pos, 1), safe_call(kwo, 1, k=2), safe_call(kwo2, y=1, x=2))
