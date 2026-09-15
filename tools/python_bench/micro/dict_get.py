"""`dict.get` with a default on int keys, half of them missing."""
import sys
import time

N = 300000


def bench(n):
    d = {}
    for j in range(0, 1000, 2):
        d[j] = j
    s = 0
    for i in range(n):
        s += d.get(i % 1000, 1)
    return s


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("dict_get", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
