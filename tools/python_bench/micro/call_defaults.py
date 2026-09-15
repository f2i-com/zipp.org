"""Call relying on a default argument: `s = inc(s)` with `def inc(a, b=1)`."""
import sys
import time

N = 400000


def inc(a, b=1):
    return a + b


def bench(n):
    s = 0
    for i in range(n):
        s = inc(s)
    return s


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("call_defaults", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
