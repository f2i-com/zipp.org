"""Bound method call on an instance: `c.bump(1)`."""
import sys
import time

N = 250000


class Counter:
    def __init__(self):
        self.value = 0

    def bump(self, d):
        self.value += d
        return self.value


def bench(n):
    c = Counter()
    last = 0
    for i in range(n):
        last = c.bump(1)
    return last


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("method_call", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
