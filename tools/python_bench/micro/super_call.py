"""Zero-argument `super().method()` call from an overriding method."""
import sys
import time

N = 120000


class Base:
    def scale(self, a):
        return a * 2


class Derived(Base):
    def scale(self, a):
        return super().scale(a) + 1


def bench(n):
    d = Derived()
    s = 0
    for i in range(n):
        s += d.scale(i)
    return s


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("super_call", N, result)
print("@bench-time %.6f" % elapsed, file=sys.stderr)
