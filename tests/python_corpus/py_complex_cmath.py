# cmath against CPython: every function over a grid of real/imaginary parts
# that includes both signed zeros, infinities and NaN (the special-value
# tables and branch cuts), large and tiny arguments (the overflow-avoiding
# paths), errors, the argument protocols, and the constants. Values print
# to 12 significant digits (the libm last bit may differ); special values
# print exactly.
import cmath
import math

inf = float("inf")
nan = float("nan")


def f(x):
    return "%.12g" % x


def c(z):
    return "(" + f(z.real) + "," + f(z.imag) + ")"


def call(fn, *args):
    try:
        r = fn(*args)
    except (ValueError, OverflowError, TypeError, ZeroDivisionError) as e:
        return type(e).__name__
    if isinstance(r, complex):
        return c(r)
    if isinstance(r, tuple):
        return "(" + ",".join(f(x) for x in r) + ")"
    if isinstance(r, float):
        return f(r)
    return repr(r)


names = ["acos", "acosh", "asin", "asinh", "atan", "atanh", "cos", "cosh", "exp", "log", "log10", "sin", "sinh", "sqrt", "tan", "tanh",
         "phase", "polar", "isfinite", "isinf", "isnan"]
grid = [-inf, -2.5, -1.0, -0.0, 0.0, 0.5, 1.0, inf, nan]
print("-- special-value grid")
for name in names:
    fn = getattr(cmath, name)
    for re in grid:
        print(name, f(re), " ".join(call(fn, complex(re, im)) for im in grid))
print("-- rect grid")
for r in grid:
    print("rect", f(r), " ".join(call(cmath.rect, r, phi) for phi in grid + [2.0, -2.0, math.pi]))
print("-- exact special values")
for z in [complex(inf, inf), complex(-inf, 0.0), complex(0.0, -0.0), complex(-0.0, -0.0), complex(1.0, 0.0), complex(-1.0, -0.0),
          complex(nan, inf), complex(0.0, inf)]:
    out = []
    for name in ["acos", "acosh", "asinh", "atanh", "log", "sqrt", "exp", "tanh"]:
        try:
            out.append(repr(getattr(cmath, name)(z)))
        except (ValueError, OverflowError) as e:
            out.append(type(e).__name__)
    print(repr(z), " ".join(out))
print(repr(cmath.phase(-1)), repr(cmath.phase(complex(-1, -0.0))), repr(cmath.phase(-0.0)), repr(cmath.phase(1j)), cmath.polar(-2),
      cmath.polar(complex(-0.0, -0.0)), cmath.sqrt(-4), cmath.sqrt(complex(-4, -0.0)), cmath.sqrt(0j), cmath.sqrt(-0j), cmath.rect(3, 0),
      cmath.rect(-3, -0.0))
print("-- large, tiny and ordinary arguments")
special = [complex(1e308, 1e308), complex(-1e308, 1e-308), complex(1e200, -1e200), complex(3e153, 1.0), complex(1e-310, 1e-310),
           complex(-5e-324, 0.0), complex(1e-320, -1e-320), complex(710, 1), complex(-710, 1), complex(710, 0), complex(700, -2),
           complex(1, 1e-160), complex(1, -1e-200), complex(-1, 1e-200), complex(0.8, 0.6), complex(1.2, 0.5), complex(3, 4),
           complex(-2, 3), complex(0.1, -0.2), complex(20, 30), complex(-0.5, 1e6), complex(1e-8, 1e-8)]
for name in names:
    fn = getattr(cmath, name)
    print(name, " ".join(call(fn, z) for z in special))
print("-- log bases and errors")
print(call(cmath.log, 8, 2), call(cmath.log, 1j, 1j), call(cmath.log, -1, 10), call(cmath.log, 0, 2), call(cmath.log, 1, 1),
      call(cmath.log, 0), call(cmath.log, 2, 0), call(cmath.log, 100, 10.0), call(cmath.log10, 0), call(cmath.log, 1e-320),
      call(cmath.log, 5, None))
for z in [1, -1, 1j, -1j, 1 + 0j, complex(1, -0.0), complex(-1, -0.0)]:
    print(repr(z), call(cmath.atanh, z), call(cmath.atan, z))
print(call(cmath.exp, 1000), call(cmath.exp, complex(1000, 0)), call(cmath.cosh, 1000), call(cmath.sinh, complex(-1000, 1)),
      call(cmath.cos, 1000j), call(cmath.polar, complex(1e308, 1e308)), call(cmath.rect, 1e308, 0.5), call(cmath.rect, inf, inf))
print("-- argument protocols")


class Cx:
    def __complex__(self):
        return 3 + 4j


class Fl:
    def __float__(self):
        return 0.25


class Ix:
    def __index__(self):
        return 2


class BadCx:
    def __complex__(self):
        return 5


class Sub(complex):
    pass


for arg in [Cx(), Fl(), Ix(), Sub(1, 1), True, 7, 2.5, 10 ** 400, "1", None, BadCx(), [1]]:
    print(type(arg).__name__, call(cmath.sqrt, arg), call(cmath.phase, arg), call(cmath.isnan, arg), call(cmath.rect, arg, 0))
print(call(cmath.rect, 1j, 0), call(cmath.rect, 1, 1j), call(cmath.log, 4, Cx()))
print("-- isclose")
for a, b, kw in [(1, 1, {}), (1 + 1j, 1 + 1.0000000001j, {}), (1j, 1.0001j, {}), (1j, 1.0001j, {"rel_tol": 1e-3}), (0j, 1e-10j, {}),
                 (0j, 1e-10j, {"abs_tol": 1e-9}), (complex(inf, 1), complex(inf, 1), {}), (complex(inf, 1), complex(inf, 2), {}),
                 (complex(nan, 0), complex(nan, 0), {}), (1, 1j, {"rel_tol": 2.0}), (inf, -inf, {"rel_tol": 1.0}),
                 (1, 2, {"rel_tol": -1}), (1, 2, {"abs_tol": -1.0}), (Cx(), 3 + 4j, {}), (Fl(), 0.25, {"abs_tol": 0})]:
    show = [repr(v) if isinstance(v, (int, float, complex)) else type(v).__name__ for v in (a, b)]
    print(show, kw, call(lambda: cmath.isclose(a, b, **kw)))
print("-- constants")
print(cmath.pi, cmath.e, cmath.tau, cmath.inf, cmath.infj, cmath.nan, cmath.nanj, type(cmath.nanj).__name__, cmath.pi == math.pi)
print(abs(cmath.exp(1j * cmath.pi) + 1) < 1e-15, abs(cmath.exp(2j) ** 2 - cmath.exp(4j)) < 1e-15)
print(sorted(n for n in dir(cmath) if not n.startswith("_")))
