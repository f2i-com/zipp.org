"""`hasattr()` for an attribute that does not exist (AttributeError path)."""
import sys
import time

N = 100000


class Thing:
    def __init__(self):
        self.present = 1


def bench(n):
    t = Thing()
    misses = 0
    for i in range(n):
        if not hasattr(t, "absent"):
            misses += 1
    return misses


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("hasattr_miss", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
