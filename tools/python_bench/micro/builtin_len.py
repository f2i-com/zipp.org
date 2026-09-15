"""Builtin `len()` on a short list."""
import sys
import time

N = 600000


def bench(n):
    xs = [1, 2, 3]
    t = 0
    for i in range(n):
        t += len(xs)
    return t


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("builtin_len", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
