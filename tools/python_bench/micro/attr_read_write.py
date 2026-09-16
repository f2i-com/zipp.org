"""Instance attribute read and write: `p.x = p.x + p.y`."""
import sys
import time

N = 400000


class Point:
    def __init__(self, x, y):
        self.x = x
        self.y = y


def bench(n):
    p = Point(0, 1)
    for i in range(n):
        p.x = p.x + p.y
    return p.x


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("attr_read_write", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
