"""`@property` getter read: `s += box.size`."""
import sys
import time

N = 350000


class Box:
    def __init__(self, size):
        self._size = size

    @property
    def size(self):
        return self._size


def bench(n):
    box = Box(3)
    s = 0
    for i in range(n):
        s += box.size
    return s


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("property_read", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
