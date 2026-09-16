# Float arithmetic and comparison: NaN, infinities, signed zero, mixed int/float,
# float floor division and modulo, division errors, int/float equality.
inf = float("inf")
nan = float("nan")
vals = [0.0, -0.0, 1.0, -1.0, 0.5, -2.5, 3.75, 1e-300, -1e300, 1e308, inf, -inf, 2.0**53, 7.0]

for a in vals:
    print("unary", a, -a, +a, abs(a), a == -a, a + 0.0, a - a if a == a and abs(a) != inf else "skip", a * 2, a / 4)

for a in [5.5, -5.5, 7.0, -7.0, 0.0, 1e300, 123.456, -0.001]:
    row = []
    for b in [2.0, -2.0, 0.75, -0.3, 3, -3, 1.5]:
        row.append((a // b, a % b))
    print("floordiv", a, row)
print("floordiv-inf", 5.5 // inf, -5.5 // -inf, 5.5 % inf, -5.5 % -inf, 0.0 // inf, 5 // inf, 5 % inf, -5 % -inf)

for a, b in [(7, 2.0), (-7, 2.0), (7.5, 2), (-7.5, -2), (1, 3.0), (2**53, 1.0), (10**20, 3.0), (5, inf), (-5, -inf), (-3, 0.7)]:
    print("mixed", a, b, a + b, a - b, a * b, a / b, a // b, a % b, divmod(a, b))

for thunk in [lambda: 1.0 / 0, lambda: 1 / 0.0, lambda: 0.0 / 0, lambda: 5 / 0, lambda: 2**70 / 0, lambda: 1.0 // 0]:
    try:
        print("zero", thunk())
    except ZeroDivisionError as e:
        print("ZeroDivisionError", e)

print("true-div", 7 / 2, -7 / 2, 1 / 3, 2**53 / 3, 10**30 / 10**10, (2**63 + 1) / 2, 1 / 2**60, -(10**20) / 7, 3 / -0.5)
print("pow", 2.0 ** 10, 2 ** 0.5, (-8.0) ** 3, 4.0 ** -0.5, 10.0 ** -3, 0.0 ** 0, inf ** 0, nan ** 0, 0.5 ** inf, 2.0 ** -inf, 2 ** 0.0, 9 ** 0.5, 2.0 ** 1023)
print("nan", nan == nan, nan != nan, nan < 1.0, nan > 1.0, nan <= nan, nan >= 0, 1.0 == nan, nan + 1, nan * 0, -nan, abs(nan))
x = nan
print("nan-identity", x is x, x == x, x != x, x is not nan if False else "skip")
print("inf", inf + 1, inf - inf, inf * 0, inf * -1, -inf < -1e308, inf > 10**300, inf == inf, inf / inf, 1 / inf, -1 / inf, inf > 2**1000)
print("signed-zero", 0.0 == -0.0, -0.0 < 0.0, str(-0.0), repr(-0.0), -0.0 + 0.0, -0.0 - 0.0, -0.0 * -1, 0.0 / -1, -0.0 * 1, round(-0.4), round(-0.5), -0.0 // 1, 0.0 % -1, abs(-0.0))
print("exact-cmp", 2**53 == 2.0**53, 10**20 == 1e20, 10**400 > 1e308, -10**400 < -1e308, 2**1024 > 1.7976931348623157e308, 1 < 1.0000000000000002, 2**63 == 9.223372036854776e18)
print("int-float-eq", 1 == 1.0, -1 == -1.0, 0 == -0.0, True == 1.0, 3 != 3.0, 10**16 == 1e16, [1, 2.0] == [1.0, 2], (0,) == (-0.0,), {1: "a"}[1.0], 2.5 in [1, 2.5], 3 in [3.0])
for thunk in [lambda: int(inf), lambda: int(-inf), lambda: int(nan)]:
    try:
        print("conv-err", thunk())
    except (OverflowError, ValueError) as e:
        print(type(e).__name__, e)

print("conv", float(7), float(-2**63), float(2**53 + 1), float("  -1.5e3 "), float("-inf"), float("nan") != float("nan"), float("1_0.5"), int(1e18), int(-1e18), int(1.9999999999999998), int(2.5e-300))
print("is_integer", (1.0).is_integer(), (1.5).is_integer(), inf.is_integer(), (-0.0).is_integer(), float(2**60).is_integer())
print("compare-chain", 0.1 + 0.2 > 0.3, 0.1 + 0.2 == 0.30000000000000004, 1.0 < 2 < 3.5 <= 3.5, -inf < -1 < 0.0 < 1e-300 < 1 < inf)
print("minmax", max(1, 1.0), max(1.0, 1), min(0.0, -0.0), min(-0.0, 0.0), max(2, 2.5, 2), min([3.5, 2, 2.0]), sum([0.1] * 10), sum([1, 2.5, 3]))

total = 0.0
for i in range(1, 200):
    total += 1.0 / i
    total -= i % 3 * 0.001
    if total > 3.0:
        total *= 0.5
print("harmonic", total, round(total, 10), total // 0.1, total % 0.1)
xs = [((i * 7919) % 101 - 50) / 8.0 for i in range(40)]
m = sum(xs) / len(xs)
var = sum((v - m) ** 2 for v in xs) / len(xs)
print("stats", m, var, var ** 0.5, min(xs), max(xs), sorted(xs)[:5])
h = 0
for v in xs:
    h = (h * 31 + int(v * 1000)) % 1000000007
print("hash-like", h)
