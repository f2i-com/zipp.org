# Small numeric workloads mixing int and float arithmetic, loops, lists, dicts, tuples,
# closures and recursion; each prints a checksum so any fast-path slip shows.
def sieve(n):
    flags = [True] * (n + 1)
    flags[0] = flags[1] = False
    for i in range(2, int(n ** 0.5) + 1):
        if flags[i]:
            for j in range(i * i, n + 1, i):
                flags[j] = False
    return [i for i, f in enumerate(flags) if f]


primes = sieve(3000)
print("sieve", len(primes), primes[:10], primes[-3:], sum(primes) % 1000003)


def gcd(a, b):
    while b:
        a, b = b, a % b
    return a


def lcm_range(n):
    acc = 1
    for k in range(1, n + 1):
        acc = acc * k // gcd(acc, k)
    return acc


print("gcd-lcm", gcd(2**40 * 3, 6**20), lcm_range(40), lcm_range(60).bit_length())


def collatz_len(n, cache={1: 1}):
    path = []
    while n not in cache:
        path.append(n)
        n = n // 2 if n % 2 == 0 else 3 * n + 1
    length = cache[n]
    for m in reversed(path):
        length += 1
        cache[m] = length
    return length


best = max(range(1, 3000), key=collatz_len)
print("collatz", best, collatz_len(best), collatz_len(27), len(collatz_len.__defaults__[0]) > 3000)


def matmul(a, b):
    n, m, p = len(a), len(b), len(b[0])
    out = [[0.0] * p for _ in range(n)]
    for i in range(n):
        row = a[i]
        oi = out[i]
        for k in range(m):
            aik = row[k]
            bk = b[k]
            for j in range(p):
                oi[j] += aik * bk[j]
    return out


size = 12
A = [[(i * 7 + j * 3) % 11 / 10.0 - 0.5 for j in range(size)] for i in range(size)]
B = [[1.0 if i == j else (i - j) / 100.0 for j in range(size)] for i in range(size)]
C = matmul(A, B)
trace = sum(C[i][i] for i in range(size))
print("matmul", round(trace, 12), round(sum(map(sum, C)), 12), C[3][5], max(max(r) for r in C))


def newton_sqrt(x, iters=30):
    guess = x / 2.0 if x > 1 else 1.0
    for _ in range(iters):
        guess = (guess + x / guess) / 2.0
    return guess


print("newton", [newton_sqrt(v) for v in (2, 10, 1e6, 0.25)], newton_sqrt(2) ** 2 - 2)


def isqrt(n):
    x = n
    y = (x + 1) // 2
    while y < x:
        x = y
        y = (x + n // x) // 2
    return x


print("isqrt", isqrt(10**40), isqrt(2**127 - 1), isqrt(99), isqrt(2**64) ** 2 == 2**64)


def mandel(w, h, max_iter):
    rows = []
    for py in range(h):
        y0 = py / h * 2.4 - 1.2
        row = []
        for px in range(w):
            x0 = px / w * 3.2 - 2.2
            x = y = 0.0
            n = 0
            while x * x + y * y <= 4.0 and n < max_iter:
                x, y = x * x - y * y + x0, 2 * x * y + y0
                n += 1
            row.append(" .:-=+*#%@"[n % 10])
        rows.append("".join(row))
    return rows


for line in mandel(32, 10, 40):
    print("mandel", line)


def lcg(seed):
    state = seed
    while True:
        state = (state * 6364136223846793005 + 1442695040888963407) % 2**64
        yield state >> 33


gen = lcg(42)
samples = [next(gen) for _ in range(200)]
buckets = {}
for s in samples:
    buckets[s % 8] = buckets.get(s % 8, 0) + 1
print("lcg", samples[:3], sorted(buckets.items()), sum(samples) % 997)
mean = sum(samples) / len(samples)
var = sum((s - mean) ** 2 for s in samples) / len(samples)
print("lcg-stats", round(mean / 2**31, 6), round((var ** 0.5) / 2**31, 6))


def fib_memo():
    memo = {0: 0, 1: 1}

    def fib(n):
        if n not in memo:
            memo[n] = fib(n - 1) + fib(n - 2)
        return memo[n]
    return fib


fib = fib_memo()
print("fib-memo", fib(90), fib(150), [fib(i) % 10 for i in range(20)])


def partitions(n):
    ways = [1] + [0] * n
    for part in range(1, n + 1):
        for total in range(part, n + 1):
            ways[total] += ways[total - part]
    return ways[n]


print("partitions", partitions(50), partitions(120))


def digits_sum(n):
    s = 0
    while n:
        n, d = divmod(n, 10)
        s += d
    return s


print("digits", digits_sum(2**1000), digits_sum(3**500), sum(int(c) for c in str(7**300)))
points = [((i * 37) % 101 - 50, (i * 91) % 103 - 51) for i in range(60)]
cx = sum(p[0] for p in points) / len(points)
cy = sum(p[1] for p in points) / len(points)
far = max(points, key=lambda p: (p[0] - cx) ** 2 + (p[1] - cy) ** 2)
inside = sum(1 for x, y in points if x * x + y * y < 900)
print("geometry", round(cx, 6), round(cy, 6), far, inside)
hist = [0] * 10
for x, y in points:
    hist[(x * x + y * y) % 10] += 1
print("hist", hist, hist.index(max(hist)))
bits = 0
for i in range(64):
    if i % 3 == 0:
        bits |= 1 << i
    elif i % 5 == 0:
        bits ^= 1 << (63 - i)
print("bits", bits, bin(bits).count("1"), bits.bit_length(), hex(bits), bits & -bits, (bits >> 7) & 0xFF)
