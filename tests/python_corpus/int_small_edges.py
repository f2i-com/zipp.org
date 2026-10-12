# Int arithmetic and comparisons around the immediate-int boundary (2**46),
# where the interpreter's inline forms hand over to the general path: in
# straight-line code, in loops that stay interpreted (each iteration calls a
# function) and in loops long enough to compile.
B = 2**46


def ident(x):
    return x


def edges():
    out = []
    for d in (-3, -2, -1, 0, 1, 2, 3):
        for base in (B, -B, B * 2, -B * 2, B // 2, -(B // 2), 0):
            out.append(base + d)
    return out


E = edges()

for a in E:
    print("unary", a, a + 1, a - 1, a + 2, a - 2, a + 1000, a - 1000)

for a in E[:14]:
    row = []
    for b in (1, -1, 2, -2, 3, 7, B, -B, B - 1, 1 - B):
        row.append((a + b, a - b, a * b, a // b, a % b, a & b, a | b, a ^ b))
    print("binary", a, row)

for a in E[:14]:
    row = []
    for b in (B - 1, B, B + 1, -B - 1, -B, -B + 1, a, 0):
        row.append((a < b, a <= b, a > b, a >= b, a == b, a != b))
    print("cmp", a, row)


# Loops that stay interpreted by default: every iteration calls a function.
def count_up(start, n):
    total = 0
    last = None
    for i in range(start, start + n):
        total = ident(total) + i
        last = i
    return total, last


def count_down(start, n):
    total = 0
    i = start
    while i > start - n:
        total = ident(total) - i
        i -= 1
    return total, i


def cross(start, n, step):
    seen = []
    x = start
    for k in range(n):
        x = ident(x) + step
        if x >= B or x < -B:
            seen.append(k)
    return x, len(seen), seen[:3]


print("up", count_up(B - 50, 100), count_up(-B - 50, 100), count_up(0, 300))
print("down", count_down(B + 50, 100), count_down(-B + 50, 100))
print("cross", cross(B - 300, 600, 1), cross(-B + 300, 600, -1), cross(B - 7, 600, 7), cross(3, 600, B // 100))


# The same without calls: loops long enough to compile.
def tight_up(start, n):
    total = 0
    for i in range(start, start + n):
        total += i
        if i == B:
            total -= 1
    return total


def tight_mix(n):
    x = B - n
    y = -B + n
    hits = 0
    for i in range(2 * n):
        x += 1
        y -= 1
        if x > B:
            hits += 1
        if y <= -B:
            hits += 2
        if x * 2 // 2 != x or (y % 7 + 7) % 7 != y % 7:
            hits += 1000
    return x, y, hits


print("tight", tight_up(B - 2000, 4000), tight_up(-B - 2000, 4000), tight_mix(3000))

# Products and quotients whose operands are small and whose results are not
# (and the other way round).
for a, b in ((B - 1, 2), (B // 2, 2), (B // 2 + 1, 2), (2**23, 2**23), (2**23, -2**23), (-2**23, 2**23 + 1),
             (3037000500, 3037000500), (B - 1, B - 1), (-B, -B), (-B, -1), (B, 1)):
    print("mul", a, b, a * b, (a * b) // b, (a * b) % b, a * b - 1, a * b + 1, -(a * b))

# Floor division and modulo signs at the boundary.
for a in (B - 1, -B, B, -B - 1, 7, -7):
    print("floor", a, [(a // b, a % b) for b in (2, -2, 3, -3, B - 1, -B, B, 1 - B)])

# A range whose counter leaves the immediate range, and comparisons of a
# counter against a bound on the other side of it.
n = 0
for i in range(B - 5, B + 5):
    n += i - B
print("range", n, list(range(B - 2, B + 2)), list(range(-B - 2, -B + 2)), len(range(-B, B)))
k = 0
i = B - 3
while i < B + 3:
    k += 1
    i += 1
print("while", k, i)

# bool and float operands never take the int forms.
t = 0
for i in range(200):
    t = t + True + (i < 100) - False
u = 0.0
for i in range(200):
    u = u + i * 0.5 - (i > 150)
print("mixed", t, u, True + B - 1, B - True, (B - 1) + 0.0, B + 0.5 > B, B - 1 < float(B))
