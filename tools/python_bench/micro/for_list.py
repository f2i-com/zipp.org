"""`for v in xs` over a 1000-element list, summing the elements."""
import sys
import time

N = 1500000


def bench(n):
    xs = list(range(1000))
    s = 0
    for k in range(n // 1000):
        for v in xs:
            s += v
    return s


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("for_list", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
