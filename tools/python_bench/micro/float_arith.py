"""Float arithmetic and compares: `x = x * 0.5 + 1.25`, mixed updates and `<`."""
import sys
import time

N = 200000


def bench(n):
    x = 0.0
    y = 1.5
    below = 0
    for i in range(n):
        x = x * 0.5 + 1.25
        y = y * 0.999 + x * 0.001 - 0.0005
        if x < y:
            below += 1
        elif y > 2.0:
            y = y - 1.0
    return x, y, below


t0 = time.perf_counter()
result = bench(N)
elapsed = time.perf_counter() - t0
print("float_arith", N, "%.9f" % result[0], "%.9f" % result[1], result[2])
print("@bench-time %.6f" % elapsed, file=sys.stderr)
