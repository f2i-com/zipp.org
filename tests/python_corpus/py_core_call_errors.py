# Pending divergences split out of tests/python_corpus/call_errors.py: TypeError messages.
# CPython uses the function's __qualname__ (K.m, K.__init__, outer.<locals>.<lambda>), "from N to M positional
# arguments" with defaults, a dedicated positional-only-as-keyword message, and "__main__.f() argument after *"
# messages for bad star arguments.
def dflt(a, b=1):
    return (a, b)


def posonly(a, b, /, c):
    return (a, b, c)


def pos(a, b, c):
    return (a, b, c)


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
    ("too-many-dflt", lambda: dflt(1, 2, 3)),
    ("posonly-as-kw", lambda: posonly(1, b=2, c=3)),
    ("method-missing", lambda: K(1).m()),
    ("method-too-many", lambda: K(1).m(1, 2)),
    ("unbound-method", lambda: K.m(1)),
    ("static-missing", lambda: K.s()),
    ("class-too-many", lambda: K.c(1, 2)),
    ("init-missing", lambda: K()),
    ("init-too-many", lambda: K(1, 2)),
    ("init-kw", lambda: K(w=1)),
    ("lambda-missing", lambda: (lambda x, y: 0)(1)),
    ("lambda-too-many", lambda: (lambda: 0)(1, 2)),
    ("builtin-isinstance", lambda: isinstance(1)),
]
for name, thunk in cases:
    try:
        thunk()
        print(name, "no error")
    except TypeError as e:
        print(name, "TypeError:", e)


def outer():
    def inner(q):
        return q
    return inner


try:
    outer()()
except TypeError as e:
    print("nested-qualname", "TypeError:", e)
