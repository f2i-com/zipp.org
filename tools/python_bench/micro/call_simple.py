"""Simple call of a plain module-level function: `s = add(s, 1)`."""
import sys
import time

N = 1200000


def add(a, b):
    return a + b


def bench(n):
    s = 0
    for i in range(n):
        s = add(s, 1)
    return s


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("call_simple", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
