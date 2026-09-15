"""Module attribute call `math.sqrt(x)` on floats."""
import math
import sys
import time

N = 260000


def bench(n):
    xs = [2.0, 3.0, 5.0, 7.0]
    s = 0.0
    for i in range(n):
        s += math.sqrt(xs[i % 4])
    return s


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("math_sqrt", N, "%.6f" % result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
