"""Instance construction with `__init__` storing two attributes: `Vec(i, 2)`."""
import sys
import time

N = 120000


class Vec:
    def __init__(self, x, y):
        self.x = x
        self.y = y


def bench(n):
    v = None
    for i in range(n):
        v = Vec(i, 2)
    return v.x + v.y


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("instance_new", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
