# float arithmetic edge cases: remainder signs, CPython's fmod-based floor
# division, pow special cases, int/float comparison exactness, int -> float
# overflow, and NaN identity inside containers.
import math

inf = float("inf")
nan = float("nan")

# A zero remainder takes the sign of the divisor; -0.0 % positive is 0.0.
print("mod-zero-sign", -5.5 % 0.5, -7.0 % 0.5, -11.0 % 0.5, -0.0 % 1, -0.0 % 2.0, -4.0 % 2, -0.0 % 3, 5.5 % -0.5, 0.0 % -2.0)
print("mod-signs", 7.5 % 2, -7.5 % 2, 7.5 % -2, -7.5 % -2, 1e-300 % 1e300, -1e-300 % 1e300)

# Floor division by an infinity with opposite signs is -1.0; inf // finite is nan.
print("floordiv-inf", 5.5 // -inf, -5.5 // inf, -5 // inf, 5 // -inf, divmod(-5, inf), 1e300 // -inf, 0.0 // inf, 5.0 // inf)
print("inf-floordiv", inf // 2.0, -inf // 2.0, inf // -3, divmod(inf, 2.0), inf % 2.0, 2.0 % inf, -2.0 % inf)

# ZeroDivisionError messages for float /, //, % and divmod.
for thunk in (lambda: 1.0 % 0, lambda: 1.0 % 0.0, lambda: divmod(1.0, 0.0), lambda: divmod(1, 0.0),
              lambda: 1.0 // 0.0, lambda: 1 // 0.0, lambda: 1.0 / 0, lambda: 1 / 0, lambda: divmod(1, 0), lambda: 5 % 0):
    try:
        thunk()
    except ZeroDivisionError as e:
        print("ZeroDivisionError", e)

# float // follows CPython's float_divmod (floor((a - fmod(a, b)) / b) with a correction), not floor(a / b).
print("floordiv-algo", -7.0 // 0.7, divmod(-7.0, 0.7), 5.5 // 1e-3, 7.0 // 1e-3, divmod(5.5, 1e-3), 1.0 // 0.1, divmod(1.0, 0.1), 0.3 // 0.1, -0.001 % 1e-3)
print("floordiv-consistent", 7.0 // 0.1, 7.0 % 0.1, (7.0 // 0.1) * 0.1 + 7.0 % 0.1 == 7.0, divmod(-0.5, 0.25), divmod(0.5, -0.25), -0.0 // 1.0, 0.0 // -1.0)

# pow special cases: 1 ** nan, 1.0 ** inf and (-1.0) ** inf are 1.0.
print("pow-special", 1 ** nan, 1.0 ** nan, 1.0 ** inf, 1.0 ** -inf, (-1.0) ** inf, (-1.0) ** -inf, nan ** 0, nan ** 0.0)
print("pow-inf", 2.0 ** inf, 0.5 ** inf, 2.0 ** -inf, 0.5 ** -inf, inf ** 2, inf ** -1, (-inf) ** 3, (-inf) ** 2, (-inf) ** -3, 0.0 ** 0.0)
print("pow-negbase", (-8.0) ** 3, (-2.0) ** -2, (-0.0) ** 3, (-0.0) ** 2.0, 0.0 ** 5)
for thunk in (lambda: 2.0 ** 10000, lambda: 10.0 ** 400, lambda: (-10.0) ** 401, lambda: 0.0 ** -1.5, lambda: (-0.0) ** -3):
    try:
        print("pow-error", thunk())
    except (OverflowError, ZeroDivisionError) as e:
        print(type(e).__name__, e)

x = nan
print("nan-operator", nan == nan, nan != nan, x == x, x is x, nan < nan, nan >= 1, 1 <= nan)
# Distinct NaNs are distinct keys and never equal (CPython compares elements
# by identity first, so only the SAME object finds itself; Zipp's floats are
# unboxed and have no identity, which is documented in
# docs/PYTHON_FRONTEND_EXPERIMENT.md).
a, b = float("nan"), float("nan")
print("nan-distinct", a in [b], [a] == [b], len({a: 1, b: 2}), len({a, b}), {a: 1}.get(b, "missing"), [a].count(b))

# inf compared with an int too large for a float.
print("inf-vs-bigint", inf > 10**400, -inf < -10**400, 10**400 < inf, inf == 10**400, float("-inf") > -10**400, 10**400 != inf, -10**400 > -inf)

# int/float comparison is exact, not via float(int).
print("exact-cmp", 2**53 + 1 == 2**53 + 1.0, 2**53 + 1 > 2.0**53, 10**23 == 1e23, 9007199254740993 > 9007199254740992.0,
      2**63 - 1 < 9.223372036854776e18, 2**53 + 1 != float(2**53 + 1), 10**16 + 1 > 1e16, 10**16 + 1 == 1e16)
print("exact-cmp-mixed", 1 == 1.0, True == 1.0, 0.5 < True, False <= -0.0, 3 >= 2.9999999999999996, 2**1000 > 1e300, 2**1024 > 1.7976931348623157e308)
print("sorted-mixed", sorted([2**53 + 1, float(2**53), 2**53 - 1, 9007199254740992.5]), max(2**53 + 1, float(2**53)), min(1e16, 10**16 + 1))
print("mixed-set", len({2**53, float(2**53), 2**53 + 1}), float(2**53) in {2**53}, 2**53 + 1 in {float(2**53)}, {1.0: "a", 1: "b"})

# OverflowError for int -> float conversion.
for thunk in (lambda: float(10**400), lambda: 10**400 / 1, lambda: 10**400 * 1.0, lambda: 1.0 + 10**400, lambda: 10**400 - 0.5,
              lambda: math.sqrt(10**400), lambda: 10**400 ** 0.5, lambda: float(-(10**400)), lambda: 2**1024 * 1.0):
    try:
        print("overflow", thunk())
    except OverflowError as e:
        print("OverflowError", e)
print("no-overflow", 1 / 10**400, 10**400 // 1.5 if False else "skip", float(2**1023), math.log2(2**2000), math.log10(10**400), math.log(10**400) > 921)

# Floats that print via repr in containers and with round().
print("round", round(2.5), round(3.5), round(-2.5), round(0.125, 2), round(0.375, 2), round(1.5e300), round(7.25, -1), round(12345, -2), round(-0.5))
print("float-int-ops", 3 * 0.1, 0.1 + 0.2, 1e16 + 1, 2**53 + 1.0, 7 // 2.0, -7 // 2.0, 7 % -2.0, 2 ** 0.5, 10 / 4)
