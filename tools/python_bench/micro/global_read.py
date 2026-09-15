"""Module global read from inside a function: `s += K`."""
import sys
import time

N = 3000000
K = 3


def bench(n):
    s = 0
    for i in range(n):
        s += K
    return s


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("global_read", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
