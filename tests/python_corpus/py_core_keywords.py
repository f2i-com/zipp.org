# Keyword binding: parameters looked up per name, extras for **kwargs in call
# order, and the error CPython names for the first offending keyword.
def f(a, b=2, *, c=3): return (a, b, c)
def g(a, /, b, **kw): return (a, b, kw)
def h(*args, x, **kw): return (args, x, kw)
def k(a, b, c=0, *rest, d, e=5, **kw): return (a, b, c, rest, d, e, kw)
cases = [
    lambda: f(1, b=5), lambda: f(a=1), lambda: f(1, c=9, b=8), lambda: f(b=1, a=2),
    lambda: f(1, a=2, zzz=3), lambda: f(1, zzz=3, a=2), lambda: f(1, 2, 3), lambda: f(c=1),
    lambda: g(1, b=2, a=3, z=4), lambda: g(1, 2, y=1, x=2), lambda: g(a=1, b=2),
    lambda: h(1, 2, x=3, q=4, p=5), lambda: h(x=1), lambda: h(1),
    lambda: k(1, 2, 3, 4, 5, d=6, z=7, e=8), lambda: k(1, 2, d=0), lambda: k(1, d=0, b=2, c=3, w=1),
    lambda: k(1, 2, 3, b=4, d=1), lambda: f(**{"a": 1, "b": 2}), lambda: f(1, **{"c": 4}),
]
for i, c in enumerate(cases):
    try:
        print(i, c())
    except TypeError as e:
        print(i, "TypeError", str(e).replace("<lambda>.", ""))
