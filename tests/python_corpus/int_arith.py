# Int arithmetic around the machine-word boundaries a fast path would pick:
# 2**31, 2**53, 2**63, 2**64, i128, floor division and modulo signs, pow, shifts.
edges = [0, 1, -1, 2, -2, 7, -7, 255, 256, -256, 1023, 1024, -257,
         2**31 - 1, 2**31, -2**31, -2**31 - 1, 2**32 - 1, 2**32,
         2**53 - 1, 2**53, 2**53 + 1, -2**53, 2**63 - 1, 2**63, -2**63, -2**63 - 1,
         2**64 - 1, 2**64, 2**127 - 1, 2**127, -2**127, -2**127 - 1, 2**128, 10**40, -10**40]

for a in edges:
    print("add", a, a + 1, a - 1, a + a, -a, +a, abs(a), ~a)

small = [1, -1, 3, -3, 7, -7, 2**31, -2**31, 2**63, -2**63, 2**127, -2**127]
for a in edges:
    row = []
    for b in small:
        row.append((a // b, a % b))
    print("divmod", a, row)

for a in [2**31 - 1, 2**53 + 1, 2**63 - 1, 2**127 - 1, -2**127]:
    print("mul", a * a, a * -1, a * 2, a * 3, -a * a)
    print("floor-neg", a // -1, a % -1, divmod(a, -1), (-a) // -1)

print("minneg", -(-2**127), abs(-2**127), (-2**127) // -1, (-2**127) * -1, divmod(-2**127, -1))
print("maxplus", (2**127 - 1) + 1, 2**63 * 2**63, 2**64 * 2**64, (2**63 - 1) * (2**63 - 1))
print("mods", (2**127) % -3, -(2**127) % 7, (2**64) % 10, -(2**64) % 10, (-7) % 2**64, 7 % -2**64)
print("signs", -7 // 2, -7 % 2, 7 // -2, 7 % -2, -7 // -2, -7 % -2, 0 // -5, 0 % -5, -1 // 10, -1 % 10)

for base in [0, 1, -1, 2, -2, 3, 10, -10]:
    print("pow", base, [base ** e for e in range(0, 12)], base ** 31, base ** 63, base ** 64)
print("pow-neg", 2 ** -1, 2 ** -2, (-2) ** -3, 10 ** -1, 1 ** -5, (-1) ** -3)
print("pow-mod", pow(3, 200, 10**9 + 7), pow(-3, 3, 7), pow(2, 127, 2**127 - 1), pow(7, 0, 5), pow(0, 0))
print("pow-big", 3 ** 100, (-3) ** 101, 2 ** 200 // 3 ** 50, 7 ** 77 % 1000003)

for s in [0, 1, 7, 31, 32, 53, 63, 64, 65, 100, 127, 128, 200]:
    print("shift", s, 1 << s, -1 << s, (1 << s) >> s, -1 >> s, (2**127) >> s, (-2**127) >> s, 12345 >> s, -12345 >> s)
print("shift-big", (1 << 200) >> 73, -(1 << 200) >> 73, 1 << 1000 >> 998)
try:
    1 << -1
except ValueError as e:
    print("ValueError", e)
try:
    1 >> -1
except ValueError as e:
    print("ValueError", e)

bitvals = [0, 1, -1, 0xFF, -0x100, 2**31 - 1, -2**31, 2**63, -2**63, 2**64 - 1, 2**127, -2**128 + 5]
for a in bitvals:
    print("bits", a, [(a & b, a | b, a ^ b) for b in (0, -1, 0xF0F0, -2**63, 2**64 - 1)])
print("bitlen", [x.bit_length() for x in bitvals], (2**64).bit_count(), (-7).bit_count())

print("bool", True + True, True - False, True * 7, -True, ~True, ~False, True // 1, True % 2, True ** 10, True << 3, True & 3, True | 2, True ^ 1, False ** 0)
print("bool-types", type(True + True).__name__, type(True & 1).__name__, type(2 | False).__name__, type(-False).__name__, type(True ^ 1).__name__)
print("mixed-bool", 2**63 + True, 2**127 - True, [1, 2, 3][True], divmod(True, 2), abs(-True))

zeros = [lambda: 1 // 0, lambda: 1 % 0, lambda: divmod(1, 0), lambda: 2**70 // 0, lambda: 2**70 % 0, lambda: pow(2, 3, 0)]
for z in zeros:
    try:
        z()
    except (ZeroDivisionError, ValueError) as e:
        print(type(e).__name__, e)

x = 0
for i in range(-20, 20):
    x = x * 31 + i // 3 - i % 5
    x ^= i << (i & 7)
print("mix", x, x % 1000003, x.bit_length())
acc = 1
for i in range(1, 60):
    acc = acc * i
    if acc > 2**63:
        acc = acc // 2**40 - 1
print("acc", acc)
print("cmp", 2**63 > 2**63 - 1, -2**63 < -2**63 + 1, 2**127 == 2**127, 2**127 != 2**127 + 1, sorted([2**64, -2**64, 0, 2**63, -1]))
print("int()", int("-0"), int("  +17  "), int("1_000"), int("-9223372036854775809"), int("170141183460469231731687303715884105728"), int(-2.9), int(2.0**63))
