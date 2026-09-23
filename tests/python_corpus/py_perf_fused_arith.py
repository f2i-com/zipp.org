# The fused int/float fast paths at the edges of their operand types:
# i128 overflow into big ints, floor division and modulo signs, bools,
# mixed int/float (exact conversion below 2^53, rounding above), NaN and
# signed zeros, and the fallbacks for everything else.

def ops(a, b):
    out = []
    for f in (lambda: a + b, lambda: a - b, lambda: a * b, lambda: a / b, lambda: a // b,
              lambda: a % b, lambda: a & b, lambda: a | b, lambda: a ^ b):
        try:
            out.append(f())
        except Exception as e:
            out.append(type(e).__name__)
    return out


def cmps(a, b):
    return [a < b, a <= b, a > b, a >= b, a == b, a != b]


big = 2 ** 127
huge = 2 ** 200
values = [0, 1, -1, 7, -7, 3, -3, 2 ** 53, 2 ** 53 + 1, -(2 ** 53) - 1, big - 1, -big, huge, -huge,
          0.0, -0.0, 1.5, -2.5, 1e308, float("inf"), float("-inf"), True, False]
for a in values:
    for b in [3, -3, 0, 2.0, -0.0, 2 ** 64, True, 2 ** 70, huge]:
        print(repr(a), repr(b), ops(a, b), cmps(a, b))

nan = float("nan")
print(cmps(nan, nan), cmps(nan, 1), cmps(1, nan), cmps(2 ** 60, nan), nan == nan, nan != nan)
print(cmps(2 ** 53 + 1, float(2 ** 53)), cmps(float(2 ** 53), 2 ** 53 + 1), 2 ** 53 + 1 == 2.0 ** 53)
print(cmps(10 ** 30, 1e30), 10 ** 30 == 1e30, 10 ** 22 == 1e22)
print(0.0 * -1, 0 * -1.0, -0.0 + 0, 0 + -0.0, -0.0 - 0, 1 / -0.0 if False else "skip")


def loop_sum(n):
    s = 0
    f = 0.0
    x = 1
    for i in range(n):
        s += i * i
        f += i / 3
        x = (x * 1103515245 + 12345) % 2147483648
        if x // 65536 > 16384:
            s -= 1
    return s, f, x


print(loop_sum(1000))


def overflow(n):
    x = 1
    for _ in range(n):
        x = x * 3 + 1
    y = -x
    return x, y, x // 7, x % 7, y // 7, y % 7, x // -7, x % -7, x & 0xFFFF, x | 1, x ^ y


print(overflow(90))
print(overflow(200))


# Strings: equality is inline, ordering and concatenation are not.
def strs(a, b):
    return [a == b, a != b, a < b, a + b]


for a, b in [("a", "a"), ("a", "b"), ("", ""), ("é", "e"), ("\U0001F600", "￿"), ("ab", "a")]:
    print(strs(a, b))
print("x" == 1, 1 == "x", "x" != None, None == None, [1] == [1], (1, 2) < (1, 3))


class Num:
    def __init__(self, v):
        self.v = v

    def __add__(self, o):
        return "add " + str(self.v)

    def __radd__(self, o):
        return "radd " + str(self.v)

    def __lt__(self, o):
        return "lt"

    def __eq__(self, o):
        return "eq"


n = Num(3)
print(n + 1, 1 + n, 2.5 + n, n < 1, n == 1, 1 == n)
i = 0
while i < 10:
    i += 3
print(i)
w = 1.5
while w < 100:
    w *= 2
print(w)
