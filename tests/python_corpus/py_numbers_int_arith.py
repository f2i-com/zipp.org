# int and bool arithmetic edge cases: bool bit operators, negative powers,
# modular inverses, shifts, int() parsing, and huge-int limits.

# bool & | ^ bool stays a bool; mixed with an int it is an int.
print("bool-bitops", True & True, True | False, True ^ True, False ^ False,
      type(True & False).__name__, type(False | False).__name__, type(True ^ False).__name__)
print("bool-int-bitops", False & 1, True & 3, True | 2, type(True & 1).__name__, ~False, ~True, -True)
ok = True
for flag in [True, False, True]:
    ok &= flag
found = False
for flag in [False, True]:
    found |= flag
flip = True
flip ^= True
print("bool-aug", ok, found, flip, type(ok).__name__, type(found).__name__, type(flip).__name__)

# 0 ** negative int raises ZeroDivisionError (via the float power).
for thunk in (lambda: 0 ** -1, lambda: 0 ** -5, lambda: pow(0, -2), lambda: 0.0 ** -1):
    try:
        print("zero-neg-pow", thunk())
    except ZeroDivisionError as e:
        print("ZeroDivisionError", e)
print("neg-pow", 2 ** -1, (-2) ** -3, 10 ** -2, type(4 ** -1).__name__, 1 ** -100, (-1) ** -3)

# pow with a negative exponent and a modulus is the modular inverse.
print("modinv", pow(3, -1, 7), pow(38, -1, 97), pow(2, -3, 11), pow(3, -2, 7), pow(-3, -1, 7))
print("modpow", pow(2, 10, 1000), pow(-2, 3, 5), pow(2, 3, -5), pow(0, 0, 3), pow(5, 0, 1), pow(7, 2, -1))
for args in [(2, -1, 4), (0, -1, 5), (6, -1, 9)]:
    try:
        pow(*args)
    except ValueError as e:
        print("ValueError", e)
try:
    pow(2, 3, 0)
except ValueError as e:
    print("ValueError", e)
try:
    pow(2.0, 3, 5)
except TypeError as e:
    print("TypeError", e)

# Shifts, including past the operand's width.
print("shifts", 1 << 100, -1 >> 200, 5 >> 200, (1 << 64) >> 63, -(1 << 70) >> 3, 0 << 10**7)
try:
    1 << -1
except ValueError as e:
    print("ValueError", e)

# Exact int / int true division, and big quotients.
print("truediv", 10**30 / 10**10, (2**80 + 1) / 2**27, -7 / 2, 7 / -2, (10**400) / (10**399))
try:
    10**400 / 3
except OverflowError as e:
    print("OverflowError", e)

# int() parsing: underscores only between digits, base prefixes, base 0 rules.
for text, base in [("1_000", 10), ("0x_ff", 16), ("0b1_01", 0), ("0o17", 0), ("0", 0), ("000", 0),
                   ("  -42  ", 10), ("+7", 10), ("z", 36), ("Zz", 36), ("0XAB", 0)]:
    print("int-parse", repr(text), base, int(text, base))
for text, base in [("08", 0), ("1__0", 10), ("_1", 10), ("1_", 10), ("0x", 16), ("", 10), ("12a", 10),
                   ("0b2", 0), ("9", 8), ("- 1", 10)]:
    try:
        print("int-parse", int(text, base))
    except ValueError as e:
        print("ValueError", e)
try:
    int("10", 1)
except ValueError as e:
    print("ValueError", e)
for text in ["1_0.5", "1e1_0", ".5", "5.", "-1_000.25e-2"]:
    print("float-parse", repr(text), float(text))
for text in ["1__0.5", "_1.0", "1._5", "1.0_", "1e", "e5", "."]:
    try:
        print("float-parse", float(text))
    except ValueError as e:
        print("ValueError", e)

# Arithmetic that crosses 2**53 stays exact for ints.
big = 2**53
print("exact-int", big + 1, (big + 1) * 3, (big + 1) // 3, (big + 1) % 7, -(big + 1) // 3, divmod(-(big + 1), 7))
print("bit-length", (1 << 3000).bit_length(), (10**300).bit_length(), (-(2**100)).bit_length())
print("sum-int", sum([2**40, -2**41, 3]), sum([-(2**33)] * 3), sum(x * x for x in range(1000)), sum([2**64, 2**64], -1), sum(range(5), 2**70))

# Ints past 2**20 bits: every path is bounded by the same (engine) limit.
x = (1 << 3000000) - 1
print("big-shift", x.bit_length(), (x * 3).bit_length(), (x - True).bit_length(), (x << 1).bit_length(), sum([x, x]).bit_length())
print("big-pow", (2 ** 1100000).bit_length(), pow(3, 700000).bit_length(), (10 ** 1000000).bit_length(), divmod((1 << 2000000) - 1, (1 << 1000000) + 1)[1])
print("huge-counts", -1 >> 10**30, 5 >> 10**30, 0 << 10**30, 0 ** (10**30), 1 ** (10**30), (-1) ** (10**30 + 1), (-5) >> 3, 1 << 0)
