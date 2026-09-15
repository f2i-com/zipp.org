"""Counted loop: `for i in range(n): s += i` inside a function."""
import sys
import time

N = 6000000


def bench(n):
    s = 0
    for i in range(n):
        s += i
    return s


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("range_loop", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
