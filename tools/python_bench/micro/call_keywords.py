"""Keyword-argument call: `s = inc(a=s, b=1)`."""
import sys
import time

N = 220000


def inc(a, b=2):
    return a + b


def bench(n):
    s = 0
    for i in range(n):
        s = inc(a=s, b=1)
    return s


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("call_keywords", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
