# Int literals hoisted out of loops, and `%` / `//` by a positive literal
# with dividends of both signs, at the interned-range and i128 boundaries.
BIG = 2147483648


def loop_literals(n):
    total = 0
    seed = 12345
    hits = 0
    for i in range(n):
        total += i % 7
        seed = (seed * 1103515245 + 12345) % 2147483648
        if seed // 65536 < 16384:
            hits += 1
        total -= seed % 3
        total += 1024 - 1023 + 256 - 257
    return total, seed, hits


print(loop_literals(1000))


def signs():
    out = []
    for a in (-17, -7, -1, 0, 1, 7, 17, -2147483649, 2147483649, -(2 ** 100) - 3, 2 ** 100 + 3):
        out.append((a % 7, a // 7, a % 2147483648, a // 2147483648, a % 1, a // 1))
    return out


for row in signs():
    print(row)

# The same literal inside and outside a loop, in a nested loop, in a loop
# else clause, under break and continue, and in a nested function that must
# not share the enclosing loop's register.
def mixed():
    acc = [5000]
    for i in range(3):
        for j in range(2):
            acc.append(5000 + i * 10 + j)
            if j == 1:
                continue
            acc.append(-5000)
        if i == 2:
            break
    else:
        acc.append(99999)

    def inner():
        return 5000 * 2

    while len(acc) < 12:
        acc.append(70000)
    acc.append(inner())
    acc.append(5000)
    return acc


print(mixed())

# Literals just outside the interned range, negative and positive, repeated.
def edges():
    s = 0
    for _ in range(5):
        s += 1024
        s -= 1023
        s += -257
        s -= -256
        s += 170141183460469231731687303715884105727  # i128::MAX
        s -= 170141183460469231731687303715884105728  # i128::MAX + 1
    return s


print(edges())

# Division by a literal in a while loop, dividend crossing zero.
def cross():
    x = -20
    out = []
    while x <= 20:
        out.append((x // 6, x % 6, x // 1000000007, x % 1000000007))
        x += 5
    return out


print(cross())

# A zero divisor still raises, literal or not.
for expr in (lambda: 5 % 0, lambda: 5 // 0, lambda: (-5) % 0):
    try:
        expr()
    except ZeroDivisionError as e:
        print("ZeroDivisionError:", e)

# Floats and bools mixed with a positive literal divisor take the general path.
print(7.5 % 2, -7.5 // 2, True % 2, True // 1, (-3) % True)
