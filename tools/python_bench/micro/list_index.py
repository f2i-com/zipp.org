"""List subscript read and store: `a[j] = a[j] + 1`."""
import sys
import time

N = 800000


def bench(n):
    a = [0] * 1000
    for i in range(n):
        j = i % 1000
        a[j] = a[j] + 1
    return a[0], a[999], sum(a)


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("list_index", N, result[0], result[1], result[2])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
