"""Generator function consumed by a `for` loop, 1000 items per generator."""
import sys
import time

N = 750000


def numbers(count):
    for i in range(count):
        yield i


def bench(n):
    s = 0
    for k in range(n // 1000):
        for v in numbers(1000):
            s += v
    return s


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("generator_iter", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
