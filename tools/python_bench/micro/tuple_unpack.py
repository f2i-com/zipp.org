"""Tuple unpacking from an existing pair: `a, b = pair`."""
import sys
import time

N = 300000


def bench(n):
    pairs = [(1, 2), (3, 4), (5, 6), (7, 8)]
    s = 0
    for i in range(n):
        a, b = pairs[i % 4]
        s += a - b
    return s


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("tuple_unpack", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
