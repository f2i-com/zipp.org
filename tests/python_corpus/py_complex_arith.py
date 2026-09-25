# complex arithmetic: + - * / ** with int/float/bool/complex operands (both
# sides), signed zeros, infinities and NaNs, division by zero, the power
# algorithm's exact small integer exponents, unary operators, abs, and the
# operations complex refuses (// % divmod round int float ordering math).
import math

inf = float("inf")
nan = float("nan")
operands = [0, 1, -3, True, False, 2.5, -0.0, 0.0, 1e308, inf, -inf, nan, 0j, -0j, complex(-0.0, 0.0), 1j, -2j, 1 + 1j,
            3 - 4j, complex(inf, 0), complex(0, inf), complex(inf, inf), complex(nan, 1), complex(1e300, 1e300), complex(1e-320, 5e-324)]


def show(f):
    try:
        return repr(f())
    except (ZeroDivisionError, OverflowError, ValueError, TypeError) as e:
        return type(e).__name__ + ": " + str(e)


print("-- binary operators")
for a in operands:
    for b in operands:
        if not isinstance(a, complex) and not isinstance(b, complex):
            continue
        print(repr(a), repr(b), show(lambda: a + b), show(lambda: a - b), show(lambda: a * b), show(lambda: a / b))

print("-- powers")
bases = [0j, -0j, 1j, -1j, 1 + 1j, 2 - 3j, 0.5 + 0.5j, -2 + 0j, complex(1e200, 1e200), complex(inf, 0), complex(nan, 0), 2, -2, 0, 2.5, 0.0]
exponents = [0, 1, 2, 3, -1, -2, 10, 100, 101, -100, 0.5, -0.5, 1j, -1j, 2 + 1j, 0j, 1.0, 2.0, 100.0, 1e20, complex(2, 0), complex(2, -0.0),
             complex(inf, 0), complex(nan, 0)]
def r12(v):
    # Twelve significant digits: pow's transcendental paths come from the
    # platform libm, which can differ from CPython's in the last bit.
    if isinstance(v, complex):
        return complex(float(f"{v.real:.12g}"), float(f"{v.imag:.12g}"))
    return v

for a in bases:
    for b in exponents:
        if not isinstance(a, complex) and not isinstance(b, complex):
            continue
        print(repr(a), "**", repr(b), show(lambda: r12(a ** b)))
print(show(lambda: pow(1j, 2)), show(lambda: pow(1j, 2, 3)), show(lambda: pow(2, 1j, 3)), show(lambda: pow(2, 3, 1j)), show(lambda: pow(1j, 2, None)))
print(show(lambda: (1 + 1j) ** 50), show(lambda: (1 + 1j) ** -50), show(lambda: (1.0000001 + 0j) ** 1e10), show(lambda: complex(1e300, 0) ** 2))

print("-- mixed and in-place")
z = 2 + 3j
z += 1
print(z)
z -= 1j
print(z)
z *= 2
print(z)
z /= 4
print(z)
z **= 2
print(z)
z += True
print(z, type(z).__name__)
x = 5
x += 2j
print(x)
x = 1.5
x *= 1j
print(x)
print(1 + 2j * 3 - 4 / 2j, (1 + 2j) * (1 - 2j), 2j ** 2, (-1) ** 0.5, 1j * 1j, (1 + 1j) / 2, 10 / (1 + 1j), (1 + 1j) / 1e-320)
print(True + 1j, False * 1j, 1j - True, 2 ** 1j, 1j ** True, 0.5 ** (1 + 1j), (-8) ** (1 / 3 + 0j))
print(10 ** 20 * 1j, 1j * 2 ** 60, (2 ** 53 + 1) + 0j, 1j / 10 ** 30)

print("-- unary and abs")
for v in [0j, -0j, 1 + 2j, complex(-0.0, 0.0), complex(inf, nan), complex(nan, -inf), complex(nan, 2), 3 - 4j, complex(1e308, 1e308),
          complex(5e-324, 5e-324), complex(-1.5, 2.5)]:
    print(repr(v), -v, +v, show(lambda: abs(v)), show(lambda: ~v), v == +v)

print("-- refused operations")
for f in [lambda: 1j // 1, lambda: 1 // 1j, lambda: 1j % 2, lambda: 2.0 % 1j, lambda: divmod(1j, 1), lambda: divmod(1, 1j),
          lambda: divmod(1j, 1j), lambda: 1j & 1, lambda: 1 | 1j, lambda: 1j ^ 1j, lambda: 1j << 1, lambda: 1 >> 1j, lambda: round(1j),
          lambda: round(1j, 2), lambda: int(1j), lambda: float(1j), lambda: math.sqrt(1j), lambda: math.floor(1j),
          lambda: math.isclose(1j, 1), lambda: math.fsum([1j]), lambda: math.hypot(1j), lambda: 1j + "a", lambda: "a" + 1j,
          lambda: [1] * 1j, lambda: 1j * [1], lambda: "ab" * 2j, lambda: 1j + None, lambda: 1j @ 1j, lambda: 1j < 1,
          lambda: 1j + 10 ** 400, lambda: 10 ** 400 * 1j, lambda: complex(1, 1) / 0, lambda: 1 / 0j, lambda: 0j / 0j,
          lambda: 1.5 / complex(0.0, -0.0), lambda: 0j ** -1, lambda: 0j ** 1j, lambda: 0j ** -2.5, lambda: 0 ** (1j),
          lambda: 0.0 ** -1j]:
    print(show(f))
x = 1j
for f in ["floordiv", "mod"]:
    try:
        if f == "floordiv":
            x //= 2
        else:
            x %= 2
    except TypeError as e:
        print(e)

print("-- sum, math.prod, sorted keys")
print(sum([1j, 2, 3.5]), sum([0.1j] * 10), sum([1, 2j], 0.5), sum([], 0j), sum([1j], start=1j), sum([complex(-0.0, -0.0)], -0.0))
print(sum([1.5, 2.5, 1j, 1e100, -1e100]), sum([True, 1j]), math.prod([1j, 1j, 2]), math.prod([2, 1 + 1j], start=1j))
print(sorted([3 + 1j, 1 - 1j, 2], key=abs), max([1j, -3, 2 + 2j], key=abs), min([1j, 2j], key=lambda c: c.imag))
print(sum(complex(k, -k) for k in range(100)), [z * z for z in [1j, 1 + 1j, 2 - 1j]], list(map(complex, [1, 2.5, "3j"])))
