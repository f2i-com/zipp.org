"""Small list comprehension over a 3-tuple, evaluated once per iteration."""
import sys
import time

N = 100000


def bench(n):
    a = 1
    b = 2
    c = 3
    t = 0
    for i in range(n):
        t += len([x for x in (a, b, c)])
    return t


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("listcomp_small", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
