"""Swap through a tuple: `a, b = b, a`."""
import sys
import time

N = 170000


def bench(n):
    a, b = 0, 1
    flips = 0
    for i in range(n):
        a, b = b, a
        flips += a
    return a, b, flips


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("tuple_swap", N, result[0], result[1], result[2])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
